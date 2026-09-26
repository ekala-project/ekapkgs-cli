//! Client-side content-addressed chunk store.
//!
//! Mirrors the server's castore backend layout. Persists downloaded chunks
//! and metadata so that future pulls can negotiate at the chunk level,
//! transferring only the data that actually changed between package versions.
//!
//! On-disk layout:
//!   ~/.cache/ekapkgs/castore/
//!     castore.db                         — SQLite metadata
//!     chunks/{hex[0..4]}/{hex}.chunk     — blob chunks

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ekapkgs_nix::nar::ChunkReader;
#[cfg(test)]
use ekapkgs_nix::nar::{NarDirectoryEntry, NarNode};
use ekapkgs_protocol::ekapkgs::v1::{CaDirectory, CaNode, ChunkNegotiateResponse};
use prost::Message;
use rusqlite::{Connection, params};

/// Return the default path for the client chunk store.
fn default_store_path() -> PathBuf {
    let cache_dir = directories::ProjectDirs::from("", "", "ekapkgs")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache/ekapkgs")
        });
    cache_dir.join("castore")
}

pub struct ChunkStore {
    root: PathBuf,
    db: Mutex<Connection>,
}

impl ChunkStore {
    /// Open or create a chunk store at the given root directory.
    pub fn open_at(root: PathBuf) -> color_eyre::Result<Self> {
        std::fs::create_dir_all(root.join("chunks"))?;

        let db_path = root.join("castore.db");
        let conn = open_db(&db_path)?;
        create_tables(&conn)?;

        Ok(Self {
            root,
            db: Mutex::new(conn),
        })
    }

    /// Open or create the chunk store at the default cache location.
    pub fn open() -> color_eyre::Result<Self> {
        Self::open_at(default_store_path())
    }

    /// Return all known chunk digests for the `have_chunks` negotiation field.
    pub fn all_chunk_digests(&self) -> color_eyre::Result<Vec<[u8; 32]>> {
        let db = self.db.lock().expect("db lock");
        let mut stmt = db.prepare("SELECT digest FROM chunks")?;
        let digests = stmt
            .query_map([], |row| {
                let v: Vec<u8> = row.get(0)?;
                Ok(v)
            })?
            .filter_map(|r| {
                let v = r.ok()?;
                <[u8; 32]>::try_from(v.as_slice()).ok()
            })
            .collect();
        Ok(digests)
    }

    /// Store a chunk on disk and record it in the database.
    pub fn store_chunk(&self, digest: &[u8; 32], data: &[u8]) -> color_eyre::Result<()> {
        let hex = hex_encode(digest);
        let path = self.chunk_path(&hex);

        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, data)?;

            let db = self.db.lock().expect("db lock");
            db.execute(
                "INSERT OR IGNORE INTO chunks (digest, size) VALUES (?1, ?2)",
                params![digest.as_slice(), data.len() as i64],
            )?;
        }

        Ok(())
    }

    /// Read a chunk by its blake3 digest.
    pub fn get_chunk(&self, digest: &[u8; 32]) -> color_eyre::Result<Option<Vec<u8>>> {
        let hex = hex_encode(digest);
        let path = self.chunk_path(&hex);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read(path)?))
    }

    /// Store a serialized CaDirectory in the database.
    pub fn store_directory(&self, digest: &[u8; 32], data: &[u8]) -> color_eyre::Result<()> {
        let db = self.db.lock().expect("db lock");
        db.execute(
            "INSERT OR IGNORE INTO directories (digest, data) VALUES (?1, ?2)",
            params![digest.as_slice(), data],
        )?;
        Ok(())
    }

    /// Load and decode a CaDirectory from the database.
    pub fn load_directory(&self, digest: &[u8; 32]) -> color_eyre::Result<CaDirectory> {
        let db = self.db.lock().expect("db lock");
        let data: Vec<u8> = db.query_row(
            "SELECT data FROM directories WHERE digest = ?1",
            params![digest.as_slice()],
            |row| row.get(0),
        )?;
        Ok(CaDirectory::decode(data.as_slice())?)
    }

    /// Store file-to-chunk mappings in the database.
    pub fn store_file_chunks(
        &self,
        file_digest: &[u8; 32],
        chunks: &[([u8; 32], u64)],
    ) -> color_eyre::Result<()> {
        let db = self.db.lock().expect("db lock");
        for (i, (chunk_digest, chunk_size)) in chunks.iter().enumerate() {
            db.execute(
                "INSERT OR IGNORE INTO file_chunks (file_digest, chunk_index, chunk_digest, \
                 chunk_size) VALUES (?1, ?2, ?3, ?4)",
                params![
                    file_digest.as_slice(),
                    i as i64,
                    chunk_digest.as_slice(),
                    *chunk_size as i64,
                ],
            )?;
        }
        Ok(())
    }

    /// Read a file's data by concatenating its chunks in order.
    pub fn read_file_data(&self, file_digest: &[u8; 32]) -> color_eyre::Result<Vec<u8>> {
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
            let chunk_data = self.get_chunk(&chunk_digest)?.ok_or_else(|| {
                color_eyre::eyre::eyre!("missing chunk {}", hex_encode(&chunk_digest))
            })?;
            data.extend_from_slice(&chunk_data);
        }

        Ok(data)
    }

    /// Reconstruct a `NarNode` tree from a `CaNode` root.
    ///
    /// Uses directory and file-chunk metadata stored in the local database,
    /// and chunk data from the local chunk store on disk.
    ///
    /// Note: production code now uses `write_nar_streaming()` with the
    /// `ChunkReader` impl instead. This method is retained for tests.
    #[cfg(test)]
    pub fn reconstruct_node(&self, ca_node: &CaNode) -> color_eyre::Result<NarNode> {
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

    /// Index all directory and file-chunk metadata from a `ChunkNegotiateResponse`.
    ///
    /// Call this after receiving the negotiate response and before downloading
    /// chunks, so that directory/file metadata is available for NAR reassembly.
    pub fn index_response(&self, response: &ChunkNegotiateResponse) -> color_eyre::Result<()> {
        // Store all directories.
        for dir_data in &response.directories {
            let Some(digest_msg) = &dir_data.digest else {
                continue;
            };
            let Ok(digest): Result<[u8; 32], _> = digest_msg.digest.as_slice().try_into() else {
                continue;
            };
            if let Some(dir) = &dir_data.directory {
                let encoded = dir.encode_to_vec();
                self.store_directory(&digest, &encoded)?;
            }
        }

        // Store all file-to-chunk mappings.
        for mapping in &response.file_chunk_mappings {
            let Some(file_digest_msg) = &mapping.file_digest else {
                continue;
            };
            let Ok(file_digest): Result<[u8; 32], _> = file_digest_msg.digest.as_slice().try_into()
            else {
                continue;
            };

            let chunks: Vec<([u8; 32], u64)> = mapping
                .chunks
                .iter()
                .filter_map(|cm| {
                    let d = cm.digest.as_ref()?;
                    let digest: [u8; 32] = d.digest.as_slice().try_into().ok()?;
                    Some((digest, cm.size))
                })
                .collect();

            if !chunks.is_empty() {
                self.store_file_chunks(&file_digest, &chunks)?;
            }
        }

        Ok(())
    }

    /// Record a CAS path mapping so its chunks are included in future
    /// `have_chunks` negotiation requests.
    pub fn record_cas_path(&self, hash: &str, root_node: &CaNode) -> color_eyre::Result<()> {
        let root_bytes = root_node.encode_to_vec();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let db = self.db.lock().expect("db lock");
        db.execute(
            "INSERT OR REPLACE INTO cas_paths (hash, root_node, added_at) VALUES (?1, ?2, ?3)",
            params![hash, root_bytes, now as i64],
        )?;
        Ok(())
    }

    /// Return total bytes stored in chunks.
    pub fn total_chunk_bytes(&self) -> color_eyre::Result<u64> {
        let db = self.db.lock().expect("db lock");
        let total: i64 = db.query_row("SELECT COALESCE(SUM(size), 0) FROM chunks", [], |row| {
            row.get(0)
        })?;
        Ok(total as u64)
    }

    /// Evict oldest CAS path entries until total chunk size is under
    /// `target_bytes`. Returns the number of bytes freed.
    pub fn evict_to_size(&self, target_bytes: u64) -> color_eyre::Result<u64> {
        let current = self.total_chunk_bytes()?;
        if current <= target_bytes {
            return Ok(0);
        }

        let db = self.db.lock().expect("db lock");

        // Load paths ordered by age (oldest first).
        let mut stmt = db.prepare("SELECT hash FROM cas_paths ORDER BY added_at ASC")?;
        let hashes: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .filter_map(std::result::Result::ok)
            .collect();
        drop(stmt);

        let mut total_freed: u64 = 0;
        let to_free = current - target_bytes;

        for hash in &hashes {
            if total_freed >= to_free {
                break;
            }

            let tx = db.unchecked_transaction()?;

            // Collect file digests for this path's root node.
            let root_bytes: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT root_node FROM cas_paths WHERE hash = ?1",
                    params![hash],
                    |row| row.get(0),
                )
                .ok();

            let Some(root_bytes) = root_bytes else {
                continue;
            };

            // Collect file digests from the tree.
            let mut file_digests = Vec::new();
            if let Ok(root_node) = CaNode::decode(root_bytes.as_slice()) {
                collect_file_digests(&root_node, &db, &mut file_digests);
            }

            // Delete the cas_paths row.
            tx.execute("DELETE FROM cas_paths WHERE hash = ?1", params![hash])?;

            // Collect chunk digests, then clean up orphans.
            let mut chunk_digests = Vec::new();
            for file_digest in &file_digests {
                let mut cstmt =
                    tx.prepare("SELECT chunk_digest FROM file_chunks WHERE file_digest = ?1")?;
                let chunks: Vec<Vec<u8>> = cstmt
                    .query_map(params![file_digest.as_slice()], |row| row.get(0))?
                    .filter_map(std::result::Result::ok)
                    .collect();
                drop(cstmt);
                for c in chunks {
                    if let Ok(d) = <[u8; 32]>::try_from(c.as_slice()) {
                        chunk_digests.push(d);
                    }
                }

                // Check if this file_digest is still referenced by another path.
                let still_used: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM cas_paths WHERE hash != ?1)",
                    params![hash],
                    |row| row.get(0),
                )?;
                if !still_used {
                    tx.execute(
                        "DELETE FROM file_chunks WHERE file_digest = ?1",
                        params![file_digest.as_slice()],
                    )?;
                }
            }

            // Delete orphaned chunks.
            for chunk_digest in &chunk_digests {
                let still_ref: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM file_chunks WHERE chunk_digest = ?1)",
                    params![chunk_digest.as_slice()],
                    |row| row.get(0),
                )?;
                if !still_ref {
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
                    if let Some(s) = size {
                        total_freed += s as u64;
                    }
                    let hex = hex_encode(chunk_digest);
                    let path = self.chunk_path(&hex);
                    let _ = std::fs::remove_file(path);
                }
            }

            tx.commit()?;
        }

        Ok(total_freed)
    }

    fn chunk_path(&self, hex: &str) -> PathBuf {
        let prefix = &hex[..4.min(hex.len())];
        self.root
            .join("chunks")
            .join(prefix)
            .join(format!("{hex}.chunk"))
    }
}

impl ChunkReader for ChunkStore {
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

// --- Database helpers ---

fn open_db(path: &Path) -> color_eyre::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
    Ok(conn)
}

fn create_tables(conn: &Connection) -> color_eyre::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chunks (
            digest       BLOB PRIMARY KEY,
            size         INTEGER NOT NULL
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
        CREATE TABLE IF NOT EXISTS cas_paths (
            hash         TEXT PRIMARY KEY,
            root_node    BLOB NOT NULL,
            added_at     INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_file_chunks_digest ON file_chunks(chunk_digest);",
    )?;
    Ok(())
}

fn hex_encode(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Recursively collect file digests from a CaNode tree.
fn collect_file_digests(ca_node: &CaNode, db: &Connection, out: &mut Vec<[u8; 32]>) {
    let Some(node) = ca_node.node.as_ref() else {
        return;
    };
    match node {
        ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(file) => {
            if let Some(digest) = &file.digest {
                if let Ok(d) = <[u8; 32]>::try_from(digest.digest.as_slice()) {
                    out.push(d);
                }
            }
        },
        ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Directory(dir) => {
            if let Some(digest) = &dir.digest {
                if let Ok(d) = <[u8; 32]>::try_from(digest.digest.as_slice()) {
                    let data: Option<Vec<u8>> = db
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
                                    collect_file_digests(child, db, out);
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

#[cfg(test)]
mod tests {
    use ekapkgs_nix::nar::write_nar;
    use ekapkgs_protocol::ekapkgs::v1::{
        B3Digest, CaDirectoryEntry, CaDirectoryNode, CaFileNode, CaSymlinkNode,
    };

    use super::*;

    fn setup() -> (tempfile::TempDir, ChunkStore) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = ChunkStore::open_at(dir.path().to_path_buf()).unwrap();
        (dir, store)
    }

    #[test]
    fn open_creates_db_and_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        ChunkStore::open_at(dir.path().to_path_buf()).unwrap();
        assert!(dir.path().join("castore.db").exists());
        assert!(dir.path().join("chunks").is_dir());
    }

    #[test]
    fn store_and_get_chunk() {
        let (_dir, store) = setup();
        let data = b"hello chunk data";
        let digest = *blake3::hash(data).as_bytes();

        store.store_chunk(&digest, data).unwrap();
        let retrieved = store.get_chunk(&digest).unwrap().unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn get_missing_chunk_returns_none() {
        let (_dir, store) = setup();
        let missing = [0u8; 32];
        assert!(store.get_chunk(&missing).unwrap().is_none());
    }

    #[test]
    fn all_chunk_digests() {
        let (_dir, store) = setup();

        let d1 = b"chunk one";
        let d2 = b"chunk two";
        let h1 = *blake3::hash(d1).as_bytes();
        let h2 = *blake3::hash(d2).as_bytes();

        store.store_chunk(&h1, d1).unwrap();
        store.store_chunk(&h2, d2).unwrap();

        let digests = store.all_chunk_digests().unwrap();
        assert_eq!(digests.len(), 2);
        assert!(digests.contains(&h1));
        assert!(digests.contains(&h2));
    }

    #[test]
    fn store_and_load_directory() {
        let (_dir, store) = setup();

        let ca_dir = CaDirectory {
            entries: vec![CaDirectoryEntry {
                name: "hello.txt".to_string(),
                node: Some(CaNode {
                    node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(
                        CaSymlinkNode {
                            target: "/tmp".to_string(),
                        },
                    )),
                }),
            }],
        };

        let encoded = ca_dir.encode_to_vec();
        let digest = *blake3::hash(&encoded).as_bytes();

        store.store_directory(&digest, &encoded).unwrap();
        let loaded = store.load_directory(&digest).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].name, "hello.txt");
    }

    #[test]
    fn file_chunks_roundtrip() {
        let (_dir, store) = setup();

        let chunk1 = b"first chunk data";
        let chunk2 = b"second chunk data";
        let h1 = *blake3::hash(chunk1).as_bytes();
        let h2 = *blake3::hash(chunk2).as_bytes();

        store.store_chunk(&h1, chunk1).unwrap();
        store.store_chunk(&h2, chunk2).unwrap();

        let file_data = [chunk1.as_slice(), chunk2.as_slice()].concat();
        let file_digest = *blake3::hash(&file_data).as_bytes();

        store
            .store_file_chunks(
                &file_digest,
                &[(h1, chunk1.len() as u64), (h2, chunk2.len() as u64)],
            )
            .unwrap();

        let reconstructed = store.read_file_data(&file_digest).unwrap();
        assert_eq!(reconstructed, file_data);
    }

    #[test]
    fn reconstruct_symlink() {
        let (_dir, store) = setup();

        let ca_node = CaNode {
            node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(
                CaSymlinkNode {
                    target: "/nix/store/abc123".to_string(),
                },
            )),
        };

        let nar_node = store.reconstruct_node(&ca_node).unwrap();
        assert_eq!(
            nar_node,
            NarNode::Symlink {
                target: "/nix/store/abc123".to_string(),
            }
        );
    }

    #[test]
    fn reconstruct_regular_file() {
        let (_dir, store) = setup();

        let file_content = b"hello world file content";
        let chunk_digest = *blake3::hash(file_content).as_bytes();
        let file_digest = chunk_digest; // single chunk = same digest

        store.store_chunk(&chunk_digest, file_content).unwrap();
        store
            .store_file_chunks(&file_digest, &[(chunk_digest, file_content.len() as u64)])
            .unwrap();

        let ca_node = CaNode {
            node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(
                CaFileNode {
                    digest: Some(B3Digest {
                        digest: file_digest.to_vec(),
                    }),
                    size: file_content.len() as u64,
                    executable: true,
                },
            )),
        };

        let nar_node = store.reconstruct_node(&ca_node).unwrap();
        assert_eq!(
            nar_node,
            NarNode::Regular {
                executable: true,
                data: file_content.to_vec(),
            }
        );
    }

    #[test]
    fn reconstruct_directory() {
        let (_dir, store) = setup();

        // Store file chunks.
        let file_data = b"test file";
        let chunk_digest = *blake3::hash(file_data).as_bytes();
        let file_digest = chunk_digest;
        store.store_chunk(&chunk_digest, file_data).unwrap();
        store
            .store_file_chunks(&file_digest, &[(chunk_digest, file_data.len() as u64)])
            .unwrap();

        // Build and store directory.
        let ca_dir = CaDirectory {
            entries: vec![
                CaDirectoryEntry {
                    name: "data.txt".to_string(),
                    node: Some(CaNode {
                        node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(
                            CaFileNode {
                                digest: Some(B3Digest {
                                    digest: file_digest.to_vec(),
                                }),
                                size: file_data.len() as u64,
                                executable: false,
                            },
                        )),
                    }),
                },
                CaDirectoryEntry {
                    name: "link".to_string(),
                    node: Some(CaNode {
                        node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(
                            CaSymlinkNode {
                                target: "data.txt".to_string(),
                            },
                        )),
                    }),
                },
            ],
        };

        let dir_encoded = ca_dir.encode_to_vec();
        let dir_digest = *blake3::hash(&dir_encoded).as_bytes();
        store.store_directory(&dir_digest, &dir_encoded).unwrap();

        // Build root CaNode.
        let root = CaNode {
            node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Directory(
                CaDirectoryNode {
                    digest: Some(B3Digest {
                        digest: dir_digest.to_vec(),
                    }),
                    size: 2,
                },
            )),
        };

        let nar_node = store.reconstruct_node(&root).unwrap();
        let nar_bytes = write_nar(&nar_node);

        // Verify the reconstructed NAR matches what we expect.
        let expected = NarNode::Directory {
            entries: vec![
                NarDirectoryEntry {
                    name: "data.txt".to_string(),
                    node: NarNode::Regular {
                        executable: false,
                        data: file_data.to_vec(),
                    },
                },
                NarDirectoryEntry {
                    name: "link".to_string(),
                    node: NarNode::Symlink {
                        target: "data.txt".to_string(),
                    },
                },
            ],
        };
        assert_eq!(nar_node, expected);

        // Also verify NAR bytes roundtrip.
        let parsed = ekapkgs_nix::nar::parse_nar(&nar_bytes).unwrap();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn record_and_recall_cas_path() {
        let (dir, store) = setup();

        let root = CaNode {
            node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(
                CaSymlinkNode {
                    target: "/test".to_string(),
                },
            )),
        };

        store.record_cas_path("abc123", &root).unwrap();

        // Reopen the store and verify the path persists.
        drop(store);
        let store2 = ChunkStore::open_at(dir.path().to_path_buf()).unwrap();
        let db = store2.db.lock().expect("db lock");
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM cas_paths WHERE hash = 'abc123'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn chunk_deduplication() {
        let (_dir, store) = setup();
        let data = b"shared chunk content";
        let digest = *blake3::hash(data).as_bytes();

        // Store twice — should only create one file.
        store.store_chunk(&digest, data).unwrap();
        store.store_chunk(&digest, data).unwrap();

        let digests = store.all_chunk_digests().unwrap();
        assert_eq!(digests.len(), 1);
    }
}
