//! Application state machine for the interactive console.

use std::collections::HashMap;

use tokio::sync::mpsc::UnboundedSender;

use crate::console_client::ConsoleClient;
use crate::dto::{
    HealthResponse, KeyView, PricingView, ProviderSecretView, ProviderView, RouteView,
    UsageRecordView, UsageSummaryView,
};

/// Page size for Usage → recent (server-side pagination).
pub const USAGE_PAGE_SIZE: usize = 50;

use super::forms::{
    current_period, ConfirmAction, KeyForm, OauthFlow, PricingOverrideForm, ProviderAddChooser,
    ProviderForm, RouteWizard,
};
use super::msg::Msg;
use super::net;
use super::theme::{Theme, ThemeMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview = 0,
    Providers = 1,
    Routes = 2,
    Keys = 3,
    Usage = 4,
    Pricing = 5,
    Logs = 6,
}

impl Tab {
    pub const ALL: [Tab; 7] = [
        Tab::Overview,
        Tab::Providers,
        Tab::Routes,
        Tab::Keys,
        Tab::Usage,
        Tab::Pricing,
        Tab::Logs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Overview",
            Tab::Providers => "Providers",
            Tab::Routes => "Routes",
            Tab::Keys => "Keys",
            Tab::Usage => "Usage",
            Tab::Pricing => "Pricing",
            Tab::Logs => "Logs",
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
            Tab::Logs => "Log",
        }
    }

    pub fn from_index(i: usize) -> Tab {
        Self::ALL[i.min(Self::ALL.len() - 1)]
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
    /// auto | dark | light — `CONDUIT_THEME` at launch; `T` cycles at runtime.
    pub theme_mode: ThemeMode,
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
    /// Lifetime rollup for Overview (`period=all`). Not shared with Usage month.
    pub overview_summary: Option<UsageSummaryView>,
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
    /// Vertical scroll offset (lines) for Usage → Recent request detail pane.
    /// Clamped to `[0, usage_detail_scroll_max]`; reset when selection / pane changes.
    /// No wrap-around at either end.
    pub usage_detail_scroll: u16,
    /// Max scroll from last paint (content lines − viewport). Updated in draw.
    pub usage_detail_scroll_max: u16,
    /// Vertical scroll for the Keyboard reference (`?`) modal. Same clamp rules.
    pub help_scroll: u16,
    pub help_scroll_max: u16,

    /// Logs tab state machine (mode/level/stream isolated from other tabs).
    pub logs: super::logs::LogsState,

    /// Selection index into the **filtered** view for each tab.
    pub selected: [usize; 7],

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


mod actions;
mod filter;
mod forms_ops;
mod keybinds;
mod messages;
mod secrets;
mod usage_nav;

impl App {
    pub fn new(console_addr: &str, tx: UnboundedSender<Msg>) -> Self {
        let theme_mode = ThemeMode::from_env();
        let theme = theme_mode.resolve();
        let status = format!(
            "Ready — ? help · / filter · T theme ({})",
            theme_mode.status_label(theme.kind)
        );
        Self {
            client: ConsoleClient::new(console_addr),
            console_addr: console_addr.to_string(),
            tab: Tab::Overview,
            mode: Mode::Browse,
            should_quit: false,
            loading: false,
            status,
            error: None,
            theme_mode,
            theme,
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
            overview_summary: None,
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
            usage_detail_scroll: 0,
            usage_detail_scroll_max: 0,
            help_scroll: 0,
            help_scroll_max: 0,
            logs: super::logs::LogsState::default(),
            selected: [0; 7],
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
            Tab::Logs => {
                self.logs.start_load(self.client.clone(), gen, &self.filter, self.tx.clone());
            }
            other => net::spawn_load_tab(self.client.clone(), other.index(), gen, self.tx.clone()),
        }
    }

    /// Bump and return the load generation for a data-replacing request.
    /// Skips 0, which is the exempt sentinel used by mutation-path responses.
    pub(crate) fn next_gen(&mut self) -> u64 {
        self.data_gen = self.data_gen.wrapping_add(1);
        if self.data_gen == 0 {
            self.data_gen = 1;
        }
        self.data_gen
    }

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

    #[test]
    fn logs_tab_exists_and_cycles() {
        assert_eq!(Tab::ALL.len(), 7);
        assert_eq!(Tab::from_index(6), Tab::Logs);
        assert_eq!(Tab::Logs.title(), "Logs");
        assert_eq!(Tab::Logs.short(), "Log");
        assert_eq!(Tab::Logs.index(), 6);
    }

    #[test]
    fn logs_page_msg_fills_ring() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new("http://localhost:0", tx);
        app.tab = Tab::Logs;
        app.apply_msg(Msg::LogsPage {
            gen: 0,
            result: Ok(crate::dto::LogsPage {
                date: "2026-07-24".into(),
                lines: vec![crate::dto::LogLineView {
                    level: Some("INFO".into()),
                    raw: "hello-from-history".into(),
                    ..Default::default()
                }],
                next_cursor: None,
                prev_cursor: None,
                truncated: false,
                source: "file".into(),
            }),
        });
        assert_eq!(app.logs.len(), 1);
        assert!(app.logs.selected_line().unwrap().raw.contains("hello-from-history"));
        assert_eq!(app.logs.date, "2026-07-24");
    }

    #[test]
    fn logs_disabled_meta_sets_message() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new("http://localhost:0", tx);
        app.tab = Tab::Logs;
        app.apply_msg(Msg::LogsMeta {
            gen: 0,
            result: Ok(crate::dto::LogsMeta {
                enabled: false,
                message: Some("file logging is disabled".into()),
                today: "2026-07-24".into(),
                ..Default::default()
            }),
        });
        assert!(
            app.logs
                .message
                .as_deref()
                .unwrap_or("")
                .contains("disabled")
        );
    }

    #[test]
    fn logs_quit_stops_stream() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new("http://localhost:0", tx);
        app.tab = Tab::Logs;
        // Structural wiring: quit/leave must stop the live stream.
        let src = include_str!("actions.rs");
        assert!(
            src.contains("self.logs.on_leave()"),
            "quit/leave must stop stream"
        );
        app.handle_action(Action::Quit);
        assert!(app.should_quit);
    }


    /// Regression: pressing `a` on the merged Pricing table used to flip
    /// `pricing_pane` to Overrides without rebuilding the filtered-index
    /// cache. Esc/save then drew Overrides with merged indices → OOB panic.
    #[test]
    fn pricing_add_override_refreshes_filter_cache() {
        use crate::dto::PricingView;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new("http://localhost:0", tx);
        app.tab = Tab::Pricing;
        app.pricing_pane = PricingPane::Merged;
        app.pricing = vec![
            PricingView {
                provider_kind: "openai".into(),
                model_id: "gpt-4o".into(),
                input_per_mtok: 2.5,
                output_per_mtok: 10.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                reasoning_per_mtok: None,
                effective_from: String::new(),
            },
            PricingView {
                provider_kind: "anthropic".into(),
                model_id: "claude-sonnet".into(),
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                reasoning_per_mtok: None,
                effective_from: String::new(),
            },
        ];
        app.pricing_overrides = vec![];
        app.refresh_filtered();
        assert_eq!(app.filtered_indices(), &[0, 1], "merged cache");
        app.selected[Tab::Pricing.index()] = 1;

        app.handle_action(Action::Add);

        assert!(
            matches!(app.mode, Mode::PricingOverrideForm(_)),
            "add opens override form"
        );
        assert_eq!(app.pricing_pane, PricingPane::Overrides);
        // Cache must track the (empty) overrides list, not leftover merged
        // indices — otherwise Browse after Esc indexes past overrides.len().
        assert!(
            app.filtered_indices().is_empty(),
            "overrides filter cache must be empty, got {:?}",
            app.filtered_indices()
        );
        assert_eq!(app.selected[Tab::Pricing.index()], 0);
    }
}


