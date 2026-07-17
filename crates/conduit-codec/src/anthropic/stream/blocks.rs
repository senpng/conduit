use conduit_ir::canonical::{BlockKind, Usage};

/// Tracks the accumulated state of a single content block during streaming.
#[derive(Debug, Clone)]
pub struct BlockState {
    pub index: u32,
    pub kind: BlockKind,
    /// Accumulated text for text/thinking blocks.
    pub text_buf: String,
    /// Accumulated partial JSON for tool_use blocks.
    pub json_buf: String,
    /// Tool ID for tool_use blocks.
    pub tool_id: Option<String>,
    /// Tool name for tool_use blocks.
    pub tool_name: Option<String>,
    /// Thinking signature, filled in on signature_delta.
    pub signature: Option<String>,
}

impl BlockState {
    pub fn new_text(index: u32) -> Self {
        Self {
            index,
            kind: BlockKind::Text,
            text_buf: String::new(),
            json_buf: String::new(),
            tool_id: None,
            tool_name: None,
            signature: None,
        }
    }

    pub fn new_thinking(index: u32) -> Self {
        Self {
            index,
            kind: BlockKind::Thinking,
            text_buf: String::new(),
            json_buf: String::new(),
            tool_id: None,
            tool_name: None,
            signature: None,
        }
    }

    pub fn new_tool_use(index: u32, id: String, name: String) -> Self {
        Self {
            index,
            kind: BlockKind::ToolUse,
            text_buf: String::new(),
            json_buf: String::new(),
            tool_id: Some(id),
            tool_name: Some(name),
            signature: None,
        }
    }
}

/// Usage accumulated across streaming message_delta events.
#[derive(Debug, Default, Clone)]
pub struct StreamUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

impl From<StreamUsage> for Usage {
    fn from(s: StreamUsage) -> Self {
        Usage {
            prompt_tokens: s.prompt_tokens,
            completion_tokens: s.completion_tokens,
            total_tokens: s.prompt_tokens + s.completion_tokens,
            reasoning_tokens: 0,
            cache_read_tokens: s.cache_read_tokens,
            cache_write_tokens: s.cache_write_tokens,
        }
    }
}
