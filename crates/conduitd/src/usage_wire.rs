//! Usage ledger wiring used by the daemon quota engine.
//!
//! This ledger is a **billing source of truth**, not just observability: every
//! finished request must be persisted (at-least-once). The gateway hot path
//! **enqueues** finished requests and a single background worker batches inserts
//! into `usage_records` / `usage_attempts`, keeping SQLite writes off the
//! request path and avoiding connection-pool stampede under load.
//!
//! Durability under burst: if the queue is full, the record is NOT dropped. The
//! enqueue path first applies brief backpressure, then falls back to a
//! **synchronous** insert on the caller's task. A record is only ever lost if
//! that synchronous insert *also* fails (e.g. DB unavailable) — such losses are
//! counted in [`dropped_records`] and logged at `error` so they can be alerted
//! on and reconciled, never silently swallowed.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use conduit_ir::error::QuotaError;
use conduit_quota::check::{QuotaRecordRequest, RecordFn};
use conduit_store::{new_usage_attempt, StorePool, UsageRepo};
use tokio::sync::mpsc;
use tracing::{error, warn};

/// Bound on in-flight ledger records waiting for the writer.
const USAGE_QUEUE_CAP: usize = 16_384;
/// Max rows flushed per SQLite transaction.
const USAGE_BATCH_MAX: usize = 64;
/// How long the enqueue path waits for queue space before falling back to a
/// synchronous insert. Short so the hot path is not stalled for long.
const ENQUEUE_BACKPRESSURE: std::time::Duration = std::time::Duration::from_millis(100);

/// Count of ledger records that could not be persisted at all (queue full AND
/// the synchronous fallback insert failed). For billing this must stay 0;
/// expose it to metrics / alerting. Process-lifetime cumulative.
static DROPPED_RECORDS: AtomicU64 = AtomicU64::new(0);

/// Cumulative count of ledger records lost since process start (see
/// [`DROPPED_RECORDS`]). Non-zero means the billing ledger is missing entries.
pub fn dropped_records() -> u64 {
    DROPPED_RECORDS.load(Ordering::Relaxed)
}

/// Handle for draining the usage writer during graceful shutdown.
///
/// On shutdown, call [`UsageWriterHandle::drain`] *before* closing the DB pool:
/// it signals the writer to flush every queued record and waits for it to
/// finish, so in-flight billing rows are not lost on a normal restart/deploy.
/// (A hard `kill -9` can still lose the queue — that needs a write-ahead log.)
pub struct UsageWriterHandle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    worker: tokio::task::JoinHandle<()>,
}

impl UsageWriterHandle {
    /// Signal the writer to drain the queue and wait up to `timeout` for it to
    /// finish flushing. Returns `true` if it drained cleanly within the budget.
    pub async fn drain(self, timeout: std::time::Duration) -> bool {
        let _ = self.shutdown_tx.send(true);
        match tokio::time::timeout(timeout, self.worker).await {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                warn!(error = %e, "usage ledger writer join failed during drain");
                false
            }
            Err(_) => {
                warn!("usage ledger drain timed out; some queued records may be unflushed");
                false
            }
        }
    }
}

/// Build the `record_fn` injected into [`InMemoryQuotaEngine`](conduit_quota::InMemoryQuotaEngine),
/// plus a [`UsageWriterHandle`] to drain the writer on shutdown.
///
/// Fast path: enqueue and return. If the queue is full, apply brief
/// backpressure, then fall back to a synchronous insert so the record is never
/// dropped on overflow. Only a failed synchronous fallback loses a record — and
/// that is counted in [`dropped_records`] and logged at `error`.
pub fn make_record_fn(pool: StorePool) -> (RecordFn, UsageWriterHandle) {
    let (tx, rx) = mpsc::channel::<QuotaRecordRequest>(USAGE_QUEUE_CAP);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(usage_writer_loop(pool.clone(), rx, shutdown_rx));

    let record_fn: RecordFn = Arc::new(move |req: QuotaRecordRequest| {
        let tx = tx.clone();
        let pool = pool.clone();
        Box::pin(async move {
            match tx.try_send(req) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Full(req)) => {
                    // Queue saturated: retry-with-space for a brief window
                    // (keeping ownership of `req`), then fall back to a
                    // synchronous insert rather than dropping the record.
                    match enqueue_with_backpressure(&tx, req).await {
                        Ok(()) => Ok(()),
                        Err(EnqueueGaveUp::Full(req)) => {
                            persist_or_count(&pool, req, "usage ledger queue full").await
                        }
                        Err(EnqueueGaveUp::Closed(req)) => {
                            persist_or_count(&pool, req, "usage ledger writer stopped").await
                        }
                    }
                }
                Err(mpsc::error::TrySendError::Closed(req)) => {
                    persist_or_count(&pool, req, "usage ledger writer stopped").await
                }
            }
        })
    });

    (
        record_fn,
        UsageWriterHandle {
            shutdown_tx,
            worker,
        },
    )
}

/// Why [`enqueue_with_backpressure`] returned the record instead of enqueuing.
enum EnqueueGaveUp {
    /// Queue stayed full for the whole backpressure window.
    Full(QuotaRecordRequest),
    /// The writer task is gone.
    Closed(QuotaRecordRequest),
}

/// Poll `try_send` for up to [`ENQUEUE_BACKPRESSURE`], sleeping between tries,
/// while retaining ownership of `req` (unlike `Sender::send`, which consumes the
/// value even on timeout). Returns the record back on give-up so the caller can
/// persist it synchronously.
async fn enqueue_with_backpressure(
    tx: &mpsc::Sender<QuotaRecordRequest>,
    mut req: QuotaRecordRequest,
) -> Result<(), EnqueueGaveUp> {
    let deadline = ENQUEUE_BACKPRESSURE;
    let step = std::time::Duration::from_millis(5);
    let mut waited = std::time::Duration::ZERO;
    loop {
        match tx.try_send(req) {
            Ok(()) => return Ok(()),
            Err(mpsc::error::TrySendError::Full(r)) => {
                if waited >= deadline {
                    return Err(EnqueueGaveUp::Full(r));
                }
                req = r;
                tokio::time::sleep(step).await;
                waited += step;
            }
            Err(mpsc::error::TrySendError::Closed(r)) => return Err(EnqueueGaveUp::Closed(r)),
        }
    }
}

/// Synchronous fallback: persist `req` directly; on failure count it as dropped
/// (billing gap) and surface the error. `reason` describes why we fell back.
async fn persist_or_count(
    pool: &StorePool,
    req: QuotaRecordRequest,
    reason: &str,
) -> Result<(), QuotaError> {
    let request_id = req.request_id.clone();
    match persist_request(pool, req).await {
        Ok(()) => {
            warn!(reason, request_id = %request_id, "usage ledger fell back to synchronous insert");
            Ok(())
        }
        Err(e) => {
            let dropped = DROPPED_RECORDS.fetch_add(1, Ordering::Relaxed) + 1;
            error!(
                reason,
                error = %e,
                request_id = %request_id,
                dropped_total = dropped,
                "usage ledger record LOST (billing gap): queue full and synchronous insert failed"
            );
            Err(e)
        }
    }
}

/// Synchronous insert (tests / tools that need fail-closed on DB errors).
pub async fn persist_request(pool: &StorePool, req: QuotaRecordRequest) -> Result<(), QuotaError> {
    let (row, attempts) = build_ledger_rows(req);
    UsageRepo::new(pool)
        .insert_ledger(&row, &attempts)
        .await
        .map_err(|e| QuotaError::Backend(format!("usage record: {e}")))
}

async fn usage_writer_loop(
    pool: StorePool,
    mut rx: mpsc::Receiver<QuotaRecordRequest>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        // Wait for either the next record or a shutdown signal.
        let first = tokio::select! {
            r = rx.recv() => match r {
                Some(r) => r,
                None => break, // all senders dropped: nothing left to flush.
            },
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break; // enter drain phase below.
                }
                continue;
            }
        };

        let mut batch = vec![first];
        while batch.len() < USAGE_BATCH_MAX {
            match rx.try_recv() {
                Ok(r) => batch.push(r),
                Err(_) => break,
            }
        }
        flush_batch(&pool, batch).await;
    }

    // Drain phase: on shutdown, flush every record still queued so in-flight
    // billing rows survive a normal restart/deploy.
    let mut drained = 0usize;
    loop {
        let mut batch = Vec::new();
        while batch.len() < USAGE_BATCH_MAX {
            match rx.try_recv() {
                Ok(r) => batch.push(r),
                Err(_) => break,
            }
        }
        if batch.is_empty() {
            break;
        }
        drained += batch.len();
        flush_batch(&pool, batch).await;
    }
    if drained > 0 {
        warn!(drained, "usage ledger drained queued records on shutdown");
    }
}

/// Persist one batch in a single transaction; on batch failure, retry each row
/// individually with backoff so a transient DB hiccup can't drop the whole
/// batch. Rows that still won't persist are counted as billing gaps.
async fn flush_batch(pool: &StorePool, batch: Vec<QuotaRecordRequest>) {
    let items: Vec<_> = batch.into_iter().map(build_ledger_rows).collect();
    let n = items.len();
    if let Err(e) = UsageRepo::new(pool).insert_ledger_batch(&items).await {
        warn!(
            error = %e,
            batch_size = n,
            "usage ledger batch write failed; retrying rows individually"
        );
        for (row, attempts) in &items {
            if let Err(e) = insert_row_with_retry(pool, row, attempts).await {
                let dropped = DROPPED_RECORDS.fetch_add(1, Ordering::Relaxed) + 1;
                error!(
                    error = %e,
                    request_id = %row.request_id,
                    dropped_total = dropped,
                    "usage ledger record LOST (billing gap): row insert failed after retries"
                );
            }
        }
    }
}

/// Insert one ledger row with bounded exponential backoff. Absorbs transient
/// SQLite contention (`SQLITE_BUSY`, brief pool exhaustion) so billing rows are
/// not lost to momentary hiccups.
async fn insert_row_with_retry(
    pool: &StorePool,
    row: &conduit_store::UsageRecordRow,
    attempts: &[conduit_store::UsageAttemptRow],
) -> Result<(), conduit_store::StoreError> {
    const MAX_ATTEMPTS: u32 = 5;
    let mut delay = std::time::Duration::from_millis(20);
    let mut last_err = None;
    for _ in 0..MAX_ATTEMPTS {
        match UsageRepo::new(pool).insert_ledger(row, attempts).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_millis(500));
            }
        }
    }
    Err(last_err.expect("loop ran at least once"))
}

fn build_ledger_rows(
    req: QuotaRecordRequest,
) -> (
    conduit_store::UsageRecordRow,
    Vec<conduit_store::UsageAttemptRow>,
) {
    let key_id = {
        let id = req.downstream_key_id.trim();
        if id.is_empty() || id == "_anonymous" || id == "_local" {
            None
        } else {
            Some(id.to_string())
        }
    };

    let row = conduit_store::UsageRecordRow {
        request_id: req.request_id.clone(),
        downstream_key_id: key_id,
        alias: req.alias,
        provider_id: req.provider_id,
        provider_kind: req.provider_kind,
        model_id: req.model_id,
        prompt_tokens: req.prompt_tokens,
        completion_tokens: req.completion_tokens,
        total_tokens: req.total_tokens,
        reasoning_tokens: req.reasoning_tokens,
        cache_read_tokens: req.cache_read_tokens,
        cache_write_tokens: req.cache_write_tokens,
        cost_usd: req.cost_usd,
        stream: req.stream,
        status: req.status,
        error_class: req.error_class,
        http_status: req.http_status,
        finish_reason: req.finish_reason,
        duration_ms: req.duration_ms,
        ttfb_ms: req.ttfb_ms,
        route_strategy: req.route_strategy,
        attempt_no: req.attempt_no,
        attempt_count: req.attempt_count.max(1),
        session_id: req.session_id,
        affinity_hit: req.affinity_hit,
        pool_id: req.pool_id,
        selected_reason: req.selected_reason,
        ..conduit_store::UsageRecordRow::stamped()
    };

    let attempts: Vec<_> = req
        .attempts
        .into_iter()
        .map(|a| {
            new_usage_attempt(
                req.request_id.clone(),
                a.attempt_no,
                a.provider_id,
                a.provider_kind,
                a.model_id,
                a.status,
                a.error_class,
                a.http_status,
                a.duration_ms,
                a.ttfb_ms,
                a.reason,
            )
        })
        .collect();

    (row, attempts)
}

#[cfg(test)]
mod tests {
    use conduit_quota::QuotaAttemptRecord;
    use conduit_store::open_db;

    use super::*;

    fn base_req() -> QuotaRecordRequest {
        QuotaRecordRequest {
            request_id: "tr-1".into(),
            downstream_key_id: "dk_abc".into(),
            alias: Some("gpt".into()),
            provider_id: Some("p1".into()),
            provider_kind: Some("openai".into()),
            model_id: Some("gpt-4o".into()),
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            reasoning_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: 0.02,
            stream: false,
            status: "ok".into(),
            error_class: None,
            http_status: None,
            finish_reason: Some("stop".into()),
            duration_ms: Some(100),
            ttfb_ms: Some(25),
            route_strategy: Some("fixed".into()),
            attempt_no: 0,
            attempt_count: 1,
            session_id: None,
            affinity_hit: None,
            pool_id: None,
            selected_reason: Some("fixed".into()),
            attempts: Vec::new(),
        }
    }

    async fn wait_for_rows(pool: &StorePool, min: usize) {
        for _ in 0..200 {
            let n = UsageRepo::new(pool)
                .list_page(conduit_store::UsageListOpts {
                    limit: min.max(1),
                    offset: 0,
                    key_id: None,
                    period: None,
                    q: None,
                    sort: Default::default(),
                    tz_offset_minutes: 0,
                })
                .await
                .map(|p| p.total as usize)
                .unwrap_or(0);
            if n >= min {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {min} usage rows");
    }

    #[tokio::test]
    async fn record_writes_usage_row() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let (record_fn, _writer) = make_record_fn(pool.clone());

        (record_fn)(base_req()).await.unwrap();
        wait_for_rows(&pool, 1).await;

        let rows = UsageRepo::new(&pool).list(10, None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "tr-1");
        assert_eq!(rows[0].downstream_key_id.as_deref(), Some("dk_abc"));
        assert!((rows[0].cost_usd - 0.02).abs() < 1e-9);
        assert_eq!(rows[0].status, "ok");
        assert_eq!(rows[0].duration_ms, Some(100));
        assert_eq!(rows[0].ttfb_ms, Some(25));
        assert_eq!(rows[0].finish_reason.as_deref(), Some("stop"));
    }

    #[tokio::test]
    async fn zero_token_and_error_rows_persist() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let (record_fn, _writer) = make_record_fn(pool.clone());

        let mut zero = base_req();
        zero.request_id = "tr-zero".into();
        zero.prompt_tokens = 0;
        zero.completion_tokens = 0;
        zero.total_tokens = 0;
        zero.cost_usd = 0.0;
        zero.ttfb_ms = None;
        (record_fn)(zero).await.unwrap();

        let mut err = base_req();
        err.request_id = "tr-err".into();
        err.prompt_tokens = 0;
        err.completion_tokens = 0;
        err.total_tokens = 0;
        err.cost_usd = 0.0;
        err.status = "error".into();
        err.error_class = Some("rate_limited".into());
        err.http_status = Some(429);
        err.finish_reason = None;
        err.ttfb_ms = None;
        err.attempt_count = 2;
        err.attempts = vec![
            QuotaAttemptRecord {
                attempt_no: 0,
                provider_id: Some("p0".into()),
                provider_kind: Some("openai".into()),
                model_id: Some("gpt-4o".into()),
                status: "error".into(),
                error_class: Some("rate_limited".into()),
                http_status: Some(429),
                duration_ms: Some(10),
                ttfb_ms: None,
                reason: Some("initial".into()),
            },
            QuotaAttemptRecord {
                attempt_no: 1,
                provider_id: Some("p1".into()),
                provider_kind: Some("openai".into()),
                model_id: Some("gpt-4o".into()),
                status: "error".into(),
                error_class: Some("rate_limited".into()),
                http_status: Some(429),
                duration_ms: Some(20),
                ttfb_ms: None,
                reason: Some("retry".into()),
            },
        ];
        (record_fn)(err).await.unwrap();
        wait_for_rows(&pool, 2).await;

        let repo = UsageRepo::new(&pool);
        let rows = repo.list(10, None, None).await.unwrap();
        assert_eq!(rows.len(), 2);
        let zero_row = rows.iter().find(|r| r.request_id == "tr-zero").unwrap();
        assert_eq!(zero_row.status, "ok");
        assert_eq!(zero_row.total_tokens, 0);
        let err_row = rows.iter().find(|r| r.request_id == "tr-err").unwrap();
        assert_eq!(err_row.status, "error");
        assert_eq!(err_row.error_class.as_deref(), Some("rate_limited"));

        let attempts = repo.list_attempts("tr-err").await.unwrap();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0].provider_id.as_deref(), Some("p0"));
        assert_eq!(attempts[1].reason.as_deref(), Some("retry"));
    }

    #[tokio::test]
    async fn anonymous_key_stored_as_null() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let (record_fn, _writer) = make_record_fn(pool.clone());

        let mut req = base_req();
        req.request_id = "tr-2".into();
        req.downstream_key_id = "_anonymous".into();
        req.alias = None;
        req.provider_id = None;
        req.provider_kind = None;
        req.model_id = None;
        req.prompt_tokens = 1;
        req.completion_tokens = 0;
        req.total_tokens = 1;
        req.cost_usd = 0.0;
        req.stream = true;
        req.finish_reason = None;
        req.duration_ms = Some(5);
        req.ttfb_ms = Some(2);
        req.route_strategy = None;
        req.selected_reason = None;
        (record_fn)(req).await.unwrap();
        wait_for_rows(&pool, 1).await;

        let rows = UsageRepo::new(&pool).list(10, None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].downstream_key_id.is_none());
        assert!(rows[0].stream);
    }

    #[tokio::test]
    async fn persist_request_fails_closed_on_db_error() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        pool.close().await;
        let err = persist_request(&pool, base_req())
            .await
            .expect_err("must fail closed");
        assert!(matches!(err, QuotaError::Backend(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn enqueue_returns_quickly_under_load() {
        let pool = open_db("sqlite::memory:").await.unwrap();
        let (record_fn, _writer) = make_record_fn(pool.clone());
        let start = std::time::Instant::now();
        for i in 0..200 {
            let mut r = base_req();
            r.request_id = format!("burst-{i}");
            (record_fn)(r).await.unwrap();
        }
        // Enqueue path must not wait on SQLite for each row.
        assert!(
            start.elapsed() < std::time::Duration::from_millis(500),
            "enqueue of 200 rows took {:?}",
            start.elapsed()
        );
        wait_for_rows(&pool, 200).await;
    }

    #[tokio::test]
    async fn sync_fallback_persists_record_without_loss() {
        // The overflow fallback must actually write the row (billing: no loss).
        let pool = open_db("sqlite::memory:").await.unwrap();

        let mut req = base_req();
        req.request_id = "tr-fallback".into();
        persist_or_count(&pool, req, "test overflow")
            .await
            .expect("fallback insert must succeed");

        let rows = UsageRepo::new(&pool).list(10, None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "tr-fallback");
    }

    #[tokio::test]
    async fn sync_fallback_counts_loss_when_db_unavailable() {
        // If even the synchronous fallback fails, the record is counted as a
        // billing gap rather than silently swallowed.
        let pool = open_db("sqlite::memory:").await.unwrap();
        pool.close().await;
        let before = dropped_records();

        let err = persist_or_count(&pool, base_req(), "test overflow")
            .await
            .expect_err("must surface the DB error");
        assert!(matches!(err, QuotaError::Backend(_)), "got {err:?}");
        assert!(
            dropped_records() > before,
            "a lost billing record must bump the dropped counter (before={before}, now={})",
            dropped_records()
        );
    }

    #[tokio::test]
    async fn insert_row_with_retry_persists() {
        // The per-row retry path used when a batch write fails must persist.
        let pool = open_db("sqlite::memory:").await.unwrap();
        let mut req = base_req();
        req.request_id = "tr-retry".into();
        let (row, attempts) = build_ledger_rows(req);

        insert_row_with_retry(&pool, &row, &attempts)
            .await
            .expect("row insert must succeed");

        let rows = UsageRepo::new(&pool).list(10, None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "tr-retry");
    }

    #[tokio::test]
    async fn drain_flushes_queued_records_on_shutdown() {
        // Records enqueued right before shutdown must be flushed by drain(),
        // not lost — this is the normal restart/deploy durability guarantee.
        let pool = open_db("sqlite::memory:").await.unwrap();
        let (record_fn, writer) = make_record_fn(pool.clone());

        // Enqueue a burst; some rows will still be in the queue when we drain.
        for i in 0..300 {
            let mut r = base_req();
            r.request_id = format!("drain-{i}");
            (record_fn)(r).await.unwrap();
        }

        // Drop the record_fn so no new work can arrive, then drain.
        drop(record_fn);
        let clean = writer.drain(std::time::Duration::from_secs(30)).await;
        assert!(clean, "drain must finish within the timeout");

        // Every enqueued record must be persisted after drain.
        let total = UsageRepo::new(&pool)
            .list_page(conduit_store::UsageListOpts {
                limit: 500,
                offset: 0,
                key_id: None,
                period: None,
                q: None,
                sort: Default::default(),
                tz_offset_minutes: 0,
            })
            .await
            .unwrap()
            .total;
        assert_eq!(total, 300, "drain must flush all queued billing rows");
    }
}
