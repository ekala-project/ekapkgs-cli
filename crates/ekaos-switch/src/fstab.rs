//! `/etc/fstab` parsing and systemd mount unit names.

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filesystem {
    pub device: String,
    pub fs_type: String,
    pub options: String,
}

/// Filesystems by mount point, and swap devices with their options.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Fstab {
    pub filesystems: BTreeMap<String, Filesystem>,
    pub swaps: BTreeMap<String, String>,
}

impl Fstab {
    pub fn parse(text: &str) -> Self {
        let mut fstab = Self::default();
        for line in text.lines() {
            // Like switch-to-configuration-ng, any line with a comment is
            // skipped entirely.
            if line.contains('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let (Some(device), Some(mount_point), Some(fs_type)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let options = fields.next().unwrap_or_default().to_owned();

            if fs_type == "swap" {
                fstab.swaps.insert(device.to_owned(), options);
            } else {
                fstab.filesystems.insert(
                    mount_point.to_owned(),
                    Filesystem {
                        device: device.to_owned(),
                        fs_type: fs_type.to_owned(),
                        options,
                    },
                );
            }
        }
        fstab
    }
}

/// The mount (or automount) unit for a path, as `systemd-escape -p
/// --suffix=mount` names it.
pub fn mount_unit_name(path: &str, automount: bool) -> String {
    let suffix = if automount { "automount" } else { "mount" };
    format!("{}.{suffix}", escape_path(path))
}

/// systemd's path escaping (`unit_name_path_escape`).
pub fn escape_path(path: &str) -> String {
    // Collapse duplicate slashes and drop leading and trailing ones.
    let trimmed: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    if trimmed.is_empty() {
        return "-".to_owned();
    }
    let joined = trimmed.join("/");

    let mut out = String::with_capacity(joined.len());
    for (i, b) in joined.bytes().enumerate() {
        match b {
            b'/' => out.push('-'),
            b'.' if i == 0 => out.push_str("\\x2e"),
            b if b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.') => {
                out.push(b as char)
            },
            b => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_filesystems_and_swaps() {
        let fstab = Fstab::parse(
            "# comment\n/dev/vda2 / ext4 defaults 0 1\n/dev/vda1 /boot vfat umask=077 0 2 # \
             trailing\n/dev/vda3 none swap pri=1\ntmpfs /tmp tmpfs\nincomplete line\n",
        );
        assert_eq!(fstab.filesystems.len(), 2);
        assert_eq!(fstab.filesystems["/"].device, "/dev/vda2");
        assert_eq!(fstab.filesystems["/tmp"].options, "");
        assert_eq!(fstab.swaps["/dev/vda3"], "pri=1");
    }

    #[test]
    fn escapes_like_systemd() {
        assert_eq!(mount_unit_name("/", false), "-.mount");
        assert_eq!(mount_unit_name("/nix", false), "nix.mount");
        assert_eq!(mount_unit_name("/var/lib/foo/", false), "var-lib-foo.mount");
        assert_eq!(mount_unit_name("//mnt//data", true), "mnt-data.automount");
        assert_eq!(escape_path("/mnt/my-disk"), "mnt-my\\x2ddisk");
        assert_eq!(escape_path("/mnt/a b"), "mnt-a\\x20b");
        assert_eq!(escape_path("/.hidden/x.y"), "\\x2ehidden-x.y");
        assert_eq!(escape_path("/a\\b"), "a\\x5cb");
    }
}
