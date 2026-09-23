//! Store path index for fast package resolution.
//!
//! Maps package names to their nix store paths. Populated during
//! `system switch` / `home switch` from the closure, and updated
//! incrementally when the slow path resolves new packages.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct StorePathIndex {
    pub entries: HashMap<String, StorePathEntry>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct StorePathEntry {
    pub store_path: String,
}

impl StorePathIndex {
    /// Path to the index file.
    fn index_path() -> color_eyre::Result<PathBuf> {
        let cache_dir = directories::ProjectDirs::from("", "", "ekapkgs")
            .map(|d| d.cache_dir().to_path_buf())
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                PathBuf::from(home).join(".cache/ekapkgs")
            });
        std::fs::create_dir_all(&cache_dir)?;
        Ok(cache_dir.join("store-paths.db"))
    }

    /// Load the index from disk. Returns an empty index if the file
    /// does not exist or is corrupt.
    pub fn load() -> color_eyre::Result<Self> {
        let path = Self::index_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read(&path)?;
        match serde_json::from_slice(&data) {
            Ok(idx) => Ok(idx),
            Err(e) => {
                tracing::warn!("Corrupt store path index, resetting: {e}");
                Ok(Self::default())
            },
        }
    }

    /// Save the index to disk.
    pub fn save(&self) -> color_eyre::Result<()> {
        let path = Self::index_path()?;
        let data = serde_json::to_vec(self)?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        std::io::Write::write_all(&mut tmp, &data)?;
        tmp.persist(&path)?;
        Ok(())
    }

    /// Look up a package by name.
    pub fn lookup(&self, name: &str) -> Option<&StorePathEntry> {
        self.entries.get(name)
    }

    /// Clear the entire index (used after flake updates when store
    /// paths change).
    pub fn invalidate(&mut self) {
        self.entries.clear();
    }

    /// Check if a store path exists on disk.
    pub fn path_exists_locally(store_path: &str) -> bool {
        Path::new(store_path).exists()
    }

    /// Populate the index from a closure's path-info output.
    ///
    /// Parses package names from store path conventions and maps them
    /// to their store paths. This is called after `system switch` or
    /// `home switch` to bootstrap the fast path.
    pub fn populate_from_closure(&mut self, store_paths: &[String]) {
        for path in store_paths {
            let (name, _version) = ekapkgs_nix::store::parse_store_path_name(path);
            if !name.is_empty() {
                self.entries.insert(
                    name.to_owned(),
                    StorePathEntry {
                        store_path: path.clone(),
                    },
                );
            }
        }
    }

    /// Populate from JSON output of `nix build --json` (resolves a
    /// single package and adds it to the index for next time).
    pub fn add_from_build_output(&mut self, name: &str, store_path: &str) {
        self.entries.insert(
            name.to_owned(),
            StorePathEntry {
                store_path: store_path.to_owned(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_index_lookup_returns_none() {
        let idx = StorePathIndex::default();
        assert!(idx.lookup("vim").is_none());
    }

    #[test]
    fn add_and_lookup() {
        let mut idx = StorePathIndex::default();
        idx.add_from_build_output("vim", "/nix/store/abc123-vim-9.0");
        let entry = idx.lookup("vim").unwrap();
        assert_eq!(entry.store_path, "/nix/store/abc123-vim-9.0");
    }

    #[test]
    fn invalidate_clears_all() {
        let mut idx = StorePathIndex::default();
        idx.add_from_build_output("vim", "/nix/store/abc123-vim-9.0");
        idx.invalidate();
        assert!(idx.lookup("vim").is_none());
    }

    #[test]
    fn populate_from_closure() {
        let mut idx = StorePathIndex::default();
        idx.populate_from_closure(&[
            "/nix/store/abc123-hello-2.10".to_owned(),
            "/nix/store/def456-vim-9.0".to_owned(),
        ]);
        assert!(idx.lookup("hello").is_some());
        assert!(idx.lookup("vim").is_some());
        assert_eq!(
            idx.lookup("hello").unwrap().store_path,
            "/nix/store/abc123-hello-2.10"
        );
    }

    #[test]
    fn save_load_roundtrip() {
        // We can't easily test save/load since index_path() uses a fixed location,
        // but we can test serialization roundtrip.
        let mut idx = StorePathIndex::default();
        idx.add_from_build_output("curl", "/nix/store/xyz-curl-8.0");
        let data = serde_json::to_vec(&idx).unwrap();
        let loaded: StorePathIndex = serde_json::from_slice(&data).unwrap();
        assert_eq!(
            loaded.lookup("curl").unwrap().store_path,
            "/nix/store/xyz-curl-8.0"
        );
    }
}
