//! The switch itself, in switch-to-configuration-ng's order.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::fstab::Fstab;
use crate::manager::{JobKind, Manager};
use crate::plan::{Journal, List, Planner, read_list, remove_if_exists};
use crate::{Action, Error, Options, Outcome};

const SYSINIT_REACTIVATION_TARGET: &str = "sysinit-reactivation.target";

/// Where the running system keeps its state. Everything is relative to
/// `root` so the engine can be exercised against a fake tree.
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self { root: "/".into() }
    }
}

impl Paths {
    pub fn run_dir(&self) -> PathBuf {
        self.root.join("run/ekaos")
    }

    pub fn unit_dir(&self) -> PathBuf {
        self.root.join("etc/systemd/system")
    }

    fn fstab(&self) -> PathBuf {
        self.root.join("etc/fstab")
    }

    fn system_conf(&self) -> PathBuf {
        self.root.join("etc/systemd/system.conf")
    }

    fn current_system(&self) -> PathBuf {
        self.root.join("run/current-system")
    }

    fn pid1(&self) -> PathBuf {
        self.root.join("proc/1/exe")
    }

    fn activation_list(&self, kind: &str) -> PathBuf {
        self.run_dir().join(format!("activation-{kind}-list"))
    }
}

/// Run `cmd`, reporting a spawn failure or non-zero exit as `what`.
fn run(cmd: &mut Command, what: &'static str) -> Result<(), Error> {
    let status = cmd
        .status()
        .map_err(|e| Error::io(format!("running {what}"), e))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Command { what, status })
    }
}

fn sorted(units: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    let mut units: Vec<String> = units.into_iter().map(|u| u.as_ref().to_owned()).collect();
    units.sort_by_key(|u| u.to_lowercase());
    units.join(", ")
}

/// Print a unit list when it is not empty.
fn report(prefix: &str, units: impl IntoIterator<Item = impl AsRef<str>>) {
    let units = sorted(units);
    if !units.is_empty() {
        say!("{prefix}: {units}");
    }
}

/// Install the boot loader and flush the store, for switch and boot.
pub(crate) fn install_bootloader(opts: &Options) -> Result<(), Error> {
    let mut cmd = Command::new(opts.toplevel.join("bin/install-bootloader"));
    cmd.arg(&opts.toplevel);
    if opts.install_bootloader {
        cmd.env("EKAOS_INSTALL_BOOTLOADER", "1");
    }
    run(&mut cmd, "the boot loader installer")?;

    // In case the new generation hangs the machine.
    if !opts.no_sync {
        let store =
            std::fs::File::open("/nix/store").map_err(|e| Error::io("opening /nix/store", e))?;
        nix::unistd::syncfs(std::os::fd::AsRawFd::as_raw_fd(&store))
            .map_err(|e| Error::io("syncing /nix/store", e.into()))?;
    }
    Ok(())
}

/// Queue `kind` jobs for `units`, recording the ones systemd refuses.
fn submit_all(
    manager: &mut dyn Manager,
    kind: JobKind,
    units: &BTreeSet<String>,
    outcome: &mut Outcome,
) {
    let mut ordered: Vec<&String> = units.iter().collect();
    ordered.sort_by_key(|u| u.to_lowercase());
    for unit in ordered {
        if let Err(e) = manager.submit(kind, unit) {
            say!("failed to {kind} {unit}: {e}");
            outcome.failed_jobs.push(crate::FailedJob {
                unit: unit.clone(),
                kind,
                result: e.to_string(),
            });
        }
    }
}

/// Whether pid 1 has to re-execute into the new systemd.
fn systemd_changed(paths: &Paths, toplevel: &Path) -> bool {
    let canonical = |p: PathBuf| p.canonicalize().ok();
    let pid1_changed =
        canonical(paths.pid1()) != canonical(toplevel.join("systemd/lib/systemd/systemd"));
    // /etc is copied from the toplevel, so compare contents.
    let conf_changed = std::fs::read(paths.system_conf()).ok()
        != std::fs::read(toplevel.join("etc/systemd/system.conf")).ok();
    pid1_changed || conf_changed
}

/// Switch the running system. `opts.action` is `Switch`, `Test` or
/// `DryActivate`; the boot loader has already been handled.
pub(crate) fn activate(
    opts: &Options,
    paths: &Paths,
    manager: &mut dyn Manager,
) -> Result<Outcome, Error> {
    let toplevel = &opts.toplevel;
    let dry = opts.action == Action::DryActivate;
    let mut outcome = Outcome::default();

    let current_version =
        std::fs::read_to_string(paths.current_system().join("init-interface-version"))
            .unwrap_or_default();
    let new_version = std::fs::read_to_string(toplevel.join("init-interface-version"))
        .map_err(|e| Error::io("reading init-interface-version", e))?;
    if current_version != new_version {
        say!(
            "warning: the new configuration has an init that is incompatible with the running \
             one; it will take effect after a reboot"
        );
        outcome.reboot_required = true;
        return Ok(outcome);
    }

    let run_dir = paths.run_dir();
    if !dry {
        std::fs::create_dir_all(&run_dir)
            .map_err(|e| Error::io(format!("creating {}", run_dir.display()), e))?;
    }
    let journal = Journal::new(&run_dir, dry);

    let active = manager.active_units()?;
    let mut planner = Planner::new(
        paths.unit_dir(),
        toplevel.join("etc/systemd/system"),
        &active,
        &journal,
    );
    planner.show_all_units = opts.show_all_units;
    planner.classify_active()?;

    let current_fstab = Fstab::parse(&std::fs::read_to_string(paths.fstab()).unwrap_or_default());
    let new_fstab =
        Fstab::parse(&std::fs::read_to_string(toplevel.join("etc/fstab")).unwrap_or_default());
    planner.diff_fstab(&current_fstab, &new_fstab);

    let restart_systemd = systemd_changed(paths, toplevel);

    if dry {
        let plan = planner.into_plan();
        report("would stop the following units", plan.shown(&plan.stop));
        report("would NOT stop the following changed units", &plan.skip);
        for device in &plan.swapoff {
            say!("would stop swap device: {device}");
        }
        say!("would activate the configuration...");
        if restart_systemd {
            say!("would restart systemd");
        }
        report("would reload the following units", &plan.reload);
        report("would restart the following units", &plan.restart);
        report("would start the following units", plan.shown(&plan.start));
        return Ok(outcome);
    }

    tracing::info!("switching to system configuration {}", toplevel.display());

    for device in &planner.plan.swapoff {
        say!("stopping swap device: {device}");
        let swapoff = toplevel.join("sw/bin/swapoff");
        if let Err(e) = run(Command::new(swapoff).arg(device), "swapoff") {
            say!("failed to stop swapping to {device}: {e}");
        }
    }

    if !planner.plan.stop.is_empty() {
        report(
            "stopping the following units",
            planner.plan.shown(&planner.plan.stop),
        );
        for unit in planner.plan.stop.clone() {
            // A unit that is already gone needs no stopping.
            if let Err(e) = manager.submit(JobKind::Stop, &unit) {
                tracing::debug!("stopping {unit}: {e}");
            }
        }
        manager.wait_jobs()?;
    }
    report(
        "NOT restarting the following changed units",
        &planner.plan.skip,
    );

    say!("activating the configuration...");
    let activate = toplevel.join("activate");
    match Command::new(&activate)
        .arg(opts.action.as_str())
        .env("EKAOS_ACTION", opts.action.as_str())
        .status()
    {
        Ok(status) if status.success() => {},
        // A toplevel may have no activation script.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Ok(status) => {
            say!("the activation script failed ({status})");
            outcome.activation_failed = true;
        },
        Err(e) => {
            say!("failed to run the activation script: {e}");
            outcome.activation_failed = true;
        },
    }

    // The activation script can ask for units to be restarted or reloaded.
    let restart_requests = read_list(&paths.activation_list("restart"));
    planner.activation_restarts(&restart_requests)?;
    remove_if_exists(&paths.activation_list("restart"))?;
    let reload_requests = read_list(&paths.activation_list("reload"));
    planner.activation_reloads(&reload_requests);
    remove_if_exists(&paths.activation_list("reload"))?;

    // Re-execute with the running systemd first, in case the new one has
    // trouble talking to it.
    if restart_systemd {
        say!("restarting systemd...");
        manager.reexecute()?;
    }
    manager.reset_failed()?;
    manager.reload()?;

    // Activation already ran daemon-reload for user managers; re-execute
    // them so they pick up a new systemd too.
    let systemctl = toplevel.join("systemd/bin/systemctl");
    match manager.users() {
        Ok(users) => {
            for user in users {
                say!("reloading user units for {}...", user.name);
                let machine = format!("{}@", user.uid);
                if let Err(e) = run(
                    Command::new(&systemctl).args(["--user", "-M", &machine, "daemon-reexec"]),
                    "systemctl --user daemon-reexec",
                ) {
                    say!("failed to reload user units for {}: {e}", user.name);
                }
            }
        },
        Err(e) => say!("unable to list users with logind: {e}"),
    }

    // Re-run units ordered before sysinit.target, in order. Restarting
    // sysinit.target itself would restart everything.
    say!("restarting {SYSINIT_REACTIVATION_TARGET}");
    match manager.submit(JobKind::Restart, SYSINIT_REACTIVATION_TARGET) {
        Ok(()) => manager.wait_jobs()?,
        Err(e) => {
            say!("failed to restart {SYSINIT_REACTIVATION_TARGET}: {e}");
            outcome.failed_jobs.push(crate::FailedJob {
                unit: SYSINIT_REACTIVATION_TARGET.to_owned(),
                kind: JobKind::Restart,
                result: e.to_string(),
            });
        },
    }

    // A unit to be reloaded may have been stopped with a dependency. If
    // so, start it instead, unless it only starts manually.
    for unit in planner.plan.reload.clone() {
        if !manager.is_active(&unit)? {
            if planner.startable(&unit) {
                planner.plan.start.insert(unit.clone());
                journal.record(List::Start, &unit);
            }
            planner.plan.reload.remove(&unit);
            journal.unrecord(List::Reload, &unit);
        }
    }

    let plan = planner.into_plan();

    if !plan.reload.is_empty() {
        report("reloading the following units", &plan.reload);
        submit_all(manager, JobKind::Reload, &plan.reload, &mut outcome);
        manager.wait_jobs()?;
        journal.clear(List::Reload)?;
    }

    if !plan.restart.is_empty() {
        report("restarting the following units", &plan.restart);
        submit_all(manager, JobKind::Restart, &plan.restart, &mut outcome);
        manager.wait_jobs()?;
        journal.clear(List::Restart)?;
    }

    // Start the active targets and the changed units stopped above, which
    // targets may not pull in if they were started by hand.
    report("starting the following units", plan.shown(&plan.start));
    submit_all(manager, JobKind::Start, &plan.start, &mut outcome);
    manager.wait_jobs()?;
    journal.clear(List::Start)?;

    for job in manager.failed_jobs() {
        say!("failed to {} {}", job.kind, job.unit);
        outcome.failed_jobs.push(job);
    }

    // Failures are only seen once systemd has had a moment, so wait for the
    // bus to go quiet first. This is a race by nature.
    manager.settle();
    for (unit, state) in manager.active_units()? {
        if state.state == "failed" {
            outcome.failed_units.push(unit);
            continue;
        }
        // auto-restart is a failure if the last run failed.
        if state.substate == "auto-restart"
            && unit.ends_with(".service")
            && manager.exec_main_status(&unit)? != 0
        {
            outcome.failed_units.push(unit);
            continue;
        }
        // Scopes are created by other programs, not by the configuration.
        if !active.contains_key(&unit) && !unit.ends_with(".scope") {
            outcome.new_units.push(unit);
        }
    }
    outcome.failed_units.sort_by_key(|u| u.to_lowercase());
    outcome.new_units.sort_by_key(|u| u.to_lowercase());

    report("the following new units were started", &outcome.new_units);
    report("warning: the following units failed", &outcome.failed_units);

    if outcome.exit_code() == 0 {
        tracing::info!(
            "finished switching to system configuration {}",
            toplevel.display()
        );
    } else {
        tracing::error!(
            "switching to system configuration {} failed (status {})",
            toplevel.display(),
            outcome.exit_code()
        );
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::manager::{FailedJob, User};
    use crate::plan::UnitState;

    /// Records calls and replays a scripted unit state.
    #[derive(Default)]
    struct FakeManager {
        units: BTreeMap<String, UnitState>,
        after: Option<BTreeMap<String, UnitState>>,
        inactive: BTreeSet<String>,
        calls: Vec<String>,
        failing_jobs: Vec<FailedJob>,
        listed: usize,
    }

    impl Manager for FakeManager {
        fn active_units(&mut self) -> Result<BTreeMap<String, UnitState>, Error> {
            self.listed += 1;
            Ok(match (&self.after, self.listed) {
                (Some(after), n) if n > 1 => after.clone(),
                _ => self.units.clone(),
            })
        }

        fn is_active(&mut self, unit: &str) -> Result<bool, Error> {
            Ok(!self.inactive.contains(unit))
        }

        fn exec_main_status(&mut self, _unit: &str) -> Result<i32, Error> {
            Ok(1)
        }

        fn submit(&mut self, kind: JobKind, unit: &str) -> Result<(), Error> {
            self.calls.push(format!("{kind} {unit}"));
            Ok(())
        }

        fn wait_jobs(&mut self) -> Result<(), Error> {
            self.calls.push("wait".to_owned());
            Ok(())
        }

        fn failed_jobs(&mut self) -> Vec<FailedJob> {
            std::mem::take(&mut self.failing_jobs)
        }

        fn reexecute(&mut self) -> Result<(), Error> {
            self.calls.push("reexecute".to_owned());
            Ok(())
        }

        fn reload(&mut self) -> Result<(), Error> {
            self.calls.push("reload".to_owned());
            Ok(())
        }

        fn reset_failed(&mut self) -> Result<(), Error> {
            self.calls.push("reset-failed".to_owned());
            Ok(())
        }

        fn settle(&mut self) {}

        fn users(&mut self) -> Result<Vec<User>, Error> {
            Ok(Vec::new())
        }
    }

    struct Tree {
        _dir: tempfile::TempDir,
        paths: Paths,
        toplevel: PathBuf,
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn tree() -> Tree {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let toplevel = dir.path().join("toplevel");
        write(
            &root.join("run/current-system/init-interface-version"),
            "systemd 1",
        );
        write(&toplevel.join("init-interface-version"), "systemd 1");
        write(&root.join("etc/systemd/system.conf"), "[Manager]\n");
        write(&toplevel.join("etc/systemd/system.conf"), "[Manager]\n");
        // pid 1 and the new systemd are the same binary.
        write(&toplevel.join("systemd/lib/systemd/systemd"), "");
        std::fs::create_dir_all(root.join("proc/1")).unwrap();
        std::os::unix::fs::symlink(
            toplevel.join("systemd/lib/systemd/systemd"),
            root.join("proc/1/exe"),
        )
        .unwrap();
        Tree {
            _dir: dir,
            paths: Paths { root },
            toplevel,
        }
    }

    fn options(tree: &Tree, action: Action) -> Options {
        Options {
            toplevel: tree.toplevel.clone(),
            action,
            ..Options::default()
        }
    }

    fn active(tree: &Tree, unit: &str) -> (String, UnitState) {
        (
            unit.to_owned(),
            UnitState {
                state: "active".to_owned(),
                substate: "running".to_owned(),
                fragment_path: tree.paths.unit_dir().join(unit).display().to_string(),
            },
        )
    }

    #[test]
    fn incompatible_init_requires_reboot() {
        let tree = tree();
        write(&tree.toplevel.join("init-interface-version"), "systemd 2");
        let mut manager = FakeManager::default();
        let outcome = activate(&options(&tree, Action::Switch), &tree.paths, &mut manager).unwrap();
        assert!(outcome.reboot_required);
        assert_eq!(outcome.exit_code(), 100);
        assert!(manager.calls.is_empty());
    }

    #[test]
    fn switch_runs_jobs_in_order() {
        let tree = tree();
        let units = tree.paths.unit_dir();
        let new_units = tree.toplevel.join("etc/systemd/system");
        write(&units.join("a.service"), "[Service]\nExecStart=/a\n");
        write(&new_units.join("a.service"), "[Service]\nExecStart=/b\n");
        write(
            &units.join("r.service"),
            "[Service]\nExecStart=/a\nX-StopIfChanged=false\n",
        );
        write(
            &new_units.join("r.service"),
            "[Service]\nExecStart=/b\nX-StopIfChanged=false\n",
        );
        // The activation script records what it ran and requests a reload.
        let log = tree.paths.root.join("activate.log");
        write(
            &tree.toplevel.join("activate"),
            &format!(
                "#!/bin/sh\necho \"$1 $EKAOS_ACTION\" > {}\necho r2.service > {}\n",
                log.display(),
                tree.paths
                    .run_dir()
                    .join("activation-reload-list")
                    .display()
            ),
        );
        std::fs::set_permissions(
            tree.toplevel.join("activate"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        let mut manager = FakeManager {
            units: [
                active(&tree, "a.service"),
                active(&tree, "r.service"),
                active(&tree, "r2.service"),
            ]
            .into(),
            ..FakeManager::default()
        };
        let mut after = manager.units.clone();
        after.insert("new.service".to_owned(), active(&tree, "new.service").1);
        let mut failed = active(&tree, "broken.service").1;
        failed.state = "failed".to_owned();
        after.insert("broken.service".to_owned(), failed);
        manager.after = Some(after);

        let outcome = activate(&options(&tree, Action::Switch), &tree.paths, &mut manager).unwrap();

        assert_eq!(std::fs::read_to_string(&log).unwrap(), "switch switch\n");
        assert_eq!(
            manager.calls,
            [
                "stop a.service",
                "wait",
                "reset-failed",
                "reload",
                "restart sysinit-reactivation.target",
                "wait",
                "reload r2.service",
                "wait",
                "restart r.service",
                "wait",
                "start a.service",
                "wait",
            ]
        );
        assert_eq!(outcome.new_units, ["new.service"]);
        assert_eq!(outcome.failed_units, ["broken.service"]);
        assert_eq!(outcome.exit_code(), 4);
        // Every list was consumed.
        for list in ["start", "restart", "reload", "activation-reload"] {
            assert!(!tree.paths.run_dir().join(format!("{list}-list")).exists());
        }
    }

    #[test]
    fn changed_systemd_is_reexecuted() {
        let tree = tree();
        write(
            &tree.toplevel.join("etc/systemd/system.conf"),
            "[Manager]\nDefaultTimeoutStopSec=10\n",
        );
        let mut manager = FakeManager::default();
        let outcome = activate(&options(&tree, Action::Test), &tree.paths, &mut manager).unwrap();
        assert_eq!(outcome.exit_code(), 0);
        assert_eq!(manager.calls[..3], ["reexecute", "reset-failed", "reload"]);
    }

    #[test]
    fn inactive_reloads_become_starts() {
        let tree = tree();
        write(&tree.paths.run_dir().join("reload-list"), "gone.service\n");
        write(
            &tree.toplevel.join("etc/systemd/system/gone.service"),
            "[Service]\nExecStart=/a\n",
        );
        let mut manager = FakeManager {
            inactive: ["gone.service".to_owned()].into(),
            ..FakeManager::default()
        };
        activate(&options(&tree, Action::Test), &tree.paths, &mut manager).unwrap();
        assert!(manager.calls.contains(&"start gone.service".to_owned()));
        assert!(!manager.calls.iter().any(|c| c == "reload gone.service"));
    }

    #[test]
    fn failed_activation_and_jobs_are_reported() {
        let tree = tree();
        write(&tree.toplevel.join("activate"), "#!/bin/sh\nexit 1\n");
        std::fs::set_permissions(
            tree.toplevel.join("activate"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let mut manager = FakeManager::default();
        let outcome = activate(&options(&tree, Action::Test), &tree.paths, &mut manager).unwrap();
        assert!(outcome.activation_failed);
        assert_eq!(outcome.exit_code(), 2);

        let mut manager = FakeManager {
            failing_jobs: vec![FailedJob {
                unit: "x.service".to_owned(),
                kind: JobKind::Start,
                result: "failed".to_owned(),
            }],
            ..FakeManager::default()
        };
        let outcome = activate(&options(&tree, Action::Test), &tree.paths, &mut manager).unwrap();
        assert_eq!(outcome.exit_code(), 4);
    }

    #[test]
    fn dry_activate_changes_nothing() {
        let tree = tree();
        let units = tree.paths.unit_dir();
        write(&units.join("a.service"), "[Service]\nExecStart=/a\n");
        write(
            &tree.toplevel.join("etc/systemd/system/a.service"),
            "[Service]\nExecStart=/b\n",
        );
        write(&tree.toplevel.join("activate"), "#!/bin/sh\nexit 1\n");
        let mut manager = FakeManager {
            units: [active(&tree, "a.service")].into(),
            ..FakeManager::default()
        };
        let outcome = activate(
            &options(&tree, Action::DryActivate),
            &tree.paths,
            &mut manager,
        )
        .unwrap();
        assert_eq!(outcome.exit_code(), 0);
        assert!(manager.calls.is_empty());
        assert!(!tree.paths.run_dir().exists());
    }
}
