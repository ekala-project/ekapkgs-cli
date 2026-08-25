//! Local substituter proxy for transparent nix integration.
//!
//! Runs a lightweight HTTP server implementing the nix binary cache protocol.
//! Nix talks to it via `substituters = http://localhost:PORT`. The proxy
//! batches narinfo queries, negotiates with the upstream ekapkgs server using
//! the gRPC protocol, and proxies NAR downloads.
//!
//! Usage:
//!   ekapkgs substituter --port 7422
//!   # then in nix.conf or flake:
//!   substituters = http://localhost:7422

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ekapkgs_protocol::ekapkgs::v1::{NegotiateResponse, PathManifestEntry};
use tokio::sync::Mutex;

use crate::config::ClientConfig;

/// How long to cache negotiate results before re-querying.
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Shared state for the proxy server.
struct ProxyState {
    /// Upstream ekapkgs server URL.
    upstream: String,
    /// HTTP client for proxying NAR downloads.
    http_client: reqwest::Client,
    /// Cache of negotiate results: hash → (entry, fetched_at).
    narinfo_cache: Mutex<HashMap<String, (PathManifestEntry, Instant)>>,
    /// The upstream server's base HTTP URL (for NAR proxying).
    upstream_http: String,
}

impl ProxyState {
    /// Look up a narinfo, negotiating with upstream if not cached.
    async fn get_narinfo(&self, hash: &str) -> Option<PathManifestEntry> {
        // Check cache first.
        {
            let cache = self.narinfo_cache.lock().await;
            if let Some((entry, fetched_at)) = cache.get(hash) {
                if fetched_at.elapsed() < CACHE_TTL {
                    return Some(entry.clone());
                }
            }
        }

        // Not cached — negotiate with upstream.
        self.negotiate_for_hash(hash).await;

        // Check cache again after negotiation.
        let cache = self.narinfo_cache.lock().await;
        cache.get(hash).map(|(entry, _)| entry.clone())
    }

    /// Negotiate with the upstream server for a single hash.
    /// Caches all results from the response.
    async fn negotiate_for_hash(&self, hash: &str) {
        let response =
            match crate::negotiate::negotiate(&self.upstream, vec![hash.to_owned()], Vec::new())
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Negotiate failed for {hash}: {e}");
                    return;
                },
            };

        self.cache_response(&response).await;
    }

    /// Cache all entries from a negotiate response.
    async fn cache_response(&self, response: &NegotiateResponse) {
        let now = Instant::now();
        let mut cache = self.narinfo_cache.lock().await;
        for entry in &response.available {
            if let Some(hash) = entry
                .store_path
                .rsplit('/')
                .next()
                .and_then(|b| b.split('-').next())
            {
                cache.insert(hash.to_owned(), (entry.clone(), now));
            }
        }
    }
}

pub fn execute(port: u16, upstream: Option<String>) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;

    let upstream_url = match upstream {
        Some(url) => url,
        None => {
            let cache = config.primary_cache().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "no cache configured — use --upstream or configure in config.toml"
                )
            })?;
            cache.url.clone()
        },
    };

    // Derive the HTTP base URL from the upstream gRPC URL.
    let upstream_http = upstream_url
        .replace("grpc://", "http://")
        .replace("grpcs://", "https://");

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let state = Arc::new(ProxyState {
            upstream: upstream_url,
            http_client: reqwest::Client::new(),
            narinfo_cache: Mutex::new(HashMap::new()),
            upstream_http,
        });

        let app = Router::new()
            .route("/nix-cache-info", get(nix_cache_info))
            .route("/{hash_narinfo}", get(get_narinfo))
            .route("/nar/{file}", get(get_nar))
            .with_state(state);

        let addr = format!("127.0.0.1:{port}");
        tracing::info!("Substituter proxy listening on http://{addr}");
        tracing::info!("Add to nix.conf: substituters = http://{addr}");

        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    })
}

/// GET /nix-cache-info
async fn nix_cache_info() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/x-nix-cache-info")],
        "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 20\n",
    )
}

/// GET /{hash}.narinfo
async fn get_narinfo(
    State(state): State<Arc<ProxyState>>,
    Path(hash_narinfo): Path<String>,
) -> Response {
    let hash = hash_narinfo
        .strip_suffix(".narinfo")
        .unwrap_or(&hash_narinfo);

    let Some(entry) = state.get_narinfo(hash).await else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };

    // Build narinfo text from the manifest entry.
    let narinfo = build_narinfo(&entry, hash);

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/x-nix-narinfo")],
        narinfo,
    )
        .into_response()
}

/// GET /nar/{file}
async fn get_nar(State(state): State<Arc<ProxyState>>, Path(file): Path<String>) -> Response {
    // Proxy the NAR download to the upstream server.
    let nar_url = format!("{}/nar/{file}", state.upstream_http);

    let resp = match state.http_client.get(&nar_url).send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("NAR proxy failed: {e}");
            return (StatusCode::BAD_GATEWAY, "upstream error").into_response();
        },
    };

    if !resp.status().is_success() {
        return (
            StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            "upstream error",
        )
            .into_response();
    }

    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/x-nix-nar")
        .to_owned();

    match resp.bytes().await {
        Ok(data) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            data.to_vec(),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("NAR proxy read failed: {e}");
            (StatusCode::BAD_GATEWAY, "upstream error").into_response()
        },
    }
}

/// Build a narinfo text string from a PathManifestEntry.
fn build_narinfo(entry: &PathManifestEntry, hash: &str) -> String {
    let refs: Vec<&str> = entry
        .references
        .iter()
        .map(|r| r.rsplit('/').next().unwrap_or(r.as_str()))
        .collect();

    let compression = match entry.compression {
        c if c == ekapkgs_protocol::ekapkgs::v1::Compression::Zstd as i32 => "zstd",
        c if c == ekapkgs_protocol::ekapkgs::v1::Compression::Xz as i32 => "xz",
        _ => "none",
    };

    let mut narinfo = String::new();
    narinfo.push_str(&format!("StorePath: {}\n", entry.store_path));
    narinfo.push_str(&format!("URL: {}\n", entry.url));
    narinfo.push_str(&format!("Compression: {compression}\n"));
    if !entry.file_hash.is_empty() {
        narinfo.push_str(&format!("FileHash: {}\n", entry.file_hash));
    }
    if entry.file_size > 0 {
        narinfo.push_str(&format!("FileSize: {}\n", entry.file_size));
    }
    narinfo.push_str(&format!("NarHash: {}\n", entry.nar_hash));
    narinfo.push_str(&format!("NarSize: {}\n", entry.nar_size));
    if !refs.is_empty() {
        narinfo.push_str(&format!("References: {}\n", refs.join(" ")));
    }
    for sig in &entry.signatures {
        narinfo.push_str(&format!("Sig: {sig}\n"));
    }
    if !entry.ca.is_empty() {
        narinfo.push_str(&format!("CA: {}\n", entry.ca));
    }
    let _ = hash;
    narinfo
}
