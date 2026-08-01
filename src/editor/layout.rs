use crate::view::View;

use super::{Focus, SearchState, Side};

#[derive(Debug)]
pub struct EditorPane {
    pub view: View,
}

/// One file pane on the left, plus at most one pane on the right.
///
/// The right pane owns its own state, so the kinds are mutually exclusive by
/// construction. They used not to be: "the shell is open" was a `Layout`
/// variant while the find pane was a separate `Option<SearchState>` on the
/// editor and the terminal a separate `Option<vt100::Parser>`, so Ctrl+F over
/// an open shell left both alive and the renderer picked a winner. Installing a
/// right pane now drops whatever was there.
pub struct Layout {
    pub(super) left: EditorPane,
    pub(super) right: Option<RightPane>,
}

/// What occupies the right half. The left half is always a file, so every kind
/// of split — a second file, find/replace, the shell, whatever comes next — is
/// a variant here rather than a flag somewhere else.
pub(super) enum RightPane {
    /// A second file, drawn side by side with the left one.
    Editor(EditorPane),
    /// A second file, aligned against the left one line by line.
    Diff(DiffPane),
    Search(SearchState),
    /// The shell. Carries no state of its own: the session outlives the pane,
    /// so it lives in [`super::Editor::shell`] and this only says it is on
    /// screen — the same way an [`EditorPane`] names a document that is kept in
    /// `Editor::documents`.
    Shell,
}

/// The diff view's own state. Its scroll position is an index into the aligned
/// rows, which interleave both files and include blanks where one side has no
/// line — so it is a line number in neither, and cannot live in a
/// [`crate::view::Scroll`].
pub(super) struct DiffPane {
    pub(super) pane: EditorPane,
    pub(super) top_row: usize,
}

impl Layout {
    pub fn new(view: View) -> Self {
        Self {
            left: EditorPane { view },
            right: None,
        }
    }

    /// The pane the caret is in, or `None` while the shell has focus.
    ///
    /// A right-side focus falls back to the left pane when the right half is
    /// not a file: there is only one file pane in that case, and it is the one
    /// the caret belongs to.
    pub fn active_editor(&self, focus: Focus) -> Option<&EditorPane> {
        match focus {
            Focus::Shell => None,
            Focus::Editor(Side::Right) | Focus::Completion(Side::Right) => {
                Some(self.right_editor().unwrap_or(&self.left))
            }
            Focus::Editor(Side::Left) | Focus::Completion(Side::Left) | Focus::Overlay => {
                Some(&self.left)
            }
        }
    }

    pub fn active_editor_mut(&mut self, focus: Focus) -> Option<&mut EditorPane> {
        match focus {
            Focus::Shell => None,
            Focus::Editor(Side::Right) | Focus::Completion(Side::Right) => match &mut self.right {
                Some(RightPane::Editor(pane)) => Some(pane),
                Some(RightPane::Diff(diff)) => Some(&mut diff.pane),
                _ => Some(&mut self.left),
            },
            Focus::Editor(Side::Left) | Focus::Completion(Side::Left) | Focus::Overlay => {
                Some(&mut self.left)
            }
        }
    }

    pub fn panes_mut(&mut self) -> Vec<&mut EditorPane> {
        let mut panes = vec![&mut self.left];
        match &mut self.right {
            Some(RightPane::Editor(pane)) => panes.push(pane),
            Some(RightPane::Diff(diff)) => panes.push(&mut diff.pane),
            _ => {}
        }
        panes
    }

    /// The two file panes and whether they are drawn as a diff, when the right
    /// half holds a second file.
    pub fn split(&self) -> Option<(&EditorPane, &EditorPane, bool)> {
        match &self.right {
            Some(RightPane::Editor(pane)) => Some((&self.left, pane, false)),
            Some(RightPane::Diff(diff)) => Some((&self.left, &diff.pane, true)),
            _ => None,
        }
    }

    pub(super) fn right_editor(&self) -> Option<&EditorPane> {
        match &self.right {
            Some(RightPane::Editor(pane)) => Some(pane),
            Some(RightPane::Diff(diff)) => Some(&diff.pane),
            _ => None,
        }
    }

    pub(super) fn is_diff(&self) -> bool {
        matches!(self.right, Some(RightPane::Diff(_)))
    }

    /// Whether the right half is a second file drawn plainly. The diff also
    /// holds a file, but it is a different pane and answers to its own keys.
    pub(super) fn is_editor_split(&self) -> bool {
        matches!(self.right, Some(RightPane::Editor(_)))
    }

    pub(super) fn diff(&self) -> Option<&DiffPane> {
        match &self.right {
            Some(RightPane::Diff(diff)) => Some(diff),
            _ => None,
        }
    }

    pub(super) fn diff_mut(&mut self) -> Option<&mut DiffPane> {
        match &mut self.right {
            Some(RightPane::Diff(diff)) => Some(diff),
            _ => None,
        }
    }

    pub(super) fn search(&self) -> Option<&SearchState> {
        match &self.right {
            Some(RightPane::Search(search)) => Some(search),
            _ => None,
        }
    }

    pub(super) fn search_mut(&mut self) -> Option<&mut SearchState> {
        match &mut self.right {
            Some(RightPane::Search(search)) => Some(search),
            _ => None,
        }
    }

    pub(super) fn is_shell(&self) -> bool {
        matches!(self.right, Some(RightPane::Shell))
    }
}
