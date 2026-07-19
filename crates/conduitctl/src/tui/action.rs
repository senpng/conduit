//! User intents derived from keyboard input.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Quit,
    Tick,
    NextTab,
    PrevTab,
    Tab(usize),
    Up,
    Down,
    Refresh,
    Add,
    Edit,
    Delete,
    ConfirmYes,
    ConfirmNo,
    Cancel,
    Submit,
    Help,
    NextField,
    PrevField,
    Char(char),
    Backspace,
    DeleteChar,
    Left,
    Right,
    Home,
    End,
    /// Pricing: reload layers from disk
    PricingReload,
    /// Pricing: sync from LiteLLM
    PricingSync,
    /// Usage: previous / next calendar month
    PeriodPrev,
    PeriodNext,
    /// OAuth: open auth URL in browser
    OpenBrowser,
    /// OAuth: cancel pending session
    OauthCancel,
    /// OAuth: refresh selected provider tokens
    OauthRefresh,
    /// Providers: probe OAuth subscription remaining (Claude 5h/7d, Codex 7d, Grok mo)
    RefreshQuota,
    /// Providers: decrypt & show upstream secret in detail
    ViewSecret,
    /// Copy decrypted secret to system clipboard (full dump).
    CopySecretFull,
    /// Copy primary secret only (API key or OAuth access_token).
    CopySecretPrimary,
    /// Route wizard: add target
    WizardAddTarget,
    /// Route wizard: remove current target
    WizardRemoveTarget,
    /// Route wizard: cycle strategy
    WizardCycleStrategy,
    /// Route wizard: next / prev step
    WizardNextStep,
    WizardPrevStep,
    /// Provider form: cycle kind preset
    CycleKind,
    /// Providers tab: set / rotate upstream API key
    SetSecret,
    /// Pricing: toggle merged table ↔ operator overrides
    TogglePricingView,
    /// Usage: cycle sort (cost / tokens / date)
    CycleUsageSort,
    /// Usage: toggle detail pane (recent | by_model | by_key | by_day)
    CycleUsageDetail,
    /// Start filter input (`/`)
    StartFilter,
    /// Cycle theme: auto → dark → light → auto (`T`)
    ToggleTheme,
    /// Page up / down in lists
    PageUp,
    PageDown,
    /// Jump to first / last
    GoTop,
    GoBottom,
    /// Provider-add chooser: pick option by index (0-based)
    ChooserPick(usize),
}
