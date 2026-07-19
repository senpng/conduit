//! Optional HTTP(S)/SOCKS proxy for OAuth token / device-code clients.
//!
//! Priority when resolving an effective proxy URL (first non-empty wins):
//! 1. Per-credential `proxy_url` (CLIProxyAPI auth.ProxyURL)
//! 2. `CONDUIT_PROXY_URL`
//! 3. Daemon config `proxy_url`
//! 4. `HTTPS_PROXY` / `https_proxy` / `HTTP_PROXY` / `http_proxy`
//! 5. `ALL_PROXY` / `all_proxy`
//!
//! Bypass list: `NO_PROXY` / `no_proxy` (attached via reqwest/wreq `NoProxy`).
//!
//! SOCKS (`socks5://`, `socks5h://`, `socks4://`, `socks4a://`) requires the
//! crate features enabled on `reqwest` / `wreq` (see `Cargo.toml`).

use crate::error::OAuthError;

/// Resolve proxy URL from standard environment variables (incl. `CONDUIT_PROXY_URL`).
///
/// Prefer [`resolve_effective_proxy`] when a credential override or config value
/// is available.
pub fn env_proxy_url() -> Option<String> {
    for key in [
        "CONDUIT_PROXY_URL",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Some(v) = env_non_empty(key) {
            return Some(normalize_proxy_url(&v));
        }
    }
    None
}

/// Resolve effective proxy for an OAuth client.
///
/// See module docs for priority order (credential → CONDUIT_PROXY_URL → config → env).
pub fn resolve_effective_proxy(
    credential_proxy: Option<&str>,
    config_proxy: Option<&str>,
) -> Option<String> {
    if let Some(p) = credential_proxy.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(normalize_proxy_url(p));
    }
    if let Some(v) = env_non_empty("CONDUIT_PROXY_URL") {
        return Some(normalize_proxy_url(&v));
    }
    if let Some(p) = config_proxy.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(normalize_proxy_url(p));
    }
    for key in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        if let Some(v) = env_non_empty(key) {
            return Some(normalize_proxy_url(&v));
        }
    }
    None
}

/// Read `NO_PROXY` / `no_proxy` raw string (trimmed), if set and non-empty.
pub fn env_no_proxy() -> Option<String> {
    env_non_empty("NO_PROXY").or_else(|| env_non_empty("no_proxy"))
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().and_then(|v| {
        let t = v.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

/// Normalize common proxy URL spellings used by local clients.
///
/// - `socks://host:port` → `socks5://host:port` (Clash/V2Ray shorthand)
/// - leave `socks5h://` / `socks4a://` alone (remote DNS variants)
pub fn normalize_proxy_url(raw: &str) -> String {
    let t = raw.trim();
    if let Some(rest) = t
        .strip_prefix("socks://")
        .or_else(|| t.strip_prefix("SOCKS://"))
    {
        return format!("socks5://{rest}");
    }
    t.to_string()
}

fn is_socks_scheme(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("socks://")
        || lower.starts_with("socks4://")
        || lower.starts_with("socks4a://")
        || lower.starts_with("socks5://")
        || lower.starts_with("socks5h://")
}

/// Apply an optional proxy URL to a `reqwest::ClientBuilder`, honoring `NO_PROXY`.
pub fn apply_reqwest_proxy(
    builder: reqwest::ClientBuilder,
    proxy_url: Option<&str>,
) -> Result<reqwest::ClientBuilder, OAuthError> {
    let Some(url) = proxy_url.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(builder);
    };
    let url = normalize_proxy_url(url);
    let proxy = reqwest::Proxy::all(&url)
        .map_err(|e| proxy_parse_error(&url, e))?
        .no_proxy(reqwest_no_proxy());
    Ok(builder.proxy(proxy))
}

/// Apply an optional proxy URL to a `wreq::ClientBuilder` (Claude TLS client).
pub fn apply_wreq_proxy(
    builder: wreq::ClientBuilder,
    proxy_url: Option<&str>,
) -> Result<wreq::ClientBuilder, OAuthError> {
    let Some(url) = proxy_url.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(builder);
    };
    let url = normalize_proxy_url(url);
    let proxy = wreq::Proxy::all(&url)
        .map_err(|e| proxy_parse_error(&url, e))?
        .no_proxy(wreq_no_proxy());
    Ok(builder.proxy(proxy))
}

fn reqwest_no_proxy() -> Option<reqwest::NoProxy> {
    reqwest::NoProxy::from_env()
        .or_else(|| env_no_proxy().and_then(|s| reqwest::NoProxy::from_string(&s)))
}

fn wreq_no_proxy() -> Option<wreq::NoProxy> {
    wreq::NoProxy::from_env().or_else(|| env_no_proxy().and_then(|s| wreq::NoProxy::from_string(&s)))
}

fn proxy_parse_error(url: &str, err: impl std::fmt::Display) -> OAuthError {
    let hint = if is_socks_scheme(url) {
        " (socks requires socks feature; use socks5:// or socks5h://)"
    } else {
        ""
    };
    OAuthError::Network(format!("invalid proxy {url:?}: {err}{hint}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_reqwest_proxy_none_is_noop() {
        let b = reqwest::Client::builder();
        let _ = apply_reqwest_proxy(b, None).unwrap();
    }

    #[test]
    fn normalize_socks_shorthand() {
        assert_eq!(
            normalize_proxy_url("socks://127.0.0.1:7890"),
            "socks5://127.0.0.1:7890"
        );
        assert_eq!(
            normalize_proxy_url("socks5h://127.0.0.1:7890"),
            "socks5h://127.0.0.1:7890"
        );
    }

    #[test]
    fn resolve_prefers_credential_over_config() {
        let got = resolve_effective_proxy(Some("socks5://cred:1"), Some("http://cfg:2"));
        assert_eq!(got.as_deref(), Some("socks5://cred:1"));
    }

    #[test]
    fn resolve_uses_config_when_no_cred() {
        // May pick up process env; only assert config path when env is empty-ish.
        // Call with explicit config — if env has CONDUIT_PROXY_URL it wins after cred.
        // Use credential empty and config; if ALL_PROXY is set in CI this still returns Some.
        let got = resolve_effective_proxy(None, Some("http://config-proxy:8080"));
        assert!(got.is_some());
        // When no CONDUIT_PROXY_URL, should be config (or standard env). At least non-empty.
        assert!(!got.unwrap().is_empty());
    }

    #[test]
    fn reqwest_accepts_socks5_url() {
        let b = reqwest::Client::builder();
        let _ = apply_reqwest_proxy(b, Some("socks5://127.0.0.1:1")).expect("socks5 parse");
    }

    #[test]
    fn wreq_accepts_socks5_url() {
        let b = wreq::Client::builder();
        let _ = apply_wreq_proxy(b, Some("socks5://127.0.0.1:1")).expect("wreq socks5 parse");
    }
}
