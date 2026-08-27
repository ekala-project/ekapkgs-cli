use std::collections::HashMap;
use std::io::Write;

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::manifest::{self, ManifestEntry, PackageManifest};
use ekapkgs_nix::store::{self, PathInfoEntry};
use ekapkgs_nix::{NixCommand, eval};
use serde::Serialize;

use crate::cli::SbomFormat;

pub fn execute(
    installable: &str,
    format: &SbomFormat,
    buildtime: bool,
    output: Option<&str>,
) -> color_eyre::Result<()> {
    let inst = Installable::new(installable);

    // Step 1: Build the installable to realize it in the store.
    // We need the output paths to exist for `nix path-info -r` to work.
    let outputs: Vec<eval::BuildOutput> = NixCommand::new(&["build"])
        .arg(installable)
        .arg("--json")
        .json()?;

    let root_path = outputs
        .first()
        .and_then(|o| o.outputs.get("out").cloned())
        .ok_or_else(|| color_eyre::eyre::eyre!("no output path for installable"))?;

    // Step 2: Try to load embedded package manifest (ekaos systems only).
    let manifest = manifest::load_manifest(&root_path);
    if manifest.is_some() {
        tracing::info!("Found embedded package manifest");
    }

    // Step 3: Query the closure (paths must be realized in the store).
    let spinner = ekapkgs_ui::progress::spinner("Querying closure...");
    let closure_entries = if buildtime {
        // For buildtime, get all derivation output paths and query their info.
        let paths = eval::derivation_closure_paths(&inst)?;
        // We can't get references for unbuilt paths, so just create
        // entries with the paths we have.
        paths
            .into_iter()
            .map(|path| PathInfoEntry {
                path,
                nar_size: 0,
                closure_size: 0,
                references: Vec::new(),
            })
            .collect()
    } else {
        store::closure_path_info(&inst)?
    };
    spinner.finish_and_clear();

    // Step 4: Build the component list.
    let manifest_index = build_manifest_index(manifest.as_ref());
    let components: Vec<SbomComponent> = closure_entries
        .iter()
        .map(|entry| build_component(entry, &manifest_index))
        .collect();

    // Step 5: Format and write output.
    let writer: Box<dyn Write> = match output {
        Some(path) => Box::new(std::fs::File::create(path)?),
        None => Box::new(std::io::stdout().lock()),
    };

    match format {
        SbomFormat::Cyclonedx => write_cyclonedx(
            writer,
            installable,
            &root_path,
            &components,
            &closure_entries,
        ),
        SbomFormat::Csv => write_csv(writer, &components),
    }
}

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

struct SbomComponent {
    bom_ref: String,
    pname: String,
    version: String,
    description: String,
    homepage: String,
    licenses: Vec<SbomLicense>,
    store_path: String,
    nar_size: u64,
    role: String,
    source: String,
    cpe: Option<String>,
    purl: Option<String>,
    source_provenance: Vec<String>,
    known_vulnerabilities: Vec<String>,
    changelog: String,
    main_program: String,
}

struct SbomLicense {
    spdx_id: Option<String>,
    name: String,
}

// ---------------------------------------------------------------------------
// Component construction
// ---------------------------------------------------------------------------

fn build_manifest_index(manifest: Option<&PackageManifest>) -> HashMap<String, &ManifestEntry> {
    let mut index = HashMap::new();
    if let Some(m) = manifest {
        for entry in &m.packages {
            index.insert(entry.store_path.clone(), entry);

            // Also index by output paths so transitive deps can match.
            for output_path in entry.outputs.values() {
                if output_path != &entry.store_path {
                    index.insert(output_path.clone(), entry);
                }
            }
        }
    }
    index
}

fn build_component(
    entry: &PathInfoEntry,
    manifest_index: &HashMap<String, &ManifestEntry>,
) -> SbomComponent {
    let bom_ref = store::store_path_hash(&entry.path)
        .unwrap_or("unknown")
        .to_owned();

    if let Some(manifest_entry) = manifest_index.get(&entry.path) {
        SbomComponent {
            bom_ref,
            pname: manifest_entry.pname.clone(),
            version: manifest_entry.version.clone(),
            description: manifest_entry.description.clone(),
            homepage: manifest_entry.homepage.clone(),
            licenses: manifest_entry
                .license
                .iter()
                .map(|l| SbomLicense {
                    spdx_id: l.spdx_id.clone(),
                    name: l.full_name.clone(),
                })
                .collect(),
            store_path: entry.path.clone(),
            nar_size: entry.nar_size,
            role: manifest_entry.role.clone(),
            source: manifest_entry.source.clone(),
            cpe: manifest_entry.cpe.clone(),
            purl: manifest_entry.purl.clone(),
            source_provenance: manifest_entry.source_provenance.clone(),
            known_vulnerabilities: manifest_entry.known_vulnerabilities.clone(),
            changelog: manifest_entry.changelog.clone(),
            main_program: manifest_entry.main_program.clone(),
        }
    } else {
        // Heuristic fallback.
        let (pname, version) = store::parse_store_path_name(&entry.path);
        SbomComponent {
            bom_ref,
            pname: pname.to_owned(),
            version: version.to_owned(),
            description: String::new(),
            homepage: String::new(),
            licenses: Vec::new(),
            store_path: entry.path.clone(),
            nar_size: entry.nar_size,
            role: String::new(),
            source: String::new(),
            cpe: None,
            purl: None,
            source_provenance: Vec::new(),
            known_vulnerabilities: Vec::new(),
            changelog: String::new(),
            main_program: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// CycloneDX 1.5 JSON output
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CycloneDxBom {
    bom_format: &'static str,
    spec_version: &'static str,
    version: u32,
    serial_number: String,
    metadata: CdxMetadata,
    components: Vec<CdxComponent>,
    dependencies: Vec<CdxDependency>,
}

#[derive(Serialize)]
struct CdxMetadata {
    timestamp: String,
    tools: Vec<CdxTool>,
    component: CdxComponent,
}

#[derive(Serialize)]
struct CdxTool {
    name: String,
    version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CdxComponent {
    #[serde(rename = "type")]
    component_type: &'static str,
    #[serde(rename = "bom-ref")]
    bom_ref: String,
    name: String,
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// CPE 2.3 identifier for vulnerability matching.
    #[serde(skip_serializing_if = "Option::is_none")]
    cpe: Option<String>,
    /// Package URL identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    purl: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    licenses: Vec<CdxLicenseChoice>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    external_references: Vec<CdxExternalRef>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    properties: Vec<CdxProperty>,
}

#[derive(Serialize)]
struct CdxLicenseChoice {
    license: CdxLicense,
}

#[derive(Serialize)]
struct CdxLicense {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

#[derive(Serialize)]
struct CdxExternalRef {
    #[serde(rename = "type")]
    ref_type: &'static str,
    url: String,
}

#[derive(Serialize)]
struct CdxProperty {
    name: String,
    value: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CdxDependency {
    #[serde(rename = "ref")]
    dep_ref: String,
    depends_on: Vec<String>,
}

fn write_cyclonedx(
    writer: impl Write,
    installable: &str,
    root_path: &str,
    components: &[SbomComponent],
    closure_entries: &[PathInfoEntry],
) -> color_eyre::Result<()> {
    let serial = format!("urn:uuid:{}", uuid::Uuid::new_v4());
    let timestamp = chrono_like_timestamp();

    let root_bom_ref = store::store_path_hash(root_path)
        .unwrap_or("root")
        .to_owned();

    let root_component = CdxComponent {
        component_type: "application",
        bom_ref: root_bom_ref.clone(),
        name: installable.to_owned(),
        version: String::new(),
        description: None,
        cpe: None,
        purl: None,
        licenses: Vec::new(),
        external_references: Vec::new(),
        properties: vec![CdxProperty {
            name: "nix:store_path".into(),
            value: root_path.to_owned(),
        }],
    };

    let cdx_components: Vec<CdxComponent> = components
        .iter()
        .filter(|c| c.store_path != root_path)
        .map(component_to_cdx)
        .collect();

    // Build dependency graph from closure references.
    let hash_set: HashMap<&str, &str> = closure_entries
        .iter()
        .filter_map(|e| {
            let hash = store::store_path_hash(&e.path)?;
            Some((e.path.as_str(), hash))
        })
        .collect();

    let dependencies: Vec<CdxDependency> = closure_entries
        .iter()
        .filter_map(|entry| {
            let dep_ref = store::store_path_hash(&entry.path)?.to_owned();
            let depends_on: Vec<String> = entry
                .references
                .iter()
                .filter(|r| r.as_str() != entry.path)
                .filter_map(|r| hash_set.get(r.as_str()).map(|h| (*h).to_owned()))
                .collect();
            Some(CdxDependency {
                dep_ref,
                depends_on,
            })
        })
        .collect();

    let bom = CycloneDxBom {
        bom_format: "CycloneDX",
        spec_version: "1.5",
        version: 1,
        serial_number: serial,
        metadata: CdxMetadata {
            timestamp,
            tools: vec![CdxTool {
                name: "ekapkgs".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            }],
            component: root_component,
        },
        components: cdx_components,
        dependencies,
    };

    serde_json::to_writer_pretty(writer, &bom)?;
    Ok(())
}

fn component_to_cdx(c: &SbomComponent) -> CdxComponent {
    let licenses: Vec<CdxLicenseChoice> = c
        .licenses
        .iter()
        .map(|l| CdxLicenseChoice {
            license: CdxLicense {
                id: l.spdx_id.clone(),
                name: if l.spdx_id.is_some() {
                    None
                } else {
                    Some(l.name.clone())
                },
            },
        })
        .collect();

    let mut external_refs = Vec::new();
    if !c.homepage.is_empty() {
        external_refs.push(CdxExternalRef {
            ref_type: "website",
            url: c.homepage.clone(),
        });
    }
    if !c.changelog.is_empty() {
        external_refs.push(CdxExternalRef {
            ref_type: "release-notes",
            url: c.changelog.clone(),
        });
    }

    let mut properties = vec![CdxProperty {
        name: "nix:store_path".into(),
        value: c.store_path.clone(),
    }];
    if c.nar_size > 0 {
        properties.push(CdxProperty {
            name: "nix:nar_size".into(),
            value: c.nar_size.to_string(),
        });
    }
    if !c.role.is_empty() {
        properties.push(CdxProperty {
            name: "nix:role".into(),
            value: c.role.clone(),
        });
    }
    if !c.source.is_empty() {
        properties.push(CdxProperty {
            name: "nix:source".into(),
            value: c.source.clone(),
        });
    }
    if !c.source_provenance.is_empty() {
        properties.push(CdxProperty {
            name: "nix:sourceProvenance".into(),
            value: c.source_provenance.join(","),
        });
    }
    if !c.main_program.is_empty() {
        properties.push(CdxProperty {
            name: "nix:mainProgram".into(),
            value: c.main_program.clone(),
        });
    }
    for cve in &c.known_vulnerabilities {
        properties.push(CdxProperty {
            name: "nix:knownVulnerability".into(),
            value: cve.clone(),
        });
    }

    CdxComponent {
        component_type: "library",
        bom_ref: c.bom_ref.clone(),
        name: c.pname.clone(),
        version: c.version.clone(),
        description: if c.description.is_empty() {
            None
        } else {
            Some(c.description.clone())
        },
        cpe: c.cpe.clone(),
        purl: c.purl.clone(),
        licenses,
        external_references: external_refs,
        properties,
    }
}

// ---------------------------------------------------------------------------
// CSV output
// ---------------------------------------------------------------------------

fn write_csv(mut writer: impl Write, components: &[SbomComponent]) -> color_eyre::Result<()> {
    writeln!(
        writer,
        "pname,version,license,role,source,store_path,nar_size"
    )?;
    for c in components {
        let license_str: String = c
            .licenses
            .iter()
            .map(|l| l.spdx_id.as_deref().unwrap_or(&l.name))
            .collect::<Vec<_>>()
            .join(";");
        writeln!(
            writer,
            "{},{},{},{},{},{},{}",
            csv_escape(&c.pname),
            csv_escape(&c.version),
            csv_escape(&license_str),
            csv_escape(&c.role),
            csv_escape(&c.source),
            csv_escape(&c.store_path),
            c.nar_size,
        )?;
    }
    Ok(())
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Produce an ISO 8601 timestamp without pulling in chrono.
fn chrono_like_timestamp() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();

    // Simple UTC timestamp calculation.
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Days since epoch to Y-M-D (simplified Gregorian).
    let (year, month, day) = days_to_date(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_date(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from Howard Hinnant's date library.
    days += 719_468;
    let era = days / 146_097;
    let doe = days % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_plain() {
        assert_eq!(csv_escape("hello"), "hello");
    }

    #[test]
    fn csv_escape_with_comma() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
    }

    #[test]
    fn csv_escape_with_quotes() {
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn timestamp_format() {
        let ts = chrono_like_timestamp();
        // Should match YYYY-MM-DDTHH:MM:SSZ pattern.
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn build_component_with_manifest() {
        let entry = PathInfoEntry {
            path: "/nix/store/abc123-nginx-1.26.2".into(),
            nar_size: 1024,
            closure_size: 0,
            references: Vec::new(),
        };

        let manifest_entry = ManifestEntry {
            pname: "nginx".into(),
            version: "1.26.2".into(),
            store_path: "/nix/store/abc123-nginx-1.26.2".into(),
            outputs: HashMap::new(),
            license: vec![manifest::LicenseInfo {
                spdx_id: Some("BSD-2-Clause".into()),
                full_name: "BSD 2-clause".into(),
            }],
            description: "HTTP server".into(),
            homepage: "https://nginx.org".into(),
            role: "service".into(),
            source: "services.nginx".into(),
            cpe: Some("cpe:2.3:a:f5:nginx:1.26.2:*:*:*:*:*:*:*".into()),
            purl: Some("pkg:nix/nixpkgs/nginx@1.26.2".into()),
            source_provenance: vec!["fromSource".into()],
            known_vulnerabilities: Vec::new(),
            changelog: "https://nginx.org/en/CHANGES".into(),
            main_program: "nginx".into(),
        };

        let mut index = HashMap::new();
        index.insert("/nix/store/abc123-nginx-1.26.2".to_owned(), &manifest_entry);

        let component = build_component(&entry, &index);
        assert_eq!(component.pname, "nginx");
        assert_eq!(component.version, "1.26.2");
        assert_eq!(component.role, "service");
        assert_eq!(component.licenses.len(), 1);
        assert_eq!(
            component.licenses[0].spdx_id.as_deref(),
            Some("BSD-2-Clause")
        );
    }

    #[test]
    fn build_component_heuristic_fallback() {
        let entry = PathInfoEntry {
            path: "/nix/store/xyz789-curl-8.5.0".into(),
            nar_size: 512,
            closure_size: 0,
            references: Vec::new(),
        };

        let index = HashMap::new();
        let component = build_component(&entry, &index);
        assert_eq!(component.pname, "curl");
        assert_eq!(component.version, "8.5.0");
        assert!(component.role.is_empty());
        assert!(component.licenses.is_empty());
    }

    #[test]
    fn cyclonedx_serialization() {
        let components = vec![SbomComponent {
            bom_ref: "abc123".into(),
            pname: "hello".into(),
            version: "2.10".into(),
            description: "A program that produces a familiar greeting".into(),
            homepage: "https://www.gnu.org/software/hello/".into(),
            licenses: vec![SbomLicense {
                spdx_id: Some("GPL-3.0-or-later".into()),
                name: "GNU GPLv3+".into(),
            }],
            store_path: "/nix/store/abc123-hello-2.10".into(),
            nar_size: 1024,
            role: "user".into(),
            source: "environment.systemPackages".into(),
            cpe: Some("cpe:2.3:a:gnu:hello:2.10:*:*:*:*:*:*:*".into()),
            purl: Some("pkg:nix/nixpkgs/hello@2.10".into()),
            source_provenance: vec!["fromSource".into()],
            known_vulnerabilities: Vec::new(),
            changelog: String::new(),
            main_program: "hello".into(),
        }];

        let closure_entries = vec![PathInfoEntry {
            path: "/nix/store/abc123-hello-2.10".into(),
            nar_size: 1024,
            closure_size: 0,
            references: Vec::new(),
        }];

        let mut buf = Vec::new();
        write_cyclonedx(
            &mut buf,
            "nixpkgs#hello",
            "/nix/store/abc123-hello-2.10",
            &components,
            &closure_entries,
        )
        .unwrap();

        let json: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(json["bomFormat"], "CycloneDX");
        assert_eq!(json["specVersion"], "1.5");
        assert!(
            json["serialNumber"]
                .as_str()
                .unwrap()
                .starts_with("urn:uuid:")
        );
        assert_eq!(json["metadata"]["tools"][0]["name"], "ekapkgs");
        // Root component is in metadata, not in components list (filtered out).
        assert_eq!(json["components"].as_array().unwrap().len(), 0);
    }
}
