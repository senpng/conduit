//! Usage rollup panes: by model / key / provider / day.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};
use ratatui::Frame;

use super::super::super::app::{App, UsageSort};
use super::super::super::widgets::{
    format_tokens, format_tok_per_sec, format_usd, heat_level, month_day_spends, pad_display,
    panel_block, truncate, DaySpend,
};
use super::super::common::{
    contribution_graph_lines, name_base_style, name_spans, or_em_dash, provider_health_cmp,
    render_scrollable_table, resolve_key_name, resolve_provider_name, sort_by_cost_desc,
    sort_by_tokens_desc, sort_usage_items, usage_by_day_tuples, usage_trailing_tuples,
    ResolvedName,
};
use super::helpers::{
    draw_token_cost_list, draw_usage_rollup_detail, list_label_bar_widths, max_tok_cost,
    models_for_day, models_for_key, period_shares, rollup_empty, usage_master_detail,
};

pub(crate) fn draw_usage_by_model(
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
        rollup_empty(frame, area, app, "By model", "no rollup data");
        return;
    }
    sort_usage_items(&mut items, app.usage_sort);
    let sel = sel.min(items.len().saturating_sub(1));
    let (list_area, detail_area, show_detail) = usage_master_detail(area);
    let (max_tok, max_cost) = max_tok_cost(items.iter().map(|x| x.4), items.iter().map(|x| x.3));
    let (label_w, bar_w) = list_label_bar_widths(list_area.width, 10);
    let bar_by_cost = matches!(app.usage_sort, UsageSort::Cost);

    draw_token_cost_list(
        frame,
        list_area,
        theme,
        format!("By model  ·  tokens  ·  sort={}", app.usage_sort.label()),
        sel,
        items.len(),
        bar_w,
        bar_by_cost,
        max_tok,
        max_cost,
        |i| {
            let mark = if i == sel { "▶ " } else { "  " };
            let label = &items[i].1;
            vec![Span::styled(
                format!("{mark}{}", pad_display(label, label_w)),
                if i == sel {
                    theme.accent_bold()
                } else {
                    Style::default().fg(theme.fg)
                },
            )]
        },
        |i| (items[i].4, items[i].3, items[i].5),
    );

    if show_detail {
        let (label, provider, cost, tok, req, tps) = (
            &items[sel].1,
            &items[sel].2,
            items[sel].3,
            items[sel].4,
            items[sel].5,
            items[sel].6,
        );
        let (tok_share, cost_share) = period_shares(cost, tok, period_total, period_tokens);
        draw_usage_rollup_detail(
            frame,
            detail_area,
            theme,
            "Model detail",
            label,
            &[
                ("label", label.clone()),
                ("provider", or_em_dash(provider)),
                ("tokens", format_tokens(tok)),
                ("tok share", tok_share),
                ("tok/s", format_tok_per_sec(tps)),
                ("requests", req.to_string()),
                ("cost", format_usd(cost)),
                ("$ share", cost_share),
            ],
            tok as f64 / max_tok,
            &[], // model pane is already the list
        );
    }
}

pub(crate) fn draw_usage_by_key(
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
        rollup_empty(frame, area, app, "By key", "no rollup data");
        return;
    }
    match app.usage_sort {
        UsageSort::Cost => sort_by_cost_desc(&mut items, |x| x.2),
        UsageSort::Tokens => sort_by_tokens_desc(&mut items, |x| x.3),
        // Keys have no timestamp; fall back to name.
        UsageSort::Date => items.sort_by(|a, b| a.0.text.cmp(&b.0.text)),
    }
    let sel = sel.min(items.len().saturating_sub(1));
    let (list_area, detail_area, show_detail) = usage_master_detail(area);
    let (max_tok, max_cost) = max_tok_cost(items.iter().map(|x| x.3), items.iter().map(|x| x.2));
    let (label_w, bar_w) = list_label_bar_widths(list_area.width, 12);
    let bar_by_cost = matches!(app.usage_sort, UsageSort::Cost);

    draw_token_cost_list(
        frame,
        list_area,
        theme,
        format!("By key  ·  tokens  ·  sort={}", app.usage_sort.label()),
        sel,
        items.len(),
        bar_w,
        bar_by_cost,
        max_tok,
        max_cost,
        |i| {
            let mark = if i == sel { "▶ " } else { "  " };
            let mut spans = vec![Span::styled(mark, name_base_style(theme, i == sel))];
            spans.extend(name_spans(theme, &items[i].0, i == sel, Some(label_w)));
            spans
        },
        |i| (items[i].3, items[i].2, items[i].4),
    );

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
        let (tok_share, cost_share) = period_shares(cost, tok, period_total, period_tokens);
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
                ("key id", or_em_dash(id)),
                ("total tok", format_tokens(tok)),
                ("tok share", tok_share),
                ("prompt tok", format_tokens(prompt)),
                ("completion", format_tokens(completion)),
                ("requests", req.to_string()),
                ("cost", format_usd(cost)),
                ("$ share", cost_share),
            ],
            tok as f64 / max_tok,
            &models,
        );
    }
}

pub(crate) fn draw_usage_by_provider(
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
        rollup_empty(frame, area, app, "By provider", "no provider rows");
        return;
    }
    match app.usage_sort {
        UsageSort::Cost => sort_by_cost_desc(&mut rows_data, |x| x.6),
        UsageSort::Tokens => sort_by_tokens_desc(&mut rows_data, |x| x.7),
        // Default health view: worst success, slowest TTFB, most tokens.
        UsageSort::Date => {
            rows_data.sort_by(|a, b| provider_health_cmp(a.4, a.5, a.7, b.4, b.5, b.7))
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
                    ("tok/s", format_tok_per_sec(*tps)),
                    ("cost", format_usd(*cost)),
                ],
                *tok as f64 / max_tok,
                &[],
            );
        }
    }
}

pub(crate) fn draw_usage_by_day(frame: &mut Frame, area: Rect, app: &App, sel: usize, period_total: f64) {
    let theme = &app.theme;
    let period = app
        .usage_summary
        .as_ref()
        .map(|s| s.period.as_str())
        .unwrap_or(app.usage_period.as_str());
    let calendar = month_day_spends(period, &usage_by_day_tuples(app));
    let days = usage_day_view_rows(app, &calendar);
    if days.is_empty() {
        rollup_empty(frame, area, app, "By day", "no days in period");
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
pub(crate) fn usage_day_view_rows(app: &App, calendar: &[DaySpend]) -> Vec<DaySpend> {
    let filtered = app.filtered_indices();
    let mut days: Vec<DaySpend> = filtered
        .iter()
        .filter_map(|&i| calendar.get(i).cloned())
        .collect();
    match app.usage_sort {
        UsageSort::Cost => sort_by_cost_desc(&mut days, |d| d.total_usd),
        UsageSort::Tokens => sort_by_tokens_desc(&mut days, |d| d.total_tokens),
        UsageSort::Date => {}
    }
    days
}

fn day_spend_calendar_index(d: &DaySpend) -> Option<usize> {
    d.date
        .get(8..10)
        .and_then(|s| s.parse::<usize>().ok())
        .map(|dom| dom.saturating_sub(1))
}

