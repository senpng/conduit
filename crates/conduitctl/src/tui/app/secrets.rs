//! Provider secret formatting, lookup, and clipboard copy.

use crate::dto::ProviderSecretView;

use super::{App, Mode, Tab};

impl App {
    pub(crate) fn selected_provider_id(&self) -> Option<String> {
        if self.tab != Tab::Providers {
            return None;
        }
        let idx = self.selected_data_index()?;
        self.providers.get(idx).map(|p| p.id.clone())
    }

    pub(crate) fn selected_key_id(&self) -> Option<String> {
        if self.tab != Tab::Keys {
            return None;
        }
        let idx = self.selected_data_index()?;
        self.keys.get(idx).map(|k| k.id.clone())
    }

    pub fn quota_for(&self, provider_id: &str) -> Option<&crate::dto::QuotaSnapshotView> {
        self.quota_snapshots
            .iter()
            .find(|q| q.provider_id == provider_id)
    }

    pub fn cooldown_for(&self, provider_id: &str) -> Option<&crate::dto::CooldownView> {
        self.cooldowns
            .iter()
            .find(|c| c.provider_id == provider_id)
    }

    pub fn secret_for(&self, provider_id: &str) -> Option<&ProviderSecretView> {
        self.provider_secrets.get(provider_id)
    }

    /// Copy decrypted secret to the system clipboard.
    ///
    /// - `primary_only`: API key or OAuth `access_token`
    /// - full: multi-line dump (or the modal body for one-shot key reveals)
    pub(crate) fn copy_secret(&mut self, primary_only: bool) {
        let selected = self.selected_provider_id();
        let text = if let Some(id) = selected.as_deref() {
            if let Some(view) = self.secret_for(id) {
                if primary_only {
                    secret_primary_value(view)
                } else {
                    format_provider_secret_modal(view)
                }
            } else if let Mode::SecretReveal { secret, .. } = &self.mode {
                // Downstream key create / one-shot reveal modal.
                secret.clone()
            } else {
                self.status = "No secret loaded — press v to decrypt first".into();
                return;
            }
        } else if let Mode::SecretReveal { secret, .. } = &self.mode {
            secret.clone()
        } else {
            self.status = "No secret loaded — press v to decrypt first".into();
            return;
        };

        if text.trim().is_empty() {
            self.status = "Nothing to copy".into();
            return;
        }

        match super::super::clipboard::copy_to_clipboard(&text) {
            Ok(()) => {
                let what = if primary_only {
                    "primary secret (api_key / access_token)"
                } else {
                    "full secret dump"
                };
                let n = text.len();
                self.status = format!("Copied {what} to clipboard ({n} bytes)");
            }
            Err(e) => {
                self.status = format!("Clipboard failed: {e}");
            }
        }
    }

}

/// Primary credential string: API key or OAuth access_token.
pub(crate) fn secret_primary_value(view: &crate::dto::ProviderSecretView) -> String {
    if view.secret_kind == "api_key" {
        return view.api_key.clone().unwrap_or_default();
    }
    view.oauth
        .as_ref()
        .map(|o| o.access_token.clone())
        .unwrap_or_default()
}

/// Multi-line body for the secret reveal modal.
pub(crate) fn format_provider_secret_modal(view: &crate::dto::ProviderSecretView) -> String {
    use super::super::widgets::format_local_time;

    let mut lines = vec![
        format!("provider   {}", view.provider_id),
        format!("name       {}", view.provider_name),
        format!("kind       {} / {}", view.provider_kind, view.secret_kind),
        format!("key_id     {}", view.key_id),
        String::new(),
    ];
    if view.secret_kind == "api_key" {
        lines.push(format!(
            "api_key    {}",
            view.api_key.as_deref().unwrap_or("(empty)")
        ));
        return lines.join("\n");
    }
    if let Some(o) = &view.oauth {
        fn push(lines: &mut Vec<String>, k: &str, v: &str) {
            if !v.is_empty() {
                lines.push(format!("{k:<12}{v}"));
            }
        }
        push(&mut lines, "type", &o.provider_type);
        push(&mut lines, "auth_kind", &o.auth_kind);
        if let Some(e) = &o.email {
            push(&mut lines, "email", e);
        }
        if let Some(a) = &o.account_id {
            push(&mut lines, "account_id", a);
        }
        if let Some(pl) = &o.plan_type {
            push(&mut lines, "plan", pl);
        }
        if let Some(org) = &o.organization_name {
            push(&mut lines, "org", org);
        }
        if let Some(oid) = &o.organization_id {
            push(&mut lines, "org_id", oid);
        }
        if let Some(sub) = &o.sub {
            push(&mut lines, "sub", sub);
        }
        if let Some(exp) = &o.expired {
            push(&mut lines, "expired", &format_local_time(exp));
        }
        if let Some(lr) = &o.last_refresh {
            push(&mut lines, "last_refresh", &format_local_time(lr));
        }
        if let Some(bu) = &o.base_url {
            push(&mut lines, "base_url", bu);
        }
        if let Some(px) = &o.proxy_url {
            push(&mut lines, "proxy_url", px);
        }
        if let Some(ua) = o.using_api {
            push(&mut lines, "using_api", &ua.to_string());
        }
        lines.push(String::new());
        push(&mut lines, "access_token", &o.access_token);
        push(&mut lines, "refresh_token", &o.refresh_token);
        if let Some(id_tok) = &o.id_token {
            push(&mut lines, "id_token", id_tok);
        }
        if let Some(tt) = &o.token_type {
            push(&mut lines, "token_type", tt);
        }
        if let Some(te) = &o.token_endpoint {
            push(&mut lines, "token_endpoint", te);
        }
        if !o.extra.is_empty() {
            lines.push(String::new());
            lines.push("extra:".into());
            for (k, v) in &o.extra {
                let val = match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                lines.push(format!("  {k} = {val}"));
            }
        }
    } else {
        lines.push("(no oauth payload)".into());
    }
    lines.join("\n")
}

