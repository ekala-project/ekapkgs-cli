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
    "127.0.0.1:8080".to_owned()
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
