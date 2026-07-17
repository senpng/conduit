//! Chrome-impersonating HTTP client for Claude OAuth relay.
//!
//! CLIProxyAPI uses `utls.HelloChrome_Auto` for Anthropic Messages traffic.
//! In Rust/wreq there is no symbol named `HelloChrome_Auto`; the equivalent is
//! **the latest Chrome profile shipped by the current `wreq-util` crate**
//! (utls Auto = “recommended / usually latest Chrome in this library version”).

use std::{sync::OnceLock, time::Duration};

use wreq::Client;
use wreq_util::{Emulation, Profile};

static CHROME_CLIENT: OnceLock<Client> = OnceLock::new();

/// Align with Go utls `HelloChrome_Auto`: latest Chrome preset in this wreq-util.
///
/// Note: `Emulation::ChromeN` associated consts are typed as [`Profile`] and
/// implement conversion into wreq's emulation config.
/// Bump when upgrading `wreq-util` if a newer `ChromeN` variant is available.
#[inline]
pub fn chrome_auto() -> Profile {
    // Same as Emulation::Chrome149 (const alias on Emulation → Profile).
    Profile::Chrome149
}

/// Shared Chrome-fingerprint client for `api.anthropic.com` OAuth Messages.
///
/// **TLS + HTTP/2 only** (`headers(false)`): CLIProxyAPI uses utls for the
/// ClientHello fingerprint and then sets **Claude Code** HTTP headers itself.
/// Enabling wreq's Chrome default headers would inject `sec-ch-ua` / Chrome UA
/// and break that parity.
pub fn chrome_client() -> &'static Client {
    CHROME_CLIENT.get_or_init(|| {
        Client::builder()
            .emulation(
                Emulation::builder()
                    .profile(chrome_auto())
                    .headers(false)
                    .http2(true)
                    .build(),
            )
            .connect_timeout(Duration::from_secs(5))
            .tcp_keepalive(Duration::from_secs(30))
            .pool_max_idle_per_host(20)
            .build()
            .expect("chrome impersonation client must build")
    })
}
