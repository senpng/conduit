//! Map crossterm key events → [`Action`] based on current UI mode.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::action::Action;
use super::app::{Mode, Tab};

pub fn map_key(mode: &Mode, tab: Tab, key: KeyEvent) -> Option<Action> {
    // Global interrupt always quits from browse; cancels overlays.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(match mode {
            Mode::Browse => Action::Quit,
            _ => Action::Cancel,
        });
    }

    match mode {
        Mode::Browse => map_browse(tab, key),
        Mode::Filter => map_filter(key),
        Mode::Help => match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
                Some(Action::Cancel)
            }
            _ => None,
        },
        Mode::Confirm(_) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => Some(Action::ConfirmYes),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(Action::ConfirmNo),
            _ => None,
        },
        Mode::Alert { .. } => match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Some(Action::Cancel),
            _ => None,
        },
        Mode::SecretReveal { .. } => match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Some(Action::Cancel),
            // y / c — full decrypted dump (Ctrl+c is global cancel)
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('c') => {
                Some(Action::CopySecretFull)
            }
            // a — primary only (api_key / access_token)
            KeyCode::Char('a') | KeyCode::Char('A') => Some(Action::CopySecretPrimary),
            _ => None,
        },
        Mode::ProviderAddChooser(_) => match key.code {
            KeyCode::Esc => Some(Action::Cancel),
            KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
            KeyCode::Enter | KeyCode::Char(' ') => Some(Action::Submit),
            KeyCode::Char('1') => Some(Action::ChooserPick(0)),
            KeyCode::Char('2') => Some(Action::ChooserPick(1)),
            KeyCode::Char('3') => Some(Action::ChooserPick(2)),
            KeyCode::Char('4') => Some(Action::ChooserPick(3)),
            _ => None,
        },
        Mode::ProviderForm(f) => map_form(key, f.focus, f.fields.len()),
        Mode::KeyForm(f) => map_form(key, f.focus, f.fields.len()),
        Mode::PricingOverrideForm(f) => map_form(key, f.focus, f.fields.len()),
        Mode::RouteWizard(w) => map_route_wizard(w.step, key),
        Mode::OauthFlow(flow) => map_oauth(flow.pending_session_id.is_some(), key),
    }
}

fn map_filter(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Enter => Some(Action::Cancel), // keep filter, exit edit mode
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Up | KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::Up)
        }
        KeyCode::Down | KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::Down)
        }
        KeyCode::Up => Some(Action::Up),
        KeyCode::Down => Some(Action::Down),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::Char(c))
        }
        _ => None,
    }
}

fn map_browse(tab: Tab, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Char('?') => Some(Action::Help),
        KeyCode::Tab => Some(Action::NextTab),
        KeyCode::BackTab => Some(Action::PrevTab),
        KeyCode::Char('1') => Some(Action::Tab(0)),
        KeyCode::Char('2') => Some(Action::Tab(1)),
        KeyCode::Char('3') => Some(Action::Tab(2)),
        KeyCode::Char('4') => Some(Action::Tab(3)),
        KeyCode::Char('5') => Some(Action::Tab(4)),
        KeyCode::Char('6') => Some(Action::Tab(5)),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::GoTop),
        KeyCode::End | KeyCode::Char('G') => Some(Action::GoBottom),
        KeyCode::Char('/') => Some(Action::StartFilter),
        KeyCode::Char('r') => Some(Action::Refresh),
        // Shift-T — cycle auto/dark/light (plain `t` is Usage detail).
        KeyCode::Char('T') => Some(Action::ToggleTheme),
        KeyCode::Char('a') => Some(Action::Add),
        // Usage / Pricing / Keys have right-hand detail panes — no Enter modal.
        KeyCode::Char('e') if tab != Tab::Usage => Some(Action::Edit),
        KeyCode::Enter if !matches!(tab, Tab::Usage | Tab::Pricing | Tab::Keys) => {
            Some(Action::Edit)
        }
        KeyCode::Char('d') => Some(Action::Delete),
        // Keys: reveal raw token in a modal (copy from there).
        KeyCode::Char('v') if tab == Tab::Keys => Some(Action::ViewSecret),
        KeyCode::Char('[') if tab == Tab::Usage => Some(Action::PeriodPrev),
        KeyCode::Char(']') if tab == Tab::Usage => Some(Action::PeriodNext),
        KeyCode::Char('R') if tab == Tab::Pricing => Some(Action::PricingReload),
        KeyCode::Char('s') if tab == Tab::Pricing => Some(Action::PricingSync),
        KeyCode::Char('o') if tab == Tab::Pricing => Some(Action::TogglePricingView),
        KeyCode::Char('s') if tab == Tab::Providers => Some(Action::SetSecret),
        KeyCode::Char('v') if tab == Tab::Providers => Some(Action::ViewSecret),
        // y / Y — copy decrypted secret while it is loaded in detail
        KeyCode::Char('y') if tab == Tab::Providers => Some(Action::CopySecretFull),
        KeyCode::Char('Y') if tab == Tab::Providers => Some(Action::CopySecretPrimary),
        KeyCode::Char('o') if tab == Tab::Providers => Some(Action::OpenBrowser), // re-auth OAuth
        KeyCode::Char('x') if tab == Tab::Providers => Some(Action::OauthRefresh),
        KeyCode::Char('u') if tab == Tab::Providers => Some(Action::RefreshQuota),
        KeyCode::Char('c') if tab == Tab::Usage => Some(Action::CycleUsageSort),
        KeyCode::Char('t') if tab == Tab::Usage => Some(Action::CycleUsageDetail),
        _ => None,
    }
}

fn map_form(key: KeyEvent, focus: usize, n_fields: usize) -> Option<Action> {
    let on_last = n_fields == 0 || focus + 1 >= n_fields;
    match key.code {
        KeyCode::Esc => Some(Action::Cancel),
        // Ctrl+Enter / Ctrl+s / F2 always submit (Ctrl+Enter is flaky on many terminals).
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Submit),
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Submit),
        KeyCode::F(2) => Some(Action::Submit),
        // Enter on last field saves; otherwise advances focus.
        KeyCode::Enter if on_last => Some(Action::Submit),
        KeyCode::Enter | KeyCode::Tab | KeyCode::Down => Some(Action::NextField),
        KeyCode::BackTab | KeyCode::Up => Some(Action::PrevField),
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Delete => Some(Action::DeleteChar),
        KeyCode::Left => Some(Action::Left),
        KeyCode::Right => Some(Action::Right),
        KeyCode::Home => Some(Action::Home),
        KeyCode::End => Some(Action::End),
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::CycleKind)
        }
        KeyCode::Char(c) => Some(Action::Char(c)),
        _ => None,
    }
}

fn map_route_wizard(step: usize, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::Submit)
        }
        KeyCode::F(2) => Some(Action::Submit),
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::WizardNextStep)
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::WizardPrevStep)
        }
        KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::WizardAddTarget)
        }
        KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::WizardRemoveTarget)
        }
        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::WizardCycleStrategy)
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Action::CycleKind)
        }
        // Step 1: ↑↓ select target; Tab toggles model/overrides field.
        KeyCode::Up if step == 1 => Some(Action::Up),
        KeyCode::Down if step == 1 => Some(Action::Down),
        KeyCode::Tab if step == 1 => Some(Action::NextField),
        KeyCode::BackTab if step == 1 => Some(Action::PrevField),
        KeyCode::Tab | KeyCode::Down if step != 1 => Some(Action::NextField),
        KeyCode::BackTab | KeyCode::Up if step != 1 => Some(Action::PrevField),
        // Review step: Enter saves; earlier steps: Enter advances.
        KeyCode::Enter if step >= 2 => Some(Action::Submit),
        KeyCode::Enter => Some(Action::WizardNextStep),
        KeyCode::Backspace => Some(Action::Backspace),
        KeyCode::Delete => Some(Action::DeleteChar),
        KeyCode::Left => Some(Action::Left),
        KeyCode::Right => Some(Action::Right),
        KeyCode::Home => Some(Action::Home),
        KeyCode::End => Some(Action::End),
        KeyCode::Char(c) => Some(Action::Char(c)),
        _ => None,
    }
}

fn map_oauth(pending: bool, key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Enter if !pending => Some(Action::Submit),
        KeyCode::Char('c') if pending => Some(Action::OauthCancel),
        KeyCode::Char('o') => Some(Action::OpenBrowser),
        KeyCode::Tab | KeyCode::Down => Some(Action::NextField),
        KeyCode::BackTab | KeyCode::Up => Some(Action::PrevField),
        KeyCode::Char(c) if !pending => Some(Action::Char(c)),
        KeyCode::Backspace if !pending => Some(Action::Backspace),
        KeyCode::Left if !pending => Some(Action::Left),
        KeyCode::Right if !pending => Some(Action::Right),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    use crate::tui::forms::{KeyForm, ProviderForm};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[test]
    fn browse_q_quits() {
        assert_eq!(
            map_key(&Mode::Browse, Tab::Overview, key(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
    }

    #[test]
    fn shift_t_toggles_theme() {
        assert_eq!(
            map_key(&Mode::Browse, Tab::Overview, key(KeyCode::Char('T'))),
            Some(Action::ToggleTheme)
        );
        // plain `t` is Usage-only detail cycle — not theme.
        assert_eq!(
            map_key(&Mode::Browse, Tab::Overview, key(KeyCode::Char('t'))),
            None
        );
    }

    #[test]
    fn keys_enter_does_not_edit() {
        // Detail pane shows the key; Enter no longer opens the edit modal.
        assert_eq!(
            map_key(&Mode::Browse, Tab::Keys, key(KeyCode::Enter)),
            None
        );
    }

    #[test]
    fn keys_v_reveals_secret() {
        assert_eq!(
            map_key(&Mode::Browse, Tab::Keys, key(KeyCode::Char('v'))),
            Some(Action::ViewSecret)
        );
    }

    #[test]
    fn keys_e_still_edits() {
        assert_eq!(
            map_key(&Mode::Browse, Tab::Keys, key(KeyCode::Char('e'))),
            Some(Action::Edit)
        );
    }

    #[test]
    fn number_keys_select_tab() {
        assert_eq!(
            map_key(&Mode::Browse, Tab::Overview, key(KeyCode::Char('3'))),
            Some(Action::Tab(2))
        );
        // Only 6 tabs — 7 no longer maps to OAuth
        assert_eq!(
            map_key(&Mode::Browse, Tab::Overview, key(KeyCode::Char('6'))),
            Some(Action::Tab(5))
        );
    }

    #[test]
    fn provider_add_chooser_numbers() {
        use crate::tui::forms::ProviderAddChooser;
        assert_eq!(
            map_key(
                &Mode::ProviderAddChooser(ProviderAddChooser::new()),
                Tab::Providers,
                key(KeyCode::Char('2'))
            ),
            Some(Action::ChooserPick(1))
        );
    }

    #[test]
    fn form_enter_on_last_field_submits() {
        let mut f = KeyForm::create();
        f.focus = f.fields.len() - 1;
        assert_eq!(
            map_key(&Mode::KeyForm(f), Tab::Keys, key(KeyCode::Enter)),
            Some(Action::Submit)
        );
    }

    #[test]
    fn form_enter_on_first_field_advances() {
        let f = KeyForm::create();
        assert_eq!(
            map_key(&Mode::KeyForm(f), Tab::Keys, key(KeyCode::Enter)),
            Some(Action::NextField)
        );
    }

    #[test]
    fn form_ctrl_s_submits() {
        let f = ProviderForm::create();
        assert_eq!(
            map_key(
                &Mode::ProviderForm(f),
                Tab::Providers,
                key_mod(KeyCode::Char('s'), KeyModifiers::CONTROL)
            ),
            Some(Action::Submit)
        );
    }

    #[test]
    fn provider_edit_enter_on_base_url_submits() {
        use crate::dto::ProviderView;
        let p = ProviderView {
            id: "x".into(),
            name: "n".into(),
            kind: "openai".into(),
            base_url: "https://x".into(),
            upstream_key_ref: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        };
        let mut f = ProviderForm::edit(&p);
        f.focus = 1; // base_url — last field
        assert_eq!(f.fields.len(), 2);
        assert_eq!(
            map_key(&Mode::ProviderForm(f), Tab::Providers, key(KeyCode::Enter)),
            Some(Action::Submit)
        );
    }
}
