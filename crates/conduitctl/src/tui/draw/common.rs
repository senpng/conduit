//! Shared layout / naming / chart helpers used by multiple draw tabs.

use ratatui::layout::{Constraint, Direction, Layout, Rect};  // Constraint/Direction/Layout used by split_master_detail
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Table, TableState};
use ratatui::Frame;

use super::super::app::App;
use super::super::theme::Theme;
use super::super::widgets::{
    build_contribution_weeks, display_width, format_usd, heat_level, truncate,
    ContributionCell,
};

/// Stateful table render so the selected row stays in the visible viewport.
///
/// Each frame starts with offset 0; ratatui's [`TableState`] scrolls just far
/// enough for `selected` to appear (same behaviour as keeping a persistent
/// offset, without needing App-level scroll state).
pub(crate) fn render_scrollable_table(frame: &mut Frame, table: Table, area: Rect, selected: usize) {
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, area, &mut state);
}

/// First visible index so `selected` stays in a window of `visible` rows.
/// Selection sits at the bottom of the window when past the first page
/// (matches Table's ephemeral-offset behaviour).
pub(crate) fn list_window_start(selected: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len == 0 {
        return 0;
    }
    let visible = visible.min(len);
    let max_start = len.saturating_sub(visible);
    let start = selected.saturating_sub(visible.saturating_sub(1));
    start.min(max_start)
}

/// Split `flex` columns between a name column (~`name_pct`%) and a bar column.
///
/// `min_name` / `min_bar` are preferred minimums. On wide panes this is
/// `(flex * pct / 100).clamp(min_name, flex - min_bar)`. On narrow panes where
/// `flex < min_name + min_bar`, name is pinned to `min_name` and the bar takes
/// the remainder (possibly below `min_bar`). The clamp upper bound is floored
/// to `min_name` so `min > max` never happens — a raw `clamp` panics there.
pub(crate) fn split_name_bar(
    flex: usize,
    name_pct: usize,
    min_name: usize,
    min_bar: usize,
) -> (usize, usize) {
    let upper = flex.saturating_sub(min_bar).max(min_name);
    let name_w = (flex * name_pct / 100).clamp(min_name, upper);
    let bar_w = flex.saturating_sub(name_w);
    (name_w, bar_w)
}

/// Compact latency for KPI / health columns (`42ms`, `1.2s`, or `—`).
pub(crate) fn format_latency_ms(ms: Option<f64>) -> String {
    match ms {
        Some(ms) if ms >= 1000.0 => format!("{:.1}s", ms / 1000.0),
        Some(ms) => format!("{ms:.0}ms"),
        None => "—".into(),
    }
}

/// Compact tok/s for tight columns (no unit suffix): `1.2k`, `42.3`, or `—`.
pub(crate) fn format_tps_compact(v: Option<f64>) -> String {
    use super::super::widgets::format_tokens;
    match v {
        None => "—".into(),
        Some(v) if v >= 1000.0 => format_tokens(v.round() as u64),
        Some(v) => format!("{v:.1}"),
    }
}

/// Styled padded+truncated cell for fixed-width table columns.
pub(crate) fn padded_cell(text: &str, width: usize, style: Style) -> Span<'static> {
    use super::super::widgets::pad_display;
    Span::styled(pad_display(&truncate(text, width), width), style)
}

/// Leading space + cells separated by single spaces (overview health rows).
pub(crate) fn spaced_row(cells: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = Vec::with_capacity(cells.len() * 2 + 1);
    spans.push(Span::raw(" "));
    for (i, cell) in cells.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(cell);
    }
    Line::from(spans)
}

/// Prefer a rich `Line` when it fits `max_w`; otherwise a single truncated plain line.
pub(crate) fn fit_or_truncate(
    plain: String,
    rich: Line<'static>,
    max_w: usize,
    fallback_style: Style,
) -> Line<'static> {
    if display_width(&plain) <= max_w {
        rich
    } else {
        Line::from(Span::styled(truncate(&plain, max_w), fallback_style))
    }
}

/// Traffic-light style for a success rate in `[0, 1]`.
pub(crate) fn success_rate_style(theme: &Theme, rate: f64) -> Style {
    if rate >= 0.99 {
        theme.success()
    } else if rate >= 0.95 {
        theme.warning()
    } else {
        theme.error()
    }
}

/// Health-first ordering: worst success, then slowest TTFB, then most tokens.
///
/// Shared by overview provider strip and Usage → by provider (Date sort).
pub(crate) fn provider_health_cmp(
    a_rate: f64,
    a_ttfb: Option<f64>,
    a_tokens: u64,
    b_rate: f64,
    b_ttfb: Option<f64>,
    b_tokens: u64,
) -> std::cmp::Ordering {
    a_rate
        .partial_cmp(&b_rate)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| {
            b_ttfb
                .unwrap_or(f64::MAX)
                .partial_cmp(&a_ttfb.unwrap_or(f64::MAX))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| b_tokens.cmp(&a_tokens))
}

/// Empty strings render as an em dash in detail / list cells.
pub(crate) fn or_em_dash(s: &str) -> String {
    if s.is_empty() {
        "—".into()
    } else {
        s.to_string()
    }
}

/// Share of total as a short percent label (`12%`, `3.4%`, `<0.1%`).
pub(crate) fn format_share_pct(pct: f64) -> String {
    if pct >= 9.95 {
        format!("{pct:.0}%")
    } else if pct >= 0.05 {
        format!("{pct:.1}%")
    } else if pct > 0.0 {
        "<0.1%".into()
    } else {
        "0%".into()
    }
}

/// Ranked token-share rows: `name  ████  12%  1.19M` (chart-colored).
///
/// Used by overview month model mix and all-time top models — same recipe so
/// the two panels stay aligned.
pub(crate) fn token_share_lines(
    rows: &[(String, u64)],
    width: usize,
    max_rows: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use super::super::widgets::{format_tokens, pad_display, ratio_bar};

    if rows.is_empty() || width == 0 || max_rows == 0 {
        return Vec::new();
    }
    let total: u64 = rows.iter().map(|(_, t)| *t).sum::<u64>().max(1);
    let n = max_rows.min(rows.len());

    let pct_w = rows
        .iter()
        .take(n)
        .map(|(_, tok)| {
            display_width(&format_share_pct(*tok as f64 / total as f64 * 100.0))
        })
        .max()
        .unwrap_or(4)
        .max(4);
    let tok_w = rows
        .iter()
        .take(n)
        .map(|(_, tok)| display_width(&format_tokens(*tok)))
        .max()
        .unwrap_or(4)
        .max(4);
    // leading space + 3 gaps (name|bar, bar|pct, pct|tok)
    let gaps = 4usize;
    let flex = width.saturating_sub(pct_w + tok_w + gaps).max(10);
    let (name_w, bar_w) = split_name_bar(flex, 40, 8, 6);

    let mut lines = Vec::with_capacity(n);
    for (i, (name, tok)) in rows.iter().take(n).enumerate() {
        let pct = *tok as f64 / total as f64 * 100.0;
        let color = theme.chart_color(i);
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                pad_display(&truncate(name, name_w), name_w),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                ratio_bar(*tok as f64 / total as f64, bar_w),
                Style::default().fg(color),
            ),
            Span::raw(" "),
            Span::styled(pad_display(&format_share_pct(pct), pct_w), theme.muted()),
            Span::raw(" "),
            Span::styled(
                pad_display(&format_tokens(*tok), tok_w),
                theme.warning(),
            ),
        ]));
    }
    lines
}

pub(crate) fn split_master_detail(area: Rect) -> (Rect, Rect) {
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);
    (parts[0], parts[1])
}

/// Period token total for KPI cards. Prefer top-level `total_tokens`; fall back
/// to day/entry sums for older daemons that omit the field.
pub(crate) fn summary_total_tokens(s: &crate::dto::UsageSummaryView) -> u64 {
    if s.total_tokens > 0 {
        s.total_tokens
    } else if !s.by_day.is_empty() {
        s.by_day.iter().map(|d| d.total_tokens).sum()
    } else {
        s.entries.iter().map(|e| e.total_tokens).sum()
    }
}

pub(crate) fn day_tuples_from_summary(
    s: &crate::dto::UsageSummaryView,
) -> Vec<(String, u64, u64, f64)> {
    s.by_day
        .iter()
        .map(|d| {
            (
                d.day.clone(),
                d.request_count,
                d.total_tokens,
                d.total_usd,
            )
        })
        .collect()
}

pub(crate) fn trailing_tuples_from_summary(
    s: &crate::dto::UsageSummaryView,
) -> Vec<(String, u64, u64, f64)> {
    let src = if s.by_day_trailing.is_empty() {
        s.by_day.as_slice()
    } else {
        s.by_day_trailing.as_slice()
    };
    src.iter()
        .map(|d| {
            (
                d.day.clone(),
                d.request_count,
                d.total_tokens,
                d.total_usd,
            )
        })
        .collect()
}

pub(crate) fn usage_by_day_tuples(app: &App) -> Vec<(String, u64, u64, f64)> {
    app.usage_summary
        .as_ref()
        .map(day_tuples_from_summary)
        .unwrap_or_default()
}

/// Overview daily rows (`period=all` includes full history; month chart filters).
pub(crate) fn overview_by_day_tuples(app: &App) -> Vec<(String, u64, u64, f64)> {
    app.overview_summary
        .as_ref()
        .map(day_tuples_from_summary)
        .unwrap_or_default()
}

/// Trailing-year daily rows for the contribution graph (falls back to period).
pub(crate) fn usage_trailing_tuples(app: &App) -> Vec<(String, u64, u64, f64)> {
    app.usage_summary
        .as_ref()
        .map(trailing_tuples_from_summary)
        .unwrap_or_default()
}

pub(crate) fn overview_trailing_tuples(app: &App) -> Vec<(String, u64, u64, f64)> {
    app.overview_summary
        .as_ref()
        .map(trailing_tuples_from_summary)
        .unwrap_or_default()
}

pub(crate) fn today_ymd() -> String {
    let n = chrono::Local::now().date_naive();
    n.format("%Y-%m-%d").to_string()
}

pub(crate) fn contribution_graph_lines(
    theme: &Theme,
    by_day: &[(String, u64, u64, f64)],
    selected_date: Option<&str>,
    idle_hint: &str,
    heat_by_tokens: bool,
) -> Vec<Line<'static>> {
    let today = today_ymd();
    let weeks = build_contribution_weeks(by_day, &today, heat_by_tokens);
    if weeks.is_empty() {
        return vec![Line::from(Span::styled("  (no graph)", theme.subtle()))];
    }

    // Fit as many trailing weeks as the terminal allows (~2 cols each).
    // Caller draws into a panel; we use a soft cap of 52.
    let max_weeks = 52usize.min(weeks.len());
    let start = weeks.len().saturating_sub(max_weeks);
    let shown = &weeks[start..];

    let mut lines = Vec::new();

    // Month labels on the first week-column of each new month (tokscale).
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut month_spans = vec![Span::raw("    ")]; // gutter for day labels
    let mut last_month: Option<u32> = None;
    for week in shown {
        // First non-empty day in the week determines month.
        let m = week.iter().flatten().find_map(|c| {
            c.date
                .get(5..7)
                .and_then(|s| s.parse::<u32>().ok())
        });
        if let Some(m) = m {
            if last_month != Some(m) && (1..=12).contains(&m) {
                month_spans.push(Span::styled(
                    format!("{:<2}", MONTHS[(m - 1) as usize]),
                    theme.muted(),
                ));
                last_month = Some(m);
            } else {
                month_spans.push(Span::raw("  "));
            }
        } else {
            month_spans.push(Span::raw("  "));
        }
    }
    lines.push(Line::from(month_spans));

    // Rows = weekdays; only Mon/Wed/Fri labeled (tokscale / GitHub).
    const ROW_LABEL: [&str; 7] = ["", "Mon", "", "Wed", "", "Fri", ""];
    for wd in 0..7 {
        let mut spans = vec![Span::styled(
            format!("{:<4}", ROW_LABEL[wd]),
            theme.subtle(),
        )];
        for week in shown {
            match &week[wd] {
                None => spans.push(Span::raw("  ")),
                Some(cell) => {
                    let lvl = heat_level_from_intensity(cell.intensity);
                    let selected = selected_date == Some(cell.date.as_str());
                    let (glyph, style) = if selected {
                        ("▓▓", theme.heat_selected_style(lvl))
                    } else if cell.intensity <= 0.0 && cell.total_usd <= 0.0 {
                        ("· ", theme.subtle())
                    } else {
                        ("██", theme.heat_cell_style(lvl))
                    };
                    spans.push(Span::styled(glyph, style));
                }
            }
        }
        lines.push(Line::from(spans));
    }

    // Legend (tokscale footer style).
    let mut footer = vec![
        Span::raw("    "),
        Span::styled("Less ", theme.subtle()),
        Span::styled("· ", theme.subtle()),
    ];
    for lvl in 1u8..=4 {
        footer.push(Span::styled("██", theme.heat_cell_style(lvl)));
        footer.push(Span::raw(" "));
    }
    footer.push(Span::styled("More", theme.subtle()));

    if let Some(date) = selected_date {
        if let Some(cell) = find_cell(shown, date) {
            footer.push(Span::styled(
                format!(
                    "   ▸ {}  {}  ·  {} req",
                    cell.date,
                    format_usd(cell.total_usd),
                    cell.request_count
                ),
                theme.accent_bold(),
            ));
        }
    } else if !idle_hint.is_empty() {
        footer.push(Span::styled(format!("   {idle_hint}"), theme.subtle()));
    }
    lines.push(Line::from(footer));

    lines
}

pub(crate) fn heat_level_from_intensity(intensity: f64) -> u8 {
    // Same 0..=4 tokscale bands as `widgets::heat_level`; intensity is already
    // normalized to 0..=1, so peak is 1.0. Delegated so the thresholds live in
    // one place and can't drift between the heatmap and its legend.
    heat_level(intensity, 1.0)
}

pub(crate) fn find_cell<'a>(
    weeks: &'a [[Option<ContributionCell>; 7]],
    date: &str,
) -> Option<&'a ContributionCell> {
    for week in weeks {
        for cell in week.iter().flatten() {
            if cell.date == date {
                return Some(cell);
            }
        }
    }
    None
}


pub(crate) struct ResolvedName {
    /// Human-readable text (never empty).
    pub(crate) text: String,
    /// Soft-deleted (or hard-missing) — render with strikethrough.
    pub(crate) deleted: bool,
}

/// Map downstream key id → human name.
///
/// Prefers the name/deleted flag from usage summary (includes soft-deleted
/// rows). Falls back to the live Keys list, then a truncated id.
pub(crate) fn resolve_key_name(
    keys: &[crate::dto::KeyView],
    id: &str,
    summary_name: &str,
    summary_deleted: bool,
) -> ResolvedName {
    if id.is_empty() {
        return ResolvedName {
            text: "(anonymous)".into(),
            deleted: false,
        };
    }
    if !summary_name.is_empty() {
        return ResolvedName {
            text: summary_name.to_string(),
            deleted: summary_deleted,
        };
    }
    if let Some(k) = keys.iter().find(|k| k.id == id) {
        let text = if k.name.is_empty() {
            id.to_string()
        } else {
            k.name.clone()
        };
        return ResolvedName {
            text,
            deleted: false,
        };
    }
    // Soft-deleted with empty name, or hard-missing row.
    ResolvedName {
        text: if summary_deleted {
            truncate(id, 12)
        } else {
            format!("(deleted) {}", truncate(id, 12))
        },
        deleted: true,
    }
}

/// Map provider id → human name.
///
/// Prefers the name/deleted flag from usage summary (includes soft-deleted
/// rows). Falls back to the live Providers list, then a truncated id.
pub(crate) fn resolve_provider_name(
    providers: &[crate::dto::ProviderView],
    id: &str,
    summary_name: &str,
    summary_deleted: bool,
) -> ResolvedName {
    if id.is_empty() || id == "(unknown)" {
        return ResolvedName {
            text: "(unknown)".into(),
            deleted: false,
        };
    }
    if !summary_name.is_empty() {
        return ResolvedName {
            text: summary_name.to_string(),
            deleted: summary_deleted,
        };
    }
    if let Some(p) = providers.iter().find(|p| p.id == id) {
        let text = if p.name.is_empty() {
            id.to_string()
        } else {
            p.name.clone()
        };
        return ResolvedName {
            text,
            deleted: false,
        };
    }
    // Soft-deleted with empty name, or hard-missing row.
    ResolvedName {
        text: if summary_deleted {
            truncate(id, 12)
        } else {
            format!("(gone) {}", truncate(id, 12))
        },
        deleted: true,
    }
}

/// Base style for a usage name label (no strikethrough — that is text-only).
pub(crate) fn name_base_style(theme: &Theme, selected: bool) -> Style {
    if selected {
        theme.accent_bold()
    } else {
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
    }
}

/// Render a name so strikethrough covers **glyphs only**, never trailing pad.
///
/// When `width` is set, the text is truncated and right-padded to that many
/// display columns; padding is a separate unstyled-modifier span so terminals
/// do not draw a strike through empty cells.
pub(crate) fn name_spans(theme: &Theme, name: &ResolvedName, selected: bool, width: Option<usize>) -> Vec<Span<'static>> {
    let style = name_base_style(theme, selected);
    let text_style = if name.deleted {
        style.add_modifier(Modifier::CROSSED_OUT)
    } else {
        style
    };
    let text = match width {
        Some(w) => truncate(&name.text, w),
        None => name.text.clone(),
    };
    let mut spans = vec![Span::styled(text.clone(), text_style)];
    if let Some(w) = width {
        let pad = w.saturating_sub(display_width(&text));
        if pad > 0 {
            spans.push(Span::styled(
                " ".repeat(pad),
                style, // same color/bold, no CROSSED_OUT
            ));
        }
    }
    spans
}


pub(crate) fn sort_usage_items(
    items: &mut [(usize, String, String, f64, u64, u64, Option<f64>)],
    sort: super::super::app::UsageSort,
) {
    use super::super::app::UsageSort;
    match sort {
        UsageSort::Cost => sort_by_cost_desc(items, |x| x.3),
        UsageSort::Tokens => items.sort_by(|a, b| b.4.cmp(&a.4)),
        UsageSort::Date => items.sort_by(|a, b| a.1.cmp(&b.1)), // label A–Z
    }
}

/// Descending cost sort (NaN-safe).
pub(crate) fn sort_by_cost_desc<T>(items: &mut [T], cost: impl Fn(&T) -> f64) {
    items.sort_by(|a, b| {
        cost(b)
            .partial_cmp(&cost(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Descending token sort.
pub(crate) fn sort_by_tokens_desc<T>(items: &mut [T], tokens: impl Fn(&T) -> u64) {
    items.sort_by(|a, b| tokens(b).cmp(&tokens(a)));
}

/// Format token counts; show "—" when zero so cache columns stay scannable.
pub(crate) fn fmt_tok(n: u32) -> String {
    if n == 0 {
        "—".into()
    } else {
        n.to_string()
    }
}


pub(crate) fn wrapped_line_count(lines: &[Line<'_>], width: usize) -> usize {
    if width == 0 {
        return lines.len();
    }
    lines
        .iter()
        .map(|line| {
            let w = line.width().max(1);
            w.div_ceil(width).max(1)
        })
        .sum()
}

