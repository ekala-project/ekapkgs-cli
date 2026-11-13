# Server Storage Backends

## Backends

- **filesystem** — cache directory with `{hash}.narinfo` + `nar/` files, LRU garbage collection
- **nix-store** — serves directly from `/nix/store` via the nix daemon
- **castore** — content-addressed chunk store with file-level deduplication (see below)

## Content-Addressed Store (`castore` backend)

The `castore` storage backend decomposes NARs into a content-addressed Merkle tree of blake3-hashed chunks, enabling file-level deduplication across store paths. Two packages sharing identical files store those files' chunks only once.

### On-disk layout

```
{root}/
  chunks/{hex[0..4]}/{hex}.chunk   — FastCDC blob chunks of 1 KiB or more (16K-256K, 64K avg)
  packs/pack-NNNNNN.dat            — chunks under 1 KiB, appended as records
  castore.db                       — SQLite metadata (WAL mode)
  castore.db.recover-from          — present while a background copy is pending (see below)
```

### Pack files (`storage/pack.rs`)

Chunks smaller than `INLINE_THRESHOLD` (1 KiB) are appended to pack files instead of being stored in SQLite, so the database holds only metadata. Each record is `digest (32) | len (u32 LE) | data`, after an 8-byte `EKPACK01` header.

- Reads verify the stored digest, length and blake3 of the data; a damaged or torn record reads as a missing chunk.
- Each process start writes to a new pack (never after a possibly torn tail). Packs rotate at 1 GiB.
- Writers must call `PackStore::sync()` before committing rows that point into a pack (`put_nar`, `store_chunk`, recovery batches do).
- Packs are never rewritten: chunks evicted by GC leave dead bytes. There is no compaction yet.

### SQLite schema (castore.db, `SCHEMA_VERSION` = 2)

- `cas_paths` — maps store path hash → root CaNode (protobuf), narinfo metadata, `last_access` for GC
- `chunks` (`WITHOUT ROWID`) — chunk digest → size, `zstd_level`, `disk_size`, and `pack_id`/`pack_offset` for packed chunks
- `file_chunks` (`WITHOUT ROWID`) — file digest → ordered list of (chunk_digest, chunk_size) pairs
- `path_files` (`WITHOUT ROWID`) — store path hash → file digests, for GC reference checks
- `directories` — directory digest → serialized CaDirectory protobuf
- `_meta` — `schema_version`, `clean_shutdown`

The GC-only indexes (`GC_INDEXES`) are created in `open_db`, not `create_tables`, so they can be skipped while a background copy is pending.

### Schema migration

v1 databases (small chunk data in `chunks.data`, rowid tables, no `schema_version`) are not upgraded in place. `open_db` renames the file to `castore.db.v1-<ts>`, starts a fresh v2 database, and the background copy (`spawn_recovery`, same as corruption recovery) moves the rows over, appending inline data to packs. The cache is partially populated until the copy finishes. The old file is kept; the log says when it can be deleted. Offline tools (`compress-chunks`) refuse to run on a v1 database (`require_current_schema`).

### WAL size

`journal_size_limit` (64 MiB) shrinks the WAL whenever it is reset, and `spawn_wal_maintenance` tries a `wal_checkpoint(TRUNCATE)` every 5 minutes, waiting at most 250 ms for readers. Automatic PASSIVE checkpoints alone can't reset the WAL while readers are active, which let it grow to several GB.

### Ingest flow (`put_nar`)

1. Parse incoming NAR into a `NarNode` tree (`ekapkgs-nix/src/nar.rs`)
2. Recursively walk the tree (`ingest_node`):
   - **Files**: FastCDC chunk the content, write chunk files to disk (idempotent), record chunks and file-chunk mappings in DB
   - **Directories**: Serialize as `CaDirectory` protobuf, store in SQLite with blake3 digest
   - **Symlinks**: Encoded directly in the CaNode tree
3. Store root CaNode + narinfo metadata in `cas_paths`
4. All DB writes wrapped in a single SQLite transaction for crash safety

### Retrieval flow (`get_nar`)

1. Load root CaNode from `cas_paths`
2. Recursively reconstruct `NarNode` tree from CAS data (load directories from SQLite, concatenate file chunks from disk)
3. Directories verified against expected blake3 digest on load
4. Serialize back to NAR bytes via `write_nar()`

### Compression options

Two independent compression knobs under `[compression]` in the server config:

- **`compress_chunks`** (default: `true`) — zstd-compresses individual CAS chunks on disk. Existing compressed chunks continue to decompress correctly when this is disabled; only newly written chunks are affected.
  - **When to disable**: high-throughput ingest where CPU is the bottleneck, or when disk space is cheap and you want to minimize write amplification and memory pressure from per-chunk zstd encoder contexts.

- **`compress_nars`** (default: `false`) — zstd-compresses reconstructed NAR files before serving. When enabled, narinfo advertises `Compression: zstd` and URLs use `.nar.zst`.
  - **When to disable** (keep default): clients and server are on the same LAN or localhost, where bandwidth is plentiful and compression just adds CPU overhead. Also unnecessary when the HTTP compression middleware (`[compression] enable = true`) already handles transport-level zstd, since clients that accept `Accept-Encoding: zstd` get compressed responses either way.
  - **When to enable**: serving clients over WAN or metered connections where narinfo-level compression lets Nix's own `builtins.fetchurl` decompress the NAR directly, without relying on HTTP middleware. Also useful when clients do not send `Accept-Encoding: zstd` (e.g. stock Nix without the ekapkgs client).

### Chunk-level negotiation (`NegotiateChunks` RPC)

Server-side (`api/negotiate.rs`): walks requested paths' Merkle trees and returns root nodes, directory data, file-chunk mappings, and only the chunks the client is missing. Request size capped (10K want, 10K have, 500K have_chunks).

Client-side (`cas_pull.rs`, `chunk_store.rs`): maintains a local chunk store at `~/.cache/ekapkgs/castore/` with SQLite metadata. Downloads missing chunks in parallel with blake3 verification, reassembles NARs, verifies sha256 against narinfo hash, then imports to nix store. Falls back to NAR streaming on failure.

### 3-tier pull fallback

The client pull command (`commands/cache.rs`) tries in order:
1. **CAS chunks** — if server has CAS data, negotiate at chunk level
2. **gRPC streaming** — stream full NARs over `StreamNars` RPC
3. **HTTP batch** — individual HTTP GET per NAR

Each tier falls back to the next on failure or if the server doesn't support it.

### Startup integrity and crash recovery

Startup never scans the database, so systemd's start timeout can't be hit on a large cache.

- `_meta.clean_shutdown` is set to `0` at startup and `1` in `on_shutdown`. The server handles SIGTERM/SIGINT by draining connections (10s cap) and then calling `on_shutdown`.
- If the previous run was unclean, `spawn_integrity_check` runs `PRAGMA quick_check` on a read-only connection in the background while the server keeps serving. `mark_clean_shutdown` is skipped until that check passes, so an interrupted check runs again on the next start.
- If corruption is found, it writes `castore.db.needs-recovery` and exits with status 75. On restart, `open_db` sees the marker, renames the DB to `castore.db.corrupt-<ts>`, starts with an empty DB, and `spawn_recovery` copies back whatever rows it can read.
- The copy source is recorded in `castore.db.recover-from`. If the server restarts before the copy finishes, it resumes on the next start (inserts are `OR IGNORE`, and packed chunks are checked before appending). The marker is removed when the copy completes.
- While recovery runs: the GC-only indexes (`GC_INDEXES`) are dropped and rebuilt at the end, the writer uses a 512 MiB cache and `synchronous=NORMAL`, the bloom filters stay bypassed so recovered rows are served immediately, and `path_files_ready` stays `false` so GC and the `path_files` backfill don't run.

### GC support

- `evict_path()` walks the Merkle tree, deletes the path, removes orphaned file_chunks and chunks (computed reference checks, not tracked ref_count)
- `update_access()` for batch last-access updates
- `paths_by_access_asc()` for LRU eviction ordering
- Shared chunks (referenced by multiple paths) are preserved until all referencing paths are evicted

### HTTP endpoints

- `GET/PUT /cas/chunk/{b3hex}` — individual chunk download/upload with blake3 verification
- `GET /nar/{hash}.nar` — transparently reconstructs NAR from CAS chunks

### Protobuf types (`proto/ekapkgs/v1/castore.proto`)

- `CaNode` — oneof: `CaDirectoryNode`, `CaFileNode`, `CaSymlinkNode`
- `CaDirectory` — repeated `CaDirectoryEntry` (name + CaNode)
- `B3Digest` — 32-byte blake3 digest
- `ChunkMeta` — digest + size
- `ChunkNegotiateRequest/Response` — chunk-level negotiation messages
- `CaPathMapping` — store path hash → root CaNode
- `FileChunkMapping` — file digest → ordered chunk list
