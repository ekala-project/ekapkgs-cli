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

/// Versioned wrapper for `nix derivation show` output (format version >= 4).
#[derive(Debug, Deserialize)]
pub struct DerivationShowOutput {
    pub derivations: std::collections::HashMap<String, DerivationInfo>,
    #[allow(dead_code)]
    pub version: Option<u32>,
}

/// Result of evaluating a derivation closure via `nix derivation show -r`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivationInfo {
    pub inputs: Option<DerivationInputs>,
    pub outputs: std::collections::HashMap<String, DerivationOutput>,
}

/// Input derivations and sources for a derivation.
#[derive(Debug, Deserialize)]
pub struct DerivationInputs {
    #[serde(default)]
    pub drvs: std::collections::HashMap<String, serde_json::Value>,
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
    let show: DerivationShowOutput = NixCommand::new(&["derivation", "show"])
        .arg("-r")
        .arg(&installable.raw)
        .json()?;

    let mut paths = Vec::new();
    for drv in show.derivations.values() {
        for output in drv.outputs.values() {
            if let Some(path) = &output.path {
                // New format uses bare hash-name; normalize to full store paths.
                if path.starts_with("/nix/store/") {
                    paths.push(path.clone());
                } else {
                    paths.push(format!("/nix/store/{path}"));
                }
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
    let show: DerivationShowOutput = NixCommand::new(&["derivation", "show"])
        .arg("-r")
        .arg(&installable.raw)
        .json()?;

    let mut fod_paths = Vec::new();
    for drv in show.derivations.values() {
        for output in drv.outputs.values() {
            if output.hash.is_some() {
                if let Some(path) = &output.path {
                    if path.starts_with("/nix/store/") {
                        fod_paths.push(path.clone());
                    } else {
                        fod_paths.push(format!("/nix/store/{path}"));
                    }
                }
            }
        }
    }

    fod_paths.sort();
    fod_paths.dedup();
    Ok(fod_paths)
}
