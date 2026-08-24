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

    /// Resolve a subpath under the root, ensuring it stays within bounds.
    /// Returns `None` if the resolved path would escape the root directory.
    fn safe_join(&self, subpath: &str) -> Option<PathBuf> {
        // Reject obvious traversal patterns before touching the filesystem.
        if subpath.contains("..") || subpath.contains('\0') {
            return None;
        }
        let joined = self.root.join(subpath);
        // Canonicalize to resolve any symlinks; fall back to the joined path
        // if the file doesn't exist yet (for writes).
        let resolved = joined.canonicalize().unwrap_or_else(|_| joined.clone());
        let root_resolved = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        if resolved.starts_with(&root_resolved) {
            Some(joined)
        } else {
            None
        }
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
        let Some(path) = self.safe_join(file_path) else {
            tracing::warn!("Rejected NAR path traversal attempt: {file_path}");
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(std::fs::read(path)?))
    }
}
