//! Overview tab — metrics, contribution heatmap, month chart, provider health.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::super::app::App;
use super::super::theme::Theme;
use super::super::widgets::{
    format_tokens, format_usd, month_day_spends, panel_block, panel_block_borders, ratio_bar,
    DaySpend,
};
use super::common::{
    contribution_graph_lines, fit_or_truncate, format_latency_ms, format_tps_compact, name_spans,
    or_em_dash, overview_by_day_tuples, overview_trailing_tuples, padded_cell, provider_health_cmp,
    resolve_provider_name, spaced_row, split_name_bar, success_rate_style, summary_total_tokens,
    token_share_lines,
};

// ── Overview dashboard ──────────────────────────────────────────────────────

pub(crate) fn draw_overview(frame: &mut Frame, area: Rect, app: &App) {
    // Adaptive layout — month chart is the primary short-range signal, so it
    // gets more vertical room than the rank lists (multi-row day bars).
    let h = area.height;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if h >= 36 {
            [
                Constraint::Length(3),  // metrics
                Constraint::Length(11), // contribution + top models
                Constraint::Length(14), // month token bars (full width)
                Constraint::Min(4),     // provider health
            ]
        } else if h >= 28 {
            [
                Constraint::Length(3),
                Constraint::Length(10),
                Constraint::Length(12),
                Constraint::Min(3),
            ]
        } else {
            [
                Constraint::Length(3),
                Constraint::Length(9),
                Constraint::Length(10),
                Constraint::Min(2),
            ]
        })
        .split(area);

    draw_overview_metrics(frame, rows[0], app);

    // One horizontal split recipe for both chart rows so left edges line up
    // and side-by-side panels share a single vertical border (no double line).
    let pair = |area: Rect| {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area)
    };
    let mid = pair(rows[1]);
    draw_overview_contribution(frame, mid[0], app);
    draw_overview_top_models(frame, mid[1], app);

    // Same 58/42 split as the contribution row so the month chart frame
    // lines up with the heatmap (model mix fills the right column).
    let lower = pair(rows[2]);
    draw_overview_month_spark(frame, lower[0], lower[1], app);

    draw_overview_provider_health(frame, rows[3], app);
}

/// Left column of an overview pair.
fn overview_left_block(theme: &Theme, title: impl Into<String>, focused: bool) -> Block<'static> {
    panel_block_borders(theme, title, focused, Borders::ALL)
}

/// Right column of an overview pair (independent full border — avoids a
/// missing shared edge when left-panel content paints up to the join).
fn overview_right_block(theme: &Theme, title: impl Into<String>) -> Block<'static> {
    panel_block_borders(theme, title, false, Borders::ALL)
}

/// Re-paint the right border column of a `Borders::ALL` panel after content.
/// Long Paragraph lines can still clobber the border cell on some terminals;
/// this forces the edge back on top.
fn restore_right_border(frame: &mut Frame, area: Rect, border_style: Style) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let x = area.x + area.width - 1;
    let buf = frame.buffer_mut();
    let bottom = area.y + area.height - 1;
    for y in area.y..=bottom {
        let Some(cell) = buf.cell_mut((x, y)) else {
            continue;
        };
        let sym = if y == area.y {
            "┐"
        } else if y == bottom {
            "┘"
        } else {
            "│"
        };
        cell.set_symbol(sym);
        cell.set_style(border_style);
    }
}

fn draw_overview_metrics(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let health = app
        .health
        .as_ref()
        .map(|h| h.status.clone())
        .unwrap_or_else(|| "down".into());
    let health_style = if app.daemon_ok() {
        theme.success()
    } else {
        theme.error()
    };

    // Tokens first among usage KPIs (lifetime); $ / latency stay secondary.
    let kpis = overview_usage_kpis(app, theme);
    super::super::widgets::metric_strip(
        frame,
        area,
        theme,
        &[
            ("HEALTH", health, health_style),
            ("TOKENS", kpis.tokens, kpis.tok_style),
            ("SPEND", kpis.cost, kpis.spend_style),
            ("SUCCESS", kpis.success, kpis.ok_style),
            ("TTFB", kpis.ttfb, Style::default().fg(theme.chart[4])),
            ("LAT", kpis.latency, Style::default().fg(theme.chart[2])),
            ("SPEED", kpis.speed, Style::default().fg(theme.chart[0])),
        ],
    );
}

/// Usage KPI strip values (placeholders until overview summary loads).
struct OverviewUsageKpis {
    tokens: String,
    cost: String,
    success: String,
    ttfb: String,
    latency: String,
    speed: String,
    tok_style: Style,
    spend_style: Style,
    ok_style: Style,
}

fn overview_usage_kpis(app: &App, theme: &Theme) -> OverviewUsageKpis {
    let Some(s) = app.overview_summary.as_ref() else {
        let muted = theme.muted();
        return OverviewUsageKpis {
            tokens: "… · all-time".into(),
            cost: "…".into(),
            success: "…".into(),
            ttfb: "…".into(),
            latency: "…".into(),
            speed: "…".into(),
            tok_style: muted,
            spend_style: muted,
            ok_style: muted,
        };
    };
    let period = if s.period.eq_ignore_ascii_case("all") {
        "all-time"
    } else {
        s.period.as_str()
    };
    OverviewUsageKpis {
        tokens: format!("{} · {period}", format_tokens(summary_total_tokens(s))),
        cost: format_usd(s.total_usd),
        success: format!("{:.0}%", s.success_rate * 100.0),
        ttfb: format_latency_ms(s.avg_ttfb_ms),
        latency: format_latency_ms(s.avg_duration_ms),
        speed: super::super::widgets::format_tok_per_sec(s.tokens_per_sec),
        tok_style: theme.warning(),
        spend_style: theme.success(),
        ok_style: success_rate_style(theme, s.success_rate),
    }
}

fn draw_overview_contribution(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = overview_left_block(
        theme,
        "Contribution  ·  52 weeks  ·  tokens by day",
        true,
    );
    let border_style = theme.border_active();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // Clear inner so long lines never ghost into the right border cell.
    frame.render_widget(Clear, inner);

    let rows = overview_trailing_tuples(app);
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  no usage yet — send traffic through the gateway",
                theme.subtle(),
            )),
            inner,
        );
        restore_right_border(frame, area, border_style);
        return;
    }
    let lines = contribution_graph_lines(theme, &rows, None, "", true);
    frame.render_widget(Paragraph::new(lines), inner);
    restore_right_border(frame, area, border_style);
}

/// Current-month **token** chart + optional model mix (right column).
///
/// `chart_area` / `mix_area` share the same horizontal split as Contribution /
/// Top models so the month chart frame matches the heatmap width.
fn draw_overview_month_spark(frame: &mut Frame, chart_area: Rect, mix_area: Rect, app: &App) {
    let theme = &app.theme;
    let period = super::super::forms::current_period();
    // One by_day_model scan feeds both the Models panel ranking and bar stacks.
    let by_day = period_day_model_tokens(app, &period);
    let model_mix = month_model_token_mix(app, &by_day);

    draw_month_token_chart(frame, chart_area, app, &period, &model_mix, &by_day);
    if model_mix.is_empty() {
        // Keep a right panel so the split still mirrors the row above.
        let block = overview_right_block(theme, format!("Models  {period}  ·  no traffic"));
        frame.render_widget(block, mix_area);
    } else {
        draw_token_share_panel(
            frame,
            mix_area,
            theme,
            format!("Models  ·  {period}  ·  tokens"),
            &model_mix,
            "  no model split",
        );
    }
}

/// Aggregated month-chart numbers derived from calendar day spends.
struct MonthChartStats {
    month_tok: u64,
    active_n: usize,
    peak_idx: Option<usize>,
    peak_share: f64,
    avg_active: u64,
}

impl MonthChartStats {
    fn from_days(days: &[DaySpend]) -> Self {
        let month_tok: u64 = days.iter().map(|d| d.total_tokens).sum();
        let active_n = days
            .iter()
            .filter(|d| d.total_tokens > 0 || d.request_count > 0)
            .count();
        let peak_idx = days
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.total_tokens.cmp(&b.total_tokens))
            .map(|(i, _)| i);
        let peak_share = peak_idx
            .and_then(|i| days.get(i))
            .map(|p| {
                if month_tok > 0 {
                    p.total_tokens as f64 / month_tok as f64 * 100.0
                } else {
                    0.0
                }
            })
            .unwrap_or(0.0);
        let avg_active = if active_n > 0 {
            month_tok / active_n as u64
        } else {
            0
        };
        Self {
            month_tok,
            active_n,
            peak_idx,
            peak_share,
            avg_active,
        }
    }

    fn peak_day<'a>(&self, days: &'a [DaySpend]) -> Option<&'a DaySpend> {
        self.peak_idx.and_then(|i| days.get(i))
    }
}

fn draw_month_token_chart(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    period: &str,
    model_mix: &[(String, u64)],
    by_day: &DayModelTokens,
) {
    let theme = &app.theme;
    let block = panel_block(
        theme,
        format!("This month  {period}  ·  tokens by day"),
        true,
    );
    let border_style = theme.border_active();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Clear, inner);

    let days = month_day_spends(period, &overview_by_day_tuples(app));
    let stats = MonthChartStats::from_days(&days);

    if stats.active_n == 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  no tokens this calendar month",
                theme.subtle(),
            )),
            inner,
        );
        restore_right_border(frame, area, border_style);
        return;
    }

    let show_callout = inner.height >= 5;
    let header_h: u16 = if show_callout { 2 } else { 1 };
    let body_h = inner.height.saturating_sub(header_h + 1).max(1); // + axis

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_h),
            Constraint::Length(body_h),
            Constraint::Length(1),
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(month_chart_header(
            theme,
            chunks[0].width as usize,
            &stats,
            days.len(),
            stats.peak_day(&days),
            show_callout,
        )),
        chunks[0],
    );

    let n = days.len().max(1);
    // Keep a right gutter so baseline glyphs never sit on the border cell.
    let chart_w = chunks[1].width as usize;
    let label_w = 2usize;
    let col_w = chart_w.saturating_sub(label_w + 1).max(1);
    let toks: Vec<f64> = days.iter().map(|d| d.total_tokens as f64).collect();
    // Stacks reuse the same by_day map as the Models ranking (no second scan).
    let stacks = month_day_model_stacks(&days, model_mix, by_day, theme);

    draw_day_bar_panel(
        frame,
        chunks[1],
        &toks,
        n,
        col_w,
        "T",
        theme.chart_color(1),
        theme,
        stats.peak_idx,
        &stacks,
    );

    if chunks[2].height > 0 {
        let axis_w = (chunks[2].width as usize)
            .saturating_sub(label_w + 1)
            .max(1);
        let axis = day_axis_line(n, axis_w.min(col_w), theme, stats.peak_idx);
        let mut spans = vec![Span::raw("  ")];
        spans.extend(axis.spans);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(theme.surface()),
            chunks[2],
        );
    }

    restore_right_border(frame, area, border_style);
}

/// Header lines for the month token chart (summary + optional peak callout).
fn month_chart_header(
    theme: &Theme,
    header_w: usize,
    stats: &MonthChartStats,
    n_days: usize,
    peak_day: Option<&DaySpend>,
    show_callout: bool,
) -> Vec<Line<'static>> {
    let tok_s = format_tokens(stats.month_tok);
    let avg_s = format_tokens(stats.avg_active);
    let active_n = stats.active_n;
    let line1_plain = format!("  tok {tok_s}  active {active_n}/{n_days}  avg/active {avg_s}");
    let mut lines = vec![fit_or_truncate(
        line1_plain,
        Line::from(vec![
            Span::styled("  tok ", theme.subtle()),
            Span::styled(format!("{tok_s}  "), theme.warning()),
            Span::styled(format!("active {active_n}/{n_days}  "), theme.accent_bold()),
            Span::styled(format!("avg/active {avg_s}"), theme.subtle()),
        ]),
        header_w,
        theme.subtle(),
    )];
    if show_callout {
        if let Some(p) = peak_day {
            let dom = p.date.get(8..10).unwrap_or("??");
            let peak_tok_s = format_tokens(p.total_tokens);
            let req = if p.request_count > 0 {
                format!("  · {} req", p.request_count)
            } else {
                String::new()
            };
            let share = format!("{:.0}% of month", stats.peak_share);
            let line2_plain = format!("  peak day {dom}  {peak_tok_s}  {share}{req}");
            lines.push(fit_or_truncate(
                line2_plain,
                Line::from(vec![
                    Span::styled("  peak ", theme.subtle()),
                    Span::styled(format!("day {dom}"), theme.accent_bold()),
                    Span::styled(format!("  {peak_tok_s}  "), theme.muted()),
                    Span::styled(
                        share,
                        if stats.peak_share >= 80.0 {
                            theme.warning()
                        } else {
                            theme.subtle()
                        },
                    ),
                    Span::styled(req, theme.subtle()),
                ]),
                header_w,
                theme.muted(),
            ));
        }
    }
    lines
}

/// day → label → tokens for the calendar month (positive tokens only).
type DayModelTokens = std::collections::HashMap<String, std::collections::HashMap<String, u64>>;

/// Single scan of `by_day_model` for `period` — shared by mix ranking and stacks.
fn period_day_model_tokens(app: &App, period: &str) -> DayModelTokens {
    let mut by_day = DayModelTokens::new();
    let Some(s) = app.overview_summary.as_ref() else {
        return by_day;
    };
    for m in &s.by_day_model {
        if m.day.starts_with(period) && m.total_tokens > 0 {
            *by_day
                .entry(m.day.clone())
                .or_default()
                .entry(m.label.clone())
                .or_default() += m.total_tokens;
        }
    }
    by_day
}

/// Aggregate model token totals for the month (from day×model, else by_model).
fn month_model_token_mix(app: &App, by_day: &DayModelTokens) -> Vec<(String, u64)> {
    let mut map: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for day_map in by_day.values() {
        for (label, tok) in day_map {
            *map.entry(label.clone()).or_default() += *tok;
        }
    }
    // Fallback: when day×model rows are missing, use all-time by_model.
    if map.is_empty() {
        if let Some(s) = app.overview_summary.as_ref() {
            for m in &s.by_model {
                if m.total_tokens > 0 {
                    *map.entry(m.label.clone()).or_default() += m.total_tokens;
                }
            }
        }
    }
    let mut rows: Vec<(String, u64)> = map.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// Per-day model token stacks for the month chart (bottom → top).
///
/// Colors follow the same ranking as the right-hand Models panel
/// (`theme.chart_color(i)` for the i-th month-ranked model). Empty segment
/// lists mean “no day×model breakdown” and fall back to a solid bar color.
fn month_day_model_stacks(
    days: &[DaySpend],
    model_mix: &[(String, u64)],
    by_day: &DayModelTokens,
    theme: &Theme,
) -> Vec<Vec<(ratatui::style::Color, f64)>> {
    let empty = || days.iter().map(|_| Vec::new()).collect();
    if model_mix.is_empty() || days.is_empty() || by_day.is_empty() {
        // No per-day model rows — solid bars (mix may still come from by_model).
        return empty();
    }

    days.iter()
        .map(|d| {
            let Some(day_map) = by_day.get(&d.date) else {
                return Vec::new();
            };
            let mut segs: Vec<(ratatui::style::Color, f64)> = Vec::new();
            let mut used = 0u64;
            for (i, (name, _)) in model_mix.iter().enumerate() {
                let t = day_map.get(name).copied().unwrap_or(0);
                if t > 0 {
                    segs.push((theme.chart_color(i), t as f64));
                    used += t;
                }
            }
            // Models not in the month ranking (rare) + any ledger remainder.
            let day_sum: u64 = day_map.values().sum();
            let rest = d.total_tokens.max(day_sum).saturating_sub(used);
            if rest > 0 {
                segs.push((theme.muted, rest as f64));
            }
            segs
        })
        .collect()
}

/// Multi-row column bars for one series (`$` or tokens), day-aligned.
///
/// When `stacks[day]` is non-empty, each bar is colored by model share using
/// the same palette order as the Models panel; otherwise the solid `fg` is used.
fn draw_day_bar_panel(
    frame: &mut Frame,
    area: Rect,
    values: &[f64],
    n_days: usize,
    col_w: usize,
    label: &str,
    fg: ratatui::style::Color,
    theme: &Theme,
    peak_idx: Option<usize>,
    stacks: &[Vec<(ratatui::style::Color, f64)>],
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let bar_h = area.height as usize;
    let lines = day_bar_lines(values, col_w, n_days, bar_h, fg, theme, peak_idx, stacks);
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    for (i, mut spark) in lines.into_iter().enumerate() {
        let prefix = if i == 0 {
            Span::styled(format!("{label} "), theme.subtle())
        } else {
            Span::raw("  ")
        };
        let mut spans = vec![prefix];
        spans.append(&mut spark.spans);
        out.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(out), area);
}

/// Inclusive column range `[start, end)` for a calendar day — spreads days
/// evenly across the full panel width (so the chart looks wide, not left-packed).
fn day_col_range(day_i: usize, n_days: usize, width: usize) -> (usize, usize) {
    if n_days == 0 || width == 0 {
        return (0, 0);
    }
    let start = day_i * width / n_days;
    let end = ((day_i + 1) * width / n_days).max(start + 1).min(width);
    (start, end)
}

/// Pick the model color at height `y` from the bottom of a stacked bar
/// (`y` in `[0, lv]`, segments ordered bottom → top).
pub(crate) fn stack_color_at(
    y: f64,
    lv: f64,
    segments: &[(ratatui::style::Color, f64)],
    fallback: ratatui::style::Color,
) -> ratatui::style::Color {
    if segments.is_empty() || lv <= 0.0 {
        return fallback;
    }
    let total: f64 = segments.iter().map(|(_, w)| *w).sum();
    if total <= 0.0 {
        return fallback;
    }
    let y = y.clamp(0.0, lv);
    let mut cum = 0.0;
    for (color, w) in segments {
        cum += (*w / total) * lv;
        // Prefer lower segment on exact boundaries so thin slices stay visible.
        if y <= cum + 1e-12 {
            return *color;
        }
    }
    segments.last().map(|(c, _)| *c).unwrap_or(fallback)
}

/// Filled height in bar rows for a day value (`0` or `[0.5, height]`).
fn day_bar_level(value: f64, max: f64, height: usize) -> f64 {
    if value <= 0.0 {
        0.0
    } else {
        ((value / max) * height as f64).clamp(0.5, height as f64)
    }
}

/// Glyph for one cell of a vertical day bar at row threshold `threshold`.
///
/// Baseline uses ASCII `-` (not box-drawing) so ambiguous-width terminals never
/// advance two cells and punch through the right border.
fn day_bar_glyph(lv: f64, threshold: f64, baseline_row: bool) -> char {
    if lv <= 0.0 {
        if baseline_row {
            '-'
        } else {
            ' '
        }
    } else if lv + 1e-9 >= threshold {
        '█'
    } else if lv + 1e-9 >= threshold - 0.5 {
        '▄'
    } else {
        ' '
    }
}

/// Style for a filled (or baseline) day-bar cell.
fn day_bar_cell_style(
    ch: char,
    lv: f64,
    threshold: f64,
    is_peak: bool,
    segs: &[(ratatui::style::Color, f64)],
    fg: ratatui::style::Color,
    theme: &Theme,
    baseline_row: bool,
) -> Style {
    let base = Style::default().fg(fg);
    let quiet = Style::default().fg(theme.subtle);
    if lv <= 0.0 && baseline_row {
        return quiet;
    }
    if ch == ' ' {
        return base;
    }
    // Sample stack color at the vertical mid of this cell (from bottom).
    let y_sample = if lv + 1e-9 >= threshold {
        (threshold - 0.5).clamp(0.0, lv)
    } else {
        // Half-block: color of the upper edge of the filled portion.
        (lv - 0.25).clamp(0.0, lv)
    };
    let color = if segs.is_empty() {
        // No day×model data: solid bar; peak day uses warning (legacy).
        if is_peak {
            theme.warning
        } else {
            fg
        }
    } else {
        // Keep model colors on peak day; bold marks the peak.
        stack_color_at(y_sample, lv, segs, fg)
    };
    let mut style = Style::default().fg(color);
    if is_peak {
        style = style.add_modifier(Modifier::BOLD);
    }
    style
}

/// Run-length encode parallel char/style buffers into spans.
fn spans_from_styled_buf(buf: &[char], styles: &[Style]) -> Vec<Span<'static>> {
    if buf.is_empty() {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_st = styles[0];
    for (c, &st) in buf.iter().zip(styles.iter()) {
        if st != run_st {
            if !run.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut run), run_st));
            }
            run_st = st;
        }
        run.push(*c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_st));
    }
    spans
}

/// Multi-line column chart: each day is a vertical stack of `█`,
/// **stretched across the full width** (variable bar width per day).
///
/// Optional per-day `stacks` paint model shares (bottom → top) with the Models
/// panel palette. Peak day keeps model colors and only adds bold emphasis.
fn day_bar_lines(
    values: &[f64],
    width: usize,
    n_days: usize,
    height: usize,
    fg: ratatui::style::Color,
    theme: &Theme,
    peak_idx: Option<usize>,
    stacks: &[Vec<(ratatui::style::Color, f64)>],
) -> Vec<Line<'static>> {
    if values.is_empty() || width == 0 || n_days == 0 || height == 0 {
        return vec![Line::from(Span::styled("—".to_string(), theme.subtle()))];
    }
    let max = values.iter().copied().fold(0.0_f64, f64::max).max(1e-12);
    let base_style = Style::default().fg(fg);

    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let threshold = (height - row) as f64;
        let baseline_row = row + 1 == height;
        let mut buf = vec![' '; width];
        let mut styles = vec![base_style; width];

        for day_i in 0..n_days {
            let (c0, c1) = day_col_range(day_i, n_days, width);
            let v = values.get(day_i).copied().unwrap_or(0.0);
            let lv = day_bar_level(v, max, height);
            let is_peak = peak_idx == Some(day_i);
            let segs = stacks.get(day_i).map(|s| s.as_slice()).unwrap_or(&[]);
            let ch = day_bar_glyph(lv, threshold, baseline_row);
            let st =
                day_bar_cell_style(ch, lv, threshold, is_peak, segs, fg, theme, baseline_row);
            // Fill the day's slot; leave a 1-col gutter when each day has ≥3 cols.
            let bar_end = if c1 - c0 >= 3 { c1 - 1 } else { c1 };
            for x in c0..bar_end {
                buf[x] = ch;
                styles[x] = st;
            }
        }
        lines.push(Line::from(spans_from_styled_buf(&buf, &styles)));
    }
    lines
}

/// Day numbers under the bars; peak day is emphasized when provided.
fn day_axis_line(
    n_days: usize,
    width: usize,
    theme: &Theme,
    peak_idx: Option<usize>,
) -> Line<'static> {
    if n_days == 0 || width == 0 {
        return Line::from("");
    }
    let subtle = theme.subtle();
    let mut buf = vec![' '; width];
    let mut styles = vec![subtle; width];
    let mut used = vec![false; width];

    let step = if width >= 20 { 5usize } else { 10 };
    // Prefer peak, then last day, then first, then regular ticks — so dense
    // labels drop non-critical marks first when they collide.
    let mut ticks: Vec<usize> = (0..n_days)
        .filter(|&i| i == 0 || i + 1 == n_days || (i + 1) % step == 0 || peak_idx == Some(i))
        .collect();
    ticks.sort_by_key(|&i| {
        let rank = match (Some(i) == peak_idx, i + 1 == n_days, i == 0) {
            (true, _, _) => 0,
            (_, true, _) => 1,
            (_, _, true) => 2,
            _ => 3,
        };
        (rank, i)
    });

    let peak_style = theme.accent_bold();
    for day_i in ticks {
        let label = format!("{}", day_i + 1);
        let (c0, c1) = day_col_range(day_i, n_days, width);
        let slot = c1 - c0;
        let start = if label.len() <= slot {
            c0 + (slot - label.len()) / 2
        } else {
            c0.min(width.saturating_sub(label.len()))
        };
        let end = start + label.len();
        if end > width || used[start..end].iter().any(|&u| u) {
            continue;
        }
        let st = if Some(day_i) == peak_idx {
            peak_style
        } else {
            subtle
        };
        for (k, ch) in label.chars().enumerate() {
            buf[start + k] = ch;
            styles[start + k] = st;
            used[start + k] = true;
        }
    }

    Line::from(spans_from_styled_buf(&buf, &styles))
}

/// Right-column token-share list (`name  ████  99%  1.19M`).
/// Shared by month Models mix and all-time Top models.
fn draw_token_share_panel(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    title: impl Into<String>,
    rows: &[(String, u64)],
    empty_hint: &str,
) {
    let block = overview_right_block(theme, title);
    let border_style = theme.border();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Clear, inner);

    if rows.is_empty() || inner.width < 8 || inner.height == 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(empty_hint.to_string(), theme.subtle())),
            inner,
        );
        restore_right_border(frame, area, border_style);
        return;
    }

    let n = (inner.height as usize).min(rows.len()).min(8);
    let lines = token_share_lines(rows, inner.width as usize, n, theme);
    frame.render_widget(Paragraph::new(lines), inner);
    restore_right_border(frame, area, border_style);
}

/// All-time model token ranking — same single-line recipe as month Models mix.
fn draw_overview_top_models(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut rows: Vec<(String, u64)> = app
        .overview_summary
        .as_ref()
        .map(|s| {
            s.by_model
                .iter()
                .filter(|m| m.total_tokens > 0)
                .map(|m| (m.label.clone(), m.total_tokens))
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    draw_token_share_panel(
        frame,
        area,
        theme,
        "Top models  ·  tokens  ·  all-time",
        &rows,
        "  no model tokens",
    );
}

/// Provider health strip: success bar + TTFB + token volume (all-time).
///
/// Layout (adaptive width):
/// ```text
///  name            kind        ████████  99%   42ms   3.4M
/// ```
fn draw_overview_provider_health(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = panel_block(
        theme,
        "Provider health  ·  all-time  ·  tokens  ·  5 Usage → by provider",
        false,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut providers: Vec<&crate::dto::UsageProviderEntry> = app
        .overview_summary
        .as_ref()
        .map(|s| s.by_provider.iter().collect())
        .unwrap_or_default();

    if providers.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  tip  ", theme.accent_bold()),
                Span::styled(
                    "2 add provider · 3 route · 4 key · 5 usage  ·  ? help  ·  T theme",
                    theme.muted(),
                ),
            ])),
            inner,
        );
        return;
    }

    // Health-first: worst success, then slowest TTFB, then most tokens.
    providers.sort_by(|a, b| {
        provider_health_cmp(
            a.success_rate,
            a.avg_ttfb_ms,
            a.total_tokens,
            b.success_rate,
            b.avg_ttfb_ms,
            b.total_tokens,
        )
    });

    let cols = ProviderHealthCols::for_width(inner.width as usize);
    let show_header = inner.height >= 3 && cols.width >= 48;
    let body_h = if show_header {
        inner.height.saturating_sub(1)
    } else {
        inner.height
    };
    let n = (body_h as usize).max(1).min(providers.len()).min(8);
    let muted = theme.muted();

    let mut lines: Vec<Line> = Vec::with_capacity(n + usize::from(show_header));
    if show_header {
        lines.push(cols.header_line(muted));
    }
    for p in providers.iter().take(n) {
        lines.push(cols.row_line(theme, app, p, muted));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Adaptive column widths for the provider health strip.
struct ProviderHealthCols {
    width: usize,
    name_w: usize,
    kind_w: usize,
    bar_w: usize,
    pct_w: usize,
    ttfb_w: usize,
    tok_w: usize,
    tps_w: usize,
}

impl ProviderHealthCols {
    fn for_width(width: usize) -> Self {
        // " name……  kind………  ████████████  100%  5.5s  1.19M"
        let kind_w = if width >= 90 {
            14
        } else if width >= 70 {
            12
        } else {
            10
        };
        let pct_w = 5usize;
        let ttfb_w = 7usize;
        let tok_w = 8usize;
        let tps_w = 7usize;
        // 1 leading + 6 inter-column spaces
        let fixed = kind_w + pct_w + ttfb_w + tok_w + tps_w + 7;
        let flex = width.saturating_sub(fixed).max(16);
        let (name_w, bar_w) = split_name_bar(flex, 45, 12, 8);
        Self {
            width,
            name_w,
            kind_w,
            bar_w,
            pct_w,
            ttfb_w,
            tok_w,
            tps_w,
        }
    }

    fn header_line(&self, muted: Style) -> Line<'static> {
        spaced_row(vec![
            padded_cell("name", self.name_w, muted),
            padded_cell("kind", self.kind_w, muted),
            padded_cell("success", self.bar_w + 1 + self.pct_w, muted),
            padded_cell("ttfb", self.ttfb_w, muted),
            padded_cell("tokens", self.tok_w, muted),
            padded_cell("tok/s", self.tps_w, muted),
        ])
    }

    fn row_line(
        &self,
        theme: &Theme,
        app: &App,
        p: &crate::dto::UsageProviderEntry,
        muted: Style,
    ) -> Line<'static> {
        let name = resolve_provider_name(&app.providers, &p.provider_id, &p.name, p.deleted);
        let kind = or_em_dash(&p.provider_kind);
        let rate = p.success_rate.clamp(0.0, 1.0);
        let rate_style = success_rate_style(theme, rate);
        let pct = format!("{:>3.0}%", rate * 100.0);

        // Name may be multi-span (deleted/strike); keep it glued, then metric cells.
        let mut spans = vec![Span::raw(" ")];
        spans.extend(name_spans(theme, &name, false, Some(self.name_w)));
        for cell in [
            padded_cell(&kind, self.kind_w, theme.subtle()),
            Span::styled(ratio_bar(rate, self.bar_w), rate_style),
            padded_cell(&pct, self.pct_w, rate_style),
            padded_cell(&format_latency_ms(p.avg_ttfb_ms), self.ttfb_w, muted),
            padded_cell(
                &format_tokens(p.total_tokens),
                self.tok_w,
                theme.warning(),
            ),
            padded_cell(&format_tps_compact(p.tokens_per_sec), self.tps_w, muted),
        ] {
            spans.push(Span::raw(" "));
            spans.push(cell);
        }
        Line::from(spans)
    }
}


