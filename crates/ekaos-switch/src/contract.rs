//! The toplevel switch contract.
//!
//! An ekaos toplevel declares which version of the switch contract it was
//! built for in `$toplevel/ekaos-switch-version`. The contract covers the
//! files the engine reads from the toplevel (`activate`,
//! `bin/install-bootloader`, `init-interface-version`, `etc/`, `systemd/`)
//! and the unit keys it interprets. An engine refuses toplevels with a
//! contract it does not implement, instead of guessing.

use std::path::Path;

use crate::Error;

/// Contract versions this engine implements.
pub const SUPPORTED_VERSIONS: &[u32] = &[1];

/// Read and check the contract version of a toplevel.
pub fn check(toplevel: &Path) -> Result<u32, Error> {
    let path = toplevel.join("ekaos-switch-version");
    let text = std::fs::read_to_string(&path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::NotSwitchable(toplevel.to_owned())
        } else {
            Error::io(format!("reading {}", path.display()), source)
        }
    })?;

    let text = text.trim();
    let version = text
        .parse::<u32>()
        .map_err(|_| Error::UnsupportedContract(text.to_owned()))?;
    if SUPPORTED_VERSIONS.contains(&version) {
        Ok(version)
    } else {
        Err(Error::UnsupportedContract(text.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_supported_versions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ekaos-switch-version"), "1\n").unwrap();
        assert_eq!(check(dir.path()).unwrap(), 1);
    }

    #[test]
    fn rejects_unknown_versions() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["2", "v1", ""] {
            std::fs::write(dir.path().join("ekaos-switch-version"), bad).unwrap();
            assert!(
                matches!(check(dir.path()), Err(Error::UnsupportedContract(v)) if v == bad),
                "{bad:?} accepted"
            );
        }
    }

    #[test]
    fn rejects_toplevels_without_a_contract() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(check(dir.path()), Err(Error::NotSwitchable(_))));
    }
}
