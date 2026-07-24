//! Responses `previous_response_id` continuation protocol helpers.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Durable state required to expand a Responses `previous_response_id` turn.
///
/// This is deliberately protocol-only data. Persistence, tenancy, and expiry
/// policies are supplied by the hosting gateway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponsesContinuation {
    pub input_items: Vec<Value>,
    pub output_items: Vec<Value>,
}

/// Result of inspecting a client Responses request for continuation handling.
pub enum ResponsesContinuationRequest {
    /// The request is self-contained (or supplied a complete replacement
    /// transcript), so it can be decoded immediately.
    Ready(Value),
    /// The gateway must load this response id before decoding the request.
    Incremental {
        previous_response_id: String,
        body: Value,
    },
}

impl ResponsesContinuation {
    pub fn new(input: Value, output_items: Vec<Value>) -> Self {
        Self {
            input_items: responses_input_items(input),
            output_items,
        }
    }

    pub fn from_json(input_items_json: &str, output_items_json: &str) -> serde_json::Result<Self> {
        Ok(Self {
            input_items: serde_json::from_str(input_items_json)?,
            output_items: serde_json::from_str(output_items_json)?,
        })
    }

    pub fn input_items_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&self.input_items)
    }

    pub fn output_items_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&self.output_items)
    }
}

/// Inspect a Responses request before decoding it. Full transcripts are kept as
/// supplied; incremental turns request a continuation lookup.
pub fn prepare_responses_continuation(body: Value) -> ResponsesContinuationRequest {
    let previous_response_id = body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let Some(previous_response_id) = previous_response_id else {
        return ResponsesContinuationRequest::Ready(body);
    };
    if responses_input_contains_full_transcript(&responses_input_items_from_body(&body)) {
        return ResponsesContinuationRequest::Ready(remove_previous_response_id(body));
    }
    ResponsesContinuationRequest::Incremental {
        previous_response_id,
        body,
    }
}

/// Merge a persisted Responses transcript into an incremental request.
pub fn merge_responses_continuation(body: Value, continuation: &ResponsesContinuation) -> Value {
    let mut items = continuation.input_items.clone();
    items.extend(continuation.output_items.clone());
    items.extend(responses_input_items_from_body(&body));
    let mut body = remove_previous_response_id(body);
    body["input"] = Value::Array(dedupe_response_function_calls(items));
    body
}

/// Whether an incremental request can safely start a fresh continuation when
/// its referenced response has expired. Tool outputs cannot: their matching
/// calls are required by the upstream request.
pub fn can_reset_responses_continuation(body: &Value) -> bool {
    !responses_input_items_from_body(body).iter().any(|item| {
        matches!(
            response_item_type(item),
            "function_call_output" | "custom_tool_call_output"
        )
    })
}

/// Remove an unusable response id before forwarding a fresh Responses turn.
pub fn reset_responses_continuation(body: Value) -> Value {
    remove_previous_response_id(body)
}

/// Whether this client permits the Responses server to retain continuation
/// state. The protocol default is `true` when the field is absent.
pub fn responses_store_enabled(body: &Value) -> bool {
    body.get("store").and_then(Value::as_bool).unwrap_or(true)
}

/// Read a complete Responses response object's output items.
pub fn response_output_items(response: &Value) -> Vec<Value> {
    response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn responses_input_items_from_body(body: &Value) -> Vec<Value> {
    body.get("input")
        .cloned()
        .map(responses_input_items)
        .unwrap_or_default()
}

fn responses_input_items(input: Value) -> Vec<Value> {
    match input {
        Value::Array(items) => items,
        Value::String(text) => vec![json!({
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": text}],
        })],
        _ => Vec::new(),
    }
}

fn responses_input_contains_full_transcript(input: &[Value]) -> bool {
    input.iter().any(|item| {
        matches!(
            response_item_type(item),
            "function_call" | "custom_tool_call"
        ) || (response_item_type(item) == "message"
            && item.get("role").and_then(Value::as_str) == Some("assistant"))
    })
}

fn dedupe_response_function_calls(items: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| {
            if response_item_type(item) != "function_call" {
                return true;
            }
            let Some(call_id) = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
            else {
                return true;
            };
            seen.insert(call_id.to_string())
        })
        .collect()
}

fn remove_previous_response_id(mut body: Value) -> Value {
    if let Some(object) = body.as_object_mut() {
        object.remove("previous_response_id");
    }
    body
}

fn response_item_type(item: &Value) -> &str {
    item.get("type").and_then(Value::as_str).unwrap_or_default()
}
