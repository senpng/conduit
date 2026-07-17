use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
pub struct OAuthArgs {
    #[command(subcommand)]
    pub command: OAuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum OAuthCommand {
    /// List supported OAuth providers
    List,
    /// Start an OAuth login (claude | codex | grok)
    Start {
        kind: String,
        #[arg(long)]
        name: Option<String>,
        /// Re-auth existing provider id
        #[arg(long)]
        provider_id: Option<String>,
        /// Poll until complete (default true)
        #[arg(long, default_value_t = true)]
        wait: bool,
    },
    /// Poll session status
    Status { session_id: String },
    /// Cancel a pending session
    Cancel { session_id: String },
    /// Force-refresh OAuth tokens for a provider
    Refresh { provider_id: String },
}

pub async fn run(console_addr: &str, args: OAuthArgs, _output: &str) -> Result<()> {
    let base = console_addr.trim_end_matches('/');
    let client = reqwest::Client::new();

    match args.command {
        OAuthCommand::List => {
            let resp = client
                .get(format!("{base}/console/oauth/providers"))
                .send()
                .await?;
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        OAuthCommand::Start {
            kind,
            name,
            provider_id,
            wait,
        } => {
            let mut body = serde_json::json!({});
            if let Some(n) = name {
                body["name"] = serde_json::json!(n);
            }
            if let Some(p) = provider_id {
                body["provider_id"] = serde_json::json!(p);
            }
            let resp = client
                .post(format!("{base}/console/oauth/{kind}/start"))
                .json(&body)
                .send()
                .await?;
            if !resp.status().is_success() {
                let t = resp.text().await.unwrap_or_default();
                bail!("start failed: {t}");
            }
            let session: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&session)?);

            if let Some(url) = session.get("auth_url").and_then(|u| u.as_str()) {
                println!("\nOpen this URL in your browser:\n  {url}\n");
            }
            if let Some(code) = session.get("user_code").and_then(|u| u.as_str()) {
                let uri = session
                    .get("verification_uri")
                    .and_then(|u| u.as_str())
                    .unwrap_or("");
                println!("\nDevice code: {code}");
                println!("Visit: {uri}\n");
            }

            let sid = session
                .get("session_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            if wait && !sid.is_empty() {
                println!("Waiting for authorization (Ctrl-C to stop polling)...");
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let st = client
                        .get(format!("{base}/console/oauth/sessions/{sid}"))
                        .send()
                        .await?
                        .json::<serde_json::Value>()
                        .await?;
                    let status = st.get("status").and_then(|s| s.as_str()).unwrap_or("?");
                    match status {
                        "completed" => {
                            println!("✓ OAuth completed");
                            println!("{}", serde_json::to_string_pretty(&st)?);
                            break;
                        }
                        "error" | "cancelled" => {
                            bail!(
                                "OAuth {}: {}",
                                status,
                                st.get("error")
                                    .and_then(|e| e.as_str())
                                    .unwrap_or("unknown")
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        OAuthCommand::Status { session_id } => {
            let resp = client
                .get(format!("{base}/console/oauth/sessions/{session_id}"))
                .send()
                .await?;
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        OAuthCommand::Cancel { session_id } => {
            let resp = client
                .post(format!("{base}/console/oauth/sessions/{session_id}/cancel"))
                .send()
                .await?;
            if resp.status().is_success() {
                println!("Session {session_id} cancelled");
            } else {
                bail!("cancel failed: HTTP {}", resp.status());
            }
        }
        OAuthCommand::Refresh { provider_id } => {
            let resp = client
                .post(format!("{base}/console/oauth/{provider_id}/refresh"))
                .send()
                .await?;
            if !resp.status().is_success() {
                bail!("refresh failed: {}", resp.text().await.unwrap_or_default());
            }
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
    }
    Ok(())
}
