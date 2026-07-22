//! Ratatui drawing — product-grade shell, master/detail, charts.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};
use ratatui::Frame;

use super::app::{App, Mode, PricingPane, Tab, UsageDetail};
use super::forms::{ConfirmAction, ProviderFormKind};
use super::theme::Theme;
use super::widgets::{
    build_contribution_weeks, detail_kv, display_width, empty_state, fill_bg, format_local_time,
    format_local_time_short, format_tokens, format_usd, health_badge, heat_level, keybind_line,
    modal, month_day_spends, pad_display, panel_block, panel_block_borders, ratio_bar, spinner,
    truncate, ContributionCell, DaySpend,
};

/// Stateful table render so the selected row stays in the visible viewport.
///
/// Each frame starts with offset 0; ratatui's [`TableState`] scrolls just far
/// enough for `selected` to appear (same behaviour as keeping a persistent
/// offset, without needing App-level scroll state).
fn render_scrollable_table(frame: &mut Frame, table: Table, area: Rect, selected: usize) {
    let mut state = TableState::default().with_selected(Some(selected));
    frame.render_stateful_widget(table, area, &mut state);
}

/// First visible index so `selected` stays in a window of `visible` rows.
/// Selection sits at the bottom of the window when past the first page
/// (matches Table's ephemeral-offset behaviour).
fn list_window_start(selected: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len == 0 {
        return 0;
    }
    let visible = visible.min(len);
    let max_start = len.saturating_sub(visible);
    let start = selected.saturating_sub(visible.saturating_sub(1));
    start.min(max_start)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    fill_bg(frame, frame.area(), &app.theme);

    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // brand line
            Constraint::Length(3), // tabs
            Constraint::Min(6),    // body
            Constraint::Length(1), // filter / status
            Constraint::Length(1), // keybinds
        ])
        .split(frame.area());

    draw_brand(frame, root[0], app);
    draw_tabs(frame, root[1], app);
    draw_body(frame, root[2], app);
    draw_status_line(frame, root[3], app);
    draw_keybind_footer(frame, root[4], app);

    // Overlays — re-borrow theme after body may have written scroll bounds.
    let mut help_bounds: Option<(u16, u16)> = None;
    {
        let theme = &app.theme;
        match &app.mode {
            Mode::Browse | Mode::Filter => {}
            Mode::Help => {
                help_bounds = Some(draw_help(frame, theme, app.help_scroll));
            }
            Mode::Confirm(a) => draw_confirm(frame, theme, a),
            Mode::Alert { title, body } => {
                modal(
                    frame,
                    theme,
                    title,
                    body.lines()
                        .map(|l| Line::from(l.to_string()))
                        .collect(),
                    theme.border_active(),
                );
            }
            Mode::SecretReveal {
                title,
                secret,
                single_value,
            } => {
                let mut lines: Vec<Line> = secret
                    .lines()
                    .map(|l| {
                        Line::from(Span::styled(
                            l.to_string(),
                            Style::default().fg(theme.warning),
                        ))
                    })
                    .collect();
                if lines.is_empty() {
                    lines.push(Line::from(Span::styled("(empty)", theme.subtle())));
                }
                lines.push(Line::from(""));
                // A lone token copies the same either way — drop the `a` hint.
                let footer = if *single_value {
                    "y/c copy · Enter/Esc close"
                } else {
                    "y/c copy full · a copy token/key · Enter/Esc close"
                };
                lines.push(Line::from(Span::styled(footer, theme.muted())));
                modal(frame, theme, title, lines, Style::default().fg(theme.warning));
            }
            Mode::ProviderAddChooser(c) => draw_provider_add_chooser(frame, theme, c),
            Mode::ProviderForm(f) => draw_provider_form(frame, theme, f),
            Mode::KeyForm(f) => draw_key_form(frame, theme, f),
            Mode::RouteWizard(w) => draw_route_wizard(frame, theme, w),
            Mode::OauthFlow(f) => draw_oauth_flow(frame, theme, f),
            Mode::PricingOverrideForm(f) => draw_pricing_override_form(frame, theme, f),
        }
    }
    if let Some((scroll, max)) = help_bounds {
        app.help_scroll = scroll;
        app.help_scroll_max = max;
    }
}

fn draw_brand(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let ok = app.daemon_ok();
    let badge = if app.health.is_none() && app.loading {
        Span::styled(
            format!(" {} connecting ", spinner(app.tick_frame)),
            theme.badge_warn(),
        )
    } else if ok {
        health_badge(theme, true, "online")
    } else {
        health_badge(theme, false, "offline")
    };

    let ver = app
        .health
        .as_ref()
        .map(|h| h.version.clone())
        .unwrap_or_else(|| "—".into());

    let line = Line::from(vec![
        Span::styled(" ◆ conduit ", theme.accent_bold()),
        Span::styled("ctl ", theme.muted()),
        badge,
        Span::styled(format!("  v{ver}  "), theme.subtle()),
        Span::styled("│ ", theme.subtle()),
        Span::styled(truncate(&app.console_addr, 40), theme.muted()),
        Span::raw("  "),
        Span::styled(
            if app.loading {
                format!("{} syncing", spinner(app.tick_frame))
            } else {
                String::new()
            },
            theme.accent_bold(),
        ),
    ]);
    frame.render_widget(Paragraph::new(line).style(theme.base()), area);
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let narrow = area.width < 72;
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let label = if narrow {
                format!(" {} {} ", i + 1, t.short())
            } else {
                format!(" {} {} ", i + 1, t.title())
            };
            let active = t.index() == app.tab.index();
            Line::from(Span::styled(
                label,
                if active {
                    theme.accent_bold()
                } else {
                    theme.muted()
                },
            ))
        })
        .collect();

    let tabs = Tabs::new(titles)
        .select(app.tab.index())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border())
                .style(theme.surface()),
        )
        .highlight_style(theme.accent_bold())
        .divider(Span::styled("│", theme.subtle()));
    frame.render_widget(tabs, area);
}

fn draw_status_line(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let mut spans = Vec::new();

    if matches!(app.mode, Mode::Filter) {
        spans.push(Span::styled(" filter ", theme.badge_warn()));
        spans.push(Span::styled(
            format!(" /{}_ ", app.filter),
            theme.accent_bold(),
        ));
        spans.push(Span::styled(" Enter apply · Esc leave ", theme.subtle()));
    } else if !app.filter.is_empty() {
        spans.push(Span::styled(
            format!(" /{}  ({} matches) ", app.filter, app.list_len()),
            theme.muted(),
        ));
    }

    if let Some(err) = &app.error {
        spans.push(Span::styled(
            format!(" {} ", truncate(err, 60)),
            theme.error(),
        ));
    } else if !app.status.is_empty() {
        spans.push(Span::styled(format!(" {} ", app.status), theme.muted()));
    }

    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(theme.base()),
        area,
    );
}

fn draw_keybind_footer(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let binds = app.context_keybinds();
    // Single-row footer: fit chips to terminal width; overflow becomes "… +N".
    // Full map is always on `?` (Keyboard reference).
    frame.render_widget(
        Paragraph::new(keybind_line(theme, &binds, area.width as usize)).style(theme.base()),
        area,
    );
}

fn draw_body(frame: &mut Frame, area: Rect, app: &mut App) {
    match app.tab {
        Tab::Overview => draw_overview(frame, area, app),
        Tab::Providers => draw_master_detail_providers(frame, area, app),
        Tab::Routes => draw_master_detail_routes(frame, area, app),
        Tab::Keys => draw_master_detail_keys(frame, area, app),
        Tab::Usage => draw_usage(frame, area, app),
        Tab::Pricing => draw_pricing(frame, area, app),
    }
}

// ── Overview dashboard ──────────────────────────────────────────────────────

fn draw_overview(frame: &mut Frame, area: Rect, app: &App) {
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
    let (cost, tokens, period_label, success, ttfb, latency, speed, tok_style, spend_style, ok_style) =
        match app.overview_summary.as_ref() {
            Some(s) => {
                let success = format!("{:.0}%", s.success_rate * 100.0);
                let ttfb = s
                    .avg_ttfb_ms
                    .map(|ms| format!("{ms:.0}ms"))
                    .unwrap_or_else(|| "—".into());
                let latency = s
                    .avg_duration_ms
                    .map(|ms| {
                        if ms >= 1000.0 {
                            format!("{:.1}s", ms / 1000.0)
                        } else {
                            format!("{ms:.0}ms")
                        }
                    })
                    .unwrap_or_else(|| "—".into());
                let speed = super::widgets::format_tok_per_sec(s.tokens_per_sec);
                let ok_style = if s.success_rate >= 0.99 {
                    theme.success()
                } else if s.success_rate >= 0.95 {
                    theme.warning()
                } else {
                    theme.error()
                };
                let period_label = if s.period.eq_ignore_ascii_case("all") {
                    "all-time".into()
                } else {
                    s.period.clone()
                };
                (
                    format_usd(s.total_usd),
                    format_tokens(summary_total_tokens(s)),
                    period_label,
                    success,
                    ttfb,
                    latency,
                    speed,
                    theme.warning(),
                    theme.success(),
                    ok_style,
                )
            }
            None => (
                "…".into(),
                "…".into(),
                "all-time".into(),
                "…".into(),
                "…".into(),
                "…".into(),
                "…".into(),
                theme.muted(),
                theme.muted(),
                theme.muted(),
            ),
        };

    // Tokens first among usage KPIs (lifetime); $ / latency stay secondary.
    super::widgets::metric_strip(
        frame,
        area,
        theme,
        &[
            (
                "HEALTH",
                health,
                if app.daemon_ok() {
                    theme.success()
                } else {
                    theme.error()
                },
            ),
            (
                "TOKENS",
                format!("{tokens} · {period_label}"),
                tok_style,
            ),
            ("SPEND", cost, spend_style),
            ("SUCCESS", success, ok_style),
            ("TTFB", ttfb, Style::default().fg(theme.chart[4])),
            ("LAT", latency, Style::default().fg(theme.chart[2])),
            ("SPEED", speed, Style::default().fg(theme.chart[0])),
        ],
    );
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
    let period = super::forms::current_period();
    let model_mix = month_model_token_mix(app, &period);

    draw_month_token_chart(frame, chart_area, app, &period);
    if model_mix.is_empty() {
        // Keep a right panel so the split still mirrors the row above.
        let block = overview_right_block(theme, format!("Models  {period}  ·  no traffic"));
        frame.render_widget(block, mix_area);
    } else {
        draw_month_model_mix(frame, mix_area, &model_mix, &period, theme);
    }
}

fn draw_month_token_chart(frame: &mut Frame, area: Rect, app: &App, period: &str) {
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
    let month_tok: u64 = days.iter().map(|d| d.total_tokens).sum();
    let active_n = days
        .iter()
        .filter(|d| d.total_tokens > 0 || d.request_count > 0)
        .count();
    let peak_day = days
        .iter()
        .max_by(|a, b| a.total_tokens.cmp(&b.total_tokens));
    let peak_tok = peak_day.map(|d| d.total_tokens).unwrap_or(0);
    let peak_idx = peak_day.and_then(|p| days.iter().position(|d| d.date == p.date));
    let peak_share = if month_tok > 0 {
        peak_tok as f64 / month_tok as f64 * 100.0
    } else {
        0.0
    };
    let avg_active = if active_n > 0 {
        month_tok / active_n as u64
    } else {
        0
    };

    if active_n == 0 {
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

    let header_w = chunks[0].width as usize;
    let line1 = format!(
        "  tok {}  active {active_n}/{}  avg/active {}",
        format_tokens(month_tok),
        days.len(),
        format_tokens(avg_active),
    );
    let mut header_lines = if display_width(&line1) <= header_w {
        vec![Line::from(vec![
            Span::styled("  tok ", theme.subtle()),
            Span::styled(format!("{}  ", format_tokens(month_tok)), theme.warning()),
            Span::styled(
                format!("active {active_n}/{}  ", days.len()),
                theme.accent_bold(),
            ),
            Span::styled(
                format!("avg/active {}", format_tokens(avg_active)),
                theme.subtle(),
            ),
        ])]
    } else {
        vec![Line::from(Span::styled(
            truncate(&line1, header_w),
            theme.subtle(),
        ))]
    };
    if show_callout {
        if let Some(p) = peak_day {
            let dom = p.date.get(8..10).unwrap_or("??");
            let req = if p.request_count > 0 {
                format!("  · {} req", p.request_count)
            } else {
                String::new()
            };
            let line2 = format!(
                "  peak day {dom}  {}  {peak_share:.0}% of month{req}",
                format_tokens(p.total_tokens),
            );
            if display_width(&line2) <= header_w {
                header_lines.push(Line::from(vec![
                    Span::styled("  peak ", theme.subtle()),
                    Span::styled(format!("day {dom}"), theme.accent_bold()),
                    Span::styled(
                        format!("  {}  ", format_tokens(p.total_tokens)),
                        theme.muted(),
                    ),
                    Span::styled(
                        format!("{peak_share:.0}% of month"),
                        if peak_share >= 80.0 {
                            theme.warning()
                        } else {
                            theme.subtle()
                        },
                    ),
                    Span::styled(req, theme.subtle()),
                ]));
            } else {
                header_lines.push(Line::from(Span::styled(
                    truncate(&line2, header_w),
                    theme.muted(),
                )));
            }
        }
    }
    frame.render_widget(Paragraph::new(header_lines), chunks[0]);

    let n = days.len().max(1);
    // Keep a right gutter so baseline glyphs never sit on the border cell.
    let chart_w = chunks[1].width as usize;
    let label_w = 2usize;
    let col_w = chart_w.saturating_sub(label_w + 1).max(1);
    let toks: Vec<f64> = days.iter().map(|d| d.total_tokens as f64).collect();
    let model_mix = month_model_token_mix(app, period);
    let stacks = month_day_model_stacks(app, period, &days, &model_mix, theme);

    draw_day_bar_panel(
        frame,
        chunks[1],
        &toks,
        n,
        col_w,
        "T",
        theme.chart_color(1),
        theme,
        peak_idx,
        &stacks,
    );

    if chunks[2].height > 0 {
        let axis_w = (chunks[2].width as usize)
            .saturating_sub(label_w + 1)
            .max(1);
        let axis = day_axis_line(n, axis_w.min(col_w), theme, peak_idx);
        let mut spans = vec![Span::raw("  ")];
        spans.extend(axis.spans);
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(theme.surface()),
            chunks[2],
        );
    }

    restore_right_border(frame, area, border_style);
}

/// Right-hand model token share for the calendar month.
fn draw_month_model_mix(
    frame: &mut Frame,
    area: Rect,
    mix: &[(String, u64)],
    period: &str,
    theme: &Theme,
) {
    let block = overview_right_block(theme, format!("Models  ·  {period}  ·  tokens"));
    let border_style = theme.border();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Clear, inner);

    if mix.is_empty() || inner.width < 8 || inner.height == 0 {
        frame.render_widget(
            Paragraph::new(Span::styled("  no model split", theme.subtle())),
            inner,
        );
        restore_right_border(frame, area, border_style);
        return;
    }

    let total: u64 = mix.iter().map(|(_, t)| *t).sum::<u64>().max(1);
    let n = (inner.height as usize).min(mix.len()).min(8);
    let w = inner.width as usize;

    // Pre-size trailing columns so every row aligns; bar fills remaining width.
    // Layout: " name……  ████████████  100%  1.19M"
    let pct_w = mix
        .iter()
        .take(n)
        .map(|(_, tok)| {
            let pct = *tok as f64 / total as f64 * 100.0;
            display_width(&format_model_pct(pct))
        })
        .max()
        .unwrap_or(4)
        .max(4);
    let tok_w = mix
        .iter()
        .take(n)
        .map(|(_, tok)| display_width(&format_tokens(*tok)))
        .max()
        .unwrap_or(4)
        .max(4);
    // leading space + gaps around name/bar/pct/tok
    let gaps = 4usize;
    let flex = w.saturating_sub(pct_w + tok_w + gaps).max(10);
    // ~40% name, rest bar — no max clamp so wide panes use full width.
    let name_w = (flex * 40 / 100).clamp(8, flex.saturating_sub(6));
    let bar_w = flex.saturating_sub(name_w);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(n);
    for (i, (name, tok)) in mix.iter().take(n).enumerate() {
        let pct = *tok as f64 / total as f64 * 100.0;
        let pct_s = format_model_pct(pct);
        let tok_s = format_tokens(*tok);
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
            Span::styled(pad_display(&pct_s, pct_w), theme.muted()),
            Span::raw(" "),
            Span::styled(pad_display(&tok_s, tok_w), theme.warning()),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);
    restore_right_border(frame, area, border_style);
}

fn format_model_pct(pct: f64) -> String {
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

/// Aggregate model token totals for `YYYY-MM` from `by_day_model`.
fn month_model_token_mix(app: &App, period: &str) -> Vec<(String, u64)> {
    let Some(s) = app.overview_summary.as_ref() else {
        return Vec::new();
    };
    let mut map: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for m in &s.by_day_model {
        if m.day.starts_with(period) && m.total_tokens > 0 {
            *map.entry(m.label.clone()).or_default() += m.total_tokens;
        }
    }
    // Fallback: when day×model rows are missing, use all-time by_model.
    if map.is_empty() {
        for m in &s.by_model {
            if m.total_tokens > 0 {
                *map.entry(m.label.clone()).or_default() += m.total_tokens;
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
    app: &App,
    period: &str,
    days: &[DaySpend],
    model_mix: &[(String, u64)],
    theme: &Theme,
) -> Vec<Vec<(ratatui::style::Color, f64)>> {
    if model_mix.is_empty() || days.is_empty() {
        return days.iter().map(|_| Vec::new()).collect();
    }
    let Some(s) = app.overview_summary.as_ref() else {
        return days.iter().map(|_| Vec::new()).collect();
    };

    // day → label → tokens (period only, positive tokens).
    let mut by_day: std::collections::HashMap<String, std::collections::HashMap<String, u64>> =
        std::collections::HashMap::new();
    for m in &s.by_day_model {
        if m.day.starts_with(period) && m.total_tokens > 0 {
            *by_day
                .entry(m.day.clone())
                .or_default()
                .entry(m.label.clone())
                .or_default() += m.total_tokens;
        }
    }
    if by_day.is_empty() {
        // No per-day model rows — solid bars (mix may still come from by_model).
        return days.iter().map(|_| Vec::new()).collect();
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
            let rest = d
                .total_tokens
                .max(day_sum)
                .saturating_sub(used);
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
fn stack_color_at(
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

    let mut day_vals = vec![0.0_f64; n_days];
    for (i, &v) in values.iter().enumerate().take(n_days) {
        day_vals[i] = v;
    }

    let mut lines = Vec::with_capacity(height);
    let base_style = Style::default().fg(fg);
    let quiet = Style::default().fg(theme.subtle);

    for row in 0..height {
        let threshold = (height - row) as f64;
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut buf = vec![' '; width];
        let mut styles = vec![base_style; width];

        for day_i in 0..n_days {
            let (c0, c1) = day_col_range(day_i, n_days, width);
            let v = day_vals[day_i];
            let lv = if v <= 0.0 {
                0.0
            } else {
                ((v / max) * height as f64).clamp(0.5, height as f64)
            };
            let is_peak = peak_idx == Some(day_i);
            let segs = stacks.get(day_i).map(|s| s.as_slice()).unwrap_or(&[]);
            // Baseline uses ASCII `-` (not box-drawing `┈`) so ambiguous-width
            // terminals never advance two cells and punch through the right border.
            let ch = if lv <= 0.0 {
                if row + 1 == height {
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
            };
            let st = if lv <= 0.0 && row + 1 == height {
                quiet
            } else if ch == ' ' {
                base_style
            } else {
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
            };
            // Fill the day's slot; leave a 1-col gutter between days when
            // each day has ≥3 columns so bars read as separate columns.
            let bar_end = if c1 - c0 >= 3 { c1 - 1 } else { c1 };
            for x in c0..bar_end {
                buf[x] = ch;
                styles[x] = st;
            }
        }

        let mut run = String::new();
        let mut run_st = styles[0];
        let flush = |run: &mut String, st: Style, out: &mut Vec<Span<'static>>| {
            if !run.is_empty() {
                out.push(Span::styled(std::mem::take(run), st));
            }
        };
        for c in 0..width {
            if styles[c] != run_st {
                flush(&mut run, run_st, &mut spans);
                run_st = styles[c];
            }
            run.push(buf[c]);
        }
        flush(&mut run, run_st, &mut spans);
        lines.push(Line::from(spans));
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
    let mut buf = vec![' '; width];
    let mut used = vec![false; width];
    let mut peak_range: Option<(usize, usize)> = None;

    let step = if width >= n_days * 2 {
        5usize
    } else if width >= 20 {
        5
    } else {
        10
    };
    let mut ticks: Vec<usize> = (0..n_days)
        .filter(|i| *i == 0 || (i + 1) % step == 0)
        .collect();
    if n_days > 1 {
        let last = n_days - 1;
        if !ticks.contains(&last) {
            ticks.push(last);
        }
    }
    if let Some(p) = peak_idx {
        if p < n_days && !ticks.contains(&p) {
            ticks.push(p);
        }
    }
    ticks.sort_by(|a, b| {
        let rank = |i: usize| {
            if Some(i) == peak_idx {
                0
            } else if i + 1 == n_days {
                1
            } else if i == 0 {
                2
            } else {
                3
            }
        };
        rank(*a).cmp(&rank(*b)).then(a.cmp(b))
    });

    for day_i in ticks {
        let label = format!("{}", day_i + 1);
        let (c0, c1) = day_col_range(day_i, n_days, width);
        let slot = c1 - c0;
        // Center label in the day's slot when it fits.
        let start = if label.len() <= slot {
            c0 + (slot - label.len()) / 2
        } else {
            c0.min(width.saturating_sub(label.len()))
        };
        let end = start + label.len();
        if end > width || used[start..end].iter().any(|&u| u) {
            continue;
        }
        for (k, ch) in label.chars().enumerate() {
            buf[start + k] = ch;
            used[start + k] = true;
        }
        if Some(day_i) == peak_idx {
            peak_range = Some((start, end));
        }
    }

    if let Some((s, e)) = peak_range {
        let mut spans = Vec::new();
        if s > 0 {
            spans.push(Span::styled(
                buf[..s].iter().collect::<String>(),
                theme.subtle(),
            ));
        }
        spans.push(Span::styled(
            buf[s..e].iter().collect::<String>(),
            theme.accent_bold(),
        ));
        if e < width {
            spans.push(Span::styled(
                buf[e..].iter().collect::<String>(),
                theme.subtle(),
            ));
        }
        Line::from(spans)
    } else {
        Line::from(Span::styled(
            buf.into_iter().collect::<String>(),
            theme.subtle(),
        ))
    }
}

/// All-time model token ranking — same single-line recipe as month Models mix:
/// `name  ████████  99%  1.19M` filling the full panel width.
fn draw_overview_top_models(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = overview_right_block(theme, "Top models  ·  tokens  ·  all-time");
    let border_style = theme.border();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Clear, inner);

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

    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("  no model tokens", theme.subtle())),
            inner,
        );
        restore_right_border(frame, area, border_style);
        return;
    }

    let total: u64 = rows.iter().map(|(_, t)| *t).sum::<u64>().max(1);
    let n = (inner.height as usize).min(rows.len()).min(8);
    let w = inner.width as usize;

    let pct_w = rows
        .iter()
        .take(n)
        .map(|(_, tok)| display_width(&format_model_pct(*tok as f64 / total as f64 * 100.0)))
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
    let flex = w.saturating_sub(pct_w + tok_w + gaps).max(10);
    let name_w = (flex * 40 / 100).clamp(8, flex.saturating_sub(6));
    let bar_w = flex.saturating_sub(name_w);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(n);
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
            Span::styled(pad_display(&format_model_pct(pct), pct_w), theme.muted()),
            Span::raw(" "),
            Span::styled(
                pad_display(&format_tokens(*tok), tok_w),
                theme.warning(),
            ),
        ]));
    }

    frame.render_widget(Paragraph::new(lines), inner);
    restore_right_border(frame, area, border_style);
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

    // Health-first: worst success rate, then slowest TTFB, then most tokens
    // (so a low-traffic flaky provider still surfaces before a healthy giant).
    providers.sort_by(|a, b| {
        a.success_rate
            .partial_cmp(&b.success_rate)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let ta = a.avg_ttfb_ms.unwrap_or(f64::MAX);
                let tb = b.avg_ttfb_ms.unwrap_or(f64::MAX);
                tb.partial_cmp(&ta).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.total_tokens.cmp(&a.total_tokens))
    });

    let width = inner.width as usize;
    // Column recipe — fill the full panel width (no max clamp on name).
    // Leading space + gaps between the 6 fields.
    // " name……  kind………  ████████████  100%  5.5s  1.19M"
    let kind_w = if width >= 90 {
        14 // "claude-oauth"
    } else if width >= 70 {
        12
    } else {
        10
    };
    let pct_w = 5usize; // "100%"
    let ttfb_w = 7usize; // "1200ms" / "5.5s  "
    let tok_w = 8usize; // "12.34M "
    let tps_w = 7usize; // "12.34M " tok/s (compact, no unit suffix)
    // 1 leading + 6 inter-column spaces
    let gaps = 7usize;
    let fixed = kind_w + pct_w + ttfb_w + tok_w + tps_w + gaps;
    let flex = width.saturating_sub(fixed).max(16);
    // Give name room for "codex (user@email.com)"; leftover grows the success bar.
    // Never force a min bar wider than remaining flex (avoids overflow on narrow panes).
    let name_w = (flex * 45 / 100).clamp(12, flex.saturating_sub(8));
    let bar_w = flex.saturating_sub(name_w);

    let show_header = inner.height >= 3 && width >= 48;
    let body_h = if show_header {
        inner.height.saturating_sub(1)
    } else {
        inner.height
    };
    let n = (body_h as usize).max(1).min(providers.len()).min(8);

    let mut lines: Vec<Line> = Vec::with_capacity(n + 1);
    if show_header {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(pad_display("name", name_w), theme.muted()),
            Span::raw(" "),
            Span::styled(pad_display("kind", kind_w), theme.muted()),
            Span::raw(" "),
            Span::styled(pad_display("success", bar_w + 1 + pct_w), theme.muted()),
            Span::raw(" "),
            Span::styled(pad_display("ttfb", ttfb_w), theme.muted()),
            Span::raw(" "),
            Span::styled(pad_display("tokens", tok_w), theme.muted()),
            Span::raw(" "),
            Span::styled(pad_display("tok/s", tps_w), theme.muted()),
        ]));
    }

    for p in providers.iter().take(n) {
        let name = resolve_provider_name(
            &app.providers,
            &p.provider_id,
            &p.name,
            p.deleted,
        );
        let kind = if p.provider_kind.is_empty() {
            "—"
        } else {
            p.provider_kind.as_str()
        };
        let rate = p.success_rate.clamp(0.0, 1.0);
        let rate_style = if rate >= 0.99 {
            theme.success()
        } else if rate >= 0.95 {
            theme.warning()
        } else {
            theme.error()
        };
        let ttfb = p
            .avg_ttfb_ms
            .map(|ms| {
                if ms >= 1000.0 {
                    format!("{:.1}s", ms / 1000.0)
                } else {
                    format!("{ms:.0}ms")
                }
            })
            .unwrap_or_else(|| "—".into());
        let tok = format_tokens(p.total_tokens);
        let tps = match p.tokens_per_sec {
            None => "—".into(),
            Some(v) if v >= 1000.0 => format_tokens(v.round() as u64),
            Some(v) => format!("{v:.1}"),
        };
        let bar = ratio_bar(rate, bar_w);
        let pct = format!("{:>3.0}%", rate * 100.0);

        let mut name_line = vec![Span::raw(" ")];
        name_line.extend(name_spans(theme, &name, false, Some(name_w)));
        name_line.push(Span::raw(" "));
        name_line.push(Span::styled(
            pad_display(&truncate(kind, kind_w), kind_w),
            theme.subtle(),
        ));
        name_line.push(Span::raw(" "));
        name_line.push(Span::styled(bar, rate_style));
        name_line.push(Span::raw(" "));
        name_line.push(Span::styled(pad_display(&pct, pct_w), rate_style));
        name_line.push(Span::raw(" "));
        name_line.push(Span::styled(
            pad_display(&truncate(&ttfb, ttfb_w), ttfb_w),
            theme.muted(),
        ));
        name_line.push(Span::raw(" "));
        name_line.push(Span::styled(
            pad_display(&truncate(&tok, tok_w), tok_w),
            theme.warning(),
        ));
        name_line.push(Span::raw(" "));
        name_line.push(Span::styled(
            pad_display(&truncate(&tps, tps_w), tps_w),
            theme.muted(),
        ));
        lines.push(Line::from(name_line));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

// ── Master / detail lists ───────────────────────────────────────────────────

fn split_master_detail(area: Rect) -> (Rect, Rect) {
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);
    (parts[0], parts[1])
}

fn draw_master_detail_providers(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    if filtered.is_empty() {
        empty_state(
            frame,
            area,
            theme,
            "Providers",
            if app.filter.is_empty() {
                "press a — add via API key or OAuth (Claude / Codex / Grok)"
            } else {
                "no matches — Esc clear filter"
            },
        );
        return;
    }

    let (list_area, detail_area) = split_master_detail(area);
    let sel = app.selected[Tab::Providers.index()].min(filtered.len() - 1);

    // Fixed columns for kind / remaining / full ULID; NAME takes the rest so
    // OAuth labels like `codex (user@email.com)` are not hard-capped.
    let kind_w = 14usize;
    let rem_w = 18usize;
    let id_w = 26usize; // ULID length — same as Keys list
    let col_spacing = 3usize; // 4 columns → 3 gaps of column_spacing(1)
    let border = 2usize; // panel left+right
    let name_w = (list_area.width as usize)
        .saturating_sub(border + kind_w + rem_w + id_w + col_spacing)
        .max(16);

    let header = Row::new(vec!["NAME", "KIND", "REMAINING", "ID"]).style(theme.header_cell());
    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(view_i, &data_i)| {
            let p = &app.providers[data_i];
            let remaining = provider_remaining_cell(app, p);
            let row = Row::new(vec![
                truncate(&p.name, name_w),
                truncate(&p.kind, kind_w),
                truncate(&remaining, rem_w),
                truncate(&p.id, id_w),
            ]);
            if view_i == sel {
                row.style(theme.selection())
            } else {
                row.style(Style::default().fg(theme.fg))
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Min(name_w as u16),
            Constraint::Length(kind_w as u16),
            Constraint::Length(rem_w as u16),
            Constraint::Length(id_w as u16),
        ],
    )
    .header(header)
    .block(panel_block(
        theme,
        format!("Providers  {}/{}", sel + 1, filtered.len()),
        true,
    ))
    .column_spacing(1);
    render_scrollable_table(frame, table, list_area, sel);

    // Detail
    let p = &app.providers[filtered[sel]];
    let lines = detail_kv(
        theme,
        &[
            ("id", p.id.clone()),
            ("name", p.name.clone()),
            ("kind", p.kind.clone()),
            ("base_url", p.base_url.clone()),
            ("key_ref", p.upstream_key_ref.clone()),
            ("created", format_local_time(&p.created_at)),
            ("updated", format_local_time(&p.updated_at)),
        ],
    );
    let mut body = vec![
        Line::from(Span::styled(
            format!("  {}", p.name),
            theme.accent_bold(),
        )),
        Line::from(""),
    ];
    body.extend(lines);
    body.push(Line::from(""));

    // Remaining / cooldown block
    body.extend(provider_quota_detail_lines(theme, app, p));
    body.push(Line::from(""));

    // Decrypted secret (after `v`)
    body.extend(provider_secret_detail_lines(theme, app, p));
    body.push(Line::from(""));

    // Kind-pool membership: same-kind providers share a route pool target.
    let kind_n = app
        .providers
        .iter()
        .filter(|x| x.kind.eq_ignore_ascii_case(&p.kind))
        .count();
    body.push(Line::from(Span::styled("  Kind pool", theme.accent_bold())));
    body.push(Line::from(Span::styled(
        format!(
            "  {kind_n} account{} of kind «{}» — route wizard Ctrl-k → pool · {}",
            if kind_n == 1 { "" } else { "s" },
            p.kind,
            p.kind,
        ),
        theme.subtle(),
    )));
    body.push(Line::from(""));

    let oauth = super::forms::ProviderForm::is_oauth_kind_label(&p.kind);
    if oauth {
        body.push(Line::from(Span::styled(
            "  OAuth provider",
            theme.success(),
        )));
        body.push(Line::from(Span::styled(
            "  v secret · y copy full · Y copy token · u remaining · o re-auth · x ↻ · d del",
            theme.subtle(),
        )));
    } else {
        body.push(Line::from(Span::styled(
            "  v secret · y copy full · Y copy key · e edit · s set key · d delete",
            theme.subtle(),
        )));
    }
    frame.render_widget(
        Paragraph::new(body).block(panel_block(theme, "Detail", false)),
        detail_area,
    );
}

fn provider_remaining_cell(app: &App, p: &crate::dto::ProviderView) -> String {
    if let Some(cd) = app.cooldown_for(&p.id) {
        if cd.remaining_secs > 0 {
            return format!("❄ {}s", cd.remaining_secs);
        }
    }
    if let Some(q) = app.quota_for(&p.id) {
        if let Some(label) = q.remaining_label() {
            return label;
        }
    }
    if super::forms::ProviderForm::is_oauth_kind_label(&p.kind) {
        "…".into()
    } else {
        "—".into()
    }
}

fn provider_quota_detail_lines(
    theme: &super::theme::Theme,
    app: &App,
    p: &crate::dto::ProviderView,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(Line::from(Span::styled("  Remaining", theme.accent_bold())));

    if let Some(cd) = app.cooldown_for(&p.id) {
        if cd.remaining_secs > 0 {
            out.push(Line::from(vec![
                Span::styled(format!("  {:<16}", "cooldown"), theme.muted()),
                Span::styled(
                    format!("{}s ({})", cd.remaining_secs, cd.reason),
                    theme.warning(),
                ),
            ]));
        }
    }

    if let Some(q) = app.quota_for(&p.id) {
        if let Some(pct) = q.session_remaining_pct {
            let reset = q
                .session_resets_at
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| format!("  reset {}", format_local_time_short(s)))
                .unwrap_or_default();
            out.push(Line::from(vec![
                Span::styled(format!("  {:<16}", "session 5h"), theme.muted()),
                Span::styled(format!("{pct:.0}% left{reset}"), remaining_style(theme, pct)),
            ]));
        }
        if let Some(pct) = q.weekly_remaining_pct {
            let reset = q
                .weekly_resets_at
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| format!("  reset {}", format_local_time_short(s)))
                .unwrap_or_default();
            out.push(Line::from(vec![
                Span::styled(format!("  {:<16}", "weekly 7d"), theme.muted()),
                Span::styled(format!("{pct:.0}% left{reset}"), remaining_style(theme, pct)),
            ]));
        }
        if q.session_remaining_pct.is_none() && q.weekly_remaining_pct.is_none() {
            if let Some(label) = q.remaining_label() {
                out.push(Line::from(vec![
                    Span::styled(format!("  {:<16}", "rate limit"), theme.muted()),
                    Span::styled(label, Style::default().fg(theme.fg)),
                ]));
            } else {
                out.push(Line::from(Span::styled(
                    "  no remaining data yet",
                    theme.subtle(),
                )));
            }
        }
        if !q.source.is_empty() {
            out.push(Line::from(Span::styled(
                format!(
                    "  source {} · captured {}",
                    q.source,
                    format_local_time(&q.captured_at)
                ),
                theme.subtle(),
            )));
        }
        if let Some(acct) = q.details.get("account") {
            out.push(Line::from(Span::styled(
                format!("  account {acct}"),
                theme.subtle(),
            )));
        }
    } else if super::forms::ProviderForm::is_oauth_kind_label(&p.kind) {
        out.push(Line::from(Span::styled(
            "  press u to probe OAuth remaining (Claude 5h·7d, Codex 7d, Grok mo)",
            theme.subtle(),
        )));
    } else {
        out.push(Line::from(Span::styled(
            "  — (API-key providers show last-seen headers after traffic)",
            theme.subtle(),
        )));
    }
    out
}

fn remaining_style(theme: &super::theme::Theme, pct: f64) -> Style {
    if pct <= 10.0 {
        theme.error()
    } else if pct <= 25.0 {
        theme.warning()
    } else {
        theme.success()
    }
}

fn provider_secret_detail_lines(
    theme: &super::theme::Theme,
    app: &App,
    p: &crate::dto::ProviderView,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    out.push(Line::from(Span::styled("  Secret", theme.accent_bold())));

    let Some(sec) = app.secret_for(&p.id) else {
        out.push(Line::from(Span::styled(
            "  press v to decrypt & show API key / OAuth tokens",
            theme.subtle(),
        )));
        return out;
    };

    out.push(Line::from(vec![
        Span::styled(format!("  {:<16}", "kind"), theme.muted()),
        Span::styled(sec.secret_kind.clone(), theme.warning()),
    ]));
    out.push(Line::from(vec![
        Span::styled(format!("  {:<16}", "key_id"), theme.muted()),
        Span::styled(sec.key_id.clone(), Style::default().fg(theme.fg)),
    ]));

    if sec.secret_kind == "api_key" {
        let key = sec.api_key.as_deref().unwrap_or("");
        out.push(Line::from(vec![
            Span::styled(format!("  {:<16}", "api_key"), theme.muted()),
            Span::styled(key.to_string(), theme.warning()),
        ]));
        return out;
    }

    if let Some(o) = &sec.oauth {
        let push = |out: &mut Vec<Line<'static>>, k: &str, v: String| {
            if v.is_empty() {
                return;
            }
            out.push(Line::from(vec![
                Span::styled(format!("  {k:<16}"), theme.muted()),
                Span::styled(v, Style::default().fg(theme.fg)),
            ]));
        };
        let push_secret = |out: &mut Vec<Line<'static>>, k: &str, v: &str| {
            if v.is_empty() {
                return;
            }
            out.push(Line::from(vec![
                Span::styled(format!("  {k:<16}"), theme.muted()),
                Span::styled(v.to_string(), theme.warning()),
            ]));
        };
        push(&mut out, "type", o.provider_type.clone());
        push(&mut out, "auth_kind", o.auth_kind.clone());
        if let Some(e) = &o.email {
            push(&mut out, "email", e.clone());
        }
        if let Some(a) = &o.account_id {
            push(&mut out, "account_id", a.clone());
        }
        if let Some(pl) = &o.plan_type {
            push(&mut out, "plan", pl.clone());
        }
        if let Some(org) = &o.organization_name {
            push(&mut out, "org", org.clone());
        }
        if let Some(oid) = &o.organization_id {
            push(&mut out, "org_id", oid.clone());
        }
        if let Some(sub) = &o.sub {
            push(&mut out, "sub", sub.clone());
        }
        if let Some(exp) = &o.expired {
            push(&mut out, "expired", format_local_time(exp));
        }
        if let Some(lr) = &o.last_refresh {
            push(&mut out, "last_refresh", format_local_time(lr));
        }
        if let Some(bu) = &o.base_url {
            push(&mut out, "base_url", bu.clone());
        }
        if let Some(px) = &o.proxy_url {
            push(&mut out, "proxy_url", px.clone());
        }
        if let Some(ua) = o.using_api {
            push(&mut out, "using_api", ua.to_string());
        }
        // Always show cloak_mode for Claude OAuth (API defaults omitted → auto).
        if super::forms::ProviderForm::is_claude_oauth_kind(&p.kind)
            || o.provider_type.eq_ignore_ascii_case("claude")
        {
            let cloak = o
                .cloak_mode
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("auto");
            push(&mut out, "cloak_mode", cloak.to_string());
        }
        push_secret(&mut out, "access_token", &o.access_token);
        push_secret(&mut out, "refresh_token", &o.refresh_token);
        if let Some(id_tok) = &o.id_token {
            push_secret(&mut out, "id_token", id_tok);
        }
        if !o.extra.is_empty() {
            for (k, v) in &o.extra {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                // Skip huge blobs in detail; show short keys fully.
                let shown = if val.len() > 80 {
                    format!("{}…", truncate(&val, 72))
                } else {
                    val
                };
                out.push(Line::from(vec![
                    Span::styled(format!("  {k:<16}"), theme.muted()),
                    Span::styled(shown, theme.subtle()),
                ]));
            }
        }
    } else {
        out.push(Line::from(Span::styled(
            "  (oauth payload missing)",
            theme.subtle(),
        )));
    }
    out
}

fn draw_master_detail_routes(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    if filtered.is_empty() {
        empty_state(
            frame,
            area,
            theme,
            "Routes",
            if app.filter.is_empty() {
                "press a — multi-target wizard (single provider or kind pool)"
            } else {
                "no matches"
            },
        );
        return;
    }
    let (list_area, detail_area) = split_master_detail(area);
    let sel = app.selected[Tab::Routes.index()].min(filtered.len() - 1);

    // en + STRAT fixed; ALIAS/TARGETS split the rest (2:3) so long model
    // aliases and `pool:…→…` summaries use available width instead of the old
    // hard caps (22 / 40) that truncated even when the pane was wide.
    let en_w = 2usize;
    let strat_w = 11usize; // longest strategy label: `round_robin`
    let col_spacing = 3usize; // 4 columns → 3 gaps
    let border = 2usize;
    let flex = (list_area.width as usize)
        .saturating_sub(border + en_w + strat_w + col_spacing)
        .max(20);
    let alias_w = (flex * 2 / 5).max(16);
    let targets_w = flex.saturating_sub(alias_w).max(20);

    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(view_i, &data_i)| {
            let r = &app.routes[data_i];
            let en = if r.enabled { "●" } else { "○" };
            let summary =
                super::forms::summarize_route_targets(&r.targets_json, &app.providers);
            let row = Row::new(vec![
                en.into(),
                truncate(&r.match_alias, alias_w),
                truncate(&r.strategy, strat_w),
                truncate(&summary, targets_w),
            ]);
            if view_i == sel {
                row.style(theme.selection())
            } else {
                row.style(Style::default().fg(theme.fg))
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(en_w as u16),
            Constraint::Length(alias_w as u16),
            Constraint::Length(strat_w as u16),
            Constraint::Min(targets_w as u16),
        ],
    )
    .column_spacing(1)
    .header(
        Row::new(vec!["", "ALIAS", "STRAT", "TARGETS"]).style(theme.header_cell()),
    )
    .block(panel_block(
        theme,
        format!("Routes  {}/{}", sel + 1, filtered.len()),
        true,
    ));
    render_scrollable_table(frame, table, list_area, sel);

    let r = &app.routes[filtered[sel]];
    let targets_pretty = serde_json::from_str::<serde_json::Value>(&r.targets_json)
        .map(|v| serde_json::to_string_pretty(&v).unwrap_or_else(|_| r.targets_json.clone()))
        .unwrap_or_else(|_| r.targets_json.clone());
    let summary = super::forms::summarize_route_targets(&r.targets_json, &app.providers);

    let mut lines = detail_kv(
        theme,
        &[
            ("id", r.id.clone()),
            ("alias", r.match_alias.clone()),
            ("strategy", r.strategy.clone()),
            ("enabled", r.enabled.to_string()),
            ("summary", summary),
        ],
    );
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  targets  (pool_* expands to all accounts of that kind)",
        theme.muted(),
    )));
    for tl in targets_pretty.lines().take(12) {
        lines.push(Line::from(Span::styled(
            format!("  {tl}"),
            Style::default().fg(theme.fg),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  e edit · a add · d delete  ·  pool = multi-account of same kind",
        theme.subtle(),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel_block(theme, "Detail", false)),
        detail_area,
    );
}

fn draw_master_detail_keys(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    if filtered.is_empty() {
        empty_state(
            frame,
            area,
            theme,
            "Keys",
            "press a to create a downstream API key",
        );
        return;
    }
    let (list_area, detail_area) = split_master_detail(area);
    let sel = app.selected[Tab::Keys.index()].min(filtered.len() - 1);

    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(view_i, &data_i)| {
            let k = &app.keys[data_i];
            let en = if k.enabled { "●" } else { "○" };
            let rpm = k
                .rate_limit_rpm
                .map(|r| r.to_string())
                .unwrap_or_else(|| "—".into());
            let row = Row::new(vec![
                en.into(),
                truncate(&k.name, 24),
                rpm,
                truncate(&k.id, 26),
            ]);
            if view_i == sel {
                row.style(theme.selection())
            } else {
                row.style(Style::default().fg(theme.fg))
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(2),
            // A fixed 45% NAME left a big gap before RPM for short names and
            // truncated the ID. Make both flexible; ID gets the larger share so
            // the 26-char ULID shows in full when there's room.
            Constraint::Fill(1), // NAME
            // RPM holds a short number (or "—") + the "RPM" header — 6 is plenty.
            Constraint::Length(6),
            Constraint::Fill(2), // ID
        ],
    )
    .header(Row::new(vec!["", "NAME", "RPM", "ID"]).style(theme.header_cell()))
    .block(panel_block(
        theme,
        format!("Keys  {}/{}", sel + 1, filtered.len()),
        true,
    ));
    render_scrollable_table(frame, table, list_area, sel);

    let k = &app.keys[filtered[sel]];
    let mut lines = detail_kv(
        theme,
        &[
            ("id", k.id.clone()),
            ("name", k.name.clone()),
            ("enabled", k.enabled.to_string()),
            (
                "rpm",
                k.rate_limit_rpm
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "unlimited".into()),
            ),
            ("whitelist", k.model_whitelist.to_string()),
            ("created", format_local_time(&k.created_at)),
        ],
    );
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  v show token · e edit · d delete",
        theme.subtle(),
    )));
    frame.render_widget(
        Paragraph::new(lines).block(panel_block(theme, "Detail", false)),
        detail_area,
    );
}

// ── Usage dashboard ─────────────────────────────────────────────────────────

/// Period token total for KPI cards. Prefer top-level `total_tokens`; fall back
/// to day/entry sums for older daemons that omit the field.
fn summary_total_tokens(s: &crate::dto::UsageSummaryView) -> u64 {
    if s.total_tokens > 0 {
        s.total_tokens
    } else if !s.by_day.is_empty() {
        s.by_day.iter().map(|d| d.total_tokens).sum()
    } else {
        s.entries.iter().map(|e| e.total_tokens).sum()
    }
}

fn draw_usage(frame: &mut Frame, area: Rect, app: &mut App) {
    let theme = &app.theme;
    // Heatmap needs ~10 rows (legend + 7 weekdays + pad); give it room.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // metric cards
            Constraint::Length(12), // GitHub-style daily spend calendar
            Constraint::Min(5),
        ])
        .split(area);

    let (period, total_usd, tokens, success_pct, avg_ttfb) =
        if let Some(s) = &app.usage_summary {
            let ttfb = s
                .avg_ttfb_ms
                .map(|ms| format!("{ms:.0}ms"))
                .unwrap_or_else(|| "—".into());
            (
                s.period.clone(),
                s.total_usd,
                format_tokens(summary_total_tokens(s)),
                format!("{:.0}%", s.success_rate * 100.0),
                ttfb,
            )
        } else {
            (
                app.usage_period.clone(),
                0.0,
                "…".into(),
                "—".into(),
                "—".into(),
            )
        };

    super::widgets::metric_strip(
        frame,
        chunks[0],
        theme,
        &[
            ("PERIOD", period, theme.accent_bold()),
            ("COST", format_usd(total_usd), theme.success()),
            ("TOKENS", tokens, theme.warning()),
            ("SUCCESS", success_pct, theme.success()),
            ("TTFB", avg_ttfb, Style::default().fg(theme.chart[4])),
        ],
    );

    draw_usage_heatmap(frame, chunks[1], app);
    draw_usage_detail(frame, chunks[2], app);
}

fn day_tuples_from_summary(
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

fn trailing_tuples_from_summary(
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

fn usage_by_day_tuples(app: &App) -> Vec<(String, u64, u64, f64)> {
    app.usage_summary
        .as_ref()
        .map(day_tuples_from_summary)
        .unwrap_or_default()
}

/// Overview daily rows (`period=all` includes full history; month chart filters).
fn overview_by_day_tuples(app: &App) -> Vec<(String, u64, u64, f64)> {
    app.overview_summary
        .as_ref()
        .map(day_tuples_from_summary)
        .unwrap_or_default()
}

/// Trailing-year daily rows for the contribution graph (falls back to period).
fn usage_trailing_tuples(app: &App) -> Vec<(String, u64, u64, f64)> {
    app.usage_summary
        .as_ref()
        .map(trailing_tuples_from_summary)
        .unwrap_or_default()
}

fn overview_trailing_tuples(app: &App) -> Vec<(String, u64, u64, f64)> {
    app.overview_summary
        .as_ref()
        .map(trailing_tuples_from_summary)
        .unwrap_or_default()
}

fn today_ymd() -> String {
    let n = chrono::Local::now().date_naive();
    n.format("%Y-%m-%d").to_string()
}

/// Top-of-Usage tokscale/GitHub contribution graph (52 weeks).
fn draw_usage_heatmap(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let period = app
        .usage_summary
        .as_ref()
        .map(|s| s.period.clone())
        .unwrap_or_else(|| app.usage_period.clone());

    // Highlight the selected calendar day when browsing by-day.
    let selected_date = if app.usage_detail == UsageDetail::ByDay {
        let cal = month_day_spends(&period, &usage_by_day_tuples(app));
        let rows = usage_day_view_rows(app, &cal);
        let sel = app.selected[Tab::Usage.index()].min(rows.len().saturating_sub(1));
        rows.get(sel).map(|d| d.date.clone())
    } else {
        None
    };

    let block = panel_block(
        theme,
        format!("Contribution graph  ·  52 weeks  ·  tokens  ·  period cards: {period}"),
        true,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = usage_trailing_tuples(app);
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("  no daily data", theme.subtle())),
            inner,
        );
        return;
    }
    let idle = if app.usage_detail == UsageDetail::ByDay {
        String::new()
    } else {
        "t · by-day".into()
    };
    let lines = contribution_graph_lines(theme, &rows, selected_date.as_deref(), &idle, true);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// tokscale-style contribution graph lines.
///
/// ```text
///   Jul Aug Sep …
///   Mon ██ ·· ██ …
///   Wed …
///   Fri …
///   Less · ██ ██ ██ ██ More
/// ```
///
/// When `heat_by_tokens` is true, cell intensity follows daily tokens;
/// otherwise it follows daily spend.
fn contribution_graph_lines(
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

fn heat_level_from_intensity(intensity: f64) -> u8 {
    let r = if intensity.is_finite() {
        intensity.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if r <= 0.0 {
        0
    } else if r < 0.25 {
        1
    } else if r < 0.50 {
        2
    } else if r < 0.75 {
        3
    } else {
        4
    }
}

fn find_cell<'a>(
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

/// Shared list + right-hand detail split used by every Usage pane.
fn usage_master_detail(area: Rect) -> (Rect, Rect, bool) {
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if area.width >= 130 {
            // Wide: fixed detail so request_id / model / provider fit without
            // truncating as aggressively as the old 44-col pane.
            [Constraint::Min(70), Constraint::Length(56)]
        } else if area.width >= 100 {
            [Constraint::Percentage(62), Constraint::Percentage(38)]
        } else if area.width >= 90 {
            [Constraint::Percentage(65), Constraint::Percentage(35)]
        } else {
            [Constraint::Percentage(100), Constraint::Length(0)]
        })
        .split(area);
    let show = parts[1].width >= 28;
    (parts[0], parts[1], show)
}

fn draw_usage_detail(frame: &mut Frame, area: Rect, app: &mut App) {
    let sel = app.selected[Tab::Usage.index()];
    let period_total = app
        .usage_summary
        .as_ref()
        .map(|s| s.total_usd)
        .unwrap_or(0.0)
        .max(1e-12);
    let period_tokens = app
        .usage_summary
        .as_ref()
        .map(summary_total_tokens)
        .unwrap_or(0)
        .max(1);

    match app.usage_detail {
        UsageDetail::Recent => draw_usage_recent(frame, area, app, sel),
        UsageDetail::ByModel => {
            draw_usage_by_model(frame, area, app, sel, period_total, period_tokens)
        }
        UsageDetail::ByKey => {
            draw_usage_by_key(frame, area, app, sel, period_total, period_tokens)
        }
        UsageDetail::ByDay => draw_usage_by_day(frame, area, app, sel, period_total),
        UsageDetail::ByProvider => {
            draw_usage_by_provider(frame, area, app, sel, period_tokens)
        }
    }
}

fn draw_usage_recent(frame: &mut Frame, area: Rect, app: &mut App, sel: usize) {
    let title = {
        let total = app.usage_total;
        let from = if total == 0 {
            0
        } else {
            app.usage_offset.saturating_add(1)
        };
        let to = app
            .usage_offset
            .saturating_add(app.usage_recent.len())
            .min(total as usize);
        let page = if total == 0 {
            0
        } else {
            app.usage_offset / super::app::USAGE_PAGE_SIZE + 1
        };
        let pages = if total == 0 {
            0
        } else {
            (total as usize).div_ceil(super::app::USAGE_PAGE_SIZE)
        };
        format!(
            "Recent  {from}–{to}/{total} · p.{page}/{pages} · sort={}  (cR=cache read · cW=cache write)",
            app.usage_sort.label()
        )
    };
    if app.usage_recent.is_empty() {
        let hint = if !app.filter.is_empty() {
            "no matches for filter — Esc clear · / edit"
        } else {
            "no requests recorded"
        };
        empty_state(frame, area, &app.theme, &title, hint);
        app.usage_detail_scroll = 0;
        app.usage_detail_scroll_max = 0;
        return;
    }
    let (list_area, detail_area, show_detail) = usage_master_detail(area);

    let inner_w = list_area.width.saturating_sub(4) as usize;
    let fixed = 8 + 6 + 6 + 10 + 4;
    let flex = inner_w.saturating_sub(fixed);
    let ts_w = if flex >= 56 {
        30usize
    } else if flex >= 44 {
        25
    } else if flex >= 34 {
        20
    } else {
        16
    }
    .min(flex.saturating_sub(12).max(12));
    let model_w = flex.saturating_sub(ts_w).max(12);

    // Server already sorted/paginated — display in API order.
    let sel = sel.min(app.usage_recent.len().saturating_sub(1));

    // List paint — theme borrow ends before we write scroll bounds back.
    {
        let theme = &app.theme;
        let rows: Vec<Row> = app
            .usage_recent
            .iter()
            .enumerate()
            .map(|(view_i, u)| {
                let row = Row::new(vec![
                    Cell::from(truncate(&format_local_time(&u.ts), ts_w)),
                    Cell::from(truncate(u.model_id.as_deref().unwrap_or(""), model_w)),
                    Cell::from(u.total_tokens.to_string()),
                    Cell::from(fmt_tok(u.cache_read_tokens)),
                    Cell::from(fmt_tok(u.cache_write_tokens)),
                    Cell::from(format_usd(u.cost_usd)),
                ]);
                if view_i == sel {
                    row.style(theme.selection())
                } else {
                    row
                }
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(ts_w as u16),
                Constraint::Min(model_w as u16),
                Constraint::Length(8),
                Constraint::Length(6),
                Constraint::Length(6),
                Constraint::Length(10),
            ],
        )
        .header(
            Row::new(vec!["TS", "MODEL", "TOK", "cR", "cW", "COST"]).style(theme.header_cell()),
        )
        .block(panel_block(theme, title, true));
        render_scrollable_table(frame, table, list_area, sel);
    }

    if show_detail {
        if sel >= app.usage_recent.len() {
            app.usage_detail_scroll = 0;
            app.usage_detail_scroll_max = 0;
        } else {
            let scroll_in = app.usage_detail_scroll;
            let (scroll, max_scroll) = draw_usage_record_detail(
                frame,
                detail_area,
                &app.theme,
                &app.usage_recent[sel],
                &app.pricing,
                scroll_in,
            );
            // Publish bounds so Ctrl+j stops at the bottom (no wrap).
            app.usage_detail_scroll = scroll;
            app.usage_detail_scroll_max = max_scroll;
        }
    } else {
        app.usage_detail_scroll = 0;
        app.usage_detail_scroll_max = 0;
    }
}

fn draw_usage_by_model(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sel: usize,
    period_total: f64,
    period_tokens: u64,
) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    let mut items: Vec<(usize, String, String, f64, u64, u64, Option<f64>)> = app
        .usage_summary
        .as_ref()
        .map(|s| {
            filtered
                .iter()
                .filter_map(|&i| {
                    s.by_model.get(i).map(|m| {
                        (
                            i,
                            m.label.clone(),
                            m.provider_kind.clone(),
                            m.total_usd,
                            m.total_tokens,
                            m.request_count,
                            m.tokens_per_sec,
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        let hint = if !app.filter.is_empty() {
            "no matches for filter"
        } else {
            "no rollup data"
        };
        empty_state(frame, area, theme, "By model", hint);
        return;
    }
    sort_usage_items(&mut items, app.usage_sort);
    let sel = sel.min(items.len().saturating_sub(1));
    let (list_area, detail_area, show_detail) = usage_master_detail(area);
    let max_tok = items.iter().map(|x| x.4).max().unwrap_or(1).max(1) as f64;
    let max_cost = items
        .iter()
        .map(|x| x.3)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    // Token-first list: reserve room for bar + tokens + cost + reqs.
    let label_w = (list_area.width as usize)
        .saturating_sub(36)
        .clamp(10, 32);
    let bar_w = (list_area.width as usize)
        .saturating_sub(label_w + 28)
        .clamp(6, 20);

    let bar_by_cost = matches!(app.usage_sort, super::app::UsageSort::Cost);
    let block = panel_block(
        theme,
        format!("By model  ·  tokens  ·  sort={}", app.usage_sort.label()),
        true,
    );
    let inner = block.inner(list_area);
    let visible = inner.height as usize;
    let start = list_window_start(sel, items.len(), visible);
    let end = (start + visible).min(items.len());
    let lines: Vec<Line> = items[start..end]
        .iter()
        .enumerate()
        .map(|(off, (_, label, _, cost, tok, req, _tps))| {
            let i = start + off;
            let mark = if i == sel { "▶ " } else { "  " };
            let ratio = if bar_by_cost {
                *cost / max_cost
            } else {
                *tok as f64 / max_tok
            };
            Line::from(vec![
                Span::styled(
                    format!("{mark}{}", pad_display(label, label_w)),
                    if i == sel {
                        theme.accent_bold()
                    } else {
                        Style::default().fg(theme.fg)
                    },
                ),
                Span::styled(
                    ratio_bar(ratio, bar_w),
                    Style::default().fg(theme.chart_color(i)),
                ),
                Span::styled(
                    format!(" {:>6}", format_tokens(*tok)),
                    theme.warning(),
                ),
                Span::styled(
                    format!("  {}  {}r", format_usd(*cost), req),
                    theme.muted(),
                ),
            ])
        })
        .collect();
    frame.render_widget(block, list_area);
    frame.render_widget(Paragraph::new(lines), inner);

    if show_detail {
        let (label, provider, cost, tok, req, tps) = (
            &items[sel].1,
            &items[sel].2,
            items[sel].3,
            items[sel].4,
            items[sel].5,
            items[sel].6,
        );
        let cost_share = (cost / period_total) * 100.0;
        let tok_share = tok as f64 / period_tokens as f64 * 100.0;
        draw_usage_rollup_detail(
            frame,
            detail_area,
            theme,
            "Model detail",
            label,
            &[
                ("label", label.clone()),
                (
                    "provider",
                    if provider.is_empty() {
                        "—".into()
                    } else {
                        provider.clone()
                    },
                ),
                ("tokens", format_tokens(tok)),
                ("tok share", format!("{tok_share:.1}% of period")),
                ("tok/s", super::widgets::format_tok_per_sec(tps)),
                ("requests", req.to_string()),
                ("cost", format_usd(cost)),
                ("$ share", format!("{cost_share:.1}% of period")),
            ],
            tok as f64 / max_tok,
            &[], // model pane is already the list
        );
    }
}

fn draw_usage_by_key(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sel: usize,
    period_total: f64,
    period_tokens: u64,
) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    // (ResolvedName, key_id, cost, tokens, reqs, prompt, completion)
    let mut items: Vec<(ResolvedName, String, f64, u64, u64, u64, u64)> = app
        .usage_summary
        .as_ref()
        .map(|s| {
            filtered
                .iter()
                .filter_map(|&i| {
                    s.entries.get(i).map(|e| {
                        let id = e.downstream_key_id.clone();
                        let name = resolve_key_name(&app.keys, &id, &e.name, e.deleted);
                        (
                            name,
                            id,
                            e.total_usd,
                            e.total_tokens,
                            e.request_count,
                            e.prompt_tokens,
                            e.completion_tokens,
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        let hint = if !app.filter.is_empty() {
            "no matches for filter"
        } else {
            "no rollup data"
        };
        empty_state(frame, area, theme, "By key", hint);
        return;
    }
    match app.usage_sort {
        super::app::UsageSort::Cost => {
            items.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
        }
        super::app::UsageSort::Tokens => items.sort_by(|a, b| b.3.cmp(&a.3)),
        super::app::UsageSort::Date => {
            // Keys have no timestamp; fall back to name.
            items.sort_by(|a, b| a.0.text.cmp(&b.0.text))
        }
    }
    let sel = sel.min(items.len().saturating_sub(1));
    let (list_area, detail_area, show_detail) = usage_master_detail(area);
    let max_tok = items.iter().map(|x| x.3).max().unwrap_or(1).max(1) as f64;
    let max_cost = items
        .iter()
        .map(|x| x.2)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let label_w = (list_area.width as usize)
        .saturating_sub(36)
        .max(12);
    let bar_w = (list_area.width as usize)
        .saturating_sub(label_w + 28)
        .clamp(6, 20);

    let bar_by_cost = matches!(app.usage_sort, super::app::UsageSort::Cost);
    let block = panel_block(
        theme,
        format!("By key  ·  tokens  ·  sort={}", app.usage_sort.label()),
        true,
    );
    let inner = block.inner(list_area);
    let visible = inner.height as usize;
    let start = list_window_start(sel, items.len(), visible);
    let end = (start + visible).min(items.len());
    let lines: Vec<Line> = items[start..end]
        .iter()
        .enumerate()
        .map(|(off, (name, _id, cost, tok, req, _, _))| {
            let i = start + off;
            let mark = if i == sel { "▶ " } else { "  " };
            let ratio = if bar_by_cost {
                *cost / max_cost
            } else {
                *tok as f64 / max_tok
            };
            let mut spans = vec![Span::styled(mark, name_base_style(theme, i == sel))];
            spans.extend(name_spans(theme, name, i == sel, Some(label_w)));
            spans.push(Span::styled(
                ratio_bar(ratio, bar_w),
                Style::default().fg(theme.chart_color(i)),
            ));
            spans.push(Span::styled(
                format!(" {:>6}", format_tokens(*tok)),
                theme.warning(),
            ));
            spans.push(Span::styled(
                format!("  {}  {}r", format_usd(*cost), req),
                theme.muted(),
            ));
            Line::from(spans)
        })
        .collect();
    frame.render_widget(block, list_area);
    frame.render_widget(Paragraph::new(lines), inner);

    if show_detail {
        let (name, id, cost, tok, req, prompt, completion) = (
            &items[sel].0,
            &items[sel].1,
            items[sel].2,
            items[sel].3,
            items[sel].4,
            items[sel].5,
            items[sel].6,
        );
        let cost_share = (cost / period_total) * 100.0;
        let tok_share = tok as f64 / period_tokens as f64 * 100.0;
        let heading = name.text.clone();
        let models = models_for_key(app, id);
        draw_usage_rollup_detail(
            frame,
            detail_area,
            theme,
            "Key detail",
            &heading,
            &[
                ("name", name.text.clone()),
                (
                    "key id",
                    if id.is_empty() {
                        "—".into()
                    } else {
                        id.clone()
                    },
                ),
                ("total tok", format_tokens(tok)),
                ("tok share", format!("{tok_share:.1}% of period")),
                ("prompt tok", format_tokens(prompt)),
                ("completion", format_tokens(completion)),
                ("requests", req.to_string()),
                ("cost", format_usd(cost)),
                ("$ share", format!("{cost_share:.1}% of period")),
            ],
            tok as f64 / max_tok,
            &models,
        );
    }
}

fn models_for_key(app: &App, key_id: &str) -> Vec<ModelBreakRow> {
    app.usage_summary
        .as_ref()
        .map(|s| {
            s.by_key_model
                .iter()
                .filter(|m| m.downstream_key_id == key_id)
                .map(|m| ModelBreakRow {
                    label: m.label.clone(),
                    provider_kind: m.provider_kind.clone(),
                    request_count: m.request_count,
                    total_tokens: m.total_tokens,
                    total_usd: m.total_usd,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn models_for_day(app: &App, day: &str) -> Vec<ModelBreakRow> {
    app.usage_summary
        .as_ref()
        .map(|s| {
            s.by_day_model
                .iter()
                .filter(|m| m.day == day)
                .map(|m| ModelBreakRow {
                    label: m.label.clone(),
                    provider_kind: m.provider_kind.clone(),
                    request_count: m.request_count,
                    total_tokens: m.total_tokens,
                    total_usd: m.total_usd,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone)]
struct ModelBreakRow {
    label: String,
    provider_kind: String,
    request_count: u64,
    total_tokens: u64,
    total_usd: f64,
}

/// Resolved display label for a usage rollup entity (provider / key).
#[derive(Clone, Debug)]
struct ResolvedName {
    /// Human-readable text (never empty).
    text: String,
    /// Soft-deleted (or hard-missing) — render with strikethrough.
    deleted: bool,
}

/// Map downstream key id → human name.
///
/// Prefers the name/deleted flag from usage summary (includes soft-deleted
/// rows). Falls back to the live Keys list, then a truncated id.
fn resolve_key_name(
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
fn resolve_provider_name(
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
fn name_base_style(theme: &Theme, selected: bool) -> Style {
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
fn name_spans(theme: &Theme, name: &ResolvedName, selected: bool, width: Option<usize>) -> Vec<Span<'static>> {
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

/// Provider pane: health (success / TTFB) **plus** token volume.
fn draw_usage_by_provider(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sel: usize,
    period_tokens: u64,
) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    // (ResolvedName, id, kind, reqs, success_rate, ttfb, cost, tokens, tokens_per_sec)
    let mut rows_data: Vec<(
        ResolvedName,
        String,
        String,
        u64,
        f64,
        Option<f64>,
        f64,
        u64,
        Option<f64>,
    )> = app
        .usage_summary
        .as_ref()
        .map(|s| {
            filtered
                .iter()
                .filter_map(|&i| {
                    s.by_provider.get(i).map(|p| {
                        (
                            resolve_provider_name(
                                &app.providers,
                                &p.provider_id,
                                &p.name,
                                p.deleted,
                            ),
                            p.provider_id.clone(),
                            p.provider_kind.clone(),
                            p.request_count,
                            p.success_rate,
                            p.avg_ttfb_ms,
                            p.total_usd,
                            p.total_tokens,
                            p.tokens_per_sec,
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if rows_data.is_empty() {
        let hint = if !app.filter.is_empty() {
            "no matches for filter"
        } else {
            "no provider rows"
        };
        empty_state(frame, area, theme, "By provider", hint);
        return;
    }
    match app.usage_sort {
        super::app::UsageSort::Cost => {
            rows_data.sort_by(|a, b| b.6.partial_cmp(&a.6).unwrap_or(std::cmp::Ordering::Equal))
        }
        super::app::UsageSort::Tokens => {
            rows_data.sort_by(|a, b| b.7.cmp(&a.7))
        }
        super::app::UsageSort::Date => {
            // Default health view: worst success rate first, then slowest TTFB.
            rows_data.sort_by(|a, b| {
                a.4.partial_cmp(&b.4)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        let ta = a.5.unwrap_or(f64::MAX);
                        let tb = b.5.unwrap_or(f64::MAX);
                        tb.partial_cmp(&ta).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .then_with(|| b.7.cmp(&a.7))
            })
        }
    }
    let sel = sel.min(rows_data.len().saturating_sub(1));
    let (list_area, detail_area, show_detail) = usage_master_detail(area);

    // Fixed metric columns; NAME takes remaining width so OAuth labels like
    // `claude (user@outlook.com)` are not hard-capped (was clamp max 28).
    let kind_w = 12usize;
    let tok_w = 8usize;
    let req_w = 6usize;
    let success_w = 8usize;
    let ttfb_w = 8usize;
    let tps_w = 8usize;
    let col_spacing = 6usize; // 7 columns → 6 gaps
    let border = 2usize;
    let name_w = (list_area.width as usize)
        .saturating_sub(border + kind_w + tok_w + req_w + success_w + ttfb_w + tps_w + col_spacing)
        .max(12);

    let rows: Vec<Row> = rows_data
        .iter()
        .enumerate()
        .map(|(i, (name, _id, kind, req, rate, ttfb, _cost, tok, tps))| {
            let ttfb_s = ttfb
                .map(|ms| format!("{ms:.0}ms"))
                .unwrap_or_else(|| "—".into());
            let tps_s = match tps {
                None => "—".into(),
                Some(v) if *v >= 1000.0 => format_tokens(v.round() as u64),
                Some(v) => format!("{v:.1}"),
            };
            let name_cell = Cell::from(Line::from(name_spans(
                theme,
                name,
                i == sel,
                Some(name_w),
            )));
            let row = Row::new(vec![
                name_cell,
                Cell::from(truncate(kind, kind_w)),
                Cell::from(format_tokens(*tok)),
                Cell::from(req.to_string()),
                Cell::from(format!("{:.0}%", rate * 100.0)),
                Cell::from(ttfb_s),
                Cell::from(tps_s),
            ]);
            if i == sel {
                row.style(theme.selection())
            } else {
                row
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Min(name_w as u16),
            Constraint::Length(kind_w as u16),
            Constraint::Length(tok_w as u16),
            Constraint::Length(req_w as u16),
            Constraint::Length(success_w as u16),
            Constraint::Length(ttfb_w as u16),
            Constraint::Length(tps_w as u16),
        ],
    )
    .header(
        Row::new(vec![
            "name", "kind", "tokens", "reqs", "success", "ttfb", "tok/s",
        ])
        .style(theme.header_cell()),
    )
    .block(panel_block(
        theme,
        &format!(
            "By provider  ·  tokens  ·  sort={}  ·  t cycle",
            app.usage_sort.label()
        ),
        true,
    ))
    .column_spacing(1);
    render_scrollable_table(frame, table, list_area, sel);

    if show_detail {
        if let Some((name, id, kind, req, rate, ttfb, cost, tok, tps)) = rows_data.get(sel) {
            let ttfb_s = ttfb
                .map(|ms| format!("{ms:.0}ms"))
                .unwrap_or_else(|| "—".into());
            let tok_share = *tok as f64 / period_tokens.max(1) as f64 * 100.0;
            let max_tok = rows_data.iter().map(|r| r.7).max().unwrap_or(1).max(1) as f64;
            draw_usage_rollup_detail(
                frame,
                detail_area,
                theme,
                "Provider detail",
                &name.text,
                &[
                    ("name", name.text.clone()),
                    ("id", id.clone()),
                    ("kind", kind.clone()),
                    ("tokens", format_tokens(*tok)),
                    ("tok share", format!("{tok_share:.1}% of period")),
                    ("requests", req.to_string()),
                    ("success", format!("{:.1}%", rate * 100.0)),
                    ("avg ttfb", ttfb_s),
                    ("tok/s", super::widgets::format_tok_per_sec(*tps)),
                    ("cost", format_usd(*cost)),
                ],
                *tok as f64 / max_tok,
                &[],
            );
        }
    }
}

fn draw_usage_by_day(frame: &mut Frame, area: Rect, app: &App, sel: usize, period_total: f64) {
    let theme = &app.theme;
    let period = app
        .usage_summary
        .as_ref()
        .map(|s| s.period.as_str())
        .unwrap_or(app.usage_period.as_str());
    let calendar = month_day_spends(period, &usage_by_day_tuples(app));
    let days = usage_day_view_rows(app, &calendar);
    if days.is_empty() {
        let hint = if !app.filter.is_empty() {
            "no matches for filter"
        } else {
            "no days in period"
        };
        empty_state(frame, area, theme, "By day", hint);
        return;
    }
    let sel = sel.min(days.len().saturating_sub(1));
    let cal_idx = day_spend_calendar_index(&days[sel]);

    let (list_area, detail_area, show_detail) = usage_master_detail(area);
    let max_cost = calendar
        .iter()
        .map(|d| d.total_usd)
        .fold(0.0_f64, f64::max)
        .max(1e-9);

    // Mini heatmap above the day table when vertical space allows.
    let list_chunks = if list_area.height >= 16 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(10), Constraint::Min(4)])
            .split(list_area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(4)])
            .split(list_area)
    };

    if list_chunks[0].height >= 8 {
        let block = panel_block(theme, "Contribution (↑↓ select · c sort)", true);
        let inner = block.inner(list_chunks[0]);
        frame.render_widget(block, list_chunks[0]);
        let sel_date = cal_idx.and_then(|i| calendar.get(i).map(|d| d.date.as_str()));
        let lines =
            contribution_graph_lines(theme, &usage_trailing_tuples(app), sel_date, "", true);
        frame.render_widget(Paragraph::new(lines), inner);
    }

    let rows: Vec<Row> = days
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let heat = heat_level(d.total_usd, max_cost);
            let mark = if heat == 0 { "· " } else { "██" };
            let row = Row::new(vec![
                Cell::from(Line::from(vec![
                    Span::styled(mark, theme.heat_cell_style(heat)),
                    Span::raw(format!(" {}", d.date)),
                ])),
                Cell::from(d.request_count.to_string()),
                Cell::from(format_tokens(d.total_tokens)),
                Cell::from(format_usd(d.total_usd)),
                Cell::from(format!("{:.0}%", (d.total_usd / max_cost) * 100.0)),
            ]);
            if i == sel {
                row.style(theme.selection())
            } else {
                row
            }
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(14),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(5),
        ],
    )
    .header(
        Row::new(vec!["DAY", "REQS", "TOKENS", "COST", "%"]).style(theme.header_cell()),
    )
    .block(panel_block(
        theme,
        format!("By day  {}/{}", sel + 1, days.len()),
        true,
    ));
    render_scrollable_table(frame, table, list_chunks[1], sel);

    if show_detail {
        let d = &days[sel];
        let share = (d.total_usd / period_total) * 100.0;
        let models = models_for_day(app, &d.date);
        draw_usage_rollup_detail(
            frame,
            detail_area,
            theme,
            "Day detail",
            &d.date,
            &[
                ("day", d.date.clone()),
                ("requests", d.request_count.to_string()),
                ("tokens", format_tokens(d.total_tokens)),
                ("cost", format_usd(d.total_usd)),
                ("share", format!("{share:.1}% of period")),
                (
                    "avg $/req",
                    if d.request_count > 0 {
                        format_usd(d.total_usd / d.request_count as f64)
                    } else {
                        "—".into()
                    },
                ),
            ],
            d.total_usd / max_cost,
            &models,
        );
    }
}

/// Filtered + sorted day rows for the By day pane (same order as the table).
fn usage_day_view_rows(app: &App, calendar: &[DaySpend]) -> Vec<DaySpend> {
    let filtered = app.filtered_indices();
    let mut days: Vec<DaySpend> = filtered
        .iter()
        .filter_map(|&i| calendar.get(i).cloned())
        .collect();
    match app.usage_sort {
        super::app::UsageSort::Cost => {
            days.sort_by(|a, b| {
                b.total_usd
                    .partial_cmp(&a.total_usd)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        super::app::UsageSort::Tokens => {
            days.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));
        }
        super::app::UsageSort::Date => {}
    }
    days
}

fn day_spend_calendar_index(d: &DaySpend) -> Option<usize> {
    d.date
        .get(8..10)
        .and_then(|s| s.parse::<usize>().ok())
        .map(|dom| dom.saturating_sub(1))
}

fn draw_usage_rollup_detail(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    title: &str,
    heading: &str,
    rows: &[(&str, String)],
    bar_ratio: f64,
    models: &[ModelBreakRow],
) {
    let mut lines = vec![
        Line::from(Span::styled(
            format!("  {heading}"),
            theme.accent_bold(),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("  {}", ratio_bar(bar_ratio, 22)),
            Style::default().fg(theme.accent),
        )),
        Line::from(""),
    ];
    lines.extend(detail_kv(theme, rows));

    // Per-model breakdown for this key/day — token-first.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Models (by tokens)", theme.title())));
    if models.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (no model breakdown)",
            theme.subtle(),
        )));
    } else {
        let max_m = models
            .iter()
            .map(|m| m.total_tokens)
            .max()
            .unwrap_or(1)
            .max(1) as f64;
        let bar_w = (area.width as usize).saturating_sub(20).clamp(6, 16);
        let mut ordered: Vec<&ModelBreakRow> = models.iter().collect();
        ordered.sort_by(|a, b| {
            b.total_tokens
                .cmp(&a.total_tokens)
                .then_with(|| {
                    b.total_usd
                        .partial_cmp(&a.total_usd)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let max_rows = (area.height as usize).saturating_sub(14).max(3);
        for (i, m) in ordered.iter().take(max_rows).enumerate() {
            let prov = if m.provider_kind.is_empty() {
                String::new()
            } else {
                format!(" · {}", truncate(&m.provider_kind, 10))
            };
            lines.push(Line::from(Span::styled(
                format!("  {}", truncate(&m.label, 22)),
                Style::default().fg(theme.chart_color(i)),
            )));
            lines.push(Line::from(vec![
                Span::styled(
                    format!(
                        "  {}",
                        ratio_bar(m.total_tokens as f64 / max_m, bar_w)
                    ),
                    Style::default().fg(theme.chart_color(i)),
                ),
                Span::styled(
                    format!(" {}", format_tokens(m.total_tokens)),
                    theme.warning(),
                ),
                Span::styled(
                    format!("  {}  {}r{prov}", format_usd(m.total_usd), m.request_count),
                    theme.muted(),
                ),
            ]));
        }
        if ordered.len() > max_rows {
            lines.push(Line::from(Span::styled(
                format!("  … +{} more models", ordered.len() - max_rows),
                theme.subtle(),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  t cycle pane · c sort · ↑↓ select",
        theme.subtle(),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel_block(theme, title, true)),
        area,
    );
}

/// Sort by-model rollup rows: (idx, label, provider, cost, tokens, reqs).
fn sort_usage_items(
    items: &mut [(usize, String, String, f64, u64, u64, Option<f64>)],
    sort: super::app::UsageSort,
) {
    use super::app::UsageSort;
    match sort {
        UsageSort::Cost => {
            items.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
        }
        UsageSort::Tokens => items.sort_by(|a, b| b.4.cmp(&a.4)),
        UsageSort::Date => items.sort_by(|a, b| a.1.cmp(&b.1)), // label A–Z
    }
}

/// Format token counts; show "—" when zero so cache columns stay scannable.
fn fmt_tok(n: u32) -> String {
    if n == 0 {
        "—".into()
    } else {
        n.to_string()
    }
}

/// Returns `(clamped_scroll, max_scroll)` so the app can refuse further down-scroll.
fn draw_usage_record_detail(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    u: &crate::dto::UsageRecordView,
    pricing: &[crate::dto::PricingView],
    scroll: u16,
) -> (u16, u16) {
    let mut lines = vec![
        Line::from(Span::styled(
            // Detail pane: show full model / ts (wrap handles overflow).
            format!("  {}", u.model_id.as_deref().unwrap_or("(model)")),
            theme.accent_bold(),
        )),
        Line::from(Span::styled(
            format!("  {}", format_local_time(&u.ts)),
            theme.subtle(),
        )),
        Line::from(""),
        Line::from(Span::styled("  Tokens", theme.title())),
    ];
    lines.extend(detail_kv(
        theme,
        &[
            ("prompt", u.prompt_tokens.to_string()),
            ("completion", u.completion_tokens.to_string()),
            ("reasoning", u.reasoning_tokens.to_string()),
            ("cache read", u.cache_read_tokens.to_string()),
            ("cache write", u.cache_write_tokens.to_string()),
            ("total", u.total_tokens.to_string()),
        ],
    ));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Request", theme.title())));
    // Exact (not sum/sum-estimated) throughput for this one request: the
    // generation window is duration_ms minus ttfb_ms (0 if ttfb is unknown).
    let tps = match u.duration_ms {
        Some(dur) if u.completion_tokens > 0 => {
            let gen_ms = dur.saturating_sub(u.ttfb_ms.unwrap_or(0));
            (gen_ms > 0).then(|| u.completion_tokens as f64 * 1000.0 / gen_ms as f64)
        }
        _ => None,
    };
    lines.extend(detail_kv(
        theme,
        &[
            ("status", u.status.clone()),
            (
                "error",
                u.error_class.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "duration",
                u.duration_ms
                    .map(|ms| format!("{ms}ms"))
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "ttfb",
                u.ttfb_ms
                    .map(|ms| format!("{ms}ms"))
                    .unwrap_or_else(|| "—".into()),
            ),
            ("tok/s", super::widgets::format_tok_per_sec(tps)),
            ("cost", format_usd(u.cost_usd)),
            ("stream", if u.stream { "yes" } else { "no" }.into()),
            (
                "alias",
                u.alias.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "provider",
                u.provider_id
                    .clone()
                    .or_else(|| u.provider_kind.clone())
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "strategy",
                u.route_strategy.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "attempts",
                format!("{}/{}", u.attempt_no + 1, u.attempt_count.max(1)),
            ),
            (
                "key",
                u.downstream_key_id
                    .as_deref()
                    .map(|s| truncate(s, 28))
                    .unwrap_or_else(|| "—".into()),
            ),
            // 56-col pane − padding/label ≈ 34 usable value columns.
            ("request_id", truncate(&u.request_id, 34)),
        ],
    ));

    // Matched unit prices from pricing table (if loaded).
    if let Some(rate) = lookup_pricing(
        pricing,
        u.provider_kind.as_deref(),
        u.model_id.as_deref(),
    ) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Pricing  ($ / MTok)",
            theme.title(),
        )));
        lines.extend(detail_kv(
            theme,
            &[
                ("input", format!("{:.4}", rate.input_per_mtok)),
                ("output", format!("{:.4}", rate.output_per_mtok)),
                (
                    "cache read",
                    rate.cache_read_per_mtok
                        .map(|v| format!("{v:.4}"))
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    "cache write",
                    rate.cache_write_per_mtok
                        .map(|v| format!("{v:.4}"))
                        .unwrap_or_else(|| "—".into()),
                ),
                (
                    "reasoning",
                    rate.reasoning_per_mtok
                        .map(|v| format!("{v:.4}"))
                        .unwrap_or_else(|| "—".into()),
                ),
            ],
        ));
    }

    if u.cache_read_tokens > 0 || u.cache_write_tokens > 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "  cache tok  read={}  write={}",
                u.cache_read_tokens, u.cache_write_tokens
            ),
            theme.success(),
        )));
    }

    // Estimate wrapped height (ratatui's line_count is unstable/private in 0.29).
    // Clamp scroll so overshoot (resize / short content) pins to the bottom.
    // No wrap: top/bottom are hard stops. Title carries a scroll hint when overflow.
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let total = wrapped_line_count(&lines, inner_w);
    let max_scroll = total.saturating_sub(inner_h) as u16;
    let scroll = scroll.min(max_scroll);
    let title = if max_scroll == 0 {
        "Request detail".into()
    } else if scroll == 0 {
        format!("Request detail  ↓^j  +{max_scroll}")
    } else if scroll >= max_scroll {
        "Request detail  ↑^k".into()
    } else {
        format!(
            "Request detail  ^j/^k  {}/{}",
            scroll + 1,
            max_scroll + 1
        )
    };

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(panel_block(theme, title, true)),
        area,
    );
    (scroll, max_scroll)
}

/// Approximate how many terminal rows `lines` occupy at `width` with wrap.
fn wrapped_line_count(lines: &[Line<'_>], width: usize) -> usize {
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

/// Best-effort match of a usage row to a pricing table entry.
fn lookup_pricing<'a>(
    pricing: &'a [crate::dto::PricingView],
    provider_kind: Option<&str>,
    model_id: Option<&str>,
) -> Option<&'a crate::dto::PricingView> {
    let model = model_id.filter(|s| !s.is_empty())?;
    let kind = provider_kind.unwrap_or("");
    // Exact (kind, model)
    if !kind.is_empty() {
        if let Some(p) = pricing
            .iter()
            .find(|p| p.provider_kind == kind && p.model_id == model)
        {
            return Some(p);
        }
    }
    // Model-only exact
    if let Some(p) = pricing.iter().find(|p| p.model_id == model) {
        return Some(p);
    }
    // Prefix on model_id
    pricing.iter().find(|p| {
        model.starts_with(&p.model_id) || p.model_id.starts_with(model)
    })
}

// ── Pricing ─────────────────────────────────────────────────────────────────

fn draw_pricing(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    let rows_src = match app.pricing_pane {
        PricingPane::Merged => &app.pricing,
        PricingPane::Overrides => &app.pricing_overrides,
    };
    let title = match app.pricing_pane {
        PricingPane::Merged => format!(
            "Merged  {}/{}  ·  o overrides · $/MTok",
            filtered.len(),
            rows_src.len()
        ),
        PricingPane::Overrides => format!(
            "Overrides  {}  ·  o merged · a/e/d · pricing.json",
            filtered.len()
        ),
    };

    if filtered.is_empty() {
        let hint = if app.pricing_pane == PricingPane::Overrides {
            if app.loading {
                "loading overrides…"
            } else {
                "no overrides yet — select a model in merged view and press a"
            }
        } else if app.loading {
            "loading pricing…"
        } else {
            "r refresh · R reload · s sync LiteLLM"
        };
        empty_state(frame, area, theme, &title, hint);
        return;
    }

    let (list_area, detail_area) = split_master_detail(area);
    let sel = app.selected[Tab::Pricing.index()].min(filtered.len() - 1);

    // Dynamic model column — same idea as Usage (don't hard-cap to 28 chars).
    let inner_w = list_area.width.saturating_sub(4) as usize;
    let fixed = 3 + 12 + 8 + 8 + 7 + 7 + 6; // badge+prov+IN+OUT+cR+cW+gaps
    let model_w = inner_w.saturating_sub(fixed).max(16);

    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(view_i, &data_i)| {
            let p = &rows_src[data_i];
            let is_ov = app.pricing_pane == PricingPane::Overrides
                || app.pricing_overrides.iter().any(|o| {
                    o.provider_kind == p.provider_kind && o.model_id == p.model_id
                });
            let badge = if is_ov { "OV" } else { "" };
            let row = Row::new(vec![
                badge.into(),
                truncate(&p.provider_kind, 12),
                truncate(&p.model_id, model_w),
                fmt_rate(p.input_per_mtok),
                fmt_rate(p.output_per_mtok),
                fmt_opt_rate(p.cache_read_per_mtok),
                fmt_opt_rate(p.cache_write_per_mtok),
            ]);
            if view_i == sel {
                row.style(theme.selection())
            } else {
                row.style(Style::default().fg(theme.fg))
            }
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Length(12),
            Constraint::Min(model_w as u16),
            Constraint::Length(8),
            Constraint::Length(8),
            Constraint::Length(7),
            Constraint::Length(7),
        ],
    )
    .header(
        Row::new(vec!["", "PROVIDER", "MODEL", "IN", "OUT", "cR", "cW"])
            .style(theme.header_cell()),
    )
    .block(panel_block(
        theme,
        format!("{title}  ($/MTok · cR/cW = cache)"),
        true,
    ));
    render_scrollable_table(frame, table, list_area, sel);

    let p = &rows_src[filtered[sel]];
    draw_pricing_detail(frame, detail_area, theme, p, app);
}

fn fmt_rate(v: f64) -> String {
    if !v.is_finite() {
        return "—".into();
    }
    // Compact but readable (avoid "2.500" noise and "0.000" for tiny rates).
    if v == 0.0 {
        "0".into()
    } else if v >= 100.0 {
        format!("{v:.2}")
    } else if v >= 1.0 {
        format!("{v:.3}")
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn fmt_opt_rate(v: Option<f64>) -> String {
    match v {
        Some(x) => fmt_rate(x),
        None => "—".into(),
    }
}

fn draw_pricing_detail(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    p: &crate::dto::PricingView,
    app: &App,
) {
    let is_ov = app.pricing_overrides.iter().any(|o| {
        o.provider_kind == p.provider_kind && o.model_id == p.model_id
    });
    let mut lines = vec![
        Line::from(Span::styled(
            format!("  {}", p.model_id), // full model id in detail
            theme.accent_bold(),
        )),
        Line::from(Span::styled(
            format!(
                "  {}{}",
                p.provider_kind,
                if is_ov {
                    "  ·  operator override (pricing.json)"
                } else {
                    ""
                }
            ),
            if is_ov {
                theme.success()
            } else {
                theme.muted()
            },
        )),
        Line::from(""),
        Line::from(Span::styled("  Rates  ($ / MTok)", theme.title())),
    ];
    lines.extend(detail_kv(
        theme,
        &[
            ("input", fmt_rate(p.input_per_mtok)),
            ("output", fmt_rate(p.output_per_mtok)),
            ("cache read", fmt_opt_rate(p.cache_read_per_mtok)),
            ("cache write", fmt_opt_rate(p.cache_write_per_mtok)),
            ("reasoning", fmt_opt_rate(p.reasoning_per_mtok)),
            (
                "effective",
                if p.effective_from.is_empty() {
                    "—".into()
                } else {
                    format_local_time(&p.effective_from)
                },
            ),
        ],
    ));

    // Quick example: cost for 1M in + 1M out + optional cache
    let sample = p.input_per_mtok
        + p.output_per_mtok
        + p.cache_read_per_mtok.unwrap_or(0.0)
        + p.cache_write_per_mtok.unwrap_or(0.0);
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  sample 1M in+out[+cache] ≈ ${sample:.4}"),
        theme.subtle(),
    )));

    if p.cache_read_per_mtok.is_some() || p.cache_write_per_mtok.is_some() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!(
                "  cache  read={}  write={}",
                fmt_opt_rate(p.cache_read_per_mtok),
                fmt_opt_rate(p.cache_write_per_mtok)
            ),
            theme.success(),
        )));
    }

    // Period usage for this model (prefetched with Pricing).
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Usage (period)", theme.title())));
    match usage_for_pricing_row(app, p) {
        Some((period, req, tok, usd, share)) => {
            lines.extend(detail_kv(
                theme,
                &[
                    ("period", period),
                    ("requests", req.to_string()),
                    ("tokens", format_tokens(tok)),
                    ("spend", format_usd(usd)),
                    ("share", format!("{share:.1}% of period")),
                ],
            ));
        }
        None if app.usage_summary.is_none() => {
            lines.push(Line::from(Span::styled(
                format!(
                    "    {} loading usage…",
                    super::widgets::spinner(app.tick_frame)
                ),
                theme.muted(),
            )));
        }
        None => {
            lines.push(Line::from(Span::styled(
                "    no traffic for this model in the current period",
                theme.subtle(),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  a override selected · o toggle pane",
        theme.subtle(),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel_block(theme, "Pricing detail", true)),
        area,
    );
}

/// Match current-month usage rollup to a pricing row (by model id, prefer same provider).
fn usage_for_pricing_row(
    app: &App,
    p: &crate::dto::PricingView,
) -> Option<(String, u64, u64, f64, f64)> {
    let summary = app.usage_summary.as_ref()?;
    let period = summary.period.clone();
    let period_total = summary.total_usd.max(1e-12);

    // Score candidates: exact model+provider > exact model > prefix/contains.
    let mut best: Option<(&crate::dto::UsageModelEntry, i32)> = None;
    for m in &summary.by_model {
        let mut score = 0i32;
        if m.label == p.model_id {
            score += 100;
        } else if m.label.contains(&p.model_id) || p.model_id.contains(&m.label) {
            score += 40;
        } else if m.label.starts_with(&p.model_id) || p.model_id.starts_with(&m.label) {
            score += 30;
        } else {
            continue;
        }
        if !m.provider_kind.is_empty() && m.provider_kind == p.provider_kind {
            score += 50;
        } else if !m.provider_kind.is_empty()
            && (m.provider_kind.contains(&p.provider_kind)
                || p.provider_kind.contains(&m.provider_kind))
        {
            score += 20;
        }
        match best {
            Some((_, s)) if s >= score => {}
            _ => best = Some((m, score)),
        }
    }
    let (m, _) = best?;
    let share = (m.total_usd / period_total) * 100.0;
    Some((
        period,
        m.request_count,
        m.total_tokens,
        m.total_usd,
        share,
    ))
}

// ── Forms / modals ──────────────────────────────────────────────────────────

fn draw_provider_add_chooser(
    frame: &mut Frame,
    theme: &Theme,
    c: &super::forms::ProviderAddChooser,
) {
    use super::forms::PROVIDER_ADD_OPTIONS;
    let area = super::widgets::centered(frame.area(), 64, 50);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(" Add provider ", theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![
        Line::from(Span::styled(
            "  How should this provider authenticate?",
            theme.muted(),
        )),
        Line::from(""),
    ];
    for (i, label) in PROVIDER_ADD_OPTIONS.iter().enumerate() {
        let sel = i == c.selected;
        lines.push(Line::from(Span::styled(
            format!(
                "  {} {}. {}",
                if sel { "▸" } else { " " },
                i + 1,
                label
            ),
            if sel {
                theme.accent_bold()
            } else {
                Style::default().fg(theme.fg)
            },
        )));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "  ↑↓ move · 1-4 jump · Enter confirm · Esc cancel",
        theme.subtle(),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Keyboard reference modal. Returns `(clamped_scroll, max_scroll)` — no wrap.
fn draw_help(frame: &mut Frame, theme: &Theme, scroll: u16) -> (u16, u16) {
    let body = "\
Global
  1-6 / Tab     Switch tab          j/k ↑↓     Move
  PgUp/PgDn     Page                g / G      Top / bottom
  /             Filter lists        r          Refresh
  T             Theme auto/dark/light (also CONDUIT_THEME=…)
  t             Home → Usage by-day calendar · on Usage cycles detail
  ?             Help                q          Quit
  j/k ↑↓        Scroll this help    PgUp/PgDn  Page this help
  g / G         Help top / bottom   Esc/?/q    Close help

Providers  (OAuth is an add method here)
  a             Add — choose API key or OAuth (Claude/Codex/Grok)
  e             Edit metadata
  s             Set / rotate API key (or re-auth if OAuth)
  v             Decrypt & show secret (API key or OAuth tokens) in detail
  y             Copy full decrypted dump to clipboard (after v)
  Y             Copy primary only (api_key / access_token)
  o             Re-auth OAuth provider
  x             Force-refresh OAuth tokens
  u             Probe OAuth remaining (Claude 5h/7d · Codex 7d · Grok mo)
  d             Delete

Secret reveal modal
  y / c         Copy full dump
  a             Copy primary (api_key / access_token)
  Enter/Esc     Dismiss

Routes
  a add · e edit · d delete
  Wizard: Ctrl-k cycle single provider ↔ kind pool
          Ctrl-y strategy fixed / fallback / weighted
          pool target = multi-account of same kind

Keys
  a add · e edit · d delete · v show token
  (right pane is detail; no Enter modal · v reveals raw token to copy)

Usage
  [ ] month · c sort · t cycle list · / filter
  Daily spend = GitHub-style heat calendar (cost intensity)
  By day: ↑↓ select day cells · zero days still listed
  Recent: PgUp/PgDn page · g/G first/last page
  Recent detail: Ctrl+j / Ctrl+k scroll (no wrap)
  (right pane is detail; no Enter modal)

Pricing
  o             Toggle merged ↔ overrides pane
  Merged:       R reload · s sync LiteLLM  (read-only list)
  Overrides:    a add · e edit · d delete  (writes pricing.json)

OAuth flow (from Providers → add / re-auth)
  Enter start · o open URL · c cancel pending · Esc close

Forms
  Tab fields · Enter on last = save · Ctrl-s / F2 save · Esc cancel
";
    let lines: Vec<Line> = body
        .lines()
        .map(|l| Line::from(l.to_string()))
        .collect();

    // Slightly taller than generic modals so more of the reference fits.
    let area = super::widgets::centered(frame.area(), 74, 70);
    frame.render_widget(Clear, area);

    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let total = wrapped_line_count(&lines, inner_w);
    let max_scroll = total.saturating_sub(inner_h) as u16;
    let scroll = scroll.min(max_scroll);
    let title = if max_scroll == 0 {
        "Keyboard reference".into()
    } else if scroll == 0 {
        format!("Keyboard reference  ↓j  +{max_scroll}")
    } else if scroll >= max_scroll {
        "Keyboard reference  ↑k".into()
    } else {
        format!(
            "Keyboard reference  j/k  {}/{}",
            scroll + 1,
            max_scroll + 1
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(format!(" {title} "), theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .style(theme.surface()),
        inner,
    );
    (scroll, max_scroll)
}

fn draw_confirm(frame: &mut Frame, theme: &Theme, action: &ConfirmAction) {
    let (title, body) = match action {
        ConfirmAction::DeleteProvider { id, name } => (
            "Confirm delete provider",
            format!("Delete provider «{name}»\n{id}\n\n[y] yes   [n] cancel"),
        ),
        ConfirmAction::DeleteRoute { id, alias } => (
            "Confirm delete route",
            format!("Delete route «{alias}»\n{id}\n\n[y] yes   [n] cancel"),
        ),
        ConfirmAction::DeleteKey { id, name } => (
            "Confirm revoke key",
            format!("Revoke key «{name}»\n{id}\n\n[y] yes   [n] cancel"),
        ),
        ConfirmAction::SetProviderSecret { name, .. } => (
            "Set provider secret",
            format!("Rotate API key for «{name}»?\n\n[y] continue   [n] cancel"),
        ),
        ConfirmAction::DeletePricingOverride {
            provider_kind,
            model_id,
        } => (
            "Confirm delete override",
            format!("Remove {provider_kind} / {model_id} from pricing.json?\n\n[y] yes   [n] cancel"),
        ),
    };
    modal(
        frame,
        theme,
        title,
        body.lines().map(|l| Line::from(l.to_string())).collect(),
        Style::default().fg(theme.error),
    );
}

fn form_modal(
    frame: &mut Frame,
    theme: &Theme,
    title: &str,
    labels: &[&str],
    fields: &[super::input::InputField],
    focus: usize,
    error: Option<&str>,
    footer: &str,
) {
    let area = super::widgets::centered(frame.area(), 70, 62);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(format!(" {title} "), theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from("")];
    for (i, label) in labels.iter().enumerate() {
        let focused = i == focus;
        let marker = if focused { "▸" } else { " " };
        // Focused label: accent so which field is active is obvious.
        let label_style = if focused {
            theme.accent_bold()
        } else {
            theme.muted()
        };
        lines.push(Line::from(Span::styled(
            format!(" {marker} {label}"),
            label_style,
        )));

        // Value line: caret at InputField.cursor (not always at end).
        let base = if focused {
            Style::default()
                .fg(theme.fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg).bg(theme.surface)
        };
        let caret =
            super::input::InputField::caret_style(theme.accent, theme.background);
        let mut value_spans = vec![Span::styled("   ", base)];
        if let Some(field) = fields.get(i) {
            value_spans.extend(field.line_spans(base, caret, focused).spans);
        }
        // Soft pad after the value so the selection bar is easy to spot.
        if focused {
            value_spans.push(Span::styled("  ", base));
        }
        lines.push(Line::from(value_spans));
        lines.push(Line::from(""));
    }
    if let Some(err) = error {
        lines.push(Line::from(Span::styled(format!("  ! {err}"), theme.error())));
    }
    lines.push(Line::from(Span::styled(format!("  {footer}"), theme.subtle())));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_provider_form(frame: &mut Frame, theme: &Theme, f: &super::forms::ProviderForm) {
    let mut labels: Vec<&str> = f.labels();
    let mut extra_title = f.title();
    if let Some(kind) = &f.kind_display {
        extra_title = format!("{} · kind={kind}", f.title());
    }
    if matches!(f.kind, ProviderFormKind::Create) {
        // labels already include Kind
        let _ = &mut labels;
    }
    form_modal(
        frame,
        theme,
        &extra_title,
        &labels,
        &f.fields,
        f.focus,
        f.error.as_deref(),
        "Tab next · Enter on last = save · Ctrl-s save · Esc cancel · Ctrl-k cycle kind",
    );
}

fn draw_key_form(frame: &mut Frame, theme: &Theme, f: &super::forms::KeyForm) {
    let labels = f.labels();
    let hint = if f.is_edit() {
        "Tab next · Enter on last = save · Ctrl-s save · Ctrl-k toggle enabled · Esc cancel"
    } else {
        "Tab next · Enter on last = save · Ctrl-s save · Esc cancel"
    };
    form_modal(
        frame,
        theme,
        &f.title(),
        &labels,
        &f.fields,
        f.focus,
        f.error.as_deref(),
        hint,
    );
}

fn draw_pricing_override_form(
    frame: &mut Frame,
    theme: &Theme,
    f: &super::forms::PricingOverrideForm,
) {
    let labels = super::forms::PricingOverrideForm::labels();
    let title = if f.editing {
        "Edit pricing override"
    } else if f
        .fields
        .get(1)
        .map(|x| !x.value.trim().is_empty())
        .unwrap_or(false)
    {
        "Add override (prefilled from selection)"
    } else {
        "Add pricing override"
    };
    form_modal(
        frame,
        theme,
        title,
        &labels,
        &f.fields,
        f.focus,
        f.error.as_deref(),
        "USD / MTok · focus on Input — tweak & Enter/Ctrl-s save · Esc cancel",
    );
}

fn draw_route_wizard(frame: &mut Frame, theme: &Theme, w: &super::forms::RouteWizard) {
    let area = super::widgets::centered(frame.area(), 78, 70);
    frame.render_widget(Clear, area);
    let title = format!(
        "Route wizard · step {}/3 · {}",
        w.step + 1,
        if w.edit_id.is_some() { "edit" } else { "create" }
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(format!(" {title} "), theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    // step indicators
    let steps = ["Alias", "Targets", "Review"];
    let mut step_spans = vec![Span::styled("  ", theme.subtle())];
    for (i, s) in steps.iter().enumerate() {
        if i > 0 {
            step_spans.push(Span::styled(" › ", theme.subtle()));
        }
        step_spans.push(Span::styled(
            format!(" {s} "),
            if i == w.step {
                theme.badge_ok()
            } else if i < w.step {
                theme.muted()
            } else {
                theme.subtle()
            },
        ));
    }
    lines.push(Line::from(step_spans));
    lines.push(Line::from(""));

    match w.step {
        0 => {
            let base = Style::default()
                .fg(theme.fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD);
            let caret =
                super::input::InputField::caret_style(theme.accent, theme.background);
            let mut alias_spans = vec![Span::styled("  ▸ match_alias: ", theme.accent_bold())];
            alias_spans.extend(w.match_alias.line_spans(base, caret, true).spans);
            alias_spans.push(Span::styled("  ", base));
            lines.push(Line::from(alias_spans));
            lines.push(Line::from(Span::styled(
                format!(
                    "    strategy: {}  — {}  (Ctrl-y / Ctrl-k cycle)",
                    w.strategy(),
                    w.strategy_hint()
                ),
                theme.muted(),
            )));
            lines.push(Line::from(Span::styled(
                "    fixed | fallback | weighted",
                theme.subtle(),
            )));
        }
        1 => {
            lines.push(Line::from(Span::styled(
                "  ↑↓ target · Tab field · Ctrl-k single/pool · Ctrl-t/w add/remove",
                theme.subtle(),
            )));
            lines.push(Line::from(Span::styled(
                "  pool = all accounts of a provider kind (multi-OAuth ready)",
                theme.subtle(),
            )));
            lines.push(Line::from(""));
            for (i, t) in w.targets.iter().enumerate() {
                let label = w.binding_label(&t.binding);
                let selected = i == w.target_focus;
                let mark = if selected { "▸" } else { " " };
                let pool_mark = if t.binding.is_pool() { "◇" } else { "·" };
                let row_style = if selected {
                    theme.accent_bold()
                } else {
                    Style::default().fg(theme.fg)
                };
                lines.push(Line::from(Span::styled(
                    format!(" {mark} [{i}] {pool_mark} {label}"),
                    row_style,
                )));

                // Active target: show real caret on the field being edited.
                if selected {
                    let base = Style::default()
                        .fg(theme.fg)
                        .bg(theme.selection_bg)
                        .add_modifier(Modifier::BOLD);
                    let caret =
                        super::input::InputField::caret_style(theme.accent, theme.background);
                    let edit_model = w.field_in_target == 0;
                    let mut model_spans = vec![Span::styled(
                        "     model=",
                        if edit_model {
                            theme.accent_bold()
                        } else {
                            theme.muted()
                        },
                    )];
                    model_spans.extend(
                        t.model_id
                            .line_spans(base, caret, edit_model)
                            .spans,
                    );
                    lines.push(Line::from(model_spans));

                    let mut ov_spans = vec![Span::styled(
                        "     overrides=",
                        if !edit_model {
                            theme.accent_bold()
                        } else {
                            theme.muted()
                        },
                    )];
                    // Show caret on full overrides field (not truncated) so position is real.
                    ov_spans.extend(
                        t.overrides
                            .line_spans(base, caret, !edit_model)
                            .spans,
                    );
                    lines.push(Line::from(ov_spans));
                } else {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "     model={}  overrides={}",
                            t.model_id.display(),
                            truncate(&t.overrides.display(), 40)
                        ),
                        Style::default().fg(theme.fg),
                    )));
                }
            }
            lines.push(Line::from(Span::styled(
                format!(
                    "\n  editing: {}",
                    if w.field_in_target == 0 {
                        "model_id"
                    } else {
                        "overrides JSON"
                    }
                ),
                theme.muted(),
            )));
        }
        _ => match w.to_body() {
            Ok(body) => {
                lines.push(Line::from(format!("  alias:    {}", body.match_alias)));
                lines.push(Line::from(format!("  strategy: {}", body.strategy)));
                lines.push(Line::from(Span::styled(
                    "  bindings:",
                    theme.muted(),
                )));
                for (i, t) in w.targets.iter().enumerate() {
                    lines.push(Line::from(format!(
                        "  [{i}] {} → {}",
                        w.binding_label(&t.binding),
                        t.model_id.value.trim()
                    )));
                }
                lines.push(Line::from(Span::styled("  targets JSON:", theme.muted())));
                if let Ok(pretty) = serde_json::to_string_pretty(&body.targets) {
                    for l in pretty.lines().take(12) {
                        lines.push(Line::from(format!("  {l}")));
                    }
                }
            }
            Err(e) => lines.push(Line::from(Span::styled(format!("  ! {e}"), theme.error()))),
        },
    }
    if let Some(err) = &w.error {
        lines.push(Line::from(Span::styled(format!("  ! {err}"), theme.error())));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Enter next/save · Ctrl-n/p · Ctrl-s save · Esc cancel",
        theme.subtle(),
    )));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .alignment(Alignment::Left),
        inner,
    );
}

fn draw_oauth_flow(frame: &mut Frame, theme: &Theme, f: &super::forms::OauthFlow) {
    let area = super::widgets::centered(frame.area(), 72, 62);
    frame.render_widget(Clear, area);
    let reauth = !f.provider_id.value.trim().is_empty();
    let title = if reauth {
        " Re-auth OAuth provider "
    } else {
        " Add provider · OAuth "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(title, theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    if f.pending_session_id.is_none() && f.result_message.is_none() {
        lines.push(Line::from(Span::styled(
            format!("  kind     {}", f.kind()),
            theme.accent_bold(),
        )));
        if reauth {
            lines.push(Line::from(Span::styled(
                format!("  provider {}", f.provider_id.display()),
                theme.muted(),
            )));
        }
        let name_focused = f.focus == 1;
        let marker = if name_focused { "▸" } else { " " };
        let base = if name_focused {
            Style::default()
                .fg(theme.fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let caret =
            super::input::InputField::caret_style(theme.accent, theme.background);
        let mut name_spans = vec![
            Span::styled(
                format!("  {marker} name     "),
                if name_focused {
                    theme.accent_bold()
                } else {
                    theme.muted()
                },
            ),
        ];
        name_spans.extend(f.name.line_spans(base, caret, name_focused).spans);
        if name_focused {
            name_spans.push(Span::styled("  ", base));
        }
        lines.push(Line::from(name_spans));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Enter start login · Esc cancel  (returns to Providers)",
            theme.subtle(),
        )));
    } else {
        if let Some(sid) = &f.pending_session_id {
            lines.push(Line::from(format!("  session  {sid}")));
        }
        if let Some(st) = &f.session_status {
            lines.push(Line::from(vec![
                Span::styled("  status   ", theme.muted()),
                Span::styled(st.clone(), theme.accent_bold()),
            ]));
        }
        if let Some(url) = &f.auth_url {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("  auth_url", theme.muted())));
            lines.push(Line::from(Span::styled(
                format!("  {url}"),
                theme.accent_bold(),
            )));
            lines.push(Line::from(Span::styled(
                "  press o to open in browser",
                theme.subtle(),
            )));
        }
        if let Some(code) = &f.user_code {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  device code  {code}"),
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        if let Some(uri) = &f.verification_uri {
            lines.push(Line::from(format!("  visit  {uri}")));
        }
        if let Some(msg) = &f.result_message {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(format!("  {msg}"), theme.success())));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  c cancel · o open · Esc close",
            theme.subtle(),
        )));
    }
    if let Some(err) = &f.error {
        lines.push(Line::from(Span::styled(format!("  ! {err}"), theme.error())));
    }
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
    );
}

#[cfg(test)]
mod list_window_tests {
    use super::list_window_start;

    #[test]
    fn empty_or_zero_visible() {
        assert_eq!(list_window_start(0, 0, 10), 0);
        assert_eq!(list_window_start(5, 20, 0), 0);
    }

    #[test]
    fn short_list_fits_entirely() {
        assert_eq!(list_window_start(0, 5, 10), 0);
        assert_eq!(list_window_start(4, 5, 10), 0);
    }

    #[test]
    fn selection_pins_to_bottom_of_window() {
        // visible=5, selected=7 → window starts at 3 (indices 3..=7)
        assert_eq!(list_window_start(7, 20, 5), 3);
        // selected still on first page
        assert_eq!(list_window_start(2, 20, 5), 0);
        // last item
        assert_eq!(list_window_start(19, 20, 5), 15);
    }
}

#[cfg(test)]
mod stack_color_tests {
    use super::stack_color_at;
    use ratatui::style::Color;

    #[test]
    fn empty_segments_use_fallback() {
        assert_eq!(
            stack_color_at(1.0, 4.0, &[], Color::Red),
            Color::Red
        );
    }

    #[test]
    fn bottom_segment_then_top() {
        let segs = [(Color::Blue, 50.0), (Color::Green, 50.0)];
        // lv=4 → blue occupies [0,2], green (2,4]
        assert_eq!(stack_color_at(0.0, 4.0, &segs, Color::Red), Color::Blue);
        assert_eq!(stack_color_at(1.5, 4.0, &segs, Color::Red), Color::Blue);
        assert_eq!(stack_color_at(2.0, 4.0, &segs, Color::Red), Color::Blue);
        assert_eq!(stack_color_at(2.5, 4.0, &segs, Color::Red), Color::Green);
        assert_eq!(stack_color_at(4.0, 4.0, &segs, Color::Red), Color::Green);
    }

    #[test]
    fn unequal_shares() {
        // 75% blue bottom, 25% green top on height 4 → blue [0,3], green (3,4]
        let segs = [(Color::Blue, 75.0), (Color::Green, 25.0)];
        assert_eq!(stack_color_at(2.9, 4.0, &segs, Color::Red), Color::Blue);
        assert_eq!(stack_color_at(3.5, 4.0, &segs, Color::Red), Color::Green);
    }
}
