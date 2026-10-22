//! Dynamic tab completion for package and service arguments.
//!
//! Reads cached indexes from `~/.cache/ekapkgs/indexes/` and the installed
//! manifests from `~/.config/ekapkgs/` to provide instant completions.
//! Never triggers nix evaluation or network requests — if no index exists,
//! returns empty completions.

use std::ffi::OsStr;

use clap_complete::engine::CompletionCandidate;
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Package completions (from search index)
// ---------------------------------------------------------------------------

fn open_search_db() -> Option<Connection> {
    let dir = crate::commands::search::cache_dir().ok()?;
    let path = dir.join("search.db");
    if !path.exists() {
        return None;
    }
    Connection::open(&path).ok()
}

/// Complete package names from the search index.
pub fn complete_packages(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    let current_lower = current.to_lowercase();
    let like_pattern = format!("{current_lower}%");

    let Some(conn) = open_search_db() else {
        return Vec::new();
    };

    let Ok(mut stmt) = conn.prepare(
        "SELECT attr, pname, version, description, outputs, main_program
         FROM packages
         WHERE LOWER(attr) LIKE ?1 OR LOWER(pname) LIKE ?1
         LIMIT 100",
    ) else {
        return Vec::new();
    };

    let mut candidates: Vec<CompletionCandidate> = stmt
        .query_map([&like_pattern], |row| {
            let attr: String = row.get(0)?;
            let pname: String = row.get(1)?;
            let version: String = row.get(2)?;
            let description: Option<String> = row.get(3)?;
            let outputs_json: Option<String> = row.get(4)?;
            let main_program: Option<String> = row.get(5)?;

            let outputs: Vec<String> = outputs_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();

            let mut help = format!("{pname} ({version})");
            if !outputs.is_empty() && outputs != ["out"] {
                help.push_str(&format!(" [{}]", outputs.join(", ")));
            }
            if let Some(prog) = &main_program {
                help.push_str(&format!(" bin:{prog}"));
            }
            if let Some(desc) = &description {
                if !desc.is_empty() {
                    help.push(' ');
                    help.push_str(desc);
                }
            }
            Ok(CompletionCandidate::new(attr).help(Some(help.into())))
        })
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
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
