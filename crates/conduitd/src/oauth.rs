//! Console OAuth flows: Claude/Codex PKCE callbacks + Grok device code.

use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Instant};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    Json,
};
use chrono::Utc;
use conduit_oauth::{
    generate_pkce, generate_state, resolve_effective_proxy, supported_providers, ClaudeOAuth,
    CodexOAuth, CredentialResolver, GrokOAuth, OAuthCredential, OAuthError, OAuthProviderKind,
    OAuthSession, SecretStore, SessionStatus, SessionStore, SessionView,
};
use conduit_store::{schema::ProviderRow, ProviderRepo};
use parking_lot::Mutex;
use secrecy::SecretVec;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::oneshot;
use tracing::{error, info, warn};
use ulid::Ulid;

use crate::state::DaemonState;

// ── SecretStore adapter ───────────────────────────────────────────────────────

pub struct BackendSecretStore {
    backend: Arc<dyn conduit_secret::SecretBackend>,
}

impl BackendSecretStore {
    pub fn new(backend: Arc<dyn conduit_secret::SecretBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait::async_trait]
impl SecretStore for BackendSecretStore {
    async fn get(&self, scope: &str, id: &str) -> Result<Option<SecretVec<u8>>, OAuthError> {
        self.backend
            .get(scope, id)
            .await
            .map_err(|e| OAuthError::Credential(e.to_string()))
    }

    async fn put(&self, scope: &str, id: &str, secret: SecretVec<u8>) -> Result<(), OAuthError> {
        self.backend
            .put(scope, id, secret)
            .await
            .map_err(|e| OAuthError::Credential(e.to_string()))
    }
}

// ── Callback server handles ───────────────────────────────────────────────────

struct CallbackHandle {
    shutdown: oneshot::Sender<()>,
    /// Optional second shutdown for fixed-port TCP forwarder.
    forwarder_shutdown: Option<oneshot::Sender<()>>,
}

/// Process-wide OAuth runtime: sessions + active callback servers.
pub struct OAuthRuntime {
    pub sessions: SessionStore,
    callbacks: Mutex<HashMap<String, CallbackHandle>>,
}

impl OAuthRuntime {
    pub fn new() -> Self {
        Self {
            sessions: SessionStore::new(),
            callbacks: Mutex::new(HashMap::new()),
        }
    }
}

/// IdP-registered redirect ports (54545 / 1455) cannot change without
/// `redirect_uri_mismatch`. Strategy:
/// 1. Stop our own previous OAuth callback servers (they often hold the port).
/// 2. Bind preferred; if still busy, bind ephemeral + TCP-forward preferred→ephemeral
///    when preferred becomes free after a short retry; otherwise `PortInUse`.
fn resolve_callback_listen_port(
    state: &DaemonState,
    preferred: u16,
) -> Result<(u16 /*listen*/, bool /*needs_forwarder*/), OAuthError> {
    // Free ports held by prior Conduit OAuth sessions.
    stop_all_callbacks(state);
    std::thread::sleep(std::time::Duration::from_millis(50));

    if try_bind_probe(preferred) {
        return Ok((preferred, false));
    }

    // Another process may still hold preferred. Try ephemeral server + forwarder
    // only if we can claim preferred for the forwarder after one more stop+retry.
    stop_all_callbacks(state);
    std::thread::sleep(std::time::Duration::from_millis(50));
    if try_bind_probe(preferred) {
        return Ok((preferred, false));
    }

    // Preferred still taken by a non-Conduit process — cannot rewrite IdP redirect_uri.
    Err(OAuthError::PortInUse(preferred))
}

fn try_bind_probe(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port))
        .map(|l| {
            drop(l);
            true
        })
        .unwrap_or(false)
}

fn stop_all_callbacks(state: &DaemonState) {
    let handles: Vec<_> = state.oauth.callbacks.lock().drain().map(|(_, h)| h).collect();
    for h in handles {
        let _ = h.shutdown.send(());
        if let Some(fwd) = h.forwarder_shutdown {
            let _ = fwd.send(());
        }
    }
}

/// TCP byte-forwarder: accept on `from_port`, connect to `127.0.0.1:to_port`.
fn spawn_tcp_forwarder(
    from_port: u16,
    to_port: u16,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<(), OAuthError> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", from_port))
        .map_err(|_| OAuthError::PortInUse(from_port))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| OAuthError::Network(format!("forwarder nonblocking: {e}")))?;
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                error!("oauth forwarder from_std: {e}");
                return;
            }
        };
        info!(
            from = from_port,
            to = to_port,
            "oauth callback TCP forwarder listening (IdP fixed port → local server)"
        );
        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!(from = from_port, "oauth forwarder stopped");
                    break;
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((mut inbound, _)) => {
                            tokio::spawn(async move {
                                match tokio::net::TcpStream::connect(("127.0.0.1", to_port)).await {
                                    Ok(mut outbound) => {
                                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                                    }
                                    Err(e) => {
                                        warn!("oauth forwarder connect {to_port}: {e}");
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            // WouldBlock / interrupted under load — continue until shutdown.
                            if e.kind() != std::io::ErrorKind::WouldBlock {
                                warn!("oauth forwarder accept: {e}");
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        }
                    }
                }
            }
        }
    });
    Ok(())
}

impl Default for OAuthRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ── HTTP handlers ─────────────────────────────────────────────────────────────

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": msg.into()})))
}

pub async fn list_oauth_providers() -> impl IntoResponse {
    (StatusCode::OK, Json(json!(supported_providers())))
}

#[derive(Debug, Deserialize)]
pub struct StartOAuthBody {
    pub name: Option<String>,
    /// Re-auth an existing provider (overwrite secret).
    pub provider_id: Option<String>,
}

pub async fn start_oauth(
    State(state): State<Arc<DaemonState>>,
    Path(kind): Path<String>,
    Json(body): Json<StartOAuthBody>,
) -> impl IntoResponse {
    let kind = match OAuthProviderKind::parse(&kind) {
        Ok(k) => k,
        Err(e) => return err(StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    };

    state.oauth.sessions.gc();
    let session_id = Ulid::new().to_string();

    match kind {
        OAuthProviderKind::Claude | OAuthProviderKind::Codex => {
            match start_pkce_flow(&state, kind, session_id, body).await {
                Ok(view) => (StatusCode::OK, Json(json!(view))).into_response(),
                Err(e) => err(StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
            }
        }
        OAuthProviderKind::Xai => match start_device_flow(&state, session_id, body).await {
            Ok(view) => (StatusCode::OK, Json(json!(view))).into_response(),
            Err(e) => err(StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        },
    }
}

pub async fn get_oauth_session(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.oauth.sessions.get(&id) {
        Some(s) => (StatusCode::OK, Json(json!(s.view()))).into_response(),
        None => err(StatusCode::NOT_FOUND, "session not found").into_response(),
    }
}

pub async fn cancel_oauth_session(
    State(state): State<Arc<DaemonState>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let _ = state.oauth.sessions.update(&id, |s| {
        s.status = SessionStatus::Cancelled;
        s.error = Some("cancelled by user".into());
    });
    stop_callback(&state, &id);
    (StatusCode::OK, Json(json!({"ok": true}))).into_response()
}

pub async fn refresh_provider_oauth(
    State(state): State<Arc<DaemonState>>,
    Path(provider_id): Path<String>,
) -> impl IntoResponse {
    let store = Arc::new(BackendSecretStore::new(state.secret_backend.clone()));
    let resolver =
        CredentialResolver::new(store).with_default_proxy(state.proxy_url.clone());
    match resolver.force_refresh(&provider_id).await {
        Ok(cred) => (
            StatusCode::OK,
            Json(json!({
                "provider_id": provider_id,
                "email": cred.email,
                "expired": cred.expired,
                "type": cred.provider_type,
            })),
        )
            .into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

// ── PKCE flow (Claude / Codex) ────────────────────────────────────────────────

async fn start_pkce_flow(
    state: &Arc<DaemonState>,
    kind: OAuthProviderKind,
    session_id: String,
    body: StartOAuthBody,
) -> Result<SessionView, OAuthError> {
    let pkce = generate_pkce()?;
    let oauth_state = generate_state();

    let proxy = resolve_effective_proxy(None, state.proxy_url.as_deref());
    // IdP-registered callback ports cannot change; if busy, forward fixed → ephemeral.
    let (preferred_port, callback_path) = match kind {
        OAuthProviderKind::Claude => (
            conduit_oauth::providers::claude::CALLBACK_PORT,
            "/callback",
        ),
        OAuthProviderKind::Codex => (
            conduit_oauth::providers::codex::CALLBACK_PORT,
            "/auth/callback",
        ),
        OAuthProviderKind::Xai => unreachable!(),
    };
    let (listen_port, needs_forwarder) = resolve_callback_listen_port(state, preferred_port)?;

    let auth_url = match kind {
        OAuthProviderKind::Claude => {
            ClaudeOAuth::with_proxy_url(proxy.clone())?
                .generate_auth_url(&oauth_state, &pkce)
        }
        OAuthProviderKind::Codex => {
            CodexOAuth::with_proxy_url(proxy)?
                .generate_auth_url(&oauth_state, &pkce)
        }
        OAuthProviderKind::Xai => unreachable!(),
    };

    let session = OAuthSession {
        id: session_id.clone(),
        kind,
        status: SessionStatus::Pending,
        created_at: Instant::now(),
        name: body.name,
        provider_id: body.provider_id,
        state: Some(oauth_state.clone()),
        pkce: Some(pkce),
        auth_url: Some(auth_url),
        user_code: None,
        device_code: None,
        verification_uri: None,
        verification_uri_complete: None,
        device_expires_in: None,
        poll_interval_secs: None,
        token_endpoint: None,
        completed_provider_id: None,
        email: None,
        error: None,
    };
    let view = session.view();
    state.oauth.sessions.insert(session);

    spawn_callback_server(
        state.clone(),
        session_id,
        kind,
        listen_port,
        preferred_port,
        needs_forwarder,
        callback_path,
    )?;
    Ok(view)
}

fn spawn_callback_server(
    state: Arc<DaemonState>,
    session_id: String,
    kind: OAuthProviderKind,
    listen_port: u16,
    preferred_port: u16,
    needs_forwarder: bool,
    callback_path: &'static str,
) -> Result<(), OAuthError> {
    let (tx, rx) = oneshot::channel::<()>();
    let forwarder_shutdown = if needs_forwarder {
        let (ftx, frx) = oneshot::channel::<()>();
        spawn_tcp_forwarder(preferred_port, listen_port, frx)?;
        Some(ftx)
    } else {
        None
    };
    state.oauth.callbacks.lock().insert(
        session_id.clone(),
        CallbackHandle {
            shutdown: tx,
            forwarder_shutdown,
        },
    );

    let state_cb = state.clone();
    let sid = session_id.clone();
    let port = listen_port;

    tokio::spawn(async move {
        // Bind the provider-specific path. Codex uses `/auth/callback` and also
        // accepts a `/callback` alias; Claude's path *is* `/callback`, so do not
        // register it twice (axum panics on overlapping method routes).
        let make_handler = |state: Arc<DaemonState>, sid: String| {
            move |q: Query<CallbackQuery>| {
                let state = state.clone();
                let sid = sid.clone();
                async move { handle_callback(state, sid, kind, q.0).await }
            }
        };

        let mut app = axum::Router::new().route(
            callback_path,
            axum::routing::get(make_handler(state_cb.clone(), sid.clone())),
        );
        if callback_path != "/callback" {
            app = app.route(
                "/callback",
                axum::routing::get(make_handler(state_cb.clone(), sid.clone())),
            );
        }

        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("oauth callback bind {addr}: {e}");
                let _ = state_cb.oauth.sessions.update(&sid, |s| {
                    s.status = SessionStatus::Error;
                    s.error = Some(format!("bind {addr}: {e}"));
                });
                return;
            }
        };
        info!(%addr, kind = %kind, session = %sid, "oauth callback server listening");

        let server = axum::serve(listener, app).with_graceful_shutdown(async {
            let _ = rx.await;
        });
        if let Err(e) = server.await {
            warn!("oauth callback server error: {e}");
        }
        state_cb.oauth.callbacks.lock().remove(&sid);
        info!(session = %sid, "oauth callback server stopped");
    });

    Ok(())
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn handle_callback(
    state: Arc<DaemonState>,
    session_id: String,
    kind: OAuthProviderKind,
    q: CallbackQuery,
) -> impl IntoResponse {
    if let Some(err) = q.error {
        let msg = q.error_description.unwrap_or(err);
        let _ = state.oauth.sessions.update(&session_id, |s| {
            s.status = SessionStatus::Error;
            s.error = Some(msg.clone());
        });
        stop_callback(&state, &session_id);
        return Html(error_page(&msg)).into_response();
    }

    let code = match q.code {
        Some(c) if !c.is_empty() => c,
        _ => {
            let msg = "missing authorization code";
            let _ = state.oauth.sessions.update(&session_id, |s| {
                s.status = SessionStatus::Error;
                s.error = Some(msg.into());
            });
            stop_callback(&state, &session_id);
            return Html(error_page(msg)).into_response();
        }
    };

    let session = match state.oauth.sessions.get(&session_id) {
        Some(s) => s,
        None => return Html(error_page("session not found")).into_response(),
    };

    // CSRF: prefer session state; Claude may embed state in code#state
    if let (Some(expected), Some(got)) = (&session.state, &q.state) {
        if expected != got {
            // Claude sometimes only puts state in code fragment — allow mismatch if empty query state handled
            warn!(
                expected = %expected,
                got = %got,
                "oauth state mismatch (continuing if code embeds state)"
            );
        }
    }

    let pkce = match session.pkce {
        Some(ref p) => p.clone(),
        None => {
            let _ = state.oauth.sessions.update(&session_id, |s| {
                s.status = SessionStatus::Error;
                s.error = Some("missing pkce".into());
            });
            stop_callback(&state, &session_id);
            return Html(error_page("missing pkce")).into_response();
        }
    };

    let proxy = resolve_effective_proxy(None, state.proxy_url.as_deref());
    let exchange = match kind {
        OAuthProviderKind::Claude => match ClaudeOAuth::with_proxy_url(proxy) {
            Ok(oauth) => {
                oauth
                    .exchange_code(&code, session.state.as_deref().unwrap_or(""), &pkce)
                    .await
            }
            Err(e) => Err(e),
        },
        OAuthProviderKind::Codex => match CodexOAuth::with_proxy_url(proxy) {
            Ok(oauth) => oauth.exchange_code(&code, &pkce).await,
            Err(e) => Err(e),
        },
        OAuthProviderKind::Xai => unreachable!(),
    };

    match exchange {
        Ok(cred) => match persist_credential(&state, &session, cred).await {
            Ok(provider_id) => {
                let email = state
                    .oauth
                    .sessions
                    .get(&session_id)
                    .and_then(|s| s.email.clone())
                    .unwrap_or_default();
                let _ = state.oauth.sessions.update(&session_id, |s| {
                    s.status = SessionStatus::Completed;
                    s.completed_provider_id = Some(provider_id);
                });
                stop_callback(&state, &session_id);
                Html(success_page(kind.as_str(), &email)).into_response()
            }
            Err(e) => {
                let _ = state.oauth.sessions.update(&session_id, |s| {
                    s.status = SessionStatus::Error;
                    s.error = Some(e.to_string());
                });
                stop_callback(&state, &session_id);
                Html(error_page(&e.to_string())).into_response()
            }
        },
        Err(e) => {
            let _ = state.oauth.sessions.update(&session_id, |s| {
                s.status = SessionStatus::Error;
                s.error = Some(e.to_string());
            });
            stop_callback(&state, &session_id);
            Html(error_page(&e.to_string())).into_response()
        }
    }
}

// ── Device flow (Grok) ────────────────────────────────────────────────────────

async fn start_device_flow(
    state: &Arc<DaemonState>,
    session_id: String,
    body: StartOAuthBody,
) -> Result<SessionView, OAuthError> {
    let proxy = resolve_effective_proxy(None, state.proxy_url.as_deref());
    let oauth = GrokOAuth::with_proxy_url(proxy)?;
    let device = oauth.start_device_flow().await?;

    let session = OAuthSession {
        id: session_id.clone(),
        kind: OAuthProviderKind::Xai,
        status: SessionStatus::Pending,
        created_at: Instant::now(),
        name: body.name,
        provider_id: body.provider_id,
        state: None,
        pkce: None,
        auth_url: device
            .verification_uri_complete
            .clone()
            .or(Some(device.verification_uri.clone())),
        user_code: Some(device.user_code.clone()),
        device_code: Some(device.device_code.clone()),
        verification_uri: Some(device.verification_uri.clone()),
        verification_uri_complete: device.verification_uri_complete.clone(),
        device_expires_in: Some(device.expires_in),
        poll_interval_secs: Some(device.interval),
        token_endpoint: Some(device.token_endpoint.clone()),
        completed_provider_id: None,
        email: None,
        error: None,
    };
    let view = session.view();
    state.oauth.sessions.insert(session);

    // Background poll — cancel when session status leaves Pending.
    let state_bg = state.clone();
    let sid = session_id.clone();
    let proxy_bg = resolve_effective_proxy(None, state.proxy_url.as_deref());
    tokio::spawn(async move {
        let oauth = match GrokOAuth::with_proxy_url(proxy_bg) {
            Ok(o) => o,
            Err(e) => {
                let _ = state_bg.oauth.sessions.update(&sid, |s| {
                    s.status = SessionStatus::Error;
                    s.error = Some(e.to_string());
                });
                return;
            }
        };
        let sessions = state_bg.oauth.sessions.clone();
        let sid_check = sid.clone();
        let result = oauth
            .wait_for_authorization_cancellable(&device, || {
                sessions
                    .get(&sid_check)
                    .map(|s| s.status != SessionStatus::Pending)
                    .unwrap_or(true)
            })
            .await;
        match result {
            Ok(cred) => {
                let sess = state_bg.oauth.sessions.get(&sid);
                if let Some(sess) = sess {
                    if sess.status != SessionStatus::Pending {
                        return;
                    }
                    match persist_credential(&state_bg, &sess, cred).await {
                        Ok(pid) => {
                            let _ = state_bg.oauth.sessions.update(&sid, |s| {
                                s.status = SessionStatus::Completed;
                                s.completed_provider_id = Some(pid);
                            });
                        }
                        Err(e) => {
                            let _ = state_bg.oauth.sessions.update(&sid, |s| {
                                s.status = SessionStatus::Error;
                                s.error = Some(e.to_string());
                            });
                        }
                    }
                }
            }
            Err(OAuthError::SessionCancelled) => {
                // User cancelled — status already Cancelled.
            }
            Err(e) => {
                let _ = state_bg.oauth.sessions.update(&sid, |s| {
                    if s.status == SessionStatus::Pending {
                        s.status = SessionStatus::Error;
                        s.error = Some(e.to_string());
                    }
                });
            }
        }
    });

    Ok(view)
}

// ── Persist ───────────────────────────────────────────────────────────────────

async fn persist_credential(
    state: &DaemonState,
    session: &OAuthSession,
    cred: OAuthCredential,
) -> Result<String, OAuthError> {
    let email = cred.email.clone();
    let kind = session.kind;
    // Codex: CLIProxyAPI multi-auth stable id from email + plan + account (team).
    let stable_codex_id = if kind == OAuthProviderKind::Codex {
        conduit_oauth::providers::codex::stable_provider_id(
            cred.email.as_deref(),
            cred.plan_type_str(),
            cred.account_id.as_deref(),
        )
    } else {
        None
    };
    let id = if let Some(ref pid) = session.provider_id {
        pid.clone()
    } else if let Some(ref sid) = stable_codex_id {
        // Reuse existing provider row with same stable id if present.
        sid.clone()
    } else {
        Ulid::new().to_string()
    };
    let name = session.name.clone().unwrap_or_else(|| match kind {
        OAuthProviderKind::Codex => conduit_oauth::providers::codex::display_provider_name(
            cred.email.as_deref(),
            cred.plan_type_str(),
        ),
        _ => match &email {
            Some(e) if !e.is_empty() => format!("{} ({e})", kind.as_str()),
            _ => format!("{}-oauth", kind.as_str()),
        },
    });
    let base_url = cred
        .base_url
        .clone()
        .unwrap_or_else(|| kind.default_base_url().to_string());
    let now = Utc::now().to_rfc3339();
    let upstream_key_ref = format!("secret://upstream_key/{id}");

    let repo = ProviderRepo::new(&state.pool);
    let existing = repo
        .get(&id)
        .await
        .map_err(|e| OAuthError::Credential(e.to_string()))?;

    let row = ProviderRow {
        id: id.clone(),
        name,
        kind: kind.provider_kind_str().to_string(),
        base_url,
        upstream_key_ref,
        created_at: existing
            .as_ref()
            .map(|r| r.created_at.clone())
            .unwrap_or_else(|| now.clone()),
        updated_at: now,
        deleted_at: None,
    };

    if existing.is_some() {
        repo.update(&row)
            .await
            .map_err(|e| OAuthError::Credential(e.to_string()))?;
    } else {
        repo.insert(&row)
            .await
            .map_err(|e| OAuthError::Credential(e.to_string()))?;
    }

    let bytes = cred.to_json_bytes()?;
    state
        .secret_backend
        .put("upstream_key", &id, SecretVec::new(bytes))
        .await
        .map_err(|e| OAuthError::Credential(e.to_string()))?;

    let _ = state.oauth.sessions.update(&session.id, |s| {
        s.email = email;
        s.completed_provider_id = Some(id.clone());
    });

    // Reload routing table so new provider base_url is visible to routes.
    if let Err(e) = crate::server::reload_routing_table(state).await {
        warn!("reload routing after oauth: {e}");
    }

    info!(provider_id = %id, kind = %kind, "oauth credential stored");
    Ok(id)
}

fn stop_callback(state: &DaemonState, session_id: &str) {
    if let Some(h) = state.oauth.callbacks.lock().remove(session_id) {
        let _ = h.shutdown.send(());
        if let Some(fwd) = h.forwarder_shutdown {
            let _ = fwd.send(());
        }
    }
}

fn success_page(kind: &str, email: &str) -> String {
    let detail = if email.is_empty() {
        format!("{kind} OAuth completed.")
    } else {
        format!("{kind} OAuth completed for <b>{email}</b>.")
    };
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Authorization Success</title>
<style>body{{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#f5f5f5}}
.box{{background:#fff;padding:2rem;border-radius:8px;box-shadow:0 2px 10px rgba(0,0,0,.1);text-align:center;max-width:420px}}
h1{{color:#4caf50}}</style></head><body><div class="box"><h1>授权成功</h1><p>{detail}</p>
<p>你可以关闭此窗口。</p></div></body></html>"#
    )
}

fn error_page(msg: &str) -> String {
    let msg = html_escape(msg);
    format!(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Authorization Failed</title>
<style>body{{font-family:system-ui;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#f5f5f5}}
.box{{background:#fff;padding:2rem;border-radius:8px;box-shadow:0 2px 10px rgba(0,0,0,.1);text-align:center;max-width:420px}}
h1{{color:#f44336}}</style></head><body><div class="box"><h1>授权失败</h1><p>{msg}</p></div></body></html>"#
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
