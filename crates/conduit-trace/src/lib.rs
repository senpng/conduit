//! conduit-trace — Append-only segmented event log for Conduit v2.
//!
//! This crate provides the "flight data recorder" for the gateway:
//!
//! * **[`LogWriter`] / [`LogReader`]** — zstd-compressed, length-prefixed,
//!   segmented on-disk log (one file per day, rotating at 64 MiB).
//! * **[`TraceIndex`]** — SQLite index for fast metadata queries.
//! * **[`TraceSink`]** — mpsc channel + background batching flush task.
//! * **[`TraceStore`]** — high-level facade over all of the above.

pub mod error;
pub mod index;
pub mod log;
pub mod schema;
pub mod sink;

use std::path::PathBuf;

use conduit_ir::trace::TraceEvent;
pub use error::TraceError;
use futures::StreamExt;
pub use index::{TraceFilter, TraceIndex};
pub use log::{DurabilityMode, LogOffset, LogReader, LogWriter};
pub use schema::{event_to_index_row, TraceIndexRow};
pub use sink::{TraceSink, TraceSubscriber};

// ---------------------------------------------------------------------------
// TraceStore — main facade
// ---------------------------------------------------------------------------

/// The primary entry point for reading and writing trace data.
///
/// Holds a [`LogWriter`] and a [`TraceIndex`].
/// Create via [`TraceStore::open`] and share via `Arc<TraceStore>`.
pub struct TraceStore {
    pub log: LogWriter,
    pub index: TraceIndex,
    pub data_dir: PathBuf,
}

impl TraceStore {
    /// Open (or create) a store at `data_dir`.
    ///
    /// Creates the following layout:
    /// ```text
    /// data_dir/
    ///   segments/          ← segment files written by LogWriter
    ///   trace.db           ← SQLite metadata index
    /// ```
    pub async fn open(data_dir: PathBuf) -> Result<Self, TraceError> {
        tokio::fs::create_dir_all(&data_dir)
            .await
            .map_err(TraceError::Io)?;

        let log = LogWriter::new(data_dir.clone()).await?;
        let index = TraceIndex::open(&data_dir.join("trace.db")).await?;

        let store = Self {
            log,
            index,
            data_dir,
        };
        store.rebuild_missing_index_rows().await?;

        Ok(store)
    }

    /// Restore index entries for events that reached the append-only log before
    /// a SQLite index write failed or the process was interrupted.
    async fn rebuild_missing_index_rows(&self) -> Result<(), TraceError> {
        let reader = LogReader::new(self.data_dir.clone());
        let mut events = Box::pin(reader.stream_all_with_offsets());
        while let Some(item) = events.next().await {
            let (event, offset) = item?;
            let row = event_to_index_row(&event, offset.segment, offset.offset);
            self.index.insert(&row).await?;
        }
        Ok(())
    }

    /// Append a single [`TraceEvent`] to the log and index.
    pub async fn append(&self, event: &TraceEvent) -> Result<(), TraceError> {
        // Write to the segment log and get back the offset.
        let offsets = self.log.append(std::slice::from_ref(event)).await?;
        let offset = offsets
            .into_iter()
            .next()
            .ok_or_else(|| TraceError::Serialization("log.append returned no offsets".into()))?;

        // Index the metadata row.
        let row = event_to_index_row(event, offset.segment.clone(), offset.offset);
        self.index.insert(&row).await?;

        Ok(())
    }

    /// Query the metadata index.
    pub async fn query(&self, filter: &TraceFilter) -> Result<Vec<TraceIndexRow>, TraceError> {
        self.index.query(filter).await
    }

    /// Fetch the full [`TraceEvent`] for a given event ID by looking up the log
    /// offset from the index and reading the segment file.
    pub async fn get_full(&self, id: &str) -> Result<Option<TraceEvent>, TraceError> {
        let row = match self.index.get(id).await? {
            Some(r) => r,
            None => return Ok(None),
        };

        let reader = LogReader::new(self.data_dir.clone());
        let event = reader
            .read_at(LogOffset {
                segment: row.segment,
                offset: row.offset as u64,
            })
            .await?;

        Ok(Some(event))
    }

    /// Load the complete audit trail for a request.
    ///
    /// Accepts either an event `id` or a shared `trace_id`. Returns all events
    /// belonging to that request in chronological order (request body, routing,
    /// response body, usage, errors).
    pub async fn get_bundle(
        &self,
        id_or_trace_id: &str,
    ) -> Result<Option<Vec<TraceEvent>>, TraceError> {
        // Prefer direct event lookup to resolve its trace_id.
        let anchor = if let Some(row) = self.index.get(id_or_trace_id).await? {
            row
        } else {
            // Treat input as trace_id.
            let rows = self.index.list_by_trace_id(id_or_trace_id).await?;
            if rows.is_empty() {
                return Ok(None);
            }
            rows.into_iter().next().unwrap()
        };

        let tid = if anchor.trace_id.is_empty() {
            anchor.id.clone()
        } else {
            anchor.trace_id.clone()
        };

        let rows = self.index.list_by_trace_id(&tid).await?;
        if rows.is_empty() {
            return Ok(None);
        }

        let reader = LogReader::new(self.data_dir.clone());
        let mut events = Vec::with_capacity(rows.len());
        for row in rows {
            let event = reader
                .read_at(LogOffset {
                    segment: row.segment,
                    offset: row.offset as u64,
                })
                .await?;
            events.push(event);
        }
        Ok(Some(events))
    }

    /// Flush and fsync the active log segment to durable storage.
    ///
    /// Call this during graceful shutdown after draining the `TraceSink`.
    pub async fn shutdown(&self) -> Result<(), TraceError> {
        self.log.shutdown().await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduit_ir::trace::TraceEventKind;

    use super::*;

    fn sample_event() -> TraceEvent {
        TraceEvent::with_trace_id(
            ulid::Ulid::new().to_string(),
            TraceEventKind::RequestReceived {
                downstream_key_id: Some("dk-store-test".into()),
                alias: "gpt-4o".into(),
                stream: false,
                request: serde_json::json!({
                    "alias": "gpt-4o",
                    "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}]
                }),
                request_ir: None,
                wire_format: None,
                request_headers: None,
            },
        )
    }

    #[tokio::test]
    async fn append_and_get_bundle_includes_request_body() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TraceStore::open(tmp.path().to_path_buf()).await.unwrap();
        let event = sample_event();
        let tid = event.trace_id.clone();
        store.append(&event).await.unwrap();
        store
            .append(&TraceEvent::with_trace_id(
                tid.clone(),
                TraceEventKind::UpstreamResponse {
                    status: 200,
                    latency_ms: 10,
                    ttfb_ms: None,
                    response: Some(serde_json::json!({
                        "choices": [{"message": {"content": "hello"}}]
                    })),
                    wire_format: None,
                    stream: false,
                    stream_frames: None,
                    response_headers: None,
                    upstream_request_headers: None,
                    upstream_response_headers: None,
                },
            ))
            .await
            .unwrap();

        let bundle = store.get_bundle(&tid).await.unwrap().unwrap();
        assert_eq!(bundle.len(), 2);
        match &bundle[0].kind {
            TraceEventKind::RequestReceived { request, .. } => {
                assert_eq!(request["messages"][0]["content"][0]["text"], "hi");
            }
            _ => panic!("expected request_received"),
        }
        match &bundle[1].kind {
            TraceEventKind::UpstreamResponse {
                response: Some(r), ..
            } => {
                assert_eq!(r["choices"][0]["message"]["content"], "hello");
            }
            _ => panic!("expected upstream_response with body"),
        }
    }

    #[tokio::test]
    async fn open_creates_directory_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("conduit_data");
        let _store = TraceStore::open(data_dir.clone()).await.unwrap();
        assert!(data_dir.join("trace.db").exists());
        assert!(data_dir.join("segments").exists());
    }

    #[tokio::test]
    async fn append_and_get_full() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TraceStore::open(tmp.path().to_path_buf()).await.unwrap();

        let event = sample_event();
        store.append(&event).await.unwrap();

        let back = store.get_full(&event.id).await.unwrap().unwrap();
        assert_eq!(back.id, event.id);
    }

    #[tokio::test]
    async fn query_returns_appended_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TraceStore::open(tmp.path().to_path_buf()).await.unwrap();

        for _ in 0..5 {
            store.append(&sample_event()).await.unwrap();
        }

        let rows = store
            .query(&TraceFilter {
                limit: 20,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 5);
    }

    #[tokio::test]
    async fn get_full_missing_id_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = TraceStore::open(tmp.path().to_path_buf()).await.unwrap();
        let result = store.get_full("NONEXISTENT_ID").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn open_rebuilds_index_for_existing_segment_events() {
        let tmp = tempfile::tempdir().unwrap();
        let event = sample_event();
        let event_id = event.id.clone();
        let writer = LogWriter::new(tmp.path().to_path_buf()).await.unwrap();
        writer.append(&[event]).await.unwrap();
        drop(writer);

        let store = TraceStore::open(tmp.path().to_path_buf()).await.unwrap();
        let restored = store.get_full(&event_id).await.unwrap();
        assert!(
            restored.is_some(),
            "opening should rebuild missing index rows"
        );
    }
}
