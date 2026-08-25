use serde::Deserialize;

use crate::command::{NixCommand, NixError};
use crate::installable::Installable;

/// A single entry from `nix path-info --json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathInfoEntry {
    pub path: String,
    pub nar_size: u64,
    #[serde(default)]
    pub closure_size: u64,
    #[serde(default)]
    pub references: Vec<String>,
}

/// Get path info with sizes for all paths in a closure.
///
/// Calls `nix path-info -rS --json <installable>`.
pub fn closure_path_info(installable: &Installable) -> Result<Vec<PathInfoEntry>, NixError> {
    NixCommand::new(&["path-info"])
        .arg("-r")
        .arg("-S")
        .arg("--json")
        .arg(&installable.raw)
        .json()
}

/// Query the local nix store for which paths exist.
///
/// Returns `(have, want)` — paths present locally and paths missing.
pub fn partition_local(paths: &[String]) -> Result<(Vec<String>, Vec<String>), NixError> {
    let mut have = Vec::new();
    let mut want = Vec::new();

    // Query all paths at once. nix path-info exits non-zero if any path
    // is missing, so we query individually for reliable partitioning.
    for path in paths {
        let result = NixCommand::new(&["path-info"]).arg(path).output();

        match result {
            Ok(_) => have.push(path.clone()),
            Err(NixError::Failed { .. }) => want.push(path.clone()),
            Err(e) => return Err(e),
        }
    }

    Ok((have, want))
}

/// Import paths from a local binary cache directory into the nix store.
///
/// Uses `nix copy --all --from file://<path>` to bulk import.
pub fn import_from_local_cache(cache_dir: &std::path::Path) -> Result<(), NixError> {
    let url = format!("file://{}", cache_dir.display());

    NixCommand::new(&["copy"])
        .arg("--all")
        .arg("--from")
        .arg(&url)
        .stream()?;

    Ok(())
}

/// Extract the hash portion from a store path.
///
/// Given `/nix/store/abc123-hello-1.0`, returns `abc123`.
pub fn store_path_hash(path: &str) -> Option<&str> {
    let basename = path.rsplit('/').next()?;
    basename.split('-').next()
}
