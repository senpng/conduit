//! Server-Sent Events (SSE) frame parsing for console `/console/traces/stream`.
//!
//! Pure string parsing — no HTTP. Used by CLI `trace tail` and
//! `ConsoleClient::subscribe_traces`.

use serde::Deserialize;

/// One complete SSE frame (delimited by a blank line in the wire stream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSseFrame {
    /// Optional `event:` field (e.g. `message`, `lagged`).
    pub event: Option<String>,
    /// Concatenated `data:` field(s), joined with `\n` per the SSE spec.
    pub data: String,
}

/// Classified console stream frame after interpreting `event:` + `data:`.
#[derive(Debug, Clone, PartialEq)]
pub enum SseFrame {
    /// Trace event payload (JSON body of a normal / `event: message` frame).
    /// Kept as raw JSON so CLI can print without requiring IR decode success.
    TraceData(String),
    /// Daemon notified that the broadcast subscriber lagged (`event: lagged`).
    Lagged { skipped: u64 },
}

/// Intermediate parse result used by tests and callers that need the raw fields.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedSseFrame {
    /// Frame with at least one `data:` line (and optional `event:`).
    Frame(RawSseFrame),
    /// Comment / empty / no-data frame — ignore.
    Ignored,
}

#[derive(Debug, Deserialize)]
struct LaggedPayload {
    skipped: u64,
}

/// Extract the concatenated `data:` fields from one SSE frame text.
///
/// Multiple `data:` lines are joined with `\n` (SSE standard).
pub fn extract_sse_data(frame: &str) -> Option<String> {
    parse_sse_frame(frame).and_then(|raw| {
        if raw.data.is_empty() {
            None
        } else {
            Some(raw.data)
        }
    })
}

/// Parse one SSE frame (without the trailing blank-line delimiter) into fields.
pub fn parse_sse_frame(frame: &str) -> Option<RawSseFrame> {
    let mut event: Option<String> = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in frame.lines() {
        if line.starts_with(':') {
            // Comment — ignore.
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            // Spec: optional single leading space after the colon is stripped.
            let payload = rest.strip_prefix(' ').unwrap_or(rest);
            data_lines.push(payload);
        }
    }

    if data_lines.is_empty() && event.is_none() {
        return None;
    }

    Some(RawSseFrame {
        event,
        data: data_lines.join("\n"),
    })
}

/// Classify a raw frame into [`SseFrame`].
///
/// - `event: lagged` with `{"skipped":N}` → [`SseFrame::Lagged`]
/// - otherwise any non-empty data → [`SseFrame::TraceData`]
pub fn classify_sse_frame(raw: &RawSseFrame) -> Option<SseFrame> {
    let is_lagged = raw
        .event
        .as_deref()
        .map(|e| e.eq_ignore_ascii_case("lagged"))
        .unwrap_or(false);

    if is_lagged {
        let skipped = serde_json::from_str::<LaggedPayload>(&raw.data)
            .map(|p| p.skipped)
            .unwrap_or(0);
        return Some(SseFrame::Lagged { skipped });
    }

    if raw.data.is_empty() {
        return None;
    }

    Some(SseFrame::TraceData(raw.data.clone()))
}

/// Parse + classify in one step.
pub fn parse_and_classify(frame: &str) -> Option<SseFrame> {
    let raw = parse_sse_frame(frame)?;
    classify_sse_frame(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_sse_data_parses_event_payload() {
        let frame =
            "event: message\ndata: {\"id\":\"01ABC\",\"kind\":{\"type\":\"request_received\"}}\n";
        let data = extract_sse_data(frame).unwrap();
        assert!(data.contains("01ABC"));
        assert!(data.contains("request_received"));
        assert!(!data.contains("not yet implemented"));
    }

    #[test]
    fn extract_sse_data_joins_multiline() {
        let frame = "data: line1\ndata: line2\n";
        assert_eq!(extract_sse_data(frame).unwrap(), "line1\nline2");
    }

    #[test]
    fn parse_sse_frame_joins_multiline_data() {
        let frame = "data: {\"a\":1}\ndata: {\"b\":2}\n";
        let raw = parse_sse_frame(frame).unwrap();
        assert_eq!(raw.data, "{\"a\":1}\n{\"b\":2}");
        assert!(raw.event.is_none());
    }

    #[test]
    fn parse_sse_frame_strips_single_leading_space_after_data_colon() {
        let frame = "data: {\"ok\":true}\n";
        let raw = parse_sse_frame(frame).unwrap();
        assert_eq!(raw.data, "{\"ok\":true}");
    }

    #[test]
    fn classify_distinguishes_lagged_from_trace_data() {
        let lagged_frame = "event: lagged\ndata: {\"skipped\":42}\n";
        let raw = parse_sse_frame(lagged_frame).unwrap();
        match classify_sse_frame(&raw).unwrap() {
            SseFrame::Lagged { skipped } => assert_eq!(skipped, 42),
            other => panic!("expected Lagged, got {:?}", other),
        }

        let trace_frame = "event: message\ndata: {\"id\":\"01XYZ\",\"trace_id\":\"t1\"}\n";
        let raw = parse_sse_frame(trace_frame).unwrap();
        match classify_sse_frame(&raw).unwrap() {
            SseFrame::TraceData(s) => {
                assert!(s.contains("01XYZ"));
                assert!(!s.contains("skipped"));
            }
            other => panic!("expected TraceData, got {:?}", other),
        }
    }

    #[test]
    fn parse_and_classify_lagged_without_event_message_default() {
        // Default frames (no event field) are trace data, not lagged.
        let frame = "data: {\"id\":\"x\"}\n";
        match parse_and_classify(frame).unwrap() {
            SseFrame::TraceData(s) => assert!(s.contains("\"id\":\"x\"")),
            other => panic!("expected TraceData, got {:?}", other),
        }
    }

    #[test]
    fn lagged_malformed_json_defaults_skipped_zero() {
        let frame = "event: lagged\ndata: not-json\n";
        match parse_and_classify(frame).unwrap() {
            SseFrame::Lagged { skipped } => assert_eq!(skipped, 0),
            other => panic!("expected Lagged, got {:?}", other),
        }
    }
}
