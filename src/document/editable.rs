use std::{
    collections::HashMap,
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
    process::Command,
};

use ropey::Rope;

use crate::error::Result;

pub struct EditableDocument {
    pub path: PathBuf,
    pub rope: Rope,
    pub indent_width: usize,
    pub use_hard_tabs: bool,
    pub git_gutter_markers: HashMap<usize, char>,
}

impl EditableDocument {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let rope = Rope::from_reader(file)?;
        Ok(Self {
            path: path.to_path_buf(),
            indent_width: detect_indent_width(&rope),
            use_hard_tabs: uses_hard_tabs(path),
            git_gutter_markers: load_git_gutter_markers(path),
            rope,
        })
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

    pub fn indent_width(&self) -> usize {
        self.indent_width.max(1)
    }

    pub fn save(&mut self, path: &Path) -> Result<()> {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        self.rope.write_to(writer)?;
        self.reload_git_gutter_markers();
        Ok(())
    }

    pub fn reload_git_gutter_markers(&mut self) {
        self.git_gutter_markers = load_git_gutter_markers(&self.path);
    }

    pub fn git_gutter_marker(&self, line_number: usize) -> Option<char> {
        self.git_gutter_markers.get(&line_number).copied()
    }

    pub fn next_git_marker_row(&self, current_row: usize, page_width: usize) -> Option<usize> {
        let current_line_number = self.line_number_for_display_row(current_row, page_width);
        self.git_hunk_start_lines()
            .into_iter()
            .find(|line_number| *line_number > current_line_number)
            .map(|line_number| self.display_row_for_line_number(line_number, page_width))
    }

    pub fn previous_git_marker_row(&self, current_row: usize, page_width: usize) -> Option<usize> {
        let current_line_number = self.line_number_for_display_row(current_row, page_width);
        self.git_hunk_start_lines()
            .into_iter()
            .rev()
            .find(|line_number| *line_number < current_line_number)
            .map(|line_number| self.display_row_for_line_number(line_number, page_width))
    }

    pub fn display_line_width(&self, display_row: usize, page_width: usize) -> usize {
        let page_width = page_width.max(1);
        let mut current_row = 0usize;

        for line in self.rope.lines() {
            let line_text = line.to_string();
            let trimmed = line_text.trim_end_matches('\n').trim_end_matches('\r');
            let wrapped = wrap_visible_segments(trimmed, page_width);

            if display_row < current_row.saturating_add(wrapped.len()) {
                let row_in_line = display_row - current_row;
                return wrapped[row_in_line].chars().count();
            }

            current_row = current_row.saturating_add(wrapped.len());
        }

        0
    }

    pub fn jump_row_for_line_number(&self, line_number: usize, page_width: usize) -> Option<usize> {
        if line_number == 0 || line_number > self.rope.len_lines() {
            return None;
        }

        Some(self.display_row_for_line_number(line_number, page_width.max(1)))
    }

    pub fn display_line_text(&self, display_row: usize, page_width: usize) -> String {
        let page_width = page_width.max(1);
        let mut current_row = 0usize;

        for line in self.rope.lines() {
            let line_text = line.to_string();
            let trimmed = line_text.trim_end_matches('\n').trim_end_matches('\r');
            let wrapped = wrap_visible_segments(trimmed, page_width);

            if display_row < current_row.saturating_add(wrapped.len()) {
                return wrapped[display_row - current_row].clone();
            }

            current_row = current_row.saturating_add(wrapped.len());
        }

        String::new()
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

    pub fn delete_forward(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let cursor_char_idx = self.char_index_for_display_position(display_row, display_column, page_width);
        if cursor_char_idx >= self.rope.len_chars() {
            return None;
        }

        self.rope.remove(cursor_char_idx..cursor_char_idx.saturating_add(1));
        Some(self.display_position_for_char_index(cursor_char_idx, page_width))
    }

    pub fn remove_display_range(
        &mut self,
        display_row: usize,
        start_column: usize,
        end_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let start_char_idx =
            self.char_index_for_display_position(display_row, start_column, page_width);
        let end_char_idx =
            self.char_index_for_display_position(display_row, end_column, page_width);

        if end_char_idx <= start_char_idx {
            return None;
        }

        self.rope.remove(start_char_idx..end_char_idx);
        Some(self.display_position_for_char_index(start_char_idx, page_width))
    }

    pub fn insert_tab(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> (usize, usize) {
        if self.use_hard_tabs {
            return self.insert_char(display_row, display_column, page_width, '\t');
        }

        let insert_char_idx =
            self.char_index_for_display_position(display_row, display_column, page_width);
        let spaces = " ".repeat(self.indent_width.max(1));
        self.rope.insert(insert_char_idx, &spaces);
        self.display_position_for_char_index(
            insert_char_idx.saturating_add(self.indent_width.max(1)),
            page_width,
        )
    }

    pub fn open_below(&mut self, display_row: usize, page_width: usize) -> (usize, usize) {
        let page_width = page_width.max(1);
        let line_number = self.line_number_for_display_row(display_row, page_width);
        let line_index = line_number.saturating_sub(1);
        let line_start = self.rope.line_to_char(line_index);
        let line_text = self.rope.line(line_index).to_string();
        let insert_char_idx = line_start.saturating_add(trimmed_line_char_len(&line_text));

        self.rope.insert_char(insert_char_idx, '\n');
        self.display_position_for_char_index(insert_char_idx.saturating_add(1), page_width)
    }

    pub fn current_line_text(&self, display_row: usize, page_width: usize) -> String {
        let line_number = self.line_number_for_display_row(display_row, page_width.max(1));
        let line_index = line_number.saturating_sub(1);
        self.rope
            .line(line_index)
            .to_string()
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_owned()
    }

    pub fn clear_current_line(
        &mut self,
        display_row: usize,
        page_width: usize,
    ) -> Option<(String, (usize, usize))> {
        let page_width = page_width.max(1);
        let line_number = self.line_number_for_display_row(display_row, page_width);
        let line_index = line_number.saturating_sub(1);
        let line_start = self.rope.line_to_char(line_index);
        let line_text = self.rope.line(line_index).to_string();
        let trimmed_len = trimmed_line_char_len(&line_text);
        let removed = line_text
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_owned();
        let indent_len = removed.chars().take_while(|ch| ch.is_whitespace()).count();

        if trimmed_len > 0 {
            self.rope.remove(
                line_start.saturating_add(indent_len)..line_start.saturating_add(trimmed_len),
            );
        }

        Some((
            removed,
            self.display_position_for_char_index(
                line_start.saturating_add(indent_len),
                page_width,
            ),
        ))
    }

    pub fn delete_current_line(
        &mut self,
        display_row: usize,
        page_width: usize,
    ) -> Option<(String, (usize, usize))> {
        let page_width = page_width.max(1);
        let line_number = self.line_number_for_display_row(display_row, page_width);
        let line_index = line_number.saturating_sub(1);
        let line_start = self.rope.line_to_char(line_index);
        let line_text = self.rope.line(line_index).to_string();
        let had_newline = line_text.ends_with('\n');
        let removed = line_text
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_owned();
        let line_char_len = line_text.chars().count();

        if self.rope.len_lines() <= 1 {
            self.rope = Rope::from_str("");
            return Some((removed, (0, 0)));
        }

        let removal_end = if had_newline {
            line_start.saturating_add(line_char_len)
        } else {
            line_start.saturating_add(trimmed_line_char_len(&line_text))
        };
        self.rope.remove(line_start..removal_end);

        let target_line_index = line_index.min(self.rope.len_lines().saturating_sub(1));
        let target_char_index = self.rope.line_to_char(target_line_index);
        Some((
            removed,
            self.display_position_for_char_index(target_char_index, page_width),
        ))
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

    fn line_number_for_display_row(&self, display_row: usize, page_width: usize) -> usize {
        let page_width = page_width.max(1);
        let mut current_row = 0usize;

        for line_index in 0..self.rope.len_lines() {
            let line_text = self.rope.line(line_index).to_string();
            let line_char_len = trimmed_line_char_len(&line_text);
            let wrapped_rows = wrapped_row_count(line_char_len, page_width);

            if display_row < current_row.saturating_add(wrapped_rows) {
                return line_index + 1;
            }

            current_row = current_row.saturating_add(wrapped_rows);
        }

        self.rope.len_lines().max(1)
    }

    fn display_row_for_line_number(&self, target_line_number: usize, page_width: usize) -> usize {
        let page_width = page_width.max(1);
        let mut current_row = 0usize;

        for line_index in 0..self.rope.len_lines() {
            if line_index + 1 >= target_line_number {
                return current_row;
            }

            let line_text = self.rope.line(line_index).to_string();
            let line_char_len = trimmed_line_char_len(&line_text);
            current_row = current_row.saturating_add(wrapped_row_count(line_char_len, page_width));
        }

        current_row
    }

    fn git_hunk_start_lines(&self) -> Vec<usize> {
        let mut line_numbers: Vec<usize> = self.git_gutter_markers.keys().copied().collect();
        line_numbers.sort_unstable();

        let mut hunk_starts = Vec::new();
        let mut previous_line_number: Option<usize> = None;

        for line_number in line_numbers {
            if match previous_line_number {
                Some(previous) => line_number > previous.saturating_add(1),
                None => true,
            } {
                hunk_starts.push(line_number);
            }
            previous_line_number = Some(line_number);
        }

        hunk_starts
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

fn detect_indent_width(rope: &Rope) -> usize {
    let mut indent_widths = Vec::new();

    for line in rope.lines() {
        let text = line.to_string();
        let trimmed = text.trim_end_matches('\n').trim_end_matches('\r');
        if trimmed.trim().is_empty() {
            continue;
        }

        let indent_width = trimmed.chars().take_while(|ch| ch.is_whitespace()).count();
        if indent_width > 0 {
            indent_widths.push(indent_width);
        }

        if indent_widths.len() >= 128 {
            break;
        }
    }

    if indent_widths.is_empty() {
        return 4;
    }

    for candidate in [4usize, 2, 8] {
        if indent_widths.iter().any(|width| *width == candidate)
            && indent_widths.iter().all(|width| width % candidate == 0)
        {
            return candidate;
        }
    }

    let mut common_divisor = indent_widths[0];
    for indent_width in indent_widths.into_iter().skip(1) {
        common_divisor = gcd(common_divisor, indent_width);
    }

    common_divisor.max(1)
}

fn gcd(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }

    left
}

fn uses_hard_tabs(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "Makefile")
}

fn load_git_gutter_markers(path: &Path) -> HashMap<usize, char> {
    let output = match Command::new("git")
        .args(["diff", "--unified=0", "--no-color", "--"])
        .arg(path)
        .output()
    {
        Ok(output) => output,
        Err(_) => return HashMap::new(),
    };

    if !output.status.success() && output.status.code() != Some(1) {
        return HashMap::new();
    }

    let diff = String::from_utf8_lossy(&output.stdout);
    let mut markers = HashMap::new();

    for line in diff.lines() {
        let Some(hunk) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some((range_part, _)) = hunk.split_once(" @@") else {
            continue;
        };
        let ranges: Vec<&str> = range_part.split_whitespace().collect();
        if ranges.len() < 2 {
            continue;
        }

        let Some((old_start, old_count)) = parse_diff_range(ranges[0].trim_start_matches('-')) else {
            continue;
        };
        let Some((new_start, new_count)) = parse_diff_range(ranges[1].trim_start_matches('+')) else {
            continue;
        };

        if old_count == 0 && new_count > 0 {
            for line_number in new_start..new_start.saturating_add(new_count) {
                markers.insert(line_number, 'A');
            }
        } else if old_count > 0 && new_count == 0 {
            markers.insert(new_start.max(1), 'D');
        } else {
            for line_number in new_start..new_start.saturating_add(new_count.max(1)) {
                markers.insert(line_number, 'M');
            }
            if new_count == 0 {
                markers.insert(old_start, 'M');
            }
        }
    }

    markers
}

fn parse_diff_range(range: &str) -> Option<(usize, usize)> {
    let (start, count) = match range.split_once(',') {
        Some((start, count)) => (start, count),
        None => (range, "1"),
    };

    Some((start.parse().ok()?, count.parse().ok()?))
}
