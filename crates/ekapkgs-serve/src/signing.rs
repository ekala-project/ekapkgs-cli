use ed25519_dalek::{Signer, SigningKey};
use ekapkgs_protocol::ekapkgs::v1::{CertSignature, CertificateChain, SigningCertificate};

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
        let contents = std::fs::read_to_string(path)?.trim().to_owned();
        let (name, key_b64) = contents
            .split_once(':')
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid key format: expected name:base64"))?;

        let key_bytes = data_encoding::BASE64
            .decode(key_b64.as_bytes())
            .map_err(|e| color_eyre::eyre::eyre!("invalid base64 in key: {e}"))?;

        if key_bytes.len() < 32 {
            return Err(color_eyre::eyre::eyre!(
                "key too short: expected at least 32 bytes, got {}",
                key_bytes.len()
            ));
        }

        let secret: [u8; 32] = key_bytes[..32].try_into()?;
        let signing_key = SigningKey::from_bytes(&secret);

        Ok(Self {
            key_name: name.to_owned(),
            signing_key,
        })
    }

    /// Compute the nix narinfo fingerprint.
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
    pub fn sign(&self, fingerprint: &str) -> String {
        let signature = self.signing_key.sign(fingerprint.as_bytes());
        let sig_b64 = data_encoding::BASE64.encode(signature.to_bytes().as_ref());
        format!("{}:{sig_b64}", self.key_name)
    }

    pub fn key_name(&self) -> &str {
        &self.key_name
    }
}

/// Certificate-based signer that produces CertSignature alongside standard sigs.
pub struct CertSigner {
    cert_name: String,
    signing_key: SigningKey,
    pub chain: CertificateChain,
}

impl CertSigner {
    /// Load a certificate and its private key from files.
    pub fn from_files(
        cert_path: &std::path::Path,
        key_path: &std::path::Path,
    ) -> color_eyre::Result<Self> {
        let cert_json = std::fs::read_to_string(cert_path)?;
        let cert_file: CertFileFormat = serde_json::from_str(&cert_json)?;

        let key_contents = std::fs::read_to_string(key_path)?.trim().to_owned();
        let (_name, key_b64) = key_contents
            .split_once(':')
            .ok_or_else(|| color_eyre::eyre::eyre!("invalid key format"))?;
        let key_bytes = data_encoding::BASE64.decode(key_b64.as_bytes())?;
        let secret: [u8; 32] = key_bytes[..32].try_into()?;
        let signing_key = SigningKey::from_bytes(&secret);

        let signing_cert = SigningCertificate {
            name: cert_file.name.clone(),
            public_key: data_encoding::BASE64.decode(cert_file.public_key.as_bytes())?,
            not_before: cert_file.not_before,
            not_after: cert_file.not_after,
            issuer: cert_file.issuer,
            issuer_signature: data_encoding::BASE64
                .decode(cert_file.issuer_signature.as_bytes())?,
        };

        let chain = CertificateChain {
            signing_cert: Some(signing_cert),
            intermediates: Vec::new(),
        };

        Ok(Self {
            cert_name: cert_file.name,
            signing_key,
            chain,
        })
    }

    /// Sign a narinfo fingerprint with the certificate key.
    pub fn sign(&self, fingerprint: &str) -> CertSignature {
        let signature = self.signing_key.sign(fingerprint.as_bytes());
        CertSignature {
            cert_name: self.cert_name.clone(),
            signature: signature.to_bytes().to_vec(),
        }
    }
}

#[derive(serde::Deserialize)]
struct CertFileFormat {
    name: String,
    public_key: String,
    not_before: u64,
    not_after: u64,
    issuer: String,
    issuer_signature: String,
}
