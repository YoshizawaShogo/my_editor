use std::{
    collections::HashMap,
    fs::File,
    io::BufWriter,
    path::{Path, PathBuf},
    process::Command,
};

use lsp_types::{Position, TextEdit};
use ropey::Rope;

use crate::{
    document::DiagnosticEntry,
    error::Result,
};

pub struct EditableDocument {
    pub path: PathBuf,
    pub rope: Rope,
    pub indent_width: usize,
    pub use_hard_tabs: bool,
    pub git_gutter_markers: HashMap<usize, char>,
    pub diagnostics: HashMap<usize, Vec<DiagnosticEntry>>,
    pub semantic_tokens: HashMap<usize, Vec<SyntaxTokenSpan>>,
    pub undo_stack: Vec<Rope>,
    pub redo_stack: Vec<Rope>,
    pub undo_group_active: bool,
    pub undo_group_snapshot_taken: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Default)]
pub struct DiagnosticSummary {
    pub warnings: usize,
    pub errors: usize,
}

#[derive(Clone, Copy)]
pub enum SyntaxHighlightKind {
    Keyword,
    String,
    Comment,
    Type,
    Function,
    Variable,
    Parameter,
    Number,
    Operator,
    Macro,
    Namespace,
    Property,
}

#[derive(Clone)]
pub struct SyntaxTokenSpan {
    pub start: usize,
    pub length: usize,
    pub kind: SyntaxHighlightKind,
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
            diagnostics: HashMap::new(),
            semantic_tokens: HashMap::new(),
            rope,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            undo_group_active: false,
            undo_group_snapshot_taken: false,
        })
    }

    pub fn begin_undo_group(&mut self) {
        self.undo_group_active = true;
        self.undo_group_snapshot_taken = false;
    }

    pub fn end_undo_group(&mut self) {
        self.undo_group_active = false;
        self.undo_group_snapshot_taken = false;
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
            let wrapped = wrap_visible_segments(line, page_width);
            let line_tokens = self
                .semantic_tokens
                .get(&(line_index + 1))
                .cloned()
                .unwrap_or_default();

            for (row_in_line, piece) in wrapped.into_iter().enumerate() {
                if skipped_rows < start_row {
                    skipped_rows += 1;
                    continue;
                }

                let piece_start = row_in_line.saturating_mul(page_width);
                let piece_end = piece_start.saturating_add(piece.chars().count());

                rows.push(EditableRow {
                    line_number: line_index + 1,
                    text: piece,
                    syntax_spans: line_tokens
                        .iter()
                        .filter_map(|token| {
                            let token_start = token.start;
                            let token_end = token.start.saturating_add(token.length);
                            let overlap_start = token_start.max(piece_start);
                            let overlap_end = token_end.min(piece_end);
                            (overlap_start < overlap_end).then(|| SyntaxTokenSpan {
                                start: overlap_start.saturating_sub(piece_start),
                                length: overlap_end.saturating_sub(overlap_start),
                                kind: token.kind,
                            })
                        })
                        .collect(),
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
                syntax_spans: Vec::new(),
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

    pub fn diagnostic_marker(&self, line_number: usize) -> Option<DiagnosticSeverity> {
        let entries = self.diagnostics.get(&line_number)?;
        if entries
            .iter()
            .any(|entry| entry.severity == DiagnosticSeverity::Error)
        {
            Some(DiagnosticSeverity::Error)
        } else if entries
            .iter()
            .any(|entry| entry.severity == DiagnosticSeverity::Warning)
        {
            Some(DiagnosticSeverity::Warning)
        } else {
            None
        }
    }

    pub fn next_diagnostic_row(
        &self,
        current_row: usize,
        page_width: usize,
        error_only: bool,
    ) -> Option<usize> {
        let current_line_number = self.line_number_for_display_row(current_row, page_width);
        let mut line_numbers: Vec<usize> = self
            .diagnostics
            .iter()
            .filter(|(_, entries)| {
                !error_only
                    || entries
                        .iter()
                        .any(|entry| entry.severity == DiagnosticSeverity::Error)
            })
            .map(|(line_number, _entries)| *line_number)
            .collect();
        line_numbers.sort_unstable();
        line_numbers
            .into_iter()
            .find(|line_number| *line_number > current_line_number)
            .map(|line_number| self.display_row_for_line_number(line_number, page_width))
    }

    pub fn previous_diagnostic_row(
        &self,
        current_row: usize,
        page_width: usize,
        error_only: bool,
    ) -> Option<usize> {
        let current_line_number = self.line_number_for_display_row(current_row, page_width);
        let mut line_numbers: Vec<usize> = self
            .diagnostics
            .iter()
            .filter(|(_, entries)| {
                !error_only
                    || entries
                        .iter()
                        .any(|entry| entry.severity == DiagnosticSeverity::Error)
            })
            .map(|(line_number, _entries)| *line_number)
            .collect();
        line_numbers.sort_unstable();
        line_numbers
            .into_iter()
            .rev()
            .find(|line_number| *line_number < current_line_number)
            .map(|line_number| self.display_row_for_line_number(line_number, page_width))
    }

    pub fn set_diagnostics(&mut self, diagnostics: HashMap<usize, Vec<DiagnosticEntry>>) {
        self.diagnostics = diagnostics;
    }

    pub fn set_semantic_tokens(&mut self, semantic_tokens: HashMap<usize, Vec<SyntaxTokenSpan>>) {
        self.semantic_tokens = semantic_tokens;
    }

    pub fn diagnostic_summary(&self) -> DiagnosticSummary {
        let mut summary = DiagnosticSummary::default();
        for entries in self.diagnostics.values() {
            for entry in entries {
                match entry.severity {
                    DiagnosticSeverity::Warning => summary.warnings += 1,
                    DiagnosticSeverity::Error => summary.errors += 1,
                }
            }
        }
        summary
    }

    pub fn diagnostics_for_display_row(
        &self,
        display_row: usize,
        page_width: usize,
    ) -> Vec<DiagnosticEntry> {
        let line_number = self.line_number_for_display_row(display_row, page_width.max(1));
        self.diagnostics.get(&line_number).cloned().unwrap_or_default()
    }

    pub fn collect_diagnostics(&self) -> Vec<(usize, Vec<DiagnosticEntry>)> {
        let mut diagnostics = self
            .diagnostics
            .iter()
            .map(|(line_number, entries)| (*line_number, entries.clone()))
            .collect::<Vec<_>>();
        diagnostics.sort_by_key(|(line_number, _)| *line_number);
        diagnostics
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

    pub fn first_match_row(&self, query: &str, page_width: usize) -> Option<usize> {
        self.first_match_position(query, page_width).map(|(row, _)| row)
    }

    pub fn first_match_position(&self, query: &str, page_width: usize) -> Option<(usize, usize)> {
        if query.is_empty() {
            return None;
        }

        for (line_index, line) in self.rope.lines().enumerate() {
            let text = line.to_string();
            let trimmed = text.trim_end_matches('\n').trim_end_matches('\r');
            if let Some(column) = trimmed.find(query) {
                return Some((
                    self.display_row_for_line_number(line_index + 1, page_width.max(1)),
                    column,
                ));
            }
        }

        None
    }

    pub fn next_match_position(
        &self,
        query: &str,
        start_row: usize,
        start_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        if query.is_empty() {
            return None;
        }

        let total_rows = self.total_rows(page_width.max(1));
        for row in start_row..total_rows {
            let line_text = self.display_line_text(row, page_width.max(1));
            let search_start = if row == start_row {
                start_column.min(line_text.len())
            } else {
                0
            };
            if let Some(offset) = line_text[search_start..].find(query) {
                return Some((row, search_start + offset));
            }
        }

        None
    }

    pub fn previous_match_position(
        &self,
        query: &str,
        start_row: usize,
        start_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        if query.is_empty() {
            return None;
        }

        for row in (0..=start_row).rev() {
            let line_text = self.display_line_text(row, page_width.max(1));
            let search_end = if row == start_row {
                start_column.min(line_text.len())
            } else {
                line_text.len()
            };
            let haystack = &line_text[..search_end];
            if let Some(column) = haystack.rfind(query) {
                return Some((row, column));
            }
        }

        None
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

    pub fn full_text(&self) -> String {
        self.rope.to_string()
    }

    pub fn replace_all(&mut self, find: &str, replace: &str) -> usize {
        if find.is_empty() {
            return 0;
        }

        let text = self.rope.to_string();
        let count = text.match_indices(find).count();
        if count == 0 {
            return 0;
        }

        self.push_undo_snapshot();
        self.rope = Rope::from_str(&text.replace(find, replace));
        self.reload_git_gutter_markers();
        count
    }

    pub fn lsp_position_for_display_position(
        &self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Position {
        let char_index =
            self.char_index_for_display_position(display_row, display_column, page_width.max(1));
        let line_index = self.rope.char_to_line(char_index);
        let line_start = self.rope.line_to_char(line_index);
        let character = char_index.saturating_sub(line_start) as u32;
        Position::new(line_index as u32, character)
    }

    pub fn display_position_for_lsp_position(
        &self,
        line: u32,
        character: u32,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let line_index = usize::try_from(line).ok()?;
        if line_index >= self.rope.len_lines() {
            return None;
        }

        let line_start = self.rope.line_to_char(line_index);
        let line_text = self.rope.line(line_index).to_string();
        let trimmed_len = trimmed_line_char_len(&line_text);
        let char_index = line_start.saturating_add((character as usize).min(trimmed_len));
        Some(self.display_position_for_char_index(char_index, page_width.max(1)))
    }

    pub fn apply_text_edits(&mut self, edits: &[TextEdit]) {
        if edits.is_empty() {
            return;
        }

        self.begin_undo_group();
        self.push_undo_snapshot();

        let mut edits = edits.to_vec();
        edits.sort_by(|left, right| {
            let left_line = left.range.start.line;
            let right_line = right.range.start.line;
            right_line
                .cmp(&left_line)
                .then(right.range.start.character.cmp(&left.range.start.character))
        });

        for edit in edits {
            let start = self.char_index_for_lsp_position(edit.range.start);
            let end = self.char_index_for_lsp_position(edit.range.end);
            self.rope.remove(start..end);
            self.rope.insert(start, &edit.new_text);
        }

        self.end_undo_group();
        self.reload_git_gutter_markers();
    }

    pub fn insert_char(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
        ch: char,
    ) -> (usize, usize) {
        self.push_undo_snapshot();
        let insert_char_idx = self.char_index_for_display_position(display_row, display_column, page_width);
        self.rope.insert_char(insert_char_idx, ch);
        self.reload_git_gutter_markers();
        self.display_position_for_char_index(insert_char_idx.saturating_add(1), page_width)
    }

    pub fn insert_newline(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> (usize, usize) {
        self.push_undo_snapshot();
        let insert_char_idx = self.char_index_for_display_position(display_row, display_column, page_width);
        self.rope.insert_char(insert_char_idx, '\n');
        self.reload_git_gutter_markers();
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

        self.push_undo_snapshot();
        self.rope.remove((cursor_char_idx - 1)..cursor_char_idx);
        self.reload_git_gutter_markers();
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

        self.push_undo_snapshot();
        self.rope.remove(cursor_char_idx..cursor_char_idx.saturating_add(1));
        self.reload_git_gutter_markers();
        Some(self.display_position_for_char_index(cursor_char_idx, page_width))
    }

    pub fn remove_display_range(
        &mut self,
        start_row: usize,
        start_column: usize,
        end_row: usize,
        end_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let start_char_idx =
            self.char_index_for_display_position(start_row, start_column, page_width);
        let end_char_idx =
            self.char_index_for_display_position(end_row, end_column, page_width);

        if end_char_idx <= start_char_idx {
            return None;
        }

        self.push_undo_snapshot();
        self.rope.remove(start_char_idx..end_char_idx);
        self.reload_git_gutter_markers();
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
        self.push_undo_snapshot();
        self.rope.insert(insert_char_idx, &spaces);
        self.reload_git_gutter_markers();
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

        self.push_undo_snapshot();
        self.rope.insert_char(insert_char_idx, '\n');
        self.reload_git_gutter_markers();
        self.display_position_for_char_index(insert_char_idx.saturating_add(1), page_width)
    }

    pub fn open_above(&mut self, display_row: usize, page_width: usize) -> (usize, usize) {
        let page_width = page_width.max(1);
        let line_number = self.line_number_for_display_row(display_row, page_width);
        let line_index = line_number.saturating_sub(1);
        let insert_char_idx = self.rope.line_to_char(line_index);

        self.push_undo_snapshot();
        self.rope.insert_char(insert_char_idx, '\n');
        self.reload_git_gutter_markers();
        self.display_position_for_char_index(insert_char_idx, page_width)
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

    pub fn matching_bracket_position(
        &self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let cursor_char_index =
            self.char_index_for_display_position(display_row, display_column, page_width.max(1));

        if let Some((bracket_index, bracket)) = self.bracket_at_char_index(cursor_char_index) {
            return self.find_matching_bracket(bracket_index, bracket, page_width.max(1));
        }

        cursor_char_index
            .checked_sub(1)
            .and_then(|index| self.bracket_at_char_index(index))
            .and_then(|(bracket_index, bracket)| {
                self.find_matching_bracket(bracket_index, bracket, page_width.max(1))
            })
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
            self.push_undo_snapshot();
            self.rope.remove(
                line_start.saturating_add(indent_len)..line_start.saturating_add(trimmed_len),
            );
            self.reload_git_gutter_markers();
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
            self.push_undo_snapshot();
            self.rope = Rope::from_str("");
            self.reload_git_gutter_markers();
            return Some((removed, (0, 0)));
        }

        let removal_end = if had_newline {
            line_start.saturating_add(line_char_len)
        } else {
            line_start.saturating_add(trimmed_line_char_len(&line_text))
        };
        self.push_undo_snapshot();
        self.rope.remove(line_start..removal_end);
        self.reload_git_gutter_markers();

        let target_line_index = line_index.min(self.rope.len_lines().saturating_sub(1));
        let target_char_index = self.rope.line_to_char(target_line_index);
        Some((
            removed,
            self.display_position_for_char_index(target_char_index, page_width),
        ))
    }

    pub fn undo(&mut self) -> bool {
        let Some(previous_rope) = self.undo_stack.pop() else {
            return false;
        };

        self.redo_stack.push(self.rope.clone());
        self.rope = previous_rope;
        self.reload_git_gutter_markers();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next_rope) = self.redo_stack.pop() else {
            return false;
        };

        self.undo_stack.push(self.rope.clone());
        self.rope = next_rope;
        self.reload_git_gutter_markers();
        true
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

    fn char_index_for_lsp_position(&self, position: Position) -> usize {
        let line_index = usize::try_from(position.line)
            .unwrap_or(usize::MAX)
            .min(self.rope.len_lines().saturating_sub(1));
        let line_start = self.rope.line_to_char(line_index);
        let line_text = self.rope.line(line_index).to_string();
        let trimmed_len = trimmed_line_char_len(&line_text);
        line_start.saturating_add((position.character as usize).min(trimmed_len))
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

    fn push_undo_snapshot(&mut self) {
        if self.undo_group_active {
            if self.undo_group_snapshot_taken {
                return;
            }
            self.undo_group_snapshot_taken = true;
        }

        self.undo_stack.push(self.rope.clone());
        self.redo_stack.clear();
    }

    fn bracket_at_char_index(&self, char_index: usize) -> Option<(usize, char)> {
        if char_index >= self.rope.len_chars() {
            return None;
        }

        let ch = self.rope.char(char_index);
        is_supported_bracket(ch).then_some((char_index, ch))
    }

    fn find_matching_bracket(
        &self,
        bracket_index: usize,
        bracket: char,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let (matching_bracket, search_forward) = matching_bracket(bracket)?;
        let mut depth = 0usize;

        if search_forward {
            for index in bracket_index.saturating_add(1)..self.rope.len_chars() {
                let ch = self.rope.char(index);
                if ch == bracket {
                    depth = depth.saturating_add(1);
                } else if ch == matching_bracket {
                    if depth == 0 {
                        return Some(
                            self.display_position_for_char_index(index.saturating_add(1), page_width),
                        );
                    }
                    depth = depth.saturating_sub(1);
                }
            }
        } else {
            for index in (0..bracket_index).rev() {
                let ch = self.rope.char(index);
                if ch == bracket {
                    depth = depth.saturating_add(1);
                } else if ch == matching_bracket {
                    if depth == 0 {
                        return Some(self.display_position_for_char_index(index, page_width));
                    }
                    depth = depth.saturating_sub(1);
                }
            }
        }

        None
    }
}

pub struct EditablePage {
    pub rows: Vec<EditableRow>,
}

pub struct EditableRow {
    pub line_number: usize,
    pub text: String,
    pub syntax_spans: Vec<SyntaxTokenSpan>,
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

fn is_supported_bracket(ch: char) -> bool {
    matches!(ch, '(' | ')' | '{' | '}' | '[' | ']')
}

fn matching_bracket(ch: char) -> Option<(char, bool)> {
    match ch {
        '(' => Some((')', true)),
        '{' => Some(('}', true)),
        '[' => Some((']', true)),
        ')' => Some(('(', false)),
        '}' => Some(('{', false)),
        ']' => Some(('[', false)),
        _ => None,
    }
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
