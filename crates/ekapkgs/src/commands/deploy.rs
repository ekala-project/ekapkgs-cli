use std::process::{Command, Stdio};

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, NixError, eval, store};

use crate::cli::ActivationMode;
use crate::config::ClientConfig;

pub fn execute(
    installable: &str,
    target_host: &str,
    build_host: Option<&str>,
    mode: &ActivationMode,
    dry_run: bool,
) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    let mode_flag = match mode {
        ActivationMode::Switch => "switch",
        ActivationMode::Boot => "boot",
        ActivationMode::Test => "test",
    };

    // Phase 1: Build
    tracing::info!("Building {installable}...");

    if config.primary_cache().is_some() {
        let spinner = ekapkgs_ui::progress::spinner("Evaluating closure...");
        let closure_paths = eval::derivation_closure_paths(&inst)?;
        spinner.finish_and_clear();

        crate::prefetch::prefetch_closure(&config, &closure_paths)?;
    }

    let system_path = if build_host.is_some() {
        // Remote build — use nixos-rebuild for the full lifecycle.
        return deploy_via_nixos_rebuild(installable, target_host, build_host, mode_flag, dry_run);
    } else {
        // Local build.
        let outputs: Vec<BuildOutput> = NixCommand::new(&["build"])
            .arg(installable)
            .arg("--json")
            .json()?;

        let path = outputs
            .first()
            .and_then(|o| o.outputs.get("out").cloned())
            .ok_or_else(|| color_eyre::eyre::eyre!("build produced no output"))?;

        tracing::info!("Built {}", path);
        path
    };

    if dry_run {
        // Show what would happen.
        let spinner = ekapkgs_ui::progress::spinner("Analyzing closure...");
        let closure_paths = eval::derivation_closure_paths(&inst)?;
        spinner.finish_and_clear();

        let total_size: u64 = store::closure_path_info(&inst)
            .map(|entries| entries.iter().map(|e| e.nar_size).sum())
            .unwrap_or(0);

        println!("Dry run:");
        println!("  System path:  {system_path}");
        println!("  Target:       {target_host}");
        println!("  Mode:         {mode_flag}");
        println!("  Closure:      {} paths", closure_paths.len());
        println!(
            "  Total size:   {}",
            ekapkgs_ui::format::format_bytes(total_size)
        );
        return Ok(());
    }

    // Phase 2: Transfer closure to target via SSH.
    tracing::info!("Copying closure to {target_host}...");
    match NixCommand::new(&["copy"])
        .arg("--to")
        .arg(format!("ssh-ng://{target_host}"))
        .arg(&system_path)
        .stream()
    {
        Ok(_) => {},
        Err(NixError::Failed { status, .. }) => {
            return Err(color_eyre::eyre::eyre!(
                "nix copy failed with exit code {}",
                status.code().unwrap_or(1)
            ));
        },
        Err(e) => return Err(e.into()),
    }

    // Phase 3: Activate on target.
    tracing::info!("Activating on {target_host} ({mode_flag})...");

    let activate_cmd = format!("{system_path}/bin/switch-to-configuration {mode_flag}");
    let status = Command::new("ssh")
        .arg(target_host)
        .arg("sudo")
        .arg(&activate_cmd)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to ssh to {target_host}: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Activation failed on {target_host} (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    tracing::info!("Deploy complete");
    Ok(())
}

/// Deploy using nixos-rebuild, which handles build-host and target-host natively.
fn deploy_via_nixos_rebuild(
    installable: &str,
    target_host: &str,
    build_host: Option<&str>,
    mode_flag: &str,
    dry_run: bool,
) -> color_eyre::Result<()> {
    let mut cmd = Command::new("nixos-rebuild");
    cmd.arg(mode_flag);
    cmd.arg("--flake").arg(installable);
    cmd.arg("--target-host").arg(target_host);

    if let Some(bh) = build_host {
        cmd.arg("--build-host").arg(bh);
    }

    if dry_run {
        cmd.arg("--dry-run");
    }

    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to run nixos-rebuild: {e}"))?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    tracing::info!("Deploy complete");
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildOutput {
    outputs: std::collections::HashMap<String, String>,
}
