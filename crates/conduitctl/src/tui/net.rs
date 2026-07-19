//! Spawn async console API calls and forward results as [`Msg`].

use tokio::sync::mpsc::UnboundedSender;

use crate::console_client::ConsoleClient;
use crate::dto::{
    CreateKeyBody, CreateProviderBody, CreateRouteBody, SetSecretBody, UpdateProviderBody,
    UsageListResponse,
};

use super::app::USAGE_PAGE_SIZE;
use super::msg::{Msg, RefreshKind};

pub fn spawn_load_tab(client: ConsoleClient, tab_idx: usize, tx: UnboundedSender<Msg>) {
    match tab_idx {
        0 => spawn_overview(client, tx),
        1 => spawn_providers(client, tx),
        2 => spawn_routes(client, tx),
        3 => spawn_keys(client, tx),
        4 => spawn_usage(
            client,
            None,
            0,
            USAGE_PAGE_SIZE,
            None,
            Some("date".into()),
            tx,
        ),
        5 => spawn_pricing(client, tx),
        _ => {}
    }
}

fn empty_usage_page() -> UsageListResponse {
    UsageListResponse {
        entries: Vec::new(),
        total: 0,
        limit: 0,
        offset: 0,
        sort: None,
    }
}

pub fn spawn_overview(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let health = client.health().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Health(health));
        // Counts for metric strip
        let providers = client.list_providers_typed().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Providers(providers));
        let routes = client.list_routes_typed().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Routes(routes));
        let keys = client.list_keys_typed().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Keys(keys));
        // Current-month spend / sparkline / top models (defaults to UTC month on server).
        let summary = client
            .usage_summary_typed(None)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Msg::Usage {
            summary: Some(summary),
            recent: Some(Ok(empty_usage_page())),
        });
    });
}

pub fn spawn_providers(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.list_providers_typed().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Providers(r));
        // Probe OAuth remaining (Claude/Codex usage + Grok billing), then load snapshots + cooldowns.
        let _ = client.refresh_all_quotas().await;
        spawn_quota_state(client, tx).await;
    });
}

/// Decrypt provider secret for detail pane.
pub fn spawn_provider_secret(
    client: ConsoleClient,
    provider_id: String,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let r = client
            .get_provider_secret(&provider_id)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Msg::ProviderSecret(r));
    });
}

/// Probe selected (or all) OAuth remaining, then reload snapshots.
pub fn spawn_quota_refresh(
    client: ConsoleClient,
    provider_id: Option<String>,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        if let Some(id) = provider_id {
            let _ = client.refresh_quota(&id).await;
        } else {
            let _ = client.refresh_all_quotas().await;
        }
        spawn_quota_state(client, tx).await;
    });
}

async fn spawn_quota_state(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    let snapshots = client
        .list_quota_snapshots()
        .await
        .map(|r| r.entries)
        .map_err(|e| e.to_string());
    let cooldowns = client
        .list_cooldowns()
        .await
        .map(|r| r.entries)
        .map_err(|e| e.to_string());
    let _ = tx.send(Msg::Quota {
        snapshots,
        cooldowns,
    });
}

pub fn spawn_routes(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.list_routes_typed().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Routes(r));
    });
}

pub fn spawn_keys(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.list_keys_typed().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Keys(r));
    });
}

/// Full Usage tab load: period summary + one recent page + pricing/keys for detail panes.
pub fn spawn_usage(
    client: ConsoleClient,
    period: Option<String>,
    offset: usize,
    limit: usize,
    q: Option<String>,
    sort: Option<String>,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let summary = client
            .usage_summary_typed(period.as_deref())
            .await
            .map_err(|e| e.to_string());
        let recent = client
            .list_usage_page(
                limit,
                offset,
                period.as_deref(),
                q.as_deref(),
                sort.as_deref(),
            )
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Msg::Usage {
            summary: Some(summary),
            recent: Some(recent),
        });
        // Pricing rates used by usage detail pane (cache read/write unit prices).
        let pricing = client.list_pricing_typed().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Pricing(pricing));
        // Key names for Usage → by key rollup labels.
        let keys = client.list_keys_typed().await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::Keys(keys));
    });
}

/// List page only (filter / sort / page flip) — skips summary + pricing.
pub fn spawn_usage_page(
    client: ConsoleClient,
    period: Option<String>,
    offset: usize,
    limit: usize,
    q: Option<String>,
    sort: Option<String>,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let recent = client
            .list_usage_page(
                limit,
                offset,
                period.as_deref(),
                q.as_deref(),
                sort.as_deref(),
            )
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Msg::Usage {
            summary: None,
            recent: Some(recent),
        });
    });
}

pub fn spawn_pricing(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        // Fetch in parallel so Overrides + Usage show together on first paint
        // (sequential order left usage empty until the very end).
        let c1 = client.clone();
        let c2 = client.clone();
        let c3 = client.clone();
        let (overrides, pricing, summary) = tokio::join!(
            async move { c1.list_pricing_overrides().await.map_err(|e| e.to_string()) },
            async move { c2.list_pricing_typed().await.map_err(|e| e.to_string()) },
            async move { c3.usage_summary_typed(None).await.map_err(|e| e.to_string()) },
        );
        // Usage first so detail panes have spend as soon as the list appears.
        let _ = tx.send(Msg::Usage {
            summary: Some(summary),
            recent: Some(Ok(empty_usage_page())),
        });
        let _ = tx.send(Msg::PricingOverrides(overrides));
        let _ = tx.send(Msg::Pricing(pricing));
    });
}

/// Lightweight: only period usage summary (for Pricing detail without full reload).
pub fn spawn_usage_summary_only(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let summary = client
            .usage_summary_typed(None)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Msg::Usage {
            summary: Some(summary),
            recent: None,
        });
    });
}

pub fn spawn_pricing_overrides(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let o = client
            .list_pricing_overrides()
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Msg::PricingOverrides(o));
    });
}

fn overrides_from_mutation_body(v: &serde_json::Value) -> Option<Vec<crate::dto::PricingView>> {
    let arr = v.get("overrides")?;
    serde_json::from_value(arr.clone()).ok()
}

pub fn spawn_upsert_pricing_override(
    client: ConsoleClient,
    body: crate::dto::UpsertPricingOverrideBody,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let pk = body.provider_kind.clone();
        let mid = body.model_id.clone();
        match client.upsert_pricing_override(&body).await {
            Ok(v) => {
                // Immediate list update (don't wait for full refresh).
                if let Some(rows) = overrides_from_mutation_body(&v) {
                    let _ = tx.send(Msg::PricingOverrides(Ok(rows)));
                }
                let _ = tx.send(Msg::Mutated {
                    ok: true,
                    message: format!("Override saved: {pk} / {mid}"),
                    refresh: RefreshKind::Pricing,
                    secret: None,
                });
            }
            Err(e) => {
                let _ = tx.send(Msg::Mutated {
                    ok: false,
                    message: e.to_string(),
                    refresh: RefreshKind::None,
                    secret: None,
                });
            }
        }
    });
}

pub fn spawn_delete_pricing_override(
    client: ConsoleClient,
    provider_kind: String,
    model_id: String,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        match client
            .delete_pricing_override(&provider_kind, &model_id)
            .await
        {
            Ok(v) => {
                if let Some(rows) = overrides_from_mutation_body(&v) {
                    let _ = tx.send(Msg::PricingOverrides(Ok(rows)));
                }
                let _ = tx.send(Msg::Mutated {
                    ok: true,
                    message: format!("Override deleted: {provider_kind} / {model_id}"),
                    refresh: RefreshKind::Pricing,
                    secret: None,
                });
            }
            Err(e) => {
                let _ = tx.send(Msg::Mutated {
                    ok: false,
                    message: e.to_string(),
                    refresh: RefreshKind::None,
                    secret: None,
                });
            }
        }
    });
}

pub fn spawn_create_provider(
    client: ConsoleClient,
    body: CreateProviderBody,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let r = client.create_provider(&body).await;
        let msg = match r {
            Ok(v) => {
                let id = v
                    .get("id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string();
                Msg::Mutated {
                    ok: true,
                    message: format!("Provider {id} created"),
                    refresh: RefreshKind::Providers,
                    secret: None,
                }
            }
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_update_provider(
    client: ConsoleClient,
    id: String,
    body: UpdateProviderBody,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let r = client.update_provider(&id, &body).await;
        let msg = match r {
            Ok(_) => Msg::Mutated {
                ok: true,
                message: format!("Provider {id} updated"),
                refresh: RefreshKind::Providers,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_set_secret(
    client: ConsoleClient,
    id: String,
    body: SetSecretBody,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let r = client.set_provider_secret(&id, &body).await;
        let msg = match r {
            Ok(()) => Msg::Mutated {
                ok: true,
                message: format!("Secret stored for provider {id}"),
                refresh: RefreshKind::Providers,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_delete_provider(client: ConsoleClient, id: String, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.delete_provider(&id).await;
        let msg = match r {
            Ok(()) => Msg::Mutated {
                ok: true,
                message: format!("Provider {id} deleted"),
                refresh: RefreshKind::Providers,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_create_route(
    client: ConsoleClient,
    body: CreateRouteBody,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let r = client.create_route(&body).await;
        let msg = match r {
            Ok(v) => {
                let id = v
                    .get("id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string();
                Msg::Mutated {
                    ok: true,
                    message: format!("Route {id} created (alias={})", body.match_alias),
                    refresh: RefreshKind::Routes,
                    secret: None,
                }
            }
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_update_route(
    client: ConsoleClient,
    id: String,
    body: CreateRouteBody,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let r = client.update_route(&id, &body).await;
        let msg = match r {
            Ok(_) => Msg::Mutated {
                ok: true,
                message: format!("Route {id} updated"),
                refresh: RefreshKind::Routes,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_delete_route(client: ConsoleClient, id: String, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.delete_route(&id).await;
        let msg = match r {
            Ok(()) => Msg::Mutated {
                ok: true,
                message: format!("Route {id} deleted"),
                refresh: RefreshKind::Routes,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_create_key(client: ConsoleClient, body: CreateKeyBody, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.create_key(&body).await;
        let _ = tx.send(Msg::KeyCreated(r.map_err(|e| e.to_string())));
    });
}

pub fn spawn_delete_key(client: ConsoleClient, id: String, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.delete_key(&id).await;
        let msg = match r {
            Ok(()) => Msg::Mutated {
                ok: true,
                message: format!("Key {id} revoked"),
                refresh: RefreshKind::Keys,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_pricing_reload(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.reload_pricing().await;
        let msg = match r {
            Ok(_) => Msg::Mutated {
                ok: true,
                message: "Pricing reloaded".into(),
                refresh: RefreshKind::Pricing,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_pricing_sync(client: ConsoleClient, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.sync_pricing(None).await;
        let msg = match r {
            Ok(v) => Msg::Mutated {
                ok: true,
                message: format!("Pricing synced: {v}"),
                refresh: RefreshKind::Pricing,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn spawn_oauth_start(
    client: ConsoleClient,
    kind: String,
    name: Option<String>,
    provider_id: Option<String>,
    tx: UnboundedSender<Msg>,
) {
    tokio::spawn(async move {
        let mut body = serde_json::json!({});
        if let Some(n) = name {
            if !n.is_empty() {
                body["name"] = serde_json::json!(n);
            }
        }
        if let Some(p) = provider_id {
            if !p.is_empty() {
                body["provider_id"] = serde_json::json!(p);
            }
        }
        let r = client
            .start_oauth_typed(&kind, &body)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Msg::OauthStarted(r));
    });
}

pub fn spawn_oauth_poll(client: ConsoleClient, session_id: String, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client
            .oauth_session_typed(&session_id)
            .await
            .map_err(|e| e.to_string());
        let _ = tx.send(Msg::OauthPolled(r));
    });
}

pub fn spawn_oauth_cancel(client: ConsoleClient, session_id: String, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.cancel_oauth(&session_id).await.map_err(|e| e.to_string());
        let _ = tx.send(Msg::OauthCancelled(r));
    });
}

pub fn spawn_oauth_refresh(client: ConsoleClient, provider_id: String, tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let r = client.refresh_oauth(&provider_id).await;
        let msg = match r {
            Ok(v) => Msg::Mutated {
                ok: true,
                message: format!("OAuth refresh ok: {v}"),
                refresh: RefreshKind::Providers,
                secret: None,
            },
            Err(e) => Msg::Mutated {
                ok: false,
                message: e.to_string(),
                refresh: RefreshKind::None,
                secret: None,
            },
        };
        let _ = tx.send(msg);
    });
}

pub fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = url;
        Err("open browser not supported on this platform".into())
    }
}
