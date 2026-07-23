//! Console log API — meta, history paging, and live SSE follow.
//!
//! Handlers only need [`crate::state::LogRuntime`]; they call
//! [`crate::log_reader`] for pure file IO.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::log_reader::{
    self, LevelFloor, LogsPage, PageDirection, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT,
};
use crate::state::DaemonState;

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({"error": msg.into()})))
}

fn internal(e: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

const DISABLED_MSG: &str =
    "file logging is disabled; enable [log] to_file = true (or --log-to-file) and restart conduitd";

/// Query for `GET /console/logs`.
#[derive(Debug, Deserialize)]
pub struct LogsQuery {
    pub date: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
    pub level: Option<String>,
    pub q: Option<String>,
    pub direction: Option<String>,
}

/// Query for `GET /console/logs/stream`.
#[derive(Debug, Deserialize)]
pub struct LogsStreamQuery {
    pub level: Option<String>,
    pub q: Option<String>,
    pub backfill: Option<usize>,
}

/// GET /console/logs/meta
pub async fn logs_meta(State(state): State<Arc<DaemonState>>) -> impl IntoResponse {
    let log = &state.log;
    let message = if !log.to_file {
        Some(DISABLED_MSG.to_string())
    } else {
        None
    };
    let meta = log_reader::build_meta(
        log.to_file,
        &log.dir,
        &log.prefix,
        &log.format,
        &log.level,
        message,
    );
    (StatusCode::OK, Json(meta)).into_response()
}

/// GET /console/logs
pub async fn list_logs(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    let log = &state.log;
    if !log.to_file {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": DISABLED_MSG,
                "source": "unavailable",
                "date": q.date.unwrap_or_else(log_reader::local_today_string),
                "lines": [],
            })),
        )
            .into_response();
    }

    let date = q
        .date
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(log_reader::local_today_string);
    if log_reader::parse_date(&date).is_none() {
        return err(StatusCode::BAD_REQUEST, "date must be YYYY-MM-DD").into_response();
    }
    let limit = q.limit.unwrap_or(DEFAULT_PAGE_LIMIT).clamp(1, MAX_PAGE_LIMIT);
    let level_floor = q.level.as_deref().and_then(LevelFloor::parse);
    let direction = PageDirection::parse(q.direction.as_deref().unwrap_or("backward"));
    let filter_q = q.q.clone().filter(|s| !s.is_empty());
    let dir = log.dir.clone();
    let prefix = log.prefix.clone();
    let cursor = q.cursor.clone();

    let result = tokio::task::spawn_blocking(move || {
        log_reader::read_page(
            &dir,
            &prefix,
            &date,
            limit,
            cursor.as_deref(),
            direction,
            level_floor,
            filter_q.as_deref(),
        )
    })
    .await;

    match result {
        Ok(Ok(page)) => (StatusCode::OK, Json(page)).into_response(),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::InvalidInput => {
            err(StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
        Ok(Err(e)) => internal(e).into_response(),
        Err(e) => internal(e).into_response(),
    }
}

/// GET /console/logs/stream — SSE live follow of today's log file.
pub async fn stream_logs(
    State(state): State<Arc<DaemonState>>,
    Query(q): Query<LogsStreamQuery>,
) -> impl IntoResponse {
    let log = state.log.clone();
    let level_floor = q.level.as_deref().and_then(LevelFloor::parse);
    let filter_q = q.q.clone().filter(|s| !s.is_empty());
    let backfill = q.backfill.unwrap_or(100).clamp(0, MAX_PAGE_LIMIT);

    let stream = async_stream::stream! {
        if !log.to_file {
            yield Ok::<Event, Infallible>(
                Event::default()
                    .event("error")
                    .data(json!({"message": DISABLED_MSG}).to_string()),
            );
            return;
        }

        let mut date = log_reader::local_today_string();
        yield Ok(Event::default().event("meta").data(
            json!({
                "date": date,
                "dir": log.dir.display().to_string(),
                "prefix": log.prefix,
            })
            .to_string(),
        ));

        let (seed_lines, mut offset) = match log_reader::tail_for_follow(
            &log.dir,
            &log.prefix,
            &date,
            backfill.max(1),
            level_floor,
            filter_q.as_deref(),
        ) {
            Ok(v) => v,
            Err(e) => {
                yield Ok(Event::default().event("error").data(
                    json!({"message": format!("log file read failed: {e}")}).to_string(),
                ));
                return;
            }
        };

        if backfill == 0 {
            offset = log_reader::file_len(&log.dir, &log.prefix, &date).unwrap_or(0);
        } else {
            for line in seed_lines {
                if let Ok(data) = serde_json::to_string(&line) {
                    yield Ok(Event::default().event("line").data(data));
                }
            }
        }

        let mut tick = tokio::time::interval(Duration::from_millis(400));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3600);

        loop {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tick.tick().await;

            let today = log_reader::local_today_string();
            if today != date {
                date = today;
                offset = 0;
                yield Ok(Event::default()
                    .event("rotate")
                    .data(json!({"date": date}).to_string()));
            }

            match log_reader::read_appended(
                &log.dir,
                &log.prefix,
                &date,
                offset,
                level_floor,
                filter_q.as_deref(),
            ) {
                Ok((lines, new_off)) => {
                    offset = new_off;
                    for line in lines {
                        if let Ok(data) = serde_json::to_string(&line) {
                            yield Ok(Event::default().event("line").data(data));
                        }
                    }
                }
                Err(e) => {
                    yield Ok(Event::default().event("error").data(
                        json!({"message": format!("follow read failed: {e}")}).to_string(),
                    ));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    };

    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
        .into_response()
}

/// Empty page helper for tests / disabled responses.
#[allow(dead_code)]
pub fn unavailable_page(date: String) -> LogsPage {
    LogsPage {
        date,
        lines: Vec::new(),
        next_cursor: None,
        prev_cursor: None,
        truncated: false,
        source: "unavailable".into(),
    }
}
