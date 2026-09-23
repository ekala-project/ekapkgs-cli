//! Dynamic tab completion for package and service arguments.
//!
//! Reads cached indexes from `~/.cache/ekapkgs/indexes/` and the installed
//! manifests from `~/.config/ekapkgs/` to provide instant completions.
//! Never triggers nix evaluation or network requests — if no index exists,
//! returns empty completions.

use std::ffi::OsStr;
use std::io::Read;
use std::path::PathBuf;

use clap_complete::engine::CompletionCandidate;

// ---------------------------------------------------------------------------
// Package completions (from search index)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct PackageSearchEntry {
    attr: String,
    #[serde(default)]
    pname: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    outputs: Vec<String>,
    #[serde(default)]
    main_program: Option<String>,
}

fn index_dir() -> PathBuf {
    let dir = directories::ProjectDirs::from("", "", "ekapkgs")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache/ekapkgs")
        });
    dir.join("indexes")
}

fn load_package_index() -> Option<Vec<PackageSearchEntry>> {
    let dir = index_dir();
    let path = dir.join("packages-nixpkgs.json.zst");
    if !path.exists() {
        return None;
    }
    let compressed = std::fs::read(&path).ok()?;
    let mut decoder = zstd::Decoder::new(compressed.as_slice()).ok()?;
    let mut data = Vec::new();
    decoder.read_to_end(&mut data).ok()?;
    serde_json::from_slice(&data).ok()
}

/// Complete package names from the search index.
pub fn complete_packages(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let current_lower = current.to_lowercase();

    let Some(entries) = load_package_index() else {
        return Vec::new();
    };

    let mut candidates: Vec<CompletionCandidate> = entries
        .iter()
        .filter(|e| {
            e.attr.to_lowercase().starts_with(&current_lower)
                || e.pname.to_lowercase().starts_with(&current_lower)
        })
        .take(100)
        .map(|e| {
            let mut help = format!("{} ({})", e.pname, e.version);
            if !e.outputs.is_empty() && e.outputs != ["out"] {
                help.push_str(&format!(" [{}]", e.outputs.join(", ")));
            }
            if let Some(prog) = &e.main_program {
                help.push_str(&format!(" bin:{prog}"));
            }
            if !e.description.is_empty() {
                help.push(' ');
                help.push_str(&e.description);
            }
            CompletionCandidate::new(&e.attr).help(Some(help.into()))
        })
        .collect();

    candidates.sort();
    candidates
}

// ---------------------------------------------------------------------------
// Installed package completions (from manifest)
// ---------------------------------------------------------------------------

/// Complete from installed home packages (for `remove`).
pub fn complete_home_pkgs(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let Ok(manifest) = crate::config::HomePackages::load() else {
        return Vec::new();
    };
    manifest
        .packages
        .iter()
        .filter(|p| p.name.starts_with(current))
        .map(|p| CompletionCandidate::new(&p.name))
        .collect()
}

/// Complete from installed system packages (for `remove`).
pub fn complete_system_pkgs(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let Ok(manifest) = crate::config::SystemPackages::load() else {
        return Vec::new();
    };
    manifest
        .packages
        .iter()
        .filter(|p| p.name.starts_with(current))
        .map(|p| CompletionCandidate::new(&p.name))
        .collect()
}

// ---------------------------------------------------------------------------
// Service completions (from schema cache)
// ---------------------------------------------------------------------------

/// Complete service names from the schema cache.
pub fn complete_services(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let current_lower = current.to_lowercase();

    let Ok(Some(schema)) = crate::service_schema::read_cache() else {
        return Vec::new();
    };

    schema
        .services
        .iter()
        .filter(|(name, _)| name.to_lowercase().starts_with(&current_lower))
        .map(|(name, def)| {
            let help = if def.description.is_empty() {
                None
            } else {
                Some(def.description.clone().into())
            };
            CompletionCandidate::new(name).help(help)
        })
        .collect()
}

/// Complete from installed home services (for `remove`).
pub fn complete_home_svcs(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let Ok(manifest) = crate::config::HomeServices::load() else {
        return Vec::new();
    };
    manifest
        .services
        .iter()
        .filter(|s| s.name.starts_with(current))
        .map(|s| CompletionCandidate::new(&s.name))
        .collect()
}
