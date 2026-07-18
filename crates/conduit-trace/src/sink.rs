use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, RwLock,
};

use async_trait::async_trait;
use conduit_ir::trace::TraceEvent;
use tokio::{
    sync::{mpsc, Notify},
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
    /// Signals the background loop to flush and exit during graceful shutdown.
    shutdown: Arc<Notify>,
    /// Notified by the background loop once it has flushed and exited.
    drained: Arc<Notify>,
    /// Set once the first [`drain`](Self::drain) has completed the shutdown
    /// handshake, so repeat calls return immediately instead of hanging on a
    /// loop that has already exited.
    drain_done: Arc<AtomicBool>,
}

impl TraceSink {
    /// Spawn the background flush task and return a sink + its join handle.
    ///
    /// Tracing starts **enabled**. Call [`set_enabled`] after construction to
    /// honor operator config.
    ///
    /// [`drain`](Self::drain) coordinates a clean flush-and-exit with the task
    /// via internal signals, so the returned [`JoinHandle`] is only for callers
    /// that want to observe or abort the task; awaiting it is not required.
    pub async fn start(store: Arc<TraceStore>) -> (Self, JoinHandle<()>) {
        let subscribers: Arc<RwLock<Vec<Arc<dyn TraceSubscriber>>>> =
            Arc::new(RwLock::new(Vec::new()));
        let (tx, rx) = mpsc::channel::<TraceEvent>(8096);
        let shutdown = Arc::new(Notify::new());
        let drained = Arc::new(Notify::new());
        let handle = tokio::spawn(sink_loop(
            store,
            rx,
            subscribers.clone(),
            shutdown.clone(),
            drained.clone(),
        ));
        (
            Self {
                tx,
                subscribers,
                enabled: Arc::new(AtomicBool::new(true)),
                shutdown,
                drained,
                drain_done: Arc::new(AtomicBool::new(false)),
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

    /// Flush all in-flight events and wait for the background task to exit.
    ///
    /// Called during graceful shutdown. Signals the background loop to drain
    /// its channel, flush the final batch, and exit; then awaits confirmation.
    /// Unlike a fixed sleep, this returns exactly when the last event is
    /// durably appended.
    ///
    /// Idempotent: the shutdown handshake runs only once. After it has
    /// completed, further calls return immediately (the loop has already
    /// flushed and exited, so there is nothing left to wait for). Calling it
    /// concurrently from two tasks is not supported — production drains once.
    pub async fn drain(&self) {
        // A prior drain already flushed-and-exited the loop: nothing to wait on,
        // and signalling `shutdown` again would park a permit no one consumes
        // while we block forever on `drained`. Return immediately instead.
        if self.drain_done.load(Ordering::Acquire) {
            return;
        }

        // Register interest in the drained signal *before* asking the loop to
        // shut down, so a fast loop cannot fire `notify_waiters` between our
        // `notify_one` and the point where we start awaiting (lost wakeup).
        let drained = self.drained.notified();
        self.shutdown.notify_one();
        drained.await;

        // Publish completion so repeat callers take the fast path above.
        self.drain_done.store(true, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Background flush loop
// ---------------------------------------------------------------------------

async fn sink_loop(
    store: Arc<TraceStore>,
    mut rx: mpsc::Receiver<TraceEvent>,
    subscribers: Arc<RwLock<Vec<Arc<dyn TraceSubscriber>>>>,
    shutdown: Arc<Notify>,
    drained: Arc<Notify>,
) {
    let mut batch: Vec<TraceEvent> = Vec::with_capacity(128);
    let mut flush_ticker = interval(Duration::from_millis(50));

    loop {
        tokio::select! {
            _ = shutdown.notified() => {
                // Graceful shutdown: drain everything already queued, flush, exit.
                // `try_recv` never blocks — senders may still be alive.
                while let Ok(event) = rx.try_recv() {
                    batch.push(event);
                }
                if !batch.is_empty() {
                    flush(&store, &mut batch, &subscribers).await;
                }
                drained.notify_waiters();
                return;
            }
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
                        drained.notify_waiters();
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
    if items.is_empty() {
        return;
    }

    // Two steps, so we can broadcast events that are DURABLE even if the index
    // write later fails. The log is the source of truth; the SQLite index is a
    // rebuildable derived view (recovered on next open via checkpoint replay).
    //
    // Step 1 — append the whole batch to the segment log (single fsync under
    // Fsync mode). If this fails nothing is durable: skip subscribers entirely.
    let offsets = match store.log.append(&items).await {
        Ok(offsets) if offsets.len() == items.len() => offsets,
        Ok(offsets) => {
            error!(
                batch_len = items.len(),
                offset_len = offsets.len(),
                "TraceSink: log.append returned a mismatched offset count; dropping batch"
            );
            return;
        }
        Err(e) => {
            // Nothing reached durable storage — never silently drop, log it.
            error!(
                batch_len = items.len(),
                error = %e,
                "TraceSink: failed to append batch to log"
            );
            return;
        }
    };

    // The events are now durably on disk. Notify subscribers regardless of the
    // index outcome: the live SSE tail must not miss events that ARE recorded.
    let subs: Vec<Arc<dyn TraceSubscriber>> = subscribers
        .read()
        .expect("subscribers lock poisoned")
        .clone();
    if !subs.is_empty() {
        for event in &items {
            for sub in &subs {
                sub.on_event(event).await;
            }
        }
    }

    // Step 2 — index the batch in one transaction. On failure the events remain
    // in the durable log and are re-indexed on the next open; we only lose fast
    // metadata queries for them until then, so log and continue.
    let rows: Vec<crate::TraceIndexRow> = items
        .iter()
        .zip(offsets)
        .map(|(event, offset)| {
            crate::event_to_index_row(event, offset.segment, offset.offset)
        })
        .collect();
    if let Err(e) = store.index.insert_batch(&rows).await {
        error!(
            batch_len = rows.len(),
            error = %e,
            "TraceSink: batch is durable in the log but index insert failed; \
             it will be recovered on next open"
        );
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

    /// `drain` must return only after every queued event is durably persisted —
    /// deterministically, without relying on a fixed sleep. Enqueue a burst,
    /// drain, then assert all rows are present with no post-drain sleep.
    #[tokio::test]
    async fn drain_flushes_all_queued_events_deterministically() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TraceStore::open(tmp.path().to_path_buf()).await.unwrap());
        let (sink, _handle) = TraceSink::start(store.clone()).await;

        const N: usize = 250; // spans multiple 100-event flush batches
        for i in 0..N {
            sink.send(TraceEvent::new(TraceEventKind::RequestReceived {
                downstream_key_id: Some(format!("dk-{i}")),
                alias: "m".into(),
                stream: false,
                request: serde_json::json!({}),
                request_ir: None,
                wire_format: None,
                request_headers: None,
            }))
            .unwrap();
        }

        // No sleep: drain returns exactly when the last batch is persisted.
        sink.drain().await;

        let rows = store
            .query(&TraceFilter {
                limit: 1000,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), N, "drain must persist every queued event");
    }

    /// A second `drain()` must return immediately, not hang. Before the
    /// idempotency guard, the repeat call parked a `shutdown` permit no one
    /// consumed and blocked forever on `drained`.
    #[tokio::test]
    async fn drain_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TraceStore::open(tmp.path().to_path_buf()).await.unwrap());
        let (sink, _handle) = TraceSink::start(store.clone()).await;

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

        sink.drain().await;
        // Would hang forever without the guard; a timeout turns a hang into a
        // test failure instead of a stuck suite.
        tokio::time::timeout(Duration::from_secs(5), sink.drain())
            .await
            .expect("second drain must return immediately, not hang");
    }

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
