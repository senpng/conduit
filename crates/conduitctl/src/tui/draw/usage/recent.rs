//! Usage → Recent list + request detail pane.

use ratatui::layout::{Constraint, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

use super::super::super::app::App;
use super::super::super::theme::Theme;
use super::super::super::widgets::{
    detail_kv, empty_state, format_local_time, format_tok_per_sec, format_usd,
    panel_block, truncate,
};
use super::super::common::{fmt_tok, render_scrollable_table, wrapped_line_count};
use super::helpers::usage_master_detail;

pub(crate) fn draw_usage_recent(frame: &mut Frame, area: Rect, app: &mut App, sel: usize) {
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
            app.usage_offset / super::super::super::app::USAGE_PAGE_SIZE + 1
        };
        let pages = if total == 0 {
            0
        } else {
            (total as usize).div_ceil(super::super::super::app::USAGE_PAGE_SIZE)
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

/// Sort by-model rollup rows: (idx, label, provider, cost, tokens, reqs).
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
                "wire",
                u.wire_format
                    .clone()
                    .unwrap_or_else(|| "—".into()),
            ),
            (
                "loss",
                if u.loss_count == 0 {
                    "0".into()
                } else {
                    format!("{}  (fields in daemon logs)", u.loss_count)
                },
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
            ("tok/s", format_tok_per_sec(tps)),
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


