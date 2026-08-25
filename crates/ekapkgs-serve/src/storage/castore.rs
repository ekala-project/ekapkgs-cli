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
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ekapkgs_protocol::ekapkgs::v1::{
    B3Digest, CaDirectory, CaDirectoryEntry, CaDirectoryNode, CaFileNode, CaNode, CaSymlinkNode,
    ChunkMeta,
};
use prost::Message;
use rusqlite::{Connection, params};

use super::nar::{NarDirectoryEntry, NarNode, parse_nar, write_nar};
use super::{NarInfo, StorageBackend};

/// FastCDC chunking parameters.
const CHUNK_MIN: u32 = 16 * 1024; // 16 KiB
const CHUNK_AVG: u32 = 64 * 1024; // 64 KiB
const CHUNK_MAX: u32 = 256 * 1024; // 256 KiB

pub struct CastoreBackend {
    root: PathBuf,
    db: Mutex<Connection>,
}

impl CastoreBackend {
    /// Create a new CAS backend rooted at the given directory.
    pub fn new(root: PathBuf) -> color_eyre::Result<Self> {
        std::fs::create_dir_all(root.join("chunks"))?;
        std::fs::create_dir_all(root.join("dirs"))?;

        let db_path = root.join("castore.db");
        let conn = open_db(&db_path)?;
        create_tables(&conn)?;

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

    /// Walk the Merkle trees for the requested store paths and return chunks
    /// that the client does not already have.
    pub fn walk_missing_chunks(
        &self,
        want_hashes: &[&str],
        have_digests: &HashSet<[u8; 32]>,
    ) -> color_eyre::Result<Vec<ChunkMeta>> {
        let mut missing = Vec::new();
        let mut seen = HashSet::new();

        for hash in want_hashes {
            if let Some(root) = self.get_root_node(hash)? {
                self.collect_missing_chunks(&root, have_digests, &mut seen, &mut missing)?;
            }
        }

        Ok(missing)
    }

    /// Store a chunk from an external upload, returning its digest.
    pub fn store_chunk_external(&self, data: &[u8]) -> color_eyre::Result<[u8; 32]> {
        self.store_chunk(data)
    }

    // --- Private helpers ---

    fn chunk_path(&self, hex: &str) -> PathBuf {
        let prefix = &hex[..4.min(hex.len())];
        self.root
            .join("chunks")
            .join(prefix)
            .join(format!("{hex}.chunk"))
    }

    fn dir_path(&self, hex: &str) -> PathBuf {
        self.root.join("dirs").join(format!("{hex}.dir"))
    }

    /// Store a chunk, returning its blake3 digest.
    fn store_chunk(&self, data: &[u8]) -> color_eyre::Result<[u8; 32]> {
        let hash = blake3::hash(data);
        let digest = *hash.as_bytes();
        let hex = hex_encode(&digest);
        let path = self.chunk_path(&hex);

        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, data)?;

            // Track in the chunks table.
            let db = self.db.lock().expect("db lock");
            db.execute(
                "INSERT OR IGNORE INTO chunks (digest, size, ref_count) VALUES (?1, ?2, 1)",
                params![digest.as_slice(), data.len() as i64],
            )?;
        }

        Ok(digest)
    }

    /// Store a serialized CaDirectory, returning its blake3 digest.
    fn store_directory(&self, dir: &CaDirectory) -> color_eyre::Result<[u8; 32]> {
        let encoded = dir.encode_to_vec();
        let hash = blake3::hash(&encoded);
        let digest = *hash.as_bytes();
        let hex = hex_encode(&digest);
        let path = self.dir_path(&hex);

        if !path.exists() {
            std::fs::write(&path, &encoded)?;
        }

        Ok(digest)
    }

    /// Load a CaDirectory from disk by its digest.
    fn load_directory(&self, digest: &[u8; 32]) -> color_eyre::Result<CaDirectory> {
        let hex = hex_encode(digest);
        let path = self.dir_path(&hex);
        let data = std::fs::read(&path)?;
        Ok(CaDirectory::decode(data.as_slice())?)
    }

    /// Ingest a NarNode tree into CAS, returning the root CaNode.
    fn ingest_node(&self, node: &NarNode) -> color_eyre::Result<CaNode> {
        match node {
            NarNode::Regular { executable, data } => {
                // Chunk the file data using FastCDC.
                let chunks = fastcdc::v2020::FastCDC::new(data, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX);
                let mut chunk_digests = Vec::new();
                let mut total_size = 0u64;

                for chunk in chunks {
                    let chunk_data = &data[chunk.offset..chunk.offset + chunk.length];
                    let digest = self.store_chunk(chunk_data)?;
                    chunk_digests.push((digest, chunk.length as u64));
                    total_size += chunk.length as u64;
                }

                // If the file is small enough to be a single chunk, the file digest
                // IS the chunk digest. For multi-chunk files, we store a "chunk list"
                // blob that maps the file digest to its constituent chunks.
                //
                // For simplicity, we always store the whole file blob as well and use
                // its blake3 hash as the file digest. The chunk-level dedup still works
                // because individual chunks are stored once.
                let file_hash = blake3::hash(data);
                let file_digest = *file_hash.as_bytes();

                // Store chunk metadata in the DB for this file.
                if chunk_digests.len() > 1 {
                    let db = self.db.lock().expect("db lock");
                    for (i, (chunk_digest, chunk_size)) in chunk_digests.iter().enumerate() {
                        db.execute(
                            "INSERT OR IGNORE INTO file_chunks (file_digest, chunk_index, \
                             chunk_digest, chunk_size) VALUES (?1, ?2, ?3, ?4)",
                            params![
                                file_digest.as_slice(),
                                i as i64,
                                chunk_digest.as_slice(),
                                *chunk_size as i64
                            ],
                        )?;
                    }
                } else if chunk_digests.len() == 1 {
                    // Single chunk — file digest may differ from chunk digest if
                    // the file size equals the chunk (common case). Store mapping anyway.
                    let db = self.db.lock().expect("db lock");
                    let (chunk_digest, chunk_size) = &chunk_digests[0];
                    db.execute(
                        "INSERT OR IGNORE INTO file_chunks (file_digest, chunk_index, \
                         chunk_digest, chunk_size) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            file_digest.as_slice(),
                            0i64,
                            chunk_digest.as_slice(),
                            *chunk_size as i64
                        ],
                    )?;
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
                    let child_node = self.ingest_node(&entry.node)?;
                    dir_size += 1;
                    ca_entries.push(CaDirectoryEntry {
                        name: entry.name.clone(),
                        node: Some(child_node),
                    });
                }

                let ca_dir = CaDirectory {
                    entries: ca_entries,
                };
                let dir_digest = self.store_directory(&ca_dir)?;

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

    /// Recursively collect chunk digests that the client is missing.
    fn collect_missing_chunks(
        &self,
        ca_node: &CaNode,
        have: &HashSet<[u8; 32]>,
        seen: &mut HashSet<[u8; 32]>,
        missing: &mut Vec<ChunkMeta>,
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

                // Get all chunks for this file.
                let db = self.db.lock().expect("db lock");
                let mut stmt = db.prepare(
                    "SELECT chunk_digest, chunk_size FROM file_chunks WHERE file_digest = ?1 \
                     ORDER BY chunk_index",
                )?;
                let chunks: Vec<(Vec<u8>, i64)> = stmt
                    .query_map(params![file_digest.as_slice()], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })?
                    .collect::<Result<_, _>>()?;
                drop(stmt);
                drop(db);

                for (chunk_digest_vec, chunk_size) in chunks {
                    if let Ok(chunk_digest) = <[u8; 32]>::try_from(chunk_digest_vec.as_slice()) {
                        if !have.contains(&chunk_digest) && seen.insert(chunk_digest) {
                            missing.push(ChunkMeta {
                                digest: Some(B3Digest {
                                    digest: chunk_digest.to_vec(),
                                }),
                                size: chunk_size as u64,
                            });
                        }
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

                if let Ok(ca_dir) = self.load_directory(&dir_digest) {
                    for entry in &ca_dir.entries {
                        if let Some(child) = &entry.node {
                            self.collect_missing_chunks(child, have, seen, missing)?;
                        }
                    }
                }
            },

            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(_) => {
                // Symlinks have no chunks.
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

        let db = self.db.lock().expect("db lock");
        db.execute(
            "INSERT OR REPLACE INTO cas_paths (hash, store_path, nar_hash, nar_size, root_node, \
             references_, signatures, ca, deriver, added_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, \
             ?8, ?9, ?10)",
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
            ],
        )?;

        Ok(())
    }

    /// Load narinfo metadata from SQLite.
    fn load_metadata(&self, hash: &str) -> color_eyre::Result<Option<NarInfo>> {
        let db = self.db.lock().expect("db lock");
        let mut stmt = db.prepare(
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

        // Reconstruct the NAR from the CAS tree.
        let nar_node = self.reconstruct_node(&root_node)?;
        Ok(Some(write_nar(&nar_node)))
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

        // Parse the NAR and ingest into CAS.
        let nar_node = parse_nar(data)?;
        let root_node = self.ingest_node(&nar_node)?;

        // If metadata already exists (put_narinfo was called first), update it
        // with the actual root node. Otherwise, store minimal metadata.
        if let Some(existing) = self.load_metadata(hash)? {
            self.store_metadata(hash, &existing, &root_node)?;
        } else {
            // Compute nar_hash and nar_size from the data.
            let nar_hash = sha256_hash(data);
            let placeholder_info = NarInfo {
                store_path: String::new(),
                url: format!("nar/{hash}.nar"),
                compression: "none".to_owned(),
                file_hash: String::new(),
                file_size: 0,
                nar_hash: format!("sha256:{nar_hash}"),
                nar_size: data.len() as u64,
                references: Vec::new(),
                deriver: None,
                signatures: Vec::new(),
                ca: None,
            };
            self.store_metadata(hash, &placeholder_info, &root_node)?;
        }

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

// --- Database helpers ---

fn open_db(path: &Path) -> color_eyre::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
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
            added_at     INTEGER NOT NULL
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
        CREATE INDEX IF NOT EXISTS idx_file_chunks_digest ON file_chunks(chunk_digest);",
    )?;
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
    fn walk_missing_chunks_works() {
        let (_dir, backend) = setup_backend();

        let node = NarNode::Regular {
            executable: false,
            data: b"some file content".to_vec(),
        };
        let nar_data = write_nar(&node);
        backend.put_nar("nar/walk123.nar", &nar_data).unwrap();

        // With empty have set, all chunks should be missing.
        let missing = backend
            .walk_missing_chunks(&["walk123"], &HashSet::new())
            .unwrap();
        assert!(!missing.is_empty());

        // With all chunks in have set, nothing should be missing.
        let have: HashSet<[u8; 32]> = missing
            .iter()
            .filter_map(|cm| {
                cm.digest
                    .as_ref()
                    .and_then(|d| d.digest.as_slice().try_into().ok())
            })
            .collect();
        let missing2 = backend.walk_missing_chunks(&["walk123"], &have).unwrap();
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
}
