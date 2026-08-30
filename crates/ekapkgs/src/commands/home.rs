use std::process::Stdio;

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval};
use yansi::Paint;

use crate::cli::{HomeCommand, HomePackagesCommand};
use crate::config::{ClientConfig, HomePackageEntry, HomePackages};

/// Profile path for imperatively-installed packages (relative to `$HOME`).
const PACKAGES_PROFILE: &str = ".ekapkgs-packages";

pub fn execute(command: HomeCommand) -> color_eyre::Result<()> {
    match command {
        HomeCommand::Switch { installable, extra } => cmd_switch(&installable, &extra),
        HomeCommand::Build { installable, extra } => cmd_build(&installable, &extra),
        HomeCommand::Generations => cmd_generations(),
        HomeCommand::Packages { command } => cmd_packages(command),
    }
}

// ---------------------------------------------------------------------------
// Switch / Build
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Generations
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Packages
// ---------------------------------------------------------------------------

fn cmd_packages(command: HomePackagesCommand) -> color_eyre::Result<()> {
    match command {
        HomePackagesCommand::Add { packages, flake } => {
            cmd_packages_add(&packages, flake.as_deref())
        },
        HomePackagesCommand::Remove { packages } => cmd_packages_remove(&packages),
        HomePackagesCommand::List { json } => cmd_packages_list(json),
        HomePackagesCommand::Export { output } => cmd_packages_export(output.as_deref()),
        HomePackagesCommand::Import { file, merge } => cmd_packages_import(&file, merge),
    }
}

fn packages_profile_path() -> color_eyre::Result<String> {
    let home = std::env::var("HOME")
        .map_err(|_| color_eyre::eyre::eyre!("HOME environment variable not set"))?;
    Ok(format!("{home}/{PACKAGES_PROFILE}"))
}

fn cmd_packages_add(packages: &[String], flake_override: Option<&str>) -> color_eyre::Result<()> {
    let mut manifest = HomePackages::load()?;
    let profile = packages_profile_path()?;
    let mut added = 0u32;

    for name in packages {
        let entry = HomePackageEntry {
            name: name.clone(),
            flake: flake_override.map(str::to_owned),
        };

        let installable = manifest.resolve_installable(&entry);

        if !manifest.add(entry) {
            tracing::warn!("{name} is already in the manifest, skipping");
            continue;
        }

        tracing::info!("Installing {installable}...");
        NixCommand::new(&["profile", "install"])
            .arg("--profile")
            .arg(&profile)
            .arg(&installable)
            .stream()?;

        added += 1;
    }

    manifest.save()?;

    if added > 0 {
        println!(
            "Added {added} package(s) to {}",
            HomePackages::manifest_path().display()
        );

        // Hint about PATH on first use.
        let profile_bin = format!("{profile}/bin");
        if let Ok(path) = std::env::var("PATH") {
            if !path.contains(&profile_bin) {
                println!(
                    "\n{}",
                    format!(
                        "Hint: add {profile_bin} to your PATH to use these packages:\n  export \
                         PATH=\"$HOME/{PACKAGES_PROFILE}/bin:$PATH\""
                    )
                    .dim()
                );
            }
        }
    }

    Ok(())
}

fn cmd_packages_remove(packages: &[String]) -> color_eyre::Result<()> {
    let mut manifest = HomePackages::load()?;
    let profile = packages_profile_path()?;
    let mut removed = 0u32;

    for name in packages {
        if !manifest.remove(name) {
            tracing::warn!("{name} is not in the manifest, skipping");
            continue;
        }

        tracing::info!("Removing {name} from profile...");
        // `nix profile remove` accepts a regex matching the package name.
        if let Err(e) = NixCommand::new(&["profile", "remove"])
            .arg("--profile")
            .arg(&profile)
            .arg(name)
            .stream()
        {
            tracing::warn!("Failed to remove {name} from nix profile: {e}");
        }

        removed += 1;
    }

    manifest.save()?;

    if removed > 0 {
        println!("Removed {removed} package(s)");
    }

    Ok(())
}

fn cmd_packages_list(json_output: bool) -> color_eyre::Result<()> {
    let manifest = HomePackages::load()?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&manifest.packages)?);
        return Ok(());
    }

    if manifest.packages.is_empty() {
        println!("No packages installed.");
        println!(
            "{}",
            "Use `ekapkgs home packages add <package>` to add one.".dim()
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
    let manifest = HomePackages::load()?;
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
    let imported: HomePackages = toml::from_str(&contents)?;
    let profile = packages_profile_path()?;

    let mut manifest = if merge {
        let mut current = HomePackages::load()?;
        for entry in imported.packages {
            // Imported entries win on name conflict.
            current.remove(&entry.name);
            current.packages.push(entry);
        }
        current
    } else {
        imported.clone()
    };

    // Preserve the version field.
    manifest.version = default_manifest_version();

    manifest.save()?;

    // Sync the nix profile: install all packages from the manifest.
    let mut installed = 0u32;
    for entry in &manifest.packages {
        let installable = manifest.resolve_installable(entry);
        tracing::info!("Installing {installable}...");
        match NixCommand::new(&["profile", "install"])
            .arg("--profile")
            .arg(&profile)
            .arg(&installable)
            .stream()
        {
            Ok(_) => installed += 1,
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
// Helpers
// ---------------------------------------------------------------------------

fn home_dir() -> color_eyre::Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .map_err(|_| color_eyre::eyre::eyre!("HOME environment variable not set"))
}

fn dirs_path() -> color_eyre::Result<std::path::PathBuf> {
    Ok(home_dir()?.join(".config/ekaos"))
}

fn default_manifest_version() -> u32 {
    1
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildOutput {
    outputs: std::collections::HashMap<String, String>,
}
