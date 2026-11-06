//! Hourly cache-status snapshot ring buffer.
//!
//! Records all `CacheStatusResponse` fields into a separate SQLite database
//! (`cache_history.db`) once per hour.  The table is bounded at 1 000 rows;
//! the oldest rows are deleted when the limit is reached.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use crate::AppState;

/// Maximum number of snapshots retained.
const MAX_SNAPSHOTS: u64 = 1000;

/// Interval between snapshots.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(3600);

/// A single point-in-time snapshot of cache metrics.
pub struct Snapshot {
    pub timestamp: u64,
    pub backend: String,
    pub store_paths: u64,
    pub total_nar_size: u64,
    pub chunks: u64,
    pub chunk_bytes: u64,
    pub unique_files: u64,
    pub total_file_chunk_refs: u64,
    pub disk_bytes: u64,
    pub inline_chunks: u64,
    pub compressed_chunks: u64,
    pub gc_runs: u64,
    pub gc_paths_evicted: u64,
    pub gc_bytes_freed: u64,
}

/// Open (or create) the history database with appropriate pragmas.
fn open_db(path: &Path) -> color_eyre::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;PRAGMA synchronous=NORMAL;PRAGMA busy_timeout=5000;PRAGMA \
         foreign_keys=OFF;",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS snapshots (
            timestamp           INTEGER NOT NULL PRIMARY KEY,
            backend             TEXT NOT NULL,
            store_paths         INTEGER NOT NULL,
            total_nar_size      INTEGER NOT NULL,
            chunks              INTEGER NOT NULL,
            chunk_bytes         INTEGER NOT NULL,
            unique_files        INTEGER NOT NULL,
            total_file_chunk_refs INTEGER NOT NULL,
            disk_bytes          INTEGER NOT NULL,
            inline_chunks       INTEGER NOT NULL,
            compressed_chunks   INTEGER NOT NULL,
            gc_runs             INTEGER NOT NULL,
            gc_paths_evicted    INTEGER NOT NULL,
            gc_bytes_freed      INTEGER NOT NULL
        );",
    )?;
    Ok(conn)
}

/// Insert a snapshot and trim the ring buffer.
fn record_snapshot(conn: &Connection, snap: &Snapshot) -> color_eyre::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO snapshots (
            timestamp, backend, store_paths, total_nar_size,
            chunks, chunk_bytes, unique_files, total_file_chunk_refs,
            disk_bytes, inline_chunks, compressed_chunks,
            gc_runs, gc_paths_evicted, gc_bytes_freed
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            snap.timestamp as i64,
            snap.backend,
            snap.store_paths as i64,
            snap.total_nar_size as i64,
            snap.chunks as i64,
            snap.chunk_bytes as i64,
            snap.unique_files as i64,
            snap.total_file_chunk_refs as i64,
            snap.disk_bytes as i64,
            snap.inline_chunks as i64,
            snap.compressed_chunks as i64,
            snap.gc_runs as i64,
            snap.gc_paths_evicted as i64,
            snap.gc_bytes_freed as i64,
        ],
    )?;

    // Trim oldest rows beyond the cap.
    conn.execute(
        "DELETE FROM snapshots WHERE timestamp NOT IN (SELECT timestamp FROM snapshots ORDER BY \
         timestamp DESC LIMIT ?1)",
        params![MAX_SNAPSHOTS as i64],
    )?;

    Ok(())
}

/// Read all snapshots ordered by time ascending.
pub fn read_snapshots(db_path: &Path) -> color_eyre::Result<Vec<Snapshot>> {
    let conn = open_db(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT timestamp, backend, store_paths, total_nar_size,
                chunks, chunk_bytes, unique_files, total_file_chunk_refs,
                disk_bytes, inline_chunks, compressed_chunks,
                gc_runs, gc_paths_evicted, gc_bytes_freed
         FROM snapshots ORDER BY timestamp ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Snapshot {
            timestamp: row.get::<_, i64>(0)? as u64,
            backend: row.get(1)?,
            store_paths: row.get::<_, i64>(2)? as u64,
            total_nar_size: row.get::<_, i64>(3)? as u64,
            chunks: row.get::<_, i64>(4)? as u64,
            chunk_bytes: row.get::<_, i64>(5)? as u64,
            unique_files: row.get::<_, i64>(6)? as u64,
            total_file_chunk_refs: row.get::<_, i64>(7)? as u64,
            disk_bytes: row.get::<_, i64>(8)? as u64,
            inline_chunks: row.get::<_, i64>(9)? as u64,
            compressed_chunks: row.get::<_, i64>(10)? as u64,
            gc_runs: row.get::<_, i64>(11)? as u64,
            gc_paths_evicted: row.get::<_, i64>(12)? as u64,
            gc_bytes_freed: row.get::<_, i64>(13)? as u64,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Collect the current cache status into a [`Snapshot`].
fn collect_snapshot(state: &AppState) -> Snapshot {
    let storage = &state.storage;
    let metrics = &state.metrics;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let mut snap = Snapshot {
        timestamp: now,
        backend: String::new(),
        store_paths: 0,
        total_nar_size: 0,
        chunks: 0,
        chunk_bytes: 0,
        unique_files: 0,
        total_file_chunk_refs: 0,
        disk_bytes: 0,
        inline_chunks: 0,
        compressed_chunks: 0,
        gc_runs: metrics.gc_runs_total.get(),
        gc_paths_evicted: metrics.gc_paths_evicted_total.get(),
        gc_bytes_freed: metrics.gc_bytes_freed_total.get(),
    };

    if storage.supports_cas() {
        snap.backend = "castore".to_owned();
        if let Some(cas) = storage
            .as_any()
            .downcast_ref::<crate::storage::castore::CastoreBackend>()
        {
            let s = cas.cache_stats();
            snap.store_paths = s.paths;
            snap.total_nar_size = s.nar_size;
            snap.chunks = s.chunks;
            snap.chunk_bytes = s.chunk_bytes;
            snap.unique_files = s.unique_files;
            snap.total_file_chunk_refs = s.file_chunk_refs;
            snap.disk_bytes = s.disk_bytes;
            snap.inline_chunks = s.inline_chunks;
            snap.compressed_chunks = s.compressed_chunks;
        }
    } else if storage
        .as_any()
        .downcast_ref::<crate::storage::nix_store::NixStoreBackend>()
        .is_some()
    {
        snap.backend = "nix-store".to_owned();
    } else {
        snap.backend = "filesystem".to_owned();
    }

    snap
}

/// Spawn the background task that records a snapshot every hour.
///
/// Returns the path to the history database for the gRPC handler.
pub fn spawn_history_loop(state: Arc<AppState>, storage_root: &Path) -> PathBuf {
    let db_path = storage_root.join("cache_history.db");
    let path_clone = db_path.clone();

    tokio::spawn(async move {
        // Record one snapshot immediately at startup.
        let db_path = path_clone;
        record_one(&state, &db_path);

        let mut interval = tokio::time::interval(SNAPSHOT_INTERVAL);
        // The first tick fires immediately — we already recorded above, so skip it.
        interval.tick().await;

        loop {
            interval.tick().await;
            record_one(&state, &db_path);
        }
    });

    db_path
}

/// Record a single snapshot, logging errors instead of panicking.
fn record_one(state: &AppState, db_path: &Path) {
    let snap = collect_snapshot(state);
    match open_db(db_path) {
        Ok(conn) => {
            if let Err(e) = record_snapshot(&conn, &snap) {
                tracing::warn!("failed to record cache history snapshot: {e}");
            }
        },
        Err(e) => {
            tracing::warn!("failed to open cache history database: {e}");
        },
    }
}
