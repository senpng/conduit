//! ProviderClient: a single upstream provider connection.
//! Each provider has a base URL, auth strategy, and shared HTTP client.

use std::{sync::Arc, time::Duration};

use conduit_codec::{apply_codex_chatgpt_account_body, WireCodec};
use conduit_ir::{
    canonical::{
        CanonicalChatRequest, CanonicalChatResponse, CanonicalChunk, Capabilities, HealthStatus,
        ModelInfo,
    },
    error::ProviderError,
    loss::LossReport,
};
use futures::stream::{BoxStream, StreamExt};
use reqwest::StatusCode;
use secrecy::SecretString;
use serde_json::{json, Map, Value};
use tracing::{debug, instrument, warn};

use crate::{
    auth::{AuthStrategy, BearerAuth, CompositeAuth, HeaderAuth},
    claude_oauth,
    client::HttpClientFactory,
    rate_limit::{self, RateLimitHeaderSink},
    sse::{map_reqwest_error, response_to_sse},
};

pub type ChatResult = (CanonicalChatResponse, LossReport);
pub type StreamResult = (
    BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
    LossReport,
);

/// Timeouts applied at each layer of the HTTP call.
#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    /// Max time to establish TCP + TLS connection.
    pub connect_ms: u64,
    /// Max time from sending request to receiving first byte of response body.
    pub first_byte_ms: u64,
    /// Max quiet period with no upstream body bytes during a streaming response.
    /// Idle is measured on the raw byte stream so SSE comment keepalives count.
    pub stream_idle_ms: u64,
    /// Hard cap for a streaming response after headers are received.
    /// Aligns with gateway `TimeoutLayer` (300s) by default.
    pub stream_overall_ms: u64,
    /// Overall hard cap for the entire request (non-streaming).
    pub overall_ms: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_ms: 5_000,
            first_byte_ms: 30_000,
            // 3 minutes: reasoning models often pause between tokens / rounds.
            // Still well under stream_overall_ms so truly stuck streams die.
            stream_idle_ms: 180_000,
            stream_overall_ms: 300_000,
            overall_ms: 120_000,
        }
    }
}

impl TimeoutConfig {
    /// Idle + overall options for an open SSE body.
    pub fn stream_timeout_opts(&self) -> crate::sse::StreamTimeoutOpts {
        crate::sse::StreamTimeoutOpts {
            idle_ms: self.stream_idle_ms,
            overall_ms: self.stream_overall_ms,
        }
    }
}

/// Which chat path the provider uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamPath {
    /// OpenAI-compatible `/v1/chat/completions`
    ChatCompletions,
    /// Anthropic `/v1/messages`
    Messages,
    /// OpenAI Responses / Codex `/responses`
    Responses,
}

#[derive(Clone)]
pub struct ProviderClientConfig {
    pub id: String,
    pub kind: String,
    pub base_url: String,
    pub auth: Arc<dyn AuthStrategy>,
    pub timeouts: TimeoutConfig,
    pub path: UpstreamPath,
    /// Static target-specific fields merged after protocol encoding.
    pub request_overrides: Map<String, Value>,
    /// Optional sink for `anthropic-ratelimit-*` / `x-ratelimit-*` / `retry-after` headers.
    pub rate_limit_sink: Option<RateLimitHeaderSink>,
}

impl ProviderClientConfig {
    pub fn openai(id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: "openai".to_string(),
            base_url: base_url.into(),
            auth: Arc::new(BearerAuth),
            timeouts: TimeoutConfig::default(),
            path: UpstreamPath::ChatCompletions,
            request_overrides: Map::new(),
            rate_limit_sink: None,
        }
    }

    pub fn anthropic(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: "anthropic".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            auth: Arc::new(HeaderAuth::anthropic()),
            timeouts: TimeoutConfig::default(),
            path: UpstreamPath::Messages,
            request_overrides: Map::new(),
            rate_limit_sink: None,
        }
    }

    pub fn claude_oauth(
        id: impl Into<String>,
        base_url: impl Into<String>,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: "claude-oauth".to_string(),
            base_url: base_url.into(),
            auth: Arc::new(CompositeAuth::bearer_with_headers(extra_headers)),
            timeouts: TimeoutConfig::default(),
            path: UpstreamPath::Messages,
            request_overrides: Map::new(),
            rate_limit_sink: None,
        }
    }

    pub fn codex_oauth(
        id: impl Into<String>,
        base_url: impl Into<String>,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: "codex-oauth".to_string(),
            base_url: base_url.into(),
            auth: Arc::new(CompositeAuth::bearer_with_headers(extra_headers)),
            timeouts: TimeoutConfig::default(),
            path: UpstreamPath::Responses,
            request_overrides: Map::new(),
            rate_limit_sink: None,
        }
    }

    /// Grok OAuth / CLI subscription path: Responses API on cli-chat-proxy.
    pub fn grok_oauth(
        id: impl Into<String>,
        base_url: impl Into<String>,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            id: id.into(),
            kind: "grok-oauth".to_string(),
            base_url: base_url.into(),
            auth: Arc::new(CompositeAuth::bearer_with_headers(extra_headers)),
            timeouts: TimeoutConfig::default(),
            path: UpstreamPath::Responses,
            request_overrides: Map::new(),
            rate_limit_sink: None,
        }
    }

    pub fn with_rate_limit_sink(mut self, sink: RateLimitHeaderSink) -> Self {
        self.rate_limit_sink = Some(sink);
        self
    }

    pub fn with_extra_headers(mut self, headers: Vec<(String, String)>) -> Self {
        if headers.is_empty() {
            return self;
        }
        // Wrap existing auth
        let primary_name = self.auth.name();
        let auth: Arc<dyn AuthStrategy> = if primary_name == "header" {
            Arc::new(CompositeAuth::header_with_headers("x-api-key", headers))
        } else {
            Arc::new(CompositeAuth::bearer_with_headers(headers))
        };
        self.auth = auth;
        self
    }

    pub fn with_request_overrides(mut self, overrides: Map<String, Value>) -> Self {
        self.request_overrides = overrides;
        self
    }
}

/// A typed upstream client for a single provider.
pub struct ProviderClient<C: WireCodec> {
    pub config: ProviderClientConfig,
    _codec: std::marker::PhantomData<C>,
}

impl<C: WireCodec + 'static> ProviderClient<C> {
    pub fn new(config: ProviderClientConfig) -> Self {
        Self {
            config,
            _codec: std::marker::PhantomData,
        }
    }

    fn client(&self) -> &'static reqwest::Client {
        HttpClientFactory::get()
    }

    fn chat_url(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        match self.config.path {
            UpstreamPath::Messages => {
                let url = format!("{base}/v1/messages");
                if claude_oauth::is_claude_oauth_kind(&self.config.kind) {
                    claude_oauth::messages_url_with_beta(&url)
                } else {
                    url
                }
            }
            UpstreamPath::ChatCompletions => {
                // base may already include /v1
                if base.ends_with("/v1") {
                    format!("{base}/chat/completions")
                } else {
                    format!("{base}/v1/chat/completions")
                }
            }
            UpstreamPath::Responses => {
                if base.ends_with("/responses") {
                    base.to_string()
                } else {
                    format!("{base}/responses")
                }
            }
        }
    }

    /// Compact endpoint: `{base}/responses/compact` (Codex backend parity).
    fn compact_url(&self) -> String {
        let responses = self.chat_url();
        if responses.ends_with("/responses") {
            format!("{responses}/compact")
        } else {
            format!("{responses}/responses/compact")
        }
    }

    /// POST raw Responses body to `/responses/compact` (non-stream JSON).
    ///
    /// Does **not** IR-encode: compact must preserve items like
    /// `compaction_trigger` that the chat codec would drop.
    #[instrument(skip(self, body, secret), fields(provider = %self.config.id))]
    pub async fn responses_compact(
        &self,
        body: Value,
        secret: &SecretString,
    ) -> Result<Value, ProviderError> {
        if self.config.path != UpstreamPath::Responses {
            return Err(ProviderError::InvalidRequest(
                "/responses/compact requires a Responses upstream path".into(),
            ));
        }
        let url = self.compact_url();
        let body_bytes = body.to_string().len();
        debug!(
            provider = %self.config.id,
            kind = %self.config.kind,
            url = %url,
            body_bytes,
            "upstream responses compact request"
        );

        let mut builder = self
            .client()
            .post(&url)
            .timeout(Duration::from_millis(self.config.timeouts.overall_ms))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body);
        builder = self.apply_auth(builder, secret);
        // Compact is non-stream JSON; do not force SSE Accept from Responses headers.
        if self.config.kind != "codex-oauth" {
            builder = self.apply_provider_headers(builder);
        } else {
            builder = builder.header("Connection", "Keep-Alive");
        }

        let started = std::time::Instant::now();
        let resp = builder.send().await.map_err(|e| {
            let err = map_reqwest_error(e);
            debug!(
                provider = %self.config.id,
                url = %url,
                elapsed_ms = started.elapsed().as_millis() as u64,
                error = %err,
                "upstream compact transport error"
            );
            err
        })?;

        rate_limit::emit(
            &self.config.rate_limit_sink,
            &self.config.id,
            rate_limit::collect_from_reqwest(resp.headers()),
        );

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(self.map_status_error(status, &text));
        }

        let val: Value = resp.json().await.map_err(|e| {
            debug!(
                provider = %self.config.id,
                error = %e,
                "upstream compact response json parse failed"
            );
            ProviderError::Serialization(e.to_string())
        })?;

        debug!(
            provider = %self.config.id,
            kind = %self.config.kind,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "upstream compact response ok"
        );
        Ok(val)
    }

    fn models_url(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/models")
        } else {
            format!("{base}/v1/models")
        }
    }

    fn apply_auth(
        &self,
        builder: reqwest::RequestBuilder,
        key: &SecretString,
    ) -> reqwest::RequestBuilder {
        self.config.auth.apply(builder, key)
    }

    fn apply_provider_headers(
        &self,
        mut builder: reqwest::RequestBuilder,
    ) -> reqwest::RequestBuilder {
        match self.config.path {
            UpstreamPath::Messages => {
                // anthropic-version is often also in OAuth extra_headers; set default if missing
                builder = builder.header("anthropic-version", "2023-06-01");
            }
            UpstreamPath::Responses => {
                // CLIProxyAPI Codex path does not set OpenAI-Beta for ChatGPT-account
                // `/responses`. Grok CLI chat-proxy also omits it.
                if self.config.kind == "codex-oauth" {
                    builder = builder
                        .header("Accept", "text/event-stream")
                        .header("Connection", "Keep-Alive");
                }
            }
            UpstreamPath::ChatCompletions => {}
        }
        builder
    }

    fn map_status_error(&self, status: StatusCode, body: &str) -> ProviderError {
        let err = match status.as_u16() {
            401 | 403 => ProviderError::Unauthorized(body.to_string()),
            429 => ProviderError::RateLimited(body.to_string()),
            400 | 422 => ProviderError::InvalidRequest(body.to_string()),
            413 => ProviderError::ContextLengthExceeded,
            s if s >= 500 => ProviderError::Upstream5xx(format!("{}: {}", s, body)),
            s => ProviderError::InvalidRequest(format!("HTTP {}: {}", s, body)),
        };
        debug!(
            provider = %self.config.id,
            kind = %self.config.kind,
            status = status.as_u16(),
            body_preview = %truncate_for_log(body, 400),
            error = %err,
            "upstream non-success status"
        );
        err
    }

    #[instrument(
        skip(self, req, secret),
        fields(provider = %self.config.id, alias = %req.alias, request_id = %req.id)
    )]
    pub async fn chat(
        &self,
        req: &CanonicalChatRequest,
        secret: &SecretString,
    ) -> Result<ChatResult, ProviderError> {
        // Claude OAuth: full CLIProxyAPI relay (Chrome TLS + cloak + cch + tools).
        if claude_oauth::is_claude_oauth_kind(&self.config.kind) {
            // Prefer model_id-shaped alias when callers already rewrote it;
            // otherwise use request alias (pipeline path passes model_id explicitly).
            return claude_oauth::chat_oauth::<C>(
                &self.config.base_url,
                &self.config.kind,
                req,
                &req.alias,
                secret,
                &claude_oauth::ClaudeOAuthRelayOptions::default(),
                &self.config.request_overrides,
                self.config.timeouts.overall_ms,
                self.config.rate_limit_sink.clone(),
                &self.config.id,
            )
            .await;
        }

        let url = self.chat_url();
        let (mut body, encode_loss) = C::encode_request(req, false);
        // Codex ChatGPT accounts cannot use non-stream Responses; callers should
        // use chat_stream + aggregate. If chat() is still invoked, apply constraints
        // so the error surface is clearer after rewrite.
        if self.config.kind == "codex-oauth" {
            body = apply_codex_chatgpt_account_body(body);
            apply_codex_service_tier(&mut body, req);
        }
        apply_request_overrides(&mut body, &self.config.request_overrides);

        let body_bytes = body.to_string().len();
        debug!(
            request_id = %req.id,
            provider = %self.config.id,
            kind = %self.config.kind,
            url = %url,
            alias = %req.alias,
            stream = false,
            body_bytes,
            message_count = req.messages.len(),
            overall_ms = self.config.timeouts.overall_ms,
            "upstream chat request"
        );

        let mut builder = self
            .client()
            .post(&url)
            .timeout(Duration::from_millis(self.config.timeouts.overall_ms))
            .json(&body);
        builder = self.apply_auth(builder, secret);
        builder = self.apply_provider_headers(builder);

        let started = std::time::Instant::now();
        let resp = builder.send().await.map_err(|e| {
            let err = map_reqwest_error(e);
            debug!(
                request_id = %req.id,
                provider = %self.config.id,
                url = %url,
                elapsed_ms = started.elapsed().as_millis() as u64,
                error = %err,
                "upstream chat transport error"
            );
            err
        })?;

        // Capture rate-limit headers before consuming the body.
        rate_limit::emit(
            &self.config.rate_limit_sink,
            &self.config.id,
            rate_limit::collect_from_reqwest(resp.headers()),
        );

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(self.map_status_error(status, &text));
        }

        let val: Value = resp
            .json()
            .await
            .map_err(|e| {
                debug!(
                    request_id = %req.id,
                    provider = %self.config.id,
                    error = %e,
                    "upstream chat response json parse failed"
                );
                ProviderError::Serialization(e.to_string())
            })?;

        let (response, decode_loss) = C::decode_response(val, &req.alias)
            .map_err(|e| {
                debug!(
                    request_id = %req.id,
                    provider = %self.config.id,
                    error = %e,
                    "upstream chat response decode failed"
                );
                ProviderError::Serialization(e.to_string())
            })?;

        debug!(
            request_id = %req.id,
            provider = %self.config.id,
            kind = %self.config.kind,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            prompt_tokens = response.usage.prompt_tokens,
            completion_tokens = response.usage.completion_tokens,
            finish_reason = ?response.finish_reason,
            "upstream chat response ok"
        );

        let mut combined = encode_loss;
        combined.merge(decode_loss);
        Ok((response, combined))
    }

    #[instrument(
        skip(self, req, secret),
        fields(provider = %self.config.id, alias = %req.alias, request_id = %req.id)
    )]
    pub async fn chat_stream(
        &self,
        req: &CanonicalChatRequest,
        secret: &SecretString,
    ) -> Result<StreamResult, ProviderError> {
        if claude_oauth::is_claude_oauth_kind(&self.config.kind) {
            return claude_oauth::chat_oauth_stream::<C>(
                &self.config.base_url,
                &self.config.kind,
                req,
                &req.alias,
                secret,
                &claude_oauth::ClaudeOAuthRelayOptions::default(),
                &self.config.request_overrides,
                self.config.rate_limit_sink.clone(),
                &self.config.id,
                self.config.timeouts.stream_timeout_opts(),
            )
            .await;
        }

        let url = self.chat_url();
        let (mut body, encode_loss) = C::encode_request(req, true);
        if self.config.kind == "codex-oauth" {
            // CLIProxyAPI: force stream/store and strip unsupported sampling fields.
            body = apply_codex_chatgpt_account_body(body);
            apply_codex_service_tier(&mut body, req);
        }
        apply_request_overrides(&mut body, &self.config.request_overrides);

        let body_bytes = body.to_string().len();
        let stream_opts = self.config.timeouts.stream_timeout_opts();
        debug!(
            request_id = %req.id,
            provider = %self.config.id,
            kind = %self.config.kind,
            url = %url,
            alias = %req.alias,
            stream = true,
            body_bytes,
            message_count = req.messages.len(),
            first_byte_ms = self.config.timeouts.first_byte_ms,
            stream_idle_ms = stream_opts.idle_ms,
            stream_overall_ms = stream_opts.overall_ms,
            "upstream chat_stream request"
        );

        let mut builder = self.client().post(&url).json(&body);
        builder = self.apply_auth(builder, secret);
        builder = self.apply_provider_headers(builder);

        let started = std::time::Instant::now();
        let resp = tokio::time::timeout(
            Duration::from_millis(self.config.timeouts.first_byte_ms),
            builder.send(),
        )
        .await
        .map_err(|_| {
            warn!(
                request_id = %req.id,
                provider = %self.config.id,
                url = %url,
                elapsed_ms = started.elapsed().as_millis() as u64,
                first_byte_ms = self.config.timeouts.first_byte_ms,
                "upstream chat_stream first-byte timeout"
            );
            ProviderError::Timeout
        })?
        .map_err(|e| {
            let err = map_reqwest_error(e);
            warn!(
                request_id = %req.id,
                provider = %self.config.id,
                url = %url,
                elapsed_ms = started.elapsed().as_millis() as u64,
                error = %err,
                "upstream chat_stream transport error"
            );
            err
        })?;

        rate_limit::emit(
            &self.config.rate_limit_sink,
            &self.config.id,
            rate_limit::collect_from_reqwest(resp.headers()),
        );

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(self.map_status_error(status, &text));
        }

        debug!(
            request_id = %req.id,
            provider = %self.config.id,
            kind = %self.config.kind,
            status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "upstream chat_stream headers ok; beginning SSE"
        );

        let sse = response_to_sse(resp, stream_opts);
        let mut decode_state = C::StreamState::default();
        let provider_id = self.config.id.clone();
        let stream = sse
            .map(move |result| match result {
                Ok(data) => {
                    if data == "[DONE]" {
                        return Ok(Vec::new());
                    }
                    match C::decode_chunk_stateful(&mut decode_state, &data) {
                        Ok((chunks, _loss)) => Ok(chunks),
                        Err(e) => {
                            warn!(
                                provider = %provider_id,
                                error = %e,
                                data_preview = %truncate_for_log(&data, 200),
                                "codec decode error"
                            );
                            Err(ProviderError::Serialization(e.to_string()))
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        provider = %provider_id,
                        error = %e,
                        "upstream SSE event error"
                    );
                    Err(e)
                }
            })
            .map(|result| match result {
                Ok(chunks) => chunks.into_iter().map(Ok).collect::<Vec<_>>(),
                Err(e) => vec![Err(e)],
            })
            .flat_map(futures::stream::iter);

        Ok((Box::pin(stream), encode_loss))
    }

    pub async fn list_models(
        &self,
        secret: &SecretString,
    ) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = self.models_url();
        let mut builder = self.client().get(&url).timeout(Duration::from_secs(10));
        builder = self.apply_auth(builder, secret);
        builder = self.apply_provider_headers(builder);

        let resp = builder
            .send()
            .await
            .map_err(map_reqwest_error)?;
        if !resp.status().is_success() {
            return Ok(vec![]);
        }

        let val: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Serialization(e.to_string()))?;
        let data = val
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(data
            .iter()
            .filter_map(|m| {
                let model_id = m.get("id")?.as_str()?.to_string();
                Some(ModelInfo {
                    provider_id: self.config.id.clone(),
                    model_id: model_id.clone(),
                    display_name: model_id,
                    capabilities: Capabilities {
                        streaming: true,
                        ..Default::default()
                    },
                    input_price_per_mtok: 0.0,
                    output_price_per_mtok: 0.0,
                })
            })
            .collect())
    }

    pub async fn health_check(&self, secret: &SecretString) -> HealthStatus {
        let url = self.models_url();
        let mut builder = self.client().get(&url).timeout(Duration::from_secs(5));
        builder = self.apply_auth(builder, secret);
        match builder.send().await {
            Ok(r) if r.status().is_success() => HealthStatus::Healthy,
            Ok(r) if r.status().as_u16() == 401 => HealthStatus::Degraded,
            Ok(_) => HealthStatus::Degraded,
            Err(_) => HealthStatus::Unhealthy,
        }
    }
}

/// Truncate a string for log fields without panicking on multi-byte UTF-8.
pub(crate) fn truncate_for_log(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn apply_codex_service_tier(body: &mut Value, request: &CanonicalChatRequest) {
    if request.sampling.service_tier.as_deref() == Some("priority") {
        body["service_tier"] = json!("priority");
    }
}

pub(crate) fn apply_request_overrides(body: &mut Value, overrides: &Map<String, Value>) {
    let Some(target) = body.as_object_mut() else {
        return;
    };
    for (key, value) in overrides {
        if matches!(
            key.as_str(),
            "model" | "stream" | "store" | "input" | "messages"
        ) {
            continue;
        }
        target.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn codex_priority_service_tier_is_added_to_the_request_body() {
        let mut request = CanonicalChatRequest::new("gpt-5.6-terra", vec![]);
        request.sampling.service_tier = Some("priority".into());
        let mut body = serde_json::json!({"model": "gpt-5.6-terra"});

        apply_codex_service_tier(&mut body, &request);

        assert_eq!(body["service_tier"], "priority");
    }

    #[test]
    fn request_overrides_merge_extra_fields_without_replacing_gateway_fields() {
        let mut body = serde_json::json!({
            "model": "gpt-5.6-terra",
            "stream": true,
            "store": false,
            "input": "hello"
        });
        let overrides = serde_json::json!({
            "service_tier": "priority",
            "model": "other-model",
            "stream": false,
            "store": true,
            "input": "replacement"
        })
        .as_object()
        .unwrap()
        .clone();

        apply_request_overrides(&mut body, &overrides);

        assert_eq!(body["service_tier"], "priority");
        assert_eq!(body["model"], "gpt-5.6-terra");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["input"], "hello");
    }

    #[test]
    fn compact_url_appends_compact_to_responses_base() {
        use conduit_codec::OpenAIResponsesCodec;
        let cfg = ProviderClientConfig::codex_oauth(
            "codex-1",
            "https://chatgpt.com/backend-api/codex",
            vec![],
        );
        let client = ProviderClient::<OpenAIResponsesCodec>::new(cfg);
        assert_eq!(
            client.compact_url(),
            "https://chatgpt.com/backend-api/codex/responses/compact"
        );
        let cfg2 = ProviderClientConfig::codex_oauth(
            "codex-2",
            "https://example.com/backend-api/codex/responses",
            vec![],
        );
        let client2 = ProviderClient::<OpenAIResponsesCodec>::new(cfg2);
        assert_eq!(
            client2.compact_url(),
            "https://example.com/backend-api/codex/responses/compact"
        );
    }

    #[tokio::test]
    async fn responses_compact_posts_to_compact_path_and_returns_json() {
        use conduit_codec::OpenAIResponsesCodec;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses/compact"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_1",
                "object": "response.compaction",
                "usage": {"input_tokens": 1, "output_tokens": 2, "total_tokens": 3}
            })))
            .mount(&server)
            .await;

        let cfg = ProviderClientConfig::codex_oauth("codex-test", server.uri(), vec![]);
        let client = ProviderClient::<OpenAIResponsesCodec>::new(cfg);
        let body = json!({
            "model": "gpt-5.4",
            "instructions": "",
            "input": [
                {"type": "message", "role": "user", "content": "history"},
                {"type": "compaction_trigger"}
            ]
        });
        let out = client
            .responses_compact(body, &SecretString::new("test-token".into()))
            .await
            .expect("compact ok");
        assert_eq!(out["object"], "response.compaction");
        assert_eq!(out["usage"]["total_tokens"], 3);
    }
}
