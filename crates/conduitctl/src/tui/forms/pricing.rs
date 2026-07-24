//! Pricing override form.

use super::super::input::InputField;

#[derive(Debug, Clone)]
pub struct PricingOverrideForm {
    pub fields: Vec<InputField>,
    pub focus: usize,
    pub error: Option<String>,
    /// When editing, original keys (for delete-before-rename if we ever rename).
    pub editing: bool,
}

impl PricingOverrideForm {
    /// Field order: provider_kind, model_id, input_per_mtok, output_per_mtok,
    /// cache_read (optional), cache_write (optional).
    pub fn create() -> Self {
        Self {
            fields: vec![
                InputField::new("openai"),
                InputField::new(""),
                InputField::new("1.0"),
                InputField::new("4.0"),
                InputField::new(""),
                InputField::new(""),
                InputField::new(""), // reasoning optional
            ],
            focus: 0,
            error: None,
            editing: false,
        }
    }

    pub fn edit(p: &crate::dto::PricingView) -> Self {
        let mut f = Self::from_row(p);
        f.editing = true;
        f
    }

    /// Prefill from a merged-table (or override) row to quickly create an override.
    /// Focus lands on input price so the operator can tweak and save.
    pub fn from_row(p: &crate::dto::PricingView) -> Self {
        Self {
            fields: vec![
                InputField::new(&p.provider_kind),
                InputField::new(&p.model_id),
                InputField::new(format_rate(p.input_per_mtok)),
                InputField::new(format_rate(p.output_per_mtok)),
                InputField::new(
                    p.cache_read_per_mtok
                        .map(format_rate)
                        .unwrap_or_default(),
                ),
                InputField::new(
                    p.cache_write_per_mtok
                        .map(format_rate)
                        .unwrap_or_default(),
                ),
                InputField::new(
                    p.reasoning_per_mtok
                        .map(format_rate)
                        .unwrap_or_default(),
                ),
            ],
            focus: 2, // Input $/MTok
            error: None,
            editing: false,
        }
    }

    pub fn labels() -> [&'static str; 7] {
        [
            "Provider kind",
            "Model id",
            "Input $/MTok",
            "Output $/MTok",
            "Cache read $/MTok (optional)",
            "Cache write $/MTok (optional)",
            "Reasoning $/MTok (optional)",
        ]
    }

    pub fn to_body(&self) -> Result<crate::dto::UpsertPricingOverrideBody, String> {
        let provider_kind = self.fields[0].value.trim();
        let model_id = self.fields[1].value.trim();
        if provider_kind.is_empty() || model_id.is_empty() {
            return Err("provider_kind and model_id are required".into());
        }
        let input_per_mtok = parse_nonneg_finite(&self.fields[2].value, "input_per_mtok")?;
        let output_per_mtok = parse_nonneg_finite(&self.fields[3].value, "output_per_mtok")?;
        if input_per_mtok == 0.0 && output_per_mtok == 0.0 {
            return Err("at least one of input/output must be positive".into());
        }
        Ok(crate::dto::UpsertPricingOverrideBody {
            provider_kind: provider_kind.to_string(),
            model_id: model_id.to_string(),
            input_per_mtok,
            output_per_mtok,
            cache_read_per_mtok: parse_opt_nonneg_finite(&self.fields[4].value)?,
            cache_write_per_mtok: parse_opt_nonneg_finite(&self.fields[5].value)?,
            reasoning_per_mtok: self
                .fields
                .get(6)
                .map(|f| parse_opt_nonneg_finite(&f.value))
                .transpose()?
                .flatten(),
            effective_from: None,
        })
    }
}

/// Parse a required non-negative finite f64 field (shared by pricing form).
pub(crate) fn parse_nonneg_finite(s: &str, name: &str) -> Result<f64, String> {
    let v: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("invalid {name}"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!("{name} must be finite and non-negative"));
    }
    Ok(v)
}

/// Optional variant: empty → `None`, otherwise a non-negative finite f64.
fn parse_opt_nonneg_finite(s: &str) -> Result<Option<f64>, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let v: f64 = t.parse().map_err(|_| format!("invalid number: {t}"))?;
    if !v.is_finite() || v < 0.0 {
        return Err(format!("invalid number: {t}"));
    }
    Ok(Some(v))
}

pub(crate) fn format_rate(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    // Keep enough precision for sub-cent MTok rates without noisy trailing zeros.
    // Sub-micro rates would round to 0 at 6 decimals, silently zeroing a tiny
    // rate when its override row is edited — widen precision for those.
    let decimals = if v.abs() < 1e-6 { 12 } else { 6 };
    let s = format!("{v:.decimals$}");
    let t = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if t.is_empty() {
        "0".to_string()
    } else {
        t
    }
}


