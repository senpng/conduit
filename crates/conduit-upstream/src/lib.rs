pub mod auth;
pub mod claude_oauth;
pub mod client;
pub mod provider;
pub mod rate_limit;
pub mod sse;

pub use auth::{AuthStrategy, BearerAuth, CompositeAuth, HeaderAuth};
pub use client::HttpClientFactory;
pub use provider::{ProviderClient, ProviderClientConfig, TimeoutConfig, UpstreamPath};
pub use rate_limit::RateLimitHeaderSink;
pub use sse::{
    classify_transport_message, map_reqwest_error, response_to_sse, StreamTimeoutOpts,
};
