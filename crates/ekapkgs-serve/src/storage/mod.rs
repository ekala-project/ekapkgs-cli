pub mod filesystem;
pub mod nix_store;

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
                .map(|r| {
                    r.rsplit('/')
                        .next()
                        .unwrap_or(r.as_str())
                })
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
                "StorePath" => store_path = Some(value.to_string()),
                "URL" => url = Some(value.to_string()),
                "Compression" => compression = Some(value.to_string()),
                "FileHash" => file_hash = Some(value.to_string()),
                "FileSize" => file_size = Some(value.parse().ok()?),
                "NarHash" => nar_hash = Some(value.to_string()),
                "NarSize" => nar_size = Some(value.parse().ok()?),
                "References" => {
                    references = value.split_whitespace().map(String::from).collect();
                }
                "Deriver" => deriver = Some(value.to_string()),
                "Sig" => signatures.push(value.to_string()),
                "CA" => ca = Some(value.to_string()),
                _ => {}
            }
        }

        Some(NarInfo {
            store_path: store_path?,
            url: url?,
            compression: compression.unwrap_or_else(|| "none".to_string()),
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
                available.push((*hash).to_string());
            }
        }
        Ok(available)
    }

    /// Get a NAR file as bytes. Returns `(data, content_type)`.
    fn get_nar(&self, file_path: &str) -> color_eyre::Result<Option<Vec<u8>>>;
}
