use std::path::PathBuf;

use super::{NarInfo, StorageBackend};

/// Storage backend that reads from a pre-populated binary cache directory.
///
/// Expects the standard layout: `{root}/{hash}.narinfo` and `{root}/nar/...`.
pub struct FilesystemBackend {
    root: PathBuf,
}

impl FilesystemBackend {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl StorageBackend for FilesystemBackend {
    fn has_narinfo(&self, hash: &str) -> color_eyre::Result<bool> {
        let path = self.root.join(format!("{hash}.narinfo"));
        Ok(path.exists())
    }

    fn get_narinfo(&self, hash: &str) -> color_eyre::Result<Option<NarInfo>> {
        let text = match self.get_narinfo_text(hash)? {
            Some(t) => t,
            None => return Ok(None),
        };
        Ok(NarInfo::parse(&text))
    }

    fn get_narinfo_text(&self, hash: &str) -> color_eyre::Result<Option<String>> {
        let path = self.root.join(format!("{hash}.narinfo"));
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read_to_string(path)?))
    }

    fn get_nar(&self, file_path: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        let path = self.root.join(file_path);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read(path)?))
    }
}
