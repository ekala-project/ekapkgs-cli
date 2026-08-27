use std::io::Read;
use std::path::PathBuf;

use ekapkgs_nix::NixCommand;
use yansi::Paint;

use crate::cli::SearchCommand;

pub fn execute(command: SearchCommand) -> color_eyre::Result<()> {
    match command {
        SearchCommand::Packages {
            query,
            flake,
            json,
            limit,
        } => cmd_packages(&query, &flake, json, limit),
        SearchCommand::Options {
            query,
            flake,
            json,
            limit,
        } => cmd_options(&query, &flake, json, limit),
        SearchCommand::Files { query, json, limit } => cmd_files(&query, json, limit),
        SearchCommand::Update { flake, remote } => cmd_update(&flake, remote.as_deref()),
    }
}

// ---------------------------------------------------------------------------
// Index cache infrastructure
// ---------------------------------------------------------------------------

fn cache_dir() -> color_eyre::Result<PathBuf> {
    let dir = directories::ProjectDirs::from("", "", "ekapkgs")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache/ekapkgs")
        });
    let index_dir = dir.join("indexes");
    std::fs::create_dir_all(&index_dir)?;
    Ok(index_dir)
}

fn index_path(name: &str) -> color_eyre::Result<PathBuf> {
    Ok(cache_dir()?.join(format!("{name}.json.zst")))
}

fn write_index(name: &str, data: &[u8]) -> color_eyre::Result<()> {
    let path = index_path(name)?;
    let compressed = zstd::encode_all(data, 3)?;
    std::fs::write(&path, compressed)?;
    tracing::info!("Wrote index {} ({} bytes)", path.display(), data.len());
    Ok(())
}

fn read_index(name: &str) -> color_eyre::Result<Option<Vec<u8>>> {
    let path = index_path(name)?;
    if !path.exists() {
        return Ok(None);
    }
    let compressed = std::fs::read(&path)?;
    let mut decoder = zstd::Decoder::new(compressed.as_slice())?;
    let mut data = Vec::new();
    decoder.read_to_end(&mut data)?;
    Ok(Some(data))
}

fn load_or_generate_index<F>(name: &str, generate: F) -> color_eyre::Result<Vec<u8>>
where
    F: FnOnce() -> color_eyre::Result<Vec<u8>>,
{
    if let Some(data) = read_index(name)? {
        return Ok(data);
    }
    let spinner = ekapkgs_ui::progress::spinner(&format!("Generating {name} index..."));
    let data = generate()?;
    spinner.finish_and_clear();
    write_index(name, &data)?;
    Ok(data)
}

// ---------------------------------------------------------------------------
// Package search
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, serde::Serialize)]
struct PackageEntry {
    #[serde(default)]
    pname: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
}

fn generate_package_index(flake: &str) -> color_eyre::Result<Vec<u8>> {
    // `nix search <flake> --json ^` returns { "attr": { pname, version, description }, ... }
    let output = NixCommand::new(&["search"])
        .arg(flake)
        .arg("--json")
        .arg("^")
        .output()?;
    // Convert the map format to a flat array with attr paths.
    let map: std::collections::HashMap<String, PackageEntry> =
        serde_json::from_slice(&output.stdout)?;
    let entries: Vec<PackageSearchEntry> = map
        .into_iter()
        .map(|(attr, entry)| PackageSearchEntry {
            attr,
            pname: entry.pname,
            version: entry.version,
            description: entry.description,
        })
        .collect();
    Ok(serde_json::to_vec(&entries)?)
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PackageSearchEntry {
    attr: String,
    pname: String,
    version: String,
    description: String,
}

fn cmd_packages(
    query: &str,
    flake: &str,
    json_output: bool,
    limit: usize,
) -> color_eyre::Result<()> {
    let index_name = format!("packages-{}", flake.replace(['/', '#'], "-"));
    let data = load_or_generate_index(&index_name, || generate_package_index(flake))?;
    let entries: Vec<PackageSearchEntry> = serde_json::from_slice(&data)?;

    let query_lower = query.to_lowercase();
    let mut results: Vec<(u8, &PackageSearchEntry)> = entries
        .iter()
        .filter_map(|e| {
            let attr_lower = e.attr.to_lowercase();
            let pname_lower = e.pname.to_lowercase();
            let desc_lower = e.description.to_lowercase();

            // Score: 0 = exact pname, 1 = pname prefix, 2 = attr contains, 3 = desc contains
            if pname_lower == query_lower {
                Some((0, e))
            } else if pname_lower.starts_with(&query_lower) {
                Some((1, e))
            } else if attr_lower.contains(&query_lower) || pname_lower.contains(&query_lower) {
                Some((2, e))
            } else if desc_lower.contains(&query_lower) {
                Some((3, e))
            } else {
                None
            }
        })
        .collect();

    results.sort_by_key(|(score, e)| (*score, e.pname.clone()));
    results.truncate(limit);

    if json_output {
        let out: Vec<&PackageSearchEntry> = results.iter().map(|(_, e)| *e).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("No packages matching '{query}'.");
        return Ok(());
    }

    for (_, entry) in &results {
        println!(
            "{} {}",
            format!("* {}", entry.attr).bold(),
            format!("({})", entry.version).dim()
        );
        if !entry.description.is_empty() {
            println!("  {}", entry.description);
        }
    }
    println!();
    println!("{} result(s)", results.len());

    Ok(())
}

// ---------------------------------------------------------------------------
// Option search
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, serde::Serialize)]
struct OptionSearchEntry {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default, rename = "type")]
    option_type: String,
    #[serde(default)]
    default: serde_json::Value,
    #[serde(default)]
    example: serde_json::Value,
    #[serde(default)]
    declarations: Vec<String>,
    #[serde(default)]
    read_only: bool,
}

fn generate_option_index(flake: &str) -> color_eyre::Result<Vec<u8>> {
    // Use nix eval with an inline expression that serializes options.
    // This works with any flake that has an ekaos-style options tree.
    let expr = format!(
        r#"
        let
          flake = builtins.getFlake "{flake}";
          pkgs = flake.legacyPackages.${{builtins.currentSystem}} or flake.pkgs.${{builtins.currentSystem}} or (import <nixpkgs> {{}});
          lib = pkgs.lib;
          eval = flake.config or
                 (if builtins.hasAttr "options" flake then flake else
                  if builtins.hasAttr "ekaosConfigurations" flake then
                    (builtins.head (builtins.attrValues flake.ekaosConfigurations))
                  else
                    {{ options = {{}}; }});
          optionsList = lib.optionAttrSetToDocList (eval.options or {{}});
          filtered = builtins.filter (o: !(o.internal or false) && !(o.visible or true == false)) optionsList;
          mapped = map (o: {{
            name = o.name;
            description = o.description or "";
            type = o.type or "unspecified";
            default = builtins.tryEval (builtins.toJSON (o.default or null));
            example = builtins.tryEval (builtins.toJSON (o.example or null));
            declarations = o.declarations or [];
            readOnly = o.readOnly or false;
          }}) filtered;
        in builtins.toJSON mapped
        "#
    );

    let output = NixCommand::new(&["eval"])
        .arg("--impure")
        .arg("--expr")
        .arg(&expr)
        .output();

    match output {
        Ok(out) => {
            // nix eval --expr wraps the result in quotes since it's a string.
            // Parse the outer string, then the inner JSON.
            let raw = String::from_utf8_lossy(&out.stdout);
            let unquoted: String = serde_json::from_str(raw.trim())?;
            Ok(unquoted.into_bytes())
        },
        Err(_) => {
            // Fallback: return empty index if evaluation fails.
            tracing::warn!("Could not evaluate options for {flake}, using empty index");
            Ok(b"[]".to_vec())
        },
    }
}

fn cmd_options(
    query: &str,
    flake: &str,
    json_output: bool,
    limit: usize,
) -> color_eyre::Result<()> {
    let index_name = format!("options-{}", flake.replace(['/', '#'], "-"));
    let data = load_or_generate_index(&index_name, || generate_option_index(flake))?;
    let entries: Vec<OptionSearchEntry> = serde_json::from_slice(&data)?;

    let query_lower = query.to_lowercase();
    let mut results: Vec<(u8, &OptionSearchEntry)> = entries
        .iter()
        .filter_map(|e| {
            let name_lower = e.name.to_lowercase();
            let desc_lower = e.description.to_lowercase();

            if name_lower == query_lower {
                Some((0, e))
            } else if name_lower.starts_with(&query_lower) {
                Some((1, e))
            } else if name_lower.contains(&query_lower) {
                Some((2, e))
            } else if desc_lower.contains(&query_lower) {
                Some((3, e))
            } else {
                None
            }
        })
        .collect();

    results.sort_by_key(|(score, e)| (*score, e.name.clone()));
    results.truncate(limit);

    if json_output {
        let out: Vec<&OptionSearchEntry> = results.iter().map(|(_, e)| *e).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("No options matching '{query}'.");
        return Ok(());
    }

    for (_, entry) in &results {
        println!("{}", entry.name.bold());
        if !entry.option_type.is_empty() {
            println!("  {}: {}", "Type".dim(), entry.option_type);
        }
        if !entry.description.is_empty() {
            // Truncate long descriptions.
            let desc = if entry.description.len() > 200 {
                format!("{}...", &entry.description[..200])
            } else {
                entry.description.clone()
            };
            println!("  {}", desc);
        }
        if !entry.default.is_null() {
            let default_str = entry.default.as_str().unwrap_or("(complex)");
            if default_str.len() <= 80 {
                println!("  {}: {}", "Default".dim(), default_str);
            }
        }
        println!();
    }
    println!("{} result(s)", results.len());

    Ok(())
}

// ---------------------------------------------------------------------------
// File search
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, serde::Serialize)]
struct FileSearchEntry {
    file: String,
    package: String,
}

fn cmd_files(query: &str, json_output: bool, limit: usize) -> color_eyre::Result<()> {
    // Try nix-locate first (from nix-index).
    if let Ok(results) = search_via_nix_locate(query, limit) {
        if json_output {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else if results.is_empty() {
            println!("No files matching '{query}'.");
        } else {
            for entry in &results {
                println!("{}  {}", entry.file, format!("({})", entry.package).dim());
            }
            println!();
            println!("{} result(s)", results.len());
        }
        return Ok(());
    }

    // Fallback: try loading a cached file index.
    if let Some(data) = read_index("files")? {
        let entries: Vec<FileSearchEntry> = serde_json::from_slice(&data)?;
        let query_lower = query.to_lowercase();
        let results: Vec<&FileSearchEntry> = entries
            .iter()
            .filter(|e| e.file.to_lowercase().contains(&query_lower))
            .take(limit)
            .collect();

        if json_output {
            println!("{}", serde_json::to_string_pretty(&results)?);
        } else if results.is_empty() {
            println!("No files matching '{query}'.");
        } else {
            for entry in &results {
                println!("{}  {}", entry.file, format!("({})", entry.package).dim());
            }
            println!();
            println!("{} result(s)", results.len());
        }
        return Ok(());
    }

    Err(color_eyre::eyre::eyre!(
        "No file index available. Install nix-index (`nix-locate`) or run `ekapkgs search update`."
    ))
}

fn search_via_nix_locate(query: &str, limit: usize) -> color_eyre::Result<Vec<FileSearchEntry>> {
    let output = std::process::Command::new("nix-locate")
        .arg("--top-level")
        .arg("--minimal")
        .arg("--whole-name")
        .arg(query)
        .output();

    // If that doesn't match, try pattern match.
    let output = match output {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => o,
        _ => {
            let o = std::process::Command::new("nix-locate")
                .arg("--top-level")
                .arg("--minimal")
                .arg(query)
                .output()
                .map_err(|e| color_eyre::eyre::eyre!("nix-locate not found: {e}"))?;
            if !o.status.success() {
                return Err(color_eyre::eyre::eyre!("nix-locate failed"));
            }
            o
        },
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let results: Vec<FileSearchEntry> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .take(limit)
        .map(|line| {
            // nix-locate --minimal outputs lines like "package.attr"
            let package = line.trim().to_owned();
            FileSearchEntry {
                file: query.to_owned(),
                package,
            }
        })
        .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Update command
// ---------------------------------------------------------------------------

fn cmd_update(flake: &str, remote: Option<&str>) -> color_eyre::Result<()> {
    if let Some(url) = remote {
        return download_indexes(url);
    }

    // Generate package index.
    let pkg_name = format!("packages-{}", flake.replace(['/', '#'], "-"));
    let spinner = ekapkgs_ui::progress::spinner("Generating package index...");
    match generate_package_index(flake) {
        Ok(data) => {
            spinner.finish_and_clear();
            write_index(&pkg_name, &data)?;
            println!(
                "Package index: {} entries",
                serde_json::from_slice::<Vec<PackageSearchEntry>>(&data)
                    .map(|v| v.len())
                    .unwrap_or(0)
            );
        },
        Err(e) => {
            spinner.finish_and_clear();
            tracing::warn!("Failed to generate package index: {e}");
        },
    }

    // Generate option index.
    let opt_name = format!("options-{}", flake.replace(['/', '#'], "-"));
    let spinner = ekapkgs_ui::progress::spinner("Generating option index...");
    match generate_option_index(flake) {
        Ok(data) => {
            spinner.finish_and_clear();
            write_index(&opt_name, &data)?;
            println!(
                "Option index: {} entries",
                serde_json::from_slice::<Vec<OptionSearchEntry>>(&data)
                    .map(|v| v.len())
                    .unwrap_or(0)
            );
        },
        Err(e) => {
            spinner.finish_and_clear();
            tracing::warn!("Failed to generate option index: {e}");
        },
    }

    println!("Indexes updated in {}", cache_dir()?.display());
    Ok(())
}

fn download_indexes(base_url: &str) -> color_eyre::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = reqwest::Client::new();
        let dir = cache_dir()?;

        for name in &["packages", "options", "files"] {
            let url = format!("{base_url}/{name}.json.zst");
            let spinner = ekapkgs_ui::progress::spinner(&format!("Downloading {name} index..."));

            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let bytes = resp.bytes().await?;
                    let path = dir.join(format!("{name}.json.zst"));
                    std::fs::write(&path, &bytes)?;
                    spinner.finish_and_clear();
                    println!("Downloaded {name} index ({} bytes)", bytes.len());
                },
                Ok(resp) => {
                    spinner.finish_and_clear();
                    tracing::warn!("Failed to download {name} index: HTTP {}", resp.status());
                },
                Err(e) => {
                    spinner.finish_and_clear();
                    tracing::warn!("Failed to download {name} index: {e}");
                },
            }
        }

        Ok(())
    })
}
