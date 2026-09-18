use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::AppState;
use crate::api::compat::is_valid_store_hash;

/// Serve a `/{hash}.ls` directory listing. Called from the narinfo handler
/// when the path ends in `.ls`.
pub async fn get_listing_inner(state: &AppState, hash: &str) -> Response {
    if !is_valid_store_hash(hash) {
        return not_found();
    }

    // Resolve the hash to a store path.
    let store_path = match state.storage.get_narinfo(hash) {
        Ok(Some(ni)) => ni.store_path,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!("listing lookup failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        },
    };

    let path = PathBuf::from(&store_path);
    if !path.exists() {
        return not_found();
    }

    let Some(root) = build_file_tree(&path) else {
        return not_found();
    };

    let listing = NarListing { version: 1, root };

    let body = match serde_json::to_string(&listing) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("listing serialization failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        },
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "max-age=31536000"),
        ],
        body,
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CACHE_CONTROL, "no-store")],
        "not found",
    )
        .into_response()
}

#[derive(Serialize)]
struct NarListing {
    version: u32,
    root: FileTree,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum FileTree {
    Regular {
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        executable: bool,
        size: u64,
    },
    Directory {
        entries: BTreeMap<String, FileTree>,
    },
    Symlink {
        target: String,
    },
}

/// Build a FileTree from a filesystem path using bounded recursion.
///
/// Uses `symlink_metadata` so symlinks are represented as-is rather than
/// followed. Directory entries are sorted lexicographically via `BTreeMap`.
/// Depth is bounded at 256 to prevent pathological cases.
fn build_file_tree(path: &std::path::Path) -> Option<FileTree> {
    build_recursive(path, 0)
}

fn build_recursive(p: &std::path::Path, depth: usize) -> Option<FileTree> {
    if depth > 256 {
        return None;
    }
    let meta = std::fs::symlink_metadata(p).ok()?;
    if meta.is_symlink() {
        let target = std::fs::read_link(p).ok()?;
        Some(FileTree::Symlink {
            target: target.to_string_lossy().into_owned(),
        })
    } else if meta.is_file() {
        let executable = meta.mode() & 0o111 != 0;
        Some(FileTree::Regular {
            executable,
            size: meta.len(),
        })
    } else if meta.is_dir() {
        let mut entries = BTreeMap::new();
        let read_dir = std::fs::read_dir(p).ok()?;
        let mut dir_entries: Vec<(String, PathBuf)> = Vec::new();
        for entry in read_dir {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            dir_entries.push((name, entry.path()));
        }
        for (name, child_path) in dir_entries {
            if let Some(tree) = build_recursive(&child_path, depth + 1) {
                entries.insert(name, tree);
            }
        }
        Some(FileTree::Directory { entries })
    } else {
        None
    }
}
