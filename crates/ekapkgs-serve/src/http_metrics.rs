//! Per-request HTTP metrics middleware.
//!
//! Records `http_requests_total` (counter) and `http_request_duration_seconds`
//! (histogram) with bounded cardinality labels: method, matched path pattern, status.

use axum::body::Body;
use axum::extract::MatchedPath;
use axum::http::{Request, Response};
use prometheus::{HistogramVec, IntCounterVec};
use tower::{Layer, Service};

/// Layer that records HTTP request metrics.
#[derive(Clone)]
pub struct HttpMetricsLayer {
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
}

impl HttpMetricsLayer {
    pub fn new(requests_total: IntCounterVec, request_duration: HistogramVec) -> Self {
        Self {
            requests_total,
            request_duration,
        }
    }
}

impl<S> Layer<S> for HttpMetricsLayer {
    type Service = HttpMetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        HttpMetricsService {
            inner,
            requests_total: self.requests_total.clone(),
            request_duration: self.request_duration.clone(),
        }
    }
}

#[derive(Clone)]
pub struct HttpMetricsService<S> {
    inner: S,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
}

impl<S> Service<Request<Body>> for HttpMetricsService<S>
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
        let requests_total = self.requests_total.clone();
        let request_duration = self.request_duration.clone();

        // Extract method and matched path before passing to inner.
        let method = normalize_method(req.method());
        let path_pattern = req
            .extensions()
            .get::<MatchedPath>()
            .map(|mp| mp.as_str().to_owned());

        let start = std::time::Instant::now();
        let mut inner = self.inner.clone();

        Box::pin(async move {
            let response = inner.call(req).await?;

            let status_code = response.status();
            let elapsed = start.elapsed().as_secs_f64();

            // Log error responses at appropriate levels.
            if status_code.is_server_error() {
                tracing::error!(
                    method,
                    path = path_pattern.as_deref().unwrap_or("unmatched"),
                    status = status_code.as_u16(),
                    elapsed_secs = elapsed,
                    "server error"
                );
            } else if status_code.is_client_error() {
                tracing::debug!(
                    method,
                    path = path_pattern.as_deref().unwrap_or("unmatched"),
                    status = status_code.as_u16(),
                    elapsed_secs = elapsed,
                    "client error"
                );
            }

            // Only record metrics for matched routes (bounded cardinality).
            if let Some(path) = path_pattern {
                let status = status_code.as_u16().to_string();

                requests_total
                    .with_label_values(&[method, &path, &status])
                    .inc();
                request_duration
                    .with_label_values(&[method, &path, &status])
                    .observe(elapsed);
            }

            Ok(response)
        })
    }
}

/// Normalize HTTP method to a fixed set for bounded cardinality.
fn normalize_method(method: &axum::http::Method) -> &'static str {
    match *method {
        axum::http::Method::GET => "GET",
        axum::http::Method::HEAD => "HEAD",
        axum::http::Method::POST => "POST",
        axum::http::Method::PUT => "PUT",
        _ => "other",
    }
}
