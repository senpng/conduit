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
use serde_json::Value;
use tracing::{instrument, warn};

use crate::{
    auth::{AuthStrategy, BearerAuth, CompositeAuth, HeaderAuth},
    claude_oauth,
    client::HttpClientFactory,
    sse::response_to_sse,
};

/// Timeouts applied at each layer of the HTTP call.
#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    /// Max time to establish TCP + TLS connection.
    pub connect_ms: u64,
    /// Max time from sending request to receiving first byte of response body.
    pub first_byte_ms: u64,
    /// Max time with no data received during a streaming response.
    pub stream_idle_ms: u64,
    /// Overall hard cap for the entire request (non-streaming).
    pub overall_ms: u64,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            connect_ms: 5_000,
            first_byte_ms: 30_000,
            stream_idle_ms: 60_000,
            overall_ms: 120_000,
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
        }
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
        match status.as_u16() {
            401 | 403 => ProviderError::Unauthorized(body.to_string()),
            429 => ProviderError::RateLimited(body.to_string()),
            400 | 422 => ProviderError::InvalidRequest(body.to_string()),
            413 => ProviderError::ContextLengthExceeded,
            s if s >= 500 => ProviderError::Upstream5xx(format!("{}: {}", s, body)),
            s => ProviderError::InvalidRequest(format!("HTTP {}: {}", s, body)),
        }
    }

    #[instrument(skip(self, req, secret), fields(provider = %self.config.id, alias = %req.alias))]
    pub async fn chat(
        &self,
        req: &CanonicalChatRequest,
        secret: &SecretString,
    ) -> Result<(CanonicalChatResponse, LossReport), ProviderError> {
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
                self.config.timeouts.overall_ms,
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
        }

        let mut builder = self
            .client()
            .post(&url)
            .timeout(Duration::from_millis(self.config.timeouts.overall_ms))
            .json(&body);
        builder = self.apply_auth(builder, secret);
        builder = self.apply_provider_headers(builder);

        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Network(e.to_string())
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(self.map_status_error(status, &text));
        }

        let val: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Serialization(e.to_string()))?;

        let (response, decode_loss) = C::decode_response(val, &req.alias)
            .map_err(|e| ProviderError::Serialization(e.to_string()))?;

        let mut combined = encode_loss;
        combined.merge(decode_loss);
        Ok((response, combined))
    }

    #[instrument(skip(self, req, secret), fields(provider = %self.config.id, alias = %req.alias))]
    pub async fn chat_stream(
        &self,
        req: &CanonicalChatRequest,
        secret: &SecretString,
    ) -> Result<
        (
            BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
            LossReport,
        ),
        ProviderError,
    > {
        if claude_oauth::is_claude_oauth_kind(&self.config.kind) {
            return claude_oauth::chat_oauth_stream::<C>(
                &self.config.base_url,
                &self.config.kind,
                req,
                &req.alias,
                secret,
                &claude_oauth::ClaudeOAuthRelayOptions::default(),
                self.config.timeouts.overall_ms,
            )
            .await;
        }

        let url = self.chat_url();
        let (mut body, encode_loss) = C::encode_request(req, true);
        if self.config.kind == "codex-oauth" {
            // CLIProxyAPI: force stream/store and strip unsupported sampling fields.
            body = apply_codex_chatgpt_account_body(body);
        }

        let mut builder = self
            .client()
            .post(&url)
            .timeout(Duration::from_millis(self.config.timeouts.overall_ms))
            .json(&body);
        builder = self.apply_auth(builder, secret);
        builder = self.apply_provider_headers(builder);

        let resp = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Network(e.to_string())
            }
        })?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(self.map_status_error(status, &text));
        }

        let sse = response_to_sse(resp);
        let mut decode_state = C::StreamState::default();
        let stream = sse
            .map(move |result| match result {
                Ok(data) => {
                    if data == "[DONE]" {
                        return Ok(Vec::new());
                    }
                    match C::decode_chunk_stateful(&mut decode_state, &data) {
                        Ok((chunks, _loss)) => Ok(chunks),
                        Err(e) => {
                            warn!("codec decode error: {}", e);
                            Err(ProviderError::Serialization(e.to_string()))
                        }
                    }
                }
                Err(e) => Err(e),
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
            .map_err(|e| ProviderError::Network(e.to_string()))?;
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
