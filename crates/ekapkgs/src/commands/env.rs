use ekapkgs_nix::NixCommand;
use yansi::Paint;

use crate::cli::{EnvCommand, EnvHookShell};
use crate::config::{ENV_MANIFEST_NAME, EnvFlakeEntry, EnvManifest, EnvPackageEntry, TrustedEnvs};

pub fn execute(command: EnvCommand) -> color_eyre::Result<()> {
    match command {
        EnvCommand::Init { flake } => cmd_init(&flake),
        EnvCommand::Add { packages, flake } => cmd_add(&packages, flake.as_deref()),
        EnvCommand::Remove { packages } => cmd_remove(&packages),
        EnvCommand::FlakeAdd {
            ref_,
            devshell,
            rev,
            override_inputs,
        } => cmd_flake_add(&ref_, &devshell, rev.as_deref(), &override_inputs),
        EnvCommand::FlakeRemove { ref_ } => cmd_flake_remove(&ref_),
        EnvCommand::FlakePin { ref_, rev } => cmd_flake_pin(&ref_, rev.as_deref()),
        EnvCommand::List { json } => cmd_list(json),
        EnvCommand::Reload => cmd_reload(),
        EnvCommand::Allow => cmd_allow(),
        EnvCommand::Disallow => cmd_disallow(),
        EnvCommand::Hook { shell } => {
            cmd_hook(shell);
            Ok(())
        },
        EnvCommand::ProfileBin { dir } => cmd_profile_bin(&dir),
        EnvCommand::IsTrusted { dir } => cmd_is_trusted(&dir),
        EnvCommand::Fingerprint { dir } => {
            cmd_fingerprint(&dir);
            Ok(())
        },
        EnvCommand::ReloadHook { dir } => cmd_reload_hook(&dir),
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
    println!(
        "{}",
        "Add packages with `ekapkgs env add` or flakes with `ekapkgs env flake-add`".dim()
    );

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

fn cmd_flake_add(
    ref_: &str,
    devshell: &str,
    rev: Option<&str>,
    override_inputs: &[String],
) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let mut manifest = EnvManifest::load_from(&dir)?;

    let inputs: std::collections::HashMap<String, String> = override_inputs
        .iter()
        .filter_map(|s| {
            let (k, v) = s.split_once('=')?;
            Some((k.to_owned(), v.to_owned()))
        })
        .collect();

    let entry = EnvFlakeEntry {
        ref_: ref_.to_owned(),
        devshell: devshell.to_owned(),
        rev: rev.map(str::to_owned),
        inputs,
    };

    if !manifest.add_flake(entry) {
        println!("Flake {ref_} is already in the manifest");
        return Ok(());
    }

    manifest.save_to(&dir)?;
    println!("Added flake {ref_} to {ENV_MANIFEST_NAME}");
    println!(
        "{}",
        "Run `ekapkgs env reload` to build, then `ekapkgs env allow` to trust.".dim()
    );

    Ok(())
}

fn cmd_flake_remove(ref_: &str) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let mut manifest = EnvManifest::load_from(&dir)?;

    if !manifest.remove_flake(ref_) {
        tracing::warn!("Flake {ref_} is not in the manifest");
        return Ok(());
    }

    manifest.save_to(&dir)?;
    println!("Removed flake {ref_} from {ENV_MANIFEST_NAME}");

    Ok(())
}

fn cmd_flake_pin(ref_: &str, rev: Option<&str>) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let mut manifest = EnvManifest::load_from(&dir)?;

    let Some(entry) = manifest.flakes.iter_mut().find(|f| f.ref_ == ref_) else {
        return Err(color_eyre::eyre::eyre!(
            "Flake {ref_} is not in the manifest"
        ));
    };

    let pin_rev = match rev {
        Some(r) => r.to_owned(),
        None => {
            // Resolve the current revision via `nix flake metadata`.
            let output = NixCommand::new(&["flake", "metadata", "--json"])
                .arg(ref_)
                .output()?;
            let meta: serde_json::Value = serde_json::from_slice(&output.stdout)?;
            meta["revision"]
                .as_str()
                .or_else(|| meta["locked"]["rev"].as_str())
                .ok_or_else(|| color_eyre::eyre::eyre!("Could not resolve revision for {ref_}"))?
                .to_owned()
        },
    };

    entry.rev = Some(pin_rev.clone());
    manifest.save_to(&dir)?;
    println!("Pinned {ref_} to {pin_rev}");

    Ok(())
}

fn cmd_list(json_output: bool) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let manifest = EnvManifest::load_from(&dir)?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
        return Ok(());
    }

    let has_packages = !manifest.packages.is_empty();
    let has_flakes = !manifest.flakes.is_empty();

    if !has_packages && !has_flakes {
        println!("No packages or flakes in this environment.");
        println!(
            "{}",
            "Use `ekapkgs env add` or `ekapkgs env flake-add` to add one.".dim()
        );
        return Ok(());
    }

    if has_flakes {
        println!("{}:", "Flakes".bold());
        for entry in &manifest.flakes {
            let mut desc = format!("  {} ", entry.ref_.bold());
            if entry.devshell != "default" {
                desc.push_str(&format!("devshell={} ", entry.devshell));
            }
            if let Some(rev) = &entry.rev {
                let short = if rev.len() > 8 { &rev[..8] } else { rev };
                desc.push_str(&format!("{}", format!("(pinned {short})").dim()));
            }
            if !entry.inputs.is_empty() {
                for (k, v) in &entry.inputs {
                    desc.push_str(&format!(" {}", format!("{k}={v}").dim()));
                }
            }
            println!("{desc}");
        }
    }

    if has_packages {
        if has_flakes {
            println!();
        }
        println!("{}:", "Packages".bold());
        for entry in &manifest.packages {
            let flake_display = entry.flake.as_deref().unwrap_or(&manifest.flake);
            println!(
                "  {} {}",
                entry.name.bold(),
                format!("({flake_display})").dim()
            );
        }
    }

    let total = manifest.flakes.len() + manifest.packages.len();
    println!("\n{total} entry(ies)");

    Ok(())
}

fn cmd_hook(shell: EnvHookShell) {
    match shell {
        EnvHookShell::Bash => print!("{}", bash_hook()),
        EnvHookShell::Zsh => print!("{}", zsh_hook()),
        EnvHookShell::Fish => print!("{}", fish_hook()),
    }
}

fn cmd_allow() -> color_eyre::Result<()> {
    let dir = cwd()?;
    let manifest_path = dir.join(ENV_MANIFEST_NAME);

    if !manifest_path.exists() {
        return Err(color_eyre::eyre::eyre!(
            "No {ENV_MANIFEST_NAME} found in {}",
            dir.display()
        ));
    }

    let mut trusted = TrustedEnvs::load()?;
    trusted.allow(&dir)?;
    trusted.save()?;

    println!("Allowed {}", dir.canonicalize().unwrap_or(dir).display());

    Ok(())
}

fn cmd_disallow() -> color_eyre::Result<()> {
    let dir = cwd()?;

    let mut trusted = TrustedEnvs::load()?;
    trusted.disallow(&dir);
    trusted.save()?;

    println!("Disallowed {}", dir.canonicalize().unwrap_or(dir).display());

    Ok(())
}

fn cmd_profile_bin(dir: &str) -> color_eyre::Result<()> {
    let dir_path = std::path::Path::new(dir);

    // Only return the profile bin if the environment is trusted.
    let trusted = TrustedEnvs::load()?;
    if !trusted.is_trusted(dir_path) {
        return Ok(());
    }

    let profile = EnvManifest::profile_path(dir_path)?;
    let bin = profile.join("bin");
    if bin.is_dir() {
        println!("{}", bin.display());
    }
    Ok(())
}

fn cmd_is_trusted(dir: &str) -> color_eyre::Result<()> {
    let dir_path = std::path::Path::new(dir);
    let trusted = TrustedEnvs::load()?;
    if trusted.is_trusted(dir_path) {
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn cmd_reload() -> color_eyre::Result<()> {
    let dir = cwd()?;
    let manifest = EnvManifest::load_from(&dir)?;
    let profile = EnvManifest::profile_path(&dir)?;
    let profile_str = profile.to_string_lossy();

    sync_profile_from_manifest(&manifest, &profile_str, &dir)?;

    // Re-allow so the trust hash is up to date.
    let mut trusted = TrustedEnvs::load()?;
    trusted.allow(&dir)?;
    trusted.save()?;

    println!("Environment reloaded");
    Ok(())
}

fn cmd_fingerprint(dir: &str) {
    let dir_path = std::path::Path::new(dir);
    println!("{}", compute_fingerprint(dir_path));
}

fn cmd_reload_hook(dir: &str) -> color_eyre::Result<()> {
    let dir_path = std::path::Path::new(dir);

    let trusted = TrustedEnvs::load()?;
    if !trusted.is_trusted(dir_path) {
        return Ok(());
    }

    let manifest = EnvManifest::load_from(dir_path)?;
    let profile = EnvManifest::profile_path(dir_path)?;
    let profile_str = profile.to_string_lossy();

    sync_profile_from_manifest(&manifest, &profile_str, dir_path)?;

    let bin = profile.join("bin");
    if bin.is_dir() {
        println!("{}", bin.display());
    }
    Ok(())
}

/// Compute a fingerprint from mtimes of manifest, flake.nix, and flake.lock.
fn compute_fingerprint(dir: &std::path::Path) -> String {
    let mut hasher = blake3::Hasher::new();

    for name in &[ENV_MANIFEST_NAME, "flake.nix", "flake.lock"] {
        let path = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&path) {
            if let Ok(mtime) = meta.modified() {
                let dur = mtime
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                hasher.update(&dur.as_nanos().to_le_bytes());
            }
        }
    }

    let h = hasher.finalize();
    h.to_hex().as_str()[..32].to_owned()
}

/// Build the profile from manifest packages and flake dev shells.
#[allow(clippy::unnecessary_wraps)]
fn sync_profile_from_manifest(
    manifest: &EnvManifest,
    profile_str: &str,
    _dir: &std::path::Path,
) -> color_eyre::Result<()> {
    // Install each flake dev shell.
    for flake_entry in &manifest.flakes {
        let mut flake_ref = flake_entry.ref_.clone();
        if let Some(rev) = &flake_entry.rev {
            // Append ?rev= if not already present.
            if !flake_ref.contains('?') {
                flake_ref.push_str(&format!("?rev={rev}"));
            }
        }

        let installable = format!(
            "{flake_ref}#devShells.{}.{}",
            current_system(),
            flake_entry.devshell
        );
        tracing::info!("Installing flake dev shell {installable}...");

        let mut cmd = NixCommand::new(&["profile", "install"])
            .arg("--profile")
            .arg(profile_str);

        // Apply input overrides.
        for (name, value) in &flake_entry.inputs {
            cmd = cmd.arg("--override-input").arg(name).arg(value);
        }

        cmd = cmd.arg(&installable);

        if let Err(e) = cmd.stream() {
            tracing::warn!("Failed to install flake {}: {e}", flake_entry.ref_);
        }
    }

    // Install individual packages.
    for entry in &manifest.packages {
        let installable = manifest.resolve_installable(entry);
        tracing::info!("Installing {installable}...");
        if let Err(e) = NixCommand::new(&["profile", "install"])
            .arg("--profile")
            .arg(profile_str)
            .arg(&installable)
            .stream()
        {
            tracing::warn!("Failed to install {}: {e}", entry.name);
        }
    }

    Ok(())
}

fn current_system() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        "x86_64-linux"
    }
    #[cfg(target_arch = "aarch64")]
    {
        "aarch64-linux"
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        "x86_64-linux"
    }
}

fn bash_hook() -> &'static str {
    r#"# ekapkgs env hook for bash
# Add to ~/.bashrc:  eval "$(ekapkgs env hook bash)"

_ekapkgs_env_deactivate() {
    if [[ -n "${_EKAPKGS_ENV_PATH_BACKUP:-}" ]]; then
        export PATH="$_EKAPKGS_ENV_PATH_BACKUP"
        unset _EKAPKGS_ENV_PATH_BACKUP
    fi
    unset EKAPKGS_ENV
    unset _EKAPKGS_ENV_FINGERPRINT
}

_ekapkgs_env_activate() {
    local found_dir="$1"

    if ! ekapkgs env _is-trusted "$found_dir" 2>/dev/null; then
        if [[ "${_EKAPKGS_ENV_WARNED:-}" != "$found_dir" ]]; then
            echo "ekapkgs: $found_dir is blocked. Run \`ekapkgs env allow\` to approve." >&2
            _EKAPKGS_ENV_WARNED="$found_dir"
        fi
        return 1
    fi
    unset _EKAPKGS_ENV_WARNED

    local profile_bin
    profile_bin="$(ekapkgs env _profile-bin "$found_dir" 2>/dev/null)"
    if [[ -n "$profile_bin" && -d "$profile_bin" ]]; then
        export _EKAPKGS_ENV_PATH_BACKUP="$PATH"
        export PATH="$profile_bin:$PATH"
        export EKAPKGS_ENV="$found_dir"
        export _EKAPKGS_ENV_FINGERPRINT="$(ekapkgs env _fingerprint "$found_dir" 2>/dev/null)"
    fi
}

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
            # Switching directories — deactivate old, activate new.
            _ekapkgs_env_deactivate
            _ekapkgs_env_activate "$found_dir"
        elif [[ -n "$prev_env" ]]; then
            # Same directory — check if files changed (flake.nix, flake.lock, manifest).
            local cur_fp
            cur_fp="$(ekapkgs env _fingerprint "$found_dir" 2>/dev/null)"
            if [[ -n "$cur_fp" && "$cur_fp" != "${_EKAPKGS_ENV_FINGERPRINT:-}" ]]; then
                _ekapkgs_env_deactivate
                # Reload the profile in the background, then re-activate.
                local profile_bin
                profile_bin="$(ekapkgs env _reload "$found_dir" 2>/dev/null)"
                if [[ -n "$profile_bin" && -d "$profile_bin" ]]; then
                    export _EKAPKGS_ENV_PATH_BACKUP="$PATH"
                    export PATH="$profile_bin:$PATH"
                    export EKAPKGS_ENV="$found_dir"
                    export _EKAPKGS_ENV_FINGERPRINT="$cur_fp"
                fi
            fi
        fi
    else
        # No manifest found — deactivate if active.
        if [[ -n "$prev_env" ]]; then
            _ekapkgs_env_deactivate
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

_ekapkgs_env_deactivate() {
    if [[ -n "${_EKAPKGS_ENV_PATH_BACKUP:-}" ]]; then
        export PATH="$_EKAPKGS_ENV_PATH_BACKUP"
        unset _EKAPKGS_ENV_PATH_BACKUP
    fi
    unset EKAPKGS_ENV
    unset _EKAPKGS_ENV_FINGERPRINT
}

_ekapkgs_env_activate() {
    local found_dir="$1"

    if ! ekapkgs env _is-trusted "$found_dir" 2>/dev/null; then
        if [[ "${_EKAPKGS_ENV_WARNED:-}" != "$found_dir" ]]; then
            echo "ekapkgs: $found_dir is blocked. Run \`ekapkgs env allow\` to approve." >&2
            _EKAPKGS_ENV_WARNED="$found_dir"
        fi
        return 1
    fi
    unset _EKAPKGS_ENV_WARNED

    local profile_bin
    profile_bin="$(ekapkgs env _profile-bin "$found_dir" 2>/dev/null)"
    if [[ -n "$profile_bin" && -d "$profile_bin" ]]; then
        export _EKAPKGS_ENV_PATH_BACKUP="$PATH"
        export PATH="$profile_bin:$PATH"
        export EKAPKGS_ENV="$found_dir"
        export _EKAPKGS_ENV_FINGERPRINT="$(ekapkgs env _fingerprint "$found_dir" 2>/dev/null)"
    fi
}

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
            _ekapkgs_env_deactivate
            _ekapkgs_env_activate "$found_dir"
        elif [[ -n "$prev_env" ]]; then
            local cur_fp
            cur_fp="$(ekapkgs env _fingerprint "$found_dir" 2>/dev/null)"
            if [[ -n "$cur_fp" && "$cur_fp" != "${_EKAPKGS_ENV_FINGERPRINT:-}" ]]; then
                _ekapkgs_env_deactivate
                local profile_bin
                profile_bin="$(ekapkgs env _reload "$found_dir" 2>/dev/null)"
                if [[ -n "$profile_bin" && -d "$profile_bin" ]]; then
                    export _EKAPKGS_ENV_PATH_BACKUP="$PATH"
                    export PATH="$profile_bin:$PATH"
                    export EKAPKGS_ENV="$found_dir"
                    export _EKAPKGS_ENV_FINGERPRINT="$cur_fp"
                fi
            fi
        fi
    else
        if [[ -n "$prev_env" ]]; then
            _ekapkgs_env_deactivate
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

function _ekapkgs_env_deactivate
    if set -q _EKAPKGS_ENV_PATH_BACKUP
        set -gx PATH $_EKAPKGS_ENV_PATH_BACKUP
        set -e _EKAPKGS_ENV_PATH_BACKUP
    end
    set -e EKAPKGS_ENV
    set -e _EKAPKGS_ENV_FINGERPRINT
end

function _ekapkgs_env_activate
    set -l found_dir $argv[1]

    if not ekapkgs env _is-trusted "$found_dir" 2>/dev/null
        if test "$_EKAPKGS_ENV_WARNED" != "$found_dir"
            echo "ekapkgs: $found_dir is blocked. Run \`ekapkgs env allow\` to approve." >&2
            set -g _EKAPKGS_ENV_WARNED "$found_dir"
        end
        return 1
    end
    set -e _EKAPKGS_ENV_WARNED

    set -l profile_bin (ekapkgs env _profile-bin "$found_dir" 2>/dev/null)
    if test -n "$profile_bin"; and test -d "$profile_bin"
        set -gx _EKAPKGS_ENV_PATH_BACKUP $PATH
        set -gx PATH "$profile_bin" $PATH
        set -gx EKAPKGS_ENV "$found_dir"
        set -gx _EKAPKGS_ENV_FINGERPRINT (ekapkgs env _fingerprint "$found_dir" 2>/dev/null)
    end
end

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
            _ekapkgs_env_deactivate
            _ekapkgs_env_activate "$found_dir"
        else if test -n "$prev_env"
            set -l cur_fp (ekapkgs env _fingerprint "$found_dir" 2>/dev/null)
            if test -n "$cur_fp"; and test "$cur_fp" != "$_EKAPKGS_ENV_FINGERPRINT"
                _ekapkgs_env_deactivate
                set -l profile_bin (ekapkgs env _reload "$found_dir" 2>/dev/null)
                if test -n "$profile_bin"; and test -d "$profile_bin"
                    set -gx _EKAPKGS_ENV_PATH_BACKUP $PATH
                    set -gx PATH "$profile_bin" $PATH
                    set -gx EKAPKGS_ENV "$found_dir"
                    set -gx _EKAPKGS_ENV_FINGERPRINT "$cur_fp"
                end
            end
        end
    else
        if test -n "$prev_env"
            _ekapkgs_env_deactivate
        end
    end
end

# Run on shell start.
_ekapkgs_env_hook
"#
}
