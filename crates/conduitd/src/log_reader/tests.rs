//! Unit tests for [`crate::log_reader`] against real temporary log files.

use super::*;
use std::io::Write;

fn write_log(dir: &Path, date: &str, body: &str) {
    let p = log_path(dir, DEFAULT_LOG_PREFIX, date);
    let mut f = File::create(&p).unwrap();
    f.write_all(body.as_bytes()).unwrap();
}

#[test]
fn parse_pretty_extracts_level_and_target() {
    let line = parse_line(
        "2026-07-24T08:12:01.234Z  INFO conduit_pipeline::egress: upstream ok provider_id=x",
    );
    assert_eq!(line.level.as_deref(), Some("INFO"));
    assert_eq!(line.target.as_deref(), Some("conduit_pipeline::egress"));
    assert!(line.message.as_deref().unwrap_or("").contains("upstream ok"));
    assert!(line.raw.contains("upstream ok"));
}

#[test]
fn parse_json_extracts_fields() {
    let raw = r#"{"timestamp":"2026-07-24T08:12:01.234Z","level":"WARN","target":"conduitd","fields":{"message":"cooldown skip"}}"#;
    let line = parse_line(raw);
    assert_eq!(line.level.as_deref(), Some("WARN"));
    assert_eq!(line.target.as_deref(), Some("conduitd"));
    assert_eq!(line.message.as_deref(), Some("cooldown skip"));
    assert_eq!(line.raw, raw);
}

#[test]
fn unparseable_line_keeps_raw() {
    let line = parse_line("not a structured line at all");
    assert!(line.raw.contains("not a structured"));
    assert!(line.message.is_some() || !line.raw.is_empty());
}

#[test]
fn level_floor_filters() {
    let info = parse_line("2026-07-24T00:00:00Z  INFO t: hi");
    let warn = parse_line("2026-07-24T00:00:00Z  WARN t: bad");
    assert!(line_matches(&info, Some(LevelFloor::Info), None));
    assert!(!line_matches(&info, Some(LevelFloor::Warn), None));
    assert!(line_matches(&warn, Some(LevelFloor::Warn), None));
    assert!(!line_matches(&warn, Some(LevelFloor::Error), None));
}

#[test]
fn substring_filter_case_insensitive() {
    let line = parse_line("2026-07-24T00:00:00Z  INFO t: Provider_ID=claude");
    assert!(line_matches(&line, None, Some("provider_id")));
    assert!(!line_matches(&line, None, Some("missing-token")));
}

#[test]
fn list_dates_newest_first() {
    let tmp = tempfile::tempdir().unwrap();
    write_log(tmp.path(), "2026-07-22", "a\n");
    write_log(tmp.path(), "2026-07-24", "b\n");
    write_log(tmp.path(), "2026-07-23", "c\n");
    fs::write(tmp.path().join("other.log"), "x").unwrap();
    let dates = list_available_dates(tmp.path(), DEFAULT_LOG_PREFIX);
    assert_eq!(
        dates,
        vec![
            "2026-07-24".to_string(),
            "2026-07-23".to_string(),
            "2026-07-22".to_string()
        ]
    );
}

#[test]
fn tail_returns_last_n_chronological() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::new();
    for i in 1..=10 {
        body.push_str(&format!(
            "2026-07-24T00:00:{i:02}Z  INFO conduitd: line-{i}\n"
        ));
    }
    write_log(tmp.path(), "2026-07-24", &body);
    let page = read_page(
        tmp.path(),
        DEFAULT_LOG_PREFIX,
        "2026-07-24",
        3,
        None,
        PageDirection::Backward,
        None,
        None,
    )
    .unwrap();
    assert_eq!(page.lines.len(), 3);
    assert!(page.lines[0].raw.contains("line-8"));
    assert!(page.lines[1].raw.contains("line-9"));
    assert!(page.lines[2].raw.contains("line-10"));
    assert_eq!(page.source, "file");
}

#[test]
fn level_filter_on_page() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "\
2026-07-24T00:00:01Z  INFO conduitd: keep-info\n\
2026-07-24T00:00:02Z  WARN conduitd: keep-warn\n\
2026-07-24T00:00:03Z  ERROR conduitd: keep-error\n\
2026-07-24T00:00:04Z  DEBUG conduitd: drop-debug\n";
    write_log(tmp.path(), "2026-07-24", body);
    let page = read_page(
        tmp.path(),
        DEFAULT_LOG_PREFIX,
        "2026-07-24",
        50,
        None,
        PageDirection::Backward,
        Some(LevelFloor::Warn),
        None,
    )
    .unwrap();
    assert_eq!(page.lines.len(), 2);
    assert!(page
        .lines
        .iter()
        .all(|l| matches!(l.level.as_deref(), Some("WARN") | Some("ERROR"))));
}

#[test]
fn q_filter_on_page() {
    let tmp = tempfile::tempdir().unwrap();
    let body = "\
2026-07-24T00:00:01Z  INFO conduitd: alpha-one\n\
2026-07-24T00:00:02Z  INFO conduitd: beta-two\n\
2026-07-24T00:00:03Z  INFO conduitd: alpha-three\n";
    write_log(tmp.path(), "2026-07-24", body);
    let page = read_page(
        tmp.path(),
        DEFAULT_LOG_PREFIX,
        "2026-07-24",
        50,
        None,
        PageDirection::Backward,
        None,
        Some("alpha"),
    )
    .unwrap();
    assert_eq!(page.lines.len(), 2);
    assert!(page.lines.iter().all(|l| l.raw.contains("alpha")));
}

#[test]
fn cursor_backward_loads_older() {
    let tmp = tempfile::tempdir().unwrap();
    let mut body = String::new();
    for i in 1..=6 {
        body.push_str(&format!(
            "2026-07-24T00:00:{i:02}Z  INFO conduitd: line-{i}\n"
        ));
    }
    write_log(tmp.path(), "2026-07-24", &body);
    let page1 = read_page(
        tmp.path(),
        DEFAULT_LOG_PREFIX,
        "2026-07-24",
        2,
        None,
        PageDirection::Backward,
        None,
        None,
    )
    .unwrap();
    assert_eq!(page1.lines.len(), 2);
    assert!(page1.lines[0].raw.contains("line-5"));
    let cursor = page1.next_cursor.expect("next_cursor for older");
    let page2 = read_page(
        tmp.path(),
        DEFAULT_LOG_PREFIX,
        "2026-07-24",
        2,
        Some(&cursor),
        PageDirection::Backward,
        None,
        None,
    )
    .unwrap();
    assert_eq!(page2.lines.len(), 2);
    assert!(page2.lines[0].raw.contains("line-3"));
    assert!(page2.lines[1].raw.contains("line-4"));
}

#[test]
fn missing_file_returns_empty_page() {
    let tmp = tempfile::tempdir().unwrap();
    let page = read_page(
        tmp.path(),
        DEFAULT_LOG_PREFIX,
        "2099-01-01",
        10,
        None,
        PageDirection::Backward,
        None,
        None,
    )
    .unwrap();
    assert!(page.lines.is_empty());
    assert!(!page.truncated);
}

#[test]
fn meta_disabled_message() {
    let tmp = tempfile::tempdir().unwrap();
    let m = build_meta(
        false,
        tmp.path(),
        DEFAULT_LOG_PREFIX,
        "pretty",
        "info",
        Some("file logging disabled".into()),
    );
    assert!(!m.enabled);
    assert!(m.available_dates.is_empty());
    assert_eq!(m.message.as_deref(), Some("file logging disabled"));
    assert!(!m.today.is_empty());
}

#[test]
fn meta_lists_dates_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    write_log(tmp.path(), "2026-07-24", "hello marker-line\n");
    let m = build_meta(true, tmp.path(), DEFAULT_LOG_PREFIX, "pretty", "info", None);
    assert!(m.enabled);
    assert!(m.available_dates.contains(&"2026-07-24".to_string()));
}

#[test]
fn read_appended_only_complete_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let path = log_path(tmp.path(), DEFAULT_LOG_PREFIX, "2026-07-24");
    {
        let mut f = File::create(&path).unwrap();
        write!(f, "2026-07-24T00:00:01Z  INFO t: one\n").unwrap();
        write!(f, "partial").unwrap();
    }
    let (lines, off) =
        read_appended(tmp.path(), DEFAULT_LOG_PREFIX, "2026-07-24", 0, None, None).unwrap();
    assert_eq!(lines.len(), 1);
    assert!(lines[0].raw.contains("one"));
    {
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(f, "-done\n2026-07-24T00:00:02Z  INFO t: two\n").unwrap();
    }
    let (lines2, _) =
        read_appended(tmp.path(), DEFAULT_LOG_PREFIX, "2026-07-24", off, None, None).unwrap();
    assert_eq!(lines2.len(), 2);
    assert!(lines2[0].raw.contains("partial-done"));
    assert!(lines2[1].raw.contains("two"));
}

#[test]
fn cursor_encode_decode_roundtrip() {
    let c = ByteCursor { offset: 12345 };
    assert_eq!(ByteCursor::decode(&c.encode()), Some(c));
}

#[test]
fn page_direction_parse() {
    assert_eq!(PageDirection::parse("forward"), PageDirection::Forward);
    assert_eq!(PageDirection::parse("newer"), PageDirection::Forward);
    assert_eq!(PageDirection::parse("backward"), PageDirection::Backward);
    assert_eq!(PageDirection::parse(""), PageDirection::Backward);
}
