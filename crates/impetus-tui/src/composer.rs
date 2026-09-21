use unicode_width::UnicodeWidthChar;

/// Visual/edit layout for the prompt box.
///
/// - [`SingleLine`]: Enter submits; newline keybinds do not insert `\n`.
/// - [`MultiLine`]: Enter submits; Shift/Alt+Enter and Ctrl+J insert newline;
///   a trailing `\` on the current line continues (strip `\` + newline).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ComposerLayoutMode {
    #[default]
    SingleLine,
    MultiLine,
}

impl ComposerLayoutMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::SingleLine => "single",
            Self::MultiLine => "multi",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::SingleLine => Self::MultiLine,
            Self::MultiLine => Self::SingleLine,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Composer {
    text: String,
    cursor: usize,
    layout_mode: ComposerLayoutMode,
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

    pub fn layout_mode(&self) -> ComposerLayoutMode {
        self.layout_mode
    }

    pub fn toggle_layout_mode(&mut self) {
        self.layout_mode = self.layout_mode.toggle();
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
        if self.layout_mode == ComposerLayoutMode::SingleLine {
            return;
        }
        self.insert_char('\n');
    }

    /// If current line ends with `\`, strip it and insert a newline (multiline only).
    /// Returns true when continuation was applied (caller must not submit).
    pub fn try_backslash_continuation(&mut self) -> bool {
        if self.layout_mode != ComposerLayoutMode::MultiLine {
            return false;
        }
        let chars: Vec<char> = self.text.chars().collect();
        let mut end = self.cursor;
        while end < chars.len() && chars[end] != '\n' {
            end += 1;
        }
        if end == 0 || chars[end - 1] != '\\' {
            return false;
        }
        let start = char_to_byte(&self.text, end - 1);
        let stop = char_to_byte(&self.text, end);
        self.text.replace_range(start..stop, "");
        self.cursor = end - 1;
        self.insert_char('\n');
        true
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

    #[test]
    fn single_line_mode_rejects_newline_key() {
        let mut composer = Composer::default();
        assert_eq!(composer.layout_mode(), ComposerLayoutMode::SingleLine);
        composer.insert_str("hello");
        composer.newline();
        assert_eq!(composer.text(), "hello");
        assert!(!composer.text().contains('\n'));
    }

    #[test]
    fn multiline_mode_inserts_newline() {
        let mut composer = Composer::default();
        composer.toggle_layout_mode();
        composer.insert_str("hello");
        composer.newline();
        composer.insert_str("world");
        assert_eq!(composer.text(), "hello\nworld");
    }

    #[test]
    fn toggle_layout_mode_round_trips() {
        let mut composer = Composer::default();
        composer.toggle_layout_mode();
        assert_eq!(composer.layout_mode(), ComposerLayoutMode::MultiLine);
        composer.toggle_layout_mode();
        assert_eq!(composer.layout_mode(), ComposerLayoutMode::SingleLine);
    }

    #[test]
    fn backslash_continuation_only_in_multiline() {
        let mut single = Composer::default();
        single.insert_str("line\\");
        assert!(!single.try_backslash_continuation());
        assert_eq!(single.text(), "line\\");

        let mut multi = Composer::default();
        multi.toggle_layout_mode();
        multi.insert_str("line\\");
        assert!(multi.try_backslash_continuation());
        assert_eq!(multi.text(), "line\n");
    }

    #[test]
    fn backslash_continuation_works_from_mid_line() {
        let mut composer = Composer::default();
        composer.toggle_layout_mode();
        composer.insert_str("ab\\");
        composer.move_left();
        composer.move_left();
        assert_eq!(composer.cursor, 1);
        assert!(composer.try_backslash_continuation());
        assert_eq!(composer.text(), "ab\n");
    }
}
