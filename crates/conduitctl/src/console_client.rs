//! HTTP client for the conduitd console API.
//!
//! Shared by CLI subcommands. DTOs live in `dto`.

use std::time::Duration;

use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use serde_json::Value;
use thiserror::Error;

use crate::dto::{
    CooldownListResponse, CreateKeyBody, CreateProviderBody, CreateRouteBody, HealthResponse,
    KeyCreateResponse, KeySecretView, KeyView, OAuthSessionView, PricingView, ProviderSecretView,
    ProviderView, QuotaListResponse, RouteView, SetSecretBody, UpdateKeyBody, UpdateProviderBody,
    UpsertPricingOverrideBody, UsageListResponse, UsageSummaryView,
};

/// Errors from console HTTP / SSE transport.
#[derive(Debug, Error)]
pub enum ConsoleError {
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("decode: {0}")]
    Decode(#[from] serde_json::Error),
}

/// Shared console API client (loopback by default).
#[derive(Clone)]
pub struct ConsoleClient {
    base: String,
    /// Short-lived CRUD calls (JSON).
    http: reqwest::Client,
}

impl ConsoleClient {
    /// Create a client for `console_addr` (e.g. `http://127.0.0.1:4001`).
    pub fn new(console_addr: &str) -> Self {
        let base = console_addr.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            // Pricing sync can pull a large remote map; match CLI pricing timeout.
            .timeout(Duration::from_secs(90))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self { base, http }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    // ── Health ──────────────────────────────────────────────────────────────

    pub async fn health(&self) -> Result<HealthResponse, ConsoleError> {
        let url = format!("{}/health", self.base);
        self.get_json(&url).await
    }

    // ── Providers ───────────────────────────────────────────────────────────

    pub async fn list_providers(&self) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/providers", self.base);
        self.get_json(&url).await
    }

    pub async fn list_providers_typed(&self) -> Result<Vec<ProviderView>, ConsoleError> {
        let url = format!("{}/console/providers", self.base);
        self.get_json(&url).await
    }

    pub async fn create_provider(&self, body: &CreateProviderBody) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/providers", self.base);
        self.post_json(&url, body).await
    }

    pub async fn update_provider(
        &self,
        id: &str,
        body: &UpdateProviderBody,
    ) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/providers/{}", self.base, id);
        self.put_json(&url, body).await
    }

    pub async fn set_provider_secret(
        &self,
        id: &str,
        body: &SetSecretBody,
    ) -> Result<(), ConsoleError> {
        let url = format!("{}/console/providers/{}/secret", self.base, id);
        self.put_unit(&url, body).await
    }

    /// Decrypt and return the upstream secret (API key or OAuth bundle).
    pub async fn get_provider_secret(
        &self,
        id: &str,
    ) -> Result<ProviderSecretView, ConsoleError> {
        let url = format!("{}/console/providers/{}/secret", self.base, id);
        self.get_json(&url).await
    }

    pub async fn delete_provider(&self, id: &str) -> Result<(), ConsoleError> {
        let url = format!("{}/console/providers/{}", self.base, id);
        self.delete_ok(&url).await
    }

    // ── Routes (path parameter is route **id**, not alias) ──────────────────

    pub async fn list_routes(&self) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/routes", self.base);
        self.get_json(&url).await
    }

    pub async fn list_routes_typed(&self) -> Result<Vec<RouteView>, ConsoleError> {
        let url = format!("{}/console/routes", self.base);
        self.get_json(&url).await
    }

    pub async fn get_route(&self, id: &str) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/routes/{}", self.base, id);
        self.get_json(&url).await
    }

    pub async fn delete_route(&self, id: &str) -> Result<(), ConsoleError> {
        let url = format!("{}/console/routes/{}", self.base, id);
        self.delete_ok(&url).await
    }

    pub async fn create_route(&self, body: &CreateRouteBody) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/routes", self.base);
        self.post_json(&url, body).await
    }

    /// PUT /console/routes/{id} — body uses same `targets` array shape as create.
    pub async fn update_route(
        &self,
        id: &str,
        body: &CreateRouteBody,
    ) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/routes/{}", self.base, id);
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
    ) -> Result<T, ConsoleError> {
        let resp = self.http.put(url).json(body).send().await?;
        self.json_response(resp).await
    }

    async fn put_unit<B: serde::Serialize>(&self, url: &str, body: &B) -> Result<(), ConsoleError> {
        let resp = self.http.put(url).json(body).send().await?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(ConsoleError::Http {
            status: status.as_u16(),
            body,
        })
    }

    async fn post_empty_json(&self, url: &str) -> Result<Value, ConsoleError> {
        let resp = self.http.post(url).send().await?;
        self.json_response(resp).await
    }

    // ── Keys / usage / OAuth ────────────────────────────────────────────────

    pub async fn list_keys(&self) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/keys", self.base);
        self.get_json(&url).await
    }

    pub async fn list_keys_typed(&self) -> Result<Vec<KeyView>, ConsoleError> {
        let url = format!("{}/console/keys", self.base);
        self.get_json(&url).await
    }

    /// Decrypt and return a downstream key's raw token (reveal endpoint).
    pub async fn get_key_secret(&self, id: &str) -> Result<KeySecretView, ConsoleError> {
        let url = format!("{}/console/keys/{}/secret", self.base, id);
        self.get_json(&url).await
    }

    pub async fn create_key(
        &self,
        body: &CreateKeyBody,
    ) -> Result<KeyCreateResponse, ConsoleError> {
        let url = format!("{}/console/keys", self.base);
        self.post_json(&url, body).await
    }

    pub async fn update_key(&self, id: &str, body: &UpdateKeyBody) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/keys/{}", self.base, id);
        self.put_json(&url, body).await
    }

    pub async fn delete_key(&self, id: &str) -> Result<(), ConsoleError> {
        let url = format!("{}/console/keys/{}", self.base, id);
        self.delete_ok(&url).await
    }

    /// Period rollup of request consumption (`GET /console/usage/summary`).
    pub async fn list_usage_summary(&self) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/usage/summary", self.base);
        self.get_json(&url).await
    }

    pub async fn usage_summary_typed(
        &self,
        period: Option<&str>,
    ) -> Result<UsageSummaryView, ConsoleError> {
        let mut url = format!("{}/console/usage/summary", self.base);
        if let Some(p) = period {
            url.push_str(&format!("?period={p}"));
        }
        self.get_json(&url).await
    }

    /// Paginated per-request usage rows.
    ///
    /// Optional `period` (`YYYY-MM`) scopes to that calendar month.
    /// `offset` / `q` / `sort` support real pagination, free-text filter, and sort.
    pub async fn list_usage(
        &self,
        limit: usize,
        period: Option<&str>,
    ) -> Result<Value, ConsoleError> {
        self.list_usage_query(limit, 0, period, None, None, None)
            .await
    }

    pub async fn list_usage_typed(
        &self,
        limit: usize,
        period: Option<&str>,
    ) -> Result<UsageListResponse, ConsoleError> {
        self.list_usage_page(limit, 0, period, None, None).await
    }

    /// Full usage list query (pagination + filter + sort).
    pub async fn list_usage_page(
        &self,
        limit: usize,
        offset: usize,
        period: Option<&str>,
        q: Option<&str>,
        sort: Option<&str>,
    ) -> Result<UsageListResponse, ConsoleError> {
        self.list_usage_query(limit, offset, period, None, q, sort)
            .await
    }

    async fn list_usage_query<T: serde::de::DeserializeOwned>(
        &self,
        limit: usize,
        offset: usize,
        period: Option<&str>,
        key_id: Option<&str>,
        q: Option<&str>,
        sort: Option<&str>,
    ) -> Result<T, ConsoleError> {
        let mut url = format!("{}/console/usage?limit={limit}&offset={offset}", self.base);
        if let Some(p) = period {
            url.push_str(&format!("&period={}", urlencoding_path(p)));
        }
        if let Some(k) = key_id {
            url.push_str(&format!("&key_id={}", urlencoding_path(k)));
        }
        if let Some(query) = q.filter(|s| !s.is_empty()) {
            url.push_str(&format!("&q={}", urlencoding_path(query)));
        }
        if let Some(s) = sort.filter(|s| !s.is_empty()) {
            url.push_str(&format!("&sort={}", urlencoding_path(s)));
        }
        self.get_json(&url).await
    }

    pub async fn list_pricing_typed(&self) -> Result<Vec<PricingView>, ConsoleError> {
        let url = format!("{}/console/pricing", self.base);
        self.get_json(&url).await
    }

    /// Operator overrides only (`pricing.json`), not the full merged table.
    pub async fn list_pricing_overrides(&self) -> Result<Vec<PricingView>, ConsoleError> {
        let url = format!("{}/console/pricing/overrides", self.base);
        self.get_json(&url).await
    }

    pub async fn upsert_pricing_override(
        &self,
        body: &UpsertPricingOverrideBody,
    ) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/pricing/overrides", self.base);
        self.put_json(&url, body).await
    }

    pub async fn delete_pricing_override(
        &self,
        provider_kind: &str,
        model_id: &str,
    ) -> Result<Value, ConsoleError> {
        // Query params: model ids may contain `/` which breaks path routing.
        let url = format!(
            "{}/console/pricing/overrides?provider_kind={}&model_id={}",
            self.base,
            urlencoding_path(provider_kind),
            urlencoding_path(model_id),
        );
        let resp = self.http.delete(&url).send().await?;
        self.json_response(resp).await
    }

    pub async fn reload_pricing(&self) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/pricing/reload", self.base);
        self.post_empty_json(&url).await
    }

    pub async fn sync_pricing(&self, source_url: Option<&str>) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/pricing/sync", self.base);
        let body = match source_url {
            Some(u) => serde_json::json!({ "url": u }),
            None => serde_json::json!({}),
        };
        self.post_json(&url, &body).await
    }

    pub async fn list_oauth_kinds(&self) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/oauth/providers", self.base);
        self.get_json(&url).await
    }

    pub async fn start_oauth(&self, kind: &str, body: &Value) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/oauth/{}/start", self.base, kind);
        self.post_json(&url, body).await
    }

    pub async fn start_oauth_typed(
        &self,
        kind: &str,
        body: &Value,
    ) -> Result<OAuthSessionView, ConsoleError> {
        let url = format!("{}/console/oauth/{}/start", self.base, kind);
        self.post_json(&url, body).await
    }

    pub async fn oauth_session(&self, id: &str) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/oauth/sessions/{}", self.base, id);
        self.get_json(&url).await
    }

    pub async fn oauth_session_typed(&self, id: &str) -> Result<OAuthSessionView, ConsoleError> {
        let url = format!("{}/console/oauth/sessions/{}", self.base, id);
        self.get_json(&url).await
    }

    pub async fn cancel_oauth(&self, id: &str) -> Result<(), ConsoleError> {
        let url = format!("{}/console/oauth/sessions/{}/cancel", self.base, id);
        self.post_empty_unit(&url).await
    }

    pub async fn refresh_oauth(&self, provider_id: &str) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/oauth/{}/refresh", self.base, provider_id);
        self.post_empty_json(&url).await
    }

    /// Last-seen / probed upstream quota snapshots.
    pub async fn list_quota_snapshots(&self) -> Result<QuotaListResponse, ConsoleError> {
        let url = format!("{}/console/quota-snapshots", self.base);
        self.get_json(&url).await
    }

    pub async fn list_cooldowns(&self) -> Result<CooldownListResponse, ConsoleError> {
        let url = format!("{}/console/cooldowns", self.base);
        self.get_json(&url).await
    }

    /// Probe all OAuth providers' subscription remaining (Claude/Codex usage + Grok billing).
    pub async fn refresh_all_quotas(&self) -> Result<Value, ConsoleError> {
        let url = format!("{}/console/quota-snapshots/refresh", self.base);
        self.post_empty_json(&url).await
    }

    /// Probe one provider's remaining (OAuth usage API when applicable).
    pub async fn refresh_quota(&self, provider_id: &str) -> Result<Value, ConsoleError> {
        let url = format!(
            "{}/console/quota-snapshots/{provider_id}/refresh",
            self.base
        );
        self.post_empty_json(&url).await
    }

    async fn post_empty_unit(&self, url: &str) -> Result<(), ConsoleError> {
        let resp = self.http.post(url).send().await?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(ConsoleError::Http {
            status: status.as_u16(),
            body,
        })
    }

    // ── HTTP helpers ────────────────────────────────────────────────────────

    async fn get_json<T: DeserializeOwned>(&self, url: &str) -> Result<T, ConsoleError> {
        let resp = self.http.get(url).send().await?;
        self.json_response(resp).await
    }

    async fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> Result<T, ConsoleError> {
        let resp = self.http.post(url).json(body).send().await?;
        self.json_response(resp).await
    }

    async fn delete_ok(&self, url: &str) -> Result<(), ConsoleError> {
        let resp = self.http.delete(url).send().await?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        Err(ConsoleError::Http {
            status: status.as_u16(),
            body,
        })
    }

    async fn json_response<T: DeserializeOwned>(
        &self,
        resp: reqwest::Response,
    ) -> Result<T, ConsoleError> {
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ConsoleError::Http {
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

/// Build the console path segment for route get/remove (always the route **id**).
pub fn route_console_path(id: &str) -> String {
    format!("/console/routes/{}", id)
}

/// Percent-encode a query/path value (including `/`).
fn urlencoding_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
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
    fn route_console_path_uses_id_segment() {
        assert_eq!(
            route_console_path("01HQROUTEID"),
            "/console/routes/01HQROUTEID"
        );
        // Must not invent alias-based paths.
        assert!(!route_console_path("gpt-4o").contains("alias"));
    }

    #[test]
    fn console_client_trims_trailing_slash() {
        let c = ConsoleClient::new("http://127.0.0.1:4001/");
        assert_eq!(c.base_url(), "http://127.0.0.1:4001");
    }
}
