//! Debounced background apply via systemd transient timers.
//!
//! After package mutations, schedules a `home switch` with a 5-second delay.
//! Rapid-fire adds coalesce into a single build. If an apply is already
//! running, a flag is set so it re-schedules after completion.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn cache_dir() -> color_eyre::Result<PathBuf> {
    let dir = directories::ProjectDirs::from("", "", "ekapkgs")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache/ekapkgs")
        });
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Schedule a debounced background `home switch` via a systemd transient timer.
///
/// If a previous timer is pending, it is cancelled and replaced. If an apply
/// is currently running, an `apply-pending` flag is written so it re-runs
/// after completion.
pub fn schedule_home_apply() -> color_eyre::Result<()> {
    // Cancel any pending (not yet started) apply timer.
    let _ = Command::new("systemctl")
        .args(["--user", "stop", "ekapkgs-home-apply.timer"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    // Check if an apply is currently running.
    let active = Command::new("systemctl")
        .args(["--user", "is-active", "ekapkgs-home-apply.service"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());

    if active {
        // An apply is running. Mark that we need a re-run after it finishes.
        let flag = cache_dir()?.join("apply-pending");
        std::fs::write(&flag, "")?;
        tracing::debug!("Apply already running, set apply-pending flag");
        return Ok(());
    }

    // Schedule a new apply in 5 seconds.
    let status = Command::new("systemd-run")
        .args([
            "--user",
            "--collect",
            "--unit=ekapkgs-home-apply",
            "--on-active=5s",
            "--timer-property=AccuracySec=1s",
        ])
        .arg("ekapkgs")
        .args(["home", "switch"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {
            tracing::debug!("Scheduled background home apply in 5s");
        },
        Ok(_) | Err(_) => {
            // systemd-run not available (e.g., not on a systemd system).
            // Silently skip — the apply will happen on next manual switch.
            tracing::debug!("Could not schedule background apply (no systemd?)");
        },
    }

    Ok(())
}

/// Check for and clear the `apply-pending` flag. Called after `home apply`
/// completes to determine if another apply should be scheduled.
pub fn check_and_clear_pending() -> color_eyre::Result<bool> {
    let flag = cache_dir()?.join("apply-pending");
    if flag.exists() {
        std::fs::remove_file(&flag)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
