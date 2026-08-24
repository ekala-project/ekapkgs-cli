use ed25519_dalek::{Signer, SigningKey};

/// A nix-compatible narinfo signer.
///
/// Produces signatures in the format `key-name:base64-signature` over the
/// standard nix fingerprint: `1;{store_path};{nar_hash};{nar_size};{refs}`.
pub struct NarInfoSigner {
    key_name: String,
    signing_key: SigningKey,
}

impl NarInfoSigner {
    /// Load a nix signing key from a file.
    ///
    /// The file format is `key-name:base64-encoded-ed25519-secret-key`.
    pub fn from_file(path: &std::path::Path) -> color_eyre::Result<Self> {
        let contents = std::fs::read_to_string(path)?.trim().to_string();
        let (name, key_b64) = contents
            .split_once(':')
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid key format: expected name:base64"))?;

        let key_bytes = data_encoding::BASE64
            .decode(key_b64.as_bytes())
            .map_err(|e| color_eyre::eyre::eyre!("invalid base64 in key: {e}"))?;

        // Nix stores the full 64-byte keypair (secret + public) in the file.
        if key_bytes.len() < 32 {
            return Err(color_eyre::eyre::eyre!(
                "key too short: expected at least 32 bytes, got {}",
                key_bytes.len()
            ));
        }

        let secret: [u8; 32] = key_bytes[..32].try_into()?;
        let signing_key = SigningKey::from_bytes(&secret);

        Ok(Self {
            key_name: name.to_string(),
            signing_key,
        })
    }

    /// Compute the nix narinfo fingerprint.
    ///
    /// Format: `1;{store_path};{nar_hash};{nar_size};{refs}`
    pub fn fingerprint(
        store_path: &str,
        nar_hash: &str,
        nar_size: u64,
        references: &[String],
    ) -> String {
        let refs = references.join(",");
        format!("1;{store_path};{nar_hash};{nar_size};{refs}")
    }

    /// Sign a narinfo fingerprint and return the signature in nix format.
    ///
    /// Returns `key-name:base64-signature`.
    pub fn sign(&self, fingerprint: &str) -> String {
        let signature = self.signing_key.sign(fingerprint.as_bytes());
        let sig_b64 = data_encoding::BASE64.encode(signature.to_bytes().as_ref());
        format!("{}:{sig_b64}", self.key_name)
    }

    pub fn key_name(&self) -> &str {
        &self.key_name
    }
}
