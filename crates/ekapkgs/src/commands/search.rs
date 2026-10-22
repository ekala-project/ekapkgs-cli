use std::io::Read;
use std::path::PathBuf;

use ekapkgs_nix::NixCommand;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rusqlite::Connection;
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

pub(crate) fn cache_dir() -> color_eyre::Result<PathBuf> {
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

/// Path to the search database.
fn db_path() -> color_eyre::Result<PathBuf> {
    Ok(cache_dir()?.join("search.db"))
}

/// Open the search database, returning `None` if it doesn't exist.
fn open_db() -> color_eyre::Result<Option<Connection>> {
    let path = db_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&path)?;
    Ok(Some(conn))
}

/// Open the search database, creating it from nix if it doesn't exist.
fn open_or_generate_db(flake: &str) -> color_eyre::Result<Connection> {
    if let Some(conn) = open_db()? {
        return Ok(conn);
    }

    // Try downloading from configured index_url.
    if let Ok(config) = crate::config::ClientConfig::load() {
        if let Some(url) = &config.defaults.index_url {
            if try_download_db(url).is_ok() {
                if let Some(conn) = open_db()? {
                    return Ok(conn);
                }
            }
        }
    }

    // Generate locally.
    let path = db_path()?;
    generate_local_db(&path, flake)?;
    let conn = Connection::open(&path)?;
    Ok(conn)
}

/// Download `search.db.zst` from a remote URL, decompress, and save.
fn try_download_db(base_url: &str) -> color_eyre::Result<()> {
    let url = format!("{base_url}/search.db.zst");
    let spinner = ekapkgs_ui::progress::spinner("Downloading search index...");
    let rt = tokio::runtime::Runtime::new()?;
    let compressed = rt.block_on(async {
        let resp = reqwest::Client::new().get(&url).send().await?;
        if !resp.status().is_success() {
            return Err(color_eyre::eyre::eyre!("HTTP {} for {url}", resp.status()));
        }
        let bytes = resp.bytes().await?;
        Ok(bytes.to_vec())
    })?;
    spinner.finish_and_clear();

    let mut decoder = zstd::Decoder::new(compressed.as_slice())?;
    let mut data = Vec::new();
    decoder.read_to_end(&mut data)?;

    let path = db_path()?;
    std::fs::write(&path, &data)?;
    tracing::info!(
        "Downloaded search index to {} ({} bytes)",
        path.display(),
        data.len()
    );
    Ok(())
}

/// Generate a local search.db by running `nix search` and `nix eval`.
fn generate_local_db(path: &std::path::Path, flake: &str) -> color_eyre::Result<()> {
    let conn = Connection::open(path)?;

    conn.execute_batch(
        "PRAGMA journal_mode = OFF;
         PRAGMA synchronous = OFF;
         PRAGMA locking_mode = EXCLUSIVE;
         PRAGMA page_size = 4096;",
    )?;

    create_schema(&conn)?;

    // Generate package entries.
    let spinner = ekapkgs_ui::progress::spinner("Generating package index...");
    let packages = generate_package_entries(flake);
    spinner.finish_and_clear();
    match packages {
        Ok(entries) => {
            println!("Package index: {} entries", entries.len());
            insert_packages(&conn, &entries)?;
        },
        Err(e) => tracing::warn!("Failed to generate package index: {e}"),
    }

    // Generate option entries.
    let spinner = ekapkgs_ui::progress::spinner("Generating option index...");
    let options = generate_option_entries(flake);
    spinner.finish_and_clear();
    match options {
        Ok(entries) => {
            println!("Option index: {} entries", entries.len());
            insert_options(&conn, &entries)?;
        },
        Err(e) => tracing::warn!("Failed to generate option index: {e}"),
    }

    // Optimize FTS indexes.
    conn.execute_batch(
        "INSERT INTO packages_fts(packages_fts) VALUES('optimize');
         INSERT INTO files_fts(files_fts) VALUES('optimize');",
    )?;
    conn.execute_batch("VACUUM;")?;

    println!("Index saved to {}", path.display());
    Ok(())
}

/// Create the SQLite schema (matches eka-ci's search_index crate).
fn create_schema(conn: &Connection) -> color_eyre::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS packages (
            attr         TEXT PRIMARY KEY,
            pname        TEXT NOT NULL,
            version      TEXT NOT NULL,
            description  TEXT,
            outputs      TEXT,
            main_program TEXT
        );

        CREATE TABLE IF NOT EXISTS files (
            file    TEXT NOT NULL,
            package TEXT NOT NULL,
            output  TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_files_file    ON files(file);
        CREATE INDEX IF NOT EXISTS idx_files_package ON files(package);

        CREATE TABLE IF NOT EXISTS options (
            name         TEXT PRIMARY KEY,
            description  TEXT,
            type         TEXT,
            default_val  TEXT,
            example      TEXT,
            declarations TEXT,
            read_only    INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS metadata (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS packages_fts USING fts5(
            attr, pname, description,
            content='packages',
            content_rowid='rowid'
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(
            file, package,
            content='files',
            content_rowid='rowid'
        );",
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// JSON index helpers (kept for sonames, which remain json.zst)
// ---------------------------------------------------------------------------

pub(crate) fn index_path(name: &str) -> color_eyre::Result<PathBuf> {
    Ok(cache_dir()?.join(format!("{name}.json.zst")))
}

pub(crate) fn read_index(name: &str) -> color_eyre::Result<Option<Vec<u8>>> {
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

pub(crate) fn try_download_index(base_url: &str, name: &str) -> color_eyre::Result<Vec<u8>> {
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

    let mut decoder = zstd::Decoder::new(compressed.as_slice())?;
    let mut data = Vec::new();
    decoder.read_to_end(&mut data)?;
    Ok(data)
}

// ---------------------------------------------------------------------------
// Package search
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Deserialize, serde::Serialize)]
struct PackageSearchEntry {
    attr: String,
    pname: String,
    version: String,
    description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main_program: Option<String>,
}

/// Load a PackageSearchEntry from a SQLite row.
fn row_to_package(row: &rusqlite::Row<'_>) -> rusqlite::Result<PackageSearchEntry> {
    let outputs_json: Option<String> = row.get(4)?;
    let outputs: Vec<String> = outputs_json
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    Ok(PackageSearchEntry {
        attr: row.get(0)?,
        pname: row.get(1)?,
        version: row.get(2)?,
        description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        outputs,
        main_program: row.get(5)?,
    })
}

const PKG_SELECT: &str =
    "SELECT attr, pname, version, description, outputs, main_program FROM packages";

fn query_all_packages(conn: &Connection) -> color_eyre::Result<Vec<PackageSearchEntry>> {
    let mut stmt = conn.prepare(&format!("{PKG_SELECT} ORDER BY pname"))?;
    let entries = stmt
        .query_map([], row_to_package)?
        .filter_map(Result::ok)
        .collect();
    Ok(entries)
}

fn query_packages_matching(
    conn: &Connection,
    query: &str,
) -> color_eyre::Result<Vec<(u8, PackageSearchEntry)>> {
    let query_lower = query.to_lowercase();
    let like_pattern = format!("%{query_lower}%");

    let mut stmt = conn.prepare(&format!(
        "{PKG_SELECT} WHERE LOWER(pname) LIKE ?1 OR LOWER(attr) LIKE ?1 OR LOWER(description) \
         LIKE ?1"
    ))?;

    let entries: Vec<PackageSearchEntry> = stmt
        .query_map([&like_pattern], row_to_package)?
        .filter_map(Result::ok)
        .collect();

    let mut results: Vec<(u8, PackageSearchEntry)> = entries
        .into_iter()
        .map(|e| {
            let pname_lower = e.pname.to_lowercase();
            let attr_lower = e.attr.to_lowercase();
            let desc_lower = e.description.to_lowercase();
            let score = if pname_lower == query_lower {
                0
            } else if pname_lower.starts_with(&query_lower) {
                1
            } else if attr_lower.contains(&query_lower) || pname_lower.contains(&query_lower) {
                2
            } else if desc_lower.contains(&query_lower) {
                3
            } else {
                4
            };
            (score, e)
        })
        .collect();

    results.sort_by_key(|(score, e)| (*score, e.pname.clone()));
    Ok(results)
}

fn cmd_packages(
    query: &str,
    flake: &str,
    json_output: bool,
    names_only: bool,
    limit: usize,
) -> color_eyre::Result<()> {
    let conn = open_or_generate_db(flake)?;

    let effective_limit = if query.is_empty() { 0 } else { limit };

    let mut results: Vec<(u8, PackageSearchEntry)> = if query.is_empty() {
        query_all_packages(&conn)?
            .into_iter()
            .map(|e| (0u8, e))
            .collect()
    } else {
        query_packages_matching(&conn, query)?
    };

    // If substring matching found few results, add fuzzy matches as tier 4.
    if !query.is_empty() && results.len() < effective_limit.max(1) {
        // Load pname + attr for fuzzy matching (lightweight query).
        let mut stmt = conn.prepare(&format!("{PKG_SELECT} ORDER BY pname"))?;
        let all_entries: Vec<PackageSearchEntry> = stmt
            .query_map([], row_to_package)?
            .filter_map(Result::ok)
            .collect();

        let fuzzy = fuzzy_match_entries(&all_entries, query, effective_limit);
        for (_, e) in fuzzy {
            if !results.iter().any(|(_, r)| r.attr == e.attr) {
                results.push((4, e.clone()));
            }
        }
    }

    results.sort_by_key(|(score, e)| (*score, e.pname.clone()));
    if effective_limit > 0 {
        results.truncate(effective_limit);
    }

    if json_output {
        let out: Vec<&PackageSearchEntry> = results.iter().map(|(_, e)| e).collect();
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
// Local generation helpers
// ---------------------------------------------------------------------------

/// Raw shape of each entry returned by `nix search --json`.
#[derive(serde::Deserialize)]
struct NixSearchEntry {
    #[serde(default)]
    pname: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
}

fn generate_package_entries(flake: &str) -> color_eyre::Result<Vec<PackageSearchEntry>> {
    let output = NixCommand::new(&["search"])
        .arg(flake)
        .arg("--json")
        .arg("^")
        .output()?;
    let map: std::collections::HashMap<String, NixSearchEntry> =
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
    Ok(entries)
}

fn insert_packages(conn: &Connection, entries: &[PackageSearchEntry]) -> color_eyre::Result<()> {
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO packages (attr, pname, version, description, outputs, \
         main_program)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut fts_stmt = conn.prepare(
        "INSERT INTO packages_fts (rowid, attr, pname, description)
         VALUES (?1, ?2, ?3, ?4)",
    )?;

    for pkg in entries {
        let outputs = if pkg.outputs.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&pkg.outputs).unwrap_or_default())
        };
        let desc = if pkg.description.is_empty() {
            None
        } else {
            Some(&pkg.description)
        };

        let rowid = stmt.insert(rusqlite::params![
            pkg.attr,
            pkg.pname,
            pkg.version,
            desc,
            outputs,
            pkg.main_program,
        ])?;

        fts_stmt.execute(rusqlite::params![
            rowid,
            pkg.attr,
            pkg.pname,
            pkg.description,
        ])?;
    }
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

/// Escape a string for use inside nix double quotes.
fn escape_nix_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace("${", "\\${")
}

fn generate_option_entries(flake: &str) -> color_eyre::Result<Vec<OptionSearchEntry>> {
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
            let raw = String::from_utf8_lossy(&out.stdout);
            let unquoted: String = serde_json::from_str(raw.trim())?;
            let entries: Vec<OptionSearchEntry> = serde_json::from_str(&unquoted)?;
            Ok(entries)
        },
        Err(_) => {
            tracing::warn!("Could not evaluate options for {flake}, using empty index");
            Ok(Vec::new())
        },
    }
}

fn insert_options(conn: &Connection, entries: &[OptionSearchEntry]) -> color_eyre::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO options (name, description, type, default_val, example, \
         declarations, read_only)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;

    for opt in entries {
        let declarations = if opt.declarations.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&opt.declarations).unwrap_or_default())
        };
        let default_val = if opt.default.is_null() {
            None
        } else {
            Some(opt.default.to_string())
        };
        let example_val = if opt.example.is_null() {
            None
        } else {
            Some(opt.example.to_string())
        };

        stmt.execute(rusqlite::params![
            opt.name,
            if opt.description.is_empty() {
                None
            } else {
                Some(&opt.description)
            },
            if opt.option_type.is_empty() {
                None
            } else {
                Some(&opt.option_type)
            },
            default_val,
            example_val,
            declarations,
            opt.read_only as i32,
        ])?;
    }
    Ok(())
}

fn cmd_options(
    query: &str,
    flake: &str,
    json_output: bool,
    limit: usize,
) -> color_eyre::Result<()> {
    let conn = open_or_generate_db(flake)?;

    let like_pattern = format!("%{}%", query.to_lowercase());
    let mut stmt = conn.prepare(
        "SELECT name, description, type, default_val, example, declarations, read_only
         FROM options
         WHERE LOWER(name) LIKE ?1 OR LOWER(description) LIKE ?1",
    )?;

    let entries: Vec<OptionSearchEntry> = stmt
        .query_map([&like_pattern], |row| {
            let declarations_json: Option<String> = row.get(5)?;
            let declarations: Vec<String> = declarations_json
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            let default_str: Option<String> = row.get(3)?;
            let default_val = default_str
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::Value::Null);
            let example_str: Option<String> = row.get(4)?;
            let example_val = example_str
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::Value::Null);
            Ok(OptionSearchEntry {
                name: row.get(0)?,
                description: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                option_type: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                default: default_val,
                example: example_val,
                declarations,
                read_only: row.get::<_, i32>(6)? != 0,
            })
        })?
        .filter_map(Result::ok)
        .collect();

    let query_lower = query.to_lowercase();
    let mut results: Vec<(u8, OptionSearchEntry)> = entries
        .into_iter()
        .map(|e| {
            let name_lower = e.name.to_lowercase();
            let desc_lower = e.description.to_lowercase();
            let score = if name_lower == query_lower {
                0
            } else if name_lower.starts_with(&query_lower) {
                1
            } else if name_lower.contains(&query_lower) {
                2
            } else if desc_lower.contains(&query_lower) {
                3
            } else {
                4
            };
            (score, e)
        })
        .collect();

    results.sort_by_key(|(score, e)| (*score, e.name.clone()));
    results.truncate(limit);

    if json_output {
        let out: Vec<&OptionSearchEntry> = results.iter().map(|(_, e)| e).collect();
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
    // Try SQLite database first.
    if let Some(conn) = open_db()? {
        let has_files: bool = conn
            .query_row("SELECT EXISTS(SELECT 1 FROM files LIMIT 1)", [], |row| {
                row.get(0)
            })
            .unwrap_or(false);

        if has_files {
            let like_pattern = format!("%{}%", query.to_lowercase());
            let mut stmt = conn.prepare(
                "SELECT file, package, output FROM files WHERE LOWER(file) LIKE ?1 LIMIT ?2",
            )?;
            let results: Vec<FileSearchEntry> = stmt
                .query_map(rusqlite::params![like_pattern, limit as i64], |row| {
                    Ok(FileSearchEntry {
                        file: row.get(0)?,
                        package: row.get(1)?,
                        output: row.get::<_, Option<String>>(2)?,
                    })
                })?
                .filter_map(Result::ok)
                .collect();
            return print_file_results(&results, query, json_output, names_only);
        }
    }

    // Try downloading database from remote if configured.
    if let Ok(config) = crate::config::ClientConfig::load() {
        if let Some(url) = &config.defaults.index_url {
            if try_download_db(url).is_ok() {
                if let Some(conn) = open_db()? {
                    let like_pattern = format!("%{}%", query.to_lowercase());
                    let mut stmt = conn.prepare(
                        "SELECT file, package, output FROM files WHERE LOWER(file) LIKE ?1 LIMIT \
                         ?2",
                    )?;
                    let results: Vec<FileSearchEntry> = stmt
                        .query_map(rusqlite::params![like_pattern, limit as i64], |row| {
                            Ok(FileSearchEntry {
                                file: row.get(0)?,
                                package: row.get(1)?,
                                output: row.get::<_, Option<String>>(2)?,
                            })
                        })?
                        .filter_map(Result::ok)
                        .collect();
                    return print_file_results(&results, query, json_output, names_only);
                }
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
    let remote_url = remote.map(ToOwned::to_owned).or_else(|| {
        crate::config::ClientConfig::load()
            .ok()
            .and_then(|c| c.defaults.index_url)
    });
    if let Some(url) = &remote_url {
        return download_db(url);
    }

    // Generate locally.
    let path = db_path()?;
    // Remove existing DB so we start fresh.
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    generate_local_db(&path, flake)?;
    Ok(())
}

fn download_db(base_url: &str) -> color_eyre::Result<()> {
    try_download_db(base_url)?;
    let path = db_path()?;
    let meta = std::fs::metadata(&path)?;
    println!(
        "Downloaded search index ({:.1} MiB) to {}",
        meta.len() as f64 / 1_048_576.0,
        path.display()
    );
    Ok(())
}
