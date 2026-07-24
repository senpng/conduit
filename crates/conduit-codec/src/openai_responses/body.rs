//! Request body transforms for Codex compact + ChatGPT-account constraints.

use serde_json::{json, Value};

/// Prepare a Responses body for `POST …/responses/compact` (CLIProxyAPI parity).
///
/// Unlike normal Codex chat, compact is **non-stream JSON** and must preserve
/// free-form input items such as `compaction_trigger`. Do not IR-round-trip.
///
/// - rewrite `model`
/// - drop `stream` (upstream rejects streaming compact)
/// - ensure `instructions` is a string (default `""`)
/// - map `system` → `developer` in input messages
/// - strip invalid `reasoning.encrypted_content` (and orphan ids when `store`≠true)
pub fn prepare_responses_compact_body(mut body: Value, model: &str) -> Value {
    body["model"] = json!(model);
    if let Some(obj) = body.as_object_mut() {
        obj.remove("stream");
        // Compact is a one-shot summarize call; sampling knobs are unused.
        for key in [
            "temperature",
            "top_p",
            "top_k",
            "max_output_tokens",
            "max_completion_tokens",
            "max_tokens",
            "stream_options",
        ] {
            obj.remove(key);
        }
    }
    match body.get("instructions") {
        None | Some(Value::Null) => {
            body["instructions"] = json!("");
        }
        _ => {}
    }
    if let Some(input) = body.get_mut("input").and_then(|v| v.as_array_mut()) {
        for item in input.iter_mut() {
            if item.get("role").and_then(|r| r.as_str()) == Some("system") {
                item["role"] = json!("developer");
            }
        }
    }
    sanitize_responses_reasoning_encrypted_content(&mut body);
    body
}

/// Drop invalid GPT/Codex reasoning `encrypted_content` fields from `input`.
///
/// Aligns with CLIProxyAPI `sanitizeOpenAIResponsesReasoningEncryptedContent`:
/// non-string / whitespace / bad shape → remove field; when `store` is not true,
/// also drop orphan reasoning `id`s that would trigger store lookups.
pub fn sanitize_responses_reasoning_encrypted_content(body: &mut Value) {
    let store_true = body.get("store").and_then(Value::as_bool).unwrap_or(false);
    let strip_orphan_ids = !store_true;
    let Some(input) = body.get_mut("input").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for item in input.iter_mut() {
        if item.get("type").and_then(Value::as_str) != Some("reasoning") {
            continue;
        }
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        match obj.get("encrypted_content") {
            None => {
                if strip_orphan_ids {
                    obj.remove("id");
                }
            }
            Some(Value::Null) => {
                obj.remove("encrypted_content");
                if strip_orphan_ids {
                    obj.remove("id");
                }
            }
            Some(Value::String(s)) => {
                if !is_plausible_gpt_reasoning_signature(s) {
                    obj.remove("encrypted_content");
                    if strip_orphan_ids {
                        obj.remove("id");
                    }
                }
            }
            Some(_) => {
                obj.remove("encrypted_content");
                if strip_orphan_ids {
                    obj.remove("id");
                }
            }
        }
    }
}

/// Transport-shape check for GPT/Codex reasoning encrypted_content (Fernet-like).
/// Not a cryptographic verify — only rejects obviously unusable values.
fn is_plausible_gpt_reasoning_signature(raw: &str) -> bool {
    let sig = raw.trim();
    if sig.is_empty() || sig.len() != raw.len() {
        // empty or has surrounding whitespace → invalid
        return false;
    }
    if sig.len() > 32 * 1024 * 1024 {
        return false;
    }
    if !sig.starts_with("gAAAA") {
        return false;
    }
    if !sig
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'='))
    {
        return false;
    }
    // Prefer raw URL-safe base64; fall back to padded URL encoding.
    use base64::Engine;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(sig)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(sig));
    let Ok(decoded) = decoded else {
        return false;
    };
    // version(1) + timestamp(8) + iv(16) + hmac(32) + ciphertext(≥16, multiple of 16)
    if decoded.len() < 73 || decoded[0] != 0x80 {
        return false;
    }
    let ciphertext_len = decoded.len() - 1 - 8 - 16 - 32;
    ciphertext_len > 0 && ciphertext_len % 16 == 0
}

/// Apply CLIProxyAPI-style ChatGPT-account Codex request constraints.
///
/// Upstream rules observed on `chatgpt.com/backend-api/codex/responses`:
/// - `stream` must be `true` (non-stream clients are aggregated by the proxy)
/// - `store` must be `false`
/// - no `temperature` / `top_p` / `max_output_tokens`
/// - `instructions` present (empty string ok)
/// - `system` role → `developer`
/// - optional Codex defaults: reasoning, include encrypted content, parallel tools
pub fn apply_codex_chatgpt_account_body(mut body: Value) -> Value {
    body["stream"] = json!(true);
    body["store"] = json!(false);

    if let Some(obj) = body.as_object_mut() {
        for key in [
            "temperature",
            "top_p",
            "top_k",
            "max_output_tokens",
            "max_completion_tokens",
            "max_tokens",
            "user",
            "truncation",
            "context_management",
            "stream_options",
            "previous_response_id",
            "prompt_cache_retention",
            "safety_identifier",
        ] {
            obj.remove(key);
        }
    }

    match body.get("instructions") {
        None | Some(Value::Null) => {
            body["instructions"] = json!("");
        }
        _ => {}
    }

    if body.get("parallel_tool_calls").is_none() {
        body["parallel_tool_calls"] = json!(true);
    }
    if body.get("include").is_none() {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    // Preserve caller-provided reasoning if already set by encode_request.
    if body.get("reasoning").is_none() {
        body["reasoning"] = json!({
            "effort": "medium",
            "summary": "auto",
        });
    } else if body.pointer("/reasoning/summary").is_none() {
        if let Some(r) = body.get_mut("reasoning").and_then(|v| v.as_object_mut()) {
            r.insert("summary".into(), json!("auto"));
        }
    }

    // Codex rejects role "system" in input — map to "developer" (CLIProxyAPI parity).
    if let Some(input) = body.get_mut("input").and_then(|v| v.as_array_mut()) {
        for item in input.iter_mut() {
            if item.get("role").and_then(|r| r.as_str()) == Some("system") {
                item["role"] = json!("developer");
            }
        }
    }

    body
}
