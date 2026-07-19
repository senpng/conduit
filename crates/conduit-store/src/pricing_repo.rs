use std::{collections::BTreeMap, path::Path, sync::Arc};

// Shared source of truth for pricing kind fallbacks (see `conduit_ir::pricing`).
use conduit_ir::pricing::pricing_kind_aliases as kind_aliases;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::{
    litellm::{convert_litellm_json, LITELLM_CACHE_FILENAME},
    schema::PricingRow,
    StoreError,
};

// ── Default embedded pricing ──────────────────────────────────────────────────

/// Minimal built-in pricing table (USD per million tokens).
/// Callers should overwrite with a current `pricing.json` from the app dir.
pub const DEFAULT_PRICING_JSON: &str = r#"[
  {"provider_kind":"openai","model_id":"gpt-4o","input_per_mtok":2.50,"output_per_mtok":10.00,"cache_read_per_mtok":1.25,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2024-05-13"},
  {"provider_kind":"openai","model_id":"gpt-4o-mini","input_per_mtok":0.15,"output_per_mtok":0.60,"cache_read_per_mtok":0.075,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2024-07-18"},
  {"provider_kind":"openai","model_id":"o1","input_per_mtok":15.00,"output_per_mtok":60.00,"cache_read_per_mtok":7.50,"cache_write_per_mtok":null,"reasoning_per_mtok":60.00,"effective_from":"2024-12-17"},
  {"provider_kind":"openai","model_id":"o3-mini","input_per_mtok":1.10,"output_per_mtok":4.40,"cache_read_per_mtok":0.55,"cache_write_per_mtok":null,"reasoning_per_mtok":4.40,"effective_from":"2025-01-31"},
  {"provider_kind":"anthropic","model_id":"claude-3-5-sonnet-20241022","input_per_mtok":3.00,"output_per_mtok":15.00,"cache_read_per_mtok":0.30,"cache_write_per_mtok":3.75,"reasoning_per_mtok":null,"effective_from":"2024-10-22"},
  {"provider_kind":"anthropic","model_id":"claude-3-5-haiku-20241022","input_per_mtok":0.80,"output_per_mtok":4.00,"cache_read_per_mtok":0.08,"cache_write_per_mtok":1.00,"reasoning_per_mtok":null,"effective_from":"2024-11-05"},
  {"provider_kind":"anthropic","model_id":"claude-opus-4-5","input_per_mtok":15.00,"output_per_mtok":75.00,"cache_read_per_mtok":1.50,"cache_write_per_mtok":18.75,"reasoning_per_mtok":null,"effective_from":"2025-07-15"},
  {"provider_kind":"claude-oauth","model_id":"claude-opus-4-5","input_per_mtok":15.00,"output_per_mtok":75.00,"cache_read_per_mtok":1.50,"cache_write_per_mtok":18.75,"reasoning_per_mtok":null,"effective_from":"2025-07-15"},
  {"provider_kind":"claude-oauth","model_id":"claude-sonnet-4","input_per_mtok":3.00,"output_per_mtok":15.00,"cache_read_per_mtok":0.30,"cache_write_per_mtok":3.75,"reasoning_per_mtok":null,"effective_from":"2025-05-01"},
  {"provider_kind":"google","model_id":"gemini-2.0-flash","input_per_mtok":0.10,"output_per_mtok":0.40,"cache_read_per_mtok":null,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2025-02-05"},
  {"provider_kind":"grok-oauth","model_id":"grok-4.5","input_per_mtok":3.00,"output_per_mtok":15.00,"cache_read_per_mtok":null,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2025-07-01"},
  {"provider_kind":"grok-oauth","model_id":"grok-4.5-build","input_per_mtok":3.00,"output_per_mtok":15.00,"cache_read_per_mtok":null,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2025-07-01"},
  {"provider_kind":"grok-oauth","model_id":"grok-3","input_per_mtok":3.00,"output_per_mtok":15.00,"cache_read_per_mtok":null,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2025-02-01"},
  {"provider_kind":"grok-oauth","model_id":"grok-2","input_per_mtok":2.00,"output_per_mtok":10.00,"cache_read_per_mtok":null,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2024-12-01"},
  {"provider_kind":"xai","model_id":"grok-4.5","input_per_mtok":3.00,"output_per_mtok":15.00,"cache_read_per_mtok":null,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2025-07-01"},
  {"provider_kind":"codex-oauth","model_id":"gpt-5","input_per_mtok":1.25,"output_per_mtok":10.00,"cache_read_per_mtok":null,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2025-08-01"},
  {"provider_kind":"codex","model_id":"gpt-5","input_per_mtok":1.25,"output_per_mtok":10.00,"cache_read_per_mtok":null,"cache_write_per_mtok":null,"reasoning_per_mtok":null,"effective_from":"2025-08-01"}
]"#;

// ── In-memory snapshot ────────────────────────────────────────────────────────

/// Thread-safe, hot-reloadable pricing snapshot.
#[derive(Clone)]
pub struct PricingSnapshot(Arc<RwLock<Vec<PricingRow>>>);

impl PricingSnapshot {
    fn from_rows(rows: Vec<PricingRow>) -> Self {
        Self(Arc::new(RwLock::new(rows)))
    }

    /// Look up pricing for `(provider_kind, model_id)`.
    ///
    /// Fallback order:
    /// 1. Exact `(provider_kind, model_id)`
    /// 2. Kind aliases (e.g. `grok-oauth` → `xai`)
    /// 3. Prefix match on model_id within the same kind family
    /// 4. Any row with the same `model_id` (last resort)
    pub async fn get(&self, provider_kind: &str, model_id: &str) -> Option<PricingRow> {
        let guard = self.0.read().await;
        let kind = provider_kind.trim();
        let model = model_id.trim();

        if let Some(r) = guard
            .iter()
            .find(|r| r.provider_kind == kind && r.model_id == model)
        {
            return Some(r.clone());
        }

        for alt in kind_aliases(kind).iter().copied() {
            if let Some(r) = guard
                .iter()
                .find(|r| r.provider_kind == alt && r.model_id == model)
            {
                return Some(r.clone());
            }
        }

        // Prefix: "grok-4.5-build" → "grok-4.5"
        for alt in std::iter::once(kind).chain(kind_aliases(kind).iter().copied()) {
            if let Some(r) = guard.iter().find(|r| {
                r.provider_kind == alt
                    && (model.starts_with(&r.model_id) || r.model_id.starts_with(model))
            }) {
                return Some(r.clone());
            }
        }

        guard.iter().find(|r| r.model_id == model).cloned()
    }

    /// Replace the entire in-memory snapshot with new rows.
    pub async fn replace(&self, rows: Vec<PricingRow>) {
        let mut guard = self.0.write().await;
        *guard = rows;
    }

    /// Snapshot all rows.
    pub async fn all(&self) -> Vec<PricingRow> {
        self.0.read().await.clone()
    }
}

// ── PricingRepo ───────────────────────────────────────────────────────────────

pub struct PricingRepo {
    pool: SqlitePool,
    snapshot: PricingSnapshot,
}

impl PricingRepo {
    /// Create a `PricingRepo` and load initial data.
    ///
    /// Load / merge order (later wins on `(provider_kind, model_id)`):
    /// 1. Embedded `DEFAULT_PRICING_JSON`
    /// 2. `{app_dir}/pricing.litellm.json` (last LiteLLM sync cache)
    /// 3. `{app_dir}/pricing.json` (operator overrides)
    ///
    /// Whatever is loaded is also written back to the DB so other processes
    /// see it.
    pub async fn new(pool: SqlitePool, app_dir: &Path) -> Result<Self, StoreError> {
        let rows = Self::load_from_file_or_default(app_dir).await;
        let snapshot = PricingSnapshot::from_rows(rows.clone());
        let repo = Self { pool, snapshot };
        repo.upsert_all(&rows).await?;
        Ok(repo)
    }

    /// Reload pricing from data-dir files (hot reload).
    ///
    /// On success the in-memory snapshot and the DB are both updated.
    pub async fn reload(&self, app_dir: &Path) -> Result<(), StoreError> {
        let rows = Self::load_from_file_or_default(app_dir).await;
        self.upsert_all(&rows).await?;
        self.snapshot.replace(rows).await;
        info!("pricing hot-reloaded");
        Ok(())
    }

    /// Apply a LiteLLM cost-map JSON blob: convert, cache to disk, reload.
    ///
    /// Writes Conduit-format rows to `{app_dir}/pricing.litellm.json`. Operator
    /// overrides in `pricing.json` still win on the subsequent reload merge.
    ///
    /// Returns `(row_count_after_reload, source_models, skipped)`.
    pub async fn apply_litellm_json(
        &self,
        app_dir: &Path,
        litellm_json: &str,
        sync_date: &str,
    ) -> Result<(usize, usize, usize), StoreError> {
        let stats =
            convert_litellm_json(litellm_json, sync_date).map_err(StoreError::Serialization)?;

        let cache_path = app_dir.join(LITELLM_CACHE_FILENAME);
        let text = serde_json::to_string_pretty(&stats.rows)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        if let Some(parent) = cache_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StoreError::Serialization(format!("create data dir: {e}")))?;
        }
        tokio::fs::write(&cache_path, text).await.map_err(|e| {
            StoreError::Serialization(format!("write {LITELLM_CACHE_FILENAME}: {e}"))
        })?;

        info!(
            path = %cache_path.display(),
            source_models = stats.source_models,
            rows = stats.rows.len(),
            skipped = stats.skipped,
            "LiteLLM pricing cache written"
        );

        self.reload(app_dir).await?;
        let total = self.snapshot.all().await.len();
        Ok((total, stats.source_models, stats.skipped))
    }

    /// In-memory lookup — does not hit the DB.
    pub async fn get_price(&self, provider_kind: &str, model_id: &str) -> Option<PricingRow> {
        self.snapshot.get(provider_kind, model_id).await
    }

    /// All rows from the in-memory snapshot.
    pub async fn all(&self) -> Vec<PricingRow> {
        self.snapshot.all().await
    }

    /// Read operator overrides only (`{app_dir}/pricing.json`), not merged layers.
    pub async fn list_overrides(app_dir: &Path) -> Result<Vec<PricingRow>, StoreError> {
        let path = app_dir.join("pricing.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
            StoreError::Serialization(format!("read pricing.json: {e}"))
        })?;
        // Accept either a bare array or tokscale-like `{ "models": { ... } }` is not used —
        // Conduit format is always a JSON array of PricingRow.
        serde_json::from_str(&content).map_err(|e| {
            StoreError::Serialization(format!("parse pricing.json: {e}"))
        })
    }

    /// Upsert one operator override into `pricing.json`, then hot-reload layers.
    pub async fn upsert_override(
        &self,
        app_dir: &Path,
        row: PricingRow,
    ) -> Result<Vec<PricingRow>, StoreError> {
        if row.provider_kind.trim().is_empty() || row.model_id.trim().is_empty() {
            return Err(StoreError::Serialization(
                "provider_kind and model_id are required".into(),
            ));
        }
        if !row.input_per_mtok.is_finite()
            || !row.output_per_mtok.is_finite()
            || row.input_per_mtok < 0.0
            || row.output_per_mtok < 0.0
        {
            return Err(StoreError::Serialization(
                "input_per_mtok and output_per_mtok must be finite and non-negative".into(),
            ));
        }
        if row.input_per_mtok == 0.0 && row.output_per_mtok == 0.0 {
            return Err(StoreError::Serialization(
                "at least one of input_per_mtok or output_per_mtok must be positive".into(),
            ));
        }

        let mut rows = Self::list_overrides(app_dir).await?;
        if let Some(existing) = rows.iter_mut().find(|r| {
            r.provider_kind == row.provider_kind && r.model_id == row.model_id
        }) {
            *existing = row;
        } else {
            rows.push(row);
        }
        rows.sort_by(|a, b| {
            a.provider_kind
                .cmp(&b.provider_kind)
                .then_with(|| a.model_id.cmp(&b.model_id))
        });
        Self::write_overrides_file(app_dir, &rows).await?;
        self.reload(app_dir).await?;
        Ok(rows)
    }

    /// Delete one operator override from `pricing.json`, then hot-reload layers.
    pub async fn delete_override(
        &self,
        app_dir: &Path,
        provider_kind: &str,
        model_id: &str,
    ) -> Result<Vec<PricingRow>, StoreError> {
        let mut rows = Self::list_overrides(app_dir).await?;
        let before = rows.len();
        rows.retain(|r| !(r.provider_kind == provider_kind && r.model_id == model_id));
        if rows.len() == before {
            return Err(StoreError::NotFound(format!(
                "override {provider_kind}/{model_id} not found"
            )));
        }
        Self::write_overrides_file(app_dir, &rows).await?;
        self.reload(app_dir).await?;
        Ok(rows)
    }

    async fn write_overrides_file(app_dir: &Path, rows: &[PricingRow]) -> Result<(), StoreError> {
        if let Some(parent) = app_dir.parent() {
            // app_dir itself should exist; still ensure
            let _ = parent;
        }
        tokio::fs::create_dir_all(app_dir).await.map_err(|e| {
            StoreError::Serialization(format!("create data dir: {e}"))
        })?;
        let path = app_dir.join("pricing.json");
        let text = serde_json::to_string_pretty(rows)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        tokio::fs::write(&path, text)
            .await
            .map_err(|e| StoreError::Serialization(format!("write pricing.json: {e}")))?;
        info!(path = %path.display(), rows = rows.len(), "pricing overrides written");
        Ok(())
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    async fn load_from_file_or_default(app_dir: &Path) -> Vec<PricingRow> {
        let defaults: Vec<PricingRow> =
            serde_json::from_str(DEFAULT_PRICING_JSON).expect("DEFAULT_PRICING_JSON must be valid");

        let mut map: BTreeMap<(String, String), PricingRow> = defaults
            .into_iter()
            .map(|r| ((r.provider_kind.clone(), r.model_id.clone()), r))
            .collect();

        // Layer 2: last LiteLLM sync (optional).
        Self::merge_pricing_file(
            &mut map,
            &app_dir.join(LITELLM_CACHE_FILENAME),
            "litellm cache",
        )
        .await;

        // Layer 3: operator overrides (highest priority).
        Self::merge_pricing_file(&mut map, &app_dir.join("pricing.json"), "pricing.json").await;

        let merged: Vec<PricingRow> = map.into_values().collect();
        info!(rows = merged.len(), "pricing layers merged");
        merged
    }

    async fn merge_pricing_file(
        map: &mut BTreeMap<(String, String), PricingRow>,
        path: &Path,
        label: &str,
    ) {
        if !path.exists() {
            return;
        }
        match tokio::fs::read_to_string(path).await {
            Ok(content) => match serde_json::from_str::<Vec<PricingRow>>(&content) {
                Ok(file_rows) => {
                    let n = file_rows.len();
                    for r in file_rows {
                        map.insert((r.provider_kind.clone(), r.model_id.clone()), r);
                    }
                    info!(
                        path = %path.display(),
                        rows = n,
                        "pricing layer loaded ({label})"
                    );
                }
                Err(e) => warn!("failed to parse {label} ({}): {e}", path.display()),
            },
            Err(e) => warn!("failed to read {label} ({}): {e}", path.display()),
        }
    }

    async fn upsert_all(&self, rows: &[PricingRow]) -> Result<(), StoreError> {
        for row in rows {
            sqlx::query(
                r#"INSERT INTO pricing
                   (provider_kind, model_id, input_per_mtok, output_per_mtok,
                    cache_read_per_mtok, cache_write_per_mtok, reasoning_per_mtok, effective_from)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                   ON CONFLICT(provider_kind, model_id) DO UPDATE SET
                       input_per_mtok        = excluded.input_per_mtok,
                       output_per_mtok       = excluded.output_per_mtok,
                       cache_read_per_mtok   = excluded.cache_read_per_mtok,
                       cache_write_per_mtok  = excluded.cache_write_per_mtok,
                       reasoning_per_mtok    = excluded.reasoning_per_mtok,
                       effective_from        = excluded.effective_from"#,
            )
            .bind(&row.provider_kind)
            .bind(&row.model_id)
            .bind(row.input_per_mtok)
            .bind(row.output_per_mtok)
            .bind(row.cache_read_per_mtok)
            .bind(row.cache_write_per_mtok)
            .bind(row.reasoning_per_mtok)
            .bind(&row.effective_from)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Sqlx(e.to_string()))?;
        }
        Ok(())
    }
}

// ── Serde helper for pricing.json ─────────────────────────────────────────────

/// Alias for `PricingRow` to make the JSON file format self-documenting.
#[derive(Debug, Deserialize, Serialize)]
pub struct PricingEntry {
    pub provider_kind: String,
    pub model_id: String,
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
    pub reasoning_per_mtok: Option<f64>,
    pub effective_from: String,
}

impl From<PricingEntry> for PricingRow {
    fn from(e: PricingEntry) -> Self {
        PricingRow {
            provider_kind: e.provider_kind,
            model_id: e.model_id,
            input_per_mtok: e.input_per_mtok,
            output_per_mtok: e.output_per_mtok,
            cache_read_per_mtok: e.cache_read_per_mtok,
            cache_write_per_mtok: e.cache_write_per_mtok,
            reasoning_per_mtok: e.reasoning_per_mtok,
            effective_from: e.effective_from,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::open_db;

    #[tokio::test]
    async fn default_pricing_loads() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let dir = tempdir().unwrap();
        let repo = PricingRepo::new(pool, dir.path()).await.unwrap();

        let row = repo.get_price("openai", "gpt-4o").await.unwrap();
        assert_eq!(row.input_per_mtok, 2.50);
        assert_eq!(row.output_per_mtok, 10.00);
    }

    #[tokio::test]
    async fn upsert_and_delete_override_roundtrip() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let dir = tempdir().unwrap();
        let repo = PricingRepo::new(pool, dir.path()).await.unwrap();

        let row = PricingRow {
            provider_kind: "openai".into(),
            model_id: "custom-model".into(),
            input_per_mtok: 1.5,
            output_per_mtok: 6.0,
            cache_read_per_mtok: Some(0.75),
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
            effective_from: "2026-01-01".into(),
        };
        let listed = repo.upsert_override(dir.path(), row).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].model_id, "custom-model");

        let got = repo.get_price("openai", "custom-model").await.unwrap();
        assert!((got.input_per_mtok - 1.5).abs() < f64::EPSILON);

        let after = repo
            .delete_override(dir.path(), "openai", "custom-model")
            .await
            .unwrap();
        assert!(after.is_empty());
        assert!(repo.get_price("openai", "custom-model").await.is_none());
    }

    #[tokio::test]
    async fn custom_pricing_file_overrides_default() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let dir = tempdir().unwrap();

        let custom = serde_json::json!([{
            "provider_kind": "openai",
            "model_id": "gpt-4o",
            "input_per_mtok": 1.00,
            "output_per_mtok": 4.00,
            "cache_read_per_mtok": null,
            "cache_write_per_mtok": null,
            "reasoning_per_mtok": null,
            "effective_from": "2025-01-01"
        }]);
        tokio::fs::write(
            dir.path().join("pricing.json"),
            serde_json::to_string(&custom).unwrap(),
        )
        .await
        .unwrap();

        let repo = PricingRepo::new(pool, dir.path()).await.unwrap();
        let row = repo.get_price("openai", "gpt-4o").await.unwrap();
        assert_eq!(row.input_per_mtok, 1.00);
        // Partial file must not wipe other defaults (merge, not replace).
        let mini = repo.get_price("openai", "gpt-4o-mini").await.unwrap();
        assert_eq!(mini.input_per_mtok, 0.15);
    }

    #[tokio::test]
    async fn hot_reload_updates_snapshot() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let dir = tempdir().unwrap();
        let repo = PricingRepo::new(pool, dir.path()).await.unwrap();

        // Write a new pricing.json.
        let updated = serde_json::json!([{
            "provider_kind": "openai",
            "model_id": "gpt-4o",
            "input_per_mtok": 0.50,
            "output_per_mtok": 2.00,
            "cache_read_per_mtok": null,
            "cache_write_per_mtok": null,
            "reasoning_per_mtok": null,
            "effective_from": "2025-06-01"
        }]);
        tokio::fs::write(
            dir.path().join("pricing.json"),
            serde_json::to_string(&updated).unwrap(),
        )
        .await
        .unwrap();

        repo.reload(dir.path()).await.unwrap();
        let row = repo.get_price("openai", "gpt-4o").await.unwrap();
        assert_eq!(row.input_per_mtok, 0.50);
    }

    #[tokio::test]
    async fn litellm_cache_then_user_override() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let dir = tempdir().unwrap();

        let litellm_rows = serde_json::json!([{
            "provider_kind": "openai",
            "model_id": "gpt-4o",
            "input_per_mtok": 9.99,
            "output_per_mtok": 19.99,
            "cache_read_per_mtok": null,
            "cache_write_per_mtok": null,
            "reasoning_per_mtok": null,
            "effective_from": "2026-01-01"
        }]);
        tokio::fs::write(
            dir.path().join(crate::LITELLM_CACHE_FILENAME),
            serde_json::to_string(&litellm_rows).unwrap(),
        )
        .await
        .unwrap();

        let user = serde_json::json!([{
            "provider_kind": "openai",
            "model_id": "gpt-4o",
            "input_per_mtok": 1.00,
            "output_per_mtok": 2.00,
            "cache_read_per_mtok": null,
            "cache_write_per_mtok": null,
            "reasoning_per_mtok": null,
            "effective_from": "2026-02-01"
        }]);
        tokio::fs::write(
            dir.path().join("pricing.json"),
            serde_json::to_string(&user).unwrap(),
        )
        .await
        .unwrap();

        let repo = PricingRepo::new(pool, dir.path()).await.unwrap();
        let row = repo.get_price("openai", "gpt-4o").await.unwrap();
        assert_eq!(row.input_per_mtok, 1.00); // user wins
    }

    #[tokio::test]
    async fn apply_litellm_json_writes_cache_and_reloads() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let dir = tempdir().unwrap();
        let repo = PricingRepo::new(pool, dir.path()).await.unwrap();

        let litellm = r#"{
          "new-model-xyz": {
            "litellm_provider": "openai",
            "mode": "chat",
            "input_cost_per_token": 1e-6,
            "output_cost_per_token": 2e-6
          }
        }"#;
        let (total, source, _skipped) = repo
            .apply_litellm_json(dir.path(), litellm, "2026-07-17")
            .await
            .unwrap();
        assert_eq!(source, 1);
        assert!(total >= 1);
        assert!(dir.path().join(crate::LITELLM_CACHE_FILENAME).exists());

        let row = repo.get_price("openai", "new-model-xyz").await.unwrap();
        assert!((row.input_per_mtok - 1.0).abs() < 1e-9);
        // OAuth alias emitted
        assert!(repo
            .get_price("codex-oauth", "new-model-xyz")
            .await
            .is_some());
    }
}
