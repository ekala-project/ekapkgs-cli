//! Prometheus metrics for the binary cache server.

use prometheus::{
    Encoder, HistogramVec, IntCounter, IntCounterVec, IntGauge, Registry, TextEncoder,
    register_histogram_vec_with_registry, register_int_counter_vec_with_registry,
    register_int_counter_with_registry, register_int_gauge_with_registry,
};

/// All server metrics, registered with a single prometheus registry.
pub struct Metrics {
    pub registry: Registry,

    // Negotiate
    pub negotiate_requests_total: IntCounter,
    pub negotiate_paths_requested: HistogramVec,
    pub negotiate_paths_available: HistogramVec,

    // Narinfo serving
    pub narinfo_requests_total: IntCounterVec,

    // NAR downloads
    pub nar_downloads_total: IntCounter,
    pub nar_download_bytes_total: IntCounter,

    // Push
    pub push_narinfo_total: IntCounter,
    pub push_nar_total: IntCounter,
    pub push_nar_bytes_total: IntCounter,

    // GC
    pub gc_runs_total: IntCounter,
    pub gc_paths_evicted_total: IntCounter,
    pub gc_bytes_freed_total: IntCounter,

    // Cache state
    pub cache_size_bytes: IntGauge,
    pub cache_paths_total: IntGauge,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        let negotiate_requests_total = register_int_counter_with_registry!(
            "ekapkgs_negotiate_requests_total",
            "Total negotiate RPCs",
            registry
        )
        .expect("metric");

        let negotiate_paths_requested = register_histogram_vec_with_registry!(
            "ekapkgs_negotiate_paths_requested",
            "Paths in want set per negotiate request",
            &[],
            vec![1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0],
            registry
        )
        .expect("metric");

        let negotiate_paths_available = register_histogram_vec_with_registry!(
            "ekapkgs_negotiate_paths_available",
            "Paths found per negotiate request",
            &[],
            vec![1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0],
            registry
        )
        .expect("metric");

        let narinfo_requests_total = register_int_counter_vec_with_registry!(
            "ekapkgs_narinfo_requests_total",
            "GET narinfo requests",
            &["status"],
            registry
        )
        .expect("metric");

        let nar_downloads_total = register_int_counter_with_registry!(
            "ekapkgs_nar_downloads_total",
            "GET NAR requests",
            registry
        )
        .expect("metric");

        let nar_download_bytes_total = register_int_counter_with_registry!(
            "ekapkgs_nar_download_bytes_total",
            "NAR bytes served",
            registry
        )
        .expect("metric");

        let push_narinfo_total = register_int_counter_with_registry!(
            "ekapkgs_push_narinfo_total",
            "PUT narinfo requests",
            registry
        )
        .expect("metric");

        let push_nar_total = register_int_counter_with_registry!(
            "ekapkgs_push_nar_total",
            "PUT NAR requests",
            registry
        )
        .expect("metric");

        let push_nar_bytes_total = register_int_counter_with_registry!(
            "ekapkgs_push_nar_bytes_total",
            "NAR bytes received",
            registry
        )
        .expect("metric");

        let gc_runs_total = register_int_counter_with_registry!(
            "ekapkgs_gc_runs_total",
            "GC invocations",
            registry
        )
        .expect("metric");

        let gc_paths_evicted_total = register_int_counter_with_registry!(
            "ekapkgs_gc_paths_evicted_total",
            "Paths evicted by GC",
            registry
        )
        .expect("metric");

        let gc_bytes_freed_total = register_int_counter_with_registry!(
            "ekapkgs_gc_bytes_freed_total",
            "Bytes freed by GC",
            registry
        )
        .expect("metric");

        let cache_size_bytes = register_int_gauge_with_registry!(
            "ekapkgs_cache_size_bytes",
            "Current cache size in bytes",
            registry
        )
        .expect("metric");

        let cache_paths_total = register_int_gauge_with_registry!(
            "ekapkgs_cache_paths_total",
            "Current number of paths in cache",
            registry
        )
        .expect("metric");

        Self {
            registry,
            negotiate_requests_total,
            negotiate_paths_requested,
            negotiate_paths_available,
            narinfo_requests_total,
            nar_downloads_total,
            nar_download_bytes_total,
            push_narinfo_total,
            push_nar_total,
            push_nar_bytes_total,
            gc_runs_total,
            gc_paths_evicted_total,
            gc_bytes_freed_total,
            cache_size_bytes,
            cache_paths_total,
        }
    }

    /// Render all metrics in Prometheus text format.
    pub fn render(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        String::from_utf8(buffer).unwrap()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
