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
  chunks/{hex[0..4]}/{hex}.chunk   — FastCDC blob chunks (16K-256K, 64K avg)
  castore.db                       — SQLite metadata (WAL mode)
```

### SQLite schema (castore.db)

- `cas_paths` — maps store path hash → root CaNode (protobuf), narinfo metadata, `last_access` for GC
- `chunks` — chunk digest → size
- `file_chunks` — file digest → ordered list of (chunk_digest, chunk_size) pairs
- `directories` — directory digest → serialized CaDirectory protobuf

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

### Chunk-level negotiation (`NegotiateChunks` RPC)

Server-side (`api/negotiate.rs`): walks requested paths' Merkle trees and returns root nodes, directory data, file-chunk mappings, and only the chunks the client is missing. Request size capped (10K want, 10K have, 500K have_chunks).

Client-side (`cas_pull.rs`, `chunk_store.rs`): maintains a local chunk store at `~/.cache/ekapkgs/castore/` with SQLite metadata. Downloads missing chunks in parallel with blake3 verification, reassembles NARs, verifies sha256 against narinfo hash, then imports to nix store. Falls back to NAR streaming on failure.

### 3-tier pull fallback

The client pull command (`commands/cache.rs`) tries in order:
1. **CAS chunks** — if server has CAS data, negotiate at chunk level
2. **gRPC streaming** — stream full NARs over `StreamNars` RPC
3. **HTTP batch** — individual HTTP GET per NAR

Each tier falls back to the next on failure or if the server doesn't support it.

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
