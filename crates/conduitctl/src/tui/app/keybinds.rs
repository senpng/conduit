//! Contextual footer keybinds and daemon health.

use super::{App, PricingPane, Tab, UsageDetail};

impl App {
    pub fn context_keybinds(&self) -> Vec<(&'static str, &'static str)> {
        let mut v = vec![
            ("?", "help"),
            ("q", "quit"),
            ("r", "refresh"),
            ("T", "theme"),
            ("tab", "next"),
        ];
        match self.tab {
            Tab::Overview => {
                v.extend([("t", "by day")]);
            }
            Tab::Providers => {
                v.extend([
                    ("a", "add"),
                    ("e", "edit"),
                    ("s", "set key"),
                    ("v", "view secret"),
                    ("y", "copy secret"),
                    ("Y", "copy token"),
                    ("o", "re-auth"),
                    ("x", "token ↻"),
                    ("u", "remaining"),
                    ("d", "delete"),
                    ("/", "filter"),
                ]);
            }
            Tab::Routes => {
                v.extend([("a", "add"), ("e", "edit"), ("d", "delete"), ("/", "filter")]);
            }
            Tab::Keys => {
                v.extend([
                    ("a", "create"),
                    ("e", "edit"),
                    ("v", "show token"),
                    ("d", "revoke"),
                    ("/", "filter"),
                ]);
            }
            Tab::Usage => {
                v.extend([
                    ("[", "prev mo"),
                    ("]", "next mo"),
                    ("c", "sort"),
                    ("t", "detail"),
                    ("/", "filter"),
                ]);
                if self.usage_detail == UsageDetail::Recent {
                    v.extend([
                        ("PgUp", "prev pg"),
                        ("PgDn", "next pg"),
                        ("^j/^k", "detail"),
                    ]);
                }
                if self.usage_detail == UsageDetail::ByDay {
                    v.extend([("↑↓", "day cell")]);
                }
            }
            Tab::Pricing => match self.pricing_pane {
                PricingPane::Merged => {
                    v.extend([
                        ("o", "overrides"),
                        ("a", "override selected"),
                        ("R", "reload"),
                        ("s", "sync"),
                        ("/", "filter"),
                    ]);
                }
                PricingPane::Overrides => {
                    // Edit/delete only apply in the overrides pane.
                    v.extend([
                        ("o", "merged"),
                        ("a", "add"),
                        ("e", "edit"),
                        ("d", "delete"),
                        ("/", "filter"),
                    ]);
                }
            },
            Tab::Logs => {
                v.extend(self.logs.context_keybinds());
            }
        }
        v
    }

    pub fn daemon_ok(&self) -> bool {
        self.health
            .as_ref()
            .map(|h| h.status.eq_ignore_ascii_case("ok") || h.status.eq_ignore_ascii_case("healthy"))
            .unwrap_or(false)
    }

}
