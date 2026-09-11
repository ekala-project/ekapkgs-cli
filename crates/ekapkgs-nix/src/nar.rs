//! Minimal NAR (Nix Archive) format parser and writer.
//!
//! The NAR format is:
//!   str("nix-archive-1") nar-obj
//!
//! Where str(s) = int(|s|) + pad(s), int(n) = 64-bit LE, and pad(s) = s
//! padded with null bytes to a multiple of 8.
//!
//! A nar-obj is: str("(") str("type") str(<type>) <content> str(")")
//!
//! Types:
//!   "regular"   — optional str("executable") str("") then str("contents") str(data)
//!   "directory"  — zero or more entries, each:
//!       str("entry") str("(") str("name") str(name) str("node") <nar-obj> str(")")
//!       entries MUST be sorted by name
//!   "symlink"   — str("target") str(target)

use color_eyre::eyre::{bail, ensure};

/// A parsed NAR tree node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NarNode {
    Regular { executable: bool, data: Vec<u8> },
    Directory { entries: Vec<NarDirectoryEntry> },
    Symlink { target: String },
}

/// A single entry within a NAR directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarDirectoryEntry {
    pub name: String,
    pub node: NarNode,
}

// --- Parsing ---

/// A cursor over a byte slice for reading NAR fields.
struct NarReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> NarReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Read a 64-bit little-endian integer.
    fn read_u64(&mut self) -> color_eyre::Result<u64> {
        ensure!(
            self.pos + 8 <= self.data.len(),
            "unexpected EOF reading u64 at offset {}",
            self.pos
        );
        let val = u64::from_le_bytes(self.data[self.pos..self.pos + 8].try_into()?);
        self.pos += 8;
        Ok(val)
    }

    /// Read a NAR string (length-prefixed, padded to 8-byte alignment).
    fn read_str(&mut self) -> color_eyre::Result<&'a [u8]> {
        let len = self.read_u64()? as usize;
        ensure!(
            self.pos + len <= self.data.len(),
            "unexpected EOF reading string of length {len} at offset {}",
            self.pos
        );
        let s = &self.data[self.pos..self.pos + len];
        self.pos += len;
        // Skip padding to 8-byte alignment.
        let pad = (8 - (len % 8)) % 8;
        self.pos += pad;
        Ok(s)
    }

    /// Read a NAR string and expect it to match a specific value.
    fn expect_str(&mut self, expected: &str) -> color_eyre::Result<()> {
        let s = self.read_str()?;
        ensure!(
            s == expected.as_bytes(),
            "expected {:?}, got {:?} at offset {}",
            expected,
            String::from_utf8_lossy(s),
            self.pos
        );
        Ok(())
    }

    /// Read a NAR string and return it as a UTF-8 String.
    fn read_string(&mut self) -> color_eyre::Result<String> {
        let s = self.read_str()?;
        Ok(String::from_utf8(s.to_vec())?)
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
}

/// Parse a NAR archive from bytes into a `NarNode` tree.
pub fn parse_nar(data: &[u8]) -> color_eyre::Result<NarNode> {
    let mut reader = NarReader::new(data);
    reader.expect_str("nix-archive-1")?;
    let node = parse_nar_obj(&mut reader)?;
    // Allow trailing data (some NARs may have padding).
    Ok(node)
}

fn parse_nar_obj(reader: &mut NarReader<'_>) -> color_eyre::Result<NarNode> {
    reader.expect_str("(")?;
    reader.expect_str("type")?;
    let type_str = reader.read_string()?;

    let node = match type_str.as_str() {
        "regular" => parse_regular(reader)?,
        "directory" => parse_directory(reader)?,
        "symlink" => parse_symlink(reader)?,
        other => bail!("unknown NAR node type: {other:?}"),
    };

    reader.expect_str(")")?;
    Ok(node)
}

fn parse_regular(reader: &mut NarReader<'_>) -> color_eyre::Result<NarNode> {
    let mut executable = false;
    // Read fields — could be "executable" then "contents", or just "contents".
    loop {
        let field = reader.read_string()?;
        match field.as_str() {
            "executable" => {
                // The value is always an empty string.
                reader.read_str()?;
                executable = true;
            },
            "contents" => {
                let data = reader.read_str()?.to_vec();
                return Ok(NarNode::Regular { executable, data });
            },
            other => bail!("unexpected field in regular file: {other:?}"),
        }
    }
}

fn parse_directory(reader: &mut NarReader<'_>) -> color_eyre::Result<NarNode> {
    let mut entries = Vec::new();

    loop {
        // Peek: next string is either "entry" or ")" (end of directory).
        if reader.remaining() < 8 {
            break;
        }

        // Read the next string to see if it's "entry" or we're at the end.
        let token = reader.read_string()?;
        match token.as_str() {
            "entry" => {
                reader.expect_str("(")?;
                reader.expect_str("name")?;
                let name = reader.read_string()?;
                reader.expect_str("node")?;
                let node = parse_nar_obj(reader)?;
                reader.expect_str(")")?;
                entries.push(NarDirectoryEntry { name, node });
            },
            ")" => {
                // End of directory — but the outer parse_nar_obj will also
                // try to read ")". We need to signal that we already consumed it.
                // Restructure: the directory parser should NOT consume the final ")".
                // However, we already read the token. So we need to detect this.
                //
                // Actually, looking at the format more carefully:
                // directory = str("(") str("type") str("directory") entries* str(")")
                // where entries = str("entry") str("(") ... str(")")
                //
                // So the ")" here IS the directory's closing paren, and the outer
                // parse_nar_obj also expects ")". This means we consumed the paren
                // that the outer function needs.
                //
                // Fix: Don't consume the ")" here. Instead, peek. But our reader
                // doesn't support peeking. Let's restructure by having the directory
                // parser NOT read the final closing paren — instead, it returns when
                // it finds a ")" token (but doesn't consume it).
                //
                // Since we already consumed it, we need to "unread" it.
                // The simplest fix: rewind the reader position.
                //
                // A ")" string is encoded as: 8 bytes (int 1) + 1 byte (")") + 7 pad = 16 bytes.
                reader.pos -= 16; // Rewind past the ")" string we just consumed.
                break;
            },
            other => bail!("unexpected token in directory: {other:?}"),
        }
    }

    Ok(NarNode::Directory { entries })
}

fn parse_symlink(reader: &mut NarReader<'_>) -> color_eyre::Result<NarNode> {
    reader.expect_str("target")?;
    let target = reader.read_string()?;
    Ok(NarNode::Symlink { target })
}

// --- Writing ---

/// Write a `NarNode` tree to NAR format bytes.
pub fn write_nar(node: &NarNode) -> Vec<u8> {
    let mut buf = Vec::new();
    write_str(&mut buf, "nix-archive-1");
    write_nar_obj(&mut buf, node);
    buf
}

fn write_nar_obj(buf: &mut Vec<u8>, node: &NarNode) {
    write_str(buf, "(");
    write_str(buf, "type");

    match node {
        NarNode::Regular { executable, data } => {
            write_str(buf, "regular");
            if *executable {
                write_str(buf, "executable");
                write_str(buf, "");
            }
            write_str(buf, "contents");
            write_bytes(buf, data);
        },
        NarNode::Directory { entries } => {
            write_str(buf, "directory");
            for entry in entries {
                write_str(buf, "entry");
                write_str(buf, "(");
                write_str(buf, "name");
                write_str(buf, &entry.name);
                write_str(buf, "node");
                write_nar_obj(buf, &entry.node);
                write_str(buf, ")");
            }
        },
        NarNode::Symlink { target } => {
            write_str(buf, "symlink");
            write_str(buf, "target");
            write_str(buf, target);
        },
    }

    write_str(buf, ")");
}

/// Write a length-prefixed, padded string.
fn write_str(buf: &mut Vec<u8>, s: &str) {
    write_bytes(buf, s.as_bytes());
}

/// Write a length-prefixed, padded byte sequence.
fn write_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    let len = data.len() as u64;
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(data);
    let pad = (8 - (data.len() % 8)) % 8;
    buf.extend(std::iter::repeat_n(0u8, pad));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_regular_file() {
        let node = NarNode::Regular {
            executable: false,
            data: b"hello world".to_vec(),
        };
        let nar = write_nar(&node);
        let parsed = parse_nar(&nar).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn roundtrip_executable_file() {
        let node = NarNode::Regular {
            executable: true,
            data: b"#!/bin/sh\necho hi\n".to_vec(),
        };
        let nar = write_nar(&node);
        let parsed = parse_nar(&nar).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn roundtrip_empty_file() {
        let node = NarNode::Regular {
            executable: false,
            data: Vec::new(),
        };
        let nar = write_nar(&node);
        let parsed = parse_nar(&nar).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn roundtrip_symlink() {
        let node = NarNode::Symlink {
            target: "/nix/store/abc123-target".to_string(),
        };
        let nar = write_nar(&node);
        let parsed = parse_nar(&nar).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn roundtrip_empty_directory() {
        let node = NarNode::Directory {
            entries: Vec::new(),
        };
        let nar = write_nar(&node);
        let parsed = parse_nar(&nar).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn roundtrip_directory_with_files() {
        let node = NarNode::Directory {
            entries: vec![
                NarDirectoryEntry {
                    name: "a.txt".to_string(),
                    node: NarNode::Regular {
                        executable: false,
                        data: b"file a".to_vec(),
                    },
                },
                NarDirectoryEntry {
                    name: "b.sh".to_string(),
                    node: NarNode::Regular {
                        executable: true,
                        data: b"#!/bin/sh\n".to_vec(),
                    },
                },
                NarDirectoryEntry {
                    name: "link".to_string(),
                    node: NarNode::Symlink {
                        target: "a.txt".to_string(),
                    },
                },
            ],
        };
        let nar = write_nar(&node);
        let parsed = parse_nar(&nar).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn roundtrip_nested_directories() {
        let node = NarNode::Directory {
            entries: vec![
                NarDirectoryEntry {
                    name: "bin".to_string(),
                    node: NarNode::Directory {
                        entries: vec![NarDirectoryEntry {
                            name: "hello".to_string(),
                            node: NarNode::Regular {
                                executable: true,
                                data: b"ELF binary".to_vec(),
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
                                data: b"shared library".to_vec(),
                            },
                        }],
                    },
                },
            ],
        };
        let nar = write_nar(&node);
        let parsed = parse_nar(&nar).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn write_nar_header() {
        let node = NarNode::Regular {
            executable: false,
            data: b"x".to_vec(),
        };
        let nar = write_nar(&node);
        // First 8 bytes: length of "nix-archive-1" = 13
        assert_eq!(&nar[..8], &13u64.to_le_bytes());
        assert_eq!(&nar[8..21], b"nix-archive-1");
    }

    #[test]
    fn large_file_roundtrip() {
        let data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        let node = NarNode::Regular {
            executable: false,
            data,
        };
        let nar = write_nar(&node);
        let parsed = parse_nar(&nar).unwrap();
        assert_eq!(node, parsed);
    }
}
