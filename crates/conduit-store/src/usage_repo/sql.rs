//! Shared SQL fragments and timezone helpers for the usage ledger.

/// Clamp client-provided offset to a sane range (minutes east of UTC).
pub fn clamp_tz_offset_minutes(minutes: i32) -> i32 {
    minutes.clamp(-14 * 60, 14 * 60)
}

/// SQLite `date(..., ?)` modifier, e.g. `+480 minutes` / `-300 minutes`.
pub(crate) fn offset_modifier(tz_offset_minutes: i32) -> String {
    format!("{:+} minutes", clamp_tz_offset_minutes(tz_offset_minutes))
}

/// SQL fragment: stored UTC `ts` → local calendar day, shifted by `?1` (the
/// offset modifier). Expands to
/// `date(replace(substr(ts, 1, 19), 'T', ' '), ?1)` — `ts` comes from
/// `Utc::now().to_rfc3339()`, whose first 19 chars are `YYYY-MM-DDTHH:MM:SS`.
/// Reused across every day-bucketed aggregate so the shift math lives in
/// exactly one place.
pub(crate) const LOCAL_DAY: &str = "date(replace(substr(ts, 1, 19), 'T', ' '), ?1)";

/// SQL fragment: size-weighted generation throughput
/// (`Σcompletion_tokens / Σ(duration_ms − ttfb_ms)`, ms → s), aliased
/// `tokens_per_sec`. `NULL` when no eligible row exists (division guarded by
/// `NULLIF(..., 0)`). The numerator/denominator filters must stay identical so
/// rows without `duration_ms` never leak into the numerator.
pub(crate) const TOKENS_PER_SEC: &str = r#"SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
             THEN completion_tokens ELSE 0 END) * 1000.0
    / NULLIF(SUM(CASE WHEN duration_ms IS NOT NULL AND completion_tokens > 0
        -- COALESCE(ttfb_ms,0) is required: SQLite's scalar MAX()
        -- returns NULL if any argument is NULL, unlike agg MAX().
        THEN MAX(duration_ms - COALESCE(ttfb_ms, 0), 0)
        ELSE NULL END), 0) AS tokens_per_sec"#;

/// Shared `WHERE` for period + key aggregates, using the same `IS NULL OR`
/// null-skip pattern as [`UsageRepo::list_page`]:
/// - `?1` — offset modifier (used by [`LOCAL_DAY`])
/// - `?2` — `YYYY-MM%` day pattern, or `NULL` for all-time
/// - `?3` — downstream key id, or `NULL` for all keys
///
/// Bind `(off, period_pat, key_id)` in that order after any leading params.
pub(crate) const PERIOD_KEY_WHERE: &str = "WHERE (?2 IS NULL OR date(replace(substr(ts, 1, 19), 'T', ' '), ?1) LIKE ?2)\n       AND (?3 IS NULL OR downstream_key_id = ?3)";

/// `YYYY-MM%` for local calendar months; `None` for all-time (`period == "all"`).
pub(crate) fn period_day_like(period: &str) -> Option<String> {
    if period.eq_ignore_ascii_case("all") {
        None
    } else {
        // Local day is `YYYY-MM-DD`; prefix match scopes one calendar month.
        Some(format!("{}%", period.trim()))
    }
}

