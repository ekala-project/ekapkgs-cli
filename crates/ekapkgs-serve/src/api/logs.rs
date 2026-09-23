use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// GET /log/{drv} — serve build logs from `/var/log/nix/drvs/`.
///
/// Accepts bzip2 encoding and serves compressed logs efficiently.
/// Falls back to transparent decompression when the client doesn't support bzip2.
pub async fn get_log(
    State(_state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(drv): Path<String>,
) -> Response {
    // Reject path traversal attempts.
    if drv.contains('/') || drv.contains('\\') || drv.contains("..") || drv.contains('\0') {
        return not_found();
    }

    // Extract the derivation hash from the first 32 chars.
    // The drv path basename looks like: {hash}-{name}.drv
    let drv_name = drv
        .strip_suffix(".drv")
        .or_else(|| drv.strip_suffix(".drv.bz2"))
        .unwrap_or(&drv);

    if drv_name.len() < 2 {
        return not_found();
    }

    // Build log path: /var/log/nix/drvs/{first 2 chars}/{rest}
    let (prefix, rest) = drv_name.split_at(2);

    // Try uncompressed log first, then bzip2 compressed.
    let log_dir: PathBuf = PathBuf::from("/nix/var/log/nix/drvs").join(prefix);

    let bz2_path = log_dir.join(format!("{rest}.drv.bz2"));
    let plain_path = log_dir.join(format!("{rest}.drv"));

    // Check if client accepts bzip2 encoding.
    let accepts_bzip2 = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| accepts_encoding(s, "bzip2"));

    if bz2_path.exists() {
        if accepts_bzip2 {
            // Serve raw bzip2 without decompression.
            let Ok(data) = std::fs::read(&bz2_path) else {
                return not_found();
            };
            return (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_owned()),
                    (header::CONTENT_ENCODING, "bzip2".to_owned()),
                    (header::CACHE_CONTROL, "max-age=31536000".to_owned()),
                    (header::CONTENT_LENGTH, data.len().to_string()),
                ],
                data,
            )
                .into_response();
        }

        // Transparently decompress bzip2.
        let Ok(compressed) = std::fs::read(&bz2_path) else {
            return not_found();
        };
        let mut decoder = bzip2::read::BzDecoder::new(compressed.as_slice());
        let mut decompressed = Vec::new();
        if decoder.read_to_end(&mut decompressed).is_err() {
            return (StatusCode::INTERNAL_SERVER_ERROR, "decompression error").into_response();
        }

        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_owned()),
                (header::CACHE_CONTROL, "max-age=31536000".to_owned()),
                (header::CONTENT_LENGTH, decompressed.len().to_string()),
            ],
            decompressed,
        )
            .into_response();
    }

    if plain_path.exists() {
        let Ok(data) = std::fs::read(&plain_path) else {
            return not_found();
        };
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_owned()),
                (header::CACHE_CONTROL, "max-age=31536000".to_owned()),
                (header::CONTENT_LENGTH, data.len().to_string()),
            ],
            data,
        )
            .into_response();
    }

    not_found()
}

/// Check if an Accept-Encoding header value includes a specific encoding.
/// Respects q=0 as explicit opt-out.
fn accepts_encoding(header: &str, encoding: &str) -> bool {
    for part in header.split(',') {
        let part = part.trim();
        let (name, params) = part.split_once(';').unwrap_or((part, ""));
        if name.trim().eq_ignore_ascii_case(encoding) {
            // Check for q=0 (explicit rejection).
            let params = params.trim();
            if let Some(q_str) = params
                .strip_prefix("q=")
                .or_else(|| params.strip_prefix("Q="))
            {
                if q_str.trim() == "0" || q_str.trim() == "0.0" || q_str.trim() == "0.00" {
                    return false;
                }
            }
            return true;
        }
    }
    false
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
        ],
        "not found",
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accepts_encoding_simple() {
        assert!(accepts_encoding("gzip, bzip2, zstd", "bzip2"));
        assert!(accepts_encoding("bzip2", "bzip2"));
        assert!(!accepts_encoding("gzip, zstd", "bzip2"));
    }

    #[test]
    fn test_accepts_encoding_q_zero() {
        assert!(!accepts_encoding("bzip2;q=0", "bzip2"));
        assert!(!accepts_encoding("bzip2; q=0", "bzip2"));
        assert!(!accepts_encoding("bzip2;Q=0", "bzip2"));
    }

    #[test]
    fn test_accepts_encoding_q_nonzero() {
        assert!(accepts_encoding("bzip2;q=0.5", "bzip2"));
        assert!(accepts_encoding("bzip2; q=1", "bzip2"));
    }
}
