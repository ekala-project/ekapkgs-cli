//! Content-addressed storage backend.
//!
//! Decomposes NARs into a Merkle tree of blake3-hashed chunks using FastCDC
//! for content-defined chunking. Identical file contents across store paths
//! are stored only once.
//!
//! On-disk layout:
//!   {root}/chunks/{hex[0..4]}/{hex}.chunk  — blob chunks
//!   {root}/dirs/{hex}.dir                  — serialized CaDirectory protobufs
//!   {root}/castore.db                      — SQLite metadata

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ekapkgs_nix::bloom::BloomFilter;

use ekapkgs_nix::nar::{ChunkReader, NarDirectoryEntry, NarNode, parse_nar, write_nar_streaming};
use ekapkgs_protocol::ekapkgs::v1::{
    B3Digest, CaDirectory, CaDirectoryEntry, CaDirectoryNode, CaFileNode, CaNode, CaSymlinkNode,
    ChunkMeta,
};
use prost::Message;
use rusqlite::{Connection, params};

use super::{NarInfo, StorageBackend};

/// A file digest and its ordered list of (chunk_digest, chunk_size) pairs.
type FileChunkMap = Vec<([u8; 32], Vec<([u8; 32], u64)>)>;

/// Combined result of a single CAS tree walk: missing chunks, directory data,
/// and file-to-chunk mappings.
type CasTreeWalkResult = (Vec<ChunkMeta>, Vec<([u8; 32], CaDirectory)>, FileChunkMap);

/// FastCDC chunking parameters.
const CHUNK_MIN: u32 = 16 * 1024; // 16 KiB
const CHUNK_AVG: u32 = 64 * 1024; // 64 KiB
const CHUNK_MAX: u32 = 256 * 1024; // 256 KiB

/// Represents the client's set of already-cached chunk digests.
///
/// Either an exact hash set (from the flat `have_chunks` list) or a
/// probabilistic bloom filter (compact, ~1% FPR).
pub enum ChunkHaveCheck {
    Exact(HashSet<[u8; 32]>),
    Bloom(BloomFilter),
}

impl ChunkHaveCheck {
    /// Returns `true` if the client (probably) already has this chunk.
    pub fn contains(&self, digest: &[u8; 32]) -> bool {
        match self {
            Self::Exact(set) => set.contains(digest),
            Self::Bloom(bf) => bf.maybe_contains(digest),
        }
    }
}

pub struct CastoreBackend {
    root: PathBuf,
    db: Mutex<Connection>,
}

impl CastoreBackend {
    /// Create a new CAS backend rooted at the given directory.
    pub fn new(root: PathBuf) -> color_eyre::Result<Self> {
        std::fs::create_dir_all(root.join("chunks"))?;

        let db_path = root.join("castore.db");
        let conn = open_db(&db_path)?;
        create_tables(&conn)?;

        // Migrate: move any existing on-disk directories into SQLite.
        let dirs_path = root.join("dirs");
        if dirs_path.is_dir() {
            migrate_dirs_to_db(&conn, &dirs_path)?;
        }

        Ok(Self {
            root,
            db: Mutex::new(conn),
        })
    }

    /// Get a chunk by its blake3 digest (as 32 raw bytes).
    pub fn get_chunk_by_digest(&self, digest: &[u8; 32]) -> color_eyre::Result<Option<Vec<u8>>> {
        let hex = hex_encode(digest);
        let path = self.chunk_path(&hex);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read(path)?))
    }

    /// Get the CAS root node for a store path hash.
    pub fn get_root_node(&self, hash: &str) -> color_eyre::Result<Option<CaNode>> {
        let db = self.db.lock().expect("db lock");
        let mut stmt = db.prepare("SELECT root_node FROM cas_paths WHERE hash = ?1")?;
        let result: Option<Vec<u8>> = stmt.query_row(params![hash], |row| row.get(0)).ok();
        drop(stmt);
        drop(db);

        match result {
            Some(bytes) => Ok(Some(CaNode::decode(bytes.as_slice())?)),
            None => Ok(None),
        }
    }

    /// Walk the Merkle trees for the requested store paths in a single pass,
    /// collecting missing chunks, directory data, and file-to-chunk mappings.
    ///
    /// `have` can be either an exact set of digests (from the flat
    /// `have_chunks` list) or a bloom filter (compact, ~1% FPR).
    pub fn walk_cas_trees(
        &self,
        want_hashes: &[&str],
        have: &ChunkHaveCheck,
    ) -> color_eyre::Result<CasTreeWalkResult> {
        let mut missing_chunks = Vec::new();
        let mut chunk_seen = HashSet::new();
        let mut directories = Vec::new();
        let mut dir_seen = HashSet::new();
        let mut file_mappings: FileChunkMap = Vec::new();
        let mut file_seen = HashSet::new();

        for hash in want_hashes {
            if let Some(root) = self.get_root_node(hash)? {
                self.walk_cas_node(
                    &root,
                    have,
                    &mut chunk_seen,
                    &mut missing_chunks,
                    &mut dir_seen,
                    &mut directories,
                    &mut file_seen,
                    &mut file_mappings,
                )?;
            }
        }

        Ok((missing_chunks, directories, file_mappings))
    }

    /// Store a chunk from an external upload, returning its digest.
    pub fn store_chunk_external(&self, data: &[u8]) -> color_eyre::Result<[u8; 32]> {
        self.store_chunk(data)
    }

    // --- GC methods ---

    /// Update `last_access` for a batch of store path hashes.
    pub fn update_access(&self, hashes: &[String]) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let Ok(db) = self.db.lock() else { return };
        if let Ok(tx) = db.unchecked_transaction() {
            for hash in hashes {
                let _ = tx.execute(
                    "UPDATE cas_paths SET last_access = ?1 WHERE hash = ?2",
                    params![now as i64, hash],
                );
            }
            let _ = tx.commit();
        }
    }

    /// Return total bytes stored in chunks.
    pub fn total_chunk_bytes(&self) -> color_eyre::Result<u64> {
        let db = self.db.lock().expect("db lock");
        let total: i64 = db.query_row("SELECT COALESCE(SUM(size), 0) FROM chunks", [], |row| {
            row.get(0)
        })?;
        Ok(total as u64)
    }

    /// Return total number of CAS paths stored.
    pub fn total_paths(&self) -> color_eyre::Result<u64> {
        let db = self.db.lock().expect("db lock");
        let count: i64 = db.query_row("SELECT COUNT(*) FROM cas_paths", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Evict a store path and its exclusively-referenced data.
    ///
    /// Deletes the `cas_paths` row, then removes any chunks that are no
    /// longer referenced by any remaining file_chunks row. Returns bytes
    /// freed from chunk file deletions.
    ///
    /// Directories are left in SQLite (they are small and will be
    /// overwritten naturally on future ingests).
    pub fn evict_path(&self, hash: &str) -> color_eyre::Result<u64> {
        let db = self.db.lock().expect("db lock");
        let tx = db.unchecked_transaction()?;

        // 1. Load root node and walk the tree to collect all file digests.
        let root_bytes: Option<Vec<u8>> = tx
            .query_row(
                "SELECT root_node FROM cas_paths WHERE hash = ?1",
                params![hash],
                |row| row.get(0),
            )
            .ok();

        let Some(root_bytes) = root_bytes else {
            return Ok(0);
        };

        let mut file_digests = HashSet::new();
        if let Ok(root_node) = CaNode::decode(root_bytes.as_slice()) {
            Self::collect_file_digests_from_tree(&tx, &root_node, &mut file_digests);
        }

        // 2. Delete the cas_paths row.
        tx.execute("DELETE FROM cas_paths WHERE hash = ?1", params![hash])?;

        // 3. For each file digest, collect its chunk digests. We don't delete file_chunks rows here
        //    because the same file_digest may be shared across multiple paths (file-level dedup).
        //    The file_chunks rows are small and will be cleaned up when no remaining path uses
        //    them.
        let mut chunk_digests_to_check = HashSet::new();
        for file_digest in &file_digests {
            let mut stmt =
                tx.prepare("SELECT chunk_digest FROM file_chunks WHERE file_digest = ?1")?;
            let chunks: Vec<Vec<u8>> = stmt
                .query_map(params![file_digest.as_slice()], |row| row.get(0))?
                .filter_map(std::result::Result::ok)
                .collect();
            drop(stmt);

            for chunk_vec in chunks {
                if let Ok(d) = <[u8; 32]>::try_from(chunk_vec.as_slice()) {
                    chunk_digests_to_check.insert(d);
                }
            }
        }

        // Check if these file_digests are still referenced by any remaining
        // cas_paths. If not, delete the file_chunks rows.
        for file_digest in &file_digests {
            let still_needed = self.is_file_digest_referenced(&tx, file_digest, hash)?;
            if !still_needed {
                tx.execute(
                    "DELETE FROM file_chunks WHERE file_digest = ?1",
                    params![file_digest.as_slice()],
                )?;
            }
        }

        // 4. For each chunk, check if it's still referenced by any remaining file_chunks row. If
        //    not, delete the chunk from DB and disk.
        let mut bytes_freed: u64 = 0;
        for chunk_digest in &chunk_digests_to_check {
            let still_referenced: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM file_chunks WHERE chunk_digest = ?1)",
                params![chunk_digest.as_slice()],
                |row| row.get(0),
            )?;

            if !still_referenced {
                let size: Option<i64> = tx
                    .query_row(
                        "SELECT size FROM chunks WHERE digest = ?1",
                        params![chunk_digest.as_slice()],
                        |row| row.get(0),
                    )
                    .ok();

                tx.execute(
                    "DELETE FROM chunks WHERE digest = ?1",
                    params![chunk_digest.as_slice()],
                )?;

                if let Some(size) = size {
                    bytes_freed += size as u64;
                }

                // Delete the chunk file from disk.
                let hex = hex_encode(chunk_digest);
                let path = self.chunk_path(&hex);
                let _ = std::fs::remove_file(path);
            }
        }

        tx.commit()?;
        Ok(bytes_freed)
    }

    /// Check if a file digest is referenced by any remaining cas_paths
    /// (excluding `exclude_hash`).
    fn is_file_digest_referenced(
        &self,
        conn: &Connection,
        file_digest: &[u8; 32],
        exclude_hash: &str,
    ) -> color_eyre::Result<bool> {
        // Walk all remaining paths' trees and check if any reference this file.
        let mut stmt = conn.prepare("SELECT root_node FROM cas_paths WHERE hash != ?1")?;
        let roots: Vec<Vec<u8>> = stmt
            .query_map(params![exclude_hash], |row| row.get(0))?
            .filter_map(std::result::Result::ok)
            .collect();
        drop(stmt);

        for root_bytes in &roots {
            if let Ok(root_node) = CaNode::decode(root_bytes.as_slice()) {
                let mut digests = HashSet::new();
                Self::collect_file_digests_from_tree(conn, &root_node, &mut digests);
                if digests.contains(file_digest) {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Walk a CaNode tree recursively, collecting all file digests.
    /// Uses the DB connection to load directory contents.
    fn collect_file_digests_from_tree(
        conn: &Connection,
        ca_node: &CaNode,
        file_digests: &mut HashSet<[u8; 32]>,
    ) {
        let Some(node) = ca_node.node.as_ref() else {
            return;
        };

        match node {
            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(file) => {
                if let Some(digest) = &file.digest {
                    if let Ok(d) = <[u8; 32]>::try_from(digest.digest.as_slice()) {
                        file_digests.insert(d);
                    }
                }
            },

            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Directory(dir) => {
                if let Some(digest) = &dir.digest {
                    if let Ok(d) = <[u8; 32]>::try_from(digest.digest.as_slice()) {
                        // Load directory data from SQLite and recurse.
                        let data: Option<Vec<u8>> = conn
                            .query_row(
                                "SELECT data FROM directories WHERE digest = ?1",
                                params![d.as_slice()],
                                |row| row.get(0),
                            )
                            .ok();

                        if let Some(data) = data {
                            if let Ok(ca_dir) = CaDirectory::decode(data.as_slice()) {
                                for entry in &ca_dir.entries {
                                    if let Some(child) = &entry.node {
                                        Self::collect_file_digests_from_tree(
                                            conn,
                                            child,
                                            file_digests,
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            },

            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(_) => {},
        }
    }

    /// List paths ordered by last access time (oldest first), for GC.
    pub fn paths_by_access_asc(&self) -> color_eyre::Result<Vec<(String, u64)>> {
        let db = self.db.lock().expect("db lock");
        let mut stmt =
            db.prepare("SELECT hash, nar_size FROM cas_paths ORDER BY last_access ASC")?;
        let results = stmt
            .query_map([], |row| {
                let hash: String = row.get(0)?;
                let nar_size: i64 = row.get(1)?;
                Ok((hash, nar_size as u64))
            })?
            .filter_map(std::result::Result::ok)
            .collect();
        Ok(results)
    }

    // --- Private helpers ---

    fn chunk_path(&self, hex: &str) -> PathBuf {
        let prefix = &hex[..4.min(hex.len())];
        self.root
            .join("chunks")
            .join(prefix)
            .join(format!("{hex}.chunk"))
    }

    /// Store a chunk file to disk and record it in the database.
    ///
    /// When called outside a transaction (e.g. from external chunk uploads),
    /// acquires the DB lock internally.
    fn store_chunk(&self, data: &[u8]) -> color_eyre::Result<[u8; 32]> {
        let digest = self.write_chunk_file(data)?;
        let db = self.db.lock().expect("db lock");
        Self::insert_chunk_row(&db, &digest, data.len())?;
        Ok(digest)
    }

    /// Write a chunk file to disk, returning its blake3 digest.
    /// Idempotent — if the file already exists, this is a no-op.
    fn write_chunk_file(&self, data: &[u8]) -> color_eyre::Result<[u8; 32]> {
        let hash = blake3::hash(data);
        let digest = *hash.as_bytes();
        let hex = hex_encode(&digest);
        let path = self.chunk_path(&hex);

        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
                let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
                tmp.write_all(data)?;
                match tmp.persist(&path) {
                    Ok(_) => {},
                    Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {},
                    Err(e) => return Err(e.error.into()),
                }
            }
        }

        Ok(digest)
    }

    /// Insert a row into the chunks table. Caller must hold a connection.
    fn insert_chunk_row(
        conn: &Connection,
        digest: &[u8; 32],
        size: usize,
    ) -> color_eyre::Result<()> {
        conn.execute(
            "INSERT OR IGNORE INTO chunks (digest, size, ref_count) VALUES (?1, ?2, 1)",
            params![digest.as_slice(), size as i64],
        )?;
        Ok(())
    }

    /// Insert a row into the file_chunks table. Caller must hold a connection.
    fn insert_file_chunk_row(
        conn: &Connection,
        file_digest: &[u8; 32],
        chunk_index: usize,
        chunk_digest: &[u8; 32],
        chunk_size: u64,
    ) -> color_eyre::Result<()> {
        conn.execute(
            "INSERT OR IGNORE INTO file_chunks (file_digest, chunk_index, chunk_digest, \
             chunk_size) VALUES (?1, ?2, ?3, ?4)",
            params![
                file_digest.as_slice(),
                chunk_index as i64,
                chunk_digest.as_slice(),
                chunk_size as i64
            ],
        )?;
        Ok(())
    }

    /// Insert a directory into SQLite. Caller must hold a connection.
    fn insert_directory(conn: &Connection, dir: &CaDirectory) -> color_eyre::Result<[u8; 32]> {
        let encoded = dir.encode_to_vec();
        let hash = blake3::hash(&encoded);
        let digest = *hash.as_bytes();
        conn.execute(
            "INSERT OR IGNORE INTO directories (digest, data) VALUES (?1, ?2)",
            params![digest.as_slice(), encoded],
        )?;
        Ok(digest)
    }

    /// Load a CaDirectory from SQLite by its digest, verifying integrity.
    fn load_directory(&self, digest: &[u8; 32]) -> color_eyre::Result<CaDirectory> {
        let db = self.db.lock().expect("db lock");
        let data: Vec<u8> = db.query_row(
            "SELECT data FROM directories WHERE digest = ?1",
            params![digest.as_slice()],
            |row| row.get(0),
        )?;

        // Verify blake3 digest matches stored data to detect corruption.
        let actual = blake3::hash(&data);
        if actual.as_bytes() != digest {
            return Err(color_eyre::eyre::eyre!(
                "directory digest mismatch: expected {}, got {}",
                hex_encode(digest),
                hex_encode(actual.as_bytes()),
            ));
        }

        Ok(CaDirectory::decode(data.as_slice())?)
    }

    /// Ingest a NarNode tree into CAS, returning the root CaNode.
    ///
    /// Chunk files are written to disk outside the transaction (idempotent).
    /// All DB writes (chunks, file_chunks, directories) use the provided
    /// connection, which the caller wraps in a transaction.
    fn ingest_node(&self, conn: &Connection, node: &NarNode) -> color_eyre::Result<CaNode> {
        match node {
            NarNode::Regular { executable, data } => {
                // Chunk the file data using FastCDC.
                let chunks = fastcdc::v2020::FastCDC::new(data, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX);
                let mut chunk_digests = Vec::new();
                let mut total_size = 0u64;

                for chunk in chunks {
                    let chunk_data = &data[chunk.offset..chunk.offset + chunk.length];
                    // Write chunk file to disk (idempotent, outside transaction).
                    let digest = self.write_chunk_file(chunk_data)?;
                    // Record in DB (inside caller's transaction).
                    Self::insert_chunk_row(conn, &digest, chunk_data.len())?;
                    chunk_digests.push((digest, chunk.length as u64));
                    total_size += chunk.length as u64;
                }

                let file_hash = blake3::hash(data);
                let file_digest = *file_hash.as_bytes();

                // Store chunk metadata in the DB for this file.
                for (i, (chunk_digest, chunk_size)) in chunk_digests.iter().enumerate() {
                    Self::insert_file_chunk_row(conn, &file_digest, i, chunk_digest, *chunk_size)?;
                }

                Ok(CaNode {
                    node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(
                        CaFileNode {
                            digest: Some(B3Digest {
                                digest: file_digest.to_vec(),
                            }),
                            size: total_size,
                            executable: *executable,
                        },
                    )),
                })
            },

            NarNode::Directory { entries } => {
                let mut ca_entries = Vec::new();
                let mut dir_size = 0u64;

                for entry in entries {
                    let child_node = self.ingest_node(conn, &entry.node)?;
                    dir_size += 1;
                    ca_entries.push(CaDirectoryEntry {
                        name: entry.name.clone(),
                        node: Some(child_node),
                    });
                }

                let ca_dir = CaDirectory {
                    entries: ca_entries,
                };
                let dir_digest = Self::insert_directory(conn, &ca_dir)?;

                Ok(CaNode {
                    node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Directory(
                        CaDirectoryNode {
                            digest: Some(B3Digest {
                                digest: dir_digest.to_vec(),
                            }),
                            size: dir_size,
                        },
                    )),
                })
            },

            NarNode::Symlink { target } => Ok(CaNode {
                node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(
                    CaSymlinkNode {
                        target: target.clone(),
                    },
                )),
            }),
        }
    }

    /// Reconstruct a NarNode tree from a CaNode root.
    ///
    /// Superseded by `write_nar_streaming()` + `ChunkReader` for production
    /// use. Retained for potential future use and test convenience.
    #[allow(dead_code)]
    fn reconstruct_node(&self, ca_node: &CaNode) -> color_eyre::Result<NarNode> {
        let node = ca_node
            .node
            .as_ref()
            .ok_or_else(|| color_eyre::eyre::eyre!("CaNode has no node variant"))?;

        match node {
            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(file) => {
                let digest = file
                    .digest
                    .as_ref()
                    .ok_or_else(|| color_eyre::eyre::eyre!("CaFileNode missing digest"))?;
                let file_digest: [u8; 32] = digest
                    .digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| color_eyre::eyre::eyre!("invalid digest length"))?;

                // Read file chunks and reassemble.
                let data = self.read_file_data(&file_digest)?;

                Ok(NarNode::Regular {
                    executable: file.executable,
                    data,
                })
            },

            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Directory(dir) => {
                let digest = dir
                    .digest
                    .as_ref()
                    .ok_or_else(|| color_eyre::eyre::eyre!("CaDirectoryNode missing digest"))?;
                let dir_digest: [u8; 32] = digest
                    .digest
                    .as_slice()
                    .try_into()
                    .map_err(|_| color_eyre::eyre::eyre!("invalid digest length"))?;

                let ca_dir = self.load_directory(&dir_digest)?;
                let mut entries = Vec::new();

                for ca_entry in &ca_dir.entries {
                    let child = ca_entry
                        .node
                        .as_ref()
                        .ok_or_else(|| color_eyre::eyre::eyre!("directory entry missing node"))?;
                    let child_node = self.reconstruct_node(child)?;
                    entries.push(NarDirectoryEntry {
                        name: ca_entry.name.clone(),
                        node: child_node,
                    });
                }

                entries.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(NarNode::Directory { entries })
            },

            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(symlink) => {
                Ok(NarNode::Symlink {
                    target: symlink.target.clone(),
                })
            },
        }
    }

    /// Read a file's data by reading and concatenating its chunks.
    fn read_file_data(&self, file_digest: &[u8; 32]) -> color_eyre::Result<Vec<u8>> {
        let db = self.db.lock().expect("db lock");
        let mut stmt = db.prepare(
            "SELECT chunk_digest, chunk_size FROM file_chunks WHERE file_digest = ?1 ORDER BY \
             chunk_index",
        )?;
        let chunks: Vec<(Vec<u8>, i64)> = stmt
            .query_map(params![file_digest.as_slice()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);
        drop(db);

        let mut data = Vec::new();
        for (chunk_digest_vec, _size) in chunks {
            let chunk_digest: [u8; 32] = chunk_digest_vec
                .as_slice()
                .try_into()
                .map_err(|_| color_eyre::eyre::eyre!("invalid chunk digest length"))?;
            let chunk_data = self.get_chunk_by_digest(&chunk_digest)?.ok_or_else(|| {
                color_eyre::eyre::eyre!("missing chunk {}", hex_encode(&chunk_digest))
            })?;
            data.extend_from_slice(&chunk_data);
        }

        Ok(data)
    }

    /// Recursively walk a CaNode tree, collecting all three result sets
    /// (missing chunks, directories, file-chunk mappings) in a single pass.
    #[allow(clippy::too_many_arguments)]
    fn walk_cas_node(
        &self,
        ca_node: &CaNode,
        have: &ChunkHaveCheck,
        chunk_seen: &mut HashSet<[u8; 32]>,
        missing_chunks: &mut Vec<ChunkMeta>,
        dir_seen: &mut HashSet<[u8; 32]>,
        directories: &mut Vec<([u8; 32], CaDirectory)>,
        file_seen: &mut HashSet<[u8; 32]>,
        file_mappings: &mut FileChunkMap,
    ) -> color_eyre::Result<()> {
        let Some(node) = ca_node.node.as_ref() else {
            return Ok(());
        };

        match node {
            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(file) => {
                let Some(digest) = file.digest.as_ref() else {
                    return Ok(());
                };
                let Ok(file_digest): Result<[u8; 32], _> = digest.digest.as_slice().try_into()
                else {
                    return Ok(());
                };

                if file_seen.insert(file_digest) {
                    // Query file chunks once for both missing-chunk and mapping collection.
                    let db = self.db.lock().expect("db lock");
                    let mut stmt = db.prepare(
                        "SELECT chunk_digest, chunk_size FROM file_chunks WHERE file_digest = ?1 \
                         ORDER BY chunk_index",
                    )?;
                    let chunks: Vec<([u8; 32], u64)> = stmt
                        .query_map(params![file_digest.as_slice()], |row| {
                            let digest_vec: Vec<u8> = row.get(0)?;
                            let size: i64 = row.get(1)?;
                            Ok((digest_vec, size as u64))
                        })?
                        .filter_map(|r| {
                            let (dv, sz) = r.ok()?;
                            let d: [u8; 32] = dv.as_slice().try_into().ok()?;
                            Some((d, sz))
                        })
                        .collect();
                    drop(stmt);
                    drop(db);

                    // Collect missing chunks (those not in the client's have set).
                    for &(chunk_digest, chunk_size) in &chunks {
                        if !have.contains(&chunk_digest) && chunk_seen.insert(chunk_digest) {
                            missing_chunks.push(ChunkMeta {
                                digest: Some(B3Digest {
                                    digest: chunk_digest.to_vec(),
                                }),
                                size: chunk_size,
                            });
                        }
                    }

                    // Collect the file-to-chunk mapping.
                    if !chunks.is_empty() {
                        file_mappings.push((file_digest, chunks));
                    }
                }
            },

            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Directory(dir) => {
                let Some(digest) = dir.digest.as_ref() else {
                    return Ok(());
                };
                let Ok(dir_digest): Result<[u8; 32], _> = digest.digest.as_slice().try_into()
                else {
                    return Ok(());
                };

                if dir_seen.insert(dir_digest) {
                    if let Ok(ca_dir) = self.load_directory(&dir_digest) {
                        // Recurse into children before adding this directory.
                        for entry in &ca_dir.entries {
                            if let Some(child) = &entry.node {
                                self.walk_cas_node(
                                    child,
                                    have,
                                    chunk_seen,
                                    missing_chunks,
                                    dir_seen,
                                    directories,
                                    file_seen,
                                    file_mappings,
                                )?;
                            }
                        }
                        directories.push((dir_digest, ca_dir));
                    }
                }
            },

            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(_) => {
                // Symlinks have no chunks, directories, or file mappings.
            },
        }

        Ok(())
    }

    /// Store narinfo metadata in SQLite for a store path.
    fn store_metadata(
        &self,
        hash: &str,
        narinfo: &NarInfo,
        root_node: &CaNode,
    ) -> color_eyre::Result<()> {
        let db = self.db.lock().expect("db lock");
        Self::store_metadata_with_conn(&db, hash, narinfo, root_node)
    }

    /// Store narinfo metadata using an existing connection (for use inside
    /// transactions).
    fn store_metadata_with_conn(
        conn: &Connection,
        hash: &str,
        narinfo: &NarInfo,
        root_node: &CaNode,
    ) -> color_eyre::Result<()> {
        let root_bytes = root_node.encode_to_vec();
        let refs = narinfo
            .references
            .iter()
            .map(|r| r.rsplit('/').next().unwrap_or(r.as_str()))
            .collect::<Vec<_>>()
            .join(" ");
        let sigs = narinfo.signatures.join("\n");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        conn.execute(
            "INSERT OR REPLACE INTO cas_paths (hash, store_path, nar_hash, nar_size, root_node, \
             references_, signatures, ca, deriver, added_at, last_access) VALUES (?1, ?2, ?3, ?4, \
             ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                hash,
                narinfo.store_path,
                narinfo.nar_hash,
                narinfo.nar_size as i64,
                root_bytes,
                refs,
                sigs,
                narinfo.ca.as_deref().unwrap_or(""),
                narinfo.deriver.as_deref().unwrap_or(""),
                now as i64,
                now as i64,
            ],
        )?;

        Ok(())
    }

    /// Load narinfo metadata from SQLite.
    fn load_metadata(&self, hash: &str) -> color_eyre::Result<Option<NarInfo>> {
        let db = self.db.lock().expect("db lock");
        Self::load_metadata_with_conn(&db, hash)
    }

    /// Load narinfo metadata using an existing connection (for use inside
    /// transactions).
    fn load_metadata_with_conn(
        conn: &Connection,
        hash: &str,
    ) -> color_eyre::Result<Option<NarInfo>> {
        let mut stmt = conn.prepare(
            "SELECT store_path, nar_hash, nar_size, references_, signatures, ca, deriver FROM \
             cas_paths WHERE hash = ?1",
        )?;

        let result = stmt
            .query_row(params![hash], |row| {
                let store_path: String = row.get(0)?;
                let nar_hash: String = row.get(1)?;
                let nar_size: i64 = row.get(2)?;
                let refs_str: String = row.get(3)?;
                let sigs_str: String = row.get(4)?;
                let ca: String = row.get(5)?;
                let deriver: String = row.get(6)?;
                Ok((
                    store_path, nar_hash, nar_size, refs_str, sigs_str, ca, deriver,
                ))
            })
            .ok();

        let Some((store_path, nar_hash, nar_size, refs_str, sigs_str, ca, deriver)) = result else {
            return Ok(None);
        };

        let references: Vec<String> = if refs_str.is_empty() {
            Vec::new()
        } else {
            refs_str.split(' ').map(String::from).collect()
        };

        let signatures: Vec<String> = if sigs_str.is_empty() {
            Vec::new()
        } else {
            sigs_str.split('\n').map(String::from).collect()
        };

        Ok(Some(NarInfo {
            store_path,
            url: format!("nar/{hash}.nar"),
            compression: "none".to_owned(),
            file_hash: String::new(),
            file_size: 0,
            nar_hash,
            nar_size: nar_size as u64,
            references,
            deriver: if deriver.is_empty() {
                None
            } else {
                Some(deriver)
            },
            signatures,
            ca: if ca.is_empty() { None } else { Some(ca) },
        }))
    }
}

impl StorageBackend for CastoreBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn has_narinfo(&self, hash: &str) -> color_eyre::Result<bool> {
        let db = self.db.lock().expect("db lock");
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM cas_paths WHERE hash = ?1)",
            params![hash],
            |row| row.get(0),
        )?;
        Ok(exists)
    }

    fn get_narinfo(&self, hash: &str) -> color_eyre::Result<Option<NarInfo>> {
        self.load_metadata(hash)
    }

    fn get_narinfo_text(&self, hash: &str) -> color_eyre::Result<Option<String>> {
        Ok(self.load_metadata(hash)?.map(|ni| ni.to_narinfo_string()))
    }

    fn get_nar(&self, file_path: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        // Extract hash from NAR file path (e.g., "nar/abc123.nar").
        let filename = file_path.rsplit('/').next().unwrap_or(file_path);
        let hash = filename.split('.').next().unwrap_or(filename);

        let Some(root_node) = self.get_root_node(hash)? else {
            return Ok(None);
        };

        // Stream the NAR directly from CAS chunks — only one file's data is
        // in memory at a time.
        let mut buf = Vec::new();
        write_nar_streaming(&mut buf, &root_node, self)?;
        Ok(Some(buf))
    }

    fn put_narinfo(&self, hash: &str, content: &str) -> color_eyre::Result<bool> {
        let narinfo = NarInfo::parse(content)
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid narinfo content"))?;

        // Check if we already have CAS data for this path. If so, just update metadata.
        let db = self.db.lock().expect("db lock");
        let has_root: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM cas_paths WHERE hash = ?1)",
            params![hash],
            |row| row.get(0),
        )?;
        drop(db);

        if has_root {
            // Update metadata only — CAS data already exists (from put_nar).
            if let Some(root_node) = self.get_root_node(hash)? {
                self.store_metadata(hash, &narinfo, &root_node)?;
            }
        } else {
            // No CAS data yet — store metadata with an empty root node.
            // The NAR will be ingested when put_nar is called.
            // For now, create a placeholder.
            let placeholder = CaNode { node: None };
            self.store_metadata(hash, &narinfo, &placeholder)?;
        }

        Ok(true)
    }

    fn put_nar(&self, file_path: &str, data: &[u8]) -> color_eyre::Result<bool> {
        let filename = file_path.rsplit('/').next().unwrap_or(file_path);
        let hash = filename.split('.').next().unwrap_or(filename);

        // Parse the NAR.
        let nar_node = parse_nar(data)?;

        // Ingest into CAS within a single transaction.
        // Chunk files are written to disk outside the transaction (idempotent).
        // All DB rows (chunks, file_chunks, directories, cas_paths) are written
        // atomically — a crash mid-ingest leaves only orphan chunk files on disk,
        // not inconsistent DB state.
        let db = self.db.lock().expect("db lock");
        let tx = db.unchecked_transaction()?;
        let root_node = self.ingest_node(&tx, &nar_node)?;

        // If metadata already exists (put_narinfo was called first), update it
        // with the actual root node. Otherwise, store minimal metadata.
        if let Some(existing) = Self::load_metadata_with_conn(&tx, hash)? {
            Self::store_metadata_with_conn(&tx, hash, &existing, &root_node)?;
        } else {
            // Compute nar_hash and nar_size from the data.
            let nar_hash = sha256_hash(data);
            let placeholder_info = NarInfo {
                store_path: String::new(),
                url: format!("nar/{hash}.nar"),
                compression: "none".to_owned(),
                file_hash: format!("sha256:{nar_hash}"),
                file_size: data.len() as u64,
                nar_hash: format!("sha256:{nar_hash}"),
                nar_size: data.len() as u64,
                references: Vec::new(),
                deriver: None,
                signatures: Vec::new(),
                ca: None,
            };
            Self::store_metadata_with_conn(&tx, hash, &placeholder_info, &root_node)?;
        }

        tx.commit()?;
        Ok(true)
    }

    fn supports_cas(&self) -> bool {
        true
    }

    fn get_chunk(&self, digest: &[u8]) -> color_eyre::Result<Option<Vec<u8>>> {
        let d: [u8; 32] = digest
            .try_into()
            .map_err(|_| color_eyre::eyre::eyre!("invalid digest length"))?;
        self.get_chunk_by_digest(&d)
    }

    fn get_cas_root(&self, hash: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        Ok(self.get_root_node(hash)?.map(|n| n.encode_to_vec()))
    }
}

impl ChunkReader for CastoreBackend {
    fn read_file_data(&self, file_digest: &[u8; 32]) -> color_eyre::Result<Vec<u8>> {
        self.read_file_data(file_digest)
    }

    fn load_directory(
        &self,
        digest: &[u8; 32],
    ) -> color_eyre::Result<ekapkgs_protocol::ekapkgs::v1::CaDirectory> {
        self.load_directory(digest)
    }
}

/// Delegate all `StorageBackend` methods to the inner `CastoreBackend` so
/// an `Arc<CastoreBackend>` can be stored in `Box<dyn StorageBackend>` while
/// the GC loop holds a clone of the same `Arc`.
impl StorageBackend for std::sync::Arc<CastoreBackend> {
    fn as_any(&self) -> &dyn std::any::Any {
        // Return the inner CastoreBackend so downcast_ref works.
        &**self
    }

    fn has_narinfo(&self, hash: &str) -> color_eyre::Result<bool> {
        (**self).has_narinfo(hash)
    }

    fn get_narinfo(&self, hash: &str) -> color_eyre::Result<Option<NarInfo>> {
        (**self).get_narinfo(hash)
    }

    fn get_narinfo_text(&self, hash: &str) -> color_eyre::Result<Option<String>> {
        (**self).get_narinfo_text(hash)
    }

    fn get_nar(&self, file_path: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        (**self).get_nar(file_path)
    }

    fn put_narinfo(&self, hash: &str, content: &str) -> color_eyre::Result<bool> {
        (**self).put_narinfo(hash, content)
    }

    fn put_nar(&self, file_path: &str, data: &[u8]) -> color_eyre::Result<bool> {
        (**self).put_nar(file_path, data)
    }

    fn supports_cas(&self) -> bool {
        (**self).supports_cas()
    }

    fn get_chunk(&self, digest: &[u8]) -> color_eyre::Result<Option<Vec<u8>>> {
        (**self).get_chunk(digest)
    }

    fn get_cas_root(&self, hash: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        (**self).get_cas_root(hash)
    }
}

// --- Database helpers ---

fn open_db(path: &Path) -> color_eyre::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
    )?;
    Ok(conn)
}

fn create_tables(conn: &Connection) -> color_eyre::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS cas_paths (
            hash         TEXT PRIMARY KEY,
            store_path   TEXT NOT NULL,
            nar_hash     TEXT NOT NULL,
            nar_size     INTEGER NOT NULL,
            root_node    BLOB NOT NULL,
            references_  TEXT NOT NULL DEFAULT '',
            signatures   TEXT NOT NULL DEFAULT '',
            ca           TEXT NOT NULL DEFAULT '',
            deriver      TEXT NOT NULL DEFAULT '',
            added_at     INTEGER NOT NULL,
            last_access  INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS chunks (
            digest       BLOB PRIMARY KEY,
            size         INTEGER NOT NULL,
            ref_count    INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS file_chunks (
            file_digest  BLOB NOT NULL,
            chunk_index  INTEGER NOT NULL,
            chunk_digest BLOB NOT NULL,
            chunk_size   INTEGER NOT NULL,
            PRIMARY KEY (file_digest, chunk_index)
        );
        CREATE TABLE IF NOT EXISTS directories (
            digest       BLOB PRIMARY KEY,
            data         BLOB NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_file_chunks_digest ON file_chunks(chunk_digest);
        CREATE INDEX IF NOT EXISTS idx_cas_paths_last_access ON cas_paths(last_access);",
    )?;

    // Migration: add last_access column if missing (existing DBs).
    let has_col: bool = conn
        .prepare("SELECT last_access FROM cas_paths LIMIT 0")
        .is_ok();
    if !has_col {
        conn.execute_batch(
            "ALTER TABLE cas_paths ADD COLUMN last_access INTEGER NOT NULL DEFAULT 0;
             CREATE INDEX IF NOT EXISTS idx_cas_paths_last_access ON cas_paths(last_access);",
        )?;
    }

    Ok(())
}

/// Migrate on-disk `.dir` files into the SQLite `directories` table,
/// then remove the `dirs/` directory.
fn migrate_dirs_to_db(conn: &Connection, dirs_path: &Path) -> color_eyre::Result<()> {
    let entries: Vec<_> = std::fs::read_dir(dirs_path)?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "dir"))
        .collect();

    if entries.is_empty() {
        let _ = std::fs::remove_dir(dirs_path);
        return Ok(());
    }

    tracing::info!(
        "Migrating {} directory files from disk to SQLite",
        entries.len()
    );

    let tx = conn.unchecked_transaction()?;
    for entry in &entries {
        let data = std::fs::read(entry.path())?;
        let digest = blake3::hash(&data);
        tx.execute(
            "INSERT OR IGNORE INTO directories (digest, data) VALUES (?1, ?2)",
            params![digest.as_bytes().as_slice(), data],
        )?;
    }
    tx.commit()?;

    // Remove migrated files and the directory.
    for entry in &entries {
        let _ = std::fs::remove_file(entry.path());
    }
    let _ = std::fs::remove_dir(dirs_path);

    Ok(())
}

// --- Utility functions ---

fn hex_encode(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hash(data: &[u8]) -> String {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(data);
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use ekapkgs_nix::nar::write_nar;

    use super::*;

    fn setup_backend() -> (tempfile::TempDir, CastoreBackend) {
        let dir = tempfile::TempDir::new().unwrap();
        let backend = CastoreBackend::new(dir.path().to_path_buf()).unwrap();
        (dir, backend)
    }

    #[test]
    fn put_and_get_nar_roundtrip() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Directory {
            entries: vec![
                NarDirectoryEntry {
                    name: "hello.txt".to_string(),
                    node: NarNode::Regular {
                        executable: false,
                        data: b"Hello, world!".to_vec(),
                    },
                },
                NarDirectoryEntry {
                    name: "script.sh".to_string(),
                    node: NarNode::Regular {
                        executable: true,
                        data: b"#!/bin/sh\necho hi\n".to_vec(),
                    },
                },
            ],
        };
        let original_nar = write_nar(&node);

        // Store.
        backend.put_nar("nar/abc123.nar", &original_nar).unwrap();

        // Retrieve.
        let retrieved = backend.get_nar("nar/abc123.nar").unwrap().unwrap();
        assert_eq!(original_nar, retrieved);
    }

    #[test]
    fn narinfo_metadata_roundtrip() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Regular {
            executable: false,
            data: b"test data".to_vec(),
        };
        let nar_data = write_nar(&node);

        // Store NAR first.
        backend.put_nar("nar/meta123.nar", &nar_data).unwrap();

        // Store narinfo.
        let narinfo = "StorePath: /nix/store/meta123-test-1.0\nURL: nar/meta123.nar\nCompression: \
                       none\nNarHash: sha256:deadbeef\nNarSize: 100\nReferences: meta123-test-1.0 \
                       dep456-lib-1.0\nSig: key1:sig1==\n";
        backend.put_narinfo("meta123", narinfo).unwrap();

        // Retrieve narinfo.
        let ni = backend.get_narinfo("meta123").unwrap().unwrap();
        assert_eq!(ni.store_path, "/nix/store/meta123-test-1.0");
        assert_eq!(ni.nar_hash, "sha256:deadbeef");
        assert_eq!(ni.references.len(), 2);
        assert!(ni.signatures.contains(&"key1:sig1==".to_string()));

        // has_narinfo.
        assert!(backend.has_narinfo("meta123").unwrap());
        assert!(!backend.has_narinfo("nonexistent").unwrap());
    }

    #[test]
    fn chunk_deduplication() {
        let (_dir, backend) = setup_backend();

        // Two NARs with the same file content.
        let shared_data = b"shared content between packages".to_vec();

        let node1 = NarNode::Regular {
            executable: false,
            data: shared_data.clone(),
        };
        let node2 = NarNode::Regular {
            executable: false,
            data: shared_data,
        };

        let nar1 = write_nar(&node1);
        let nar2 = write_nar(&node2);

        backend.put_nar("nar/pkg1.nar", &nar1).unwrap();
        backend.put_nar("nar/pkg2.nar", &nar2).unwrap();

        // Both should have the same root node digest since they're identical.
        let root1 = backend.get_root_node("pkg1").unwrap().unwrap();
        let root2 = backend.get_root_node("pkg2").unwrap().unwrap();
        assert_eq!(root1, root2);

        // Count chunk files — should be 1 since the data is identical.
        let chunk_count = count_chunk_files(&backend.root);
        assert_eq!(chunk_count, 1);
    }

    #[test]
    fn get_chunk_by_digest_works() {
        let (_dir, backend) = setup_backend();

        let data = b"chunk test data";
        let digest = backend.store_chunk(data).unwrap();

        let retrieved = backend.get_chunk_by_digest(&digest).unwrap().unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn get_missing_chunk_returns_none() {
        let (_dir, backend) = setup_backend();

        let missing = [0u8; 32];
        assert!(backend.get_chunk_by_digest(&missing).unwrap().is_none());
    }

    #[test]
    fn symlink_roundtrip() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Symlink {
            target: "/nix/store/abc123-target".to_string(),
        };
        let nar_data = write_nar(&node);

        backend.put_nar("nar/sym123.nar", &nar_data).unwrap();
        let retrieved = backend.get_nar("nar/sym123.nar").unwrap().unwrap();
        assert_eq!(nar_data, retrieved);
    }

    #[test]
    fn nested_directory_roundtrip() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Directory {
            entries: vec![
                NarDirectoryEntry {
                    name: "bin".to_string(),
                    node: NarNode::Directory {
                        entries: vec![NarDirectoryEntry {
                            name: "hello".to_string(),
                            node: NarNode::Regular {
                                executable: true,
                                data: b"ELF binary data here".to_vec(),
                            },
                        }],
                    },
                },
                NarDirectoryEntry {
                    name: "lib".to_string(),
                    node: NarNode::Directory {
                        entries: vec![NarDirectoryEntry {
                            name: "libfoo.so".to_string(),
                            node: NarNode::Regular {
                                executable: false,
                                data: b"shared library data".to_vec(),
                            },
                        }],
                    },
                },
                NarDirectoryEntry {
                    name: "share".to_string(),
                    node: NarNode::Symlink {
                        target: "../lib".to_string(),
                    },
                },
            ],
        };
        let nar_data = write_nar(&node);

        backend.put_nar("nar/nested123.nar", &nar_data).unwrap();
        let retrieved = backend.get_nar("nar/nested123.nar").unwrap().unwrap();
        assert_eq!(nar_data, retrieved);
    }

    #[test]
    fn walk_cas_trees_works() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Regular {
            executable: false,
            data: b"some file content".to_vec(),
        };
        let nar_data = write_nar(&node);
        backend.put_nar("nar/walk123.nar", &nar_data).unwrap();

        // With empty have set, all chunks should be missing.
        let empty = ChunkHaveCheck::Exact(HashSet::new());
        let (missing, dirs, file_maps) = backend.walk_cas_trees(&["walk123"], &empty).unwrap();
        assert!(!missing.is_empty());
        // A single file has no directories.
        assert!(dirs.is_empty());
        // Should have one file-to-chunk mapping.
        assert_eq!(file_maps.len(), 1);

        // With all chunks in have set, nothing should be missing.
        let have: HashSet<[u8; 32]> = missing
            .iter()
            .filter_map(|cm| {
                cm.digest
                    .as_ref()
                    .and_then(|d| d.digest.as_slice().try_into().ok())
            })
            .collect();
        let have_check = ChunkHaveCheck::Exact(have);
        let (missing2, ..) = backend.walk_cas_trees(&["walk123"], &have_check).unwrap();
        assert!(missing2.is_empty());
    }

    #[test]
    fn supports_cas_returns_true() {
        let (_dir, backend) = setup_backend();
        assert!(backend.supports_cas());
    }

    fn count_chunk_files(root: &Path) -> usize {
        let chunks_dir = root.join("chunks");
        if !chunks_dir.exists() {
            return 0;
        }
        walkdir(&chunks_dir)
    }

    fn walkdir(path: &Path) -> usize {
        let mut count = 0;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    count += walkdir(&path);
                } else if path.extension().is_some_and(|e| e == "chunk") {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn evict_path_removes_exclusive_chunks() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Regular {
            executable: false,
            data: b"unique data for eviction test".to_vec(),
        };
        let nar_data = write_nar(&node);
        backend.put_nar("nar/evict1.nar", &nar_data).unwrap();

        assert!(backend.has_narinfo("evict1").unwrap());
        assert!(count_chunk_files(&backend.root) > 0);

        let freed = backend.evict_path("evict1").unwrap();
        assert!(freed > 0);
        assert!(!backend.has_narinfo("evict1").unwrap());
        assert_eq!(count_chunk_files(&backend.root), 0);
    }

    #[test]
    fn evict_path_preserves_shared_chunks() {
        let (_dir, backend) = setup_backend();

        let shared_data = b"shared data for eviction test".to_vec();

        let node1 = NarNode::Regular {
            executable: false,
            data: shared_data.clone(),
        };
        let node2 = NarNode::Regular {
            executable: false,
            data: shared_data,
        };

        let nar1 = write_nar(&node1);
        let nar2 = write_nar(&node2);

        backend.put_nar("nar/shared1.nar", &nar1).unwrap();
        backend.put_nar("nar/shared2.nar", &nar2).unwrap();

        let chunks_before = count_chunk_files(&backend.root);
        assert!(chunks_before > 0);

        // Evict one — shared chunk should remain.
        let freed = backend.evict_path("shared1").unwrap();
        assert_eq!(freed, 0); // Chunk still referenced by shared2.
        assert!(!backend.has_narinfo("shared1").unwrap());
        assert!(backend.has_narinfo("shared2").unwrap());
        assert_eq!(count_chunk_files(&backend.root), chunks_before);

        // Evict the other — now the chunk is orphaned.
        let freed = backend.evict_path("shared2").unwrap();
        assert!(freed > 0);
        assert_eq!(count_chunk_files(&backend.root), 0);
    }

    #[test]
    fn evict_path_handles_directory_nar() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Directory {
            entries: vec![
                NarDirectoryEntry {
                    name: "a.txt".to_string(),
                    node: NarNode::Regular {
                        executable: false,
                        data: b"file a content".to_vec(),
                    },
                },
                NarDirectoryEntry {
                    name: "b.txt".to_string(),
                    node: NarNode::Regular {
                        executable: true,
                        data: b"file b content".to_vec(),
                    },
                },
            ],
        };
        let nar_data = write_nar(&node);
        backend.put_nar("nar/dir1.nar", &nar_data).unwrap();

        assert!(backend.has_narinfo("dir1").unwrap());
        let chunks_before = count_chunk_files(&backend.root);
        assert_eq!(chunks_before, 2); // Two files = two chunks.

        let freed = backend.evict_path("dir1").unwrap();
        assert!(freed > 0);
        assert!(!backend.has_narinfo("dir1").unwrap());
        assert_eq!(count_chunk_files(&backend.root), 0);
    }

    #[test]
    fn evict_nonexistent_path_returns_zero() {
        let (_dir, backend) = setup_backend();
        let freed = backend.evict_path("nonexistent").unwrap();
        assert_eq!(freed, 0);
    }

    #[test]
    fn update_access_sets_last_access() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Regular {
            executable: false,
            data: b"access test".to_vec(),
        };
        let nar_data = write_nar(&node);
        backend.put_nar("nar/acc1.nar", &nar_data).unwrap();

        backend.update_access(&["acc1".to_owned()]);

        let db = backend.db.lock().expect("db lock");
        let last_access: i64 = db
            .query_row(
                "SELECT last_access FROM cas_paths WHERE hash = 'acc1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(last_access > 0);
    }

    #[test]
    fn total_chunk_bytes_and_paths() {
        let (_dir, backend) = setup_backend();

        assert_eq!(backend.total_chunk_bytes().unwrap(), 0);
        assert_eq!(backend.total_paths().unwrap(), 0);

        let node = NarNode::Regular {
            executable: false,
            data: b"metrics test data".to_vec(),
        };
        let nar_data = write_nar(&node);
        backend.put_nar("nar/met1.nar", &nar_data).unwrap();

        assert!(backend.total_chunk_bytes().unwrap() > 0);
        assert_eq!(backend.total_paths().unwrap(), 1);
    }

    #[test]
    fn paths_by_access_asc_returns_ordered() {
        let (_dir, backend) = setup_backend();

        let node1 = NarNode::Regular {
            executable: false,
            data: b"path one".to_vec(),
        };
        let node2 = NarNode::Regular {
            executable: false,
            data: b"path two".to_vec(),
        };

        backend.put_nar("nar/ord1.nar", &write_nar(&node1)).unwrap();
        backend.put_nar("nar/ord2.nar", &write_nar(&node2)).unwrap();

        // Update access for ord2 so it's more recent.
        backend.update_access(&["ord2".to_owned()]);

        let paths = backend.paths_by_access_asc().unwrap();
        assert_eq!(paths.len(), 2);
        // ord1 should come first (older access).
        assert_eq!(paths[0].0, "ord1");
        assert_eq!(paths[1].0, "ord2");
    }
}
