use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use serde::Deserialize;
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// GET /health
pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, "OK\n")
}

/// GET /version
pub async fn version() -> impl IntoResponse {
    let body = format!("{} {}\n", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    (StatusCode::OK, body)
}

/// GET /nix-cache-info
pub async fn nix_cache_info(State(_state): State<Arc<AppState>>) -> impl IntoResponse {
    let body = "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 30\n".to_owned();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/x-nix-cache-info")],
        body,
    )
}

/// GET / — Root landing page with cache info, public keys, and nix.conf snippets.
pub async fn root(State(state): State<Arc<AppState>>, headers: HeaderMap) -> impl IntoResponse {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .filter(|s| *s == "http" || *s == "https")
        .unwrap_or("http");

    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");

    let url = format!("{scheme}://{}", html_escape(host));
    let public_key = state.signer.public_key();

    let mut vars = std::collections::HashMap::new();
    vars.insert("url", url.as_str());
    vars.insert("public_key", public_key.as_str());
    vars.insert("key_name", state.signer.key_name());

    let body = render_template(ROOT_TEMPLATE_WITH_KEY, &vars);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
}

/// Single-pass `[[key]]` template renderer.
///
/// Inserted values are HTML-escaped. Unknown keys are left as `[[key]]`.
/// Values are never re-scanned (prevents injection).
fn render_template(template: &str, vars: &std::collections::HashMap<&str, &str>) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("[[") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        if let Some(end) = after_open.find("]]") {
            let key = &after_open[..end];
            if let Some(value) = vars.get(key) {
                result.push_str(value);
            } else {
                // Unknown key — leave as-is.
                result.push_str("[[");
                result.push_str(key);
                result.push_str("]]");
            }
            rest = &after_open[end + 2..];
        } else {
            // No closing brackets — emit literally.
            result.push_str("[[");
            rest = after_open;
        }
    }
    result.push_str(rest);
    result
}

/// HTML-escape a string to prevent XSS.
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

const ROOT_TEMPLATE_WITH_KEY: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Nix Binary Cache</title>
<style>
body { font-family: system-ui, sans-serif; max-width: 700px; margin: 2em auto; padding: 0 1em; color: #333; }
h1 { font-size: 1.4em; }
pre { background: #f4f4f4; padding: 1em; border-radius: 4px; overflow-x: auto; }
code { font-size: 0.9em; }
</style>
</head>
<body>
<h1>Nix Binary Cache</h1>
<p>This server provides a <a href="/nix-cache-info">Nix binary cache</a>.</p>

<h2>Public Key</h2>
<pre><code>[[public_key]]</code></pre>

<h2>Usage</h2>
<p>Add to <code>/etc/nix/nix.conf</code>:</p>
<pre><code>extra-substituters = [[url]]
extra-trusted-public-keys = [[public_key]]</code></pre>

<p>Or use with a single command:</p>
<pre><code>nix build --extra-substituters '[[url]]' --extra-trusted-public-keys '[[public_key]]' ...</code></pre>
</body>
</html>
"#;

/// Nix base32 alphabet: `0123456789abcdfghijklmnpqrsvwxyz` (no e, o, t, u).
/// Store path hashes are exactly 32 characters in this alphabet.
pub(crate) fn is_valid_store_hash(s: &str) -> bool {
    const NIX_BASE32: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";
    s.len() == 32 && s.bytes().all(|b| NIX_BASE32.contains(&b))
}

/// Loose hash validation for non-store-path contexts (NAR filenames, etc.).
fn is_valid_nix_hash(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Query parameters for narinfo endpoint.
#[derive(Deserialize)]
pub struct NarInfoQuery {
    /// When present (any value), return JSON v3 format.
    json: Option<String>,
}

/// Validate that a NAR filename is safe: `{hash}.nar` or `{hash}.nar.{compression}`.
fn is_valid_nar_filename(s: &str) -> bool {
    if s.contains('/') || s.contains('\\') || s.contains("..") {
        return false;
    }

    let hash = if let Some(h) = s.strip_suffix(".nar.xz") {
        h
    } else if let Some(h) = s.strip_suffix(".nar.zst") {
        h
    } else if let Some(h) = s.strip_suffix(".nar") {
        h
    } else {
        return false;
    };

    is_valid_nix_hash(hash)
}

/// GET /{hash}.narinfo
///
/// Supports `?json` query parameter to return NarInfo JSON v3 format.
pub async fn get_narinfo(
    State(state): State<Arc<AppState>>,
    Path(hash_narinfo): Path<String>,
    Query(query): Query<NarInfoQuery>,
) -> Response {
    // Strip the .narinfo suffix.
    let hash = hash_narinfo
        .strip_suffix(".narinfo")
        .unwrap_or(&hash_narinfo);

    if !is_valid_store_hash(hash) {
        return not_found_response();
    }

    state
        .metrics
        .narinfo_requests_total
        .with_label_values(&["attempt"])
        .inc();

    let narinfo = match state.storage.get_narinfo(hash) {
        Ok(Some(mut ni)) => {
            // Re-sign with our key if the narinfo doesn't already have our sig.
            let fingerprint = crate::signing::NarInfoSigner::fingerprint(
                &ni.store_path,
                &ni.nar_hash,
                ni.nar_size,
                &ni.references,
            );
            let sig = state.signer.sign(&fingerprint);
            if !ni.signatures.contains(&sig) {
                ni.signatures.push(sig);
            }
            ni
        },
        Ok(None) => {
            state
                .metrics
                .narinfo_requests_total
                .with_label_values(&["miss"])
                .inc();
            return not_found_response();
        },
        Err(e) => {
            tracing::error!("narinfo lookup failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
        },
    };

    // Record access for GC tracking.
    if let Some(ref tracker) = state.gc_tracker {
        tracker.record_access(hash);
    }

    state
        .metrics
        .narinfo_requests_total
        .with_label_values(&["hit"])
        .inc();

    // JSON v3 format requested via ?json query parameter.
    if query.json.is_some() {
        let json = narinfo_to_json(&narinfo);
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/json".to_owned()),
                (header::CACHE_CONTROL, "max-age=86400".to_owned()),
            ],
            json,
        )
            .into_response();
    }

    let nar_link = &narinfo.url;
    let body = narinfo.to_narinfo_string();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/x-nix-narinfo".to_owned()),
            (header::CACHE_CONTROL, "max-age=86400".to_owned()),
            (HeaderName::from_static("nix-link"), nar_link.to_owned()),
        ],
        body,
    )
        .into_response()
}

/// Serialize a NarInfo to JSON v3 format.
fn narinfo_to_json(ni: &crate::storage::NarInfo) -> String {
    let references: Vec<&str> = ni
        .references
        .iter()
        .map(|r| r.rsplit('/').next().unwrap_or(r.as_str()))
        .collect();

    let signatures: Vec<serde_json::Value> = ni
        .signatures
        .iter()
        .filter_map(|s| {
            let (key_name, sig) = s.split_once(':')?;
            Some(serde_json::json!({
                "keyName": key_name,
                "sig": sig,
            }))
        })
        .collect();

    let deriver = ni
        .deriver
        .as_ref()
        .map(|d| serde_json::Value::String(d.clone()));

    let json = serde_json::json!({
        "version": 3,
        "storeDir": "/nix/store",
        "storePath": ni.store_path,
        "url": ni.url,
        "compression": ni.compression,
        "narHash": ni.nar_hash,
        "narSize": ni.nar_size,
        "fileHash": if ni.file_hash.is_empty() { None } else { Some(&ni.file_hash) },
        "fileSize": if ni.file_size == 0 { None } else { Some(ni.file_size) },
        "downloadHash": serde_json::Value::Null,
        "downloadSize": serde_json::Value::Null,
        "references": references,
        "deriver": deriver,
        "registrationTime": serde_json::Value::Null,
        "signatures": signatures,
        "ca": ni.ca,
        "ultimate": false,
    });

    serde_json::to_string_pretty(&json).unwrap_or_default()
}

/// Standard 404 response with `Cache-Control: no-store`.
fn not_found_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CACHE_CONTROL, "no-store")],
        "not found",
    )
        .into_response()
}

/// GET /nar/{file}
///
/// Supports HTTP Range requests for resumable downloads. Returns
/// `Accept-Ranges: bytes` and `Content-Length` on all responses.
pub async fn get_nar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    if !is_valid_nar_filename(&file) {
        return not_found_response();
    }

    let nar_path = format!("nar/{file}");

    match state.storage.get_nar(&nar_path) {
        Ok(Some(data)) => {
            state.metrics.nar_downloads_total.inc();

            // Record access for GC tracking.
            if let Some(ref tracker) = state.gc_tracker {
                let hash = file.split('.').next().unwrap_or(&file);
                tracker.record_access(hash);
            }

            let content_type = if file.ends_with(".zst") {
                "application/zstd"
            } else if file.ends_with(".xz") {
                "application/x-xz"
            } else {
                "application/x-nix-nar"
            };

            let total_len = data.len();

            // Check for Range header.
            if let Some(range) = headers
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| parse_range_header(s, total_len))
            {
                let (start, end) = range;
                let slice = data[start..=end].to_vec();
                let content_range = format!("bytes {start}-{end}/{total_len}");

                state
                    .metrics
                    .nar_download_bytes_total
                    .inc_by(slice.len() as u64);

                (
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (header::CONTENT_TYPE, content_type.to_owned()),
                        (header::CONTENT_LENGTH, slice.len().to_string()),
                        (header::CONTENT_RANGE, content_range),
                        (header::ACCEPT_RANGES, "bytes".to_owned()),
                        (header::CACHE_CONTROL, "max-age=31536000".to_owned()),
                    ],
                    slice,
                )
                    .into_response()
            } else {
                state
                    .metrics
                    .nar_download_bytes_total
                    .inc_by(total_len as u64);

                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, content_type.to_owned()),
                        (header::CONTENT_LENGTH, total_len.to_string()),
                        (header::ACCEPT_RANGES, "bytes".to_owned()),
                        (header::CACHE_CONTROL, "max-age=31536000".to_owned()),
                    ],
                    data,
                )
                    .into_response()
            }
        },
        Ok(None) => not_found_response(),
        Err(e) => {
            tracing::error!("NAR fetch failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        },
    }
}

/// GET /nar/{outhash}-{narhash}.nar — nix-serve compatible NAR URL.
///
/// Extracts the outhash from the compound path segment and delegates to the
/// standard NAR handler logic.
pub async fn get_nar_compat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(file): Path<String>,
) -> Response {
    get_nar(State(state), headers, Path(file)).await
}

/// Parse a `Range: bytes=start-end` header, returning `(start, end)` inclusive.
///
/// Supports `bytes=N-` (from N to end) and `bytes=N-M` (from N to M inclusive).
/// Does not support multipart ranges.
fn parse_range_header(header: &str, total: usize) -> Option<(usize, usize)> {
    let range = header.strip_prefix("bytes=")?;
    let (start_str, end_str) = range.split_once('-')?;

    let start: usize = start_str.parse().ok()?;
    let end: usize = if end_str.is_empty() {
        total.saturating_sub(1)
    } else {
        end_str.parse().ok()?
    };

    if start >= total || end >= total || start > end {
        return None;
    }

    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_from_start() {
        assert_eq!(parse_range_header("bytes=0-99", 1000), Some((0, 99)));
    }

    #[test]
    fn parse_range_open_end() {
        assert_eq!(parse_range_header("bytes=500-", 1000), Some((500, 999)));
    }

    #[test]
    fn parse_range_middle() {
        assert_eq!(parse_range_header("bytes=100-200", 1000), Some((100, 200)));
    }

    #[test]
    fn parse_range_invalid_start_past_end() {
        assert_eq!(parse_range_header("bytes=1000-", 1000), None);
    }

    #[test]
    fn parse_range_invalid_reversed() {
        assert_eq!(parse_range_header("bytes=200-100", 1000), None);
    }

    #[test]
    fn parse_range_not_bytes() {
        assert_eq!(parse_range_header("items=0-10", 1000), None);
    }

    #[test]
    fn narinfo_json_format() {
        let ni = crate::storage::NarInfo {
            store_path: "/nix/store/abc123-hello-2.12.1".to_owned(),
            url: "nar/abc123.nar".to_owned(),
            compression: "none".to_owned(),
            file_hash: String::new(),
            file_size: 0,
            nar_hash: "sha256:deadbeef".to_owned(),
            nar_size: 196040,
            references: vec!["abc123-hello-2.12.1".to_owned()],
            deriver: Some("xyz-hello.drv".to_owned()),
            signatures: vec!["cache-1:base64sig==".to_owned()],
            ca: None,
        };

        let json_str = narinfo_to_json(&ni);
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(parsed["version"], 3);
        assert_eq!(parsed["storeDir"], "/nix/store");
        assert_eq!(parsed["storePath"], "/nix/store/abc123-hello-2.12.1");
        assert_eq!(parsed["narSize"], 196040);
        assert_eq!(parsed["signatures"][0]["keyName"], "cache-1");
        assert_eq!(parsed["signatures"][0]["sig"], "base64sig==");
        assert_eq!(parsed["ultimate"], false);
        assert!(parsed["fileHash"].is_null());
        assert!(parsed["ca"].is_null());
    }

    #[test]
    fn valid_store_hash() {
        // 32-char nix base32 hash (no e, o, t, u)
        assert!(is_valid_store_hash("0123456789abcdfghijklmnpqrsvwxyz"));
        assert!(is_valid_store_hash("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
    }

    #[test]
    fn invalid_store_hash_wrong_length() {
        assert!(!is_valid_store_hash("abc"));
        assert!(!is_valid_store_hash(""));
        assert!(!is_valid_store_hash("0123456789abcdfghijklmnpqrsvwxyza")); // 33 chars
    }

    #[test]
    fn invalid_store_hash_forbidden_chars() {
        // 'e' is not in nix base32
        assert!(!is_valid_store_hash("e123456789abcdfghijklmnpqrsvwxy"));
        // 'o' is not in nix base32
        assert!(!is_valid_store_hash("o123456789abcdfghijklmnpqrsvwxy"));
        // 't' is not in nix base32
        assert!(!is_valid_store_hash("t123456789abcdfghijklmnpqrsvwxy"));
        // 'u' is not in nix base32
        assert!(!is_valid_store_hash("u123456789abcdfghijklmnpqrsvwxy"));
    }
}
