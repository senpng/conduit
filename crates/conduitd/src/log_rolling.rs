//! Local-timezone daily log rotation for conduitd.
//!
//! `tracing-appender::rolling::daily` rotates on **UTC** midnight, so on
//! UTC+9 hosts the file name lags the wall calendar by up to nine hours.
//! This appender uses the process local timezone (`chrono::Local`) so
//! `conduitd.log.YYYY-MM-DD` matches the operator's calendar day.

use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::{Datelike, Local, NaiveDate};

/// Daily-rolling file writer keyed by the **local** calendar date.
///
/// File names: `{prefix}.{YYYY-MM-DD}` (e.g. `conduitd.log.2026-07-24`).
/// Thread-safe: intended for use under `tracing_appender::non_blocking`.
pub struct LocalDailyRollingFile {
    directory: PathBuf,
    prefix: String,
    inner: Mutex<Inner>,
}

struct Inner {
    /// Local calendar date of the open file.
    date: NaiveDate,
    file: File,
}

impl LocalDailyRollingFile {
    /// Open (or create) today's local log file under `directory`.
    pub fn new(directory: impl Into<PathBuf>, prefix: impl Into<String>) -> io::Result<Self> {
        let directory = directory.into();
        let prefix = prefix.into();
        let date = local_today();
        let file = open_log_file(&directory, &prefix, date)?;
        Ok(Self {
            directory,
            prefix,
            inner: Mutex::new(Inner { date, file }),
        })
    }

    /// Path that would be used for `date` (tests / diagnostics).
    pub fn path_for_date(&self, date: NaiveDate) -> PathBuf {
        log_path(&self.directory, &self.prefix, date)
    }
}

impl Write for LocalDailyRollingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let today = local_today();
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.date != today {
            // Flush previous day before switching files.
            let _ = guard.file.flush();
            guard.file = open_log_file(&self.directory, &self.prefix, today)?;
            guard.date = today;
        }
        guard.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.file.flush()
    }
}

fn local_today() -> NaiveDate {
    Local::now().date_naive()
}

fn log_path(directory: &Path, prefix: &str, date: NaiveDate) -> PathBuf {
    directory.join(format!(
        "{prefix}.{:04}-{:02}-{:02}",
        date.year(),
        date.month(),
        date.day()
    ))
}

fn open_log_file(directory: &Path, prefix: &str, date: NaiveDate) -> io::Result<File> {
    let path = log_path(directory, prefix, date);
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn path_uses_local_calendar_date_not_utc_suffix_only() {
        let dir = PathBuf::from("/tmp/conduit-logs");
        let p = log_path(&dir, "conduitd.log", NaiveDate::from_ymd_opt(2026, 7, 24).unwrap());
        assert_eq!(
            p,
            PathBuf::from("/tmp/conduit-logs/conduitd.log.2026-07-24")
        );
    }

    #[test]
    fn writes_to_dated_file_and_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rolling =
            LocalDailyRollingFile::new(tmp.path(), "conduitd.log").expect("open rolling");
        let today = local_today();
        let expected = rolling.path_for_date(today);

        write!(rolling, "hello\n").unwrap();
        rolling.flush().unwrap();

        assert!(expected.is_file(), "expected log at {}", expected.display());
        let mut body = String::new();
        File::open(&expected)
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert!(body.contains("hello\n"), "body={body:?}");

        write!(rolling, "world\n").unwrap();
        rolling.flush().unwrap();
        body.clear();
        File::open(&expected)
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert!(body.contains("hello\n") && body.contains("world\n"));
    }

    #[test]
    fn rollover_opens_new_date_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut rolling =
            LocalDailyRollingFile::new(tmp.path(), "conduitd.log").expect("open rolling");

        // Force "yesterday" as the open file date, then write → should open today.
        let today = local_today();
        let yesterday = today
            .pred_opt()
            .expect("yesterday exists for any reasonable test date");
        {
            let mut guard = rolling.inner.lock().unwrap();
            // Create an empty yesterday file and point the handle at it.
            guard.file = open_log_file(tmp.path(), "conduitd.log", yesterday).unwrap();
            guard.date = yesterday;
        }

        write!(rolling, "after-rollover\n").unwrap();
        rolling.flush().unwrap();

        let today_path = log_path(tmp.path(), "conduitd.log", today);
        let yday_path = log_path(tmp.path(), "conduitd.log", yesterday);
        assert!(today_path.is_file());
        let mut body = String::new();
        File::open(&today_path)
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert!(
            body.contains("after-rollover\n"),
            "today file should get post-rollover write: {body:?}"
        );
        // Yesterday file should still exist (empty or without the new line).
        assert!(yday_path.is_file());
        let mut ybody = String::new();
        File::open(&yday_path)
            .unwrap()
            .read_to_string(&mut ybody)
            .unwrap();
        assert!(
            !ybody.contains("after-rollover"),
            "yesterday must not receive post-rollover write"
        );
    }
}
