//! Deciding which units to stop, start, restart and reload.
//!
//! This is a port of the classification in nixpkgs'
//! switch-to-configuration-ng, so units configured with the same
//! `X-*` keys behave the same way on ekaos.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::fstab::{Fstab, mount_unit_name};
use crate::unit::{self, Comparison, UnitInfo};

/// A unit loaded in the running manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitState {
    pub state: String,
    pub substate: String,
    /// Where the unit was loaded from. systemd follows the unit file's
    /// symlinks, so a managed unit may be reported at its store path.
    pub fragment_path: String,
}

/// The persistent lists of pending jobs.
///
/// Units are written down as soon as they are planned, so a switch that is
/// interrupted (for example because the engine itself was killed when its
/// session was restarted) is completed by the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum List {
    Start,
    Restart,
    Reload,
}

impl List {
    fn file_name(self) -> &'static str {
        match self {
            List::Start => "start-list",
            List::Restart => "restart-list",
            List::Reload => "reload-list",
        }
    }
}

/// Reads and writes the pending-job lists under a runtime directory.
#[derive(Debug, Clone)]
pub struct Journal {
    dir: PathBuf,
    /// Dry runs plan without writing anything down.
    dry: bool,
}

impl Journal {
    pub fn new(dir: impl Into<PathBuf>, dry: bool) -> Self {
        Self {
            dir: dir.into(),
            dry,
        }
    }

    pub fn path(&self, list: List) -> PathBuf {
        self.dir.join(list.file_name())
    }

    pub fn read(&self, list: List) -> BTreeSet<String> {
        read_list(&self.path(list))
    }

    pub fn record(&self, list: List, unit: &str) {
        if self.dry {
            return;
        }
        let file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.path(list));
        match file {
            Ok(mut f) => {
                if let Err(e) = writeln!(f, "{unit}") {
                    tracing::warn!("recording {unit} in {}: {e}", list.file_name());
                }
            },
            Err(e) => tracing::warn!("recording {unit} in {}: {e}", list.file_name()),
        }
    }

    pub fn unrecord(&self, list: List, unit: &str) {
        if self.dry {
            return;
        }
        let path = self.path(list);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return;
        };
        let kept: String = contents
            .lines()
            .filter(|line| *line != unit)
            .map(|line| format!("{line}\n"))
            .collect();
        if let Err(e) = std::fs::write(&path, kept) {
            tracing::warn!("removing {unit} from {}: {e}", list.file_name());
        }
    }

    pub fn clear(&self, list: List) -> Result<(), Error> {
        if self.dry {
            return Ok(());
        }
        remove_if_exists(&self.path(list))
    }
}

/// Unit names in a list file, one per line.
pub fn read_list(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn remove_if_exists(path: &Path) -> Result<(), Error> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(Error::io(format!("removing {}", path.display()), e))
        },
        _ => Ok(()),
    }
}

/// What the switch will do to units.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Plan {
    pub stop: BTreeSet<String>,
    pub start: BTreeSet<String>,
    pub restart: BTreeSet<String>,
    pub reload: BTreeSet<String>,
    /// Changed units that are deliberately left running.
    pub skip: BTreeSet<String>,
    /// Units that are acted on but not worth listing (active targets).
    pub filter: BTreeSet<String>,
    /// Swap devices that are no longer configured.
    pub swapoff: BTreeSet<String>,
}

impl Plan {
    /// `set` without the filtered units.
    pub fn shown<'a>(&'a self, set: &'a BTreeSet<String>) -> Vec<&'a str> {
        set.iter()
            .filter(|u| !self.filter.contains(*u))
            .map(String::as_str)
            .collect()
    }
}

/// The unit files to read for a unit.
struct UnitFiles {
    new: PathBuf,
    new_base: PathBuf,
    base_name: String,
}

/// Builds a [`Plan`] from the running units and the old and new unit files.
pub struct Planner<'a> {
    /// The system unit directory of the running generation.
    pub current_units: PathBuf,
    /// The system unit directory of the new generation.
    pub new_units: PathBuf,
    pub active: &'a BTreeMap<String, UnitState>,
    pub journal: &'a Journal,
    /// Show active targets that are started anyway.
    pub show_all_units: bool,
    pub plan: Plan,
}

impl<'a> Planner<'a> {
    pub fn new(
        current_units: PathBuf,
        new_units: PathBuf,
        active: &'a BTreeMap<String, UnitState>,
        journal: &'a Journal,
    ) -> Self {
        // Pick up the jobs of an interrupted switch.
        let plan = Plan {
            start: journal.read(List::Start),
            restart: journal.read(List::Restart),
            reload: journal.read(List::Reload),
            ..Plan::default()
        };
        Self {
            current_units,
            new_units,
            active,
            journal,
            show_all_units: false,
            plan,
        }
    }

    pub fn into_plan(self) -> Plan {
        self.plan
    }

    fn add(&mut self, list: List, unit: &str) {
        let set = match list {
            List::Start => &mut self.plan.start,
            List::Restart => &mut self.plan.restart,
            List::Reload => &mut self.plan.reload,
        };
        set.insert(unit.to_owned());
        self.journal.record(list, unit);
    }

    fn drop_reload(&mut self, unit: &str) {
        if self.plan.reload.remove(unit) {
            self.journal.unrecord(List::Reload, unit);
        }
    }

    /// Instance units without their own file are read from their
    /// template. `use_template` decides when, given whether the current
    /// and new files exist.
    fn files(&self, unit: &str, use_template: impl Fn(bool, bool) -> bool) -> (UnitFiles, PathBuf) {
        let current = self.current_units.join(unit);
        let new = self.new_units.join(unit);
        let mut base = unit.to_owned();
        let mut current_base = current.clone();
        let mut new_base = new.clone();

        if let Some(template) = unit::template_of(unit)
            && use_template(current.exists(), new.exists())
        {
            current_base = self.current_units.join(&template);
            new_base = self.new_units.join(&template);
            base = template;
        }

        (
            UnitFiles {
                new,
                new_base,
                base_name: unit::base_name(&base).to_owned(),
            },
            current_base,
        )
    }

    /// Classify every active unit that the running generation manages.
    pub fn classify_active(&mut self) -> Result<(), Error> {
        for (unit, state) in self.active {
            // A unit loaded from a directory that takes precedence over
            // the managed one is not the generation's to switch.
            if overrides_unit_dir(&state.fragment_path) {
                continue;
            }
            if !matches!(state.state.as_str(), "active" | "activating") {
                continue;
            }

            let current_file = self.current_units.join(unit);
            let (files, current_base) = self.files(unit, |cur, new| !cur && !new);
            if !current_base.exists() {
                continue;
            }

            let removed = files
                .new_base
                .canonicalize()
                .map(|p| p == Path::new("/dev/null"))
                .unwrap_or(true);

            if removed {
                let current = load(&current_file, &current_base)?;
                if current.bool("Unit", "X-StopOnRemoval", true) {
                    self.plan.stop.insert(unit.clone());
                }
            } else if unit.ends_with(".target") {
                let new = load(&files.new, &files.new_base)?;
                // Restart active targets, which starts most of the units
                // stopped below as well as new dependencies (mounts, swap).
                // The sleep targets can stay active after a resume.
                let sleep = matches!(
                    unit.as_str(),
                    "suspend.target" | "hibernate.target" | "hybrid-sleep.target"
                );
                if !(sleep
                    || new.bool("Unit", "RefuseManualStart", false)
                    || new.bool("Unit", "X-OnlyManualStart", false))
                {
                    self.add(List::Start, unit);
                    if !self.show_all_units {
                        self.plan.filter.insert(unit.clone());
                    }
                }
                // Stopping a target first keeps the ordering of units
                // around it when both sides changed.
                if new.bool("Unit", "X-StopOnReconfiguration", false) {
                    self.plan.stop.insert(unit.clone());
                }
            } else {
                let current = load(&current_file, &current_base)?;
                let new = load(&files.new, &files.new_base)?;
                match unit::compare(&current, &new) {
                    Comparison::NeedsRestart => self.handle_modified(unit, &files, Some(&new))?,
                    Comparison::NeedsReload if !self.plan.restart.contains(unit) => {
                        self.add(List::Reload, unit);
                    },
                    _ => {},
                }
            }
        }
        Ok(())
    }

    /// Diff the old and new fstab. New filesystems are mounted when
    /// local-fs.target is restarted.
    pub fn diff_fstab(&mut self, current: &Fstab, new: &Fstab) {
        for (mount_point, fs) in &current.filesystems {
            let automount = fs.options.contains("x-systemd.automount");
            let unit = mount_unit_name(mount_point, automount);
            match new.filesystems.get(mount_point) {
                Some(new_fs) if new_fs.fs_type != fs.fs_type || new_fs.device != fs.device => {
                    if matches!(mount_point.as_str(), "/" | "/nix") {
                        if new_fs.options != fs.options {
                            self.add(List::Reload, &unit);
                        } else {
                            // / and /nix can't be unmounted.
                            self.plan.skip.insert(unit);
                        }
                    } else {
                        self.add(List::Restart, &unit);
                    }
                },
                Some(new_fs) if new_fs.options != fs.options => self.add(List::Reload, &unit),
                Some(_) => {},
                None => {
                    self.plan.stop.insert(unit);
                },
            }
        }

        // Stopping the swap unit would not reliably swapoff, because of the
        // device alias units, so these are turned off directly.
        for device in current.swaps.keys() {
            if !new.swaps.contains_key(device) {
                self.plan.swapoff.insert(device.clone());
            }
        }
    }

    /// Units the activation script asked to restart.
    pub fn activation_restarts(&mut self, units: &BTreeSet<String>) -> Result<(), Error> {
        for unit in units {
            let (files, _) = self.files(unit, |_, new| !new);
            if !self.active.contains_key(unit) {
                self.add(List::Start, unit);
                continue;
            }
            self.handle_modified(unit, &files, None)?;
        }
        Ok(())
    }

    /// Units the activation script asked to reload.
    pub fn activation_reloads(&mut self, units: &BTreeSet<String>) {
        for unit in units {
            if self.active.contains_key(unit)
                && !self.plan.restart.contains(unit)
                && !self.plan.stop.contains(unit)
            {
                self.add(List::Reload, unit);
            }
        }
    }

    /// Port of `handle_modified_unit`. Without `new_info` (requests from
    /// the activation script, which runs after units were stopped),
    /// restarts are used instead of stop and start.
    fn handle_modified(
        &mut self,
        unit: &str,
        files: &UnitFiles,
        new_info: Option<&UnitInfo>,
    ) -> Result<(), Error> {
        let restart_instead = new_info.is_none();

        if matches!(
            unit,
            "sysinit.target" | "basic.target" | "multi-user.target" | "graphical.target"
        ) || unit.ends_with(".unit")
            || unit.ends_with(".slice")
            || unit.ends_with(".socket")
        {
            // These can't be restarted directly; slice properties apply on
            // daemon-reload. Changed sockets are left alone, as in NixOS.
            return Ok(());
        }

        if unit.ends_with(".mount") {
            // Only Options changes reach here as a reload; anything else
            // restarts, except / and /nix which can't be unmounted.
            if unit == "-.mount" || unit == "nix.mount" {
                self.add(List::Reload, unit);
            } else {
                self.add(List::Restart, unit);
            }
            return Ok(());
        }

        let loaded;
        let info = match new_info {
            Some(info) => info,
            None => {
                loaded = load(&files.new, &files.new_base)?;
                &loaded
            },
        };

        let pending_stop = if restart_instead {
            self.plan.restart.contains(unit)
        } else {
            self.plan.stop.contains(unit)
        };

        if info.bool("Service", "X-ReloadIfChanged", false)
            && !self.plan.restart.contains(unit)
            && !pending_stop
        {
            self.add(List::Reload, unit);
        } else if !info.bool("Service", "X-RestartIfChanged", true)
            || info.bool("Unit", "RefuseManualStop", false)
            || info.bool("Unit", "X-OnlyManualStart", false)
        {
            self.plan.skip.insert(unit.to_owned());
        } else if !info.bool("Service", "X-StopIfChanged", true) || !unit.ends_with(".service") {
            // Non-services have no ExecStop=, so stop and start is pointless.
            self.add(List::Restart, unit);
            self.drop_reload(unit);
        } else {
            // A service with an active socket is stopped together with its
            // sockets, and the sockets are started again instead of it.
            let mut sockets: Vec<String> = info
                .values("Service", "Sockets")
                .map(|v| v.join(" ").split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default();
            if sockets.is_empty() {
                sockets.push(format!("{}.socket", files.base_name));
            }

            let mut socket_activated = false;
            for socket in &sockets {
                if !self.active.contains_key(socket) {
                    continue;
                }
                if restart_instead {
                    self.plan.restart.insert(socket.clone());
                } else {
                    self.plan.stop.insert(socket.clone());
                }
                // Only start sockets the new generation still has.
                if self.new_units.join(socket).exists() {
                    if restart_instead {
                        self.add(List::Restart, socket);
                    } else {
                        self.add(List::Start, socket);
                    }
                    socket_activated = true;
                }
                self.drop_reload(unit);
            }

            if info.bool("Service", "X-NotSocketActivated", false) {
                socket_activated = false;
            }

            if !socket_activated {
                if restart_instead {
                    self.add(List::Restart, unit);
                } else {
                    self.add(List::Start, unit);
                }
            }

            if restart_instead {
                self.plan.restart.insert(unit.to_owned());
            } else {
                self.plan.stop.insert(unit.to_owned());
            }
            self.drop_reload(unit);
        }
        Ok(())
    }

    /// Whether an inactive unit that was going to be reloaded should be
    /// started instead.
    pub fn startable(&self, unit: &str) -> bool {
        let (files, _) = self.files(unit, |_, new| !new);
        match load(&files.new, &files.new_base) {
            Ok(info) => {
                !(info.bool("Unit", "RefuseManualStart", false)
                    || info.bool("Unit", "X-OnlyManualStart", false))
            },
            // No unit file (a mount from fstab, say): let systemd decide.
            Err(_) => true,
        }
    }
}

/// Unit directories that take precedence over /etc/systemd/system.
const OVERRIDING_UNIT_DIRS: &[&str] = &[
    "/etc/systemd/system.control/",
    "/run/systemd/system.control/",
    "/run/systemd/transient/",
    "/run/systemd/generator.early/",
];

/// Whether a unit was loaded from a directory that overrides the
/// generation's units.
fn overrides_unit_dir(fragment_path: &str) -> bool {
    OVERRIDING_UNIT_DIRS
        .iter()
        .any(|dir| fragment_path.starts_with(dir))
}

fn load(unit_file: &Path, base: &Path) -> Result<UnitInfo, Error> {
    UnitInfo::load(unit_file, base)
        .map_err(|e| Error::io(format!("reading unit {}", unit_file.display()), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        current: PathBuf,
        new: PathBuf,
        run: PathBuf,
        active: BTreeMap<String, UnitState>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let current = dir.path().join("etc/systemd/system");
            let new = dir.path().join("toplevel/etc/systemd/system");
            let run = dir.path().join("run/ekaos");
            for d in [&current, &new, &run] {
                std::fs::create_dir_all(d).unwrap();
            }
            Self {
                _dir: dir,
                current,
                new,
                run,
                active: BTreeMap::new(),
            }
        }

        fn current(&mut self, unit: &str, text: &str) -> &mut Self {
            write(&self.current.join(unit), text);
            self
        }

        fn new_unit(&mut self, unit: &str, text: &str) -> &mut Self {
            write(&self.new.join(unit), text);
            self
        }

        fn both(&mut self, unit: &str, old: &str, new: &str) -> &mut Self {
            self.current(unit, old).new_unit(unit, new)
        }

        fn active(&mut self, unit: &str) -> &mut Self {
            let fragment_path = self.current.join(unit).to_string_lossy().into_owned();
            self.active.insert(
                unit.to_owned(),
                UnitState {
                    state: "active".to_owned(),
                    substate: "running".to_owned(),
                    fragment_path,
                },
            );
            self
        }

        fn journal(&self) -> Journal {
            Journal::new(&self.run, false)
        }

        fn plan(&self) -> Plan {
            let journal = self.journal();
            let mut planner = Planner::new(
                self.current.clone(),
                self.new.clone(),
                &self.active,
                &journal,
            );
            planner.classify_active().unwrap();
            planner.into_plan()
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn set(units: &[&str]) -> BTreeSet<String> {
        units.iter().map(|u| (*u).to_owned()).collect()
    }

    const A: &str = "[Service]\nExecStart=/a\n";
    const B: &str = "[Service]\nExecStart=/b\n";

    #[test]
    fn unchanged_units_are_left_alone() {
        let mut f = Fixture::new();
        f.both("a.service", A, A).active("a.service");
        assert_eq!(f.plan(), Plan::default());
    }

    #[test]
    fn changed_services_are_stopped_and_started() {
        let mut f = Fixture::new();
        f.both("a.service", A, B).active("a.service");
        let plan = f.plan();
        assert_eq!(plan.stop, set(&["a.service"]));
        assert_eq!(plan.start, set(&["a.service"]));
        assert_eq!(f.journal().read(List::Start), set(&["a.service"]));
    }

    #[test]
    fn stop_if_changed_false_restarts() {
        let mut f = Fixture::new();
        f.both("a.service", A, &format!("{B}X-StopIfChanged=false\n"))
            .active("a.service");
        let plan = f.plan();
        assert!(plan.stop.is_empty());
        assert_eq!(plan.restart, set(&["a.service"]));
    }

    #[test]
    fn restart_if_changed_false_skips() {
        let mut f = Fixture::new();
        f.both("a.service", A, &format!("{B}X-RestartIfChanged=false\n"))
            .both(
                "b.service",
                A,
                &format!("[Unit]\nRefuseManualStop=true\n{B}"),
            )
            .active("a.service")
            .active("b.service");
        let plan = f.plan();
        assert_eq!(plan.skip, set(&["a.service", "b.service"]));
        assert!(plan.stop.is_empty() && plan.start.is_empty());
    }

    #[test]
    fn reload_if_changed_reloads() {
        let mut f = Fixture::new();
        f.both("dbus.service", A, &format!("{B}X-ReloadIfChanged=true\n"))
            .active("dbus.service");
        let plan = f.plan();
        assert_eq!(plan.reload, set(&["dbus.service"]));
        assert!(plan.stop.is_empty());
    }

    #[test]
    fn reload_triggers_reload() {
        let mut f = Fixture::new();
        f.both(
            "a.service",
            &format!("[Unit]\nX-Reload-Triggers=/x\n{A}"),
            &format!("[Unit]\nX-Reload-Triggers=/y\n{A}"),
        )
        .active("a.service");
        assert_eq!(f.plan().reload, set(&["a.service"]));
    }

    #[test]
    fn removed_units_are_stopped_unless_opted_out() {
        let mut f = Fixture::new();
        f.current("gone.service", A)
            .current(
                "kept.service",
                &format!("[Unit]\nX-StopOnRemoval=false\n{A}"),
            )
            .active("gone.service")
            .active("kept.service");
        std::os::unix::fs::symlink("/dev/null", f.new.join("masked.service")).unwrap();
        f.current("masked.service", A).active("masked.service");
        assert_eq!(f.plan().stop, set(&["gone.service", "masked.service"]));
    }

    #[test]
    fn units_from_overriding_directories_are_ignored() {
        for dir in ["/run/systemd/transient", "/run/systemd/generator.early"] {
            let mut f = Fixture::new();
            f.both("a.service", A, B).active("a.service");
            f.active.get_mut("a.service").unwrap().fragment_path = format!("{dir}/a.service");
            assert_eq!(f.plan(), Plan::default(), "{dir}");
        }
    }

    #[test]
    fn units_reported_at_their_store_path_are_managed() {
        // systemd reports e.g. multi-user.target at the file its
        // /etc/systemd/system link resolves to.
        let mut f = Fixture::new();
        f.both("a.service", A, B)
            .both("multi-user.target", "[Unit]\n", "[Unit]\n")
            .active("a.service")
            .active("multi-user.target");
        for unit in ["a.service", "multi-user.target"] {
            f.active.get_mut(unit).unwrap().fragment_path =
                format!("/nix/store/aaaa-systemd/example/systemd/system/{unit}");
        }
        let plan = f.plan();
        assert_eq!(plan.stop, set(&["a.service"]));
        assert_eq!(plan.start, set(&["a.service", "multi-user.target"]));
    }

    #[test]
    fn inactive_units_are_ignored() {
        let mut f = Fixture::new();
        f.both("a.service", A, B).active("a.service");
        f.active.get_mut("a.service").unwrap().state = "failed".to_owned();
        assert_eq!(f.plan(), Plan::default());
    }

    #[test]
    fn active_targets_are_started_and_filtered() {
        let mut f = Fixture::new();
        let t = "[Unit]\nDescription=t\n";
        f.both("multi-user.target", t, t)
            .both("suspend.target", t, t)
            .both("manual.target", t, "[Unit]\nX-OnlyManualStart=true\n")
            .both(
                "local-fs.target",
                t,
                "[Unit]\nX-StopOnReconfiguration=true\n",
            )
            .active("multi-user.target")
            .active("suspend.target")
            .active("manual.target")
            .active("local-fs.target");
        let plan = f.plan();
        assert_eq!(plan.start, set(&["local-fs.target", "multi-user.target"]));
        assert_eq!(plan.filter, plan.start);
        assert_eq!(plan.stop, set(&["local-fs.target"]));
        assert!(plan.shown(&plan.start).is_empty());
    }

    #[test]
    fn socket_activated_services_restart_through_their_socket() {
        let mut f = Fixture::new();
        let sock = "[Socket]\nListenStream=22\n";
        f.both("sshd.service", A, B)
            .both("sshd.socket", sock, sock)
            .active("sshd.service")
            .active("sshd.socket");
        let plan = f.plan();
        assert_eq!(plan.stop, set(&["sshd.service", "sshd.socket"]));
        assert_eq!(plan.start, set(&["sshd.socket"]));

        // Opting out starts the service as well.
        let mut f = Fixture::new();
        f.both(
            "sshd.service",
            A,
            &format!("{B}X-NotSocketActivated=true\n"),
        )
        .both("sshd.socket", sock, sock)
        .active("sshd.service")
        .active("sshd.socket");
        assert_eq!(f.plan().start, set(&["sshd.service", "sshd.socket"]));
    }

    #[test]
    fn template_instances_use_the_template() {
        let mut f = Fixture::new();
        f.both("getty@.service", A, B).active("getty@tty1.service");
        f.active
            .get_mut("getty@tty1.service")
            .unwrap()
            .fragment_path = f
            .current
            .join("getty@.service")
            .to_string_lossy()
            .into_owned();
        let plan = f.plan();
        assert_eq!(plan.stop, set(&["getty@tty1.service"]));
        assert_eq!(plan.start, set(&["getty@tty1.service"]));
    }

    #[test]
    fn mounts_restart_except_root() {
        let mut f = Fixture::new();
        let m = |what: &str| format!("[Mount]\nWhat={what}\nWhere=/x\n");
        f.both("data.mount", &m("/dev/a"), &m("/dev/b"))
            .both("-.mount", &m("/dev/a"), &m("/dev/b"))
            .active("data.mount")
            .active("-.mount");
        let plan = f.plan();
        assert_eq!(plan.restart, set(&["data.mount"]));
        assert_eq!(plan.reload, set(&["-.mount"]));
    }

    #[test]
    fn interrupted_switches_are_resumed() {
        let f = Fixture::new();
        std::fs::write(f.run.join("restart-list"), "a.service\n\nb.service\n").unwrap();
        assert_eq!(f.plan().restart, set(&["a.service", "b.service"]));
    }

    #[test]
    fn dry_runs_do_not_record() {
        let mut f = Fixture::new();
        f.both("a.service", A, B).active("a.service");
        let journal = Journal::new(&f.run, true);
        let mut planner = Planner::new(f.current.clone(), f.new.clone(), &f.active, &journal);
        planner.classify_active().unwrap();
        assert_eq!(planner.plan.start, set(&["a.service"]));
        assert!(!journal.path(List::Start).exists());
    }

    #[test]
    fn fstab_changes() {
        let f = Fixture::new();
        let journal = f.journal();
        let mut planner = Planner::new(f.current.clone(), f.new.clone(), &f.active, &journal);
        let current = Fstab::parse(
            "/dev/a / ext4 defaults\n/dev/b /data ext4 defaults\n/dev/c /opts ext4 ro\n/dev/d \
             /gone ext4 defaults\n/dev/r /nix ext4 defaults\n/dev/s none swap\n",
        );
        let new = Fstab::parse(
            "/dev/a / ext4 noatime\n/dev/b2 /data ext4 defaults\n/dev/c /opts ext4 rw\n/dev/r2 \
             /nix ext4 defaults\n",
        );
        planner.diff_fstab(&current, &new);
        let plan = planner.into_plan();
        assert_eq!(plan.reload, set(&["-.mount", "opts.mount"]));
        assert_eq!(plan.restart, set(&["data.mount"]));
        assert_eq!(plan.stop, set(&["gone.mount"]));
        assert_eq!(plan.skip, set(&["nix.mount"]));
        assert_eq!(plan.swapoff, set(&["/dev/s"]));
    }

    #[test]
    fn activation_requests() {
        let mut f = Fixture::new();
        f.both("a.service", A, A)
            .both("b.service", A, A)
            .new_unit("new.service", A)
            .active("a.service")
            .active("b.service");
        let journal = f.journal();
        let mut planner = Planner::new(f.current.clone(), f.new.clone(), &f.active, &journal);
        planner
            .activation_restarts(&set(&["a.service", "new.service"]))
            .unwrap();
        planner.activation_reloads(&set(&["a.service", "b.service", "inactive.service"]));
        let plan = planner.into_plan();
        // After activation, a restart replaces stop and start.
        assert_eq!(plan.restart, set(&["a.service"]));
        assert!(plan.stop.is_empty());
        assert_eq!(plan.start, set(&["new.service"]));
        assert_eq!(plan.reload, set(&["b.service"]));
    }

    #[test]
    fn journal_unrecord() {
        let f = Fixture::new();
        let journal = f.journal();
        journal.record(List::Reload, "a.service");
        journal.record(List::Reload, "b.service");
        journal.unrecord(List::Reload, "a.service");
        assert_eq!(journal.read(List::Reload), set(&["b.service"]));
        journal.clear(List::Reload).unwrap();
        journal.clear(List::Reload).unwrap();
        assert!(journal.read(List::Reload).is_empty());
    }
}
