mod movement;
mod selection;

pub use movement::{is_word, move_head};
pub use selection::{Selection, Selections};

use crate::document::DocumentId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View {
    pub doc: DocumentId,
    pub selections: Selections,
    pub scroll: Scroll,
}

impl View {
    pub fn new(doc: DocumentId) -> Self {
        Self {
            doc,
            selections: Selections::default(),
            scroll: Scroll::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Scroll {
    pub top_line: usize,
    pub left_col: usize,
}
