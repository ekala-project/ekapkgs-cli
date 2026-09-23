use std::io::Read;
use std::path::PathBuf;

use ekapkgs_nix::NixCommand;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use yansi::Paint;

use crate::cli::SearchCommand;

pub fn execute(command: SearchCommand) -> color_eyre::Result<()> {
    match command {
        SearchCommand::Packages {
            query,
            flake,
            json,
            names_only,
            limit,
        } => cmd_packages(&query, &flake, json, names_only, limit),
        SearchCommand::Options {
            query,
            flake,
            json,
            limit,
        } => cmd_options(&query, &flake, json, limit),
        SearchCommand::Files {
            query,
            json,
            names_only,
            limit,
        } => cmd_files(&query, json, names_only, limit),
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

    // Try downloading from configured index_url before generating locally.
    if let Ok(config) = crate::config::ClientConfig::load() {
        if let Some(url) = &config.defaults.index_url {
            if let Ok(data) = try_download_index(url, name) {
                return Ok(data);
            }
        }
    }

    let spinner = ekapkgs_ui::progress::spinner(&format!("Generating {name} index..."));
    let data = generate()?;
    spinner.finish_and_clear();
    write_index(name, &data)?;
    Ok(data)
}

/// Try to download a single index from a remote URL. Returns the
/// decompressed data on success.
fn try_download_index(base_url: &str, name: &str) -> color_eyre::Result<Vec<u8>> {
    let url = format!("{base_url}/{name}.json.zst");
    let spinner = ekapkgs_ui::progress::spinner(&format!("Downloading {name} index..."));
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async {
        let resp = reqwest::Client::new().get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(color_eyre::eyre::eyre!("HTTP {} for {url}", resp.status()));
        }
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    });
    spinner.finish_and_clear();

    let compressed = result?;
    let dir = cache_dir()?;
    let path = dir.join(format!("{name}.json.zst"));
    std::fs::write(&path, &compressed)?;

    // Decompress for the caller.
    let mut decoder = zstd::Decoder::new(compressed.as_slice())?;
    let mut data = Vec::new();
    decoder.read_to_end(&mut data)?;
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
            outputs: Vec::new(),
            main_program: None,
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
    /// Package output names (e.g., `["out", "dev", "lib"]`).
    /// Populated by enriched indexes from CI; empty when generated locally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    outputs: Vec<String>,
    /// Binary name from `meta.mainProgram`, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main_program: Option<String>,
}

fn cmd_packages(
    query: &str,
    flake: &str,
    json_output: bool,
    names_only: bool,
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

            // Score: 0 = exact, 1 = prefix, 2 = contains, 3 = desc contains
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

    // If substring matching found few results, add fuzzy matches as tier 4.
    if !query.is_empty() && results.len() < limit.max(1) {
        let fuzzy = fuzzy_match_entries(&entries, query, limit);
        for (_, e) in &fuzzy {
            if !results.iter().any(|(_, r)| std::ptr::eq(*r, *e)) {
                results.push((4, e));
            }
        }
    }

    results.sort_by_key(|(score, e)| (*score, e.pname.clone()));
    if limit > 0 {
        results.truncate(limit);
    }

    if json_output {
        let out: Vec<&PackageSearchEntry> = results.iter().map(|(_, e)| *e).collect();
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if names_only {
        for (_, entry) in &results {
            println!("{}", entry.attr);
        }
        return Ok(());
    }

    if results.is_empty() {
        println!("No packages matching '{query}'.");
        return Ok(());
    }

    for (_, entry) in &results {
        let attr_display = highlight_match(&entry.attr, query);
        let mut version_info = format!("({})", entry.version);
        if !entry.outputs.is_empty() && entry.outputs != ["out"] {
            version_info.push_str(&format!(" [{}]", entry.outputs.join(", ")));
        }
        println!(
            "{} {}",
            format!("* {attr_display}").bold(),
            version_info.dim()
        );
        if !entry.description.is_empty() {
            let desc_display = highlight_match(&entry.description, query);
            println!("  {desc_display}");
        }
    }
    println!();
    println!("{} result(s)", results.len());

    Ok(())
}

/// Fuzzy match package entries by pname using nucleo-matcher.
fn fuzzy_match_entries<'a>(
    entries: &'a [PackageSearchEntry],
    query: &str,
    max_results: usize,
) -> Vec<(u32, &'a PackageSearchEntry)> {
    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(u32, &PackageSearchEntry)> = entries
        .iter()
        .filter_map(|entry| {
            let mut buf = Vec::new();
            let haystack = Utf32Str::new(&entry.pname, &mut buf);
            let score = pattern.score(haystack, &mut matcher)?;
            Some((score, entry))
        })
        .collect();

    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    scored.truncate(max_results);
    scored
}

/// Highlight the first occurrence of `query` (case-insensitive) in `text`.
fn highlight_match(text: &str, query: &str) -> String {
    if query.is_empty() {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    let query_lower = query.to_lowercase();
    if let Some(pos) = lower.find(&query_lower) {
        let before = &text[..pos];
        let matched = &text[pos..pos + query.len()];
        let after = &text[pos + query.len()..];
        format!("{before}{}{after}", matched.underline())
    } else {
        text.to_owned()
    }
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

/// Escape a string for use inside nix double quotes.
fn escape_nix_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace("${", "\\${")
}

fn generate_option_index(flake: &str) -> color_eyre::Result<Vec<u8>> {
    // Use nix eval with an inline expression that serializes options.
    // This works with any flake that has an ekaos-style options tree.
    let escaped_flake = escape_nix_string(flake);
    let expr = format!(
        r#"
        let
          flake = builtins.getFlake "{escaped_flake}";
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
    /// Package output containing this file (e.g., `out`, `bin`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

fn print_file_results(
    results: &[FileSearchEntry],
    query: &str,
    json_output: bool,
    names_only: bool,
) -> color_eyre::Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(results)?);
    } else if names_only {
        for entry in results {
            println!("{}", entry.package);
        }
    } else if results.is_empty() {
        println!("No files matching '{query}'.");
    } else {
        for entry in results {
            let pkg_info = if let Some(out) = &entry.output {
                format!("{}.{out}", entry.package)
            } else {
                entry.package.clone()
            };
            println!("{}  {}", entry.file, format!("({pkg_info})").dim());
        }
        println!();
        println!("{} result(s)", results.len());
    }
    Ok(())
}

fn cmd_files(
    query: &str,
    json_output: bool,
    names_only: bool,
    limit: usize,
) -> color_eyre::Result<()> {
    // Prefer local cached file index (instant, no external deps).
    if let Some(data) = read_index("files")? {
        let entries: Vec<FileSearchEntry> = serde_json::from_slice(&data)?;
        let query_lower = query.to_lowercase();
        let results: Vec<FileSearchEntry> = entries
            .into_iter()
            .filter(|e| e.file.to_lowercase().contains(&query_lower))
            .take(limit)
            .collect();
        return print_file_results(&results, query, json_output, names_only);
    }

    // Try downloading from remote if configured.
    if let Ok(config) = crate::config::ClientConfig::load() {
        if let Some(url) = &config.defaults.index_url {
            if let Ok(data) = try_download_index(url, "files") {
                let entries: Vec<FileSearchEntry> = serde_json::from_slice(&data)?;
                let query_lower = query.to_lowercase();
                let results: Vec<FileSearchEntry> = entries
                    .into_iter()
                    .filter(|e| e.file.to_lowercase().contains(&query_lower))
                    .take(limit)
                    .collect();
                return print_file_results(&results, query, json_output, names_only);
            }
        }
    }

    // Fallback: try nix-locate.
    if let Ok(results) = search_via_nix_locate(query, limit) {
        return print_file_results(&results, query, json_output, names_only);
    }

    Err(color_eyre::eyre::eyre!(
        "No file index available. Run `ekapkgs search update` or install nix-index (`nix-locate`)."
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
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(limit)
        .map(|line| {
            // nix-locate --minimal outputs "attr.output" (e.g. "cowsay.out").
            let (package, output) = match line.rsplit_once('.') {
                Some((attr, out)) => (attr.to_owned(), Some(out.to_owned())),
                None => (line.to_owned(), None),
            };
            FileSearchEntry {
                file: query.to_owned(),
                package,
                output,
            }
        })
        .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Update command
// ---------------------------------------------------------------------------

fn cmd_update(flake: &str, remote: Option<&str>) -> color_eyre::Result<()> {
    // Explicit --remote flag takes priority over config.
    let remote_url = remote.map(ToOwned::to_owned).or_else(|| {
        crate::config::ClientConfig::load()
            .ok()
            .and_then(|c| c.defaults.index_url)
    });
    if let Some(url) = &remote_url {
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
