use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
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
pub async fn get_narinfo(
    State(state): State<Arc<AppState>>,
    Path(hash_narinfo): Path<String>,
) -> Response {
    // Strip the .narinfo suffix.
    let hash = hash_narinfo
        .strip_suffix(".narinfo")
        .unwrap_or(&hash_narinfo);

    if !is_valid_store_hash(hash) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
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
            return (StatusCode::NOT_FOUND, "not found").into_response();
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

    let body = narinfo.to_narinfo_string();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/x-nix-narinfo")],
        body,
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
        return (StatusCode::NOT_FOUND, "not found").into_response();
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
                    ],
                    data,
                )
                    .into_response()
            }
        },
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => {
            tracing::error!("NAR fetch failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        },
    }
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
