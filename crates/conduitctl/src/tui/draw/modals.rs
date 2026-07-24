//! Overlay modals — help, confirm, forms, OAuth, route wizard.

use ratatui::layout::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::super::forms::{ConfirmAction, ProviderFormKind};
use super::super::theme::Theme;
use super::super::widgets::{modal, truncate};
use super::common::wrapped_line_count;

// ── Forms / modals ──────────────────────────────────────────────────────────

pub(crate) fn draw_provider_add_chooser(
    frame: &mut Frame,
    theme: &Theme,
    c: &super::super::forms::ProviderAddChooser,
) {
    use super::super::forms::PROVIDER_ADD_OPTIONS;
    let area = super::super::widgets::centered(frame.area(), 64, 50);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(" Add provider ", theme.title()))
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

/// Keyboard reference modal. Returns `(clamped_scroll, max_scroll)` — no wrap.
pub(crate) fn draw_help(frame: &mut Frame, theme: &Theme, scroll: u16) -> (u16, u16) {
    let body = "\
Global
  1-7 / Tab     Switch tab          j/k ↑↓     Move
  PgUp/PgDn     Page                g / G      Top / bottom
  /             Filter lists        r          Refresh
  T             Theme auto/dark/light (also CONDUIT_THEME=…)
  t             Home → Usage by-day calendar · on Usage cycles detail
  ?             Help                q          Quit
  j/k ↑↓        Scroll this help    PgUp/PgDn  Page this help
  g / G         Help top / bottom   Esc/?/q    Close help

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
  a add · e edit · d delete · v show token
  (right pane is detail; no Enter modal · v reveals raw token to copy)

Usage
  [ ] month · c sort · t cycle list · / filter
  Daily spend = GitHub-style heat calendar (cost intensity)
  By day: ↑↓ select day cells · zero days still listed
  Recent: PgUp/PgDn page · g/G first/last page
  Recent detail: Ctrl+j / Ctrl+k scroll (no wrap)
  (right pane is detail; no Enter modal)

Pricing
  o             Toggle merged ↔ overrides pane
  Merged:       R reload · s sync LiteLLM  (read-only list)
  Overrides:    a add · e edit · d delete  (writes pricing.json)

Logs
  f             Toggle live ↔ history
  l             Cycle level floor (error/warn/info/debug/trace)
  [ ]           Previous / next day (history)
  /             Substring filter (server-side)
  j/k           Scroll (up leaves sticky follow in live)
  G             Jump bottom + restore follow
  g             Jump top (pauses follow)
  y             Copy selected raw line
  c             Clear live buffer (not the file)
  r             Reload / reconnect stream

OAuth flow (from Providers → add / re-auth)
  Enter start · o open URL · c cancel pending · Esc close

Forms
  Tab fields · Enter on last = save · Ctrl-s / F2 save · Esc cancel
";
    let lines: Vec<Line> = body
        .lines()
        .map(|l| Line::from(l.to_string()))
        .collect();

    // Slightly taller than generic modals so more of the reference fits.
    let area = super::super::widgets::centered(frame.area(), 74, 70);
    frame.render_widget(Clear, area);

    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let inner_h = area.height.saturating_sub(2) as usize;
    let total = wrapped_line_count(&lines, inner_w);
    let max_scroll = total.saturating_sub(inner_h) as u16;
    let scroll = scroll.min(max_scroll);
    let title = if max_scroll == 0 {
        "Keyboard reference".into()
    } else if scroll == 0 {
        format!("Keyboard reference  ↓j  +{max_scroll}")
    } else if scroll >= max_scroll {
        "Keyboard reference  ↑k".into()
    } else {
        format!(
            "Keyboard reference  j/k  {}/{}",
            scroll + 1,
            max_scroll + 1
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(format!(" {title} "), theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .style(theme.surface()),
        inner,
    );
    (scroll, max_scroll)
}

pub(crate) fn draw_confirm(frame: &mut Frame, theme: &Theme, action: &ConfirmAction) {
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
    fields: &[super::super::input::InputField],
    focus: usize,
    error: Option<&str>,
    footer: &str,
) {
    let area = super::super::widgets::centered(frame.area(), 70, 62);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(format!(" {title} "), theme.title()))
        .style(theme.surface());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = vec![Line::from("")];
    for (i, label) in labels.iter().enumerate() {
        let focused = i == focus;
        let marker = if focused { "▸" } else { " " };
        // Focused label: accent so which field is active is obvious.
        let label_style = if focused {
            theme.accent_bold()
        } else {
            theme.muted()
        };
        lines.push(Line::from(Span::styled(
            format!(" {marker} {label}"),
            label_style,
        )));

        // Value line: caret at InputField.cursor (not always at end).
        let base = if focused {
            Style::default()
                .fg(theme.fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg).bg(theme.surface)
        };
        let caret =
            super::super::input::InputField::caret_style(theme.accent, theme.background);
        let mut value_spans = vec![Span::styled("   ", base)];
        if let Some(field) = fields.get(i) {
            value_spans.extend(field.line_spans(base, caret, focused).spans);
        }
        // Soft pad after the value so the selection bar is easy to spot.
        if focused {
            value_spans.push(Span::styled("  ", base));
        }
        lines.push(Line::from(value_spans));
        lines.push(Line::from(""));
    }
    if let Some(err) = error {
        lines.push(Line::from(Span::styled(format!("  ! {err}"), theme.error())));
    }
    lines.push(Line::from(Span::styled(format!("  {footer}"), theme.subtle())));
    frame.render_widget(Paragraph::new(lines), inner);
}

pub(crate) fn draw_provider_form(frame: &mut Frame, theme: &Theme, f: &super::super::forms::ProviderForm) {
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

pub(crate) fn draw_key_form(frame: &mut Frame, theme: &Theme, f: &super::super::forms::KeyForm) {
    let labels = f.labels();
    let hint = if f.is_edit() {
        "Tab next · Enter on last = save · Ctrl-s save · Ctrl-k toggle enabled · Esc cancel"
    } else {
        "Tab next · Enter on last = save · Ctrl-s save · Esc cancel"
    };
    form_modal(
        frame,
        theme,
        &f.title(),
        &labels,
        &f.fields,
        f.focus,
        f.error.as_deref(),
        hint,
    );
}

pub(crate) fn draw_pricing_override_form(
    frame: &mut Frame,
    theme: &Theme,
    f: &super::super::forms::PricingOverrideForm,
) {
    let labels = super::super::forms::PricingOverrideForm::labels();
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

pub(crate) fn draw_route_wizard(frame: &mut Frame, theme: &Theme, w: &super::super::forms::RouteWizard) {
    let area = super::super::widgets::centered(frame.area(), 78, 70);
    frame.render_widget(Clear, area);
    let title = format!(
        "Route wizard · step {}/3 · {}",
        w.step + 1,
        if w.edit_id.is_some() { "edit" } else { "create" }
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_active())
        .title_top(Span::styled(format!(" {title} "), theme.title()))
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
            let base = Style::default()
                .fg(theme.fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD);
            let caret =
                super::super::input::InputField::caret_style(theme.accent, theme.background);
            let mut alias_spans = vec![Span::styled("  ▸ match_alias: ", theme.accent_bold())];
            alias_spans.extend(w.match_alias.line_spans(base, caret, true).spans);
            alias_spans.push(Span::styled("  ", base));
            lines.push(Line::from(alias_spans));
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
                let selected = i == w.target_focus;
                let mark = if selected { "▸" } else { " " };
                let pool_mark = if t.binding.is_pool() { "◇" } else { "·" };
                let row_style = if selected {
                    theme.accent_bold()
                } else {
                    Style::default().fg(theme.fg)
                };
                lines.push(Line::from(Span::styled(
                    format!(" {mark} [{i}] {pool_mark} {label}"),
                    row_style,
                )));

                // Active target: show real caret on the field being edited.
                if selected {
                    let base = Style::default()
                        .fg(theme.fg)
                        .bg(theme.selection_bg)
                        .add_modifier(Modifier::BOLD);
                    let caret =
                        super::super::input::InputField::caret_style(theme.accent, theme.background);
                    let edit_model = w.field_in_target == 0;
                    let mut model_spans = vec![Span::styled(
                        "     model=",
                        if edit_model {
                            theme.accent_bold()
                        } else {
                            theme.muted()
                        },
                    )];
                    model_spans.extend(
                        t.model_id
                            .line_spans(base, caret, edit_model)
                            .spans,
                    );
                    lines.push(Line::from(model_spans));

                    let mut ov_spans = vec![Span::styled(
                        "     overrides=",
                        if !edit_model {
                            theme.accent_bold()
                        } else {
                            theme.muted()
                        },
                    )];
                    // Show caret on full overrides field (not truncated) so position is real.
                    ov_spans.extend(
                        t.overrides
                            .line_spans(base, caret, !edit_model)
                            .spans,
                    );
                    lines.push(Line::from(ov_spans));
                } else {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "     model={}  overrides={}",
                            t.model_id.display(),
                            truncate(&t.overrides.display(), 40)
                        ),
                        Style::default().fg(theme.fg),
                    )));
                }
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

pub(crate) fn draw_oauth_flow(frame: &mut Frame, theme: &Theme, f: &super::super::forms::OauthFlow) {
    let area = super::super::widgets::centered(frame.area(), 72, 62);
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
        .title_top(Span::styled(title, theme.title()))
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
        let name_focused = f.focus == 1;
        let marker = if name_focused { "▸" } else { " " };
        let base = if name_focused {
            Style::default()
                .fg(theme.fg)
                .bg(theme.selection_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg)
        };
        let caret =
            super::super::input::InputField::caret_style(theme.accent, theme.background);
        let mut name_spans = vec![
            Span::styled(
                format!("  {marker} name     "),
                if name_focused {
                    theme.accent_bold()
                } else {
                    theme.muted()
                },
            ),
        ];
        name_spans.extend(f.name.line_spans(base, caret, name_focused).spans);
        if name_focused {
            name_spans.push(Span::styled("  ", base));
        }
        lines.push(Line::from(name_spans));
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

