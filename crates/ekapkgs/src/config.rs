use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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

// ---------------------------------------------------------------------------
// Home package manifest
// ---------------------------------------------------------------------------

fn default_manifest_version() -> u32 {
    1
}

fn default_flake() -> String {
    "nixpkgs".to_owned()
}

/// Imperative package manifest stored at `~/.config/ekapkgs/home-packages.toml`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HomePackages {
    #[serde(default = "default_manifest_version")]
    pub version: u32,
    #[serde(default = "default_flake")]
    pub flake: String,
    #[serde(default)]
    pub packages: Vec<HomePackageEntry>,
}

impl Default for HomePackages {
    fn default() -> Self {
        Self {
            version: default_manifest_version(),
            flake: default_flake(),
            packages: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HomePackageEntry {
    pub name: String,
    /// Overrides the manifest-level default flake for this package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake: Option<String>,
}

impl HomePackages {
    /// Path to the manifest file.
    pub fn manifest_path() -> PathBuf {
        let config_dir = directories::ProjectDirs::from("", "", "ekapkgs")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("~/.config/ekapkgs"));
        config_dir.join("home-packages.toml")
    }

    /// Load from disk, returning defaults if the file does not exist.
    pub fn load() -> color_eyre::Result<Self> {
        let path = Self::manifest_path();
        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    /// Write the manifest back to disk.
    pub fn save(&self) -> color_eyre::Result<()> {
        let path = Self::manifest_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        Ok(())
    }

    /// Add a package if it is not already present. Returns `true` if added.
    pub fn add(&mut self, entry: HomePackageEntry) -> bool {
        if self.packages.iter().any(|p| p.name == entry.name) {
            return false;
        }
        self.packages.push(entry);
        true
    }

    /// Remove a package by name. Returns `true` if it was present.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|p| p.name != name);
        self.packages.len() < before
    }

    /// Resolve the full nix installable for a package entry.
    pub fn resolve_installable(&self, entry: &HomePackageEntry) -> String {
        let flake = entry.flake.as_deref().unwrap_or(&self.flake);
        format!("{flake}#{}", entry.name)
    }
}

// ---------------------------------------------------------------------------
// System package manifest
// ---------------------------------------------------------------------------

/// Imperative system package manifest stored at `~/.config/ekapkgs/system-packages.toml`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SystemPackages {
    #[serde(default = "default_manifest_version")]
    pub version: u32,
    #[serde(default = "default_flake")]
    pub flake: String,
    #[serde(default)]
    pub packages: Vec<SystemPackageEntry>,
}

impl Default for SystemPackages {
    fn default() -> Self {
        Self {
            version: default_manifest_version(),
            flake: default_flake(),
            packages: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SystemPackageEntry {
    pub name: String,
    /// Overrides the manifest-level default flake for this package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake: Option<String>,
}

impl SystemPackages {
    /// Path to the manifest file.
    pub fn manifest_path() -> PathBuf {
        let config_dir = directories::ProjectDirs::from("", "", "ekapkgs")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("~/.config/ekapkgs"));
        config_dir.join("system-packages.toml")
    }

    /// Load from disk, returning defaults if the file does not exist.
    pub fn load() -> color_eyre::Result<Self> {
        let path = Self::manifest_path();
        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    /// Write the manifest back to disk.
    pub fn save(&self) -> color_eyre::Result<()> {
        let path = Self::manifest_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        Ok(())
    }

    /// Add a package if it is not already present. Returns `true` if added.
    pub fn add(&mut self, entry: SystemPackageEntry) -> bool {
        if self.packages.iter().any(|p| p.name == entry.name) {
            return false;
        }
        self.packages.push(entry);
        true
    }

    /// Remove a package by name. Returns `true` if it was present.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|p| p.name != name);
        self.packages.len() < before
    }

    /// Resolve the full nix installable for a package entry.
    pub fn resolve_installable(&self, entry: &SystemPackageEntry) -> String {
        let flake = entry.flake.as_deref().unwrap_or(&self.flake);
        format!("{flake}#{}", entry.name)
    }
}
