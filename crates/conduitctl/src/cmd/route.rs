use anyhow::Result;
use clap::{Parser, Subcommand};
use conduitctl::{route_console_path, ConsoleClient, ConsoleError};

#[derive(Debug, Parser)]
pub struct RouteArgs {
    #[command(subcommand)]
    pub command: RouteCommand,
}

#[derive(Debug, Subcommand)]
pub enum RouteCommand {
    List,
    /// Get a route by its **id** (not match_alias). Path: `/console/routes/{id}`.
    Get {
        /// Route id (ULID allocated by the daemon)
        id: String,
    },
    /// Remove a route by its **id** (not match_alias). Path: `/console/routes/{id}`.
    Remove {
        /// Route id (ULID allocated by the daemon)
        id: String,
    },
}

pub async fn run(console_addr: &str, args: RouteArgs, _output: &str) -> Result<()> {
    let client = ConsoleClient::new(console_addr);

    match args.command {
        RouteCommand::List => {
            let body = client
                .list_routes()
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        RouteCommand::Get { id } => {
            // Path uses route id — see `route_console_path`.
            debug_assert_eq!(route_console_path(&id), format!("/console/routes/{}", id));
            let body = client
                .get_route(&id)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        RouteCommand::Remove { id } => match client.delete_route(&id).await {
            Ok(()) => println!("Route {} removed", id),
            Err(ConsoleError::Http { status, body }) => {
                anyhow::bail!("failed: HTTP {} — {}", status, body);
            }
            Err(e) => anyhow::bail!("failed: {}", e),
        },
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn get_and_remove_document_id_not_alias() {
        let mut cmd = RouteArgs::command();
        let help = cmd.render_long_help().to_string();
        // Help must describe id (acceptance criterion).
        assert!(
            help.to_lowercase().contains("id"),
            "help should mention id: {help}"
        );
        // Subcommand args should be named `id`.
        let get = cmd.find_subcommand("get").expect("get subcommand");
        let args: Vec<_> = get
            .get_arguments()
            .map(|a| a.get_id().to_string())
            .collect();
        assert!(
            args.iter().any(|a| a == "id"),
            "get should take id, got {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "alias"),
            "get must not take alias: {args:?}"
        );

        let remove = cmd.find_subcommand("remove").expect("remove");
        let rargs: Vec<_> = remove
            .get_arguments()
            .map(|a| a.get_id().to_string())
            .collect();
        assert!(rargs.iter().any(|a| a == "id"));
        assert!(!rargs.iter().any(|a| a == "alias"));
    }

    #[test]
    fn route_path_helper_matches_console_contract() {
        assert_eq!(
            route_console_path("01HROUTEEXAMPLE000000000000"),
            "/console/routes/01HROUTEEXAMPLE000000000000"
        );
    }
}
