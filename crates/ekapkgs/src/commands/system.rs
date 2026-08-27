use std::collections::HashSet;
use std::path::Path;
use std::process::{Command, Stdio};

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval};

use crate::cli::SystemCommand;
use crate::config::ClientConfig;

const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";

pub fn execute(command: SystemCommand) -> color_eyre::Result<()> {
    match command {
        SystemCommand::Switch {
            installable,
            dry_run,
            extra,
        } => cmd_activate(&installable, "switch", dry_run, &extra),
        SystemCommand::Boot { installable, extra } => {
            cmd_activate(&installable, "boot", false, &extra)
        },
        SystemCommand::Test { installable, extra } => {
            cmd_activate(&installable, "test", false, &extra)
        },
        SystemCommand::Build { installable, extra } => cmd_build(&installable, &extra),
        SystemCommand::ListGenerations => cmd_list_generations(),
        SystemCommand::Rollback { dry_run } => cmd_rollback(dry_run),
        SystemCommand::PruneBootEntries {
            boot_mount,
            gc,
            dry_run,
        } => cmd_prune_boot_entries(&boot_mount, gc, dry_run),
    }
}

fn cmd_activate(
    installable: &str,
    mode: &str,
    dry_run: bool,
    extra: &[String],
) -> color_eyre::Result<()> {
    let system_path = build_system(installable, extra)?;

    if dry_run {
        println!("Dry run:");
        println!("  System path: {system_path}");
        println!("  Mode:        {mode}");
        return Ok(());
    }

    // Update the system profile (requires root).
    tracing::info!("Setting system profile...");
    let status = Command::new("sudo")
        .arg("nix-env")
        .arg("--profile")
        .arg(SYSTEM_PROFILE)
        .arg("--set")
        .arg(&system_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to set system profile: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Failed to set system profile (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    // Activate the configuration.
    tracing::info!("Activating ({mode})...");
    let activate_cmd = format!("{system_path}/bin/switch-to-configuration");
    let status = Command::new("sudo")
        .arg(&activate_cmd)
        .arg(mode)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to run activation: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Activation failed (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    tracing::info!("System activation complete");
    Ok(())
}

fn cmd_build(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let store_path = build_system(installable, extra)?;
    println!("{store_path}");
    Ok(())
}

fn build_system(installable: &str, extra: &[String]) -> color_eyre::Result<String> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    // Pre-fetch from cache if configured.
    if config.primary_cache().is_some() {
        let spinner = ekapkgs_ui::progress::spinner("Evaluating system closure...");
        match eval::derivation_closure_paths(&inst) {
            Ok(closure_paths) => {
                spinner.finish_and_clear();
                crate::prefetch::prefetch_closure(&config, &closure_paths)?;
            },
            Err(_) => {
                spinner.finish_and_clear();
            },
        }
    }

    // Build.
    tracing::info!("Building system configuration...");
    let outputs: Vec<BuildOutput> = NixCommand::new(&["build"])
        .arg(installable)
        .arg("--json")
        .args(extra.iter().map(String::as_str))
        .json()?;

    let path = outputs
        .first()
        .and_then(|o| o.outputs.get("out").cloned())
        .ok_or_else(|| color_eyre::eyre::eyre!("build produced no output"))?;

    tracing::info!("Built {}", path);
    Ok(path)
}

fn cmd_list_generations() -> color_eyre::Result<()> {
    let profile_dir = Path::new(SYSTEM_PROFILE).parent().unwrap_or(Path::new("/"));

    let mut generations: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(profile_dir)?
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // System generations are named system-N-link
            let num: u64 = name
                .strip_prefix("system-")?
                .strip_suffix("-link")?
                .parse()
                .ok()?;
            let target = std::fs::read_link(e.path()).ok()?;
            Some((num, target))
        })
        .collect();

    generations.sort_by_key(|(num, _)| *num);

    if generations.is_empty() {
        println!("No system generations found.");
        return Ok(());
    }

    // Find current generation.
    let current_target = std::fs::read_link(SYSTEM_PROFILE).ok();

    for (num, target) in &generations {
        let marker = if current_target.as_ref() == Some(target) {
            " (current)"
        } else {
            ""
        };
        println!("{num:>4}  {}{marker}", target.display());
    }

    Ok(())
}

fn cmd_rollback(dry_run: bool) -> color_eyre::Result<()> {
    // Find the previous generation.
    let current_target = std::fs::read_link(SYSTEM_PROFILE)
        .map_err(|e| color_eyre::eyre::eyre!("failed to read system profile: {e}"))?;

    let profile_dir = Path::new(SYSTEM_PROFILE).parent().unwrap_or(Path::new("/"));

    let mut generations: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(profile_dir)?
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let num: u64 = name
                .strip_prefix("system-")?
                .strip_suffix("-link")?
                .parse()
                .ok()?;
            let target = std::fs::read_link(e.path()).ok()?;
            Some((num, target))
        })
        .collect();

    generations.sort_by_key(|(num, _)| *num);

    // Find the generation before the current one.
    let current_idx = generations
        .iter()
        .position(|(_, target)| *target == current_target);

    let prev = match current_idx {
        Some(idx) if idx > 0 => &generations[idx - 1],
        Some(_) => {
            return Err(color_eyre::eyre::eyre!(
                "Already at the oldest generation, nothing to roll back to"
            ));
        },
        None => {
            // Current profile doesn't match any generation link; use the latest.
            generations
                .last()
                .ok_or_else(|| color_eyre::eyre::eyre!("No system generations found"))?
        },
    };

    let (prev_num, prev_path) = prev;
    tracing::info!(
        "Rolling back to generation {prev_num}: {}",
        prev_path.display()
    );

    if dry_run {
        println!("Dry run:");
        println!("  Would roll back to generation {prev_num}");
        println!("  System path: {}", prev_path.display());
        return Ok(());
    }

    let prev_path_str = prev_path
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("generation path is not valid UTF-8"))?;

    // Set the profile to the previous generation.
    let status = Command::new("sudo")
        .arg("nix-env")
        .arg("--profile")
        .arg(SYSTEM_PROFILE)
        .arg("--set")
        .arg(prev_path_str)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to set system profile: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Failed to set system profile (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    // Activate.
    let activate_cmd = format!("{prev_path_str}/bin/switch-to-configuration");
    let status = Command::new("sudo")
        .arg(&activate_cmd)
        .arg("switch")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to run activation: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Rollback activation failed (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    tracing::info!("Rolled back to generation {prev_num}");
    Ok(())
}

fn cmd_prune_boot_entries(boot_mount: &str, gc: bool, dry_run: bool) -> color_eyre::Result<()> {
    if gc {
        tracing::info!("Running garbage collection...");
        let mut cmd = Command::new("sudo");
        cmd.arg("nix-collect-garbage").arg("-d");
        if dry_run {
            cmd.arg("--dry-run");
        }
        let status = cmd
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| color_eyre::eyre::eyre!("failed to run nix-collect-garbage: {e}"))?;

        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "Garbage collection failed (exit {})",
                status.code().unwrap_or(1)
            ));
        }
    }

    let boot_path = Path::new(boot_mount);
    let entries_dir = boot_path.join("loader/entries");
    let nixos_dir = boot_path.join("EFI/nixos");
    let uki_dir = boot_path.join("EFI/Linux");

    // Collect active generation numbers from profile links.
    let active_gens = collect_active_generations()?;
    if active_gens.is_empty() {
        println!("No active system generations found.");
        return Ok(());
    }

    tracing::info!("Active generations: {:?}", active_gens);

    let mut removed = 0u64;

    // Prune BLS entry files (nixos*-generation-N*.conf).
    if entries_dir.is_dir() {
        let (entry_removed, referenced_files) =
            prune_entry_files(&entries_dir, &active_gens, dry_run)?;
        removed += entry_removed;

        // Prune orphaned kernel/initrd files in EFI/nixos/.
        if nixos_dir.is_dir() {
            removed += prune_efi_files(&nixos_dir, &referenced_files, dry_run)?;
        }
    }

    // Prune orphaned UKI files (ekaos-*-generation-N*.efi).
    if uki_dir.is_dir() {
        removed += prune_uki_files(&uki_dir, &active_gens, dry_run)?;
    }

    if removed == 0 {
        println!("No orphaned boot entries found.");
    } else if dry_run {
        println!("Would remove {removed} file(s). Run without --dry-run to delete.");
    } else {
        println!("Removed {removed} orphaned file(s).");
    }

    Ok(())
}

/// Collect the set of generation numbers that have profile links.
fn collect_active_generations() -> color_eyre::Result<HashSet<u64>> {
    let profile_dir = Path::new(SYSTEM_PROFILE).parent().unwrap_or(Path::new("/"));
    let mut gens = HashSet::new();

    if !profile_dir.is_dir() {
        return Ok(gens);
    }

    for entry in std::fs::read_dir(profile_dir)?.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(num_str) = name
            .strip_prefix("system-")
            .and_then(|s| s.strip_suffix("-link"))
        {
            if let Ok(num) = num_str.parse::<u64>() {
                gens.insert(num);
            }
        }
    }

    Ok(gens)
}

/// Parse a generation number from a boot entry filename.
///
/// Matches patterns like:
///   nixos-generation-42.conf
///   nixos-generation-42-specialisation-foo.conf
///   nixos-myprofile-generation-42.conf
fn parse_entry_generation(filename: &str) -> Option<u64> {
    // Find "-generation-" and parse the number after it.
    let gen_marker = "-generation-";
    let idx = filename.find(gen_marker)?;
    let after = &filename[idx + gen_marker.len()..];
    // The number runs until the next '-' or '.'.
    let num_end = after.find(['-', '.']).unwrap_or(after.len());
    after[..num_end].parse().ok()
}

/// Remove orphaned .conf entry files. Returns (count_removed, set of
/// EFI filenames still referenced by surviving entries).
fn prune_entry_files(
    entries_dir: &Path,
    active_gens: &HashSet<u64>,
    dry_run: bool,
) -> color_eyre::Result<(u64, HashSet<String>)> {
    let mut removed = 0u64;
    let mut referenced_files = HashSet::new();

    for entry in std::fs::read_dir(entries_dir)?.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("nixos") || !name.ends_with(".conf") {
            continue;
        }

        let Some(gen_num) = parse_entry_generation(&name) else {
            continue;
        };

        if active_gens.contains(&gen_num) {
            // Entry is live — collect its referenced EFI files.
            if let Ok(contents) = std::fs::read_to_string(entry.path()) {
                collect_efi_refs(&contents, &mut referenced_files);
            }
        } else {
            // Orphaned entry.
            if dry_run {
                println!("  Would remove entry: {name}");
            } else {
                std::fs::remove_file(entry.path())?;
                println!("  Removed entry: {name}");
            }
            removed += 1;
        }
    }

    Ok((removed, referenced_files))
}

/// Extract EFI filenames referenced in a boot entry's linux/initrd lines.
fn collect_efi_refs(contents: &str, refs: &mut HashSet<String>) {
    for line in contents.lines() {
        let line = line.trim();
        for prefix in &["linux ", "initrd ", "devicetree "] {
            if let Some(path) = line.strip_prefix(prefix) {
                // Path is like /EFI/nixos/hash-name.efi — extract the filename.
                if let Some(filename) = path.trim().rsplit('/').next() {
                    refs.insert(filename.to_owned());
                }
            }
        }
    }
}

/// Remove kernel/initrd files in EFI/nixos/ that aren't referenced
/// by any surviving boot entry.
fn prune_efi_files(
    nixos_dir: &Path,
    referenced: &HashSet<String>,
    dry_run: bool,
) -> color_eyre::Result<u64> {
    let mut removed = 0u64;

    for entry in std::fs::read_dir(nixos_dir)?.filter_map(std::result::Result::ok) {
        if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(true) {
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        if !referenced.contains(&name) {
            if dry_run {
                println!("  Would remove EFI file: {name}");
            } else {
                std::fs::remove_file(entry.path())?;
                println!("  Removed EFI file: {name}");
            }
            removed += 1;
        }
    }

    Ok(removed)
}

/// Remove UKI files for generations that no longer exist.
fn prune_uki_files(
    uki_dir: &Path,
    active_gens: &HashSet<u64>,
    dry_run: bool,
) -> color_eyre::Result<u64> {
    let mut removed = 0u64;

    for entry in std::fs::read_dir(uki_dir)?.filter_map(std::result::Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("ekaos-") || !name.ends_with(".efi") {
            continue;
        }

        let Some(gen_num) = parse_entry_generation(&name) else {
            continue;
        };

        if !active_gens.contains(&gen_num) {
            if dry_run {
                println!("  Would remove UKI: {name}");
            } else {
                std::fs::remove_file(entry.path())?;
                println!("  Removed UKI: {name}");
            }
            removed += 1;
        }
    }

    Ok(removed)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildOutput {
    outputs: std::collections::HashMap<String, String>,
}
