//! Master/detail lists for Providers, Routes, and Keys tabs.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Row, Table, Wrap};
use ratatui::Frame;

use super::super::app::{App, Tab};
use super::super::forms::{self as forms, ProviderForm};
use super::super::theme::Theme;
use super::super::widgets::{
    detail_kv, empty_state, format_local_time, format_local_time_short, panel_block, truncate,
};
use super::common::{render_scrollable_table, split_master_detail};

pub(crate) fn draw_master_detail_providers(frame: &mut Frame, area: Rect, app: &App) {
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

    let oauth = ProviderForm::is_oauth_kind_label(&p.kind);
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
    if ProviderForm::is_oauth_kind_label(&p.kind) {
        "…".into()
    } else {
        "—".into()
    }
}

fn provider_quota_detail_lines(
    theme: &Theme,
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
    } else if ProviderForm::is_oauth_kind_label(&p.kind) {
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

fn remaining_style(theme: &Theme, pct: f64) -> Style {
    if pct <= 10.0 {
        theme.error()
    } else if pct <= 25.0 {
        theme.warning()
    } else {
        theme.success()
    }
}

fn provider_secret_detail_lines(
    theme: &Theme,
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

pub(crate) fn draw_master_detail_routes(frame: &mut Frame, area: Rect, app: &App) {
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
                forms::summarize_route_targets(&r.targets_json, &app.providers);
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
    let summary = forms::summarize_route_targets(&r.targets_json, &app.providers);

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

pub(crate) fn draw_master_detail_keys(frame: &mut Frame, area: Rect, app: &App) {
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

