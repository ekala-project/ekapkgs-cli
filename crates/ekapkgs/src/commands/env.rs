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
        EnvCommand::HasDevshell { dir, shell } => cmd_has_devshell(&dir, shell),
        EnvCommand::RenderEnv { dir, shell } => cmd_render_env(&dir, shell),
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
    let (mut manifest, _lock) = EnvManifest::load_from_locked(&dir)?;
    let profile = EnvManifest::profile_path(&dir)?;
    let profile_str = profile.to_string_lossy();
    let mut added = 0u32;

    for name in packages {
        if manifest.packages.iter().any(|p| p.name == *name) {
            tracing::warn!("{name} is already in the manifest, skipping");
            continue;
        }

        let entry = EnvPackageEntry {
            name: name.clone(),
            flake: flake_override.map(str::to_owned),
        };
        let installable = manifest.resolve_installable(&entry);

        // Install to the nix profile first — only record in the manifest
        // after the profile mutation succeeds.
        tracing::info!("Installing {installable}...");
        NixCommand::new(&["profile", "install"])
            .arg("--profile")
            .arg(profile_str.as_ref())
            .arg(&installable)
            .stream()?;

        manifest.add(entry);
        manifest.save_to(&dir)?;
        added += 1;
    }

    if added > 0 {
        println!("Added {added} package(s) to {ENV_MANIFEST_NAME}");
    }

    Ok(())
}

fn cmd_remove(packages: &[String]) -> color_eyre::Result<()> {
    let dir = cwd()?;
    let (mut manifest, _lock) = EnvManifest::load_from_locked(&dir)?;
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
    let (mut manifest, _lock) = EnvManifest::load_from_locked(&dir)?;

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
    let (mut manifest, _lock) = EnvManifest::load_from_locked(&dir)?;

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
    let (mut manifest, _lock) = EnvManifest::load_from_locked(&dir)?;

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

    // If the manifest has flakes, render the dev shell environment scripts.
    if !manifest.flakes.is_empty() {
        tracing::info!("Rendering dev shell environment...");
        render_dev_env(&manifest, &dir)?;
    }

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

fn cmd_has_devshell(dir: &str, shell: crate::cli::EnvHookShell) -> color_eyre::Result<()> {
    let dir_path = std::path::Path::new(dir);

    // Must be trusted.
    let trusted = TrustedEnvs::load()?;
    if !trusted.is_trusted(dir_path) {
        std::process::exit(1);
    }

    // Must have flakes in manifest.
    let manifest = EnvManifest::load_from(dir_path)?;
    if manifest.flakes.is_empty() {
        std::process::exit(1);
    }

    let shell_name = match shell {
        crate::cli::EnvHookShell::Bash => "bash",
        crate::cli::EnvHookShell::Zsh => "zsh",
        crate::cli::EnvHookShell::Fish => "fish",
    };

    if let Some(script) = fresh_env_script(dir_path, shell_name) {
        println!("{}", script.display());
        Ok(())
    } else {
        std::process::exit(1);
    }
}

fn cmd_render_env(dir: &str, shell: crate::cli::EnvHookShell) -> color_eyre::Result<()> {
    let dir_path = std::path::Path::new(dir);

    let trusted = TrustedEnvs::load()?;
    if !trusted.is_trusted(dir_path) {
        return Err(color_eyre::eyre::eyre!("Directory is not trusted"));
    }

    let manifest = EnvManifest::load_from(dir_path)?;
    if manifest.flakes.is_empty() {
        return Err(color_eyre::eyre::eyre!("No flakes in manifest"));
    }

    // Build the profile first (for packages).
    let profile = EnvManifest::profile_path(dir_path)?;
    let profile_str = profile.to_string_lossy();
    sync_profile_from_manifest(&manifest, &profile_str, dir_path)?;

    // Render the dev shell env scripts.
    render_dev_env(&manifest, dir_path)?;

    let shell_name = match shell {
        crate::cli::EnvHookShell::Bash => "bash",
        crate::cli::EnvHookShell::Zsh => "zsh",
        crate::cli::EnvHookShell::Fish => "fish",
    };

    let env_dir = EnvManifest::env_dir_path(dir_path)?;
    let script = env_dir.join(format!("env.{shell_name}"));
    println!("{}", script.display());
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
///
/// Removes the existing profile first so stale entries from packages or
/// flakes that were removed from the manifest don't accumulate.
#[allow(clippy::unnecessary_wraps)]
fn sync_profile_from_manifest(
    manifest: &EnvManifest,
    profile_str: &str,
    _dir: &std::path::Path,
) -> color_eyre::Result<()> {
    // Remove the existing profile so we start fresh.  This ensures
    // packages/flakes removed from the manifest are also removed from
    // the profile rather than accumulating as stale entries.
    let profile_path = std::path::Path::new(profile_str);
    if profile_path.exists() {
        let _ = std::fs::remove_file(profile_path);
    }
    // Also remove the profile lock file that nix creates alongside it.
    let lock_path = profile_path.with_extension("lock");
    if lock_path.exists() {
        let _ = std::fs::remove_file(&lock_path);
    }

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

// ---------------------------------------------------------------------------
// Dev shell rendering (nix print-dev-env)
// ---------------------------------------------------------------------------

/// Parsed output from `nix print-dev-env --json`.
#[derive(Debug, serde::Deserialize)]
struct PrintDevEnvOutput {
    #[serde(default)]
    variables: std::collections::HashMap<String, DevEnvVar>,
    #[serde(default, rename = "bashFunctions")]
    bash_functions: std::collections::HashMap<String, DevEnvFunc>,
}

#[derive(Debug, serde::Deserialize)]
struct DevEnvVar {
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    value: serde_json::Value,
}

#[derive(Debug, serde::Deserialize)]
struct DevEnvFunc {
    #[serde(default)]
    text: String,
}

/// Variables that must not be overwritten by the rendered env script.
const SKIP_VARIABLES: &[&str] = &[
    // User session
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
    "DBUS_SESSION_BUS_ADDRESS",
    "GPG_AGENT_INFO",
    "TMPDIR",
    "TMP",
    "TEMP",
    "PWD",
    "OLDPWD",
    "SHLVL",
    "_",
    // Bash internals
    "BASH",
    "BASHOPTS",
    "BASH_VERSION",
    "BASH_VERSINFO",
    "SHELLOPTS",
    "IFS",
    "LINENO",
    "MACHTYPE",
    "HOSTTYPE",
    "OSTYPE",
    "OPTERR",
    "UID",
    "EUID",
    "PPID",
    "RANDOM",
    "SECONDS",
    "COLUMNS",
    "LINES",
    // Nix build internals
    "NIX_BUILD_TOP",
    "NIX_STORE",
    "NIX_LOG_FD",
    "NIX_BUILD_CORES",
    "NIX_ATTRS_JSON_FILE",
    "NIX_ATTRS_SH_FILE",
];

/// Render dev shell environment scripts for all flake entries in the manifest.
///
/// Runs `nix print-dev-env --json` for each flake, merges the results, and
/// writes shell-specific sourceable scripts to the env cache directory.
fn render_dev_env(manifest: &EnvManifest, dir: &std::path::Path) -> color_eyre::Result<()> {
    if manifest.flakes.is_empty() {
        return Ok(());
    }

    let env_dir = EnvManifest::env_dir_path(dir)?;
    let profile = EnvManifest::profile_path(dir)?;
    let profile_bin = profile.join("bin");

    // Collect environments from all flake entries.
    let mut merged_vars: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new(); // name → (type, value_string)
    let mut merged_path_parts: Vec<String> = Vec::new();
    let mut merged_functions: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    let skip: std::collections::HashSet<&str> = SKIP_VARIABLES.iter().copied().collect();

    for flake_entry in &manifest.flakes {
        let mut flake_ref = flake_entry.ref_.clone();
        if let Some(rev) = &flake_entry.rev {
            if !flake_ref.contains('?') {
                flake_ref.push_str(&format!("?rev={rev}"));
            }
        }

        let installable = format!(
            "{flake_ref}#devShells.{}.{}",
            current_system(),
            flake_entry.devshell
        );

        tracing::info!("Rendering dev shell for {installable}...");

        let mut cmd = NixCommand::new(&["print-dev-env", "--json"]);
        for (name, value) in &flake_entry.inputs {
            cmd = cmd.arg("--override-input").arg(name).arg(value);
        }
        cmd = cmd.arg(&installable);

        let output: PrintDevEnvOutput = cmd.json()?;

        // Merge variables.
        for (name, var) in output.variables {
            if skip.contains(name.as_str()) {
                continue;
            }
            let val_str = match &var.value {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if name == "PATH" {
                // Collect PATH parts for merging.
                for p in val_str.split(':') {
                    if !p.is_empty() && !merged_path_parts.contains(&p.to_owned()) {
                        merged_path_parts.push(p.to_owned());
                    }
                }
            } else if var.type_ == "exported" || var.type_ == "var" {
                merged_vars.insert(name, (var.type_.clone(), val_str));
            }
        }

        // Merge functions.
        for (name, func) in output.bash_functions {
            merged_functions.insert(name, func.text);
        }
    }

    // Prepend profile bin if it exists (for packages alongside flakes).
    if profile_bin.is_dir() {
        let bin_str = profile_bin.to_string_lossy().into_owned();
        if !merged_path_parts.contains(&bin_str) {
            merged_path_parts.insert(0, bin_str);
        }
    }

    // Write bash/zsh script.
    let mut bash_lines: Vec<String> = Vec::new();
    bash_lines.push("# Generated by ekapkgs env — do not edit".to_owned());
    bash_lines.push(String::new());

    // Export variables.
    for (name, (type_, value)) in &merged_vars {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        if type_ == "exported" {
            bash_lines.push(format!("export {name}=\"{escaped}\""));
        } else {
            bash_lines.push(format!("{name}=\"{escaped}\""));
        }
    }

    // PATH with user's original PATH appended.
    if !merged_path_parts.is_empty() {
        let dev_path = merged_path_parts.join(":");
        bash_lines.push(format!("export PATH=\"{dev_path}:${{PATH}}\""));
    }

    bash_lines.push("export IN_NIX_SHELL=impure".to_owned());
    bash_lines.push(String::new());

    // Bash functions.
    for (name, body) in &merged_functions {
        bash_lines.push(format!("{name}() {body}"));
    }

    let bash_content = bash_lines.join("\n");
    std::fs::write(env_dir.join("env.bash"), &bash_content)?;
    std::fs::write(env_dir.join("env.zsh"), &bash_content)?;

    // Write fish script.
    let mut fish_lines: Vec<String> = Vec::new();
    fish_lines.push("# Generated by ekapkgs env — do not edit".to_owned());
    fish_lines.push(String::new());

    for (name, (type_, value)) in &merged_vars {
        let escaped = value.replace('\\', "\\\\").replace('\'', "\\'");
        if type_ == "exported" {
            fish_lines.push(format!("set -gx {name} '{escaped}'"));
        } else {
            fish_lines.push(format!("set -g {name} '{escaped}'"));
        }
    }

    if !merged_path_parts.is_empty() {
        let parts: Vec<String> = merged_path_parts.iter().map(|p| format!("'{p}'")).collect();
        fish_lines.push(format!("set -gx PATH {} $PATH", parts.join(" ")));
    }

    fish_lines.push("set -gx IN_NIX_SHELL impure".to_owned());

    let fish_content = fish_lines.join("\n");
    std::fs::write(env_dir.join("env.fish"), &fish_content)?;

    // Write fingerprint for staleness checks.
    let fp = compute_fingerprint(dir);
    std::fs::write(env_dir.join("env.fingerprint"), &fp)?;

    Ok(())
}

/// Check if the rendered env scripts are fresh (fingerprint matches).
fn is_env_fresh(dir: &std::path::Path) -> bool {
    let Ok(env_dir) = EnvManifest::env_dir_path(dir) else {
        return false;
    };
    let fp_file = env_dir.join("env.fingerprint");
    let current_fp = compute_fingerprint(dir);
    match std::fs::read_to_string(&fp_file) {
        Ok(stored) => stored.trim() == current_fp,
        Err(_) => false,
    }
}

/// Get the path to a rendered env script if it exists and is fresh.
fn fresh_env_script(dir: &std::path::Path, shell: &str) -> Option<std::path::PathBuf> {
    if !is_env_fresh(dir) {
        return None;
    }
    let env_dir = EnvManifest::env_dir_path(dir).ok()?;
    let script = env_dir.join(format!("env.{shell}"));
    if script.exists() { Some(script) } else { None }
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

_ekapkgs_env_spawn_child() {
    local found_dir="$1"
    local env_script="$2"

    # Prevent recursive spawning for the same directory.
    [[ "${_EKAPKGS_ENV_CHILD:-}" == "$found_dir" ]] && return 0

    export _EKAPKGS_ENV_SPAWNING="$found_dir"

    bash --rcfile <(cat <<EKAPKGS_RCEOF
# Source user's bashrc for aliases/completions/prompt.
[[ -f ~/.bashrc ]] && source ~/.bashrc

# Source the pre-rendered dev shell environment.
source "$env_script"

# Add profile bin to PATH (for packages alongside flakes).
_ekapkgs_profile_bin="\$(ekapkgs env _profile-bin "$found_dir" 2>/dev/null)"
if [[ -n "\$_ekapkgs_profile_bin" && -d "\$_ekapkgs_profile_bin" ]]; then
    export PATH="\$_ekapkgs_profile_bin:\$PATH"
fi
unset _ekapkgs_profile_bin

export EKAPKGS_ENV="$found_dir"
export _EKAPKGS_ENV_CHILD="$found_dir"
export _EKAPKGS_ENV_FINGERPRINT="\$(ekapkgs env _fingerprint "$found_dir" 2>/dev/null)"

_ekapkgs_child_hook() {
    local cur_dir
    cur_dir="\$(pwd)"
    case "\$cur_dir" in
        "$found_dir"|"$found_dir"/*)
            # Still inside the env directory — check for fingerprint changes.
            local cur_fp
            cur_fp="\$(ekapkgs env _fingerprint "$found_dir" 2>/dev/null)"
            if [[ -n "\$cur_fp" && "\$cur_fp" != "\${_EKAPKGS_ENV_FINGERPRINT:-}" ]]; then
                echo "ekapkgs: environment changed, reloading..." >&2
                local new_script
                new_script="\$(ekapkgs env _render-env "$found_dir" bash 2>/dev/null)"
                if [[ -n "\$new_script" && -f "\$new_script" ]]; then
                    source "\$new_script"
                    export _EKAPKGS_ENV_FINGERPRINT="\$cur_fp"
                fi
            fi
            ;;
        *)
            # Left the directory tree — exit the child shell.
            exit 0
            ;;
    esac
}

if [[ ";\${PROMPT_COMMAND:-};" != *";_ekapkgs_child_hook;"* ]]; then
    PROMPT_COMMAND="_ekapkgs_child_hook\${PROMPT_COMMAND:+;\$PROMPT_COMMAND}"
fi

echo "ekapkgs: dev shell active for $found_dir" >&2
EKAPKGS_RCEOF
)
    local rc=$?

    unset _EKAPKGS_ENV_SPAWNING
    # If child exited while still in the dir (user typed exit), set cooldown.
    case "$(pwd)" in
        "$found_dir"|"$found_dir"/*) _EKAPKGS_ENV_COOLDOWN="$found_dir" ;;
    esac
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

    # Check for a pre-rendered dev shell (flake-containing manifest).
    local env_script
    env_script="$(ekapkgs env _has-devshell "$found_dir" bash 2>/dev/null)"

    if [[ -n "$env_script" ]]; then
        _ekapkgs_env_spawn_child "$found_dir" "$env_script"
        return
    fi

    # Lazy render: manifest has flakes but no cached script yet.
    if grep -q '^\[\[flakes\]\]' "$found_dir/.ekapkgs-env.toml" 2>/dev/null; then
        echo "ekapkgs: rendering dev shell (first time, may take a moment)..." >&2
        env_script="$(ekapkgs env _render-env "$found_dir" bash 2>/dev/null)"
        if [[ -n "$env_script" && -f "$env_script" ]]; then
            _ekapkgs_env_spawn_child "$found_dir" "$env_script"
            return
        fi
    fi

    # Packages-only mode: PATH modification.
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
    # Guards: don't run inside a child shell or during spawning.
    [[ -n "${_EKAPKGS_ENV_CHILD:-}" ]] && return 0
    [[ -n "${_EKAPKGS_ENV_SPAWNING:-}" ]] && return 0

    # Cooldown: user typed 'exit' while in the dir, don't re-spawn.
    if [[ -n "${_EKAPKGS_ENV_COOLDOWN:-}" ]]; then
        case "$(pwd)" in
            "${_EKAPKGS_ENV_COOLDOWN}"|"${_EKAPKGS_ENV_COOLDOWN}"/*) return 0 ;;
            *) unset _EKAPKGS_ENV_COOLDOWN ;;
        esac
    fi

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

_ekapkgs_env_spawn_child() {
    local found_dir="$1"
    local env_script="$2"

    [[ "${_EKAPKGS_ENV_CHILD:-}" == "$found_dir" ]] && return 0

    export _EKAPKGS_ENV_SPAWNING="$found_dir"

    local tmpdir
    tmpdir="$(mktemp -d)"
    local real_zdotdir="${ZDOTDIR:-$HOME}"

    cat > "$tmpdir/.zshrc" <<EKAPKGS_RCEOF
# Source user's zshrc.
[[ -f "$real_zdotdir/.zshrc" ]] && ZDOTDIR="$real_zdotdir" source "$real_zdotdir/.zshrc"

# Source the pre-rendered dev shell environment.
source "$env_script"

# Add profile bin to PATH.
_ekapkgs_profile_bin="\$(ekapkgs env _profile-bin "$found_dir" 2>/dev/null)"
if [[ -n "\$_ekapkgs_profile_bin" && -d "\$_ekapkgs_profile_bin" ]]; then
    export PATH="\$_ekapkgs_profile_bin:\$PATH"
fi
unset _ekapkgs_profile_bin

export EKAPKGS_ENV="$found_dir"
export _EKAPKGS_ENV_CHILD="$found_dir"
export _EKAPKGS_ENV_FINGERPRINT="\$(ekapkgs env _fingerprint "$found_dir" 2>/dev/null)"

_ekapkgs_child_chpwd() {
    case "\$PWD" in
        "$found_dir"|"$found_dir"/*)
            local cur_fp
            cur_fp="\$(ekapkgs env _fingerprint "$found_dir" 2>/dev/null)"
            if [[ -n "\$cur_fp" && "\$cur_fp" != "\${_EKAPKGS_ENV_FINGERPRINT:-}" ]]; then
                echo "ekapkgs: environment changed, reloading..." >&2
                local new_script
                new_script="\$(ekapkgs env _render-env "$found_dir" zsh 2>/dev/null)"
                if [[ -n "\$new_script" && -f "\$new_script" ]]; then
                    source "\$new_script"
                    export _EKAPKGS_ENV_FINGERPRINT="\$cur_fp"
                fi
            fi
            ;;
        *)
            exit 0
            ;;
    esac
}
autoload -Uz add-zsh-hook
add-zsh-hook chpwd _ekapkgs_child_chpwd

echo "ekapkgs: dev shell active for $found_dir" >&2
EKAPKGS_RCEOF

    ZDOTDIR="$tmpdir" zsh
    rm -rf "$tmpdir"

    unset _EKAPKGS_ENV_SPAWNING
    case "$(pwd)" in
        "$found_dir"|"$found_dir"/*) _EKAPKGS_ENV_COOLDOWN="$found_dir" ;;
    esac
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

    local env_script
    env_script="$(ekapkgs env _has-devshell "$found_dir" zsh 2>/dev/null)"

    if [[ -n "$env_script" ]]; then
        _ekapkgs_env_spawn_child "$found_dir" "$env_script"
        return
    fi

    if grep -q '^\[\[flakes\]\]' "$found_dir/.ekapkgs-env.toml" 2>/dev/null; then
        echo "ekapkgs: rendering dev shell (first time, may take a moment)..." >&2
        env_script="$(ekapkgs env _render-env "$found_dir" zsh 2>/dev/null)"
        if [[ -n "$env_script" && -f "$env_script" ]]; then
            _ekapkgs_env_spawn_child "$found_dir" "$env_script"
            return
        fi
    fi

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
    [[ -n "${_EKAPKGS_ENV_CHILD:-}" ]] && return 0
    [[ -n "${_EKAPKGS_ENV_SPAWNING:-}" ]] && return 0

    if [[ -n "${_EKAPKGS_ENV_COOLDOWN:-}" ]]; then
        case "$PWD" in
            "${_EKAPKGS_ENV_COOLDOWN}"|"${_EKAPKGS_ENV_COOLDOWN}"/*) return 0 ;;
            *) unset _EKAPKGS_ENV_COOLDOWN ;;
        esac
    fi

    local manifest_name=".ekapkgs-env.toml"
    local prev_env="${EKAPKGS_ENV:-}"
    local cur_dir="$PWD"

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

function _ekapkgs_env_spawn_child
    set -l found_dir $argv[1]
    set -l env_script $argv[2]

    if set -q _EKAPKGS_ENV_CHILD
        if test "$_EKAPKGS_ENV_CHILD" = "$found_dir"
            return 0
        end
    end

    set -gx _EKAPKGS_ENV_SPAWNING "$found_dir"

    fish --init-command "
        source '$env_script'

        set -l _ekapkgs_profile_bin (ekapkgs env _profile-bin '$found_dir' 2>/dev/null)
        if test -n \"\$_ekapkgs_profile_bin\"; and test -d \"\$_ekapkgs_profile_bin\"
            set -gx PATH \"\$_ekapkgs_profile_bin\" \$PATH
        end

        set -gx EKAPKGS_ENV '$found_dir'
        set -gx _EKAPKGS_ENV_CHILD '$found_dir'
        set -gx _EKAPKGS_ENV_FINGERPRINT (ekapkgs env _fingerprint '$found_dir' 2>/dev/null)

        function _ekapkgs_child_hook --on-variable PWD
            switch \$PWD
                case '$found_dir' '$found_dir/*'
                    set -l cur_fp (ekapkgs env _fingerprint '$found_dir' 2>/dev/null)
                    if test -n \"\$cur_fp\"; and test \"\$cur_fp\" != \"\$_EKAPKGS_ENV_FINGERPRINT\"
                        echo 'ekapkgs: environment changed, reloading...' >&2
                        set -l new_script (ekapkgs env _render-env '$found_dir' fish 2>/dev/null)
                        if test -n \"\$new_script\"; and test -f \"\$new_script\"
                            source \"\$new_script\"
                            set -gx _EKAPKGS_ENV_FINGERPRINT \"\$cur_fp\"
                        end
                    end
                case '*'
                    exit 0
            end
        end

        echo 'ekapkgs: dev shell active for $found_dir' >&2
    "

    set -e _EKAPKGS_ENV_SPAWNING
    switch (pwd)
        case "$found_dir" "$found_dir/*"
            set -g _EKAPKGS_ENV_COOLDOWN "$found_dir"
    end
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

    set -l env_script (ekapkgs env _has-devshell "$found_dir" fish 2>/dev/null)

    if test -n "$env_script"
        _ekapkgs_env_spawn_child "$found_dir" "$env_script"
        return
    end

    if grep -q '^\[\[flakes\]\]' "$found_dir/.ekapkgs-env.toml" 2>/dev/null
        echo "ekapkgs: rendering dev shell (first time, may take a moment)..." >&2
        set env_script (ekapkgs env _render-env "$found_dir" fish 2>/dev/null)
        if test -n "$env_script"; and test -f "$env_script"
            _ekapkgs_env_spawn_child "$found_dir" "$env_script"
            return
        end
    end

    set -l profile_bin (ekapkgs env _profile-bin "$found_dir" 2>/dev/null)
    if test -n "$profile_bin"; and test -d "$profile_bin"
        set -gx _EKAPKGS_ENV_PATH_BACKUP $PATH
        set -gx PATH "$profile_bin" $PATH
        set -gx EKAPKGS_ENV "$found_dir"
        set -gx _EKAPKGS_ENV_FINGERPRINT (ekapkgs env _fingerprint "$found_dir" 2>/dev/null)
    end
end

function _ekapkgs_env_hook --on-variable PWD
    if set -q _EKAPKGS_ENV_CHILD
        return 0
    end
    if set -q _EKAPKGS_ENV_SPAWNING
        return 0
    end

    if set -q _EKAPKGS_ENV_COOLDOWN
        switch (pwd)
            case "$_EKAPKGS_ENV_COOLDOWN" "$_EKAPKGS_ENV_COOLDOWN/*"
                return 0
            case '*'
                set -e _EKAPKGS_ENV_COOLDOWN
        end
    end

    set -l manifest_name ".ekapkgs-env.toml"
    set -l prev_env "$EKAPKGS_ENV"

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ENV_MANIFEST_NAME, EnvManifest, EnvPackageEntry};

    #[test]
    fn fingerprint_stable_for_same_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest = EnvManifest::default();
        manifest.save_to(dir.path()).unwrap();

        let fp1 = compute_fingerprint(dir.path());
        let fp2 = compute_fingerprint(dir.path());
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_changes_on_manifest_edit() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut manifest = EnvManifest::default();
        manifest.save_to(dir.path()).unwrap();

        let fp1 = compute_fingerprint(dir.path());

        // Wait briefly so mtime differs.
        std::thread::sleep(std::time::Duration::from_millis(50));

        manifest.add(EnvPackageEntry {
            name: "jq".into(),
            flake: None,
        });
        manifest.save_to(dir.path()).unwrap();

        let fp2 = compute_fingerprint(dir.path());
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn fingerprint_changes_on_flake_nix_creation() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest = EnvManifest::default();
        manifest.save_to(dir.path()).unwrap();

        let fp1 = compute_fingerprint(dir.path());

        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(dir.path().join("flake.nix"), "{ }").unwrap();

        let fp2 = compute_fingerprint(dir.path());
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn fingerprint_changes_on_flake_lock_update() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest = EnvManifest::default();
        manifest.save_to(dir.path()).unwrap();
        std::fs::write(dir.path().join("flake.lock"), "{}").unwrap();

        let fp1 = compute_fingerprint(dir.path());

        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(dir.path().join("flake.lock"), "{\"version\": 2}").unwrap();

        let fp2 = compute_fingerprint(dir.path());
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn fingerprint_ignores_unrelated_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest = EnvManifest::default();
        manifest.save_to(dir.path()).unwrap();

        let fp1 = compute_fingerprint(dir.path());

        std::fs::write(dir.path().join("README.md"), "hello").unwrap();

        let fp2 = compute_fingerprint(dir.path());
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn fingerprint_handles_missing_manifest() {
        let dir = tempfile::TempDir::new().unwrap();
        // No manifest at all — should still produce a stable fingerprint.
        let fp1 = compute_fingerprint(dir.path());
        let fp2 = compute_fingerprint(dir.path());
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn hook_output_contains_manifest_name() {
        let bash = bash_hook();
        assert!(bash.contains(ENV_MANIFEST_NAME));
        let zsh = zsh_hook();
        assert!(zsh.contains(ENV_MANIFEST_NAME));
        let fish = fish_hook();
        assert!(fish.contains(ENV_MANIFEST_NAME));
    }

    #[test]
    fn hook_bash_contains_fingerprint_check() {
        let bash = bash_hook();
        assert!(bash.contains("_fingerprint"));
        assert!(bash.contains("_EKAPKGS_ENV_FINGERPRINT"));
    }

    #[test]
    fn hook_zsh_contains_trust_check() {
        let zsh = zsh_hook();
        assert!(zsh.contains("_is-trusted"));
        assert!(zsh.contains("ekapkgs env allow"));
    }

    #[test]
    fn hook_fish_contains_deactivate() {
        let fish = fish_hook();
        assert!(fish.contains("_ekapkgs_env_deactivate"));
        assert!(fish.contains("_EKAPKGS_ENV_PATH_BACKUP"));
    }
}
