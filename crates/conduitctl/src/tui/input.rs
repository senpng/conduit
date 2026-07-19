//! Minimal single-line text field for forms (no external textarea crate).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Default)]
pub struct InputField {
    pub value: String,
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
