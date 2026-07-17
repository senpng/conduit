use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};

use async_trait::async_trait;
use conduit_ir::trace::TraceEvent;
use tokio::{
    sync::mpsc,
    task::JoinHandle,
    time::{interval, Duration},
};
use tracing::error;

use crate::{error::TraceError, TraceStore};

// ---------------------------------------------------------------------------
// TraceSubscriber
// ---------------------------------------------------------------------------

/// Receives events after they have been successfully written to the trace store.
///
/// Implementations are called in-order after a successful `store.append`.
/// If the write fails the subscribers are **not** called.
#[async_trait]
pub trait TraceSubscriber: Send + Sync + 'static {
    async fn on_event(&self, ev: &TraceEvent);
}

// ---------------------------------------------------------------------------
// TraceSink
// ---------------------------------------------------------------------------

/// A non-blocking send handle to the background trace writer.
///
/// Create one [`TraceSink`] per application and share it via `Arc`.  The
/// `start` method spawns a background task that batches events and flushes them
/// to the [`TraceStore`] every 50 ms or when the batch reaches 100 events.
///
/// When **disabled** (`set_enabled(false)`), [`send`] is a no-op success so the
/// pipeline hot path stays cheap and request **usage** can still be recorded
/// independently of traces.
///
/// If the channel is full while enabled, `send` returns
/// `Err(TraceError::ChannelFull)` so the caller can log the drop — events are
/// **never silently discarded** while tracing is on.
pub struct TraceSink {
    tx: mpsc::Sender<TraceEvent>,
    subscribers: Arc<RwLock<Vec<Arc<dyn TraceSubscriber>>>>,
    enabled: Arc<AtomicBool>,
}

impl TraceSink {
    /// Spawn the background flush task and return a sink + its join handle.
    ///
    /// Tracing starts **enabled**. Call [`set_enabled`] after construction to
    /// honor operator config.
    pub async fn start(store: Arc<TraceStore>) -> (Self, JoinHandle<()>) {
        let subscribers: Arc<RwLock<Vec<Arc<dyn TraceSubscriber>>>> =
            Arc::new(RwLock::new(Vec::new()));
        let (tx, rx) = mpsc::channel::<TraceEvent>(8096);
        let handle = tokio::spawn(sink_loop(store, rx, subscribers.clone()));
        (
            Self {
                tx,
                subscribers,
                enabled: Arc::new(AtomicBool::new(true)),
            },
            handle,
        )
    }

    /// Whether new events are currently accepted for persistence.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Enable or disable accepting new trace events (lock-free, hot-path safe).
    ///
    /// Does not delete existing segments/index rows. In-flight channel items
    /// still flush after disable until the queue drains.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Register a subscriber to receive events after successful disk writes.
    pub fn register(&self, sub: Arc<dyn TraceSubscriber>) {
        self.subscribers
            .write()
            .expect("subscribers lock poisoned")
            .push(sub);
    }

    /// Send a trace event to the background writer.
    ///
    /// When disabled, returns `Ok(())` without enqueueing.
    /// When enabled and the channel is full, returns
    /// `Err(TraceError::ChannelFull)`.
    pub fn send(&self, event: TraceEvent) -> Result<(), TraceError> {
        if !self.is_enabled() {
            return Ok(());
        }
        self.tx.try_send(event).map_err(|_| TraceError::ChannelFull)
    }

    /// Wait for the in-flight channel to drain and the last batch to flush.
    ///
    /// Called during graceful shutdown: drop the sender side (done externally
    /// by dropping the `TraceSink`) then await this method so the background
    /// task exits cleanly.  Here we give the background loop a chance to
    /// process its final tick.
    pub async fn drain(&self) {
        // Give the background loop enough time to process remaining events.
        // The loop exits when the channel is closed (sender dropped), but
        // since we still hold a reference, we just wait for the queue to empty.
        let mut attempts = 0u32;
        loop {
            if self.tx.capacity() == self.tx.max_capacity() || attempts >= 100 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            attempts += 1;
        }
        // Final sleep to let the background flush tick fire.
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

// ---------------------------------------------------------------------------
// Background flush loop
// ---------------------------------------------------------------------------

async fn sink_loop(
    store: Arc<TraceStore>,
    mut rx: mpsc::Receiver<TraceEvent>,
    subscribers: Arc<RwLock<Vec<Arc<dyn TraceSubscriber>>>>,
) {
    let mut batch: Vec<TraceEvent> = Vec::with_capacity(128);
    let mut flush_ticker = interval(Duration::from_millis(50));

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Some(event) => {
                        // Stream deltas should reach live tail with minimal delay.
                        let urgent = matches!(
                            event.kind,
                            conduit_ir::trace::TraceEventKind::StreamDelta { .. }
                        );
                        batch.push(event);
                        if urgent || batch.len() >= 100 {
                            flush(&store, &mut batch, &subscribers).await;
                        }
                    }
                    None => {
                        // Channel closed; flush remaining and exit.
                        if !batch.is_empty() {
                            flush(&store, &mut batch, &subscribers).await;
                        }
                        return;
                    }
                }
            }
            _ = flush_ticker.tick() => {
                if !batch.is_empty() {
                    flush(&store, &mut batch, &subscribers).await;
                }
            }
        }
    }
}

async fn flush(
    store: &Arc<TraceStore>,
    batch: &mut Vec<TraceEvent>,
    subscribers: &Arc<RwLock<Vec<Arc<dyn TraceSubscriber>>>>,
) {
    let items: Vec<TraceEvent> = std::mem::take(batch);
    for event in items {
        match store.append(&event).await {
            Ok(()) => {
                // Clone the subscriber list before awaiting to avoid holding the
                // sync lock across an async boundary.
                let subs: Vec<Arc<dyn TraceSubscriber>> = subscribers
                    .read()
                    .expect("subscribers lock poisoned")
                    .clone();
                for sub in &subs {
                    sub.on_event(&event).await;
                }
            }
            Err(e) => {
                // We NEVER silently drop — log the error so operators can act.
                // Subscribers are NOT called: the event was not written.
                error!(
                    event_id = %event.id,
                    error = %e,
                    "TraceSink: failed to append event to store"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use conduit_ir::trace::TraceEventKind;

    use super::*;
    use crate::TraceFilter;

    #[tokio::test]
    async fn send_and_flush() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TraceStore::open(tmp.path().to_path_buf()).await.unwrap());
        let (sink, _handle) = TraceSink::start(store.clone()).await;

        let event = TraceEvent::new(TraceEventKind::RequestReceived {
            downstream_key_id: Some("dk-sink".into()),
            alias: "claude-3-5-sonnet".into(),
            stream: false,
            request: serde_json::json!({}),
            request_ir: None,
            wire_format: None,
            request_headers: None,
        });
        sink.send(event).unwrap();

        // Allow the background task to flush.
        tokio::time::sleep(Duration::from_millis(200)).await;

        let rows = store
            .query(&TraceFilter {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn disabled_send_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TraceStore::open(tmp.path().to_path_buf()).await.unwrap());
        let (sink, _handle) = TraceSink::start(store.clone()).await;
        assert!(sink.is_enabled());
        sink.set_enabled(false);
        assert!(!sink.is_enabled());

        sink.send(TraceEvent::new(TraceEventKind::RequestReceived {
            downstream_key_id: Some("dk".into()),
            alias: "m".into(),
            stream: false,
            request: serde_json::json!({}),
            request_ir: None,
            wire_format: None,
            request_headers: None,
        }))
        .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
        let rows = store
            .query(&TraceFilter {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(rows.is_empty(), "disabled sink must not persist events");
    }

    struct CounterSubscriber {
        count: Arc<AtomicU64>,
    }

    #[async_trait]
    impl TraceSubscriber for CounterSubscriber {
        async fn on_event(&self, _ev: &TraceEvent) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn subscriber_called_on_successful_write() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TraceStore::open(tmp.path().to_path_buf()).await.unwrap());
        let (sink, _handle) = TraceSink::start(store.clone()).await;

        let counter = Arc::new(AtomicU64::new(0));
        sink.register(Arc::new(CounterSubscriber {
            count: counter.clone(),
        }));

        let event = TraceEvent::new(TraceEventKind::RequestReceived {
            downstream_key_id: Some("dk-sub".into()),
            alias: "gpt-4o".into(),
            stream: false,
            request: serde_json::json!({}),
            request_ir: None,
            wire_format: None,
            request_headers: None,
        });
        sink.send(event).unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }
}
