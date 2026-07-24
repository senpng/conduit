//! Shared JSON/SSE helpers for the Responses codec.

use conduit_ir::canonical::{CanonicalContent, Usage};
use serde_json::{json, Value};

pub(crate) fn sse(event: Value) -> String {
    format!("data: {event}\n\n")
}

pub(crate) fn output_text_part(text: &str) -> Value {
    json!({"type": "output_text", "text": text, "annotations": []})
}

pub(crate) fn text_item_json(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "message",
        "status": status,
        "role": "assistant",
        "content": [output_text_part(text)],
    })
}

pub(crate) fn tool_item_json(id: &str, name: &str, arguments: &str, status: &str) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "status": status,
        "call_id": id,
        "name": name,
        "arguments": arguments,
    })
}

/// Encode IR [`Usage`] as OpenAI Responses `usage` (official SDK shape).
///
/// Stainless/OpenAI Python `ResponseUsage` requires both
/// `input_tokens_details` and `output_tokens_details` objects (not optional).
/// Omitting them makes strict clients fail with:
/// `missing field input_tokens_details`.
pub(crate) fn usage_json(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage.prompt_tokens,
        "input_tokens_details": {
            "cached_tokens": usage.cache_read_tokens,
            "cache_write_tokens": usage.cache_write_tokens,
        },
        "output_tokens": usage.completion_tokens,
        "output_tokens_details": {
            "reasoning_tokens": usage.reasoning_tokens,
        },
        "total_tokens": usage.total_tokens,
    })
}

pub(crate) fn content_to_text(content: &[CanonicalContent]) -> String {
    content
        .iter()
        .filter_map(|c| {
            if let CanonicalContent::Text { text } = c {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(crate) fn parse_usage(u: &Value) -> Usage {
    let prompt = u
        .get("input_tokens")
        .or_else(|| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let completion = u
        .get("output_tokens")
        .or_else(|| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let total = u
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or((prompt + completion) as u64) as u32;
    let reasoning = u
        .pointer("/output_tokens_details/reasoning_tokens")
        .or_else(|| u.pointer("/completion_tokens_details/reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cache_read = u
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cache_write = u
        .pointer("/input_tokens_details/cache_write_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        reasoning_tokens: reasoning,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    }
}
