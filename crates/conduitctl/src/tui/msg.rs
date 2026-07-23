//! Messages from async network tasks back to the UI thread.

use crate::dto::{
    CooldownView, HealthResponse, KeyCreateResponse, KeySecretView, KeyView, LogLineView, LogsMeta,
    LogsPage, OAuthSessionView, PricingView, ProviderSecretView, ProviderView, QuotaSnapshotView,
    RouteView, UsageListResponse, UsageSummaryView,
};

#[derive(Debug)]
pub enum Msg {
    Health(Result<HealthResponse, String>),
    /// `gen` is the request generation captured when the load was spawned; the
    /// UI drops any data response whose `gen` is older than the latest issued
    /// load, so a slow in-flight request can't clobber fresher data (tab flip,
    /// page flip, filter change all bump the generation).
    Providers {
        gen: u64,
        result: Result<Vec<ProviderView>, String>,
    },
    Routes {
        gen: u64,
        result: Result<Vec<RouteView>, String>,
    },
    Keys {
        gen: u64,
        result: Result<Vec<KeyView>, String>,
    },
    /// `summary` / `recent` are independently optional so page flips can skip the rollup.
    Usage {
        gen: u64,
        summary: Option<Result<UsageSummaryView, String>>,
        recent: Option<Result<UsageListResponse, String>>,
    },
    /// Lifetime usage rollup for Overview KPIs / rankings (`period=all`).
    /// Kept separate from [`Msg::Usage`] so Usage-tab month flips don't clobber home.
    OverviewSummary {
        gen: u64,
        result: Result<UsageSummaryView, String>,
    },
    Pricing {
        gen: u64,
        result: Result<Vec<PricingView>, String>,
    },
    PricingOverrides {
        gen: u64,
        result: Result<Vec<PricingView>, String>,
    },
    /// Upstream quota snapshots + cooldowns (OAuth remaining).
    Quota {
        snapshots: Result<Vec<QuotaSnapshotView>, String>,
        cooldowns: Result<Vec<CooldownView>, String>,
    },
    Mutated {
        ok: bool,
        message: String,
        refresh: RefreshKind,
        /// One-time secret reveal (downstream key raw token).
        secret: Option<String>,
    },
    OauthStarted(Result<OAuthSessionView, String>),
    OauthPolled(Result<OAuthSessionView, String>),
    OauthCancelled(Result<(), String>),
    KeyCreated(Result<KeyCreateResponse, String>),
    /// Decrypted provider secret for detail / modal.
    ///
    /// `show_modal`: open the full secret reveal dialog (user pressed `v`).
    /// When false, only refresh the in-memory cache (edit prefill / post-save).
    ProviderSecret {
        result: Result<ProviderSecretView, String>,
        show_modal: bool,
    },
    /// Decrypted downstream key raw token for the reveal modal.
    KeySecret(Result<KeySecretView, String>),
    /// Logs meta (enabled / dates).
    LogsMeta {
        gen: u64,
        result: Result<LogsMeta, String>,
    },
    /// History page of log lines.
    LogsPage {
        gen: u64,
        result: Result<LogsPage, String>,
    },
    /// One live SSE line (or control event via status).
    LogsStreamLine {
        gen: u64,
        line: LogLineView,
    },
    /// Live stream meta / rotate / error / ended.
    LogsStreamEvent {
        gen: u64,
        kind: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // overview/usage/oauth/all used by apply_refresh; keep full set for callers
pub enum RefreshKind {
    None,
    Overview,
    Providers,
    Routes,
    Keys,
    Usage,
    Pricing,
    PricingOverrides,
    Oauth,
    Logs,
    All,
}
