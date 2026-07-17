//! SSE parsing utilities built on top of `eventsource-stream`.

use std::pin::Pin;

use conduit_ir::error::ProviderError;
use futures::{Stream, StreamExt, TryStreamExt};
use reqwest::Response;

pub type SseStream = Pin<Box<dyn Stream<Item = Result<String, ProviderError>> + Send>>;

/// Convert a reqwest streaming response into a stream of SSE data lines.
/// Handles chunked transfer, UTF-8 boundary splits, and comment lines via
/// the `eventsource-stream` crate.
pub fn response_to_sse(response: Response) -> SseStream {
    use eventsource_stream::Eventsource;

    let stream = response
        .bytes_stream()
        .map_err(|e| ProviderError::Network(e.to_string()))
        .eventsource()
        .map_err(|e| ProviderError::Network(e.to_string()))
        .filter_map(|result| async move {
            match result {
                Ok(event) => {
                    // Skip comment events (data is empty) and retry events
                    if event.data.is_empty() || event.data == "[DONE]" {
                        if event.data == "[DONE]" {
                            Some(Ok("[DONE]".to_string()))
                        } else {
                            None
                        }
                    } else {
                        Some(Ok(event.data))
                    }
                }
                Err(e) => Some(Err(e)),
            }
        });

    Box::pin(stream)
}

#[cfg(test)]
mod tests {
    // Integration tests via wiremock live in tests/ directory
}
