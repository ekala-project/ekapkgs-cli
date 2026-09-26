use std::collections::HashMap;

/// A single entry mapping a soname to its providing package and store path.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SonameEntry {
    /// The soname, e.g. `libxml2.so.2`.
    pub soname: String,
    /// The Nix package attribute name, e.g. `libxml2`.
    pub package: String,
    /// Full store path, e.g. `/nix/store/abc123...-libxml2-2.13.5`.
    pub store_path: String,
    /// Relative path inside the store path, e.g. `lib/libxml2.so.2.13.5`.
    pub file_path: String,
    /// From `meta.priority` — lower values are preferred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

/// In-memory soname index for resolving library names to Nix store paths.
pub struct SonameIndex {
    /// Best entry per soname (lowest priority wins).
    entries: HashMap<String, SonameEntry>,
    /// Sorted list of all sonames, for readdir.
    all_sonames: Vec<String>,
}

impl SonameIndex {
    /// Load the soname index from the local cache, falling back to remote download.
    pub fn load() -> color_eyre::Result<Self> {
        let data = if let Some(data) = crate::commands::search::read_index("sonames")? {
            data
        } else if let Ok(config) = crate::config::ClientConfig::load() {
            if let Some(url) = &config.defaults.index_url {
                crate::commands::search::try_download_index(url, "sonames")?
            } else {
                return Err(color_eyre::eyre::eyre!(
                    "No soname index available. Configure index_url in config.toml \
                     or provide a sonames.json.zst index file."
                ));
            }
        } else {
            return Err(color_eyre::eyre::eyre!(
                "No soname index available and no config found."
            ));
        };

        let raw_entries: Vec<SonameEntry> = serde_json::from_slice(&data)?;
        Ok(Self::from_entries(raw_entries))
    }

    /// Build the index from a flat list of entries, deduplicating by priority.
    fn from_entries(raw: Vec<SonameEntry>) -> Self {
        let mut entries: HashMap<String, SonameEntry> = HashMap::new();

        for entry in raw {
            let soname = entry.soname.clone();
            match entries.get(&soname) {
                Some(existing) => {
                    let existing_pri = existing.priority.unwrap_or(i32::MAX);
                    let new_pri = entry.priority.unwrap_or(i32::MAX);
                    if new_pri < existing_pri {
                        entries.insert(soname, entry);
                    }
                },
                None => {
                    entries.insert(soname, entry);
                },
            }
        }

        let mut all_sonames: Vec<String> = entries.keys().cloned().collect();
        all_sonames.sort();

        Self {
            entries,
            all_sonames,
        }
    }

    /// Look up a soname, returning the best-priority entry.
    pub fn lookup(&self, soname: &str) -> Option<&SonameEntry> {
        self.entries.get(soname)
    }

    /// All sonames in sorted order (for readdir).
    pub fn all_sonames(&self) -> &[String] {
        &self.all_sonames
    }

    /// Number of distinct sonames in the index.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}
