use anyhow::Result;
use clap::{Parser, Subcommand};
use conduitctl::{
    dto::{UpdateSettingsBody, UpdateTraceSettingsBody},
    AdminClient,
};

#[derive(Debug, Parser)]
pub struct SettingsArgs {
    #[command(subcommand)]
    pub command: SettingsCommand,
}

#[derive(Debug, Subcommand)]
pub enum SettingsCommand {
    /// Show current settings (trace enable, …)
    Show,
    /// Enable or disable request trace recording
    Trace {
        /// `on` | `off`
        state: String,
    },
}

pub async fn run(admin_addr: &str, args: SettingsArgs, output: &str) -> Result<()> {
    let client = AdminClient::new(admin_addr);

    match args.command {
        SettingsCommand::Show => {
            let s = client.get_settings().await?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!(
                    "trace recording: {}",
                    if s.trace.enabled { "on" } else { "off" }
                );
                if let Some(def) = s.trace.config_default {
                    println!("  config default:  {}", if def { "on" } else { "off" });
                }
                if let Some(ov) = s.trace.runtime_override {
                    println!("  runtime override: {}", if ov { "on" } else { "off" });
                } else {
                    println!("  runtime override: (none)");
                }
            }
        }
        SettingsCommand::Trace { state } => {
            let enabled = match state.trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "1" | "enable" | "enabled" => true,
                "off" | "false" | "0" | "disable" | "disabled" => false,
                other => anyhow::bail!("expected on|off, got {other}"),
            };
            let s = client
                .update_settings(&UpdateSettingsBody {
                    trace: Some(UpdateTraceSettingsBody { enabled }),
                })
                .await?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!(
                    "trace recording set to {}",
                    if s.trace.enabled { "on" } else { "off" }
                );
            }
        }
    }
    Ok(())
}
