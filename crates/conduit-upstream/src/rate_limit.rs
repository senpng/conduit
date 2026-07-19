//! Capture upstream rate-limit / quota response headers (Claude `anthropic-ratelimit-*`,
//! generic `x-ratelimit-*`, `retry-after`).

use std::sync::Arc;

/// Invoked with `(provider_id, header_name_value_pairs)` when a response carries
/// rate-limit related headers (success or error).
pub type RateLimitHeaderSink = Arc<dyn Fn(&str, Vec<(String, String)>) + Send + Sync>;

/// Header names we always forward when present (case-insensitive match on name).
fn is_interesting_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("ratelimit")
        || n.contains("rate-limit")
        || n == "retry-after"
        || n == "retry-after-ms"
        || n.starts_with("anthropic-ratelimit")
        || n.starts_with("x-ratelimit")
}

/// Collect interesting headers from a generic name/value iterator.
pub fn collect_rate_limit_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in headers {
        if is_interesting_header(name) {
            let v = value.trim();
            if !v.is_empty() {
                out.push((name.to_ascii_lowercase(), v.to_string()));
            }
        }
    }
    out
}

/// reqwest response headers.
pub fn collect_from_reqwest(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in headers.iter() {
        let n = name.as_str();
        if !is_interesting_header(n) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            let v = v.trim();
            if !v.is_empty() {
                out.push((n.to_ascii_lowercase(), v.to_string()));
            }
        }
    }
    out
}

/// wreq / http HeaderMap (Claude OAuth Chrome client).
pub fn collect_from_http(headers: &http::HeaderMap) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, value) in headers.iter() {
        let n = name.as_str();
        if !is_interesting_header(n) {
            continue;
        }
        if let Ok(v) = value.to_str() {
            let v = v.trim();
            if !v.is_empty() {
                out.push((n.to_ascii_lowercase(), v.to_string()));
            }
        }
    }
    out
}

/// Fire sink if non-empty header list.
pub fn emit(sink: &Option<RateLimitHeaderSink>, provider_id: &str, headers: Vec<(String, String)>) {
    if headers.is_empty() {
        return;
    }
    if let Some(cb) = sink {
        cb(provider_id, headers);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_anthropic_and_retry_after() {
        let h = collect_rate_limit_headers([
            ("anthropic-ratelimit-requests-remaining", "9"),
            ("Content-Type", "application/json"),
            ("Retry-After", "30"),
        ]);
        assert_eq!(h.len(), 2);
        assert!(h.iter().any(|(k, v)| k.contains("requests-remaining") && v == "9"));
        assert!(h.iter().any(|(k, v)| k == "retry-after" && v == "30"));
    }
}
