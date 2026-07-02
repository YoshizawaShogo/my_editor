use ropey::Rope;

use std::path::Path;
use std::time::Instant;

use crate::{
    document::{Change, History, LineEnding, PersistedHistory, Revision, content_hash},
    position::{CharIdx, char_idx_to_line_col},
    view::{Selection, Selections},
};

#[derive(Debug)]
pub struct Editable {
    text: Rope,
    pub line_ending: LineEnding,
    pub modified: bool,
    history: History,
    pub diagnostics: Vec<crate::lsp::Diagnostic>,
    pub git_lines: Vec<crate::editor::GitLine>,
    pub semantic_spans: Vec<crate::lsp::SemanticSpan>,
    pub syntax: Option<crate::highlight::IncrementalHighlighter>,
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
            history: History::default(),
            diagnostics: Vec::new(),
            git_lines: Vec::new(),
            semantic_spans: Vec::new(),
            syntax: None,
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
        self.modified = false;
    }

    pub fn enable_highlight(&mut self, language: &str) {
        self.syntax =
            crate::highlight::IncrementalHighlighter::new(language, &self.text.to_string());
    }

    pub fn persisted_history(&self, path: &Path) -> PersistedHistory {
        let (past, future) = self.history.snapshot();
        PersistedHistory {
            path: path.to_path_buf(),
            base_hash: content_hash(&self.text.to_string()),
            line_ending: self.line_ending,
            past,
            future,
        }
    }

    pub fn restore_history(&mut self, history: PersistedHistory) {
        self.history.restore(history.past, history.future);
    }

    pub fn insert(&mut self, selections: &mut Selections, inserted: &str) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let replacements = vec![inserted.to_owned(); targets.len()];
        self.replace_ranges(selections, targets, replacements, None);
    }

    pub fn insert_timed(&mut self, selections: &mut Selections, inserted: &str, at: Instant) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let replacements = vec![inserted.to_owned(); targets.len()];
        self.replace_ranges(selections, targets, replacements, Some(at));
    }

    pub fn insert_fragments(&mut self, selections: &mut Selections, fragments: &[String]) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let replacements = if fragments.len() == targets.len() {
            fragments.to_vec()
        } else {
            vec![fragments.join("\n"); targets.len()]
        };
        self.replace_ranges(selections, targets, replacements, None);
    }

    pub fn insert_newline(&mut self, selections: &mut Selections) {
        let targets: Vec<_> = selections.iter().copied().collect();
        let replacements = targets
            .iter()
            .map(|selection| {
                let (line, _) = char_idx_to_line_col(&self.text, selection.head);
                let indentation: String = self
                    .text
                    .line(line)
                    .chars()
                    .take_while(|character| matches!(character, ' ' | '\t'))
                    .collect();
                format!("\n{indentation}")
            })
            .collect();
        self.replace_ranges(selections, targets, replacements, None);
    }

    pub fn delete_backward(&mut self, selections: &mut Selections) {
        let targets: Vec<_> = selections
            .iter()
            .map(|selection| {
                if selection.is_caret() && selection.head.0 > 0 {
                    Selection {
                        anchor: CharIdx(selection.head.0 - 1),
                        head: selection.head,
                    }
                } else {
                    *selection
                }
            })
            .collect();
        let replacements = vec![String::new(); targets.len()];
        self.replace_ranges(selections, targets, replacements, None);
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
        self.replace_ranges(selections, targets, replacements, None);
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
            self.text.remove(start..end);
            self.text.insert(start, &change.removed);
        }
        *selections = revision.selections_before.clone();
        self.history.finish_undo(revision);
        if let Some(syntax) = &mut self.syntax {
            syntax.reparse(&self.text.to_string(), false);
        }
        self.modified = true;
        true
    }

    pub fn redo(&mut self, selections: &mut Selections) -> bool {
        let Some(revision) = self.history.take_redo() else {
            return false;
        };
        for change in &revision.changes {
            self.apply_change(change);
        }
        *selections = revision.selections_after.clone();
        self.history.finish_redo(revision);
        if let Some(syntax) = &mut self.syntax {
            syntax.reparse(&self.text.to_string(), false);
        }
        self.modified = true;
        true
    }

    fn replace_ranges(
        &mut self,
        selections: &mut Selections,
        targets: Vec<Selection>,
        replacements: Vec<String>,
        insert_at: Option<Instant>,
    ) {
        assert_eq!(targets.len(), replacements.len());
        let before = selections.clone();
        let primary = selections.primary_index();
        let mut edits: Vec<PendingEdit> = targets
            .into_iter()
            .zip(replacements)
            .enumerate()
            .filter_map(|(selection_index, (selection, inserted))| {
                let range = selection.range();
                if range.is_empty() && inserted.is_empty() {
                    return None;
                }
                Some(PendingEdit {
                    selection_index,
                    removed: self.text.slice(range.clone()).to_string(),
                    range,
                    inserted,
                })
            })
            .collect();
        if edits.is_empty() {
            return;
        }
        edits.sort_by_key(|edit| edit.range.start);

        let mut after_ranges: Vec<_> = before.iter().copied().collect();
        let mut offset = 0isize;
        for edit in &edits {
            let inserted_len = edit.inserted.chars().count();
            let caret = (edit.range.start as isize + offset + inserted_len as isize) as usize;
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
        self.modified = true;
    }

    fn apply_change(&mut self, change: &Change) {
        self.text.remove(change.range.start.0..change.range.end.0);
        self.text.insert(change.range.start.0, &change.inserted);
    }
}

struct PendingEdit {
    selection_index: usize,
    range: std::ops::Range<usize>,
    removed: String,
    inserted: String,
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
    fn newline_copies_the_current_indentation() {
        let mut editable = Editable::new("    value");
        let mut selections = Selections::single(Selection::caret(CharIdx(9)));

        editable.insert_newline(&mut selections);

        assert_eq!(editable.text().to_string(), "    value\n    ");
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
}
