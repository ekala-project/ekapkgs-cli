use std::process::Stdio;

use yansi::Paint;

use crate::cli::StatusCommand;
use crate::config::{ENV_MANIFEST_NAME, HomeServices};

pub fn execute(command: &StatusCommand) -> color_eyre::Result<()> {
    match command {
        StatusCommand::Services { json } => cmd_services(*json),
    }
}

/// A unified service entry with scope information.
#[derive(serde::Serialize)]
struct UnifiedService {
    scope: String,
    name: String,
    status: String,
    port: Option<u16>,
    source: String,
}

fn cmd_services(json_output: bool) -> color_eyre::Result<()> {
    let mut services: Vec<UnifiedService> = Vec::new();

    // 1. Home services (from home-services.toml + systemd).
    if let Ok(manifest) = HomeServices::load() {
        for entry in &manifest.services {
            if !entry.enable {
                continue;
            }
            let status = query_systemd_status(&entry.name);
            let port = extract_first_port(&entry.config);
            services.push(UnifiedService {
                scope: "home".to_owned(),
                name: entry.name.clone(),
                status,
                port,
                source: "ekapkgs home services".to_owned(),
            });
        }
    }

    // 2. Env services (check $EKAPKGS_ENV for active dev shell with process-compose).
    if let Ok(env_dir) = std::env::var("EKAPKGS_ENV") {
        let env_path = std::path::Path::new(&env_dir);
        let manifest_path = env_path.join(ENV_MANIFEST_NAME);
        if manifest_path.exists() {
            // Check for process-compose services via `pc-status` if available.
            if let Ok(output) = std::process::Command::new("pc")
                .arg("status")
                .arg("-o")
                .arg("json")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
            {
                if output.status.success() {
                    if let Ok(pc_status) =
                        serde_json::from_slice::<serde_json::Value>(&output.stdout)
                    {
                        if let Some(processes) = pc_status.as_array() {
                            for proc in processes {
                                let name = proc["name"].as_str().unwrap_or("unknown");
                                let status = proc["status"].as_str().unwrap_or("unknown");
                                services.push(UnifiedService {
                                    scope: format!(
                                        "env:{}",
                                        env_path.file_name().unwrap_or_default().to_string_lossy()
                                    ),
                                    name: name.to_owned(),
                                    status: status.to_owned(),
                                    port: None,
                                    source: manifest_path.to_string_lossy().into_owned(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&services)?);
        return Ok(());
    }

    if services.is_empty() {
        println!("No managed services found.");
        println!("{}", "Use `ekapkgs home services add` to add one.".dim());
        return Ok(());
    }

    println!(
        "{:<12} {:<24} {:<12} {:<8} {}",
        "SCOPE".bold(),
        "SERVICE".bold(),
        "STATUS".bold(),
        "PORT".bold(),
        "SOURCE".bold()
    );
    for svc in &services {
        let port_str = svc.port.map_or_else(|| "-".to_owned(), |p| p.to_string());
        println!(
            "{:<12} {:<24} {:<12} {port_str:<8} {}",
            svc.scope, svc.name, svc.status, svc.source
        );
    }

    Ok(())
}

/// Query systemd user service status, returning a simple state string.
fn query_systemd_status(name: &str) -> String {
    let output = std::process::Command::new("systemctl")
        .arg("--user")
        .arg("show")
        .arg(format!("{name}.service"))
        .arg("-p")
        .arg("ActiveState")
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if let Some(val) = line.strip_prefix("ActiveState=") {
                    return val.to_owned();
                }
            }
            "unknown".to_owned()
        },
        _ => "unknown".to_owned(),
    }
}

/// Extract the first port number from a service config's ports table.
fn extract_first_port(config: &toml::Table) -> Option<u16> {
    if let Some(toml::Value::Table(ports_table)) = config.get("ports") {
        for (_name, value) in ports_table {
            if let Some(table) = value.as_table() {
                if let Some(port) = table.get("port").and_then(toml::Value::as_integer) {
                    return u16::try_from(port).ok();
                }
            }
        }
    }
    config
        .get("port")
        .and_then(toml::Value::as_integer)
        .and_then(|p| u16::try_from(p).ok())
}
