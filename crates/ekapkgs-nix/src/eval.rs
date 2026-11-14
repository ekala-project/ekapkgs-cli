use std::path::Path;

use serde::Deserialize;

use crate::command::{NixCommand, NixError};
use crate::installable::Installable;

/// One entry of `nix build --json`, with or without `--dry-run`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum BuildOutput {
    /// A derivation installable: `{"drvPath": ..., "outputs": {...}}`.
    #[serde(rename_all = "camelCase")]
    Derivation {
        drv_path: String,
        outputs: std::collections::HashMap<String, String>,
    },
    /// A store path installable, e.g. `/nix/store/...-hello`, which nix
    /// reports as the bare path.
    Path(String),
}

impl BuildOutput {
    /// The store path of the `out` output, or the store path itself.
    pub fn out(&self) -> Option<&str> {
        match self {
            Self::Derivation { outputs, .. } => outputs.get("out").map(String::as_str),
            Self::Path(path) => Some(path),
        }
    }

    /// The store paths of all outputs.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        let (outputs, path) = match self {
            Self::Derivation { outputs, .. } => (Some(outputs), None),
            Self::Path(path) => (None, Some(path.as_str())),
        };
        outputs
            .into_iter()
            .flat_map(|o| o.values().map(String::as_str))
            .chain(path)
    }
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

/// Whether a flake installable such as `.#foo.bar` evaluates to a derivation.
///
/// Evaluates `<installable>.type`, which forces the value no further than
/// `nix eval <installable> --apply '_: null'` does and builds nothing. It
/// needs no `--apply`, so iets evaluates it.
pub fn is_derivation(installable: &str) -> bool {
    NixCommand::new(&["eval"])
        .arg("--raw")
        .arg(format!("{installable}.type"))
        .output()
        .is_ok_and(|output| output.stdout == b"derivation")
}

/// Build an installable and return the store paths of the outputs it built.
///
/// Calls `nix build <installable> --print-out-paths <extra>`. Unlike
/// `--json`, iets runs this natively. Like any `nix build` without
/// `--no-link`, it leaves a `result` link. The list is never empty.
pub fn build_out_paths(
    installable: &Installable,
    extra: &[String],
) -> Result<Vec<String>, NixError> {
    let output = NixCommand::new(&["build"])
        .arg(&installable.raw)
        .arg("--print-out-paths")
        .args(extra)
        .output()?;
    let paths: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if paths.is_empty() {
        return Err(NixError::Empty {
            context: format!("nix build {installable} printed no output paths"),
        });
    }
    Ok(paths)
}

/// Get the full derivation closure for an installable.
///
/// Calls `nix derivation show -r <installable>` and extracts all output
/// store paths from the derivation graph.
pub fn derivation_closure_paths(installable: &Installable) -> Result<Vec<String>, NixError> {
    closure_paths(installable, None)
}

/// [`derivation_closure_paths`], with the root flake locked by `lock_file`
/// instead of its own `flake.lock`.
///
/// Passes `--reference-lock-file`. Nix locks the flake again starting from
/// that file and would write the result over the flake's `flake.lock`, so
/// `--no-write-lock-file` is passed too.
pub fn derivation_closure_paths_with_lock(
    installable: &Installable,
    lock_file: &Path,
) -> Result<Vec<String>, NixError> {
    closure_paths(installable, Some(lock_file))
}

fn closure_paths(
    installable: &Installable,
    lock_file: Option<&Path>,
) -> Result<Vec<String>, NixError> {
    let mut cmd = NixCommand::new(&["derivation", "show"])
        .arg("-r")
        .arg(&installable.raw);
    if let Some(lock_file) = lock_file {
        cmd = cmd
            .arg("--reference-lock-file")
            .arg(lock_file.to_string_lossy())
            .arg("--no-write-lock-file");
    }
    let show: DerivationShowOutput = cmd.json()?;

    Ok(collect_output_paths(&show))
}

/// Get the output store paths of an installable, without any dependencies.
///
/// A bare store path (not a `.drv`) is returned as-is, since it is already an
/// output and need not be valid locally. Anything else is resolved with
/// `nix derivation show` (non-recursive), returning every output of the
/// installable's derivation.
pub fn output_paths(installable: &Installable) -> Result<Vec<String>, NixError> {
    if installable.raw.starts_with("/nix/store/") && !installable.raw.ends_with(".drv") {
        return Ok(vec![installable.raw.clone()]);
    }

    let show: DerivationShowOutput = NixCommand::new(&["derivation", "show"])
        .arg(&installable.raw)
        .json()?;

    Ok(collect_output_paths(&show))
}

/// Collect the sorted, deduplicated output paths from `nix derivation show`.
fn collect_output_paths(show: &DerivationShowOutput) -> Vec<String> {
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
    paths
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_output_of_a_derivation() {
        let outputs: Vec<BuildOutput> = serde_json::from_str(
            r#"[{"drvPath":"/nix/store/a.drv","outputs":{"out":"/nix/store/a","dev":"/nix/store/a-dev"}}]"#,
        )
        .unwrap();
        assert_eq!(outputs[0].out(), Some("/nix/store/a"));
        let mut paths: Vec<&str> = outputs[0].paths().collect();
        paths.sort_unstable();
        assert_eq!(paths, ["/nix/store/a", "/nix/store/a-dev"]);
    }

    #[test]
    fn build_output_of_a_derivation_without_out() {
        let outputs: Vec<BuildOutput> = serde_json::from_str(
            r#"[{"drvPath":"/nix/store/a.drv","outputs":{"bin":"/nix/store/a-bin"}}]"#,
        )
        .unwrap();
        assert_eq!(outputs[0].out(), None);
        assert_eq!(outputs[0].paths().collect::<Vec<_>>(), ["/nix/store/a-bin"]);
    }

    #[test]
    fn build_output_of_a_store_path() {
        // `nix build /nix/store/...-a --json`
        let outputs: Vec<BuildOutput> = serde_json::from_str(r#"["/nix/store/a"]"#).unwrap();
        assert_eq!(outputs[0], BuildOutput::Path("/nix/store/a".to_owned()));
        assert_eq!(outputs[0].out(), Some("/nix/store/a"));
        assert_eq!(outputs[0].paths().collect::<Vec<_>>(), ["/nix/store/a"]);
    }
}
