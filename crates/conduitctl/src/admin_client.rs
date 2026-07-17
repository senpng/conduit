//! HTTP client for the conduitd admin API.
//!
//! Shared by CLI subcommands. DTOs live in `dto`.

use std::time::Duration;

use futures::StreamExt;
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{
    dto::{
        CreateKeyBody, CreateProviderBody, CreateRouteBody, HealthResponse, KeyCreateResponse,
        TraceListResponse,
    },
    util::sse::{classify_sse_frame, parse_sse_frame, SseFrame},
};

/// Errors from admin HTTP / SSE transport.
#[derive(Debug, Error)]
pub enum AdminError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("sse: {0}")]
    Sse(String),
}

/// Shared admin API client (loopback by default).
#[derive(Clone)]
pub struct AdminClient {
    base: String,
    /// Short-lived CRUD calls (JSON).
    http: reqwest::Client,
    /// Long-lived SSE (no total request timeout — required for `trace tail`).
    http_sse: reqwest::Client,
}

impl AdminClient {
    /// Create a client for `admin_addr` (e.g. `http://127.0.0.1:4001`).
    pub fn new(admin_addr: &str) -> Self {
        let base = admin_addr.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        // SSE streams must not inherit a 30s total timeout — that aborts the body
        // mid-stream with "error decoding response body" after idle/keep-alive.
        let http_sse = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // No overall timeout; read stays open until server/client closes.
            .pool_max_idle_per_host(2)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            base,
            http,
            http_sse,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    // ── Health ──────────────────────────────────────────────────────────────

    pub async fn health(&self) -> Result<HealthResponse, AdminError> {
        let url = format!("{}/health", self.base);
        self.get_json(&url).await
    }

    // ── Settings ────────────────────────────────────────────────────────────

    pub async fn get_settings(&self) -> Result<crate::dto::SettingsResponse, AdminError> {
        let url = format!("{}/admin/settings", self.base);
        self.get_json(&url).await
    }

    pub async fn update_settings(
        &self,
        body: &crate::dto::UpdateSettingsBody,
    ) -> Result<crate::dto::SettingsResponse, AdminError> {
        let url = format!("{}/admin/settings", self.base);
        self.put_json(&url, body).await
    }

    // ── Traces ──────────────────────────────────────────────────────────────

    pub async fn list_traces(&self, limit: usize) -> Result<TraceListResponse, AdminError> {
        let url = format!("{}/admin/traces?limit={}", self.base, limit);
        self.get_json(&url).await
    }

    pub async fn get_trace_bundle(&self, id: &str) -> Result<Value, AdminError> {
        let url = format!("{}/admin/traces/{}", self.base, id);
        self.get_json(&url).await
    }

    pub async fn replay_dry_run(&self, id: &str) -> Result<Value, AdminError> {
        let url = format!("{}/admin/traces/{}/replay?dry_run=true", self.base, id);
        self.post_empty_json(&url).await
    }

    /// Subscribe to `GET /admin/traces/stream` (SSE).
    ///
    /// Sends [`SseFrame`] values (trace data or lagged).
    pub fn subscribe_traces(
        &self,
        tx: mpsc::Sender<Result<SseFrame, AdminError>>,
    ) -> JoinHandle<()> {
        let client = self.clone();
        tokio::spawn(async move {
            if let Err(e) = client.run_trace_sse(tx.clone()).await {
                let _ = tx.send(Err(e)).await;
            }
        })
    }

    async fn run_trace_sse(
        &self,
        tx: mpsc::Sender<Result<SseFrame, AdminError>>,
    ) -> Result<(), AdminError> {
        let url = format!("{}/admin/traces/stream", self.base);
        // Use the SSE client (no total request timeout).
        let resp = self
            .http_sse
            .get(&url)
            .header("accept", "text/event-stream")
            .header("cache-control", "no-cache")
            .send()
            .await
            .map_err(|e| AdminError::Sse(format!("connect: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(AdminError::Http { status, body });
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                // Surface common causes clearly (timeout was the usual culprit).
                AdminError::Sse(format!("stream read: {e}"))
            })?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buf.find("\n\n") {
                let frame = buf[..pos].to_string();
                buf = buf[pos + 2..].to_string();

                if let Some(raw) = parse_sse_frame(&frame) {
                    if let Some(classified) = classify_sse_frame(&raw) {
                        // Reject legacy stub payloads if reintroduced.
                        if let SseFrame::TraceData(ref payload) = classified {
                            if payload.contains("not yet implemented") {
                                return Err(AdminError::Sse(
                                    "received stub tail payload; admin stream is broken".into(),
                                ));
                            }
                        }
                        // try_send: never block the SSE read loop (UI applies backpressure via drop).
                        match tx.try_send(Ok(classified)) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                // Drop frame when consumer is not keeping up.
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                // Receiver dropped — stop quietly.
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        // Clean EOF from server (daemon restart / graceful close).
        Ok(())
    }

    // ── Providers ───────────────────────────────────────────────────────────

    pub async fn list_providers(&self) -> Result<Value, AdminError> {
        let url = format!("{}/admin/providers", self.base);
        self.get_json(&url).await
    }

    pub async fn create_provider(&self, body: &CreateProviderBody) -> Result<Value, AdminError> {
        let url = format!("{}/admin/providers", self.base);
        self.post_json(&url, body).await
    }

    pub async fn delete_provider(&self, id: &str) -> Result<(), AdminError> {
        let url = format!("{}/admin/providers/{}", self.base, id);
        self.delete_ok(&url).await
    }

    // ── Routes (path parameter is route **id**, not alias) ──────────────────

    pub async fn list_routes(&self) -> Result<Value, AdminError> {
        let url = format!("{}/admin/routes", self.base);
        self.get_json(&url).await
    }

    pub async fn get_route(&self, id: &str) -> Result<Value, AdminError> {
        let url = format!("{}/admin/routes/{}", self.base, id);
        self.get_json(&url).await
    }

    pub async fn delete_route(&self, id: &str) -> Result<(), AdminError> {
        let url = format!("{}/admin/routes/{}", self.base, id);
        self.delete_ok(&url).await
    }

    pub async fn create_route(&self, body: &CreateRouteBody) -> Result<Value, AdminError> {
        let url = format!("{}/admin/routes", self.base);
        self.post_json(&url, body).await
    }

    /// PUT /admin/routes/{id} — body uses same `targets` array shape as create.
    pub async fn update_route(
        &self,
        id: &str,
        body: &CreateRouteBody,
    ) -> Result<Value, AdminError> {
        let url = format!("{}/admin/routes/{}", self.base, id);
        // UpdateRouteBody fields are optional; send full replace via present fields.
        let payload = serde_json::json!({
            "match_alias": body.match_alias,
            "strategy": body.strategy,
            "targets": body.targets,
            "retry_policy": body.retry_policy,
        });
        self.put_json(&url, &payload).await
    }

    async fn put_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, AdminError> {
        let resp = self.http.put(url).json(body).send().await?;
        self.json_response(resp).await
    }

    // ── Keys / usage / OAuth ────────────────────────────────────────────────

    pub async fn list_keys(&self) -> Result<Value, AdminError> {
        let url = format!("{}/admin/keys", self.base);
        self.get_json(&url).await
    }

    pub async fn create_key(&self, body: &CreateKeyBody) -> Result<KeyCreateResponse, AdminError> {
        let url = format!("{}/admin/keys", self.base);
        self.post_json(&url, body).await
    }

    pub async fn delete_key(&self, id: &str) -> Result<(), AdminError> {
        let url = format!("{}/admin/keys/{}", self.base, id);
        self.delete_ok(&url).await
    }

    /// Period rollup of request consumption (`GET /admin/usage/summary`).
    pub async fn list_usage_summary(&self) -> Result<Value, AdminError> {
        let url = format!("{}/admin/usage/summary", self.base);
        self.get_json(&url).await
    }

    /// Recent per-request usage rows.
    pub async fn list_usage(&self, limit: usize) -> Result<Value, AdminError> {
        let url = format!("{}/admin/usage?limit={limit}", self.base);
        self.get_json(&url).await
    }

    pub async fn start_oauth(&self, kind: &str, body: &Value) -> Result<Value, AdminError> {
        let url = format!("{}/admin/oauth/{}/start", self.base, kind);
        self.post_json(&url, body).await
    }

    pub async fn oauth_session(&self, id: &str) -> Result<Value, AdminError> {
        let url = format!("{}/admin/oauth/sessions/{}", self.base, id);
        self.get_json(&url).await
    }

    pub async fn cancel_oauth(&self, id: &str) -> Result<(), AdminError> {
        let url = format!("{}/admin/oauth/sessions/{}/cancel", self.base, id);
        self.post_empty_unit(&url).await
    }

    async fn post_empty_unit(&self, url: &str) -> Result<(), AdminError> {
        let resp = self.http.post(url).send().await?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(AdminError::Http {
            status: status.as_u16(),
            body,
        })
    }

    // ── HTTP helpers ────────────────────────────────────────────────────────

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, AdminError> {
        let resp = self.http.get(url).send().await?;
        self.json_response(resp).await
    }

    async fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, AdminError> {
        let resp = self.http.post(url).json(body).send().await?;
        self.json_response(resp).await
    }

    async fn post_empty_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, AdminError> {
        let resp = self.http.post(url).send().await?;
        self.json_response(resp).await
    }

    async fn delete_ok(&self, url: &str) -> Result<(), AdminError> {
        let resp = self.http.delete(url).send().await?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(AdminError::Http {
            status: status.as_u16(),
            body,
        })
    }

    async fn json_response<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, AdminError> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AdminError::Http {
                status: status.as_u16(),
                body,
            });
        }
        let body = resp.bytes().await?;
        Ok(serde_json::from_slice(&body)?)
    }
}

/// Build the exact JSON body sent by `provider add` (testable without HTTP).
pub fn provider_create_request_body(
    name: &str,
    kind: &str,
    base_url: &str,
    api_key: Option<&str>,
) -> CreateProviderBody {
    let mut body = CreateProviderBody::new(name, kind, base_url);
    if let Some(k) = api_key {
        body = body.with_api_key(k);
    }
    body
}

/// Build the admin path segment for route get/remove (always the route **id**).
pub fn route_admin_path(id: &str) -> String {
    format!("/admin/routes/{}", id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_create_request_body_matches_create_provider_body_contract() {
        let body = provider_create_request_body(
            "my-openai",
            "openai",
            "https://api.openai.com/v1",
            Some("sk-live"),
        );
        let json = serde_json::to_value(&body).unwrap();
        // Acceptance: name/kind/base_url[/api_key], never client `id`.
        assert!(json.get("id").is_none(), "must not send client id: {json}");
        let keys: std::collections::BTreeSet<_> = json
            .as_object()
            .unwrap()
            .keys()
            .map(|s| s.as_str())
            .collect();
        assert_eq!(
            keys,
            ["api_key", "base_url", "kind", "name"]
                .into_iter()
                .collect()
        );
        assert_eq!(json["name"], "my-openai");
        assert_eq!(json["kind"], "openai");
        assert_eq!(json["base_url"], "https://api.openai.com/v1");
        assert_eq!(json["api_key"], "sk-live");
    }

    #[test]
    fn route_admin_path_uses_id_segment() {
        assert_eq!(route_admin_path("01HQROUTEID"), "/admin/routes/01HQROUTEID");
        // Must not invent alias-based paths.
        assert!(!route_admin_path("gpt-4o").contains("alias"));
    }

    #[test]
    fn admin_client_trims_trailing_slash() {
        let c = AdminClient::new("http://127.0.0.1:4001/");
        assert_eq!(c.base_url(), "http://127.0.0.1:4001");
    }
}
