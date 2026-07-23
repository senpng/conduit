//! Logs tab state machine — isolated from the rest of [`super::app::App`].
//!
//! Own mode/level/day/follow/stream lifecycle so `App::handle_action` does not
//! accumulate `if tab == Logs` branches for every global key.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::console_client::ConsoleClient;
use crate::dto::{LogLineView, LogsMeta, LogsPage};

use super::action::Action;
use super::clipboard;
use super::msg::Msg;
use super::net;

/// Cap for the in-memory live/history ring.
pub const RING_MAX: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogsMode {
    #[default]
    Live,
    History,
}

impl LogsMode {
    pub fn label(self) -> &'static str {
        match self {
            LogsMode::Live => "live",
            LogsMode::History => "history",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            LogsMode::Live => LogsMode::History,
            LogsMode::History => LogsMode::Live,
        }
    }
}

/// Level floor sent to the console API (string form matches daemon `LevelFloor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogsLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl LogsLevel {
    const CYCLE: [LogsLevel; 5] = [
        LogsLevel::Error,
        LogsLevel::Warn,
        LogsLevel::Info,
        LogsLevel::Debug,
        LogsLevel::Trace,
    ];

    pub fn next(self) -> Self {
        let i = Self::CYCLE.iter().position(|&l| l == self).unwrap_or(2);
        Self::CYCLE[(i + 1) % Self::CYCLE.len()]
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
}

/// Side-effects the parent App should run after a Logs action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogsEffect {
    None,
    Status(String),
    /// Parent should bump gen and spawn meta + stream/history.
    Reload,
}

pub struct LogsState {
    pub mode: LogsMode,
    pub level: LogsLevel,
    pub date: String,
    pub meta: Option<LogsMeta>,
    /// Ring (oldest → newest).
    pub lines: VecDeque<LogLineView>,
    pub selected: usize,
    pub follow: bool,
    pub message: Option<String>,
    stream_cancel: Option<Arc<AtomicBool>>,
    stream_gen: u64,
}

impl Default for LogsState {
    fn default() -> Self {
        Self {
            mode: LogsMode::default(),
            level: LogsLevel::default(),
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            meta: None,
            lines: VecDeque::new(),
            selected: 0,
            follow: true,
            message: None,
            stream_cancel: None,
            stream_gen: 0,
        }
    }
}

impl LogsState {
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn selected_line(&self) -> Option<&LogLineView> {
        self.lines.get(self.selected)
    }

    pub fn stop_stream(&mut self) {
        if let Some(flag) = self.stream_cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
        self.stream_gen = 0;
    }

    /// Called when leaving the Logs tab or quitting the TUI.
    pub fn on_leave(&mut self) {
        self.stop_stream();
    }

    pub fn handle(&mut self, action: &Action) -> LogsEffect {
        match action {
            Action::ToggleLogsMode => {
                self.mode = self.mode.toggle();
                self.follow = self.mode == LogsMode::Live;
                self.lines.clear();
                self.selected = 0;
                LogsEffect::Reload
            }
            Action::CycleLogsLevel => {
                self.level = self.level.next();
                let status = format!("Logs level ≥ {}", self.level.as_str());
                self.lines.clear();
                self.selected = 0;
                // Reload then status — parent applies reload.
                let _ = status;
                LogsEffect::Reload
            }
            Action::ClearLogsBuffer => {
                if self.mode == LogsMode::Live {
                    self.lines.clear();
                    self.selected = 0;
                    self.follow = true;
                    LogsEffect::Status("Live buffer cleared".into())
                } else {
                    LogsEffect::None
                }
            }
            Action::CopyLogLine => {
                if let Some(line) = self.selected_line() {
                    match clipboard::copy_to_clipboard(&line.raw) {
                        Ok(()) => LogsEffect::Status("Copied log line".into()),
                        Err(e) => LogsEffect::Status(format!("Copy failed: {e}")),
                    }
                } else {
                    LogsEffect::None
                }
            }
            Action::LogsDayPrev => self.shift_date(-1),
            Action::LogsDayNext => self.shift_date(1),
            Action::Up | Action::ScrollDetailUp => {
                self.move_sel(-1);
                LogsEffect::None
            }
            Action::Down | Action::ScrollDetailDown => {
                self.move_sel(1);
                LogsEffect::None
            }
            Action::PageUp => {
                self.move_sel(-10);
                LogsEffect::None
            }
            Action::PageDown => {
                self.move_sel(10);
                LogsEffect::None
            }
            Action::GoTop => {
                self.selected = 0;
                self.follow = false;
                LogsEffect::None
            }
            Action::GoBottom => {
                self.selected = self.lines.len().saturating_sub(1);
                self.follow = self.mode == LogsMode::Live;
                LogsEffect::None
            }
            _ => LogsEffect::None,
        }
    }

    fn shift_date(&mut self, delta: i32) -> LogsEffect {
        if self.mode != LogsMode::History {
            return LogsEffect::Status("Day switch is for history mode (press f)".into());
        }
        let dates = self
            .meta
            .as_ref()
            .map(|m| m.available_dates.clone())
            .unwrap_or_default();
        if dates.is_empty() {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&self.date, "%Y-%m-%d") {
                if let Some(nd) = d.checked_add_signed(chrono::Duration::days(delta as i64)) {
                    self.date = nd.format("%Y-%m-%d").to_string();
                    self.lines.clear();
                    self.selected = 0;
                    return LogsEffect::Reload;
                }
            }
            return LogsEffect::None;
        }
        // available_dates is newest-first.
        let pos = dates.iter().position(|d| d == &self.date);
        let new_date = match (pos, delta.signum()) {
            (Some(i), -1) => dates.get(i + 1).cloned(),
            (Some(i), 1) if i > 0 => dates.get(i - 1).cloned(),
            (None, _) => dates.first().cloned(),
            _ => None,
        };
        if let Some(d) = new_date {
            self.date = d;
            self.lines.clear();
            self.selected = 0;
            LogsEffect::Reload
        } else {
            LogsEffect::Status("No more log dates in that direction".into())
        }
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = self.lines.len();
        if n == 0 {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, (n as i32) - 1) as usize;
        if self.selected != next {
            self.selected = next;
            let last = n.saturating_sub(1);
            self.follow = self.mode == LogsMode::Live && next == last;
        }
    }

    fn push_line(&mut self, line: LogLineView) {
        self.lines.push_back(line);
        while self.lines.len() > RING_MAX {
            self.lines.pop_front();
            if self.selected > 0 {
                self.selected -= 1;
            }
        }
        if self.follow {
            self.selected = self.lines.len().saturating_sub(1);
        }
    }

    /// Spawn network loads for the current mode. `gen` is shared by meta + data.
    pub fn start_load(
        &mut self,
        client: ConsoleClient,
        gen: u64,
        filter: &str,
        tx: tokio::sync::mpsc::UnboundedSender<Msg>,
    ) {
        self.message = None;
        net::spawn_logs_meta(client.clone(), gen, tx.clone());
        match self.mode {
            LogsMode::Live => {
                self.lines.clear();
                self.selected = 0;
                self.follow = true;
                self.start_stream(client, gen, filter, tx);
            }
            LogsMode::History => {
                self.stop_stream();
                net::spawn_logs_page(
                    client,
                    gen,
                    Some(self.date.clone()),
                    200,
                    None,
                    Some(self.level.as_str().to_string()),
                    if filter.is_empty() {
                        None
                    } else {
                        Some(filter.to_string())
                    },
                    tx,
                );
            }
        }
    }

    fn start_stream(
        &mut self,
        client: ConsoleClient,
        gen: u64,
        filter: &str,
        tx: tokio::sync::mpsc::UnboundedSender<Msg>,
    ) {
        self.stop_stream();
        self.stream_gen = gen;
        let cancel = Arc::new(AtomicBool::new(false));
        self.stream_cancel = Some(cancel.clone());
        net::spawn_logs_stream(
            client,
            gen,
            Some(self.level.as_str().to_string()),
            if filter.is_empty() {
                None
            } else {
                Some(filter.to_string())
            },
            100,
            cancel,
            tx,
        );
    }

    pub fn apply_msg(&mut self, msg: &Msg) -> Option<LogsEffect> {
        match msg {
            Msg::LogsMeta { gen, result } => {
                // Parent already dropped stale gens for data_gen; stream-only
                // meta still accepted when gen matches stream or is 0.
                let _ = gen;
                match result {
                    Ok(meta) => {
                        if !meta.enabled {
                            self.message = meta.message.clone().or_else(|| {
                                Some(
                                    "file logging disabled — enable [log] to_file and restart conduitd"
                                        .into(),
                                )
                            });
                        } else if let Some(m) = &meta.message {
                            self.message = Some(m.clone());
                        } else {
                            self.message = None;
                        }
                        if self.mode == LogsMode::Live {
                            self.date = meta.today.clone();
                        } else if self.date.is_empty() {
                            self.date = meta.today.clone();
                        }
                        self.meta = Some(meta.clone());
                        None
                    }
                    Err(e) => {
                        self.message = Some(e.clone());
                        Some(LogsEffect::Status(format!("logs meta: {e}")))
                    }
                }
            }
            Msg::LogsPage { result, .. } => match result {
                Ok(page) => {
                    self.apply_page(page);
                    let status = if self.lines.is_empty() && self.message.is_none() {
                        format!("No log lines for {}", self.date)
                    } else {
                        format!("Logs {} · {} lines", self.date, self.lines.len())
                    };
                    Some(LogsEffect::Status(status))
                }
                Err(e) => {
                    self.message = Some(e.clone());
                    Some(LogsEffect::Status(e.clone()))
                }
            },
            Msg::LogsStreamLine { gen, line } => {
                if *gen != 0 && *gen != self.stream_gen {
                    return None;
                }
                if self.mode != LogsMode::Live {
                    return None;
                }
                self.push_line(line.clone());
                None
            }
            Msg::LogsStreamEvent { gen, kind, message } => {
                if *gen != 0 && *gen != self.stream_gen {
                    return None;
                }
                match kind.as_str() {
                    "meta" => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(message) {
                            if let Some(d) = v.get("date").and_then(|x| x.as_str()) {
                                self.date = d.to_string();
                            }
                        }
                        None
                    }
                    "rotate" => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(message) {
                            if let Some(d) = v.get("date").and_then(|x| x.as_str()) {
                                self.date = d.to_string();
                            }
                        }
                        Some(LogsEffect::Status(format!("Log rotated → {}", self.date)))
                    }
                    "error" => {
                        self.message = Some(message.clone());
                        Some(LogsEffect::Status(format!("Logs stream: {message}")))
                    }
                    "ended" => {
                        if self.mode == LogsMode::Live {
                            Some(LogsEffect::Status(
                                "Logs stream ended — press r to reconnect".into(),
                            ))
                        } else {
                            None
                        }
                    }
                    _ => Some(LogsEffect::Status(message.clone())),
                }
            }
            _ => None,
        }
    }

    fn apply_page(&mut self, page: &LogsPage) {
        // HTTP 503 path now surfaces as Err; keep source check as belt-and-suspenders.
        if page.source == "unavailable" {
            self.message = Some(
                "file logging unavailable — enable [log] to_file and restart conduitd".into(),
            );
            self.lines.clear();
            self.selected = 0;
            return;
        }
        self.date = page.date.clone();
        self.lines = page.lines.iter().cloned().collect();
        self.selected = self.lines.len().saturating_sub(1);
        if self.mode == LogsMode::Live {
            self.follow = true;
        }
    }

    pub fn context_keybinds(&self) -> Vec<(&'static str, &'static str)> {
        vec![
            ("f", "live/hist"),
            ("l", "level"),
            ("[", "prev day"),
            ("]", "next day"),
            ("/", "filter"),
            ("G", "follow"),
            ("y", "copy"),
            ("c", "clear"),
        ]
    }
}

/// Draw the Logs tab body into `area`.
pub fn draw(frame: &mut ratatui::Frame, area: ratatui::layout::Rect, state: &LogsState, theme: &super::theme::Theme, loading: bool) {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

    use super::widgets::{empty_state, truncate};

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(area);

    let follow = if state.mode == LogsMode::Live {
        if state.follow {
            "follow ●"
        } else {
            "paused ○"
        }
    } else {
        "history"
    };
    let enabled = state.meta.as_ref().map(|m| m.enabled).unwrap_or(true);
    let title = format!(
        " Logs  {}  ·  {}  ·  level≥{}  ·  {}  ·  {} lines ",
        state.mode.label(),
        state.date,
        state.level.as_str(),
        follow,
        state.lines.len()
    );
    let header = Paragraph::new(Line::from(vec![
        Span::styled(title, theme.title()),
        if !enabled {
            Span::styled("  file logging off ", theme.error())
        } else {
            Span::raw("")
        },
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme.border())
            .style(theme.surface()),
    );
    frame.render_widget(header, chunks[0]);

    if let Some(msg) = &state.message {
        if state.lines.is_empty() {
            empty_state(frame, chunks[1], theme, "Logs unavailable", msg);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " selected line — empty ",
                    theme.muted(),
                )))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(theme.border())
                        .style(theme.surface()),
                ),
                chunks[2],
            );
            return;
        }
    }

    if state.lines.is_empty() {
        let hint = if loading {
            "Loading log lines…"
        } else {
            "No log lines — wait for traffic, press r, or switch day with [ ]"
        };
        empty_state(frame, chunks[1], theme, "Logs", hint);
    } else {
        let inner_h = chunks[1].height.saturating_sub(2) as usize;
        let sel = state.selected.min(state.lines.len() - 1);
        let start = list_window_start(sel, state.lines.len(), inner_h);
        let mut lines: Vec<Line> = Vec::new();
        for (i, line) in state.lines.iter().enumerate().skip(start).take(inner_h) {
            let active = i == sel;
            let level = line.level.as_deref().unwrap_or("");
            let ts = line
                .ts
                .as_deref()
                .map(|t| if t.len() >= 19 { &t[11..19] } else { t })
                .unwrap_or("        ");
            let target = line.target.as_deref().unwrap_or("");
            let msg = line.message.as_deref().unwrap_or(line.raw.as_str());
            let row = format!(
                "{ts}  {lvl:<5}  {tgt}  {msg}",
                lvl = if level.is_empty() { "?" } else { level },
                tgt = truncate(target, 22),
                msg = msg
            );
            let style = if active {
                theme.accent_bold()
            } else {
                theme.base()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if active { "▌ " } else { "  " },
                    if active {
                        theme.accent_bold()
                    } else {
                        theme.subtle()
                    },
                ),
                Span::styled(
                    truncate(&row, chunks[1].width.saturating_sub(4) as usize),
                    style,
                ),
            ]));
        }
        let list = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme.border())
                .title_top(Span::styled(
                    format!(
                        " {} ",
                        if state.mode == LogsMode::Live {
                            "stream"
                        } else {
                            "file"
                        }
                    ),
                    theme.muted(),
                ))
                .style(theme.surface()),
        );
        frame.render_widget(list, chunks[1]);
    }

    let detail = state
        .selected_line()
        .map(|l| l.raw.as_str())
        .unwrap_or("");
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .style(theme.base())
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme.border())
                    .title_top(Span::styled(" selected raw (y copy) ", theme.muted()))
                    .style(theme.surface()),
            ),
        chunks[2],
    );
}

fn list_window_start(selected: usize, len: usize, visible: usize) -> usize {
    if visible == 0 || len == 0 {
        return 0;
    }
    let visible = visible.min(len);
    let max_start = len.saturating_sub(visible);
    let start = selected.saturating_sub(visible.saturating_sub(1));
    start.min(max_start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticky_follow_pauses_on_scroll_up() {
        let mut s = LogsState::default();
        s.mode = LogsMode::Live;
        s.follow = true;
        for i in 0..3 {
            s.lines.push_back(LogLineView {
                raw: format!("line-{i}"),
                ..Default::default()
            });
        }
        s.selected = 2;
        s.move_sel(-1);
        assert!(!s.follow);
        assert_eq!(s.selected, 1);
        s.handle(&Action::GoBottom);
        assert!(s.follow);
        assert_eq!(s.selected, 2);
    }

    #[test]
    fn ring_uses_vecdeque_pop_front() {
        let mut s = LogsState::default();
        s.follow = true;
        for i in 0..(RING_MAX + 10) {
            s.push_line(LogLineView {
                raw: format!("{i}"),
                ..Default::default()
            });
        }
        assert_eq!(s.lines.len(), RING_MAX);
        assert_eq!(s.lines.front().unwrap().raw, "10");
    }

    #[test]
    fn stop_stream_sets_cancel_flag() {
        let mut s = LogsState::default();
        let flag = Arc::new(AtomicBool::new(false));
        s.stream_cancel = Some(flag.clone());
        s.stream_gen = 7;
        s.on_leave();
        assert!(flag.load(Ordering::Relaxed));
        assert!(s.stream_cancel.is_none());
        assert_eq!(s.stream_gen, 0);
    }

    #[test]
    fn level_cycles() {
        let mut l = LogsLevel::Error;
        for expected in ["warn", "info", "debug", "trace", "error"] {
            l = l.next();
            assert_eq!(l.as_str(), expected);
        }
    }
}
