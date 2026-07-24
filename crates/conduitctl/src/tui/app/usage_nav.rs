//! Usage pagination / tab switch / logs dispatch / usage page apply / tick.

use super::super::action::Action;
use super::super::net;
use crate::dto::UsageListResponse;
use super::{App, Mode, Tab, UsageDetail, USAGE_PAGE_SIZE};

impl App {
    pub(crate) fn spawn_usage_load(&mut self, include_summary: bool) {
        self.loading = true;
        self.error = None;
        self.usage_applied_filter = self.filter.clone();
        let gen = self.next_gen();
        let q = if self.filter.is_empty() {
            None
        } else {
            Some(self.filter.clone())
        };
        if include_summary {
            net::spawn_usage(
                self.client.clone(),
                Some(self.usage_period.clone()),
                self.usage_offset,
                USAGE_PAGE_SIZE,
                q,
                Some(self.usage_sort.label().to_string()),
                gen,
                self.tx.clone(),
            );
        } else {
            net::spawn_usage_page(
                self.client.clone(),
                Some(self.usage_period.clone()),
                self.usage_offset,
                USAGE_PAGE_SIZE,
                q,
                Some(self.usage_sort.label().to_string()),
                gen,
                self.tx.clone(),
            );
        }
    }

    pub(crate) fn usage_last_offset(&self) -> usize {
        let total = self.usage_total as usize;
        if total == 0 {
            return 0;
        }
        ((total - 1) / USAGE_PAGE_SIZE) * USAGE_PAGE_SIZE
    }

    pub(crate) fn usage_next_page(&mut self) {
        let next = self.usage_offset.saturating_add(USAGE_PAGE_SIZE);
        if (next as u64) >= self.usage_total && self.usage_total > 0 {
            self.status = "Already on last page".into();
            return;
        }
        if self.usage_total == 0 {
            return;
        }
        self.usage_offset = next;
        self.selected[Tab::Usage.index()] = 0;
        self.usage_detail_scroll = 0;
        self.spawn_usage_load(false);
    }

    pub(crate) fn usage_prev_page(&mut self) {
        if self.usage_offset == 0 {
            self.status = "Already on first page".into();
            return;
        }
        self.usage_offset = self.usage_offset.saturating_sub(USAGE_PAGE_SIZE);
        self.selected[Tab::Usage.index()] = 0;
        self.usage_detail_scroll = 0;
        self.spawn_usage_load(false);
    }

    pub(crate) fn switch_tab(&mut self, tab: Tab) {
        if !matches!(self.mode, Mode::Browse | Mode::Filter) {
            return;
        }
        // Leaving Logs always stops the live SSE worker.
        if self.tab == Tab::Logs && tab != Tab::Logs {
            self.logs.on_leave();
        }
        self.mode = Mode::Browse;
        self.filter.clear();
        self.usage_applied_filter.clear();
        self.usage_offset = 0;
        self.tab = tab;
        // Reflect the new tab immediately; the pending load will refresh again
        // once its data arrives.
        self.refresh_filtered();
        self.request_refresh();
    }

    pub(crate) fn dispatch_logs(&mut self, action: &Action) {
        use super::super::logs::LogsEffect;
        match self.logs.handle(action) {
            LogsEffect::None => {
                self.selected[Tab::Logs.index()] = self.logs.selected;
            }
            LogsEffect::Status(s) => {
                self.status = s;
                self.selected[Tab::Logs.index()] = self.logs.selected;
                self.refresh_filtered();
            }
            LogsEffect::Reload => {
                if matches!(action, Action::CycleLogsLevel) {
                    self.status = format!("Logs level ≥ {}", self.logs.level.as_str());
                }
                self.selected[Tab::Logs.index()] = 0;
                self.request_refresh();
            }
        }
    }

    pub(crate) fn apply_usage_page(&mut self, page: UsageListResponse) {
        self.usage_recent = page.entries;
        self.usage_total = page.total;
        // Prefer server-reported offset/limit when present.
        if page.limit > 0 {
            // keep page size constant; offset from response if coherent
            self.usage_offset = page.offset;
        }
        if self.tab == Tab::Usage && self.usage_detail == UsageDetail::Recent {
            let from = self.usage_offset.saturating_add(1).min(self.usage_total as usize);
            let to = self
                .usage_offset
                .saturating_add(self.usage_recent.len())
                .min(self.usage_total as usize);
            let page_n = if self.usage_total == 0 {
                0
            } else {
                self.usage_offset / USAGE_PAGE_SIZE + 1
            };
            let pages = if self.usage_total == 0 {
                0
            } else {
                (self.usage_total as usize).div_ceil(USAGE_PAGE_SIZE)
            };
            self.status = if self.usage_total == 0 {
                "Usage: no matching requests".into()
            } else {
                format!("Usage {from}–{to} of {} · page {page_n}/{pages}", self.usage_total)
            };
        }
    }

    pub(crate) fn on_tick(&mut self) {
        self.tick_frame = self.tick_frame.wrapping_add(1);
        if let Mode::OauthFlow(f) = &mut self.mode {
            if f.pending_session_id.is_some() {
                f.poll_ticks = f.poll_ticks.wrapping_add(1);
                // ~2s at 200ms tick → every 10 ticks. Skip if a poll is still
                // in flight so a slow one can't let requests pile up.
                if f.poll_ticks % 10 == 0 && !f.poll_inflight {
                    if let Some(sid) = f.pending_session_id.clone() {
                        f.poll_inflight = true;
                        net::spawn_oauth_poll(self.client.clone(), sid, self.tx.clone());
                    }
                }
            }
        }
    }

}
