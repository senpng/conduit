//! Top-of-Usage contribution heatmap.

use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::super::super::app::{App, Tab, UsageDetail};
use super::super::super::widgets::{month_day_spends, panel_block};
use super::super::common::{
    contribution_graph_lines, usage_by_day_tuples, usage_trailing_tuples,
};
use super::rollups::usage_day_view_rows;

pub(crate) fn draw_usage_heatmap(frame: &mut Frame, area: Rect, app: &App) {
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
