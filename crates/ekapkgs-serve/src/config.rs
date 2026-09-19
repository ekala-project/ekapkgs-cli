use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub signing: SigningConfig,
    pub auth: Option<AuthConfig>,
    /// Zstd compression settings for HTTP responses.
    #[serde(default)]
    pub compression: CompressionConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Path to TLS certificate file (PEM).
    pub tls_cert_path: Option<PathBuf>,
    /// Path to TLS private key file (PEM).
    pub tls_key_path: Option<PathBuf>,
    /// Cache priority advertised in nix-cache-info. Default: 30.
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// Client request timeout in seconds. Default: 30.
    #[serde(default = "default_request_timeout")]
    pub client_request_timeout_secs: u64,
    /// Expose /metrics endpoint. Default: true.
    /// Set to false to disable the metrics endpoint entirely.
    /// Consider binding to a separate internal address or using a reverse proxy
    /// to restrict access to metrics in production.
    #[serde(default = "default_true")]
    pub enable_metrics: bool,
}

fn default_bind() -> String {
    "127.0.0.1:8080".to_owned()
}

fn default_priority() -> u32 {
    30
}

fn default_request_timeout() -> u64 {
    30
}

#[derive(Debug, Deserialize)]
#[serde(tag = "backend")]
pub enum StorageConfig {
    #[serde(rename = "filesystem")]
    Filesystem { path: PathBuf },
    #[serde(rename = "nix-store")]
    NixStore,
    #[serde(rename = "castore")]
    Castore {
        /// Root directory for content-addressed chunk and directory storage.
        path: PathBuf,
        /// Optional GC configuration.
        gc: Option<GcRawConfig>,
    },
}

#[derive(Debug, Deserialize)]
pub struct SigningConfig {
    pub secret_key_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CompressionConfig {
    /// Master switch for HTTP response compression. Default: true.
    #[serde(default = "default_true")]
    pub enable: bool,
    /// Zstd compression level. Default: 1.
    #[serde(default = "default_compression_level")]
    pub level: i32,
    /// Enable long distance matching for NAR responses. Default: true.
    #[serde(default = "default_true")]
    pub long_distance_matching: bool,
    /// Zstd window_log parameter. 0 = auto (use cap). Default: 0.
    #[serde(default)]
    pub window_log: u32,
    /// Maximum concurrent LDM encoders per worker. Default: 16.
    #[serde(default = "default_max_ldm_encoders")]
    pub max_ldm_encoders: u32,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enable: true,
            level: 1,
            long_distance_matching: true,
            window_log: 0,
            max_ldm_encoders: 16,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_compression_level() -> i32 {
    1
}

fn default_max_ldm_encoders() -> u32 {
    16
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
