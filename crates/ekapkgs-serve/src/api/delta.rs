use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// GET /delta/{base_hash}/{target_hash}
/// Serve a pre-computed zstd-dict-compressed delta NAR.
pub async fn get_delta(
    State(state): State<Arc<AppState>>,
    Path((base_hash, target_hash)): Path<(String, String)>,
) -> Response {
    match state.delta_cache.get(&base_hash, &target_hash) {
        Some(delta) => {
            // Zero-copy: wrap the Arc<[u8]> as Bytes without copying.
            let body = Bytes::from_owner(delta);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/zstd")],
                body,
            )
                .into_response()
        },
        None => (StatusCode::NOT_FOUND, "delta not found").into_response(),
    }
}
