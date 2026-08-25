use std::path::PathBuf;

use ekapkgs_nix::installable::Installable;
use ekapkgs_nix::{NixCommand, eval, store};
use futures::StreamExt;

use crate::cli::{AuthCommand, CacheCommand};
use crate::config::ClientConfig;

pub fn execute(command: CacheCommand) -> color_eyre::Result<()> {
    match command {
        CacheCommand::Push {
            paths,
            cache,
            sources_only,
        } => {
            if sources_only {
                cmd_push_sources(&paths, cache.as_deref())
            } else {
                cmd_push(&paths, cache.as_deref())
            }
        },
        CacheCommand::Pull { paths, cache } => cmd_pull(&paths, cache.as_deref()),
        CacheCommand::Warm {
            installable,
            from_flake_lock_diff,
            old,
            new,
            cache,
        } => cmd_warm(
            &installable,
            from_flake_lock_diff.as_deref(),
            old.as_deref(),
            new.as_deref(),
            cache.as_deref(),
        ),
        CacheCommand::Auth { command } => cmd_auth(command),
    }
}

// --- push ---

fn cmd_push(paths: &[String], cache_url: Option<&str>) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;

    let server_url = match cache_url {
        Some(url) => url.to_owned(),
        None => {
            let cache = config.primary_cache().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "no cache configured — use --cache or configure in config.toml"
                )
            })?;
            cache.url.clone()
        },
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
                let base = base_url.to_owned();
                let token = token.clone();
                let path = path.clone();
                async move { push_single_path(&client, &base, token.as_deref(), &path).await }
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
                },
                Ok(PushResult::AlreadyExists) => {
                    skipped += 1;
                    bar.inc(1);
                },
                Err(e) => {
                    tracing::warn!("Push failed: {e}");
                    failed += 1;
                    bar.inc(1);
                },
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

// --- push --sources-only ---

fn cmd_push_sources(paths: &[String], cache_url: Option<&str>) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;

    let server_url = match cache_url {
        Some(url) => url.to_owned(),
        None => {
            let cache = config.primary_cache().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "no cache configured — use --cache or configure in config.toml"
                )
            })?;
            cache.url.clone()
        },
    };

    let token = config.push_token(&server_url);
    let base_url = server_url.trim_end_matches('/');

    for input in paths {
        let inst = Installable::new(input);

        // 1. Get the derivation graph JSON (compressed for transfer).
        tracing::info!("Evaluating derivation graph for {input}...");
        let drv_graph_json = eval::derivation_graph_json(&inst)?;
        let drv_graph_compressed = zstd::bulk::compress(&drv_graph_json, 3)
            .map_err(|e| color_eyre::eyre::eyre!("zstd compress failed: {e}"))?;

        tracing::info!(
            "Derivation graph: {} ({} compressed)",
            ekapkgs_ui::format::format_bytes(drv_graph_json.len() as u64),
            ekapkgs_ui::format::format_bytes(drv_graph_compressed.len() as u64),
        );

        // 2. Identify FOD (fixed-output derivation) paths.
        let fod_paths = eval::extract_fod_paths(&inst)?;
        tracing::info!(
            "Found {} fixed-output derivations (sources)",
            fod_paths.len()
        );

        if fod_paths.is_empty() && drv_graph_json.is_empty() {
            tracing::info!("Nothing to push for {input}");
            continue;
        }

        // 3. Check which FODs the server already has.
        let fod_hashes: Vec<String> = fod_paths
            .iter()
            .filter_map(|p| store::store_path_hash(p).map(String::from))
            .collect();

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let client = reqwest::Client::new();

            // Check which FODs are already on the server.
            let mut to_push = Vec::new();
            let mut already_present = 0u64;

            for (path, hash) in fod_paths.iter().zip(fod_hashes.iter()) {
                let check_url = format!("{base_url}/{hash}.narinfo");
                let resp = client.head(&check_url).send().await?;
                if resp.status().is_success() {
                    already_present += 1;
                } else {
                    to_push.push(path.clone());
                }
            }

            if already_present > 0 {
                tracing::info!("{already_present} FODs already on server");
            }

            if to_push.is_empty() {
                tracing::info!("All FODs already present, pushing derivation graph only");
            } else {
                tracing::info!("{} FODs to push", to_push.len());

                // Push FOD NARs.
                let bar = ekapkgs_ui::progress::item_bar(to_push.len() as u64, "sources");
                let results =
                    futures::stream::iter(to_push.iter())
                        .map(|path| {
                            let client = client.clone();
                            let base = base_url.to_owned();
                            let token = token.clone();
                            let path = path.clone();
                            async move {
                                push_single_path(&client, &base, token.as_deref(), &path).await
                            }
                        })
                        .buffer_unordered(8)
                        .collect::<Vec<_>>()
                        .await;

                for result in &results {
                    match result {
                        Ok(_) => bar.inc(1),
                        Err(e) => {
                            tracing::warn!("FOD push failed: {e}");
                            bar.inc(1);
                        },
                    }
                }
                bar.finish_and_clear();
            }

            // 4. Push the compressed derivation graph as a special object.
            let drv_url = format!("{base_url}/source-manifest");
            let mut req = client
                .put(&drv_url)
                .header("Content-Type", "application/zstd")
                .body(drv_graph_compressed);
            if let Some(t) = &token {
                req = req.header("Authorization", format!("Bearer {t}"));
            }
            match req.send().await {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!("Derivation graph uploaded");
                },
                Ok(resp) => {
                    tracing::warn!(
                        "Derivation graph upload returned {}: server may not support source-only \
                         transfers yet",
                        resp.status()
                    );
                },
                Err(e) => {
                    tracing::warn!("Derivation graph upload failed: {e}");
                },
            }

            tracing::info!(
                "Source-only push complete for {input}: {} FODs transferred",
                to_push.len()
            );

            Ok::<(), color_eyre::Report>(())
        })?;
    }

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
        .map(|r| r.rsplit('/').next().unwrap_or(r).to_owned())
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
        return Err(color_eyre::eyre::eyre!(
            "NAR upload failed: {status} {body}"
        ));
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
                    let closure_output = NixCommand::new(&["path-info", "--recursive", "--json"])
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
        Some(url) => url.to_owned(),
        None => {
            let cache = config.primary_cache().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "no cache configured — use --cache or configure in config.toml"
                )
            })?;
            cache.url.clone()
        },
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
            crate::negotiate::negotiate(&server_url, want_hashes.clone(), have_hashes.clone())
                .await?;

        spinner.finish_and_clear();

        let avail = response.available.len();
        let unavail = response.unavailable.len();

        if avail > 0 {
            tracing::info!("{avail} paths available from cache");

            // Try gRPC streaming first for a single-connection transfer.
            // Fall back to individual HTTP downloads if the server doesn't
            // support the StreamNars RPC.
            match crate::download::stream_and_import(&server_url, &response).await {
                Ok(()) => {
                    tracing::info!("Imported {avail} paths (streamed)");
                },
                Err(e) => {
                    // Check if this is an UNIMPLEMENTED gRPC status, meaning
                    // the server doesn't support streaming.
                    let is_unimplemented = e
                        .downcast_ref::<tonic::Status>()
                        .is_some_and(|s| s.code() == tonic::Code::Unimplemented);

                    if is_unimplemented {
                        tracing::info!("Server does not support streaming, using HTTP downloads");
                        crate::download::download_and_import(
                            &server_url,
                            &response,
                            config.defaults.max_parallel_downloads,
                        )
                        .await?;
                        tracing::info!("Imported {avail} paths");
                    } else {
                        return Err(e);
                    }
                },
            }
        }

        if unavail > 0 {
            tracing::warn!("{unavail} paths not available on cache");
        }

        Ok::<(), color_eyre::Report>(())
    })?;

    Ok(())
}

// --- warm ---

fn cmd_warm(
    installable: &str,
    from_flake_lock_diff: Option<&str>,
    old_lock: Option<&str>,
    new_lock: Option<&str>,
    cache_url: Option<&str>,
) -> color_eyre::Result<()> {
    let config = ClientConfig::load()?;

    let server_url = match cache_url {
        Some(url) => url.to_owned(),
        None => {
            let cache = config.primary_cache().ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "no cache configured — use --cache or configure in config.toml"
                )
            })?;
            cache.url.clone()
        },
    };

    // Resolve old and new flake.lock contents.
    let (old_lock_content, new_lock_content) = if let Some(diff_range) = from_flake_lock_diff {
        // Parse "OLD..NEW" git range (e.g., "HEAD~1..HEAD").
        let (old_ref, new_ref) = diff_range.split_once("..").ok_or_else(|| {
            color_eyre::eyre::eyre!("invalid diff range: expected OLD..NEW, got {diff_range}")
        })?;
        let old = git_show_file(old_ref, "flake.lock")?;
        let new = git_show_file(new_ref, "flake.lock")?;
        (old, new)
    } else {
        let old_path = old_lock.ok_or_else(|| {
            color_eyre::eyre::eyre!("either --from-flake-lock-diff or --old is required")
        })?;
        let new_path = new_lock.unwrap_or("flake.lock");
        let old = std::fs::read_to_string(old_path)?;
        let new = std::fs::read_to_string(new_path)?;
        (old, new)
    };

    // Write temporary flake.lock files for evaluation.
    let temp_dir = tempfile::tempdir()?;
    let old_lock_path = temp_dir.path().join("flake.lock.old");
    let new_lock_path = temp_dir.path().join("flake.lock.new");
    std::fs::write(&old_lock_path, &old_lock_content)?;
    std::fs::write(&new_lock_path, &new_lock_content)?;

    // Evaluate closures at both flake.lock versions.
    tracing::info!("Evaluating old closure...");
    let old_closure = eval_with_lock(installable, &old_lock_path)?;
    tracing::info!("Evaluating new closure...");
    let new_closure = eval_with_lock(installable, &new_lock_path)?;

    // Compute the diff: paths in new closure not in old closure.
    let old_set: std::collections::HashSet<&str> = old_closure.iter().map(String::as_str).collect();
    let diff_paths: Vec<&str> = new_closure
        .iter()
        .filter(|p| !old_set.contains(p.as_str()))
        .map(String::as_str)
        .collect();

    tracing::info!(
        "Closure diff: {} new paths ({} total in new, {} in old)",
        diff_paths.len(),
        new_closure.len(),
        old_closure.len(),
    );

    if diff_paths.is_empty() {
        tracing::info!("No new paths to warm");
        return Ok(());
    }

    // Partition diff paths into local and remote.
    let (have, want) = store::partition_local(
        &diff_paths
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>(),
    )?;

    if want.is_empty() {
        tracing::info!("All {} diff paths already in local store", have.len());
        return Ok(());
    }

    tracing::info!(
        "{} paths to warm ({} already local)",
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

        let response = crate::negotiate::negotiate(&server_url, want_hashes, have_hashes).await?;

        spinner.finish_and_clear();

        let avail = response.available.len();
        let unavail = response.unavailable.len();

        if avail > 0 {
            tracing::info!("{avail} paths available from cache");
            match crate::download::stream_and_import(&server_url, &response).await {
                Ok(()) => {
                    tracing::info!("Warmed {avail} paths (streamed)");
                },
                Err(e) => {
                    let is_unimplemented = e
                        .downcast_ref::<tonic::Status>()
                        .is_some_and(|s| s.code() == tonic::Code::Unimplemented);
                    if is_unimplemented {
                        crate::download::download_and_import(
                            &server_url,
                            &response,
                            config.defaults.max_parallel_downloads,
                        )
                        .await?;
                        tracing::info!("Warmed {avail} paths");
                    } else {
                        return Err(e);
                    }
                },
            }
        }

        if unavail > 0 {
            tracing::warn!("{unavail} paths not available on cache");
        }

        Ok::<(), color_eyre::Report>(())
    })?;

    Ok(())
}

/// Get a file's content at a specific git revision.
fn git_show_file(rev: &str, path: &str) -> color_eyre::Result<String> {
    let output = std::process::Command::new("git")
        .args(["show", &format!("{rev}:{path}")])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(color_eyre::eyre::eyre!(
            "git show {rev}:{path} failed: {stderr}"
        ));
    }

    Ok(String::from_utf8(output.stdout)?)
}

/// Evaluate a closure with a specific flake.lock file.
///
/// Uses `--override-input` to pin the flake's lock to the specified file.
/// Falls back to evaluating with the lock file copied into a temp flake.
fn eval_with_lock(
    installable: &str,
    lock_path: &std::path::Path,
) -> color_eyre::Result<Vec<String>> {
    // Create a temporary directory with the current flake source and the
    // overridden flake.lock.
    let temp_dir = tempfile::tempdir()?;

    // Copy the current directory's flake.nix (and other files) to temp.
    // We only need flake.nix and the lock file.
    if std::path::Path::new("flake.nix").exists() {
        std::fs::copy("flake.nix", temp_dir.path().join("flake.nix"))?;
    }
    std::fs::copy(lock_path, temp_dir.path().join("flake.lock"))?;

    // Copy nix/ directory if it exists (for overlays, modules).
    if std::path::Path::new("nix").is_dir() {
        copy_dir_recursive(std::path::Path::new("nix"), &temp_dir.path().join("nix"))?;
    }

    // Evaluate the installable in the temp directory.
    let inst_path = format!(
        "path:{}#{}",
        temp_dir.path().display(),
        installable.trim_start_matches(".#")
    );
    let inst = Installable::new(&inst_path);
    eval::derivation_closure_paths(&inst).map_err(Into::into)
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
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
        },

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
        },

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
        },
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
