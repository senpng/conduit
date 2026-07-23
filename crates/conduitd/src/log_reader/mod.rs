//! Read and page daemon log files for the console API / TUI Logs tab.
//!
//! Pure IO helpers with no axum dependency. Files follow
//! [`crate::log_rolling`]: `{prefix}.{YYYY-MM-DD}` under a directory, rotated
//! on the **local** calendar day. Line parsing supports tracing-subscriber
//! `pretty` and `json` formats; unparseable lines always keep `raw`.

use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{Datelike, Local, NaiveDate};
use serde::{Deserialize, Serialize};

/// Default log file name prefix (matches `init_tracing` / log_rolling).
pub const DEFAULT_LOG_PREFIX: &str = "conduitd.log";
/// Maximum lines returned by a single history page.
pub const MAX_PAGE_LIMIT: usize = 1000;
/// Default history page size.
pub const DEFAULT_PAGE_LIMIT: usize = 200;
/// Max bytes scanned when seeking a window (protects huge files).
const MAX_SCAN_BYTES: u64 = 2 * 1024 * 1024;

// ── Public types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogsMeta {
    pub enabled: bool,
    pub dir: String,
    pub prefix: String,
    pub format: String,
    pub level_filter: String,
    /// Local calendar dates with a log file present, newest first.
    pub available_dates: Vec<String>,
    pub today: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogLine {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogsPage {
    pub date: String,
    pub lines: Vec<LogLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_cursor: Option<String>,
    pub truncated: bool,
    pub source: String,
}

/// Level floor for filtering (includes higher severities).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LevelFloor {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
}

impl LevelFloor {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

/// History page direction relative to a cursor (or default tail).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageDirection {
    /// Toward older lines (file start). Default for history.
    #[default]
    Backward,
    /// Toward newer lines (file end).
    Forward,
}

impl PageDirection {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "forward" | "fwd" | "newer" => Self::Forward,
            _ => Self::Backward,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteCursor {
    pub offset: u64,
}

impl ByteCursor {
    pub fn encode(self) -> String {
        format!("o{}", self.offset)
    }

    pub fn decode(s: &str) -> Option<Self> {
        let s = s.trim();
        let n = s.strip_prefix('o').unwrap_or(s);
        n.parse::<u64>().ok().map(|offset| Self { offset })
    }
}

// ── Paths / meta ────────────────────────────────────────────────────────────

pub fn local_today_string() -> String {
    format_date(Local::now().date_naive())
}

pub fn format_date(d: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    let parts: Vec<_> = s.trim().split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    NaiveDate::from_ymd_opt(parts[0].parse().ok()?, parts[1].parse().ok()?, parts[2].parse().ok()?)
}

pub fn log_path(directory: &Path, prefix: &str, date: &str) -> PathBuf {
    directory.join(format!("{prefix}.{date}"))
}

pub fn list_available_dates(directory: &Path, prefix: &str) -> Vec<String> {
    let Ok(rd) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let needle = format!("{prefix}.");
    let mut dates: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let suffix = name.strip_prefix(&needle)?.to_string();
            parse_date(&suffix)?;
            Some(suffix)
        })
        .collect();
    dates.sort();
    dates.reverse();
    dates
}

pub fn build_meta(
    enabled: bool,
    dir: &Path,
    prefix: &str,
    format: &str,
    level_filter: &str,
    message: Option<String>,
) -> LogsMeta {
    LogsMeta {
        enabled,
        dir: dir.display().to_string(),
        prefix: prefix.to_string(),
        format: format.to_string(),
        level_filter: level_filter.to_string(),
        available_dates: if enabled {
            list_available_dates(dir, prefix)
        } else {
            Vec::new()
        },
        today: local_today_string(),
        message,
    }
}

// ── Parse / filter ──────────────────────────────────────────────────────────

pub fn parse_line(raw: &str) -> LogLine {
    let raw_trim = raw.trim_end_matches(['\r', '\n']);
    if raw_trim.is_empty() {
        return LogLine {
            ts: None,
            level: None,
            target: None,
            message: None,
            raw: raw_trim.to_string(),
            offset: None,
        };
    }
    if raw_trim.starts_with('{') {
        if let Some(line) = parse_json_line(raw_trim) {
            return line;
        }
    }
    if let Some(line) = parse_pretty_line(raw_trim) {
        return line;
    }
    LogLine {
        ts: None,
        level: None,
        target: None,
        message: Some(raw_trim.to_string()),
        raw: raw_trim.to_string(),
        offset: None,
    }
}

fn parse_json_line(raw: &str) -> Option<LogLine> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let obj = v.as_object()?;
    let ts = obj
        .get("timestamp")
        .or_else(|| obj.get("time"))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let level = obj
        .get("level")
        .and_then(|x| x.as_str())
        .map(|s| s.to_ascii_uppercase());
    let target = obj
        .get("target")
        .or_else(|| obj.get("span").and_then(|s| s.get("name")))
        .and_then(|x| x.as_str())
        .map(str::to_string);
    let message = obj
        .get("fields")
        .and_then(|f| f.get("message"))
        .and_then(|x| x.as_str())
        .or_else(|| obj.get("message").and_then(|x| x.as_str()))
        .map(str::to_string);
    Some(LogLine {
        ts,
        level,
        target,
        message,
        raw: raw.to_string(),
        offset: None,
    })
}

/// tracing-subscriber pretty-ish: `2026-07-24T08:12:01.234Z  INFO target: message`
fn parse_pretty_line(raw: &str) -> Option<LogLine> {
    let s = strip_ansi(raw);
    let s = s.trim();
    let mut rest = s;
    let mut ts = None;
    if let Some(sp) = s.find(char::is_whitespace) {
        let candidate = &s[..sp];
        if candidate.contains('T') || candidate.len() >= 10 {
            ts = Some(candidate.to_string());
            rest = s[sp..].trim_start();
        }
    }
    let mut level = None;
    if let Some(sp) = rest.find(char::is_whitespace) {
        let tok = &rest[..sp];
        if LevelFloor::parse(tok).is_some() {
            level = Some(tok.to_ascii_uppercase());
            rest = rest[sp..].trim_start();
        }
    } else if !rest.is_empty() && LevelFloor::parse(rest).is_some() {
        level = Some(rest.to_ascii_uppercase());
        rest = "";
    }
    // Module paths use `::` — take the *last* colon as target/message separator.
    let (target, message) = if let Some(idx) = rest.rfind(':') {
        let t = rest[..idx].trim();
        let msg = rest[idx + 1..].trim();
        if !t.is_empty() && !t.contains(' ') {
            (Some(t.to_string()), Some(msg.to_string()))
        } else {
            (None, Some(rest.to_string()))
        }
    } else if !rest.is_empty() {
        (None, Some(rest.to_string()))
    } else {
        (None, None)
    };
    if ts.is_none() && level.is_none() && target.is_none() {
        return None;
    }
    Some(LogLine {
        ts,
        level,
        target,
        message,
        raw: raw.to_string(),
        offset: None,
    })
}

fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            i += 2;
            while i < bytes.len() {
                let c = bytes[i];
                i += 1;
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

pub fn line_matches(line: &LogLine, level_floor: Option<LevelFloor>, q: Option<&str>) -> bool {
    if let Some(floor) = level_floor {
        if let Some(lv) = line.level.as_deref().and_then(LevelFloor::parse) {
            if lv < floor {
                return false;
            }
        }
        // Unknown level: keep the line.
    }
    if let Some(q) = q.map(str::trim).filter(|s| !s.is_empty()) {
        let ql = q.to_ascii_lowercase();
        let hit = ci_contains(&line.raw, &ql)
            || line
                .message
                .as_deref()
                .map(|m| ci_contains(m, &ql))
                .unwrap_or(false)
            || line
                .target
                .as_deref()
                .map(|t| ci_contains(t, &ql))
                .unwrap_or(false);
        if !hit {
            return false;
        }
    }
    true
}

fn ci_contains(hay: &str, needle_lower: &str) -> bool {
    hay.to_ascii_lowercase().contains(needle_lower)
}

// ── File reading ────────────────────────────────────────────────────────────

/// Read a history page from the dated log file.
///
/// - No cursor → tail (newest `limit` matching lines, chronological).
/// - Cursor + [`PageDirection::Backward`] → lines strictly older than cursor.
/// - Cursor + [`PageDirection::Forward`] → lines at/after cursor.
pub fn read_page(
    directory: &Path,
    prefix: &str,
    date: &str,
    limit: usize,
    cursor: Option<&str>,
    direction: PageDirection,
    level_floor: Option<LevelFloor>,
    q: Option<&str>,
) -> io::Result<LogsPage> {
    let limit = limit.clamp(1, MAX_PAGE_LIMIT);
    let path = log_path(directory, prefix, date);
    if !path.is_file() {
        return Ok(empty_page(date, "file"));
    }
    let mut file = File::open(&path)?;
    let file_len = file.metadata()?.len();
    if file_len == 0 {
        return Ok(empty_page(date, "file"));
    }

    let filter = |line: &LogLine| line_matches(line, level_floor, q);

    let (matched, truncated) = if let Some(cur_s) = cursor {
        let cur = ByteCursor::decode(cur_s)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid cursor"))?;
        match direction {
            PageDirection::Backward => {
                let end = cur.offset.min(file_len);
                let start = end.saturating_sub(MAX_SCAN_BYTES);
                let truncated = start > 0;
                let mut m = scan_window(&mut file, start, end, &filter)?;
                // Drop lines at/after cursor; keep the newest `limit` older lines.
                m.retain(|(off, _)| *off < cur.offset);
                while m.len() > limit {
                    m.pop_front();
                }
                (m, truncated)
            }
            PageDirection::Forward => {
                let start = cur.offset.min(file_len);
                let end = (start + MAX_SCAN_BYTES).min(file_len);
                let truncated = end < file_len;
                let mut m = scan_window(&mut file, start, end, &filter)?;
                m.retain(|(off, _)| *off >= cur.offset);
                while m.len() > limit {
                    m.pop_back();
                }
                (m, truncated)
            }
        }
    } else {
        // Tail: newest page.
        let start = file_len.saturating_sub(MAX_SCAN_BYTES);
        let truncated = start > 0;
        let mut m = scan_window(&mut file, start, file_len, &filter)?;
        while m.len() > limit {
            m.pop_front();
        }
        (m, truncated)
    };

    Ok(page_from_matched(date, matched, truncated))
}

fn empty_page(date: &str, source: &str) -> LogsPage {
    LogsPage {
        date: date.to_string(),
        lines: Vec::new(),
        next_cursor: None,
        prev_cursor: None,
        truncated: false,
        source: source.into(),
    }
}

/// Scan `[start, end)` for complete lines, applying `filter`. Returns chronological matches.
fn scan_window<F>(
    file: &mut File,
    start: u64,
    end: u64,
    filter: &F,
) -> io::Result<VecDeque<(u64, LogLine)>>
where
    F: Fn(&LogLine) -> bool,
{
    let chunk = read_range(file, start, end)?;
    let (aligned_start, text) = align_to_line_start(start, &chunk);
    let mut matched = VecDeque::new();
    for (off, raw) in split_lines_with_offsets(aligned_start, &text) {
        let mut line = parse_line(&raw);
        line.offset = Some(off);
        if filter(&line) {
            matched.push_back((off, line));
        }
    }
    Ok(matched)
}

fn page_from_matched(
    date: &str,
    matched: VecDeque<(u64, LogLine)>,
    truncated_scan: bool,
) -> LogsPage {
    let has_older = truncated_scan || matched.front().map(|(o, _)| *o > 0).unwrap_or(false);
    let next_cursor = if has_older {
        matched
            .front()
            .map(|(off, _)| ByteCursor { offset: *off }.encode())
    } else {
        None
    };
    let prev_cursor = matched.back().map(|(off, line)| {
        let end = off.saturating_add(line.raw.len() as u64).saturating_add(1);
        ByteCursor { offset: end }.encode()
    });
    LogsPage {
        date: date.to_string(),
        lines: matched.into_iter().map(|(_, l)| l).collect(),
        next_cursor,
        prev_cursor,
        truncated: truncated_scan,
        source: "file".into(),
    }
}

fn read_range(file: &mut File, start: u64, end: u64) -> io::Result<Vec<u8>> {
    if end <= start {
        return Ok(Vec::new());
    }
    let len = (end - start) as usize;
    file.seek(SeekFrom::Start(start))?;
    let mut buf = vec![0u8; len];
    let mut read_total = 0;
    while read_total < len {
        match file.read(&mut buf[read_total..]) {
            Ok(0) => break,
            Ok(n) => read_total += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    buf.truncate(read_total);
    Ok(buf)
}

fn align_to_line_start(start_offset: u64, chunk: &[u8]) -> (u64, String) {
    if start_offset == 0 {
        return (0, String::from_utf8_lossy(chunk).into_owned());
    }
    if let Some(pos) = chunk.iter().position(|&b| b == b'\n') {
        let off = start_offset + pos as u64 + 1;
        let text = String::from_utf8_lossy(&chunk[pos + 1..]).into_owned();
        (off, text)
    } else {
        (start_offset + chunk.len() as u64, String::new())
    }
}

fn split_lines_with_offsets(start_offset: u64, text: &str) -> Vec<(u64, String)> {
    let mut out = Vec::new();
    let mut off = start_offset;
    for line in text.split_inclusive('\n') {
        let is_nl = line.ends_with('\n');
        let content = if is_nl { &line[..line.len() - 1] } else { line };
        let content = content.strip_suffix('\r').unwrap_or(content);
        if !content.is_empty() {
            out.push((off, content.to_string()));
        }
        off += line.len() as u64;
    }
    out
}

/// Read new complete lines appended after `from_offset`.
pub fn read_appended(
    directory: &Path,
    prefix: &str,
    date: &str,
    from_offset: u64,
    level_floor: Option<LevelFloor>,
    q: Option<&str>,
) -> io::Result<(Vec<LogLine>, u64)> {
    let path = log_path(directory, prefix, date);
    if !path.is_file() {
        return Ok((Vec::new(), from_offset));
    }
    let mut file = File::open(&path)?;
    let file_len = file.metadata()?.len();
    if file_len <= from_offset {
        return Ok((Vec::new(), from_offset));
    }
    let chunk = read_range(&mut file, from_offset, file_len)?;
    let (complete, consumed) = split_complete_lines(from_offset, &chunk);
    let mut lines = Vec::new();
    for (off, raw) in complete {
        let mut line = parse_line(&raw);
        line.offset = Some(off);
        if line_matches(&line, level_floor, q) {
            lines.push(line);
        }
    }
    Ok((lines, from_offset + consumed))
}

fn split_complete_lines(start_offset: u64, chunk: &[u8]) -> (Vec<(u64, String)>, u64) {
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line_start = 0usize;
    while i < chunk.len() {
        if chunk[i] == b'\n' {
            let slice = &chunk[line_start..i];
            let content = std::str::from_utf8(slice)
                .map(|s| s.strip_suffix('\r').unwrap_or(s).to_string())
                .unwrap_or_else(|_| String::from_utf8_lossy(slice).into_owned());
            if !content.is_empty() {
                out.push((start_offset + line_start as u64, content));
            }
            i += 1;
            line_start = i;
        } else {
            i += 1;
        }
    }
    (out, line_start as u64)
}

pub fn file_len(directory: &Path, prefix: &str, date: &str) -> io::Result<u64> {
    let path = log_path(directory, prefix, date);
    if !path.is_file() {
        return Ok(0);
    }
    Ok(fs::metadata(path)?.len())
}

/// Tail `limit` matching lines and return (lines, eof_offset) for follow seed.
pub fn tail_for_follow(
    directory: &Path,
    prefix: &str,
    date: &str,
    limit: usize,
    level_floor: Option<LevelFloor>,
    q: Option<&str>,
) -> io::Result<(Vec<LogLine>, u64)> {
    let page = read_page(
        directory,
        prefix,
        date,
        limit,
        None,
        PageDirection::Backward,
        level_floor,
        q,
    )?;
    let eof = file_len(directory, prefix, date)?;
    Ok((page.lines, eof))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
