//! Downstream wire protocol for a gateway request.

/// Downstream wire protocol for a gateway request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WireFormat {
    /// OpenAI Chat Completions (`POST /v1/chat/completions`).
    OpenaiChat,
    /// OpenAI Responses (`POST /v1/responses`).
    OpenaiResponses,
    /// Anthropic Messages (`POST /v1/messages`).
    AnthropicMessages,
}

impl WireFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai.chat",
            Self::OpenaiResponses => "openai.responses",
            Self::AnthropicMessages => "anthropic.messages",
        }
    }

    /// Low-cardinality HTTP route template for this wire protocol.
    pub fn http_route(self) -> &'static str {
        match self {
            Self::OpenaiChat => "/v1/chat/completions",
            Self::OpenaiResponses => "/v1/responses",
            Self::AnthropicMessages => "/v1/messages",
        }
    }
}

impl std::fmt::Display for WireFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
