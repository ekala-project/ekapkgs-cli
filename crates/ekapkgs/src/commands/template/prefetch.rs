use std::path::{Path, PathBuf};
use std::process::Command;

use color_eyre::eyre::eyre;

use super::types::{ExpressionInfo, Fetcher};

/// Result of prefetching a source archive.
pub struct Prefetched {
    /// SRI hash string (e.g., `sha256-...`).
    pub hash: String,
    /// Store path of the unpacked source tree, when it was requested.
    pub path: Option<PathBuf>,
}

/// Archive URL for the given git ref of a remote fetcher.
fn archive_url(fetcher: &Fetcher, git_ref: &str) -> color_eyre::Result<String> {
    match fetcher {
        Fetcher::GitHub {
            owner,
            repo,
            domain,
        } => {
            let host = domain.as_deref().unwrap_or("github.com");
            Ok(format!(
                "https://{host}/{owner}/{repo}/archive/{git_ref}.tar.gz"
            ))
        },
        Fetcher::GitLab {
            owner,
            repo,
            domain,
        } => {
            let host = domain.as_deref().unwrap_or("gitlab.com");
            Ok(format!(
                "https://{host}/{owner}/{repo}/-/archive/{git_ref}/{repo}-{git_ref}.tar.gz"
            ))
        },
        Fetcher::Local => Err(eyre!("Cannot prefetch local source")),
    }
}

/// Prefetch the tagged release archive.
///
/// Returns the SRI hash and, when `want_tree` is set, the store path of the
/// unpacked source.
pub fn prefetch_source(info: &ExpressionInfo, want_tree: bool) -> color_eyre::Result<Prefetched> {
    let tag = if info.version_has_v_prefix {
        format!("v{}", info.version)
    } else {
        info.version.clone()
    };
    let git_ref = match info.fetcher {
        Fetcher::GitHub { .. } => format!("refs/tags/{tag}"),
        _ => tag,
    };
    let url = archive_url(&info.fetcher, &git_ref)?;
    eprintln!("Prefetching source hash...");
    prefetch_url(&url, want_tree)
}

/// Download the default branch (`HEAD`) of a remote fetcher and return the
/// store path of the unpacked source. Used for project type detection when
/// no tagged release could be fetched.
pub fn prefetch_default_branch(fetcher: &Fetcher) -> color_eyre::Result<PathBuf> {
    let url = archive_url(fetcher, "HEAD")?;
    eprintln!("Downloading default branch for project detection...");
    let (_, path) = prefetch_with_nix(&url)?;
    Ok(path)
}

/// Prefetch the hash of the unpacked archive at `url`, as `fetchzip` (and so
/// `fetchFromGitHub`/`fetchFromGitLab`) computes it.
///
/// `iets prefetch` adds nothing to the store, so it is only used when the
/// unpacked tree is not needed. Otherwise, and as a fallback, this runs
/// `nix-prefetch-url`.
fn prefetch_url(url: &str, want_tree: bool) -> color_eyre::Result<Prefetched> {
    if !want_tree {
        if let Some(iets) = ekapkgs_nix::iets::binary() {
            match prefetch_with_iets(iets, url) {
                Ok(hash) => return Ok(Prefetched { hash, path: None }),
                Err(err) => tracing::warn!("iets prefetch failed, trying nix-prefetch-url: {err}"),
            }
        }
    }

    let (hash, path) = prefetch_with_nix(url)?;
    Ok(Prefetched {
        hash,
        path: Some(path),
    })
}

/// `iets prefetch --unpack` prints the SRI hash and adds nothing to the store.
fn prefetch_with_iets(iets: &Path, url: &str) -> color_eyre::Result<String> {
    let output = Command::new(iets)
        .arg("prefetch")
        .arg("--unpack")
        .arg(url)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("{}", stderr.trim()));
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

/// `nix-prefetch-url --unpack --print-path` prints a base-32 hash and the
/// store path of the unpacked tree; `nix hash` converts the hash to SRI.
fn prefetch_with_nix(url: &str) -> color_eyre::Result<(String, PathBuf)> {
    let output = Command::new("nix-prefetch-url")
        .arg("--unpack")
        .arg("--print-path")
        .arg("--type")
        .arg("sha256")
        .arg(url)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("nix-prefetch-url failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout)?;
    let mut lines = stdout.lines().map(str::trim);
    let hex_hash = lines
        .next()
        .filter(|l| !l.is_empty())
        .ok_or_else(|| eyre!("nix-prefetch-url printed no hash"))?;
    let path = lines
        .next()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| eyre!("nix-prefetch-url printed no store path"))?;

    // Convert to SRI format
    let sri_output = Command::new("nix")
        .arg("hash")
        .arg("to-sri")
        .arg("--type")
        .arg("sha256")
        .arg(hex_hash)
        .output()?;

    if !sri_output.status.success() {
        let stderr = String::from_utf8_lossy(&sri_output.stderr);
        return Err(eyre!("nix hash to-sri failed: {stderr}"));
    }

    Ok((
        String::from_utf8(sri_output.stdout)?.trim().to_owned(),
        path,
    ))
}
