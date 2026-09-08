//! Cursor jump history — the back/forward stack behind Ctrl+E / Ctrl+R.

use crate::document::{Document, DocumentId};
use crate::position::CharIdx;
use crate::view::{Selection, View};

impl super::Editor {
    pub(super) fn current_location(&self) -> Option<(DocumentId, CharIdx)> {
        let pane = self.layout.active_editor(self.focus)?;
        Some((pane.view.doc, pane.view.selections.primary().head))
    }

    /// Remember the caret's current spot before a jump so Ctrl+E can return to it.
    pub(super) fn record_jump_origin(&mut self) {
        let Some(location) = self.current_location() else {
            return;
        };
        if let Some(&last) = self.nav_back.last() {
            if last == location {
                return;
            }
            // Collapse consecutive origins on the same line, so moving or clicking
            // around within one line does not fill the back-stack with near-
            // duplicates that each need a separate Ctrl+E to step past.
            if last.0 == location.0 && self.same_line(location.0, last.1, location.1) {
                *self.nav_back.last_mut().expect("checked non-empty") = location;
                self.nav_forward.clear();
                return;
            }
        }
        self.nav_back.push(location);
        if self.nav_back.len() > 200 {
            self.nav_back.remove(0);
        }
        self.nav_forward.clear();
    }

    /// Whether two caret indices in `doc` fall on the same line.
    fn same_line(&self, doc: DocumentId, a: CharIdx, b: CharIdx) -> bool {
        self.documents
            .get(&doc)
            .and_then(Document::editable_opt)
            .is_some_and(|editable| {
                let text = editable.text();
                let line = |index: CharIdx| text.char_to_line(index.0.min(text.len_chars()));
                line(a) == line(b)
            })
    }

    /// Ctrl+E / Ctrl+R: step back and forward through visited caret locations.
    pub(super) fn navigate_history(&mut self, back: bool) {
        let Some(current) = self.current_location() else {
            return;
        };
        let target = if back {
            self.nav_back.pop()
        } else {
            self.nav_forward.pop()
        };
        let Some((doc, head)) = target else {
            self.status = Some(if back {
                "戻る履歴がありません".to_owned()
            } else {
                "進む履歴がありません".to_owned()
            });
            self.dirty = true;
            return;
        };
        if !self.documents.contains_key(&doc) {
            // The document was closed; drop the stale entry and retry.
            return self.navigate_history(back);
        }
        if back {
            self.nav_forward.push(current);
        } else {
            self.nav_back.push(current);
        }
        self.go_to_location(doc, head);
    }

    pub(super) fn go_to_location(&mut self, doc: DocumentId, head: CharIdx) {
        let clamped = self
            .documents
            .get(&doc)
            .and_then(Document::editable_opt)
            .map_or(head, |editable| {
                CharIdx(head.0.min(editable.text().len_chars()))
            });
        let focus = self.focus;
        if let Some(pane) = self.layout.active_editor_mut(focus) {
            if pane.view.doc != doc {
                pane.view = View::new(doc);
            }
            pane.view.selections.set_single(Selection::caret(clamped));
        }
        self.reveal_caret_with_context();
        self.dirty = true;
    }
}
