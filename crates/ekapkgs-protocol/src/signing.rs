use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::ekapkgs::v1::{CertSignature, CertificateChain, SigningCertificate};

#[derive(Debug, thiserror::Error)]
pub enum CertError {
    #[error("certificate has no signing cert")]
    MissingCert,
    #[error("issuer '{issuer}' not in trusted roots")]
    UntrustedIssuer { issuer: String },
    #[error("certificate '{name}' has expired (not_after={not_after}, now={now})")]
    Expired { name: String, not_after: u64, now: u64 },
    #[error("certificate '{name}' is not yet valid (not_before={not_before}, now={now})")]
    NotYetValid { name: String, not_before: u64, now: u64 },
    #[error("invalid issuer signature on certificate '{name}'")]
    InvalidIssuerSignature { name: String },
    #[error("invalid path signature: {0}")]
    InvalidPathSignature(String),
    #[error("invalid key length: expected {expected}, got {got}")]
    InvalidKeyLength { expected: usize, got: usize },
    #[error("ed25519 error: {0}")]
    Ed25519(#[from] ed25519_dalek::SignatureError),
}

/// A trusted root CA public key.
pub struct TrustedRoot {
    pub name: String,
    pub public_key: VerifyingKey,
}

/// Compute the canonical bytes that a CA signs over to issue a certificate.
///
/// Format: `name || public_key(32) || not_before(8 LE) || not_after(8 LE) || issuer`
pub fn certificate_sign_payload(cert: &SigningCertificate) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(cert.name.as_bytes());
    payload.extend_from_slice(&cert.public_key);
    payload.extend_from_slice(&cert.not_before.to_le_bytes());
    payload.extend_from_slice(&cert.not_after.to_le_bytes());
    payload.extend_from_slice(cert.issuer.as_bytes());
    payload
}

/// Verify a certificate chain against a set of trusted root CAs.
pub fn verify_chain(
    chain: &CertificateChain,
    trusted_roots: &[TrustedRoot],
    now_unix: u64,
) -> Result<VerifyingKey, CertError> {
    let cert = chain
        .signing_cert
        .as_ref()
        .ok_or(CertError::MissingCert)?;

    // Find the trusted root that matches the issuer.
    let root = trusted_roots
        .iter()
        .find(|r| r.name == cert.issuer)
        .ok_or_else(|| CertError::UntrustedIssuer {
            issuer: cert.issuer.clone(),
        })?;

    // Check validity period.
    if now_unix < cert.not_before {
        return Err(CertError::NotYetValid {
            name: cert.name.clone(),
            not_before: cert.not_before,
            now: now_unix,
        });
    }
    if now_unix > cert.not_after {
        return Err(CertError::Expired {
            name: cert.name.clone(),
            not_after: cert.not_after,
            now: now_unix,
        });
    }

    // Verify issuer signature.
    let payload = certificate_sign_payload(cert);
    let sig_bytes: [u8; 64] = cert
        .issuer_signature
        .as_slice()
        .try_into()
        .map_err(|_| CertError::InvalidKeyLength {
            expected: 64,
            got: cert.issuer_signature.len(),
        })?;
    let signature = Signature::from_bytes(&sig_bytes);
    root.public_key
        .verify(&payload, &signature)
        .map_err(|_| CertError::InvalidIssuerSignature {
            name: cert.name.clone(),
        })?;

    // Extract the cert's public key for verifying individual path signatures.
    let key_bytes: [u8; 32] = cert
        .public_key
        .as_slice()
        .try_into()
        .map_err(|_| CertError::InvalidKeyLength {
            expected: 32,
            got: cert.public_key.len(),
        })?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes)?;

    Ok(verifying_key)
}

/// Verify a certificate-based path signature.
pub fn verify_path_signature(
    verifying_key: &VerifyingKey,
    fingerprint: &str,
    cert_sig: &CertSignature,
) -> Result<(), CertError> {
    let sig_bytes: [u8; 64] = cert_sig
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| CertError::InvalidKeyLength {
            expected: 64,
            got: cert_sig.signature.len(),
        })?;
    let signature = Signature::from_bytes(&sig_bytes);
    verifying_key
        .verify(fingerprint.as_bytes(), &signature)
        .map_err(|e| CertError::InvalidPathSignature(e.to_string()))
}

/// Generate a new ed25519 keypair for use as a CA or signing key.
pub fn generate_keypair() -> (SigningKey, VerifyingKey) {
    let mut csprng = rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut csprng);
    let verifying_key = signing_key.verifying_key();
    (signing_key, verifying_key)
}

/// Issue a signing certificate signed by a CA key.
pub fn issue_certificate(
    ca_key: &SigningKey,
    ca_name: &str,
    cert_name: &str,
    cert_public_key: &VerifyingKey,
    not_before: u64,
    not_after: u64,
) -> SigningCertificate {
    let cert = SigningCertificate {
        name: cert_name.to_string(),
        public_key: cert_public_key.as_bytes().to_vec(),
        not_before,
        not_after,
        issuer: ca_name.to_string(),
        issuer_signature: Vec::new(), // placeholder, computed below
    };

    let payload = certificate_sign_payload(&cert);
    let signature = ca_key.sign(&payload);

    SigningCertificate {
        issuer_signature: signature.to_bytes().to_vec(),
        ..cert
    }
}
