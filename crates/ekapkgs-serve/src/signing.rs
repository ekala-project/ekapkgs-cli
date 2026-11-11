use ed25519_dalek::{Signer, SigningKey};
use ekapkgs_protocol::ekapkgs::v1::{CertSignature, CertificateChain, SigningCertificate};
use tracing::log::info;

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
        info!("Loading key at {}", path.display());
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
    ///
    /// Matches nix's `ValidPathInfo::fingerprint`: the NAR hash is rendered
    /// as `<algo>:<nix32>` and references as sorted full store paths, even
    /// though narinfos commonly carry an SRI hash and bare reference names.
    /// Signatures over any other rendering are rejected by nix.
    pub fn fingerprint(
        store_path: &str,
        nar_hash: &str,
        nar_size: u64,
        references: &[String],
    ) -> String {
        let store_dir = store_path
            .rsplit_once('/')
            .map_or("/nix/store", |(dir, _)| dir);
        let mut refs: Vec<String> = references
            .iter()
            .map(|r| {
                if r.starts_with('/') {
                    r.clone()
                } else {
                    format!("{store_dir}/{r}")
                }
            })
            .collect();
        refs.sort();
        let refs = refs.join(",");
        let nar_hash = normalize_hash(nar_hash);
        format!("1;{store_path};{nar_hash};{nar_size};{refs}")
    }

    /// Sign `fingerprint` and add the signature to `signatures`, replacing any
    /// earlier signature under this key name (e.g. one over a stale
    /// fingerprint format).
    pub fn apply_signature(&self, signatures: &mut Vec<String>, fingerprint: &str) {
        let sig = self.sign(fingerprint);
        let prefix = format!("{}:", self.key_name);
        signatures.retain(|s| !s.starts_with(&prefix));
        signatures.push(sig);
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

    /// Return the public key in nix format: `key-name:base64-public-key`.
    pub fn public_key(&self) -> String {
        let pub_b64 =
            data_encoding::BASE64.encode(self.signing_key.verifying_key().as_bytes().as_ref());
        format!("{}:{pub_b64}", self.key_name)
    }
}

/// Nix base32 alphabet (omits `e`, `o`, `t`, `u`).
const NIX32_ALPHABET: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// Render a hash as `<algo>:<nix32>`, the form nix uses in fingerprints.
///
/// Accepts SRI (`sha256-<base64>`) and `<algo>:<hex|nix32|base64>` input.
/// Unrecognised input is returned unchanged.
fn normalize_hash(hash: &str) -> String {
    let (algo, digest, is_sri) = if let Some((algo, digest)) = hash.split_once(':') {
        (algo, digest, false)
    } else if let Some((algo, digest)) = hash.split_once('-') {
        (algo, digest, true)
    } else {
        return hash.to_owned();
    };

    let size: usize = match algo {
        "md5" => 16,
        "sha1" => 20,
        "sha256" => 32,
        "sha512" => 64,
        _ => return hash.to_owned(),
    };
    let nix32_len = (size * 8 - 1) / 5 + 1;

    let bytes = if is_sri || digest.len() == size.div_ceil(3) * 4 {
        data_encoding::BASE64.decode(digest.as_bytes()).ok()
    } else if digest.len() == size * 2 {
        data_encoding::HEXLOWER_PERMISSIVE
            .decode(digest.as_bytes())
            .ok()
    } else if digest.len() == nix32_len && digest.bytes().all(|b| NIX32_ALPHABET.contains(&b)) {
        return format!("{algo}:{digest}");
    } else {
        None
    };

    match bytes {
        Some(bytes) if bytes.len() == size => format!("{algo}:{}", nix32_encode(&bytes)),
        _ => hash.to_owned(),
    }
}

/// Encode bytes in nix base32, which reads the input from its last bit.
fn nix32_encode(bytes: &[u8]) -> String {
    let len = (bytes.len() * 8 - 1) / 5 + 1;
    (0..len)
        .rev()
        .map(|n| {
            let b = n * 5;
            let (i, j) = (b / 8, b % 8);
            let lo = bytes[i] >> j;
            let hi = bytes
                .get(i + 1)
                .map_or(0, |&next| next.checked_shl((8 - j) as u32).unwrap_or(0));
            NIX32_ALPHABET[((lo | hi) & 0x1f) as usize] as char
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    // `nix hash convert --hash-algo sha256 --to nix32
    // sha256-Kfq38z2JwPPa59Si5B/CZOEl8BQo5TtA6ZQRFaSn9Dw=`
    const SRI: &str = "sha256-Kfq38z2JwPPa59Si5B/CZOEl8BQo5TtA6ZQRFaSn9Dw=";
    const NIX32: &str = "sha256:0g7llyj1a4clx503pr982kq2bqb4q8gy98nlwzdg7h497prvgyi9";

    #[test]
    fn normalize_sri_hash() {
        assert_eq!(normalize_hash(SRI), NIX32);
    }

    #[test]
    fn normalize_nix32_hash_unchanged() {
        assert_eq!(normalize_hash(NIX32), NIX32);
    }

    #[test]
    fn normalize_hex_hash() {
        let hex = "sha256:29fab7f33d89c0f3dae7d4a2e41fc264e125f01428e53b40e9941115a4a7f43c";
        assert_eq!(normalize_hash(hex), NIX32);
    }

    #[test]
    fn normalize_unknown_hash_unchanged() {
        assert_eq!(normalize_hash("blake3:abc"), "blake3:abc");
    }

    #[test]
    fn fingerprint_matches_nix_format() {
        let fp = NarInfoSigner::fingerprint(
            "/nix/store/m9qcqddh9x03kd48pzqh85r0fpk7ljmg-ekapkgs-test-a",
            SRI,
            176,
            &[
                "zzz-last".to_owned(),
                "/nix/store/98iv8z3f0nvjgvlr10n4wmhpbh78rk0q-ekapkgs-test-b".to_owned(),
            ],
        );
        assert_eq!(
            fp,
            format!(
                "1;/nix/store/m9qcqddh9x03kd48pzqh85r0fpk7ljmg-ekapkgs-test-a;{NIX32};176;/nix/\
                 store/98iv8z3f0nvjgvlr10n4wmhpbh78rk0q-ekapkgs-test-b,/nix/store/zzz-last"
            )
        );
    }
}
