use std::path::Path;
use std::process::Command;

use color_eyre::eyre::eyre;

use super::types::{ExpressionInfo, Fetcher};

/// Prefetch the hash of the unpacked source archive, as `fetchzip` (and so
/// `fetchFromGitHub`/`fetchFromGitLab`) computes it.
///
/// Uses `iets prefetch` when iets is installed, otherwise `nix-prefetch-url`.
/// Returns the SRI hash string (e.g., `sha256-...`).
pub fn prefetch_source_hash(info: &ExpressionInfo) -> color_eyre::Result<String> {
    let tag = if info.version_has_v_prefix {
        format!("v{}", info.version)
    } else {
        info.version.clone()
    };

    let url = match &info.fetcher {
        Fetcher::GitHub {
            owner,
            repo,
            domain,
        } => {
            let host = domain.as_deref().unwrap_or("github.com");
            format!(
                "https://{host}/{owner}/{repo}/archive/refs/tags/{tag}.tar.gz",
                host = host,
                owner = owner,
                repo = repo,
                tag = tag,
            )
        },
        Fetcher::GitLab {
            owner,
            repo,
            domain,
        } => {
            let host = domain.as_deref().unwrap_or("gitlab.com");
            format!(
                "https://{host}/{owner}/{repo}/-/archive/{tag}/{repo}-{tag}.tar.gz",
                host = host,
                owner = owner,
                repo = repo,
                tag = tag,
            )
        },
        Fetcher::Local => return Err(eyre!("Cannot prefetch hash for local source")),
    };

    eprintln!("Prefetching source hash...");

    if let Some(iets) = ekapkgs_nix::iets::binary() {
        match prefetch_with_iets(iets, &url) {
            Ok(hash) => return Ok(hash),
            Err(err) => tracing::warn!("iets prefetch failed, trying nix-prefetch-url: {err}"),
        }
    }

    prefetch_with_nix(&url)
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

/// `nix-prefetch-url --unpack` prints a base-32 hash, which `nix hash`
/// converts to SRI.
fn prefetch_with_nix(url: &str) -> color_eyre::Result<String> {
    let output = Command::new("nix-prefetch-url")
        .arg("--unpack")
        .arg("--type")
        .arg("sha256")
        .arg(url)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("nix-prefetch-url failed: {stderr}"));
    }

    let hex_hash = String::from_utf8(output.stdout)?.trim().to_owned();

    // Convert to SRI format
    let sri_output = Command::new("nix")
        .arg("hash")
        .arg("to-sri")
        .arg("--type")
        .arg("sha256")
        .arg(&hex_hash)
        .output()?;

    if !sri_output.status.success() {
        let stderr = String::from_utf8_lossy(&sri_output.stderr);
        return Err(eyre!("nix hash to-sri failed: {stderr}"));
    }

    Ok(String::from_utf8(sri_output.stdout)?.trim().to_owned())
}
