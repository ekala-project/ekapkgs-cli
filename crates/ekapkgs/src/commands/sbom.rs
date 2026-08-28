use std::collections::HashMap;
use std::io::Write;

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::manifest::{self, ManifestEntry, PackageManifest};
use ekapkgs_nix::store::{self, PathInfoEntry};
use ekapkgs_nix::{NixCommand, eval};
use serde::{Deserialize, Serialize};

use crate::cli::{SbomDiffFormat, SbomFormat};

// ---------------------------------------------------------------------------
// Nix eval --apply metadata extraction
// ---------------------------------------------------------------------------

/// Metadata extracted from a nix package via `nix eval --apply`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvalMeta {
    pname: String,
    version: String,
    description: String,
    homepage: String,
    changelog: String,
    main_program: String,
    license: Vec<EvalLicense>,
    cpe: Option<String>,
    purl: Option<String>,
    #[serde(default)]
    source_provenance: Vec<String>,
    #[serde(default)]
    src_urls: Vec<String>,
    #[serde(default)]
    store_paths: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EvalLicense {
    spdx_id: Option<String>,
    full_name: String,
}

/// Nix expression fragment that extracts SBOM-relevant metadata from a package.
const EXTRACT_META_NIX: &str = r#"
  p: let
    meta = p.meta or {};
    identifiers = meta.identifiers or {};
    license = meta.license or null;
    licenses = if builtins.isList license then license
               else if license != null then [ license ]
               else [];
    rawProv = meta.sourceProvenance or [];
    provenance = builtins.map (s:
      if builtins.isAttrs s then s.shortName or "unknown"
      else builtins.toString s
    ) rawProv;
    src = p.src or null;
    srcUrls = if src != null
      then (src.urls or (if src ? url then [ src.url ] else []))
      else [];
  in {
    pname = p.pname or (builtins.parseDrvName (p.name or "unknown")).name;
    version = p.version or (builtins.parseDrvName (p.name or "unknown")).version;
    description = meta.description or "";
    homepage = meta.homepage or "";
    changelog = meta.changelog or "";
    mainProgram = meta.mainProgram or "";
    license = map (l: {
      spdxId = l.spdxId or null;
      fullName = l.fullName or "unknown";
    }) licenses;
    cpe = identifiers.cpe or null;
    purl = identifiers.purl or null;
    sourceProvenance = provenance;
    srcUrls = srcUrls;
    storePaths = builtins.listToAttrs (map (o: {
      name = o;
      value = builtins.unsafeDiscardStringContext (builtins.toString p.${o});
    }) (p.outputs or [ "out" ]));
  }
"#;

/// Nix expression that recursively walks build dependencies and collects
/// metadata for each package in the build closure.
const EXTRACT_CLOSURE_META_NIX: &str = r#"
  pkg: let
    extractMeta = EXTRACT_META_PLACEHOLDER;

    getDrvKey = p:
      builtins.unsafeDiscardStringContext (builtins.toString (p.drvPath or p.outPath or p));

    depAttrs = [
      "buildInputs" "nativeBuildInputs" "propagatedBuildInputs"
      "propagatedNativeBuildInputs" "depsBuildBuild"
    ];

    getDeps = p:
      builtins.concatLists (map (attr:
        let val = p.${attr} or []; in
        if builtins.isList val then builtins.filter builtins.isAttrs val
        else []
      ) depAttrs);

    collect = seen: queue:
      if queue == [] then seen
      else let
        p = builtins.head queue;
        rest = builtins.tail queue;
        key = getDrvKey p;
      in if seen ? ${key} then collect seen rest
         else let
           deps = getDeps p;
           newSeen = seen // { ${key} = extractMeta p; };
         in collect newSeen (rest ++ deps);

  in builtins.attrValues (collect {} [ pkg ])
"#;

/// Evaluate package metadata for a single installable via `nix eval --apply`.
fn eval_package_meta(installable: &str) -> Option<EvalMeta> {
    let result: Result<EvalMeta, _> = NixCommand::new(&["eval", "--json"])
        .arg(installable)
        .arg("--apply")
        .arg(EXTRACT_META_NIX)
        .json();

    match result {
        Ok(meta) => Some(meta),
        Err(e) => {
            tracing::debug!("Failed to eval package meta: {e}");
            None
        },
    }
}

/// Evaluate metadata for all packages in the build closure via `nix eval --apply`.
fn eval_closure_meta(installable: &str) -> Option<Vec<EvalMeta>> {
    let apply_expr = EXTRACT_CLOSURE_META_NIX.replace("EXTRACT_META_PLACEHOLDER", EXTRACT_META_NIX);

    let result: Result<Vec<EvalMeta>, _> = NixCommand::new(&["eval", "--json"])
        .arg(installable)
        .arg("--apply")
        .arg(&apply_expr)
        .json();

    match result {
        Ok(metas) => {
            tracing::info!("Eval found metadata for {} packages", metas.len());
            Some(metas)
        },
        Err(e) => {
            tracing::debug!("Failed to eval closure meta: {e}");
            None
        },
    }
}

/// Build a store-path-keyed index from eval metadata.
fn build_eval_meta_index(metas: &[EvalMeta]) -> HashMap<String, &EvalMeta> {
    let mut index = HashMap::new();
    for meta in metas {
        for store_path in meta.store_paths.values() {
            index.insert(store_path.clone(), meta);
        }
    }
    index
}

pub fn execute(
    installable: &str,
    format: &SbomFormat,
    buildtime: bool,
    output: Option<&str>,
) -> color_eyre::Result<()> {
    let (root_path, components, closure_entries) = build_sbom_components(installable, buildtime)?;

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

pub fn execute_diff(
    old: &str,
    new: &str,
    format: &SbomDiffFormat,
    output: Option<&str>,
) -> color_eyre::Result<()> {
    let (_, old_components, _) = build_sbom_components(old, false)?;
    let (_, new_components, _) = build_sbom_components(new, false)?;

    let diff = compute_diff(&old_components, &new_components);

    let writer: Box<dyn Write> = match output {
        Some(path) => Box::new(std::fs::File::create(path)?),
        None => Box::new(std::io::stdout().lock()),
    };

    match format {
        SbomDiffFormat::Text => write_diff_text(writer, &diff),
        SbomDiffFormat::Json => write_diff_json(writer, &diff),
        SbomDiffFormat::Csv => write_diff_csv(writer, &diff),
    }
}

/// Build and query a closure, returning the root path, component list,
/// and raw closure entries.
fn build_sbom_components(
    installable: &str,
    buildtime: bool,
) -> color_eyre::Result<(String, Vec<SbomComponent>, Vec<PathInfoEntry>)> {
    let inst = Installable::new(installable);

    let outputs: Vec<eval::BuildOutput> = NixCommand::new(&["build"])
        .arg(installable)
        .arg("--json")
        .json()?;

    let root_path = outputs
        .first()
        .and_then(|o| {
            o.outputs
                .get("out")
                .or_else(|| o.outputs.values().next())
                .cloned()
        })
        .ok_or_else(|| color_eyre::eyre::eyre!("no output path for installable"))?;

    let manifest = manifest::load_manifest(&root_path);
    if manifest.is_some() {
        tracing::info!("Found embedded package manifest");
    }

    // Eval package metadata via nix eval --apply.
    // Use recursive closure walk for both runtime and buildtime — the nix
    // expression traverses buildInputs/propagatedBuildInputs which covers
    // runtime deps too, enriching the entire closure with metadata.
    let eval_spinner = ekapkgs_ui::progress::spinner("Evaluating package metadata...");
    let eval_metas: Vec<EvalMeta> = eval_closure_meta(installable)
        .or_else(|| {
            // Fall back to single-package eval if closure walk fails.
            eval_package_meta(installable).map(|m| vec![m])
        })
        .unwrap_or_default();
    eval_spinner.finish_and_clear();
    let eval_index = build_eval_meta_index(&eval_metas);

    let spinner = ekapkgs_ui::progress::spinner("Querying closure...");
    let closure_entries = if buildtime {
        let paths = eval::derivation_closure_paths(&inst)?;
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

    let manifest_index = build_manifest_index(manifest.as_ref());
    let raw_components: Vec<SbomComponent> = closure_entries
        .iter()
        .map(|entry| build_component(entry, &manifest_index, &eval_index))
        .collect();
    let components = coalesce_components(raw_components);

    Ok((root_path, components, closure_entries))
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
    store_paths: Vec<String>,
    nar_size: u64,
    role: String,
    source: String,
    cpe: Option<String>,
    purl: Option<String>,
    source_provenance: Vec<String>,
    known_vulnerabilities: Vec<String>,
    changelog: String,
    main_program: String,
    src_urls: Vec<String>,
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
    eval_index: &HashMap<String, &EvalMeta>,
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
            store_paths: vec![entry.path.clone()],
            nar_size: entry.nar_size,
            role: manifest_entry.role.clone(),
            source: manifest_entry.source.clone(),
            cpe: manifest_entry.cpe.clone(),
            purl: manifest_entry.purl.clone(),
            source_provenance: manifest_entry.source_provenance.clone(),
            known_vulnerabilities: manifest_entry.known_vulnerabilities.clone(),
            changelog: manifest_entry.changelog.clone(),
            main_program: manifest_entry.main_program.clone(),
            src_urls: Vec::new(),
        }
    } else if let Some(eval_meta) = eval_index.get(&entry.path) {
        SbomComponent {
            bom_ref,
            pname: eval_meta.pname.clone(),
            version: eval_meta.version.clone(),
            description: eval_meta.description.clone(),
            homepage: eval_meta.homepage.clone(),
            licenses: eval_meta
                .license
                .iter()
                .map(|l| SbomLicense {
                    spdx_id: l.spdx_id.clone(),
                    name: l.full_name.clone(),
                })
                .collect(),
            store_paths: vec![entry.path.clone()],
            nar_size: entry.nar_size,
            role: String::new(),
            source: String::new(),
            cpe: eval_meta.cpe.clone(),
            purl: eval_meta.purl.clone(),
            source_provenance: eval_meta.source_provenance.clone(),
            known_vulnerabilities: Vec::new(),
            changelog: eval_meta.changelog.clone(),
            main_program: eval_meta.main_program.clone(),
            src_urls: eval_meta.src_urls.clone(),
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
            store_paths: vec![entry.path.clone()],
            nar_size: entry.nar_size,
            role: String::new(),
            source: String::new(),
            cpe: None,
            purl: None,
            source_provenance: Vec::new(),
            known_vulnerabilities: Vec::new(),
            changelog: String::new(),
            main_program: String::new(),
            src_urls: Vec::new(),
        }
    }
}

/// Merge components that share the same `(pname, version)` into a single
/// entry. Multi-output nix packages produce one store path per output;
/// this collapses them into one SBOM component with aggregated size and
/// all store paths listed.
fn coalesce_components(components: Vec<SbomComponent>) -> Vec<SbomComponent> {
    let mut result: Vec<SbomComponent> = Vec::new();
    let mut key_to_idx: HashMap<(String, String), usize> = HashMap::new();

    for c in components {
        let key = (c.pname.clone(), c.version.clone());
        if let Some(&idx) = key_to_idx.get(&key) {
            let existing = &mut result[idx];
            existing.nar_size += c.nar_size;
            existing.store_paths.extend(c.store_paths);

            // Keep the richest metadata.
            if existing.description.is_empty() && !c.description.is_empty() {
                existing.description = c.description;
            }
            if existing.homepage.is_empty() && !c.homepage.is_empty() {
                existing.homepage = c.homepage;
            }
            if existing.changelog.is_empty() && !c.changelog.is_empty() {
                existing.changelog = c.changelog;
            }
            if existing.main_program.is_empty() && !c.main_program.is_empty() {
                existing.main_program = c.main_program;
            }
            if existing.licenses.is_empty() && !c.licenses.is_empty() {
                existing.licenses = c.licenses;
            }
            if existing.cpe.is_none() && c.cpe.is_some() {
                existing.cpe = c.cpe;
            }
            if existing.purl.is_none() && c.purl.is_some() {
                existing.purl = c.purl;
            }
            if existing.role.is_empty() && !c.role.is_empty() {
                existing.role = c.role;
            }
            if existing.source.is_empty() && !c.source.is_empty() {
                existing.source = c.source;
            }
            if existing.source_provenance.is_empty() && !c.source_provenance.is_empty() {
                existing.source_provenance = c.source_provenance;
            }
            if existing.src_urls.is_empty() && !c.src_urls.is_empty() {
                existing.src_urls = c.src_urls;
            }
            existing
                .known_vulnerabilities
                .extend(c.known_vulnerabilities);
        } else {
            key_to_idx.insert(key, result.len());
            result.push(c);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// CycloneDX 1.7 JSON output
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
    tools: CdxTools,
    component: CdxComponent,
}

/// CycloneDX 1.6+ tools format using `components` array.
#[derive(Serialize)]
struct CdxTools {
    components: Vec<CdxToolComponent>,
}

#[derive(Serialize)]
struct CdxToolComponent {
    #[serde(rename = "type")]
    component_type: &'static str,
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
        .filter(|c| !c.store_paths.contains(&root_path.to_owned()))
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
            if depends_on.is_empty() {
                return None;
            }
            Some(CdxDependency {
                dep_ref,
                depends_on,
            })
        })
        .collect();

    let bom = CycloneDxBom {
        bom_format: "CycloneDX",
        spec_version: "1.7",
        version: 1,
        serial_number: serial,
        metadata: CdxMetadata {
            timestamp,
            tools: CdxTools {
                components: vec![CdxToolComponent {
                    component_type: "application",
                    name: "ekapkgs".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                }],
            },
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

    // Add source or binary distribution references.
    let is_binary = c
        .source_provenance
        .iter()
        .any(|s| s.contains("binary") || s.contains("Binary"));
    let dist_type = if is_binary {
        "distribution"
    } else {
        "source-distribution"
    };
    for url in &c.src_urls {
        external_refs.push(CdxExternalRef {
            ref_type: dist_type,
            url: url.clone(),
        });
    }

    let mut properties: Vec<CdxProperty> = c
        .store_paths
        .iter()
        .map(|p| CdxProperty {
            name: "nix:store_path".into(),
            value: p.clone(),
        })
        .collect();
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

    let component_type = if c.main_program.is_empty() {
        "library"
    } else {
        "application"
    };

    CdxComponent {
        component_type,
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
            csv_escape(&c.store_paths.join(" ")),
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

// ---------------------------------------------------------------------------
// SBOM diff
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiffEntry {
    status: &'static str,
    pname: String,
    old_version: String,
    new_version: String,
    old_nar_size: u64,
    new_nar_size: u64,
    role: String,
    /// Package URL for downstream tooling identification.
    #[serde(skip_serializing_if = "Option::is_none")]
    purl: Option<String>,
    /// Metadata changes detected for this package.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    metadata_changes: Vec<MetadataChange>,
}

/// A single metadata field that changed between old and new.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct MetadataChange {
    field: &'static str,
    old_value: String,
    new_value: String,
}

struct SbomDiff {
    added: Vec<DiffEntry>,
    removed: Vec<DiffEntry>,
    changed: Vec<DiffEntry>,
    summary: DiffSummary,
}

struct DiffSummary {
    old_count: usize,
    new_count: usize,
    added: usize,
    removed: usize,
    changed: usize,
    size_delta: i64,
}

/// Compare metadata between two components and return a list of changes.
fn diff_metadata(old: &SbomComponent, new: &SbomComponent) -> Vec<MetadataChange> {
    let mut changes = Vec::new();

    // License changes.
    let old_lic = format_licenses(&old.licenses);
    let new_lic = format_licenses(&new.licenses);
    if old_lic != new_lic {
        changes.push(MetadataChange {
            field: "license",
            old_value: old_lic,
            new_value: new_lic,
        });
    }

    // CVE changes.
    let mut old_cves = old.known_vulnerabilities.clone();
    let mut new_cves = new.known_vulnerabilities.clone();
    old_cves.sort();
    new_cves.sort();
    if old_cves != new_cves {
        let added_cves: Vec<&String> = new_cves.iter().filter(|c| !old_cves.contains(c)).collect();
        let removed_cves: Vec<&String> =
            old_cves.iter().filter(|c| !new_cves.contains(c)).collect();
        if !added_cves.is_empty() {
            changes.push(MetadataChange {
                field: "cve:added",
                old_value: String::new(),
                new_value: added_cves
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            });
        }
        if !removed_cves.is_empty() {
            changes.push(MetadataChange {
                field: "cve:removed",
                old_value: removed_cves
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                new_value: String::new(),
            });
        }
    }

    // Source provenance changes.
    let mut old_prov = old.source_provenance.clone();
    let mut new_prov = new.source_provenance.clone();
    old_prov.sort();
    new_prov.sort();
    if old_prov != new_prov {
        changes.push(MetadataChange {
            field: "sourceProvenance",
            old_value: old_prov.join(","),
            new_value: new_prov.join(","),
        });
    }

    // CPE changes.
    if old.cpe != new.cpe {
        changes.push(MetadataChange {
            field: "cpe",
            old_value: old.cpe.clone().unwrap_or_default(),
            new_value: new.cpe.clone().unwrap_or_default(),
        });
    }

    // PURL changes.
    if old.purl != new.purl {
        changes.push(MetadataChange {
            field: "purl",
            old_value: old.purl.clone().unwrap_or_default(),
            new_value: new.purl.clone().unwrap_or_default(),
        });
    }

    // Role changes.
    if old.role != new.role && !old.role.is_empty() && !new.role.is_empty() {
        changes.push(MetadataChange {
            field: "role",
            old_value: old.role.clone(),
            new_value: new.role.clone(),
        });
    }

    changes
}

fn format_licenses(licenses: &[SbomLicense]) -> String {
    let mut ids: Vec<&str> = licenses
        .iter()
        .map(|l| l.spdx_id.as_deref().unwrap_or(&l.name))
        .collect();
    ids.sort();
    ids.join(",")
}

fn compute_diff(old: &[SbomComponent], new: &[SbomComponent]) -> SbomDiff {
    // Index by pname. When multiple store paths share a pname (e.g.,
    // multi-output packages), keep the one with the largest nar_size
    // as representative.
    let old_by_name = index_by_pname(old);
    let new_by_name = index_by_pname(new);

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut size_delta: i64 = 0;

    // Check for removed and changed packages.
    for (pname, old_comp) in &old_by_name {
        if let Some(new_comp) = new_by_name.get(pname) {
            let metadata_changes = diff_metadata(old_comp, new_comp);
            let path_changed = old_comp.store_paths != new_comp.store_paths;

            if path_changed || !metadata_changes.is_empty() {
                let delta = new_comp.nar_size as i64 - old_comp.nar_size as i64;
                size_delta += delta;
                changed.push(DiffEntry {
                    status: if path_changed { "changed" } else { "metadata" },
                    pname: pname.clone(),
                    old_version: old_comp.version.clone(),
                    new_version: new_comp.version.clone(),
                    old_nar_size: old_comp.nar_size,
                    new_nar_size: new_comp.nar_size,
                    role: if new_comp.role.is_empty() {
                        old_comp.role.clone()
                    } else {
                        new_comp.role.clone()
                    },
                    purl: new_comp.purl.clone().or_else(|| old_comp.purl.clone()),
                    metadata_changes,
                });
            }
        } else {
            size_delta -= old_comp.nar_size as i64;
            removed.push(DiffEntry {
                status: "removed",
                pname: pname.clone(),
                old_version: old_comp.version.clone(),
                new_version: String::new(),
                old_nar_size: old_comp.nar_size,
                new_nar_size: 0,
                role: old_comp.role.clone(),
                purl: old_comp.purl.clone(),
                metadata_changes: Vec::new(),
            });
        }
    }

    // Check for added packages.
    for (pname, new_comp) in &new_by_name {
        if !old_by_name.contains_key(pname) {
            size_delta += new_comp.nar_size as i64;

            // Surface CVEs on newly added packages.
            let metadata_changes = if new_comp.known_vulnerabilities.is_empty() {
                Vec::new()
            } else {
                vec![MetadataChange {
                    field: "cve:added",
                    old_value: String::new(),
                    new_value: new_comp.known_vulnerabilities.join(","),
                }]
            };

            added.push(DiffEntry {
                status: "added",
                pname: pname.clone(),
                old_version: String::new(),
                new_version: new_comp.version.clone(),
                old_nar_size: 0,
                new_nar_size: new_comp.nar_size,
                role: new_comp.role.clone(),
                purl: new_comp.purl.clone(),
                metadata_changes,
            });
        }
    }

    added.sort_by(|a, b| a.pname.cmp(&b.pname));
    removed.sort_by(|a, b| a.pname.cmp(&b.pname));
    changed.sort_by(|a, b| a.pname.cmp(&b.pname));

    let summary = DiffSummary {
        old_count: old_by_name.len(),
        new_count: new_by_name.len(),
        added: added.len(),
        removed: removed.len(),
        changed: changed.len(),
        size_delta,
    };

    SbomDiff {
        added,
        removed,
        changed,
        summary,
    }
}

fn index_by_pname(components: &[SbomComponent]) -> HashMap<String, &SbomComponent> {
    let mut map: HashMap<String, &SbomComponent> = HashMap::new();
    for comp in components {
        map.entry(comp.pname.clone())
            .and_modify(|existing| {
                if comp.nar_size > existing.nar_size {
                    *existing = comp;
                }
            })
            .or_insert(comp);
    }
    map
}

fn write_diff_text(mut writer: impl Write, diff: &SbomDiff) -> color_eyre::Result<()> {
    let s = &diff.summary;
    writeln!(
        writer,
        "{} packages -> {} packages",
        s.old_count, s.new_count
    )?;

    let size_str = if s.size_delta >= 0 {
        format!("+{}", ekapkgs_ui::format::format_bytes(s.size_delta as u64))
    } else {
        format!(
            "-{}",
            ekapkgs_ui::format::format_bytes((-s.size_delta) as u64)
        )
    };
    writeln!(
        writer,
        "  {} added, {} removed, {} changed ({})",
        s.added, s.removed, s.changed, size_str
    )?;

    if !diff.changed.is_empty() {
        writeln!(writer)?;
        for entry in &diff.changed {
            let delta = entry.new_nar_size as i64 - entry.old_nar_size as i64;
            let delta_str = if delta >= 0 {
                format!("+{}", ekapkgs_ui::format::format_bytes(delta as u64))
            } else {
                format!("-{}", ekapkgs_ui::format::format_bytes((-delta) as u64))
            };
            if entry.old_version == entry.new_version {
                // Metadata-only change, same version.
                write!(writer, "  {}: {}", entry.pname, entry.new_version)?;
            } else {
                write!(
                    writer,
                    "  {}: {} -> {}",
                    entry.pname, entry.old_version, entry.new_version
                )?;
            }
            if delta != 0 {
                write!(writer, ", {delta_str}")?;
            }
            writeln!(writer)?;
            write_metadata_changes(&mut writer, &entry.metadata_changes)?;
        }
    }

    if !diff.added.is_empty() {
        writeln!(writer)?;
        for entry in &diff.added {
            let size = ekapkgs_ui::format::format_bytes(entry.new_nar_size);
            write!(writer, "  + {} {}", entry.pname, entry.new_version)?;
            if !entry.role.is_empty() {
                write!(writer, " ({})", entry.role)?;
            }
            writeln!(writer, ", {size}")?;
            write_metadata_changes(&mut writer, &entry.metadata_changes)?;
        }
    }

    if !diff.removed.is_empty() {
        writeln!(writer)?;
        for entry in &diff.removed {
            let size = ekapkgs_ui::format::format_bytes(entry.old_nar_size);
            write!(writer, "  - {} {}", entry.pname, entry.old_version)?;
            if !entry.role.is_empty() {
                write!(writer, " ({})", entry.role)?;
            }
            writeln!(writer, ", {size}")?;
        }
    }

    Ok(())
}

fn write_metadata_changes(
    writer: &mut impl Write,
    changes: &[MetadataChange],
) -> color_eyre::Result<()> {
    for mc in changes {
        match mc.field {
            "cve:added" => {
                for cve in mc.new_value.split(',') {
                    writeln!(writer, "      CVE+ {cve}")?;
                }
            },
            "cve:removed" => {
                for cve in mc.old_value.split(',') {
                    writeln!(writer, "      CVE- {cve}")?;
                }
            },
            "license" => {
                writeln!(
                    writer,
                    "      license: {} -> {}",
                    mc.old_value, mc.new_value
                )?;
            },
            "sourceProvenance" => {
                writeln!(
                    writer,
                    "      provenance: {} -> {}",
                    mc.old_value, mc.new_value
                )?;
            },
            "role" => {
                writeln!(writer, "      role: {} -> {}", mc.old_value, mc.new_value)?;
            },
            _ => {
                writeln!(
                    writer,
                    "      {}: {} -> {}",
                    mc.field, mc.old_value, mc.new_value
                )?;
            },
        }
    }
    Ok(())
}

fn write_diff_json(writer: impl Write, diff: &SbomDiff) -> color_eyre::Result<()> {
    #[derive(Serialize)]
    struct DiffOutput<'a> {
        summary: SummaryJson,
        changes: &'a [DiffEntry],
        added: &'a [DiffEntry],
        removed: &'a [DiffEntry],
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SummaryJson {
        old_count: usize,
        new_count: usize,
        size_delta: i64,
    }

    let output = DiffOutput {
        summary: SummaryJson {
            old_count: diff.summary.old_count,
            new_count: diff.summary.new_count,
            size_delta: diff.summary.size_delta,
        },
        changes: &diff.changed,
        added: &diff.added,
        removed: &diff.removed,
    };

    serde_json::to_writer_pretty(writer, &output)?;
    Ok(())
}

fn write_diff_csv(mut writer: impl Write, diff: &SbomDiff) -> color_eyre::Result<()> {
    writeln!(
        writer,
        "status,pname,old_version,new_version,old_nar_size,new_nar_size,role"
    )?;
    let all: Vec<&DiffEntry> = diff
        .changed
        .iter()
        .chain(diff.added.iter())
        .chain(diff.removed.iter())
        .collect();
    for e in all {
        writeln!(
            writer,
            "{},{},{},{},{},{},{}",
            e.status,
            csv_escape(&e.pname),
            csv_escape(&e.old_version),
            csv_escape(&e.new_version),
            e.old_nar_size,
            e.new_nar_size,
            csv_escape(&e.role),
        )?;
    }
    Ok(())
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

        let eval_index = HashMap::new();
        let component = build_component(&entry, &index, &eval_index);
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
        let eval_index = HashMap::new();
        let component = build_component(&entry, &index, &eval_index);
        assert_eq!(component.pname, "curl");
        assert_eq!(component.version, "8.5.0");
        assert!(component.role.is_empty());
        assert!(component.licenses.is_empty());
    }

    fn make_component(pname: &str, version: &str, nar_size: u64, role: &str) -> SbomComponent {
        SbomComponent {
            bom_ref: format!("hash-{pname}-{version}"),
            pname: pname.into(),
            version: version.into(),
            description: String::new(),
            homepage: String::new(),
            licenses: Vec::new(),
            store_paths: vec![format!("/nix/store/hash-{pname}-{version}")],
            nar_size,
            role: role.into(),
            source: String::new(),
            cpe: None,
            purl: None,
            source_provenance: Vec::new(),
            known_vulnerabilities: Vec::new(),
            changelog: String::new(),
            main_program: String::new(),
            src_urls: Vec::new(),
        }
    }

    #[test]
    fn diff_detects_added_packages() {
        let old = vec![make_component("hello", "2.10", 1000, "")];
        let new = vec![
            make_component("hello", "2.10", 1000, ""),
            make_component("curl", "8.5.0", 500, "user"),
        ];
        let diff = compute_diff(&old, &new);
        assert_eq!(diff.summary.added, 1);
        assert_eq!(diff.summary.removed, 0);
        assert_eq!(diff.summary.changed, 0);
        assert_eq!(diff.added[0].pname, "curl");
    }

    #[test]
    fn diff_detects_removed_packages() {
        let old = vec![
            make_component("hello", "2.10", 1000, ""),
            make_component("curl", "8.5.0", 500, ""),
        ];
        let new = vec![make_component("hello", "2.10", 1000, "")];
        let diff = compute_diff(&old, &new);
        assert_eq!(diff.summary.added, 0);
        assert_eq!(diff.summary.removed, 1);
        assert_eq!(diff.removed[0].pname, "curl");
    }

    #[test]
    fn diff_detects_version_change() {
        let old = vec![make_component("nginx", "1.26.2", 1000, "service")];
        let new = vec![make_component("nginx", "1.26.3", 1200, "service")];
        let diff = compute_diff(&old, &new);
        assert_eq!(diff.summary.changed, 1);
        assert_eq!(diff.summary.added, 0);
        assert_eq!(diff.summary.removed, 0);
        assert_eq!(diff.changed[0].old_version, "1.26.2");
        assert_eq!(diff.changed[0].new_version, "1.26.3");
        assert_eq!(diff.summary.size_delta, 200);
    }

    #[test]
    fn diff_unchanged_not_reported() {
        let old = vec![make_component("hello", "2.10", 1000, "")];
        let new = vec![make_component("hello", "2.10", 1000, "")];
        let diff = compute_diff(&old, &new);
        assert_eq!(diff.summary.changed, 0);
        assert_eq!(diff.summary.added, 0);
        assert_eq!(diff.summary.removed, 0);
    }

    #[test]
    fn diff_text_output() {
        let old = vec![
            make_component("nginx", "1.26.2", 1000, "service"),
            make_component("redis", "7.0", 800, "service"),
        ];
        let new = vec![
            make_component("nginx", "1.26.3", 1200, "service"),
            make_component("postgres", "16.2", 5000, "service"),
        ];
        let diff = compute_diff(&old, &new);
        let mut buf = Vec::new();
        write_diff_text(&mut buf, &diff).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("nginx: 1.26.2 -> 1.26.3"));
        assert!(output.contains("+ postgres"));
        assert!(output.contains("- redis"));
    }

    #[test]
    fn diff_json_output() {
        let old = vec![make_component("hello", "2.10", 1000, "")];
        let new = vec![make_component("hello", "2.12", 1100, "")];
        let diff = compute_diff(&old, &new);
        let mut buf = Vec::new();
        write_diff_json(&mut buf, &diff).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(json["summary"]["oldCount"], 1);
        assert_eq!(json["summary"]["sizeDelta"], 100);
        assert_eq!(json["changes"].as_array().unwrap().len(), 1);
        assert_eq!(json["changes"][0]["pname"], "hello");
        assert_eq!(json["changes"][0]["oldVersion"], "2.10");
        assert_eq!(json["changes"][0]["newVersion"], "2.12");
    }

    fn make_component_with_meta(
        pname: &str,
        version: &str,
        nar_size: u64,
        cves: &[&str],
        license_id: &str,
        provenance: &str,
    ) -> SbomComponent {
        let mut c = make_component(pname, version, nar_size, "");
        c.known_vulnerabilities = cves.iter().map(|s| (*s).to_owned()).collect();
        if !license_id.is_empty() {
            c.licenses = vec![SbomLicense {
                spdx_id: Some(license_id.into()),
                name: license_id.into(),
            }];
        }
        if !provenance.is_empty() {
            c.source_provenance = vec![provenance.into()];
        }
        c
    }

    #[test]
    fn diff_detects_new_cve() {
        let old = vec![make_component_with_meta(
            "openssl",
            "3.3.1",
            5000,
            &[],
            "Apache-2.0",
            "fromSource",
        )];
        let new = vec![make_component_with_meta(
            "openssl",
            "3.3.1",
            5000,
            &["CVE-2024-1234"],
            "Apache-2.0",
            "fromSource",
        )];
        // Same store path means no "changed" — but metadata differs.
        // Force different store paths to trigger diff.
        let mut new = new;
        new[0].store_paths = vec!["/nix/store/different-openssl-3.3.1".into()];

        let diff = compute_diff(&old, &new);
        assert_eq!(diff.summary.changed, 1);
        let entry = &diff.changed[0];
        assert!(
            entry
                .metadata_changes
                .iter()
                .any(|mc| mc.field == "cve:added" && mc.new_value.contains("CVE-2024-1234"))
        );
    }

    #[test]
    fn diff_detects_cve_fixed() {
        let mut old = make_component_with_meta(
            "openssl",
            "3.3.1",
            5000,
            &["CVE-2024-1234"],
            "Apache-2.0",
            "fromSource",
        );
        old.store_paths = vec!["/nix/store/old-openssl-3.3.1".into()];

        let mut new =
            make_component_with_meta("openssl", "3.3.2", 5100, &[], "Apache-2.0", "fromSource");
        new.store_paths = vec!["/nix/store/new-openssl-3.3.2".into()];

        let diff = compute_diff(&[old], &[new]);
        assert_eq!(diff.summary.changed, 1);
        assert!(
            diff.changed[0]
                .metadata_changes
                .iter()
                .any(|mc| mc.field == "cve:removed" && mc.old_value.contains("CVE-2024-1234"))
        );
    }

    #[test]
    fn diff_detects_license_change() {
        let mut old = make_component_with_meta("foo", "1.0", 100, &[], "MIT", "fromSource");
        old.store_paths = vec!["/nix/store/old-foo-1.0".into()];

        let mut new =
            make_component_with_meta("foo", "2.0", 100, &[], "GPL-3.0-or-later", "fromSource");
        new.store_paths = vec!["/nix/store/new-foo-2.0".into()];

        let diff = compute_diff(&[old], &[new]);
        assert_eq!(diff.summary.changed, 1);
        let lic = diff.changed[0]
            .metadata_changes
            .iter()
            .find(|mc| mc.field == "license")
            .unwrap();
        assert_eq!(lic.old_value, "MIT");
        assert_eq!(lic.new_value, "GPL-3.0-or-later");
    }

    #[test]
    fn diff_detects_provenance_change() {
        let mut old = make_component_with_meta("blob", "1.0", 100, &[], "", "binaryNativeCode");
        old.store_paths = vec!["/nix/store/old-blob-1.0".into()];

        let mut new = make_component_with_meta("blob", "1.1", 100, &[], "", "fromSource");
        new.store_paths = vec!["/nix/store/new-blob-1.1".into()];

        let diff = compute_diff(&[old], &[new]);
        let prov = diff.changed[0]
            .metadata_changes
            .iter()
            .find(|mc| mc.field == "sourceProvenance")
            .unwrap();
        assert_eq!(prov.old_value, "binaryNativeCode");
        assert_eq!(prov.new_value, "fromSource");
    }

    #[test]
    fn diff_metadata_only_no_store_path_change() {
        // Same store path but different metadata (e.g., manifest updated).
        let mut old = make_component_with_meta("lib", "1.0", 100, &[], "MIT", "");
        let mut new = make_component_with_meta("lib", "1.0", 100, &["CVE-2025-9999"], "MIT", "");
        // Same store path — only metadata differs.
        old.store_paths = vec!["/nix/store/same-lib-1.0".into()];
        new.store_paths = vec!["/nix/store/same-lib-1.0".into()];

        let diff = compute_diff(&[old], &[new]);
        // Metadata-only change should still be reported.
        assert_eq!(diff.summary.changed, 1);
        assert_eq!(diff.changed[0].status, "metadata");
        assert!(
            diff.changed[0]
                .metadata_changes
                .iter()
                .any(|mc| mc.field == "cve:added")
        );
    }

    #[test]
    fn diff_text_shows_cve_changes() {
        let mut old = make_component("openssl", "3.3.1", 5000, "");
        old.store_paths = vec!["/nix/store/old-openssl-3.3.1".into()];
        old.known_vulnerabilities = vec!["CVE-2024-0001".into()];

        let mut new = make_component("openssl", "3.3.2", 5100, "");
        new.store_paths = vec!["/nix/store/new-openssl-3.3.2".into()];
        new.known_vulnerabilities = vec!["CVE-2024-0002".into()];

        let diff = compute_diff(&[old], &[new]);
        let mut buf = Vec::new();
        write_diff_text(&mut buf, &diff).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("CVE+ CVE-2024-0002"));
        assert!(output.contains("CVE- CVE-2024-0001"));
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
            store_paths: vec!["/nix/store/abc123-hello-2.10".into()],
            nar_size: 1024,
            role: "user".into(),
            source: "environment.systemPackages".into(),
            cpe: Some("cpe:2.3:a:gnu:hello:2.10:*:*:*:*:*:*:*".into()),
            purl: Some("pkg:nix/nixpkgs/hello@2.10".into()),
            source_provenance: vec!["fromSource".into()],
            known_vulnerabilities: Vec::new(),
            changelog: String::new(),
            main_program: "hello".into(),
            src_urls: Vec::new(),
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
        assert_eq!(json["specVersion"], "1.7");
        assert!(
            json["serialNumber"]
                .as_str()
                .unwrap()
                .starts_with("urn:uuid:")
        );
        assert_eq!(
            json["metadata"]["tools"]["components"][0]["name"],
            "ekapkgs"
        );
        // Root component is in metadata, not in components list (filtered out).
        assert_eq!(json["components"].as_array().unwrap().len(), 0);
    }
}
