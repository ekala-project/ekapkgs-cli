mod detect;
mod expression;
mod prefetch;
mod types;
mod url;

use std::path::{Path, PathBuf};

use types::{ExpressionInfo, Fetcher, TemplateKind};

use crate::cli::TemplateCommand;

pub fn execute(command: TemplateCommand) -> color_eyre::Result<()> {
    // `None` means auto-detect.
    let (explicit_kind, opts) = match command {
        TemplateCommand::Stdenv { opts } => (Some(TemplateKind::Stdenv), opts),
        TemplateCommand::Cmake { opts } => (Some(TemplateKind::Cmake), opts),
        TemplateCommand::Meson { opts } => (Some(TemplateKind::Meson), opts),
        TemplateCommand::Rust { opts } => (Some(TemplateKind::Rust), opts),
        TemplateCommand::Go { opts } => (Some(TemplateKind::Go), opts),
        TemplateCommand::Python { opts } => (Some(TemplateKind::Python), opts),
        TemplateCommand::Auto { opts } => (None, opts),
    };

    // When the path is a directory, write `default.nix` inside it and use it
    // as the project directory for auto-detection.
    let (output_path, project_dir) = resolve_paths(&opts.path)?;

    let mut info = ExpressionInfo::with_defaults(explicit_kind.unwrap_or(TemplateKind::Stdenv));

    // Apply CLI overrides
    if let Some(ref pname) = opts.pname {
        info.pname.clone_from(pname);
    }
    if let Some(ref version) = opts.version {
        info.version.clone_from(version);
    }
    if let Some(ref desc) = opts.description {
        info.description.clone_from(desc);
    }
    if let Some(ref license) = opts.license {
        info.license.clone_from(license);
    }

    // Unpacked remote source, used for auto-detection.
    let mut source_dir: Option<PathBuf> = None;

    // Fetch metadata from URL if provided
    if let Some(ref from_url) = opts.from_url {
        let fetcher = url::parse_url(from_url, opts.forge)?;
        info.fetcher = fetcher;

        match &info.fetcher {
            Fetcher::GitHub {
                owner,
                repo,
                domain,
            } => {
                let owner = owner.clone();
                let repo = repo.clone();
                let domain = domain.clone();
                if let Err(e) =
                    url::fetch_github_metadata(&owner, &repo, domain.as_deref(), &mut info)
                {
                    eprintln!("Warning: could not fetch GitHub metadata: {e}");
                }
            },
            Fetcher::GitLab {
                owner,
                repo,
                domain,
            } => {
                let owner = owner.clone();
                let repo = repo.clone();
                let domain = domain.clone();
                if let Err(e) =
                    url::fetch_gitlab_metadata(&owner, &repo, domain.as_deref(), &mut info)
                {
                    eprintln!("Warning: could not fetch GitLab metadata: {e}");
                }
            },
            Fetcher::Local => {},
        }

        // Prefetch source hash (and unpacked tree) for any remote fetcher
        if !matches!(info.fetcher, Fetcher::Local) {
            match prefetch::prefetch_source(&info, explicit_kind.is_none()) {
                Ok(prefetched) => {
                    info.src_hash = prefetched.hash;
                    source_dir = prefetched.path;
                },
                Err(e) => eprintln!("Warning: could not prefetch source hash: {e}"),
            }

            // No tagged release could be fetched; fall back to the default
            // branch so the project type can still be detected.
            if explicit_kind.is_none() && source_dir.is_none() {
                source_dir = Some(prefetch::prefetch_default_branch(&info.fetcher)?);
            }
        }
    }

    if explicit_kind.is_none() {
        let dir = source_dir.as_deref().unwrap_or(&project_dir);
        let detected = detect::detect_template(dir).ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "Could not auto-detect project type from files in {}. Use an explicit template \
                 subcommand (stdenv, cmake, meson, rust, go, python).",
                opts.from_url
                    .as_deref()
                    .map_or_else(|| dir.display().to_string(), str::to_owned)
            )
        })?;
        eprintln!("Auto-detected: {detected}");
        info.kind = detected;
    }
    let kind = info.kind;

    let expr = expression::generate(&info);

    if opts.stdout {
        print!("{expr}");
    } else {
        let path = output_path.as_path();
        if path.exists() {
            eprintln!("Warning: {} already exists, overwriting", path.display());
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &expr)?;
        eprintln!("Wrote {}", path.display());
    }

    if kind == TemplateKind::Python {
        eprintln!("Note: Python packages in core-pkgs live in python/pkgs/<name>/default.nix");
    }

    Ok(())
}

/// Resolve the output file and project directory from the positional path.
///
/// A directory (existing, or given with a trailing `/`) yields
/// `<dir>/default.nix` with `<dir>` as the project directory. Otherwise the
/// path is the output file and the current directory is the project directory.
fn resolve_paths(path: &str) -> color_eyre::Result<(PathBuf, PathBuf)> {
    let p = Path::new(path);
    if p.is_dir() || path.ends_with('/') {
        Ok((p.join("default.nix"), p.to_path_buf()))
    } else {
        Ok((p.to_path_buf(), std::env::current_dir()?))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn existing_directory_gets_default_nix() {
        let dir = TempDir::new().unwrap();
        let (out, project) = resolve_paths(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(out, dir.path().join("default.nix"));
        assert_eq!(project, dir.path());
    }

    #[test]
    fn trailing_slash_is_treated_as_directory() {
        let (out, project) = resolve_paths("pkgs/foo/").unwrap();
        assert_eq!(out, Path::new("pkgs/foo/default.nix"));
        assert_eq!(project, Path::new("pkgs/foo/"));
    }

    #[test]
    fn file_path_is_used_verbatim() {
        let (out, project) = resolve_paths("foo.nix").unwrap();
        assert_eq!(out, Path::new("foo.nix"));
        assert_eq!(project, std::env::current_dir().unwrap());
    }
}
