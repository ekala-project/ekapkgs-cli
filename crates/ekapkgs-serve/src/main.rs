mod api;
mod config;
mod signing;
mod storage;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use clap::{Parser, Subcommand};

use ekapkgs_protocol::ekapkgs::v1::cache_service_server::CacheServiceServer;

use config::Config;
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

pub struct AppState {
    pub storage: Box<dyn StorageBackend>,
    pub signer: NarInfoSigner,
    pub cert_signer: Option<signing::CertSigner>,
}

fn build_http_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/nix-cache-info", get(api::compat::nix_cache_info))
        .route("/{hash_narinfo}", get(api::compat::get_narinfo))
        .route("/nar/{file}", get(api::compat::get_nar))
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
    let ca_key_contents = std::fs::read_to_string(ca_key_path)?.trim().to_string();
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

async fn cmd_serve(cli: Cli) -> color_eyre::Result<()> {
    let bind_addr: String;
    let storage_backend: Box<dyn StorageBackend>;
    let signer: NarInfoSigner;
    let cert_signer: Option<signing::CertSigner>;

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
            config::StorageConfig::Filesystem { path } => {
                Box::new(storage::filesystem::FilesystemBackend::new(path))
            }
            config::StorageConfig::NixStore => {
                Box::new(storage::nix_store::NixStoreBackend::new())
            }
        };
    } else {
        bind_addr = cli.bind.unwrap_or_else(|| "0.0.0.0:8080".to_string());

        let signing_key = cli.signing_key.ok_or_else(|| {
            color_eyre::eyre::eyre!("either --config or --signing-key is required")
        })?;
        signer = NarInfoSigner::from_file(&signing_key)?;
        cert_signer = None;

        let storage_str = cli.storage.unwrap_or_else(|| "nix-store".to_string());
        storage_backend = if storage_str == "nix-store" {
            Box::new(storage::nix_store::NixStoreBackend::new())
        } else {
            Box::new(storage::filesystem::FilesystemBackend::new(
                PathBuf::from(storage_str),
            ))
        };
    }

    let state = Arc::new(AppState {
        storage: storage_backend,
        signer,
        cert_signer,
    });

    let addr: SocketAddr = bind_addr.parse()?;

    let grpc_service = CacheServiceServer::new(api::negotiate::NegotiateService {
        state: Arc::clone(&state),
    });

    let app = build_http_router(state).route_service(
        "/ekapkgs.v1.CacheService/Negotiate",
        grpc_service,
    );

    tracing::info!("Listening on {addr} (gRPC + HTTP)");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
