//! conduitd — the Conduit v2 daemon process.
//!
//! Starts the OpenAI-compatible gateway on the configured port,
//! admin API, and all background services (trace sink, quota cleanup).

use anyhow::Result;
use clap::Parser;
use conduitd::{config, server};
use tracing::info;

#[derive(Debug, Parser)]
#[command(name = "conduitd", about = "Conduit v2 LLM gateway daemon")]
pub struct Args {
    /// Path to conduit.toml config file
    #[arg(long, env = "CONDUIT_CONFIG", default_value = "conduit.toml")]
    pub config: std::path::PathBuf,

    /// Override gateway listen port
    #[arg(long, env = "CONDUIT_PORT")]
    pub port: Option<u16>,

    /// Data directory for trace logs, secrets, and SQLite DB
    #[arg(long, env = "CONDUIT_DATA_DIR", default_value = "~/.conduit")]
    pub data_dir: std::path::PathBuf,

    /// Log format: "json" or "pretty"
    #[arg(long, env = "CONDUIT_LOG_FORMAT", default_value = "pretty")]
    pub log_format: String,

    /// Log level filter
    #[arg(long, env = "CONDUIT_LOG", default_value = "info")]
    pub log_level: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    init_tracing(&args.log_format, &args.log_level);

    info!(version = env!("CARGO_PKG_VERSION"), "conduitd starting");

    let cfg = config::Config::load(&args.config).unwrap_or_else(|_| {
        info!("No config file found, using defaults");
        config::Config::default()
    });

    let port = args.port.unwrap_or(cfg.gateway.port);
    let data_dir = expand_tilde(&args.data_dir);

    server::run(cfg, port, data_dir).await
}

fn init_tracing(format: &str, level: &str) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    match format {
        "json" => {
            fmt().json().with_env_filter(filter).init();
        }
        _ => {
            fmt().with_env_filter(filter).init();
        }
    }
}

fn expand_tilde(path: &std::path::Path) -> std::path::PathBuf {
    if let Some(s) = path.to_str() {
        if let Some(stripped) = s.strip_prefix("~/") {
            if let Ok(home) = std::env::var("HOME") {
                return std::path::PathBuf::from(home).join(stripped);
            }
        }
    }
    path.to_path_buf()
}
