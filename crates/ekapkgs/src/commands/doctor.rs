use std::path::Path;
use std::process::{Command, Stdio};

use ekapkgs_nix::NixCommand;
use yansi::Paint;

use crate::config::ClientConfig;

#[allow(clippy::unnecessary_wraps)]
pub fn execute() -> color_eyre::Result<()> {
    println!("{}", "ekapkgs doctor".bold());
    println!();

    check_nix_installed();
    check_nix_store();
    check_config();
    check_disk_space();

    // Config-dependent checks.
    if let Ok(config) = ClientConfig::load() {
        check_caches_reachable(&config);
        check_trust_keys(&config);
    }

    Ok(())
}

fn print_pass(label: &str, detail: &str) {
    println!("  {} {} {}", "PASS".green().bold(), label, detail.dim());
}

fn print_warn(label: &str, detail: &str) {
    println!("  {} {} {}", "WARN".yellow().bold(), label, detail.dim());
}

fn print_fail(label: &str, detail: &str) {
    println!("  {} {} {}", "FAIL".red().bold(), label, detail.dim());
}

fn check_nix_installed() {
    match NixCommand::new(&["--version"]).output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout);
            let version = version.trim();
            print_pass("Nix installed", version);
        },
        Err(_) => {
            print_fail("Nix installed", "nix not found in PATH");
        },
    }
}

fn check_nix_store() {
    match NixCommand::new(&["store", "ping"]).output() {
        Ok(_) => {
            print_pass("Nix store", "store is reachable");
        },
        Err(e) => {
            print_fail("Nix store", &format!("store ping failed: {e}"));
        },
    }
}

fn check_config() {
    match ClientConfig::load() {
        Ok(config) => {
            let cache_count = config.caches.len();
            print_pass(
                "Configuration",
                &format!("{cache_count} cache(s) configured"),
            );
        },
        Err(e) => {
            print_fail("Configuration", &format!("failed to load: {e}"));
        },
    }
}

fn check_disk_space() {
    let store_path = if Path::new("/nix/store").exists() {
        "/nix/store"
    } else {
        "/"
    };

    let output = Command::new("df")
        .args(["--output=avail", "-B1", store_path])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            // df output has a header line, then the value.
            if let Some(avail_str) = stdout.lines().nth(1) {
                if let Ok(avail) = avail_str.trim().parse::<u64>() {
                    let formatted = ekapkgs_ui::format::format_bytes(avail);
                    if avail < 1_073_741_824 {
                        print_fail("Disk space", &format!("{formatted} available on {store_path}"));
                    } else if avail < 10_737_418_240 {
                        print_warn("Disk space", &format!("{formatted} available on {store_path}"));
                    } else {
                        print_pass("Disk space", &format!("{formatted} available on {store_path}"));
                    }
                } else {
                    print_warn("Disk space", "could not parse available space");
                }
            } else {
                print_warn("Disk space", "unexpected df output");
            }
        },
        _ => {
            print_warn("Disk space", "could not query disk space");
        },
    }
}

fn check_caches_reachable(config: &ClientConfig) {
    if config.caches.is_empty() {
        return;
    }

    let Ok(rt) = tokio::runtime::Runtime::new() else {
        print_warn("Cache connectivity", "could not create async runtime");
        return;
    };

    for cache in &config.caches {
        let url = &cache.url;
        let result = rt.block_on(async {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()?;

            let check_url = if url.starts_with("grpc://") || url.starts_with("https://") || url.starts_with("http://") {
                let base = url
                    .replace("grpc://", "https://")
                    .trim_end_matches('/')
                    .to_owned();
                format!("{base}/nix-cache-info")
            } else {
                format!("{}/nix-cache-info", url.trim_end_matches('/'))
            };

            let resp = client.head(&check_url).send().await?;
            Ok::<bool, color_eyre::Report>(resp.status().is_success())
        });

        match result {
            Ok(true) => print_pass(&format!("Cache {url}"), "reachable"),
            Ok(false) => print_warn(&format!("Cache {url}"), "responded but not OK"),
            Err(e) => print_fail(&format!("Cache {url}"), &format!("unreachable: {e}")),
        }
    }
}

fn check_trust_keys(config: &ClientConfig) {
    for cache in &config.caches {
        if let Some(trust_root) = &cache.trust_root {
            if Path::new(trust_root).exists() {
                print_pass(
                    &format!("Trust root for {}", cache.url),
                    &format!("{trust_root} exists"),
                );
            } else {
                print_fail(
                    &format!("Trust root for {}", cache.url),
                    &format!("{trust_root} not found"),
                );
            }
        }
    }
}
