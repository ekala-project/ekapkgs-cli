//! Putting a built system generation into effect.
//!
//! ekaos toplevels, which declare an `ekaos-switch-version`, are switched
//! in-process by the `ekaos-switch` engine. Other toplevels, e.g. stock
//! NixOS, still bring their own `bin/switch-to-configuration`, which runs
//! as a child process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use color_eyre::eyre::{WrapErr, eyre};
pub(super) use ekaos_switch::Action;

/// What to activate, and how.
pub(super) struct Request<'a> {
    /// The system profile the generation belongs to.
    pub profile: &'a str,
    /// The generation: the toplevel the profile points at.
    pub generation: &'a Path,
    pub specialisation: Option<&'a str>,
    pub action: Action,
    pub install_bootloader: bool,
}

/// How a toplevel is switched.
#[derive(Debug, PartialEq, Eq)]
enum Switcher {
    /// In-process, with the ekaos-switch engine.
    Engine,
    /// The toplevel's own `bin/switch-to-configuration`.
    Legacy,
}

/// Decide how to switch `toplevel`. A toplevel that declares a contract
/// this build does not implement is refused rather than guessed at.
fn switcher_for(toplevel: &Path) -> color_eyre::Result<Switcher> {
    match ekaos_switch::contract::check(toplevel) {
        Ok(_) => Ok(Switcher::Engine),
        Err(ekaos_switch::Error::NotSwitchable(_)) => Ok(Switcher::Legacy),
        Err(e) => Err(e.into()),
    }
}

/// The toplevel to activate: the generation, or one of its
/// specialisations.
fn target(generation: &Path, specialisation: Option<&str>) -> color_eyre::Result<PathBuf> {
    let Some(spec) = specialisation else {
        return Ok(generation.to_owned());
    };
    let path = generation.join("specialisation").join(spec);
    if !path.exists() {
        return Err(eyre!(
            "Specialisation '{spec}' not found at {}",
            path.display()
        ));
    }
    Ok(path)
}

/// Whether `action` makes the generation the profile's current one.
/// `test` and `dry-activate` leave the profile, and so the boot default,
/// alone.
fn sets_profile(action: Action) -> bool {
    matches!(action, Action::Switch | Action::Boot)
}

/// Get ready for a command that activates a generation. Call it first,
/// before building anything or starting any thread.
///
/// Fails when not running as root, so nothing is built in vain. Blocks
/// SIGHUP, so a switch keeps going when its terminal goes away, e.g. an
/// SSH session dropping mid-switch: stopping halfway would leave units
/// stopped. Threads started later and child processes inherit the mask.
pub(super) fn prepare(what: &str) -> color_eyre::Result<()> {
    if !nix::unistd::geteuid().is_root() {
        return Err(eyre!(
            "`ekapkgs system {what}` needs root; run it with sudo"
        ));
    }
    nix::sys::signal::SigSet::from(nix::sys::signal::Signal::SIGHUP)
        .thread_block()
        .wrap_err("blocking SIGHUP")
}

/// Activate a generation. On failure the profile and the running system
/// are put back to what they were before.
pub(super) fn apply(req: &Request<'_>) -> color_eyre::Result<()> {
    let toplevel = target(req.generation, req.specialisation)?;
    // Refuse an unknown contract before anything changes.
    let switcher = switcher_for(&toplevel)?;

    let before = Before {
        // Resolved through the generation link to the store path
        profile: std::fs::canonicalize(req.profile).ok(),
        system: std::fs::canonicalize(CURRENT_SYSTEM).ok(),
    };
    if sets_profile(req.action) {
        set_profile(req.profile, req.generation)?;
    }

    let result = run(&switcher, &toplevel, req.action, req.install_bootloader);
    if let Err(e) = &result {
        tracing::error!("{e}");
        // Another switch holds the lock and nothing was activated, so
        // only the profile needs restoring.
        let reactivate = !matches!(
            e.downcast_ref::<ekaos_switch::Error>(),
            Some(ekaos_switch::Error::Locked)
        );
        roll_back(req, &before, reactivate);
    }
    result
}

const CURRENT_SYSTEM: &str = "/run/current-system";

/// The state an activation started from.
struct Before {
    /// What the profile pointed at.
    profile: Option<PathBuf>,
    /// The running system, which differs from the profile after a `test`.
    system: Option<PathBuf>,
}

/// What a failed `action` has to put back into effect: the boot default
/// for `boot`, the running system otherwise.
fn rollback_target(action: Action, before: &Before) -> Option<&Path> {
    match action {
        Action::DryActivate => None,
        Action::Boot => before.profile.as_deref(),
        Action::Switch | Action::Test => before.system.as_deref(),
    }
}

/// Put back what was in effect before a failed activation.
fn roll_back(req: &Request<'_>, before: &Before, reactivate: bool) {
    if sets_profile(req.action) {
        if let Some(previous) = before.profile.as_deref().filter(|p| *p != req.generation) {
            if let Err(e) = set_profile(req.profile, previous) {
                tracing::error!(
                    "{e:#}; restore the profile with: nix-env --profile {} --set {}",
                    req.profile,
                    previous.display()
                );
            }
        }
    }
    if !reactivate {
        return;
    }
    let Some(previous) = rollback_target(req.action, before) else {
        return;
    };

    tracing::warn!(
        "Rolling back to the previous configuration ({})...",
        previous.display()
    );
    let rolled_back =
        switcher_for(previous).and_then(|switcher| run(&switcher, previous, req.action, false));
    match rolled_back {
        Ok(()) => tracing::info!("Rolled back to the previous configuration"),
        Err(e) => tracing::error!(
            "Rolling back also failed: {e:#}; the system may be in an inconsistent state"
        ),
    }
}

fn run(
    switcher: &Switcher,
    toplevel: &Path,
    action: Action,
    install_bootloader: bool,
) -> color_eyre::Result<()> {
    tracing::info!("Activating ({})...", action.as_str());
    match switcher {
        Switcher::Engine => run_engine(toplevel, action, install_bootloader),
        Switcher::Legacy => run_legacy(toplevel, action, install_bootloader),
    }
}

fn run_engine(toplevel: &Path, action: Action, install_bootloader: bool) -> color_eyre::Result<()> {
    let outcome = ekaos_switch::switch(&ekaos_switch::Options {
        toplevel: toplevel.to_owned(),
        action,
        install_bootloader,
        ..Default::default()
    })?;

    if outcome.reboot_required {
        tracing::warn!(
            "This configuration changes the init system; reboot to finish switching to it"
        );
    }
    if !outcome.new_units.is_empty() {
        tracing::info!("New units: {}", outcome.new_units.join(", "));
    }
    let mut problems = Vec::new();
    if outcome.activation_failed {
        problems.push("the activation script failed".to_owned());
    }
    if !outcome.failed_jobs.is_empty() {
        let jobs: Vec<String> = outcome
            .failed_jobs
            .iter()
            .map(|j| format!("{} {}", j.kind, j.unit))
            .collect();
        problems.push(format!("jobs failed: {}", jobs.join(", ")));
    }
    if !outcome.failed_units.is_empty() {
        problems.push(format!("units failed: {}", outcome.failed_units.join(", ")));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(eyre!("Activation failed: {}", problems.join("; ")))
    }
}

fn run_legacy(toplevel: &Path, action: Action, install_bootloader: bool) -> color_eyre::Result<()> {
    let mut cmd = Command::new(toplevel.join("bin/switch-to-configuration"));
    cmd.arg(action.as_str());
    if install_bootloader {
        cmd.env("NIXOS_INSTALL_BOOTLOADER", "1");
    }
    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .wrap_err("running switch-to-configuration")?;
    if status.success() {
        Ok(())
    } else {
        Err(eyre!(
            "Activation failed (exit {})",
            status.code().unwrap_or(1)
        ))
    }
}

/// Point `profile` at `generation`, adding a profile generation.
pub(super) fn set_profile(profile: &str, generation: &Path) -> color_eyre::Result<()> {
    if let Some(dir) = Path::new(profile).parent() {
        std::fs::create_dir_all(dir).wrap_err_with(|| format!("creating {}", dir.display()))?;
    }
    tracing::info!("Setting system profile...");
    let status = Command::new("nix-env")
        .arg("--profile")
        .arg(profile)
        .arg("--set")
        .arg(generation)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .wrap_err("running nix-env")?;
    if status.success() {
        Ok(())
    } else {
        Err(eyre!(
            "Failed to set system profile (exit {})",
            status.code().unwrap_or(1)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_switch_and_boot_set_the_profile() {
        assert!(sets_profile(Action::Switch));
        assert!(sets_profile(Action::Boot));
        assert!(!sets_profile(Action::Test));
        assert!(!sets_profile(Action::DryActivate));
    }

    #[test]
    fn rollback_restores_what_the_action_changed() {
        let before = Before {
            profile: Some(PathBuf::from("/nix/store/profile")),
            system: Some(PathBuf::from("/nix/store/running")),
        };
        let profile = Some(Path::new("/nix/store/profile"));
        let running = Some(Path::new("/nix/store/running"));
        assert_eq!(rollback_target(Action::Boot, &before), profile);
        assert_eq!(rollback_target(Action::Switch, &before), running);
        assert_eq!(rollback_target(Action::Test, &before), running);
        assert_eq!(rollback_target(Action::DryActivate, &before), None);
    }

    #[test]
    fn ekaos_toplevels_use_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ekaos-switch-version"), "1\n").unwrap();
        assert_eq!(switcher_for(dir.path()).unwrap(), Switcher::Engine);
    }

    #[test]
    fn toplevels_without_a_contract_use_their_own_switcher() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(switcher_for(dir.path()).unwrap(), Switcher::Legacy);
    }

    #[test]
    fn unknown_contracts_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ekaos-switch-version"), "99\n").unwrap();
        let err = switcher_for(dir.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("unsupported ekaos switch contract")
        );
    }

    #[test]
    fn specialisations_resolve_under_the_generation() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(target(dir.path(), None).unwrap(), dir.path());
        assert!(target(dir.path(), Some("gpu")).is_err());
        std::fs::create_dir_all(dir.path().join("specialisation/gpu")).unwrap();
        assert_eq!(
            target(dir.path(), Some("gpu")).unwrap(),
            dir.path().join("specialisation/gpu")
        );
    }
}
