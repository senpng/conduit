use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub gateway: GatewayConfig,
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
}
