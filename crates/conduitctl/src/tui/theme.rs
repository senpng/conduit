//! Visual theme for the operator TUI (tokscale / k9s-inspired console).
//!
//! Supports dark / light palettes, optional follow-system (`auto`), and
//! `CONDUIT_THEME=dark|light|auto`.

use std::process::Command;

use ratatui::style::{Color, Modifier, Style};

/// Resolved palette kind (what is currently painted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

impl ThemeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// User preference: follow system, or force a palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    /// Resolve from OS / terminal (`COLORFGBG`, macOS appearance, …).
    #[default]
    Auto,
    Dark,
    Light,
}

impl ThemeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Dark,
            Self::Dark => Self::Light,
            Self::Light => Self::Auto,
        }
    }

    /// `CONDUIT_THEME=dark|light|auto` (case-insensitive). Unknown → Auto.
    pub fn from_env() -> Self {
        match std::env::var("CONDUIT_THEME") {
            Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
                "dark" | "d" => Self::Dark,
                "light" | "l" => Self::Light,
                "auto" | "system" | "a" | "" => Self::Auto,
                _ => Self::Auto,
            },
            Err(_) => Self::Auto,
        }
    }

    pub fn resolve(self) -> Theme {
        match self {
            Self::Dark => Theme::dark(),
            Self::Light => Theme::light(),
            Self::Auto => {
                if system_prefers_dark() {
                    Theme::dark()
                } else {
                    Theme::light()
                }
            }
        }
    }

    /// Short status fragment, e.g. `auto→dark` or `light`.
    pub fn status_label(self, resolved: ThemeKind) -> String {
        match self {
            Self::Auto => format!("auto→{}", resolved.as_str()),
            other => other.as_str().to_string(),
        }
    }
}

/// Semantic colors used across the shell.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub kind: ThemeKind,
    pub background: Color,
    pub surface: Color,
    pub surface_alt: Color,
    pub border: Color,
    pub border_focus: Color,
    pub accent: Color,
    pub accent_dim: Color,
    pub fg: Color,
    pub muted: Color,
    pub subtle: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub title: Color,
    pub chart: [Color; 6],
}

impl Theme {
    /// Dark zinc + cyan accent (calm operator console).
    pub fn dark() -> Self {
        Self {
            kind: ThemeKind::Dark,
            background: Color::Rgb(15, 17, 21),
            surface: Color::Rgb(22, 25, 31),
            surface_alt: Color::Rgb(28, 32, 40),
            border: Color::Rgb(48, 54, 66),
            border_focus: Color::Rgb(56, 189, 248), // sky-400
            accent: Color::Rgb(56, 189, 248),
            accent_dim: Color::Rgb(14, 116, 144),
            fg: Color::Rgb(226, 232, 240),
            muted: Color::Rgb(148, 163, 184),
            subtle: Color::Rgb(100, 116, 139),
            success: Color::Rgb(52, 211, 153),
            warning: Color::Rgb(251, 191, 36),
            error: Color::Rgb(248, 113, 113),
            selection_bg: Color::Rgb(30, 58, 80),
            selection_fg: Color::Rgb(224, 242, 254),
            title: Color::Rgb(186, 230, 253),
            chart: [
                Color::Rgb(56, 189, 248),
                Color::Rgb(52, 211, 153),
                Color::Rgb(167, 139, 250),
                Color::Rgb(251, 191, 36),
                Color::Rgb(244, 114, 182),
                Color::Rgb(94, 234, 212),
            ],
        }
    }

    /// Light zinc + sky accent (readable on bright terminals / system light mode).
    pub fn light() -> Self {
        Self {
            kind: ThemeKind::Light,
            background: Color::Rgb(248, 250, 252), // slate-50
            surface: Color::Rgb(255, 255, 255),
            surface_alt: Color::Rgb(241, 245, 249), // slate-100
            border: Color::Rgb(203, 213, 225),      // slate-300
            border_focus: Color::Rgb(2, 132, 199),  // sky-600
            accent: Color::Rgb(2, 132, 199),
            accent_dim: Color::Rgb(125, 211, 252), // sky-300
            fg: Color::Rgb(15, 23, 42),            // slate-900
            muted: Color::Rgb(71, 85, 105),        // slate-600
            subtle: Color::Rgb(148, 163, 184),     // slate-400
            success: Color::Rgb(5, 150, 105),      // emerald-600
            warning: Color::Rgb(217, 119, 6),      // amber-600
            error: Color::Rgb(220, 38, 38),        // red-600
            selection_bg: Color::Rgb(224, 242, 254), // sky-100
            selection_fg: Color::Rgb(12, 74, 110),  // sky-900
            title: Color::Rgb(3, 105, 161),         // sky-700
            chart: [
                Color::Rgb(2, 132, 199),
                Color::Rgb(5, 150, 105),
                Color::Rgb(124, 58, 237),
                Color::Rgb(217, 119, 6),
                Color::Rgb(219, 39, 119),
                Color::Rgb(13, 148, 136),
            ],
        }
    }

    pub fn base(&self) -> Style {
        Style::default().fg(self.fg).bg(self.background)
    }

    pub fn surface(&self) -> Style {
        Style::default().fg(self.fg).bg(self.surface)
    }

    pub fn border(&self) -> Style {
        Style::default().fg(self.border)
    }

    pub fn border_active(&self) -> Style {
        Style::default().fg(self.border_focus)
    }

    pub fn accent_bold(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn title(&self) -> Style {
        Style::default()
            .fg(self.title)
            .add_modifier(Modifier::BOLD)
    }

    pub fn muted(&self) -> Style {
        Style::default().fg(self.muted)
    }

    pub fn subtle(&self) -> Style {
        Style::default().fg(self.subtle)
    }

    pub fn success(&self) -> Style {
        Style::default().fg(self.success)
    }

    pub fn warning(&self) -> Style {
        Style::default().fg(self.warning)
    }

    pub fn error(&self) -> Style {
        Style::default().fg(self.error)
    }

    pub fn selection(&self) -> Style {
        Style::default().fg(self.selection_fg).bg(self.selection_bg)
    }

    pub fn header_cell(&self) -> Style {
        Style::default()
            .fg(self.warning)
            .add_modifier(Modifier::BOLD)
    }

    pub fn key_hint(&self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge_ok(&self) -> Style {
        Style::default()
            .fg(Color::Black)
            .bg(self.success)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge_err(&self) -> Style {
        Style::default()
            .fg(Color::Black)
            .bg(self.error)
            .add_modifier(Modifier::BOLD)
    }

    pub fn badge_warn(&self) -> Style {
        Style::default()
            .fg(Color::Black)
            .bg(self.warning)
            .add_modifier(Modifier::BOLD)
    }

    pub fn chart_color(&self, i: usize) -> Color {
        self.chart[i % self.chart.len()]
    }
}

impl Default for Theme {
    fn default() -> Self {
        ThemeMode::from_env().resolve()
    }
}

// ── System appearance detection ─────────────────────────────────────────────

/// Best-effort: does the OS / terminal prefer a dark palette?
///
/// Order: macOS `AppleInterfaceStyle` → GNOME `color-scheme` → `COLORFGBG` →
/// default **dark** (operator-console bias when unknown).
pub fn system_prefers_dark() -> bool {
    #[cfg(target_os = "macos")]
    {
        if let Some(v) = prefer_dark_macos() {
            return v;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Some(v) = prefer_dark_linux() {
            return v;
        }
    }
    if let Some(v) = prefer_dark_from_colorfgbg(std::env::var("COLORFGBG").ok().as_deref()) {
        return v;
    }
    true
}

/// Parse `COLORFGBG` (`fg;bg`, 0–15). Low bg indices are usually dark.
///
/// Public for unit tests.
pub fn prefer_dark_from_colorfgbg(raw: Option<&str>) -> Option<bool> {
    let v = raw?.trim();
    if v.is_empty() {
        return None;
    }
    // Take last semicolon-separated field as background (handles "default;0").
    let bg_s = v.rsplit(';').next()?.trim();
    let bg: u8 = bg_s.parse().ok()?;
    // 0–6, 8 → dark; 7, 9–15 → light (common xterm palette convention).
    Some(bg <= 6 || bg == 8)
}

#[cfg(target_os = "macos")]
fn prefer_dark_macos() -> Option<bool> {
    // Light mode: key is missing → non-zero exit. Dark: prints "Dark".
    let out = Command::new("defaults")
        .args(["read", "-g", "AppleInterfaceStyle"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        Some(s.to_ascii_lowercase().contains("dark"))
    } else {
        // Missing key ⇒ Light appearance.
        Some(false)
    }
}

#[cfg(target_os = "linux")]
fn prefer_dark_linux() -> Option<bool> {
    // GNOME 42+: color-scheme is 'prefer-dark' | 'prefer-light' | 'default'
    if let Ok(out) = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "color-scheme"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
            if s.contains("prefer-dark") {
                return Some(true);
            }
            if s.contains("prefer-light") {
                return Some(false);
            }
        }
    }
    // Older: gtk-theme name often contains "-dark"
    if let Ok(out) = Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "gtk-theme"])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).to_ascii_lowercase();
            if s.contains("dark") {
                return Some(true);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorfgbg_dark_backgrounds() {
        assert_eq!(prefer_dark_from_colorfgbg(Some("15;0")), Some(true));
        assert_eq!(prefer_dark_from_colorfgbg(Some("7;8")), Some(true));
        assert_eq!(prefer_dark_from_colorfgbg(Some("0;6")), Some(true));
    }

    #[test]
    fn colorfgbg_light_backgrounds() {
        assert_eq!(prefer_dark_from_colorfgbg(Some("0;15")), Some(false));
        assert_eq!(prefer_dark_from_colorfgbg(Some("0;7")), Some(false));
        assert_eq!(prefer_dark_from_colorfgbg(Some("default;15")), Some(false));
    }

    #[test]
    fn colorfgbg_missing() {
        assert_eq!(prefer_dark_from_colorfgbg(None), None);
        assert_eq!(prefer_dark_from_colorfgbg(Some("")), None);
        assert_eq!(prefer_dark_from_colorfgbg(Some("nope")), None);
    }

    #[test]
    fn mode_cycle() {
        assert_eq!(ThemeMode::Auto.next(), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.next(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.next(), ThemeMode::Auto);
    }

    #[test]
    fn mode_resolve_forced() {
        assert_eq!(ThemeMode::Dark.resolve().kind, ThemeKind::Dark);
        assert_eq!(ThemeMode::Light.resolve().kind, ThemeKind::Light);
    }

    #[test]
    fn light_and_dark_differ() {
        let d = Theme::dark();
        let l = Theme::light();
        assert_ne!(d.background, l.background);
        assert_ne!(d.fg, l.fg);
        assert_eq!(d.kind, ThemeKind::Dark);
        assert_eq!(l.kind, ThemeKind::Light);
    }

    #[test]
    fn status_label() {
        assert_eq!(
            ThemeMode::Auto.status_label(ThemeKind::Light),
            "auto→light"
        );
        assert_eq!(ThemeMode::Dark.status_label(ThemeKind::Dark), "dark");
    }
}
