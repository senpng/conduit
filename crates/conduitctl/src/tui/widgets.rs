//! Reusable visual primitives for a denser, product-grade TUI.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

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
    let title = title.into();
    Block::default()
        .borders(Borders::ALL)
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

pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 1 {
        "…".into()
    } else {
        let t: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{t}…")
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

pub fn format_usd(v: f64) -> String {
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
