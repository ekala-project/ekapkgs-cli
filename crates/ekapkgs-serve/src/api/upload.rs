use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// Validate the bearer token against the configured write tokens.
fn check_auth(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    let tokens = match &state.write_tokens {
        Some(t) if !t.is_empty() => t,
        _ => {
            // No auth configured — reject all writes.
            return Err((StatusCode::FORBIDDEN, "push not enabled").into_response());
        }
    };

    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));

    match auth {
        Some(token) if tokens.contains(&token.to_string()) => Ok(()),
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

    let content = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid utf-8").into_response(),
    };

    // Re-sign the narinfo with our key before storing.
    let narinfo = match crate::storage::NarInfo::parse(content) {
        Some(mut ni) => {
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
        }
        None => return (StatusCode::BAD_REQUEST, "invalid narinfo").into_response(),
    };

    let signed_content = narinfo.to_narinfo_string();

    match state.storage.put_narinfo(hash, &signed_content) {
        Ok(true) => {
            // Register with GC tracker if present.
            if let Some(ref tracker) = state.gc_tracker {
                tracker.record_access(hash);
            }
            tracing::debug!("Stored narinfo for {hash}");
            (StatusCode::OK, "ok").into_response()
        }
        Ok(false) => (StatusCode::METHOD_NOT_ALLOWED, "read-only backend").into_response(),
        Err(e) => {
            tracing::error!("Failed to store narinfo: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
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

    let nar_path = format!("nar/{file}");

    match state.storage.put_nar(&nar_path, &body) {
        Ok(true) => {
            if let Some(ref tracker) = state.gc_tracker {
                let hash = file.split('.').next().unwrap_or(&file);
                tracker.record_access(hash);
            }
            tracing::debug!("Stored NAR {file}");
            (StatusCode::OK, "ok").into_response()
        }
        Ok(false) => (StatusCode::METHOD_NOT_ALLOWED, "read-only backend").into_response(),
        Err(e) => {
            tracing::error!("Failed to store NAR: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        }
    }
}
