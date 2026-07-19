pub mod key_repo;
pub mod litellm;
pub mod migrations;
pub mod pricing_repo;
pub mod provider_repo;
pub mod response_continuation_repo;
pub mod route_repo;
pub mod schema;
pub mod usage_repo;

use std::{
    str::FromStr,
    sync::atomic::{AtomicU64, Ordering},
};

pub use key_repo::KeyRepo;
pub use litellm::{
    convert_litellm_json, LiteLlmConvertStats, DEFAULT_LITELLM_PRICING_URL, LITELLM_CACHE_FILENAME,
};
pub use migrations::run_migrations;
pub use pricing_repo::{PricingRepo, PricingSnapshot, DEFAULT_PRICING_JSON};
pub use provider_repo::ProviderRepo;
pub use response_continuation_repo::{ResponseContinuationRepo, RESPONSE_CONTINUATION_TTL};
pub use route_repo::RouteRepo;
pub use schema::{
    secret_key_id_from_ref, AppEventRow, DownstreamKeyRow, PricingRow, ProviderRow, RouteRow,
    UsageAttemptRow, UsageRecordRow,
};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use thiserror::Error;
pub use usage_repo::{
    clamp_tz_offset_minutes, new_usage_attempt, new_usage_record, UsageDayRow, UsageListOpts,
    UsageListPage, UsageListSort, UsageModelRow, UsageOutcomeSummary, UsageProviderRow, UsageRepo,
    UsageSummaryRow,
};

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlx error: {0}")]
    Sqlx(String),

    #[error("migration error: {0}")]
    Migration(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("serialization error: {0}")]
    Serialization(String),
}

// ── Pool handle ───────────────────────────────────────────────────────────────

/// The application-wide SQLite connection pool.
///
/// Call `open_db` once at startup and share the returned pool via `Arc` (or
/// simply clone it — `SqlitePool` is internally reference-counted).
pub type StorePool = SqlitePool;

/// Open (or create) the SQLite database at `url` and run schema migrations.
///
/// `url` may be:
/// - `"sqlite:///abs/path/to/conduit.db"` for a file-backed database.
/// - `"sqlite::memory:"` for an in-process ephemeral database (tests).
///
/// Pool sizing is intentionally modest: SQLite has a single writer. Gateway
/// usage inserts are offloaded to a single async writer (see conduitd
/// `usage_wire`), so the pool is mostly for short console/auth reads. Too many
/// connections increase lock contention and slow-acquire warnings under load.
/// Sequence for unique shared-cache in-memory DB names (isolates parallel tests).
static MEMORY_DB_SEQ: AtomicU64 = AtomicU64::new(1);

pub async fn open_db(url: &str) -> Result<StorePool, StoreError> {
    // Private `sqlite::memory:` is per-connection (broken with pool size > 1).
    // Map to a *unique* shared-cache memory URI so one open_db() pool shares one
    // DB across connections, without cross-talk between concurrent tests.
    let is_memory = matches!(
        url.trim(),
        "sqlite::memory:" | "sqlite://:memory:" | ":memory:"
    );
    let memory_url;
    let url = if is_memory {
        let n = MEMORY_DB_SEQ.fetch_add(1, Ordering::Relaxed);
        memory_url = format!("sqlite:file:conduit_mem_{n}?mode=memory&cache=shared");
        memory_url.as_str()
    } else {
        url
    };

    let opts = SqliteConnectOptions::from_str(url)
        .map_err(|e| StoreError::Migration(e.to_string()))?
        .create_if_missing(true)
        // Enable foreign-key constraint enforcement.
        .foreign_keys(true)
        // WAL mode: concurrent reads don't block writes.
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // NORMAL: flush after each transaction; safe with WAL.
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        // Bound SQLITE_BUSY waits (usage writer is serialized; readers should
        // not sit for multi-second acquires under normal load).
        .busy_timeout(std::time::Duration::from_secs(3))
        // Keep temp tables in memory, not on disk.
        .pragma("temp_store", "MEMORY")
        // ~64 MiB page cache (negative = KiB units).
        .pragma("cache_size", "-65536")
        // Memory-map the DB for faster reads on large usage tables.
        .pragma("mmap_size", "268435456")
        // Checkpoint more eagerly so WAL does not grow unbounded under write load.
        .pragma("wal_autocheckpoint", "1000");

    let (max_connections, min_connections) = if is_memory {
        // Small multi-conn pool so async usage writer + list() can overlap in tests.
        (4, 1)
    } else {
        // Console reads + auth lookups; usage inserts are batched on one writer task.
        (12, 2)
    };

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .min_connections(min_connections)
        .acquire_timeout(std::time::Duration::from_secs(8))
        .connect_with(opts)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

    run_migrations(&pool).await?;

    Ok(pool)
}
