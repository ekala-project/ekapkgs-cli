//! Package name validation against the search index with fuzzy suggestions.

use rusqlite::Connection;

/// Validate a package name against the search index. Returns `Ok(())` if the
/// package exists, or an error with "did you mean?" suggestions if not.
///
/// Gracefully degrades: if no index is available, validation is skipped.
pub fn validate_package_name(name: &str, flake: &str) -> color_eyre::Result<()> {
    let Some(conn) = open_search_db()? else {
        return Ok(()); // No index, skip validation
    };

    // Exact match on pname.
    let found: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM packages WHERE pname = ?1)",
        [name],
        |row| row.get(0),
    )?;
    if found {
        return Ok(());
    }

    // Exact match on attr path suffix.
    let suffix_pattern = format!("%{name}");
    let found: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM packages WHERE attr LIKE ?1)",
        [&suffix_pattern],
        |row| row.get(0),
    )?;
    if found {
        return Ok(());
    }

    // Load candidates for fuzzy matching.
    let entries = load_fuzzy_candidates(&conn)?;
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

fn open_search_db() -> color_eyre::Result<Option<Connection>> {
    let dir = crate::commands::search::cache_dir()?;
    let path = dir.join("search.db");
    if !path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&path)?;
    Ok(Some(conn))
}

struct PackageCandidate {
    pname: String,
    description: String,
}

fn load_fuzzy_candidates(conn: &Connection) -> color_eyre::Result<Vec<PackageCandidate>> {
    let mut stmt = conn.prepare("SELECT pname, description FROM packages")?;
    let entries = stmt
        .query_map([], |row| {
            Ok(PackageCandidate {
                pname: row.get(0)?,
                description: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(entries)
}

fn fuzzy_match_packages<'a>(
    entries: &'a [PackageCandidate],
    query: &str,
    max_results: usize,
) -> Vec<&'a PackageCandidate> {
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config, Matcher, Utf32Str};

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(u32, &PackageCandidate)> = entries
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
