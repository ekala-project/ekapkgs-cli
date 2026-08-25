mod api;
mod config;
mod gc;
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
    pub gc_tracker: Option<Arc<gc::GcTracker>>,
    pub write_tokens: Option<Vec<String>>,
    pub delta_cache: DeltaCache,
}

/// Cache for computed delta NARs, keyed by (base_hash, target_hash).
///
/// Populated during negotiate when the server finds a suitable delta candidate,
/// consumed by the delta HTTP endpoint and StreamNars handler.
pub struct DeltaCache {
    entries: std::sync::Mutex<std::collections::HashMap<(String, String), Vec<u8>>>,
}

impl DeltaCache {
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    pub fn insert(&self, base_hash: String, target_hash: String, delta: Vec<u8>) {
        self.entries
            .lock()
            .expect("delta cache lock")
            .insert((base_hash, target_hash), delta);
    }

    pub fn get(&self, base_hash: &str, target_hash: &str) -> Option<Vec<u8>> {
        self.entries
            .lock()
            .expect("delta cache lock")
            .get(&(base_hash.to_owned(), target_hash.to_owned()))
            .cloned()
    }

    /// Find any cached delta targeting the given hash.
    pub fn get_for_target(&self, target_hash: &str) -> Option<Vec<u8>> {
        let entries = self.entries.lock().expect("delta cache lock");
        entries
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

fn build_http_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/nix-cache-info", get(api::compat::nix_cache_info))
        .route(
            "/{hash_narinfo}",
            get(api::compat::get_narinfo).put(api::upload::put_narinfo),
        )
        .route(
            "/nar/{file}",
            get(api::compat::get_nar).put(api::upload::put_nar),
        )
        .route(
            "/cas/chunk/{b3hex}",
            get(api::chunks::get_chunk).put(api::chunks::put_chunk),
        )
        .route(
            "/delta/{base_hash}/{target_hash}",
            get(api::delta::get_delta),
        )
        .with_state(state)
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

    std::fs::write(&secret_path, format!("{name}:{secret_b64}\n"))?;
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

    // Write the signing secret key.
    let key_path = output.join(format!("{name}.key"));
    let key_b64 = BASE64.encode(cert_secret.as_bytes());
    std::fs::write(&key_path, format!("{name}:{key_b64}\n"))?;

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
    let gc_tracker: Option<Arc<gc::GcTracker>>;
    let write_tokens: Option<Vec<String>>;

    if let Some(config_path) = &cli.config {
        let config = Config::load(config_path)?;
        bind_addr = cli.bind.unwrap_or(config.server.bind);
        signer = NarInfoSigner::from_file(&config.signing.secret_key_file)?;
        cert_signer = if let Some(ref cert_config) = config.signing.certificate {
            Some(signing::CertSigner::from_files(
                &cert_config.cert_file,
                &cert_config.private_key_file,
            )?)
        } else {
            None
        };
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
                    Some(gc::init(&path, gc_config)?)
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
                    Some(gc::init(&path, gc_config)?)
                } else {
                    None
                };
                gc_tracker = gc_t;
                Box::new(storage::castore::CastoreBackend::new(path)?)
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
    } else {
        bind_addr = cli.bind.unwrap_or_else(|| "0.0.0.0:8080".to_owned());

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

        let storage_str = cli.storage.unwrap_or_else(|| "nix-store".to_owned());
        storage_backend = if storage_str == "nix-store" {
            Box::new(storage::nix_store::NixStoreBackend::new())
        } else {
            Box::new(storage::filesystem::FilesystemBackend::new(PathBuf::from(
                storage_str,
            )))
        };
    }

    let state = Arc::new(AppState {
        storage: storage_backend,
        signer,
        cert_signer,
        gc_tracker,
        write_tokens,
        delta_cache: DeltaCache::new(),
    });

    let addr: SocketAddr = bind_addr.parse()?;

    let grpc_service = CacheServiceServer::new(api::negotiate::NegotiateService {
        state: Arc::clone(&state),
    });

    let app = build_http_router(state)
        .route_service("/ekapkgs.v1.CacheService/Negotiate", grpc_service.clone())
        .route_service(
            "/ekapkgs.v1.CacheService/NegotiateChunks",
            grpc_service.clone(),
        )
        .route_service("/ekapkgs.v1.CacheService/StreamNars", grpc_service);

    tracing::info!("Listening on {addr} (gRPC + HTTP)");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
