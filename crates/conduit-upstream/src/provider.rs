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
use tracing::{instrument, warn};

use crate::{
    auth::{AuthStrategy, BearerAuth, CompositeAuth, HeaderAuth},
    claude_oauth,
    client::HttpClientFactory,
    sse::response_to_sse,
};

/// Serialize HTTP headers without dropping multi-valued fields.
pub fn header_pairs_to_json(headers: impl IntoIterator<Item = (String, String)>) -> Value {
    use std::collections::BTreeMap;

    let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, value) in headers {
        values.entry(name).or_default().push(value);
    }

    let mut result = serde_json::Map::new();
    for (name, mut entries) in values {
        let value = if entries.len() == 1 {
            Value::String(entries.pop().unwrap())
        } else {
            Value::Array(entries.into_iter().map(Value::String).collect())
        };
        result.insert(name, value);
    }
    Value::Object(result)
}

pub fn headers_to_json(headers: &reqwest::header::HeaderMap) -> Value {
    header_pairs_to_json(headers.iter().map(|(name, value)| {
        (
            name.as_str().to_owned(),
            value.to_str().unwrap_or("<non-utf8>").to_owned(),
        )
    }))
}

#[derive(Debug, Clone)]
pub struct UpstreamHeaders {
    pub request: Value,
    pub response: Value,
}

pub type ChatResult = (CanonicalChatResponse, LossReport, UpstreamHeaders);
pub type StreamResult = (
    BoxStream<'static, Result<CanonicalChunk, ProviderError>>,
    LossReport,
    UpstreamHeaders,
);

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
    /// Static target-specific fields merged after protocol encoding.
    pub request_overrides: Map<String, Value>,
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

        let mut builder = self
            .client()
            .post(&url)
            .timeout(Duration::from_millis(self.config.timeouts.overall_ms))
            .json(&body);
        builder = self.apply_auth(builder, secret);
        builder = self.apply_provider_headers(builder);

        let request = builder
            .build()
            .map_err(|e| ProviderError::Serialization(e.to_string()))?;
        let request_headers = headers_to_json(request.headers());
        let resp = self.client().execute(request).await.map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Network(e.to_string())
            }
        })?;

        let status = resp.status();
        let response_headers = headers_to_json(resp.headers());
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
        Ok((
            response,
            combined,
            UpstreamHeaders {
                request: request_headers,
                response: response_headers,
            },
        ))
    }

    #[instrument(skip(self, req, secret), fields(provider = %self.config.id, alias = %req.alias))]
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

        let mut builder = self.client().post(&url).json(&body);
        builder = self.apply_auth(builder, secret);
        builder = self.apply_provider_headers(builder);

        let request = builder
            .build()
            .map_err(|e| ProviderError::Serialization(e.to_string()))?;
        let request_headers = headers_to_json(request.headers());
        let resp = tokio::time::timeout(
            Duration::from_millis(self.config.timeouts.first_byte_ms),
            self.client().execute(request),
        )
        .await
        .map_err(|_| ProviderError::Timeout)?
        .map_err(|e| {
            if e.is_timeout() {
                ProviderError::Timeout
            } else {
                ProviderError::Network(e.to_string())
            }
        })?;

        let status = resp.status();
        let response_headers = headers_to_json(resp.headers());
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

        Ok((
            Box::pin(stream),
            encode_loss,
            UpstreamHeaders {
                request: request_headers,
                response: response_headers,
            },
        ))
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
    fn header_pairs_to_json_preserves_sensitive_and_repeated_headers() {
        let headers = header_pairs_to_json(vec![
            ("authorization".into(), "Bearer secret-token".into()),
            ("set-cookie".into(), "first=value".into()),
            ("set-cookie".into(), "second=value".into()),
        ]);

        assert_eq!(headers["authorization"], "Bearer secret-token");
        assert_eq!(
            headers["set-cookie"],
            serde_json::json!(["first=value", "second=value"])
        );
    }

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
}
