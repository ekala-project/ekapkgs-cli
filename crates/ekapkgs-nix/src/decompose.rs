//! NAR decomposition into a CAS Merkle tree.
//!
//! Converts a `NarNode` tree into a `CaNode` tree with FastCDC-chunked file
//! data. This is the same algorithm used by the server's `CastoreBackend`,
//! factored out so the client can decompose NARs locally for push negotiation.
//!
//! Outputs chunk data and CAS metadata without writing to disk — the caller
//! is responsible for storage.

use ekapkgs_protocol::ekapkgs::v1::{
    B3Digest, CaDirectory, CaDirectoryEntry, CaDirectoryNode, CaFileNode, CaNode, CaSymlinkNode,
    ChunkMeta, FileChunkMapping,
};
use prost::Message;

use crate::nar::NarNode;

/// FastCDC chunking parameters (must match the server).
const CHUNK_MIN: u32 = 16 * 1024; // 16 KiB
const CHUNK_AVG: u32 = 64 * 1024; // 64 KiB
const CHUNK_MAX: u32 = 256 * 1024; // 256 KiB

/// A chunk produced during decomposition: its blake3 digest and raw data.
pub struct Chunk {
    pub digest: [u8; 32],
    pub data: Vec<u8>,
}

/// Result of decomposing a NAR into CAS format.
pub struct DecomposeResult {
    /// The CAS root node.
    pub root_node: CaNode,
    /// All chunks (deduplicated by digest).
    pub chunks: Vec<Chunk>,
    /// All directories in the tree (digest, encoded protobuf).
    pub directories: Vec<([u8; 32], CaDirectory)>,
    /// File-to-chunk mappings.
    pub file_chunk_mappings: Vec<FileChunkMapping>,
}

/// Decompose a `NarNode` tree into CAS format.
pub fn decompose_nar(node: &NarNode) -> DecomposeResult {
    let mut chunks = Vec::new();
    let mut chunk_seen = std::collections::HashSet::new();
    let mut directories = Vec::new();
    let mut file_chunk_mappings = Vec::new();

    let root_node = decompose_node(
        node,
        &mut chunks,
        &mut chunk_seen,
        &mut directories,
        &mut file_chunk_mappings,
    );

    DecomposeResult {
        root_node,
        chunks,
        directories,
        file_chunk_mappings,
    }
}

fn decompose_node(
    node: &NarNode,
    chunks: &mut Vec<Chunk>,
    chunk_seen: &mut std::collections::HashSet<[u8; 32]>,
    directories: &mut Vec<([u8; 32], CaDirectory)>,
    file_chunk_mappings: &mut Vec<FileChunkMapping>,
) -> CaNode {
    match node {
        NarNode::Regular { executable, data } => {
            let cdc_chunks = fastcdc::v2020::FastCDC::new(data, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX);
            let mut chunk_metas = Vec::new();
            let mut total_size = 0u64;

            for chunk in cdc_chunks {
                let chunk_data = &data[chunk.offset..chunk.offset + chunk.length];
                let hash = blake3::hash(chunk_data);
                let digest = *hash.as_bytes();

                if chunk_seen.insert(digest) {
                    chunks.push(Chunk {
                        digest,
                        data: chunk_data.to_vec(),
                    });
                }

                chunk_metas.push((digest, chunk.length as u64));
                total_size += chunk.length as u64;
            }

            let file_hash = blake3::hash(data);
            let file_digest = *file_hash.as_bytes();

            if !chunk_metas.is_empty() {
                file_chunk_mappings.push(FileChunkMapping {
                    file_digest: Some(B3Digest {
                        digest: file_digest.to_vec(),
                    }),
                    chunks: chunk_metas
                        .iter()
                        .map(|(d, s)| ChunkMeta {
                            digest: Some(B3Digest { digest: d.to_vec() }),
                            size: *s,
                        })
                        .collect(),
                });
            }

            CaNode {
                node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(
                    CaFileNode {
                        digest: Some(B3Digest {
                            digest: file_digest.to_vec(),
                        }),
                        size: total_size,
                        executable: *executable,
                    },
                )),
            }
        },

        NarNode::Directory { entries } => {
            let mut ca_entries = Vec::new();
            let mut dir_size = 0u64;

            for entry in entries {
                let child_node = decompose_node(
                    &entry.node,
                    chunks,
                    chunk_seen,
                    directories,
                    file_chunk_mappings,
                );
                dir_size += 1;
                ca_entries.push(CaDirectoryEntry {
                    name: entry.name.clone(),
                    node: Some(child_node),
                });
            }

            let ca_dir = CaDirectory {
                entries: ca_entries,
            };
            let encoded = ca_dir.encode_to_vec();
            let hash = blake3::hash(&encoded);
            let dir_digest = *hash.as_bytes();
            directories.push((dir_digest, ca_dir));

            CaNode {
                node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Directory(
                    CaDirectoryNode {
                        digest: Some(B3Digest {
                            digest: dir_digest.to_vec(),
                        }),
                        size: dir_size,
                    },
                )),
            }
        },

        NarNode::Symlink { target } => CaNode {
            node: Some(ekapkgs_protocol::ekapkgs::v1::ca_node::Node::Symlink(
                CaSymlinkNode {
                    target: target.clone(),
                },
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nar::NarDirectoryEntry;

    #[test]
    fn decompose_regular_file() {
        let node = NarNode::Regular {
            executable: false,
            data: b"hello world".to_vec(),
        };
        let result = decompose_nar(&node);

        // Should produce 1 chunk and 1 file mapping.
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.file_chunk_mappings.len(), 1);
        assert!(result.directories.is_empty());

        // Root node should be a file.
        let root = result.root_node.node.as_ref().unwrap();
        assert!(matches!(
            root,
            ekapkgs_protocol::ekapkgs::v1::ca_node::Node::File(_)
        ));
    }

    #[test]
    fn decompose_directory() {
        let node = NarNode::Directory {
            entries: vec![
                NarDirectoryEntry {
                    name: "a.txt".to_owned(),
                    node: NarNode::Regular {
                        executable: false,
                        data: b"file a".to_vec(),
                    },
                },
                NarDirectoryEntry {
                    name: "link".to_owned(),
                    node: NarNode::Symlink {
                        target: "a.txt".to_owned(),
                    },
                },
            ],
        };
        let result = decompose_nar(&node);

        assert_eq!(result.chunks.len(), 1); // one file
        assert_eq!(result.directories.len(), 1); // one directory
        assert_eq!(result.file_chunk_mappings.len(), 1);
    }

    #[test]
    fn decompose_deduplicates_chunks() {
        // Two files with identical content should produce one chunk.
        let node = NarNode::Directory {
            entries: vec![
                NarDirectoryEntry {
                    name: "a.txt".to_owned(),
                    node: NarNode::Regular {
                        executable: false,
                        data: b"shared content".to_vec(),
                    },
                },
                NarDirectoryEntry {
                    name: "b.txt".to_owned(),
                    node: NarNode::Regular {
                        executable: false,
                        data: b"shared content".to_vec(),
                    },
                },
            ],
        };
        let result = decompose_nar(&node);
        assert_eq!(result.chunks.len(), 1);
    }

    #[test]
    fn decompose_matches_server_chunking() {
        // Verify that our decomposition produces the same blake3 digests as
        // the server would. This is a smoke test — the real verification is
        // that the chunk parameters match.
        let data = b"test data for chunking verification";
        let node = NarNode::Regular {
            executable: true,
            data: data.to_vec(),
        };
        let result = decompose_nar(&node);
        let chunk = &result.chunks[0];

        // Single chunk should equal the file data.
        assert_eq!(chunk.data, data);
        assert_eq!(chunk.digest, *blake3::hash(data).as_bytes());
    }
}
