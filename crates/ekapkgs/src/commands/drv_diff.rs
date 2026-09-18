use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{self, Write};

use ekapkgs_nix::eval::{self, DerivationInfo};
use ekapkgs_nix::store;
use yansi::Paint;

/// Execute a recursive derivation diff between two `.drv` paths.
pub fn execute(a: &str, b: &str, max_depth: Option<usize>) -> color_eyre::Result<()> {
    let mut seen = HashSet::new();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    diff_drvs(&mut out, a, b, 0, max_depth, &mut seen)?;
    Ok(())
}

/// Recursively diff two derivation paths, printing differences.
fn diff_drvs(
    out: &mut impl Write,
    drv_a: &str,
    drv_b: &str,
    depth: usize,
    max_depth: Option<usize>,
    seen: &mut HashSet<(String, String)>,
) -> color_eyre::Result<()> {
    if drv_a == drv_b {
        return Ok(());
    }

    let pair = (drv_a.to_owned(), drv_b.to_owned());
    if seen.contains(&pair) {
        writeln!(out, "{}{}", indent(depth), "  (already compared)".dim())?;
        return Ok(());
    }
    seen.insert(pair);

    if let Some(limit) = max_depth {
        if depth > limit {
            writeln!(out, "{}{}", indent(depth), "  (max depth reached)".dim())?;
            return Ok(());
        }
    }

    let info_a = eval::show_derivation(drv_a)?;
    let info_b = eval::show_derivation(drv_b)?;

    let name_a = drv_display_name(drv_a);
    let name_b = drv_display_name(drv_b);

    writeln!(
        out,
        "{}{}",
        indent(depth),
        format!("• {name_a} ≠ {name_b}").bold()
    )?;

    // Collect the derivation's own output paths for env-var filtering.
    let own_output_paths: BTreeSet<&str> = info_a
        .outputs
        .values()
        .chain(info_b.outputs.values())
        .filter_map(|o| o.path.as_deref())
        .collect();

    diff_field(
        out,
        depth,
        "builder",
        info_a.builder.as_deref(),
        info_b.builder.as_deref(),
    )?;
    diff_field(
        out,
        depth,
        "system",
        info_a.system.as_deref(),
        info_b.system.as_deref(),
    )?;
    diff_list(out, depth, "arguments", &info_a.args, &info_b.args)?;
    diff_outputs(out, depth, &info_a, &info_b)?;
    diff_env(out, depth, &info_a.env, &info_b.env, &own_output_paths)?;
    diff_input_srcs(out, depth, &info_a, &info_b)?;
    diff_input_drvs(out, depth, &info_a, &info_b, max_depth, seen)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Field-level comparisons
// ---------------------------------------------------------------------------

fn diff_field(
    out: &mut impl Write,
    depth: usize,
    label: &str,
    a: Option<&str>,
    b: Option<&str>,
) -> io::Result<()> {
    if a == b {
        return Ok(());
    }
    writeln!(out, "{}  {} {label}", indent(depth), "•".bold())?;
    if let Some(v) = a {
        writeln!(out, "{}    {}", indent(depth), format!("- {v}").red())?;
    }
    if let Some(v) = b {
        writeln!(out, "{}    {}", indent(depth), format!("+ {v}").green())?;
    }
    Ok(())
}

fn diff_list(
    out: &mut impl Write,
    depth: usize,
    label: &str,
    a: &[String],
    b: &[String],
) -> io::Result<()> {
    if a == b {
        return Ok(());
    }
    writeln!(
        out,
        "{}  {} The {label} do not match",
        indent(depth),
        "•".bold()
    )?;
    for v in a {
        writeln!(out, "{}    {}", indent(depth), format!("- {v}").red())?;
    }
    for v in b {
        writeln!(out, "{}    {}", indent(depth), format!("+ {v}").green())?;
    }
    Ok(())
}

fn diff_outputs(
    out: &mut impl Write,
    depth: usize,
    a: &DerivationInfo,
    b: &DerivationInfo,
) -> io::Result<()> {
    let keys_a: BTreeSet<&String> = a.outputs.keys().collect();
    let keys_b: BTreeSet<&String> = b.outputs.keys().collect();

    if keys_a != keys_b {
        writeln!(
            out,
            "{}  {} The set of outputs do not match",
            indent(depth),
            "•".bold()
        )?;
        for k in keys_a.difference(&keys_b) {
            writeln!(out, "{}    {}", indent(depth), format!("- {k}").red())?;
        }
        for k in keys_b.difference(&keys_a) {
            writeln!(out, "{}    {}", indent(depth), format!("+ {k}").green())?;
        }
    }

    // Compare output hashes for fixed-output derivations.
    for key in keys_a.intersection(&keys_b) {
        let oa = &a.outputs[*key];
        let ob = &b.outputs[*key];
        if oa.hash != ob.hash || oa.hash_algo != ob.hash_algo {
            writeln!(
                out,
                "{}  {} Output {key} hash differs",
                indent(depth),
                "•".bold()
            )?;
            if let Some(h) = &oa.hash {
                let algo = oa.hash_algo.as_deref().unwrap_or("?");
                writeln!(
                    out,
                    "{}    {}",
                    indent(depth),
                    format!("- {algo}:{h}").red()
                )?;
            }
            if let Some(h) = &ob.hash {
                let algo = ob.hash_algo.as_deref().unwrap_or("?");
                writeln!(
                    out,
                    "{}    {}",
                    indent(depth),
                    format!("+ {algo}:{h}").green()
                )?;
            }
        }
    }
    Ok(())
}

fn diff_env(
    out: &mut impl Write,
    depth: usize,
    env_a: &std::collections::HashMap<String, String>,
    env_b: &std::collections::HashMap<String, String>,
    own_output_paths: &BTreeSet<&str>,
) -> io::Result<()> {
    // Merge all keys and sort for stable output.
    let all_keys: BTreeSet<&String> = env_a.keys().chain(env_b.keys()).collect();

    let mut diffs: Vec<(&str, Option<&str>, Option<&str>)> = Vec::new();
    for key in &all_keys {
        let va = env_a.get(*key).map(String::as_str);
        let vb = env_b.get(*key).map(String::as_str);
        if va == vb {
            continue;
        }
        // Filter mechanical noise: skip env vars whose value is one of the
        // derivation's own output paths (these change every time anything changes).
        if is_output_path_env(key, va, own_output_paths)
            && is_output_path_env(key, vb, own_output_paths)
        {
            continue;
        }
        diffs.push((key.as_str(), va, vb));
    }

    if diffs.is_empty() {
        return Ok(());
    }

    writeln!(
        out,
        "{}  {} The following environment variables differ",
        indent(depth),
        "•".bold()
    )?;
    for (key, va, vb) in &diffs {
        writeln!(out, "{}    {}:", indent(depth), key.bold())?;
        if let Some(v) = va {
            writeln!(out, "{}      {}", indent(depth), format!("- {v}").red())?;
        }
        if let Some(v) = vb {
            writeln!(out, "{}      {}", indent(depth), format!("+ {v}").green())?;
        }
    }
    Ok(())
}

/// Returns true if the environment variable value is one of the derivation's
/// own output paths (mechanical noise that changes every time).
fn is_output_path_env(key: &str, value: Option<&str>, own_output_paths: &BTreeSet<&str>) -> bool {
    // Standard nix output env vars like `out`, `dev`, `lib`, etc.
    let Some(v) = value else { return false };
    // Direct match: value is exactly an output path.
    if own_output_paths.contains(v) {
        return true;
    }
    // Also filter `outputs` which lists output names.
    if key == "outputs" {
        return true;
    }
    false
}

fn diff_input_srcs(
    out: &mut impl Write,
    depth: usize,
    a: &DerivationInfo,
    b: &DerivationInfo,
) -> io::Result<()> {
    let srcs_a: BTreeSet<&str> = a
        .inputs
        .as_ref()
        .map(|i| i.srcs.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let srcs_b: BTreeSet<&str> = b
        .inputs
        .as_ref()
        .map(|i| i.srcs.iter().map(String::as_str).collect())
        .unwrap_or_default();

    if srcs_a == srcs_b {
        return Ok(());
    }

    writeln!(
        out,
        "{}  {} The input sources do not match",
        indent(depth),
        "•".bold()
    )?;
    for s in srcs_a.difference(&srcs_b) {
        writeln!(out, "{}    {}", indent(depth), format!("- {s}").red())?;
    }
    for s in srcs_b.difference(&srcs_a) {
        writeln!(out, "{}    {}", indent(depth), format!("+ {s}").green())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Recursive input derivation diff
// ---------------------------------------------------------------------------

fn diff_input_drvs(
    out: &mut impl Write,
    depth: usize,
    a: &DerivationInfo,
    b: &DerivationInfo,
    max_depth: Option<usize>,
    seen: &mut HashSet<(String, String)>,
) -> color_eyre::Result<()> {
    let empty = std::collections::HashMap::new();
    let drvs_a = a.inputs.as_ref().map_or(&empty, |i| &i.drvs);
    let drvs_b = b.inputs.as_ref().map_or(&empty, |i| &i.drvs);

    // Build name-indexed maps: derivation name → (drv_path, outputs).
    let index_a = index_input_drvs(drvs_a);
    let index_b = index_input_drvs(drvs_b);

    let all_names: BTreeSet<&str> = index_a.keys().chain(index_b.keys()).copied().collect();

    let mut added: Vec<&str> = Vec::new();
    let mut removed: Vec<&str> = Vec::new();
    let mut changed: Vec<(&str, &str, &str)> = Vec::new();

    for name in &all_names {
        match (index_a.get(name), index_b.get(name)) {
            (Some(_), None) => removed.push(name),
            (None, Some(_)) => added.push(name),
            (Some((path_a, _)), Some((path_b, _))) => {
                if path_a != path_b {
                    changed.push((name, *path_a, *path_b));
                }
            },
            (None, None) => unreachable!(),
        }
    }

    if !removed.is_empty() || !added.is_empty() {
        writeln!(
            out,
            "{}  {} The set of input derivation names do not match",
            indent(depth),
            "•".bold()
        )?;
        for name in &removed {
            writeln!(out, "{}    {}", indent(depth), format!("- {name}").red())?;
        }
        for name in &added {
            writeln!(out, "{}    {}", indent(depth), format!("+ {name}").green())?;
        }
    }

    if !changed.is_empty() {
        writeln!(
            out,
            "{}  {} The following input derivations differ",
            indent(depth),
            "•".bold()
        )?;
        for (_, path_a, path_b) in &changed {
            writeln!(out)?;
            diff_drvs(out, path_a, path_b, depth + 1, max_depth, seen)?;
        }
    }

    Ok(())
}

/// Index input derivations by their parsed name (not full store path).
///
/// Returns name → (drv_path, output_names).
fn index_input_drvs(
    drvs: &std::collections::HashMap<String, eval::DerivationInputDrv>,
) -> BTreeMap<&str, (&str, &[String])> {
    let mut map = BTreeMap::new();
    for (path, input) in drvs {
        let (name, _) = store::parse_store_path_name(path);
        map.insert(name, (path.as_str(), input.outputs()));
    }
    map
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a human-readable display name from a drv store path.
fn drv_display_name(drv_path: &str) -> &str {
    drv_path.rsplit('/').next().unwrap_or(drv_path)
}

fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}
