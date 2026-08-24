mod api;
mod config;
mod signing;
mod storage;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use clap::Parser;

use config::Config;
use signing::NarInfoSigner;
use storage::StorageBackend;

#[derive(Parser)]
#[command(name = "ekapkgs-serve", about = "ekapkgs binary cache server")]
struct Cli {
    /// Path to config file.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Bind address (overrides config).
    #[arg(short, long)]
    bind: Option<String>,

    /// Storage backend: "nix-store" or path to cache directory.
    #[arg(short, long)]
    storage: Option<String>,

    /// Path to nix signing key file.
    #[arg(long)]
    signing_key: Option<PathBuf>,

    #[command(flatten)]
    verbose: clap_verbosity_flag::Verbosity,
}

pub struct AppState {
    pub storage: Box<dyn StorageBackend>,
    pub signer: NarInfoSigner,
}

fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/nix-cache-info", get(api::compat::nix_cache_info))
        .route("/{hash_narinfo}", get(api::compat::get_narinfo))
        .route("/nar/{file}", get(api::compat::get_nar))
        .with_state(state)
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    ekapkgs_ui::logging::init(&cli.verbose);

    // Build configuration from file + CLI overrides.
    let bind_addr: String;
    let storage_backend: Box<dyn StorageBackend>;
    let signer: NarInfoSigner;

    if let Some(config_path) = &cli.config {
        let config = Config::load(config_path)?;
        bind_addr = cli.bind.unwrap_or(config.server.bind);
        signer = NarInfoSigner::from_file(&config.signing.secret_key_file)?;
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

        let signing_key = cli
            .signing_key
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "either --config or --signing-key is required"
                )
            })?;
        signer = NarInfoSigner::from_file(&signing_key)?;

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
    });

    let app = build_router(state);

    let addr: SocketAddr = bind_addr.parse()?;
    tracing::info!("Listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
