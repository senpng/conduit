use anyhow::Result;
use clap::{Parser, Subcommand};
use conduitctl::{provider_create_request_body, ConsoleClient, ConsoleError};

#[derive(Debug, Parser)]
pub struct ProviderArgs {
    #[command(subcommand)]
    pub command: ProviderCommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderCommand {
    List,
    /// Create a provider (daemon allocates id). Body: name, kind, base_url [, api_key].
    Add {
        /// Human-readable provider name
        #[arg(long)]
        name: String,
        /// Provider kind (openai, anthropic, claude-oauth, …)
        #[arg(long)]
        kind: String,
        /// Upstream base URL
        #[arg(long)]
        base_url: String,
        /// Optional API key stored immediately in the secret backend
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Set Claude OAuth cloak mode (`auto` | `always` | `never`).
    Cloak {
        /// Provider id (claude-oauth)
        id: String,
        /// Cloak mode: auto, always, or never
        #[arg(long)]
        mode: String,
    },
    Remove {
        id: String,
    },
    Health {
        id: String,
    },
}

pub async fn run(console_addr: &str, args: ProviderArgs, _output: &str) -> Result<()> {
    let client = ConsoleClient::new(console_addr);

    match args.command {
        ProviderCommand::List => {
            let body = client
                .list_providers()
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        ProviderCommand::Add {
            name,
            kind,
            base_url,
            api_key,
        } => {
            let body = provider_create_request_body(&name, &kind, &base_url, api_key.as_deref());
            // Contract check: never send a client-generated `id`.
            debug_assert!(serde_json::to_value(&body)
                .ok()
                .and_then(|v| v.get("id").cloned())
                .is_none());
            match client.create_provider(&body).await {
                Ok(resp) => {
                    let id = resp
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)");
                    println!("Provider {} added (name={})", id, name);
                }
                Err(ConsoleError::Http { status, body }) => {
                    anyhow::bail!("failed: HTTP {} — {}", status, body);
                }
                Err(e) => anyhow::bail!("failed: {}", e),
            }
        }
        ProviderCommand::Cloak { id, mode } => {
            let body = conduitctl::dto::UpdateOAuthSettingsBody::cloak_mode(mode.trim());
            match client.update_provider_oauth_settings(&id, &body).await {
                Ok(resp) => {
                    println!("{}", serde_json::to_string_pretty(&resp)?);
                }
                Err(ConsoleError::Http { status, body }) => {
                    anyhow::bail!("failed: HTTP {} — {}", status, body);
                }
                Err(e) => anyhow::bail!("failed: {}", e),
            }
        }
        ProviderCommand::Remove { id } => {
            client
                .delete_provider(&id)
                .await
                .map_err(|e| anyhow::anyhow!("failed: {}", e))?;
            println!("Provider {} removed", id);
        }
        ProviderCommand::Health { id } => {
            // Endpoint may be missing on older daemons; keep direct call for now.
            let base = client.base_url();
            let resp = reqwest::Client::new()
                .get(format!("{}/console/providers/{}/health", base, id))
                .send()
                .await?;
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn add_cli_uses_name_kind_base_url_not_id() {
        #[derive(Debug, Parser)]
        struct Wrap {
            #[command(subcommand)]
            command: ProviderCommand,
        }
        // Simulate: provider add --name x --kind openai --base-url https://example
        let w = Wrap::try_parse_from([
            "provider",
            "add",
            "--name",
            "prod",
            "--kind",
            "openai",
            "--base-url",
            "https://api.openai.com/v1",
        ])
        .expect("parse");
        match w.command {
            ProviderCommand::Add {
                name,
                kind,
                base_url,
                api_key,
            } => {
                assert_eq!(name, "prod");
                assert_eq!(kind, "openai");
                assert_eq!(base_url, "https://api.openai.com/v1");
                assert!(api_key.is_none());
                let body = provider_create_request_body(&name, &kind, &base_url, None);
                let v = serde_json::to_value(&body).unwrap();
                assert!(v.get("id").is_none());
                assert_eq!(v["name"], "prod");
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }
}
