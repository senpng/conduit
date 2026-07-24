//! OAuth flow overlay state (provider add / re-auth).

use crate::dto::ProviderView;

use super::super::input::InputField;

/// OAuth subscription kinds, in chooser / cycle order. Single source of truth
/// for [`OauthFlow::kind`] / [`OauthFlow::cycle_kind`] / [`OauthFlow::start_new`]
/// so the string list and its indices can't drift apart.
pub const OAUTH_KINDS: &[&str] = &["claude", "codex", "grok"];


#[derive(Debug, Clone)]
pub struct OauthFlow {
    /// 0 = claude, 1 = codex, 2 = grok
    pub kind_idx: usize,
    pub name: InputField,
    pub provider_id: InputField,
    pub focus: usize,
    pub pending_session_id: Option<String>,
    pub session_status: Option<String>,
    pub auth_url: Option<String>,
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub result_message: Option<String>,
    pub error: Option<String>,
    pub poll_ticks: u32,
    /// True while a status poll is in flight, so `on_tick` doesn't stack a new
    /// poll on top of a slow/hung one (which would let dozens accumulate).
    pub poll_inflight: bool,
}

impl OauthFlow {
    pub fn new() -> Self {
        Self {
            kind_idx: 0,
            name: InputField::new(""),
            provider_id: InputField::new(""),
            focus: 0,
            pending_session_id: None,
            session_status: None,
            auth_url: None,
            user_code: None,
            verification_uri: None,
            result_message: None,
            error: None,
            poll_ticks: 0,
            poll_inflight: false,
        }
    }

    /// Start a new OAuth provider (from Providers → add).
    pub fn start_new(kind_idx: usize, name: &str) -> Self {
        let mut f = Self::new();
        f.kind_idx = kind_idx.min(OAUTH_KINDS.len() - 1);
        if !name.is_empty() {
            f.name = InputField::new(name);
        }
        f.focus = 1; // name field first; kind is fixed from chooser
        f
    }

    /// Re-authenticate an existing OAuth provider row.
    pub fn reauth(p: &ProviderView) -> Self {
        let mut f = Self::new();
        f.provider_id = InputField::new(&p.id);
        f.name = InputField::new(&p.name);
        f.kind_idx = if p.kind.contains("claude") {
            0
        } else if p.kind.contains("codex") {
            1
        } else if p.kind.contains("grok") || p.kind.contains("xai") {
            2
        } else {
            0
        };
        f.focus = 0;
        f
    }

    pub fn kind(&self) -> &'static str {
        OAUTH_KINDS[self.kind_idx.min(OAUTH_KINDS.len() - 1)]
    }

    pub fn cycle_kind(&mut self) {
        // Kind is usually fixed by the add chooser; still allow cycle when re-authing.
        self.kind_idx = (self.kind_idx + 1) % OAUTH_KINDS.len();
    }
}


