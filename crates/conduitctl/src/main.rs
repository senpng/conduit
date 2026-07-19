//! conduitctl — Conduit v2 command-line interface.
//!
//! Provides human, machine, and interactive TUI interfaces for the conduitd daemon.

use std::io::{self, IsTerminal};

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};

mod cmd;

use conduitctl::tui;

#[derive(Debug, Parser)]
#[command(name = "conduitctl", about = "Conduit v2 gateway CLI")]
pub struct Args {
    /// Daemon console address (host:port)
    #[arg(
        long,
        env = "CONDUIT_CONSOLE_ADDR",
        default_value = "http://127.0.0.1:4001",
        global = true
    )]
    pub console_addr: String,

    /// Output format: "human" | "json"
    #[arg(long, env = "CONDUIT_OUTPUT", default_value = "human", global = true)]
    pub output: String,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Interactive terminal console (full operator TUI)
    Tui,
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
    #[command(name = "oauth")]
    OAuth(cmd::oauth::OAuthArgs),
    /// Check daemon health
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    match args.command {
        None => {
            if io::stdout().is_terminal() {
                tui::run(&args.console_addr).await
            } else {
                Args::command().print_help()?;
                println!();
                Ok(())
            }
        }
        Some(Command::Tui) => tui::run(&args.console_addr).await,
        Some(Command::Usage(a)) => cmd::usage::run(&args.console_addr, a, &args.output).await,
        Some(Command::Pricing(a)) => cmd::pricing::run(&args.console_addr, a, &args.output).await,
        Some(Command::Provider(a)) => {
            cmd::provider::run(&args.console_addr, a, &args.output).await
        }
        Some(Command::Route(a)) => cmd::route::run(&args.console_addr, a, &args.output).await,
        Some(Command::Key(a)) => cmd::key::run(&args.console_addr, a, &args.output).await,
        Some(Command::OAuth(a)) => cmd::oauth::run(&args.console_addr, a, &args.output).await,
        Some(Command::Status) => cmd::status::run(&args.console_addr).await,
    }
}
