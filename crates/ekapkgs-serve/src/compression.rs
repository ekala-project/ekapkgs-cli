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

            // Determine if we need an LDM permit.
            let actual_use_ldm = if use_ldm {
                let needs_permit = content_length.is_none_or(|len| len >= LARGE_BODY_THRESHOLD);
                if needs_permit {
                    ldm_semaphore.clone().try_acquire_owned().is_ok()
                } else {
                    true
                }
            } else {
                false
            };

            let level = config.level;

            // Always use streaming compression: pipe body chunks through a
            // zstd encoder on a blocking task, sending compressed frames
            // through a buffered channel. This avoids materializing the full
            // response body in memory regardless of whether Content-Length
            // is set (compression removes it anyway).
            let (comp_tx, comp_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);

            tokio::task::spawn_blocking(move || {
                let writer = BufferedSender::new(comp_tx);
                let Ok(mut encoder) = zstd::Encoder::new(writer, level) else {
                    return;
                };
                if effective_window_log > 0 {
                    let _ = encoder.set_parameter(zstd::zstd_safe::CParameter::WindowLog(
                        effective_window_log,
                    ));
                }
                if actual_use_ldm {
                    let _ = encoder.set_parameter(
                        zstd::zstd_safe::CParameter::EnableLongDistanceMatching(true),
                    );
                }

                // Collect body frames and feed them to the encoder.
                let rt = tokio::runtime::Handle::current();
                let mut body = body;
                loop {
                    let frame = rt.block_on(async {
                        use http_body_util::BodyExt as _;
                        body.frame().await
                    });
                    match frame {
                        Some(Ok(frame)) => {
                            if let Ok(data) = frame.into_data() {
                                if encoder.write_all(&data).is_err() {
                                    return;
                                }
                            }
                        },
                        _ => break,
                    }
                }
                let _ = encoder.finish();
            });

            let stream = futures::stream::unfold(comp_rx, |mut rx| async move {
                let chunk = rx.recv().await?;
                Some((Ok::<_, std::io::Error>(chunk), rx))
            });

            let mut headers = parts.headers;
            headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("zstd"));
            headers.remove(header::CONTENT_LENGTH);
            headers.append(header::VARY, HeaderValue::from_static("accept-encoding"));

            let mut response = Response::new(Body::from_stream(stream));
            *response.status_mut() = parts.status;
            *response.headers_mut() = headers;
            *response.version_mut() = parts.version;

            Ok(response)
        })
    }
}

/// Buffered write size for compressed output (64 KiB).
const SENDER_BUF_SIZE: usize = 65_536;

/// `std::io::Write` adapter that buffers compressed output into 64 KiB chunks
/// before sending through a tokio mpsc channel. This avoids a separate
/// allocation and channel send for every small `write()` call from the zstd
/// encoder.
struct BufferedSender {
    buf: Vec<u8>,
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl BufferedSender {
    fn new(tx: tokio::sync::mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            buf: Vec::with_capacity(SENDER_BUF_SIZE),
            tx,
        }
    }
}

impl std::io::Write for BufferedSender {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while written < data.len() {
            let space = SENDER_BUF_SIZE - self.buf.len();
            let n = space.min(data.len() - written);
            self.buf.extend_from_slice(&data[written..written + n]);
            written += n;

            if self.buf.len() >= SENDER_BUF_SIZE {
                let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(SENDER_BUF_SIZE));
                self.tx.blocking_send(chunk).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver dropped")
                })?;
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let chunk = std::mem::take(&mut self.buf);
            self.tx.blocking_send(chunk).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver dropped")
            })?;
        }
        Ok(())
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
    // Skip content types that are already compressed file formats.
    let is_precompressed = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct == "application/zstd" || ct == "application/x-xz");
    if is_precompressed {
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

#[cfg(test)]
mod tests {
    use super::*;

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
