//! Reusable visual primitives for a denser, product-grade TUI.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use super::theme::Theme;

pub fn fill_bg(frame: &mut Frame, area: Rect, theme: &Theme) {
    // Clear symbols first — Block::style only patches colors and can leave
    // leftover glyphs from a previous frame when the backend diffs poorly.
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.background)),
        area,
    );
}

pub fn panel_block<'a>(theme: &Theme, title: impl Into<String>, focused: bool) -> Block<'a> {
    panel_block_borders(theme, title, focused, Borders::ALL)
}

/// Like [`panel_block`] but with selectable borders — use to join side-by-side
/// panels without a double vertical line (left: ALL, right: TOP|RIGHT|BOTTOM).
pub fn panel_block_borders<'a>(
    theme: &Theme,
    title: impl Into<String>,
    focused: bool,
    borders: Borders,
) -> Block<'a> {
    let title = title.into();
    Block::default()
        .borders(borders)
        .border_style(if focused {
            theme.border_active()
        } else {
            theme.border()
        })
        .title_top(Span::styled(format!(" {title} "), theme.title()))
        .style(theme.surface())
}

/// Equal-width metric cards across `area`.
///
/// Layout (recommended `Constraint::Length(3)`):
/// ```text
/// ┌ HEALTH ──┐
/// │    ok    │
/// └──────────┘
/// ```
/// Titles are forced to the **top** border via `title_top`. The inner area is
/// cleared + surface-filled so under-drawn panels cannot ghost through empty
/// rows when the strip is taller than 3.
pub fn metric_strip(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    items: &[(&str, String, Style)],
) {
    if items.is_empty() || area.width < 8 {
        return;
    }
    let constraints: Vec<Constraint> = items
        .iter()
        .map(|_| Constraint::Ratio(1, items.len() as u32))
        .collect();
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);
    for (i, (label, value, style)) in items.iter().enumerate() {
        let cell = cols[i];
        // Reset every cell so nothing from a lower panel / prior frame bleeds in.
        frame.render_widget(Clear, cell);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border())
            // Explicit top — never inherit a bottom title_position.
            .title_top(Span::styled(format!(" {label} "), theme.muted()))
            .style(theme.surface());
        let inner = block.inner(cell);
        frame.render_widget(block, cell);

        // Paint the full inner rectangle (Block only styles, does not blank
        // symbols on rows the Paragraph does not touch).
        frame.render_widget(Block::default().style(theme.surface()), inner);

        // Vertically center a single-line value when the card is taller than 3.
        let value_area = if inner.height > 1 {
            Rect {
                x: inner.x,
                y: inner.y + (inner.height.saturating_sub(1)) / 2,
                width: inner.width,
                height: 1,
            }
        } else {
            inner
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                value.clone(),
                style.add_modifier(Modifier::BOLD),
            )))
            .alignment(Alignment::Center),
            value_area,
        );
    }
}

pub fn ratio_bar(ratio: f64, width: usize) -> String {
    let width = width.max(4);
    let r = ratio.clamp(0.0, 1.0);
    let filled = ((r * width as f64).round() as usize).min(width);
    let empty = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

// ── Daily spend calendar (GitHub contribution-style) ────────────────────────

/// One calendar day of gateway spend for heatmap cells.
#[derive(Debug, Clone)]
pub struct DaySpend {
    /// `YYYY-MM-DD`
    pub date: String,
    pub request_count: u64,
    pub total_tokens: u64,
    pub total_usd: f64,
}

/// Parse `YYYY-MM` → (year, month).
pub fn parse_year_month(period: &str) -> Option<(i32, u32)> {
    let mut parts = period.trim().split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    if (1..=12).contains(&m) && (1970..=2100).contains(&y) {
        Some((y, m))
    } else {
        None
    }
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
            if leap {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Heat bucket 0..=4 from intensity 0..=1 (tokscale / GitHub thresholds).
pub fn heat_level(value: f64, max: f64) -> u8 {
    if value <= 0.0 || !value.is_finite() || max <= 0.0 {
        return 0;
    }
    let r = (value / max).clamp(0.0, 1.0);
    // Match tokscale `intensity_color` bands.
    if r < 0.25 {
        1
    } else if r < 0.50 {
        2
    } else if r < 0.75 {
        3
    } else {
        4
    }
}

/// One cell in a tokscale-style contribution column (Sun..=Sat).
#[derive(Debug, Clone)]
pub struct ContributionCell {
    pub date: String,
    pub request_count: u64,
    pub total_usd: f64,
    /// 0.0..=1.0 relative to peak metric in the window (cost or tokens).
    pub intensity: f64,
}

/// Build ~52 weeks of contribution columns ending on `today_ymd` (`YYYY-MM-DD`).
///
/// Mirrors tokscale `build_contribution_graph_for_today`: start on the Sunday
/// on or before (today − 364 days), fill every day through today (zeros when
/// missing). Intensity is relative to peak **tokens** when `heat_by_tokens`,
/// otherwise peak **cost**.
pub fn build_contribution_weeks(
    by_day: &[(String, u64, u64, f64)],
    today_ymd: &str,
    heat_by_tokens: bool,
) -> Vec<[Option<ContributionCell>; 7]> {
    let Some(today) = parse_ymd(today_ymd) else {
        return Vec::new();
    };
    let (ty, tm, td) = today;
    // Weekday of today (0=Sun).
    let today_wd = weekday_sun0(ty, tm, td) as i64;
    // tokscale: start = today - (364 + days_to_sunday)
    let start_offset = 364 + today_wd;
    let Some((sy, sm, sd)) = add_days(ty, tm, td, -start_offset) else {
        return Vec::new();
    };

    let mut map = std::collections::HashMap::new();
    for (d, r, t, c) in by_day {
        map.insert(d.as_str(), (*r, *t, *c));
    }
    let max_metric = if heat_by_tokens {
        by_day
            .iter()
            .map(|(_, _, t, _)| *t as f64)
            .fold(0.0_f64, f64::max)
            .max(1e-12)
    } else {
        by_day
            .iter()
            .map(|(_, _, _, c)| *c)
            .fold(0.0_f64, f64::max)
            .max(1e-12)
    };

    let mut weeks: Vec<[Option<ContributionCell>; 7]> = Vec::new();
    let mut col = [const { None }; 7];
    let mut y = sy;
    let mut m = sm;
    let mut d = sd;
    let mut wd = weekday_sun0(y, m, d) as usize;

    loop {
        let date = format!("{y:04}-{m:02}-{d:02}");
        let (r, t, c) = map.get(date.as_str()).copied().unwrap_or((0, 0, 0.0));
        let metric = if heat_by_tokens { t as f64 } else { c };
        let intensity = if metric > 0.0 {
            (metric / max_metric).clamp(0.0, 1.0)
        } else {
            0.0
        };
        col[wd] = Some(ContributionCell {
            date: date.clone(),
            request_count: r,
            total_usd: c,
            intensity,
        });

        let is_end = y == ty && m == tm && d == td;
        if wd == 6 || is_end {
            weeks.push(col);
            col = [const { None }; 7];
            wd = 0;
        } else {
            wd += 1;
        }
        if is_end {
            break;
        }
        let Some(next) = add_days(y, m, d, 1) else {
            break;
        };
        y = next.0;
        m = next.1;
        d = next.2;
    }
    weeks
}

fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let mut p = s.trim().split('-');
    let y: i32 = p.next()?.parse().ok()?;
    let m: u32 = p.next()?.parse().ok()?;
    let d: u32 = p.next()?.parse().ok()?;
    if (1..=12).contains(&m) && (1..=31).contains(&d) {
        Some((y, m, d))
    } else {
        None
    }
}

fn add_days(y: i32, m: u32, d: u32, delta: i64) -> Option<(i32, u32, u32)> {
    // Civil day arithmetic via Rata Die (Howard Hinnant).
    let day_number = date_to_rd(y, m, d)? + delta;
    rd_to_date(day_number)
}

fn date_to_rd(y: i32, m: u32, d: u32) -> Option<i64> {
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let y = y as i64;
    let m = m as i64;
    let d = d as i64;
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

fn rd_to_date(mut z: i64) -> Option<(i32, u32, u32)> {
    z += 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    Some((y as i32, m as u32, d as u32))
}

/// Weekday index for heatmap rows: **Sunday = 0** … Saturday = 6 (GitHub layout).
pub fn weekday_sun0(year: i32, month: u32, day: u32) -> u32 {
    // Sakamoto's methods — no chrono dependency needed here.
    let mut y = year;
    let m = month;
    let d = day;
    static T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    if m < 3 {
        y -= 1;
    }
    let w = (y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as i32) % 7;
    // Sakamoto: 0=Sunday … 6=Saturday — already what we want.
    w.rem_euclid(7) as u32
}

/// Full calendar for `period` (`YYYY-MM`), zero-filled days with no traffic.
///
/// `by_day` rows are keyed by `YYYY-MM-DD` (UTC day from the usage ledger).
pub fn month_day_spends(
    period: &str,
    by_day: &[(String, u64, u64, f64)],
) -> Vec<DaySpend> {
    let Some((y, m)) = parse_year_month(period) else {
        return by_day
            .iter()
            .map(|(d, r, t, c)| DaySpend {
                date: d.clone(),
                request_count: *r,
                total_tokens: *t,
                total_usd: *c,
            })
            .collect();
    };
    let n = days_in_month(y, m);
    let mut map = std::collections::HashMap::new();
    for (d, r, t, c) in by_day {
        map.insert(d.as_str(), (*r, *t, *c));
    }
    (1..=n)
        .map(|dom| {
            let date = format!("{y:04}-{m:02}-{dom:02}");
            let (r, t, c) = map.get(date.as_str()).copied().unwrap_or((0, 0, 0.0));
            DaySpend {
                date,
                request_count: r,
                total_tokens: t,
                total_usd: c,
            }
        })
        .collect()
}

pub fn empty_state(frame: &mut Frame, area: Rect, theme: &Theme, title: &str, hint: &str) {
    let block = panel_block(theme, title, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = vec![
        Line::from(""),
        Line::from(Span::styled("  nothing here yet", theme.muted())),
        Line::from(""),
        Line::from(Span::styled(format!("  {hint}"), theme.subtle())),
    ];
    frame.render_widget(Paragraph::new(text), inner);
}

pub fn spinner(frame: u64) -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(frame as usize) % FRAMES.len()]
}

pub fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area)[1];
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup)[1]
}

/// Dim the whole frame then draw a bordered modal.
pub fn modal(
    frame: &mut Frame,
    theme: &Theme,
    title: &str,
    body: Vec<Line>,
    border_style: Style,
) {
    // Soft dim: full-area clear with surface_alt tint is expensive; use a dark block.
    let area = centered(frame.area(), 72, 58);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title_top(Span::styled(format!(" {title} "), theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: false }).style(theme.surface()),
        inner,
    );
}

pub fn keybind_line(theme: &Theme, pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ·  ", theme.subtle()));
        }
        spans.push(Span::styled(format!(" {key} "), theme.key_hint().bg(theme.surface_alt)));
        spans.push(Span::styled(format!(" {label}"), theme.muted()));
    }
    Line::from(spans)
}

/// Display columns a string occupies in a terminal cell grid (CJK/emoji = 2,
/// control chars = 0). Use this instead of `chars().count()` whenever the
/// result is compared against a column budget.
pub fn display_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Truncate `s` to at most `max` **display columns**, appending `…` when cut.
/// Wide characters (CJK, emoji) count as 2 columns, so the result never
/// overflows the budget even by a single cell.
pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 1 {
        return "…".into();
    }
    // Reserve one column for the ellipsis; stop before a wide char would spill.
    let budget = max - 1;
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Truncate to `width` display columns, then pad with spaces to exactly fill
/// `width` columns. Column-accurate replacement for `format!("{:<width$}", …)`,
/// which mis-measures CJK/emoji because it pads by `char` count.
pub fn pad_display(s: &str, width: usize) -> String {
    let t = truncate(s, width);
    let pad = width.saturating_sub(display_width(&t));
    if pad == 0 {
        t
    } else {
        let mut out = t;
        out.extend(std::iter::repeat(' ').take(pad));
        out
    }
}

pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a throughput value (`UsageOutcomeSummary::tokens_per_sec` /
/// `UsageProviderRow::tokens_per_sec`) as e.g. `"42.3 tok/s"` or `"1.2k tok/s"`.
pub fn format_tok_per_sec(v: Option<f64>) -> String {
    match v {
        None => "—".into(),
        Some(v) if v >= 1000.0 => format!("{} tok/s", format_tokens(v.round() as u64)),
        Some(v) => format!("{v:.1} tok/s"),
    }
}

pub fn format_usd(v: f64) -> String {
    // Collapse signed zero / float dust so we never render "$-0.0000".
    // (`format!("{:.4}", -0.0)` keeps the sign bit even though -0.0 == 0.0.)
    let v = if !v.is_finite() || v.abs() < 1e-9 {
        0.0
    } else {
        v
    };
    if v >= 100.0 {
        format!("${v:.2}")
    } else if v >= 1.0 {
        format!("${v:.3}")
    } else {
        format!("${v:.4}")
    }
}

/// Format a timestamp for display in the **client's local timezone**.
///
/// Accepts RFC3339 / ISO-8601, unix epoch seconds (string), or common
/// `YYYY-MM-DD HH:MM:SS` forms. Unparseable input is returned trimmed.
pub fn format_local_time(raw: &str) -> String {
    format_local_time_with(raw, "%Y-%m-%d %H:%M:%S")
}

/// Compact local time for narrow list columns (`MM-DD HH:MM`).
pub fn format_local_time_short(raw: &str) -> String {
    format_local_time_with(raw, "%m-%d %H:%M")
}

fn format_local_time_with(raw: &str, fmt: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return "—".into();
    }
    if let Some(local) = parse_to_local(s) {
        return local.format(fmt).to_string();
    }
    // Date-only calendar labels (usage by-day) — keep as-is.
    if s.len() == 10 && s.as_bytes().get(4) == Some(&b'-') && s.as_bytes().get(7) == Some(&b'-') {
        return s.to_string();
    }
    s.to_string()
}

fn parse_to_local(s: &str) -> Option<DateTime<Local>> {
    // RFC3339 / ISO-8601 with offset or Z
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Local));
    }
    // Trailing Z sometimes paired with space separator
    let normalized = s.replace(' ', "T");
    if let Ok(dt) = DateTime::parse_from_rfc3339(&normalized) {
        return Some(dt.with_timezone(&Local));
    }
    // ISO without timezone → assume UTC (server storage convention)
    const NAIVE_FMTS: &[&str] = &[
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
    ];
    for f in NAIVE_FMTS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(s, f)
            .or_else(|_| NaiveDateTime::parse_from_str(&normalized, f))
        {
            return Some(Utc.from_utc_datetime(&naive).with_timezone(&Local));
        }
    }
    // Unix seconds (integer string) — used by some snapshot `captured_at` fields
    if let Ok(secs) = s.parse::<i64>() {
        if (1_000_000_000..4_000_000_000).contains(&secs) {
            return Utc
                .timestamp_opt(secs, 0)
                .single()
                .map(|dt| dt.with_timezone(&Local));
        }
        // Milliseconds
        if (1_000_000_000_000..4_000_000_000_000).contains(&secs) {
            return Utc
                .timestamp_millis_opt(secs)
                .single()
                .map(|dt| dt.with_timezone(&Local));
        }
    }
    None
}

#[cfg(test)]
mod width_tests {
    use super::*;

    #[test]
    fn display_width_counts_cjk_as_two() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("中文"), 4); // two full-width cells each
        assert_eq!(display_width("a中b"), 4);
    }

    #[test]
    fn truncate_respects_display_columns() {
        // "中文字" is 6 columns; budget 5 keeps "中" (2) + "…" (1) = 3 cols,
        // never spilling a wide char past the limit.
        let t = truncate("中文字", 5);
        assert!(display_width(&t) <= 5, "width {} of {t:?}", display_width(&t));
        assert!(t.ends_with('…'));
        // Fits exactly → unchanged.
        assert_eq!(truncate("中文", 4), "中文");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    #[test]
    fn pad_display_fills_by_columns_not_chars() {
        // A char-count pad would make this 14 wide (12 chars); column pad must
        // account for the two full-width cells and land at exactly 14 columns.
        let p = pad_display("中文name", 14);
        assert_eq!(display_width(&p), 14);
        // ASCII path still behaves like a plain left-pad.
        assert_eq!(pad_display("ab", 5), "ab   ");
    }

    #[test]
    fn zero_width_is_empty() {
        assert_eq!(truncate("anything", 0), "");
        assert_eq!(pad_display("x", 0), "");
    }
}

#[cfg(test)]
mod format_usd_tests {
    use super::*;

    #[test]
    fn zero_and_signed_zero_are_plain() {
        assert_eq!(format_usd(0.0), "$0.0000");
        assert_eq!(format_usd(-0.0), "$0.0000");
        assert_eq!(format_usd(-1e-15), "$0.0000");
    }

    #[test]
    fn small_positive_keeps_four_decimals() {
        assert_eq!(format_usd(0.0123), "$0.0123");
    }

    #[test]
    fn negative_spend_still_shows_sign() {
        // Thresholds use `v >= 1.0`, so true negatives always take the
        // four-decimal branch — only dust/signed-zero is collapsed.
        assert_eq!(format_usd(-0.42), "$-0.4200");
        assert!(format_usd(-12.5).starts_with("$-"), "{}", format_usd(-12.5));
    }
}

#[cfg(test)]
mod time_tests {
    use super::*;

    #[test]
    fn rfc3339_z_converts_to_local() {
        let s = format_local_time("2026-07-20T12:00:00Z");
        // Local offset varies; just ensure we don't leave the raw Z form.
        assert!(!s.contains('Z'), "got {s}");
        assert!(s.starts_with("2026-07-20"), "got {s}");
    }

    #[test]
    fn unix_seconds_parse() {
        // 2026-01-01T00:00:00Z
        let s = format_local_time("1767225600");
        assert!(s.starts_with("2026-01-0"), "got {s}");
    }

    #[test]
    fn empty_is_em_dash() {
        assert_eq!(format_local_time(""), "—");
        assert_eq!(format_local_time("   "), "—");
    }

    #[test]
    fn date_only_passthrough() {
        assert_eq!(format_local_time("2026-07-15"), "2026-07-15");
    }
}

/// Vertical list of key=value for detail panes.
pub fn detail_kv(theme: &Theme, rows: &[(&str, String)]) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for (k, v) in rows {
        out.push(Line::from(vec![
            Span::styled(format!("  {k:<16}"), theme.muted()),
            Span::styled(v.clone(), Style::default().fg(theme.fg)),
        ]));
    }
    out
}

pub fn health_badge(theme: &Theme, ok: bool, label: &str) -> Span<'static> {
    if ok {
        Span::styled(format!(" {label} "), theme.badge_ok())
    } else {
        Span::styled(format!(" {label} "), theme.badge_err())
    }
}

#[cfg(test)]
mod heatmap_tests {
    use super::*;

    #[test]
    fn heat_level_buckets() {
        // tokscale bands: 0 | <0.25 | <0.50 | <0.75 | else
        assert_eq!(heat_level(0.0, 10.0), 0);
        assert_eq!(heat_level(1.0, 10.0), 1); // 0.10
        assert_eq!(heat_level(3.0, 10.0), 2); // 0.30
        assert_eq!(heat_level(5.0, 10.0), 3); // 0.50 → grade 3 (>=0.50)
        assert_eq!(heat_level(8.0, 10.0), 4); // 0.80
        assert_eq!(heat_level(10.0, 10.0), 4);
    }

    #[test]
    fn contribution_weeks_cover_year() {
        let weeks = build_contribution_weeks(&[], "2026-07-20", false);
        // ~52–53 weeks from start Sunday through today.
        assert!(weeks.len() >= 52, "got {}", weeks.len());
        assert!(weeks.len() <= 54, "got {}", weeks.len());
        // Every week has 7 slots; last week ends on the given day.
        let last = weeks.last().unwrap();
        let last_day = last.iter().flatten().last().unwrap();
        assert_eq!(last_day.date, "2026-07-20");
    }

    #[test]
    fn month_fills_zeros() {
        let rows = vec![("2026-07-02".into(), 1u64, 100u64, 1.5f64)];
        let days = month_day_spends("2026-07", &rows);
        assert_eq!(days.len(), 31);
        assert_eq!(days[0].total_usd, 0.0);
        assert_eq!(days[1].date, "2026-07-02");
        assert_eq!(days[1].total_usd, 1.5);
        assert_eq!(days[30].date, "2026-07-31");
    }

    #[test]
    fn weekday_sun0_known() {
        // 2026-07-01 was a Wednesday → Sun0 index 3
        assert_eq!(weekday_sun0(2026, 7, 1), 3);
        // 2026-07-05 was a Sunday → 0
        assert_eq!(weekday_sun0(2026, 7, 5), 0);
    }
}
