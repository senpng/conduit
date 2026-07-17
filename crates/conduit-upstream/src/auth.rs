use reqwest::RequestBuilder;
use secrecy::{ExposeSecret, SecretString};

/// Strategy for authenticating requests to upstream providers.
pub trait AuthStrategy: Send + Sync {
    fn apply(&self, builder: RequestBuilder, secret: &SecretString) -> RequestBuilder;
    fn name(&self) -> &'static str;
}

/// Bearer token authentication (Authorization: Bearer <token>)
/// Used by OpenAI, Grok, Claude OAuth, Codex OAuth, etc.
pub struct BearerAuth;

impl AuthStrategy for BearerAuth {
    fn apply(&self, builder: RequestBuilder, secret: &SecretString) -> RequestBuilder {
        builder.bearer_auth(secret.expose_secret())
    }
    fn name(&self) -> &'static str {
        "bearer"
    }
}

/// Custom header authentication (e.g., x-api-key for Anthropic API keys)
pub struct HeaderAuth {
    pub header_name: String,
}

impl HeaderAuth {
    pub fn new(header_name: impl Into<String>) -> Self {
        Self {
            header_name: header_name.into(),
        }
    }
    /// Creates the standard Anthropic `x-api-key` header auth.
    pub fn anthropic() -> Self {
        Self::new("x-api-key")
    }
}

impl AuthStrategy for HeaderAuth {
    fn apply(&self, builder: RequestBuilder, secret: &SecretString) -> RequestBuilder {
        builder.header(&self.header_name, secret.expose_secret())
    }
    fn name(&self) -> &'static str {
        "header"
    }
}

/// Apply bearer (or header) auth plus a fixed set of extra headers (OAuth metadata).
pub struct CompositeAuth {
    pub primary: Box<dyn AuthStrategy>,
    pub extra_headers: Vec<(String, String)>,
}

impl CompositeAuth {
    pub fn bearer_with_headers(extra_headers: Vec<(String, String)>) -> Self {
        Self {
            primary: Box::new(BearerAuth),
            extra_headers,
        }
    }

    pub fn header_with_headers(
        header_name: impl Into<String>,
        extra_headers: Vec<(String, String)>,
    ) -> Self {
        Self {
            primary: Box::new(HeaderAuth::new(header_name)),
            extra_headers,
        }
    }
}

impl AuthStrategy for CompositeAuth {
    fn apply(&self, mut builder: RequestBuilder, secret: &SecretString) -> RequestBuilder {
        builder = self.primary.apply(builder, secret);
        for (k, v) in &self.extra_headers {
            builder = builder.header(k, v);
        }
        builder
    }
    fn name(&self) -> &'static str {
        "composite"
    }
}
