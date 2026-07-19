use sqlx::{Row, SqlitePool};
use tracing::info;

use crate::StoreError;

/// Full schema SQL executed on every startup.
///
/// Uses `CREATE TABLE IF NOT EXISTS` throughout so it is idempotent and safe
/// to run against an already-migrated database.
///
/// Soft-delete: `providers`, `routes`, and `downstream_keys` carry a nullable
/// `deleted_at` column. Operator delete sets this timestamp; list/get/auth
/// queries ignore soft-deleted rows. Route `match_alias` uniqueness applies
/// only to active rows (partial unique index).
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS providers (
    id                TEXT PRIMARY KEY,
    name              TEXT NOT NULL,
    kind              TEXT NOT NULL,
    base_url          TEXT NOT NULL,
    upstream_key_ref  TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    deleted_at        TEXT
);

CREATE TABLE IF NOT EXISTS routes (
    id                  TEXT PRIMARY KEY,
    match_alias         TEXT NOT NULL,
    strategy            TEXT NOT NULL,
    targets_json        TEXT NOT NULL,
    retry_policy_json   TEXT NOT NULL,
    enabled             INTEGER NOT NULL DEFAULT 1,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    deleted_at          TEXT
);

-- Partial unique index on match_alias is created after `deleted_at` is guaranteed
-- to exist (see migrate_routes_soft_delete_unique). Putting it here would break
-- upgrades of pre-soft-delete databases where CREATE TABLE IF NOT EXISTS is a no-op.

CREATE TABLE IF NOT EXISTS downstream_keys (
    id                   TEXT PRIMARY KEY,
    name                 TEXT NOT NULL,
    key_hash             TEXT NOT NULL,
    model_whitelist      TEXT NOT NULL DEFAULT '[]',
    monthly_budget_usd   REAL,
    rate_limit_rpm       INTEGER,
    enabled              INTEGER NOT NULL DEFAULT 1,
    created_at           TEXT NOT NULL,
    updated_at           TEXT NOT NULL,
    deleted_at           TEXT
);

CREATE INDEX IF NOT EXISTS idx_downstream_keys_hash
    ON downstream_keys(key_hash);

-- Per-request consumption ledger.
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

-- Short-lived Responses API compatibility state. Contains only tool-call
-- metadata required to turn a `previous_response_id` + tool output into a
-- complete upstream request.
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
    add_soft_delete_columns(pool).await?;
    migrate_routes_soft_delete_unique(pool).await?;
    info!("conduit-store schema up to date");
    Ok(())
}

/// Add `deleted_at` to config tables on pre-soft-delete databases.
async fn add_soft_delete_columns(pool: &SqlitePool) -> Result<(), StoreError> {
    for sql in [
        "ALTER TABLE providers ADD COLUMN deleted_at TEXT",
        "ALTER TABLE routes ADD COLUMN deleted_at TEXT",
        "ALTER TABLE downstream_keys ADD COLUMN deleted_at TEXT",
    ] {
        if let Err(error) = sqlx::query(sql).execute(pool).await {
            if !error.to_string().contains("duplicate column name") {
                return Err(StoreError::Migration(error.to_string()));
            }
        }
    }
    Ok(())
}

/// Older installs created `routes.match_alias` as `UNIQUE` on the table.
/// Soft-delete reuses aliases after delete, so replace that with a partial
/// unique index over active rows only. Always ensures the partial index exists.
async fn migrate_routes_soft_delete_unique(pool: &SqlitePool) -> Result<(), StoreError> {
    let table_sql: String = sqlx::query(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'routes'",
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StoreError::Migration(e.to_string()))?
    .map(|r| r.get::<String, _>("sql"))
    .unwrap_or_default();

    // Detect table-level UNIQUE on match_alias (legacy). PRIMARY KEY alone does
    // not contain the word UNIQUE in SQLite's CREATE TABLE SQL.
    let has_table_unique = table_sql.to_uppercase().contains("MATCH_ALIAS")
        && table_sql
            .to_uppercase()
            .replace("PRIMARY KEY", "")
            .contains("UNIQUE");

    if has_table_unique {
        info!("migrating routes table: drop table-level UNIQUE on match_alias for soft-delete");
        let mut tx = pool
            .begin()
            .await
            .map_err(|e| StoreError::Migration(e.to_string()))?;
        sqlx::query(
            r#"CREATE TABLE routes_rebuilt (
                id                  TEXT PRIMARY KEY,
                match_alias         TEXT NOT NULL,
                strategy            TEXT NOT NULL,
                targets_json        TEXT NOT NULL,
                retry_policy_json   TEXT NOT NULL,
                enabled             INTEGER NOT NULL DEFAULT 1,
                created_at          TEXT NOT NULL,
                updated_at          TEXT NOT NULL,
                deleted_at          TEXT
            )"#,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;
        sqlx::query(
            r#"INSERT INTO routes_rebuilt
                   (id, match_alias, strategy, targets_json, retry_policy_json,
                    enabled, created_at, updated_at, deleted_at)
               SELECT id, match_alias, strategy, targets_json, retry_policy_json,
                      enabled, created_at, updated_at, deleted_at
               FROM routes"#,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| StoreError::Migration(e.to_string()))?;
        sqlx::query("DROP TABLE routes")
            .execute(&mut *tx)
            .await
            .map_err(|e| StoreError::Migration(e.to_string()))?;
        sqlx::query("ALTER TABLE routes_rebuilt RENAME TO routes")
            .execute(&mut *tx)
            .await
            .map_err(|e| StoreError::Migration(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| StoreError::Migration(e.to_string()))?;
    }

    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_routes_match_alias_active \
         ON routes(match_alias) WHERE deleted_at IS NULL",
    )
    .execute(pool)
    .await
    .map_err(|e| StoreError::Migration(e.to_string()))?;
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

#[cfg(test)]
mod soft_delete_migration_tests {
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;

    async fn bare_pool() -> SqlitePool {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn migrates_legacy_routes_unique_to_soft_delete() {
        let pool = bare_pool().await;
        // Pre-soft-delete schema: table-level UNIQUE on match_alias, no deleted_at.
        sqlx::raw_sql(
            r#"
            CREATE TABLE providers (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                base_url TEXT NOT NULL,
                upstream_key_ref TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE routes (
                id TEXT PRIMARY KEY,
                match_alias TEXT UNIQUE NOT NULL,
                strategy TEXT NOT NULL,
                targets_json TEXT NOT NULL,
                retry_policy_json TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE downstream_keys (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                key_hash TEXT NOT NULL,
                model_whitelist TEXT NOT NULL DEFAULT '[]',
                monthly_budget_usd REAL,
                rate_limit_rpm INTEGER,
                enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            INSERT INTO routes
                (id, match_alias, strategy, targets_json, retry_policy_json, enabled, created_at, updated_at)
            VALUES
                ('r1', 'fast', 'fixed', '[]', '{}', 1, 't', 't');
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        run_migrations(&pool).await.unwrap();

        let cols: Vec<String> = sqlx::query("PRAGMA table_info(routes)")
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.get::<String, _>("name"))
            .collect();
        assert!(cols.contains(&"deleted_at".to_string()));

        let table_sql: String = sqlx::query(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'routes'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("sql");
        assert!(
            !table_sql.to_uppercase().contains("UNIQUE"),
            "table-level UNIQUE should be gone: {table_sql}"
        );

        let has_partial = sqlx::query(
            "SELECT 1 AS ok FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_routes_match_alias_active'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap()
        .is_some();
        assert!(has_partial);

        // Soft-delete frees alias for reuse (partial unique index).
        sqlx::query("UPDATE routes SET deleted_at = '2026-01-01T00:00:00Z' WHERE id = 'r1'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO routes
             (id, match_alias, strategy, targets_json, retry_policy_json, enabled, created_at, updated_at, deleted_at)
             VALUES ('r2', 'fast', 'fixed', '[]', '{}', 1, 't', 't', NULL)",
        )
        .execute(&pool)
        .await
        .unwrap();
    }
}
