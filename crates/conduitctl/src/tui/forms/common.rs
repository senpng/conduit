//! Shared confirm dialogs and usage period helpers.

use chrono::{Datelike, Local};

#[derive(Debug, Clone)]
pub enum ConfirmAction {
    DeleteProvider { id: String, name: String },
    DeleteRoute { id: String, alias: String },
    DeleteKey { id: String, name: String },
    SetProviderSecret { id: String, name: String },
    DeletePricingOverride {
        provider_kind: String,
        model_id: String,
    },
}

pub fn current_period() -> String {
    // Client-local calendar month (matches usage rollups with tz_offset_minutes).
    let now = Local::now();
    format!("{:04}-{:02}", now.year(), now.month())
}

pub fn shift_period(period: &str, delta: i32) -> String {
    // Reuse the strict YYYY-MM parser so a malformed month can't drive the
    // arithmetic into a pathological range (the old `while` normalization could
    // loop enormously on a giant month value). Fall back to the current month.
    let Some((y, m)) = super::super::widgets::parse_year_month(period) else {
        return current_period();
    };
    // Absolute month index, shifted by delta, split back out via euclidean
    // div/rem — no loop, no overflow (i64 dwarfs any valid year).
    let idx = (y as i64) * 12 + (m as i64 - 1) + delta as i64;
    let year = idx.div_euclid(12);
    let month = idx.rem_euclid(12) + 1;
    format!("{year:04}-{month:02}")
}

