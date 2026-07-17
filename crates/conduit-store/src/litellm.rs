//! Convert LiteLLM's `model_prices_and_context_window.json` into Conduit pricing rows.
//!
//! LiteLLM is the de-facto open pricing map for LLM gateways. Costs are stored
//! as USD **per token**; Conduit stores USD **per million tokens**.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;

use crate::schema::PricingRow;

/// Canonical LiteLLM pricing map URL (raw GitHub main).
pub const DEFAULT_LITELLM_PRICING_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// Filename under the data dir for the last successful LiteLLM sync.
pub const LITELLM_CACHE_FILENAME: &str = "pricing.litellm.json";

/// Modes we map into Conduit token pricing (text completions).
const ACCEPTED_MODES: &[&str] = &["chat", "completion"];

#[derive(Debug, Clone, Deserialize)]
struct LiteLlmEntry {
    #[serde(default)]
    litellm_provider: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    input_cost_per_token: Option<f64>,
    #[serde(default)]
    output_cost_per_token: Option<f64>,
    #[serde(default)]
    cache_read_input_token_cost: Option<f64>,
    #[serde(default)]
    cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    output_cost_per_reasoning_token: Option<f64>,
}

/// Result of a LiteLLM → Conduit conversion.
#[derive(Debug, Clone)]
pub struct LiteLlmConvertStats {
    pub rows: Vec<PricingRow>,
    /// Entries considered (chat/completion with token costs).
    pub source_models: usize,
    /// Entries skipped (sample_spec, image, missing costs, …).
    pub skipped: usize,
}

/// Parse a LiteLLM cost-map JSON blob into Conduit [`PricingRow`]s.
///
/// - Keeps only `chat` / `completion` modes with at least one token cost field.
/// - Converts per-token USD → per-million-token USD (`* 1_000_000`).
/// - Maps `litellm_provider` into Conduit `provider_kind` and emits OAuth aliases
///   (`claude-oauth`, `grok-oauth`, `codex-oauth`, …) so subscription routes resolve.
/// - `effective_from` is `sync_date` (ISO date, e.g. `2026-07-17`) for all rows.
pub fn convert_litellm_json(json_text: &str, sync_date: &str) -> Result<LiteLlmConvertStats, String> {
    let root: BTreeMap<String, Value> =
        serde_json::from_str(json_text).map_err(|e| format!("invalid LiteLLM JSON: {e}"))?;

    let mut by_key: BTreeMap<(String, String), PricingRow> = BTreeMap::new();
    let mut source_models = 0usize;
    let mut skipped = 0usize;

    for (key, value) in root {
        if key == "sample_spec" {
            skipped += 1;
            continue;
        }
        let entry: LiteLlmEntry = match serde_json::from_value(value) {
            Ok(e) => e,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let mode = entry.mode.as_deref().unwrap_or("");
        if !mode.is_empty() && !ACCEPTED_MODES.contains(&mode) {
            skipped += 1;
            continue;
        }

        let has_input = entry.input_cost_per_token.is_some();
        let has_output = entry.output_cost_per_token.is_some();
        if !has_input && !has_output {
            skipped += 1;
            continue;
        }

        // Skip pure zero-priced stubs (placeholders).
        let input = entry.input_cost_per_token.unwrap_or(0.0);
        let output = entry.output_cost_per_token.unwrap_or(0.0);
        if input == 0.0 && output == 0.0 {
            skipped += 1;
            continue;
        }

        let litellm_provider = entry
            .litellm_provider
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if litellm_provider.is_empty() {
            skipped += 1;
            continue;
        }

        let provider_kind = map_provider_kind(&litellm_provider);
        let model_id = normalize_model_id(&key, &litellm_provider, &provider_kind);
        if model_id.is_empty() {
            skipped += 1;
            continue;
        }

        source_models += 1;
        let row = PricingRow {
            provider_kind: provider_kind.clone(),
            model_id: model_id.clone(),
            input_per_mtok: per_token_to_mtok(input),
            output_per_mtok: per_token_to_mtok(output),
            cache_read_per_mtok: entry
                .cache_read_input_token_cost
                .map(per_token_to_mtok),
            cache_write_per_mtok: entry
                .cache_creation_input_token_cost
                .map(per_token_to_mtok),
            reasoning_per_mtok: entry
                .output_cost_per_reasoning_token
                .map(per_token_to_mtok),
            effective_from: sync_date.to_string(),
        };

        insert_with_aliases(&mut by_key, row);
    }

    let rows: Vec<PricingRow> = by_key.into_values().collect();
    Ok(LiteLlmConvertStats {
        rows,
        source_models,
        skipped,
    })
}

fn per_token_to_mtok(per_token: f64) -> f64 {
    per_token * 1_000_000.0
}

/// Map LiteLLM provider id → Conduit `provider_kind`.
fn map_provider_kind(litellm_provider: &str) -> String {
    match litellm_provider {
        "openai" => "openai".into(),
        "anthropic" => "anthropic".into(),
        "xai" => "xai".into(),
        "gemini" | "vertex_ai-language-models" | "vertex_ai" | "vertex_ai-vision-models" => {
            "google".into()
        }
        "azure" | "azure_ai" => "azure".into(),
        "bedrock" | "bedrock_converse" => "bedrock".into(),
        "groq" => "groq".into(),
        "mistral" | "mistral_ai" => "mistral".into(),
        "deepseek" => "deepseek".into(),
        "cohere" => "cohere".into(),
        "together_ai" => "together".into(),
        "fireworks_ai" => "fireworks".into(),
        "openrouter" => "openrouter".into(),
        "ollama" => "ollama".into(),
        other => other.to_string(),
    }
}

/// Extra Conduit kinds that should share the same rates as a primary kind.
fn oauth_aliases(primary_kind: &str) -> &'static [&'static str] {
    match primary_kind {
        "openai" => &["codex-oauth", "codex"],
        "anthropic" => &["claude-oauth"],
        "xai" => &["grok-oauth", "grok"],
        _ => &[],
    }
}

fn insert_with_aliases(
    map: &mut BTreeMap<(String, String), PricingRow>,
    row: PricingRow,
) {
    for alias in oauth_aliases(&row.provider_kind) {
        let mut a = row.clone();
        a.provider_kind = (*alias).to_string();
        map.insert((a.provider_kind.clone(), a.model_id.clone()), a);
    }
    map.insert((row.provider_kind.clone(), row.model_id.clone()), row);
}

/// Strip provider prefixes from LiteLLM map keys.
///
/// Examples:
/// - `gpt-4o` + openai → `gpt-4o`
/// - `xai/grok-4.5` + xai → `grok-4.5`
/// - `openrouter/anthropic/claude-3` → keep full after first slash if multi-segment
fn normalize_model_id(key: &str, litellm_provider: &str, provider_kind: &str) -> String {
    let key = key.trim();
    if key.is_empty() {
        return String::new();
    }

    // Exact "provider/model" for matching provider.
    for prefix in [
        format!("{litellm_provider}/"),
        format!("{provider_kind}/"),
    ] {
        if let Some(rest) = key.strip_prefix(&prefix) {
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }

    // xai/grok-… even if provider already mapped.
    if let Some(rest) = key.strip_prefix("xai/") {
        return rest.to_string();
    }

    key.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "sample_spec": { "mode": "chat", "input_cost_per_token": 0.0 },
      "gpt-4o": {
        "litellm_provider": "openai",
        "mode": "chat",
        "input_cost_per_token": 2.5e-6,
        "output_cost_per_token": 1e-5,
        "cache_read_input_token_cost": 1.25e-6
      },
      "claude-opus-4-5": {
        "litellm_provider": "anthropic",
        "mode": "chat",
        "input_cost_per_token": 5e-6,
        "output_cost_per_token": 2.5e-5,
        "cache_read_input_token_cost": 5e-7,
        "cache_creation_input_token_cost": 6.25e-6
      },
      "xai/grok-4.5": {
        "litellm_provider": "xai",
        "mode": "chat",
        "input_cost_per_token": 2e-6,
        "output_cost_per_token": 6e-6,
        "cache_read_input_token_cost": 5e-7
      },
      "dall-e-3": {
        "litellm_provider": "openai",
        "mode": "image_generation",
        "output_cost_per_image": 0.04
      },
      "zero-stub": {
        "litellm_provider": "openai",
        "mode": "chat",
        "input_cost_per_token": 0.0,
        "output_cost_per_token": 0.0
      }
    }"#;

    #[test]
    fn converts_token_costs_and_aliases() {
        let stats = convert_litellm_json(SAMPLE, "2026-07-17").unwrap();
        assert_eq!(stats.source_models, 3);

        let gpt = stats
            .rows
            .iter()
            .find(|r| r.provider_kind == "openai" && r.model_id == "gpt-4o")
            .unwrap();
        assert!((gpt.input_per_mtok - 2.5).abs() < 1e-9);
        assert!((gpt.output_per_mtok - 10.0).abs() < 1e-9);
        assert!((gpt.cache_read_per_mtok.unwrap() - 1.25).abs() < 1e-9);
        assert_eq!(gpt.effective_from, "2026-07-17");

        // OAuth aliases for openai / anthropic / xai
        assert!(stats
            .rows
            .iter()
            .any(|r| r.provider_kind == "codex-oauth" && r.model_id == "gpt-4o"));
        assert!(stats
            .rows
            .iter()
            .any(|r| r.provider_kind == "claude-oauth" && r.model_id == "claude-opus-4-5"));
        assert!(stats
            .rows
            .iter()
            .any(|r| r.provider_kind == "grok-oauth" && r.model_id == "grok-4.5"));
        assert!(stats
            .rows
            .iter()
            .any(|r| r.provider_kind == "xai" && r.model_id == "grok-4.5"));

        // image / zero stubs skipped
        assert!(!stats.rows.iter().any(|r| r.model_id == "dall-e-3"));
        assert!(!stats.rows.iter().any(|r| r.model_id == "zero-stub"));
    }

    #[test]
    fn anthropic_cache_write_mapped() {
        let stats = convert_litellm_json(SAMPLE, "2026-07-17").unwrap();
        let claude = stats
            .rows
            .iter()
            .find(|r| r.provider_kind == "anthropic" && r.model_id == "claude-opus-4-5")
            .unwrap();
        assert!((claude.cache_write_per_mtok.unwrap() - 6.25).abs() < 1e-9);
        assert!((claude.input_per_mtok - 5.0).abs() < 1e-9);
        assert!((claude.output_per_mtok - 25.0).abs() < 1e-9);
    }
}
