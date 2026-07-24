//! Pricing tab — rate table and detail pane.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Row, Table, Wrap};
use ratatui::Frame;

use super::super::app::{App, PricingPane, Tab};
use super::super::theme::Theme;
use super::super::widgets::{
    detail_kv, empty_state, format_local_time, format_tokens, format_usd, panel_block, truncate,
};
use super::common::{render_scrollable_table, split_master_detail};

// ── Pricing ─────────────────────────────────────────────────────────────────

pub(crate) fn draw_pricing(frame: &mut Frame, area: Rect, app: &App) {
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
    // Guard against a stale filter cache (e.g. pane switched without
    // refresh_filtered): drop indices that no longer point into rows_src.
    let filtered: Vec<usize> = filtered
        .iter()
        .copied()
        .filter(|&i| i < rows_src.len())
        .collect();
    if filtered.is_empty() {
        empty_state(frame, area, theme, &title, "no pricing rows");
        return;
    }
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
                    super::super::widgets::spinner(app.tick_frame)
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


