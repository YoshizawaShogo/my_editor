use ropey::Rope;

use std::collections::BTreeSet;
use std::time::Instant;

use crate::{
    document::{Change, History, LineEnding, Revision, content_hash},
    position::{CharIdx, char_idx_to_line_col},
    view::{Selection, Selections},
};

#[derive(Debug)]
pub struct Editable {
    text: Rope,
    pub line_ending: LineEnding,
    pub modified: bool,
    saved_hash: u64,
    history: History,
    pub diagnostics: Vec<crate::lsp::Diagnostic>,
    pub git_lines: Vec<crate::editor::GitLine>,
    pub semantic_spans: Vec<crate::lsp::SemanticSpan>,
    pub syntax: Option<crate::highlight::IncrementalHighlighter>,
    pending_lsp_changes: Vec<lsp_types::TextDocumentContentChangeEvent>,
}

impl Default for Editable {
    fn default() -> Self {
        Self::new("")
    }
}

impl Editable {
    pub fn new(text: &str) -> Self {
        Self {
            text: Rope::from_str(text),
            line_ending: LineEnding::Lf,
            modified: false,
            saved_hash: content_hash(text),
            history: History::default(),
            diagnostics: Vec::new(),
            git_lines: Vec::new(),
            semantic_spans: Vec::new(),
            syntax: None,
            pending_lsp_changes: Vec::new(),
        }
    }

    pub fn text(&self) -> &Rope {
        &self.text
    }

    pub fn contents_for_save(&self) -> String {
        let contents = self.text.to_string();
        match self.line_ending {
            LineEnding::Lf => contents,
            LineEnding::Crlf => contents.replace('\n', "\r\n"),
        }
    }

    pub fn mark_saved(&mut self) {
        self.history.break_group();
        self.saved_hash = content_hash(&self.text.to_string());
        self.modified = false;
    }

    pub fn enable_highlight(&mut self, language: &str) {
        self.syntax =
            crate::highlight::IncrementalHighlighter::new(language, &self.text.to_string());
    }

    pub fn take_lsp_changes(&mut self) -> Vec<lsp_types::TextDocumentContentChangeEvent> {
        std::mem::take(&mut self.pending_lsp_changes)
    }

    pub fn insert(&mut self, selections: &mut Selections, inserted: &str) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let replacements = vec![inserted.to_owned(); targets.len()];
        self.replace_ranges(selections, targets, replacements, None, None, None);
    }

    pub fn insert_timed(&mut self, selections: &mut Selections, inserted: &str, at: Instant) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let replacements = vec![inserted.to_owned(); targets.len()];
        self.replace_ranges(selections, targets, replacements, Some(at), None, None);
    }

    pub fn insert_pair(
        &mut self,
        selections: &mut Selections,
        opening: char,
        closing: char,
        at: Option<Instant>,
    ) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let replacements = targets
            .iter()
            .map(|selection| {
                let selected = self.text.slice(selection.range()).to_string();
                format!("{opening}{selected}{closing}")
            })
            .collect();
        let cursor_backs = vec![1; targets.len()];
        self.replace_ranges(
            selections,
            targets,
            replacements,
            at,
            None,
            Some(cursor_backs),
        );
    }

    pub fn insert_fragments(&mut self, selections: &mut Selections, fragments: &[String]) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let replacements = if fragments.len() == targets.len() {
            fragments.to_vec()
        } else {
            vec![fragments.join("\n"); targets.len()]
        };
        self.replace_ranges(selections, targets, replacements, None, None, None);
    }

    pub fn insert_linewise_fragments(&mut self, selections: &mut Selections, fragments: &[String]) {
        let targets: Vec<_> = selections
            .iter()
            .map(|selection| {
                let line = self
                    .text
                    .char_to_line(selection.head.0.min(self.text.len_chars()));
                Selection::caret(CharIdx(self.text.line_to_char(line)))
            })
            .collect();
        let replacements = if fragments.len() == targets.len() {
            fragments
                .iter()
                .map(|fragment| format!("{fragment}\n"))
                .collect()
        } else {
            vec![format!("{}\n", fragments.join("\n")); targets.len()]
        };
        self.replace_ranges(selections, targets, replacements, None, None, None);
    }

    pub fn insert_newline(
        &mut self,
        selections: &mut Selections,
        line_comment: Option<&str>,
        tab_size: usize,
        insert_spaces: bool,
    ) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let edits = targets
            .iter()
            .map(|selection| {
                let insertion = selection.range().start;
                let (line, column) = char_idx_to_line_col(&self.text, CharIdx(insertion));
                let line_text = self.text.line(line);
                let full_indentation: String = line_text
                    .chars()
                    .take_while(|character| matches!(character, ' ' | '\t'))
                    .collect();
                let raw_indentation: String = if column < full_indentation.chars().count() {
                    line_text.chars().take(column).collect()
                } else {
                    full_indentation
                };
                let indentation = normalized_indentation(&raw_indentation, tab_size, insert_spaces);
                let inside_empty_brackets = selection.is_caret()
                    && insertion > 0
                    && insertion < self.text.len_chars()
                    && matches!(self.text.char(insertion - 1), '(' | '[' | '{')
                    && matching_pair(self.text.char(insertion - 1))
                        == Some(self.text.char(insertion));
                if inside_empty_brackets {
                    let unit = if insert_spaces {
                        " ".repeat(tab_size.max(1))
                    } else {
                        "\t".to_owned()
                    };
                    let inner_indentation = format!("{indentation}{unit}");
                    let cursor_back = 1 + indentation.chars().count();
                    return (format!("\n{inner_indentation}\n{indentation}"), cursor_back);
                }
                let prefix: String = self
                    .text
                    .slice(self.text.line_to_char(line)..insertion)
                    .into();
                let comment = line_comment
                    .and_then(|marker| continued_line_comment(&prefix, &raw_indentation, marker));
                (
                    comment.map_or_else(
                        || format!("\n{indentation}"),
                        |comment| format!("\n{indentation}{comment} "),
                    ),
                    0,
                )
            })
            .collect::<Vec<_>>();
        let (replacements, cursor_backs): (Vec<_>, Vec<_>) = edits.into_iter().unzip();
        self.replace_ranges(
            selections,
            targets,
            replacements,
            None,
            None,
            Some(cursor_backs),
        );
    }

    pub fn indent_lines(
        &mut self,
        selections: &mut Selections,
        indentation: &str,
        tab_size: usize,
        outdent: bool,
    ) {
        let mut lines = BTreeSet::new();
        for selection in selections.iter() {
            let range = selection.range();
            let start = self
                .text
                .char_to_line(range.start.min(self.text.len_chars()));
            let last = if range.end > range.start {
                range.end.saturating_sub(1)
            } else {
                range.end
            };
            let end = self.text.char_to_line(last.min(self.text.len_chars()));
            lines.extend(start..=end);
        }

        let mut edits = Vec::new();
        for line in lines {
            let start = self.text.line_to_char(line);
            if outdent {
                let text = self.text.line(line);
                let length = if text.get_char(0) == Some('\t') {
                    1
                } else {
                    text.chars()
                        .take_while(|character| *character == ' ')
                        .take(tab_size.max(1))
                        .count()
                };
                if length > 0 {
                    edits.push((start..start + length, String::new()));
                }
            } else {
                edits.push((start..start, indentation.to_owned()));
            }
        }
        if edits.is_empty() {
            return;
        }

        let before = selections.clone();
        let remapped = selections
            .iter()
            .map(|selection| Selection {
                anchor: CharIdx(remap_index(selection.anchor.0, &edits)),
                head: CharIdx(remap_index(selection.head.0, &edits)),
            })
            .collect::<Vec<_>>();
        let primary = selections.primary_index();
        let mut changes = edits
            .into_iter()
            .rev()
            .map(|(range, inserted)| Change {
                removed: self.text.slice(range.clone()).to_string(),
                range: CharIdx(range.start)..CharIdx(range.end),
                inserted,
            })
            .collect::<Vec<_>>();
        for change in &changes {
            if let Some(syntax) = &mut self.syntax {
                syntax.edit(
                    &self.text,
                    change.range.start.0..change.range.end.0,
                    &change.inserted,
                );
            }
            self.record_lsp_change(change.range.start.0..change.range.end.0, &change.inserted);
            self.update_semantic_spans(
                change.range.start.0..change.range.end.0,
                change.inserted.chars().count(),
            );
            self.apply_change(change);
        }
        if let Some(syntax) = &mut self.syntax {
            syntax.reparse(&self.text.to_string(), true);
        }
        let after = Selections::from_vec(remapped, primary);
        *selections = after.clone();
        self.history.record(
            Revision {
                changes: std::mem::take(&mut changes),
                selections_before: before,
                selections_after: after,
            },
            None,
        );
        self.refresh_modified();
    }

    pub fn delete_backward(&mut self, selections: &mut Selections) {
        self.delete_backward_smart(selections, 1, false);
    }

    pub fn delete_backward_smart(
        &mut self,
        selections: &mut Selections,
        tab_size: usize,
        insert_spaces: bool,
    ) {
        let targets: Vec<_> = selections
            .iter()
            .map(|selection| {
                if !selection.is_caret() || selection.head.0 == 0 {
                    return *selection;
                }
                let head = selection.head.0;
                if head < self.text.len_chars()
                    && matching_pair(self.text.char(head - 1)) == Some(self.text.char(head))
                {
                    return Selection {
                        anchor: CharIdx(head - 1),
                        head: CharIdx(head + 1),
                    };
                }
                let delete = if insert_spaces
                    && head >= tab_size.max(1)
                    && self
                        .text
                        .slice(head - tab_size.max(1)..head)
                        .chars()
                        .all(|character| character == ' ')
                {
                    tab_size.max(1)
                } else {
                    1
                };
                Selection {
                    anchor: CharIdx(head - delete),
                    head: selection.head,
                }
            })
            .collect();
        let replacements = vec![String::new(); targets.len()];
        self.replace_ranges(selections, targets, replacements, None, None, None);
    }

    pub fn insert_closing_brace(&mut self, selections: &mut Selections, at: Option<Instant>) {
        let edits: Vec<_> = selections
            .iter()
            .map(|selection| {
                if !selection.is_caret() {
                    return (*selection, "}".to_owned());
                }
                let head = selection.head.0.min(self.text.len_chars());
                let line = self.text.char_to_line(head);
                let line_start = self.text.line_to_char(line);
                let only_indentation = self
                    .text
                    .slice(line_start..head)
                    .chars()
                    .all(|character| matches!(character, ' ' | '\t'));
                if !only_indentation {
                    return (*selection, "}".to_owned());
                }
                let Some(opening) = unmatched_opening_brace(&self.text, head) else {
                    return (*selection, "}".to_owned());
                };
                let opening_line = self.text.char_to_line(opening);
                let indentation: String = self
                    .text
                    .line(opening_line)
                    .chars()
                    .take_while(|character| matches!(character, ' ' | '\t'))
                    .collect();
                (
                    Selection {
                        anchor: CharIdx(line_start),
                        head: selection.head,
                    },
                    format!("{indentation}}}"),
                )
            })
            .collect();
        let (targets, replacements): (Vec<_>, Vec<_>) = edits.into_iter().unzip();
        self.replace_ranges(selections, targets, replacements, at, None, None);
    }

    pub fn delete_forward(&mut self, selections: &mut Selections) {
        let targets: Vec<_> = selections
            .iter()
            .map(|selection| {
                if selection.is_caret() && selection.head.0 < self.text.len_chars() {
                    Selection {
                        anchor: selection.head,
                        head: CharIdx(selection.head.0 + 1),
                    }
                } else {
                    *selection
                }
            })
            .collect();
        let replacements = vec![String::new(); targets.len()];
        self.replace_ranges(selections, targets, replacements, None, None, None);
    }

    pub fn delete_lines(&mut self, selections: &mut Selections) {
        let mut lines: Vec<_> = selections
            .iter()
            .map(|selection| {
                self.text
                    .char_to_line(selection.head.0.min(self.text.len_chars()))
            })
            .collect();
        lines.sort_unstable();
        lines.dedup();
        let mut ranges = Vec::<std::ops::Range<usize>>::new();
        for line in lines {
            let mut start = self.text.line_to_char(line);
            let end = if line + 1 < self.text.len_lines() {
                self.text.line_to_char(line + 1)
            } else {
                if line > 0 {
                    start = start.saturating_sub(1);
                }
                self.text.len_chars()
            };
            if let Some(previous) = ranges.last_mut()
                && start <= previous.end
            {
                previous.end = previous.end.max(end);
            } else {
                ranges.push(start..end);
            }
        }
        let targets: Vec<_> = ranges
            .into_iter()
            .map(|range| Selection {
                anchor: CharIdx(range.start),
                head: CharIdx(range.end),
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let selections_before = selections.clone();
        *selections = Selections::from_vec(targets.clone(), 0);
        let replacements = vec![String::new(); targets.len()];
        self.replace_ranges(
            selections,
            targets,
            replacements,
            None,
            Some(selections_before),
            None,
        );
    }

    pub fn selected_texts(&self, selections: &Selections) -> Vec<String> {
        selections
            .iter()
            .map(|selection| self.text.slice(selection.range()).to_string())
            .collect()
    }

    pub fn break_history_group(&mut self) {
        self.history.break_group();
    }

    pub fn undo(&mut self, selections: &mut Selections) -> bool {
        let Some(revision) = self.history.take_undo() else {
            return false;
        };
        for change in revision.changes.iter().rev() {
            let start = change.range.start.0;
            let end = start + change.inserted.chars().count();
            self.record_lsp_change(start..end, &change.removed);
            self.update_semantic_spans(start..end, change.removed.chars().count());
            self.text.remove(start..end);
            self.text.insert(start, &change.removed);
        }
        *selections = revision.selections_before.clone();
        self.history.finish_undo(revision);
        if let Some(syntax) = &mut self.syntax {
            syntax.reparse(&self.text.to_string(), false);
        }
        self.refresh_modified();
        true
    }

    pub fn redo(&mut self, selections: &mut Selections) -> bool {
        let Some(revision) = self.history.take_redo() else {
            return false;
        };
        for change in &revision.changes {
            self.record_lsp_change(change.range.start.0..change.range.end.0, &change.inserted);
            self.update_semantic_spans(
                change.range.start.0..change.range.end.0,
                change.inserted.chars().count(),
            );
            self.apply_change(change);
        }
        *selections = revision.selections_after.clone();
        self.history.finish_redo(revision);
        if let Some(syntax) = &mut self.syntax {
            syntax.reparse(&self.text.to_string(), false);
        }
        self.refresh_modified();
        true
    }

    fn replace_ranges(
        &mut self,
        selections: &mut Selections,
        targets: Vec<Selection>,
        replacements: Vec<String>,
        insert_at: Option<Instant>,
        history_selections_before: Option<Selections>,
        cursor_backs: Option<Vec<usize>>,
    ) {
        assert_eq!(targets.len(), replacements.len());
        let working_selections = selections.clone();
        let before = history_selections_before.unwrap_or_else(|| working_selections.clone());
        let primary = selections.primary_index();
        let cursor_backs = cursor_backs.unwrap_or_else(|| vec![0; targets.len()]);
        let mut edits: Vec<PendingEdit> = targets
            .into_iter()
            .zip(replacements)
            .zip(cursor_backs)
            .enumerate()
            .filter_map(|(selection_index, ((selection, inserted), cursor_back))| {
                let range = selection.range();
                if range.is_empty() && inserted.is_empty() {
                    return None;
                }
                Some(PendingEdit {
                    selection_index,
                    removed: self.text.slice(range.clone()).to_string(),
                    range,
                    inserted,
                    cursor_back,
                })
            })
            .collect();
        if edits.is_empty() {
            return;
        }
        edits.sort_by_key(|edit| edit.range.start);

        let mut after_ranges: Vec<_> = working_selections.iter().copied().collect();
        let mut offset = 0isize;
        for edit in &edits {
            let inserted_len = edit.inserted.chars().count();
            let caret = (edit.range.start as isize + offset + inserted_len as isize
                - edit.cursor_back.min(inserted_len) as isize) as usize;
            after_ranges[edit.selection_index] = Selection::caret(CharIdx(caret));
            offset += inserted_len as isize - edit.range.len() as isize;
        }

        let mut changes: Vec<_> = edits
            .into_iter()
            .rev()
            .map(|edit| Change {
                range: CharIdx(edit.range.start)..CharIdx(edit.range.end),
                removed: edit.removed,
                inserted: edit.inserted,
            })
            .collect();
        for change in &changes {
            if let Some(syntax) = &mut self.syntax {
                syntax.edit(
                    &self.text,
                    change.range.start.0..change.range.end.0,
                    &change.inserted,
                );
            }
            self.record_lsp_change(change.range.start.0..change.range.end.0, &change.inserted);
            self.update_semantic_spans(
                change.range.start.0..change.range.end.0,
                change.inserted.chars().count(),
            );
            self.apply_change(change);
        }
        if let Some(syntax) = &mut self.syntax {
            syntax.reparse(&self.text.to_string(), true);
        }

        let after = Selections::from_vec(after_ranges, primary);
        *selections = after.clone();
        self.history.record(
            Revision {
                changes: std::mem::take(&mut changes),
                selections_before: before,
                selections_after: after,
            },
            insert_at,
        );
        self.refresh_modified();
    }

    fn apply_change(&mut self, change: &Change) {
        self.text.remove(change.range.start.0..change.range.end.0);
        self.text.insert(change.range.start.0, &change.inserted);
    }

    fn update_semantic_spans(&mut self, replaced: std::ops::Range<usize>, inserted_len: usize) {
        let removed_len = replaced.len();
        self.semantic_spans.retain_mut(|span| {
            if removed_len == 0 {
                if span.end.0 <= replaced.start {
                    return true;
                }
                if span.start.0 >= replaced.start {
                    span.start.0 += inserted_len;
                    span.end.0 += inserted_len;
                } else {
                    span.end.0 += inserted_len;
                }
                return true;
            }
            if span.end.0 <= replaced.start {
                return true;
            }
            if span.start.0 >= replaced.end {
                span.start.0 = shift_index(span.start.0, inserted_len, removed_len);
                span.end.0 = shift_index(span.end.0, inserted_len, removed_len);
                return true;
            }
            false
        });
    }

    fn record_lsp_change(&mut self, replaced: std::ops::Range<usize>, inserted: &str) {
        self.pending_lsp_changes
            .push(lsp_types::TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range::new(
                    crate::position::char_idx_to_lsp_position(&self.text, CharIdx(replaced.start)),
                    crate::position::char_idx_to_lsp_position(&self.text, CharIdx(replaced.end)),
                )),
                range_length: None,
                text: inserted.to_owned(),
            });
    }

    fn refresh_modified(&mut self) {
        self.modified = content_hash(&self.text.to_string()) != self.saved_hash;
    }
}

fn continued_line_comment(prefix: &str, indentation: &str, marker: &str) -> Option<String> {
    let content = prefix.strip_prefix(indentation)?;
    if !content.starts_with(marker) {
        return None;
    }
    if marker == "//" {
        if content.starts_with("///") && !content.starts_with("////") {
            return Some("///".to_owned());
        }
        if content.starts_with("//!") {
            return Some("//!".to_owned());
        }
    }
    Some(marker.to_owned())
}

fn normalized_indentation(indentation: &str, tab_size: usize, insert_spaces: bool) -> String {
    if !insert_spaces {
        return indentation.to_owned();
    }
    let width = indentation.chars().fold(0, |column, character| {
        crate::position::display_col_after(column, character, tab_size)
    });
    " ".repeat(width)
}

fn remap_index(index: usize, edits: &[(std::ops::Range<usize>, String)]) -> usize {
    let mut offset = 0isize;
    for (range, inserted) in edits {
        if index < range.start {
            break;
        }
        if !range.is_empty() && index < range.end {
            return (range.start as isize + offset) as usize;
        }
        offset += inserted.chars().count() as isize - range.len() as isize;
    }
    (index as isize + offset) as usize
}

fn shift_index(index: usize, inserted_len: usize, removed_len: usize) -> usize {
    if inserted_len >= removed_len {
        index + inserted_len - removed_len
    } else {
        index.saturating_sub(removed_len - inserted_len)
    }
}

struct PendingEdit {
    selection_index: usize,
    range: std::ops::Range<usize>,
    removed: String,
    inserted: String,
    cursor_back: usize,
}

fn matching_pair(opening: char) -> Option<char> {
    match opening {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '\'' => Some('\''),
        '"' => Some('"'),
        '`' => Some('`'),
        _ => None,
    }
}

fn unmatched_opening_brace(text: &Rope, before: usize) -> Option<usize> {
    let mut depth = 0;
    for index in (0..before.min(text.len_chars())).rev() {
        match text.char(index) {
            '}' => depth += 1,
            '{' if depth == 0 => return Some(index),
            '{' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_undo_and_redo_are_reversible() {
        let mut editable = Editable::new("abc");
        let mut selections = Selections::single(Selection::caret(CharIdx(3)));

        editable.insert(&mut selections, "日");
        assert_eq!(editable.text().to_string(), "abc日");
        assert_eq!(selections.primary().head, CharIdx(4));

        assert!(editable.undo(&mut selections));
        assert_eq!(editable.text().to_string(), "abc");
        assert_eq!(selections.primary().head, CharIdx(3));

        assert!(editable.redo(&mut selections));
        assert_eq!(editable.text().to_string(), "abc日");
    }

    #[test]
    fn undoing_to_the_saved_contents_clears_modified_state() {
        let mut editable = Editable::new("saved");
        let mut selections = Selections::single(Selection::caret(CharIdx(5)));

        editable.insert(&mut selections, "!");
        assert!(editable.modified);
        assert!(editable.undo(&mut selections));

        assert_eq!(editable.text().to_string(), "saved");
        assert!(!editable.modified);
    }

    #[test]
    fn save_marks_a_history_boundary_without_discarding_undo() {
        let mut editable = Editable::new("");
        let mut selections = Selections::single(Selection::caret(CharIdx(0)));
        let start = Instant::now();

        editable.insert_timed(&mut selections, "a", start);
        editable.mark_saved();
        editable.insert_timed(
            &mut selections,
            "b",
            start + std::time::Duration::from_millis(10),
        );

        assert!(editable.undo(&mut selections));
        assert_eq!(editable.text().to_string(), "a");
        assert!(!editable.modified);
        assert!(editable.undo(&mut selections));
        assert_eq!(editable.text().to_string(), "");
        assert!(editable.modified);
    }

    #[test]
    fn newline_copies_the_current_indentation() {
        let mut editable = Editable::new("    value");
        let mut selections = Selections::single(Selection::caret(CharIdx(9)));

        editable.insert_newline(&mut selections, None, 4, true);

        assert_eq!(editable.text().to_string(), "    value\n    ");
        assert_eq!(selections.primary().head, CharIdx(14));
        assert_eq!(
            editable
                .text()
                .chars()
                .filter(|character| *character == '\n')
                .count(),
            1
        );
    }

    #[test]
    fn newline_inside_indentation_does_not_duplicate_existing_indent() {
        let mut editable = Editable::new("    value");
        let mut selections = Selections::single(Selection::caret(CharIdx(2)));

        editable.insert_newline(&mut selections, None, 4, true);

        assert_eq!(editable.text().to_string(), "  \n    value");
        assert_eq!(selections.primary().head, CharIdx(5));
    }

    #[test]
    fn newline_continues_line_and_doc_comments() {
        let mut editable = Editable::new("    // note");
        let mut selections = Selections::single(Selection::caret(CharIdx(11)));
        editable.insert_newline(&mut selections, Some("//"), 4, true);
        assert_eq!(editable.text().to_string(), "    // note\n    // ");

        let mut editable = Editable::new("/// docs");
        let mut selections = Selections::single(Selection::caret(CharIdx(8)));
        editable.insert_newline(&mut selections, Some("//"), 4, true);
        assert_eq!(editable.text().to_string(), "/// docs\n/// ");
    }

    #[test]
    fn semantic_spans_shift_after_edits_without_clearing_unaffected_tokens() {
        let mut editable = Editable::new("foo bar");
        editable.semantic_spans = vec![
            crate::lsp::SemanticSpan {
                start: CharIdx(0),
                end: CharIdx(3),
                token_type: 0,
            },
            crate::lsp::SemanticSpan {
                start: CharIdx(4),
                end: CharIdx(7),
                token_type: 1,
            },
        ];
        let mut selections = Selections::single(Selection::caret(CharIdx(0)));

        editable.insert(&mut selections, "x");

        assert_eq!(editable.semantic_spans[0].start, CharIdx(1));
        assert_eq!(editable.semantic_spans[0].end, CharIdx(4));
        assert_eq!(editable.semantic_spans[1].start, CharIdx(5));
        assert_eq!(editable.semantic_spans[1].end, CharIdx(8));
    }

    #[test]
    fn multi_cursor_edit_offsets_and_undo_are_correct() {
        let mut editable = Editable::new("abcd");
        let mut selections = Selections::from_vec(
            vec![Selection::caret(CharIdx(1)), Selection::caret(CharIdx(3))],
            1,
        );

        editable.insert(&mut selections, "X");

        assert_eq!(editable.text().to_string(), "aXbcXd");
        assert_eq!(
            selections.iter().map(|s| s.head.0).collect::<Vec<_>>(),
            vec![2, 5]
        );

        assert!(editable.undo(&mut selections));
        assert_eq!(editable.text().to_string(), "abcd");
        assert_eq!(
            selections.iter().map(|s| s.head.0).collect::<Vec<_>>(),
            vec![1, 3]
        );

        assert!(editable.redo(&mut selections));
        assert_eq!(editable.text().to_string(), "aXbcXd");
    }

    #[test]
    fn multi_cursor_delete_uses_original_coordinates() {
        let mut editable = Editable::new("abcdef");
        let mut selections = Selections::from_vec(
            vec![Selection::caret(CharIdx(2)), Selection::caret(CharIdx(5))],
            0,
        );

        editable.delete_backward(&mut selections);

        assert_eq!(editable.text().to_string(), "acdf");
        assert_eq!(
            selections.iter().map(|s| s.head.0).collect::<Vec<_>>(),
            vec![1, 3]
        );
    }

    #[test]
    fn nearby_typed_characters_undo_as_one_revision() {
        let mut editable = Editable::new("");
        let mut selections = Selections::default();
        let start = Instant::now();

        editable.insert_timed(&mut selections, "a", start);
        editable.insert_timed(
            &mut selections,
            "b",
            start + std::time::Duration::from_millis(100),
        );

        assert_eq!(editable.text().to_string(), "ab");
        assert!(editable.undo(&mut selections));
        assert_eq!(editable.text().to_string(), "");
    }

    #[test]
    fn pairs_place_the_caret_inside_and_backspace_removes_both_characters() {
        for (opening, closing) in [
            ('(', ')'),
            ('[', ']'),
            ('{', '}'),
            ('\'', '\''),
            ('"', '"'),
            ('`', '`'),
        ] {
            let mut editable = Editable::new("");
            let mut selections = Selections::default();

            editable.insert_pair(&mut selections, opening, closing, None);
            assert_eq!(editable.text().to_string(), format!("{opening}{closing}"));
            assert_eq!(selections.primary().head, CharIdx(1));

            editable.delete_backward_smart(&mut selections, 4, true);
            assert_eq!(editable.text().to_string(), "");
            assert_eq!(selections.primary().head, CharIdx(0));
        }
    }

    #[test]
    fn soft_tab_backspace_removes_one_configured_indent_unit() {
        let mut editable = Editable::new("a    b");
        let mut selections = Selections::single(Selection::caret(CharIdx(5)));

        editable.delete_backward_smart(&mut selections, 4, true);

        assert_eq!(editable.text().to_string(), "ab");
        assert_eq!(selections.primary().head, CharIdx(1));
    }

    #[test]
    fn closing_brace_aligns_an_indented_blank_line_with_its_opening_brace() {
        let mut editable = Editable::new("    if ready {\n        ");
        let mut selections =
            Selections::single(Selection::caret(CharIdx(editable.text().len_chars())));

        editable.insert_closing_brace(&mut selections, None);

        assert_eq!(editable.text().to_string(), "    if ready {\n    }");
        assert_eq!(selections.primary().head, CharIdx(20));
        assert!(editable.undo(&mut selections));
        assert_eq!(editable.text().to_string(), "    if ready {\n        ");
    }

    #[test]
    fn newline_inside_empty_brackets_creates_an_indented_blank_line() {
        for (opening, closing) in [('(', ')'), ('[', ']'), ('{', '}')] {
            let mut editable = Editable::new(&format!("    call{opening}{closing}"));
            let mut selections = Selections::single(Selection::caret(CharIdx(9)));

            editable.insert_newline(&mut selections, None, 4, true);

            assert_eq!(
                editable.text().to_string(),
                format!("    call{opening}\n        \n    {closing}")
            );
            assert_eq!(selections.primary().head, CharIdx(18));
            assert!(editable.undo(&mut selections));
            assert_eq!(
                editable.text().to_string(),
                format!("    call{opening}{closing}")
            );
        }
    }
}
