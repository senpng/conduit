//! Visual theme for the operator TUI (tokscale / k9s-inspired dark console).

use ratatui::style::{Color, Modifier, Style};

/// Semantic colors used across the shell.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
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
    /// Default dark zinc + cyan accent (calm operator console).
    pub fn dark() -> Self {
        Self {
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
        Self::dark()
    }
}
