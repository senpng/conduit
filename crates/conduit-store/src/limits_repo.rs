//! Model token-limits store — separate from pricing.
//!
//! Layers (later wins on `(provider_kind, model_id)`):
//! 1. `{app_dir}/limits.litellm.json` — LiteLLM sync cache
//! 2. `{app_dir}/limits.json` — operator overrides

use std::{collections::BTreeMap, path::Path, sync::Arc};

use conduit_ir::pricing::pricing_kind_aliases as kind_aliases;
use tokio::sync::RwLock;
use tracing::info;

use crate::{
    litellm::{convert_litellm_limits, LITELLM_LIMITS_CACHE_FILENAME},
    schema::ModelLimitsRow,
    StoreError,
};

/// Operator override filename under the data dir.
pub const LIMITS_OVERRIDE_FILENAME: &str = "limits.json";

/// Thread-safe, hot-reloadable model-limits snapshot.
#[derive(Clone)]
pub struct LimitsSnapshot(Arc<RwLock<Vec<ModelLimitsRow>>>);

impl LimitsSnapshot {
    fn from_rows(rows: Vec<ModelLimitsRow>) -> Self {
        Self(Arc::new(RwLock::new(rows)))
    }

    /// Look up limits for `(provider_kind, model_id)`.
    ///
    /// Fallback order mirrors pricing:
    /// exact → kind aliases → model_id prefix within kind family → any same model_id.
    pub async fn get(&self, provider_kind: &str, model_id: &str) -> Option<ModelLimitsRow> {
        let guard = self.0.read().await;
        lookup_limits_rows(&guard, provider_kind, model_id)
    }

    pub async fn replace(&self, rows: Vec<ModelLimitsRow>) {
        let mut guard = self.0.write().await;
        *guard = rows;
    }

    pub async fn all(&self) -> Vec<ModelLimitsRow> {
        self.0.read().await.clone()
    }
}

/// Pure lookup over a slice of limit rows (used by async snapshot + tests).
pub fn lookup_limits_rows(
    rows: &[ModelLimitsRow],
    provider_kind: &str,
    model_id: &str,
) -> Option<ModelLimitsRow> {
    let kind = provider_kind.trim();
    let model = model_id.trim();
    if kind.is_empty() || model.is_empty() {
        return None;
    }

    if let Some(r) = rows
        .iter()
        .find(|r| r.provider_kind == kind && r.model_id == model)
    {
        return Some(r.clone());
    }

    for alt in kind_aliases(kind).iter().copied() {
        if let Some(r) = rows
            .iter()
            .find(|r| r.provider_kind == alt && r.model_id == model)
        {
            return Some(r.clone());
        }
    }

    for alt in std::iter::once(kind).chain(kind_aliases(kind).iter().copied()) {
        if let Some(r) = rows.iter().find(|r| {
            r.provider_kind == alt
                && (model.starts_with(&r.model_id) || r.model_id.starts_with(model))
        }) {
            return Some(r.clone());
        }
    }

    rows.iter().find(|r| r.model_id == model).cloned()
}

/// File-backed model limits (no SQLite table — keeps pricing DB schema price-only).
pub struct LimitsRepo {
    snapshot: LimitsSnapshot,
}

impl LimitsRepo {
    /// Load limits from data-dir files (may be empty).
    pub async fn new(app_dir: &Path) -> Result<Self, StoreError> {
        let rows = Self::load_merged(app_dir).await;
        Ok(Self {
            snapshot: LimitsSnapshot::from_rows(rows),
        })
    }

    pub async fn reload(&self, app_dir: &Path) -> Result<(), StoreError> {
        let rows = Self::load_merged(app_dir).await;
        self.snapshot.replace(rows).await;
        info!("model limits hot-reloaded");
        Ok(())
    }

    /// Convert LiteLLM JSON → limits cache file → reload.
    ///
    /// Returns `(row_count_after_reload, source_models, skipped)`.
    pub async fn apply_litellm_json(
        &self,
        app_dir: &Path,
        litellm_json: &str,
    ) -> Result<(usize, usize, usize), StoreError> {
        let stats = convert_litellm_limits(litellm_json).map_err(StoreError::Serialization)?;

        let cache_path = app_dir.join(LITELLM_LIMITS_CACHE_FILENAME);
        let text = serde_json::to_string_pretty(&stats.rows)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        if let Some(parent) = cache_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StoreError::Serialization(format!("create data dir: {e}")))?;
        }
        tokio::fs::write(&cache_path, text).await.map_err(|e| {
            StoreError::Serialization(format!("write {LITELLM_LIMITS_CACHE_FILENAME}: {e}"))
        })?;

        info!(
            path = %cache_path.display(),
            source_models = stats.source_models,
            rows = stats.rows.len(),
            skipped = stats.skipped,
            "LiteLLM model-limits cache written"
        );

        self.reload(app_dir).await?;
        let total = self.snapshot.all().await.len();
        Ok((total, stats.source_models, stats.skipped))
    }

    pub async fn get(&self, provider_kind: &str, model_id: &str) -> Option<ModelLimitsRow> {
        self.snapshot.get(provider_kind, model_id).await
    }

    pub async fn all(&self) -> Vec<ModelLimitsRow> {
        self.snapshot.all().await
    }

    async fn load_merged(app_dir: &Path) -> Vec<ModelLimitsRow> {
        let mut map: BTreeMap<(String, String), ModelLimitsRow> = BTreeMap::new();
        Self::merge_file(&mut map, &app_dir.join(LITELLM_LIMITS_CACHE_FILENAME)).await;
        Self::merge_file(&mut map, &app_dir.join(LIMITS_OVERRIDE_FILENAME)).await;
        map.into_values().collect()
    }

    async fn merge_file(
        map: &mut BTreeMap<(String, String), ModelLimitsRow>,
        path: &Path,
    ) {
        if !path.exists() {
            return;
        }
        let Ok(content) = tokio::fs::read_to_string(path).await else {
            return;
        };
        let Ok(rows) = serde_json::from_str::<Vec<ModelLimitsRow>>(&content) else {
            return;
        };
        for row in rows {
            if row.max_input_tokens == 0 {
                continue;
            }
            map.insert(
                (row.provider_kind.clone(), row.model_id.clone()),
                row,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: &str, model: &str, input: u64) -> ModelLimitsRow {
        ModelLimitsRow {
            provider_kind: kind.into(),
            model_id: model.into(),
            max_input_tokens: input,
            max_output_tokens: Some(64_000),
        }
    }

    #[test]
    fn lookup_exact_and_kind_alias() {
        let rows = vec![row("xai", "grok-4.5", 500_000)];
        let got = lookup_limits_rows(&rows, "grok-oauth", "grok-4.5").unwrap();
        assert_eq!(got.max_input_tokens, 500_000);
    }

    #[test]
    fn lookup_model_prefix() {
        let rows = vec![row("xai", "grok-4.5", 500_000)];
        let got = lookup_limits_rows(&rows, "xai", "grok-4.5-build-nightly").unwrap();
        assert_eq!(got.max_input_tokens, 500_000);
    }

    #[test]
    fn lookup_missing_is_none() {
        let rows = vec![row("xai", "grok-4.5", 500_000)];
        assert!(lookup_limits_rows(&rows, "openai", "gpt-4o").is_none());
    }
}
