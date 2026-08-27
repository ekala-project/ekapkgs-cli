use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Package manifest embedded in ekaos system closures.
///
/// Generated at Nix evaluation time by the `package-manifest.nix` module
/// and placed at `<toplevel>/package-manifest.json`. Contains authoritative
/// package metadata (name, version, license, role) that would otherwise
/// require heuristic reconstruction from store paths.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageManifest {
    pub version: u32,
    pub system: String,
    pub ekaos_version: String,
    pub packages: Vec<ManifestEntry>,
}

/// A single package entry in the manifest.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestEntry {
    pub pname: String,
    pub version: String,
    pub store_path: String,
    #[serde(default)]
    pub outputs: HashMap<String, String>,
    #[serde(default)]
    pub license: Vec<LicenseInfo>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub source: String,
    /// CPE 2.3 identifier for vulnerability matching (e.g.,
    /// `cpe:2.3:a:gnu:hello:2.10:*:*:*:*:*:*:*`).
    #[serde(default)]
    pub cpe: Option<String>,
    /// Package URL identifier (e.g., `pkg:nix/nixpkgs/hello@2.10`).
    #[serde(default)]
    pub purl: Option<String>,
    /// Source provenance types (e.g., `["fromSource"]` or `["binaryNativeCode"]`).
    #[serde(default)]
    pub source_provenance: Vec<String>,
    /// Known CVE identifiers.
    #[serde(default)]
    pub known_vulnerabilities: Vec<String>,
    /// Changelog URL.
    #[serde(default)]
    pub changelog: String,
    /// Name of the main executable.
    #[serde(default)]
    pub main_program: String,
}

/// License metadata for a package.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseInfo {
    pub spdx_id: Option<String>,
    pub full_name: String,
}

/// Try to load a package manifest from a built store path.
///
/// Looks for `<store_path>/package-manifest.json`. Returns `None` if
/// the file does not exist or cannot be parsed (i.e., the target is
/// not an ekaos system closure).
pub fn load_manifest(store_path: &str) -> Option<PackageManifest> {
    let manifest_path = format!("{store_path}/package-manifest.json");
    let contents = std::fs::read_to_string(manifest_path).ok()?;
    serde_json::from_str(&contents).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_full_manifest() {
        let json = r#"{
            "version": 1,
            "system": "x86_64-linux",
            "ekaosVersion": "24.11",
            "packages": [
                {
                    "pname": "nginx",
                    "version": "1.26.2",
                    "storePath": "/nix/store/abc123-nginx-1.26.2",
                    "outputs": { "out": "/nix/store/abc123-nginx-1.26.2" },
                    "license": [
                        { "spdxId": "BSD-2-Clause", "fullName": "BSD 2-clause \"Simplified\" License" }
                    ],
                    "description": "A reverse proxy and lightweight HTTP server",
                    "homepage": "https://nginx.org",
                    "role": "service",
                    "source": "services.nginx"
                },
                {
                    "pname": "coreutils",
                    "version": "9.5",
                    "storePath": "/nix/store/def456-coreutils-9.5",
                    "license": [
                        { "spdxId": "GPL-3.0-or-later", "fullName": "GNU General Public License v3.0 or later" }
                    ],
                    "role": "default",
                    "source": "environment.defaultPackages"
                }
            ]
        }"#;

        let manifest: PackageManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.system, "x86_64-linux");
        assert_eq!(manifest.ekaos_version, "24.11");
        assert_eq!(manifest.packages.len(), 2);

        let nginx = &manifest.packages[0];
        assert_eq!(nginx.pname, "nginx");
        assert_eq!(nginx.version, "1.26.2");
        assert_eq!(nginx.role, "service");
        assert_eq!(nginx.source, "services.nginx");
        assert_eq!(nginx.license.len(), 1);
        assert_eq!(nginx.license[0].spdx_id.as_deref(), Some("BSD-2-Clause"));

        let coreutils = &manifest.packages[1];
        assert_eq!(coreutils.pname, "coreutils");
        assert_eq!(coreutils.role, "default");
        assert!(coreutils.outputs.is_empty()); // missing field defaults
        assert!(coreutils.description.is_empty());
    }

    #[test]
    fn deserialize_minimal_manifest() {
        let json = r#"{
            "version": 1,
            "system": "aarch64-linux",
            "ekaosVersion": "25.05",
            "packages": []
        }"#;

        let manifest: PackageManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.version, 1);
        assert!(manifest.packages.is_empty());
    }

    #[test]
    fn deserialize_entry_with_null_spdx_id() {
        let json = r#"{
            "pname": "custom-tool",
            "version": "0.1.0",
            "storePath": "/nix/store/xyz-custom-tool-0.1.0",
            "license": [{ "spdxId": null, "fullName": "Custom License" }],
            "role": "user",
            "source": "environment.systemPackages"
        }"#;

        let entry: ManifestEntry = serde_json::from_str(json).unwrap();
        assert!(entry.license[0].spdx_id.is_none());
        assert_eq!(entry.license[0].full_name, "Custom License");
    }

    #[test]
    fn load_manifest_nonexistent_path() {
        assert!(load_manifest("/nix/store/nonexistent-path").is_none());
    }
}
