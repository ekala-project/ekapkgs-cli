pub mod castore;
pub mod filesystem;
pub mod nar;
pub mod nix_store;
#[cfg(feature = "s3")]
pub mod s3;

/// Parsed narinfo metadata.
#[derive(Debug, Clone)]
pub struct NarInfo {
    pub store_path: String,
    pub url: String,
    pub compression: String,
    pub file_hash: String,
    pub file_size: u64,
    pub nar_hash: String,
    pub nar_size: u64,
    pub references: Vec<String>,
    pub deriver: Option<String>,
    pub signatures: Vec<String>,
    pub ca: Option<String>,
}

impl NarInfo {
    /// Serialize to the standard narinfo text format.
    pub fn to_narinfo_string(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("StorePath: {}\n", self.store_path));
        s.push_str(&format!("URL: {}\n", self.url));
        s.push_str(&format!("Compression: {}\n", self.compression));
        s.push_str(&format!("FileHash: {}\n", self.file_hash));
        s.push_str(&format!("FileSize: {}\n", self.file_size));
        s.push_str(&format!("NarHash: {}\n", self.nar_hash));
        s.push_str(&format!("NarSize: {}\n", self.nar_size));
        if !self.references.is_empty() {
            // References are basenames, not full paths.
            let refs: Vec<&str> = self
                .references
                .iter()
                .map(|r| r.rsplit('/').next().unwrap_or(r.as_str()))
                .collect();
            s.push_str(&format!("References: {}\n", refs.join(" ")));
        }
        if let Some(ref deriver) = self.deriver {
            s.push_str(&format!("Deriver: {}\n", deriver));
        }
        for sig in &self.signatures {
            s.push_str(&format!("Sig: {}\n", sig));
        }
        if let Some(ref ca) = self.ca {
            s.push_str(&format!("CA: {}\n", ca));
        }
        s
    }

    /// Parse a narinfo from its text representation.
    pub fn parse(text: &str) -> Option<Self> {
        let mut store_path = None;
        let mut url = None;
        let mut compression = None;
        let mut file_hash = None;
        let mut file_size = None;
        let mut nar_hash = None;
        let mut nar_size = None;
        let mut references = Vec::new();
        let mut deriver = None;
        let mut signatures = Vec::new();
        let mut ca = None;

        for line in text.lines() {
            let (key, value) = line.split_once(": ")?;
            match key {
                "StorePath" => store_path = Some(value.to_owned()),
                "URL" => url = Some(value.to_owned()),
                "Compression" => compression = Some(value.to_owned()),
                "FileHash" => file_hash = Some(value.to_owned()),
                "FileSize" => file_size = Some(value.parse().ok()?),
                "NarHash" => nar_hash = Some(value.to_owned()),
                "NarSize" => nar_size = Some(value.parse().ok()?),
                "References" => {
                    references = value.split_whitespace().map(String::from).collect();
                },
                "Deriver" => deriver = Some(value.to_owned()),
                "Sig" => signatures.push(value.to_owned()),
                "CA" => ca = Some(value.to_owned()),
                _ => {},
            }
        }

        Some(NarInfo {
            store_path: store_path?,
            url: url?,
            compression: compression.unwrap_or_else(|| "none".to_owned()),
            file_hash: file_hash.unwrap_or_default(),
            file_size: file_size.unwrap_or(0),
            nar_hash: nar_hash?,
            nar_size: nar_size?,
            references,
            deriver,
            signatures,
            ca,
        })
    }

    /// Extract the hash portion of the store path.
    pub fn store_path_hash(&self) -> Option<&str> {
        let basename = self.store_path.rsplit('/').next()?;
        basename.split('-').next()
    }
}

/// Abstract storage backend for the binary cache server.
pub trait StorageBackend: Send + Sync {
    /// Downcast to a concrete type for backend-specific operations.
    fn as_any(&self) -> &dyn std::any::Any;
    /// Check if a narinfo exists for the given store path hash.
    fn has_narinfo(&self, hash: &str) -> color_eyre::Result<bool>;

    /// Get narinfo for a store path hash.
    fn get_narinfo(&self, hash: &str) -> color_eyre::Result<Option<NarInfo>>;

    /// Get the raw narinfo text for a store path hash (for serving).
    fn get_narinfo_text(&self, hash: &str) -> color_eyre::Result<Option<String>>;

    /// Batch check: which of these hashes have narinfos?
    fn query_available(&self, hashes: &[&str]) -> color_eyre::Result<Vec<String>> {
        let mut available = Vec::new();
        for hash in hashes {
            if self.has_narinfo(hash)? {
                available.push((*hash).to_owned());
            }
        }
        Ok(available)
    }

    /// Get a NAR file as bytes. Returns `(data, content_type)`.
    fn get_nar(&self, file_path: &str) -> color_eyre::Result<Option<Vec<u8>>>;

    /// Store a narinfo. Returns false if the backend is read-only.
    fn put_narinfo(&self, hash: &str, content: &str) -> color_eyre::Result<bool> {
        let _ = (hash, content);
        Ok(false)
    }

    /// Store a NAR file. Returns false if the backend is read-only.
    fn put_nar(&self, file_path: &str, data: &[u8]) -> color_eyre::Result<bool> {
        let _ = (file_path, data);
        Ok(false)
    }

    /// Whether this backend supports content-addressed chunk operations.
    fn supports_cas(&self) -> bool {
        false
    }

    /// Get a chunk by its blake3 digest.
    fn get_chunk(&self, _digest: &[u8]) -> color_eyre::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Get the serialized CaNode root for a store path hash.
    fn get_cas_root(&self, _hash: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_NARINFO: &str = "\
StorePath: /nix/store/abc123def456-hello-2.12.1
URL: nar/abc123def456.nar.xz
Compression: xz
FileHash: sha256:1b2c3d4e5f
FileSize: 12345
NarHash: sha256:a1b2c3d4e5
NarSize: 67890
References: abc123def456-hello-2.12.1 xyz789-glibc-2.39
Deriver: qqq111-hello-2.12.1.drv
Sig: cache.example.org-1:base64sig==
CA: text:sha256:cafebabe
";

    #[test]
    fn parse_narinfo_roundtrip() {
        let ni = NarInfo::parse(SAMPLE_NARINFO).expect("should parse");
        assert_eq!(ni.store_path, "/nix/store/abc123def456-hello-2.12.1");
        assert_eq!(ni.url, "nar/abc123def456.nar.xz");
        assert_eq!(ni.compression, "xz");
        assert_eq!(ni.file_hash, "sha256:1b2c3d4e5f");
        assert_eq!(ni.file_size, 12345);
        assert_eq!(ni.nar_hash, "sha256:a1b2c3d4e5");
        assert_eq!(ni.nar_size, 67890);
        assert_eq!(ni.references.len(), 2);
        assert_eq!(ni.deriver.as_deref(), Some("qqq111-hello-2.12.1.drv"));
        assert_eq!(ni.signatures, vec!["cache.example.org-1:base64sig=="]);
        assert_eq!(ni.ca.as_deref(), Some("text:sha256:cafebabe"));
    }

    #[test]
    fn narinfo_store_path_hash() {
        let ni = NarInfo::parse(SAMPLE_NARINFO).unwrap();
        assert_eq!(ni.store_path_hash(), Some("abc123def456"));
    }

    #[test]
    fn parse_narinfo_minimal() {
        let text = "\
StorePath: /nix/store/zzz-minimal-1.0
URL: nar/zzz.nar
NarHash: sha256:deadbeef
NarSize: 100
";
        let ni = NarInfo::parse(text).expect("should parse minimal");
        assert_eq!(ni.store_path, "/nix/store/zzz-minimal-1.0");
        assert_eq!(ni.compression, "none");
        assert!(ni.references.is_empty());
        assert!(ni.signatures.is_empty());
    }

    #[test]
    fn narinfo_serialize_deserialize() {
        let ni = NarInfo::parse(SAMPLE_NARINFO).unwrap();
        let text = ni.to_narinfo_string();
        let ni2 = NarInfo::parse(&text).expect("should re-parse");
        assert_eq!(ni.store_path, ni2.store_path);
        assert_eq!(ni.nar_hash, ni2.nar_hash);
        assert_eq!(ni.nar_size, ni2.nar_size);
        assert_eq!(ni.file_size, ni2.file_size);
    }

    #[test]
    fn parse_narinfo_missing_required_fields() {
        assert!(NarInfo::parse("URL: nar/x.nar\nNarHash: sha256:abc\nNarSize: 1\n").is_none());
        assert!(
            NarInfo::parse("StorePath: /nix/store/x\nNarHash: sha256:abc\nNarSize: 1\n").is_none()
        );
    }

    #[test]
    fn narinfo_multiple_signatures() {
        let text = "\
StorePath: /nix/store/aaa-pkg-1.0
URL: nar/aaa.nar
NarHash: sha256:abc
NarSize: 100
Sig: key1:sig1==
Sig: key2:sig2==
Sig: key3:sig3==
";
        let ni = NarInfo::parse(text).unwrap();
        assert_eq!(ni.signatures.len(), 3);
    }
}
