//! Unit-file parsing and comparison.
//!
//! This follows nixpkgs' switch-to-configuration-ng, so that unit files
//! written for it are classified the same way: drop-ins are merged in
//! glob order, an empty assignment clears earlier values, and `[Install]`
//! is ignored.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

/// Values per key within one section, in assignment order.
pub type Section = BTreeMap<String, Vec<String>>;

/// A parsed unit: its sections, with all drop-ins applied.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UnitInfo {
    sections: BTreeMap<String, Section>,
}

impl UnitInfo {
    /// Parse a unit file and its drop-ins.
    ///
    /// `base_unit_file` is the file that defines the unit; for an instance
    /// of a template without its own file (`foo@bar.service`) it is the
    /// template (`foo@.service`). Drop-ins are read from
    /// `<base_unit_file>.d/*.conf`, then, for instances, from
    /// `<unit_file>.d/*.conf`.
    pub fn load(unit_file: &Path, base_unit_file: &Path) -> io::Result<Self> {
        let mut info = Self::default();
        info.apply(&std::fs::read_to_string(base_unit_file)?);

        for dropin in dropins(base_unit_file)? {
            info.apply(&std::fs::read_to_string(dropin)?);
        }
        if unit_file != base_unit_file {
            for dropin in dropins(unit_file)? {
                info.apply(&std::fs::read_to_string(dropin)?);
            }
        }

        Ok(info)
    }

    /// Parse unit text without drop-ins.
    pub fn parse(text: &str) -> Self {
        let mut info = Self::default();
        info.apply(text);
        info
    }

    /// Apply one unit file or drop-in on top of what was parsed so far.
    pub fn apply(&mut self, text: &str) {
        let mut section: Option<String> = None;

        for line in logical_lines(text) {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = Some(name.to_owned());
                continue;
            }
            // [Install] has no keys that matter for switching.
            let Some(name) = section.as_deref().filter(|s| *s != "Install") else {
                continue;
            };
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            let values = self
                .sections
                .entry(name.to_owned())
                .or_default()
                .entry(key.trim().to_owned())
                .or_default();
            let value = value.trim();
            if value.is_empty() {
                // An empty assignment resets the list.
                values.clear();
            } else {
                values.push(value.to_owned());
            }
        }
    }

    /// All values of `key` in `section`.
    pub fn values(&self, section: &str, key: &str) -> Option<&[String]> {
        self.sections
            .get(section)
            .and_then(|s| s.get(key))
            .map(Vec::as_slice)
    }

    /// A boolean setting, using the last assignment, or `default` when the
    /// key is unset.
    pub fn bool(&self, section: &str, key: &str, default: bool) -> bool {
        match self.values(section, key).and_then(<[String]>::last) {
            Some(v) => matches!(v.as_str(), "1" | "yes" | "true" | "on"),
            None => default,
        }
    }

    pub fn sections(&self) -> &BTreeMap<String, Section> {
        &self.sections
    }
}

/// Join backslash-continued lines.
fn logical_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for raw in text.lines() {
        if let Some(head) = raw.strip_suffix('\\') {
            current.push_str(head);
            current.push(' ');
        } else {
            current.push_str(raw);
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// `<unit>.d/*.conf`, sorted.
fn dropins(unit_file: &Path) -> io::Result<Vec<PathBuf>> {
    let mut dir = unit_file.as_os_str().to_owned();
    dir.push(".d");
    let entries = match std::fs::read_dir(PathBuf::from(dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "conf"))
        .collect();
    files.sort();
    Ok(files)
}

/// How a unit changed between two generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Equal,
    NeedsRestart,
    NeedsReload,
}

/// `[Unit]` keys whose change does not require a restart.
const UNIT_SECTION_IGNORES: &[&str] = &[
    "X-Reload-Triggers",
    "Description",
    "Documentation",
    "OnFailure",
    "OnSuccess",
    "OnFailureJobMode",
    "IgnoreOnIsolate",
    "StopWhenUnneeded",
    "RefuseManualStart",
    "RefuseManualStop",
    "AllowIsolate",
    "CollectMode",
    "SourcePath",
];

fn ignored(key: &str) -> bool {
    UNIT_SECTION_IGNORES.contains(&key)
}

/// Compare two versions of a unit.
///
/// Units differ unless the only changes are to ignored `[Unit]` keys. A
/// change to `X-Reload-Triggers`, or to `Options` of a `[Mount]`, asks for a
/// reload instead of a restart.
pub fn compare(current: &UnitInfo, new: &UnitInfo) -> Comparison {
    let mut result = Comparison::Equal;
    let mut new_sections: BTreeSet<&str> = new.sections.keys().map(String::as_str).collect();

    for (section_name, section) in &current.sections {
        let Some(new_section) = new.sections.get(section_name) else {
            // A removed [Unit] section is fine if it only held ignored keys.
            if section_name == "Unit" && section.keys().all(|k| ignored(k)) {
                continue;
            }
            return Comparison::NeedsRestart;
        };
        new_sections.remove(section_name.as_str());

        let mut new_keys: BTreeSet<&str> = new_section.keys().map(String::as_str).collect();
        for (key, value) in section {
            new_keys.remove(key.as_str());
            let Some(new_value) = new_section.get(key) else {
                if section_name == "Unit" && ignored(key) {
                    continue;
                }
                return Comparison::NeedsRestart;
            };
            if value == new_value {
                continue;
            }
            if section_name == "Unit" {
                if key == "X-Reload-Triggers" {
                    result = Comparison::NeedsReload;
                    continue;
                }
                if ignored(key) {
                    continue;
                }
            }
            if section_name == "Mount" && key == "Options" {
                result = Comparison::NeedsReload;
                continue;
            }
            return Comparison::NeedsRestart;
        }

        // Keys only present in the new unit.
        for key in new_keys {
            if section_name != "Unit" {
                return Comparison::NeedsRestart;
            }
            if key == "X-Reload-Triggers" {
                result = Comparison::NeedsReload;
            } else if !ignored(key) {
                return Comparison::NeedsRestart;
            }
        }
    }

    // Sections only present in the new unit: only a [Unit] with ignored keys
    // is tolerated.
    if !new_sections.is_empty() {
        if new_sections.len() != 1 || !new_sections.contains("Unit") {
            return Comparison::NeedsRestart;
        }
        for key in new.sections["Unit"].keys() {
            if !ignored(key) {
                return Comparison::NeedsRestart;
            }
            if key == "X-Reload-Triggers" {
                result = Comparison::NeedsReload;
            }
        }
    }

    result
}

/// For an instance name like `foo@bar.service`, the template name
/// `foo@.service`.
///
/// This matches `^(.*)@[^.]*\.(.*)$`: the last `@` that is followed by a
/// `.`, and the instance runs up to the first `.` after it.
pub fn template_of(unit: &str) -> Option<String> {
    unit.match_indices('@').rev().find_map(|(at, _)| {
        let rest = &unit[at + 1..];
        let dot = rest.find('.')?;
        Some(format!("{}@.{}", &unit[..at], &rest[dot + 1..]))
    })
}

/// A unit name without its type suffix (`foo.service` -> `foo`).
pub fn base_name(unit: &str) -> &str {
    match unit.rsplit_once('.') {
        Some((base, suffix)) if suffix.chars().all(|c| c.is_ascii_lowercase()) => base,
        _ => unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(text: &str) -> UnitInfo {
        UnitInfo::parse(text)
    }

    #[test]
    fn parses_sections_and_ignores_install() {
        let u = unit(
            "[Unit]\nDescription=x\n# \
             comment\n[Service]\nExecStart=/a\n[Install]\nWantedBy=multi-user.target\n",
        );
        assert_eq!(u.values("Unit", "Description"), Some(&["x".to_owned()][..]));
        assert_eq!(
            u.values("Service", "ExecStart"),
            Some(&["/a".to_owned()][..])
        );
        assert!(u.sections().get("Install").is_none());
    }

    #[test]
    fn empty_assignment_resets() {
        let mut u = unit("[Service]\nExecStart=/a\nExecStart=/b\n");
        u.apply("[Service]\nExecStart=\nExecStart=/c\n");
        assert_eq!(
            u.values("Service", "ExecStart"),
            Some(&["/c".to_owned()][..])
        );

        u.apply("[Service]\nExecStart=/d\n");
        assert_eq!(
            u.values("Service", "ExecStart").map(<[String]>::len),
            Some(2)
        );
    }

    #[test]
    fn continuation_lines_are_joined() {
        let u = unit("[Service]\nExecStart=/bin/a \\\n  --flag\n");
        assert_eq!(
            u.values("Service", "ExecStart"),
            Some(&["/bin/a    --flag".to_owned()][..])
        );
    }

    #[test]
    fn bools_use_last_value_and_default() {
        let u = unit("[Service]\nX-RestartIfChanged=true\nX-RestartIfChanged=false\n");
        assert!(!u.bool("Service", "X-RestartIfChanged", true));
        assert!(u.bool("Service", "X-StopIfChanged", true));
        assert!(!u.bool("Unit", "X-OnlyManualStart", false));
    }

    #[test]
    fn load_applies_template_and_instance_dropins() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("getty@.service");
        std::fs::write(&base, "[Service]\nExecStart=/getty\n").unwrap();
        std::fs::create_dir(dir.path().join("getty@.service.d")).unwrap();
        std::fs::write(
            dir.path().join("getty@.service.d/50-ekaos-switch.conf"),
            "[Service]\nX-RestartIfChanged=false\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("getty@tty1.service.d")).unwrap();
        std::fs::write(
            dir.path().join("getty@tty1.service.d/override.conf"),
            "[Service]\nExecStart=\nExecStart=/other\n",
        )
        .unwrap();
        // Not a .conf file: ignored.
        std::fs::write(
            dir.path().join("getty@.service.d/README"),
            "[Service]\nX=1\n",
        )
        .unwrap();

        let u = UnitInfo::load(&dir.path().join("getty@tty1.service"), &base).unwrap();
        assert!(!u.bool("Service", "X-RestartIfChanged", true));
        assert_eq!(
            u.values("Service", "ExecStart"),
            Some(&["/other".to_owned()][..])
        );
        assert!(u.values("Service", "X").is_none());
    }

    #[test]
    fn compare_equal_and_ignored_keys() {
        let a = unit("[Unit]\nDescription=a\n[Service]\nExecStart=/a\n");
        let b = unit("[Unit]\nDescription=b\nDocumentation=man:x\n[Service]\nExecStart=/a\n");
        assert_eq!(compare(&a, &b), Comparison::Equal);
        assert_eq!(compare(&b, &a), Comparison::Equal);
    }

    #[test]
    fn compare_service_change_needs_restart() {
        let a = unit("[Service]\nExecStart=/a\n");
        let b = unit("[Service]\nExecStart=/b\n");
        assert_eq!(compare(&a, &b), Comparison::NeedsRestart);
        let c = unit("[Service]\nExecStart=/a\nUser=x\n");
        assert_eq!(compare(&a, &c), Comparison::NeedsRestart);
        assert_eq!(compare(&c, &a), Comparison::NeedsRestart);
        let d = unit("[Service]\nExecStart=/a\n[Socket]\nListenStream=1\n");
        assert_eq!(compare(&a, &d), Comparison::NeedsRestart);
    }

    #[test]
    fn compare_reload_triggers_need_reload() {
        let a = unit("[Unit]\nX-Reload-Triggers=/nix/store/a\n[Service]\nExecStart=/a\n");
        let b = unit("[Unit]\nX-Reload-Triggers=/nix/store/b\n[Service]\nExecStart=/a\n");
        assert_eq!(compare(&a, &b), Comparison::NeedsReload);

        // Added in the new unit, with the [Unit] section itself new.
        let c = unit("[Service]\nExecStart=/a\n");
        assert_eq!(compare(&c, &a), Comparison::NeedsReload);

        // A reload trigger plus a real change still restarts.
        let d = unit("[Unit]\nX-Reload-Triggers=/nix/store/b\n[Service]\nExecStart=/b\n");
        assert_eq!(compare(&a, &d), Comparison::NeedsRestart);
    }

    #[test]
    fn compare_restart_triggers_need_restart() {
        let a = unit("[Unit]\nX-Restart-Triggers=/nix/store/a\n[Service]\nExecStart=/a\n");
        let b = unit("[Unit]\nX-Restart-Triggers=/nix/store/b\n[Service]\nExecStart=/a\n");
        assert_eq!(compare(&a, &b), Comparison::NeedsRestart);
    }

    #[test]
    fn compare_mount_options_need_reload() {
        let a = unit("[Mount]\nWhat=/dev/a\nOptions=ro\n");
        let b = unit("[Mount]\nWhat=/dev/a\nOptions=rw\n");
        assert_eq!(compare(&a, &b), Comparison::NeedsReload);
    }

    #[test]
    fn template_and_base_names() {
        assert_eq!(
            template_of("getty@tty1.service").as_deref(),
            Some("getty@.service")
        );
        assert_eq!(
            template_of("a@b@c.service").as_deref(),
            Some("a@b@.service")
        );
        assert_eq!(template_of("a@b.c@d").as_deref(), Some("a@.c@d"));
        assert_eq!(template_of("sshd.service"), None);
        assert_eq!(base_name("sshd.service"), "sshd");
        assert_eq!(base_name("getty@.service"), "getty@");
        assert_eq!(base_name("noext"), "noext");
    }
}
