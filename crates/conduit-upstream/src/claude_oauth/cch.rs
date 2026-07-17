//! Anthropic Claude Code `cch` body signature (CLIProxyAPI / Claude Code parity).

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;
use xxhash_rust::xxh64::xxh64;

/// Seed from CLIProxyAPI `claudeCCHSeed = 0x6E52736AC806831E`.
const CLAUDE_CCH_SEED: u64 = 0x6E_52_73_6A_C8_06_83_1E;

fn cch_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bcch=([0-9a-f]{5});").expect("cch regex"))
}

/// Sign `system[0].text` billing header `cch=` over the full body with cch zeroed.
/// No-op if billing header / cch placeholder missing.
pub fn sign_anthropic_messages_body(mut body: Value) -> Value {
    let Some(billing) = body
        .pointer("/system/0/text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
    else {
        return body;
    };
    if !billing.starts_with("x-anthropic-billing-header:") {
        return body;
    }
    if !cch_pattern().is_match(&billing) {
        return body;
    }

    let unsigned_billing = cch_pattern()
        .replace_all(&billing, "cch=00000;")
        .into_owned();
    if let Some(slot) = body.pointer_mut("/system/0/text") {
        *slot = Value::String(unsigned_billing.clone());
    }

    let Ok(unsigned_bytes) = serde_json::to_vec(&body) else {
        return body;
    };
    let cch = format!("{:05x}", xxh64(&unsigned_bytes, CLAUDE_CCH_SEED) & 0xF_FFFF);
    let signed_billing = cch_pattern()
        .replace_all(&unsigned_billing, format!("cch={cch};").as_str())
        .into_owned();
    if let Some(slot) = body.pointer_mut("/system/0/text") {
        *slot = Value::String(signed_billing);
    }
    body
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn signs_placeholder_cch() {
        let body = json!({
            "system": [{
                "type": "text",
                "text": "x-anthropic-billing-header: cc_version=2.1.63.abc; cc_entrypoint=cli; cch=00000;"
            }],
            "messages": [{"role": "user", "content": "hi"}]
        });
        let signed = sign_anthropic_messages_body(body);
        let text = signed["system"][0]["text"].as_str().unwrap();
        assert!(text.contains("cch="));
        assert!(!text.contains("cch=00000;"), "cch should be filled: {text}");
        let cap = cch_pattern().captures(text).unwrap();
        assert_eq!(cap.get(1).unwrap().as_str().len(), 5);
    }
}
