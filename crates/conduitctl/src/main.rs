//! conduitctl — Conduit v2 command-line interface.
//!
//! Provides human and machine interfaces for the conduitd daemon.

use anyhow::Result;
use clap::{Parser, Subcommand};

mod cmd;

#[derive(Debug, Parser)]
#[command(name = "conduitctl", about = "Conduit v2 gateway CLI")]
pub struct Args {
    /// Daemon admin address (host:port)
    #[arg(
        long,
        env = "CONDUIT_ADMIN_ADDR",
        default_value = "http://127.0.0.1:4001",
        global = true
    )]
    pub admin_addr: String,

    /// Output format: "human" | "json"
    #[arg(long, env = "CONDUIT_OUTPUT", default_value = "human", global = true)]
    pub output: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Show traces
    Trace(cmd::trace::TraceArgs),
    /// Request usage / cost ledger
    Usage(cmd::usage::UsageArgs),
    /// Model pricing (list / reload / sync from LiteLLM)
    Pricing(cmd::pricing::PricingArgs),
    /// Provider management
    Provider(cmd::provider::ProviderArgs),
    /// Route management
    Route(cmd::route::RouteArgs),
    /// Key management
    Key(cmd::key::KeyArgs),
    /// OAuth login (Claude / Codex / Grok)
    OAuth(cmd::oauth::OAuthArgs),
    /// Check daemon health
    Status,
    /// Runtime settings (trace enable/disable, …)
    Settings(cmd::settings::SettingsArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match args.command {
        Command::Trace(a) => cmd::trace::run(&args.admin_addr, a, &args.output).await,
        Command::Usage(a) => cmd::usage::run(&args.admin_addr, a, &args.output).await,
        Command::Pricing(a) => cmd::pricing::run(&args.admin_addr, a, &args.output).await,
        Command::Provider(a) => cmd::provider::run(&args.admin_addr, a, &args.output).await,
        Command::Route(a) => cmd::route::run(&args.admin_addr, a, &args.output).await,
        Command::Key(a) => cmd::key::run(&args.admin_addr, a, &args.output).await,
        Command::OAuth(a) => cmd::oauth::run(&args.admin_addr, a, &args.output).await,
        Command::Status => cmd::status::run(&args.admin_addr).await,
        Command::Settings(a) => cmd::settings::run(&args.admin_addr, a, &args.output).await,
    }
}
