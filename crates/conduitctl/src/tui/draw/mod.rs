//! Ratatui drawing — product-grade shell, master/detail, charts.

mod common;
mod lists;
mod modals;
mod overview;
mod pricing;
mod usage;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Frame;

use super::app::{App, Mode, Tab};
use super::widgets::{fill_bg, health_badge, keybind_line, modal, spinner, truncate};

use lists::{draw_master_detail_keys, draw_master_detail_providers, draw_master_detail_routes};
use modals::{
    draw_confirm, draw_help, draw_key_form, draw_oauth_flow, draw_pricing_override_form,
    draw_provider_add_chooser, draw_provider_form, draw_route_wizard,
};
use overview::draw_overview;
use pricing::draw_pricing;
use usage::draw_usage;

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
        Tab::Logs => super::logs::draw(frame, area, &app.logs, &app.theme, app.loading),
    }
}

#[cfg(test)]
mod list_window_tests {
    use super::common::list_window_start;

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
    use super::overview::stack_color_at;
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

#[cfg(test)]
mod split_name_bar_tests {
    use super::common::split_name_bar;

    #[test]
    fn wide_pane_matches_percentage_recipe() {
        // On a wide pane the split is the plain clamp(min_name, flex - min_bar).
        // flex=100, 40% → name 40, bar 60.
        assert_eq!(split_name_bar(100, 40, 8, 6), (40, 60));
        // flex=50, 45% → name 22, bar 28.
        assert_eq!(split_name_bar(50, 45, 12, 8), (22, 28));
    }

    #[test]
    fn narrow_pane_does_not_panic_and_pins_min_name() {
        // These are the exact widths that made the old `.clamp(min, flex-k)`
        // panic (min > max). name pins to min_name; bar takes what's left.
        // month/top-models sites: min_name=8, min_bar=6.
        for flex in 10..=13 {
            let (name, bar) = split_name_bar(flex, 40, 8, 6);
            assert_eq!(name, 8, "flex={flex}");
            assert_eq!(bar, flex - 8, "flex={flex}");
        }
        // provider-health site: min_name=12, min_bar=8.
        for flex in 16..=19 {
            let (name, bar) = split_name_bar(flex, 45, 12, 8);
            assert_eq!(name, 12, "flex={flex}");
            assert_eq!(bar, flex - 12, "flex={flex}");
        }
    }

    #[test]
    fn extreme_narrow_saturates_bar_to_zero() {
        // flex below min_name: name still pinned, bar saturates to 0 (no underflow).
        let (name, bar) = split_name_bar(5, 40, 8, 6);
        assert_eq!(name, 8);
        assert_eq!(bar, 0);
    }
}

