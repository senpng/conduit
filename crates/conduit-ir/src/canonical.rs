use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::loss::LossReport;

// ---------------------------------------------------------------------------
// Content types
// ---------------------------------------------------------------------------

/// A single piece of content within a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CanonicalContent {
    Text {
        text: String,
    },
    Image {
        url: String,
        media_type: Option<String>,
        detail: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: Vec<CanonicalContent>,
        is_error: Option<bool>,
    },
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Tool types
// ---------------------------------------------------------------------------

/// A tool call made by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// A tool result returned to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

/// Definition of a tool available to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

/// Controls which tool(s) the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolChoice {
    /// Model decides whether to call a tool.
    Auto,
    /// Model must call exactly one tool.
    Required,
    /// Model must not call any tool.
    None,
    /// Model must call this specific tool.
    Tool { name: String },
    /// Model must call one of these tools. Some providers degrade to Required.
    AnyOf { names: Vec<String> },
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

/// Speaker role in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single turn in a conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub role: Role,
    pub content: Vec<CanonicalContent>,
    /// Optional name for multi-agent scenarios.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl CanonicalMessage {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![CanonicalContent::Text { text: text.into() }],
            name: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![CanonicalContent::Text { text: text.into() }],
            name: None,
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![CanonicalContent::Text { text: text.into() }],
            name: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Response format
// ---------------------------------------------------------------------------

/// Constrains the output format of the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema {
        schema: serde_json::Value,
        strict: Option<bool>,
    },
}

// ---------------------------------------------------------------------------
// Sampling parameters
// ---------------------------------------------------------------------------

/// Sampling hyper-parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Sampling {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u8>,
    /// OpenAI-style reasoning effort (`none` / `minimal` / `low` / `medium` / `high` / `xhigh` / `auto`).
    /// Populated from Anthropic `thinking` when decoding Claude → IR.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// OpenAI Codex Responses service tier. `priority` enables the Fast tier
    /// for models that support it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

// ---------------------------------------------------------------------------
// Request metadata
// ---------------------------------------------------------------------------

/// Caller-supplied metadata attached to every request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RequestMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Canonical chat request
// ---------------------------------------------------------------------------

/// The canonical representation of a chat-completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalChatRequest {
    /// Stable ULID assigned at ingress.
    pub id: String,
    /// The virtual model alias the caller requested.
    pub alias: String,
    pub messages: Vec<CanonicalMessage>,
    #[serde(default)]
    pub tools: Vec<ToolDef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
    pub sampling: Sampling,
    pub meta: RequestMeta,
    pub stream: bool,
    /// Codec degradation warnings accumulated while converting this request.
    #[serde(default)]
    pub loss_report: LossReport,
}

impl CanonicalChatRequest {
    pub fn new(alias: impl Into<String>, messages: Vec<CanonicalMessage>) -> Self {
        Self {
            id: ulid::Ulid::new().to_string(),
            alias: alias.into(),
            messages,
            tools: Vec::new(),
            tool_choice: None,
            response_format: None,
            sampling: Sampling::default(),
            meta: RequestMeta::default(),
            stream: false,
            loss_report: LossReport::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Token usage
// ---------------------------------------------------------------------------

/// Token usage breakdown for a completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Extended thinking / chain-of-thought tokens (o1/o3/Claude extended thinking).
    pub reasoning_tokens: u32,
    /// Prompt tokens served from the provider's KV cache.
    #[serde(default)]
    pub cache_read_tokens: u32,
    /// Prompt tokens written into the provider's KV cache.
    #[serde(default)]
    pub cache_write_tokens: u32,
}

impl Usage {
    pub fn merge(&mut self, other: &Usage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
    }
}

// ---------------------------------------------------------------------------
// Finish reason
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    /// Provider-specific finish reason that has no canonical mapping.
    Other(String),
}

// ---------------------------------------------------------------------------
// Streaming chunk types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockKind {
    Text,
    ToolUse,
    Thinking,
}

/// An incremental delta within a streaming chunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

/// A single streaming chunk from the provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CanonicalChunk {
    pub request_id: String,
    pub index: u32,
    pub block_index: u32,
    pub block_kind: Option<BlockKind>,
    pub delta: Option<BlockDelta>,
    /// Present only on the final chunk.
    pub finish_reason: Option<FinishReason>,
    /// Present only on the final chunk.
    pub usage: Option<Usage>,
    /// Tool call id when this chunk begins a tool call block.
    pub tool_use_id: Option<String>,
    /// Tool name when this chunk begins a tool call block.
    pub tool_name: Option<String>,
}

impl CanonicalChunk {
    /// A text-delta chunk (`block_kind = Text`), all other fields defaulted.
    pub fn text_delta(text: impl Into<String>) -> Self {
        Self {
            block_kind: Some(BlockKind::Text),
            delta: Some(BlockDelta::TextDelta { text: text.into() }),
            ..Self::default()
        }
    }

    /// A thinking-delta chunk (`block_kind = Thinking`), all other fields defaulted.
    pub fn thinking_delta(thinking: impl Into<String>) -> Self {
        Self {
            block_kind: Some(BlockKind::Thinking),
            delta: Some(BlockDelta::ThinkingDelta {
                thinking: thinking.into(),
            }),
            ..Self::default()
        }
    }

    /// A terminal chunk carrying only a finish reason and optional usage.
    pub fn finish(reason: FinishReason, usage: Option<Usage>) -> Self {
        Self {
            finish_reason: Some(reason),
            usage,
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Non-streaming response
// ---------------------------------------------------------------------------

/// A complete (non-streaming) chat-completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalChatResponse {
    pub id: String,
    pub request_id: String,
    pub model: String,
    pub choices: Vec<CanonicalMessage>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// ---------------------------------------------------------------------------
// Model info / health
// ---------------------------------------------------------------------------

/// Capabilities a model supports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Capabilities {
    pub vision: bool,
    pub function_calling: bool,
    pub json_mode: bool,
    pub streaming: bool,
    pub extended_thinking: bool,
    pub context_length: u32,
}

/// Metadata about a model available on a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    pub capabilities: Capabilities,
    pub input_price_per_mtok: f64,
    pub output_price_per_mtok: f64,
}

/// Current health of a provider endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_message_constructors() {
        let m = CanonicalMessage::system("hello");
        assert_eq!(m.role, Role::System);
        assert_eq!(
            m.content,
            vec![CanonicalContent::Text {
                text: "hello".into()
            }]
        );
    }

    #[test]
    fn usage_merge() {
        let mut a = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let b = Usage {
            prompt_tokens: 20,
            completion_tokens: 10,
            total_tokens: 30,
            reasoning_tokens: 3,
            cache_read_tokens: 1,
            cache_write_tokens: 2,
        };
        a.merge(&b);
        assert_eq!(a.prompt_tokens, 30);
        assert_eq!(a.completion_tokens, 15);
        assert_eq!(a.total_tokens, 45);
        assert_eq!(a.reasoning_tokens, 3);
        assert_eq!(a.cache_read_tokens, 1);
        assert_eq!(a.cache_write_tokens, 2);
    }

    #[test]
    fn canonical_request_roundtrip() {
        let req = CanonicalChatRequest::new("gpt-4o", vec![CanonicalMessage::user("hi")]);
        let json = serde_json::to_string(&req).unwrap();
        let decoded: CanonicalChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req.alias, decoded.alias);
        assert_eq!(req.messages.len(), decoded.messages.len());
    }

    #[test]
    fn tool_choice_serialization() {
        let tc = ToolChoice::AnyOf {
            names: vec!["search".into(), "calc".into()],
        };
        let json = serde_json::to_value(&tc).unwrap();
        assert_eq!(json["type"], "any_of");
        assert_eq!(json["names"][0], "search");
    }

    #[test]
    fn finish_reason_other() {
        let fr = FinishReason::Other("provider_specific".into());
        let json = serde_json::to_string(&fr).unwrap();
        let back: FinishReason = serde_json::from_str(&json).unwrap();
        assert_eq!(fr, back);
    }
}
