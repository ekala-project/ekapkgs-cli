use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

use crate::AppState;

/// Validate that a string looks like a nix store hash: only lowercase alphanumeric.
fn is_valid_nix_hash(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// Validate that a NAR filename is safe: `{hash}.nar` or `{hash}.nar.{compression}`.
fn is_valid_nar_filename(s: &str) -> bool {
    // Must not contain path separators or traversal.
    if s.contains('/') || s.contains('\\') || s.contains("..") {
        return false;
    }

    // Expected formats: {hash}.nar, {hash}.nar.xz, {hash}.nar.zst
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

/// Validate the bearer token against the configured write tokens.
#[allow(clippy::result_large_err)]
pub fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    let tokens = match &state.write_tokens {
        Some(t) if !t.is_empty() => t,
        _ => {
            // No auth configured — reject all writes.
            return Err((StatusCode::FORBIDDEN, "push not enabled").into_response());
        },
    };

    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match auth {
        Some(token)
            if tokens.iter().any(|t| {
                let a = t.as_bytes();
                let b = token.as_bytes();
                a.len() == b.len() && a.ct_eq(b).into()
            }) =>
        {
            Ok(())
        },
        _ => Err((StatusCode::UNAUTHORIZED, "invalid or missing token").into_response()),
    }
}

/// PUT /{hash}.narinfo
pub async fn put_narinfo(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(hash_narinfo): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(e) = check_auth(&state, &headers) {
        return e;
    }

    let hash = hash_narinfo
        .strip_suffix(".narinfo")
        .unwrap_or(&hash_narinfo);

    if !is_valid_nix_hash(hash) {
        return (StatusCode::BAD_REQUEST, "invalid hash").into_response();
    }

    let Ok(content) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid utf-8").into_response();
    };

    // Re-sign the narinfo with our key before storing.
    let narinfo = match crate::storage::NarInfo::parse(content) {
        Some(mut ni) => {
            // Verify the narinfo's StorePath hash matches the URL hash.
            match ni.store_path_hash() {
                Some(sp_hash) if sp_hash == hash => {},
                _ => {
                    return (
                        StatusCode::BAD_REQUEST,
                        "narinfo StorePath hash does not match URL",
                    )
                        .into_response();
                },
            }

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
        None => return (StatusCode::BAD_REQUEST, "invalid narinfo").into_response(),
    };

    let signed_content = narinfo.to_narinfo_string();

    match state.storage.put_narinfo(hash, &signed_content) {
        Ok(true) => {
            state.metrics.push_narinfo_total.inc();
            // Register with GC tracker if present.
            if let Some(ref tracker) = state.gc_tracker {
                tracker.record_access(hash);
            }
            tracing::debug!("Stored narinfo for {hash}");
            (StatusCode::OK, "ok").into_response()
        },
        Ok(false) => (StatusCode::METHOD_NOT_ALLOWED, "read-only backend").into_response(),
        Err(e) => {
            tracing::error!("Failed to store narinfo: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        },
    }
}

/// PUT /nar/{file}
pub async fn put_nar(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(file): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(e) = check_auth(&state, &headers) {
        return e;
    }

    if !is_valid_nar_filename(&file) {
        return (StatusCode::BAD_REQUEST, "invalid nar filename").into_response();
    }

    let nar_path = format!("nar/{file}");

    match state.storage.put_nar(&nar_path, &body) {
        Ok(true) => {
            state.metrics.push_nar_total.inc();
            state.metrics.push_nar_bytes_total.inc_by(body.len() as u64);
            if let Some(ref tracker) = state.gc_tracker {
                let hash = file.split('.').next().unwrap_or(&file);
                tracker.record_access(hash);
            }
            tracing::debug!("Stored NAR {file}");
            (StatusCode::OK, "ok").into_response()
        },
        Ok(false) => (StatusCode::METHOD_NOT_ALLOWED, "read-only backend").into_response(),
        Err(e) => {
            tracing::error!("Failed to store NAR: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        },
    }
}
