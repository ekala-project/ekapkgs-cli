use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ClientConfig {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub caches: Vec<CacheConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Defaults {
    #[serde(default = "default_parallel")]
    pub max_parallel_downloads: usize,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            max_parallel_downloads: default_parallel(),
        }
    }
}

fn default_parallel() -> usize {
    8
}

#[allow(dead_code)] // trust fields used by cert verification
#[derive(Debug, Deserialize, Clone)]
pub struct CacheConfig {
    pub url: String,
    pub trusted_key: Option<String>,
    pub trust_root: Option<String>,
    /// Bearer token for pushing to this cache.
    pub token: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: u32,
    #[serde(default)]
    pub protocol: CacheProtocol,
}

fn default_priority() -> u32 {
    10
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CacheProtocol {
    #[default]
    Auto,
    Legacy,
    Grpc,
}

impl ClientConfig {
    pub fn load() -> color_eyre::Result<Self> {
        let config_dir = directories::ProjectDirs::from("", "", "ekapkgs")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("~/.config/ekapkgs"));

        let config_path = config_dir.join("config.toml");
        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    /// Get the primary ekapkgs cache (first configured, or none).
    pub fn primary_cache(&self) -> Option<&CacheConfig> {
        self.caches
            .iter()
            .filter(|c| c.protocol != CacheProtocol::Legacy)
            .min_by_key(|c| c.priority)
    }

    /// Get the push token for a given cache URL.
    pub fn push_token(&self, url: &str) -> Option<String> {
        self.caches
            .iter()
            .find(|c| c.url == url)
            .and_then(|c| c.token.clone())
    }
}
