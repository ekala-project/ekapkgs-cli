use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval, store};

use crate::config::ClientConfig;

pub fn execute(installable: &str, extra: &[String]) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;
    let inst = Installable::new(installable);

    let spinner = ekapkgs_ui::progress::spinner("Evaluating closure...");
    let closure_paths = eval::derivation_closure_paths(&inst)?;
    let (have, want) = store::partition_local(&closure_paths)?;
    spinner.finish_and_clear();

    // If cache configured, negotiate to show detailed breakdown.
    if let Some(cache) = config.primary_cache() {
        let want_hashes: Vec<String> = want
            .iter()
            .filter_map(|p| store::store_path_hash(p).map(String::from))
            .collect();
        let have_hashes: Vec<String> = have
            .iter()
            .filter_map(|p| store::store_path_hash(p).map(String::from))
            .collect();

        let server_url = cache.url.clone();
        let rt = tokio::runtime::Runtime::new()?;
        let result = rt.block_on(async {
            crate::negotiate::negotiate(&server_url, want_hashes, have_hashes).await
        });

        match result {
            Ok(response) => {
                let from_cache = response.available.len();
                let must_build = response.unavailable.len();

                println!("Closure: {} paths total", closure_paths.len());
                println!("  Already local:       {}", have.len());
                println!("  From ekapkgs cache:  {from_cache}");
                println!("  Must build/substitute: {must_build}");

                if response.total_download_size > 0 {
                    println!(
                        "  Cache download:      {} (unpacked: {})",
                        ekapkgs_ui::format::format_bytes(response.total_download_size),
                        ekapkgs_ui::format::format_bytes(response.total_nar_size),
                    );
                }
            },
            Err(e) => {
                tracing::warn!("Cache negotiation failed: {e}");
                println!("Closure: {} paths total", closure_paths.len());
                println!("  Already local:     {}", have.len());
                println!("  Need to fetch/build: {}", want.len());
            },
        }
    } else {
        println!("Closure: {} paths total", closure_paths.len());
        println!("  Already local:     {}", have.len());
        println!("  Need to fetch/build: {}", want.len());
    }

    // Also run nix build --dry-run for nix's native output.
    println!();
    let mut cmd = NixCommand::new(&["build"]).arg(installable).arg("--dry-run");
    for arg in extra {
        cmd = cmd.arg(arg);
    }
    let _ = cmd.stream();

    Ok(())
}
