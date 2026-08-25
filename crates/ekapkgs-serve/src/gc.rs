use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};
use tokio::sync::mpsc;

/// Parsed GC configuration with sizes as bytes.
pub struct GcConfig {
    pub max_size: u64,
    pub target_size: u64,
    pub gc_interval: Duration,
}

/// Event sent from API handlers to the GC background task.
enum GcEvent {
    Access { hash: String },
}

/// Optional metrics for GC operations.
#[derive(Clone)]
pub struct GcMetrics {
    pub runs_total: prometheus::IntCounter,
    pub paths_evicted_total: prometheus::IntCounter,
    pub bytes_freed_total: prometheus::IntCounter,
    pub cache_size_bytes: prometheus::IntGauge,
    pub cache_paths_total: prometheus::IntGauge,
}

/// Shared handle for recording accesses. Placed in AppState.
///
/// API handlers call `record_access` after successful reads. Events are
/// batched and flushed to SQLite periodically by a background task.
pub struct GcTracker {
    event_tx: mpsc::UnboundedSender<GcEvent>,
}

impl GcTracker {
    /// Record that a store path hash was accessed. Non-blocking.
    pub fn record_access(&self, hash: &str) {
        let _ = self.event_tx.send(GcEvent::Access {
            hash: hash.to_owned(),
        });
    }
}

/// Initialize the GC system: create the tracker, database, and background task.
///
/// Returns the tracker to put in AppState. The background task is spawned
/// automatically and runs for the server's lifetime.
#[allow(clippy::needless_pass_by_value)]
pub fn init(
    cache_root: &Path,
    config: GcConfig,
    gc_metrics: Option<GcMetrics>,
) -> color_eyre::Result<Arc<GcTracker>> {
    let db_path = cache_root.join(".ekapkgs-gc.db");

    // Initialize database.
    let conn = open_db(&db_path)?;
    create_tables(&conn)?;
    drop(conn);

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let tracker = Arc::new(GcTracker { event_tx });

    let max_size = config.max_size;
    let target_size = config.target_size;
    let gc_interval = config.gc_interval;

    // Spawn the event flush + GC loop.
    let db = db_path.clone();
    let root = cache_root.to_path_buf();
    let metrics = gc_metrics;
    tokio::spawn(async move {
        gc_loop(
            event_rx,
            db,
            root,
            max_size,
            target_size,
            gc_interval,
            metrics,
        )
        .await;
    });

    // Spawn the initial scan.
    let scan_db = db_path;
    let scan_root = cache_root.to_path_buf();
    tokio::spawn(async move {
        if let Err(e) = tokio::task::spawn_blocking(move || initial_scan(&scan_db, &scan_root))
            .await
            .unwrap_or_else(|e| Err(color_eyre::eyre::eyre!("join error: {e}")))
        {
            tracing::error!("GC initial scan failed: {e}");
        }
    });

    Ok(tracker)
}

/// The main background loop: flushes access events and runs periodic GC.
async fn gc_loop(
    mut event_rx: mpsc::UnboundedReceiver<GcEvent>,
    db_path: PathBuf,
    cache_root: PathBuf,
    max_size: u64,
    target_size: u64,
    gc_interval: Duration,
    gc_metrics: Option<GcMetrics>,
) {
    let mut interval = tokio::time::interval(gc_interval);
    let mut pending: Vec<String> = Vec::new();

    loop {
        tokio::select! {
            Some(event) = event_rx.recv() => {
                match event {
                    GcEvent::Access { hash } => pending.push(hash),
                }

                if pending.len() >= 500 {
                    let batch = std::mem::take(&mut pending);
                    let db = db_path.clone();
                    let _ = tokio::task::spawn_blocking(move || flush_accesses(&db, &batch)).await;
                }
            }

            _ = interval.tick() => {
                if !pending.is_empty() {
                    let batch = std::mem::take(&mut pending);
                    let db = db_path.clone();
                    let _ = tokio::task::spawn_blocking(move || flush_accesses(&db, &batch)).await;
                }

                let db = db_path.clone();
                let root = cache_root.clone();
                let m = gc_metrics.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = maybe_run_gc(&db, &root, max_size, target_size, m.as_ref()) {
                        tracing::error!("GC failed: {e}");
                    }
                }).await;
            }
        }
    }
}

// --- Database helpers ---

fn open_db(path: &Path) -> color_eyre::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    Ok(conn)
}

fn create_tables(conn: &Connection) -> color_eyre::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS store_paths (
            hash         TEXT PRIMARY KEY,
            store_path   TEXT NOT NULL,
            nar_url      TEXT NOT NULL,
            file_size    INTEGER NOT NULL,
            narinfo_size INTEGER NOT NULL,
            last_access  INTEGER NOT NULL,
            added_at     INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS refs (
            referrer TEXT NOT NULL,
            referee  TEXT NOT NULL,
            PRIMARY KEY (referrer, referee)
        );
        CREATE INDEX IF NOT EXISTS idx_refs_referee ON refs(referee);
        CREATE INDEX IF NOT EXISTS idx_store_paths_last_access ON store_paths(last_access);",
    )?;
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Batch update last_access for a set of hashes.
fn flush_accesses(db_path: &Path, hashes: &[String]) {
    let conn = match open_db(db_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("GC flush: failed to open db: {e}");
            return;
        },
    };

    let now = now_unix();
    if let Ok(tx) = conn.unchecked_transaction() {
        for hash in hashes {
            let _ = tx.execute(
                "UPDATE store_paths SET last_access = ?1 WHERE hash = ?2",
                params![now as i64, hash],
            );
        }
        let _ = tx.commit();
    }
}

// --- GC algorithm ---

/// Check total size and run GC if over max_size.
fn maybe_run_gc(
    db_path: &Path,
    cache_root: &Path,
    max_size: u64,
    target_size: u64,
    gc_metrics: Option<&GcMetrics>,
) -> color_eyre::Result<()> {
    let conn = open_db(db_path)?;

    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(file_size + narinfo_size), 0) FROM store_paths",
        [],
        |row| row.get(0),
    )?;

    let total = total as u64;
    if total <= max_size {
        return Ok(());
    }

    tracing::info!(
        "GC triggered: cache is {} ({} over limit)",
        format_bytes(total),
        format_bytes(total - max_size),
    );

    run_gc(&conn, cache_root, total, target_size, gc_metrics)
}

/// Reference-aware LRU eviction.
///
/// Iteratively finds paths with no external referrers (roots) and evicts
/// the oldest ones first. Evicting a root may unpin its dependencies,
/// making them eligible for eviction in subsequent iterations.
fn run_gc(
    conn: &Connection,
    cache_root: &Path,
    current_size: u64,
    target_size: u64,
    gc_metrics: Option<&GcMetrics>,
) -> color_eyre::Result<()> {
    let to_free = current_size.saturating_sub(target_size);
    if to_free == 0 {
        return Ok(());
    }

    // Load all paths ordered by last access (oldest first).
    struct PathEntry {
        hash: String,
        nar_url: String,
        size: u64,
    }

    let mut stmt = conn.prepare(
        "SELECT hash, nar_url, file_size + narinfo_size AS total_size
         FROM store_paths ORDER BY last_access ASC",
    )?;

    let entries: Vec<PathEntry> = stmt
        .query_map([], |row| {
            let size: i64 = row.get(2)?;
            Ok(PathEntry {
                hash: row.get(0)?,
                nar_url: row.get(1)?,
                size: size as u64,
            })
        })?
        .filter_map(std::result::Result::ok)
        .collect();

    // Build reverse reference graph: referee -> set of referrers.
    let mut referenced_by: HashMap<String, HashSet<String>> = HashMap::new();
    let mut stmt = conn.prepare("SELECT referrer, referee FROM refs")?;
    let refs: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(std::result::Result::ok)
        .collect();

    let all_hashes: HashSet<&str> = entries.iter().map(|e| e.hash.as_str()).collect();

    for (referrer, referee) in &refs {
        // Only track references from paths that exist in our set.
        if all_hashes.contains(referrer.as_str()) {
            referenced_by
                .entry(referee.clone())
                .or_default()
                .insert(referrer.clone());
        }
    }

    let mut evict_set: HashSet<String> = HashSet::new();
    let mut freed: u64 = 0;

    // Iteratively find and evict "root" paths (those with no external referrers).
    loop {
        if freed >= to_free {
            break;
        }

        let mut evicted_any = false;

        for entry in &entries {
            if evict_set.contains(&entry.hash) {
                continue;
            }

            // A path is evictable if no retained path references it.
            // Self-references don't count as external.
            let pinned = referenced_by
                .get(&entry.hash)
                .map(|referrers| {
                    referrers.iter().any(|r| {
                        r != &entry.hash
                            && all_hashes.contains(r.as_str())
                            && !evict_set.contains(r)
                    })
                })
                .unwrap_or(false);

            if !pinned {
                evict_set.insert(entry.hash.clone());
                freed += entry.size;
                evicted_any = true;

                if freed >= to_free {
                    break;
                }
            }
        }

        if !evicted_any {
            tracing::warn!("GC: all remaining paths are pinned by references");
            break;
        }
    }

    if evict_set.is_empty() {
        return Ok(());
    }

    tracing::info!(
        "GC: evicting {} paths, freeing {}",
        evict_set.len(),
        format_bytes(freed),
    );

    // Delete files and DB entries in batches.
    let to_evict: Vec<&PathEntry> = entries
        .iter()
        .filter(|e| evict_set.contains(&e.hash))
        .collect();

    for chunk in to_evict.chunks(50) {
        let tx = conn.unchecked_transaction()?;
        for entry in chunk {
            let narinfo_path = cache_root.join(format!("{}.narinfo", entry.hash));
            if let Err(e) = std::fs::remove_file(&narinfo_path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("GC: failed to delete {}: {e}", narinfo_path.display());
                }
            }

            let nar_path = cache_root.join(&entry.nar_url);
            if let Err(e) = std::fs::remove_file(&nar_path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("GC: failed to delete {}: {e}", nar_path.display());
                }
            }

            let _ = tx.execute("DELETE FROM refs WHERE referrer = ?1", params![entry.hash]);
            let _ = tx.execute("DELETE FROM refs WHERE referee = ?1", params![entry.hash]);
            let _ = tx.execute(
                "DELETE FROM store_paths WHERE hash = ?1",
                params![entry.hash],
            );
        }
        tx.commit()?;
    }

    // Update metrics.
    if let Some(m) = gc_metrics {
        m.runs_total.inc();
        m.paths_evicted_total.inc_by(evict_set.len() as u64);
        m.bytes_freed_total.inc_by(freed);
    }

    let remaining: i64 = conn.query_row(
        "SELECT COALESCE(SUM(file_size + narinfo_size), 0) FROM store_paths",
        [],
        |row| row.get(0),
    )?;
    let path_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM store_paths", [], |row| row.get(0))?;

    if let Some(m) = gc_metrics {
        m.cache_size_bytes.set(remaining);
        m.cache_paths_total.set(path_count);
    }

    tracing::info!(
        "GC complete: cache is now {}",
        format_bytes(remaining as u64)
    );

    Ok(())
}

// --- Initial scan ---

/// Walk the cache directory and register any paths not yet in the DB.
fn initial_scan(db_path: &Path, cache_root: &Path) -> color_eyre::Result<()> {
    let conn = open_db(db_path)?;
    let now = now_unix();
    let mut count = 0u64;

    let entries = std::fs::read_dir(cache_root)?;
    let tx = conn.unchecked_transaction()?;

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if !name.ends_with(".narinfo") {
            continue;
        }

        let hash = name.strip_suffix(".narinfo").unwrap_or(&name);

        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM store_paths WHERE hash = ?1)",
            params![hash],
            |row| row.get(0),
        )?;

        if exists {
            continue;
        }

        let Ok(narinfo_text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Some(narinfo) = crate::storage::NarInfo::parse(&narinfo_text) else {
            continue;
        };

        let narinfo_size = narinfo_text.len() as u64;
        let nar_path = cache_root.join(&narinfo.url);
        let file_size = std::fs::metadata(&nar_path).map(|m| m.len()).unwrap_or(0);

        tx.execute(
            "INSERT OR IGNORE INTO store_paths (hash, store_path, nar_url, file_size, \
             narinfo_size, last_access, added_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                hash,
                narinfo.store_path,
                narinfo.url,
                file_size as i64,
                narinfo_size as i64,
                now as i64,
                now as i64,
            ],
        )?;

        for reference in &narinfo.references {
            let ref_hash = reference.split('-').next().unwrap_or(reference);
            tx.execute(
                "INSERT OR IGNORE INTO refs (referrer, referee) VALUES (?1, ?2)",
                params![hash, ref_hash],
            )?;
        }

        count += 1;
    }

    tx.commit()?;

    if count > 0 {
        tracing::info!("GC: registered {count} paths from initial scan");
    }

    Ok(())
}

// --- Helpers ---

/// Parse a human-readable byte size string (e.g., "50GiB", "100MB").
pub fn parse_byte_size(s: &str) -> color_eyre::Result<u64> {
    let s = s.trim();
    let (num_str, unit) = s
        .find(|c: char| c.is_alphabetic())
        .map(|i| (&s[..i], &s[i..]))
        .unwrap_or((s, "B"));

    let num: f64 = num_str
        .trim()
        .parse()
        .map_err(|_| color_eyre::eyre::eyre!("invalid number in size: {s}"))?;

    let multiplier: u64 = match unit.to_uppercase().as_str() {
        "B" | "" => 1,
        "KB" | "K" => 1_000,
        "MB" | "M" => 1_000_000,
        "GB" | "G" => 1_000_000_000,
        "TB" | "T" => 1_000_000_000_000,
        "KIB" | "KI" => 1_024,
        "MIB" | "MI" => 1_048_576,
        "GIB" | "GI" => 1_073_741_824,
        "TIB" | "TI" => 1_099_511_627_776,
        _ => return Err(color_eyre::eyre::eyre!("unknown size unit: {unit}")),
    };

    Ok((num * multiplier as f64) as u64)
}

fn format_bytes(bytes: u64) -> String {
    const GIB: u64 = 1_073_741_824;
    const MIB: u64 = 1_048_576;
    const KIB: u64 = 1_024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_byte_size_gib() {
        assert_eq!(parse_byte_size("50GiB").unwrap(), 50 * 1_073_741_824);
    }

    #[test]
    fn parse_byte_size_gb() {
        assert_eq!(parse_byte_size("100GB").unwrap(), 100_000_000_000);
    }

    #[test]
    fn parse_byte_size_mib() {
        assert_eq!(parse_byte_size("512MiB").unwrap(), 512 * 1_048_576);
    }

    #[test]
    fn parse_byte_size_bytes() {
        assert_eq!(parse_byte_size("1024").unwrap(), 1024);
        assert_eq!(parse_byte_size("1024B").unwrap(), 1024);
    }

    #[test]
    fn parse_byte_size_fractional() {
        assert_eq!(parse_byte_size("1.5GiB").unwrap(), 1_610_612_736);
    }

    #[test]
    fn parse_byte_size_invalid() {
        assert!(parse_byte_size("abc").is_err());
        assert!(parse_byte_size("50XYZ").is_err());
    }

    #[test]
    fn format_bytes_display() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1_048_576), "1.0 MiB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GiB");
    }
}
