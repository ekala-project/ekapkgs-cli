mod detect;
mod expression;
mod prefetch;
mod types;
mod url;

use std::path::Path;

use types::{ExpressionInfo, Fetcher, TemplateKind};

use crate::cli::TemplateCommand;

pub fn execute(command: TemplateCommand) -> color_eyre::Result<()> {
    let (kind, opts) = match command {
        TemplateCommand::Stdenv { opts } => (TemplateKind::Stdenv, opts),
        TemplateCommand::Cmake { opts } => (TemplateKind::Cmake, opts),
        TemplateCommand::Meson { opts } => (TemplateKind::Meson, opts),
        TemplateCommand::Rust { opts } => (TemplateKind::Rust, opts),
        TemplateCommand::Go { opts } => (TemplateKind::Go, opts),
        TemplateCommand::Python { opts } => (TemplateKind::Python, opts),
        TemplateCommand::Auto { opts } => {
            let dir = std::env::current_dir()?;
            let detected = detect::detect_template(&dir).ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "Could not auto-detect project type from files in {}. Use an explicit \
                     template subcommand (stdenv, cmake, meson, rust, go, python).",
                    dir.display()
                )
            })?;
            eprintln!("Auto-detected: {detected}");
            (detected, opts)
        },
    };

    let mut info = ExpressionInfo::with_defaults(kind);

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

    // Fetch metadata from URL if provided
    if let Some(ref from_url) = opts.from_url {
        let fetcher = url::parse_url(from_url)?;
        info.fetcher = fetcher;

        if let Fetcher::GitHub {
            ref owner,
            ref repo,
        } = info.fetcher
        {
            let owner = owner.clone();
            let repo = repo.clone();
            if let Err(e) = url::fetch_github_metadata(&owner, &repo, &mut info) {
                eprintln!("Warning: could not fetch GitHub metadata: {e}");
            }

            // Prefetch source hash
            match prefetch::prefetch_source_hash(&info) {
                Ok(hash) => info.src_hash = hash,
                Err(e) => eprintln!("Warning: could not prefetch source hash: {e}"),
            }
        }
    }

    let expr = expression::generate(&info);

    if opts.stdout {
        print!("{expr}");
    } else {
        let path = Path::new(&opts.path);
        if path.exists() {
            eprintln!("Warning: {} already exists, overwriting", path.display());
        }
        std::fs::write(path, &expr)?;
        eprintln!("Wrote {}", path.display());
    }

    if kind == TemplateKind::Python {
        eprintln!("Note: Python packages in core-pkgs live in python/pkgs/<name>/default.nix");
    }

    Ok(())
}
