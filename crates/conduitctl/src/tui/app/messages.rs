//! Async message handling and post-mutation refresh.

use super::super::forms::OauthFlow;
use super::super::msg::{Msg, RefreshKind};
use super::super::net;
use super::secrets::format_provider_secret_modal;
use super::{App, Mode, PricingPane, Tab};

impl App {
    pub(crate) fn is_stale(&self, gen: u64) -> bool {
        gen != 0 && gen < self.data_gen
    }

    pub fn apply_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Health(r) => {
                self.loading = false;
                match r {
                    Ok(h) => {
                        self.health = Some(h);
                        self.error = None;
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.health = None;
                    }
                }
            }
            Msg::Providers { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.providers = v;
                        // Drop decrypted secrets for providers that no longer exist.
                        self.provider_secrets
                            .retain(|id, _| self.providers.iter().any(|p| p.id == *id));
                        self.clamp_sel(Tab::Providers);
                        if let Mode::RouteWizard(w) = &mut self.mode {
                            w.set_providers(self.providers.clone());
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Routes { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.routes = v;
                        self.clamp_sel(Tab::Routes);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Keys { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.keys = v;
                        self.clamp_sel(Tab::Keys);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Quota {
                snapshots,
                cooldowns,
            } => {
                self.loading = false;
                match snapshots {
                    Ok(v) => self.quota_snapshots = v,
                    // Empty error string is a "don't touch" placeholder from the
                    // quota-probe failure path — surface only real messages.
                    Err(e) if !e.is_empty() => {
                        self.status = format!("quota snapshots: {e}");
                    }
                    Err(_) => {}
                }
                match cooldowns {
                    Ok(v) => self.cooldowns = v,
                    Err(e) if !e.is_empty() => {
                        self.status = format!("cooldowns: {e}");
                    }
                    Err(_) => {}
                }
                if self.tab == Tab::Providers {
                    let n = self.quota_snapshots.len();
                    self.status = if n == 0 {
                        "No quota data yet — OAuth remaining probes after login traffic or press u"
                            .into()
                    } else {
                        format!("Quota updated ({n} snapshot{})", if n == 1 { "" } else { "s" })
                    };
                }
            }
            Msg::ProviderSecret {
                result,
                show_modal,
            } => {
                self.loading = false;
                match result {
                    Ok(view) => {
                        let id = view.provider_id.clone();
                        if show_modal {
                            let title = format!(
                                "Secret · {} ({})",
                                if view.provider_name.is_empty() {
                                    view.provider_id.as_str()
                                } else {
                                    view.provider_name.as_str()
                                },
                                view.secret_kind
                            );
                            let body = format_provider_secret_modal(&view);
                            self.provider_secrets.insert(id, view);
                            self.mode = Mode::SecretReveal {
                                title,
                                secret: body,
                                single_value: false,
                            };
                            self.status =
                                "Secret decrypted — shown in detail & modal (v to hide; others keep)"
                                    .into();
                        } else {
                            self.provider_secrets.insert(id, view);
                            // Keep status quiet on background prefill / post-save refresh.
                            if !matches!(self.mode, Mode::ProviderForm(_)) {
                                self.status = "Secret cache refreshed".into();
                            }
                        }
                    }
                    Err(e) => {
                        // Silent refresh failures should not clobber an open form.
                        if show_modal {
                            self.error = Some(e);
                            self.status = "Failed to decrypt secret".into();
                        } else {
                            self.status = format!("Secret refresh failed: {e}");
                        }
                    }
                }
            }
            Msg::KeySecret(r) => {
                self.loading = false;
                match r {
                    Ok(view) => {
                        let title = if view.name.is_empty() {
                            format!("Key {} — token", view.id)
                        } else {
                            format!("Key {} — token", view.name)
                        };
                        self.mode = Mode::SecretReveal {
                            title,
                            secret: view.key,
                            single_value: true,
                        };
                        self.status = "Key revealed — y/c copy · Esc close".into();
                    }
                    Err(e) => {
                        self.error = Some(e);
                        self.status = "Failed to reveal key".into();
                    }
                }
            }
            Msg::LogsMeta { .. }
            | Msg::LogsPage { .. }
            | Msg::LogsStreamLine { .. }
            | Msg::LogsStreamEvent { .. } => {
                // Stale check for page/meta uses data_gen; stream lines use logs.stream_gen.
                if let Msg::LogsMeta { gen, .. } | Msg::LogsPage { gen, .. } = &msg {
                    if self.is_stale(*gen) {
                        return;
                    }
                }
                if matches!(msg, Msg::LogsPage { .. }) {
                    self.loading = false;
                }
                if let Some(eff) = self.logs.apply_msg(&msg) {
                    use super::super::logs::LogsEffect;
                    match eff {
                        LogsEffect::None => {}
                        LogsEffect::Status(s) => self.status = s,
                        LogsEffect::Reload => self.request_refresh(),
                    }
                }
                self.selected[Tab::Logs.index()] = self.logs.selected;
                self.clamp_sel(Tab::Logs);
                if matches!(msg, Msg::LogsPage { .. } | Msg::LogsStreamLine { .. }) {
                    self.loading = false;
                    self.refresh_filtered();
                }
            }
            Msg::OverviewSummary { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(s) => self.overview_summary = Some(s),
                    Err(e) => {
                        self.status = format!("overview usage unavailable: {e}");
                    }
                }
            }
            Msg::Usage { gen, summary, recent } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                if let Some(summary) = summary {
                    match summary {
                        Ok(s) => self.usage_summary = Some(s),
                        Err(e) => {
                            // Pricing tab preloads usage for detail panes — don't paint
                            // a hard error over pricing if the summary fails.
                            if self.tab == Tab::Usage {
                                self.error = Some(e);
                            } else {
                                self.status = format!("usage summary unavailable: {e}");
                            }
                        }
                    }
                }
                if let Some(recent) = recent {
                    match recent {
                        Ok(page) => {
                            // Pricing/overview preload may send empty page — don't wipe
                            // a list already loaded from the Usage tab.
                            let empty_preload = page.entries.is_empty()
                                && page.total == 0
                                && page.limit == 0
                                && self.tab != Tab::Usage
                                && !self.usage_recent.is_empty();
                            if !empty_preload {
                                self.apply_usage_page(page);
                            }
                            if self.tab == Tab::Usage {
                                self.clamp_sel(Tab::Usage);
                            }
                        }
                        Err(e) => {
                            if self.tab == Tab::Usage {
                                self.error = Some(e);
                            }
                        }
                    }
                }
            }
            Msg::Pricing { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                        self.pricing = v;
                        self.clamp_sel(Tab::Pricing);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::PricingOverrides { gen, result } => {
                if self.is_stale(gen) {
                    return;
                }
                self.loading = false;
                match result {
                    Ok(v) => {
                    // If we just saved a specific row, keep selection on it.
                    let prefer = self
                        .pricing_overrides
                        .get(self.selected_data_index().unwrap_or(usize::MAX))
                        .map(|p| (p.provider_kind.clone(), p.model_id.clone()));
                    self.pricing_overrides = v;
                    // Data replaced → refresh cache before mapping data→view index.
                    self.refresh_filtered();
                    if self.tab == Tab::Pricing && self.pricing_pane == PricingPane::Overrides {
                        if let Some((pk, mid)) = prefer {
                            if let Some(i) = self
                                .pricing_overrides
                                .iter()
                                .position(|p| p.provider_kind == pk && p.model_id == mid)
                            {
                                // Map data index → filtered view index.
                                let filtered = self.filtered_indices();
                                if let Some(view_i) = filtered.iter().position(|&di| di == i) {
                                    self.selected[Tab::Pricing.index()] = view_i;
                                }
                            }
                        }
                        self.clamp_sel(Tab::Pricing);
                    }
                    if self
                        .error
                        .as_ref()
                        .is_some_and(|e| e.contains("404"))
                    {
                        self.error = None;
                    }
                }
                    Err(e) => {
                        // Older daemons / missing route: keep any rows we already
                        // have (e.g. from upsert response) and only warn.
                        if e.contains("404") {
                            if self.pricing_overrides.is_empty() {
                                self.status =
                                "overrides API unavailable (restart conduitd?) — showing merged only"
                                    .into();
                            }
                        } else {
                            self.error = Some(e);
                        }
                    }
                }
            }
            Msg::Mutated {
                ok,
                message,
                refresh,
                secret,
            } => {
                self.loading = false;
                if ok {
                    self.status = message;
                    self.error = None;
                    self.apply_refresh(refresh);
                    if let Some(sec) = secret {
                        self.mode = Mode::SecretReveal {
                            title: "Secret (copy now — shown once)".into(),
                            secret: sec,
                            single_value: true,
                        };
                    }
                } else {
                    self.error = Some(message);
                    self.status = "Error".into();
                }
            }
            Msg::KeyCreated(r) => {
                self.loading = false;
                match r {
                Ok(k) => {
                    self.status = format!("Key {} created", k.id);
                    self.apply_refresh(RefreshKind::Keys);
                    self.mode = Mode::SecretReveal {
                        title: format!("Downstream key {} — copy now (shown once)", k.name),
                        secret: k.key,
                        single_value: true,
                    };
                }
                    Err(e) => {
                        self.error = Some(e);
                    }
                }
            }
            Msg::OauthStarted(r) => {
                self.loading = false;
                match r {
                    Ok(s) => {
                    if let Mode::OauthFlow(f) = &mut self.mode {
                        f.pending_session_id = Some(s.session_id.clone());
                        f.session_status = Some(s.status.clone());
                        f.auth_url = s.auth_url.clone();
                        f.user_code = s.user_code.clone();
                        f.verification_uri = s.verification_uri.clone();
                        f.error = s.error.clone();
                        f.poll_ticks = 0;
                    } else {
                        // entered from browse somehow — open flow
                        let mut f = OauthFlow::new();
                        f.pending_session_id = Some(s.session_id.clone());
                        f.session_status = Some(s.status.clone());
                        f.auth_url = s.auth_url;
                        f.user_code = s.user_code;
                        f.verification_uri = s.verification_uri;
                        self.mode = Mode::OauthFlow(f);
                    }
                    self.status = "OAuth pending — complete in browser".into();
                }
                    Err(e) => {
                        if let Mode::OauthFlow(f) = &mut self.mode {
                            f.error = Some(e.clone());
                        }
                        self.error = Some(e);
                    }
                }
            }
            Msg::OauthPolled(r) => {
                self.loading = false;
                if let Mode::OauthFlow(f) = &mut self.mode {
                    f.poll_inflight = false;
                }
                match r {
                    Ok(s) => {
                        if let Mode::OauthFlow(f) = &mut self.mode {
                            f.session_status = Some(s.status.clone());
                            f.auth_url = s.auth_url.or(f.auth_url.clone());
                            f.user_code = s.user_code.or(f.user_code.clone());
                            f.verification_uri = s.verification_uri.or(f.verification_uri.clone());
                            f.error = s.error.clone();
                            let st = s.status.to_lowercase();
                            if st == "completed" {
                                f.pending_session_id = None;
                                f.result_message = Some(format!(
                                    "Completed. provider_id={:?} email={:?}",
                                    s.provider_id, s.email
                                ));
                                self.status = "OAuth completed — provider list refreshed".into();
                                self.tab = Tab::Providers;
                                self.apply_refresh(RefreshKind::Providers);
                            } else if st == "error" || st == "cancelled" {
                                f.pending_session_id = None;
                                f.result_message =
                                    Some(s.error.unwrap_or_else(|| st.clone()));
                                self.status = format!("OAuth {st}");
                            }
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::OauthCancelled(r) => {
                self.loading = false;
                match r {
                    Ok(()) => {
                        if let Mode::OauthFlow(f) = &mut self.mode {
                            f.pending_session_id = None;
                            f.session_status = Some("cancelled".into());
                            f.result_message = Some("Cancelled".into());
                        }
                        self.status = "OAuth cancelled".into();
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    pub(crate) fn apply_refresh(&mut self, kind: RefreshKind) {
        let gen = self.next_gen();
        match kind {
            RefreshKind::None => {}
            RefreshKind::Overview => net::spawn_overview(self.client.clone(), gen, self.tx.clone()),
            RefreshKind::Providers => {
                net::spawn_providers(self.client.clone(), gen, self.tx.clone());
            }
            RefreshKind::Routes => net::spawn_routes(self.client.clone(), gen, self.tx.clone()),
            RefreshKind::Keys => net::spawn_keys(self.client.clone(), gen, self.tx.clone()),
            RefreshKind::Usage => {
                self.spawn_usage_load(true);
            }
            RefreshKind::Pricing => net::spawn_pricing(self.client.clone(), gen, self.tx.clone()),
            RefreshKind::PricingOverrides => {
                net::spawn_pricing_overrides(self.client.clone(), gen, self.tx.clone());
            }
            RefreshKind::Oauth => {
                // OAuth is not a tab; refresh provider list after login.
                net::spawn_providers(self.client.clone(), gen, self.tx.clone());
            }
            RefreshKind::Logs => {
                self.request_refresh();
            }
            RefreshKind::All => {
                net::spawn_overview(self.client.clone(), gen, self.tx.clone());
            }
        }
    }

}
