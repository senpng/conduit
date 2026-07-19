//! Application state machine for the interactive console.

use std::collections::HashMap;

use crossterm::event::KeyEvent;
use tokio::sync::mpsc::UnboundedSender;

use crate::console_client::ConsoleClient;
use crate::dto::{
    HealthResponse, KeyView, PricingView, ProviderSecretView, ProviderView, RouteView,
    UsageListResponse, UsageRecordView, UsageSummaryView,
};

/// Page size for Usage → recent (server-side pagination).
pub const USAGE_PAGE_SIZE: usize = 50;

use super::action::Action;
use super::forms::{
    current_period, shift_period, ConfirmAction, KeyForm, OauthFlow, PricingOverrideForm,
    ProviderAddChooser, ProviderForm, ProviderFormKind, RouteWizard,
};
use super::input::InputField;
use super::msg::{Msg, RefreshKind};
use super::net;
use super::theme::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview = 0,
    Providers = 1,
    Routes = 2,
    Keys = 3,
    Usage = 4,
    Pricing = 5,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::Overview,
        Tab::Providers,
        Tab::Routes,
        Tab::Keys,
        Tab::Usage,
        Tab::Pricing,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Providers => "Providers",
            Tab::Routes => "Routes",
            Tab::Keys => "Keys",
            Tab::Usage => "Usage",
            Tab::Pricing => "Pricing",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Tab::Overview => "Home",
            Tab::Providers => "Prov",
            Tab::Routes => "Route",
            Tab::Keys => "Keys",
            Tab::Usage => "Usage",
            Tab::Pricing => "Price",
        }
    }

    pub fn from_index(i: usize) -> Tab {
        Self::ALL[i.min(5)]
    }

    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone)]
pub enum Mode {
    Browse,
    /// Incremental filter for list tabs (`/`).
    Filter,
    Help,
    Confirm(ConfirmAction),
    Alert { title: String, body: String },
    /// One-shot / decrypted secret shown in a modal.
    ///
    /// `single_value` distinguishes a lone token (downstream key, one-shot
    /// reveal) from a provider's multi-field dump. For a lone token the `full`
    /// and `primary` copies are identical, so the footer drops the `a` hint.
    SecretReveal {
        title: String,
        secret: String,
        single_value: bool,
    },
    /// Choose how to add a provider (API key vs OAuth).
    ProviderAddChooser(ProviderAddChooser),
    ProviderForm(ProviderForm),
    KeyForm(KeyForm),
    RouteWizard(RouteWizard),
    /// OAuth is a provider-add path (or re-auth), not a top-level tab.
    OauthFlow(OauthFlow),
    PricingOverrideForm(PricingOverrideForm),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PricingPane {
    #[default]
    Merged,
    Overrides,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageSort {
    #[default]
    Date,
    Cost,
    Tokens,
}

impl UsageSort {
    pub fn next(self) -> Self {
        match self {
            UsageSort::Date => UsageSort::Cost,
            UsageSort::Cost => UsageSort::Tokens,
            UsageSort::Tokens => UsageSort::Date,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UsageSort::Cost => "cost",
            UsageSort::Tokens => "tokens",
            UsageSort::Date => "date",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageDetail {
    #[default]
    Recent,
    ByModel,
    ByKey,
    ByDay,
    /// Provider health: success rate + TTFB (not cost-only).
    ByProvider,
}

impl UsageDetail {
    pub fn next(self) -> Self {
        match self {
            UsageDetail::Recent => UsageDetail::ByModel,
            UsageDetail::ByModel => UsageDetail::ByKey,
            UsageDetail::ByKey => UsageDetail::ByDay,
            UsageDetail::ByDay => UsageDetail::ByProvider,
            UsageDetail::ByProvider => UsageDetail::Recent,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            UsageDetail::Recent => "recent",
            UsageDetail::ByModel => "by model",
            UsageDetail::ByKey => "by key",
            UsageDetail::ByDay => "by day",
            UsageDetail::ByProvider => "by provider",
        }
    }
}

pub struct App {
    pub client: ConsoleClient,
    pub console_addr: String,
    pub tab: Tab,
    pub mode: Mode,
    pub should_quit: bool,
    pub loading: bool,
    pub status: String,
    pub error: Option<String>,
    pub theme: Theme,
    /// Animation frame for spinners (advanced on Tick).
    pub tick_frame: u64,
    /// List filter query (applied in Browse; edited in Filter mode).
    pub filter: String,

    pub health: Option<HealthResponse>,
    pub providers: Vec<ProviderView>,
    pub routes: Vec<RouteView>,
    pub keys: Vec<KeyView>,
    /// Last-probed / last-seen upstream quota (OAuth remaining, rate-limit headers).
    pub quota_snapshots: Vec<crate::dto::QuotaSnapshotView>,
    /// Providers currently cooling after 429 / usage_limit.
    pub cooldowns: Vec<crate::dto::CooldownView>,
    /// Decrypted secrets keyed by provider id (multi-`v`; each toggles independently).
    pub provider_secrets: HashMap<String, ProviderSecretView>,
    pub usage_summary: Option<UsageSummaryView>,
    pub usage_recent: Vec<UsageRecordView>,
    pub usage_period: String,
    /// Server-side pagination for Usage → recent.
    pub usage_offset: usize,
    pub usage_total: u64,
    /// Last filter string applied to the usage list API (Recent pane).
    pub usage_applied_filter: String,
    pub pricing: Vec<PricingView>,
    pub pricing_overrides: Vec<PricingView>,
    pub pricing_pane: PricingPane,

    pub usage_sort: UsageSort,
    pub usage_detail: UsageDetail,

    /// Selection index into the **filtered** view for each tab.
    pub selected: [usize; 6],

    /// Cached result of [`Self::compute_filtered`] for the current tab/filter.
    /// Kept in sync via [`Self::refresh_filtered`] so `draw` never recomputes.
    filtered: Vec<usize>,

    /// Monotonic load generation. Bumped whenever a load that *replaces* list
    /// data is issued (tab flip, refresh, page/sort/filter change). Data
    /// responses carry the generation they were spawned under; `apply_msg`
    /// drops any tagged response older than this so a slow in-flight request
    /// can't clobber fresher data. Mutations send `gen == 0` to stay exempt.
    data_gen: u64,

    tx: UnboundedSender<Msg>,
}

impl App {
    pub fn new(console_addr: &str, tx: UnboundedSender<Msg>) -> Self {
        Self {
            client: ConsoleClient::new(console_addr),
            console_addr: console_addr.to_string(),
            tab: Tab::Overview,
            mode: Mode::Browse,
            should_quit: false,
            loading: false,
            status: "Ready — press ? for help · / filter".into(),
            error: None,
            theme: Theme::dark(),
            tick_frame: 0,
            filter: String::new(),
            health: None,
            providers: Vec::new(),
            routes: Vec::new(),
            keys: Vec::new(),
            quota_snapshots: Vec::new(),
            cooldowns: Vec::new(),
            provider_secrets: HashMap::new(),
            usage_summary: None,
            usage_recent: Vec::new(),
            usage_period: current_period(),
            usage_offset: 0,
            usage_total: 0,
            usage_applied_filter: String::new(),
            pricing: Vec::new(),
            pricing_overrides: Vec::new(),
            pricing_pane: PricingPane::Merged,
            usage_sort: UsageSort::default(),
            usage_detail: UsageDetail::default(),
            selected: [0; 6],
            filtered: Vec::new(),
            data_gen: 0,
            tx,
        }
    }

    pub fn request_refresh(&mut self) {
        self.loading = true;
        self.error = None;
        let gen = self.next_gen();
        match self.tab {
            Tab::Usage => {
                self.spawn_usage_load(true);
            }
            Tab::Pricing => {
                // Always reload pricing + period usage together.
                net::spawn_pricing(self.client.clone(), gen, self.tx.clone());
            }
            other => net::spawn_load_tab(self.client.clone(), other.index(), gen, self.tx.clone()),
        }
    }

    /// Bump and return the load generation for a data-replacing request.
    /// Skips 0, which is the exempt sentinel used by mutation-path responses.
    fn next_gen(&mut self) -> u64 {
        self.data_gen = self.data_gen.wrapping_add(1);
        if self.data_gen == 0 {
            self.data_gen = 1;
        }
        self.data_gen
    }

    pub fn handle_action(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Tick => self.on_tick(),
            Action::Help => {
                if matches!(self.mode, Mode::Browse) {
                    self.mode = Mode::Help;
                }
            }
            Action::Cancel | Action::ConfirmNo => self.close_overlay(),
            Action::ConfirmYes => self.confirm_yes(),
            Action::NextTab => self.switch_tab(Tab::from_index((self.tab.index() + 1) % 6)),
            Action::PrevTab => {
                self.switch_tab(Tab::from_index((self.tab.index() + 5) % 6));
            }
            Action::Tab(i) => self.switch_tab(Tab::from_index(i)),
            Action::Up => {
                if let Mode::ProviderAddChooser(c) = &mut self.mode {
                    c.move_sel(-1);
                } else {
                    self.move_sel(-1);
                }
            }
            Action::Down => {
                if let Mode::ProviderAddChooser(c) = &mut self.mode {
                    c.move_sel(1);
                } else {
                    self.move_sel(1);
                }
            }
            Action::PageUp => {
                if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    self.usage_prev_page();
                } else {
                    self.move_sel(-10);
                }
            }
            Action::PageDown => {
                if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    self.usage_next_page();
                } else {
                    self.move_sel(10);
                }
            }
            Action::GoTop => {
                if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    if self.usage_offset > 0 {
                        self.usage_offset = 0;
                        self.selected[Tab::Usage.index()] = 0;
                        self.spawn_usage_load(false);
                    } else {
                        self.selected[self.tab.index()] = 0;
                    }
                } else {
                    self.selected[self.tab.index()] = 0;
                }
            }
            Action::GoBottom => {
                if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
                    let last_off = self.usage_last_offset();
                    if self.usage_offset < last_off {
                        self.usage_offset = last_off;
                        self.selected[Tab::Usage.index()] = 0;
                        self.spawn_usage_load(false);
                    } else {
                        let n = self.list_len();
                        self.selected[self.tab.index()] = n.saturating_sub(1);
                    }
                } else {
                    let n = self.list_len();
                    self.selected[self.tab.index()] = n.saturating_sub(1);
                }
            }
            Action::StartFilter => {
                if matches!(self.mode, Mode::Browse)
                    && matches!(
                        self.tab,
                        Tab::Providers | Tab::Routes | Tab::Keys | Tab::Pricing | Tab::Usage
                    )
                {
                    self.mode = Mode::Filter;
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
                self.request_refresh();
            }
            Action::PeriodNext => {
                self.usage_period = shift_period(&self.usage_period, 1);
                self.usage_offset = 0;
                self.selected[Tab::Usage.index()] = 0;
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
                        net::spawn_provider_secret(self.client.clone(), id, self.tx.clone());
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
                    self.status = format!("Usage sort: {}", self.usage_sort.label());
                    // Recent list is sorted server-side; re-fetch so pagination stays correct.
                    if self.usage_detail == UsageDetail::Recent {
                        self.spawn_usage_load(false);
                    }
                }
            }
            Action::CycleUsageDetail => {
                if self.tab == Tab::Usage {
                    self.usage_detail = self.usage_detail.next();
                    self.selected[Tab::Usage.index()] = 0;
                    self.status = format!("Usage detail: {}", self.usage_detail.label());
                    // Detail switch changes which rollup list the filter applies to.
                    self.refresh_filtered();
                    // Entering Recent with a filter (or stale page) → re-fetch server page.
                    if self.usage_detail == UsageDetail::Recent
                        && self.filter != self.usage_applied_filter
                    {
                        self.usage_offset = 0;
                        self.spawn_usage_load(false);
                    }
                }
            }
        }
    }

    /// Load Usage summary + current recent page.
    /// When `include_summary` is false, only re-fetches the list page (filter/page/sort).
    fn spawn_usage_load(&mut self, include_summary: bool) {
        self.loading = true;
        self.error = None;
        self.usage_applied_filter = self.filter.clone();
        let gen = self.next_gen();
        let q = if self.filter.is_empty() {
            None
        } else {
            Some(self.filter.clone())
        };
        if include_summary {
            net::spawn_usage(
                self.client.clone(),
                Some(self.usage_period.clone()),
                self.usage_offset,
                USAGE_PAGE_SIZE,
                q,
                Some(self.usage_sort.label().to_string()),
                gen,
                self.tx.clone(),
            );
        } else {
            net::spawn_usage_page(
                self.client.clone(),
                Some(self.usage_period.clone()),
                self.usage_offset,
                USAGE_PAGE_SIZE,
                q,
                Some(self.usage_sort.label().to_string()),
                gen,
                self.tx.clone(),
            );
        }
    }

    fn usage_last_offset(&self) -> usize {
        let total = self.usage_total as usize;
        if total == 0 {
            return 0;
        }
        ((total - 1) / USAGE_PAGE_SIZE) * USAGE_PAGE_SIZE
    }

    fn usage_next_page(&mut self) {
        let next = self.usage_offset.saturating_add(USAGE_PAGE_SIZE);
        if (next as u64) >= self.usage_total && self.usage_total > 0 {
            self.status = "Already on last page".into();
            return;
        }
        if self.usage_total == 0 {
            return;
        }
        self.usage_offset = next;
        self.selected[Tab::Usage.index()] = 0;
        self.spawn_usage_load(false);
    }

    fn usage_prev_page(&mut self) {
        if self.usage_offset == 0 {
            self.status = "Already on first page".into();
            return;
        }
        self.usage_offset = self.usage_offset.saturating_sub(USAGE_PAGE_SIZE);
        self.selected[Tab::Usage.index()] = 0;
        self.spawn_usage_load(false);
    }

    fn switch_tab(&mut self, tab: Tab) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) {
            return;
        }
        self.mode = Mode::Browse;
        self.filter.clear();
        self.usage_applied_filter.clear();
        self.usage_offset = 0;
        self.tab = tab;
        // Reflect the new tab immediately; the pending load will refresh again
        // once its data arrives.
        self.refresh_filtered();
        self.request_refresh();
    }

    pub fn list_len(&self) -> usize {
        self.filtered_indices().len()
    }

    /// Cached indices into the underlying data for the current tab after
    /// filter. Recomputed by [`Self::refresh_filtered`] whenever an input
    /// changes (filter text, tab, pane/detail, or the backing data); read
    /// zero-cost every frame here.
    pub fn filtered_indices(&self) -> &[usize] {
        &self.filtered
    }

    /// Rebuild the filtered-index cache. Call after any change to filter text,
    /// tab, pricing pane, usage detail, or the underlying lists.
    pub fn refresh_filtered(&mut self) {
        self.filtered = self.compute_filtered();
    }

    fn compute_filtered(&self) -> Vec<usize> {
        // Lower-case the needle once; match case-insensitively without
        // allocating a lowercased copy of every field (the old hot path
        // called `to_lowercase()` per field, per row, per frame).
        let q = self.filter.to_lowercase();
        let match_q = |s: &str| q.is_empty() || ci_contains(s, &q);

        match self.tab {
            Tab::Overview => vec![],
            Tab::Providers => self
                .providers
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    match_q(&p.name)
                        || match_q(&p.kind)
                        || match_q(&p.id)
                        || match_q(&p.base_url)
                })
                .map(|(i, _)| i)
                .collect(),
            Tab::Routes => self
                .routes
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    match_q(&r.match_alias) || match_q(&r.strategy) || match_q(&r.id)
                })
                .map(|(i, _)| i)
                .collect(),
            Tab::Keys => self
                .keys
                .iter()
                .enumerate()
                .filter(|(_, k)| match_q(&k.name) || match_q(&k.id))
                .map(|(i, _)| i)
                .collect(),
            Tab::Usage => match self.usage_detail {
                // Recent: filter is applied server-side; list is already scoped.
                UsageDetail::Recent => (0..self.usage_recent.len()).collect(),
                UsageDetail::ByModel => self
                    .usage_summary
                    .as_ref()
                    .map(|s| {
                        s.by_model
                            .iter()
                            .enumerate()
                            .filter(|(_, m)| {
                                match_q(&m.label)
                                    || match_q(&m.provider_kind)
                                    || match_q(&m.total_usd.to_string())
                            })
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default(),
                UsageDetail::ByKey => self
                    .usage_summary
                    .as_ref()
                    .map(|s| {
                        s.entries
                            .iter()
                            .enumerate()
                            .filter(|(_, e)| {
                                match_q(&e.downstream_key_id)
                                    || self
                                        .keys
                                        .iter()
                                        .find(|k| k.id == e.downstream_key_id)
                                        .map(|k| match_q(&k.name))
                                        .unwrap_or(false)
                            })
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default(),
                UsageDetail::ByDay => self
                    .usage_summary
                    .as_ref()
                    .map(|s| {
                        s.by_day
                            .iter()
                            .enumerate()
                            .filter(|(_, d)| match_q(&d.day))
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default(),
                UsageDetail::ByProvider => self
                    .usage_summary
                    .as_ref()
                    .map(|s| {
                        s.by_provider
                            .iter()
                            .enumerate()
                            .filter(|(_, p)| {
                                let name = self
                                    .providers
                                    .iter()
                                    .find(|x| x.id == p.provider_id)
                                    .map(|x| x.name.as_str())
                                    .unwrap_or("");
                                match_q(&p.provider_id)
                                    || match_q(name)
                                    || match_q(&p.provider_kind)
                                    || match_q(&format!("{:.0}%", p.success_rate * 100.0))
                            })
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            Tab::Pricing => {
                let rows = match self.pricing_pane {
                    PricingPane::Merged => &self.pricing,
                    PricingPane::Overrides => &self.pricing_overrides,
                };
                rows.iter()
                    .enumerate()
                    .filter(|(_, p)| match_q(&p.provider_kind) || match_q(&p.model_id))
                    .map(|(i, _)| i)
                    .collect()
            }
        }
    }

    /// Underlying data index for current selection (after filter).
    pub fn selected_data_index(&self) -> Option<usize> {
        let filtered = self.filtered_indices();
        let sel = self.selected[self.tab.index()];
        filtered.get(sel).copied()
    }

    fn move_sel(&mut self, delta: i32) {
        if matches!(self.mode, Mode::Filter) {
            // arrows still move selection while filter string is open
        } else if !matches!(self.mode, Mode::Browse) {
            if let Mode::RouteWizard(w) = &mut self.mode {
                if w.step == 1 {
                    let n = w.targets.len() as i32;
                    if n == 0 {
                        return;
                    }
                    let mut i = w.target_focus as i32 + delta;
                    if i < 0 {
                        i = n - 1;
                    }
                    if i >= n {
                        i = 0;
                    }
                    w.target_focus = i as usize;
                }
            }
            return;
        }
        let n = self.list_len();
        if n == 0 {
            return;
        }
        let idx = self.tab.index();
        let mut s = self.selected[idx] as i32 + delta;
        if s < 0 {
            s = (n as i32) - 1;
        }
        if s >= n as i32 {
            s = 0;
        }
        self.selected[idx] = s as usize;
    }

    fn close_overlay(&mut self) {
        // Cancel oauth session if pending
        if let Mode::OauthFlow(f) = &self.mode {
            if let Some(sid) = &f.pending_session_id {
                net::spawn_oauth_cancel(self.client.clone(), sid.clone(), self.tx.clone());
            }
        }
        if matches!(self.mode, Mode::Filter) {
            // Esc/Enter in filter: keep filter text but leave edit mode.
            // Usage → recent applies filter server-side on leave.
            self.mode = Mode::Browse;
            if self.tab == Tab::Usage && self.filter != self.usage_applied_filter {
                self.usage_offset = 0;
                self.selected[Tab::Usage.index()] = 0;
                if self.usage_detail == UsageDetail::Recent {
                    self.spawn_usage_load(false);
                } else {
                    self.usage_applied_filter = self.filter.clone();
                }
            }
            return;
        }
        self.mode = Mode::Browse;
    }

    /// Contextual footer keybinds for the current tab.
    pub fn context_keybinds(&self) -> Vec<(&'static str, &'static str)> {
        let mut v = vec![
            ("?", "help"),
            ("q", "quit"),
            ("r", "refresh"),
            ("tab", "next"),
        ];
        match self.tab {
            Tab::Overview => {}
            Tab::Providers => {
                v.extend([
                    ("a", "add"),
                    ("e", "edit"),
                    ("s", "set key"),
                    ("v", "view secret"),
                    ("y", "copy secret"),
                    ("Y", "copy token"),
                    ("o", "re-auth"),
                    ("x", "token ↻"),
                    ("u", "remaining"),
                    ("d", "delete"),
                    ("/", "filter"),
                ]);
            }
            Tab::Routes => {
                v.extend([("a", "add"), ("e", "edit"), ("d", "delete"), ("/", "filter")]);
            }
            Tab::Keys => {
                v.extend([
                    ("a", "create"),
                    ("e", "edit"),
                    ("v", "show token"),
                    ("d", "revoke"),
                    ("/", "filter"),
                ]);
            }
            Tab::Usage => {
                v.extend([
                    ("[", "prev mo"),
                    ("]", "next mo"),
                    ("c", "sort"),
                    ("t", "detail"),
                    ("/", "filter"),
                ]);
                if self.usage_detail == UsageDetail::Recent {
                    v.extend([("PgUp", "prev pg"), ("PgDn", "next pg")]);
                }
            }
            Tab::Pricing => match self.pricing_pane {
                PricingPane::Merged => {
                    v.extend([
                        ("o", "overrides"),
                        ("a", "override selected"),
                        ("R", "reload"),
                        ("s", "sync"),
                        ("/", "filter"),
                    ]);
                }
                PricingPane::Overrides => {
                    // Edit/delete only apply in the overrides pane.
                    v.extend([
                        ("o", "merged"),
                        ("a", "add"),
                        ("e", "edit"),
                        ("d", "delete"),
                        ("/", "filter"),
                    ]);
                }
            },
        }
        v
    }

    pub fn daemon_ok(&self) -> bool {
        self.health
            .as_ref()
            .map(|h| h.status.eq_ignore_ascii_case("ok") || h.status.eq_ignore_ascii_case("healthy"))
            .unwrap_or(false)
    }

    fn confirm_yes(&mut self) {
        let action = if let Mode::Confirm(a) = &self.mode {
            a.clone()
        } else {
            return;
        };
        self.mode = Mode::Browse;
        match action {
            ConfirmAction::DeleteProvider { id, .. } => {
                self.provider_secrets.remove(&id);
                self.status = format!("Deleting provider {id}…");
                net::spawn_delete_provider(self.client.clone(), id, self.tx.clone());
            }
            ConfirmAction::DeleteRoute { id, .. } => {
                self.status = format!("Deleting route {id}…");
                net::spawn_delete_route(self.client.clone(), id, self.tx.clone());
            }
            ConfirmAction::DeleteKey { id, .. } => {
                self.status = format!("Revoking key {id}…");
                net::spawn_delete_key(self.client.clone(), id, self.tx.clone());
            }
            ConfirmAction::SetProviderSecret { id, name } => {
                self.mode = Mode::ProviderForm(ProviderForm::set_secret(&id, &name));
            }
            ConfirmAction::DeletePricingOverride {
                provider_kind,
                model_id,
            } => {
                self.status = format!("Deleting override {provider_kind}/{model_id}…");
                net::spawn_delete_pricing_override(
                    self.client.clone(),
                    provider_kind,
                    model_id,
                    self.tx.clone(),
                );
            }
        }
    }

    fn start_add(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) {
            return;
        }
        self.mode = Mode::Browse;
        match self.tab {
            Tab::Providers => {
                // OAuth is one add method alongside API keys — not a separate tab.
                self.mode = Mode::ProviderAddChooser(ProviderAddChooser::new());
            }
            Tab::Routes => {
                // Always refresh providers so the wizard has a current list.
                let gen = self.next_gen();
                net::spawn_providers(self.client.clone(), gen, self.tx.clone());
                self.mode = Mode::RouteWizard(RouteWizard::create(self.providers.clone()));
            }
            Tab::Keys => {
                self.mode = Mode::KeyForm(KeyForm::create());
            }
            Tab::Pricing => {
                // Prefill from the currently selected pricing row (merged or overrides)
                // so operators can quickly override a model's rates.
                let form = self
                    .selected_data_index()
                    .and_then(|idx| {
                        let rows = match self.pricing_pane {
                            PricingPane::Merged => &self.pricing,
                            PricingPane::Overrides => &self.pricing_overrides,
                        };
                        rows.get(idx).map(PricingOverrideForm::from_row)
                    })
                    .unwrap_or_else(PricingOverrideForm::create);
                if let (Some(pk), Some(mid)) = (
                    form.fields.first().map(|f| f.value.clone()),
                    form.fields.get(1).map(|f| f.value.clone()),
                ) {
                    if !mid.is_empty() {
                        self.status =
                            format!("Override draft from {pk} / {mid} — tweak rates & save");
                    }
                }
                self.pricing_pane = PricingPane::Overrides;
                self.mode = Mode::PricingOverrideForm(form);
            }
            _ => {
                self.status = "Add is not available on this tab".into();
            }
        }
    }

    fn confirm_provider_add_chooser(&mut self) {
        let Some(sel) = (if let Mode::ProviderAddChooser(c) = &self.mode {
            Some(c.selected)
        } else {
            None
        }) else {
            return;
        };
        match sel {
            0 => {
                self.mode = Mode::ProviderForm(ProviderForm::create());
            }
            1 => {
                // Claude OAuth
                self.mode = Mode::OauthFlow(OauthFlow::start_new(0, ""));
            }
            2 => {
                self.mode = Mode::OauthFlow(OauthFlow::start_new(1, ""));
            }
            3 => {
                self.mode = Mode::OauthFlow(OauthFlow::start_new(2, ""));
            }
            _ => {
                self.mode = Mode::Browse;
            }
        }
    }

    fn start_provider_oauth_reauth(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) || self.tab != Tab::Providers {
            return;
        }
        self.mode = Mode::Browse;
        let Some(idx) = self.selected_data_index() else {
            self.status = "Select an OAuth provider first".into();
            return;
        };
        let Some(p) = self.providers.get(idx) else {
            return;
        };
        if !ProviderForm::is_oauth_kind_label(&p.kind) {
            self.status = format!(
                "Provider «{}» is not OAuth (kind={}) — use s to set API key",
                p.name, p.kind
            );
            return;
        }
        self.mode = Mode::OauthFlow(OauthFlow::reauth(p));
    }

    fn start_edit(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) {
            return;
        }
        self.mode = Mode::Browse;
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        match self.tab {
            Tab::Providers => {
                if let Some(p) = self.providers.get(idx) {
                    self.mode = Mode::ProviderForm(ProviderForm::edit(p));
                }
            }
            Tab::Routes => {
                if let Some(r) = self.routes.get(idx).cloned() {
                    let gen = self.next_gen();
                    net::spawn_providers(self.client.clone(), gen, self.tx.clone());
                    self.mode =
                        Mode::RouteWizard(RouteWizard::edit(&r, self.providers.clone()));
                }
            }
            Tab::Keys => {
                if let Some(k) = self.keys.get(idx) {
                    self.mode = Mode::KeyForm(KeyForm::edit(k));
                }
            }
            Tab::Pricing => {
                // Edit only in overrides pane; merged table is read-only (detail on the right).
                if self.pricing_pane != PricingPane::Overrides {
                    self.status =
                        "switch to overrides (o) to edit — or press a to add an override".into();
                    return;
                }
                if let Some(p) = self.pricing_overrides.get(idx) {
                    self.mode = Mode::PricingOverrideForm(PricingOverrideForm::edit(p));
                } else {
                    self.status = "no override selected — press a to add".into();
                }
            }
            _ => self.show_detail(),
        }
    }

    fn show_detail(&mut self) {
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        let body = match self.tab {
            Tab::Routes => self.routes.get(idx).map(|r| {
                format!(
                    "id: {}\nalias: {}\nstrategy: {}\nenabled: {}\ntargets: {}",
                    r.id, r.match_alias, r.strategy, r.enabled, r.targets_json
                )
            }),
            // Usage / Pricing: detail is the right pane — no modal.
            Tab::Usage | Tab::Pricing => None,
            _ => None,
        };
        if let Some(body) = body {
            self.mode = Mode::Alert {
                title: "Detail".into(),
                body,
            };
        }
    }

    fn start_delete(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) {
            return;
        }
        self.mode = Mode::Browse;
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        match self.tab {
            Tab::Providers => {
                if let Some(p) = self.providers.get(idx) {
                    self.mode = Mode::Confirm(ConfirmAction::DeleteProvider {
                        id: p.id.clone(),
                        name: p.name.clone(),
                    });
                }
            }
            Tab::Routes => {
                if let Some(r) = self.routes.get(idx) {
                    self.mode = Mode::Confirm(ConfirmAction::DeleteRoute {
                        id: r.id.clone(),
                        alias: r.match_alias.clone(),
                    });
                }
            }
            Tab::Keys => {
                if let Some(k) = self.keys.get(idx) {
                    self.mode = Mode::Confirm(ConfirmAction::DeleteKey {
                        id: k.id.clone(),
                        name: k.name.clone(),
                    });
                }
            }
            Tab::Pricing => {
                if self.pricing_pane != PricingPane::Overrides {
                    self.status =
                        "switch to overrides (o) to delete operator rows — merged table is read-only"
                            .into();
                    return;
                }
                if let Some(p) = self.pricing_overrides.get(idx) {
                    self.mode = Mode::Confirm(ConfirmAction::DeletePricingOverride {
                        provider_kind: p.provider_kind.clone(),
                        model_id: p.model_id.clone(),
                    });
                } else {
                    self.status = "no override selected".into();
                }
            }
            _ => {
                self.status = "Delete is not available on this tab".into();
            }
        }
    }

    fn start_set_secret_filtered(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) || self.tab != Tab::Providers {
            return;
        }
        self.mode = Mode::Browse;
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        if let Some(p) = self.providers.get(idx) {
            if ProviderForm::is_oauth_kind_label(&p.kind) {
                // OAuth providers don't use static API keys — re-auth instead.
                self.mode = Mode::OauthFlow(OauthFlow::reauth(p));
                self.status = "OAuth provider — starting re-auth (use o / s)".into();
                return;
            }
            self.mode = Mode::Confirm(ConfirmAction::SetProviderSecret {
                id: p.id.clone(),
                name: p.name.clone(),
            });
        }
    }

    fn focus_delta(&mut self, delta: i32) {
        match &mut self.mode {
            Mode::ProviderForm(f) => {
                let n = f.fields.len() as i32;
                let mut i = f.focus as i32 + delta;
                if i < 0 {
                    i = n - 1;
                }
                if i >= n {
                    i = 0;
                }
                f.focus = i as usize;
            }
            Mode::KeyForm(f) => {
                let n = f.fields.len() as i32;
                let mut i = f.focus as i32 + delta;
                if i < 0 {
                    i = n - 1;
                }
                if i >= n {
                    i = 0;
                }
                f.focus = i as usize;
            }
            Mode::PricingOverrideForm(f) => {
                let n = f.fields.len() as i32;
                let mut i = f.focus as i32 + delta;
                if i < 0 {
                    i = n - 1;
                }
                if i >= n {
                    i = 0;
                }
                f.focus = i as usize;
            }
            Mode::RouteWizard(w) => {
                if w.step == 1 {
                    let mut fi = w.field_in_target as i32 + delta;
                    if fi < 0 {
                        fi = 1;
                    }
                    if fi > 1 {
                        fi = 0;
                    }
                    w.field_in_target = fi as usize;
                }
            }
            Mode::OauthFlow(f) if f.pending_session_id.is_none() => {
                // focus: 0 kind (cycle), 1 name, 2 provider_id
                let mut i = f.focus as i32 + delta;
                if i < 0 {
                    i = 2;
                }
                if i > 2 {
                    i = 0;
                }
                f.focus = i as usize;
            }
            _ => {}
        }
    }

    fn input_code(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::{KeyEventKind, KeyModifiers};
        let key = KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        let field: Option<&mut InputField> = match &mut self.mode {
            Mode::ProviderForm(f) => f.fields.get_mut(f.focus),
            Mode::KeyForm(f) => f.fields.get_mut(f.focus),
            Mode::PricingOverrideForm(f) => f.fields.get_mut(f.focus),
            Mode::RouteWizard(w) => {
                if w.step == 0 {
                    Some(&mut w.match_alias)
                } else if w.step == 1 {
                    if let Some(t) = w.targets.get_mut(w.target_focus) {
                        if w.field_in_target == 0 {
                            Some(&mut t.model_id)
                        } else {
                            Some(&mut t.overrides)
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Mode::OauthFlow(f) if f.pending_session_id.is_none() => match f.focus {
                1 => Some(&mut f.name),
                2 => Some(&mut f.provider_id),
                _ => None,
            },
            _ => None,
        };
        if let Some(field) = field {
            field.handle_key(key);
        } else if let Mode::OauthFlow(f) = &mut self.mode {
            if f.focus == 0
                && f.pending_session_id.is_none()
                && matches!(
                    code,
                    crossterm::event::KeyCode::Char(' ') | crossterm::event::KeyCode::Char('k')
                )
            {
                f.cycle_kind();
            }
        }
    }

    fn submit_form(&mut self) {
        match &self.mode {
            Mode::ProviderForm(f) => match &f.kind {
                ProviderFormKind::Create => match f.to_create_body() {
                    Ok(body) => {
                        self.status = "Creating provider…".into();
                        self.mode = Mode::Browse;
                        net::spawn_create_provider(self.client.clone(), body, self.tx.clone());
                    }
                    Err(e) => {
                        if let Mode::ProviderForm(f) = &mut self.mode {
                            f.error = Some(e);
                        }
                    }
                },
                ProviderFormKind::Edit { id } => match f.to_update_body() {
                    Ok(body) => {
                        let id = id.clone();
                        self.status = "Updating provider…".into();
                        self.mode = Mode::Browse;
                        net::spawn_update_provider(self.client.clone(), id, body, self.tx.clone());
                    }
                    Err(e) => {
                        if let Mode::ProviderForm(f) = &mut self.mode {
                            f.error = Some(e);
                        }
                    }
                },
                ProviderFormKind::SetSecret { id, .. } => match f.to_secret_body() {
                    Ok(body) => {
                        let id = id.clone();
                        // Cached plaintext is stale after overwrite.
                        self.provider_secrets.remove(&id);
                        self.status = "Storing secret…".into();
                        self.mode = Mode::Browse;
                        net::spawn_set_secret(self.client.clone(), id, body, self.tx.clone());
                    }
                    Err(e) => {
                        if let Mode::ProviderForm(f) = &mut self.mode {
                            f.error = Some(e);
                        }
                    }
                },
            },
            Mode::KeyForm(f) => {
                if let Some(id) = f.edit_id.clone() {
                    match f.to_update_body() {
                        Ok(body) => {
                            self.status = format!("Updating key {id}…");
                            self.mode = Mode::Browse;
                            net::spawn_update_key(
                                self.client.clone(),
                                id,
                                body,
                                self.tx.clone(),
                            );
                        }
                        Err(e) => {
                            if let Mode::KeyForm(f) = &mut self.mode {
                                f.error = Some(e);
                            }
                        }
                    }
                } else {
                    match f.to_create_body() {
                        Ok(body) => {
                            self.status = "Creating key…".into();
                            self.mode = Mode::Browse;
                            net::spawn_create_key(self.client.clone(), body, self.tx.clone());
                        }
                        Err(e) => {
                            if let Mode::KeyForm(f) = &mut self.mode {
                                f.error = Some(e);
                            }
                        }
                    }
                }
            }
            Mode::RouteWizard(w) => match w.to_body() {
                Ok(body) => {
                    let edit_id = w.edit_id.clone();
                    self.status = "Saving route…".into();
                    self.mode = Mode::Browse;
                    if let Some(id) = edit_id {
                        net::spawn_update_route(self.client.clone(), id, body, self.tx.clone());
                    } else {
                        net::spawn_create_route(self.client.clone(), body, self.tx.clone());
                    }
                }
                Err(e) => {
                    if let Mode::RouteWizard(w) = &mut self.mode {
                        w.error = Some(e);
                    }
                }
            },
            Mode::OauthFlow(f) if f.pending_session_id.is_none() => {
                let kind = f.kind().to_string();
                let name = {
                    let n = f.name.value.trim();
                    if n.is_empty() {
                        None
                    } else {
                        Some(n.to_string())
                    }
                };
                let provider_id = {
                    let p = f.provider_id.value.trim();
                    if p.is_empty() {
                        None
                    } else {
                        Some(p.to_string())
                    }
                };
                self.status = format!("Starting OAuth ({kind})…");
                if let Mode::OauthFlow(f) = &mut self.mode {
                    f.error = None;
                    f.result_message = None;
                }
                net::spawn_oauth_start(self.client.clone(), kind, name, provider_id, self.tx.clone());
            }
            Mode::PricingOverrideForm(f) => match f.to_body() {
                Ok(body) => {
                    self.status = format!(
                        "Saving override {} / {}…",
                        body.provider_kind, body.model_id
                    );
                    self.mode = Mode::Browse;
                    self.pricing_pane = PricingPane::Overrides;
                    // Reset selection so the new/updated row can be focused after save.
                    self.selected[Tab::Pricing.index()] = 0;
                    // Stash identity for selection restore (via temporary single-row list).
                    self.pricing_overrides = vec![crate::dto::PricingView {
                        provider_kind: body.provider_kind.clone(),
                        model_id: body.model_id.clone(),
                        input_per_mtok: body.input_per_mtok,
                        output_per_mtok: body.output_per_mtok,
                        cache_read_per_mtok: body.cache_read_per_mtok,
                        cache_write_per_mtok: body.cache_write_per_mtok,
                        reasoning_per_mtok: body.reasoning_per_mtok,
                        effective_from: String::new(),
                    }];
                    net::spawn_upsert_pricing_override(
                        self.client.clone(),
                        body,
                        self.tx.clone(),
                    );
                }
                Err(e) => {
                    if let Mode::PricingOverrideForm(f) = &mut self.mode {
                        f.error = Some(e);
                    }
                }
            },
            _ => {}
        }
    }

    fn open_browser(&mut self) {
        let url = match &self.mode {
            Mode::OauthFlow(f) => f.auth_url.clone().or_else(|| f.verification_uri.clone()),
            _ => None,
        };
        if let Some(url) = url {
            match net::open_browser(&url) {
                Ok(()) => self.status = format!("Opened browser: {url}"),
                Err(e) => self.status = format!("Could not open browser: {e} — URL: {url}"),
            }
        } else {
            self.status = "No URL to open".into();
        }
    }

    fn oauth_cancel(&mut self) {
        if let Mode::OauthFlow(f) = &self.mode {
            if let Some(sid) = f.pending_session_id.clone() {
                net::spawn_oauth_cancel(self.client.clone(), sid, self.tx.clone());
                self.status = "Cancelling OAuth session…".into();
            }
        }
    }

    fn oauth_refresh_selected(&mut self) {
        if self.tab != Tab::Providers {
            return;
        }
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        if let Some(p) = self.providers.get(idx) {
            if !ProviderForm::is_oauth_kind_label(&p.kind) {
                self.status = "Token refresh only applies to OAuth providers".into();
                return;
            }
            self.status = format!("Refreshing OAuth for {}…", p.id);
            net::spawn_oauth_refresh(self.client.clone(), p.id.clone(), self.tx.clone());
        }
    }

    /// Selected provider id when on Providers tab (for quota probe).
    fn selected_provider_id(&self) -> Option<String> {
        if self.tab != Tab::Providers {
            return None;
        }
        let idx = self.selected_data_index()?;
        self.providers.get(idx).map(|p| p.id.clone())
    }

    fn selected_key_id(&self) -> Option<String> {
        if self.tab != Tab::Keys {
            return None;
        }
        let idx = self.selected_data_index()?;
        self.keys.get(idx).map(|k| k.id.clone())
    }

    pub fn quota_for(&self, provider_id: &str) -> Option<&crate::dto::QuotaSnapshotView> {
        self.quota_snapshots
            .iter()
            .find(|q| q.provider_id == provider_id)
    }

    pub fn cooldown_for(&self, provider_id: &str) -> Option<&crate::dto::CooldownView> {
        self.cooldowns
            .iter()
            .find(|c| c.provider_id == provider_id)
    }

    pub fn secret_for(&self, provider_id: &str) -> Option<&ProviderSecretView> {
        self.provider_secrets.get(provider_id)
    }

    /// Copy decrypted secret to the system clipboard.
    ///
    /// - `primary_only`: API key or OAuth `access_token`
    /// - full: multi-line dump (or the modal body for one-shot key reveals)
    fn copy_secret(&mut self, primary_only: bool) {
        let selected = self.selected_provider_id();
        let text = if let Some(id) = selected.as_deref() {
            if let Some(view) = self.secret_for(id) {
                if primary_only {
                    secret_primary_value(view)
                } else {
                    format_provider_secret_modal(view)
                }
            } else if let Mode::SecretReveal { secret, .. } = &self.mode {
                // Downstream key create / one-shot reveal modal.
                secret.clone()
            } else {
                self.status = "No secret loaded — press v to decrypt first".into();
                return;
            }
        } else if let Mode::SecretReveal { secret, .. } = &self.mode {
            secret.clone()
        } else {
            self.status = "No secret loaded — press v to decrypt first".into();
            return;
        };

        if text.trim().is_empty() {
            self.status = "Nothing to copy".into();
            return;
        }

        match super::clipboard::copy_to_clipboard(&text) {
            Ok(()) => {
                let what = if primary_only {
                    "primary secret (api_key / access_token)"
                } else {
                    "full secret dump"
                };
                let n = text.len();
                self.status = format!("Copied {what} to clipboard ({n} bytes)");
            }
            Err(e) => {
                self.status = format!("Clipboard failed: {e}");
            }
        }
    }

    fn apply_usage_page(&mut self, page: UsageListResponse) {
        self.usage_recent = page.entries;
        self.usage_total = page.total;
        // Prefer server-reported offset/limit when present.
        if page.limit > 0 {
            // keep page size constant; offset from response if coherent
            self.usage_offset = page.offset;
        }
        if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
            let from = self.usage_offset.saturating_add(1).min(self.usage_total as usize);
            let to = self
                .usage_offset
                .saturating_add(self.usage_recent.len())
                .min(self.usage_total as usize);
            let page_n = if self.usage_total == 0 {
                0
            } else {
                self.usage_offset / USAGE_PAGE_SIZE + 1
            };
            let pages = if self.usage_total == 0 {
                0
            } else {
                (self.usage_total as usize).div_ceil(USAGE_PAGE_SIZE)
            };
            self.status = if self.usage_total == 0 {
                "Usage: no matching requests".into()
            } else {
                format!("Usage {from}–{to} of {} · page {page_n}/{pages}", self.usage_total)
            };
        }
    }

    fn on_tick(&mut self) {
        self.tick_frame = self.tick_frame.wrapping_add(1);
        if let Mode::OauthFlow(f) = &mut self.mode {
            if f.pending_session_id.is_some() {
                f.poll_ticks = f.poll_ticks.wrapping_add(1);
                // ~2s at 200ms tick → every 10 ticks. Skip if a poll is still
                // in flight so a slow one can't let requests pile up.
                if f.poll_ticks % 10 == 0 && !f.poll_inflight {
                    if let Some(sid) = f.pending_session_id.clone() {
                        f.poll_inflight = true;
                        net::spawn_oauth_poll(self.client.clone(), sid, self.tx.clone());
                    }
                }
            }
        }
    }

    /// True when a data response tagged `gen` is stale and must be dropped.
    /// `gen == 0` is the mutation-path sentinel and is never considered stale.
    fn is_stale(&self, gen: u64) -> bool {
        gen != 0 && gen < self.data_gen
    }

    pub fn apply_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Health(r) => {
                self.loading = false;
                match r {
                    Ok(h) => {
                        self.health = Some(h);
                        self.error = None;
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.health = None;
                    }
                }
            }
            Msg::Providers { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.providers = v;
                        // Drop decrypted secrets for providers that no longer exist.
                        self.provider_secrets
                            .retain(|id, _| self.providers.iter().any(|p| p.id == *id));
                        self.clamp_sel(Tab::Providers);
                        if let Mode::RouteWizard(w) = &mut self.mode {
                            w.set_providers(self.providers.clone());
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Routes { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.routes = v;
                        self.clamp_sel(Tab::Routes);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Keys { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.keys = v;
                        self.clamp_sel(Tab::Keys);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Quota {
                snapshots,
                cooldowns,
            } => {
                self.loading = false;
                match snapshots {
                    Ok(v) => self.quota_snapshots = v,
                    // Empty error string is a "don't touch" placeholder from the
                    // quota-probe failure path — surface only real messages.
                    Err(e) if !e.is_empty() => {
                        self.status = format!("quota snapshots: {e}");
                    }
                    Err(_) => {}
                }
                match cooldowns {
                    Ok(v) => self.cooldowns = v,
                    Err(e) if !e.is_empty() => {
                        self.status = format!("cooldowns: {e}");
                    }
                    Err(_) => {}
                }
                if self.tab == Tab::Providers {
                    let n = self.quota_snapshots.len();
                    self.status = if n == 0 {
                        "No quota data yet — OAuth remaining probes after login traffic or press u"
                            .into()
                    } else {
                        format!("Quota updated ({n} snapshot{})", if n == 1 { "" } else { "s" })
                    };
                }
            }
            Msg::ProviderSecret(r) => {
                self.loading = false;
                match r {
                    Ok(view) => {
                        let title = format!(
                            "Secret · {} ({})",
                            if view.provider_name.is_empty() {
                                view.provider_id.as_str()
                            } else {
                                view.provider_name.as_str()
                            },
                            view.secret_kind
                        );
                        let body = format_provider_secret_modal(&view);
                        let id = view.provider_id.clone();
                        self.provider_secrets.insert(id, view);
                        self.mode = Mode::SecretReveal {
                            title,
                            secret: body,
                            single_value: false,
                        };
                        self.status =
                            "Secret decrypted — shown in detail & modal (v to hide; others keep)"
                                .into();
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.status = "Failed to decrypt secret".into();
                    }
                }
            }
            Msg::KeySecret(r) => {
                self.loading = false;
                match r {
                    Ok(view) => {
                        let title = if view.name.is_empty() {
                            format!("Key {} — token", view.id)
                        } else {
                            format!("Key {} — token", view.name)
                        };
                        self.mode = Mode::SecretReveal {
                            title,
                            secret: view.key,
                            single_value: true,
                        };
                        self.status = "Key revealed — y/c copy · Esc close".into();
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.status = "Failed to reveal key".into();
                    }
                }
            }
            Msg::Usage { gen, summary, recent } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                if let Some(summary) = summary {
                    match summary {
                        Ok(s) => self.usage_summary = Some(s),
                        Err(e) => {
                            // Pricing tab preloads usage for detail panes — don't paint
                            // a hard error over pricing if the summary fails.
                            if self.tab == Tab::Usage {
                                self.error = Some(e);
                            } else {
                                self.status = format!("usage summary unavailable: {e}");
                            }
                        }
                    }
                }
                if let Some(recent) = recent {
                    match recent {
                        Ok(page) => {
                            // Pricing/overview preload may send empty page — don't wipe
                            // a list already loaded from the Usage tab.
                            let empty_preload = page.entries.is_empty()
                                && page.total == 0
                                && page.limit == 0
                                && self.tab != Tab::Usage
                                && !self.usage_recent.is_empty();
                            if !empty_preload {
                                self.apply_usage_page(page);
                            }
                            if self.tab == Tab::Usage {
                                self.clamp_sel(Tab::Usage);
                            }
                        }
                        Err(e) => {
                            if self.tab == Tab::Usage {
                                self.error = Some(e);
                            }
                        }
                    }
                }
            }
            Msg::Pricing { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.pricing = v;
                        self.clamp_sel(Tab::Pricing);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::PricingOverrides { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                    // If we just saved a specific row, keep selection on it.
                    let prefer = self
                        .pricing_overrides
                        .get(self.selected_data_index().unwrap_or(usize::MAX))
                        .map(|p| (p.provider_kind.clone(), p.model_id.clone()));
                    self.pricing_overrides = v;
                    // Data replaced → refresh cache before mapping data→view index.
                    self.refresh_filtered();
                    if self.tab == Tab::Pricing && self.pricing_pane == PricingPane::Overrides {
                        if let Some((pk, mid)) = prefer {
                            if let Some(i) = self
                                .pricing_overrides
                                .iter()
                                .position(|p| p.provider_kind == pk && p.model_id == mid)
                            {
                                // Map data index → filtered view index.
                                let filtered = self.filtered_indices();
                                if let Some(view_i) = filtered.iter().position(|&di| di == i) {
                                    self.selected[Tab::Pricing.index()] = view_i;
                                }
                            }
                        }
                        self.clamp_sel(Tab::Pricing);
                    }
                    if self
                        .error
                        .as_ref()
                        .is_some_and(|e| e.contains("404"))
                    {
                        self.error = None;
                    }
                }
                    Err(e) => {
                        // Older daemons / missing route: keep any rows we already
                        // have (e.g. from upsert response) and only warn.
                        if e.contains("404") {
                            if self.pricing_overrides.is_empty() {
                                self.status =
                                "overrides API unavailable (restart conduitd?) — showing merged only"
                                    .into();
                            }
                        } else {
                            self.error = Some(e);
                        }
                    }
                }
            }
            Msg::Mutated {
                ok,
                message,
                refresh,
                secret,
            } => {
                self.loading = false;
                if ok {
                    self.status = message;
                    self.error = None;
                    self.apply_refresh(refresh);
                    if let Some(sec) = secret {
                        self.mode = Mode::SecretReveal {
                            title: "Secret (copy now — shown once)".into(),
                            secret: sec,
                            single_value: true,
                        };
                    }
                } else {
                    self.error = Some(message);
                    self.status = "Error".into();
                }
            }
            Msg::KeyCreated(r) => {
                self.loading = false;
                match r {
                Ok(k) => {
                    self.status = format!("Key {} created", k.id);
                    self.apply_refresh(RefreshKind::Keys);
                    self.mode = Mode::SecretReveal {
                        title: format!("Downstream key {} — copy now (shown once)", k.name),
                        secret: k.key,
                        single_value: true,
                    };
                }
                    Err(e) => {
                        self.error = Some(e);
                    }
                }
            }
            Msg::OauthStarted(r) => {
                self.loading = false;
                match r {
                    Ok(s) => {
                    if let Mode::OauthFlow(f) = &mut self.mode {
                        f.pending_session_id = Some(s.session_id.clone());
                        f.session_status = Some(s.status.clone());
                        f.auth_url = s.auth_url.clone();
                        f.user_code = s.user_code.clone();
                        f.verification_uri = s.verification_uri.clone();
                        f.error = s.error.clone();
                        f.poll_ticks = 0;
                    } else {
                        // entered from browse somehow — open flow
                        let mut f = OauthFlow::new();
                        f.pending_session_id = Some(s.session_id.clone());
                        f.session_status = Some(s.status.clone());
                        f.auth_url = s.auth_url;
                        f.user_code = s.user_code;
                        f.verification_uri = s.verification_uri;
                        self.mode = Mode::OauthFlow(f);
                    }
                    self.status = "OAuth pending — complete in browser".into();
                }
                    Err(e) => {
                        if let Mode::OauthFlow(f) = &mut self.mode {
                            f.error = Some(e.clone());
                        }
                        self.error = Some(e);
                    }
                }
            }
            Msg::OauthPolled(r) => {
                self.loading = false;
                if let Mode::OauthFlow(f) = &mut self.mode {
                    f.poll_inflight = false;
                }
                match r {
                    Ok(s) => {
                        if let Mode::OauthFlow(f) = &mut self.mode {
                            f.session_status = Some(s.status.clone());
                            f.auth_url = s.auth_url.or(f.auth_url.clone());
                            f.user_code = s.user_code.or(f.user_code.clone());
                            f.verification_uri = s.verification_uri.or(f.verification_uri.clone());
                            f.error = s.error.clone();
                            let st = s.status.to_lowercase();
                            if st == "completed" {
                                f.pending_session_id = None;
                                f.result_message = Some(format!(
                                    "Completed. provider_id={:?} email={:?}",
                                    s.provider_id, s.email
                                ));
                                self.status = "OAuth completed — provider list refreshed".into();
                                self.tab = Tab::Providers;
                                self.apply_refresh(RefreshKind::Providers);
                            } else if st == "error" || st == "cancelled" {
                                f.pending_session_id = None;
                                f.result_message =
                                    Some(s.error.unwrap_or_else(|| st.clone()));
                                self.status = format!("OAuth {st}");
                            }
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::OauthCancelled(r) => {
                self.loading = false;
                match r {
                    Ok(()) => {
                        if let Mode::OauthFlow(f) = &mut self.mode {
                            f.pending_session_id = None;
                            f.session_status = Some("cancelled".into());
                            f.result_message = Some("Cancelled".into());
                        }
                        self.status = "OAuth cancelled".into();
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    fn clamp_sel(&mut self, tab: Tab) {
        // Data or tab changed → rebuild the filtered-index cache before we read
        // its length below (and before the next frame reads it).
        self.refresh_filtered();
        let n = match tab {
            Tab::Providers => self.providers.len(),
            Tab::Routes => self.routes.len(),
            Tab::Keys => self.keys.len(),
            Tab::Usage => self.list_len(),
            Tab::Pricing => self.list_len(),
            Tab::Overview => 0,
        };
        let i = tab.index();
        if n == 0 {
            self.selected[i] = 0;
        } else if self.selected[i] >= n {
            self.selected[i] = n - 1;
        }
    }

    fn apply_refresh(&mut self, kind: RefreshKind) {
        let gen = self.next_gen();
        match kind {
            RefreshKind::None => {}
            RefreshKind::Overview => net::spawn_overview(self.client.clone(), gen, self.tx.clone()),
            RefreshKind::Providers => {
                net::spawn_providers(self.client.clone(), gen, self.tx.clone());
            }
            RefreshKind::Routes => net::spawn_routes(self.client.clone(), gen, self.tx.clone()),
            RefreshKind::Keys => net::spawn_keys(self.client.clone(), gen, self.tx.clone()),
            RefreshKind::Usage => {
                self.spawn_usage_load(true);
            }
            RefreshKind::Pricing => net::spawn_pricing(self.client.clone(), gen, self.tx.clone()),
            RefreshKind::PricingOverrides => {
                net::spawn_pricing_overrides(self.client.clone(), gen, self.tx.clone());
            }
            RefreshKind::Oauth => {
                // OAuth is not a tab; refresh provider list after login.
                net::spawn_providers(self.client.clone(), gen, self.tx.clone());
            }
            RefreshKind::All => {
                net::spawn_overview(self.client.clone(), gen, self.tx.clone());
            }
        }
    }
}

/// Case-insensitive substring test where `needle` is already lower-cased.
/// Still lowercases `haystack` per call, but the filter result is now cached
/// (see [`App::refresh_filtered`]) so this runs only when inputs change rather
/// than on every frame.
fn ci_contains(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(needle)
}

/// Primary credential string: API key or OAuth access_token.
fn secret_primary_value(view: &crate::dto::ProviderSecretView) -> String {
    if view.secret_kind == "api_key" {
        return view.api_key.clone().unwrap_or_default();
    }
    view.oauth
        .as_ref()
        .map(|o| o.access_token.clone())
        .unwrap_or_default()
}

/// Multi-line body for the secret reveal modal.
fn format_provider_secret_modal(view: &crate::dto::ProviderSecretView) -> String {
    use super::widgets::format_local_time;

    let mut lines = vec![
        format!("provider   {}", view.provider_id),
        format!("name       {}", view.provider_name),
        format!("kind       {} / {}", view.provider_kind, view.secret_kind),
        format!("key_id     {}", view.key_id),
        String::new(),
    ];
    if view.secret_kind == "api_key" {
        lines.push(format!(
            "api_key    {}",
            view.api_key.as_deref().unwrap_or("(empty)")
        ));
        return lines.join("\n");
    }
    if let Some(o) = &view.oauth {
        fn push(lines: &mut Vec<String>, k: &str, v: &str) {
            if !v.is_empty() {
                lines.push(format!("{k:<12}{v}"));
            }
        }
        push(&mut lines, "type", &o.provider_type);
        push(&mut lines, "auth_kind", &o.auth_kind);
        if let Some(e) = &o.email {
            push(&mut lines, "email", e);
        }
        if let Some(a) = &o.account_id {
            push(&mut lines, "account_id", a);
        }
        if let Some(pl) = &o.plan_type {
            push(&mut lines, "plan", pl);
        }
        if let Some(org) = &o.organization_name {
            push(&mut lines, "org", org);
        }
        if let Some(oid) = &o.organization_id {
            push(&mut lines, "org_id", oid);
        }
        if let Some(sub) = &o.sub {
            push(&mut lines, "sub", sub);
        }
        if let Some(exp) = &o.expired {
            push(&mut lines, "expired", &format_local_time(exp));
        }
        if let Some(lr) = &o.last_refresh {
            push(&mut lines, "last_refresh", &format_local_time(lr));
        }
        if let Some(bu) = &o.base_url {
            push(&mut lines, "base_url", bu);
        }
        if let Some(px) = &o.proxy_url {
            push(&mut lines, "proxy_url", px);
        }
        if let Some(ua) = o.using_api {
            push(&mut lines, "using_api", &ua.to_string());
        }
        lines.push(String::new());
        push(&mut lines, "access_token", &o.access_token);
        push(&mut lines, "refresh_token", &o.refresh_token);
        if let Some(id_tok) = &o.id_token {
            push(&mut lines, "id_token", id_tok);
        }
        if let Some(tt) = &o.token_type {
            push(&mut lines, "token_type", tt);
        }
        if let Some(te) = &o.token_endpoint {
            push(&mut lines, "token_endpoint", te);
        }
        if !o.extra.is_empty() {
            lines.push(String::new());
            lines.push("extra:".into());
            for (k, v) in &o.extra {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                lines.push(format!("  {k} = {val}"));
            }
        }
    } else {
        lines.push("(no oauth payload)".into());
    }
    lines.join("\n")
}

#[cfg(test)]
mod filter_cache_tests {
    use super::*;
    use crate::dto::ProviderView;

    fn provider(id: &str, name: &str, kind: &str) -> ProviderView {
        ProviderView {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            base_url: String::new(),
            upstream_key_ref: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn app_with_providers(ps: Vec<ProviderView>) -> App {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new("http://localhost:0", tx);
        app.tab = Tab::Providers;
        // gen 0 is the mutation-exempt sentinel → never treated as stale.
        app.apply_msg(Msg::Providers {
            gen: 0,
            result: Ok(ps),
        });
        app
    }

    #[test]
    fn cache_populated_after_data_load() {
        let app = app_with_providers(vec![
            provider("p1", "OpenAI", "openai"),
            provider("p2", "Claude", "anthropic"),
        ]);
        assert_eq!(app.filtered_indices(), &[0, 1]);
        assert_eq!(app.list_len(), 2);
    }

    #[test]
    fn filter_narrows_cached_view() {
        let mut app = app_with_providers(vec![
            provider("p1", "OpenAI", "openai"),
            provider("p2", "Claude", "anthropic"),
        ]);
        app.mode = Mode::Filter;
        // Case-insensitive substring on any of name/kind/id/base_url.
        app.handle_action(Action::Char('c'));
        app.handle_action(Action::Char('l'));
        assert_eq!(app.filtered_indices(), &[1], "only Claude matches 'cl'");
        assert_eq!(app.list_len(), 1);
        // Backspacing restores the full view from cache.
        app.handle_action(Action::Backspace);
        app.handle_action(Action::Backspace);
        assert_eq!(app.filtered_indices(), &[0, 1]);
    }

    #[test]
    fn data_reload_refreshes_cache() {
        let mut app = app_with_providers(vec![provider("p1", "OpenAI", "openai")]);
        assert_eq!(app.list_len(), 1);
        // A fresh load with more rows must be reflected in the cache.
        app.apply_msg(Msg::Providers {
            gen: 0,
            result: Ok(vec![
                provider("p1", "OpenAI", "openai"),
                provider("p2", "Claude", "anthropic"),
                provider("p3", "Grok", "xai"),
            ]),
        });
        assert_eq!(app.list_len(), 3);
        assert_eq!(app.filtered_indices(), &[0, 1, 2]);
    }
}


