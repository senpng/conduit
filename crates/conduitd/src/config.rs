use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
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
    /// (`false`). Overridden by `CONDUIT_LOG_TO_FILE`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_file: Option<bool>,

    /// Directory for log files. Overridden by `CONDUIT_LOG_DIR`.
    /// Defaults to `<data-dir>/logs` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub port: u16,
    pub console_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gateway: GatewayConfig {
                port: 4000,
                console_port: 4001,
            },
            proxy_url: None,
            master_password: None,
            log: LogConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&text)?;
        Ok(cfg)
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
