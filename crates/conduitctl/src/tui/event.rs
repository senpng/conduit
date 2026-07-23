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
            // Scroll the keyboard reference (no wrap at ends).
            KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
            KeyCode::PageUp => Some(Action::PageUp),
            KeyCode::PageDown => Some(Action::PageDown),
            KeyCode::Home | KeyCode::Char('g') => Some(Action::GoTop),
            KeyCode::End | KeyCode::Char('G') => Some(Action::GoBottom),
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
    // Shared chrome (all tabs including Logs).
    match key.code {
        KeyCode::Char('q') => return Some(Action::Quit),
        KeyCode::Char('?') => return Some(Action::Help),
        KeyCode::Tab => return Some(Action::NextTab),
        KeyCode::BackTab => return Some(Action::PrevTab),
        KeyCode::Char('1') => return Some(Action::Tab(0)),
        KeyCode::Char('2') => return Some(Action::Tab(1)),
        KeyCode::Char('3') => return Some(Action::Tab(2)),
        KeyCode::Char('4') => return Some(Action::Tab(3)),
        KeyCode::Char('5') => return Some(Action::Tab(4)),
        KeyCode::Char('6') => return Some(Action::Tab(5)),
        KeyCode::Char('7') => return Some(Action::Tab(6)),
        KeyCode::Char('j')
            if tab == Tab::Usage && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            return Some(Action::ScrollDetailDown);
        }
        KeyCode::Char('k')
            if tab == Tab::Usage && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            return Some(Action::ScrollDetailUp);
        }
        KeyCode::Up | KeyCode::Char('k') => return Some(Action::Up),
        KeyCode::Down | KeyCode::Char('j') => return Some(Action::Down),
        KeyCode::PageUp => return Some(Action::PageUp),
        KeyCode::PageDown => return Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => return Some(Action::GoTop),
        KeyCode::End | KeyCode::Char('G') => return Some(Action::GoBottom),
        KeyCode::Char('/') => return Some(Action::StartFilter),
        KeyCode::Char('r') => return Some(Action::Refresh),
        KeyCode::Char('T') => return Some(Action::ToggleTheme),
        _ => {}
    }

    // Feature keys are tab-local so Logs never inherits CRUD / period semantics.
    if tab == Tab::Logs {
        return map_logs(key);
    }

    match key.code {
        KeyCode::Char('a') => Some(Action::Add),
        KeyCode::Char('e') if tab != Tab::Usage => Some(Action::Edit),
        KeyCode::Enter if !matches!(tab, Tab::Usage | Tab::Pricing | Tab::Keys) => {
            Some(Action::Edit)
        }
        KeyCode::Char('d') => Some(Action::Delete),
        KeyCode::Char('v') if tab == Tab::Keys => Some(Action::ViewSecret),
        KeyCode::Char('[') if tab == Tab::Usage => Some(Action::PeriodPrev),
        KeyCode::Char(']') if tab == Tab::Usage => Some(Action::PeriodNext),
        KeyCode::Char('R') if tab == Tab::Pricing => Some(Action::PricingReload),
        KeyCode::Char('s') if tab == Tab::Pricing => Some(Action::PricingSync),
        KeyCode::Char('o') if tab == Tab::Pricing => Some(Action::TogglePricingView),
        KeyCode::Char('s') if tab == Tab::Providers => Some(Action::SetSecret),
        KeyCode::Char('v') if tab == Tab::Providers => Some(Action::ViewSecret),
        KeyCode::Char('y') if tab == Tab::Providers => Some(Action::CopySecretFull),
        KeyCode::Char('Y') if tab == Tab::Providers => Some(Action::CopySecretPrimary),
        KeyCode::Char('o') if tab == Tab::Providers => Some(Action::OpenBrowser),
        KeyCode::Char('x') if tab == Tab::Providers => Some(Action::OauthRefresh),
        KeyCode::Char('u') if tab == Tab::Providers => Some(Action::RefreshQuota),
        KeyCode::Char('c') if tab == Tab::Usage => Some(Action::CycleUsageSort),
        KeyCode::Char('t') if matches!(tab, Tab::Overview | Tab::Usage) => {
            Some(Action::CycleUsageDetail)
        }
        _ => None,
    }
}

fn map_logs(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('f') => Some(Action::ToggleLogsMode),
        KeyCode::Char('l') => Some(Action::CycleLogsLevel),
        KeyCode::Char('c') => Some(Action::ClearLogsBuffer),
        KeyCode::Char('y') => Some(Action::CopyLogLine),
        KeyCode::Char('[') => Some(Action::LogsDayPrev),
        KeyCode::Char(']') => Some(Action::LogsDayNext),
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
    }

    #[test]
    fn overview_t_opens_usage_by_day() {
        // Home heatmap prompts “press t”; must not be a dead key.
        assert_eq!(
            map_key(&Mode::Browse, Tab::Overview, key(KeyCode::Char('t'))),
            Some(Action::CycleUsageDetail)
        );
    }

    #[test]
    fn usage_t_cycles_detail() {
        assert_eq!(
            map_key(&Mode::Browse, Tab::Usage, key(KeyCode::Char('t'))),
            Some(Action::CycleUsageDetail)
        );
    }

    #[test]
    fn usage_ctrl_jk_scrolls_request_detail() {
        assert_eq!(
            map_key(
                &Mode::Browse,
                Tab::Usage,
                key_mod(KeyCode::Char('j'), KeyModifiers::CONTROL)
            ),
            Some(Action::ScrollDetailDown)
        );
        assert_eq!(
            map_key(
                &Mode::Browse,
                Tab::Usage,
                key_mod(KeyCode::Char('k'), KeyModifiers::CONTROL)
            ),
            Some(Action::ScrollDetailUp)
        );
        // Plain j/k still move the list selection.
        assert_eq!(
            map_key(&Mode::Browse, Tab::Usage, key(KeyCode::Char('j'))),
            Some(Action::Down)
        );
        assert_eq!(
            map_key(&Mode::Browse, Tab::Usage, key(KeyCode::Char('k'))),
            Some(Action::Up)
        );
    }

    #[test]
    fn help_jk_scrolls_keyboard_reference() {
        assert_eq!(
            map_key(&Mode::Help, Tab::Overview, key(KeyCode::Char('j'))),
            Some(Action::Down)
        );
        assert_eq!(
            map_key(&Mode::Help, Tab::Overview, key(KeyCode::Char('k'))),
            Some(Action::Up)
        );
        assert_eq!(
            map_key(&Mode::Help, Tab::Overview, key(KeyCode::PageDown)),
            Some(Action::PageDown)
        );
        assert_eq!(
            map_key(&Mode::Help, Tab::Overview, key(KeyCode::PageUp)),
            Some(Action::PageUp)
        );
        assert_eq!(
            map_key(&Mode::Help, Tab::Overview, key(KeyCode::Char('G'))),
            Some(Action::GoBottom)
        );
        assert_eq!(
            map_key(&Mode::Help, Tab::Overview, key(KeyCode::Char('g'))),
            Some(Action::GoTop)
        );
        assert_eq!(
            map_key(&Mode::Help, Tab::Overview, key(KeyCode::Esc)),
            Some(Action::Cancel)
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
        assert_eq!(
            map_key(&Mode::Browse, Tab::Overview, key(KeyCode::Char('6'))),
            Some(Action::Tab(5))
        );
        // Tab 7 = Logs
        assert_eq!(
            map_key(&Mode::Browse, Tab::Overview, key(KeyCode::Char('7'))),
            Some(Action::Tab(6))
        );
    }

    #[test]
    fn logs_keybinds() {
        assert_eq!(
            map_key(&Mode::Browse, Tab::Logs, key(KeyCode::Char('f'))),
            Some(Action::ToggleLogsMode)
        );
        assert_eq!(
            map_key(&Mode::Browse, Tab::Logs, key(KeyCode::Char('l'))),
            Some(Action::CycleLogsLevel)
        );
        assert_eq!(
            map_key(&Mode::Browse, Tab::Logs, key(KeyCode::Char('c'))),
            Some(Action::ClearLogsBuffer)
        );
        assert_eq!(
            map_key(&Mode::Browse, Tab::Logs, key(KeyCode::Char('y'))),
            Some(Action::CopyLogLine)
        );
        assert_eq!(
            map_key(&Mode::Browse, Tab::Logs, key(KeyCode::Char('['))),
            Some(Action::LogsDayPrev)
        );
        assert_eq!(
            map_key(&Mode::Browse, Tab::Logs, key(KeyCode::Char(']'))),
            Some(Action::LogsDayNext)
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
        f.focus = 1; // base_url — last field for non-Claude
        assert_eq!(f.fields.len(), 2);
        assert_eq!(
            map_key(&Mode::ProviderForm(f), Tab::Providers, key(KeyCode::Enter)),
            Some(Action::Submit)
        );
    }
}
