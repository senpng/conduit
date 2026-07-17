use anyhow::Result;
use conduitctl::AdminClient;

pub async fn run(admin_addr: &str) -> Result<()> {
    let client = AdminClient::new(admin_addr);
    match client.health().await {
        Ok(body) => {
            println!("conduitd: {}", body.status);
            println!("version:  {}", body.version);
            if let Some(te) = body.trace_enabled {
                println!("trace:    {}", if te { "on" } else { "off" });
            }
            Ok(())
        }
        Err(conduitctl::AdminError::Http { status, .. }) => {
            anyhow::bail!("daemon returned HTTP {}", status)
        }
        Err(conduitctl::AdminError::Transport(e)) => {
            anyhow::bail!("cannot reach daemon at {}: {}", admin_addr, e)
        }
        Err(e) => anyhow::bail!("{}", e),
    }
}
