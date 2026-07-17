use anyhow::Result;
use conduitctl::ConsoleClient;

pub async fn run(console_addr: &str) -> Result<()> {
    let client = ConsoleClient::new(console_addr);
    match client.health().await {
        Ok(body) => {
            println!("conduitd: {}", body.status);
            println!("version:  {}", body.version);
            if let Some(te) = body.trace_enabled {
                println!("trace:    {}", if te { "on" } else { "off" });
            }
            Ok(())
        }
        Err(conduitctl::ConsoleError::Http { status, .. }) => {
            anyhow::bail!("daemon returned HTTP {}", status)
        }
        Err(conduitctl::ConsoleError::Transport(e)) => {
            anyhow::bail!("cannot reach daemon at {}: {}", console_addr, e)
        }
        Err(e) => anyhow::bail!("{}", e),
    }
}
