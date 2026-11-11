//! Reconcile systemd user units after a home generation change.
//!
//! Home generations link unit files into `~/.config/systemd/user/`, but
//! linking alone does not make systemd act on them. After activation, this
//! module compares the units of the old and new generation and:
//!
//! - stops units that were removed,
//! - reloads the user manager,
//! - restarts units whose definition changed and that are running,
//! - starts added units (and changed units that are not running) when one of their `WantedBy=`
//!   targets is active.
//!
//! Units are identified by the store path their home-files link resolves to,
//! so a unit counts as changed exactly when its generated content changed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use yansi::Paint;

/// Unit types that home generations may declare and that we reconcile.
const UNIT_SUFFIXES: &[&str] = &[".service", ".socket", ".timer", ".path"];

/// A unit declared by a home generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeUnit {
    /// Store path the unit file resolves to.
    pub source: PathBuf,
    /// Targets from `WantedBy=` in the `[Install]` section.
    pub wanted_by: Vec<String>,
}

/// What changed between two generations' unit sets.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UnitPlan {
    /// Units only in the old generation.
    pub removed: Vec<String>,
    /// Units only in the new generation, with their `WantedBy=` targets.
    pub added: Vec<(String, Vec<String>)>,
    /// Units in both generations whose definition changed.
    pub changed: Vec<(String, Vec<String>)>,
}

impl UnitPlan {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty() && self.added.is_empty() && self.changed.is_empty()
    }
}

/// Read the user units declared by a per-user home generation directory.
pub fn scan_units(generation: &Path) -> BTreeMap<String, HomeUnit> {
    let dir = generation.join("home-files/.config/systemd/user");
    let mut units = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return units;
    };

    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !UNIT_SUFFIXES.iter().any(|s| name.ends_with(s)) {
            continue;
        }
        // Follow the home-files link to the generated unit file.
        let Ok(source) = std::fs::canonicalize(entry.path()) else {
            continue;
        };
        if !source.is_file() {
            continue;
        }
        let wanted_by = std::fs::read_to_string(&source)
            .map(|text| parse_wanted_by(&text))
            .unwrap_or_default();
        units.insert(name, HomeUnit { source, wanted_by });
    }

    units
}

/// Extract the `WantedBy=` targets from a unit file's `[Install]` section.
pub fn parse_wanted_by(text: &str) -> Vec<String> {
    let mut in_install = false;
    let mut targets = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_install = line == "[Install]";
            continue;
        }
        if !in_install {
            continue;
        }
        if let Some(value) = line.strip_prefix("WantedBy=") {
            if value.trim().is_empty() {
                // An empty assignment resets the list.
                // Explicit path: yansi::Paint::clear would shadow Vec::clear.
                Vec::clear(&mut targets);
            } else {
                targets.extend(value.split_whitespace().map(str::to_owned));
            }
        }
    }
    targets
}

/// Compare two unit sets.
pub fn plan(old: &BTreeMap<String, HomeUnit>, new: &BTreeMap<String, HomeUnit>) -> UnitPlan {
    let mut plan = UnitPlan::default();
    for name in old.keys() {
        if !new.contains_key(name) {
            plan.removed.push(name.clone());
        }
    }
    for (name, unit) in new {
        match old.get(name) {
            None => plan.added.push((name.clone(), unit.wanted_by.clone())),
            Some(prev) if prev.source != unit.source => {
                plan.changed.push((name.clone(), unit.wanted_by.clone()));
            },
            Some(_) => {},
        }
    }
    plan
}

/// Reconcile the user manager after switching from `old` to `new`.
///
/// `old` is `None` when there was no previous generation. Problems with
/// individual units are reported but do not fail the switch: the new home
/// generation is already active at this point.
pub fn reconcile(old: Option<&Path>, new: &Path) {
    let old_units = old.map(scan_units).unwrap_or_default();
    let new_units = scan_units(new);
    let plan = plan(&old_units, &new_units);

    if plan.is_empty() {
        return;
    }

    if !user_manager_available() {
        println!(
            "{}",
            format!(
                "User units changed, but no systemd user manager is reachable. Run `systemctl \
                 --user daemon-reload` in your session to pick them up ({}).",
                summarize(&plan)
            )
            .dim()
        );
        return;
    }

    tracing::info!("Reconciling user units...");
    let mut failed = Vec::new();

    for name in &plan.removed {
        if is_active(name) {
            report("Stopping", name, systemctl(&["stop", name]), &mut failed);
        }
    }

    if !systemctl(&["daemon-reload"]) {
        tracing::warn!("systemctl --user daemon-reload failed");
    }

    for (name, wanted_by) in &plan.changed {
        if is_active(name) {
            report(
                "Restarting",
                name,
                systemctl(&["restart", name]),
                &mut failed,
            );
        } else if is_wanted(wanted_by) {
            report("Starting", name, systemctl(&["start", name]), &mut failed);
        }
    }

    for (name, wanted_by) in &plan.added {
        if is_wanted(wanted_by) {
            report("Starting", name, systemctl(&["start", name]), &mut failed);
        }
    }

    if !failed.is_empty() {
        tracing::warn!(
            "Some user units failed: {}. Check `journalctl --user -u <unit>`.",
            failed.join(", ")
        );
    }
}

/// Reload the user manager without starting or stopping anything. Used after
/// a failed activation, which may have left new unit links behind.
pub fn reload() {
    if user_manager_available() {
        let _ = systemctl(&["daemon-reload"]);
    }
}

fn summarize(plan: &UnitPlan) -> String {
    let mut parts = Vec::new();
    if !plan.added.is_empty() {
        parts.push(format!("{} added", plan.added.len()));
    }
    if !plan.changed.is_empty() {
        parts.push(format!("{} changed", plan.changed.len()));
    }
    if !plan.removed.is_empty() {
        parts.push(format!("{} removed", plan.removed.len()));
    }
    parts.join(", ")
}

fn report(action: &str, unit: &str, ok: bool, failed: &mut Vec<String>) {
    if ok {
        tracing::info!("{action} {unit}");
    } else {
        tracing::warn!("{action} {unit} failed");
        failed.push(unit.to_owned());
    }
}

/// A unit is wanted when one of its `WantedBy=` targets is active.
fn is_wanted(wanted_by: &[String]) -> bool {
    wanted_by.iter().any(|t| is_active(t))
}

fn user_manager_available() -> bool {
    Command::new("systemctl")
        .args(["--user", "show-environment"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn is_active(unit: &str) -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", unit])
        .status()
        .is_ok_and(|s| s.success())
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(source: &str, wanted_by: &[&str]) -> HomeUnit {
        HomeUnit {
            source: PathBuf::from(source),
            wanted_by: wanted_by.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn parses_wanted_by_only_from_install_section() {
        let text = [
            "[Unit]",
            "WantedBy=bogus.target",
            "[Service]",
            "ExecStart=/bin/true",
            "[Install]",
            "WantedBy=default.target",
            "WantedBy=a.target b.target",
        ]
        .join("\n");
        assert_eq!(
            parse_wanted_by(&text),
            vec!["default.target", "a.target", "b.target"]
        );
    }

    #[test]
    fn empty_wanted_by_resets_list() {
        let text = "[Install]\nWantedBy=a.target\nWantedBy=\nWantedBy=b.target\n";
        assert_eq!(parse_wanted_by(text), vec!["b.target"]);
    }

    #[test]
    fn plans_added_changed_and_removed() {
        let old = BTreeMap::from([
            ("same.service".to_owned(), unit("/s/1", &[])),
            ("edit.service".to_owned(), unit("/s/2", &["default.target"])),
            ("gone.service".to_owned(), unit("/s/3", &[])),
        ]);
        let new = BTreeMap::from([
            ("same.service".to_owned(), unit("/s/1", &[])),
            ("edit.service".to_owned(), unit("/s/4", &["default.target"])),
            ("new.socket".to_owned(), unit("/s/5", &["sockets.target"])),
        ]);
        assert_eq!(
            plan(&old, &new),
            UnitPlan {
                removed: vec!["gone.service".to_owned()],
                added: vec![("new.socket".to_owned(), vec!["sockets.target".to_owned()])],
                changed: vec![("edit.service".to_owned(), vec!["default.target".to_owned()])],
            }
        );
        assert!(plan(&new, &new).is_empty());
    }

    #[test]
    fn scans_units_through_home_files_links() {
        let store = tempfile::tempdir().unwrap();
        let unit_file = store.path().join("postgresql.service");
        std::fs::write(
            &unit_file,
            "[Service]\nExecStart=/bin/true\n[Install]\nWantedBy=default.target\n",
        )
        .unwrap();
        std::fs::write(store.path().join("mpd.conf"), "").unwrap();

        let generation = tempfile::tempdir().unwrap();
        let dir = generation.path().join("home-files/.config/systemd/user");
        std::fs::create_dir_all(dir.join("default.target.wants")).unwrap();
        std::os::unix::fs::symlink(&unit_file, dir.join("postgresql.service")).unwrap();
        std::os::unix::fs::symlink(store.path().join("mpd.conf"), dir.join("mpd.conf")).unwrap();
        std::os::unix::fs::symlink(&unit_file, dir.join("dangling.service.d")).unwrap();

        let units = scan_units(generation.path());
        assert_eq!(units.keys().collect::<Vec<_>>(), vec!["postgresql.service"]);
        let pg = &units["postgresql.service"];
        assert_eq!(pg.source, std::fs::canonicalize(&unit_file).unwrap());
        assert_eq!(pg.wanted_by, vec!["default.target"]);

        assert!(scan_units(&generation.path().join("missing")).is_empty());
    }
}
