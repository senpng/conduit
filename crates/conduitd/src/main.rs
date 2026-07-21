//! conduitd — the Conduit v2 daemon process.
//!
//! Starts the OpenAI-compatible gateway on the configured port,
//! console API, and background services (quota cleanup).

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

    /// Data directory for secrets and SQLite DB
    #[arg(long, env = "CONDUIT_DATA_DIR", default_value = "~/.conduit")]
    pub data_dir: std::path::PathBuf,

    /// Master password for AES-256-GCM secret encryption (Argon2id KEK).
    /// Prefer the env var so the password is not visible in process listings.
    /// Overrides `master_password` in conduit.toml.
    #[arg(long, env = "CONDUIT_MASTER_PASSWORD")]
    pub master_password: Option<String>,

    /// Log format: "json" or "pretty". Overrides `[log] format`
    /// in conduit.toml. Defaults to "pretty".
    #[arg(long, env = "CONDUIT_LOG_FORMAT")]
    pub log_format: Option<String>,

    /// Log level filter. Overrides `[log] level` in conduit.toml.
    /// Defaults to "info".
    #[arg(long, env = "CONDUIT_LOG")]
    pub log_level: Option<String>,

    /// Write logs to a daily-rolling file instead of stdout.
    /// Disable with --log-to-file=false to keep logging to stdout
    /// (e.g. under systemd/journald). Overrides `[log] to_file` in
    /// conduit.toml. Defaults to true.
    #[arg(long, env = "CONDUIT_LOG_TO_FILE", action = clap::ArgAction::Set)]
    pub log_to_file: Option<bool>,

    /// Directory for log files when logging to a file is enabled.
    /// Overrides `[log] dir` in conduit.toml.
    /// Defaults to <data-dir>/logs (i.e. ~/.conduit/logs).
    #[arg(long, env = "CONDUIT_LOG_DIR")]
    pub log_dir: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let data_dir = expand_tilde(&args.data_dir);

    // Load config before initializing tracing so that logging settings can
    // come from the config file. Defer any load message until the subscriber
    // is up, otherwise it would be lost.
    let (cfg, load_note) = match config::Config::load(&args.config) {
        Ok(cfg) => (cfg, None),
        Err(_) => (
            config::Config::default(),
            Some("No config file found, using defaults"),
        ),
    };

    // Resolve logging settings: CLI/env (Some) > config file (Some) > default.
    let log_format = args
        .log_format
        .or(cfg.log.format.clone())
        .unwrap_or_else(|| "pretty".to_string());
    let log_level = args
        .log_level
        .or(cfg.log.level.clone())
        .unwrap_or_else(|| "info".to_string());
    let log_to_file = args.log_to_file.or(cfg.log.to_file).unwrap_or(true);
    let log_dir = args
        .log_dir
        .clone()
        .or_else(|| cfg.log.dir.clone().map(|d| expand_tilde(&d)))
        .unwrap_or_else(|| data_dir.join("logs"));

    // Keep the appender guard alive for the whole process; dropping it
    // would flush and stop the background writer, losing buffered logs.
    let _log_guard = init_tracing(&log_format, &log_level, log_to_file, log_dir);

    info!(version = env!("CARGO_PKG_VERSION"), "conduitd starting");
    if let Some(note) = load_note {
        info!("{note}");
    }

    let port = args.port.unwrap_or(cfg.gateway.port);
    let master_password = args
        .master_password
        .or_else(|| cfg.master_password.clone())
        .map(secrecy::SecretString::new);

    server::run(cfg, port, data_dir, master_password).await
}

/// Initialize the global tracing subscriber.
///
/// When `to_file` is set, logs are written to a daily-rolling file under
/// `log_dir` via a non-blocking background writer; the returned
/// [`WorkerGuard`] must be held for the lifetime of the process. When
/// unset (or if the log directory cannot be created), logs go to stdout
/// and `None` is returned.
fn init_tracing(
    format: &str,
    level: &str,
    to_file: bool,
    log_dir: std::path::PathBuf,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, EnvFilter};

    let make_filter = || EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));

    if to_file {
        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            // Fall back to stdout rather than failing to start.
            let subscriber = fmt().with_env_filter(make_filter());
            match format {
                "json" => subscriber.json().init(),
                _ => subscriber.init(),
            }
            tracing::warn!(
                dir = %log_dir.display(),
                error = %e,
                "failed to create log directory, logging to stdout instead"
            );
            return None;
        }

        let appender = tracing_appender::rolling::daily(&log_dir, "conduitd.log");
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let subscriber = fmt().with_env_filter(make_filter()).with_writer(writer);
        // File output has no terminal, so disable ANSI colors.
        match format {
            "json" => subscriber.json().init(),
            _ => subscriber.with_ansi(false).init(),
        }
        return Some(guard);
    }

    let subscriber = fmt().with_env_filter(make_filter());
    match format {
        "json" => subscriber.json().init(),
        _ => subscriber.init(),
    }
    None
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
