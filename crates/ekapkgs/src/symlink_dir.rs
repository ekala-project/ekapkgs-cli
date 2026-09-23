//! CLI-managed symlink directory for fast package availability.
//!
//! Instead of going through `nix profile install` (15-60s), this module
//! manages `~/.ekapkgs-packages/bin/` as a plain directory of symlinks
//! pointing directly into `/nix/store/.../bin/`. The directory is on
//! PATH and provides instant package availability.

use std::path::Path;

/// Create symlinks from a store path's `bin/` directory into the
/// packages directory.
pub fn create_package_symlinks(packages_dir: &Path, store_path: &str) -> color_eyre::Result<()> {
    let bin_dir = Path::new(store_path).join("bin");
    let target_bin = packages_dir.join("bin");
    std::fs::create_dir_all(&target_bin)?;

    if bin_dir.exists() {
        for entry in std::fs::read_dir(&bin_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let link = target_bin.join(&name);
            // Remove existing symlink if present (handles upgrades).
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(entry.path(), &link)?;
        }
    }
    Ok(())
}

/// Remove symlinks that point into a specific store path.
pub fn remove_package_symlinks(packages_dir: &Path, store_path: &str) -> color_eyre::Result<()> {
    let bin_dir = Path::new(store_path).join("bin");
    let target_bin = packages_dir.join("bin");

    if bin_dir.exists() && target_bin.exists() {
        for entry in std::fs::read_dir(&bin_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let link = target_bin.join(&name);
            // Only remove if it points to OUR store path.
            if let Ok(target) = std::fs::read_link(&link) {
                if target.starts_with(store_path) {
                    std::fs::remove_file(&link)?;
                }
            }
        }
    }
    Ok(())
}

/// Rebuild the entire symlink directory from a manifest and store
/// path index. Used after `home apply` to reconcile.
pub fn rebuild_symlink_dir(
    packages_dir: &Path,
    index: &crate::store_path_index::StorePathIndex,
    package_names: &[String],
) -> color_eyre::Result<()> {
    let target_bin = packages_dir.join("bin");

    // Remove all existing symlinks.
    if target_bin.exists() {
        for entry in std::fs::read_dir(&target_bin)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                std::fs::remove_file(entry.path())?;
            }
        }
    } else {
        std::fs::create_dir_all(&target_bin)?;
    }

    // Recreate from manifest + store path index.
    for name in package_names {
        if let Some(entry) = index.lookup(name) {
            if crate::store_path_index::StorePathIndex::path_exists_locally(&entry.store_path) {
                create_package_symlinks(packages_dir, &entry.store_path)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_remove_symlinks() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = tempfile::TempDir::new().unwrap();

        // Create a fake store path with a bin directory.
        let bin = store.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("hello"), "#!/bin/sh\necho hi").unwrap();

        let store_path = store.path().to_str().unwrap();

        // Create symlinks.
        create_package_symlinks(dir.path(), store_path).unwrap();
        assert!(dir.path().join("bin/hello").is_symlink());

        // Remove symlinks.
        remove_package_symlinks(dir.path(), store_path).unwrap();
        assert!(!dir.path().join("bin/hello").exists());
    }

    #[test]
    fn remove_only_matching_symlinks() {
        let dir = tempfile::TempDir::new().unwrap();
        let store1 = tempfile::TempDir::new().unwrap();
        let store2 = tempfile::TempDir::new().unwrap();

        // Create two fake store paths.
        let bin1 = store1.path().join("bin");
        std::fs::create_dir_all(&bin1).unwrap();
        std::fs::write(bin1.join("tool1"), "").unwrap();

        let bin2 = store2.path().join("bin");
        std::fs::create_dir_all(&bin2).unwrap();
        std::fs::write(bin2.join("tool2"), "").unwrap();

        let sp1 = store1.path().to_str().unwrap();
        let sp2 = store2.path().to_str().unwrap();

        create_package_symlinks(dir.path(), sp1).unwrap();
        create_package_symlinks(dir.path(), sp2).unwrap();

        // Remove only store1's symlinks.
        remove_package_symlinks(dir.path(), sp1).unwrap();
        assert!(!dir.path().join("bin/tool1").exists());
        assert!(dir.path().join("bin/tool2").is_symlink());
    }
}
