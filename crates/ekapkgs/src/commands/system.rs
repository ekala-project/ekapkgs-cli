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

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildOutput {
    outputs: std::collections::HashMap<String, String>,
}
