pub mod castore;
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
        if !self.file_hash.is_empty() {
            s.push_str(&format!("FileHash: {}\n", self.file_hash));
        }
        if self.file_size > 0 {
            s.push_str(&format!("FileSize: {}\n", self.file_size));
        }
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

/// Return type for batch narinfo queries: `(available, unavailable)`.
type BatchNarInfoResult = (Vec<(String, NarInfo)>, Vec<String>);

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

    /// Batch fetch narinfos for multiple hashes at once.
    ///
    /// Returns `(available, unavailable)` where `available` contains
    /// `(hash, NarInfo)` pairs and `unavailable` contains hashes that
    /// were not found or errored. Default falls back to serial calls.
    #[allow(clippy::type_complexity)]
    fn batch_get_narinfo(
        &self,
        hashes: &[String],
    ) -> color_eyre::Result<(Vec<(String, NarInfo)>, Vec<String>)> {
        let mut available = Vec::new();
        let mut unavailable = Vec::new();
        for hash in hashes {
            match self.get_narinfo(hash) {
                Ok(Some(ni)) => available.push((hash.clone(), ni)),
                Ok(None) | Err(_) => unavailable.push(hash.clone()),
            }
        }
        Ok((available, unavailable))
    }

    /// Get a NAR file as bytes.
    fn get_nar(&self, file_path: &str) -> color_eyre::Result<Option<Vec<u8>>>;

    /// Check if a NAR exists without reading its data.
    ///
    /// Used for existence probes before streaming. Default implementation
    /// falls back to `has_narinfo` for the path hash extracted from the URL.
    fn has_nar(&self, file_path: &str) -> color_eyre::Result<bool> {
        let filename = file_path.rsplit('/').next().unwrap_or(file_path);
        let hash = filename.split('.').next().unwrap_or(filename);
        self.has_narinfo(hash)
    }

    /// Write a NAR to the given writer, streaming where possible.
    ///
    /// Returns `Ok(false)` if the NAR was not found. Default implementation
    /// falls back to `get_nar()` + `write_all()`.
    fn write_nar_to(
        &self,
        file_path: &str,
        w: &mut dyn std::io::Write,
    ) -> color_eyre::Result<bool> {
        match self.get_nar(file_path)? {
            Some(data) => {
                w.write_all(&data)?;
                Ok(true)
            },
            None => Ok(false),
        }
    }

    /// Stream a NAR into a channel as fixed-size byte chunks.
    ///
    /// Sends `Vec<u8>` chunks through the provided sender. Returns `false`
    /// if the NAR was not found. The default implementation calls
    /// `write_nar_to` with a `ChannelWriter` adapter.
    fn stream_nar_chunks(
        &self,
        file_path: &str,
        chunk_size: usize,
        tx: &std::sync::mpsc::SyncSender<Vec<u8>>,
    ) -> color_eyre::Result<bool> {
        let mut writer = ChannelWriter::new(chunk_size, tx.clone());
        let found = self.write_nar_to(file_path, &mut writer)?;
        if found {
            writer.flush_final()?;
        }
        Ok(found)
    }

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
    ///
    /// Returns `Bytes` for zero-copy handoff to the HTTP response layer.
    fn get_chunk(&self, _digest: &[u8]) -> color_eyre::Result<Option<bytes::Bytes>> {
        Ok(None)
    }

    /// Check if a chunk exists without reading its data.
    fn has_chunk(&self, digest: &[u8]) -> color_eyre::Result<bool> {
        Ok(self.get_chunk(digest)?.is_some())
    }

    /// Get the serialized CaNode root for a store path hash.
    fn get_cas_root(&self, _hash: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        Ok(None)
    }
}

/// `Write` adapter that buffers into `chunk_size` blocks and sends each
/// completed block through a synchronous channel. Used by
/// `stream_nar_chunks` to shuttle NAR data from a blocking writer thread
/// to an async consumer without materializing the entire NAR.
pub struct ChannelWriter {
    buf: Vec<u8>,
    chunk_size: usize,
    tx: std::sync::mpsc::SyncSender<Vec<u8>>,
}

impl ChannelWriter {
    pub fn new(chunk_size: usize, tx: std::sync::mpsc::SyncSender<Vec<u8>>) -> Self {
        Self {
            buf: Vec::with_capacity(chunk_size),
            chunk_size,
            tx,
        }
    }

    /// Flush any remaining buffered data as a final (possibly short) chunk.
    pub fn flush_final(&mut self) -> std::io::Result<()> {
        if !self.buf.is_empty() {
            let chunk = std::mem::take(&mut self.buf);
            self.tx.send(chunk).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver dropped")
            })?;
        }
        Ok(())
    }
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while written < data.len() {
            let space = self.chunk_size - self.buf.len();
            let n = space.min(data.len() - written);
            self.buf.extend_from_slice(&data[written..written + n]);
            written += n;

            if self.buf.len() >= self.chunk_size {
                let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(self.chunk_size));
                self.tx.send(chunk).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver dropped")
                })?;
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
