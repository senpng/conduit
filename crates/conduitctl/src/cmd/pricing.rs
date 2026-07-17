use anyhow::{bail, Result};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
pub struct PricingArgs {
    #[command(subcommand)]
    pub command: PricingCommand,
}

#[derive(Debug, Subcommand)]
pub enum PricingCommand {
    /// List in-memory pricing rows from the daemon
    List,
    /// Reload layers: defaults + pricing.litellm.json + pricing.json
    Reload,
    /// Fetch LiteLLM model_prices map, convert, cache, and reload
    Sync {
        /// Override source URL (default: LiteLLM GitHub raw main)
        #[arg(long)]
        url: Option<String>,
    },
}

pub async fn run(console_addr: &str, args: PricingArgs, output: &str) -> Result<()> {
    let base = console_addr.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(90))
        .build()?;

    match args.command {
        PricingCommand::List => {
            let url = format!("{base}/console/pricing");
            let resp = client.get(&url).send().await?;
            if !resp.status().is_success() {
                bail!("list pricing failed: HTTP {}", resp.status());
            }
            let body: serde_json::Value = resp.json().await?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                let rows = body.as_array().cloned().unwrap_or_default();
                println!(
                    "{:<16} {:<36} {:>10} {:>10}",
                    "PROVIDER", "MODEL", "IN/MTok", "OUT/MTok"
                );
                for r in &rows {
                    println!(
                        "{:<16} {:<36} {:>10.4} {:>10.4}",
                        r.get("provider_kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?"),
                        r.get("model_id").and_then(|v| v.as_str()).unwrap_or("?"),
                        r.get("input_per_mtok")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                        r.get("output_per_mtok")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                    );
                }
                println!("{} rows", rows.len());
            }
        }
        PricingCommand::Reload => {
            let url = format!("{base}/console/pricing/reload");
            let resp = client.post(&url).send().await?;
            let status = resp.status();
            let body: serde_json::Value = resp.json().await?;
            if !status.is_success() {
                bail!("reload failed: HTTP {status}: {body}");
            }
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!("pricing reloaded");
            }
        }
        PricingCommand::Sync { url: source_url } => {
            let url = format!("{base}/console/pricing/sync");
            let mut req = client.post(&url);
            if let Some(u) = source_url {
                req = req.json(&serde_json::json!({ "url": u }));
            } else {
                // Empty JSON object so Content-Type is set consistently.
                req = req.json(&serde_json::json!({}));
            }
            let resp = req.send().await?;
            let status = resp.status();
            let body: serde_json::Value = resp.json().await?;
            if !status.is_success() {
                bail!("sync failed: HTTP {status}: {body}");
            }
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!(
                    "synced from {} (source_models={}, total_rows={}, skipped={})",
                    body.get("url").and_then(|v| v.as_str()).unwrap_or("?"),
                    body.get("source_models")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    body.get("total_rows").and_then(|v| v.as_u64()).unwrap_or(0),
                    body.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0),
                );
            }
        }
    }
    Ok(())
}
