use std::{
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use conduit_ir::trace::TraceEvent;
use futures::Stream;
use tokio::{
    fs::{self, File, OpenOptions},
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};
use tracing::debug;

use crate::error::TraceError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default maximum segment size before rotation (64 MiB).
pub const DEFAULT_MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;

/// Subdirectory inside the data directory that holds segment files.
const SEGMENTS_DIR: &str = "segments";

/// Extension used for segment files.
const SEGMENT_EXT: &str = "cdlog";

// ---------------------------------------------------------------------------
// DurabilityMode
// ---------------------------------------------------------------------------

/// How hard the log writer pushes bytes to stable storage on the write path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityMode {
    /// `flush` only — data may remain in the OS page cache until later.
    BestEffort,
    /// `flush` + `sync_data` (fdatasync) after each append batch.
    ///
    /// Default for the audit trail: crash after a successful append should still
    /// retain the last batch when the volume supports fsync.
    #[default]
    Fsync,
}

// ---------------------------------------------------------------------------
// LogOffset
// ---------------------------------------------------------------------------

/// Pointer to a single event frame within the segmented log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LogOffset {
    /// Segment filename (basename only, e.g. `"2026-05-17.0.cdlog"`).
    pub segment: String,
    /// Byte offset of the start of the frame within the segment file.
    pub offset: u64,
}

// ---------------------------------------------------------------------------
// SegmentFile — active write handle
// ---------------------------------------------------------------------------

struct SegmentFile {
    path: PathBuf,
    file: File,
    size: u64,
    /// Sequence number within the current date.
    seq: u32,
    /// Date string used in the segment filename, e.g. `"2026-05-17"`.
    date: String,
}

impl SegmentFile {
    /// Open (or create) the segment file at `path` for append, returning its
    /// current size.
    async fn open(path: PathBuf) -> Result<(Self, u64), TraceError> {
        let date = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('.').next())
            .unwrap_or("unknown")
            .to_string();
        let seq_str = path
            .file_stem()
            .and_then(|n| n.to_str())
            .and_then(|n| n.split('.').nth(1))
            .unwrap_or("0");
        let seq: u32 = seq_str.parse().unwrap_or(0);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .map_err(TraceError::Io)?;
        let size = file.metadata().await.map_err(TraceError::Io)?.len();

        Ok((
            Self {
                path,
                file,
                size,
                seq,
                date,
            },
            size,
        ))
    }

    fn filename(&self) -> String {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    }
}

// ---------------------------------------------------------------------------
// LogWriter
// ---------------------------------------------------------------------------

/// Append-only segmented log writer.
///
/// Segment files live under `<dir>/segments/YYYY-MM-DD.{n}.cdlog`.
/// Each frame is a 4-byte big-endian length prefix followed by zstd-compressed
/// JSON bytes.  When the active segment exceeds `max_segment_bytes` a new
/// segment is opened with an incremented sequence number.
pub struct LogWriter {
    dir: PathBuf,
    current_segment: tokio::sync::Mutex<SegmentFile>,
    max_segment_bytes: u64,
    durability: DurabilityMode,
    /// Number of times `sync_data` ran on the append path (not shutdown).
    durable_syncs: AtomicU64,
}

impl LogWriter {
    /// Open the writer, creating the segments directory if it does not exist.
    /// The latest existing segment (by name) is used as the active segment.
    ///
    /// Defaults to [`DurabilityMode::Fsync`].
    pub async fn new(dir: PathBuf) -> Result<Self, TraceError> {
        Self::with_options(dir, DEFAULT_MAX_SEGMENT_BYTES, DurabilityMode::Fsync).await
    }

    /// Like [`new`] but with a custom rotation threshold (useful in tests).
    pub async fn with_max_segment_bytes(
        dir: PathBuf,
        max_segment_bytes: u64,
    ) -> Result<Self, TraceError> {
        Self::with_options(dir, max_segment_bytes, DurabilityMode::Fsync).await
    }

    /// Full constructor with rotation threshold and durability policy.
    pub async fn with_options(
        dir: PathBuf,
        max_segment_bytes: u64,
        durability: DurabilityMode,
    ) -> Result<Self, TraceError> {
        let seg_dir = dir.join(SEGMENTS_DIR);
        fs::create_dir_all(&seg_dir).await.map_err(TraceError::Io)?;

        let seg = find_or_create_segment(&seg_dir).await?;
        Ok(Self {
            dir,
            current_segment: tokio::sync::Mutex::new(seg),
            max_segment_bytes,
            durability,
            durable_syncs: AtomicU64::new(0),
        })
    }

    /// Active durability policy.
    pub fn durability(&self) -> DurabilityMode {
        self.durability
    }

    /// Count of write-path `sync_data` calls (excludes graceful shutdown).
    pub fn durable_syncs(&self) -> u64 {
        self.durable_syncs.load(Ordering::Relaxed)
    }

    /// Append a slice of events, returning one [`LogOffset`] per event.
    ///
    /// After writing the batch, honors [`DurabilityMode`]: `Fsync` issues
    /// `sync_data` on the active segment so durability is not limited to process exit.
    pub async fn append(&self, events: &[TraceEvent]) -> Result<Vec<LogOffset>, TraceError> {
        if events.is_empty() {
            return Ok(Vec::new());
        }

        let mut seg = self.current_segment.lock().await;
        let mut offsets = Vec::with_capacity(events.len());

        for event in events {
            self.rotate_if_needed(&mut seg).await?;

            let json =
                serde_json::to_vec(event).map_err(|e| TraceError::Serialization(e.to_string()))?;
            let compressed = compress(&json)?;
            let frame_len = compressed.len() as u32;

            let offset = seg.size;
            seg.file
                .write_all(&frame_len.to_be_bytes())
                .await
                .map_err(TraceError::Io)?;
            seg.file
                .write_all(&compressed)
                .await
                .map_err(TraceError::Io)?;
            seg.file.flush().await.map_err(TraceError::Io)?;

            seg.size += 4 + compressed.len() as u64;
            offsets.push(LogOffset {
                segment: seg.filename(),
                offset,
            });
        }

        // Write-path durability (not only graceful shutdown).
        if self.durability == DurabilityMode::Fsync {
            seg.file.sync_data().await.map_err(TraceError::Io)?;
            self.durable_syncs.fetch_add(1, Ordering::Relaxed);
        }

        Ok(offsets)
    }

    /// Flush and fsync the current segment to durable storage.
    ///
    /// Called during graceful shutdown to ensure no data is lost in the OS
    /// page cache.
    pub async fn shutdown(&self) -> Result<(), TraceError> {
        let mut seg = self.current_segment.lock().await;
        seg.file.flush().await.map_err(TraceError::Io)?;
        seg.file.sync_data().await.map_err(TraceError::Io)?;
        Ok(())
    }

    async fn rotate_if_needed(&self, seg: &mut SegmentFile) -> Result<(), TraceError> {
        let today = today_string();
        let needs_rotation = seg.size >= self.max_segment_bytes || seg.date != today;

        if !needs_rotation {
            return Ok(());
        }

        // Seal the outgoing segment before dropping the file handle. Without
        // this, Fsync mode only durable-syncs the *current* segment at the end
        // of `append`, leaving rotated frames potentially only in the page cache.
        if self.durability == DurabilityMode::Fsync && seg.size > 0 {
            seg.file.flush().await.map_err(TraceError::Io)?;
            seg.file.sync_data().await.map_err(TraceError::Io)?;
            self.durable_syncs.fetch_add(1, Ordering::Relaxed);
        }

        let seg_dir = self.dir.join(SEGMENTS_DIR);
        let next_seq = if seg.date == today { seg.seq + 1 } else { 0 };
        let new_filename = format!("{}.{}.{}", today, next_seq, SEGMENT_EXT);
        let new_path = seg_dir.join(&new_filename);

        debug!(path = %new_path.display(), "rotating log segment");

        let new_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&new_path)
            .await
            .map_err(TraceError::Io)?;

        seg.path = new_path;
        seg.file = new_file;
        seg.size = 0;
        seg.seq = next_seq;
        seg.date = today;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// LogReader
// ---------------------------------------------------------------------------

/// Read events back from the segmented log.
pub struct LogReader {
    dir: PathBuf,
}

impl LogReader {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Read a single event from the given offset.
    pub async fn read_at(&self, offset: LogOffset) -> Result<TraceEvent, TraceError> {
        let seg_path = self.dir.join(SEGMENTS_DIR).join(&offset.segment);
        let mut file = fs::File::open(&seg_path).await.map_err(TraceError::Io)?;
        file.seek(std::io::SeekFrom::Start(offset.offset))
            .await
            .map_err(TraceError::Io)?;

        let frame_len = read_frame_len(&mut file).await?;
        let mut compressed = vec![0u8; frame_len as usize];
        file.read_exact(&mut compressed)
            .await
            .map_err(TraceError::Io)?;

        let json = decompress(&compressed)?;
        let event: TraceEvent =
            serde_json::from_slice(&json).map_err(|e| TraceError::Serialization(e.to_string()))?;
        Ok(event)
    }

    /// Stream every event stored in the log, in chronological segment order.
    pub fn stream_all(&self) -> impl Stream<Item = Result<TraceEvent, TraceError>> + '_ {
        let dir = self.dir.clone();
        async_stream::try_stream! {
            let seg_dir = dir.join(SEGMENTS_DIR);
            let mut entries: Vec<PathBuf> = Vec::new();

            let mut rd = fs::read_dir(&seg_dir).await.map_err(TraceError::Io)?;
            while let Some(entry) = rd.next_entry().await.map_err(TraceError::Io)? {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some(SEGMENT_EXT) {
                    entries.push(p);
                }
            }
            entries.sort();

            for seg_path in entries {
                let mut file = fs::File::open(&seg_path).await.map_err(TraceError::Io)?;
                loop {
                    match read_frame_len(&mut file).await {
                        Ok(frame_len) => {
                            let mut compressed = vec![0u8; frame_len as usize];
                            file.read_exact(&mut compressed).await.map_err(TraceError::Io)?;
                            let json = decompress(&compressed)?;
                            let event: TraceEvent = serde_json::from_slice(&json)
                                .map_err(|e| TraceError::Serialization(e.to_string()))?;
                            yield event;
                        }
                        Err(TraceError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                            // Clean end of segment.
                            break;
                        }
                        Err(e) => { Err(e)?; break; }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn today_string() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

async fn find_or_create_segment(seg_dir: &Path) -> Result<SegmentFile, TraceError> {
    let today = today_string();
    let mut candidates: Vec<PathBuf> = Vec::new();

    let mut rd = fs::read_dir(seg_dir).await.map_err(TraceError::Io)?;
    while let Some(entry) = rd.next_entry().await.map_err(TraceError::Io)? {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some(SEGMENT_EXT) {
            candidates.push(p);
        }
    }
    candidates.sort();

    // Use the newest existing segment if it belongs to today, otherwise create
    // a new one.
    if let Some(latest) = candidates.last() {
        let name = latest.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with(&today) {
            let (seg, _) = SegmentFile::open(latest.clone()).await?;
            return Ok(seg);
        }
    }

    let filename = format!("{}.0.{}", today, SEGMENT_EXT);
    let path = seg_dir.join(filename);
    let (seg, _) = SegmentFile::open(path).await?;
    Ok(seg)
}

fn compress(data: &[u8]) -> Result<Vec<u8>, TraceError> {
    let mut encoder = zstd::Encoder::new(Vec::new(), 3).map_err(TraceError::Io)?;
    encoder.write_all(data).map_err(TraceError::Io)?;
    encoder.finish().map_err(TraceError::Io)
}

fn decompress(data: &[u8]) -> Result<Vec<u8>, TraceError> {
    let mut decoder = zstd::Decoder::new(data).map_err(TraceError::Io)?;
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).map_err(TraceError::Io)?;
    Ok(out)
}

async fn read_frame_len(file: &mut fs::File) -> Result<u32, TraceError> {
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf).await.map_err(TraceError::Io)?;
    Ok(u32::from_be_bytes(buf))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use conduit_ir::trace::TraceEventKind;
    use futures::StreamExt;

    use super::*;

    fn sample_event() -> TraceEvent {
        TraceEvent::new(TraceEventKind::RequestReceived {
            downstream_key_id: Some("dk-test".into()),
            alias: "gpt-4o".into(),
            stream: false,
            request: serde_json::json!({}),
            request_ir: None,
            wire_format: None,
            request_headers: None,
        })
    }

    #[tokio::test]
    async fn round_trip_single_event() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = LogWriter::new(tmp.path().to_path_buf()).await.unwrap();
        let reader = LogReader::new(tmp.path().to_path_buf());

        let event = sample_event();
        let offsets = writer.append(std::slice::from_ref(&event)).await.unwrap();
        assert_eq!(offsets.len(), 1);

        let back = reader.read_at(offsets[0].clone()).await.unwrap();
        assert_eq!(back.id, event.id);
    }

    #[tokio::test]
    async fn stream_all_returns_all_events() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = LogWriter::new(tmp.path().to_path_buf()).await.unwrap();
        let reader = LogReader::new(tmp.path().to_path_buf());

        let events: Vec<TraceEvent> = (0..10).map(|_| sample_event()).collect();
        writer.append(&events).await.unwrap();

        let streamed: Vec<_> = reader
            .stream_all()
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(streamed.len(), 10);
    }

    #[tokio::test]
    async fn rotation_creates_new_segment() {
        let tmp = tempfile::tempdir().unwrap();
        // Set a tiny max size to force rotation after the first event.
        let writer = LogWriter::with_max_segment_bytes(tmp.path().to_path_buf(), 1)
            .await
            .unwrap();

        let e1 = sample_event();
        let e2 = sample_event();
        let offsets = writer.append(&[e1, e2]).await.unwrap();

        // Both events must be at the start of their respective segments.
        assert_ne!(
            offsets[0].segment, offsets[1].segment,
            "should be in separate segments"
        );
    }

    #[tokio::test]
    async fn fsync_mode_syncs_on_append_path() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = LogWriter::with_options(
            tmp.path().to_path_buf(),
            DEFAULT_MAX_SEGMENT_BYTES,
            DurabilityMode::Fsync,
        )
        .await
        .unwrap();
        assert_eq!(writer.durability(), DurabilityMode::Fsync);
        assert_eq!(writer.durable_syncs(), 0);

        let event = sample_event();
        let offsets = writer.append(std::slice::from_ref(&event)).await.unwrap();
        assert_eq!(offsets.len(), 1);
        assert_eq!(
            writer.durable_syncs(),
            1,
            "Fsync mode must call sync_data on the write path"
        );

        // Re-open reader and recover the frame after durable append.
        let reader = LogReader::new(tmp.path().to_path_buf());
        let back = reader.read_at(offsets[0].clone()).await.unwrap();
        assert_eq!(back.id, event.id);

        // Second batch increments the counter again.
        writer.append(&[sample_event()]).await.unwrap();
        assert_eq!(writer.durable_syncs(), 2);
    }

    #[tokio::test]
    async fn best_effort_mode_skips_write_path_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = LogWriter::with_options(
            tmp.path().to_path_buf(),
            DEFAULT_MAX_SEGMENT_BYTES,
            DurabilityMode::BestEffort,
        )
        .await
        .unwrap();
        assert_eq!(writer.durability(), DurabilityMode::BestEffort);

        let event = sample_event();
        let offsets = writer.append(std::slice::from_ref(&event)).await.unwrap();
        assert_eq!(writer.durable_syncs(), 0);

        // Still readable within the same process after flush.
        let reader = LogReader::new(tmp.path().to_path_buf());
        let back = reader.read_at(offsets[0].clone()).await.unwrap();
        assert_eq!(back.id, event.id);
    }

    #[tokio::test]
    async fn fsync_mode_syncs_rotated_segment_before_replace() {
        let tmp = tempfile::tempdir().unwrap();
        // Force rotation after every event so multi-event append rotates mid-batch.
        let writer = LogWriter::with_options(tmp.path().to_path_buf(), 1, DurabilityMode::Fsync)
            .await
            .unwrap();

        let e1 = sample_event();
        let e2 = sample_event();
        let e1_id = e1.id.clone();
        let e2_id = e2.id.clone();
        let offsets = writer.append(&[e1, e2]).await.unwrap();
        assert_ne!(offsets[0].segment, offsets[1].segment);

        // At least: sync old segment on rotate + sync final current segment at end.
        assert!(
            writer.durable_syncs() >= 2,
            "expected rotate seal + final batch sync, got {}",
            writer.durable_syncs()
        );

        let reader = LogReader::new(tmp.path().to_path_buf());
        assert_eq!(reader.read_at(offsets[0].clone()).await.unwrap().id, e1_id);
        assert_eq!(reader.read_at(offsets[1].clone()).await.unwrap().id, e2_id);
    }
}
