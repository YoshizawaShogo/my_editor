//! Filling in an expanded snippet: insert the body, then walk its tab stops with
//! Tab / Shift+Tab. The stops themselves live on the document (`snippet_stops`),
//! shifted alongside diagnostics as the user edits; this tracks which stop is
//! current. (Distinct from `crate::snippet`, which holds the snippet templates.)

use crate::document::Document;
use crate::document::DocumentId;
use crate::position::CharIdx;
use crate::view::Selection;

/// A snippet being filled in. The stops themselves live on the document (so edits
/// shift them); this just remembers which document and which stop is current.
pub(super) struct SnippetSession {
    doc: DocumentId,
    current: usize,
}

impl super::Editor {
    /// Leading whitespace of the active caret's line, so a snippet's continuation
    /// lines can be re-indented to match where it is inserted.
    fn caret_line_indent(&self) -> String {
        let Some(pane) = self.layout.active_editor(self.focus) else {
            return String::new();
        };
        let Some(editable) = self
            .documents
            .get(&pane.view.doc)
            .and_then(Document::editable_opt)
        else {
            return String::new();
        };
        let text = editable.text();
        let caret = pane.view.selections.primary().head.0.min(text.len_chars());
        let line_start = text.line_to_char(text.char_to_line(caret));
        text.slice(line_start..)
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .collect()
    }

    /// Expand a chosen snippet: drop the `prefix_len` characters already typed,
    /// insert the expanded body, and select its first tab stop (or land the caret
    /// at the end) so typing overwrites the placeholder. When the snippet has more
    /// than one stop, a session is started so Tab/Shift+Tab walk the rest.
    pub(super) fn expand_snippet_body(&mut self, body: &str, prefix_len: usize) {
        let expansion = crate::snippet::expand(body, &self.caret_line_indent());
        let text = expansion.text;
        let text_len = text.chars().count();
        let stops = expansion.stops;
        let doc = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc);

        self.edit_active(|document, view| {
            let head = view.selections.primary().head.0;
            let start = head.saturating_sub(prefix_len);
            view.selections.set_single(Selection {
                anchor: CharIdx(start),
                head: CharIdx(head),
            });
            document.editable_mut().insert(&mut view.selections, &text);
            let absolute: Vec<_> = stops
                .iter()
                .map(|stop| (start + stop.start)..(start + stop.end))
                .collect();
            let (anchor, head) = match absolute.first() {
                Some(stop) => (stop.start, stop.end),
                None => (start + text_len, start + text_len),
            };
            // More than one stop: remember them on the document so Tab can walk
            // them and edits keep them aligned. A single stop needs no session.
            if absolute.len() >= 2 {
                document.editable_mut().set_snippet_stops(absolute);
            } else {
                document.editable_mut().clear_snippet_stops();
            }
            view.selections.set_single(Selection {
                anchor: CharIdx(anchor),
                head: CharIdx(head),
            });
        });

        self.snippet_session = doc
            .filter(|doc| {
                self.documents
                    .get(doc)
                    .and_then(Document::editable_opt)
                    .is_some_and(|editable| editable.snippet_stops().len() >= 2)
            })
            .map(|doc| SnippetSession { doc, current: 0 });
    }

    /// Move to the next snippet stop, returning whether a session was active and
    /// advanced. Ends the session (returning false) once the last stop is passed,
    /// so Tab falls through to indentation afterwards.
    pub(super) fn advance_snippet_stop(&mut self) -> bool {
        self.step_snippet_stop(true)
    }

    /// Move to the previous snippet stop; false when there is none before.
    pub(super) fn retreat_snippet_stop(&mut self) -> bool {
        self.step_snippet_stop(false)
    }

    fn step_snippet_stop(&mut self, forward: bool) -> bool {
        let Some(session) = &self.snippet_session else {
            return false;
        };
        let doc = session.doc;
        // A session only applies while its document is the one in focus.
        if self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
            != Some(doc)
        {
            self.clear_snippet_session();
            return false;
        }
        let stops_len = self
            .documents
            .get(&doc)
            .and_then(Document::editable_opt)
            .map_or(0, |editable| editable.snippet_stops().len());
        let target = if forward {
            session.current + 1
        } else {
            match session.current.checked_sub(1) {
                Some(previous) => previous,
                None => return false,
            }
        };
        if target >= stops_len {
            // Walked past the final stop — the snippet is done.
            self.clear_snippet_session();
            return false;
        }
        let range = self
            .documents
            .get(&doc)
            .and_then(Document::editable_opt)
            .map(|editable| editable.snippet_stops()[target].clone());
        let Some(range) = range else {
            self.clear_snippet_session();
            return false;
        };
        if let Some(session) = self.snippet_session.as_mut() {
            session.current = target;
        }
        if let Some(pane) = self.layout.active_editor_mut(self.focus) {
            pane.view.selections.set_single(Selection {
                anchor: CharIdx(range.start),
                head: CharIdx(range.end),
            });
        }
        self.ensure_cursor_visible();
        self.dirty = true;
        true
    }

    pub(super) fn clear_snippet_session(&mut self) {
        if let Some(session) = self.snippet_session.take()
            && let Some(document) = self.documents.get_mut(&session.doc)
            && let Some(editable) = document.editable_opt_mut()
        {
            editable.clear_snippet_stops();
        }
    }
}
