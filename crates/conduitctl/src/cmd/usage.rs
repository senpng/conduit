use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
pub struct UsageArgs {
    #[command(subcommand)]
    pub command: UsageCommand,
}

#[derive(Debug, Subcommand)]
pub enum UsageCommand {
    /// Show period summary (cost + tokens by key)
    Summary {
        #[arg(long)]
        period: Option<String>,
    },
    /// List recent per-request usage rows
    List {
        #[arg(long)]
        key_id: Option<String>,
        /// Calendar month (`YYYY-MM`); omit for latest across all months
        #[arg(long)]
        period: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Pagination offset (0-based)
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Free-text filter (model / alias / provider / request / key)
        #[arg(long, short = 'q')]
        query: Option<String>,
        /// Sort: date (default) | cost | tokens
        #[arg(long, default_value = "date")]
        sort: String,
    },
}

pub async fn run(console_addr: &str, args: UsageArgs, _output: &str) -> Result<()> {
    let base = console_addr.trim_end_matches('/');
    let client = reqwest::Client::new();

    match args.command {
        UsageCommand::Summary { period } => {
            let mut url = format!("{base}/console/usage/summary");
            if let Some(p) = period {
                url.push_str(&format!("?period={}", p));
            }
            let resp = client.get(&url).send().await?;
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        UsageCommand::List {
            key_id,
            period,
            limit,
            offset,
            query,
            sort,
        } => {
            let mut url = format!("{base}/console/usage?limit={limit}&offset={offset}&sort={sort}");
            if let Some(k) = key_id {
                url.push_str(&format!("&key_id={}", k));
            }
            if let Some(p) = period {
                url.push_str(&format!("&period={}", p));
            }
            if let Some(q) = query {
                url.push_str(&format!("&q={}", q));
            }
            let resp = client.get(&url).send().await?;
            let body: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
    }
    Ok(())
}
