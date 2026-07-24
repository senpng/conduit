//! Top-level Action dispatch for the TUI state machine.

use super::super::action::Action;
use super::super::forms::shift_period;
use super::super::net;
use super::{App, Mode, PricingPane, Tab, UsageDetail};

impl App {
    pub fn handle_action(&mut self, action: Action) {
        match action {
            Action::Quit => {
                self.logs.on_leave();
                self.should_quit = true;
            }
            Action::Tick => self.on_tick(),
            Action::Help => {
                if matches!(self.mode, Mode::Browse) {
                    self.mode = Mode::Help;
                    self.help_scroll = 0;
                    self.help_scroll_max = 0;
                }
            }
            Action::Cancel | Action::ConfirmNo => self.close_overlay(),
            Action::ConfirmYes => self.confirm_yes(),
            Action::NextTab => {
                let n = Tab::ALL.len();
                self.switch_tab(Tab::from_index((self.tab.index() + 1) % n));
            }
            Action::PrevTab => {
                let n = Tab::ALL.len();
                self.switch_tab(Tab::from_index((self.tab.index() + n - 1) % n));
            }
            Action::Tab(i) => self.switch_tab(Tab::from_index(i)),
            Action::Up => {
                if matches!(self.mode, Mode::Help) {
                    // Keyboard reference: stop at top (no wrap).
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                } else if let Mode::ProviderAddChooser(c) = &mut self.mode {
                    c.move_sel(-1);
                } else {
                    self.move_sel(-1);
                }
            }
            Action::Down => {
                if matches!(self.mode, Mode::Help) {
                    // Keyboard reference: stop at bottom (no wrap).
                    if self.help_scroll < self.help_scroll_max {
                        self.help_scroll += 1;
                    }
                } else if let Mode::ProviderAddChooser(c) = &mut self.mode {
                    c.move_sel(1);
                } else {
                    self.move_sel(1);
                }
            }
            Action::PageUp => {
                if matches!(self.mode, Mode::Help) {
                    self.help_scroll = self.help_scroll.saturating_sub(10);
                } else if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    self.usage_prev_page();
                } else {
                    self.move_sel(-10);
                }
            }
            Action::PageDown => {
                if matches!(self.mode, Mode::Help) {
                    self.help_scroll = self
                        .help_scroll
                        .saturating_add(10)
                        .min(self.help_scroll_max);
                } else if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    self.usage_next_page();
                } else {
                    self.move_sel(10);
                }
            }
            Action::GoTop => {
                if matches!(self.mode, Mode::Help) {
                    self.help_scroll = 0;
                } else if self.tab == Tab::Logs {
                    self.dispatch_logs(&Action::GoTop);
                } else if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    if self.usage_offset > 0 {
                        self.usage_offset = 0;
                        self.selected[Tab::Usage.index()] = 0;
                        self.usage_detail_scroll = 0;
                        self.spawn_usage_load(false);
                    } else if self.selected[self.tab.index()] != 0 {
                        self.selected[self.tab.index()] = 0;
                        self.usage_detail_scroll = 0;
                    }
                } else {
                    self.selected[self.tab.index()] = 0;
                    if self.tab == Tab::Usage {
                        self.usage_detail_scroll = 0;
                    }
                }
            }
            Action::GoBottom => {
                if matches!(self.mode, Mode::Help) {
                    self.help_scroll = self.help_scroll_max;
                } else if self.tab == Tab::Logs {
                    self.dispatch_logs(&Action::GoBottom);
                } else if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    let last_off = self.usage_last_offset();
                    if self.usage_offset < last_off {
                        self.usage_offset = last_off;
                        self.selected[Tab::Usage.index()] = 0;
                        self.usage_detail_scroll = 0;
                        self.spawn_usage_load(false);
                    } else {
                        let n = self.list_len();
                        let last = n.saturating_sub(1);
                        if self.selected[self.tab.index()] != last {
                            self.selected[self.tab.index()] = last;
                            self.usage_detail_scroll = 0;
                        }
                    }
                } else {
                    let n = self.list_len();
                    self.selected[self.tab.index()] = n.saturating_sub(1);
                    if self.tab == Tab::Usage {
                        self.usage_detail_scroll = 0;
                    }
                }
            }
            Action::StartFilter => {
                if matches!(self.mode, Mode::Browse)
                    && matches!(
                        self.tab,
                        Tab::Providers
                            | Tab::Routes
                            | Tab::Keys
                            | Tab::Pricing
                            | Tab::Usage
                            | Tab::Logs
                    )
                {
                    self.mode = Mode::Filter;
                }
            }
            Action::ToggleTheme => {
                if matches!(self.mode, Mode::Browse | Mode::Filter | Mode::Help) {
                    self.theme_mode = self.theme_mode.next();
                    self.theme = self.theme_mode.resolve();
                    self.status = format!(
                        "Theme: {}",
                        self.theme_mode.status_label(self.theme.kind)
                    );
                }
            }
            Action::Refresh => self.request_refresh(),
            Action::Add => self.start_add(),
            Action::Edit => self.start_edit(),
            Action::Delete => self.start_delete(),
            Action::Submit => {
                if matches!(self.mode, Mode::ProviderAddChooser(_)) {
                    self.confirm_provider_add_chooser();
                } else {
                    self.submit_form();
                }
            }
            Action::ChooserPick(i) => {
                if let Mode::ProviderAddChooser(c) = &mut self.mode {
                    c.selected = i.min(c.len().saturating_sub(1));
                }
                self.confirm_provider_add_chooser();
            }
            Action::NextField => self.focus_delta(1),
            Action::PrevField => self.focus_delta(-1),
            Action::Char(c) => {
                if matches!(self.mode, Mode::Filter) {
                    self.filter.push(c);
                    self.selected[self.tab.index()] = 0;
                    self.refresh_filtered();
                } else {
                    self.input_code(crossterm::event::KeyCode::Char(c));
                }
            }
            Action::Backspace => {
                if matches!(self.mode, Mode::Filter) {
                    self.filter.pop();
                    self.selected[self.tab.index()] = 0;
                    self.refresh_filtered();
                } else {
                    self.input_code(crossterm::event::KeyCode::Backspace);
                }
            }
            Action::DeleteChar => self.input_code(crossterm::event::KeyCode::Delete),
            Action::Left => self.input_code(crossterm::event::KeyCode::Left),
            Action::Right => self.input_code(crossterm::event::KeyCode::Right),
            Action::Home => self.input_code(crossterm::event::KeyCode::Home),
            Action::End => self.input_code(crossterm::event::KeyCode::End),
            Action::PricingReload => {
                self.status = "Reloading pricing…".into();
                net::spawn_pricing_reload(self.client.clone(), self.tx.clone());
            }
            Action::PricingSync => {
                self.status = "Syncing pricing (may take a while)…".into();
                net::spawn_pricing_sync(self.client.clone(), self.tx.clone());
            }
            Action::PeriodPrev => {
                self.usage_period = shift_period(&self.usage_period, -1);
                self.usage_offset = 0;
                self.selected[Tab::Usage.index()] = 0;
                self.usage_detail_scroll = 0;
                self.request_refresh();
            }
            Action::PeriodNext => {
                self.usage_period = shift_period(&self.usage_period, 1);
                self.usage_offset = 0;
                self.selected[Tab::Usage.index()] = 0;
                self.usage_detail_scroll = 0;
                self.request_refresh();
            }
            Action::OpenBrowser => {
                // In OAuth flow: open auth URL. On Providers tab: re-auth selected OAuth provider.
                if matches!(self.mode, Mode::OauthFlow(_)) {
                    self.open_browser();
                } else if self.tab == Tab::Providers {
                    self.start_provider_oauth_reauth();
                }
            }
            Action::OauthCancel => self.oauth_cancel(),
            Action::OauthRefresh => self.oauth_refresh_selected(),
            Action::RefreshQuota => {
                if self.tab == Tab::Providers {
                    let id = self.selected_provider_id();
                    self.status = if id.is_some() {
                        "Probing OAuth remaining…".into()
                    } else {
                        "Probing all OAuth remaining…".into()
                    };
                    self.loading = true;
                    net::spawn_quota_refresh(self.client.clone(), id, self.tx.clone());
                }
            }
            Action::ViewSecret => {
                if self.tab == Tab::Providers {
                    // Toggle per-provider: hide if already decrypted for the selection.
                    if let Some(id) = self.selected_provider_id() {
                        if self.provider_secrets.remove(&id).is_some() {
                            self.status = "Secret hidden".into();
                            return;
                        }
                        self.status = "Decrypting secret…".into();
                        self.loading = true;
                        net::spawn_provider_secret(
                            self.client.clone(),
                            id,
                            true,
                            self.tx.clone(),
                        );
                    } else {
                        self.status = "Select a provider first".into();
                    }
                } else if self.tab == Tab::Keys {
                    // Reveal the raw token in a one-shot modal (copy from there).
                    if let Some(id) = self.selected_key_id() {
                        self.status = "Revealing key…".into();
                        self.loading = true;
                        net::spawn_key_secret(self.client.clone(), id, self.tx.clone());
                    } else {
                        self.status = "Select a key first".into();
                    }
                }
            }
            Action::CopySecretFull => self.copy_secret(false),
            Action::CopySecretPrimary => self.copy_secret(true),
            Action::WizardAddTarget => {
                if let Mode::RouteWizard(w) = &mut self.mode {
                    w.add_target();
                }
            }
            Action::WizardRemoveTarget => {
                if let Mode::RouteWizard(w) = &mut self.mode {
                    w.remove_target();
                }
            }
            Action::WizardCycleStrategy => {
                if let Mode::RouteWizard(w) = &mut self.mode {
                    w.cycle_strategy();
                }
            }
            Action::WizardNextStep => {
                let should_submit = if let Mode::RouteWizard(w) = &mut self.mode {
                    if w.step < 2 {
                        w.step += 1;
                        false
                    } else {
                        true
                    }
                } else {
                    false
                };
                if should_submit {
                    self.submit_form();
                }
            }
            Action::WizardPrevStep => {
                if let Mode::RouteWizard(w) = &mut self.mode {
                    if w.step > 0 {
                        w.step -= 1;
                    }
                }
            }
            Action::CycleKind => {
                if let Mode::ProviderForm(f) = &mut self.mode {
                    f.cycle_kind();
                } else if let Mode::KeyForm(f) = &mut self.mode {
                    f.cycle_enabled();
                } else if let Mode::OauthFlow(f) = &mut self.mode {
                    f.cycle_kind();
                } else if let Mode::RouteWizard(w) = &mut self.mode {
                    if w.step == 1 {
                        w.cycle_provider();
                    } else if w.step == 0 {
                        w.cycle_strategy();
                    }
                }
            }
            Action::SetSecret => self.start_set_secret_filtered(),
            Action::TogglePricingView => {
                if self.tab == Tab::Pricing && matches!(self.mode, Mode::Browse | Mode::Filter) {
                    self.mode = Mode::Browse;
                    self.pricing_pane = match self.pricing_pane {
                        PricingPane::Merged => PricingPane::Overrides,
                        PricingPane::Overrides => PricingPane::Merged,
                    };
                    self.selected[Tab::Pricing.index()] = 0;
                    // Pane switch changes which list feeds the filter.
                    self.refresh_filtered();
                    self.status = match self.pricing_pane {
                        PricingPane::Merged => "Pricing: merged table".into(),
                        PricingPane::Overrides => {
                            "Pricing: operator overrides (pricing.json)".into()
                        }
                    };
                    // Ensure usage summary is present for the right-hand 用量 pane.
                    // Full pricing list may already be loaded — only pull missing pieces.
                    if self.usage_summary.is_none() {
                        let gen = self.next_gen();
                        net::spawn_usage_summary_only(self.client.clone(), gen, self.tx.clone());
                    }
                    if self.pricing_pane == PricingPane::Overrides
                        && self.pricing_overrides.is_empty()
                    {
                        let gen = self.next_gen();
                        net::spawn_pricing_overrides(self.client.clone(), gen, self.tx.clone());
                    }
                }
            }
            Action::CycleUsageSort => {
                if self.tab == Tab::Usage {
                    self.usage_sort = self.usage_sort.next();
                    self.usage_offset = 0;
                    self.selected[Tab::Usage.index()] = 0;
                    self.usage_detail_scroll = 0;
                    self.status = format!("Usage sort: {}", self.usage_sort.label());
                    // Recent list is sorted server-side; re-fetch so pagination stays correct.
                    if self.usage_detail == UsageDetail::Recent {
                        self.spawn_usage_load(false);
                    }
                }
            }
            Action::CycleUsageDetail => {
                if !matches!(self.mode, Mode::Browse | Mode::Filter) {
                    return;
                }
                match self.tab {
                    // Home heatmap: t jumps straight into the selectable by-day calendar.
                    Tab::Overview => {
                        self.usage_detail = UsageDetail::ByDay;
                        self.selected[Tab::Usage.index()] = 0;
                        self.usage_detail_scroll = 0;
                        self.status =
                            "Usage · by day — ↑↓ select a day on the calendar".into();
                        self.switch_tab(Tab::Usage);
                    }
                    Tab::Usage => {
                        self.mode = Mode::Browse;
                        self.usage_detail = self.usage_detail.next();
                        self.selected[Tab::Usage.index()] = 0;
                        self.usage_detail_scroll = 0;
                        self.status = format!("Usage detail: {}", self.usage_detail.label());
                        self.refresh_filtered();
                        if self.usage_detail == UsageDetail::Recent
                            && self.filter != self.usage_applied_filter
                        {
                            self.usage_offset = 0;
                            self.spawn_usage_load(false);
                        }
                    }
                    _ => {
                        self.status =
                            "t · daily spend calendar — press 1 (home) or 5 (Usage)".into();
                    }
                }
            }
            Action::ScrollDetailUp => {
                if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    // Stop at top — no wrap to bottom.
                    self.usage_detail_scroll = self.usage_detail_scroll.saturating_sub(1);
                }
            }
            Action::ScrollDetailDown => {
                if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    // Stop at bottom (max from last draw) — no wrap to top.
                    let max = self.usage_detail_scroll_max;
                    if self.usage_detail_scroll < max {
                        self.usage_detail_scroll += 1;
                    }
                }
            }
            Action::ToggleLogsMode
            | Action::CycleLogsLevel
            | Action::ClearLogsBuffer
            | Action::CopyLogLine
            | Action::LogsDayPrev
            | Action::LogsDayNext => {
                if self.tab == Tab::Logs && matches!(self.mode, Mode::Browse | Mode::Filter) {
                    self.dispatch_logs(&action);
                }
            }
        }
    }

}
