use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub gateway: GatewayConfig,
    pub security: SecurityConfig,
    pub trace: TraceConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    pub port: u16,
    pub console_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    /// "keychain" | "master_password"
    pub backend: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceConfig {
    /// When false, new request traces are not written (usage ledger still records).
    #[serde(default = "default_trace_enabled")]
    pub enabled: bool,
    pub max_segment_mb: u64,
    pub max_db_size_mb: u64,
    pub retention_days: u32,
}

fn default_trace_enabled() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gateway: GatewayConfig {
                port: 4000,
                console_port: 4001,
            },
            security: SecurityConfig {
                backend: "keychain".to_string(),
            },
            trace: TraceConfig {
                enabled: true,
                max_segment_mb: 64,
                max_db_size_mb: 2048,
                retention_days: 90,
            },
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

// ── Runtime settings overlay (data_dir/settings.json) ─────────────────────────

/// Operator-tunable runtime flags persisted under the data directory.
///
/// Loaded after `conduit.toml` so UI/CLI toggles survive daemon restarts.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct RuntimeSettings {
    /// Overrides [`TraceConfig::enabled`] when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_enabled: Option<bool>,
}

impl RuntimeSettings {
    pub fn path(data_dir: &Path) -> std::path::PathBuf {
        data_dir.join("settings.json")
    }

    pub fn load(data_dir: &Path) -> Self {
        let path = Self::path(data_dir);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(data_dir)?;
        let path = Self::path(data_dir);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Effective trace-on/off after merging config default with runtime overlay.
    pub fn effective_trace_enabled(&self, config_default: bool) -> bool {
        self.trace_enabled.unwrap_or(config_default)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_trace_enabled() {
        assert!(Config::default().trace.enabled);
    }

    #[test]
    fn toml_without_enabled_defaults_true() {
        let toml = r#"
[gateway]
port = 4000
console_port = 4001
[security]
backend = "keychain"
[trace]
max_segment_mb = 64
max_db_size_mb = 2048
retention_days = 90
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.trace.enabled);
    }

    #[test]
    fn runtime_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = RuntimeSettings {
            trace_enabled: Some(false),
        };
        s.save(dir.path()).unwrap();
        let loaded = RuntimeSettings::load(dir.path());
        assert_eq!(loaded.trace_enabled, Some(false));
        assert!(!loaded.effective_trace_enabled(true));
    }
}
