use std::collections::HashMap;

use serde::Deserialize;

use crate::command::{NixCommand, NixError};
use crate::installable::Installable;

/// A single entry from `nix path-info --json`.
#[derive(Debug, Clone)]
pub struct PathInfoEntry {
    pub path: String,
    pub nar_size: u64,
    pub closure_size: u64,
    pub references: Vec<String>,
}

/// Raw JSON entry from `nix path-info --json`.
///
/// Nix returns a map keyed by store path; this struct represents the value.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPathInfo {
    #[serde(default)]
    nar_size: u64,
    #[serde(default)]
    closure_size: u64,
    #[serde(default)]
    references: Vec<String>,
}

/// Get path info with sizes for all paths in a closure.
///
/// Calls `nix path-info -rS --json <installable>`.
pub fn closure_path_info(installable: &Installable) -> Result<Vec<PathInfoEntry>, NixError> {
    let map: HashMap<String, RawPathInfo> = NixCommand::new(&["path-info"])
        .arg("-r")
        .arg("-S")
        .arg("--json")
        .arg(&installable.raw)
        .json()?;

    Ok(map
        .into_iter()
        .map(|(path, info)| PathInfoEntry {
            path,
            nar_size: info.nar_size,
            closure_size: info.closure_size,
            references: info.references,
        })
        .collect())
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

/// Parse a package name and version from a store path.
///
/// Uses the same heuristic as Nix's `parseDrvName`: the version starts
/// at the first `-` followed by a digit. Returns `(name, version)` where
/// version may be empty if no version segment is found.
///
/// ```
/// # use ekapkgs_nix::store::parse_store_path_name;
/// assert_eq!(
///     parse_store_path_name("/nix/store/abc123-hello-2.10"),
///     ("hello", "2.10"),
/// );
/// assert_eq!(
///     parse_store_path_name("/nix/store/abc123-source"),
///     ("source", ""),
/// );
/// ```
pub fn parse_store_path_name(path: &str) -> (&str, &str) {
    let basename = path.rsplit('/').next().unwrap_or(path);

    // Strip the hash prefix: everything up to and including the first `-`.
    let Some((_, name_version)) = basename.split_once('-') else {
        return (basename, "");
    };

    // Find the version boundary: first `-` followed by a digit.
    let bytes = name_version.as_bytes();
    for i in 0..bytes.len().saturating_sub(1) {
        if bytes[i] == b'-' && bytes[i + 1].is_ascii_digit() {
            return (&name_version[..i], &name_version[i + 1..]);
        }
    }

    (name_version, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_package() {
        assert_eq!(
            parse_store_path_name("/nix/store/abc123-hello-2.10"),
            ("hello", "2.10"),
        );
    }

    #[test]
    fn parse_package_with_complex_version() {
        assert_eq!(
            parse_store_path_name("/nix/store/abc123-glibc-2.38-44"),
            ("glibc", "2.38-44"),
        );
    }

    #[test]
    fn parse_python_package() {
        assert_eq!(
            parse_store_path_name("/nix/store/abc123-python3.11-requests-2.31.0"),
            ("python3.11-requests", "2.31.0"),
        );
    }

    #[test]
    fn parse_no_version() {
        assert_eq!(
            parse_store_path_name("/nix/store/abc123-source"),
            ("source", ""),
        );
    }

    #[test]
    fn parse_bare_path() {
        assert_eq!(parse_store_path_name("abc123-hello-1.0"), ("hello", "1.0"));
    }

    #[test]
    fn store_hash_extraction() {
        assert_eq!(
            store_path_hash("/nix/store/abc123-hello-1.0"),
            Some("abc123"),
        );
    }
}
