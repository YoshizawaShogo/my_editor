use std::{
    fs::File,
    io::BufWriter,
    path::Path,
};

use ropey::Rope;

use crate::error::Result;

pub struct EditableDocument {
    pub rope: Rope,
}

impl EditableDocument {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let rope = Rope::from_reader(file)?;
        Ok(Self { rope })
    }

    pub fn read_page(
        &self,
        start_row: usize,
        page_height: usize,
        page_width: usize,
    ) -> Result<EditablePage> {
        let mut rows = Vec::with_capacity(page_height);
        let mut skipped_rows = 0usize;

        for (line_index, line) in self.rope.lines().enumerate() {
            let text = line.to_string();
            let line = text.trim_end_matches('\n').trim_end_matches('\r');

            for piece in wrap_visible_segments(line, page_width) {
                if skipped_rows < start_row {
                    skipped_rows += 1;
                    continue;
                }

                rows.push(EditableRow {
                    line_number: line_index + 1,
                    text: piece,
                });
                if rows.len() >= page_height {
                    return Ok(EditablePage { rows });
                }
            }
        }

        if rows.is_empty() {
            rows.push(EditableRow {
                line_number: 1,
                text: String::new(),
            });
        }

        Ok(EditablePage { rows })
    }

    pub fn total_rows(&self, page_width: usize) -> usize {
        let mut total_rows = 0usize;

        for line in self.rope.lines() {
            let text = line.to_string();
            let line = text.trim_end_matches('\n').trim_end_matches('\r');
            total_rows = total_rows.saturating_add(wrap_visible_segments(line, page_width).len());
        }

        total_rows.max(1)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        self.rope.write_to(writer)?;
        Ok(())
    }

    pub fn insert_char(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
        ch: char,
    ) -> (usize, usize) {
        let insert_char_idx = self.char_index_for_display_position(display_row, display_column, page_width);
        self.rope.insert_char(insert_char_idx, ch);
        self.display_position_for_char_index(insert_char_idx.saturating_add(1), page_width)
    }

    pub fn insert_newline(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> (usize, usize) {
        let insert_char_idx = self.char_index_for_display_position(display_row, display_column, page_width);
        self.rope.insert_char(insert_char_idx, '\n');
        self.display_position_for_char_index(insert_char_idx.saturating_add(1), page_width)
    }

    pub fn backspace(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let cursor_char_idx = self.char_index_for_display_position(display_row, display_column, page_width);
        if cursor_char_idx == 0 {
            return None;
        }

        self.rope.remove((cursor_char_idx - 1)..cursor_char_idx);
        Some(self.display_position_for_char_index(cursor_char_idx - 1, page_width))
    }

    fn char_index_for_display_position(
        &self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> usize {
        let page_width = page_width.max(1);
        let mut current_row = 0usize;

        for line_index in 0..self.rope.len_lines() {
            let line_slice = self.rope.line(line_index);
            let line_text = line_slice.to_string();
            let line_char_len = trimmed_line_char_len(&line_text);
            let wrapped_rows = wrapped_row_count(line_char_len, page_width);

            if display_row < current_row.saturating_add(wrapped_rows) {
                let row_in_line = display_row - current_row;
                let column_in_line = row_in_line
                    .saturating_mul(page_width)
                    .saturating_add(display_column)
                    .min(line_char_len);
                return self.rope.line_to_char(line_index).saturating_add(column_in_line);
            }

            current_row = current_row.saturating_add(wrapped_rows);
        }

        self.rope.len_chars()
    }

    fn display_position_for_char_index(&self, char_index: usize, page_width: usize) -> (usize, usize) {
        let page_width = page_width.max(1);
        let mut current_row = 0usize;

        for line_index in 0..self.rope.len_lines() {
            let line_slice = self.rope.line(line_index);
            let line_start = self.rope.line_to_char(line_index);
            let line_text = line_slice.to_string();
            let line_char_len = trimmed_line_char_len(&line_text);
            let line_end = line_start.saturating_add(line_char_len);

            if char_index <= line_end {
                let offset_in_line = char_index.saturating_sub(line_start).min(line_char_len);
                return (
                    current_row.saturating_add(offset_in_line / page_width),
                    offset_in_line % page_width,
                );
            }

            current_row = current_row.saturating_add(wrapped_row_count(line_char_len, page_width));
        }

        (current_row.saturating_sub(1), 0)
    }
}

pub struct EditablePage {
    pub rows: Vec<EditableRow>,
}

pub struct EditableRow {
    pub line_number: usize,
    pub text: String,
}

fn wrap_visible_segments(line: &str, page_width: usize) -> Vec<String> {
    if page_width == 0 {
        return Vec::new();
    }

    if line.is_empty() {
        return vec![String::new()];
    }

    let chars: Vec<char> = line.chars().collect();
    chars
        .chunks(page_width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn trimmed_line_char_len(line: &str) -> usize {
    line.trim_end_matches('\n')
        .trim_end_matches('\r')
        .chars()
        .count()
}

fn wrapped_row_count(line_char_len: usize, page_width: usize) -> usize {
    if page_width == 0 {
        return 1;
    }

    if line_char_len == 0 {
        1
    } else {
        line_char_len.div_ceil(page_width)
    }
}
