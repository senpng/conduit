pub mod anthropic;
pub mod openai;
pub mod openai_responses;

use conduit_ir::{
    canonical::{CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk},
    error::CodecError,
    loss::LossReport,
};
pub use openai::{
    convert_responses_to_chat_completions, should_treat_as_responses_format, OpenAiCodec,
};
pub use openai_responses::{
    apply_codex_chatgpt_account_body, can_reset_responses_continuation,
    merge_responses_continuation, prepare_responses_continuation, reset_responses_continuation,
    response_output_items, responses_store_enabled, OpenAiResponsesCodec, ResponsesContinuation,
    ResponsesContinuationRequest, ResponsesStreamEncoder,
};
use serde_json::Value;

/// Translates between the canonical IR and provider-specific wire JSON.
///
/// All methods are static (no `self`) so implementations are zero-sized types
/// that can be selected at compile time or erased via `Box<dyn ...>` adapters.
pub trait WireCodec: Send + Sync + 'static {
    /// Encode a canonical request into the provider's wire JSON format.
    ///
    /// Returns `(wire_body, loss)`.  Any degradations applied during encoding
    /// (e.g. unsupported fields stripped, tool_choice downgraded) are recorded
    /// in the returned `LossReport`.  The input `req` is **never mutated**.
    fn encode_request(req: &CanonicalChatRequest, stream: bool) -> (Value, LossReport);

    /// Decode a provider wire request body into the canonical IR.
    fn decode_request(
        body: Value,
        alias: String,
        stream: bool,
        request_id: String,
        key_id: String,
    ) -> Result<CanonicalChatRequest, CodecError>;

    /// Decode a provider non-streaming response body into the canonical IR.
    ///
    /// Returns `(response, loss)` where `loss` records any fields that could
    /// not be represented in the canonical IR.
    fn decode_response(
        body: Value,
        alias: &str,
    ) -> Result<(CanonicalChatResponse, LossReport), CodecError>;

    /// Encode a canonical response into the provider's wire JSON format.
    fn encode_response(resp: &CanonicalChatResponse) -> Value;

    /// Encode a canonical streaming chunk into SSE line(s).
    ///
    /// Returns `(sse_line, loss)`.  `sse_line` is `None` when the chunk
    /// produces no SSE output.
    fn encode_chunk(chunk: &CanonicalChunk, resp_id: &str) -> (Option<String>, LossReport);

    /// Decode a single SSE `data:` line into zero or more canonical chunks.
    ///
    /// Returns an empty vec for the `[DONE]` sentinel or empty/ping lines.
    /// One upstream frame may expand to multiple IR chunks (e.g. content +
    /// finish_reason, or reasoning + tool_calls).
    fn decode_chunk(data: &str) -> Result<(Vec<CanonicalChunk>, LossReport), CodecError>;

    /// Per-stream decode state (default: none). Responses/Codex needs this so
    /// `response.completed` does not re-emit text already streamed via deltas.
    type StreamState: Default + Send + 'static;

    /// Stateful decode for a single SSE data line. Default forwards to
    /// [`Self::decode_chunk`] and ignores state.
    fn decode_chunk_stateful(
        _state: &mut Self::StreamState,
        data: &str,
    ) -> Result<(Vec<CanonicalChunk>, LossReport), CodecError> {
        Self::decode_chunk(data)
    }

    /// Build a provider-shaped error body JSON.
    fn error_body(type_: &str, code: Option<&str>, message: &str) -> Value;

    /// Format an error as a single SSE frame so streams can carry error info.
    fn stream_error_sse(message: &str) -> String;
}
