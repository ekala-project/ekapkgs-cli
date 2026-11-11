//! Switch a running ekaos system to a new toplevel.
//!
//! This is the engine behind `ekapkgs system switch` and the toplevel's
//! `bin/switch-to-configuration`. It is a port of nixpkgs'
//! switch-to-configuration-ng: units are classified with the same unit
//! keys (`X-RestartIfChanged`, `X-ReloadIfChanged`, `X-StopIfChanged`,
//! `X-Restart-Triggers`, ...) and jobs are queued in the same order, so a
//! service behaves the same on switch as it would on NixOS.
//!
//! The toplevel must implement a [`contract`] version this engine knows.

pub mod contract;
pub mod fstab;
pub mod manager;
pub mod plan;
mod switch;
pub mod unit;

use std::path::PathBuf;

pub use manager::{FailedJob, JobKind};
use nix::fcntl::{Flock, FlockArg};

/// What to do with the new toplevel.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Make it the boot default and activate it.
    #[default]
    Switch,
    /// Make it the boot default only.
    Boot,
    /// Activate it without making it the boot default.
    Test,
    /// Print what activating it would do.
    DryActivate,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Switch => "switch",
            Action::Boot => "boot",
            Action::Test => "test",
            Action::DryActivate => "dry-activate",
        }
    }
}

impl std::str::FromStr for Action {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        Ok(match s {
            "switch" => Action::Switch,
            "boot" => Action::Boot,
            "test" => Action::Test,
            "dry-activate" => Action::DryActivate,
            _ => return Err(Error::UnknownAction(s.to_owned())),
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The toplevel to switch to.
    pub toplevel: PathBuf,
    pub action: Action,
    /// Reinstall the boot loader even if it is up to date.
    pub install_bootloader: bool,
    /// Skip flushing the store before activation.
    pub no_sync: bool,
    /// Also list active targets, which are always restarted.
    pub show_all_units: bool,
}

/// The result of a switch that ran to completion.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The new generation needs a reboot to take effect; nothing was
    /// activated.
    pub reboot_required: bool,
    pub activation_failed: bool,
    pub failed_jobs: Vec<FailedJob>,
    pub failed_units: Vec<String>,
    pub new_units: Vec<String>,
}

impl Outcome {
    /// The exit status switch-to-configuration uses for this outcome.
    pub fn exit_code(&self) -> i32 {
        if self.reboot_required {
            100
        } else if !self.failed_jobs.is_empty() || !self.failed_units.is_empty() {
            4
        } else if self.activation_failed {
            2
        } else {
            0
        }
    }

    pub fn success(&self) -> bool {
        self.exit_code() == 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("D-Bus: {0}")]
    Dbus(#[from] zbus::Error),
    #[error("switching needs root")]
    NotRoot,
    #[error("another switch is in progress")]
    Locked,
    #[error("{} is not a switchable ekaos toplevel (no ekaos-switch-version)", .0.display())]
    NotSwitchable(PathBuf),
    #[error(
        "unsupported ekaos switch contract {0:?} (supported: {supported:?}); update ekapkgs",
        supported = contract::SUPPORTED_VERSIONS
    )]
    UnsupportedContract(String),
    #[error("unknown action {0:?}")]
    UnknownAction(String),
    #[error("{what} failed ({status})")]
    Command {
        what: &'static str,
        status: std::process::ExitStatus,
    },
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("lost the connection to systemd while {0}")]
    Disconnected(&'static str),
}

impl Error {
    pub(crate) fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }
}

/// Switch the running system to `opts.toplevel`.
///
/// Must run as root. Only one switch runs at a time; a concurrent one
/// fails with [`Error::Locked`].
pub fn switch(opts: &Options) -> Result<Outcome, Error> {
    if !nix::unistd::geteuid().is_root() {
        return Err(Error::NotRoot);
    }
    contract::check(&opts.toplevel)?;

    let paths = switch::Paths::default();
    let run_dir = paths.run_dir();
    std::fs::create_dir_all(&run_dir)
        .map_err(|e| Error::io(format!("creating {}", run_dir.display()), e))?;
    let lock_path = run_dir.join("switch.lock");
    let lock_file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&lock_path)
        .map_err(|e| Error::io(format!("opening {}", lock_path.display()), e))?;
    // Held until the switch returns.
    let _lock = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock).map_err(|(_, errno)| {
        if errno == nix::errno::Errno::EWOULDBLOCK {
            Error::Locked
        } else {
            Error::io(format!("locking {}", lock_path.display()), errno.into())
        }
    })?;

    if matches!(opts.action, Action::Switch | Action::Boot) {
        switch::install_bootloader(opts)?;
    }
    if opts.action == Action::Boot {
        return Ok(Outcome::default());
    }

    let mut manager = manager::DbusManager::system()?;
    switch::activate(opts, &paths, &mut manager)
}
