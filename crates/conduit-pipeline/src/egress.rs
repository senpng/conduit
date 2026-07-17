//! L6 Egress Filter: post-response finalization — compute cost, emit final trace event.

use conduit_ir::{canonical::Usage, trace::TraceEventKind};

use super::context::PipelineContext;

/// Compute USD cost for a completed request using the pricing table.
/// Returns 0.0 if pricing is unknown (logs a warning).
///
/// Lookup tries the raw `provider_kind` first, then common OAuth/kind aliases
/// (the PricingRepo also has its own fallback chain).
pub fn compute_cost(
    provider_kind: &str,
    model_id: &str,
    usage: &Usage,
    pricing_fn: impl Fn(&str, &str) -> Option<ModelPricing>,
) -> f64 {
    let price = pricing_fn(provider_kind, model_id).or_else(|| {
        // Secondary aliases when the injected fn only does exact match.
        for alt in pricing_kind_fallbacks(provider_kind) {
            if let Some(p) = pricing_fn(alt, model_id) {
                return Some(p);
            }
        }
        None
    });

    match price {
        Some(p) => {
            let input = (usage.prompt_tokens as f64 / 1_000_000.0) * p.input_per_mtok;
            let output = (usage.completion_tokens as f64 / 1_000_000.0) * p.output_per_mtok;
            let cache_read =
                usage.cache_read_tokens as f64 / 1_000_000.0 * p.cache_read_per_mtok.unwrap_or(0.0);
            let cache_write = usage.cache_write_tokens as f64 / 1_000_000.0
                * p.cache_write_per_mtok.unwrap_or(0.0);
            let reasoning = usage.reasoning_tokens as f64 / 1_000_000.0
                * p.reasoning_per_mtok.unwrap_or(p.output_per_mtok);
            input + output + cache_read + cache_write + reasoning
        }
        None => {
            tracing::warn!(provider = %provider_kind, model = %model_id, "no pricing entry; recording cost as 0");
            0.0
        }
    }
}

fn pricing_kind_fallbacks(kind: &str) -> &'static [&'static str] {
    match kind.trim().to_ascii_lowercase().as_str() {
        "grok-oauth" | "grok" | "xai-oauth" => &["xai", "openai"],
        "claude-oauth" | "anthropic-oauth" => &["anthropic"],
        "codex-oauth" | "codex" => &["openai", "codex"],
        _ => &[],
    }
}

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
    pub reasoning_per_mtok: Option<f64>,
}

/// Finalize the pipeline context and emit the FinalUsage trace event.
pub fn finalize(ctx: &mut PipelineContext, cost_usd: f64) {
    ctx.push_event(TraceEventKind::FinalUsage {
        usage: ctx.usage.clone(),
        cost_usd,
        loss_report: ctx.loss_report.clone(),
        downstream_key_id: ctx.downstream_key_id.clone(),
    });
}
