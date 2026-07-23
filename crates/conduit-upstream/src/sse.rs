//! SSE parsing utilities built on top of `eventsource-stream`, plus stream
//! idle / overall timeout wrappers and transport-error classification.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use conduit_ir::error::ProviderError;
use futures::{Stream, StreamExt, TryStreamExt};
use pin_project_lite::pin_project;
use reqwest::Response;
use tokio::time::{Instant, Sleep};
use tracing::warn;

pub type SseStream = Pin<Box<dyn Stream<Item = Result<String, ProviderError>> + Send>>;

/// Timeouts applied while reading a streaming upstream body.
#[derive(Debug, Clone, Copy)]
pub struct StreamTimeoutOpts {
    /// Max quiet period with no body bytes / SSE events. `0` disables.
    pub idle_ms: u64,
    /// Hard cap for the whole stream (from wrapper creation). `0` disables.
    pub overall_ms: u64,
}

impl Default for StreamTimeoutOpts {
    fn default() -> Self {
        Self {
            // Match TimeoutConfig::default().stream_idle_ms — generous enough for
            // reasoning models that pause between tokens / tool-call rounds.
            idle_ms: 180_000,
            overall_ms: 300_000,
        }
    }
}

/// Map a `reqwest` error into `ProviderError`, preferring `Timeout` when the
/// client reports a timeout or the message looks like one.
pub fn map_reqwest_error(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        return ProviderError::Timeout;
    }
    classify_transport_message(&e.to_string())
}

/// Classify a free-form transport / SSE error string.
///
/// Timeout-like messages become `ProviderError::Timeout`; everything else is
/// `ProviderError::Network`.
pub fn classify_transport_message(msg: &str) -> ProviderError {
    if looks_like_timeout(msg) {
        ProviderError::Timeout
    } else {
        ProviderError::Network(msg.to_string())
    }
}

/// Heuristic: treat common timeout / deadline wording as timeout even when the
/// underlying crate did not set `is_timeout()`.
pub fn looks_like_timeout(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("deadline has elapsed")
        || lower.contains("deadline exceeded")
        || lower.contains("operation timed out")
        || lower.contains("idle timeout")
        || lower.contains("stream idle")
}

/// Convert a reqwest streaming response into a stream of SSE data lines, with
/// idle + overall timeouts. Handles chunked transfer, UTF-8 boundary splits,
/// and comment lines via the `eventsource-stream` crate.
///
/// Idle / overall watchdogs wrap the **byte** stream (before SSE parsing) so
/// that keepalives such as empty SSE comments (`:\n\n`) still reset the idle
/// timer — `eventsource-stream` discards comments and never yields them as
/// events.
pub fn response_to_sse(response: Response, timeouts: StreamTimeoutOpts) -> SseStream {
    use eventsource_stream::Eventsource;

    let byte_stream = response.bytes_stream().map_err(map_reqwest_error);
    // Timeouts on body bytes so SSE comments / partial frames count as activity.
    let timed = with_stream_timeouts(byte_stream, timeouts);

    let stream = timed
        .eventsource()
        .map_err(map_eventsource_error)
        .filter_map(|result| async move {
            match result {
                Ok(event) => {
                    // Skip empty data events; keep [DONE] as a marker.
                    // (True SSE comments never surface here — see byte-level
                    // idle reset above.)
                    if event.data.is_empty() {
                        None
                    } else if event.data == "[DONE]" {
                        Some(Ok("[DONE]".to_string()))
                    } else {
                        Some(Ok(event.data))
                    }
                }
                Err(e) => Some(Err(e)),
            }
        });

    Box::pin(stream)
}

/// Wrap any `Result<T, ProviderError>` stream with idle + overall timeouts.
///
/// Any ready item (including `Ok` with empty payload / keepalive bytes) resets
/// the idle timer. Apply this to the raw body byte stream when upstream may
/// send SSE comments that SSE parsers discard.
pub fn with_stream_timeouts<S, T>(inner: S, timeouts: StreamTimeoutOpts) -> StreamTimeouts<S>
where
    S: Stream<Item = Result<T, ProviderError>>,
{
    StreamTimeouts::new(inner, timeouts)
}

fn map_eventsource_error(e: eventsource_stream::EventStreamError<ProviderError>) -> ProviderError {
    match e {
        eventsource_stream::EventStreamError::Transport(inner) => inner,
        other => classify_transport_message(&other.to_string()),
    }
}

pin_project! {
    /// Wraps a stream and fails with `ProviderError::Timeout` when either:
    /// - no item arrives for `idle_ms`, or
    /// - total elapsed time since construction exceeds `overall_ms`.
    pub struct StreamTimeouts<S> {
        #[pin]
        inner: S,
        #[pin]
        idle_sleep: Sleep,
        #[pin]
        overall_sleep: Sleep,
        idle: Duration,
        idle_enabled: bool,
        overall_enabled: bool,
        finished: bool,
    }
}

impl<S> StreamTimeouts<S> {
    pub fn new(inner: S, timeouts: StreamTimeoutOpts) -> Self {
        let idle_enabled = timeouts.idle_ms > 0;
        let overall_enabled = timeouts.overall_ms > 0;
        // Far-future sleep when a timer is disabled so we never spuriously fire.
        let far = Duration::from_secs(365 * 24 * 3600);
        let idle = if idle_enabled {
            Duration::from_millis(timeouts.idle_ms)
        } else {
            far
        };
        let overall = if overall_enabled {
            Duration::from_millis(timeouts.overall_ms)
        } else {
            far
        };
        Self {
            inner,
            idle_sleep: tokio::time::sleep(idle),
            overall_sleep: tokio::time::sleep(overall),
            idle,
            idle_enabled,
            overall_enabled,
            finished: false,
        }
    }

    fn reset_idle(self: Pin<&mut Self>) {
        let this = self.project();
        if *this.idle_enabled {
            this.idle_sleep
                .reset(Instant::now() + *this.idle);
        }
    }
}

impl<S, T> Stream for StreamTimeouts<S>
where
    S: Stream<Item = Result<T, ProviderError>>,
{
    type Item = Result<T, ProviderError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if *self.as_mut().project().finished {
            return Poll::Ready(None);
        }

        // 1) Prefer delivering an inner item when ready.
        let inner_poll = self.as_mut().project().inner.poll_next(cx);
        match inner_poll {
            Poll::Ready(Some(item)) => {
                // Any progress (including Ok keepalive / error) resets idle.
                self.as_mut().reset_idle();
                if item.is_err() {
                    *self.as_mut().project().finished = true;
                }
                return Poll::Ready(Some(item));
            }
            Poll::Ready(None) => {
                *self.as_mut().project().finished = true;
                return Poll::Ready(None);
            }
            Poll::Pending => {}
        }

        // Snapshot flags before further projection (non-pin fields).
        let overall_enabled = *self.as_mut().project().overall_enabled;
        let idle_enabled = *self.as_mut().project().idle_enabled;
        let idle_ms = self.as_mut().project().idle.as_millis() as u64;

        // 2) Overall deadline.
        if overall_enabled {
            if let Poll::Ready(()) = self.as_mut().project().overall_sleep.poll(cx) {
                *self.as_mut().project().finished = true;
                warn!(
                    idle_enabled,
                    overall_enabled, "upstream stream overall timeout"
                );
                return Poll::Ready(Some(Err(ProviderError::Timeout)));
            }
        }

        // 3) Idle quiet period.
        if idle_enabled {
            if let Poll::Ready(()) = self.as_mut().project().idle_sleep.poll(cx) {
                *self.as_mut().project().finished = true;
                warn!(idle_ms, "upstream stream idle timeout");
                return Poll::Ready(Some(Err(ProviderError::Timeout)));
            }
        }

        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{self, StreamExt};

    fn pin_timeouts<S>(inner: S, opts: StreamTimeoutOpts) -> Pin<Box<StreamTimeouts<S>>>
    where
        S: Stream<Item = Result<String, ProviderError>>,
    {
        Box::pin(with_stream_timeouts(inner, opts))
    }

    #[test]
    fn looks_like_timeout_matches_common_phrases() {
        assert!(looks_like_timeout(
            "error sending request for url: operation timed out"
        ));
        assert!(looks_like_timeout(
            "error decoding response body: deadline has elapsed"
        ));
        assert!(looks_like_timeout("Idle Timeout"));
        assert!(!looks_like_timeout("connection reset by peer"));
        assert!(!looks_like_timeout("broken pipe"));
    }

    #[test]
    fn classify_transport_message_timeout_vs_network() {
        assert!(matches!(
            classify_transport_message("operation timed out"),
            ProviderError::Timeout
        ));
        match classify_transport_message("connection reset by peer") {
            ProviderError::Network(s) => assert!(s.contains("connection reset")),
            other => panic!("expected Network, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn idle_timeout_fires_when_stream_stalls() {
        // Never yields — idle should fire.
        let pending = stream::pending::<Result<String, ProviderError>>();
        let mut s = pin_timeouts(
            pending,
            StreamTimeoutOpts {
                idle_ms: 30,
                overall_ms: 0,
            },
        );
        let item = s.next().await;
        assert!(matches!(item, Some(Err(ProviderError::Timeout))));
    }

    #[tokio::test]
    async fn overall_timeout_fires_before_idle() {
        let pending = stream::pending::<Result<String, ProviderError>>();
        let mut s = pin_timeouts(
            pending,
            StreamTimeoutOpts {
                idle_ms: 10_000,
                overall_ms: 40,
            },
        );
        let started = Instant::now();
        let item = s.next().await;
        assert!(matches!(item, Some(Err(ProviderError::Timeout))));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "overall should fire well under idle"
        );
    }

    #[tokio::test]
    async fn data_resets_idle_timer() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use std::task::Poll as TaskPoll;

        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        // Yield one item, then stall forever. After the first item the idle
        // timer resets, so a subsequent stall should fire idle.
        let inner = stream::poll_fn(move |_cx| {
            let i = n2.fetch_add(1, Ordering::SeqCst);
            if i == 0 {
                TaskPoll::Ready(Some(Ok::<_, ProviderError>("first".into())))
            } else {
                TaskPoll::Pending
            }
        });
        let mut s = pin_timeouts(
            inner,
            StreamTimeoutOpts {
                idle_ms: 50,
                overall_ms: 0,
            },
        );
        assert_eq!(s.next().await.unwrap().unwrap(), "first");
        let item = s.next().await;
        assert!(matches!(item, Some(Err(ProviderError::Timeout))));
    }

    #[tokio::test]
    async fn successful_items_pass_through() {
        let inner = stream::iter(vec![
            Ok::<_, ProviderError>("a".into()),
            Ok("b".into()),
        ]);
        let mut s = pin_timeouts(
            inner,
            StreamTimeoutOpts {
                idle_ms: 1_000,
                overall_ms: 5_000,
            },
        );
        assert_eq!(s.next().await.unwrap().unwrap(), "a");
        assert_eq!(s.next().await.unwrap().unwrap(), "b");
        assert!(s.next().await.is_none());
    }

    #[tokio::test]
    async fn zero_timeouts_disable_watchdogs() {
        let inner = stream::iter(vec![Ok::<_, ProviderError>("ok".into())]);
        let mut s = pin_timeouts(
            inner,
            StreamTimeoutOpts {
                idle_ms: 0,
                overall_ms: 0,
            },
        );
        assert_eq!(s.next().await.unwrap().unwrap(), "ok");
        assert!(s.next().await.is_none());
    }

    /// Empty / keepalive body chunks must reset idle — SSE comment keepalives
    /// (`:\n\n`) arrive as bytes but are discarded by the SSE parser, so the
    /// watchdog has to sit on the byte stream.
    #[tokio::test]
    async fn keepalive_bytes_reset_idle_timer() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use std::task::Poll as TaskPoll;

        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        // Yield a keepalive chunk (empty Vec), then stall. Idle must not fire
        // until idle_ms after the keepalive, not from construction time.
        let inner = stream::poll_fn(move |_cx| {
            let i = n2.fetch_add(1, Ordering::SeqCst);
            if i == 0 {
                TaskPoll::Ready(Some(Ok::<_, ProviderError>(Vec::<u8>::new())))
            } else {
                TaskPoll::Pending
            }
        });
        let mut s = Box::pin(with_stream_timeouts(
            inner,
            StreamTimeoutOpts {
                idle_ms: 80,
                overall_ms: 0,
            },
        ));
        // Drain the keepalive chunk.
        assert!(s.next().await.unwrap().unwrap().is_empty());
        let started = Instant::now();
        let item = s.next().await;
        assert!(matches!(item, Some(Err(ProviderError::Timeout))));
        // If keepalive had not reset idle, this would have fired ~immediately
        // relative to construction; require a real wait near idle_ms.
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "idle should only fire after the post-keepalive quiet period"
        );
    }

    #[test]
    fn default_idle_matches_timeout_config_scale() {
        // Keep StreamTimeoutOpts and TimeoutConfig defaults aligned.
        assert_eq!(StreamTimeoutOpts::default().idle_ms, 180_000);
    }
}
