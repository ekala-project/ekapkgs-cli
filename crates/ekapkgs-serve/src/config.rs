use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub signing: SigningConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
}

fn default_bind() -> String {
    "0.0.0.0:8080".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "backend")]
pub enum StorageConfig {
    #[serde(rename = "filesystem")]
    Filesystem {
        path: PathBuf,
        gc: Option<GcRawConfig>,
    },
    #[serde(rename = "nix-store")]
    NixStore,
}

#[derive(Debug, Deserialize)]
pub struct SigningConfig {
    pub secret_key_file: PathBuf,
    pub certificate: Option<CertificateConfig>,
}

#[derive(Debug, Deserialize)]
pub struct CertificateConfig {
    pub cert_file: PathBuf,
    pub private_key_file: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct GcRawConfig {
    /// Maximum cache size (e.g., "50GiB").
    pub max_size: String,
    /// GC target size (e.g., "40GiB"). Defaults to 80% of max_size.
    pub target_size: Option<String>,
    /// GC check interval in seconds. Default: 300.
    #[serde(default = "default_gc_interval")]
    pub gc_interval_secs: u64,
}

fn default_gc_interval() -> u64 {
    300
}

impl Config {
    pub fn load(path: &std::path::Path) -> color_eyre::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}
