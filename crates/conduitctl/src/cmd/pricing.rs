use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use conduitctl::{ConsoleClient, UpsertPricingOverrideBody};

#[derive(Debug, Parser)]
pub struct PricingArgs {
    #[command(subcommand)]
    pub command: PricingCommand,
}

#[derive(Debug, Subcommand)]
pub enum PricingCommand {
    /// List in-memory pricing rows from the daemon
    List,
    /// List operator overrides only (`pricing.json`)
    Overrides,
    /// Upsert one operator override into `pricing.json` and reload
    Set {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
        /// Input price USD per million tokens
        #[arg(long)]
        input: f64,
        /// Output price USD per million tokens
        #[arg(long)]
        output: f64,
        #[arg(long)]
        cache_read: Option<f64>,
        #[arg(long)]
        cache_write: Option<f64>,
    },
    /// Delete one operator override from `pricing.json` and reload
    Unset {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        model: String,
    },
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
    let client = ConsoleClient::new(console_addr);

    match args.command {
        PricingCommand::List => {
            let body = client
                .list_pricing_typed()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!(
                    "{:<16} {:<36} {:>10} {:>10}",
                    "PROVIDER", "MODEL", "IN/MTok", "OUT/MTok"
                );
                for r in &body {
                    println!(
                        "{:<16} {:<36} {:>10.4} {:>10.4}",
                        r.provider_kind, r.model_id, r.input_per_mtok, r.output_per_mtok
                    );
                }
                println!("{} rows", body.len());
            }
        }
        PricingCommand::Overrides => {
            let body = client
                .list_pricing_overrides()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!(
                    "{:<16} {:<36} {:>10} {:>10}",
                    "PROVIDER", "MODEL", "IN/MTok", "OUT/MTok"
                );
                for r in &body {
                    println!(
                        "{:<16} {:<36} {:>10.4} {:>10.4}",
                        r.provider_kind, r.model_id, r.input_per_mtok, r.output_per_mtok
                    );
                }
                println!("{} override(s)  (file: pricing.json)", body.len());
            }
        }
        PricingCommand::Set {
            provider,
            model,
            input,
            output: out_price,
            cache_read,
            cache_write,
        } => {
            let body = UpsertPricingOverrideBody {
                provider_kind: provider.clone(),
                model_id: model.clone(),
                input_per_mtok: input,
                output_per_mtok: out_price,
                cache_read_per_mtok: cache_read,
                cache_write_per_mtok: cache_write,
                reasoning_per_mtok: None,
                effective_from: None,
            };
            let resp = client
                .upsert_pricing_override(&body)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                println!("override saved: {provider} / {model}");
            }
        }
        PricingCommand::Unset { provider, model } => {
            // Uses query-param DELETE (model ids may contain `/`).
            let resp = client
                .delete_pricing_override(&provider, &model)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&resp)?);
            } else {
                println!("override deleted: {provider} / {model}");
            }
        }
        PricingCommand::Reload => {
            let body = client
                .reload_pricing()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!("pricing reloaded");
            }
        }
        PricingCommand::Sync { url: source_url } => {
            let body = client
                .sync_pricing(source_url.as_deref())
                .await
                .map_err(|e| anyhow::anyhow!("sync failed: {e}"))?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                println!(
                    "synced from {} (source_models={}, total_rows={}, skipped={})",
                    body.get("source").and_then(|v| v.as_str()).unwrap_or("?"),
                    body.get("source_models")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    body.get("total_rows")
                        .and_then(|v| v.as_u64())
                        .or_else(|| body.get("rows").and_then(|v| v.as_u64()))
                        .unwrap_or(0),
                    body.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0),
                );
            }
            if body.get("status").and_then(|v| v.as_str()) != Some("synced") {
                bail!("unexpected sync response: {body}");
            }
        }
    }
    Ok(())
}
