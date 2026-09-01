use std::process::Stdio;

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval};
use yansi::Paint;

use crate::cli::{HomeCommand, HomePackagesCommand, HomeServicesCommand};
use crate::config::{ClientConfig, HomePackageEntry, HomePackages, HomeServiceEntry, HomeServices};

/// Profile path for imperatively-installed packages (relative to `$HOME`).
const PACKAGES_PROFILE: &str = ".ekapkgs-packages";

pub fn execute(command: HomeCommand) -> color_eyre::Result<()> {
    match command {
        HomeCommand::Switch { installable, extra } => cmd_switch(&installable, &extra),
        HomeCommand::Build { installable, extra } => cmd_build(&installable, &extra),
        HomeCommand::Generations => cmd_generations(),
        HomeCommand::Packages { command } => cmd_packages(command),
        HomeCommand::Services { command } => cmd_services(command),
    }
}

// ---------------------------------------------------------------------------
// Switch / Build
// ---------------------------------------------------------------------------

fn cmd_switch(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let store_path = build_home(installable, extra)?;

    // Activate as current user (no sudo).
    tracing::info!("Activating home configuration...");
    let activate_path = format!("{store_path}/activate");
    let status = std::process::Command::new(&activate_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("failed to run activation script: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "Home activation failed (exit {})",
            status.code().unwrap_or(1)
        ));
    }

    tracing::info!("Home configuration activated");
    Ok(())
}

fn cmd_build(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let store_path = build_home(installable, extra)?;
    println!("{store_path}");
    Ok(())
}

fn build_home(installable: &str, extra: &[String]) -> color_eyre::Result<String> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    // Pre-fetch from cache if configured.
    if config.primary_cache().is_some() {
        let spinner = ekapkgs_ui::progress::spinner("Evaluating home closure...");
        match eval::derivation_closure_paths(&inst) {
            Ok(closure_paths) => {
                spinner.finish_and_clear();
                crate::prefetch::prefetch_closure(&config, &closure_paths)?;
            },
            Err(_) => {
                spinner.finish_and_clear();
            },
        }
    }

    // Build.
    tracing::info!("Building home configuration...");
    let outputs: Vec<BuildOutput> = NixCommand::new(&["build"])
        .arg(installable)
        .arg("--json")
        .args(extra.iter().map(String::as_str))
        .json()?;

    let path = outputs
        .first()
        .and_then(|o| o.outputs.get("out").cloned())
        .ok_or_else(|| color_eyre::eyre::eyre!("build produced no output"))?;

    tracing::info!("Built {}", path);
    Ok(path)
}

// ---------------------------------------------------------------------------
// Generations
// ---------------------------------------------------------------------------

fn cmd_generations() -> color_eyre::Result<()> {
    let state_dir = dirs_path()?.join("generations");

    if !state_dir.exists() {
        println!("No generations found.");
        return Ok(());
    }

    let mut entries: Vec<(u64, std::path::PathBuf)> = std::fs::read_dir(&state_dir)?
        .filter_map(std::result::Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let num: u64 = name.parse().ok()?;
            let target = std::fs::read_link(e.path()).ok()?;
            Some((num, target))
        })
        .collect();

    entries.sort_by_key(|(num, _)| *num);

    if entries.is_empty() {
        println!("No generations found.");
        return Ok(());
    }

    for (num, target) in &entries {
        println!("{num:>4}  {}", target.display());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Packages
// ---------------------------------------------------------------------------

fn cmd_packages(command: HomePackagesCommand) -> color_eyre::Result<()> {
    match command {
        HomePackagesCommand::Add { packages, flake } => {
            cmd_packages_add(&packages, flake.as_deref())
        },
        HomePackagesCommand::Remove { packages } => cmd_packages_remove(&packages),
        HomePackagesCommand::List { json } => cmd_packages_list(json),
        HomePackagesCommand::Export { output } => cmd_packages_export(output.as_deref()),
        HomePackagesCommand::Import { file, merge } => cmd_packages_import(&file, merge),
    }
}

fn packages_profile_path() -> color_eyre::Result<String> {
    let home = std::env::var("HOME")
        .map_err(|_| color_eyre::eyre::eyre!("HOME environment variable not set"))?;
    Ok(format!("{home}/{PACKAGES_PROFILE}"))
}

fn cmd_packages_add(packages: &[String], flake_override: Option<&str>) -> color_eyre::Result<()> {
    let mut manifest = HomePackages::load()?;
    let profile = packages_profile_path()?;
    let mut added = 0u32;

    for name in packages {
        let entry = HomePackageEntry {
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
            .arg(&profile)
            .arg(&installable)
            .stream()?;

        added += 1;
    }

    manifest.save()?;

    if added > 0 {
        println!(
            "Added {added} package(s) to {}",
            HomePackages::manifest_path().display()
        );

        // Hint about PATH on first use.
        let profile_bin = format!("{profile}/bin");
        if let Ok(path) = std::env::var("PATH") {
            if !path.contains(&profile_bin) {
                println!(
                    "\n{}",
                    format!(
                        "Hint: add {profile_bin} to your PATH to use these packages:\n  export \
                         PATH=\"$HOME/{PACKAGES_PROFILE}/bin:$PATH\""
                    )
                    .dim()
                );
            }
        }
    }

    Ok(())
}

fn cmd_packages_remove(packages: &[String]) -> color_eyre::Result<()> {
    let mut manifest = HomePackages::load()?;
    let profile = packages_profile_path()?;
    let mut removed = 0u32;

    for name in packages {
        if !manifest.remove(name) {
            tracing::warn!("{name} is not in the manifest, skipping");
            continue;
        }

        tracing::info!("Removing {name} from profile...");
        // `nix profile remove` accepts a regex matching the package name.
        if let Err(e) = NixCommand::new(&["profile", "remove"])
            .arg("--profile")
            .arg(&profile)
            .arg(name)
            .stream()
        {
            tracing::warn!("Failed to remove {name} from nix profile: {e}");
        }

        removed += 1;
    }

    manifest.save()?;

    if removed > 0 {
        println!("Removed {removed} package(s)");
    }

    Ok(())
}

fn cmd_packages_list(json_output: bool) -> color_eyre::Result<()> {
    let manifest = HomePackages::load()?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&manifest.packages)?);
        return Ok(());
    }

    if manifest.packages.is_empty() {
        println!("No packages installed.");
        println!(
            "{}",
            "Use `ekapkgs home packages add <package>` to add one.".dim()
        );
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

fn cmd_packages_export(output: Option<&str>) -> color_eyre::Result<()> {
    let manifest = HomePackages::load()?;
    let contents = toml::to_string_pretty(&manifest)?;

    if let Some(path) = output {
        std::fs::write(path, &contents)?;
        println!("Exported {} package(s) to {path}", manifest.packages.len());
    } else {
        print!("{contents}");
    }

    Ok(())
}

fn cmd_packages_import(file: &str, merge: bool) -> color_eyre::Result<()> {
    let contents = std::fs::read_to_string(file)?;
    let imported: HomePackages = toml::from_str(&contents)?;
    let profile = packages_profile_path()?;

    let mut manifest = if merge {
        let mut current = HomePackages::load()?;
        for entry in imported.packages {
            // Imported entries win on name conflict.
            current.remove(&entry.name);
            current.packages.push(entry);
        }
        current
    } else {
        imported.clone()
    };

    // Preserve the version field.
    manifest.version = default_manifest_version();

    manifest.save()?;

    // Sync the nix profile: install all packages from the manifest.
    let mut installed = 0u32;
    for entry in &manifest.packages {
        let installable = manifest.resolve_installable(entry);
        tracing::info!("Installing {installable}...");
        match NixCommand::new(&["profile", "install"])
            .arg("--profile")
            .arg(&profile)
            .arg(&installable)
            .stream()
        {
            Ok(_) => installed += 1,
            Err(e) => tracing::warn!("Failed to install {}: {e}", entry.name),
        }
    }

    println!(
        "Imported {} package(s) ({installed} installed to profile)",
        manifest.packages.len()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

fn cmd_services(command: HomeServicesCommand) -> color_eyre::Result<()> {
    match command {
        HomeServicesCommand::Add {
            service,
            flake,
            sets,
            no_apply,
        } => cmd_services_add(&service, flake.as_deref(), &sets, no_apply),
        HomeServicesCommand::Remove { services, no_apply } => {
            cmd_services_remove(&services, no_apply)
        },
        HomeServicesCommand::Set {
            service,
            key,
            value,
        } => cmd_services_set(&service, &key, &value),
        HomeServicesCommand::Unset { service, key } => cmd_services_unset(&service, &key),
        HomeServicesCommand::Enable { services } => cmd_services_enable(&services),
        HomeServicesCommand::Disable { services } => cmd_services_disable(&services),
        HomeServicesCommand::Apply { dry_run } => cmd_services_apply(dry_run),
        HomeServicesCommand::Status { service, json } => {
            cmd_services_status(service.as_deref(), json)
        },
        HomeServicesCommand::Logs {
            service,
            follow,
            lines,
            since,
        } => cmd_services_logs(&service, follow, lines, since.as_deref()),
        HomeServicesCommand::Restart { services } => cmd_services_restart(&services),
        HomeServicesCommand::List { json, all } => cmd_services_list(json, all),
        HomeServicesCommand::Inspect { service, json } => cmd_services_inspect(&service, json),
        HomeServicesCommand::Validate { json } => cmd_services_validate(json),
        HomeServicesCommand::Export { output } => cmd_services_export(output.as_deref()),
        HomeServicesCommand::Import { file, merge } => cmd_services_import(&file, merge),
        HomeServicesCommand::Linger { enable, disable } => cmd_services_linger(enable, disable),
        HomeServicesCommand::Schema { service, json } => {
            cmd_services_schema(service.as_deref(), json)
        },
        HomeServicesCommand::Update { flake } => cmd_services_update(&flake),
    }
}

// --- Manifest mutation commands ---

fn cmd_services_add(
    service: &str,
    flake: Option<&str>,
    sets: &[String],
    no_apply: bool,
) -> color_eyre::Result<()> {
    let mut manifest = HomeServices::load()?;

    if manifest.get(service).is_some() {
        return Err(color_eyre::eyre::eyre!(
            "Service '{service}' already exists in the manifest. Use `set` to modify it."
        ));
    }

    let mut entry = HomeServiceEntry {
        name: service.to_owned(),
        enable: true,
        flake: flake.map(str::to_owned),
        config: toml::Table::new(),
    };

    // Apply --set key=value pairs
    for kv in sets {
        let (key, value) = kv.split_once('=').ok_or_else(|| {
            color_eyre::eyre::eyre!("Invalid --set format '{kv}': expected KEY=VALUE")
        })?;
        let toml_value = parse_toml_value(value);
        entry.set_config(key, toml_value);
    }

    manifest.add(entry);
    manifest.save()?;

    println!(
        "Added service {} to {}",
        service.bold(),
        HomeServices::manifest_path().display()
    );

    if !no_apply {
        cmd_services_apply(false)?;
    }

    Ok(())
}

fn cmd_services_remove(services: &[String], no_apply: bool) -> color_eyre::Result<()> {
    let mut manifest = HomeServices::load()?;
    let mut removed = 0u32;

    for name in services {
        if manifest.remove(name) {
            removed += 1;
        } else {
            tracing::warn!("{name} is not in the manifest, skipping");
        }
    }

    manifest.save()?;

    if removed > 0 {
        println!("Removed {removed} service(s)");
        if !no_apply {
            cmd_services_apply(false)?;
        }
    }

    Ok(())
}

fn cmd_services_set(service: &str, key: &str, value: &str) -> color_eyre::Result<()> {
    let mut manifest = HomeServices::load()?;

    let entry = manifest.get_mut(service).ok_or_else(|| {
        color_eyre::eyre::eyre!("Service '{service}' not found in manifest. Use `add` first.")
    })?;

    let toml_value = parse_toml_value(value);
    entry.set_config(key, toml_value);
    manifest.save()?;

    println!("Set {service}.{key} = {value}");
    println!(
        "{}",
        "Run `ekapkgs home services apply` to activate changes.".dim()
    );

    Ok(())
}

fn cmd_services_unset(service: &str, key: &str) -> color_eyre::Result<()> {
    let mut manifest = HomeServices::load()?;

    let entry = manifest
        .get_mut(service)
        .ok_or_else(|| color_eyre::eyre::eyre!("Service '{service}' not found in manifest."))?;

    if entry.unset_config(key) {
        manifest.save()?;
        println!("Unset {service}.{key} (reverted to default)");
        println!(
            "{}",
            "Run `ekapkgs home services apply` to activate changes.".dim()
        );
    } else {
        println!("Key '{key}' not found in {service} config.");
    }

    Ok(())
}

fn cmd_services_enable(services: &[String]) -> color_eyre::Result<()> {
    let mut manifest = HomeServices::load()?;
    let mut count = 0u32;

    for name in services {
        if let Some(entry) = manifest.get_mut(name) {
            if !entry.enable {
                entry.enable = true;
                count += 1;
            } else {
                tracing::warn!("{name} is already enabled");
            }
        } else {
            tracing::warn!("{name} is not in the manifest");
        }
    }

    manifest.save()?;

    if count > 0 {
        println!("Enabled {count} service(s)");
        println!(
            "{}",
            "Run `ekapkgs home services apply` to activate changes.".dim()
        );
    }

    Ok(())
}

fn cmd_services_disable(services: &[String]) -> color_eyre::Result<()> {
    let mut manifest = HomeServices::load()?;
    let mut count = 0u32;

    for name in services {
        if let Some(entry) = manifest.get_mut(name) {
            if entry.enable {
                entry.enable = false;
                count += 1;
            } else {
                tracing::warn!("{name} is already disabled");
            }
        } else {
            tracing::warn!("{name} is not in the manifest");
        }
    }

    manifest.save()?;

    if count > 0 {
        println!("Disabled {count} service(s)");
        println!(
            "{}",
            "Run `ekapkgs home services apply` to activate changes.".dim()
        );
    }

    Ok(())
}

// --- Build & activate ---

/// Managed unit file marker prefix. Unit files installed by ekapkgs contain
/// a comment on the first line so we can distinguish them from user-created ones.
const MANAGED_MARKER: &str = "# ekapkgs-managed";

/// Directory for user systemd unit files.
fn user_unit_dir() -> color_eyre::Result<std::path::PathBuf> {
    let home = home_dir()?;
    let dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// List currently installed ekapkgs-managed unit files.
fn installed_managed_units() -> color_eyre::Result<std::collections::HashSet<String>> {
    let dir = user_unit_dir()?;
    let mut managed = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".service") {
                // Check if it's managed by us
                if let Ok(contents) = std::fs::read_to_string(entry.path()) {
                    if contents.starts_with(MANAGED_MARKER) {
                        // Strip the .service suffix to get the service name
                        let svc_name = name.trim_end_matches(".service").to_owned();
                        managed.insert(svc_name);
                    }
                }
            }
        }
    }
    Ok(managed)
}

fn cmd_services_apply(dry_run: bool) -> color_eyre::Result<()> {
    let manifest = HomeServices::load()?;
    let enabled: Vec<_> = manifest.services.iter().filter(|s| s.enable).collect();

    // Determine what's currently installed
    let currently_installed = installed_managed_units()?;
    let desired: std::collections::HashSet<String> =
        enabled.iter().map(|s| s.name.clone()).collect();

    let to_add: Vec<&str> = desired
        .iter()
        .filter(|n| !currently_installed.contains(n.as_str()))
        .map(String::as_str)
        .collect();
    let to_remove: Vec<&str> = currently_installed
        .iter()
        .filter(|n| !desired.contains(n.as_str()))
        .map(String::as_str)
        .collect();
    let to_update: Vec<&str> = desired
        .iter()
        .filter(|n| currently_installed.contains(n.as_str()))
        .map(String::as_str)
        .collect();

    if enabled.is_empty() && to_remove.is_empty() {
        println!("No services to apply.");
        return Ok(());
    }

    if dry_run {
        for name in &to_add {
            println!("  {} {name}.service (new)", "+".bold());
        }
        for name in &to_update {
            println!("  {} {name}.service (update)", "~".bold());
        }
        for name in &to_remove {
            println!("  {} {name}.service (remove)", "-".bold());
        }
        if to_add.is_empty() && to_update.is_empty() && to_remove.is_empty() {
            println!("No changes.");
        }
        return Ok(());
    }

    // Phase 1: Stop and remove services that are no longer desired
    let unit_dir = user_unit_dir()?;
    for name in &to_remove {
        tracing::info!("Stopping {name}...");
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "stop", &format!("{name}.service")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", &format!("{name}.service")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let unit_path = unit_dir.join(format!("{name}.service"));
        if unit_path.exists() {
            std::fs::remove_file(&unit_path)?;
        }
        println!("  Removed {}", name.bold());
    }

    // Phase 2: Build unit files for enabled services
    if !enabled.is_empty() {
        let nix_expr = build_services_nix_expr(&manifest)?;

        let spinner = ekapkgs_ui::progress::spinner("Building service unit files...");
        let output = NixCommand::new(&["build"])
            .arg("--no-link")
            .arg("--json")
            .arg("--impure")
            .arg("--expr")
            .arg(&nix_expr)
            .output();
        spinner.finish_and_clear();

        let output = output?;
        let builds: Vec<BuildOutput> = serde_json::from_slice(&output.stdout)?;

        let store_path = builds
            .first()
            .and_then(|b| b.outputs.get("out").cloned())
            .ok_or_else(|| color_eyre::eyre::eyre!("nix build produced no output"))?;

        // Phase 3: Install unit files
        let built_dir = std::path::Path::new(&store_path);
        for entry in std::fs::read_dir(built_dir)?.flatten() {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if !file_name.ends_with(".service") {
                continue;
            }
            let svc_name = file_name.trim_end_matches(".service");

            // Read the built unit file and prepend our managed marker
            let unit_contents = std::fs::read_to_string(entry.path())?;
            let marked_contents = format!("{MANAGED_MARKER}\n{unit_contents}");

            let dest = unit_dir.join(&file_name);
            let is_new = !dest.exists();
            std::fs::write(&dest, marked_contents)?;

            if is_new {
                println!("  Installed {}", svc_name.bold());
            } else {
                println!("  Updated {}", svc_name.bold());
            }
        }

        // Phase 4: Reload and (re)start
        tracing::info!("Reloading systemd user daemon...");
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        for entry in &enabled {
            let unit = format!("{}.service", entry.name);
            // Enable the service
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "enable", &unit])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            // Restart (handles both new and updated services)
            let status = std::process::Command::new("systemctl")
                .args(["--user", "restart", &unit])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match status {
                Ok(s) if s.success() => {
                    tracing::info!("Started {}", entry.name);
                },
                _ => {
                    tracing::warn!("Failed to start {}", entry.name);
                },
            }
        }
    } else if !to_remove.is_empty() {
        // Only removals — still need daemon-reload
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    println!("Services applied.");
    Ok(())
}

/// Build the nix expression that produces a derivation containing all service
/// unit files. The expression imports the service infrastructure from the
/// manifest's flake and passes the TOML config as a nix attrset.
fn build_services_nix_expr(manifest: &HomeServices) -> color_eyre::Result<String> {
    // The infrastructure flake provides the service builders. Use the first
    // enabled entry's flake override, falling back to the manifest default.
    let infra_flake = manifest
        .services
        .iter()
        .filter(|s| s.enable)
        .find_map(|s| s.flake.as_deref())
        .unwrap_or(&manifest.flake);
    let flake_ref = resolve_flake_ref(infra_flake)?;

    // Convert each enabled service entry's config to a nix attrset literal
    let mut service_defs = Vec::new();
    for entry in &manifest.services {
        if !entry.enable {
            continue;
        }
        let nix_config = toml_table_to_nix(&entry.config);
        service_defs.push(format!(
            r#"    "{name}" = {{ enable = true; {config} }};"#,
            name = entry.name,
            config = nix_config,
        ));
    }
    let services_attrset = service_defs.join("\n");

    let expr = format!(
        r#"
let
  flake = builtins.getFlake "{flake_ref}";
  system = builtins.currentSystem;
  pkgs = flake.legacyPackages.${{system}}
         or flake.pkgs.${{system}}
         or (import <nixpkgs> {{}});
  services = import (flake.outPath + "/services/default.nix") {{ inherit pkgs; }};

  serviceConfig = {{
{services_attrset}
  }};

  units = services.buildSystemdUserServices serviceConfig;

  # Merge all unit file derivations into a single output directory
  merged = pkgs.runCommand "ekapkgs-user-services" {{}} ''
    mkdir -p $out
    {copy_commands}
  '';
in merged
"#,
        copy_commands = {
            let mut cmds = Vec::new();
            for entry in &manifest.services {
                if !entry.enable {
                    continue;
                }
                cmds.push(format!(
                    r#"    cp ${{units."{name}"}}/{name}.service $out/{name}.service"#,
                    name = entry.name,
                ));
            }
            cmds.join("\n")
        },
    );

    Ok(expr)
}

/// Convert a TOML table to a nix attrset string (inline).
fn toml_table_to_nix(table: &toml::Table) -> String {
    let mut parts = Vec::new();
    for (key, value) in table {
        let nix_key = escape_nix_key(key);
        let nix_val = toml_value_to_nix(value);
        parts.push(format!("{nix_key} = {nix_val};"));
    }
    parts.join(" ")
}

/// Convert a TOML value to a nix expression string.
fn toml_value_to_nix(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => format!("\"{}\"", escape_nix_string(s)),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(f) => {
            // Nix floats must have a decimal point
            let s = f.to_string();
            if s.contains('.') { s } else { format!("{s}.0") }
        },
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(toml_value_to_nix).collect();
            format!("[ {} ]", items.join(" "))
        },
        toml::Value::Table(table) => {
            let inner = toml_table_to_nix(table);
            format!("{{ {inner} }}")
        },
        toml::Value::Datetime(dt) => format!("\"{}\"", dt),
    }
}

/// Escape a nix attribute key (quote if it contains special characters).
fn escape_nix_key(key: &str) -> String {
    if key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.to_owned()
    } else {
        format!("\"{}\"", escape_nix_string(key))
    }
}

/// Escape a string for use inside nix double quotes.
fn escape_nix_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace("${", "\\${")
}

/// Resolve a flake reference to an absolute path if it's local.
fn resolve_flake_ref(flake: &str) -> color_eyre::Result<String> {
    if flake == "." || flake.starts_with("./") || flake.starts_with('/') {
        let path = std::path::Path::new(flake);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        Ok(abs
            .canonicalize()
            .unwrap_or(abs)
            .to_string_lossy()
            .into_owned())
    } else {
        Ok(flake.to_owned())
    }
}

// --- Runtime control ---

fn cmd_services_status(service: Option<&str>, json_output: bool) -> color_eyre::Result<()> {
    let manifest = HomeServices::load()?;

    let names: Vec<&str> = if let Some(name) = service {
        if manifest.get(name).is_none() {
            return Err(color_eyre::eyre::eyre!(
                "Service '{name}' is not in the manifest."
            ));
        }
        vec![name]
    } else {
        manifest
            .services
            .iter()
            .filter(|s| s.enable)
            .map(|s| s.name.as_str())
            .collect()
    };

    if names.is_empty() {
        println!("No enabled services.");
        return Ok(());
    }

    if json_output {
        let mut statuses = Vec::new();
        for name in &names {
            let status = query_systemd_status(name);
            statuses.push(status);
        }
        println!("{}", serde_json::to_string_pretty(&statuses)?);
        return Ok(());
    }

    println!(
        "{:<24} {:<12} {:<8} {}",
        "SERVICE".bold(),
        "STATUS".bold(),
        "PID".bold(),
        "DESCRIPTION".bold()
    );
    for name in &names {
        let status = query_systemd_status(name);
        let pid_str = status.pid.map_or_else(|| "-".to_owned(), |p| p.to_string());
        let desc = manifest
            .get(name)
            .and_then(|e| e.config.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        println!("{name:<24} {:<12} {pid_str:<8} {desc}", status.active_state);
    }

    Ok(())
}

fn cmd_services_logs(
    service: &str,
    follow: bool,
    lines: u32,
    since: Option<&str>,
) -> color_eyre::Result<()> {
    let mut cmd = std::process::Command::new("journalctl");
    cmd.arg("--user")
        .arg("-u")
        .arg(format!("{service}.service"))
        .arg("-n")
        .arg(lines.to_string());

    if follow {
        cmd.arg("-f");
    }
    if let Some(since_val) = since {
        cmd.arg("--since").arg(since_val);
    }

    let status = cmd
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| color_eyre::eyre::eyre!("Failed to run journalctl: {e}"))?;

    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "journalctl exited with {}",
            status.code().unwrap_or(1)
        ));
    }

    Ok(())
}

fn cmd_services_restart(services: &[String]) -> color_eyre::Result<()> {
    for name in services {
        tracing::info!("Restarting {name}...");
        let status = std::process::Command::new("systemctl")
            .arg("--user")
            .arg("restart")
            .arg(format!("{name}.service"))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| color_eyre::eyre::eyre!("Failed to run systemctl: {e}"))?;

        if status.success() {
            println!("Restarted {}", name.bold());
        } else {
            tracing::warn!("Failed to restart {name}");
        }
    }
    Ok(())
}

// --- Inspection ---

fn cmd_services_list(json_output: bool, show_all: bool) -> color_eyre::Result<()> {
    let manifest = HomeServices::load()?;

    let entries: Vec<_> = if show_all {
        manifest.services.iter().collect()
    } else {
        manifest.services.iter().filter(|s| s.enable).collect()
    };

    if json_output {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        if show_all {
            println!("No services in the manifest.");
        } else {
            println!("No enabled services. Use --all to include disabled.");
        }
        println!(
            "{}",
            "Use `ekapkgs home services add <service>` to add one.".dim()
        );
        return Ok(());
    }

    println!(
        "{:<24} {:<10} {:<20}",
        "SERVICE".bold(),
        "ENABLED".bold(),
        "FLAKE".bold()
    );
    for entry in &entries {
        let enabled = if entry.enable { "yes" } else { "no" };
        let flake = entry.flake.as_deref().unwrap_or(&manifest.flake);
        println!("{:<24} {enabled:<10} {flake:<20}", entry.name);
    }
    println!("\n{} service(s)", entries.len());

    Ok(())
}

fn cmd_services_inspect(service: &str, json_output: bool) -> color_eyre::Result<()> {
    let manifest = HomeServices::load()?;

    let entry = manifest
        .get(service)
        .ok_or_else(|| color_eyre::eyre::eyre!("Service '{service}' not found in manifest."))?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(entry)?);
        return Ok(());
    }

    println!("{}", service.bold());
    println!(
        "  {}: {}",
        "Enabled".dim(),
        if entry.enable { "yes" } else { "no" }
    );
    println!(
        "  {}: {}",
        "Flake".dim(),
        entry.flake.as_deref().unwrap_or(&manifest.flake)
    );
    println!(
        "  {}: {}",
        "Manifest".dim(),
        HomeServices::manifest_path().display()
    );

    if !entry.config.is_empty() {
        println!();
        println!("  {}:", "Configuration".bold());
        print_toml_table(&entry.config, 4);
    }

    Ok(())
}

fn cmd_services_validate(json_output: bool) -> color_eyre::Result<()> {
    let manifest = HomeServices::load()?;
    let schema = crate::service_schema::load_or_generate(".")?;

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for entry in &manifest.services {
        let options = crate::service_schema::options_for_service(&schema, &entry.name);

        // Check that required options without defaults are set in the config
        for opt in options {
            if opt.required && !has_config_key(&entry.config, &opt.path) {
                errors.push(format!(
                    "{}: required option '{}' is not set",
                    entry.name, opt.path
                ));
            }
        }

        // Warn on empty description
        if !entry.config.contains_key("description") {
            warnings.push(format!("{}: missing 'description'", entry.name));
        }
    }

    if json_output {
        let result = serde_json::json!({
            "services": manifest.services.len(),
            "errors": errors,
            "warnings": warnings,
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    println!("Validating {} service(s)...", manifest.services.len());

    for entry in &manifest.services {
        let entry_errors: Vec<_> = errors
            .iter()
            .filter(|e| e.starts_with(&entry.name))
            .collect();
        let entry_warnings: Vec<_> = warnings
            .iter()
            .filter(|w| w.starts_with(&entry.name))
            .collect();

        if entry_errors.is_empty() && entry_warnings.is_empty() {
            println!("  {}: {}", entry.name, "OK".bold());
        } else {
            println!("  {}:", entry.name);
            for e in entry_errors {
                println!("    {} {e}", "ERROR:".bold());
            }
            for w in entry_warnings {
                println!("    {} {w}", "WARNING:".dim());
            }
        }
    }

    let total_errors = errors.len();
    let total_warnings = warnings.len();
    println!("\n{total_errors} error(s), {total_warnings} warning(s)");

    if total_errors > 0 {
        return Err(color_eyre::eyre::eyre!("Validation failed"));
    }

    Ok(())
}

// --- Portability ---

fn cmd_services_export(output: Option<&str>) -> color_eyre::Result<()> {
    let manifest = HomeServices::load()?;
    let contents = toml::to_string_pretty(&manifest)?;

    if let Some(path) = output {
        std::fs::write(path, &contents)?;
        println!("Exported {} service(s) to {path}", manifest.services.len());
    } else {
        print!("{contents}");
    }

    Ok(())
}

fn cmd_services_import(file: &str, merge: bool) -> color_eyre::Result<()> {
    let contents = std::fs::read_to_string(file)?;
    let imported: HomeServices = toml::from_str(&contents)?;

    let manifest = if merge {
        let mut current = HomeServices::load()?;
        for entry in imported.services {
            current.remove(&entry.name);
            current.services.push(entry);
        }
        current
    } else {
        imported.clone()
    };

    manifest.save()?;

    println!("Imported {} service(s)", manifest.services.len());
    println!("{}", "Run `ekapkgs home services apply` to activate.".dim());

    Ok(())
}

// --- Linger ---

fn cmd_services_linger(enable: bool, disable: bool) -> color_eyre::Result<()> {
    if enable {
        let status = std::process::Command::new("loginctl")
            .arg("enable-linger")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| color_eyre::eyre::eyre!("Failed to run loginctl: {e}"))?;
        if status.success() {
            println!("Linger enabled for current user.");
        } else {
            return Err(color_eyre::eyre::eyre!("Failed to enable linger"));
        }
    } else if disable {
        let status = std::process::Command::new("loginctl")
            .arg("disable-linger")
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .map_err(|e| color_eyre::eyre::eyre!("Failed to run loginctl: {e}"))?;
        if status.success() {
            println!("Linger disabled for current user.");
        } else {
            return Err(color_eyre::eyre::eyre!("Failed to disable linger"));
        }
    } else {
        // Show status
        let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
        let linger_path = format!("/var/lib/systemd/linger/{user}");
        if std::path::Path::new(&linger_path).exists() {
            println!("Linger is {} for {user}", "enabled".bold());
        } else {
            println!("Linger is {} for {user}", "disabled".bold());
            println!(
                "{}",
                "Enable with `ekapkgs home services linger --enable` for services to persist \
                 beyond login."
                    .dim()
            );
        }
    }

    Ok(())
}

// --- Schema (existing) ---

fn cmd_services_schema(service: Option<&str>, json_output: bool) -> color_eyre::Result<()> {
    let schema = crate::service_schema::load_or_generate(".")?;

    if let Some(name) = service {
        let options = crate::service_schema::options_for_service(&schema, name);

        if json_output {
            if let Some(svc) = schema.services.get(name) {
                println!("{}", serde_json::to_string_pretty(svc)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&schema.base)?);
            }
            return Ok(());
        }

        if let Some(svc) = schema.services.get(name) {
            if !svc.description.is_empty() {
                println!("{}", svc.description);
                println!();
            }
        }

        if options.is_empty() {
            println!("No options found for service '{name}'.");
            return Ok(());
        }

        print_option_defs(options);
    } else {
        if json_output {
            println!("{}", serde_json::to_string_pretty(&schema)?);
            return Ok(());
        }

        if schema.services.is_empty() {
            println!(
                "No services discovered. Run {} to generate the schema.",
                "ekapkgs home services update".bold()
            );
            return Ok(());
        }

        println!("{}", "Discovered services:".bold());
        println!();
        for (name, svc) in &schema.services {
            let desc = if svc.description.is_empty() {
                "(no description)".to_owned()
            } else {
                svc.description.lines().next().unwrap_or("").to_owned()
            };
            let n_opts = svc.options.len();
            println!("  {} {}", name.bold(), format!("({n_opts} options)").dim());
            println!("    {desc}");
        }
        println!();
        println!(
            "{} base options available for any service",
            schema.base.options.len()
        );
    }

    Ok(())
}

fn cmd_services_update(flake: &str) -> color_eyre::Result<()> {
    let spinner = ekapkgs_ui::progress::spinner("Generating service options schema...");
    let schema = crate::service_schema::generate(flake)?;
    spinner.finish_and_clear();
    crate::service_schema::write_cache(&schema)?;
    println!(
        "Service schema: {} base options, {} services",
        schema.base.options.len(),
        schema.services.len(),
    );
    Ok(())
}

// --- Service helpers ---

/// Parse a string value into a TOML value, inferring the type.
fn parse_toml_value(s: &str) -> toml::Value {
    // Try bool
    if s == "true" {
        return toml::Value::Boolean(true);
    }
    if s == "false" {
        return toml::Value::Boolean(false);
    }

    // Try integer
    if let Ok(n) = s.parse::<i64>() {
        return toml::Value::Integer(n);
    }

    // Try float (but not if it looks like an integer)
    if s.contains('.') {
        if let Ok(f) = s.parse::<f64>() {
            return toml::Value::Float(f);
        }
    }

    // Try JSON array → TOML array (for --set 'args=["a","b"]')
    if s.starts_with('[') {
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(s) {
            let toml_arr: Vec<toml::Value> =
                arr.into_iter().filter_map(|v| json_to_toml(&v)).collect();
            return toml::Value::Array(toml_arr);
        }
    }

    // Default: string
    toml::Value::String(s.to_owned())
}

/// Convert a JSON value to a TOML value.
fn json_to_toml(v: &serde_json::Value) -> Option<toml::Value> {
    match v {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(b) => Some(toml::Value::Boolean(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml::Value::Integer(i))
            } else {
                n.as_f64().map(toml::Value::Float)
            }
        },
        serde_json::Value::String(s) => Some(toml::Value::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let items: Vec<toml::Value> = arr.iter().filter_map(json_to_toml).collect();
            Some(toml::Value::Array(items))
        },
        serde_json::Value::Object(map) => {
            let mut table = toml::Table::new();
            for (k, val) in map {
                if let Some(tv) = json_to_toml(val) {
                    table.insert(k.clone(), tv);
                }
            }
            Some(toml::Value::Table(table))
        },
    }
}

/// Query systemd user service status.
#[derive(serde::Serialize)]
struct ServiceStatus {
    name: String,
    active_state: String,
    pid: Option<u32>,
}

fn query_systemd_status(name: &str) -> ServiceStatus {
    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .arg("show")
        .arg(format!("{name}.service"))
        .arg("-p")
        .arg("ActiveState,MainPID")
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let mut active_state = "unknown".to_owned();
            let mut pid: Option<u32> = None;

            for line in stdout.lines() {
                if let Some(val) = line.strip_prefix("ActiveState=") {
                    active_state = val.to_owned();
                } else if let Some(val) = line.strip_prefix("MainPID=") {
                    if let Ok(p) = val.parse::<u32>() {
                        if p > 0 {
                            pid = Some(p);
                        }
                    }
                }
            }

            ServiceStatus {
                name: name.to_owned(),
                active_state,
                pid,
            }
        },
        _ => ServiceStatus {
            name: name.to_owned(),
            active_state: "unknown".to_owned(),
            pid: None,
        },
    }
}

/// Check if a dot-separated key exists in a TOML table.
fn has_config_key(table: &toml::Table, key: &str) -> bool {
    let segments: Vec<&str> = key.split('.').collect();
    let mut current = table;
    for (i, seg) in segments.iter().enumerate() {
        if i == segments.len() - 1 {
            return current.contains_key(*seg);
        }
        match current.get(*seg).and_then(|v| v.as_table()) {
            Some(t) => current = t,
            None => return false,
        }
    }
    false
}

/// Print a TOML table with indentation.
fn print_toml_table(table: &toml::Table, indent: usize) {
    let pad = " ".repeat(indent);
    for (key, value) in table {
        match value {
            toml::Value::Table(sub) => {
                println!("{pad}{key}:");
                print_toml_table(sub, indent + 2);
            },
            _ => {
                println!("{pad}{}: {}", key.bold(), value);
            },
        }
    }
}

/// Print option definitions from the schema.
fn print_option_defs(options: &[crate::service_schema::OptionDef]) {
    for opt in options {
        let type_name = opt.option_type.display_name();
        let req = if opt.required { " (required)" } else { "" };
        println!(
            "  {} {}{req}",
            opt.path.bold(),
            format!("[{type_name}]").dim(),
        );
        if !opt.description.is_empty() {
            if let Some(first_line) = opt.description.lines().next() {
                let line = first_line.trim();
                if !line.is_empty() {
                    println!("    {line}");
                }
            }
        }
        if let Some(default) = &opt.default {
            if default.len() <= 60 {
                println!("    {}: {default}", "Default".dim());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn home_dir() -> color_eyre::Result<std::path::PathBuf> {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .map_err(|_| color_eyre::eyre::eyre!("HOME environment variable not set"))
}

fn dirs_path() -> color_eyre::Result<std::path::PathBuf> {
    Ok(home_dir()?.join(".config/ekaos"))
}

fn default_manifest_version() -> u32 {
    1
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildOutput {
    outputs: std::collections::HashMap<String, String>,
}
