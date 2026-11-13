//! Append-only pack files for small chunks.
//!
//! Chunks below the inline threshold used to live in a `data` column of the
//! SQLite `chunks` table, which made that table most of the database. They are
//! now appended to pack files and the table only stores `(pack_id, offset)`.
//!
//! File layout: an 8-byte header (`PACK_MAGIC`) followed by records of
//!
//! ```text
//! digest: [u8; 32]   blake3 of data
//! len:    u32 LE
//! data:   [u8; len]
//! ```
//!
//! Records check themselves: a read only succeeds when the stored digest
//! matches both the requested digest and the blake3 of the data. A torn record
//! at the end of a pack (crash mid-append) is therefore detected, and only the
//! records after the last good one are affected. Each process start writes to
//! a fresh pack, so nothing is ever appended after a torn tail.
//!
//! Records are never rewritten. Chunks deleted by GC leave dead bytes behind.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// Header at the start of every pack file.
const PACK_MAGIC: &[u8; 8] = b"EKPACK01";

/// Bytes of framing before each record's data (digest + length).
pub const RECORD_OVERHEAD: u64 = 32 + 4;

/// Start a new pack once the current one reaches this size.
const PACK_MAX_SIZE: u64 = 1024 * 1024 * 1024;

/// Location of a record: `(pack_id, offset of the record header)`.
pub type PackLocation = (u32, u64);

struct Writer {
    /// The pack currently being appended to, opened on first append.
    current: Option<(u32, File, u64)>,
    /// Id for the next pack to create.
    next_id: u32,
    /// `true` when the current pack has writes that have not been fsynced.
    dirty: bool,
}

pub struct PackStore {
    dir: PathBuf,
    writer: Mutex<Writer>,
    readers: RwLock<HashMap<u32, Arc<File>>>,
}

impl PackStore {
    /// Open (or create) the pack directory.
    pub fn open(dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let mut max_id = 0;
        for entry in std::fs::read_dir(&dir)? {
            if let Some(id) = parse_pack_name(&entry?.file_name().to_string_lossy()) {
                max_id = max_id.max(id);
            }
        }
        Ok(Self {
            dir,
            writer: Mutex::new(Writer {
                current: None,
                next_id: max_id + 1,
                dirty: false,
            }),
            readers: RwLock::new(HashMap::new()),
        })
    }

    fn pack_path(&self, id: u32) -> PathBuf {
        pack_path(&self.dir, id)
    }

    /// Append a record and return where it was written.
    ///
    /// The data is not durable until [`PackStore::sync`] is called. Callers
    /// must sync before committing any database row that points at it.
    pub fn append(&self, digest: &[u8; 32], data: &[u8]) -> std::io::Result<PackLocation> {
        let len = u32::try_from(data.len())
            .map_err(|_| std::io::Error::other("pack record too large"))?;
        let mut w = self.writer.lock().expect("pack writer lock");

        let full = w
            .current
            .as_ref()
            .is_some_and(|(_, _, size)| *size >= PACK_MAX_SIZE);
        if full {
            // Make the finished pack durable before moving on.
            if let Some((_, file, _)) = &w.current {
                file.sync_data()?;
            }
            w.current = None;
            w.dirty = false;
        }
        if w.current.is_none() {
            let id = w.next_id;
            w.next_id += 1;
            let mut file = OpenOptions::new()
                .create_new(true)
                .append(true)
                .open(self.pack_path(id))?;
            file.write_all(PACK_MAGIC)?;
            w.current = Some((id, file, PACK_MAGIC.len() as u64));
        }

        let (id, file, size) = w.current.as_mut().expect("pack opened above");
        let offset = *size;
        let mut record = Vec::with_capacity(RECORD_OVERHEAD as usize + data.len());
        record.extend_from_slice(digest);
        record.extend_from_slice(&len.to_le_bytes());
        record.extend_from_slice(data);
        file.write_all(&record)?;
        *size += record.len() as u64;
        let loc = (*id, offset);
        w.dirty = true;
        Ok(loc)
    }

    /// Fsync the current pack if it has unsynced appends.
    pub fn sync(&self) -> std::io::Result<()> {
        let mut w = self.writer.lock().expect("pack writer lock");
        if w.dirty {
            if let Some((_, file, _)) = &w.current {
                file.sync_data()?;
            }
            w.dirty = false;
        }
        Ok(())
    }

    /// Read a record, verifying it against `digest`.
    ///
    /// Returns `Ok(None)` when the pack is missing or the record is damaged or
    /// does not match, so callers treat it like a missing chunk.
    pub fn read(
        &self,
        loc: PackLocation,
        digest: &[u8; 32],
        size: usize,
    ) -> std::io::Result<Option<Vec<u8>>> {
        let (id, offset) = loc;
        let Some(file) = self.reader(id)? else {
            tracing::warn!("pack {id} missing for chunk {}", hex(digest));
            return Ok(None);
        };
        let mut buf = vec![0u8; RECORD_OVERHEAD as usize + size];
        if let Err(e) = read_exact_at(&file, &mut buf, offset) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                tracing::warn!("pack {id} truncated at offset {offset}");
                return Ok(None);
            }
            return Err(e);
        }
        let (header, data) = buf.split_at(RECORD_OVERHEAD as usize);
        let stored_len = u32::from_le_bytes(header[32..36].try_into().expect("4 bytes"));
        if &header[..32] != digest
            || stored_len as usize != size
            || blake3::hash(data).as_bytes() != digest
        {
            tracing::warn!(
                "pack {id} record at offset {offset} does not match chunk {}",
                hex(digest)
            );
            return Ok(None);
        }
        buf.drain(..RECORD_OVERHEAD as usize);
        Ok(Some(buf))
    }

    fn reader(&self, id: u32) -> std::io::Result<Option<Arc<File>>> {
        if let Some(f) = self.readers.read().expect("pack readers lock").get(&id) {
            return Ok(Some(Arc::clone(f)));
        }
        let file = match File::open(self.pack_path(id)) {
            Ok(f) => Arc::new(f),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        self.readers
            .write()
            .expect("pack readers lock")
            .insert(id, Arc::clone(&file));
        Ok(Some(file))
    }
}

fn pack_path(dir: &Path, id: u32) -> PathBuf {
    dir.join(format!("pack-{id:06}.dat"))
}

fn parse_pack_name(name: &str) -> Option<u32> {
    name.strip_prefix("pack-")?
        .strip_suffix(".dat")?
        .parse()
        .ok()
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset)? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                offset += n as u64;
            },
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(data: &[u8]) -> [u8; 32] {
        *blake3::hash(data).as_bytes()
    }

    #[test]
    fn append_and_read_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let packs = PackStore::open(dir.path().to_path_buf()).unwrap();
        let a = b"first chunk";
        let b = b"second";
        let la = packs.append(&digest(a), a).unwrap();
        let lb = packs.append(&digest(b), b).unwrap();
        packs.sync().unwrap();

        assert_eq!(la, (1, PACK_MAGIC.len() as u64));
        assert_eq!(lb.1, la.1 + RECORD_OVERHEAD + a.len() as u64);
        assert_eq!(packs.read(la, &digest(a), a.len()).unwrap().unwrap(), a);
        assert_eq!(packs.read(lb, &digest(b), b.len()).unwrap().unwrap(), b);
    }

    #[test]
    fn mismatched_or_damaged_record_reads_as_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let packs = PackStore::open(dir.path().to_path_buf()).unwrap();
        let a = b"some data";
        let loc = packs.append(&digest(a), a).unwrap();
        packs.sync().unwrap();

        // Wrong digest.
        assert!(
            packs
                .read(loc, &digest(b"other"), a.len())
                .unwrap()
                .is_none()
        );
        // Wrong size.
        assert!(packs.read(loc, &digest(a), a.len() + 1).unwrap().is_none());
        // Missing pack.
        assert!(packs.read((99, 8), &digest(a), a.len()).unwrap().is_none());

        // Flip a data byte on disk.
        let path = pack_path(dir.path(), loc.0);
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        let fresh = PackStore::open(dir.path().to_path_buf()).unwrap();
        assert!(fresh.read(loc, &digest(a), a.len()).unwrap().is_none());
    }

    #[test]
    fn reopen_writes_to_a_new_pack() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = b"before restart";
        let la = {
            let packs = PackStore::open(dir.path().to_path_buf()).unwrap();
            packs.append(&digest(a), a).unwrap()
        };
        let packs = PackStore::open(dir.path().to_path_buf()).unwrap();
        let b = b"after restart";
        let lb = packs.append(&digest(b), b).unwrap();
        assert_eq!(lb.0, la.0 + 1);
        assert_eq!(packs.read(la, &digest(a), a.len()).unwrap().unwrap(), a);
        assert_eq!(packs.read(lb, &digest(b), b.len()).unwrap().unwrap(), b);
    }
}
