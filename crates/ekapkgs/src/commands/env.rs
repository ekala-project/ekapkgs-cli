use ekapkgs_nix::NixCommand;
use yansi::Paint;

use crate::cli::{EnvCommand, EnvHookShell};
use crate::config::{ENV_MANIFEST_NAME, EnvManifest, EnvPackageEntry};

pub fn execute(command: EnvCommand) -> color_eyre::Result<()> {
    match command {
        EnvCommand::Init { flake } => cmd_init(&flake),
        EnvCommand::Add { packages, flake } => cmd_add(&packages, flake.as_deref()),
        EnvCommand::Remove { packages } => cmd_remove(&packages),
        EnvCommand::List { json } => cmd_list(json),
        EnvCommand::Hook { shell } => {
            cmd_hook(shell);
            Ok(())
        },
        EnvCommand::ProfileBin { dir } => cmd_profile_bin(&dir),
    }
}

fn cwd() -> color_eyre::Result<std::path::PathBuf> {
    std::env::current_dir()
        .map_err(|e| color_eyre::eyre::eyre!("failed to determine current directory: {e}"))
}

fn cmd_init(flake: &str) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let path = dir.join(ENV_MANIFEST_NAME);

    if path.exists() {
        return Err(color_eyre::eyre::eyre!(
            "{ENV_MANIFEST_NAME} already exists in {}",
            dir.display()
        ));
    }

    let manifest = EnvManifest {
        flake: flake.to_owned(),
        ..EnvManifest::default()
    };
    manifest.save_to(&dir)?;

    println!("Created {}", path.display());
    println!("{}", "Add packages with `ekapkgs env add <package>`".dim());

    Ok(())
}

fn cmd_add(packages: &[String], flake_override: Option<&str>) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let mut manifest = EnvManifest::load_from(&dir)?;
    let profile = EnvManifest::profile_path(&dir)?;
    let profile_str = profile.to_string_lossy();
    let mut added = 0u32;

    for name in packages {
        let entry = EnvPackageEntry {
            name: name.clone(),
            flake: flake_override.map(str::to_owned),
        };

        let installable = manifest.resolve_installable(&entry);

        if !manifest.add(entry) {
            tracing::warn!("{name} is already in the manifest, skipping");
            continue;
        }

        tracing::info!("Installing {installable}...");
        NixCommand::new(&["profile", "install"])
            .arg("--profile")
            .arg(profile_str.as_ref())
            .arg(&installable)
            .stream()?;

        added += 1;
    }

    manifest.save_to(&dir)?;

    if added > 0 {
        println!("Added {added} package(s) to {ENV_MANIFEST_NAME}");
    }

    Ok(())
}

fn cmd_remove(packages: &[String]) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let mut manifest = EnvManifest::load_from(&dir)?;
    let profile = EnvManifest::profile_path(&dir)?;
    let profile_str = profile.to_string_lossy();
    let mut removed = 0u32;

    for name in packages {
        if !manifest.remove(name) {
            tracing::warn!("{name} is not in the manifest, skipping");
            continue;
        }

        tracing::info!("Removing {name} from profile...");
        if let Err(e) = NixCommand::new(&["profile", "remove"])
            .arg("--profile")
            .arg(profile_str.as_ref())
            .arg(name)
            .stream()
        {
            tracing::warn!("Failed to remove {name} from nix profile: {e}");
        }

        removed += 1;
    }

    manifest.save_to(&dir)?;

    if removed > 0 {
        println!("Removed {removed} package(s)");
    }

    Ok(())
}

fn cmd_list(json_output: bool) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let manifest = EnvManifest::load_from(&dir)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&manifest.packages)?);
        return Ok(());
    }

    if manifest.packages.is_empty() {
        println!("No packages in this environment.");
        println!("{}", "Use `ekapkgs env add <package>` to add one.".dim());
        return Ok(());
    }

    for entry in &manifest.packages {
        let flake_display = entry.flake.as_deref().unwrap_or(&manifest.flake);
        println!(
            "  {} {}",
            entry.name.bold(),
            format!("({flake_display})").dim()
        );
    }
    println!("\n{} package(s)", manifest.packages.len());

    Ok(())
}

fn cmd_hook(shell: EnvHookShell) {
    match shell {
        EnvHookShell::Bash => print!("{}", bash_hook()),
        EnvHookShell::Zsh => print!("{}", zsh_hook()),
        EnvHookShell::Fish => print!("{}", fish_hook()),
    }
}

fn cmd_profile_bin(dir: &str) -> color_eyre::Result<()> {
    let dir_path = std::path::Path::new(dir);
    let profile = EnvManifest::profile_path(dir_path)?;
    let bin = profile.join("bin");
    if bin.is_dir() {
        println!("{}", bin.display());
    }
    Ok(())
}

fn bash_hook() -> &'static str {
    r#"# ekapkgs env hook for bash
# Add to ~/.bashrc:  eval "$(ekapkgs env hook bash)"

_ekapkgs_env_hook() {
    local manifest_name=".ekapkgs-env.toml"
    local prev_env="${EKAPKGS_ENV:-}"
    local cur_dir
    cur_dir="$(pwd)"

    # Walk up to find manifest.
    local check_dir="$cur_dir"
    local found_dir=""
    while true; do
        if [[ -f "$check_dir/$manifest_name" ]]; then
            found_dir="$check_dir"
            break
        fi
        local parent
        parent="$(dirname "$check_dir")"
        if [[ "$parent" == "$check_dir" ]]; then
            break
        fi
        check_dir="$parent"
    done

    if [[ -n "$found_dir" ]]; then
        if [[ "$found_dir" != "$prev_env" ]]; then
            # Deactivate previous environment if any.
            if [[ -n "$prev_env" && -n "${_EKAPKGS_ENV_PATH_BACKUP:-}" ]]; then
                export PATH="$_EKAPKGS_ENV_PATH_BACKUP"
            fi

            # Activate new environment.
            local profile_bin
            profile_bin="$(ekapkgs env _profile-bin "$found_dir" 2>/dev/null)"
            if [[ -n "$profile_bin" && -d "$profile_bin" ]]; then
                export _EKAPKGS_ENV_PATH_BACKUP="$PATH"
                export PATH="$profile_bin:$PATH"
                export EKAPKGS_ENV="$found_dir"
            fi
        fi
    else
        # No manifest found — deactivate if active.
        if [[ -n "$prev_env" && -n "${_EKAPKGS_ENV_PATH_BACKUP:-}" ]]; then
            export PATH="$_EKAPKGS_ENV_PATH_BACKUP"
            unset _EKAPKGS_ENV_PATH_BACKUP
            unset EKAPKGS_ENV
        fi
    fi
}

if [[ ";${PROMPT_COMMAND:-};" != *";_ekapkgs_env_hook;"* ]]; then
    PROMPT_COMMAND="_ekapkgs_env_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
"#
}

fn zsh_hook() -> &'static str {
    r#"# ekapkgs env hook for zsh
# Add to ~/.zshrc:  eval "$(ekapkgs env hook zsh)"

_ekapkgs_env_hook() {
    local manifest_name=".ekapkgs-env.toml"
    local prev_env="${EKAPKGS_ENV:-}"
    local cur_dir="$PWD"

    # Walk up to find manifest.
    local check_dir="$cur_dir"
    local found_dir=""
    while true; do
        if [[ -f "$check_dir/$manifest_name" ]]; then
            found_dir="$check_dir"
            break
        fi
        local parent="${check_dir:h}"
        if [[ "$parent" == "$check_dir" ]]; then
            break
        fi
        check_dir="$parent"
    done

    if [[ -n "$found_dir" ]]; then
        if [[ "$found_dir" != "$prev_env" ]]; then
            if [[ -n "$prev_env" && -n "${_EKAPKGS_ENV_PATH_BACKUP:-}" ]]; then
                export PATH="$_EKAPKGS_ENV_PATH_BACKUP"
            fi

            local profile_bin
            profile_bin="$(ekapkgs env _profile-bin "$found_dir" 2>/dev/null)"
            if [[ -n "$profile_bin" && -d "$profile_bin" ]]; then
                export _EKAPKGS_ENV_PATH_BACKUP="$PATH"
                export PATH="$profile_bin:$PATH"
                export EKAPKGS_ENV="$found_dir"
            fi
        fi
    else
        if [[ -n "$prev_env" && -n "${_EKAPKGS_ENV_PATH_BACKUP:-}" ]]; then
            export PATH="$_EKAPKGS_ENV_PATH_BACKUP"
            unset _EKAPKGS_ENV_PATH_BACKUP
            unset EKAPKGS_ENV
        fi
    fi
}

autoload -Uz add-zsh-hook
add-zsh-hook chpwd _ekapkgs_env_hook
_ekapkgs_env_hook
"#
}

fn fish_hook() -> &'static str {
    r#"# ekapkgs env hook for fish
# Add to ~/.config/fish/config.fish:  ekapkgs env hook fish | source

function _ekapkgs_env_hook --on-variable PWD
    set -l manifest_name ".ekapkgs-env.toml"
    set -l prev_env "$EKAPKGS_ENV"

    # Walk up to find manifest.
    set -l check_dir $PWD
    set -l found_dir ""
    while true
        if test -f "$check_dir/$manifest_name"
            set found_dir "$check_dir"
            break
        end
        set -l parent (dirname "$check_dir")
        if test "$parent" = "$check_dir"
            break
        end
        set check_dir "$parent"
    end

    if test -n "$found_dir"
        if test "$found_dir" != "$prev_env"
            # Deactivate previous.
            if test -n "$prev_env"; and set -q _EKAPKGS_ENV_PATH_BACKUP
                set -gx PATH $_EKAPKGS_ENV_PATH_BACKUP
            end

            # Activate new.
            set -l profile_bin (ekapkgs env _profile-bin "$found_dir" 2>/dev/null)
            if test -n "$profile_bin"; and test -d "$profile_bin"
                set -gx _EKAPKGS_ENV_PATH_BACKUP $PATH
                set -gx PATH "$profile_bin" $PATH
                set -gx EKAPKGS_ENV "$found_dir"
            end
        end
    else
        # Deactivate.
        if test -n "$prev_env"; and set -q _EKAPKGS_ENV_PATH_BACKUP
            set -gx PATH $_EKAPKGS_ENV_PATH_BACKUP
            set -e _EKAPKGS_ENV_PATH_BACKUP
            set -e EKAPKGS_ENV
        end
    end
end

# Run on shell start.
_ekapkgs_env_hook
"#
}
