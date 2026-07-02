use ropey::Rope;
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CharIdx(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DisplayPos {
    pub line: usize,
    pub col: usize,
}

pub fn char_idx_to_line_col(text: &Rope, index: CharIdx) -> (usize, usize) {
    let index = index.0.min(text.len_chars());
    let line = text.char_to_line(index);
    (line, index - text.line_to_char(line))
}

pub fn line_col_to_char_idx(text: &Rope, line: usize, col: usize) -> CharIdx {
    let line = line.min(text.len_lines().saturating_sub(1));
    let start = text.line_to_char(line);
    let line_len = line_content_len(text, line);
    CharIdx(start + col.min(line_len))
}

pub fn char_idx_to_display_pos(text: &Rope, index: CharIdx, tab_size: usize) -> DisplayPos {
    let (line, char_col) = char_idx_to_line_col(text, index);
    let col = text
        .line(line)
        .chars()
        .take(char_col)
        .fold(0, |col, character| {
            display_col_after(col, character, tab_size)
        });
    DisplayPos { line, col }
}

pub fn display_col_to_char_idx(
    text: &Rope,
    line: usize,
    display_col: usize,
    tab_size: usize,
) -> CharIdx {
    let line = line.min(text.len_lines().saturating_sub(1));
    let start = text.line_to_char(line);
    let mut current_col = 0;
    let mut char_col = 0;
    for character in text.line(line).chars() {
        if matches!(character, '\r' | '\n') {
            break;
        }
        let next_col = display_col_after(current_col, character, tab_size);
        if display_col < next_col {
            return CharIdx(start + char_col);
        }
        current_col = next_col;
        char_col += 1;
    }
    CharIdx(start + char_col)
}

pub fn lsp_position_to_char_idx(text: &Rope, line: usize, utf16_col: usize) -> CharIdx {
    let line = line.min(text.len_lines().saturating_sub(1));
    let start = text.line_to_char(line);
    let mut units = 0;
    let mut chars = 0;
    for character in text.line(line).chars() {
        if matches!(character, '\r' | '\n') {
            break;
        }
        let width = character.len_utf16();
        if units + width > utf16_col {
            break;
        }
        units += width;
        chars += 1;
    }
    CharIdx(start + chars)
}

pub fn line_content_len(text: &Rope, line: usize) -> usize {
    let slice = text.line(line.min(text.len_lines().saturating_sub(1)));
    let mut len = slice.len_chars();
    if len > 0 && slice.char(len - 1) == '\n' {
        len -= 1;
        if len > 0 && slice.char(len - 1) == '\r' {
            len -= 1;
        }
    }
    len
}

pub fn display_col_after(col: usize, character: char, tab_size: usize) -> usize {
    if character == '\t' {
        let tab_size = tab_size.max(1);
        col + tab_size - (col % tab_size)
    } else {
        col + character.width().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_char_indices_across_lines() {
        let text = Rope::from_str("abc\n日本語\n");

        assert_eq!(char_idx_to_line_col(&text, CharIdx(5)), (1, 1));
        assert_eq!(line_col_to_char_idx(&text, 1, 2), CharIdx(6));
        assert_eq!(line_col_to_char_idx(&text, 1, 99), CharIdx(7));
    }

    #[test]
    fn display_position_accounts_for_tabs_and_wide_characters() {
        let text = Rope::from_str("a\t日");

        assert_eq!(
            char_idx_to_display_pos(&text, CharIdx(3), 4),
            DisplayPos { line: 0, col: 6 }
        );
    }

    #[test]
    fn display_column_hit_test_accounts_for_wide_characters() {
        let text = Rope::from_str("a日b");

        assert_eq!(display_col_to_char_idx(&text, 0, 1, 4), CharIdx(1));
        assert_eq!(display_col_to_char_idx(&text, 0, 2, 4), CharIdx(1));
        assert_eq!(display_col_to_char_idx(&text, 0, 3, 4), CharIdx(2));
    }

    #[test]
    fn lsp_position_uses_utf16_units() {
        let text = Rope::from_str("a😀b");
        assert_eq!(lsp_position_to_char_idx(&text, 0, 1), CharIdx(1));
        assert_eq!(lsp_position_to_char_idx(&text, 0, 3), CharIdx(2));
    }
}
