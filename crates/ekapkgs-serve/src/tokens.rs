use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// A named API token with permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    /// Human-readable name (e.g., "ci-main", "jon-laptop").
    pub name: String,
    /// The bearer token value (opaque, random).
    pub token: String,
    /// Permissions granted by this token.
    pub permissions: Permissions,
    /// Unix timestamp when the token was created.
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permissions {
    /// Can read (pull) from the cache.
    pub read: bool,
    /// Can write (push) to the cache.
    pub write: bool,
}

/// The on-disk token store. Stored as JSON alongside the server config.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TokenStore {
    pub tokens: Vec<Token>,
}

impl TokenStore {
    /// Load the token store from disk, or create an empty one.
    pub fn load(path: &Path) -> color_eyre::Result<Self> {
        if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            Ok(serde_json::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    /// Save the token store to disk.
    pub fn save(&self, path: &Path) -> color_eyre::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Create a new token with the given name and permissions.
    /// Returns the generated token value.
    pub fn create(
        &mut self,
        name: &str,
        permissions: Permissions,
    ) -> color_eyre::Result<String> {
        // Check for duplicate names.
        if self.tokens.iter().any(|t| t.name == name) {
            return Err(color_eyre::eyre::eyre!(
                "token with name '{name}' already exists"
            ));
        }

        let token_value = generate_token();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.tokens.push(Token {
            name: name.to_string(),
            token: token_value.clone(),
            permissions,
            created_at: now,
        });

        Ok(token_value)
    }

    /// Revoke (delete) a token by name.
    pub fn revoke(&mut self, name: &str) -> bool {
        let before = self.tokens.len();
        self.tokens.retain(|t| t.name != name);
        self.tokens.len() < before
    }

    /// Look up a token value and return its permissions if valid.
    #[allow(dead_code)] // used when auth checks move to per-request validation
    pub fn validate(&self, token_value: &str) -> Option<&Token> {
        self.tokens.iter().find(|t| t.token == token_value)
    }

    /// Get all bearer token strings that have write permission.
    /// Used for backward compat with the existing auth check.
    pub fn write_tokens(&self) -> Vec<String> {
        self.tokens
            .iter()
            .filter(|t| t.permissions.write)
            .map(|t| t.token.clone())
            .collect()
    }
}

/// Generate a cryptographically random token.
///
/// Format: `ekap_` prefix + 43 chars of base64url (256 bits of entropy).
fn generate_token() -> String {
    use std::io::Read;

    let mut bytes = [0u8; 32];
    // Use /dev/urandom for portability without pulling in a full CSPRNG crate.
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut bytes);
    } else {
        // Fallback: use a less ideal but functional source.
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i as u8)
                .wrapping_mul(0x9E)
                .wrapping_add(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .subsec_nanos() as u8,
                );
        }
    }

    let encoded = data_encoding::BASE64URL_NOPAD.encode(&bytes);
    format!("ekap_{encoded}")
}

/// Resolve the token store path from a config file path or default location.
pub fn default_store_path(config_path: Option<&Path>) -> PathBuf {
    if let Some(cfg) = config_path {
        cfg.with_file_name("tokens.json")
    } else {
        PathBuf::from("/etc/ekapkgs-serve/tokens.json")
    }
}
