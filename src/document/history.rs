use std::ops::Range;
use std::time::{Duration, Instant};

use crate::{position::CharIdx, view::Selections};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Change {
    pub range: Range<CharIdx>,
    pub removed: String,
    pub inserted: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Revision {
    pub changes: Vec<Change>,
    pub selections_before: Selections,
    pub selections_after: Selections,
}

#[derive(Debug, Default)]
pub struct History {
    past: Vec<Revision>,
    future: Vec<Revision>,
    last_insert: Option<Instant>,
}

impl History {
    pub fn record(&mut self, revision: Revision, insert_at: Option<Instant>) {
        if let Some(at) = insert_at
            && self.last_insert.is_some_and(|last| {
                at.saturating_duration_since(last) <= Duration::from_millis(750)
            })
            && let Some(previous) = self.past.last_mut()
        {
            previous.changes.extend(revision.changes);
            previous.selections_after = revision.selections_after;
            self.future.clear();
            self.last_insert = Some(at);
            return;
        }
        self.past.push(revision);
        self.future.clear();
        self.last_insert = insert_at;
    }

    pub fn take_undo(&mut self) -> Option<Revision> {
        self.last_insert = None;
        self.past.pop()
    }

    pub fn finish_undo(&mut self, revision: Revision) {
        self.future.push(revision);
    }

    pub fn take_redo(&mut self) -> Option<Revision> {
        self.future.pop()
    }

    pub fn finish_redo(&mut self, revision: Revision) {
        self.past.push(revision);
    }

    pub fn break_group(&mut self) {
        self.last_insert = None;
    }
}
