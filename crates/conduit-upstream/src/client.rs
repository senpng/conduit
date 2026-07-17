use std::{sync::OnceLock, time::Duration};

use reqwest::Client;

static GLOBAL_CLIENT: OnceLock<Client> = OnceLock::new();

/// Provides a shared global reqwest::Client for all upstream calls.
/// HTTP/2 multiplexing and connection pool are reused across all providers.
pub struct HttpClientFactory;

impl HttpClientFactory {
    /// Returns the shared global client. Initializes it on first call.
    pub fn get() -> &'static Client {
        GLOBAL_CLIENT.get_or_init(|| {
            Client::builder()
                .use_rustls_tls()
                .tcp_keepalive(Duration::from_secs(30))
                .pool_idle_timeout(Duration::from_secs(90))
                .pool_max_idle_per_host(20)
                .build()
                .expect("global reqwest client must build; TLS init failure is unrecoverable")
        })
    }
}
