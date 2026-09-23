//! Package name validation against the search index with fuzzy suggestions.

use std::io::Read;
use std::path::PathBuf;

/// Validate a package name against the search index. Returns `Ok(())` if the
/// package exists, or an error with "did you mean?" suggestions if not.
///
/// Gracefully degrades: if no index is available, validation is skipped.
pub fn validate_package_name(name: &str, flake: &str) -> color_eyre::Result<()> {
    let Some(entries) = load_package_index(flake)? else {
        return Ok(()); // No index, skip validation
    };

    // Exact match on pname.
    if entries.iter().any(|e| e.pname == name) {
        return Ok(());
    }

    // Exact match on attr path suffix.
    if entries.iter().any(|e| e.attr.ends_with(name)) {
        return Ok(());
    }

    // Fuzzy match.
    let suggestions = fuzzy_match_packages(&entries, name, 5);
    if suggestions.is_empty() {
        return Err(color_eyre::eyre::eyre!(
            "Package '{name}' not found in {flake}"
        ));
    }
    Err(color_eyre::eyre::eyre!(
        "Package '{name}' not found in {flake}. Did you mean:\n{}",
        suggestions
            .iter()
            .map(|s| {
                let desc = if s.description.is_empty() {
                    String::new()
                } else {
                    format!(" -- {}", s.description)
                };
                format!("  {}{desc}", s.pname)
            })
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

#[derive(serde::Deserialize)]
struct PackageSearchEntry {
    attr: String,
    pname: String,
    #[serde(default)]
    description: String,
}

fn index_dir() -> PathBuf {
    let dir = directories::ProjectDirs::from("", "", "ekapkgs")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache/ekapkgs")
        });
    dir.join("indexes")
}

fn load_package_index(flake: &str) -> color_eyre::Result<Option<Vec<PackageSearchEntry>>> {
    let index_name = format!("packages-{}", flake.replace(['/', '#'], "-"));
    let path = index_dir().join(format!("{index_name}.json.zst"));
    if !path.exists() {
        return Ok(None);
    }
    let compressed = std::fs::read(&path)?;
    let mut decoder = zstd::Decoder::new(compressed.as_slice())?;
    let mut data = Vec::new();
    decoder.read_to_end(&mut data)?;
    let entries: Vec<PackageSearchEntry> = serde_json::from_slice(&data)?;
    Ok(Some(entries))
}

fn fuzzy_match_packages<'a>(
    entries: &'a [PackageSearchEntry],
    query: &str,
    max_results: usize,
) -> Vec<&'a PackageSearchEntry> {
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config, Matcher, Utf32Str};

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
    scored.into_iter().map(|(_, e)| e).collect()
}
