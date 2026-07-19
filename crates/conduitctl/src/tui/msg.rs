//! Messages from async network tasks back to the UI thread.

use crate::dto::{
    CooldownView, HealthResponse, KeyCreateResponse, KeyView, OAuthSessionView, PricingView,
    ProviderSecretView, ProviderView, QuotaSnapshotView, RouteView, UsageRecordView,
    UsageSummaryView,
};

#[derive(Debug)]
pub enum Msg {
    Health(Result<HealthResponse, String>),
    Providers(Result<Vec<ProviderView>, String>),
    Routes(Result<Vec<RouteView>, String>),
    Keys(Result<Vec<KeyView>, String>),
    Usage {
        summary: Result<UsageSummaryView, String>,
        recent: Result<Vec<UsageRecordView>, String>,
    },
    Pricing(Result<Vec<PricingView>, String>),
    PricingOverrides(Result<Vec<PricingView>, String>),
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
    ProviderSecret(Result<ProviderSecretView, String>),
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
    All,
}
