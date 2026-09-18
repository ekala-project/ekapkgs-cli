use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;
use crate::api::compat::is_valid_store_hash;

/// GET /serve/{hash}/{path..} — serve individual files from store paths.
///
/// Supports MIME detection, automatic index.html, and directory listings.
/// Path traversal is prevented by canonicalizing and checking the result
/// stays strictly inside the store path.
pub async fn get_serve(
    State(state): State<Arc<AppState>>,
    Path((hash, tail)): Path<(String, String)>,
) -> Response {
    if !is_valid_store_hash(&hash) {
        return not_found();
    }

    // Resolve hash to store path.
    let store_path = match state.storage.get_narinfo(&hash) {
        Ok(Some(ni)) => ni.store_path,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!("serve lookup failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        },
    };

    let base = PathBuf::from(&store_path);
    if !base.exists() {
        return not_found();
    }

    let target = base.join(&tail);

    // Canonicalize to resolve symlinks and `..` components.
    let Ok(real_nix_store) = PathBuf::from("/nix/store").canonicalize() else {
        return not_found();
    };
    let Ok(canonical) = target.canonicalize() else {
        return not_found();
    };

    // Path must be strictly inside /nix/store (not equal to it).
    if !canonical.starts_with(&real_nix_store) || canonical == real_nix_store {
        return not_found();
    }

    let Ok(meta) = std::fs::metadata(&canonical) else {
        return not_found();
    };

    if meta.is_file() {
        serve_file(&canonical)
    } else if meta.is_dir() {
        // Try index.html first.
        let index = canonical.join("index.html");
        if index.is_file() {
            return serve_file(&index);
        }
        serve_directory_listing(&canonical, &hash, &tail)
    } else {
        not_found()
    }
}

/// Maximum file size served via /serve/ (256 MiB).
/// Files larger than this should be accessed via the NAR endpoint instead.
const MAX_SERVE_FILE_SIZE: u64 = 256 * 1024 * 1024;

/// Serve a single file with MIME type detection.
fn serve_file(path: &std::path::Path) -> Response {
    let Ok(meta) = std::fs::metadata(path) else {
        return not_found();
    };
    if meta.len() > MAX_SERVE_FILE_SIZE {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "file too large for /serve/ endpoint",
        )
            .into_response();
    }
    let Ok(data) = std::fs::read(path) else {
        return not_found();
    };

    let mime = path
        .extension()
        .and_then(|e| e.to_str())
        .map(mime_from_extension)
        .unwrap_or("application/octet-stream");

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime.to_owned()),
            (header::CONTENT_LENGTH, data.len().to_string()),
        ],
        data,
    )
        .into_response()
}

/// Render a directory listing as HTML.
fn serve_directory_listing(dir: &std::path::Path, hash: &str, tail: &str) -> Response {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return not_found();
    };

    let mut entries: Vec<(String, bool, u64)> = Vec::new();

    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        entries.push((name, meta.is_dir(), meta.len()));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let tail_prefix = if tail.is_empty() || tail.ends_with('/') {
        tail.to_owned()
    } else {
        format!("{tail}/")
    };

    let mut html = String::from(
        "<!DOCTYPE html>\n<html><head><meta \
         charset=\"utf-8\"><title>Index</title></head>\n<body>\n<h1>Index</h1>\n<table>\\
         n<tr><th>Name</th><th>Size</th></tr>\n",
    );

    // Parent link.
    if !tail.is_empty() {
        html.push_str("<tr><td><a href=\"../\">..</a></td><td></td></tr>\n");
    }

    for (name, is_dir, size) in &entries {
        let escaped_name = html_escape(name);
        let encoded_name = percent_encode(name);
        let display_name = if *is_dir {
            format!("{escaped_name}/")
        } else {
            escaped_name.clone()
        };
        let href = format!("/serve/{hash}/{tail_prefix}{encoded_name}");
        let size_str = if *is_dir {
            String::new()
        } else {
            format_size(*size)
        };
        html.push_str(&format!(
            "<tr><td><a href=\"{href}\">{display_name}</a></td><td>{size_str}</td></tr>\n"
        ));
    }

    html.push_str("</table>\n</body></html>\n");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
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

/// HTML-escape a string.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Percent-encode a path component for URLs.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            },
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            },
        }
    }
    out
}

/// Format a file size in human-readable form.
fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * 1024 * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Map file extensions to MIME types.
fn mime_from_extension(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" | "mjs" => "application/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "txt" | "log" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "pdf" => "application/pdf",
        "gz" | "tgz" => "application/gzip",
        "xz" => "application/x-xz",
        "zst" => "application/zstd",
        "tar" => "application/x-tar",
        "zip" => "application/zip",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}
