use serde::Deserialize;

use crate::command::{NixCommand, NixError};
use crate::installable::Installable;

/// A nix build output entry from `nix build --dry-run --json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildOutput {
    pub drv_path: String,
    pub outputs: std::collections::HashMap<String, String>,
}

/// Result of evaluating a derivation closure via `nix derivation show -r`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivationInfo {
    pub input_drvs: Option<std::collections::HashMap<String, serde_json::Value>>,
    pub outputs: std::collections::HashMap<String, DerivationOutput>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivationOutput {
    pub path: Option<String>,
    /// If set, this is a fixed-output derivation (FOD).
    pub hash: Option<String>,
    pub hash_algo: Option<String>,
}

/// Evaluate an installable and return its build output metadata.
///
/// Calls `nix build <installable> --dry-run --json`.
pub fn eval_build_outputs(installable: &Installable) -> Result<Vec<BuildOutput>, NixError> {
    NixCommand::new(&["build"])
        .arg(&installable.raw)
        .arg("--dry-run")
        .arg("--json")
        .json()
}

/// Get the full derivation closure for an installable.
///
/// Calls `nix derivation show -r <installable>` and extracts all output
/// store paths from the derivation graph.
pub fn derivation_closure_paths(installable: &Installable) -> Result<Vec<String>, NixError> {
    let derivations: std::collections::HashMap<String, DerivationInfo> =
        NixCommand::new(&["derivation", "show"])
            .arg("-r")
            .arg(&installable.raw)
            .json()?;

    let mut paths = Vec::new();
    for drv in derivations.values() {
        for output in drv.outputs.values() {
            if let Some(path) = &output.path {
                paths.push(path.clone());
            }
        }
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Get the raw derivation graph JSON for an installable.
///
/// Returns the full output of `nix derivation show -r` as bytes.
pub fn derivation_graph_json(installable: &Installable) -> Result<Vec<u8>, NixError> {
    let output = NixCommand::new(&["derivation", "show"])
        .arg("-r")
        .arg(&installable.raw)
        .output()?;
    Ok(output.stdout)
}

/// Extract fixed-output derivation (FOD) output paths from a derivation graph.
///
/// FODs are derivations whose outputs have a `hash` field set. These represent
/// fetched sources (tarballs, git checkouts, patches) rather than build results.
pub fn extract_fod_paths(installable: &Installable) -> Result<Vec<String>, NixError> {
    let derivations: std::collections::HashMap<String, DerivationInfo> =
        NixCommand::new(&["derivation", "show"])
            .arg("-r")
            .arg(&installable.raw)
            .json()?;

    let mut fod_paths = Vec::new();
    for drv in derivations.values() {
        for output in drv.outputs.values() {
            if output.hash.is_some() {
                if let Some(path) = &output.path {
                    fod_paths.push(path.clone());
                }
            }
        }
    }

    fod_paths.sort();
    fod_paths.dedup();
    Ok(fod_paths)
}
