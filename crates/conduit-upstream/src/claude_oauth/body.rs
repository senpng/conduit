//! Request body transforms for Claude OAuth relay (CLIProxyAPI parity).
//!
//! Pipeline order matches `ClaudeExecutor.Execute` for OAuth tokens:
//! cloak → max_tokens → thinking/sampling fixes → cache → betas → tools →
//! signature sanitize → web_search → cch sign.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::{
    cache::{enforce_cache_control_limit, ensure_if_missing, normalize_cache_control_ttl},
    cch::sign_anthropic_messages_body,
    cloak::{apply_oauth_system_cloak, inject_fake_user_id},
    obfuscate::{obfuscate_sensitive_words, SensitiveWordMatcher},
    options::{should_cloak, ClaudeOAuthRelayOptions},
    signature::{
        ensure_model_max_tokens, extract_and_remove_betas,
        rebuild_mid_system_messages_to_top_level, sanitize_claude_messages_for_claude_upstream,
    },
    tools::remap_oauth_tool_names,
};

/// Result of preparing an Anthropic Messages body for OAuth upstream.
#[derive(Debug)]
pub struct PreparedOAuthBody {
    pub body: Value,
    /// Upstream tool name → original client name (for response restore).
    pub tool_reverse_map: HashMap<String, String>,
    /// Extra beta flags from body `betas` field (merge into Anthropic-Beta header).
    pub extra_betas: Vec<String>,
}

/// Delete temperature; when thinking is active also drop top_p/top_k.
pub fn normalize_claude_sampling(mut body: Value) -> Value {
    if let Some(obj) = body.as_object_mut() {
        obj.remove("temperature");
    }
    let thinking_type = body
        .pointer("/thinking/type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match thinking_type.as_str() {
        "enabled" | "adaptive" | "auto" => {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("top_p");
                obj.remove("top_k");
            }
        }
        _ => {}
    }
    body
}

/// Default `thinking.display` to `summarized` when thinking is on.
pub fn ensure_claude_thinking_display(mut body: Value) -> Value {
    let thinking_type = body
        .pointer("/thinking/type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match thinking_type.as_str() {
        "enabled" | "adaptive" | "auto" => {}
        _ => return body,
    }
    let display = body
        .pointer("/thinking/display")
        .and_then(|d| d.as_str())
        .unwrap_or("");
    if !display.is_empty() {
        return body;
    }
    if let Some(thinking) = body.get_mut("thinking").and_then(|t| t.as_object_mut()) {
        thinking.insert("display".into(), json!("summarized"));
    }
    body
}

/// Disable thinking when tool_choice forces tool use.
pub fn disable_thinking_if_tool_choice_forced(mut body: Value) -> Value {
    let tc = body
        .pointer("/tool_choice/type")
        .and_then(|t| t.as_str())
        .unwrap_or("");
    if tc == "any" || tc == "tool" {
        if let Some(obj) = body.as_object_mut() {
            obj.remove("thinking");
        }
        if let Some(oc) = body
            .get_mut("output_config")
            .and_then(|o| o.as_object_mut())
        {
            oc.remove("effort");
            if oc.is_empty() {
                body.as_object_mut().map(|o| o.remove("output_config"));
            }
        }
    }
    body
}

/// Remove empty `allowed_domains` / `blocked_domains` on web_search tools.
pub fn sanitize_web_search_domains(mut body: Value) -> Value {
    let Some(tools) = body.get_mut("tools").and_then(|t| t.as_array_mut()) else {
        return body;
    };
    for tool in tools.iter_mut() {
        let ty = tool.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if !ty.starts_with("web_search_") {
            continue;
        }
        for field in ["allowed_domains", "blocked_domains"] {
            if let Some(arr) = tool.get(field).and_then(|a| a.as_array()) {
                if arr.is_empty() {
                    tool.as_object_mut().map(|o| o.remove(field));
                }
            }
        }
    }
    body
}

/// Full OAuth body pipeline matching CLIProxyAPI ClaudeExecutor for OAuth tokens.
pub fn prepare_oauth_body(
    body: Value,
    model: &str,
    access_token: &str,
    opts: &ClaudeOAuthRelayOptions,
) -> PreparedOAuthBody {
    let mut body = body;
    let oauth_mode = true; // this path is always OAuth

    // Optional: hoist mid-conversation system roles
    body = rebuild_mid_system_messages_to_top_level(body);

    // Cloak (CLIProxyAPI applyCloaking)
    let client_ua = opts.client_user_agent();
    if should_cloak(&opts.cloak_mode, client_ua) {
        use super::device_profile::{profile_claude_version, resolve_device_profile};
        let profile =
            resolve_device_profile(access_token, &opts.client_headers, &opts.header_defaults);
        let version = if opts.claude_version.trim().is_empty() {
            profile_claude_version(&profile)
        } else {
            opts.claude_version.trim().to_string()
        };
        let entrypoint = opts.effective_entrypoint();
        body = apply_oauth_system_cloak(
            body,
            model,
            &version,
            &entrypoint,
            opts.strict_mode,
            oauth_mode,
        );
        body = inject_fake_user_id(body, access_token, opts.cache_user_id);
        if let Some(matcher) = SensitiveWordMatcher::build(&opts.sensitive_words) {
            body = obfuscate_sensitive_words(body, &matcher);
        }
    }

    // max_tokens safety
    body = ensure_model_max_tokens(body, 4096);

    // Thinking / sampling constraints
    body = disable_thinking_if_tool_choice_forced(body);
    body = normalize_claude_sampling(body);
    body = ensure_claude_thinking_display(body);

    // cache_control
    body = ensure_if_missing(body);
    body = enforce_cache_control_limit(body, 4);
    body = normalize_cache_control_ttl(body);

    // body betas → header
    let (extra_betas, body) = extract_and_remove_betas(body);

    // OAuth tool rename
    let (mut body, tool_reverse_map) = remap_oauth_tool_names(body);

    // Signature sanitize for Claude upstream
    body = sanitize_claude_messages_for_claude_upstream(body);
    body = sanitize_web_search_domains(body);

    // cch over final body (Claude Code always signs on OAuth)
    body = sign_anthropic_messages_body(body);

    PreparedOAuthBody {
        body,
        tool_reverse_map,
        extra_betas,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn prepare_sets_billing_and_removes_temperature() {
        let body = json!({
            "model": "claude-sonnet-4",
            "temperature": 0.7,
            "system": "custom",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"name": "bash", "input_schema": {"type": "object"}}],
            "betas": ["custom-beta-1"]
        });
        let prepared = prepare_oauth_body(
            body,
            "claude-sonnet-4",
            "sk-ant-oat-test",
            &ClaudeOAuthRelayOptions::default(),
        );
        assert!(prepared.body.get("temperature").is_none());
        assert!(prepared.body.get("betas").is_none());
        assert_eq!(prepared.extra_betas, vec!["custom-beta-1"]);
        assert_eq!(prepared.body["tools"][0]["name"], "Bash");
        assert!(prepared.body["system"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("x-anthropic-billing-header:"));
        assert!(prepared.tool_reverse_map.contains_key("Bash"));
        let bill = prepared.body["system"][0]["text"].as_str().unwrap();
        assert!(!bill.contains("cch=00000;"), "expected signed cch: {bill}");
        assert!(prepared
            .body
            .get("metadata")
            .and_then(|m| m.get("user_id"))
            .is_some());
    }

    #[test]
    fn never_cloak_skips_billing() {
        let body = json!({
            "model": "claude-sonnet-4",
            "system": "custom",
            "messages": [{"role": "user", "content": "hi"}]
        });
        let opts = ClaudeOAuthRelayOptions {
            cloak_mode: "never".into(),
            ..Default::default()
        };
        let prepared = prepare_oauth_body(body, "claude-sonnet-4", "sk-ant-oat-test", &opts);
        // No billing cloak; cache_control may rewrite system to array form.
        let sys_text = match &prepared.body["system"] {
            serde_json::Value::String(s) => s.as_str(),
            serde_json::Value::Array(a) => a[0]["text"].as_str().unwrap_or(""),
            _ => "",
        };
        assert_eq!(sys_text, "custom");
        assert!(!sys_text.starts_with("x-anthropic-billing-header:"));
        assert!(prepared
            .body
            .get("metadata")
            .and_then(|m| m.get("user_id"))
            .is_none());
    }

    #[test]
    fn pipeline_order_strips_forced_thinking() {
        let body = json!({
            "model": "claude-sonnet-4",
            "thinking": {"type": "enabled", "budget_tokens": 1024},
            "tool_choice": {"type": "any"},
            "messages": [{"role": "user", "content": "hi"}]
        });
        let prepared = prepare_oauth_body(
            body,
            "claude-sonnet-4",
            "sk-ant-oat-test",
            &ClaudeOAuthRelayOptions::default(),
        );
        assert!(prepared.body.get("thinking").is_none());
    }
}
