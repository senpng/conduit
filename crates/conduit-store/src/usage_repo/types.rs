//! Usage list options and summary row types.

use crate::schema::UsageRecordRow;

/// Sort key for paginated usage list (always descending).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageListSort {
    #[default]
    Date,
    Cost,
    Tokens,
}

impl UsageListSort {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "cost" => Self::Cost,
            "tokens" | "token" => Self::Tokens,
            _ => Self::Date,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Cost => "cost",
            Self::Tokens => "tokens",
        }
    }
}

/// Options for [`UsageRepo::list_page`].
#[derive(Debug, Clone, Copy)]
pub struct UsageListOpts<'a> {
    pub limit: usize,
    pub offset: usize,
    pub key_id: Option<&'a str>,
    pub period: Option<&'a str>,
    /// Case-insensitive substring across model / alias / provider / request / key.
    pub q: Option<&'a str>,
    pub sort: UsageListSort,
    /// Client timezone offset minutes east of UTC (0 = UTC calendar).
    pub tz_offset_minutes: i32,
}

/// One page of usage rows plus total matching count.
#[derive(Debug, Clone)]
pub struct UsageListPage {
    pub rows: Vec<UsageRecordRow>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
}

/// Period rollup used by console summary.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageSummaryRow {
    pub downstream_key_id: String,
    pub request_count: u64,
    pub total_usd: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// One local calendar day within a period summary (`YYYY-MM-DD`).
#[derive(Debug, Clone, PartialEq)]
pub struct UsageDayRow {
    /// `YYYY-MM-DD` (UTC, from `ts` prefix).
    pub day: String,
    pub request_count: u64,
    pub total_usd: f64,
    pub total_tokens: u64,
}

/// One model/alias within a period summary.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageModelRow {
    pub label: String,
    pub provider_kind: Option<String>,
    pub request_count: u64,
    pub total_usd: f64,
    pub total_tokens: u64,
    /// Throughput = Σcompletion_tokens / Σgeneration_ms, size-weighted — NOT a
    /// row-wise mean. `None` when no eligible row exists.
    pub tokens_per_sec: Option<f64>,
}

/// Model rollup for one downstream key within a period.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageKeyModelRow {
    pub downstream_key_id: String,
    pub label: String,
    pub provider_kind: Option<String>,
    pub request_count: u64,
    pub total_usd: f64,
    pub total_tokens: u64,
}

/// Model rollup for one UTC day within a period.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageDayModelRow {
    pub day: String,
    pub label: String,
    pub provider_kind: Option<String>,
    pub request_count: u64,
    pub total_usd: f64,
    pub total_tokens: u64,
}

/// Period outcome aggregates for success rate / latency cards.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageOutcomeSummary {
    pub request_count: u64,
    pub success_count: u64,
    pub success_rate: f64,
    pub avg_ttfb_ms: Option<f64>,
    pub avg_duration_ms: Option<f64>,
    /// Throughput = Σcompletion_tokens / Σgeneration_ms, size-weighted — NOT a
    /// row-wise mean like `avg_ttfb_ms`. `None` when no eligible row exists.
    pub tokens_per_sec: Option<f64>,
}

/// Provider health rollup within a period.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageProviderRow {
    pub provider_id: String,
    pub provider_kind: Option<String>,
    pub request_count: u64,
    pub success_count: u64,
    pub success_rate: f64,
    pub avg_ttfb_ms: Option<f64>,
    pub avg_duration_ms: Option<f64>,
    /// Throughput = Σcompletion_tokens / Σgeneration_ms, size-weighted — NOT a
    /// row-wise mean like `avg_ttfb_ms`. `None` when no eligible row exists.
    pub tokens_per_sec: Option<f64>,
    pub total_usd: f64,
    pub total_tokens: u64,
}

