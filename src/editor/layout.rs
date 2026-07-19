use crate::view::View;

use super::{Focus, Side};

#[derive(Debug)]
pub struct EditorPane {
    pub view: View,
}

#[derive(Debug)]
pub enum Layout {
    EditorFull(EditorPane),
    EditorAndShell {
        editor: EditorPane,
    },
    EditorAndEditor {
        left: EditorPane,
        right: EditorPane,
        diff: bool,
    },
}

impl Layout {
    pub fn active_editor(&self, focus: Focus) -> Option<&EditorPane> {
        match (self, focus) {
            (Self::EditorFull(pane), Focus::Editor(Side::Left) | Focus::Completion(Side::Left)) => {
                Some(pane)
            }
            (Self::EditorFull(pane), Focus::Overlay) => Some(pane),
            (
                Self::EditorAndShell { editor },
                Focus::Editor(_) | Focus::Completion(_) | Focus::Overlay,
            ) => Some(editor),
            (
                Self::EditorAndEditor { left, .. },
                Focus::Editor(Side::Left) | Focus::Completion(Side::Left) | Focus::Overlay,
            ) => Some(left),
            (
                Self::EditorAndEditor { right, .. },
                Focus::Editor(Side::Right) | Focus::Completion(Side::Right),
            ) => Some(right),
            _ => None,
        }
    }

    pub fn active_editor_mut(&mut self, focus: Focus) -> Option<&mut EditorPane> {
        match (self, focus) {
            (Self::EditorFull(pane), Focus::Editor(Side::Left) | Focus::Completion(Side::Left)) => {
                Some(pane)
            }
            (Self::EditorFull(pane), Focus::Overlay) => Some(pane),
            (
                Self::EditorAndShell { editor },
                Focus::Editor(_) | Focus::Completion(_) | Focus::Overlay,
            ) => Some(editor),
            (
                Self::EditorAndEditor { left, .. },
                Focus::Editor(Side::Left) | Focus::Completion(Side::Left) | Focus::Overlay,
            ) => Some(left),
            (
                Self::EditorAndEditor { right, .. },
                Focus::Editor(Side::Right) | Focus::Completion(Side::Right),
            ) => Some(right),
            _ => None,
        }
    }

    pub fn panes_mut(&mut self) -> Vec<&mut EditorPane> {
        match self {
            Self::EditorFull(pane) => vec![pane],
            Self::EditorAndShell { editor } => vec![editor],
            Self::EditorAndEditor { left, right, .. } => vec![left, right],
        }
    }

    pub fn split(&self) -> Option<(&EditorPane, &EditorPane, bool)> {
        match self {
            Self::EditorAndEditor { left, right, diff } => Some((left, right, *diff)),
            Self::EditorFull(_) | Self::EditorAndShell { .. } => None,
        }
    }
}
