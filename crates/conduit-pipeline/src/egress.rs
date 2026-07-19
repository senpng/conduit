//! L6 Egress Filter: cost calculation and usage ledger.

use conduit_ir::{canonical::Usage, pricing::pricing_kind_aliases};

/// Compute USD cost for a completed request using the pricing table.
pub fn compute_cost(
    provider_kind: &str,
    model_id: &str,
    usage: &Usage,
    pricing_fn: impl Fn(&str, &str) -> Option<ModelPricing>,
) -> f64 {
    let price = pricing_fn(provider_kind, model_id).or_else(|| {
        for alt in pricing_kind_aliases(provider_kind).iter().copied() {
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

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: Option<f64>,
    pub cache_write_per_mtok: Option<f64>,
    pub reasoning_per_mtok: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use conduit_ir::canonical::Usage;

    #[test]
    fn compute_cost_uses_pricing_fn() {
        let usage = Usage {
            prompt_tokens: 1_000_000,
            completion_tokens: 500_000,
            total_tokens: 1_500_000,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let cost = compute_cost("openai", "gpt-4o", &usage, |_, _| {
            Some(ModelPricing {
                input_per_mtok: 1.0,
                output_per_mtok: 2.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                reasoning_per_mtok: None,
            })
        });
        assert!((cost - 2.0).abs() < 1e-12);
    }
}
