pub mod key_repo;
pub mod litellm;
pub mod migrations;
pub mod pricing_repo;
pub mod provider_repo;
pub mod response_continuation_repo;
pub mod route_repo;
pub mod schema;
pub mod usage_repo;

use std::str::FromStr;

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
    UsageRecordRow,
};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use thiserror::Error;
pub use usage_repo::{
    new_usage_record, UsageDayRow, UsageListOpts, UsageListPage, UsageListSort, UsageModelRow,
    UsageRepo, UsageSummaryRow,
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
pub async fn open_db(url: &str) -> Result<StorePool, StoreError> {
    let opts = SqliteConnectOptions::from_str(url)
        .map_err(|e| StoreError::Migration(e.to_string()))?
        .create_if_missing(true)
        // Enable foreign-key constraint enforcement.
        .foreign_keys(true)
        // WAL mode: concurrent reads don't block writes.
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        // NORMAL: flush after each transaction; safe with WAL.
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
        // Wait up to 5 s before returning SQLITE_BUSY.
        .busy_timeout(std::time::Duration::from_secs(5))
        // Keep temp tables in memory, not on disk.
        .pragma("temp_store", "MEMORY");

    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await
        .map_err(|e| StoreError::Sqlx(e.to_string()))?;

    run_migrations(&pool).await?;

    Ok(pool)
}
