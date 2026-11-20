//! Memory introspection for `CacheStatus`.
//!
//! Process totals come from `/proc/self/status`; the structures the server
//! keeps in memory report their own sizes (see
//! `CastoreBackend::memory_components`). SQLite's own allocator stats need
//! `unsafe` FFI, which this workspace forbids, so SQLite page caches are
//! reported as their configured limits.

use ekapkgs_protocol::ekapkgs::v1::MemoryComponent;

/// Process-wide memory totals, all in bytes. Zero when unavailable.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    pub rss: u64,
    pub rss_anon: u64,
    pub rss_file: u64,
    pub peak_rss: u64,
}

impl ProcessMemory {
    /// Read the current process's memory totals.
    pub fn read() -> Self {
        std::fs::read_to_string("/proc/self/status")
            .map(|s| Self::parse_status(&s))
            .unwrap_or_default()
    }

    /// Parse the `VmRSS`/`RssAnon`/`RssFile`/`VmHWM` lines of a
    /// `/proc/<pid>/status` file.
    fn parse_status(status: &str) -> Self {
        let mut mem = Self::default();
        for line in status.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let field = match key {
                "VmRSS" => &mut mem.rss,
                "RssAnon" => &mut mem.rss_anon,
                "RssFile" => &mut mem.rss_file,
                "VmHWM" => &mut mem.peak_rss,
                _ => continue,
            };
            *field = parse_kib(value).unwrap_or(0);
        }
        mem
    }
}

/// Parse a `/proc` value such as `"   1234 kB"` into bytes.
fn parse_kib(value: &str) -> Option<u64> {
    let kib: u64 = value.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

/// Resident bytes of all mappings of `file` (e.g. SQLite's mmap of the
/// database), from `/proc/self/smaps`. `None` when unavailable.
pub fn mapped_file_rss(file: &std::path::Path) -> Option<u64> {
    let smaps = std::fs::read_to_string("/proc/self/smaps").ok()?;
    Some(parse_smaps_rss(&smaps, &file.to_string_lossy()))
}

/// Sum the `Rss:` lines of the mappings in `smaps` whose path is `path`.
fn parse_smaps_rss(smaps: &str, path: &str) -> u64 {
    let mut total = 0;
    let mut in_target = false;
    for line in smaps.lines() {
        if let Some((key, value)) = line.split_once(':')
            && !key.contains(' ')
            && key.chars().next().is_some_and(char::is_uppercase)
        {
            // A field line of the current mapping.
            if in_target && key == "Rss" {
                total += parse_kib(value).unwrap_or(0);
            }
            continue;
        }
        // A mapping header: `start-end perms offset dev inode [path]`.
        in_target = mapping_path(line) == path;
    }
    total
}

/// The path column of an smaps mapping header (may contain spaces).
fn mapping_path(header: &str) -> &str {
    let mut rest = header;
    for _ in 0..5 {
        rest = rest.trim_start();
        rest = rest.find(char::is_whitespace).map_or("", |i| &rest[i..]);
    }
    rest.trim()
}

/// Shorthand for a measured component.
pub fn component(name: &str, bytes: u64, entries: u64, detail: String) -> MemoryComponent {
    MemoryComponent {
        name: name.to_owned(),
        bytes,
        entries,
        is_limit: false,
        detail,
    }
}

/// Shorthand for a component whose size is only known as an upper bound.
pub fn limit(name: &str, bytes: u64, entries: u64, detail: String) -> MemoryComponent {
    MemoryComponent {
        is_limit: true,
        ..component(name, bytes, entries, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_status() {
        let status = [
            "Name:\tekapkgs-serve",
            "VmHWM:\t  204800 kB",
            "VmRSS:\t  102400 kB",
            "RssAnon:\t   81920 kB",
            "RssFile:\t   20480 kB",
            "RssShmem:\t       0 kB",
        ]
        .join("\n");
        let status = status.as_str();
        assert_eq!(
            ProcessMemory::parse_status(status),
            ProcessMemory {
                rss: 100 * 1024 * 1024,
                rss_anon: 80 * 1024 * 1024,
                rss_file: 20 * 1024 * 1024,
                peak_rss: 200 * 1024 * 1024,
            }
        );
    }

    #[test]
    fn sums_rss_of_one_file() {
        let smaps = "\
7f0000000000-7f0040000000 r--s 00000000 fd:01 1234                       /var/cache/castore.db
Size:            1048576 kB
Rss:               10240 kB
VmFlags: rd sh mr mw me ms sd
7f0040000000-7f0080000000 r--s 00000000 fd:01 1234                       /var/cache/castore.db
Size:            1048576 kB
Rss:                2048 kB
7f0080000000-7f0080001000 rw-p 00000000 00:00 0
Rss:                   4 kB
7f0090000000-7f0090001000 r--p 00000000 fd:01 99                         /usr/lib/libc.so
Rss:                 100 kB
";
        assert_eq!(
            parse_smaps_rss(smaps, "/var/cache/castore.db"),
            12 * 1024 * 1024
        );
    }
}
