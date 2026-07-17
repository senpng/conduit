//! Sensitive-word obfuscation (CLIProxyAPI `cloak_obfuscate.go`).

use regex::Regex;
use serde_json::Value;

const ZWSP: char = '\u{200B}';

pub struct SensitiveWordMatcher {
    re: Regex,
}

impl SensitiveWordMatcher {
    /// Build matcher; words length ≥ 2, longest-first, case-insensitive.
    pub fn build(words: &[String]) -> Option<Self> {
        let mut valid: Vec<String> = words
            .iter()
            .map(|w| w.trim().to_string())
            .filter(|w| w.chars().count() >= 2 && !w.contains(ZWSP))
            .collect();
        if valid.is_empty() {
            return None;
        }
        valid.sort_by_key(|b| std::cmp::Reverse(b.len()));
        let escaped: Vec<String> = valid.iter().map(|w| regex::escape(w)).collect();
        let pattern = format!("(?i){}", escaped.join("|"));
        let re = Regex::new(&pattern).ok()?;
        Some(Self { re })
    }

    fn obfuscate_text(&self, text: &str) -> String {
        self.re
            .replace_all(text, |caps: &regex::Captures| {
                obfuscate_word(caps.get(0).map(|m| m.as_str()).unwrap_or(""))
            })
            .into_owned()
    }
}

fn obfuscate_word(word: &str) -> String {
    if word.contains(ZWSP) {
        return word.to_string();
    }
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return word.to_string();
    };
    let rest: String = chars.collect();
    if rest.is_empty() {
        return word.to_string();
    }
    format!("{first}{ZWSP}{rest}")
}

/// Obfuscate sensitive words in system + message text blocks.
pub fn obfuscate_sensitive_words(mut body: Value, matcher: &SensitiveWordMatcher) -> Value {
    // system
    match body.get_mut("system") {
        Some(Value::Array(arr)) => {
            for part in arr.iter_mut() {
                if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                        let o = matcher.obfuscate_text(t);
                        if o != t {
                            part["text"] = Value::String(o);
                        }
                    }
                }
            }
        }
        Some(Value::String(s)) => {
            let o = matcher.obfuscate_text(s);
            if o != *s {
                body["system"] = Value::String(o);
            }
        }
        _ => {}
    }

    // messages
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            match msg.get_mut("content") {
                Some(Value::String(s)) => {
                    let o = matcher.obfuscate_text(s);
                    if o != *s {
                        *s = o;
                    }
                }
                Some(Value::Array(arr)) => {
                    for part in arr.iter_mut() {
                        if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                let o = matcher.obfuscate_text(t);
                                if o != t {
                                    part["text"] = Value::String(o);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn inserts_zwsp_in_words() {
        let m = SensitiveWordMatcher::build(&["claude".into(), "anthropic".into()]).unwrap();
        let body = json!({
            "system": "use claude carefully",
            "messages": [{"role": "user", "content": "talk to Anthropic"}]
        });
        let out = obfuscate_sensitive_words(body, &m);
        let sys = out["system"].as_str().unwrap();
        assert!(sys.contains('\u{200B}'));
        assert!(sys.to_lowercase().contains('c'));
        let user = out["messages"][0]["content"].as_str().unwrap();
        assert!(user.contains('\u{200B}'));
    }
}
