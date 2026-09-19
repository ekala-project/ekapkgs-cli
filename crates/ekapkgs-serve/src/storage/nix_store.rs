use ekapkgs_nix::NixCommand;
use serde::Deserialize;

use super::{NarInfo, StorageBackend};

/// Storage backend that serves directly from the local `/nix/store`.
///
/// Equivalent to `nix-serve`: queries the nix daemon for path metadata
/// and streams NARs on-the-fly via `nix-store --dump`.
pub struct NixStoreBackend;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NixPathInfo {
    path: String,
    nar_hash: String,
    nar_size: u64,
    #[serde(default)]
    references: Vec<String>,
    deriver: Option<String>,
    #[serde(default)]
    signatures: Vec<String>,
    ca: Option<String>,
}

impl NixStoreBackend {
    pub fn new() -> Self {
        Self
    }

    /// Resolve a hash prefix to a full store path using `nix path-info`.
    fn resolve_path(&self, hash: &str) -> color_eyre::Result<Option<String>> {
        // nix path-info accepts a hash prefix and returns the full path.
        // We use --json to parse the output.
        let result = NixCommand::new(&["path-info", "--json"])
            .arg(format!("/nix/store/{hash}"))
            .output();

        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                // nix path-info --json returns an array of path info objects
                let infos: Vec<NixPathInfo> = serde_json::from_str(&stdout)?;
                Ok(infos.into_iter().next().map(|i| i.path))
            },
            Err(ekapkgs_nix::NixError::Failed { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn get_path_info(&self, hash: &str) -> color_eyre::Result<Option<NixPathInfo>> {
        let result = NixCommand::new(&["path-info", "--json"])
            .arg(format!("/nix/store/{hash}"))
            .output();

        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let infos: Vec<NixPathInfo> = serde_json::from_str(&stdout)?;
                Ok(infos.into_iter().next())
            },
            Err(ekapkgs_nix::NixError::Failed { .. }) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

impl StorageBackend for NixStoreBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn has_narinfo(&self, hash: &str) -> color_eyre::Result<bool> {
        Ok(self.resolve_path(hash)?.is_some())
    }

    fn get_narinfo(&self, hash: &str) -> color_eyre::Result<Option<NarInfo>> {
        let Some(info) = self.get_path_info(hash)? else {
            return Ok(None);
        };

        // Construct a NarInfo from the nix path-info JSON.
        // The URL is synthesized for on-the-fly NAR streaming.
        Ok(Some(NarInfo {
            store_path: info.path,
            url: format!("nar/{hash}.nar"),
            compression: "none".to_owned(),
            file_hash: String::new(),
            file_size: 0,
            nar_hash: info.nar_hash,
            nar_size: info.nar_size,
            references: info.references,
            deriver: info.deriver,
            signatures: info.signatures,
            ca: info.ca,
        }))
    }

    fn get_narinfo_text(&self, hash: &str) -> color_eyre::Result<Option<String>> {
        Ok(self.get_narinfo(hash)?.map(|ni| ni.to_narinfo_string()))
    }

    fn get_nar(&self, file_path: &str) -> color_eyre::Result<Option<Vec<u8>>> {
        // Extract the hash from the NAR file path.
        // Expected format: nar/{hash}.nar or nar/{hash}.nar.zst
        let filename = file_path.rsplit('/').next().unwrap_or(file_path);
        let hash = filename.split('.').next().unwrap_or(filename);

        let Some(store_path) = self.resolve_path(hash)? else {
            return Ok(None);
        };

        // Use nix-store --dump to produce NAR output.
        let output = std::process::Command::new("nix-store")
            .arg("--dump")
            .arg(&store_path)
            .output()?;

        if !output.status.success() {
            return Ok(None);
        }

        Ok(Some(output.stdout))
    }
}
