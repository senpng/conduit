use sqlx::{Row, SqlitePool};
use tracing::info;

use crate::StoreError;

/// Full schema SQL executed on every startup.
///
/// Uses `CREATE TABLE IF NOT EXISTS` throughout so it is idempotent and safe
/// to run against an already-migrated database.
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS providers (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    kind              TEXT NOT NULL,
    base_url          TEXT NOT NULL,
    upstream_key_ref  TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS routes (
    id                  TEXT PRIMARY KEY,
    match_alias         TEXT UNIQUE NOT NULL,
    strategy            TEXT NOT NULL,
    targets_json        TEXT NOT NULL,
    retry_policy_json   TEXT NOT NULL,
    enabled             INTEGER NOT NULL DEFAULT 1,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS downstream_keys (
    id                   TEXT PRIMARY KEY,
    name                 TEXT NOT NULL,
    key_hash             TEXT NOT NULL,
    model_whitelist      TEXT NOT NULL DEFAULT '[]',
    monthly_budget_usd   REAL,
    rate_limit_rpm       INTEGER,
    enabled              INTEGER NOT NULL DEFAULT 1,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_downstream_keys_hash
    ON downstream_keys(key_hash);

-- Per-request consumption ledger (not dependent on traces).
CREATE TABLE IF NOT EXISTS usage_records (
    id                   TEXT PRIMARY KEY,
    ts                   TEXT NOT NULL,
    request_id           TEXT NOT NULL,
    downstream_key_id    TEXT,
    alias                TEXT,
    provider_id          TEXT,
    provider_kind        TEXT,
    model_id             TEXT,
    prompt_tokens        INTEGER NOT NULL DEFAULT 0,
    completion_tokens    INTEGER NOT NULL DEFAULT 0,
    total_tokens         INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens     INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens    INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens   INTEGER NOT NULL DEFAULT 0,
    cost_usd             REAL NOT NULL DEFAULT 0,
    stream               INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_usage_ts
    ON usage_records(ts DESC);

CREATE INDEX IF NOT EXISTS idx_usage_key_ts
    ON usage_records(downstream_key_id, ts DESC);

CREATE INDEX IF NOT EXISTS idx_usage_request
    ON usage_records(request_id);

CREATE TABLE IF NOT EXISTS pricing (
    provider_kind           TEXT NOT NULL,
    model_id                TEXT NOT NULL,
    input_per_mtok          REAL NOT NULL,
    output_per_mtok         REAL NOT NULL,
    cache_read_per_mtok     REAL,
    cache_write_per_mtok    REAL,
    reasoning_per_mtok      REAL,
    effective_from          TEXT NOT NULL,
    PRIMARY KEY (provider_kind, model_id)
);

CREATE TABLE IF NOT EXISTS app_events (
    id             TEXT PRIMARY KEY,
    ts             TEXT NOT NULL,
    kind           TEXT NOT NULL,
    message        TEXT NOT NULL,
    metadata_json  TEXT
);

CREATE INDEX IF NOT EXISTS idx_events_ts
    ON app_events(ts DESC);

-- Short-lived Responses API compatibility state.  This is intentionally kept
-- separate from traces: it contains only tool-call metadata required to turn
-- a `previous_response_id` + tool output into a complete upstream request.
CREATE TABLE IF NOT EXISTS response_continuations (
    response_id              TEXT NOT NULL,
    key_scope                TEXT NOT NULL,
    input_items_json         TEXT NOT NULL DEFAULT '[]',
    output_items_json        TEXT NOT NULL DEFAULT '[]',
    expires_at               TEXT NOT NULL,
    created_at               TEXT NOT NULL,
    PRIMARY KEY (response_id, key_scope)
);

CREATE INDEX IF NOT EXISTS idx_response_continuations_expires_at
    ON response_continuations(expires_at);
"#;

/// Apply the schema to the database.
///
/// Executes `SCHEMA_SQL` as a batch of statements.  Because all statements use
/// `IF NOT EXISTS`, this is safe to call on every startup.
pub async fn run_migrations(pool: &SqlitePool) -> Result<(), StoreError> {
    info!("running conduit-store schema migrations");
    sqlx::raw_sql(SCHEMA_SQL)
        .execute(pool)
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;
    // `response_continuations` was initially introduced as a tool-call-only
    // cache.  Add the transcript columns for installations that created that
    // first schema before full Responses continuation support was available.
    for sql in [
        "ALTER TABLE response_continuations ADD COLUMN input_items_json TEXT NOT NULL DEFAULT '[]'",
        "ALTER TABLE response_continuations ADD COLUMN output_items_json TEXT NOT NULL DEFAULT '[]'",
    ] {
        if let Err(error) = sqlx::query(sql).execute(pool).await {
            // SQLite has no `ADD COLUMN IF NOT EXISTS`; a duplicate is expected
            // on every startup after the first successful migration.
            if !error.to_string().contains("duplicate column name") {
                return Err(StoreError::Migration(error.to_string()));
            }
        }
    }
    remove_legacy_response_function_calls_column(pool).await?;
    info!("conduit-store schema up to date");
    Ok(())
}

/// The first continuation schema stored function calls separately. Full
/// Responses output now contains those calls, so rebuild the narrow table
/// without the redundant column while preserving live continuation rows.
async fn remove_legacy_response_function_calls_column(pool: &SqlitePool) -> Result<(), StoreError> {
    let columns = sqlx::query("PRAGMA table_info(response_continuations)")
        .fetch_all(pool)
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;
    let has_legacy_column = columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "function_calls_json");
    if !has_legacy_column {
        return Ok(());
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;
    sqlx::query(
        r#"CREATE TABLE response_continuations_rebuilt (
            response_id       TEXT NOT NULL,
            key_scope         TEXT NOT NULL,
            input_items_json  TEXT NOT NULL DEFAULT '[]',
            output_items_json TEXT NOT NULL DEFAULT '[]',
            expires_at        TEXT NOT NULL,
            created_at        TEXT NOT NULL,
            PRIMARY KEY (response_id, key_scope)
        )"#,
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| StoreError::Migration(e.to_string()))?;
    sqlx::query(
        r#"INSERT INTO response_continuations_rebuilt
               (response_id, key_scope, input_items_json, output_items_json, expires_at, created_at)
           SELECT response_id, key_scope, input_items_json, output_items_json, expires_at, created_at
           FROM response_continuations"#,
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| StoreError::Migration(e.to_string()))?;
    sqlx::query("DROP TABLE response_continuations")
        .execute(&mut *tx)
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;
    sqlx::query("ALTER TABLE response_continuations_rebuilt RENAME TO response_continuations")
        .execute(&mut *tx)
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_response_continuations_expires_at \
         ON response_continuations(expires_at)",
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| StoreError::Migration(e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;
    Ok(())
}
