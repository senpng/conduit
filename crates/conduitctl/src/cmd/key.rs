use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
pub struct KeyArgs {
    #[command(subcommand)]
    pub command: KeyCommand,
}

#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    List,
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        rpm: Option<u32>,
    },
    Revoke {
        id: String,
    },
}

pub async fn run(console_addr: &str, args: KeyArgs, _output: &str) -> Result<()> {
    let base = console_addr.trim_end_matches('/');
    let client = reqwest::Client::new();

    match args.command {
        KeyCommand::List => {
            let resp = client.get(format!("{}/console/keys", base)).send().await?;
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        KeyCommand::Create { name, rpm } => {
            let resp = client
                .post(format!("{}/console/keys", base))
                .json(&serde_json::json!({"name": name, "rate_limit_rpm": rpm}))
                .send()
                .await?;
            let body: serde_json::Value = resp.json().await?;
            println!("Key created:");
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        KeyCommand::Revoke { id } => {
            let resp = client
                .delete(format!("{}/console/keys/{}", base, id))
                .send()
                .await?;
            if resp.status().is_success() {
                println!("Key {} revoked", id);
            } else {
                anyhow::bail!("failed: HTTP {}", resp.status());
            }
        }
    }
    Ok(())
}
