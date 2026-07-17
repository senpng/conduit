use std::path::Path;

use chrono::{DateTime, Utc};
use sqlx::{sqlite::SqliteConnectOptions, SqlitePool};
use tracing::debug;

use crate::{error::TraceError, schema::TraceIndexRow};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const INDEX_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS trace_index (
    id TEXT PRIMARY KEY,
    trace_id TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL DEFAULT '',
    ts TEXT NOT NULL,
    downstream_key_id TEXT,
    alias TEXT NOT NULL,
    provider_id TEXT,
    model_id TEXT,
    status_code INTEGER NOT NULL,
    latency_ms INTEGER NOT NULL,
    prompt_tokens INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens INTEGER NOT NULL DEFAULT 0,
    cost_usd REAL NOT NULL DEFAULT 0,
    error_kind TEXT,
    segment TEXT NOT NULL,
    offset INTEGER NOT NULL
);
"#;

/// Indexes that may reference columns added by migration — applied after ALTER.
const INDEX_INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_trace_ts ON trace_index(ts DESC);
CREATE INDEX IF NOT EXISTS idx_trace_key ON trace_index(downstream_key_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_trace_model ON trace_index(provider_id, model_id, ts DESC);
CREATE INDEX IF NOT EXISTS idx_trace_trace_id ON trace_index(trace_id, ts ASC);
CREATE INDEX IF NOT EXISTS idx_trace_kind ON trace_index(kind, ts DESC);
"#;

const SELECT_COLS: &str = r#"
    id,
    COALESCE(trace_id, '') as trace_id,
    COALESCE(kind, '') as kind,
    ts, downstream_key_id, alias, provider_id, model_id,
    status_code, latency_ms, prompt_tokens, completion_tokens,
    reasoning_tokens, cache_read_tokens, cache_write_tokens,
    cost_usd, error_kind, segment, offset
"#;

// ---------------------------------------------------------------------------
// TraceFilter
// ---------------------------------------------------------------------------

/// Query parameters for [`TraceIndex::query`].
#[derive(Debug, Clone, Default)]
pub struct TraceFilter {
    pub limit: usize,
    pub offset: usize,
    pub downstream_key_id: Option<String>,
    pub provider_id: Option<String>,
    pub model_id: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub status_code: Option<u16>,
    /// When set, only rows of this event kind (e.g. `"request_received"`).
    pub kind: Option<String>,
    pub trace_id: Option<String>,
}

// ---------------------------------------------------------------------------
// TraceIndex
// ---------------------------------------------------------------------------

/// SQLite-backed metadata index for fast querying without reading segment files.
pub struct TraceIndex {
    pool: SqlitePool,
}

impl TraceIndex {
    /// Open (or create) the SQLite index at `db_path`.
    pub async fn open(db_path: &Path) -> Result<Self, TraceError> {
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            // Avoid indefinite hang if a previous process left a WAL lock.
            .busy_timeout(std::time::Duration::from_secs(5));

        let pool = SqlitePool::connect_with(opts)
            .await
            .map_err(|e| TraceError::Database(e.to_string()))?;

        // Recover from interrupted writers (kill -9 / force-quit).
        let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE);")
            .execute(&pool)
            .await;

        // 1) Base table (no-op if already present with older columns).
        for stmt in INDEX_SCHEMA.split(';') {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .map_err(|e| TraceError::Database(e.to_string()))?;
        }

        // 2) Migrate pre-audit DBs before creating indexes on new columns.
        for alter in [
            "ALTER TABLE trace_index ADD COLUMN trace_id TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE trace_index ADD COLUMN kind TEXT NOT NULL DEFAULT ''",
        ] {
            let _ = sqlx::query(alter).execute(&pool).await;
        }

        // 3) Indexes (safe after columns exist).
        for stmt in INDEX_INDEXES.split(';') {
            let stmt = stmt.trim();
            if stmt.is_empty() {
                continue;
            }
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .map_err(|e| TraceError::Database(e.to_string()))?;
        }

        debug!(path = %db_path.display(), "trace index opened");
        Ok(Self { pool })
    }

    /// Insert a single row into the index.  Silently ignores duplicate IDs.
    pub async fn insert(&self, row: &TraceIndexRow) -> Result<(), TraceError> {
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO trace_index
                (id, trace_id, kind, ts, downstream_key_id, alias, provider_id, model_id,
                 status_code, latency_ms, prompt_tokens, completion_tokens,
                 reasoning_tokens, cache_read_tokens, cache_write_tokens,
                 cost_usd, error_kind, segment, offset)
            VALUES
                (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&row.id)
        .bind(&row.trace_id)
        .bind(&row.kind)
        .bind(&row.ts)
        .bind(&row.downstream_key_id)
        .bind(&row.alias)
        .bind(&row.provider_id)
        .bind(&row.model_id)
        .bind(row.status_code)
        .bind(row.latency_ms)
        .bind(row.prompt_tokens)
        .bind(row.completion_tokens)
        .bind(row.reasoning_tokens)
        .bind(row.cache_read_tokens)
        .bind(row.cache_write_tokens)
        .bind(row.cost_usd)
        .bind(&row.error_kind)
        .bind(&row.segment)
        .bind(row.offset)
        .execute(&self.pool)
        .await
        .map_err(|e| TraceError::Database(e.to_string()))?;

        Ok(())
    }

    /// Fetch a single row by its event ULID.
    pub async fn get(&self, id: &str) -> Result<Option<TraceIndexRow>, TraceError> {
        let sql = format!("SELECT {SELECT_COLS} FROM trace_index WHERE id = ?");
        let row = sqlx::query_as::<_, TraceIndexRow>(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| TraceError::Database(e.to_string()))?;
        Ok(row)
    }

    /// All index rows for a shared `trace_id`, ordered chronologically.
    pub async fn list_by_trace_id(&self, trace_id: &str) -> Result<Vec<TraceIndexRow>, TraceError> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM trace_index WHERE trace_id = ? OR id = ? ORDER BY ts ASC"
        );
        let rows = sqlx::query_as::<_, TraceIndexRow>(&sql)
            .bind(trace_id)
            .bind(trace_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| TraceError::Database(e.to_string()))?;
        Ok(rows)
    }

    /// Query the index with the supplied filter.
    pub async fn query(&self, filter: &TraceFilter) -> Result<Vec<TraceIndexRow>, TraceError> {
        let mut conditions: Vec<String> = Vec::new();
        let mut params: Vec<String> = Vec::new();

        if let Some(ref kid) = filter.downstream_key_id {
            conditions.push("downstream_key_id = ?".to_string());
            params.push(kid.clone());
        }
        if let Some(ref pid) = filter.provider_id {
            conditions.push("provider_id = ?".to_string());
            params.push(pid.clone());
        }
        if let Some(ref mid) = filter.model_id {
            conditions.push("model_id = ?".to_string());
            params.push(mid.clone());
        }
        if let Some(since) = filter.since {
            conditions.push("ts >= ?".to_string());
            params.push(since.to_rfc3339());
        }
        if let Some(until) = filter.until {
            conditions.push("ts <= ?".to_string());
            params.push(until.to_rfc3339());
        }
        if let Some(sc) = filter.status_code {
            conditions.push("status_code = ?".to_string());
            params.push(sc.to_string());
        }
        if let Some(ref kind) = filter.kind {
            conditions.push("kind = ?".to_string());
            params.push(kind.clone());
        }
        if let Some(ref tid) = filter.trace_id {
            conditions.push("trace_id = ?".to_string());
            params.push(tid.clone());
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let limit = if filter.limit == 0 { 50 } else { filter.limit };
        let sql = format!(
            r#"
            SELECT {SELECT_COLS}
            FROM trace_index
            {where_clause}
            ORDER BY ts DESC
            LIMIT {limit} OFFSET {}
            "#,
            filter.offset
        );

        let mut q = sqlx::query_as::<sqlx::Sqlite, TraceIndexRow>(&sql);
        for p in &params {
            q = q.bind(p.as_str());
        }

        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| TraceError::Database(e.to_string()))?;

        Ok(rows)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::TraceIndexRow;

    fn sample_row(id: &str, key: &str, ts: &str) -> TraceIndexRow {
        TraceIndexRow {
            id: id.to_string(),
            trace_id: id.to_string(),
            kind: "request_received".to_string(),
            ts: ts.to_string(),
            downstream_key_id: Some(key.to_string()),
            alias: "gpt-4o".to_string(),
            provider_id: Some("openai".to_string()),
            model_id: Some("gpt-4o".to_string()),
            status_code: 200,
            latency_ms: 500,
            prompt_tokens: 100,
            completion_tokens: 50,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: 0.01,
            error_kind: None,
            segment: "2026-05-17.0.cdlog".to_string(),
            offset: 0,
        }
    }

    async fn open_tmp_index() -> (TraceIndex, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("trace.db");
        let idx = TraceIndex::open(&db_path).await.unwrap();
        (idx, tmp)
    }

    #[tokio::test]
    async fn insert_and_get() {
        let (idx, _tmp) = open_tmp_index().await;
        let row = sample_row("id1", "k1", "2026-05-17T00:00:00Z");
        idx.insert(&row).await.unwrap();
        let got = idx.get("id1").await.unwrap().unwrap();
        assert_eq!(got.alias, "gpt-4o");
        assert_eq!(got.kind, "request_received");
    }

    #[tokio::test]
    async fn insert_or_ignore_duplicate() {
        let (idx, _tmp) = open_tmp_index().await;
        let row = sample_row("id1", "k1", "2026-05-17T00:00:00Z");
        idx.insert(&row).await.unwrap();
        idx.insert(&row).await.unwrap();
    }

    #[tokio::test]
    async fn query_by_key() {
        let (idx, _tmp) = open_tmp_index().await;
        idx.insert(&sample_row("a", "k1", "2026-05-17T01:00:00Z"))
            .await
            .unwrap();
        idx.insert(&sample_row("b", "k2", "2026-05-17T02:00:00Z"))
            .await
            .unwrap();
        let rows = idx
            .query(&TraceFilter {
                limit: 10,
                downstream_key_id: Some("k1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "a");
    }

    #[tokio::test]
    async fn list_by_trace_id() {
        let (idx, _tmp) = open_tmp_index().await;
        let mut a = sample_row("e1", "k1", "2026-05-17T01:00:00Z");
        a.trace_id = "t-shared".into();
        a.kind = "request_received".into();
        let mut b = sample_row("e2", "k1", "2026-05-17T01:00:01Z");
        b.trace_id = "t-shared".into();
        b.kind = "final_usage".into();
        idx.insert(&a).await.unwrap();
        idx.insert(&b).await.unwrap();
        let rows = idx.list_by_trace_id("t-shared").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "e1");
        assert_eq!(rows[1].id, "e2");
    }

    #[tokio::test]
    async fn query_missing_returns_empty() {
        let (idx, _tmp) = open_tmp_index().await;
        let rows = idx
            .query(&TraceFilter {
                limit: 10,
                model_id: Some("nope".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(rows.is_empty());
    }
}
