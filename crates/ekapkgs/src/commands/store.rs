use std::process::{Command, Stdio};

use ekapkgs_nix::NixCommand;

use crate::cli::StoreCommand;

pub fn execute(command: StoreCommand) -> color_eyre::Result<()> {
    match command {
        StoreCommand::Gc { older_than, dry_run } => cmd_gc(older_than.as_deref(), dry_run),
        StoreCommand::Optimize => cmd_optimize(),
        StoreCommand::Verify {
            all,
            repair,
            sigs_needed,
        } => cmd_verify(all, repair, sigs_needed),
    }
}

fn cmd_gc(older_than: Option<&str>, dry_run: bool) -> color_eyre::Result<()> {
    // Use nix-collect-garbage because it supports --delete-older-than
    // (nix store gc does not).
    let mut cmd = Command::new("nix-collect-garbage");

    if let Some(duration) = older_than {
        cmd.arg("--delete-older-than").arg(duration);
    } else {
        cmd.arg("-d");
    }

    if dry_run {
        cmd.arg("--dry-run");
    }

    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to run nix-collect-garbage: {e}"))?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

fn cmd_optimize() -> color_eyre::Result<()> {
    tracing::info!("Optimizing nix store (deduplicating via hardlinks)...");
    NixCommand::new(&["store", "optimise"]).stream()?;
    tracing::info!("Store optimization complete");
    Ok(())
}

fn cmd_verify(all: bool, repair: bool, sigs_needed: Option<u32>) -> color_eyre::Result<()> {
    let mut cmd = NixCommand::new(&["store", "verify"]);

    if all {
        cmd = cmd.arg("--all");
    }

    if repair {
        cmd = cmd.arg("--repair");
    }

    if let Some(n) = sigs_needed {
        cmd = cmd.arg("--sigs-needed").arg(n.to_string());
    }

    cmd.stream()?;
    Ok(())
}
