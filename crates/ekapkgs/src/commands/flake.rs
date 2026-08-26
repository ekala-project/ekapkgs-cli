use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval};
use yansi::Paint;

use crate::cli::FlakeCommand;

pub fn execute(command: FlakeCommand) -> color_eyre::Result<()> {
    match command {
        FlakeCommand::Show { flake_ref } => cmd_show(&flake_ref),
        FlakeCommand::Metadata { flake_ref } => cmd_metadata(&flake_ref),
        FlakeCommand::UpdateDiff { input, installable } => cmd_update_diff(&input, &installable),
    }
}

fn cmd_show(flake_ref: &str) -> color_eyre::Result<()> {
    let tree: serde_json::Value = NixCommand::new(&["flake", "show"])
        .arg(flake_ref)
        .arg("--json")
        .json()?;

    println!("{}", flake_ref.bold());
    render_tree(&tree, "", true);
    Ok(())
}

fn render_tree(value: &serde_json::Value, prefix: &str, _is_last: bool) {
    let Some(obj) = value.as_object() else {
        return;
    };

    // If this object has a "type" key, it's a leaf (a derivation/output).
    if obj.contains_key("type") {
        return;
    }

    let entries: Vec<_> = obj.iter().collect();
    for (i, (key, val)) in entries.iter().enumerate() {
        let last = i == entries.len() - 1;
        let connector = if last { "└───" } else { "├───" };
        let child_prefix = if last {
            format!("{prefix}    ")
        } else {
            format!("{prefix}│   ")
        };

        // Check if leaf (has "type" field).
        if let Some(leaf_obj) = val.as_object() {
            if let Some(typ) = leaf_obj.get("type").and_then(|t| t.as_str()) {
                println!("{prefix}{connector} {}: {}", key.bold(), typ.dim());
                continue;
            }
        }

        println!("{prefix}{connector} {}", key.bold());
        render_tree(val, &child_prefix, last);
    }
}

fn cmd_metadata(flake_ref: &str) -> color_eyre::Result<()> {
    let meta: serde_json::Value = NixCommand::new(&["flake", "metadata"])
        .arg(flake_ref)
        .arg("--json")
        .json()?;

    // Print basic metadata.
    if let Some(desc) = meta.get("description").and_then(|d| d.as_str()) {
        println!("{}: {desc}", "Description".bold());
    }
    if let Some(url) = meta.get("resolvedUrl").and_then(|u| u.as_str()) {
        println!("{}: {url}", "URL".bold());
    }
    if let Some(rev) = meta.get("revision").and_then(|r| r.as_str()) {
        println!("{}: {}", "Revision".bold(), &rev[..12.min(rev.len())]);
    }
    if let Some(modified) = meta.get("lastModified").and_then(serde_json::Value::as_i64) {
        let dt = chrono_format_timestamp(modified);
        println!("{}: {dt}", "Last modified".bold());
    }

    // Print input tree from locks.
    if let Some(locks) = meta.get("locks") {
        if let Some(nodes) = locks.get("nodes").and_then(|n| n.as_object()) {
            println!();
            println!("{}", "Inputs:".bold());
            if let Some(root) = nodes.get("root").and_then(|r| r.as_object()) {
                if let Some(inputs) = root.get("inputs").and_then(|i| i.as_object()) {
                    let entries: Vec<_> = inputs.iter().collect();
                    for (i, (name, target)) in entries.iter().enumerate() {
                        let last = i == entries.len() - 1;
                        let connector = if last { "└───" } else { "├───" };
                        let child_prefix = if last { "    " } else { "│   " };

                        let target_name = target.as_str().unwrap_or("");
                        if let Some(node) = nodes.get(target_name).and_then(|n| n.as_object()) {
                            let locked = node.get("locked").and_then(|l| l.as_object());
                            let rev = locked
                                .and_then(|l| l.get("rev"))
                                .and_then(|r| r.as_str())
                                .map(|r| &r[..12.min(r.len())])
                                .unwrap_or("?");
                            let modified = locked
                                .and_then(|l| l.get("lastModified"))
                                .and_then(serde_json::Value::as_i64)
                                .map(chrono_format_timestamp)
                                .unwrap_or_default();

                            println!(
                                "{connector} {} {} {}",
                                name.bold(),
                                rev.dim(),
                                modified.dim()
                            );

                            // Recurse into sub-inputs.
                            if let Some(sub_inputs) = node.get("inputs").and_then(|i| i.as_object())
                            {
                                let sub_entries: Vec<_> = sub_inputs.iter().collect();
                                for (j, (sub_name, sub_target)) in sub_entries.iter().enumerate() {
                                    let sub_last = j == sub_entries.len() - 1;
                                    let sub_connector = if sub_last {
                                        "└───"
                                    } else {
                                        "├───"
                                    };
                                    let sub_target_name = sub_target.as_str().unwrap_or("");
                                    let sub_rev = nodes
                                        .get(sub_target_name)
                                        .and_then(|n| n.get("locked"))
                                        .and_then(|l| l.get("rev"))
                                        .and_then(|r| r.as_str())
                                        .map(|r| &r[..12.min(r.len())])
                                        .unwrap_or("→");
                                    println!(
                                        "{child_prefix}{sub_connector} {} {}",
                                        sub_name.bold(),
                                        sub_rev.dim()
                                    );
                                }
                            }
                        } else {
                            println!("{connector} {}", name.bold());
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn chrono_format_timestamp(ts: i64) -> String {
    // Simple UTC date formatting without pulling in chrono.
    // Unix timestamp to YYYY-MM-DD HH:MM:SS UTC.
    let secs_per_day: i64 = 86400;
    let days = ts / secs_per_day;
    let rem = ts % secs_per_day;
    let hours = rem / 3600;
    let mins = (rem % 3600) / 60;
    let secs = rem % 60;

    // Days since epoch to Y-M-D (simplified civil calendar from Howard Hinnant).
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02} {hours:02}:{mins:02}:{secs:02} UTC")
}

fn cmd_update_diff(input: &str, installable: &str) -> color_eyre::Result<()> {
    let inst = Installable::new(installable);

    // Backup the current flake.lock.
    let lock_path = std::path::Path::new("flake.lock");
    if !lock_path.exists() {
        return Err(color_eyre::eyre::eyre!(
            "flake.lock not found in current directory"
        ));
    }
    let backup = std::fs::read(lock_path)?;

    // Evaluate current closure.
    let spinner = ekapkgs_ui::progress::spinner("Evaluating current closure...");
    let old_paths = eval::derivation_closure_paths(&inst)?;
    spinner.finish_and_clear();

    // Update the input.
    tracing::info!("Updating flake input '{input}'...");
    let update_result = NixCommand::new(&["flake", "update"]).arg(input).stream();

    if update_result.is_err() {
        // Try older syntax as fallback.
        let _ = NixCommand::new(&["flake", "lock"])
            .arg("--update-input")
            .arg(input)
            .stream();
    }

    // Evaluate new closure.
    let spinner = ekapkgs_ui::progress::spinner("Evaluating updated closure...");
    let new_paths = eval::derivation_closure_paths(&inst);
    spinner.finish_and_clear();

    // Restore the original flake.lock regardless of outcome.
    std::fs::write(lock_path, &backup)?;
    tracing::info!("Restored original flake.lock");

    let new_paths = new_paths?;

    // Compute diff.
    let old_set: std::collections::HashSet<&str> =
        old_paths.iter().map(std::string::String::as_str).collect();
    let new_set: std::collections::HashSet<&str> =
        new_paths.iter().map(std::string::String::as_str).collect();

    let added: Vec<_> = new_set.difference(&old_set).collect();
    let removed: Vec<_> = old_set.difference(&new_set).collect();

    println!();
    println!(
        "{}: {} paths → {} paths",
        "Closure diff".bold(),
        old_paths.len(),
        new_paths.len()
    );
    println!(
        "  {} {} added, {} {} removed",
        "+".green(),
        added.len(),
        "-".red(),
        removed.len()
    );

    if !added.is_empty() {
        println!();
        println!("{}", "Added:".green().bold());
        for path in &added {
            let name = path.rsplit('/').next().unwrap_or(path);
            println!("  + {name}");
        }
    }

    if !removed.is_empty() {
        println!();
        println!("{}", "Removed:".red().bold());
        for path in &removed {
            let name = path.rsplit('/').next().unwrap_or(path);
            println!("  - {name}");
        }
    }

    Ok(())
}
