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
    /// Flake dev shells to compose into the environment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flakes: Vec<EnvFlakeEntry>,
}

impl Default for EnvManifest {
    fn default() -> Self {
        Self {
            version: default_manifest_version(),
            flake: default_flake(),
            packages: Vec::new(),
            flakes: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EnvPackageEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flake: Option<String>,
}

/// A flake dev shell to include in the environment.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EnvFlakeEntry {
    /// Flake reference (e.g., `.`, `github:user/repo`, `path:../other`).
    pub ref_: String,
    /// Dev shell attribute to use (default: `default`).
    #[serde(default = "default_devshell")]
    pub devshell: String,
    /// Pin to a specific revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    /// Override flake inputs (e.g., `nixpkgs` → a pinned ref).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub inputs: std::collections::HashMap<String, String>,
}

fn default_devshell() -> String {
    "default".to_owned()
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

    /// Add a flake entry if not already present. Returns `true` if added.
    pub fn add_flake(&mut self, entry: EnvFlakeEntry) -> bool {
        if self.flakes.iter().any(|f| f.ref_ == entry.ref_) {
            return false;
        }
        self.flakes.push(entry);
        true
    }

    /// Remove a flake entry by ref. Returns `true` if it was present.
    pub fn remove_flake(&mut self, ref_: &str) -> bool {
        let before = self.flakes.len();
        self.flakes.retain(|f| f.ref_ != ref_);
        self.flakes.len() < before
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

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // HomePackages
    // -----------------------------------------------------------------------

    #[test]
    fn home_packages_default() {
        let hp = HomePackages::default();
        assert_eq!(hp.version, 1);
        assert_eq!(hp.flake, "nixpkgs");
        assert!(hp.packages.is_empty());
    }

    #[test]
    fn home_packages_add_and_dedup() {
        let mut hp = HomePackages::default();
        let entry = HomePackageEntry {
            name: "hello".into(),
            flake: None,
        };
        assert!(hp.add(entry.clone()));
        assert!(!hp.add(entry)); // duplicate
        assert_eq!(hp.packages.len(), 1);
    }

    #[test]
    fn home_packages_remove() {
        let mut hp = HomePackages::default();
        hp.add(HomePackageEntry {
            name: "hello".into(),
            flake: None,
        });
        hp.add(HomePackageEntry {
            name: "world".into(),
            flake: None,
        });
        assert!(hp.remove("hello"));
        assert!(!hp.remove("hello")); // already gone
        assert_eq!(hp.packages.len(), 1);
        assert_eq!(hp.packages[0].name, "world");
    }

    #[test]
    fn home_packages_resolve_installable_default_flake() {
        let hp = HomePackages::default();
        let entry = HomePackageEntry {
            name: "ripgrep".into(),
            flake: None,
        };
        assert_eq!(hp.resolve_installable(&entry), "nixpkgs#ripgrep");
    }

    #[test]
    fn home_packages_resolve_installable_override_flake() {
        let hp = HomePackages::default();
        let entry = HomePackageEntry {
            name: "my-tool".into(),
            flake: Some("github:user/repo".into()),
        };
        assert_eq!(hp.resolve_installable(&entry), "github:user/repo#my-tool");
    }

    #[test]
    fn home_packages_toml_roundtrip() {
        let mut hp = HomePackages::default();
        hp.add(HomePackageEntry {
            name: "jq".into(),
            flake: None,
        });
        hp.add(HomePackageEntry {
            name: "special".into(),
            flake: Some("github:foo/bar".into()),
        });

        let toml_str = toml::to_string_pretty(&hp).unwrap();
        let parsed: HomePackages = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.packages.len(), 2);
        assert_eq!(parsed.packages[0].name, "jq");
        assert!(parsed.packages[0].flake.is_none());
        assert_eq!(parsed.packages[1].name, "special");
        assert_eq!(parsed.packages[1].flake.as_deref(), Some("github:foo/bar"));
    }

    #[test]
    fn home_packages_deserialize_minimal() {
        let toml_str = r#"
            [[packages]]
            name = "hello"
        "#;
        let hp: HomePackages = toml::from_str(toml_str).unwrap();
        assert_eq!(hp.version, 1);
        assert_eq!(hp.flake, "nixpkgs");
        assert_eq!(hp.packages.len(), 1);
    }

    // -----------------------------------------------------------------------
    // SystemPackages (mirrors HomePackages structure)
    // -----------------------------------------------------------------------

    #[test]
    fn system_packages_add_remove() {
        let mut sp = SystemPackages::default();
        assert!(sp.add(SystemPackageEntry {
            name: "htop".into(),
            flake: None,
        }));
        assert!(!sp.add(SystemPackageEntry {
            name: "htop".into(),
            flake: None,
        }));
        assert!(sp.remove("htop"));
        assert!(!sp.remove("htop"));
    }

    #[test]
    fn system_packages_resolve_installable() {
        let sp = SystemPackages {
            flake: "my-flake".into(),
            ..SystemPackages::default()
        };
        let entry = SystemPackageEntry {
            name: "vim".into(),
            flake: None,
        };
        assert_eq!(sp.resolve_installable(&entry), "my-flake#vim");
    }

    // -----------------------------------------------------------------------
    // EnvManifest
    // -----------------------------------------------------------------------

    #[test]
    fn env_manifest_default() {
        let em = EnvManifest::default();
        assert_eq!(em.version, 1);
        assert_eq!(em.flake, "nixpkgs");
        assert!(em.packages.is_empty());
        assert!(em.flakes.is_empty());
    }

    #[test]
    fn env_manifest_add_remove_packages() {
        let mut em = EnvManifest::default();
        assert!(em.add(EnvPackageEntry {
            name: "fd".into(),
            flake: None,
        }));
        assert!(em.add(EnvPackageEntry {
            name: "rg".into(),
            flake: None,
        }));
        assert!(!em.add(EnvPackageEntry {
            name: "fd".into(),
            flake: None,
        }));
        assert_eq!(em.packages.len(), 2);
        assert!(em.remove("fd"));
        assert_eq!(em.packages.len(), 1);
        assert_eq!(em.packages[0].name, "rg");
    }

    #[test]
    fn env_manifest_add_remove_flakes() {
        let mut em = EnvManifest::default();
        let entry = EnvFlakeEntry {
            ref_: ".".into(),
            devshell: "default".into(),
            rev: None,
            inputs: std::collections::HashMap::new(),
        };
        assert!(em.add_flake(entry.clone()));
        assert!(!em.add_flake(entry)); // dedup by ref_
        assert_eq!(em.flakes.len(), 1);
        assert!(em.remove_flake("."));
        assert!(em.flakes.is_empty());
        assert!(!em.remove_flake(".")); // already gone
    }

    #[test]
    fn env_manifest_resolve_installable() {
        let em = EnvManifest::default();
        let entry = EnvPackageEntry {
            name: "jq".into(),
            flake: None,
        };
        assert_eq!(em.resolve_installable(&entry), "nixpkgs#jq");
    }

    #[test]
    fn env_manifest_save_load_roundtrip() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut em = EnvManifest::default();
        em.add(EnvPackageEntry {
            name: "jq".into(),
            flake: None,
        });
        em.flakes.push(EnvFlakeEntry {
            ref_: "github:user/repo".into(),
            devshell: "python".into(),
            rev: Some("abc123".into()),
            inputs: [("nixpkgs".into(), "github:NixOS/nixpkgs/nixos-24.05".into())]
                .into_iter()
                .collect(),
        });

        em.save_to(dir.path()).unwrap();
        let loaded = EnvManifest::load_from(dir.path()).unwrap();

        assert_eq!(loaded.packages.len(), 1);
        assert_eq!(loaded.packages[0].name, "jq");
        assert_eq!(loaded.flakes.len(), 1);
        assert_eq!(loaded.flakes[0].ref_, "github:user/repo");
        assert_eq!(loaded.flakes[0].devshell, "python");
        assert_eq!(loaded.flakes[0].rev.as_deref(), Some("abc123"));
        assert_eq!(
            loaded.flakes[0].inputs.get("nixpkgs").map(String::as_str),
            Some("github:NixOS/nixpkgs/nixos-24.05")
        );
    }

    #[test]
    fn env_manifest_load_missing_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(EnvManifest::load_from(dir.path()).is_err());
    }

    #[test]
    fn env_manifest_toml_format_packages_only() {
        let mut em = EnvManifest::default();
        em.add(EnvPackageEntry {
            name: "jq".into(),
            flake: None,
        });
        let toml_str = toml::to_string_pretty(&em).unwrap();
        // Should not contain [[flakes]] when empty.
        assert!(!toml_str.contains("[[flakes]]"));
        assert!(toml_str.contains("[[packages]]"));
        assert!(toml_str.contains("name = \"jq\""));
    }

    #[test]
    fn env_manifest_toml_format_with_flakes() {
        let mut em = EnvManifest::default();
        em.flakes.push(EnvFlakeEntry {
            ref_: ".".into(),
            devshell: "default".into(),
            rev: None,
            inputs: std::collections::HashMap::new(),
        });
        let toml_str = toml::to_string_pretty(&em).unwrap();
        assert!(toml_str.contains("[[flakes]]"));
        assert!(toml_str.contains("ref_ = \".\""));
    }

    #[test]
    fn env_manifest_deserialize_packages_only() {
        let toml_str = r#"
            version = 1
            flake = "nixpkgs"

            [[packages]]
            name = "jq"
        "#;
        let em: EnvManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(em.packages.len(), 1);
        assert!(em.flakes.is_empty());
    }

    #[test]
    fn env_manifest_deserialize_full() {
        let toml_str = r#"
            version = 1
            flake = "nixpkgs"

            [[packages]]
            name = "jq"

            [[packages]]
            name = "my-tool"
            flake = "github:user/repo"

            [[flakes]]
            ref_ = "."
            devshell = "default"

            [[flakes]]
            ref_ = "github:other/flake"
            devshell = "python"
            rev = "deadbeef"

            [flakes.inputs]
            nixpkgs = "github:NixOS/nixpkgs/nixos-24.05"
        "#;
        let em: EnvManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(em.packages.len(), 2);
        assert_eq!(em.flakes.len(), 2);
        assert_eq!(em.flakes[0].ref_, ".");
        assert_eq!(em.flakes[1].rev.as_deref(), Some("deadbeef"));
        assert_eq!(
            em.flakes[1].inputs.get("nixpkgs").map(String::as_str),
            Some("github:NixOS/nixpkgs/nixos-24.05")
        );
    }

    #[test]
    fn env_manifest_devshell_defaults_to_default() {
        let toml_str = r#"
            [[flakes]]
            ref_ = "."
        "#;
        let em: EnvManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(em.flakes[0].devshell, "default");
    }

    #[test]
    fn env_manifest_content_hash_changes_on_edit() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut em = EnvManifest::default();
        em.save_to(dir.path()).unwrap();

        let hash1 = EnvManifest::content_hash(dir.path()).unwrap();

        em.add(EnvPackageEntry {
            name: "ripgrep".into(),
            flake: None,
        });
        em.save_to(dir.path()).unwrap();

        let hash2 = EnvManifest::content_hash(dir.path()).unwrap();
        assert_ne!(hash1, hash2);
    }

    // -----------------------------------------------------------------------
    // TrustedEnvs
    // -----------------------------------------------------------------------

    #[test]
    fn trusted_envs_allow_and_check() {
        let dir = tempfile::TempDir::new().unwrap();
        let em = EnvManifest::default();
        em.save_to(dir.path()).unwrap();

        let mut trusted = TrustedEnvs::default();
        trusted.allow(dir.path()).unwrap();
        assert!(trusted.is_trusted(dir.path()));
    }

    #[test]
    fn trusted_envs_not_trusted_when_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        let em = EnvManifest::default();
        em.save_to(dir.path()).unwrap();

        let trusted = TrustedEnvs::default();
        assert!(!trusted.is_trusted(dir.path()));
    }

    #[test]
    fn trusted_envs_invalidated_by_manifest_edit() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut em = EnvManifest::default();
        em.save_to(dir.path()).unwrap();

        let mut trusted = TrustedEnvs::default();
        trusted.allow(dir.path()).unwrap();
        assert!(trusted.is_trusted(dir.path()));

        // Edit the manifest — trust should be invalidated.
        em.add(EnvPackageEntry {
            name: "new-pkg".into(),
            flake: None,
        });
        em.save_to(dir.path()).unwrap();
        assert!(!trusted.is_trusted(dir.path()));
    }

    #[test]
    fn trusted_envs_disallow() {
        let dir = tempfile::TempDir::new().unwrap();
        let em = EnvManifest::default();
        em.save_to(dir.path()).unwrap();

        let mut trusted = TrustedEnvs::default();
        trusted.allow(dir.path()).unwrap();
        assert!(trusted.is_trusted(dir.path()));

        trusted.disallow(dir.path());
        assert!(!trusted.is_trusted(dir.path()));
    }

    #[test]
    fn trusted_envs_toml_roundtrip() {
        let mut trusted = TrustedEnvs::default();
        trusted.entries.insert("/some/path".into(), "abc123".into());

        let toml_str = toml::to_string_pretty(&trusted).unwrap();
        let parsed: TrustedEnvs = toml::from_str(&toml_str).unwrap();
        assert_eq!(
            parsed.entries.get("/some/path").map(String::as_str),
            Some("abc123")
        );
    }

    // -----------------------------------------------------------------------
    // EnvFlakeEntry pinning
    // -----------------------------------------------------------------------

    #[test]
    fn env_flake_entry_pinned_serialization() {
        let entry = EnvFlakeEntry {
            ref_: "github:NixOS/nixpkgs".into(),
            devshell: "default".into(),
            rev: Some("abc123def456".into()),
            inputs: std::collections::HashMap::new(),
        };
        let toml_str = toml::to_string_pretty(&entry).unwrap();
        assert!(toml_str.contains("rev = \"abc123def456\""));
    }

    #[test]
    fn env_flake_entry_unpinned_omits_rev() {
        let entry = EnvFlakeEntry {
            ref_: ".".into(),
            devshell: "default".into(),
            rev: None,
            inputs: std::collections::HashMap::new(),
        };
        let toml_str = toml::to_string_pretty(&entry).unwrap();
        assert!(!toml_str.contains("rev"));
    }

    #[test]
    fn env_flake_entry_with_input_overrides() {
        let entry = EnvFlakeEntry {
            ref_: "github:user/repo".into(),
            devshell: "default".into(),
            rev: None,
            inputs: [("nixpkgs".into(), "github:NixOS/nixpkgs/nixos-24.05".into())]
                .into_iter()
                .collect(),
        };
        let toml_str = toml::to_string_pretty(&entry).unwrap();
        assert!(toml_str.contains("[inputs]"));
        assert!(toml_str.contains("nixpkgs = \"github:NixOS/nixpkgs/nixos-24.05\""));
    }

    #[test]
    fn env_manifest_multiple_flakes_composable() {
        let mut em = EnvManifest::default();
        em.add_flake(EnvFlakeEntry {
            ref_: ".".into(),
            devshell: "default".into(),
            rev: None,
            inputs: std::collections::HashMap::new(),
        });
        em.add_flake(EnvFlakeEntry {
            ref_: "github:user/rust-tools".into(),
            devshell: "default".into(),
            rev: Some("abc123".into()),
            inputs: std::collections::HashMap::new(),
        });
        em.add_flake(EnvFlakeEntry {
            ref_: "github:another/devenv".into(),
            devshell: "python".into(),
            rev: None,
            inputs: [("nixpkgs".into(), "github:NixOS/nixpkgs/nixos-24.05".into())]
                .into_iter()
                .collect(),
        });

        assert_eq!(em.flakes.len(), 3);

        // Roundtrip through TOML.
        let toml_str = toml::to_string_pretty(&em).unwrap();
        let parsed: EnvManifest = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.flakes.len(), 3);
        assert_eq!(parsed.flakes[0].ref_, ".");
        assert_eq!(parsed.flakes[1].rev.as_deref(), Some("abc123"));
        assert_eq!(parsed.flakes[2].devshell, "python");
        assert!(parsed.flakes[2].inputs.contains_key("nixpkgs"));
    }

    #[test]
    fn env_manifest_profile_path_deterministic() {
        let dir = tempfile::TempDir::new().unwrap();
        let path1 = EnvManifest::profile_path(dir.path()).unwrap();
        let path2 = EnvManifest::profile_path(dir.path()).unwrap();
        assert_eq!(path1, path2);
    }

    #[test]
    fn env_manifest_profile_path_differs_per_dir() {
        let dir1 = tempfile::TempDir::new().unwrap();
        let dir2 = tempfile::TempDir::new().unwrap();
        let path1 = EnvManifest::profile_path(dir1.path()).unwrap();
        let path2 = EnvManifest::profile_path(dir2.path()).unwrap();
        assert_ne!(path1, path2);
    }
}
