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
pub struct DerivationOutput {
    pub path: Option<String>,
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
