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
    /// Derivation name (e.g., `hello-2.12.3`).
    #[serde(default)]
    pub name: Option<String>,
    /// Target system (e.g., `x86_64-linux`).
    #[serde(default)]
    pub system: Option<String>,
    /// Builder executable path.
    #[serde(default)]
    pub builder: Option<String>,
    /// Builder command-line arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Build environment variables.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    pub inputs: Option<DerivationInputs>,
    pub outputs: std::collections::HashMap<String, DerivationOutput>,
}

/// Input derivations and sources for a derivation.
#[derive(Debug, Deserialize)]
pub struct DerivationInputs {
    /// Input derivations: drv path → list of output names used.
    #[serde(default)]
    pub drvs: std::collections::HashMap<String, DerivationInputDrv>,
    /// Input source store paths.
    #[serde(default)]
    pub srcs: Vec<String>,
}

/// An input derivation entry from `nix derivation show`.
///
/// In format version 4+, each input drv maps to an object with an `outputs`
/// list. We also accept a bare list of strings for compatibility.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum DerivationInputDrv {
    /// Format v4+: `{ "outputs": ["out", "dev"] }`
    Structured { outputs: Vec<String> },
    /// Bare list: `["out", "dev"]`
    Bare(Vec<String>),
}

impl DerivationInputDrv {
    /// Get the output names regardless of format.
    pub fn outputs(&self) -> &[String] {
        match self {
            Self::Structured { outputs } | Self::Bare(outputs) => outputs,
        }
    }
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

/// Load a single derivation's metadata via `nix derivation show <drv_path>`.
///
/// Returns the `DerivationInfo` for the requested derivation. The drv_path
/// should be a `/nix/store/…*.drv` path.
pub fn show_derivation(drv_path: &str) -> Result<DerivationInfo, NixError> {
    let show: DerivationShowOutput = NixCommand::new(&["derivation", "show"])
        .arg(drv_path)
        .json()?;

    show.derivations
        .into_values()
        .next()
        .ok_or(NixError::Empty {
            context: format!("nix derivation show returned no derivations for {drv_path}"),
        })
}
