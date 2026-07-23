use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Gateway listen ports. The `[gateway]` section and its fields are all
    /// optional; omitted fields fall back to the built-in defaults.
    #[serde(default)]
    pub gateway: GatewayConfig,
    /// Upstream OAuth / token HTTP client proxy (HTTP or SOCKS URL).
    ///
    /// Priority (highest first): credential `proxy_url` → `CONDUIT_PROXY_URL` →
    /// this field → `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY`.
    /// Bypass: `NO_PROXY` / `no_proxy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,

    /// Master password for AES-256-GCM secret encryption.
    ///
    /// Overridden by `CONDUIT_MASTER_PASSWORD` / `--master-password`. Prefer
    /// the env var so the password is not persisted to disk in plaintext.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_password: Option<String>,

    /// Logging settings. Each field is overridden by its matching env var /
    /// CLI flag when set (see `[log]` in the README).
    #[serde(default)]
    pub log: LogConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogConfig {
    /// Log level filter, e.g. `info` or `debug,sqlx::query=off`.
    /// Overridden by `CONDUIT_LOG`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,

    /// Log format: `pretty` or `json`. Overridden by `CONDUIT_LOG_FORMAT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    /// Whether to write logs to a daily-rolling file (`true`) or stdout
    /// (`false`). Rotation is at **local** midnight. Overridden by
    /// `CONDUIT_LOG_TO_FILE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_file: Option<bool>,

    /// Directory for log files. Overridden by `CONDUIT_LOG_DIR`.
    /// Defaults to `<data-dir>/logs` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_console_port")]
    pub console_port: u16,
}

fn default_port() -> u16 {
    4000
}

fn default_console_port() -> u16 {
    4001
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            port: default_port(),
            console_port: default_console_port(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gateway: GatewayConfig::default(),
            proxy_url: None,
            master_password: None,
            log: LogConfig::default(),
        }
    }
}

impl Config {
    /// Load config from `path`.
    ///
    /// Returns `Ok(None)` when the file does not exist (callers should fall
    /// back to defaults). Returns `Err` when the file exists but cannot be
    /// read or parsed — this must surface to the user rather than being
    /// silently replaced by defaults, otherwise a typo drops every setting.
    pub fn load(path: &Path) -> anyhow::Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("reading config file {}", path.display())))
            }
        };
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing config file {}: {e}", path.display()))?;
        Ok(Some(cfg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_ports() {
        let cfg = Config::default();
        assert_eq!(cfg.gateway.port, 4000);
        assert_eq!(cfg.gateway.console_port, 4001);
    }

    #[test]
    fn minimal_toml_loads() {
        let toml = r#"
[gateway]
port = 4000
console_port = 4001
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.gateway.port, 4000);
    }

    #[test]
    fn log_only_toml_loads_with_default_gateway() {
        // A config with only a [log] section must parse — [gateway] and its
        // fields are optional and fall back to defaults. This is the exact
        // shape that previously failed to deserialize and silently dropped
        // the log level.
        let toml = r#"
[log]
level = "debug,sqlx::query=off"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.gateway.port, 4000);
        assert_eq!(cfg.gateway.console_port, 4001);
        assert_eq!(cfg.log.level.as_deref(), Some("debug,sqlx::query=off"));
    }

    #[test]
    fn partial_gateway_fills_defaults() {
        // Only one gateway field set; the other falls back to its default.
        let toml = r#"
[gateway]
port = 8080
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.gateway.port, 8080);
        assert_eq!(cfg.gateway.console_port, 4001);
    }

    #[test]
    fn load_missing_file_returns_none() {
        let got = Config::load(Path::new("/nonexistent/conduit.toml")).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn load_malformed_file_errors() {
        // A present-but-invalid file must error, not silently fall back.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conduit.toml");
        std::fs::write(&path, "port = = broken").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains("parsing config file"));
    }

    #[test]
    fn ignores_legacy_security_section() {
        // Older configs may still contain [security] backend = "keychain".
        // Serde ignores unknown tables by default.
        let toml = r#"
[gateway]
port = 4000
console_port = 4001
[security]
backend = "keychain"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.gateway.port, 4000);
    }

    #[test]
    fn loads_proxy_url() {
        let toml = r#"
proxy_url = "socks5://127.0.0.1:7890"
[gateway]
port = 4000
console_port = 4001
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.proxy_url.as_deref(), Some("socks5://127.0.0.1:7890"));
    }

    #[test]
    fn loads_log_and_master_password() {
        let toml = r#"
master_password = "hunter2"
[gateway]
port = 4000
console_port = 4001
[log]
level = "debug"
format = "json"
to_file = false
dir = "/var/log/conduit"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.master_password.as_deref(), Some("hunter2"));
        assert_eq!(cfg.log.level.as_deref(), Some("debug"));
        assert_eq!(cfg.log.format.as_deref(), Some("json"));
        assert_eq!(cfg.log.to_file, Some(false));
        assert_eq!(
            cfg.log.dir.as_deref(),
            Some(std::path::Path::new("/var/log/conduit"))
        );
    }

    #[test]
    fn log_section_is_optional() {
        // A config with no [log] table must still parse, with all-None log fields.
        let toml = r#"
[gateway]
port = 4000
console_port = 4001
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.log.level.is_none());
        assert!(cfg.log.to_file.is_none());
        assert!(cfg.master_password.is_none());
    }
}
