//! Claude OAuth / Claude Code upstream relay — full CLIProxyAPI `ClaudeExecutor` parity.
//!
//! - **TLS**: Chrome impersonation via `wreq` + latest Chrome profile
//!   (`chrome_auto()` ≈ utls `HelloChrome_Auto` on Messages)
//! - **URL**: `/v1/messages?beta=true`
//! - **Headers**: Claude Code fingerprint + full Anthropic-Beta list
//! - **Body**: cloak → max_tokens → thinking/sampling → cache → tools → signature → cch
//! - **Response**: reverse tool-name remapping (non-stream + SSE)

mod body;
mod cache;
mod cch;
mod cloak;
mod device_profile;
mod execute;
mod headers;
mod http_client;
mod obfuscate;
mod options;
mod prompts;
mod session;
mod signature;
mod tools;

pub use body::{prepare_oauth_body, PreparedOAuthBody};
pub use device_profile::{
    is_claude_code_client, ClaudeDeviceProfile, ClaudeHeaderDefaults, DEFAULT_USER_AGENT,
};
pub use execute::{chat_oauth, chat_oauth_stream};
pub use headers::{build_claude_oauth_headers, CLAUDE_OAUTH_BETAS};
pub use options::{should_cloak, ClaudeOAuthRelayOptions};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
pub use tools::{reverse_remap_response, reverse_remap_stream_payload};

/// Whether this provider config is the Claude OAuth relay path.
pub fn is_claude_oauth_kind(kind: &str) -> bool {
    matches!(kind, "claude-oauth" | "anthropic-oauth")
}

/// Append `?beta=true` for Claude OAuth Messages URL.
pub fn messages_url_with_beta(base_chat_url: &str) -> String {
    if base_chat_url.contains('?') {
        format!("{base_chat_url}&beta=true")
    } else {
        format!("{base_chat_url}?beta=true")
    }
}

/// Prepare body for Claude OAuth (CLIProxyAPI OAuth path).
pub fn prepare_request(
    body: Value,
    model: &str,
    secret: &SecretString,
    opts: &ClaudeOAuthRelayOptions,
) -> PreparedOAuthBody {
    prepare_oauth_body(body, model, secret.expose_secret(), opts)
}
