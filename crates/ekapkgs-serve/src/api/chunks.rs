use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// GET /cas/chunk/{b3hex}
/// Download a single chunk by its blake3 hex digest.
pub async fn get_chunk(State(state): State<Arc<AppState>>, Path(b3hex): Path<String>) -> Response {
    if !state.storage.supports_cas() {
        return (StatusCode::NOT_FOUND, "CAS not available").into_response();
    }

    let Some(digest) = hex_decode(&b3hex) else {
        return (StatusCode::BAD_REQUEST, "invalid digest").into_response();
    };

    match state.storage.get_chunk(&digest) {
        Ok(Some(data)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/octet-stream")],
            data,
        )
            .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "chunk not found").into_response(),
        Err(e) => {
            tracing::error!("chunk fetch failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
        },
    }
}

/// PUT /cas/chunk/{b3hex}
/// Upload a single chunk. Verifies the blake3 digest matches.
pub async fn put_chunk(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(b3hex): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(e) = crate::api::upload::check_auth(&state, &headers) {
        return e;
    }

    if !state.storage.supports_cas() {
        return (StatusCode::NOT_FOUND, "CAS not available").into_response();
    }

    let Some(expected_digest) = hex_decode(&b3hex) else {
        return (StatusCode::BAD_REQUEST, "invalid digest").into_response();
    };

    // Verify the blake3 hash matches.
    let actual_hash = blake3::hash(&body);
    if actual_hash.as_bytes() != &expected_digest {
        return (StatusCode::BAD_REQUEST, "digest mismatch").into_response();
    }

    // Store the chunk via the CastoreBackend.
    if let Some(castore) = state
        .storage
        .as_any()
        .downcast_ref::<crate::storage::castore::CastoreBackend>()
    {
        match castore.get_chunk_by_digest(&expected_digest) {
            Ok(Some(_)) => {
                // Already exists.
                return (StatusCode::OK, "ok").into_response();
            },
            Ok(None) => {},
            Err(e) => {
                tracing::error!("chunk check failed: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response();
            },
        }

        // Store it using the internal method which handles both file and DB.
        match castore.store_chunk_external(&body) {
            Ok(_) => (StatusCode::OK, "ok").into_response(),
            Err(e) => {
                tracing::error!("chunk store failed: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
            },
        }
    } else {
        (StatusCode::NOT_FOUND, "CAS not available").into_response()
    }
}

/// Decode a hex string to a 32-byte array.
fn hex_decode(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}
