//! conduitd — the Conduit daemon process.
//!
//! Starts the OpenAI-compatible gateway on the configured port,
//! console API, and background services (quota cleanup).

use anyhow::Result;
use clap::Parser;
use conduitd::{config, server};
use tracing::info;

#[derive(Debug, Parser)]
#[command(name = "conduitd", about = "Conduit LLM gateway daemon")]
pub struct Args {
    /// Path to conduit.toml config file. When unset, conduitd looks for
    /// `conduit.toml` in the working directory, then `~/.conduit/conduit.toml`.
    #[arg(long, env = "CONDUIT_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    /// Override gateway listen port
    #[arg(long, env = "CONDUIT_PORT")]
    pub port: Option<u16>,

    /// Data directory for secrets and SQLite DB
    #[arg(long, env = "CONDUIT_DATA_DIR", default_value = "~/.conduit")]
    pub data_dir: std::path::PathBuf,

    /// Fork into the background (daemonize) after startup checks.
    /// Detaches from the controlling terminal and redirects stdin/stdout/
    /// stderr to /dev/null. Keep file logging enabled (the default) so logs
    /// are still captured. Unix only.
    #[arg(short = 'd', long, env = "CONDUIT_DAEMON")]
    pub daemon: bool,

    /// PID file path when running with --daemon.
    /// Defaults to <data-dir>/conduitd.pid.
    #[arg(long, env = "CONDUIT_PID_FILE")]
    pub pid_file: Option<std::path::PathBuf>,

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
    /// Files rotate at **local** midnight (`conduitd.log.YYYY-MM-DD`).
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

fn main() -> Result<()> {
    let args = Args::parse();

    let data_dir = expand_tilde(&args.data_dir);

    // Load config before daemonizing / initializing tracing so that logging
    // settings can come from the config file, and — crucially in daemon mode —
    // so a bad config is reported on the foreground terminal before stderr is
    // redirected to /dev/null. Defer the load note until the subscriber is up.
    //
    // A present-but-invalid config is fatal: we exit rather than silently
    // running with defaults, which would drop every configured setting.
    let (cfg, load_note) = match resolve_config(args.config.as_deref()) {
        Ok((cfg, note)) => (cfg, note),
        Err(e) => {
            eprintln!("conduitd: {e:#}");
            std::process::exit(1);
        }
    };

    // Daemonize BEFORE building the tokio runtime or initializing tracing.
    // fork() keeps only the calling thread, so the runtime's worker threads
    // and the tracing-appender writer thread must be created in the child,
    // after the fork — otherwise they would be lost and file logging would
    // silently stop. The parent process exits inside `daemonize`.
    if args.daemon {
        let log_to_file = args.log_to_file.or(cfg.log.to_file).unwrap_or(true);
        if !log_to_file {
            eprintln!(
                "conduitd: warning: --daemon with file logging disabled — stdout/stderr \
                 are redirected to /dev/null, so logs will be lost. Keep --log-to-file=true."
            );
        }

        // The data dir is normally created inside server::run; create it here
        // so the PID file (and the daemon's working directory) can live there.
        if let Err(e) = std::fs::create_dir_all(&data_dir) {
            eprintln!(
                "conduitd: failed to create data dir {}: {e}",
                data_dir.display()
            );
            std::process::exit(1);
        }

        let pid_file = args
            .pid_file
            .clone()
            .unwrap_or_else(|| data_dir.join("conduitd.pid"));
        if let Err(e) = daemonize(&pid_file, &data_dir) {
            eprintln!("conduitd: {e:#}");
            std::process::exit(1);
        }
    }

    // Build the runtime AFTER the fork so its worker threads belong to the
    // daemonized child. Mirrors `#[tokio::main]`'s multi-thread runtime.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run_async(args, cfg, load_note, data_dir))
}

/// Initialize tracing and run the servers. Runs inside the (possibly
/// daemonized) child's tokio runtime — `init_tracing` must happen here,
/// after any fork, so the appender's writer thread survives.
async fn run_async(
    args: Args,
    cfg: config::Config,
    load_note: Option<&'static str>,
    data_dir: std::path::PathBuf,
) -> Result<()> {
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
    let _log_guard = init_tracing(&log_format, &log_level, log_to_file, log_dir.clone());
    // init_tracing may fall back to stdout if the dir cannot be created/opened;
    // only advertise file logging when the guard is present.
    let log_runtime = conduitd::state::LogRuntime {
        to_file: _log_guard.is_some(),
        dir: log_dir,
        prefix: conduitd::log_reader::DEFAULT_LOG_PREFIX.into(),
        format: log_format,
        level: log_level,
    };

    info!(version = env!("CARGO_PKG_VERSION"), "conduitd starting");
    if let Some(note) = load_note {
        info!("{note}");
    }

    let port = args.port.unwrap_or(cfg.gateway.port);
    let master_password = args
        .master_password
        .or_else(|| cfg.master_password.clone())
        .map(secrecy::SecretString::new);

    server::run_with_log(cfg, port, data_dir, master_password, log_runtime).await
}

/// Fork the process into the background (Unix daemonize).
///
/// Performs the standard double-fork + `setsid`, redirects stdin/stdout/stderr
/// to `/dev/null`, sets the working directory to `data_dir`, and writes the
/// child PID to `pid_file` under an exclusive lock. The file is not removed on
/// exit — a later daemon start re-locks and overwrites it, so a leftover PID
/// file is harmless. The parent process exits inside `start()`, so all code
/// after this call runs in the daemon child.
fn daemonize(pid_file: &std::path::Path, data_dir: &std::path::Path) -> Result<()> {
    use daemonize::Daemonize;

    Daemonize::new()
        .pid_file(pid_file)
        // Default is chdir("/"); use data_dir instead so relative paths behave
        // predictably. Note: with the cwd changed, `conduit.toml` discovery is
        // best driven by an explicit --config in daemon mode.
        .working_directory(data_dir)
        .start()
        .map_err(|e| anyhow::anyhow!("daemonize failed: {e}"))
}

/// Initialize the global tracing subscriber.
///
/// When `to_file` is set, logs are written to a **local-timezone** daily-
/// rolling file under `log_dir` (`conduitd.log.YYYY-MM-DD`) via a
/// non-blocking background writer; the returned [`WorkerGuard`] must be
/// held for the lifetime of the process. When unset (or if the log
/// directory cannot be created), logs go to stdout and `None` is returned.
///
/// Line timestamps remain UTC (`…Z`) from `tracing-subscriber`; only the
/// **file name / rotation boundary** follows the host local calendar.
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

        let appender = match conduitd::log_rolling::LocalDailyRollingFile::new(
            &log_dir,
            "conduitd.log",
        ) {
            Ok(a) => a,
            Err(e) => {
                let subscriber = fmt().with_env_filter(make_filter());
                match format {
                    "json" => subscriber.json().init(),
                    _ => subscriber.init(),
                }
                tracing::warn!(
                    dir = %log_dir.display(),
                    error = %e,
                    "failed to open local daily log file, logging to stdout instead"
                );
                return None;
            }
        };
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

/// Resolve the effective config, along with a note to log once tracing is up.
///
/// If `explicit` is set (via `--config` / `CONDUIT_CONFIG`), that path must
/// load successfully — a missing or malformed file is an error. Otherwise
/// conduitd searches `./conduit.toml` then `~/.conduit/conduit.toml`, using
/// the first that exists; if none do, it falls back to built-in defaults.
/// A malformed file that *is* found is always fatal.
fn resolve_config(
    explicit: Option<&std::path::Path>,
) -> anyhow::Result<(config::Config, Option<&'static str>)> {
    if let Some(path) = explicit {
        return match config::Config::load(path)? {
            Some(cfg) => Ok((cfg, None)),
            None => Err(anyhow::anyhow!("config file {} not found", path.display())),
        };
    }

    let mut candidates = vec![std::path::PathBuf::from("conduit.toml")];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(std::path::Path::new(&home).join(".conduit/conduit.toml"));
    }

    for path in &candidates {
        if let Some(cfg) = config::Config::load(path)? {
            return Ok((cfg, None));
        }
    }

    Ok((
        config::Config::default(),
        Some("No config file found, using defaults"),
    ))
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
