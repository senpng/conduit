//! Ratatui drawing — product-grade shell, master/detail, charts.

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    BarChart, Block, Borders, Cell, Clear, Paragraph, Row, Sparkline, Table, Tabs, Wrap,
};
use ratatui::Frame;

use super::app::{App, Mode, PricingPane, Tab, UsageDetail};
use super::forms::{ConfirmAction, ProviderFormKind};
use super::theme::Theme;
use super::widgets::{
    detail_kv, empty_state, fill_bg, format_local_time, format_local_time_short, format_tokens,
    format_usd, health_badge, keybind_line, modal, panel_block, ratio_bar, spinner, truncate,
};

pub fn draw(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    fill_bg(frame, frame.area(), theme);

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

    match &app.mode {
        Mode::Browse | Mode::Filter => {}
        Mode::Help => draw_help(frame, theme),
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
        Mode::SecretReveal { title, secret } => {
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
            lines.push(Line::from(Span::styled(
                "y/c copy full · a copy token/key · Enter/Esc close",
                theme.muted(),
            )));
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
    frame.render_widget(
        Paragraph::new(keybind_line(theme, &binds)).style(theme.base()),
        area,
    );
}

fn draw_body(frame: &mut Frame, area: Rect, app: &App) {
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
    let theme = &app.theme;
    let cols = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(10),
            Constraint::Min(4),
        ])
        .split(area);

    // Metric strip
    let health = app
        .health
        .as_ref()
        .map(|h| h.status.clone())
        .unwrap_or_else(|| "down".into());
    let (cost, reqs, period, spend_style) = match app.usage_summary.as_ref() {
        Some(s) => (
            format_usd(s.total_usd),
            s.request_count.to_string(),
            s.period.clone(),
            theme.success(),
        ),
        None => (
            "…".into(),
            "…".into(),
            app.usage_period.clone(),
            theme.muted(),
        ),
    };

    super::widgets::metric_strip(
        frame,
        cols[0],
        theme,
        &[
            ("HEALTH", health, if app.daemon_ok() { theme.success() } else { theme.error() }),
            ("PROVIDERS", app.providers.len().to_string(), theme.accent_bold()),
            ("ROUTES", app.routes.len().to_string(), theme.accent_bold()),
            (
                "KEYS",
                app.keys.len().to_string(),
                theme.accent_bold(),
            ),
            ("SPEND", format!("{cost} · {period}"), spend_style),
            ("REQS", reqs, theme.warning()),
        ],
    );

    // Daily sparkline + top models
    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(cols[1]);

    draw_overview_sparkline(frame, mid[0], app);
    draw_overview_top_models(frame, mid[1], app);

    // Quick start + resource list
    draw_overview_quickstart(frame, cols[2], app);
}

fn draw_overview_sparkline(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = panel_block(theme, "Spend pulse (by day)", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let data: Vec<u64> = app
        .usage_summary
        .as_ref()
        .map(|s| {
            s.by_day
                .iter()
                .map(|d| ((d.total_usd * 10_000.0).round() as u64).max(1))
                .collect()
        })
        .unwrap_or_default();

    if data.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "  no usage yet this period — send traffic through the gateway",
                theme.subtle(),
            )),
            inner,
        );
        return;
    }

    let spark = Sparkline::default()
        .data(&data)
        .style(Style::default().fg(theme.accent));
    frame.render_widget(spark, inner);
}

fn draw_overview_top_models(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = panel_block(theme, "Top models", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let models = app
        .usage_summary
        .as_ref()
        .map(|s| s.by_model.as_slice())
        .unwrap_or(&[]);
    if models.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("  —", theme.subtle())),
            inner,
        );
        return;
    }
    let max = models
        .iter()
        .map(|m| m.total_usd)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let bar_w = (inner.width as usize).saturating_sub(28).clamp(6, 20);
    let lines: Vec<Line> = models
        .iter()
        .take(inner.height as usize)
        .enumerate()
        .map(|(i, m)| {
            Line::from(vec![
                Span::styled(
                    format!("{:<14}", truncate(&m.label, 14)),
                    Style::default().fg(theme.chart_color(i)),
                ),
                Span::styled(
                    ratio_bar(m.total_usd / max, bar_w),
                    Style::default().fg(theme.chart_color(i)),
                ),
                Span::styled(format!(" {}", format_usd(m.total_usd)), theme.muted()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_overview_quickstart(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = panel_block(theme, "Operator path", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = vec![
        Line::from(vec![
            Span::styled("  1. ", theme.accent_bold()),
            Span::raw("Providers  "),
            Span::styled("a", theme.key_hint()),
            Span::styled(" add via API key or OAuth (Claude / Codex / Grok)", theme.muted()),
        ]),
        Line::from(vec![
            Span::styled("  2. ", theme.accent_bold()),
            Span::raw("Routes     "),
            Span::styled("a", theme.key_hint()),
            Span::styled(" wizard maps client model → upstream", theme.muted()),
        ]),
        Line::from(vec![
            Span::styled("  3. ", theme.accent_bold()),
            Span::raw("Keys       "),
            Span::styled("a", theme.key_hint()),
            Span::styled(" issue downstream bearer for clients", theme.muted()),
        ]),
        Line::from(vec![
            Span::styled("  4. ", theme.accent_bold()),
            Span::raw("Usage      "),
            Span::styled("watch spend · ", theme.muted()),
            Span::styled("6", theme.key_hint()),
            Span::styled(" Pricing overrides", theme.muted()),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "  {} providers · {} routes · {} keys · filter lists with /",
                app.providers.len(),
                app.routes.len(),
                app.keys.len()
            ),
            theme.subtle(),
        )),
    ];
    frame.render_widget(Paragraph::new(text), inner);
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

    let header = Row::new(vec!["NAME", "KIND", "REMAINING", "ID"]).style(theme.header_cell());
    let rows: Vec<Row> = filtered
        .iter()
        .enumerate()
        .map(|(view_i, &data_i)| {
            let p = &app.providers[data_i];
            let remaining = provider_remaining_cell(app, p);
            let row = Row::new(vec![
                truncate(&p.name, 20),
                truncate(&p.kind, 12),
                truncate(&remaining, 18),
                truncate(&p.id, 10),
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
            Constraint::Percentage(32),
            Constraint::Percentage(20),
            Constraint::Percentage(28),
            Constraint::Percentage(20),
        ],
    )
    .header(header)
    .block(panel_block(
        theme,
        format!("Providers  {}/{}", sel + 1, filtered.len()),
        true,
    ))
    .column_spacing(1);
    frame.render_widget(table, list_area);

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
                truncate(&r.match_alias, 18),
                truncate(&r.strategy, 9),
                truncate(&summary, 28),
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
            Constraint::Percentage(28),
            Constraint::Length(9),
            Constraint::Min(18),
        ],
    )
    .header(
        Row::new(vec!["", "ALIAS", "STRAT", "TARGETS"]).style(theme.header_cell()),
    )
    .block(panel_block(
        theme,
        format!("Routes  {}/{}", sel + 1, filtered.len()),
        true,
    ));
    frame.render_widget(table, list_area);

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
                truncate(&k.id, 12),
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
            Constraint::Percentage(45),
            Constraint::Length(8),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(vec!["", "NAME", "RPM", "ID"]).style(theme.header_cell()))
    .block(panel_block(
        theme,
        format!("Keys  {}/{}", sel + 1, filtered.len()),
        true,
    ));
    frame.render_widget(table, list_area);

    let k = &app.keys[filtered[sel]];
    let lines = detail_kv(
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
    frame.render_widget(
        Paragraph::new(lines).block(panel_block(theme, "Detail", false)),
        detail_area,
    );
}

// ── Usage dashboard ─────────────────────────────────────────────────────────

fn draw_usage(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(9),
            Constraint::Min(5),
        ])
        .split(area);

    let (period, total_usd, requests, tokens) = if let Some(s) = &app.usage_summary {
        let tokens: u64 = s.entries.iter().map(|e| e.total_tokens).sum();
        let tokens = if tokens > 0 {
            tokens
        } else {
            s.by_day.iter().map(|d| d.total_tokens).sum()
        };
        (s.period.clone(), s.total_usd, s.request_count, tokens)
    } else {
        (app.usage_period.clone(), 0.0, 0, 0)
    };

    super::widgets::metric_strip(
        frame,
        chunks[0],
        theme,
        &[
            ("PERIOD", period, theme.accent_bold()),
            ("COST", format_usd(total_usd), theme.success()),
            ("REQUESTS", requests.to_string(), theme.warning()),
            ("TOKENS", format_tokens(tokens), Style::default().fg(theme.chart[4])),
        ],
    );

    // Bar chart for daily cost
    draw_usage_barchart(frame, chunks[1], app);
    draw_usage_detail(frame, chunks[2], app);
}

fn draw_usage_barchart(frame: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let days = app
        .usage_summary
        .as_ref()
        .map(|s| s.by_day.as_slice())
        .unwrap_or(&[]);

    let block = panel_block(
        theme,
        format!(
            "Daily cost  ·  sort={}  detail={}  [ ] month · / filter",
            app.usage_sort.label(),
            app.usage_detail.label()
        ),
        true,
    );
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if days.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("  no daily data", theme.subtle())),
            inner,
        );
        return;
    }

    // BarChart wants (&str, u64) with static-ish labels — use owned vec of labels.
    let take = days.len().min(14);
    let start = days.len().saturating_sub(take);
    let slice = &days[start..];
    let labels: Vec<String> = slice
        .iter()
        .map(|d| {
            if d.day.len() >= 10 {
                d.day[8..10].to_string() // day-of-month
            } else {
                d.day.clone()
            }
        })
        .collect();
    let data: Vec<(&str, u64)> = labels
        .iter()
        .zip(slice.iter())
        .map(|(l, d)| (l.as_str(), ((d.total_usd * 1000.0).round() as u64).max(1)))
        .collect();

    let chart = BarChart::default()
        .data(&data)
        .bar_width(3)
        .bar_gap(1)
        .bar_style(Style::default().fg(theme.accent))
        .value_style(Style::default().fg(theme.fg).bg(theme.accent_dim))
        .label_style(theme.subtle());
    frame.render_widget(chart, inner);
}

/// Shared list + right-hand detail split used by every Usage pane.
fn usage_master_detail(area: Rect) -> (Rect, Rect, bool) {
    let parts = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if area.width >= 120 {
            [Constraint::Min(70), Constraint::Length(44)]
        } else if area.width >= 90 {
            [Constraint::Percentage(68), Constraint::Percentage(32)]
        } else {
            [Constraint::Percentage(100), Constraint::Length(0)]
        })
        .split(area);
    let show = parts[1].width >= 28;
    (parts[0], parts[1], show)
}

fn draw_usage_detail(frame: &mut Frame, area: Rect, app: &App) {
    let sel = app.selected[Tab::Usage.index()];
    let period_total = app
        .usage_summary
        .as_ref()
        .map(|s| s.total_usd)
        .unwrap_or(0.0)
        .max(1e-12);

    match app.usage_detail {
        UsageDetail::Recent => draw_usage_recent(frame, area, app, sel),
        UsageDetail::ByModel => draw_usage_by_model(frame, area, app, sel, period_total),
        UsageDetail::ByKey => draw_usage_by_key(frame, area, app, sel, period_total),
        UsageDetail::ByDay => draw_usage_by_day(frame, area, app, sel, period_total),
    }
}

fn draw_usage_recent(frame: &mut Frame, area: Rect, app: &App, sel: usize) {
    let theme = &app.theme;
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
        empty_state(frame, area, theme, &title, hint);
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
    frame.render_widget(table, list_area);

    if show_detail {
        if let Some(u) = app.usage_recent.get(sel) {
            draw_usage_record_detail(frame, detail_area, theme, u, &app.pricing);
        }
    }
}

fn draw_usage_by_model(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sel: usize,
    period_total: f64,
) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    let mut items: Vec<(usize, String, String, f64, u64, u64)> = app
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
    let max_cost = items
        .iter()
        .map(|x| x.3)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let label_w = (list_area.width as usize)
        .saturating_sub(28)
        .clamp(12, 36);
    let bar_w = (list_area.width as usize)
        .saturating_sub(label_w + 22)
        .clamp(6, 24);

    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, (_, label, _, cost, _tok, req))| {
            let mark = if i == sel { "▶ " } else { "  " };
            Line::from(vec![
                Span::styled(
                    format!("{mark}{:<w$}", truncate(label, label_w), w = label_w),
                    if i == sel {
                        theme.accent_bold()
                    } else {
                        Style::default().fg(theme.fg)
                    },
                ),
                Span::styled(
                    ratio_bar(cost / max_cost, bar_w),
                    Style::default().fg(theme.chart_color(i)),
                ),
                Span::styled(
                    format!(" {}  {}r", format_usd(*cost), req),
                    theme.muted(),
                ),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(panel_block(theme, "By model", true)),
        list_area,
    );

    if show_detail {
        let (label, provider, cost, tok, req) = (
            &items[sel].1,
            &items[sel].2,
            items[sel].3,
            items[sel].4,
            items[sel].5,
        );
        let share = (cost / period_total) * 100.0;
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
                ("requests", req.to_string()),
                ("tokens", format_tokens(tok)),
                ("cost", format_usd(cost)),
                ("share", format!("{share:.1}% of period")),
            ],
            cost / max_cost,
            &[], // model pane is already the list
        );
    }
}

fn draw_usage_by_key(frame: &mut Frame, area: Rect, app: &App, sel: usize, period_total: f64) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    // (display_name, key_id, cost, tokens, reqs, prompt, completion)
    let mut items: Vec<(String, String, f64, u64, u64, u64, u64)> = app
        .usage_summary
        .as_ref()
        .map(|s| {
            filtered
                .iter()
                .filter_map(|&i| {
                    s.entries.get(i).map(|e| {
                        let id = e.downstream_key_id.clone();
                        let name = resolve_key_name(&app.keys, &id);
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
            items.sort_by(|a, b| a.0.cmp(&b.0))
        }
    }
    let sel = sel.min(items.len().saturating_sub(1));
    let (list_area, detail_area, show_detail) = usage_master_detail(area);
    let max_cost = items
        .iter()
        .map(|x| x.2)
        .fold(0.0_f64, f64::max)
        .max(1e-9);
    let label_w = (list_area.width as usize)
        .saturating_sub(28)
        .clamp(12, 40);
    let bar_w = (list_area.width as usize)
        .saturating_sub(label_w + 22)
        .clamp(6, 24);

    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, (name, _id, cost, _, req, _, _))| {
            let mark = if i == sel { "▶ " } else { "  " };
            Line::from(vec![
                Span::styled(
                    format!("{mark}{:<w$}", truncate(name, label_w), w = label_w),
                    if i == sel {
                        theme.accent_bold()
                    } else {
                        Style::default().fg(theme.fg)
                    },
                ),
                Span::styled(
                    ratio_bar(cost / max_cost, bar_w),
                    Style::default().fg(theme.chart_color(i)),
                ),
                Span::styled(
                    format!(" {}  {}r", format_usd(*cost), req),
                    theme.muted(),
                ),
            ])
        })
        .collect();
    frame.render_widget(
        Paragraph::new(lines).block(panel_block(theme, "By key", true)),
        list_area,
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
        let share = (cost / period_total) * 100.0;
        let heading = if id.is_empty() {
            name.clone()
        } else if name == id || name.starts_with("(deleted)") {
            name.clone()
        } else {
            name.clone()
        };
        let models = models_for_key(app, id);
        draw_usage_rollup_detail(
            frame,
            detail_area,
            theme,
            "Key detail",
            &heading,
            &[
                ("name", name.clone()),
                (
                    "key id",
                    if id.is_empty() {
                        "—".into()
                    } else {
                        id.clone()
                    },
                ),
                ("requests", req.to_string()),
                ("prompt tok", prompt.to_string()),
                ("completion", completion.to_string()),
                ("total tok", format_tokens(tok)),
                ("cost", format_usd(cost)),
                ("share", format!("{share:.1}% of period")),
            ],
            cost / max_cost,
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

/// Map downstream key id → human name from the Keys list.
fn resolve_key_name(keys: &[crate::dto::KeyView], id: &str) -> String {
    if id.is_empty() {
        return "(anonymous)".into();
    }
    if let Some(k) = keys.iter().find(|k| k.id == id) {
        if k.name.is_empty() {
            id.to_string()
        } else {
            k.name.clone()
        }
    } else {
        // Key may have been revoked; still show a readable label.
        format!("(deleted) {}", truncate(id, 12))
    }
}

fn draw_usage_by_day(frame: &mut Frame, area: Rect, app: &App, sel: usize, period_total: f64) {
    let theme = &app.theme;
    let filtered = app.filtered_indices();
    let mut days: Vec<(String, u64, u64, f64)> = app
        .usage_summary
        .as_ref()
        .map(|s| {
            filtered
                .iter()
                .filter_map(|&i| {
                    s.by_day.get(i).map(|d| {
                        (
                            d.day.clone(),
                            d.request_count,
                            d.total_tokens,
                            d.total_usd,
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if days.is_empty() {
        let hint = if !app.filter.is_empty() {
            "no matches for filter"
        } else {
            "no daily rows"
        };
        empty_state(frame, area, theme, "By day", hint);
        return;
    }
    // Keep chronological by default for day view unless cost/token sort.
    match app.usage_sort {
        super::app::UsageSort::Cost => {
            days.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal))
        }
        super::app::UsageSort::Tokens => days.sort_by(|a, b| b.2.cmp(&a.2)),
        super::app::UsageSort::Date => days.sort_by(|a, b| a.0.cmp(&b.0)),
    }
    let sel = sel.min(days.len().saturating_sub(1));
    let (list_area, detail_area, show_detail) = usage_master_detail(area);
    let max_cost = days
        .iter()
        .map(|d| d.3)
        .fold(0.0_f64, f64::max)
        .max(1e-9);

    let rows: Vec<Row> = days
        .iter()
        .enumerate()
        .map(|(i, (day, req, tok, cost))| {
            let row = Row::new(vec![
                day.clone(),
                req.to_string(),
                format_tokens(*tok),
                format_usd(*cost),
                format!("{:.0}%", (cost / max_cost) * 100.0),
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
            Constraint::Length(12),
            Constraint::Length(7),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(5),
        ],
    )
    .header(
        Row::new(vec!["DAY", "REQS", "TOKENS", "COST", "%"]).style(theme.header_cell()),
    )
    .block(panel_block(theme, "By day", true));
    frame.render_widget(table, list_area);

    if show_detail {
        let (day, req, tok, cost) = (
            &days[sel].0,
            days[sel].1,
            days[sel].2,
            days[sel].3,
        );
        let share = (cost / period_total) * 100.0;
        let models = models_for_day(app, day);
        draw_usage_rollup_detail(
            frame,
            detail_area,
            theme,
            "Day detail",
            day,
            &[
                ("day", day.clone()),
                ("requests", req.to_string()),
                ("tokens", format_tokens(tok)),
                ("cost", format_usd(cost)),
                ("share", format!("{share:.1}% of period")),
                (
                    "avg $/req",
                    if req > 0 {
                        format_usd(cost / req as f64)
                    } else {
                        "—".into()
                    },
                ),
            ],
            cost / max_cost,
            &models,
        );
    }
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

    // Per-model breakdown for this key/day.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Models", theme.title())));
    if models.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (no model breakdown)",
            theme.subtle(),
        )));
    } else {
        let max_m = models
            .iter()
            .map(|m| m.total_usd)
            .fold(0.0_f64, f64::max)
            .max(1e-9);
        let bar_w = (area.width as usize).saturating_sub(20).clamp(6, 16);
        // Cap rows so the panel stays scannable; sort by cost desc.
        let mut ordered: Vec<&ModelBreakRow> = models.iter().collect();
        ordered.sort_by(|a, b| {
            b.total_usd
                .partial_cmp(&a.total_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
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
                    format!("  {}", ratio_bar(m.total_usd / max_m, bar_w)),
                    Style::default().fg(theme.chart_color(i)),
                ),
                Span::styled(
                    format!(
                        " {}  {}r  {}{prov}",
                        format_usd(m.total_usd),
                        m.request_count,
                        format_tokens(m.total_tokens),
                    ),
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
    items: &mut [(usize, String, String, f64, u64, u64)],
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

fn draw_usage_record_detail(
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
    u: &crate::dto::UsageRecordView,
    pricing: &[crate::dto::PricingView],
) {
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
    lines.extend(detail_kv(
        theme,
        &[
            ("cost", format_usd(u.cost_usd)),
            ("stream", if u.stream { "yes" } else { "no" }.into()),
            (
                "alias",
                u.alias.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "provider",
                u.provider_kind.clone().unwrap_or_else(|| "—".into()),
            ),
            (
                "key",
                u.downstream_key_id
                    .as_deref()
                    .map(|s| truncate(s, 20))
                    .unwrap_or_else(|| "—".into()),
            ),
            ("request_id", truncate(&u.request_id, 22)),
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

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(panel_block(theme, "Request detail", true)),
        area,
    );
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
    frame.render_widget(table, list_area);

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
        .title(Span::styled(" Add provider ", theme.title()))
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

fn draw_help(frame: &mut Frame, theme: &Theme) {
    let body = "\
Global
  1-6 / Tab     Switch tab          j/k ↑↓     Move
  PgUp/PgDn     Page                g / G      Top / bottom
  /             Filter lists        r          Refresh
  ?             Help                q          Quit

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
  a add · e edit · d delete

Usage
  [ ] month · c sort · t cycle list · / filter
  Recent: PgUp/PgDn page · g/G first/last page
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
    modal(
        frame,
        theme,
        "Keyboard reference",
        body.lines().map(|l| Line::from(l.to_string())).collect(),
        theme.border_active(),
    );
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
        .title(Span::styled(format!(" {title} "), theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from("")];
    for (i, label) in labels.iter().enumerate() {
        let focused = i == focus;
        let val = fields.get(i).map(|x| x.display()).unwrap_or_default();
        let marker = if focused { "▸" } else { " " };
        let style = if focused {
            theme.accent_bold()
        } else {
            Style::default().fg(theme.fg)
        };
        lines.push(Line::from(Span::styled(
            format!(" {marker} {label}"),
            theme.muted(),
        )));
        lines.push(Line::from(Span::styled(
            format!("   {val}{}", if focused { "▌" } else { "" }),
            style,
        )));
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
    let labels = super::forms::KeyForm::labels();
    form_modal(
        frame,
        theme,
        "Create downstream key",
        &labels,
        &f.fields,
        f.focus,
        f.error.as_deref(),
        "Tab next · Enter on last = save · Esc cancel",
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
        .title(Span::styled(format!(" {title} "), theme.title()))
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
            lines.push(Line::from(Span::styled(
                format!("  ▸ match_alias: {}▌", w.match_alias.display()),
                theme.accent_bold(),
            )));
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
                let mark = if i == w.target_focus { "▸" } else { " " };
                let pool_mark = if t.binding.is_pool() { "◇" } else { "·" };
                lines.push(Line::from(Span::styled(
                    format!(
                        " {mark} [{i}] {pool_mark} {label}\n     model={}  overrides={}",
                        t.model_id.display(),
                        truncate(&t.overrides.display(), 40)
                    ),
                    if i == w.target_focus {
                        theme.accent_bold()
                    } else {
                        Style::default().fg(theme.fg)
                    },
                )));
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
        .title(Span::styled(title, theme.title()))
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
        let name_focus = f.focus == 1 || (!reauth && f.focus != 2);
        lines.push(Line::from(Span::styled(
            format!(
                "  {} name     {}{}",
                if f.focus == 1 { "▸" } else { " " },
                f.name.display(),
                if f.focus == 1 { "▌" } else { "" }
            ),
            if f.focus == 1 {
                theme.accent_bold()
            } else {
                Style::default().fg(theme.fg)
            },
        )));
        let _ = name_focus;
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
