use std::path::{Path, PathBuf};

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

// ---------------------------------------------------------------------------
// Directory environment manifest (.ekapkgs-env.toml)
// ---------------------------------------------------------------------------

/// Per-directory environment manifest.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EnvManifest {
    #[serde(default = "default_manifest_version")]
    pub version: u32,
    #[serde(default = "default_flake")]
    pub flake: String,
    #[serde(default)]
    pub packages: Vec<EnvPackageEntry>,
    /// When `true`, activate the directory's `flake.nix` dev shell
    /// instead of (or in addition to) individual packages.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub use_flake: bool,
}

impl Default for EnvManifest {
    fn default() -> Self {
        Self {
            version: default_manifest_version(),
            flake: default_flake(),
            packages: Vec::new(),
            use_flake: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EnvPackageEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake: Option<String>,
}

/// Name of the manifest file in a project directory.
pub const ENV_MANIFEST_NAME: &str = ".ekapkgs-env.toml";

impl EnvManifest {
    /// Load from a specific directory.
    pub fn load_from(dir: &Path) -> color_eyre::Result<Self> {
        let path = dir.join(ENV_MANIFEST_NAME);
        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Err(color_eyre::eyre::eyre!(
                "No {ENV_MANIFEST_NAME} found in {}",
                dir.display()
            ))
        }
    }

    /// Save to a specific directory.
    pub fn save_to(&self, dir: &Path) -> color_eyre::Result<()> {
        let path = dir.join(ENV_MANIFEST_NAME);
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        Ok(())
    }

    /// Add a package if not already present. Returns `true` if added.
    pub fn add(&mut self, entry: EnvPackageEntry) -> bool {
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
    pub fn resolve_installable(&self, entry: &EnvPackageEntry) -> String {
        let flake = entry.flake.as_deref().unwrap_or(&self.flake);
        format!("{flake}#{}", entry.name)
    }

    /// Hash the manifest contents for trust verification.
    pub fn content_hash(dir: &Path) -> color_eyre::Result<String> {
        let path = dir.join(ENV_MANIFEST_NAME);
        let contents = std::fs::read(&path)?;
        let h = blake3::hash(&contents);
        Ok(h.to_hex().as_str()[..64].to_owned())
    }

    /// Compute the profile path for a given directory under the cache.
    pub fn profile_path(dir: &Path) -> color_eyre::Result<PathBuf> {
        let cache_dir = directories::ProjectDirs::from("", "", "ekapkgs")
            .map(|d| d.cache_dir().to_path_buf())
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                PathBuf::from(home).join(".cache/ekapkgs")
            });
        let dir_canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let hash = {
            let h = blake3::hash(dir_canonical.to_string_lossy().as_bytes());
            h.to_hex().as_str()[..32].to_owned()
        };
        let env_dir = cache_dir.join("envs").join(hash);
        std::fs::create_dir_all(&env_dir)?;
        Ok(env_dir.join("profile"))
    }
}

// ---------------------------------------------------------------------------
// Environment trust database
// ---------------------------------------------------------------------------

/// Maps canonical directory paths to the blake3 hash of their manifest at
/// the time `env allow` was run.  Stored at `~/.config/ekapkgs/trusted-envs.toml`.
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct TrustedEnvs {
    #[serde(default)]
    pub entries: std::collections::HashMap<String, String>,
}

impl TrustedEnvs {
    fn db_path() -> PathBuf {
        let config_dir = directories::ProjectDirs::from("", "", "ekapkgs")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("~/.config/ekapkgs"));
        config_dir.join("trusted-envs.toml")
    }

    pub fn load() -> color_eyre::Result<Self> {
        let path = Self::db_path();
        if path.exists() {
            let contents = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> color_eyre::Result<()> {
        let path = Self::db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        Ok(())
    }

    /// Mark a directory as trusted with the current manifest hash.
    pub fn allow(&mut self, dir: &Path) -> color_eyre::Result<()> {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let hash = EnvManifest::content_hash(dir)?;
        self.entries
            .insert(canonical.to_string_lossy().into_owned(), hash);
        Ok(())
    }

    /// Remove trust for a directory.
    pub fn disallow(&mut self, dir: &Path) {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        self.entries.remove(canonical.to_string_lossy().as_ref());
    }

    /// Check if a directory's manifest is trusted (hash matches).
    pub fn is_trusted(&self, dir: &Path) -> bool {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let key = canonical.to_string_lossy();
        let Some(stored_hash) = self.entries.get(key.as_ref()) else {
            return false;
        };
        let Ok(current_hash) = EnvManifest::content_hash(dir) else {
            return false;
        };
        *stored_hash == current_hash
    }
}
