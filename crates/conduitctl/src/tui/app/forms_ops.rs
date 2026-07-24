//! Overlay forms: add/edit/delete, submit, focus, OAuth browser helpers.

use crossterm::event::KeyEvent;

use super::super::forms::{
    ConfirmAction, KeyForm, OauthFlow, PricingOverrideForm, ProviderAddChooser, ProviderForm,
    ProviderFormKind, RouteWizard,
};
use super::super::input::InputField;
use super::super::net;
use super::{App, Mode, PricingPane, Tab};

impl App {
    pub(crate) fn confirm_yes(&mut self) {
        let action = if let Mode::Confirm(a) = &self.mode {
            a.clone()
        } else {
            return;
        };
        self.mode = Mode::Browse;
        match action {
            ConfirmAction::DeleteProvider { id, .. } => {
                self.provider_secrets.remove(&id);
                self.status = format!("Deleting provider {id}…");
                net::spawn_delete_provider(self.client.clone(), id, self.tx.clone());
            }
            ConfirmAction::DeleteRoute { id, .. } => {
                self.status = format!("Deleting route {id}…");
                net::spawn_delete_route(self.client.clone(), id, self.tx.clone());
            }
            ConfirmAction::DeleteKey { id, .. } => {
                self.status = format!("Revoking key {id}…");
                net::spawn_delete_key(self.client.clone(), id, self.tx.clone());
            }
            ConfirmAction::SetProviderSecret { id, name } => {
                self.mode = Mode::ProviderForm(ProviderForm::set_secret(&id, &name));
            }
            ConfirmAction::DeletePricingOverride {
                provider_kind,
                model_id,
            } => {
                self.status = format!("Deleting override {provider_kind}/{model_id}…");
                net::spawn_delete_pricing_override(
                    self.client.clone(),
                    provider_kind,
                    model_id,
                    self.tx.clone(),
                );
            }
        }
    }

    pub(crate) fn start_add(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) {
            return;
        }
        self.mode = Mode::Browse;
        match self.tab {
            Tab::Providers => {
                // OAuth is one add method alongside API keys — not a separate tab.
                self.mode = Mode::ProviderAddChooser(ProviderAddChooser::new());
            }
            Tab::Routes => {
                // Always refresh providers so the wizard has a current list.
                let gen = self.next_gen();
                net::spawn_providers(self.client.clone(), gen, self.tx.clone());
                self.mode = Mode::RouteWizard(RouteWizard::create(self.providers.clone()));
            }
            Tab::Keys => {
                self.mode = Mode::KeyForm(KeyForm::create());
            }
            Tab::Pricing => {
                // Prefill from the currently selected pricing row (merged or overrides)
                // so operators can quickly override a model's rates.
                let form = self
                    .selected_data_index()
                    .and_then(|idx| {
                        let rows = match self.pricing_pane {
                            PricingPane::Merged => &self.pricing,
                            PricingPane::Overrides => &self.pricing_overrides,
                        };
                        rows.get(idx).map(PricingOverrideForm::from_row)
                    })
                    .unwrap_or_else(PricingOverrideForm::create);
                if let (Some(pk), Some(mid)) = (
                    form.fields.first().map(|f| f.value.clone()),
                    form.fields.get(1).map(|f| f.value.clone()),
                ) {
                    if !mid.is_empty() {
                        self.status =
                            format!("Override draft from {pk} / {mid} — tweak rates & save");
                    }
                }
                // Form is always for the operator layer. Switch pane *and*
                // rebuild the filtered-index cache before the next Browse
                // frame (Esc / save) — otherwise the cache still holds merged
                // table indices and draw panics on `pricing_overrides[i]`.
                self.pricing_pane = PricingPane::Overrides;
                self.selected[Tab::Pricing.index()] = 0;
                self.refresh_filtered();
                self.mode = Mode::PricingOverrideForm(form);
            }
            _ => {
                self.status = "Add is not available on this tab".into();
            }
        }
    }

    pub(crate) fn confirm_provider_add_chooser(&mut self) {
        let Some(sel) = (if let Mode::ProviderAddChooser(c) = &self.mode {
            Some(c.selected)
        } else {
            None
        }) else {
            return;
        };
        match sel {
            0 => {
                self.mode = Mode::ProviderForm(ProviderForm::create());
            }
            1 => {
                // Claude OAuth
                self.mode = Mode::OauthFlow(OauthFlow::start_new(0, ""));
            }
            2 => {
                self.mode = Mode::OauthFlow(OauthFlow::start_new(1, ""));
            }
            3 => {
                self.mode = Mode::OauthFlow(OauthFlow::start_new(2, ""));
            }
            _ => {
                self.mode = Mode::Browse;
            }
        }
    }

    pub(crate) fn start_provider_oauth_reauth(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) || self.tab != Tab::Providers {
            return;
        }
        self.mode = Mode::Browse;
        let Some(idx) = self.selected_data_index() else {
            self.status = "Select an OAuth provider first".into();
            return;
        };
        let Some(p) = self.providers.get(idx) else {
            return;
        };
        if !ProviderForm::is_oauth_kind_label(&p.kind) {
            self.status = format!(
                "Provider «{}» is not OAuth (kind={}) — use s to set API key",
                p.name, p.kind
            );
            return;
        }
        self.mode = Mode::OauthFlow(OauthFlow::reauth(p));
    }

    pub(crate) fn start_edit(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) {
            return;
        }
        self.mode = Mode::Browse;
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        match self.tab {
            Tab::Providers => {
                if let Some(p) = self.providers.get(idx) {
                    self.mode = Mode::ProviderForm(ProviderForm::edit(p));
                }
            }
            Tab::Routes => {
                if let Some(r) = self.routes.get(idx).cloned() {
                    let gen = self.next_gen();
                    net::spawn_providers(self.client.clone(), gen, self.tx.clone());
                    self.mode =
                        Mode::RouteWizard(RouteWizard::edit(&r, self.providers.clone()));
                }
            }
            Tab::Keys => {
                if let Some(k) = self.keys.get(idx) {
                    self.mode = Mode::KeyForm(KeyForm::edit(k));
                }
            }
            Tab::Pricing => {
                // Edit only in overrides pane; merged table is read-only (detail on the right).
                if self.pricing_pane != PricingPane::Overrides {
                    self.status =
                        "switch to overrides (o) to edit — or press a to add an override".into();
                    return;
                }
                if let Some(p) = self.pricing_overrides.get(idx) {
                    self.mode = Mode::PricingOverrideForm(PricingOverrideForm::edit(p));
                } else {
                    self.status = "no override selected — press a to add".into();
                }
            }
            _ => self.show_detail(),
        }
    }

    pub(crate) fn show_detail(&mut self) {
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        let body = match self.tab {
            Tab::Routes => self.routes.get(idx).map(|r| {
                format!(
                    "id: {}\nalias: {}\nstrategy: {}\nenabled: {}\ntargets: {}",
                    r.id, r.match_alias, r.strategy, r.enabled, r.targets_json
                )
            }),
            // Usage / Pricing: detail is the right pane — no modal.
            Tab::Usage | Tab::Pricing => None,
            _ => None,
        };
        if let Some(body) = body {
            self.mode = Mode::Alert {
                title: "Detail".into(),
                body,
            };
        }
    }

    pub(crate) fn start_delete(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) {
            return;
        }
        self.mode = Mode::Browse;
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        match self.tab {
            Tab::Providers => {
                if let Some(p) = self.providers.get(idx) {
                    self.mode = Mode::Confirm(ConfirmAction::DeleteProvider {
                        id: p.id.clone(),
                        name: p.name.clone(),
                    });
                }
            }
            Tab::Routes => {
                if let Some(r) = self.routes.get(idx) {
                    self.mode = Mode::Confirm(ConfirmAction::DeleteRoute {
                        id: r.id.clone(),
                        alias: r.match_alias.clone(),
                    });
                }
            }
            Tab::Keys => {
                if let Some(k) = self.keys.get(idx) {
                    self.mode = Mode::Confirm(ConfirmAction::DeleteKey {
                        id: k.id.clone(),
                        name: k.name.clone(),
                    });
                }
            }
            Tab::Pricing => {
                if self.pricing_pane != PricingPane::Overrides {
                    self.status =
                        "switch to overrides (o) to delete operator rows — merged table is read-only"
                            .into();
                    return;
                }
                if let Some(p) = self.pricing_overrides.get(idx) {
                    self.mode = Mode::Confirm(ConfirmAction::DeletePricingOverride {
                        provider_kind: p.provider_kind.clone(),
                        model_id: p.model_id.clone(),
                    });
                } else {
                    self.status = "no override selected".into();
                }
            }
            _ => {
                self.status = "Delete is not available on this tab".into();
            }
        }
    }

    pub(crate) fn start_set_secret_filtered(&mut self) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) || self.tab != Tab::Providers {
            return;
        }
        self.mode = Mode::Browse;
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        if let Some(p) = self.providers.get(idx) {
            if ProviderForm::is_oauth_kind_label(&p.kind) {
                // OAuth providers don't use static API keys — re-auth instead.
                self.mode = Mode::OauthFlow(OauthFlow::reauth(p));
                self.status = "OAuth provider — starting re-auth (use o / s)".into();
                return;
            }
            self.mode = Mode::Confirm(ConfirmAction::SetProviderSecret {
                id: p.id.clone(),
                name: p.name.clone(),
            });
        }
    }

    pub(crate) fn focus_delta(&mut self, delta: i32) {
        match &mut self.mode {
            Mode::ProviderForm(f) => {
                let n = f.fields.len() as i32;
                let mut i = f.focus as i32 + delta;
                if i < 0 {
                    i = n - 1;
                }
                if i >= n {
                    i = 0;
                }
                f.focus = i as usize;
            }
            Mode::KeyForm(f) => {
                let n = f.fields.len() as i32;
                let mut i = f.focus as i32 + delta;
                if i < 0 {
                    i = n - 1;
                }
                if i >= n {
                    i = 0;
                }
                f.focus = i as usize;
            }
            Mode::PricingOverrideForm(f) => {
                let n = f.fields.len() as i32;
                let mut i = f.focus as i32 + delta;
                if i < 0 {
                    i = n - 1;
                }
                if i >= n {
                    i = 0;
                }
                f.focus = i as usize;
            }
            Mode::RouteWizard(w) => {
                if w.step == 1 {
                    let mut fi = w.field_in_target as i32 + delta;
                    if fi < 0 {
                        fi = 1;
                    }
                    if fi > 1 {
                        fi = 0;
                    }
                    w.field_in_target = fi as usize;
                }
            }
            Mode::OauthFlow(f) if f.pending_session_id.is_none() => {
                // focus: 0 kind (cycle), 1 name, 2 provider_id
                let mut i = f.focus as i32 + delta;
                if i < 0 {
                    i = 2;
                }
                if i > 2 {
                    i = 0;
                }
                f.focus = i as usize;
            }
            _ => {}
        }
    }

    pub(crate) fn input_code(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::{KeyEventKind, KeyModifiers};
        let key = KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        let field: Option<&mut InputField> = match &mut self.mode {
            Mode::ProviderForm(f) => f.fields.get_mut(f.focus),
            Mode::KeyForm(f) => f.fields.get_mut(f.focus),
            Mode::PricingOverrideForm(f) => f.fields.get_mut(f.focus),
            Mode::RouteWizard(w) => {
                if w.step == 0 {
                    Some(&mut w.match_alias)
                } else if w.step == 1 {
                    if let Some(t) = w.targets.get_mut(w.target_focus) {
                        if w.field_in_target == 0 {
                            Some(&mut t.model_id)
                        } else {
                            Some(&mut t.overrides)
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Mode::OauthFlow(f) if f.pending_session_id.is_none() => match f.focus {
                1 => Some(&mut f.name),
                2 => Some(&mut f.provider_id),
                _ => None,
            },
            _ => None,
        };
        if let Some(field) = field {
            field.handle_key(key);
        } else if let Mode::OauthFlow(f) = &mut self.mode {
            if f.focus == 0
                && f.pending_session_id.is_none()
                && matches!(
                    code,
                    crossterm::event::KeyCode::Char(' ') | crossterm::event::KeyCode::Char('k')
                )
            {
                f.cycle_kind();
            }
        }
    }

    pub(crate) fn submit_form(&mut self) {
        match &self.mode {
            Mode::ProviderForm(f) => match &f.kind {
                ProviderFormKind::Create => match f.to_create_body() {
                    Ok(body) => {
                        self.status = "Creating provider…".into();
                        self.mode = Mode::Browse;
                        net::spawn_create_provider(self.client.clone(), body, self.tx.clone());
                    }
                    Err(e) => {
                        if let Mode::ProviderForm(f) = &mut self.mode {
                            f.error = Some(e);
                        }
                    }
                },
                ProviderFormKind::Edit { id } => match f.to_update_body() {
                    Ok(body) => {
                        let id = id.clone();
                        self.status = "Updating provider…".into();
                        self.mode = Mode::Browse;
                        net::spawn_update_provider(
                            self.client.clone(),
                            id,
                            body,
                            self.tx.clone(),
                        );
                    }
                    Err(e) => {
                        if let Mode::ProviderForm(f) = &mut self.mode {
                            f.error = Some(e);
                        }
                    }
                },
                ProviderFormKind::SetSecret { id, .. } => match f.to_secret_body() {
                    Ok(body) => {
                        let id = id.clone();
                        // Cached plaintext is stale after overwrite.
                        self.provider_secrets.remove(&id);
                        self.status = "Storing secret…".into();
                        self.mode = Mode::Browse;
                        net::spawn_set_secret(self.client.clone(), id, body, self.tx.clone());
                    }
                    Err(e) => {
                        if let Mode::ProviderForm(f) = &mut self.mode {
                            f.error = Some(e);
                        }
                    }
                },
            },
            Mode::KeyForm(f) => {
                if let Some(id) = f.edit_id.clone() {
                    match f.to_update_body() {
                        Ok(body) => {
                            self.status = format!("Updating key {id}…");
                            self.mode = Mode::Browse;
                            net::spawn_update_key(
                                self.client.clone(),
                                id,
                                body,
                                self.tx.clone(),
                            );
                        }
                        Err(e) => {
                            if let Mode::KeyForm(f) = &mut self.mode {
                                f.error = Some(e);
                            }
                        }
                    }
                } else {
                    match f.to_create_body() {
                        Ok(body) => {
                            self.status = "Creating key…".into();
                            self.mode = Mode::Browse;
                            net::spawn_create_key(self.client.clone(), body, self.tx.clone());
                        }
                        Err(e) => {
                            if let Mode::KeyForm(f) = &mut self.mode {
                                f.error = Some(e);
                            }
                        }
                    }
                }
            }
            Mode::RouteWizard(w) => match w.to_body() {
                Ok(body) => {
                    let edit_id = w.edit_id.clone();
                    self.status = "Saving route…".into();
                    self.mode = Mode::Browse;
                    if let Some(id) = edit_id {
                        net::spawn_update_route(self.client.clone(), id, body, self.tx.clone());
                    } else {
                        net::spawn_create_route(self.client.clone(), body, self.tx.clone());
                    }
                }
                Err(e) => {
                    if let Mode::RouteWizard(w) = &mut self.mode {
                        w.error = Some(e);
                    }
                }
            },
            Mode::OauthFlow(f) if f.pending_session_id.is_none() => {
                let kind = f.kind().to_string();
                let name = {
                    let n = f.name.value.trim();
                    if n.is_empty() {
                        None
                    } else {
                        Some(n.to_string())
                    }
                };
                let provider_id = {
                    let p = f.provider_id.value.trim();
                    if p.is_empty() {
                        None
                    } else {
                        Some(p.to_string())
                    }
                };
                self.status = format!("Starting OAuth ({kind})…");
                if let Mode::OauthFlow(f) = &mut self.mode {
                    f.error = None;
                    f.result_message = None;
                }
                net::spawn_oauth_start(self.client.clone(), kind, name, provider_id, self.tx.clone());
            }
            Mode::PricingOverrideForm(f) => match f.to_body() {
                Ok(body) => {
                    self.status = format!(
                        "Saving override {} / {}…",
                        body.provider_kind, body.model_id
                    );
                    self.mode = Mode::Browse;
                    self.pricing_pane = PricingPane::Overrides;
                    // Reset selection so the new/updated row can be focused after save.
                    self.selected[Tab::Pricing.index()] = 0;
                    // Stash identity for selection restore (via temporary single-row list).
                    self.pricing_overrides = vec![crate::dto::PricingView {
                        provider_kind: body.provider_kind.clone(),
                        model_id: body.model_id.clone(),
                        input_per_mtok: body.input_per_mtok,
                        output_per_mtok: body.output_per_mtok,
                        cache_read_per_mtok: body.cache_read_per_mtok,
                        cache_write_per_mtok: body.cache_write_per_mtok,
                        reasoning_per_mtok: body.reasoning_per_mtok,
                        effective_from: String::new(),
                    }];
                    // Pane + backing list both changed; refresh before the next
                    // draw frame (network response may arrive later).
                    self.refresh_filtered();
                    net::spawn_upsert_pricing_override(
                        self.client.clone(),
                        body,
                        self.tx.clone(),
                    );
                }
                Err(e) => {
                    if let Mode::PricingOverrideForm(f) = &mut self.mode {
                        f.error = Some(e);
                    }
                }
            },
            _ => {}
        }
    }

    pub(crate) fn open_browser(&mut self) {
        let url = match &self.mode {
            Mode::OauthFlow(f) => f.auth_url.clone().or_else(|| f.verification_uri.clone()),
            _ => None,
        };
        if let Some(url) = url {
            match net::open_browser(&url) {
                Ok(()) => self.status = format!("Opened browser: {url}"),
                Err(e) => self.status = format!("Could not open browser: {e} — URL: {url}"),
            }
        } else {
            self.status = "No URL to open".into();
        }
    }

    pub(crate) fn oauth_cancel(&mut self) {
        if let Mode::OauthFlow(f) = &self.mode {
            if let Some(sid) = f.pending_session_id.clone() {
                net::spawn_oauth_cancel(self.client.clone(), sid, self.tx.clone());
                self.status = "Cancelling OAuth session…".into();
            }
        }
    }

    pub(crate) fn oauth_refresh_selected(&mut self) {
        if self.tab != Tab::Providers {
            return;
        }
        let Some(idx) = self.selected_data_index() else {
            return;
        };
        if let Some(p) = self.providers.get(idx) {
            if !ProviderForm::is_oauth_kind_label(&p.kind) {
                self.status = "Token refresh only applies to OAuth providers".into();
                return;
            }
            self.status = format!("Refreshing OAuth for {}…", p.id);
            net::spawn_oauth_refresh(self.client.clone(), p.id.clone(), self.tx.clone());
        }
    }


}
