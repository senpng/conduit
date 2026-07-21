//! Typed provider-kind parsing and upstream dispatch.
//!
//! Pipeline call sites only invoke [`dispatch_non_stream`] / [`dispatch_stream`].
//! Adding a new known kind requires updating [`ProviderKind`] and the match arms
//! in this module — not the L2–L7 orchestration in `handle.rs`.

use conduit_ir::{
    canonical::{
        BlockDelta, CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk, CanonicalContent,
        CanonicalMessage, FinishReason, Role, Usage,
    },
    error::ProviderError,
    loss::LossReport,
};
use conduit_upstream::provider::{ProviderClient, ProviderClientConfig};
use conduit_upstream::RateLimitHeaderSink;
use futures::stream::{BoxStream, StreamExt};
use secrecy::SecretString;
use tracing::debug;

use super::context::ResolvedProvider;

/// Known upstream protocol / codec families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    OpenAi,
    Anthropic,
    /// Anthropic Messages API with OAuth Bearer + beta headers.
    ClaudeOAuth,
    /// ChatGPT Codex Responses API with OAuth.
    CodexOAuth,
    /// xAI Grok OpenAI-compatible chat with OAuth.
    GrokOAuth,
}

impl ProviderKind {
    /// Parse a configured provider-kind string.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "openai" | "" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            "claude-oauth" | "anthropic-oauth" | "claude" => Ok(Self::ClaudeOAuth),
            "codex-oauth" | "codex" => Ok(Self::CodexOAuth),
            "grok-oauth" | "grok" | "xai-oauth" | "xai" => Ok(Self::GrokOAuth),
            other => Err(format!("unknown provider kind '{other}'")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::ClaudeOAuth => "claude-oauth",
            Self::CodexOAuth => "codex-oauth",
            Self::GrokOAuth => "grok-oauth",
        }
    }
}

impl std::fmt::Display for ProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ProviderKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

/// Auth material resolved for one upstream call.
#[derive(Clone)]
pub struct UpstreamAuth {
    pub token: SecretString,
    pub extra_headers: Vec<(String, String)>,
    /// Downstream client headers (User-Agent, Stainless, Anthropic-Beta, …)
    /// for Claude OAuth device-profile / cloak parity with CLIProxyAPI.
    pub client_headers: Vec<(String, String)>,
    /// xAI OAuth: use official API instead of cli-chat-proxy (CLIProxyAPI `using_api`).
    pub using_api: bool,
}

impl Default for UpstreamAuth {
    fn default() -> Self {
        Self {
            token: SecretString::new(String::new()),
            extra_headers: vec![],
            client_headers: vec![],
            using_api: false,
        }
    }
}

pub fn resolve_kind(provider_kind: &str) -> Result<ProviderKind, ProviderError> {
    ProviderKind::parse(provider_kind).map_err(ProviderError::InvalidRequest)
}

/// Non-streaming upstream call for a resolved target.
pub async fn dispatch_non_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<(CanonicalChatResponse, LossReport), ProviderError> {
    let kind = resolve_kind(&resolved.provider_kind)?;
    debug!(
        request_id = %request.id,
        provider_id = %resolved.provider_id,
        provider_kind = %kind,
        model_id = %resolved.model_id,
        upstream_alias = %request.alias,
        stream = false,
        "dispatch non-stream"
    );
    match kind {
        ProviderKind::OpenAi => openai_non_stream(resolved, request, auth, rate_limit_sink).await,
        ProviderKind::Anthropic => {
            anthropic_non_stream(resolved, request, auth, rate_limit_sink).await
        }
        ProviderKind::ClaudeOAuth => {
            claude_oauth_non_stream(resolved, request, auth, rate_limit_sink).await
        }
        ProviderKind::CodexOAuth => {
            codex_oauth_non_stream(resolved, request, auth, rate_limit_sink).await
        }
        ProviderKind::GrokOAuth => {
            grok_oauth_non_stream(resolved, request, auth, rate_limit_sink).await
        }
    }
}

/// Streaming upstream call for a resolved target.
pub async fn dispatch_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<
    (
        BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        LossReport,
    ),
    ProviderError,
> {
    let kind = resolve_kind(&resolved.provider_kind)?;
    debug!(
        request_id = %request.id,
        provider_id = %resolved.provider_id,
        provider_kind = %kind,
        model_id = %resolved.model_id,
        upstream_alias = %request.alias,
        stream = true,
        "dispatch stream"
    );
    match kind {
        ProviderKind::OpenAi => openai_stream(resolved, request, auth, rate_limit_sink).await,
        ProviderKind::Anthropic => anthropic_stream(resolved, request, auth, rate_limit_sink).await,
        ProviderKind::ClaudeOAuth => {
            claude_oauth_stream(resolved, request, auth, rate_limit_sink).await
        }
        ProviderKind::CodexOAuth => {
            codex_oauth_stream(resolved, request, auth, rate_limit_sink).await
        }
        ProviderKind::GrokOAuth => grok_oauth_stream(resolved, request, auth, rate_limit_sink).await,
    }
}

/// Responses compact (`POST …/responses/compact`) for providers that support it.
///
/// Body is raw Responses JSON (must keep `compaction_trigger`). Only Codex OAuth
/// implements this today; other kinds return a clear InvalidRequest error.
pub async fn dispatch_responses_compact(
    resolved: &ResolvedProvider,
    body: serde_json::Value,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<serde_json::Value, ProviderError> {
    let kind = resolve_kind(&resolved.provider_kind)?;
    debug!(
        provider_id = %resolved.provider_id,
        provider_kind = %kind,
        model_id = %resolved.model_id,
        "dispatch responses compact"
    );
    match kind {
        ProviderKind::CodexOAuth => {
            codex_oauth_compact(resolved, body, auth, rate_limit_sink).await
        }
        other => Err(ProviderError::InvalidRequest(format!(
            "/responses/compact not supported for provider kind '{other}'"
        ))),
    }
}

async fn codex_oauth_compact(
    resolved: &ResolvedProvider,
    body: serde_json::Value,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<serde_json::Value, ProviderError> {
    use conduit_codec::{prepare_responses_compact_body, OpenAiResponsesCodec};
    let base_url = resolved
        .base_url
        .as_deref()
        .unwrap_or("https://chatgpt.com/backend-api/codex");
    let mut headers = auth.extra_headers.clone();
    // Merge client headers that Codex may want (session, etc.) without forcing SSE.
    for (k, v) in &auth.client_headers {
        if !headers
            .iter()
            .any(|(hk, _)| hk.eq_ignore_ascii_case(k.as_str()))
        {
            headers.push((k.clone(), v.clone()));
        }
    }
    let cfg = apply_sink(
        ProviderClientConfig::codex_oauth(&resolved.provider_id, base_url, headers)
            .with_request_overrides(resolved.request_overrides.clone()),
        rate_limit_sink,
    );
    let model = if resolved.model_id.is_empty() {
        body.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("gpt-5.4")
            .to_string()
    } else {
        resolved.model_id.clone()
    };
    let prepared = prepare_responses_compact_body(body, &model);
    // Type param is unused for compact (raw JSON path) but required by ProviderClient.
    ProviderClient::<OpenAiResponsesCodec>::new(cfg)
        .responses_compact(prepared, &auth.token)
        .await
}

fn apply_sink(cfg: ProviderClientConfig, sink: Option<RateLimitHeaderSink>) -> ProviderClientConfig {
    match sink {
        Some(s) => cfg.with_rate_limit_sink(s),
        None => cfg,
    }
}

async fn openai_non_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<(CanonicalChatResponse, LossReport), ProviderError> {
    use conduit_codec::openai::OpenAiCodec;
    let base_url = resolved
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com");
    let mut cfg = ProviderClientConfig::openai(&resolved.provider_id, base_url);
    if !auth.extra_headers.is_empty() {
        cfg = cfg.with_extra_headers(auth.extra_headers.clone());
    }
    cfg = apply_sink(cfg.with_request_overrides(resolved.request_overrides.clone()), rate_limit_sink);
    ProviderClient::<OpenAiCodec>::new(cfg)
        .chat(request, &auth.token)
        .await
}

async fn anthropic_non_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<(CanonicalChatResponse, LossReport), ProviderError> {
    use conduit_codec::anthropic::AnthropicCodec;
    let mut cfg = ProviderClientConfig::anthropic(&resolved.provider_id);
    if let Some(ref url) = resolved.base_url {
        cfg.base_url = url.clone();
    }
    if !auth.extra_headers.is_empty() {
        cfg = cfg.with_extra_headers(auth.extra_headers.clone());
    }
    cfg = apply_sink(cfg.with_request_overrides(resolved.request_overrides.clone()), rate_limit_sink);
    ProviderClient::<AnthropicCodec>::new(cfg)
        .chat(request, &auth.token)
        .await
}

fn claude_oauth_opts(
    auth: &UpstreamAuth,
) -> conduit_upstream::claude_oauth::ClaudeOAuthRelayOptions {
    let mut opts = conduit_upstream::claude_oauth::ClaudeOAuthRelayOptions::from_client_headers(
        auth.client_headers.clone(),
    );
    // Optional overrides via upstream credential extra_headers (provider attrs).
    for (k, v) in &auth.extra_headers {
        let key = k.to_ascii_lowercase();
        match key.as_str() {
            "x-conduit-cloak-mode" => opts.cloak_mode = v.clone(),
            "x-conduit-stabilize-device-profile" => {
                opts.header_defaults.stabilize_device_profile =
                    v.eq_ignore_ascii_case("true") || v == "1";
            }
            "x-conduit-user-agent" => {
                opts.header_defaults.user_agent = Some(v.clone());
            }
            _ => {}
        }
    }
    opts
}

async fn claude_oauth_non_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<(CanonicalChatResponse, LossReport), ProviderError> {
    use conduit_codec::anthropic::AnthropicCodec;
    use conduit_upstream::claude_oauth;
    let base = resolved
        .base_url
        .as_deref()
        .unwrap_or("https://api.anthropic.com");
    let opts = claude_oauth_opts(auth);
    claude_oauth::chat_oauth::<AnthropicCodec>(
        base,
        "claude-oauth",
        request,
        &resolved.model_id,
        &auth.token,
        &opts,
        &resolved.request_overrides,
        120_000,
        rate_limit_sink,
        &resolved.provider_id,
    )
    .await
}

/// ChatGPT-account Codex rejects non-stream Responses and sampling fields.
///
/// Aligns with CLIProxyAPI `ConvertOpenAIRequestToCodex` / `Execute`:
/// upstream always streams; non-stream clients get a buffered completion.
fn sanitize_codex_request(request: &CanonicalChatRequest) -> CanonicalChatRequest {
    let mut req = request.clone();
    req.sampling.max_tokens = None;
    req.sampling.temperature = None;
    req.sampling.top_p = None;
    req.stream = true;
    req
}

async fn codex_oauth_non_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<(CanonicalChatResponse, LossReport), ProviderError> {
    // CLIProxyAPI non-stream path: upstream SSE → aggregate text + usage → one response.
    let req = sanitize_codex_request(request);
    let (mut stream, loss) = codex_oauth_stream(resolved, &req, auth, rate_limit_sink).await?;
    let mut text = String::new();
    let mut finish = FinishReason::Stop;
    let mut usage = Usage::default();
    let mut resp_id = request.id.clone();
    let mut saw_terminal = false;
    while let Some(item) = stream.next().await {
        let chunk = item?;
        if !chunk.request_id.is_empty() {
            resp_id = chunk.request_id.clone();
        }
        if let Some(BlockDelta::TextDelta { text: t }) = &chunk.delta {
            text.push_str(t);
        }
        if let Some(fr) = chunk.finish_reason {
            finish = fr;
            saw_terminal = true;
        }
        if let Some(u) = chunk.usage {
            usage = u;
        }
    }
    if !saw_terminal && text.is_empty() {
        return Err(ProviderError::Network(
            "codex stream closed before response.completed".into(),
        ));
    }
    let response = CanonicalChatResponse {
        id: resp_id,
        request_id: request.id.clone(),
        model: resolved.model_id.clone(),
        choices: vec![CanonicalMessage {
            role: Role::Assistant,
            content: vec![CanonicalContent::Text { text }],
            name: None,
        }],
        finish_reason: finish,
        usage,
        created_at: chrono::Utc::now(),
    };
    Ok((response, loss))
}

fn grok_using_api(resolved: &ResolvedProvider, auth: &UpstreamAuth) -> bool {
    if auth.using_api {
        return true;
    }
    // Optional route-level override (request_overrides.using_api).
    match resolved.request_overrides.get("using_api") {
        Some(v) if v.as_bool() == Some(true) => true,
        Some(v) if v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("true") || s == "1") => true,
        _ => false,
    }
}

async fn grok_oauth_non_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<(CanonicalChatResponse, LossReport), ProviderError> {
    // CLIProxyAPI parity: OAuth chat → cli-chat-proxy (unless using_api) + Responses API.
    use conduit_codec::OpenAiResponsesCodec;
    use conduit_oauth::{cli_proxy_headers, is_cli_chat_proxy_base, resolve_oauth_chat_base};
    let using_api = grok_using_api(resolved, auth);
    let base = resolve_oauth_chat_base(resolved.base_url.as_deref(), using_api);
    let mut headers = auth.extra_headers.clone();
    // CLI identity headers only on chat-proxy path (CLIProxyAPI applyXAIChatHeaders).
    if !using_api && is_cli_chat_proxy_base(&base) {
        for (k, v) in cli_proxy_headers() {
            if !headers.iter().any(|(hk, _)| hk.eq_ignore_ascii_case(&k)) {
                headers.push((k, v));
            }
        }
    }
    let cfg = apply_sink(
        ProviderClientConfig::grok_oauth(&resolved.provider_id, base, headers)
            .with_request_overrides(resolved.request_overrides.clone()),
        rate_limit_sink,
    );
    ProviderClient::<OpenAiResponsesCodec>::new(cfg)
        .chat(request, &auth.token)
        .await
}

async fn openai_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<
    (
        BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        LossReport,
    ),
    ProviderError,
> {
    use conduit_codec::openai::OpenAiCodec;
    let base_url = resolved
        .base_url
        .as_deref()
        .unwrap_or("https://api.openai.com");
    let mut cfg = ProviderClientConfig::openai(&resolved.provider_id, base_url);
    if !auth.extra_headers.is_empty() {
        cfg = cfg.with_extra_headers(auth.extra_headers.clone());
    }
    cfg = apply_sink(cfg.with_request_overrides(resolved.request_overrides.clone()), rate_limit_sink);
    ProviderClient::<OpenAiCodec>::new(cfg)
        .chat_stream(request, &auth.token)
        .await
}

async fn anthropic_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<
    (
        BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        LossReport,
    ),
    ProviderError,
> {
    use conduit_codec::anthropic::AnthropicCodec;
    let mut cfg = ProviderClientConfig::anthropic(&resolved.provider_id);
    if let Some(ref url) = resolved.base_url {
        cfg.base_url = url.clone();
    }
    if !auth.extra_headers.is_empty() {
        cfg = cfg.with_extra_headers(auth.extra_headers.clone());
    }
    cfg = apply_sink(cfg.with_request_overrides(resolved.request_overrides.clone()), rate_limit_sink);
    ProviderClient::<AnthropicCodec>::new(cfg)
        .chat_stream(request, &auth.token)
        .await
}

async fn claude_oauth_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<
    (
        BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        LossReport,
    ),
    ProviderError,
> {
    use conduit_codec::anthropic::AnthropicCodec;
    use conduit_upstream::claude_oauth;
    let base = resolved
        .base_url
        .as_deref()
        .unwrap_or("https://api.anthropic.com");
    let opts = claude_oauth_opts(auth);
    claude_oauth::chat_oauth_stream::<AnthropicCodec>(
        base,
        "claude-oauth",
        request,
        &resolved.model_id,
        &auth.token,
        &opts,
        &resolved.request_overrides,
        rate_limit_sink,
        &resolved.provider_id,
    )
    .await
}

async fn codex_oauth_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<
    (
        BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        LossReport,
    ),
    ProviderError,
> {
    use conduit_codec::OpenAiResponsesCodec;
    let req = sanitize_codex_request(request);
    let base = resolved
        .base_url
        .as_deref()
        .unwrap_or("https://chatgpt.com/backend-api/codex");
    let cfg = apply_sink(
        ProviderClientConfig::codex_oauth(&resolved.provider_id, base, auth.extra_headers.clone())
            .with_request_overrides(resolved.request_overrides.clone()),
        rate_limit_sink,
    );
    ProviderClient::<OpenAiResponsesCodec>::new(cfg)
        .chat_stream(&req, &auth.token)
        .await
}

async fn grok_oauth_stream(
    resolved: &ResolvedProvider,
    request: &CanonicalChatRequest,
    auth: &UpstreamAuth,
    rate_limit_sink: Option<RateLimitHeaderSink>,
) -> Result<
    (
        BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
        LossReport,
    ),
    ProviderError,
> {
    use conduit_codec::OpenAiResponsesCodec;
    use conduit_oauth::{cli_proxy_headers, is_cli_chat_proxy_base, resolve_oauth_chat_base};
    let using_api = grok_using_api(resolved, auth);
    let base = resolve_oauth_chat_base(resolved.base_url.as_deref(), using_api);
    let mut headers = auth.extra_headers.clone();
    if !using_api && is_cli_chat_proxy_base(&base) {
        for (k, v) in cli_proxy_headers() {
            if !headers.iter().any(|(hk, _)| hk.eq_ignore_ascii_case(&k)) {
                headers.push((k, v));
            }
        }
    }
    let cfg = apply_sink(
        ProviderClientConfig::grok_oauth(&resolved.provider_id, base, headers)
            .with_request_overrides(resolved.request_overrides.clone()),
        rate_limit_sink,
    );
    ProviderClient::<OpenAiResponsesCodec>::new(cfg)
        .chat_stream(request, &auth.token)
        .await
}

#[cfg(test)]
mod tests {
    use conduit_ir::canonical::CanonicalMessage;

    use super::*;

    fn sample_resolved(kind: &str) -> ResolvedProvider {
        ResolvedProvider {
            provider_id: "p1".into(),
            model_id: "m1".into(),
            provider_kind: kind.into(),
            base_url: Some("http://127.0.0.1:9".into()),
            request_overrides: Default::default(),
            attempt_no: 0,
        }
    }

    fn sample_auth() -> UpstreamAuth {
        UpstreamAuth {
            token: SecretString::new("sk-test".into()),
            extra_headers: vec![],
            client_headers: vec![],
            using_api: false,
        }
    }

    #[test]
    fn parse_known_kinds() {
        assert_eq!(ProviderKind::parse("openai").unwrap(), ProviderKind::OpenAi);
        assert_eq!(
            ProviderKind::parse("anthropic").unwrap(),
            ProviderKind::Anthropic
        );
        assert_eq!(
            ProviderKind::parse("claude-oauth").unwrap(),
            ProviderKind::ClaudeOAuth
        );
        assert_eq!(
            ProviderKind::parse("codex").unwrap(),
            ProviderKind::CodexOAuth
        );
        assert_eq!(
            ProviderKind::parse("grok-oauth").unwrap(),
            ProviderKind::GrokOAuth
        );
    }

    #[test]
    fn parse_unknown_kind_errors() {
        let err = ProviderKind::parse("google").unwrap_err();
        assert!(err.contains("unknown provider kind"));
    }

    #[tokio::test]
    async fn dispatch_rejects_unknown_kind_without_network() {
        let resolved = sample_resolved("not-a-provider");
        let req = CanonicalChatRequest::new("alias", vec![CanonicalMessage::user("hi")]);
        let auth = sample_auth();

        let err = dispatch_non_stream(&resolved, &req, &auth, None)
            .await
            .unwrap_err();
        match err {
            ProviderError::InvalidRequest(msg) => {
                assert!(msg.contains("unknown provider kind"));
            }
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_oauth_kinds_reach_upstream() {
        let req = CanonicalChatRequest::new("m", vec![CanonicalMessage::user("hi")]);
        let auth = sample_auth();
        for kind in [
            "claude-oauth",
            "codex-oauth",
            "grok-oauth",
            "openai",
            "anthropic",
        ] {
            let resolved = sample_resolved(kind);
            let err = match dispatch_non_stream(&resolved, &req, &auth, None).await {
                Ok(_) => panic!("closed port must fail for {kind}"),
                Err(e) => e,
            };
            assert!(
                matches!(err, ProviderError::Network(_) | ProviderError::Timeout),
                "kind={kind}: expected network/timeout, got {err:?}"
            );
        }
    }
}
