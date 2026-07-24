//! Usage tab — heatmap, rollups, and request detail.

mod heatmap;
mod helpers;
mod recent;
mod rollups;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::Frame;

use super::super::app::{App, Tab, UsageDetail};
use super::super::widgets::{format_tokens, format_usd};
use super::common::summary_total_tokens;

use heatmap::draw_usage_heatmap;
use recent::draw_usage_recent;
use rollups::{
    draw_usage_by_day, draw_usage_by_key, draw_usage_by_model, draw_usage_by_provider,
};

pub(crate) fn draw_usage(frame: &mut Frame, area: Rect, app: &mut App) {
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

    super::super::widgets::metric_strip(
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

