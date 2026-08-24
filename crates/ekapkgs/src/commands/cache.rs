use std::path::PathBuf;

use ekapkgs_nix::NixCommand;
use ekapkgs_nix::eval;
use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::store;
use futures::StreamExt;

use crate::cli::{AuthCommand, CacheCommand};
use crate::config::ClientConfig;

pub fn execute(command: CacheCommand) -> color_eyre::Result<()> {
    match command {
        CacheCommand::Push { paths, cache } => cmd_push(&paths, cache.as_deref()),
        CacheCommand::Pull { paths, cache } => cmd_pull(&paths, cache.as_deref()),
        CacheCommand::Auth { command } => cmd_auth(command),
    }
}

// --- push ---

fn cmd_push(paths: &[String], cache_url: Option<&str>) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;

    let server_url = match cache_url {
        Some(url) => url.to_string(),
        None => {
            let cache = config.primary_cache().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "no cache configured — use --cache or configure in config.toml"
                )
            })?;
            cache.url.clone()
        }
    };

    let token = config.push_token(&server_url);
    let base_url = server_url.trim_end_matches('/');

    let store_paths = resolve_store_paths(paths)?;

    if store_paths.is_empty() {
        tracing::info!("Nothing to push");
        return Ok(());
    }

    tracing::info!("Pushing {} paths to {base_url}", store_paths.len());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let client = reqwest::Client::new();
        let bar = ekapkgs_ui::progress::item_bar(store_paths.len() as u64, "paths");

        let results = futures::stream::iter(store_paths.iter())
            .map(|path| {
                let client = client.clone();
                let base = base_url.to_string();
                let token = token.clone();
                let path = path.clone();
                async move {
                    push_single_path(&client, &base, token.as_deref(), &path).await
                }
            })
            .buffer_unordered(8)
            .collect::<Vec<_>>()
            .await;

        let mut success = 0u64;
        let mut skipped = 0u64;
        let mut failed = 0u64;

        for result in results {
            match result {
                Ok(PushResult::Uploaded) => {
                    success += 1;
                    bar.inc(1);
                }
                Ok(PushResult::AlreadyExists) => {
                    skipped += 1;
                    bar.inc(1);
                }
                Err(e) => {
                    tracing::warn!("Push failed: {e}");
                    failed += 1;
                    bar.inc(1);
                }
            }
        }

        bar.finish_and_clear();

        if success > 0 {
            tracing::info!("{success} paths uploaded");
        }
        if skipped > 0 {
            tracing::info!("{skipped} paths already on server");
        }
        if failed > 0 {
            tracing::warn!("{failed} paths failed to upload");
        }

        Ok::<(), color_eyre::Report>(())
    })?;

    Ok(())
}

enum PushResult {
    Uploaded,
    AlreadyExists,
}

async fn push_single_path(
    client: &reqwest::Client,
    base_url: &str,
    token: Option<&str>,
    store_path: &str,
) -> color_eyre::Result<PushResult> {
    let hash = store::store_path_hash(store_path)
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid store path: {store_path}"))?;

    // Check if already on server.
    let check_url = format!("{base_url}/{hash}.narinfo");
    let resp = client.head(&check_url).send().await?;
    if resp.status().is_success() {
        return Ok(PushResult::AlreadyExists);
    }

    // Get path info.
    let path_info_output = NixCommand::new(&["path-info", "--json"])
        .arg(store_path)
        .output()?;
    let path_info_str = String::from_utf8_lossy(&path_info_output.stdout);

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PathInfo {
        path: String,
        nar_hash: String,
        nar_size: u64,
        #[serde(default)]
        references: Vec<String>,
        #[serde(default)]
        signatures: Vec<String>,
        ca: Option<String>,
    }

    let infos: Vec<PathInfo> = serde_json::from_str(&path_info_str)?;
    let info = infos
        .into_iter()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("no path info for {store_path}"))?;

    // Dump NAR.
    let nar_output = std::process::Command::new("nix-store")
        .arg("--dump")
        .arg(store_path)
        .output()?;

    if !nar_output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "nix-store --dump failed for {store_path}"
        ));
    }

    let nar_data = nar_output.stdout;
    let nar_url = format!("nar/{hash}.nar");

    // Build narinfo.
    let refs: Vec<String> = info
        .references
        .iter()
        .map(|r| r.rsplit('/').next().unwrap_or(r).to_string())
        .collect();

    let mut narinfo = String::new();
    narinfo.push_str(&format!("StorePath: {}\n", info.path));
    narinfo.push_str(&format!("URL: {nar_url}\n"));
    narinfo.push_str("Compression: none\n");
    narinfo.push_str(&format!("NarHash: {}\n", info.nar_hash));
    narinfo.push_str(&format!("NarSize: {}\n", info.nar_size));
    narinfo.push_str(&format!("FileSize: {}\n", nar_data.len()));
    if !refs.is_empty() {
        narinfo.push_str(&format!("References: {}\n", refs.join(" ")));
    }
    for sig in &info.signatures {
        narinfo.push_str(&format!("Sig: {sig}\n"));
    }
    if let Some(ca) = &info.ca {
        narinfo.push_str(&format!("CA: {ca}\n"));
    }

    // Upload NAR, then narinfo.
    let nar_upload_url = format!("{base_url}/{nar_url}");
    let mut req = client.put(&nar_upload_url).body(nar_data);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(color_eyre::eyre::eyre!("NAR upload failed: {status} {body}"));
    }

    let narinfo_url = format!("{base_url}/{hash}.narinfo");
    let mut req = client.put(&narinfo_url).body(narinfo);
    if let Some(t) = token {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(color_eyre::eyre::eyre!(
            "narinfo upload failed: {status} {body}"
        ));
    }

    Ok(PushResult::Uploaded)
}

fn resolve_store_paths(inputs: &[String]) -> color_eyre::Result<Vec<String>> {
    let mut paths = Vec::new();

    for input in inputs {
        if input.starts_with("/nix/store/") {
            let output = NixCommand::new(&["path-info", "--recursive", "--json"])
                .arg(input)
                .output()?;
            let stdout = String::from_utf8_lossy(&output.stdout);

            #[derive(serde::Deserialize)]
            struct PathEntry {
                path: String,
            }
            let entries: Vec<PathEntry> = serde_json::from_str(&stdout)?;
            paths.extend(entries.into_iter().map(|e| e.path));
        } else {
            tracing::info!("Building {input}...");
            let build_output = NixCommand::new(&["build"])
                .arg(input)
                .arg("--json")
                .output()?;
            let stdout = String::from_utf8_lossy(&build_output.stdout);

            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct BuildOutput {
                outputs: std::collections::HashMap<String, String>,
            }
            let outputs: Vec<BuildOutput> = serde_json::from_str(&stdout)?;

            for build in outputs {
                for out_path in build.outputs.values() {
                    let closure_output =
                        NixCommand::new(&["path-info", "--recursive", "--json"])
                            .arg(out_path)
                            .output()?;
                    let closure_str = String::from_utf8_lossy(&closure_output.stdout);

                    #[derive(serde::Deserialize)]
                    struct PathEntry {
                        path: String,
                    }
                    let entries: Vec<PathEntry> = serde_json::from_str(&closure_str)?;
                    paths.extend(entries.into_iter().map(|e| e.path));
                }
            }
        }
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

// --- pull ---

fn cmd_pull(paths: &[String], cache_url: Option<&str>) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;

    let server_url = match cache_url {
        Some(url) => url.to_string(),
        None => {
            let cache = config.primary_cache().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "no cache configured — use --cache or configure in config.toml"
                )
            })?;
            cache.url.clone()
        }
    };

    // Resolve all inputs to closure paths and partition.
    let mut all_closure_paths = Vec::new();
    for input in paths {
        let inst = Installable::new(input);
        let closure = eval::derivation_closure_paths(&inst)?;
        all_closure_paths.extend(closure);
    }
    all_closure_paths.sort();
    all_closure_paths.dedup();

    let (have, want) = store::partition_local(&all_closure_paths)?;

    if want.is_empty() {
        tracing::info!("All {} paths already in local store", have.len());
        return Ok(());
    }

    tracing::info!(
        "{} paths to fetch ({} already local)",
        want.len(),
        have.len()
    );

    let want_hashes: Vec<String> = want
        .iter()
        .filter_map(|p| store::store_path_hash(p).map(String::from))
        .collect();
    let have_hashes: Vec<String> = have
        .iter()
        .filter_map(|p| store::store_path_hash(p).map(String::from))
        .collect();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let spinner = ekapkgs_ui::progress::spinner("Negotiating with cache...");

        let response =
            crate::negotiate::negotiate(&server_url, want_hashes, have_hashes).await?;

        spinner.finish_and_clear();

        let avail = response.available.len();
        let unavail = response.unavailable.len();

        if avail > 0 {
            tracing::info!("{avail} paths available from cache");
            crate::download::download_and_import(
                &server_url,
                &response,
                config.defaults.max_parallel_downloads,
            )
            .await?;
            tracing::info!("Imported {avail} paths");
        }

        if unavail > 0 {
            tracing::warn!("{unavail} paths not available on cache");
        }

        Ok::<(), color_eyre::Report>(())
    })?;

    Ok(())
}

// --- auth ---

fn cmd_auth(command: AuthCommand) -> color_eyre::Result<()> {
    match command {
        AuthCommand::Login { cache, token } => {
            let mut config = ClientConfig::load()?;

            // Update or add the cache entry.
            if let Some(entry) = config.caches.iter_mut().find(|c| c.url == cache) {
                entry.token = Some(token);
            } else {
                config.caches.push(crate::config::CacheConfig {
                    url: cache.clone(),
                    trusted_key: None,
                    trust_root: None,
                    token: Some(token),
                    priority: 10,
                    protocol: crate::config::CacheProtocol::Auto,
                });
            }

            save_config(&config)?;
            tracing::info!("Token saved for {cache}");
            Ok(())
        }

        AuthCommand::Logout { cache } => {
            let mut config = ClientConfig::load()?;

            if let Some(entry) = config.caches.iter_mut().find(|c| c.url == cache) {
                entry.token = None;
                save_config(&config)?;
                tracing::info!("Token removed for {cache}");
            } else {
                tracing::info!("No credentials found for {cache}");
            }

            Ok(())
        }

        AuthCommand::Status => {
            let config = ClientConfig::load()?;

            if config.caches.is_empty() {
                tracing::info!("No caches configured");
                return Ok(());
            }

            for cache in &config.caches {
                let auth_status = if cache.token.is_some() {
                    "authenticated"
                } else {
                    "no token"
                };
                let protocol = format!("{:?}", cache.protocol).to_lowercase();
                tracing::info!(
                    "{} (priority={}, protocol={}, {})",
                    cache.url,
                    cache.priority,
                    protocol,
                    auth_status,
                );
            }

            Ok(())
        }
    }
}

fn save_config(config: &ClientConfig) -> color_eyre::Result<()> {
    let config_dir = directories::ProjectDirs::from("", "", "ekapkgs")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("~/.config/ekapkgs"));

    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("config.toml");

    // Serialize manually to TOML since we need serde::Serialize.
    let mut content = String::new();

    content.push_str("[defaults]\n");
    content.push_str(&format!(
        "max_parallel_downloads = {}\n",
        config.defaults.max_parallel_downloads
    ));
    content.push('\n');

    for cache in &config.caches {
        content.push_str("[[caches]]\n");
        content.push_str(&format!("url = {:?}\n", cache.url));
        if let Some(ref key) = cache.trusted_key {
            content.push_str(&format!("trusted_key = {:?}\n", key));
        }
        if let Some(ref root) = cache.trust_root {
            content.push_str(&format!("trust_root = {:?}\n", root));
        }
        if let Some(ref token) = cache.token {
            content.push_str(&format!("token = {:?}\n", token));
        }
        content.push_str(&format!("priority = {}\n", cache.priority));
        let protocol = format!("{:?}", cache.protocol).to_lowercase();
        content.push_str(&format!("protocol = {:?}\n", protocol));
        content.push('\n');
    }

    std::fs::write(&config_path, content)?;
    Ok(())
}
