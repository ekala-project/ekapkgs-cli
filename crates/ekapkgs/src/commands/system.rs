use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval};
use jiff::Timestamp;
use yansi::Paint;

use crate::cli::{SystemCommand, SystemPackagesCommand};
use crate::config::{ClientConfig, SystemPackageEntry, SystemPackages};

const SYSTEM_PROFILE: &str = "/nix/var/nix/profiles/system";

struct Generation {
    number: u64,
    path: PathBuf,
    created: Timestamp,
}

fn discover_generations() -> color_eyre::Result<Vec<Generation>> {
    let profile_dir = Path::new(SYSTEM_PROFILE).parent().unwrap_or(Path::new("/"));
    let mut generations: Vec<Generation> = std::fs::read_dir(profile_dir)?
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let number: u64 = name
                .strip_prefix("system-")?
                .strip_suffix("-link")?
                .parse()
                .ok()?;
            let path = std::fs::read_link(e.path()).ok()?;
            let modified = e.metadata().ok()?.modified().ok()?;
            let created = Timestamp::try_from(modified).ok()?;
            Some(Generation {
                number,
                path,
                created,
            })
        })
        .collect();
    generations.sort_by_key(|g| g.number);
    Ok(generations)
}

/// Nix profile for imperatively-installed system packages.
const PACKAGES_PROFILE: &str = "/nix/var/nix/profiles/ekapkgs-system-packages";

pub fn execute(command: SystemCommand) -> color_eyre::Result<()> {
    match command {
        SystemCommand::Switch {
            installable,
            dry_run,
            channel,
            extra,
        } => {
            if let Some(ch) = &channel {
                apply_channel(ch, &installable)?;
            }
            cmd_activate(&installable, "switch", dry_run, &extra)
        },
        SystemCommand::Boot {
            installable,
            channel,
            extra,
        } => {
            if let Some(ch) = &channel {
                apply_channel(ch, &installable)?;
            }
            cmd_activate(&installable, "boot", false, &extra)
        },
        SystemCommand::Test { installable, extra } => {
            cmd_activate(&installable, "test", false, &extra)
        },
        SystemCommand::Build { installable, extra } => cmd_build(&installable, &extra),
        SystemCommand::Diff { installable, extra } => cmd_diff(&installable, &extra),
        SystemCommand::ListGenerations { json } => cmd_list_generations(json),
        SystemCommand::Rollback { dry_run } => cmd_rollback(dry_run),
        SystemCommand::PruneBootEntries {
            boot_mount,
            gc,
            dry_run,
        } => cmd_prune_boot_entries(&boot_mount, gc, dry_run),
        SystemCommand::Update { installable, extra } => cmd_update(&installable, &extra),
        SystemCommand::Packages { command } => cmd_packages(command),
    }
}

/// Update the flake lock to point a configured input at a specific channel
/// (branch). Reads `channel_input` and `channel_url` from the client config.
fn apply_channel(channel: &str, installable: &str) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;
    let input = config.defaults.channel_input.as_deref().ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "`--channel` requires `channel_input` in config (~/.config/ekapkgs/config.toml)"
        )
    })?;
    let base_url = config.defaults.channel_url.as_deref().ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "`--channel` requires `channel_url` in config (~/.config/ekapkgs/config.toml)"
        )
    })?;

    let flake_ref = format!("{base_url}/{channel}");
    tracing::info!("Switching channel: {input} → {flake_ref}");

    // Resolve the flake directory from the installable (everything before #).
    let flake_dir = installable
        .split_once('#')
        .map_or(installable, |(flake, _)| flake);

    let status = Command::new("nix")
        .arg("flake")
        .arg("lock")
        .arg(flake_dir)
        .arg("--override-input")
        .arg(input)
        .arg(&flake_ref)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to run nix flake lock: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Failed to update flake input for channel '{channel}' (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    Ok(())
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

    // Record previous profile target so we can roll back on failure.
    let prev_target = std::fs::read_link(SYSTEM_PROFILE).ok();

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
        // Activation failed — try to restore the previous profile and
        // re-activate it so the system isn't left in a broken state.
        if let Some(prev) = &prev_target {
            let prev_str = prev.to_string_lossy();
            tracing::warn!(
                "Activation failed, rolling back to previous configuration ({prev_str})..."
            );

            let profile_restored = Command::new("sudo")
                .args(["nix-env", "--profile", SYSTEM_PROFILE, "--set"])
                .arg(prev_str.as_ref())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());

            if profile_restored {
                let prev_activate = format!("{prev_str}/bin/switch-to-configuration");
                match Command::new("sudo")
                    .arg(&prev_activate)
                    .arg("switch")
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .status()
                {
                    Ok(s) if s.success() => {
                        tracing::info!("Rolled back to previous configuration");
                    },
                    Ok(s) => {
                        tracing::error!(
                            "Rollback activation also failed (exit {}); system may be in an \
                             inconsistent state",
                            s.code().unwrap_or(1)
                        );
                    },
                    Err(e) => {
                        tracing::error!(
                            "Failed to run rollback activation: {e}; system may be in an \
                             inconsistent state"
                        );
                    },
                }
            } else {
                tracing::error!(
                    "Failed to restore system profile; system may be in an inconsistent state — \
                     manually run: sudo nix-env --profile {SYSTEM_PROFILE} --set {prev_str}"
                );
            }
        }
        return Err(color_eyre::eyre::eyre!(
            "Activation failed (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    tracing::info!("System activation complete");

    // Populate the store path index from the built closure.
    populate_store_path_index(&system_path);

    Ok(())
}

fn cmd_update(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    // Update flake inputs.
    tracing::info!("Updating flake inputs...");
    NixCommand::new(&["flake", "update"]).stream()?;

    // Invalidate the store path index.
    if let Ok(mut index) = crate::store_path_index::StorePathIndex::load() {
        index.invalidate();
        let _ = index.save();
        tracing::info!("Store path index invalidated");
    }

    // Rebuild and activate.
    cmd_activate(installable, "switch", false, extra)
}

/// Populate the store path index from the closure of a built store path.
fn populate_store_path_index(store_path: &str) {
    let inst = Installable::new(store_path);
    match ekapkgs_nix::store::closure_path_info(&inst) {
        Ok(entries) => {
            let paths: Vec<String> = entries.into_iter().map(|e| e.path).collect();
            match crate::store_path_index::StorePathIndex::load() {
                Ok(mut index) => {
                    index.populate_from_closure(&paths);
                    if let Err(e) = index.save() {
                        tracing::warn!("Failed to save store path index: {e}");
                    } else {
                        tracing::info!(
                            "Updated store path index ({} entries)",
                            index.entries.len()
                        );
                    }
                },
                Err(e) => tracing::warn!("Failed to load store path index: {e}"),
            }
        },
        Err(e) => {
            tracing::debug!("Could not populate store path index: {e}");
        },
    }
}

fn cmd_build(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let store_path = build_system(installable, extra)?;
    println!("{store_path}");
    Ok(())
}

fn cmd_diff(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let new_path = build_system(installable, extra)?;
    let current = "/run/current-system";

    if !Path::new(current).exists() {
        return Err(color_eyre::eyre::eyre!(
            "{current} does not exist — is this a NixOS system?"
        ));
    }

    let current_resolved = std::fs::read_link(current)
        .unwrap_or_else(|_| PathBuf::from(current))
        .to_string_lossy()
        .into_owned();

    if current_resolved == new_path {
        println!("System is up to date.");
        std::process::exit(1);
    }

    NixCommand::new(&["store", "diff-closures"])
        .arg(&current_resolved)
        .arg(&new_path)
        .stream()?;

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

fn cmd_list_generations(json_output: bool) -> color_eyre::Result<()> {
    let generations = discover_generations()?;

    if generations.is_empty() {
        if json_output {
            println!("[]");
        } else {
            println!("No system generations found.");
        }
        return Ok(());
    }

    // Find current generation.
    let current_target = std::fs::read_link(SYSTEM_PROFILE).ok();

    if json_output {
        let entries: Vec<serde_json::Value> = generations
            .iter()
            .map(|g| {
                serde_json::json!({
                    "number": g.number,
                    "path": g.path.to_string_lossy(),
                    "created_at": g.created.to_string(),
                    "current": current_target.as_ref() == Some(&g.path),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        for g in &generations {
            let marker = if current_target.as_ref() == Some(&g.path) {
                " (current)"
            } else {
                ""
            };
            println!("{:>4}  {}{marker}", g.number, g.path.display());
        }
    }

    Ok(())
}

fn cmd_rollback(dry_run: bool) -> color_eyre::Result<()> {
    // Find the previous generation.
    let current_target = std::fs::read_link(SYSTEM_PROFILE)
        .map_err(|e| color_eyre::eyre::eyre!("failed to read system profile: {e}"))?;

    let generations = discover_generations()?;

    // Find the generation before the current one.
    let current_idx = generations.iter().position(|g| g.path == current_target);

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

    let (prev_num, prev_path) = (prev.number, &prev.path);
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
    Ok(discover_generations()?
        .into_iter()
        .map(|g| g.number)
        .collect())
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

// ---------------------------------------------------------------------------
// Packages
// ---------------------------------------------------------------------------

fn cmd_packages(command: SystemPackagesCommand) -> color_eyre::Result<()> {
    match command {
        SystemPackagesCommand::Add { packages, flake } => {
            cmd_packages_add(&packages, flake.as_deref())
        },
        SystemPackagesCommand::Remove { packages } => cmd_packages_remove(&packages),
        SystemPackagesCommand::Present { packages } => cmd_packages_present(&packages),
        SystemPackagesCommand::Missing { packages } => cmd_packages_missing(&packages),
        SystemPackagesCommand::List { json } => cmd_packages_list(json),
        SystemPackagesCommand::Export { output } => cmd_packages_export(output.as_deref()),
        SystemPackagesCommand::Import { file, merge } => cmd_packages_import(&file, merge),
    }
}

fn cmd_packages_add(packages: &[String], flake_override: Option<&str>) -> color_eyre::Result<()> {
    let (mut manifest, _lock) = SystemPackages::load_locked()?;
    let mut index = crate::store_path_index::StorePathIndex::load()?;
    let mut added = 0u32;

    let effective_flake = flake_override
        .map(str::to_owned)
        .unwrap_or_else(|| manifest.flake.clone());

    for name in packages {
        if manifest.packages.iter().any(|p| p.name == *name) {
            tracing::warn!("{name} is already in the manifest, skipping");
            continue;
        }

        // Validate the package name against the search index.
        crate::package_validate::validate_package_name(name, &effective_flake)?;

        let entry = SystemPackageEntry {
            name: name.clone(),
            flake: flake_override.map(str::to_owned),
        };
        let installable = manifest.resolve_installable(&entry);

        // System packages still use nix profile install (requires sudo),
        // but we validate first and cache the result.
        tracing::info!("Installing {installable}...");
        let status = Command::new("sudo")
            .arg("nix")
            .arg("profile")
            .arg("install")
            .arg("--profile")
            .arg(PACKAGES_PROFILE)
            .arg(&installable)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| color_eyre::eyre::eyre!("failed to run nix profile install: {e}"))?;

        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "Failed to install {installable} (exit {})",
                status.code().unwrap_or(1)
            ));
        }

        // Cache in the store path index for next time.
        let outputs: Result<Vec<BuildOutput>, _> = NixCommand::new(&["build"])
            .arg(&installable)
            .arg("--no-link")
            .arg("--json")
            .json();
        if let Ok(outputs) = outputs {
            if let Some(store_path) = outputs.first().and_then(|o| o.outputs.get("out")) {
                index.add_from_build_output(name, store_path);
                let _ = index.save();
            }
        }

        manifest.add(entry);
        manifest.save()?;
        added += 1;
    }

    if added > 0 {
        println!(
            "Added {added} package(s) to {}",
            SystemPackages::manifest_path().display()
        );
    }

    Ok(())
}

fn cmd_packages_remove(packages: &[String]) -> color_eyre::Result<()> {
    let (mut manifest, _lock) = SystemPackages::load_locked()?;
    let mut removed = 0u32;

    for name in packages {
        if !manifest.remove(name) {
            tracing::warn!("{name} is not in the manifest, skipping");
            continue;
        }

        tracing::info!("Removing {name} from profile...");
        let status = Command::new("sudo")
            .arg("nix")
            .arg("profile")
            .arg("remove")
            .arg("--profile")
            .arg(PACKAGES_PROFILE)
            .arg(name)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();

        match status {
            Ok(s) if !s.success() => {
                tracing::warn!("Failed to remove {name} from nix profile");
            },
            Err(e) => {
                tracing::warn!("Failed to remove {name} from nix profile: {e}");
            },
            _ => {},
        }

        removed += 1;
    }

    manifest.save()?;

    if removed > 0 {
        println!("Removed {removed} package(s)");
    }

    Ok(())
}

/// Check if all packages are present.
fn cmd_packages_present(packages: &[String]) -> color_eyre::Result<()> {
    let manifest = SystemPackages::load()?;

    for name in packages {
        if manifest.packages.iter().any(|p| p.name == *name) {
            continue;
        }
        if which::which(name).is_ok() {
            continue;
        }
        std::process::exit(1);
    }
    Ok(())
}

/// Check if all packages are missing.
fn cmd_packages_missing(packages: &[String]) -> color_eyre::Result<()> {
    let manifest = SystemPackages::load()?;

    for name in packages {
        if manifest.packages.iter().any(|p| p.name == *name) {
            std::process::exit(1);
        }
        if which::which(name).is_ok() {
            std::process::exit(1);
        }
    }
    Ok(())
}

fn cmd_packages_list(json_output: bool) -> color_eyre::Result<()> {
    let manifest = SystemPackages::load()?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&manifest.packages)?);
        return Ok(());
    }

    if manifest.packages.is_empty() {
        println!("No system packages installed.");
        println!(
            "{}",
            "Use `ekapkgs system packages add <package>` to add one.".dim()
        );
        return Ok(());
    }

    for entry in &manifest.packages {
        let flake_display = entry.flake.as_deref().unwrap_or(&manifest.flake);
        println!(
            "  {} {}",
            entry.name.bold(),
            format!("({flake_display})").dim()
        );
    }
    println!("\n{} package(s)", manifest.packages.len());

    Ok(())
}

fn cmd_packages_export(output: Option<&str>) -> color_eyre::Result<()> {
    let manifest = SystemPackages::load()?;
    let contents = toml::to_string_pretty(&manifest)?;

    if let Some(path) = output {
        std::fs::write(path, &contents)?;
        println!("Exported {} package(s) to {path}", manifest.packages.len());
    } else {
        print!("{contents}");
    }

    Ok(())
}

fn cmd_packages_import(file: &str, merge: bool) -> color_eyre::Result<()> {
    let contents = std::fs::read_to_string(file)?;
    let imported: SystemPackages = toml::from_str(&contents)?;
    let (old_manifest, _lock) = SystemPackages::load_locked()?;

    let mut manifest = if merge {
        let mut current = old_manifest.clone();
        for entry in imported.packages {
            current.remove(&entry.name);
            current.packages.push(entry);
        }
        current
    } else {
        imported.clone()
    };

    manifest.version = 1;

    // Remove packages from the nix profile that are in the old manifest
    // but absent from the new one.
    let new_names: HashSet<&str> = manifest.packages.iter().map(|p| p.name.as_str()).collect();
    for old_entry in &old_manifest.packages {
        if !new_names.contains(old_entry.name.as_str()) {
            tracing::info!("Removing {} from profile...", old_entry.name);
            let _ = Command::new("sudo")
                .args(["nix", "profile", "remove", "--profile", PACKAGES_PROFILE])
                .arg(&old_entry.name)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }

    manifest.save()?;

    // Sync the nix profile: install all packages from the manifest.
    let mut installed = 0u32;
    for entry in &manifest.packages {
        let installable = manifest.resolve_installable(entry);
        tracing::info!("Installing {installable}...");
        let status = Command::new("sudo")
            .arg("nix")
            .arg("profile")
            .arg("install")
            .arg("--profile")
            .arg(PACKAGES_PROFILE)
            .arg(&installable)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();

        match status {
            Ok(s) if s.success() => installed += 1,
            Ok(_) => tracing::warn!("Failed to install {}", entry.name),
            Err(e) => tracing::warn!("Failed to install {}: {e}", entry.name),
        }
    }

    println!(
        "Imported {} package(s) ({installed} installed to profile)",
        manifest.packages.len()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Build output
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildOutput {
    outputs: std::collections::HashMap<String, String>,
}
