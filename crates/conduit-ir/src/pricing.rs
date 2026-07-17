//! Shared pricing helpers.
//!
//! Cost lookup keys a price row by `(provider_kind, model_id)`, but the same
//! model is often priced under a different `provider_kind` string than the one
//! a request arrives with — e.g. an OAuth kind (`grok-oauth`) whose prices are
//! stored under the base provider (`xai` / `openai`). [`pricing_kind_aliases`]
//! is the single source of truth for those fallbacks, shared by every layer
//! that resolves prices (the store repo, the daemon's hot-path map, and the
//! pipeline's egress cost calculation) so they can never drift apart.

/// Alternate `provider_kind` strings to try when an exact price lookup misses.
///
/// Returned in priority order. The input's own kind is *not* included (callers
/// try the exact kind first, then these aliases).
pub fn pricing_kind_aliases(kind: &str) -> &'static [&'static str] {
    match kind.trim().to_ascii_lowercase().as_str() {
        "grok-oauth" | "grok" | "xai-oauth" => &["xai", "grok-oauth", "openai"],
        "xai" => &["grok-oauth", "openai"],
        "claude-oauth" | "anthropic-oauth" => &["anthropic", "claude-oauth"],
        "codex-oauth" | "codex" => &["codex", "codex-oauth", "openai"],
        "anthropic" => &["claude-oauth"],
        "openai" => &["codex-oauth"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_kinds_fall_back_to_base_provider() {
        assert_eq!(
            pricing_kind_aliases("grok-oauth"),
            &["xai", "grok-oauth", "openai"]
        );
        assert_eq!(
            pricing_kind_aliases("claude-oauth"),
            &["anthropic", "claude-oauth"]
        );
        assert_eq!(
            pricing_kind_aliases("codex-oauth"),
            &["codex", "codex-oauth", "openai"]
        );
    }

    #[test]
    fn case_and_whitespace_insensitive() {
        assert_eq!(
            pricing_kind_aliases("  GROK  "),
            &["xai", "grok-oauth", "openai"]
        );
    }

    #[test]
    fn unknown_kind_has_no_aliases() {
        assert!(pricing_kind_aliases("mistral").is_empty());
    }
}
