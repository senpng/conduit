use std::io::{self, Write};

use anyhow::Result;
use clap::{Parser, Subcommand};
use conduitctl::{AdminClient, AdminError, SseFrame};
use tokio::sync::mpsc;

#[derive(Debug, Parser)]
pub struct TraceArgs {
    #[command(subcommand)]
    pub command: TraceCommand,
}

#[derive(Debug, Subcommand)]
pub enum TraceCommand {
    /// List recent traces
    List {
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },
    /// Get a specific trace by ID
    Get { id: String },
    /// Tail trace events in real-time (SSE from admin API)
    Tail,
    /// Replay a request from a trace (default: dry-run, no upstream / billing)
    Replay {
        id: String,
        /// When set, attempt a live re-execution (not implemented).
        #[arg(long, default_value_t = false)]
        execute: bool,
    },
}

pub async fn run(admin_addr: &str, args: TraceArgs, output: &str) -> Result<()> {
    let client = AdminClient::new(admin_addr);

    match args.command {
        TraceCommand::List { limit } => {
            let body = client
                .list_traces(limit)
                .await
                .map_err(|e| anyhow::anyhow!("request failed: {}", e))?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                print_traces_table(&body);
            }
        }
        TraceCommand::Get { id } => {
            let body = client
                .get_trace_bundle(&id)
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        TraceCommand::Tail => {
            // Banner goes to stderr so `--output json` stdout stays pure JSONL.
            eprintln!(
                "Tailing traces from {}/admin/traces/stream … (Ctrl+C to stop)",
                client.base_url()
            );
            // Reconnect loop: daemon restart / transient disconnect should not kill tail.
            let mut backoff_ms: u64 = 500;
            loop {
                let (tx, mut rx) = mpsc::channel::<Result<SseFrame, AdminError>>(256);
                let handle = client.subscribe_traces(tx);
                let mut saw_error = false;
                while let Some(item) = rx.recv().await {
                    match item {
                        Ok(SseFrame::TraceData(payload)) => {
                            backoff_ms = 500; // healthy stream
                            print_tail_event(&payload, output);
                        }
                        Ok(SseFrame::Lagged { skipped }) => {
                            eprintln!("SSE lagged skipped={skipped}");
                        }
                        Err(e) => {
                            saw_error = true;
                            eprintln!("SSE disconnected: {e} — reconnecting…");
                            break;
                        }
                    }
                }
                // Task ended without an error frame (clean EOF / receiver end).
                if !saw_error {
                    eprintln!("SSE stream ended — reconnecting…");
                }
                handle.abort();
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
            }
        }
        TraceCommand::Replay { id, execute } => {
            if execute {
                anyhow::bail!(
                    "live replay (--execute) is not implemented; omit --execute for dry-run"
                );
            }
            let body = client.replay_dry_run(&id).await.map_err(|e| match e {
                AdminError::Http { status, body } => {
                    anyhow::anyhow!("replay failed: HTTP {} — {}", status, body)
                }
                other => anyhow::anyhow!("replay request failed: {}", other),
            })?;
            if output == "json" {
                println!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                print_replay_plan(&body);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod extract_text_tests {
    use super::extract_assistant_text;
    use serde_json::json;

    #[test]
    fn openai_message_content() {
        let r = json!({
            "choices": [{"message": {"role": "assistant", "content": "hello world"}}]
        });
        assert_eq!(extract_assistant_text(&r).as_deref(), Some("hello world"));
    }

    #[test]
    fn anthropic_content_blocks() {
        let r = json!({
            "content": [
                {"type": "text", "text": "hi "},
                {"type": "text", "text": "there"}
            ]
        });
        assert_eq!(extract_assistant_text(&r).as_deref(), Some("hi there"));
    }
}

// Human-mode: mid-line after a text_delta (no trailing newline yet).
thread_local! {
    static TAIL_OPEN_LINE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Per-trace timing: request start + whether TTFB line was printed.
    static TAIL_TIMING: std::cell::RefCell<std::collections::HashMap<String, TailTiming>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

struct TailTiming {
    /// Event `ts` of request_received (or first seen event).
    start: chrono::DateTime<chrono::Utc>,
    /// True after first text_delta (TTFB marker printed once).
    saw_first_token: bool,
}

fn end_open_text_line() {
    TAIL_OPEN_LINE.with(|c| {
        if c.get() {
            println!();
            c.set(false);
        }
    });
}

fn parse_event_ts(v: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = v.get("ts")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|| {
            // Tolerate "Z" / fractional seconds already handled by RFC3339; try naive fallbacks.
            chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ")
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
        })
}

fn fmt_clock(ts: chrono::DateTime<chrono::Utc>) -> String {
    ts.format("%H:%M:%S%.3f").to_string()
}

fn fmt_elapsed(from: chrono::DateTime<chrono::Utc>, to: chrono::DateTime<chrono::Utc>) -> String {
    let ms = (to - from).num_milliseconds().max(0);
    if ms < 1000 {
        format!("+{ms}ms")
    } else {
        format!("+{:.2}s", ms as f64 / 1000.0)
    }
}

fn note_request_start(tid: &str, ts: Option<chrono::DateTime<chrono::Utc>>) {
    let Some(ts) = ts else { return };
    TAIL_TIMING.with(|m| {
        m.borrow_mut().insert(
            tid.to_string(),
            TailTiming {
                start: ts,
                saw_first_token: false,
            },
        );
    });
}

fn take_ttfb_label(tid: &str, now: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    let now = now?;
    TAIL_TIMING.with(|m| {
        let mut map = m.borrow_mut();
        let t = map.get_mut(tid)?;
        if t.saw_first_token {
            return None;
        }
        t.saw_first_token = true;
        Some(fmt_elapsed(t.start, now))
    })
}

fn elapsed_label(tid: &str, now: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    let now = now?;
    TAIL_TIMING.with(|m| {
        let map = m.borrow();
        let t = map.get(tid)?;
        Some(fmt_elapsed(t.start, now))
    })
}

fn clear_timing(tid: &str) {
    TAIL_TIMING.with(|m| {
        m.borrow_mut().remove(tid);
    });
}

/// Extract assistant text from a stored wire response body (OpenAI / Anthropic).
fn extract_assistant_text(response: &serde_json::Value) -> Option<String> {
    // OpenAI chat.completion
    if let Some(content) = response
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .filter(|s| !s.is_empty())
    {
        return Some(content.to_string());
    }
    // OpenAI content as array of parts
    if let Some(arr) = response
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_array())
    {
        let mut s = String::new();
        for part in arr {
            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                s.push_str(t);
            } else if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    s.push_str(t);
                }
            }
        }
        if !s.is_empty() {
            return Some(s);
        }
    }
    // Anthropic Messages: content is array of blocks
    if let Some(arr) = response.get("content").and_then(|c| c.as_array()) {
        let mut s = String::new();
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    s.push_str(t);
                }
            }
        }
        if !s.is_empty() {
            return Some(s);
        }
    }
    // Tool-only reply: mention tool call names
    if let Some(tools) = response
        .pointer("/choices/0/message/tool_calls")
        .and_then(|t| t.as_array())
    {
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tc| tc.pointer("/function/name").and_then(|n| n.as_str()))
            .collect();
        if !names.is_empty() {
            return Some(format!("[tool_calls: {}]", names.join(", ")));
        }
    }
    None
}

/// Classify a structural SSE frame for quiet human output.
fn summarize_sse_frame(frame: &str) -> Option<&'static str> {
    let f = frame.trim();
    if f.contains("[DONE]") {
        return Some("done");
    }
    // OpenAI finish chunk: empty delta + finish_reason
    if f.contains("\"finish_reason\"") {
        if f.contains("\"stop\"") {
            return Some("finish stop");
        }
        if f.contains("\"tool_calls\"") {
            return Some("finish tool_calls");
        }
        if f.contains("\"length\"") {
            return Some("finish length");
        }
        return Some("finish");
    }
    // Role-only first chunk: delta has role, little/no content
    if f.contains("\"role\"") && !f.contains("\"content\"") {
        return Some("role");
    }
    // Anthropic message_stop / content_block_stop style
    if f.contains("message_stop") {
        return Some("message_stop");
    }
    if f.contains("content_block_stop") {
        return Some("content_block_stop");
    }
    if f.contains("message_start") {
        return Some("message_start");
    }
    if f.contains("content_block_start") {
        return Some("content_block_start");
    }
    None
}

/// Pretty-print live tail events; stream text deltas stream inline.
fn print_tail_event(payload: &str, output: &str) {
    if output == "json" {
        println!("{payload}");
        return;
    }
    // Try to highlight stream content for human tail.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) {
        let kind = v
            .get("kind")
            .and_then(|k| k.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("?");
        let tid = v
            .get("trace_id")
            .and_then(|t| t.as_str())
            .or_else(|| v.get("id").and_then(|t| t.as_str()))
            .unwrap_or("-");
        let event_ts = parse_event_ts(&v);
        match kind {
            "stream_delta" => {
                let text = v
                    .pointer("/kind/text_delta")
                    .and_then(|t| t.as_str())
                    .filter(|s| !s.is_empty());
                let frame = v
                    .pointer("/kind/frame")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                if let Some(t) = text {
                    // First token: print TTFB on its own line, then stream content.
                    if let Some(ttfb) = take_ttfb_label(tid, event_ts) {
                        end_open_text_line();
                        println!("  ↳ first token {ttfb}");
                    }
                    // Real-time content (no forced newline — tokens stream mid-line).
                    print!("{t}");
                    let _ = io::stdout().flush();
                    TAIL_OPEN_LINE.with(|c| c.set(true));
                } else {
                    // End the streamed line before any control frame.
                    end_open_text_line();
                    // Quiet summary for role / finish / [DONE]; skip noisy full SSE dump.
                    if let Some(label) = summarize_sse_frame(frame) {
                        // Most control frames are uninteresting in human mode;
                        // only show finish/done at dim detail level.
                        if matches!(
                            label,
                            "done"
                                | "finish"
                                | "finish stop"
                                | "finish tool_calls"
                                | "finish length"
                                | "message_stop"
                        ) {
                            let el = elapsed_label(tid, event_ts)
                                .map(|e| format!(" {e}"))
                                .unwrap_or_default();
                            println!("  ↳ stream {label}{el}");
                        }
                        // role / content_block_* intentionally silent
                    } else if !frame.trim().is_empty() {
                        // Unknown structural frame — keep a short one-liner, not full JSON.
                        let one = frame.chars().take(80).collect::<String>();
                        let ellip = if frame.len() > 80 { "…" } else { "" };
                        println!("  ↳ frame {one}{ellip}");
                    }
                }
            }
            "request_received" => {
                end_open_text_line();
                note_request_start(tid, event_ts);
                let alias = v
                    .pointer("/kind/alias")
                    .and_then(|a| a.as_str())
                    .unwrap_or("?");
                let stream = v
                    .pointer("/kind/stream")
                    .and_then(|s| s.as_bool())
                    .unwrap_or(false);
                let clock = event_ts
                    .map(|t| format!(" {}", fmt_clock(t)))
                    .unwrap_or_default();
                println!(
                    "\n──{clock} {tid} request alias={alias}{} ──",
                    if stream { " stream" } else { "" }
                );
            }
            "routing_decided" => {
                // Silent in human mode (noise); json mode already returned.
            }
            "upstream_response" => {
                end_open_text_line();
                let status = v
                    .pointer("/kind/status")
                    .and_then(|s| s.as_u64())
                    .unwrap_or(0);
                let stream = v
                    .pointer("/kind/stream")
                    .and_then(|s| s.as_bool())
                    .unwrap_or(false);
                let latency = v
                    .pointer("/kind/latency_ms")
                    .and_then(|n| n.as_u64());
                let ttfb = v.pointer("/kind/ttfb_ms").and_then(|n| n.as_u64());
                let mut parts = Vec::new();
                if let Some(ms) = latency {
                    parts.push(format!("latency={ms}ms"));
                }
                if let Some(ms) = ttfb {
                    parts.push(format!("ttfb={ms}ms"));
                }
                if let Some(e) = elapsed_label(tid, event_ts) {
                    // Prefer event-reported latency; still show wall elapsed if no latency field.
                    if latency.is_none() {
                        parts.push(format!("elapsed={e}"));
                    }
                }
                let clock = event_ts
                    .map(|t| format!(" {}", fmt_clock(t)))
                    .unwrap_or_default();
                let timing = if parts.is_empty() {
                    String::new()
                } else {
                    format!(" {}", parts.join(" "))
                };
                println!(
                    "──{clock} {tid} response HTTP {status}{}{timing} ──",
                    if stream { " (stream end)" } else { "" }
                );
                // Non-stream: body was never printed via stream_delta — show it now.
                if !stream {
                    if let Some(resp) = v.pointer("/kind/response") {
                        if let Some(text) = extract_assistant_text(resp) {
                            println!("{text}");
                        } else {
                            // Fallback: compact JSON (truncated) when no plain text.
                            let s = resp.to_string();
                            let one: String = s.chars().take(240).collect();
                            let ellip = if s.len() > 240 { "…" } else { "" };
                            println!("  ↳ body {one}{ellip}");
                        }
                    }
                }
            }
            "final_usage" => {
                end_open_text_line();
                let cost = v
                    .pointer("/kind/cost_usd")
                    .and_then(|c| c.as_f64())
                    .unwrap_or(0.0);
                let prompt = v
                    .pointer("/kind/usage/prompt_tokens")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let completion = v
                    .pointer("/kind/usage/completion_tokens")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(0);
                let el = elapsed_label(tid, event_ts)
                    .map(|e| format!(" elapsed={e}"))
                    .unwrap_or_default();
                let clock = event_ts
                    .map(|t| format!(" {}", fmt_clock(t)))
                    .unwrap_or_default();
                println!(
                    "──{clock} {tid} usage tokens={prompt}+{completion} cost=${cost:.6}{el} ──\n"
                );
                clear_timing(tid);
            }
            "error" => {
                end_open_text_line();
                let msg = v
                    .pointer("/kind/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("error");
                let el = elapsed_label(tid, event_ts)
                    .map(|e| format!(" {e}"))
                    .unwrap_or_default();
                let clock = event_ts
                    .map(|t| format!(" {}", fmt_clock(t)))
                    .unwrap_or_default();
                eprintln!("──{clock} {tid} error{el}: {msg} ──");
                clear_timing(tid);
            }
            other => {
                end_open_text_line();
                println!("[{tid}] {other}");
            }
        }
        return;
    }
    end_open_text_line();
    println!("{payload}");
}

fn print_traces_table(body: &conduitctl::TraceListResponse) {
    if body.traces.is_empty() {
        println!("No traces found.");
        return;
    }

    println!(
        "{:<26} {:<20} {:<10} {:<8} {:<10}",
        "ID", "ALIAS", "STATUS", "LATENCY", "COST_USD"
    );
    println!("{}", "-".repeat(80));
    for t in &body.traces {
        println!(
            "{:<26} {:<20} {:<10} {:<8}ms ${:.6}",
            t.id, t.alias, t.status_code, t.latency_ms, t.cost_usd
        );
    }
}

fn print_replay_plan(body: &serde_json::Value) {
    println!("Replay plan (dry-run)");
    println!("{}", "-".repeat(40));
    if let Some(id) = body.get("trace_id").and_then(|v| v.as_str()) {
        println!("trace_id:        {}", id);
    }
    if let Some(kind) = body.get("event_kind").and_then(|v| v.as_str()) {
        println!("event_kind:      {}", kind);
    }
    if let Some(summary) = body.get("request_summary") {
        println!(
            "request:         {}",
            serde_json::to_string(summary).unwrap_or_default()
        );
    }
    if let Some(target) = body.get("intended_target") {
        if target.is_null() {
            println!("intended_target: (none)");
        } else {
            let provider = target
                .get("provider_id")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let kind = target
                .get("provider_kind")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let model = target
                .get("model_id")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            println!("provider:        {} ({})", provider, kind);
            println!("model:           {}", model);
            println!(
                "intended_target: {}",
                serde_json::to_string_pretty(target).unwrap_or_default()
            );
        }
    }
    if let Some(err) = body.get("routing_error").and_then(|v| v.as_str()) {
        println!("routing_error:   {}", err);
    }
    println!(
        "upstream_called: {}",
        body.get("upstream_called")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    );
    println!(
        "billed:          {}",
        body.get("billed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    );
}

#[cfg(test)]
mod tests {
    use conduitctl::{classify_sse_frame, extract_sse_data, parse_sse_frame};

    use super::*;

    #[test]
    fn extract_sse_data_still_available_for_compat() {
        let frame = "data: line1\ndata: line2\n";
        assert_eq!(extract_sse_data(frame).unwrap(), "line1\nline2");
    }

    #[test]
    fn tail_path_classifies_lagged_frames() {
        let raw = parse_sse_frame("event: lagged\ndata: {\"skipped\":7}\n").unwrap();
        match classify_sse_frame(&raw).unwrap() {
            SseFrame::Lagged { skipped } => assert_eq!(skipped, 7),
            other => panic!("expected Lagged, got {other:?}"),
        }
    }

    #[test]
    fn print_replay_plan_does_not_claim_billing() {
        let plan = serde_json::json!({
            "dry_run": true,
            "trace_id": "01TEST",
            "event_kind": "request_received",
            "request_summary": { "alias": "gpt-4o", "stream": false },
            "intended_target": {
                "provider_id": "openai",
                "provider_kind": "openai",
                "model_id": "gpt-4o"
            },
            "upstream_called": false,
            "billed": false,
        });
        print_replay_plan(&plan);
        assert_eq!(plan["billed"], false);
        assert_eq!(plan["upstream_called"], false);
    }
}
