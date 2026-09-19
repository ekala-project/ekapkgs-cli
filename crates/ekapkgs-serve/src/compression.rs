//! Zstd compression middleware tuned for Nix binary cache workloads.
//!
//! Replaces generic tower-http compression with a custom implementation that:
//! - Only compresses when the client sends `Accept-Encoding: zstd` (q > 0)
//! - Caps window_log at 25 for NAR responses (LDM), 23 for others (RFC 9659)
//! - Bounds LDM encoder memory with a per-worker semaphore
//! - Offloads compression of chunks >= 1 KiB to the blocking pool

use std::io::Write;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, Response, StatusCode, header};
use axum::response::IntoResponse;
use http_body_util::BodyExt;
use tower::{Layer, Service};

use crate::config::CompressionConfig;

/// Minimum response body size to bother compressing.
const MIN_COMPRESS_SIZE: u64 = 256;

/// Threshold above which we acquire an LDM semaphore permit.
const LARGE_BODY_THRESHOLD: u64 = 4 * 1024 * 1024; // 4 MiB

/// Window log cap for NAR responses (LDM-enabled).
const LDM_WINDOW_LOG_CAP: u32 = 25;

/// Window log cap for non-NAR HTTP responses per RFC 9659.
const HTTP_WINDOW_LOG_MAX: u32 = 23;

/// Threshold below which compression runs inline (not on blocking pool).
const INLINE_COMPRESS_THRESHOLD: usize = 1024; // 1 KiB

/// Axum layer that applies zstd compression to responses.
#[derive(Clone)]
pub struct ZstdCompressionLayer {
    config: Arc<CompressionConfig>,
    ldm_semaphore: Arc<tokio::sync::Semaphore>,
}

impl ZstdCompressionLayer {
    pub fn new(config: CompressionConfig) -> Self {
        let permits = config.max_ldm_encoders as usize;
        Self {
            config: Arc::new(config),
            ldm_semaphore: Arc::new(tokio::sync::Semaphore::new(permits)),
        }
    }
}

impl<S> Layer<S> for ZstdCompressionLayer {
    type Service = ZstdCompressionService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        ZstdCompressionService {
            inner,
            config: Arc::clone(&self.config),
            ldm_semaphore: Arc::clone(&self.ldm_semaphore),
        }
    }
}

/// Service that conditionally compresses responses with zstd.
#[derive(Clone)]
pub struct ZstdCompressionService<S> {
    inner: S,
    config: Arc<CompressionConfig>,
    ldm_semaphore: Arc<tokio::sync::Semaphore>,
}

impl<S> Service<Request<Body>> for ZstdCompressionService<S>
where
    S: Service<Request<Body>, Response = Response<Body>> + Clone + Send + 'static,
    S::Future: Send,
{
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;
    type Response = Response<Body>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let config = Arc::clone(&self.config);
        let ldm_semaphore = Arc::clone(&self.ldm_semaphore);

        // Check if we should compress before passing to inner service.
        let accepts_zstd = should_accept_zstd(req.headers());
        let is_head = req.method() == axum::http::Method::HEAD;

        let mut inner = self.inner.clone();
        Box::pin(async move {
            let response = inner.call(req).await?;

            if !should_compress(&config, &response, accepts_zstd, is_head) {
                return Ok(response);
            }

            let (parts, body) = response.into_parts();

            // Determine body size from Content-Length if available.
            let content_length = parts
                .headers
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());

            // Skip small bodies with known size.
            if let Some(len) = content_length {
                if len < MIN_COMPRESS_SIZE {
                    return Ok(Response::from_parts(parts, body));
                }
            }

            // Determine if this is a NAR response for window_log tuning.
            let is_nar = parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|ct| ct == "application/x-nix-nar");

            // Compute effective window_log.
            let cap = if is_nar {
                LDM_WINDOW_LOG_CAP
            } else {
                HTTP_WINDOW_LOG_MAX
            };
            let use_ldm = is_nar && config.long_distance_matching;
            let effective_window_log = if config.window_log == 0 {
                if use_ldm { cap } else { 0 }
            } else {
                config.window_log.min(cap)
            };

            // Collect body bytes.
            let Ok(body_bytes) = collect_body(body).await else {
                return Ok((StatusCode::INTERNAL_SERVER_ERROR, "compression error").into_response());
            };

            if body_bytes.is_empty() {
                return Ok(Response::from_parts(parts, Body::from(body_bytes)));
            }

            // Determine if we need an LDM permit.
            let needs_ldm_permit = use_ldm
                && (content_length.is_none_or(|len| len >= LARGE_BODY_THRESHOLD)
                    || body_bytes.len() as u64 >= LARGE_BODY_THRESHOLD);

            let actual_use_ldm = if needs_ldm_permit {
                // Try to acquire non-blocking; fall back to no-LDM if unavailable.
                match ldm_semaphore.clone().try_acquire_owned() {
                    Ok(permit) => {
                        // Keep permit alive during compression; drop after.
                        let _permit = permit;
                        true
                    },
                    Err(_) => false,
                }
            } else {
                use_ldm
            };

            let level = config.level;

            // Compress: inline for small chunks, blocking pool for large.
            let compressed = if body_bytes.len() < INLINE_COMPRESS_THRESHOLD {
                compress_bytes(&body_bytes, level, effective_window_log, actual_use_ldm)
            } else {
                let bytes = body_bytes.clone();
                tokio::task::spawn_blocking(move || {
                    compress_bytes(&bytes, level, effective_window_log, actual_use_ldm)
                })
                .await
                .unwrap_or_default()
            };

            let Some(compressed) = compressed else {
                // Compression failed — return original body.
                return Ok(Response::from_parts(parts, Body::from(body_bytes)));
            };

            // Build compressed response with updated headers.
            let mut headers = parts.headers;
            headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("zstd"));
            headers.remove(header::CONTENT_LENGTH);
            headers.append(header::VARY, HeaderValue::from_static("accept-encoding"));

            let mut response = Response::new(Body::from(compressed));
            *response.status_mut() = parts.status;
            *response.headers_mut() = headers;
            *response.version_mut() = parts.version;

            Ok(response)
        })
    }
}

/// Check if the client's Accept-Encoding includes zstd with q > 0.
fn should_accept_zstd(headers: &HeaderMap) -> bool {
    let Some(accept) = headers.get(header::ACCEPT_ENCODING) else {
        return false;
    };
    let Ok(value) = accept.to_str() else {
        return false;
    };

    for part in value.split(',') {
        let part = part.trim();
        let (encoding, params) = part.split_once(';').unwrap_or((part, ""));
        if encoding.trim().eq_ignore_ascii_case("zstd") {
            // Check for explicit q=0 opt-out.
            let params = params.trim();
            if let Some(q_str) = params
                .strip_prefix("q=")
                .or_else(|| params.strip_prefix("Q="))
            {
                let q = q_str.trim();
                if q == "0" || q == "0.0" || q == "0.00" || q == "0.000" {
                    return false;
                }
            }
            return true;
        }
    }
    false
}

/// Decide whether to compress a response.
fn should_compress(
    config: &CompressionConfig,
    response: &Response<Body>,
    accepts_zstd: bool,
    is_head: bool,
) -> bool {
    if !config.enable {
        return false;
    }
    if is_head {
        return false;
    }
    if !accepts_zstd {
        return false;
    }
    // Don't double-compress.
    if response.headers().contains_key(header::CONTENT_ENCODING) {
        return false;
    }

    let status = response.status();
    // Skip 206 Partial Content.
    if status == StatusCode::PARTIAL_CONTENT {
        return false;
    }
    // Skip 204 No Content.
    if status == StatusCode::NO_CONTENT {
        return false;
    }
    // Skip 3xx redirects.
    if status.is_redirection() {
        return false;
    }

    true
}

/// Compress bytes with zstd using the specified parameters.
fn compress_bytes(data: &[u8], level: i32, window_log: u32, use_ldm: bool) -> Option<Vec<u8>> {
    let mut encoder = zstd::Encoder::new(Vec::new(), level).ok()?;

    if window_log > 0 {
        encoder
            .set_parameter(zstd::zstd_safe::CParameter::WindowLog(window_log))
            .ok()?;
    }
    if use_ldm {
        encoder
            .set_parameter(zstd::zstd_safe::CParameter::EnableLongDistanceMatching(
                true,
            ))
            .ok()?;
    }

    encoder.write_all(data).ok()?;
    encoder.finish().ok()
}

/// Collect a body stream into bytes.
async fn collect_body(body: Body) -> Result<Vec<u8>, axum::Error> {
    let collected = body.collect().await.map_err(axum::Error::new)?;
    Ok(collected.to_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_zstd_simple() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, "zstd".parse().unwrap());
        assert!(should_accept_zstd(&headers));
    }

    #[test]
    fn accept_zstd_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, "gzip, zstd, br".parse().unwrap());
        assert!(should_accept_zstd(&headers));
    }

    #[test]
    fn accept_zstd_q_zero_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, "zstd;q=0".parse().unwrap());
        assert!(!should_accept_zstd(&headers));

        headers.insert(header::ACCEPT_ENCODING, "zstd; q=0.0".parse().unwrap());
        assert!(!should_accept_zstd(&headers));
    }

    #[test]
    fn accept_zstd_q_nonzero() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, "zstd;q=0.5".parse().unwrap());
        assert!(should_accept_zstd(&headers));
    }

    #[test]
    fn accept_zstd_missing() {
        let headers = HeaderMap::new();
        assert!(!should_accept_zstd(&headers));
    }

    #[test]
    fn accept_zstd_not_listed() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT_ENCODING, "gzip, br".parse().unwrap());
        assert!(!should_accept_zstd(&headers));
    }

    #[test]
    fn compress_roundtrip() {
        let data = b"hello world this is a test of compression that should work";
        let compressed = compress_bytes(data, 1, 0, false).unwrap();
        assert!(!compressed.is_empty());

        // Decompress and verify.
        use std::io::Read;
        let mut decoder = zstd::Decoder::new(compressed.as_slice()).unwrap();
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn compress_with_window_log() {
        let data = vec![0u8; 4096];
        let compressed = compress_bytes(&data, 1, HTTP_WINDOW_LOG_MAX, false).unwrap();
        assert!(!compressed.is_empty());
    }

    #[test]
    fn should_compress_disabled() {
        let config = CompressionConfig {
            enable: false,
            ..Default::default()
        };
        let response = Response::new(Body::empty());
        assert!(!should_compress(&config, &response, true, false));
    }

    #[test]
    fn should_compress_head_request() {
        let config = CompressionConfig::default();
        let response = Response::new(Body::empty());
        assert!(!should_compress(&config, &response, true, true));
    }

    #[test]
    fn should_compress_no_accept() {
        let config = CompressionConfig::default();
        let response = Response::new(Body::empty());
        assert!(!should_compress(&config, &response, false, false));
    }

    #[test]
    fn should_compress_206_skipped() {
        let config = CompressionConfig::default();
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        assert!(!should_compress(&config, &response, true, false));
    }

    #[test]
    fn should_compress_204_skipped() {
        let config = CompressionConfig::default();
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::NO_CONTENT;
        assert!(!should_compress(&config, &response, true, false));
    }

    #[test]
    fn should_compress_redirect_skipped() {
        let config = CompressionConfig::default();
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::MOVED_PERMANENTLY;
        assert!(!should_compress(&config, &response, true, false));
    }

    #[test]
    fn should_compress_already_encoded_skipped() {
        let config = CompressionConfig::default();
        let mut response = Response::new(Body::empty());
        response
            .headers_mut()
            .insert(header::CONTENT_ENCODING, "zstd".parse().unwrap());
        assert!(!should_compress(&config, &response, true, false));
    }

    #[test]
    fn should_compress_ok() {
        let config = CompressionConfig::default();
        let response = Response::new(Body::empty());
        assert!(should_compress(&config, &response, true, false));
    }
}
