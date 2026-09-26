mod api;
mod compression;
mod config;
mod gc;
mod http_metrics;
pub mod metrics;
mod signing;
mod storage;
mod tokens;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use clap::{Parser, Subcommand};
use config::Config;
use ekapkgs_protocol::ekapkgs::v1::cache_service_server::CacheServiceServer;
use signing::NarInfoSigner;
use storage::StorageBackend;

#[derive(Parser)]
#[command(name = "ekapkgs-serve", about = "ekapkgs binary cache server")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Path to config file.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Bind address (overrides config).
    #[arg(short, long, global = true)]
    bind: Option<String>,

    /// Storage backend: "nix-store" or path to cache directory.
    #[arg(short, long, global = true)]
    storage: Option<String>,

    /// Path to nix signing key file.
    #[arg(long, global = true)]
    signing_key: Option<PathBuf>,

    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity,
}

#[derive(Subcommand)]
enum Command {
    /// Start the binary cache server (default).
    Serve,

    /// Generate a new root CA keypair for certificate-based signing.
    GenerateCa {
        /// Name for the CA (e.g., "ekapkgs-root-ca-1").
        name: String,
        /// Output directory for the keypair files.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },

    /// Manage API tokens for cache access.
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },

    /// Issue a signing certificate signed by a CA.
    IssueCert {
        /// Name for the certificate (e.g., "cache.example.org-2025").
        name: String,
        /// Path to the CA secret key file.
        #[arg(long)]
        ca_key: PathBuf,
        /// Name of the CA (must match the CA key name).
        #[arg(long)]
        ca_name: String,
        /// Validity duration in days.
        #[arg(long, default_value = "365")]
        days: u64,
        /// Output directory for the certificate and key files.
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Create a new API token.
    Create {
        /// Human-readable name for the token (e.g., "ci-main", "jon-laptop").
        name: String,
        /// Create a read-only token (no push permission).
        #[arg(long)]
        read_only: bool,
    },

    /// List all tokens.
    List,

    /// Revoke a token by name.
    Revoke {
        /// Name of the token to revoke.
        name: String,
    },
}

pub struct AppState {
    pub storage: Box<dyn StorageBackend>,
    pub signer: NarInfoSigner,
    pub cert_signer: Option<signing::CertSigner>,
    /// Additional certificate signers for threshold signing.
    pub cert_signers: Vec<signing::CertSigner>,
    /// Threshold: require this many valid cert signatures. 0 = no threshold.
    pub signing_threshold: u32,
    pub gc_tracker: Option<Arc<gc::GcTracker>>,
    pub write_tokens: Option<Vec<String>>,
    pub delta_cache: DeltaCache,
    pub metrics: metrics::Metrics,
    /// Cache priority advertised in nix-cache-info.
    pub priority: u32,
    /// Virtual store directory advertised to clients.
    pub store_dir: String,
}

/// Cache for computed delta NARs, keyed by (base_hash, target_hash).
///
/// Populated during negotiate when the server finds a suitable delta candidate,
/// consumed by the delta HTTP endpoint and StreamNars handler.
///
/// Capped at 256 MiB total. When the cap is exceeded, the oldest entries are
/// evicted until usage drops below the limit.
pub struct DeltaCache {
    entries: std::sync::Mutex<DeltaCacheInner>,
}

struct DeltaCacheInner {
    /// Entries in insertion order (oldest first).
    entries: Vec<((String, String), Vec<u8>)>,
    total_bytes: usize,
}

/// Maximum total bytes stored in the delta cache.
const DELTA_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;

impl DeltaCache {
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(DeltaCacheInner {
                entries: Vec::new(),
                total_bytes: 0,
            }),
        }
    }

    pub fn insert(&self, base_hash: String, target_hash: String, delta: Vec<u8>) {
        let mut inner = self.entries.lock().expect("delta cache lock");
        let delta_len = delta.len();

        // Remove existing entry for this key if present.
        if let Some(pos) = inner
            .entries
            .iter()
            .position(|((b, t), _)| b == &base_hash && t == &target_hash)
        {
            let (_, old) = inner.entries.remove(pos);
            inner.total_bytes -= old.len();
        }

        // Evict oldest entries until we have room.
        while inner.total_bytes + delta_len > DELTA_CACHE_MAX_BYTES && !inner.entries.is_empty() {
            let (_, evicted) = inner.entries.remove(0);
            inner.total_bytes -= evicted.len();
        }

        inner.total_bytes += delta_len;
        inner.entries.push(((base_hash, target_hash), delta));
    }

    pub fn get(&self, base_hash: &str, target_hash: &str) -> Option<Vec<u8>> {
        let inner = self.entries.lock().expect("delta cache lock");
        inner
            .entries
            .iter()
            .find(|((b, t), _)| b == base_hash && t == target_hash)
            .map(|(_, delta)| delta.clone())
    }

    /// Find any cached delta targeting the given hash.
    pub fn get_for_target(&self, target_hash: &str) -> Option<Vec<u8>> {
        let inner = self.entries.lock().expect("delta cache lock");
        inner
            .entries
            .iter()
            .find(|((_, t), _)| t == target_hash)
            .map(|(_, delta)| delta.clone())
    }
}

impl Default for DeltaCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Add security headers to all HTTP responses.
async fn security_headers(mut response: axum::response::Response) -> axum::response::Response {
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        "nosniff".parse().unwrap(),
    );
    headers.insert(axum::http::header::X_FRAME_OPTIONS, "DENY".parse().unwrap());
    headers.insert(
        axum::http::header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; style-src 'unsafe-inline'"
            .parse()
            .unwrap(),
    );
    response
}

async fn metrics_handler(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
) -> impl axum::response::IntoResponse {
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        state.metrics.render(),
    )
}

fn build_http_router(
    state: Arc<AppState>,
    compression_config: config::CompressionConfig,
    enable_metrics: bool,
    request_timeout: std::time::Duration,
) -> Router {
    use axum::extract::DefaultBodyLimit;

    // 1 MiB limit for narinfo metadata uploads.
    const NARINFO_BODY_LIMIT: usize = 1024 * 1024;
    // 8 GiB limit for NAR file uploads.
    const NAR_BODY_LIMIT: usize = 8 * 1024 * 1024 * 1024;
    // 16 MiB limit for CAS chunk uploads.
    const CHUNK_BODY_LIMIT: usize = 16 * 1024 * 1024;

    let mut router = Router::new()
        .route("/", get(api::compat::root))
        .route("/health", get(api::compat::health))
        .route("/version", get(api::compat::version))
        .route("/nix-cache-info", get(api::compat::nix_cache_info))
        .route(
            "/{hash_narinfo}",
            get(api::compat::get_narinfo)
                .put(api::upload::put_narinfo)
                .layer(DefaultBodyLimit::max(NARINFO_BODY_LIMIT)),
        )
        .route(
            "/nar/{file}",
            get(api::compat::get_nar)
                .put(api::upload::put_nar)
                .layer(DefaultBodyLimit::max(NAR_BODY_LIMIT)),
        )
        .route("/serve/{hash}/{*tail}", get(api::serve::get_serve))
        .route("/log/{drv}", get(api::logs::get_log))
        .route(
            "/cas/chunk/{b3hex}",
            get(api::chunks::get_chunk)
                .put(api::chunks::put_chunk)
                .layer(DefaultBodyLimit::max(CHUNK_BODY_LIMIT)),
        )
        .route(
            "/delta/{base_hash}/{target_hash}",
            get(api::delta::get_delta),
        );

    if enable_metrics {
        router = router.route("/metrics", get(metrics_handler));
    }

    let router = router
        .layer(http_metrics::HttpMetricsLayer::new(
            state.metrics.http_requests_total.clone(),
            state.metrics.http_request_duration_seconds.clone(),
        ))
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        .layer(axum::middleware::map_response(security_headers))
        .with_state(state);

    if compression_config.enable {
        router.layer(compression::ZstdCompressionLayer::new(compression_config))
    } else {
        router
    }
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let mut cli = Cli::parse();
    ekapkgs_ui::logging::init(&cli.verbose);

    let command = cli.command.take().unwrap_or(Command::Serve);
    match command {
        Command::GenerateCa { name, output } => cmd_generate_ca(&name, &output),
        Command::IssueCert {
            name,
            ca_key,
            ca_name,
            days,
            output,
        } => cmd_issue_cert(&name, &ca_key, &ca_name, days, &output),
        Command::Token { command } => cmd_token(command, cli.config.as_deref()),
        Command::Serve => cmd_serve(cli).await,
    }
}

fn cmd_generate_ca(name: &str, output: &std::path::Path) -> color_eyre::Result<()> {
    use data_encoding::BASE64;
    use ekapkgs_protocol::signing::generate_keypair;

    let (secret, public) = generate_keypair();

    let secret_path = output.join(format!("{name}.sec"));
    let public_path = output.join(format!("{name}.pub"));

    let secret_b64 = BASE64.encode(secret.as_bytes());
    let public_b64 = BASE64.encode(public.as_bytes());

    tokens::write_secret_file(&secret_path, format!("{name}:{secret_b64}\n").as_bytes())?;
    std::fs::write(&public_path, format!("{name}:{public_b64}\n"))?;

    tracing::info!("CA keypair generated:");
    tracing::info!("  Secret: {}", secret_path.display());
    tracing::info!("  Public: {}", public_path.display());
    tracing::info!("  Trust root: {name}:{public_b64}");

    Ok(())
}

fn cmd_issue_cert(
    name: &str,
    ca_key_path: &std::path::Path,
    ca_name: &str,
    days: u64,
    output: &std::path::Path,
) -> color_eyre::Result<()> {
    use data_encoding::BASE64;
    use ed25519_dalek::SigningKey;
    use ekapkgs_protocol::signing::{generate_keypair, issue_certificate};

    // Load CA secret key.
    let ca_key_contents = std::fs::read_to_string(ca_key_path)?.trim().to_owned();
    let (_ca_key_name, ca_key_b64) = ca_key_contents
        .split_once(':')
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid CA key format"))?;
    let ca_key_bytes = BASE64.decode(ca_key_b64.as_bytes())?;
    let ca_secret: [u8; 32] = ca_key_bytes[..32].try_into()?;
    let ca_signing_key = SigningKey::from_bytes(&ca_secret);

    // Generate a new signing keypair for the certificate.
    let (cert_secret, cert_public) = generate_keypair();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let not_after = now + days * 86400;

    let cert = issue_certificate(&ca_signing_key, ca_name, name, &cert_public, now, not_after);

    // Write certificate as JSON.
    let cert_path = output.join(format!("{name}.cert.json"));
    let cert_json = serde_json::to_string_pretty(&CertFile {
        name: cert.name.clone(),
        public_key: BASE64.encode(&cert.public_key),
        not_before: cert.not_before,
        not_after: cert.not_after,
        issuer: cert.issuer.clone(),
        issuer_signature: BASE64.encode(&cert.issuer_signature),
    })?;
    std::fs::write(&cert_path, &cert_json)?;

    // Write the signing secret key with restrictive permissions.
    let key_path = output.join(format!("{name}.key"));
    let key_b64 = BASE64.encode(cert_secret.as_bytes());
    tokens::write_secret_file(&key_path, format!("{name}:{key_b64}\n").as_bytes())?;

    tracing::info!("Signing certificate issued:");
    tracing::info!("  Certificate: {}", cert_path.display());
    tracing::info!("  Private key: {}", key_path.display());
    tracing::info!("  Valid for: {days} days");

    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CertFile {
    name: String,
    public_key: String,
    not_before: u64,
    not_after: u64,
    issuer: String,
    issuer_signature: String,
}

fn cmd_token(
    command: TokenCommand,
    config_path: Option<&std::path::Path>,
) -> color_eyre::Result<()> {
    let store_path = tokens::default_store_path(config_path);

    match command {
        TokenCommand::Create { name, read_only } => {
            let mut store = tokens::TokenStore::load(&store_path)?;

            let permissions = tokens::Permissions {
                read: true,
                write: !read_only,
            };

            let token_value = store.create(&name, permissions)?;
            store.save(&store_path)?;

            // Print the token — this is the only time it's shown in full.
            println!("{token_value}");
            tracing::info!(
                "Token '{name}' created ({})",
                if read_only { "read-only" } else { "read+write" }
            );

            Ok(())
        },

        TokenCommand::List => {
            let store = tokens::TokenStore::load(&store_path)?;

            if store.tokens.is_empty() {
                tracing::info!("No tokens configured");
                return Ok(());
            }

            for token in &store.tokens {
                let perms = if token.permissions.write {
                    "read+write"
                } else {
                    "read-only"
                };
                let preview = &token.token[..std::cmp::min(12, token.token.len())];
                tracing::info!(
                    "{} — {perms} — {preview}... — created {}",
                    token.name,
                    format_timestamp(token.created_at),
                );
            }

            Ok(())
        },

        TokenCommand::Revoke { name } => {
            let mut store = tokens::TokenStore::load(&store_path)?;

            if store.revoke(&name) {
                store.save(&store_path)?;
                tracing::info!("Token '{name}' revoked");
            } else {
                tracing::warn!("No token named '{name}' found");
            }

            Ok(())
        },
    }
}

fn format_timestamp(unix: u64) -> String {
    // Simple ISO-ish date without pulling in chrono.
    let secs_per_day = 86400u64;
    let days_since_epoch = unix / secs_per_day;
    // Approximate — good enough for display.
    let year = 1970 + days_since_epoch / 365;
    let remaining = days_since_epoch % 365;
    let month = remaining / 30 + 1;
    let day = remaining % 30 + 1;
    format!("{year}-{month:02}-{day:02}")
}

async fn cmd_serve(cli: Cli) -> color_eyre::Result<()> {
    let bind_addr: String;
    let storage_backend: Box<dyn StorageBackend>;
    let signer: NarInfoSigner;
    let cert_signer: Option<signing::CertSigner>;
    let mut extra_signers: Vec<signing::CertSigner> = Vec::new();
    let mut threshold: u32 = 0;
    let gc_tracker: Option<Arc<gc::GcTracker>>;
    let write_tokens: Option<Vec<String>>;
    let compression_config: config::CompressionConfig;
    let tls_cert_path: Option<PathBuf>;
    let tls_key_path: Option<PathBuf>;
    let mut enable_metrics: bool = true;
    let mut request_timeout_secs: u64 = 30;
    let mut priority: u32 = 30;
    let mut store_dir: String = "/nix/store".to_owned();
    let server_metrics = metrics::Metrics::new();

    let gc_metrics = gc::GcMetrics {
        runs_total: server_metrics.gc_runs_total.clone(),
        paths_evicted_total: server_metrics.gc_paths_evicted_total.clone(),
        bytes_freed_total: server_metrics.gc_bytes_freed_total.clone(),
        cache_size_bytes: server_metrics.cache_size_bytes.clone(),
        cache_paths_total: server_metrics.cache_paths_total.clone(),
    };

    if let Some(config_path) = &cli.config {
        let config = Config::load(config_path)?;
        bind_addr = cli.bind.unwrap_or(config.server.bind);
        warn_insecure_key_permissions(&config.signing.secret_key_file);
        signer = NarInfoSigner::from_file(&config.signing.secret_key_file)?;
        cert_signer = if let Some(ref cert_config) = config.signing.certificate {
            warn_insecure_key_permissions(&cert_config.private_key_file);
            Some(signing::CertSigner::from_files(
                &cert_config.cert_file,
                &cert_config.private_key_file,
            )?)
        } else {
            None
        };
        // Load additional certificate signers for threshold signing.
        for cert_config in &config.signing.certificates {
            warn_insecure_key_permissions(&cert_config.private_key_file);
            extra_signers.push(signing::CertSigner::from_files(
                &cert_config.cert_file,
                &cert_config.private_key_file,
            )?);
        }
        threshold = config.signing.threshold.unwrap_or(0);
        storage_backend = match config.storage {
            config::StorageConfig::Filesystem { path, gc } => {
                let gc_t = if let Some(gc_raw) = gc {
                    let max_size = gc::parse_byte_size(&gc_raw.max_size)?;
                    let target_size = gc_raw
                        .target_size
                        .as_deref()
                        .map(gc::parse_byte_size)
                        .transpose()?
                        .unwrap_or(max_size * 4 / 5); // 80% default
                    let gc_config = gc::GcConfig {
                        max_size,
                        target_size,
                        gc_interval: std::time::Duration::from_secs(gc_raw.gc_interval_secs),
                    };
                    Some(gc::init(&path, gc_config, Some(gc_metrics.clone()))?)
                } else {
                    None
                };
                gc_tracker = gc_t;
                Box::new(storage::filesystem::FilesystemBackend::new(path))
            },
            config::StorageConfig::NixStore => {
                gc_tracker = None;
                Box::new(storage::nix_store::NixStoreBackend::new())
            },
            #[cfg(feature = "s3")]
            config::StorageConfig::S3 {
                bucket,
                region,
                endpoint,
                prefix,
            } => {
                gc_tracker = None;
                let s3_config = storage::s3::S3Config {
                    bucket,
                    region,
                    endpoint,
                    prefix,
                };
                Box::new(
                    tokio::runtime::Handle::current()
                        .block_on(storage::s3::S3Backend::new(s3_config))?,
                )
            },
            #[cfg(not(feature = "s3"))]
            config::StorageConfig::S3 { .. } => {
                return Err(color_eyre::eyre::eyre!(
                    "S3 storage backend requires the 's3' feature. Rebuild with: cargo build \
                     --features s3"
                ));
            },
            config::StorageConfig::Castore { path, gc } => {
                let backend = Arc::new(storage::castore::CastoreBackend::new(path)?);
                let gc_t = if let Some(gc_raw) = gc {
                    let max_size = gc::parse_byte_size(&gc_raw.max_size)?;
                    let target_size = gc_raw
                        .target_size
                        .as_deref()
                        .map(gc::parse_byte_size)
                        .transpose()?
                        .unwrap_or(max_size * 4 / 5);
                    let gc_config = gc::GcConfig {
                        max_size,
                        target_size,
                        gc_interval: std::time::Duration::from_secs(gc_raw.gc_interval_secs),
                    };
                    Some(gc::init_cas(
                        Arc::clone(&backend),
                        gc_config,
                        Some(gc_metrics.clone()),
                    ))
                } else {
                    None
                };
                gc_tracker = gc_t;
                Box::new(backend) as Box<dyn storage::StorageBackend>
            },
        };
        // Load tokens: from token store + any legacy config tokens.
        let store_path = tokens::default_store_path(Some(config_path));
        let token_store = tokens::TokenStore::load(&store_path)?;
        let mut all_tokens = token_store.write_tokens();
        if let Some(auth) = config.auth {
            all_tokens.extend(auth.write_tokens);
        }
        write_tokens = if all_tokens.is_empty() {
            None
        } else {
            Some(all_tokens)
        };
        compression_config = config.compression;
        tls_cert_path = config.server.tls_cert_path;
        tls_key_path = config.server.tls_key_path;
        enable_metrics = config.server.enable_metrics;
        request_timeout_secs = config.server.client_request_timeout_secs;
        priority = config.server.priority;
    } else {
        bind_addr = cli.bind.unwrap_or_else(|| "127.0.0.1:8080".to_owned());

        let signing_key = cli.signing_key.ok_or_else(|| {
            color_eyre::eyre::eyre!("either --config or --signing-key is required")
        })?;
        signer = NarInfoSigner::from_file(&signing_key)?;
        cert_signer = None;
        gc_tracker = None;

        // Load tokens from default location.
        let store_path = tokens::default_store_path(cli.config.as_deref());
        let token_store = tokens::TokenStore::load(&store_path)?;
        let all_tokens = token_store.write_tokens();
        write_tokens = if all_tokens.is_empty() {
            None
        } else {
            Some(all_tokens)
        };

        compression_config = config::CompressionConfig::default();
        tls_cert_path = None;
        tls_key_path = None;

        let storage_str = cli.storage.unwrap_or_else(|| "nix-store".to_owned());
        storage_backend = if storage_str == "nix-store" {
            Box::new(storage::nix_store::NixStoreBackend::new())
        } else {
            Box::new(storage::filesystem::FilesystemBackend::new(PathBuf::from(
                storage_str,
            )))
        };
    }

    // Environment variable overlays.
    if let Ok(nix_store) = std::env::var("NIX_STORE_DIR") {
        store_dir = nix_store;
    }

    let state = Arc::new(AppState {
        storage: storage_backend,
        signer,
        cert_signer,
        cert_signers: extra_signers,
        signing_threshold: threshold,
        gc_tracker,
        write_tokens,
        delta_cache: DeltaCache::new(),
        metrics: server_metrics,
        priority,
        store_dir,
    });

    // Validate TLS config consistency.
    let use_tls = match (&tls_cert_path, &tls_key_path) {
        (Some(_), Some(_)) => true,
        (None, None) => false,
        _ => {
            return Err(color_eyre::eyre::eyre!(
                "both tls_cert_path and tls_key_path must be set, or neither"
            ));
        },
    };

    let is_unix = bind_addr.starts_with("unix:");
    if use_tls && is_unix {
        return Err(color_eyre::eyre::eyre!(
            "TLS is not compatible with Unix socket binding"
        ));
    }

    // Warn on insecure key file permissions.
    if let Some(ref key_path) = tls_key_path {
        warn_insecure_key_permissions(key_path);
    }

    let grpc_service = CacheServiceServer::new(api::negotiate::NegotiateService {
        state: Arc::clone(&state),
    })
    .max_decoding_message_size(64 * 1024 * 1024)
    .max_encoding_message_size(64 * 1024 * 1024);

    let request_timeout = std::time::Duration::from_secs(request_timeout_secs);
    let app = build_http_router(state, compression_config, enable_metrics, request_timeout)
        .route_service("/ekapkgs.v1.CacheService/Negotiate", grpc_service.clone())
        .route_service(
            "/ekapkgs.v1.CacheService/NegotiateChunks",
            grpc_service.clone(),
        )
        .route_service("/ekapkgs.v1.CacheService/StreamNars", grpc_service);

    // Check for systemd socket activation.
    let inherited_listener = try_socket_activation()?;

    if let Some(listener) = inherited_listener {
        tracing::info!("Using systemd socket activation");
        notify_ready();
        spawn_watchdog();
        axum::serve(listener, app).await?;
    } else if is_unix {
        let socket_path = bind_addr.strip_prefix("unix:").unwrap();
        // Remove stale socket file.
        let _ = std::fs::remove_file(socket_path);
        let listener = tokio::net::UnixListener::bind(socket_path)?;
        // Restrict socket to owner and group.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o660))?;
        }
        tracing::info!("Listening on unix:{socket_path} (gRPC + HTTP)");
        notify_ready();
        spawn_watchdog();
        axum::serve(listener, app.into_make_service()).await?;
    } else if use_tls {
        let cert_path = tls_cert_path.unwrap();
        let key_path = tls_key_path.unwrap();
        let addr: SocketAddr = bind_addr.parse()?;

        let rustls_config =
            axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_path, &key_path).await?;

        tracing::info!("Listening on {addr} with TLS (gRPC + HTTP)");
        notify_ready();
        spawn_watchdog();
        axum_server::bind_rustls(addr, rustls_config)
            .serve(app.into_make_service())
            .await?;
    } else {
        let addr: SocketAddr = bind_addr.parse()?;
        tracing::info!("Listening on {addr} (gRPC + HTTP)");
        let listener = tokio::net::TcpListener::bind(addr).await?;
        notify_ready();
        spawn_watchdog();
        axum::serve(listener, app).await?;
    };

    Ok(())
}

/// Warn if a key file has world/group-readable permissions.
#[cfg(unix)]
fn warn_insecure_key_permissions(path: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.mode() & 0o077 != 0 {
            tracing::warn!(
                "Key file {:?} has insecure permissions (mode {:o}). Consider chmod 600.",
                path,
                meta.mode() & 0o777
            );
        }
    }
}

#[cfg(not(unix))]
fn warn_insecure_key_permissions(_path: &std::path::Path) {}

/// Send `READY=1` to systemd if the notify socket is available.
fn notify_ready() {
    if let Err(e) = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]) {
        tracing::warn!("sd_notify READY=1 failed (non-fatal): {e}");
    }
}

/// Spawn a watchdog task that pings systemd at half the configured interval.
fn spawn_watchdog() {
    let Some(watchdog_usec) = std::env::var("WATCHDOG_USEC")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return;
    };

    let interval =
        std::time::Duration::from_micros(watchdog_usec / 2).max(std::time::Duration::from_secs(1));

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Watchdog]);
        }
    });
}

/// Attempt systemd socket activation via inherited file descriptors.
///
/// Uses the `listenfd` crate which handles LISTEN_PID checking, FD_CLOEXEC,
/// and non-blocking mode safely.
fn try_socket_activation() -> color_eyre::Result<Option<tokio::net::TcpListener>> {
    let mut listenfd = listenfd::ListenFd::from_env();

    match listenfd.take_tcp_listener(0) {
        Ok(Some(std_listener)) => {
            std_listener.set_nonblocking(true)?;
            let listener = tokio::net::TcpListener::from_std(std_listener)?;
            Ok(Some(listener))
        },
        Ok(None) | Err(_) => Ok(None),
    }
}
