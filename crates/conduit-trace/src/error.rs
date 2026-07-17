use thiserror::Error;

/// Errors that can occur within the conduit-trace crate.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TraceError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("channel send error: channel closed or full")]
    ChannelFull,

    #[error("event not found: {0}")]
    NotFound(String),
}
