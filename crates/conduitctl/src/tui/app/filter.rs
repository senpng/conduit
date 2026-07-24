//! Filter cache, selection, and list navigation.

use super::super::widgets::{days_in_month, parse_year_month};
use super::super::net;
use super::{App, Mode, PricingPane, Tab, UsageDetail};

impl App {
    pub fn list_len(&self) -> usize {
        self.filtered_indices().len()
    }

    /// Cached indices into the underlying data for the current tab after
    /// filter. Recomputed by [`Self::refresh_filtered`] whenever an input
    /// changes (filter text, tab, pane/detail, or the backing data); read
    /// zero-cost every frame here.
    pub fn filtered_indices(&self) -> &[usize] {
        &self.filtered
    }

    /// Rebuild the filtered-index cache. Call after any change to filter text,
    /// tab, pricing pane, usage detail, or the underlying lists.
    pub fn refresh_filtered(&mut self) {
        self.filtered = self.compute_filtered();
    }

    fn compute_filtered(&self) -> Vec<usize> {
        // Lower-case the needle once; match case-insensitively without
        // allocating a lowercased copy of every field (the old hot path
        // called `to_lowercase()` per field, per row, per frame).
        let q = self.filter.to_lowercase();
        let match_q = |s: &str| q.is_empty() || ci_contains(s, &q);

        match self.tab {
            Tab::Overview => vec![],
            Tab::Providers => self
                .providers
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    match_q(&p.name)
                        || match_q(&p.kind)
                        || match_q(&p.id)
                        || match_q(&p.base_url)
                })
                .map(|(i, _)| i)
                .collect(),
            Tab::Routes => self
                .routes
                .iter()
                .enumerate()
                .filter(|(_, r)| {
                    match_q(&r.match_alias) || match_q(&r.strategy) || match_q(&r.id)
                })
                .map(|(i, _)| i)
                .collect(),
            Tab::Keys => self
                .keys
                .iter()
                .enumerate()
                .filter(|(_, k)| match_q(&k.name) || match_q(&k.id))
                .map(|(i, _)| i)
                .collect(),
            Tab::Usage => match self.usage_detail {
                // Recent: filter is applied server-side; list is already scoped.
                UsageDetail::Recent => (0..self.usage_recent.len()).collect(),
                UsageDetail::ByModel => self
                    .usage_summary
                    .as_ref()
                    .map(|s| {
                        s.by_model
                            .iter()
                            .enumerate()
                            .filter(|(_, m)| {
                                match_q(&m.label)
                                    || match_q(&m.provider_kind)
                                    || match_q(&m.total_usd.to_string())
                            })
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default(),
                UsageDetail::ByKey => self
                    .usage_summary
                    .as_ref()
                    .map(|s| {
                        s.entries
                            .iter()
                            .enumerate()
                            .filter(|(_, e)| {
                                match_q(&e.downstream_key_id)
                                    || match_q(&e.name)
                                    || self
                                        .keys
                                        .iter()
                                        .find(|k| k.id == e.downstream_key_id)
                                        .map(|k| match_q(&k.name))
                                        .unwrap_or(false)
                            })
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default(),
                // Full calendar indices (day-of-month − 1), including zero-spend days
                // so the GitHub-style heatmap can highlight every cell.
                UsageDetail::ByDay => {
                    let period = self
                        .usage_summary
                        .as_ref()
                        .map(|s| s.period.as_str())
                        .unwrap_or(self.usage_period.as_str());
                    if let Some((y, m)) = parse_year_month(period) {
                        let n = days_in_month(y, m) as usize;
                        (0..n)
                            .filter(|&i| {
                                if q.is_empty() {
                                    return true;
                                }
                                let date = format!("{y:04}-{m:02}-{:02}", i + 1);
                                match_q(&date)
                                    || self
                                        .usage_summary
                                        .as_ref()
                                        .and_then(|s| {
                                            s.by_day.iter().find(|d| d.day == date)
                                        })
                                        .map(|d| {
                                            match_q(&d.total_usd.to_string())
                                                || match_q(&d.request_count.to_string())
                                        })
                                        .unwrap_or(false)
                            })
                            .collect()
                    } else {
                        self.usage_summary
                            .as_ref()
                            .map(|s| {
                                s.by_day
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, d)| match_q(&d.day))
                                    .map(|(i, _)| i)
                                    .collect()
                            })
                            .unwrap_or_default()
                    }
                }
                UsageDetail::ByProvider => self
                    .usage_summary
                    .as_ref()
                    .map(|s| {
                        s.by_provider
                            .iter()
                            .enumerate()
                            .filter(|(_, p)| {
                                let live_name = self
                                    .providers
                                    .iter()
                                    .find(|x| x.id == p.provider_id)
                                    .map(|x| x.name.as_str())
                                    .unwrap_or("");
                                match_q(&p.provider_id)
                                    || match_q(&p.name)
                                    || match_q(live_name)
                                    || match_q(&p.provider_kind)
                                    || match_q(&format!("{:.0}%", p.success_rate * 100.0))
                            })
                            .map(|(i, _)| i)
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            Tab::Pricing => {
                let rows = match self.pricing_pane {
                    PricingPane::Merged => &self.pricing,
                    PricingPane::Overrides => &self.pricing_overrides,
                };
                rows.iter()
                    .enumerate()
                    .filter(|(_, p)| match_q(&p.provider_kind) || match_q(&p.model_id))
                    .map(|(i, _)| i)
                    .collect()
            }
            // Logs: filter is applied server-side (and again on live stream);
            // the ring is already scoped.
            Tab::Logs => (0..self.logs.len()).collect(),
        }
    }

    /// Underlying data index for current selection (after filter).
    pub fn selected_data_index(&self) -> Option<usize> {
        let filtered = self.filtered_indices();
        let sel = self.selected[self.tab.index()];
        filtered.get(sel).copied()
    }

    pub(crate) fn move_sel(&mut self, delta: i32) {
        if matches!(self.mode, Mode::Filter) {
            // arrows still move selection while filter string is open
        } else if !matches!(self.mode, Mode::Browse) {
            if let Mode::RouteWizard(w) = &mut self.mode {
                if w.step == 1 {
                    let n = w.targets.len() as i32;
                    if n == 0 {
                        return;
                    }
                    // Clamp at ends — no wrap-around.
                    let next = (w.target_focus as i32 + delta).clamp(0, n - 1) as usize;
                    w.target_focus = next;
                }
            }
            return;
        }
        let n = self.list_len();
        if n == 0 {
            return;
        }
        if self.tab == Tab::Logs {
            self.logs.move_sel(delta);
            self.selected[Tab::Logs.index()] = self.logs.selected;
            return;
        }
        let idx = self.tab.index();
        // Clamp at first/last — no circular wrap (j on last stays put).
        let next = (self.selected[idx] as i32 + delta).clamp(0, (n as i32) - 1) as usize;
        if self.selected[idx] != next {
            self.selected[idx] = next;
            if self.tab == Tab::Usage {
                self.usage_detail_scroll = 0;
            }
        }
    }

    pub(crate) fn close_overlay(&mut self) {
        // Cancel oauth session if pending
        if let Mode::OauthFlow(f) = &self.mode {
            if let Some(sid) = &f.pending_session_id {
                net::spawn_oauth_cancel(self.client.clone(), sid.clone(), self.tx.clone());
            }
        }
        if matches!(self.mode, Mode::Filter) {
            // Esc/Enter in filter: keep filter text but leave edit mode.
            // Usage → recent applies filter server-side on leave.
            self.mode = Mode::Browse;
            if self.tab == Tab::Usage && self.filter != self.usage_applied_filter {
                self.usage_offset = 0;
                self.selected[Tab::Usage.index()] = 0;
                if self.usage_detail == UsageDetail::Recent {
                    self.spawn_usage_load(false);
                } else {
                    self.usage_applied_filter = self.filter.clone();
                }
            } else if self.tab == Tab::Logs {
                self.logs.lines.clear();
                self.logs.selected = 0;
                self.selected[Tab::Logs.index()] = 0;
                self.request_refresh();
            }
            return;
        }
        self.mode = Mode::Browse;
    }

    pub(crate) fn clamp_sel(&mut self, tab: Tab) {
        // Data or tab changed → rebuild the filtered-index cache before we read
        // its length below (and before the next frame reads it).
        self.refresh_filtered();
        let n = match tab {
            Tab::Providers => self.providers.len(),
            Tab::Routes => self.routes.len(),
            Tab::Keys => self.keys.len(),
            Tab::Usage => self.list_len(),
            Tab::Pricing => self.list_len(),
            Tab::Logs => self.logs.len(),
            Tab::Overview => 0,
        };
        let i = tab.index();
        if n == 0 {
            self.selected[i] = 0;
        } else if self.selected[i] >= n {
            self.selected[i] = n - 1;
        }
    }

}

/// Case-insensitive substring test where `needle` is already lower-cased.
/// Still lowercases `haystack` per call, but the filter result is now cached
/// (see [`App::refresh_filtered`]) so this runs only when inputs change rather
/// than on every frame.
fn ci_contains(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(needle)
}

