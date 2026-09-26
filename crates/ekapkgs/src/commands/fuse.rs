use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, Request,
};

use crate::cli::FuseCommand;
use crate::soname_index::{SonameEntry, SonameIndex};

const TTL: Duration = Duration::from_secs(300);
const BLOCK_SIZE: u32 = 512;

// POSIX errno constants (avoids a direct `libc` dependency).
const ENOENT: i32 = 2;
const EIO: i32 = 5;
const EBADF: i32 = 9;

/// Root directory inode.
const ROOT_INO: u64 = 1;

pub fn execute(command: FuseCommand) -> color_eyre::Result<()> {
    match command {
        FuseCommand::Mount {
            mountpoint,
            foreground,
            upstream,
        } => cmd_mount(&mountpoint, foreground, upstream.as_deref()),
        FuseCommand::Unmount { mountpoint } => cmd_unmount(&mountpoint),
        FuseCommand::Status { mountpoint } => {
            cmd_status(&mountpoint);
            Ok(())
        },
    }
}

// ---------------------------------------------------------------------------
// mount
// ---------------------------------------------------------------------------

fn cmd_mount(
    mountpoint: &str,
    _foreground: bool,
    upstream: Option<&str>,
) -> color_eyre::Result<()> {
    // Require root for system-wide mount with allow_other.
    if !nix_is_root() {
        return Err(color_eyre::eyre::eyre!(
            "Mounting at {mountpoint} requires root. Run with sudo."
        ));
    }

    // Create mount point if it doesn't exist.
    std::fs::create_dir_all(mountpoint)?;

    let config = crate::config::ClientConfig::load()?;

    let server_url = match upstream {
        Some(url) => url.to_owned(),
        None => {
            let cache = config.primary_cache().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "No cache configured. Use --upstream or configure a cache in config.toml."
                )
            })?;
            cache.url.clone()
        },
    };

    tracing::info!("Loading soname index...");
    let index = SonameIndex::load()?;
    tracing::info!("Loaded {} sonames", index.len());

    // Build inode tables.
    let mut soname_to_ino: HashMap<String, u64> = HashMap::new();
    let mut ino_to_soname: HashMap<u64, String> = HashMap::new();
    for (i, soname) in index.all_sonames().iter().enumerate() {
        let ino = (i as u64) + 2; // inode 1 = root, 2..N = sonames
        soname_to_ino.insert(soname.clone(), ino);
        ino_to_soname.insert(ino, soname.clone());
    }

    let max_parallel = config.defaults.max_parallel_downloads;

    // Create a persistent tokio runtime for async downloads inside FUSE callbacks.
    let rt = tokio::runtime::Runtime::new()?;

    let fs = LibFs {
        index: Arc::new(RwLock::new(index)),
        server_url,
        max_parallel,
        rt,
        soname_to_ino,
        ino_to_soname,
        resolved: Mutex::new(HashMap::new()),
        open_files: Mutex::new(HashMap::new()),
        next_fh: AtomicU64::new(1),
        download_locks: Mutex::new(HashMap::new()),
    };

    let options = vec![
        MountOption::RO,
        MountOption::AllowOther,
        MountOption::FSName("ekapkgs".into()),
    ];

    tracing::info!("Mounting FUSE filesystem at {mountpoint}");

    // mount2 blocks until the filesystem is unmounted.
    fuser::mount2(fs, mountpoint, &options)?;

    tracing::info!("FUSE filesystem unmounted");
    Ok(())
}

// ---------------------------------------------------------------------------
// unmount
// ---------------------------------------------------------------------------

fn cmd_unmount(mountpoint: &str) -> color_eyre::Result<()> {
    // Try fusermount3 first, then fusermount, then umount.
    let result = std::process::Command::new("fusermount3")
        .arg("-u")
        .arg(mountpoint)
        .status();

    match result {
        Ok(status) if status.success() => {
            tracing::info!("Unmounted {mountpoint}");
            return Ok(());
        },
        _ => {},
    }

    let result = std::process::Command::new("fusermount")
        .arg("-u")
        .arg(mountpoint)
        .status();

    match result {
        Ok(status) if status.success() => {
            tracing::info!("Unmounted {mountpoint}");
            return Ok(());
        },
        _ => {},
    }

    let status = std::process::Command::new("umount")
        .arg(mountpoint)
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to run umount: {e}"))?;

    if status.success() {
        tracing::info!("Unmounted {mountpoint}");
        Ok(())
    } else {
        Err(color_eyre::eyre::eyre!(
            "Failed to unmount {mountpoint}. Is it mounted?"
        ))
    }
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn cmd_status(mountpoint: &str) {
    let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();

    let is_mounted = mounts.lines().any(|line| {
        let parts: Vec<&str> = line.split_whitespace().collect();
        parts.len() >= 3 && parts[1] == mountpoint && parts[0] == "ekapkgs"
    });

    if is_mounted {
        println!("FUSE filesystem is mounted at {mountpoint}");
    } else {
        println!("FUSE filesystem is not mounted at {mountpoint}");
    }
}

// ---------------------------------------------------------------------------
// FUSE filesystem
// ---------------------------------------------------------------------------

struct LibFs {
    index: Arc<RwLock<SonameIndex>>,
    server_url: String,
    max_parallel: usize,
    rt: tokio::runtime::Runtime,

    // Inode tables (immutable after construction).
    soname_to_ino: HashMap<String, u64>,
    ino_to_soname: HashMap<u64, String>,

    // Resolved paths: ino → real file path on disk (after download).
    resolved: Mutex<HashMap<u64, PathBuf>>,

    // Open file handles: fh → open File.
    open_files: Mutex<HashMap<u64, File>>,
    next_fh: AtomicU64,

    // Per-store-hash download locks to prevent duplicate concurrent downloads.
    download_locks: Mutex<HashMap<String, Arc<std::sync::Mutex<()>>>>,
}

impl LibFs {
    fn root_attr(&self) -> FileAttr {
        let soname_count = self.ino_to_soname.len() as u32;
        FileAttr {
            ino: ROOT_INO,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::Directory,
            perm: 0o555,
            nlink: 2 + soname_count,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn placeholder_attr(&self, ino: u64) -> FileAttr {
        FileAttr {
            ino,
            size: 0,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::RegularFile,
            perm: 0o444,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn real_attr(&self, ino: u64, path: &Path) -> FileAttr {
        let Ok(meta) = std::fs::metadata(path) else {
            return self.placeholder_attr(ino);
        };

        let size = meta.len();
        let mtime = meta.modified().unwrap_or(UNIX_EPOCH);

        FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512),
            atime: mtime,
            mtime,
            ctime: mtime,
            crtime: UNIX_EPOCH,
            kind: FileType::RegularFile,
            perm: 0o444,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn attr_for_ino(&self, ino: u64) -> FileAttr {
        if ino == ROOT_INO {
            return self.root_attr();
        }

        let resolved = self.resolved.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(path) = resolved.get(&ino) {
            self.real_attr(ino, path)
        } else {
            self.placeholder_attr(ino)
        }
    }

    /// Ensure the store path for a soname entry exists, downloading if necessary.
    /// Returns the full path to the .so file on disk.
    fn ensure_available(&self, entry: &SonameEntry) -> Result<PathBuf, i32> {
        let real_path = PathBuf::from(&entry.store_path).join(&entry.file_path);

        // Fast path: already available.
        if real_path.exists() {
            return Ok(real_path);
        }

        // Extract store hash for negotiation.
        let store_hash = ekapkgs_nix::store::store_path_hash(&entry.store_path)
            .ok_or(EIO)?
            .to_owned();

        // Get or create per-hash download lock.
        let lock = {
            let mut locks = self
                .download_locks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            locks
                .entry(store_hash.clone())
                .or_insert_with(|| Arc::new(std::sync::Mutex::new(())))
                .clone()
        };

        let _guard = lock.lock().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring lock.
        if real_path.exists() {
            return Ok(real_path);
        }

        tracing::info!("Downloading {} for {}...", entry.package, entry.soname);

        // Run the async download on the persistent runtime.
        let download_result = self.rt.block_on(async {
            let response =
                crate::negotiate::negotiate_closure(&self.server_url, vec![store_hash.clone()], vec![])
                    .await?;

            if response.available.is_empty() {
                return Err(color_eyre::eyre::eyre!(
                    "Package {} not available on cache",
                    entry.package
                ));
            }

            crate::prefetch::import_with_fallback(
                &self.server_url,
                &response,
                vec![store_hash],
                vec![],
                self.max_parallel,
            )
            .await?;

            Ok(())
        });

        match download_result {
            Ok(()) => {
                if real_path.exists() {
                    Ok(real_path)
                } else {
                    tracing::error!(
                        "Downloaded {} but {} not found",
                        entry.package,
                        real_path.display()
                    );
                    Err(ENOENT)
                }
            },
            Err(e) => {
                tracing::error!("Failed to download {}: {e}", entry.package);
                Err(EIO)
            },
        }
    }
}

impl Filesystem for LibFs {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent != ROOT_INO {
            reply.error(ENOENT);
            return;
        }

        let Some(name) = name.to_str() else {
            reply.error(ENOENT);
            return;
        };

        match self.soname_to_ino.get(name) {
            Some(&ino) => {
                let attr = self.attr_for_ino(ino);
                reply.entry(&TTL, &attr, 0);
            },
            None => reply.error(ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        if ino == ROOT_INO || self.ino_to_soname.contains_key(&ino) {
            reply.attr(&TTL, &self.attr_for_ino(ino));
        } else {
            reply.error(ENOENT);
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if ino != ROOT_INO {
            reply.error(ENOENT);
            return;
        }

        let index = self.index.read().unwrap_or_else(PoisonError::into_inner);
        let sonames = index.all_sonames();

        // Entries: . (offset 0), .. (offset 1), then sonames (offset 2+).
        let mut entries: Vec<(u64, FileType, &str)> = Vec::with_capacity(sonames.len() + 2);
        entries.push((ROOT_INO, FileType::Directory, "."));
        entries.push((ROOT_INO, FileType::Directory, ".."));
        for soname in sonames {
            if let Some(&ino) = self.soname_to_ino.get(soname.as_str()) {
                entries.push((ino, FileType::RegularFile, soname.as_str()));
            }
        }

        for (i, (ino, kind, name)) in entries.iter().enumerate().skip(offset as usize) {
            // reply.add returns true when the buffer is full.
            if reply.add(*ino, (i + 1) as i64, *kind, name) {
                break;
            }
        }

        reply.ok();
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        let Some(soname) = self.ino_to_soname.get(&ino).cloned() else {
            reply.error(ENOENT);
            return;
        };

        // Look up the entry in the index.
        let entry = {
            let index = self.index.read().unwrap_or_else(PoisonError::into_inner);
            match index.lookup(&soname) {
                Some(e) => e.clone(),
                None => {
                    reply.error(ENOENT);
                    return;
                },
            }
        };

        // Ensure the store path exists (downloading if needed).
        let real_path = match self.ensure_available(&entry) {
            Ok(p) => p,
            Err(errno) => {
                reply.error(errno);
                return;
            },
        };

        // Open the real file.
        let file = match File::open(&real_path) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("Failed to open {}: {e}", real_path.display());
                reply.error(EIO);
                return;
            },
        };

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);
        self.open_files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(fh, file);

        // Cache the resolved path for future getattr calls.
        self.resolved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(ino, real_path);

        reply.opened(fh, fuser::consts::FOPEN_KEEP_CACHE);
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let files = self
            .open_files
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(file) = files.get(&fh) else {
            reply.error(EBADF);
            return;
        };

        let mut buf = vec![0u8; size as usize];
        match file.read_at(&mut buf, offset as u64) {
            Ok(n) => {
                buf.truncate(n);
                reply.data(&buf);
            },
            Err(e) => {
                tracing::error!("read error: {e}");
                reply.error(EIO);
            },
        }
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        self.open_files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&fh);
        reply.ok();
    }
}

/// Check if the current process is running as root.
fn nix_is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|uid| uid.parse::<u32>().ok())
                .map(|uid| uid == 0)
        })
        .unwrap_or(false)
}
