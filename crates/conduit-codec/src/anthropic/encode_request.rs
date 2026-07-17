use conduit_ir::canonical::{
    CanonicalChatRequest, CanonicalContent, CanonicalMessage, Role, ToolChoice, ToolDef,
};
use serde_json::{json, Value};

/// Encode a canonical request into the Anthropic Messages API wire format.
pub fn encode_request(req: &CanonicalChatRequest, stream: bool) -> Value {
    let mut body = json!({
        "model": req.alias,
        "stream": stream,
        "max_tokens": req.sampling.max_tokens.unwrap_or(4096),
    });

    // System content: extracted from system-role messages.
    let system_parts: Vec<Value> = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .flat_map(|m| encode_content(&m.content))
        .collect();

    if !system_parts.is_empty() {
        if system_parts.len() == 1 {
            if let Some(text) = system_parts[0]["text"].as_str() {
                body["system"] = json!(text);
            } else {
                body["system"] = json!(system_parts);
            }
        } else {
            body["system"] = json!(system_parts);
        }
    }

    // Non-system messages
    let non_system: Vec<&CanonicalMessage> = req
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .collect();
    body["messages"] = json!(encode_messages(&non_system));

    // Tools — omit entirely when ToolChoice::None
    if !req.tools.is_empty() && !matches!(req.tool_choice, Some(ToolChoice::None)) {
        body["tools"] = json!(encode_tools(&req.tools));
        if let Some(tc) = &req.tool_choice {
            if let Some(tc_val) = encode_tool_choice(tc) {
                body["tool_choice"] = tc_val;
            }
        }
    }

    // Sampling
    let s = &req.sampling;
    if let Some(t) = s.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = s.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(k) = s.top_k {
        body["top_k"] = json!(k);
    }
    if let Some(stop) = &s.stop {
        if !stop.is_empty() {
            body["stop_sequences"] = json!(stop);
        }
    }

    body
}

/// Encode a slice of canonical content blocks into Anthropic block JSON.
pub fn encode_content(content: &[CanonicalContent]) -> Vec<Value> {
    content
        .iter()
        .filter_map(|c| match c {
            CanonicalContent::Text { text } => Some(json!({"type": "text", "text": text})),
            CanonicalContent::Image {
                url,
                media_type,
                detail: _,
            } => {
                // Validate MIME type if provided.
                if let Some(mt) = media_type {
                    if !is_valid_image_mime(mt) {
                        tracing::warn!(mime = %mt, "Skipping image with unsupported MIME type");
                        return None;
                    }
                    // If it looks like a data URL, extract base64.
                    if url.starts_with("data:") {
                        if let Some(b64) = extract_data_url_b64(url) {
                            return Some(json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": mt,
                                    "data": b64,
                                }
                            }));
                        }
                    }
                }
                Some(json!({
                    "type": "image",
                    "source": {"type": "url", "url": url}
                }))
            }
            CanonicalContent::Thinking {
                thinking,
                signature,
            } => {
                let mut v = json!({"type": "thinking", "thinking": thinking});
                if let Some(sig) = signature {
                    v["signature"] = json!(sig);
                }
                Some(v)
            }
            CanonicalContent::ToolUse { id, name, input } => {
                Some(json!({"type": "tool_use", "id": id, "name": name, "input": input}))
            }
            CanonicalContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let inner = encode_content(content);
                Some(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": inner,
                    "is_error": is_error.unwrap_or(false),
                }))
            }
            _ => None,
        })
        .collect()
}

fn encode_messages(messages: &[&CanonicalMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let role = match msg.role {
                Role::User | Role::Tool => "user",
                Role::Assistant => "assistant",
                Role::System => "user", // should not happen after filtering
                _ => "user",
            };
            let content = encode_content(&msg.content);
            // Inline string for single-text messages (cleaner wire).
            if content.len() == 1 {
                if let Some(text) = content[0]["text"].as_str() {
                    return json!({"role": role, "content": text});
                }
            }
            json!({"role": role, "content": content})
        })
        .collect()
}

fn encode_tools(tools: &[ToolDef]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let mut v = json!({
                "name": t.name,
                "input_schema": t.parameters,
            });
            if let Some(desc) = &t.description {
                v["description"] = json!(desc);
            }
            v
        })
        .collect()
}

fn encode_tool_choice(tc: &ToolChoice) -> Option<Value> {
    match tc {
        ToolChoice::Auto => Some(json!({"type": "auto"})),
        ToolChoice::None => None,
        ToolChoice::Required => Some(json!({"type": "any"})),
        ToolChoice::AnyOf { .. } => Some(json!({"type": "any"})),
        ToolChoice::Tool { name } => Some(json!({"type": "tool", "name": name})),
        _ => None,
    }
}

fn is_valid_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    )
}

fn extract_data_url_b64(url: &str) -> Option<&str> {
    // Format: data:<mime>;base64,<data>
    let comma = url.find(',')?;
    Some(&url[comma + 1..])
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduit_ir::canonical::{CanonicalChatRequest, CanonicalMessage};

    use super::*;

    #[test]
    fn basic_encode() {
        let req =
            CanonicalChatRequest::new("claude-3-5-sonnet", vec![CanonicalMessage::user("Hi")]);
        let v = encode_request(&req, false);
        assert_eq!(v["model"].as_str().unwrap(), "claude-3-5-sonnet");
        assert!(v["messages"].as_array().is_some());
    }

    #[test]
    fn system_hoisted() {
        let req = CanonicalChatRequest::new(
            "claude-3-5-sonnet",
            vec![
                CanonicalMessage::system("You are helpful"),
                CanonicalMessage::user("Hello"),
            ],
        );
        let v = encode_request(&req, false);
        assert_eq!(v["system"].as_str().unwrap(), "You are helpful");
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"].as_str().unwrap(), "user");
    }

    #[test]
    fn tool_choice_none_omits_tools() {
        use conduit_ir::canonical::ToolDef;
        let mut req = CanonicalChatRequest::new("c3", vec![CanonicalMessage::user("hi")]);
        req.tools = vec![ToolDef {
            name: "fn".into(),
            description: None,
            parameters: json!({"type": "object"}),
        }];
        req.tool_choice = Some(ToolChoice::None);
        let v = encode_request(&req, false);
        assert!(v.get("tools").is_none());
        assert!(v.get("tool_choice").is_none());
    }

    #[test]
    fn invalid_mime_image_skipped() {
        let content = vec![CanonicalContent::Image {
            url: "https://example.com/img.bmp".into(),
            media_type: Some("image/bmp".into()),
            detail: None,
        }];
        let result = encode_content(&content);
        assert!(result.is_empty(), "unsupported MIME should be skipped");
    }
}
