use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, Default)]
pub struct Composer {
    text: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft_before_history: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ComposerView {
    pub lines: Vec<String>,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub total_rows: usize,
}

impl Composer {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft_before_history = None;
    }

    pub fn insert_char(&mut self, ch: char) {
        let byte = char_to_byte(&self.text, self.cursor);
        self.text.insert(byte, ch);
        self.cursor += 1;
        self.history_index = None;
    }

    pub fn insert_str(&mut self, value: &str) {
        let byte = char_to_byte(&self.text, self.cursor);
        self.text.insert_str(byte, value);
        self.cursor += value.chars().count();
        self.history_index = None;
    }

    pub fn newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let end = char_to_byte(&self.text, self.cursor);
        let start = char_to_byte(&self.text, self.cursor - 1);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
        self.history_index = None;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.chars().count() {
            return;
        }
        let start = char_to_byte(&self.text, self.cursor);
        let end = char_to_byte(&self.text, self.cursor + 1);
        self.text.replace_range(start..end, "");
        self.history_index = None;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub fn move_word_left(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor > 0 && chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
        while self.cursor > 0 && !chars[self.cursor - 1].is_whitespace() {
            self.cursor -= 1;
        }
    }

    pub fn move_word_right(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor < chars.len() && !chars[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < chars.len() && chars[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor > 0 && chars[self.cursor - 1] != '\n' {
            self.cursor -= 1;
        }
    }

    pub fn move_end(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        while self.cursor < chars.len() && chars[self.cursor] != '\n' {
            self.cursor += 1;
        }
    }

    pub fn delete_previous_word(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let chars: Vec<char> = self.text.chars().collect();
        let end_cursor = self.cursor;
        let mut start_cursor = self.cursor;
        while start_cursor > 0 && chars[start_cursor - 1].is_whitespace() {
            start_cursor -= 1;
        }
        while start_cursor > 0 && !chars[start_cursor - 1].is_whitespace() {
            start_cursor -= 1;
        }
        let start = char_to_byte(&self.text, start_cursor);
        let end = char_to_byte(&self.text, end_cursor);
        self.text.replace_range(start..end, "");
        self.cursor = start_cursor;
        self.history_index = None;
    }

    pub fn kill_to_line_start(&mut self) {
        let end = char_to_byte(&self.text, self.cursor);
        let mut start_cursor = self.cursor;
        let chars: Vec<char> = self.text.chars().collect();
        while start_cursor > 0 && chars[start_cursor - 1] != '\n' {
            start_cursor -= 1;
        }
        let start = char_to_byte(&self.text, start_cursor);
        self.text.replace_range(start..end, "");
        self.cursor = start_cursor;
    }

    pub fn kill_to_line_end(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut end_cursor = self.cursor;
        while end_cursor < chars.len() && chars[end_cursor] != '\n' {
            end_cursor += 1;
        }
        let start = char_to_byte(&self.text, self.cursor);
        let end = char_to_byte(&self.text, end_cursor);
        self.text.replace_range(start..end, "");
    }

    pub fn take_for_submit(&mut self) -> Option<String> {
        if self.text.trim().is_empty() {
            return None;
        }
        let text = self.text.clone();
        if self.history.last() != Some(&text) {
            self.history.push(text.clone());
            if self.history.len() > 200 {
                self.history.remove(0);
            }
        }
        self.clear();
        Some(text)
    }

    pub fn history_previous(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_index {
            None => {
                self.draft_before_history = Some(self.text.clone());
                self.history.len() - 1
            }
            Some(index) => index.saturating_sub(1),
        };
        self.history_index = Some(next);
        self.text = self.history[next].clone();
        self.cursor = self.text.chars().count();
    }

    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            let next = index + 1;
            self.history_index = Some(next);
            self.text = self.history[next].clone();
        } else {
            self.history_index = None;
            self.text = self.draft_before_history.take().unwrap_or_default();
        }
        self.cursor = self.text.chars().count();
    }

    pub fn view(&self, width: u16, max_rows: u16) -> ComposerView {
        let width = width.max(1) as usize;
        let mut all_lines = vec![String::new()];
        let mut row = 0usize;
        let mut col = 0usize;
        let mut cursor_row = 0usize;
        let mut cursor_col = 0usize;

        for (index, ch) in self.text.chars().enumerate() {
            if index == self.cursor {
                cursor_row = row;
                cursor_col = col;
            }
            if ch == '\n' {
                all_lines.push(String::new());
                row += 1;
                col = 0;
                continue;
            }
            let char_width = ch.width().unwrap_or(1).max(1);
            if col > 0 && col + char_width > width {
                all_lines.push(String::new());
                row += 1;
                col = 0;
            }
            all_lines[row].push(ch);
            col += char_width;
        }
        if self.cursor == self.text.chars().count() {
            cursor_row = row;
            cursor_col = col;
        }

        let max_rows = max_rows.max(1) as usize;
        let start = cursor_row.saturating_add(1).saturating_sub(max_rows);
        let end = (start + max_rows).min(all_lines.len());
        let visible = all_lines[start..end].to_vec();

        ComposerView {
            lines: visible,
            cursor_row: cursor_row.saturating_sub(start) as u16,
            cursor_col: cursor_col.min(width.saturating_sub(1)) as u16,
            total_rows: all_lines.len(),
        }
    }
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_editing_stays_on_char_boundaries() {
        let mut composer = Composer::default();
        composer.insert_str("привет");
        composer.move_left();
        composer.backspace();
        assert_eq!(composer.text(), "привт");
    }

    #[test]
    fn wrapped_cursor_tracks_visible_window() {
        let mut composer = Composer::default();
        composer.insert_str("123456789");
        let view = composer.view(4, 2);
        assert_eq!(view.lines, vec!["5678", "9"]);
        assert_eq!((view.cursor_row, view.cursor_col), (1, 1));
    }

    #[test]
    fn submit_preserves_intentional_code_whitespace() {
        let mut composer = Composer::default();
        composer.insert_str("    fn main() {}\n");
        assert_eq!(
            composer.take_for_submit().as_deref(),
            Some("    fn main() {}\n")
        );
    }
}
