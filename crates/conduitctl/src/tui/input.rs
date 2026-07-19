//! Minimal single-line text field for forms (no external textarea crate).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone, Default)]
pub struct InputField {
    pub value: String,
    /// Cursor position in **characters** (not bytes), 0..=value.chars().count().
    pub cursor: usize,
    pub password: bool,
}

impl InputField {
    pub fn new(initial: impl Into<String>) -> Self {
        let value = initial.into();
        let cursor = value.chars().count();
        Self {
            value,
            cursor,
            password: false,
        }
    }

    pub fn password(mut self) -> Self {
        self.password = true;
        self
    }

    pub fn display(&self) -> String {
        if self.password {
            "•".repeat(self.value.chars().count())
        } else {
            self.value.clone()
        }
    }

    /// Render the field as styled spans with an in-string caret when focused.
    ///
    /// - Mid-text: the character under the cursor uses `caret` (full cell bg).
    /// - End of text / empty: a full-width block is drawn via a space with
    ///   `caret` style — **not** a half-block glyph (`▌`), which many terminal
    ///   fonts clip to half a cell and looks "truncated".
    /// - Unfocused: plain text with `base` style.
    pub fn line_spans(&self, base: Style, caret: Style, show_caret: bool) -> Line<'static> {
        let display = self.display();
        if !show_caret {
            return Line::from(Span::styled(display, base));
        }
        let chars: Vec<char> = display.chars().collect();
        let cur = self.cursor.min(chars.len());
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
        if cur > 0 {
            let before: String = chars[..cur].iter().collect();
            spans.push(Span::styled(before, base));
        }
        if cur < chars.len() {
            // Invert the cell under the cursor (full cell, no half-block glyphs).
            spans.push(Span::styled(chars[cur].to_string(), caret));
            if cur + 1 < chars.len() {
                let after: String = chars[cur + 1..].iter().collect();
                spans.push(Span::styled(after, base));
            }
        } else {
            // End-of-line / empty-field caret: one full cell of caret bg.
            spans.push(Span::styled(" ".to_string(), caret));
        }
        Line::from(spans)
    }

    /// Solid block caret (fg on accent) — more reliable than `REVERSED`+`▌`,
    /// which half-fills the cell in many fonts and looks cut off.
    pub fn caret_style(accent: ratatui::style::Color, on_accent: ratatui::style::Color) -> Style {
        Style::default()
            .fg(on_accent)
            .bg(accent)
            .add_modifier(Modifier::BOLD)
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let idx = self.byte_index();
                self.value.insert(idx, c);
                self.cursor += 1;
                true
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let idx = self.byte_index_before();
                    self.value.remove(idx);
                    self.cursor -= 1;
                }
                true
            }
            KeyCode::Delete => {
                if self.cursor < self.value.chars().count() {
                    let idx = self.byte_index();
                    self.value.remove(idx);
                }
                true
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                true
            }
            KeyCode::Right => {
                if self.cursor < self.value.chars().count() {
                    self.cursor += 1;
                }
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.value.chars().count();
                true
            }
            _ => false,
        }
    }

    fn byte_index(&self) -> usize {
        self.value
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }

    fn byte_index_before(&self) -> usize {
        self.value
            .char_indices()
            .nth(self.cursor.saturating_sub(1))
            .map(|(i, _)| i)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    #[test]
    fn caret_at_end_on_empty() {
        let f = InputField::new("");
        let line = f.line_spans(Style::default(), Style::default(), true);
        assert_eq!(line.spans.len(), 1);
        // Full-cell space, not a half-block glyph.
        assert_eq!(line.spans[0].content.as_ref(), " ");
    }

    #[test]
    fn caret_at_end_after_text() {
        let f = InputField::new("false");
        assert_eq!(f.cursor, 5);
        let line = f.line_spans(Style::default(), Style::default(), true);
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content.as_ref(), "false");
        assert_eq!(line.spans[1].content.as_ref(), " ");
    }

    #[test]
    fn caret_in_middle_inverts_char() {
        let mut f = InputField::new("abc");
        f.cursor = 1; // under 'b'
        let line = f.line_spans(Style::default(), Style::default(), true);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content.as_ref(), "a");
        assert_eq!(line.spans[1].content.as_ref(), "b");
        assert_eq!(line.spans[2].content.as_ref(), "c");
    }

    #[test]
    fn no_caret_when_unfocused() {
        let f = InputField::new("hi");
        let line = f.line_spans(Style::default(), Style::default(), false);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content.as_ref(), "hi");
    }
}
