use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub signing: SigningConfig,
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
}

fn default_bind() -> String {
    "0.0.0.0:8080".to_owned()
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
    #[serde(rename = "castore")]
    Castore {
        /// Root directory for content-addressed chunk and directory storage.
        path: PathBuf,
        /// Optional GC configuration.
        gc: Option<GcRawConfig>,
    },
    #[serde(rename = "s3")]
    #[allow(dead_code)]
    S3 {
        /// S3 bucket name.
        bucket: String,
        /// AWS region (e.g., "us-east-1").
        #[serde(default = "default_region")]
        region: String,
        /// Custom S3 endpoint for R2, MinIO, etc.
        endpoint: Option<String>,
        /// Key prefix for all objects (e.g., "cache/" for namespacing).
        #[serde(default)]
        prefix: String,
    },
}

fn default_region() -> String {
    "us-east-1".to_owned()
}

#[derive(Debug, Deserialize)]
pub struct SigningConfig {
    pub secret_key_file: PathBuf,
    pub certificate: Option<CertificateConfig>,
    /// Additional certificates for threshold signing.
    #[serde(default)]
    pub certificates: Vec<CertificateConfig>,
    /// Threshold policy: require this many valid cert signatures.
    pub threshold: Option<u32>,
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

#[derive(Debug, Deserialize)]
pub struct AuthConfig {
    /// Bearer tokens that are allowed to push to the cache.
    #[serde(default)]
    pub write_tokens: Vec<String>,
}

impl Config {
    pub fn load(path: &std::path::Path) -> color_eyre::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }
}
