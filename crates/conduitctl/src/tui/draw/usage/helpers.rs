//! Shared layout / model-break helpers for Usage panes.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::super::super::app::App;
use super::super::super::theme::Theme;
use super::super::super::widgets::{
    detail_kv, empty_state, format_tokens, format_usd, panel_block, ratio_bar, truncate,
};
use super::super::common::list_window_start;

pub(crate) fn usage_master_detail(area: Rect) -> (Rect, Rect, bool) {
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

/// Draw empty state for a rollup pane (filter-aware).
pub(crate) fn rollup_empty(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    title: &str,
    no_data_hint: &str,
) {
    let hint = if !app.filter.is_empty() {
        "no matches for filter"
    } else {
        no_data_hint
    };
    empty_state(frame, area, &app.theme, title, hint);
}

/// Max token / cost scales for list bars.
pub(crate) fn max_tok_cost(tokens: impl Iterator<Item = u64>, costs: impl Iterator<Item = f64>) -> (f64, f64) {
    let max_tok = tokens.max().unwrap_or(1).max(1) as f64;
    let max_cost = costs.fold(0.0_f64, f64::max).max(1e-9);
    (max_tok, max_cost)
}

/// Label + bar column widths for token/cost list panes.
///
/// - `label_min <= 10` (model): clamp name to 10..=32
/// - otherwise (key): name grows with `max(label_min)` only
pub(crate) fn list_label_bar_widths(list_width: u16, label_min: usize) -> (usize, usize) {
    let w = list_width as usize;
    let label_w = if label_min <= 10 {
        w.saturating_sub(36).clamp(10, 32)
    } else {
        w.saturating_sub(36).max(label_min)
    };
    let bar_w = w.saturating_sub(label_w + 28).clamp(6, 20);
    (label_w, bar_w)
}

/// Render a scrollable token/cost list: bar + `tokens  $  Nr` metrics.
///
/// `label_spans(i)` builds the leading mark+name spans for absolute index `i`.
/// `metrics(i)` returns `(tokens, cost, requests)` for that row.
pub(crate) fn draw_token_cost_list(
    frame: &mut Frame,
    list_area: Rect,
    theme: &Theme,
    title: String,
    sel: usize,
    len: usize,
    bar_w: usize,
    bar_by_cost: bool,
    max_tok: f64,
    max_cost: f64,
    mut label_spans: impl FnMut(usize) -> Vec<Span<'static>>,
    mut metrics: impl FnMut(usize) -> (u64, f64, u64),
) {
    let block = panel_block(theme, title, true);
    let inner = block.inner(list_area);
    let visible = inner.height as usize;
    let start = list_window_start(sel, len, visible);
    let end = (start + visible).min(len);

    let mut lines = Vec::with_capacity(end.saturating_sub(start));
    for i in start..end {
        let (tok, cost, req) = metrics(i);
        let ratio = if bar_by_cost {
            cost / max_cost
        } else {
            tok as f64 / max_tok
        };
        let mut spans = label_spans(i);
        spans.push(Span::styled(
            ratio_bar(ratio, bar_w),
            Style::default().fg(theme.chart_color(i)),
        ));
        spans.push(Span::styled(
            format!(" {:>6}", format_tokens(tok)),
            theme.warning(),
        ));
        spans.push(Span::styled(
            format!("  {}  {}r", format_usd(cost), req),
            theme.muted(),
        ));
        lines.push(Line::from(spans));
    }
    frame.render_widget(block, list_area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Period token / cost share labels for detail panes.
pub(crate) fn period_shares(
    cost: f64,
    tok: u64,
    period_total: f64,
    period_tokens: u64,
) -> (String, String) {
    let tok_share = if period_tokens > 0 {
        tok as f64 / period_tokens as f64 * 100.0
    } else {
        0.0
    };
    let cost_share = if period_total > 0.0 {
        (cost / period_total) * 100.0
    } else {
        0.0
    };
    (
        format!("{tok_share:.1}% of period"),
        format!("{cost_share:.1}% of period"),
    )
}

pub(crate) fn models_for_key(app: &App, key_id: &str) -> Vec<ModelBreakRow> {
    model_break_rows(app, |s| {
        s.by_key_model
            .iter()
            .filter(|m| m.downstream_key_id == key_id)
            .map(|m| {
                (
                    m.label.as_str(),
                    m.provider_kind.as_str(),
                    m.request_count,
                    m.total_tokens,
                    m.total_usd,
                )
            })
    })
}

pub(crate) fn models_for_day(app: &App, day: &str) -> Vec<ModelBreakRow> {
    model_break_rows(app, |s| {
        s.by_day_model
            .iter()
            .filter(|m| m.day == day)
            .map(|m| {
                (
                    m.label.as_str(),
                    m.provider_kind.as_str(),
                    m.request_count,
                    m.total_tokens,
                    m.total_usd,
                )
            })
    })
}

/// Collect model-break rows from a usage summary slice.
fn model_break_rows<'a, I>(
    app: &'a App,
    pick: impl FnOnce(&'a crate::dto::UsageSummaryView) -> I,
) -> Vec<ModelBreakRow>
where
    I: Iterator<Item = (&'a str, &'a str, u64, u64, f64)>,
{
    app.usage_summary
        .as_ref()
        .map(|s| {
            pick(s)
                .map(|(label, provider_kind, request_count, total_tokens, total_usd)| {
                    ModelBreakRow {
                        label: label.to_string(),
                        provider_kind: provider_kind.to_string(),
                        request_count,
                        total_tokens,
                        total_usd,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone)]
pub(crate) struct ModelBreakRow {
    label: String,
    provider_kind: String,
    request_count: u64,
    total_tokens: u64,
    total_usd: f64,
}

pub(crate) fn draw_usage_rollup_detail(
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
