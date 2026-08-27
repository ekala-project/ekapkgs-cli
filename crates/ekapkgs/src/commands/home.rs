use std::process::Stdio;

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval};

use crate::cli::HomeCommand;
use crate::config::ClientConfig;

pub fn execute(command: HomeCommand) -> color_eyre::Result<()> {
    match command {
        HomeCommand::Switch { installable, extra } => cmd_switch(&installable, &extra),
        HomeCommand::Build { installable, extra } => cmd_build(&installable, &extra),
        HomeCommand::Generations => cmd_generations(),
        HomeCommand::Packages => cmd_packages(),
    }
}

fn cmd_switch(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let store_path = build_home(installable, extra)?;

    // Activate as current user (no sudo).
    tracing::info!("Activating home configuration...");
    let activate_path = format!("{store_path}/activate");
    let status = std::process::Command::new(&activate_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to run activation script: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Home activation failed (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    tracing::info!("Home configuration activated");
    Ok(())
}

fn cmd_build(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let store_path = build_home(installable, extra)?;
    println!("{store_path}");
    Ok(())
}

fn build_home(installable: &str, extra: &[String]) -> color_eyre::Result<String> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    // Pre-fetch from cache if configured.
    if config.primary_cache().is_some() {
        let spinner = ekapkgs_ui::progress::spinner("Evaluating home closure...");
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
    tracing::info!("Building home configuration...");
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

fn cmd_generations() -> color_eyre::Result<()> {
    let state_dir = dirs_path()?.join("generations");

    if !state_dir.exists() {
        println!("No generations found.");
        return Ok(());
    }

    let mut entries: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(&state_dir)?
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let num: u64 = name.parse().ok()?;
            let target = std::fs::read_link(e.path()).ok()?;
            Some((num, target))
        })
        .collect();

    entries.sort_by_key(|(num, _)| *num);

    if entries.is_empty() {
        println!("No generations found.");
        return Ok(());
    }

    for (num, target) in &entries {
        println!("{num:>4}  {}", target.display());
    }

    Ok(())
}

fn cmd_packages() -> color_eyre::Result<()> {
    let profile = home_dir()?.join(".ekaos-profile");

    if !profile.exists() {
        println!("No home profile active.");
        return Ok(());
    }

    let resolved = std::fs::read_link(&profile)
        .map_err(|e| color_eyre::eyre::eyre!("failed to read profile link: {e}"))?;

    // List binaries in the profile.
    let bin_dir = resolved.join("bin");
    if bin_dir.is_dir() {
        let mut bins: Vec<String> = std::fs::read_dir(&bin_dir)?
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        bins.sort();

        println!("Profile: {}", resolved.display());
        println!("Binaries ({}):", bins.len());
        for bin in &bins {
            println!("  {bin}");
        }
    } else {
        println!("Profile: {}", resolved.display());
        println!("No binaries in profile.");
    }

    Ok(())
}

fn home_dir() -> color_eyre::Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .map_err(|_| color_eyre::eyre::eyre!("HOME environment variable not set"))
}

fn dirs_path() -> color_eyre::Result<std::path::PathBuf> {
    Ok(home_dir()?.join(".config/ekaos"))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildOutput {
    outputs: std::collections::HashMap<String, String>,
}
