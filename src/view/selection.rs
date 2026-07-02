use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::position::CharIdx;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Selection {
    pub anchor: CharIdx,
    pub head: CharIdx,
}

impl Selection {
    pub fn caret(index: CharIdx) -> Self {
        Self {
            anchor: index,
            head: index,
        }
    }

    pub fn range(self) -> Range<usize> {
        self.anchor.0.min(self.head.0)..self.anchor.0.max(self.head.0)
    }

    pub fn is_caret(self) -> bool {
        self.anchor == self.head
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Selections {
    ranges: Vec<Selection>,
    primary: usize,
}

impl Default for Selections {
    fn default() -> Self {
        Self {
            ranges: vec![Selection::default()],
            primary: 0,
        }
    }
}

impl Selections {
    pub fn single(selection: Selection) -> Self {
        Self {
            ranges: vec![selection],
            primary: 0,
        }
    }

    pub fn primary(&self) -> Selection {
        self.ranges[self.primary]
    }

    pub fn primary_index(&self) -> usize {
        self.primary
    }

    pub fn iter(&self) -> impl Iterator<Item = &Selection> {
        self.ranges.iter()
    }

    pub fn from_vec(ranges: Vec<Selection>, primary: usize) -> Self {
        assert!(!ranges.is_empty(), "Selections must not be empty");
        assert!(primary < ranges.len(), "primary selection must exist");
        Self { ranges, primary }
    }

    pub fn set_single(&mut self, selection: Selection) {
        self.ranges.clear();
        self.ranges.push(selection);
        self.primary = 0;
    }

    pub fn replace_all(&mut self, ranges: Vec<Selection>) {
        assert_eq!(ranges.len(), self.ranges.len());
        self.ranges = ranges;
        self.normalize();
    }

    pub fn add(&mut self, selection: Selection, make_primary: bool) {
        self.ranges.push(selection);
        if make_primary {
            self.primary = self.ranges.len() - 1;
        }
        self.normalize();
    }

    pub fn collapse_to_primary(&mut self) {
        self.set_single(self.primary());
    }

    pub fn normalize(&mut self) {
        let primary_selection = self.primary();
        self.ranges.sort_by_key(|selection| {
            let range = selection.range();
            (range.start, range.end)
        });

        let mut merged: Vec<Selection> = Vec::with_capacity(self.ranges.len());
        for selection in self.ranges.drain(..) {
            let current = selection.range();
            if let Some(previous) = merged.last_mut() {
                let previous_range = previous.range();
                let overlaps = current.start < previous_range.end
                    || (current.start == previous_range.start && current.end == previous_range.end);
                if overlaps {
                    previous.anchor = CharIdx(previous_range.start.min(current.start));
                    previous.head = CharIdx(previous_range.end.max(current.end));
                    continue;
                }
            }
            merged.push(selection);
        }
        self.ranges = merged;
        self.primary = self
            .ranges
            .iter()
            .position(|selection| {
                let range = selection.range();
                let primary = primary_selection.range();
                range.start <= primary.start && range.end >= primary.end
            })
            .unwrap_or(0);
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_range_is_ordered() {
        let selection = Selection {
            anchor: CharIdx(8),
            head: CharIdx(3),
        };

        assert_eq!(selection.range(), 3..8);
    }

    #[test]
    fn selections_are_never_empty() {
        let selections = Selections::default();

        assert_eq!(selections.len(), 1);
        assert!(!selections.is_empty());
    }

    #[test]
    fn overlapping_selections_are_merged() {
        let mut selections = Selections::from_vec(
            vec![
                Selection {
                    anchor: CharIdx(1),
                    head: CharIdx(4),
                },
                Selection {
                    anchor: CharIdx(3),
                    head: CharIdx(6),
                },
            ],
            1,
        );

        selections.normalize();

        assert_eq!(selections.len(), 1);
        assert_eq!(selections.primary().range(), 1..6);
    }

    #[test]
    fn duplicate_carets_are_merged() {
        let mut selections = Selections::default();

        selections.add(Selection::caret(CharIdx(0)), true);

        assert_eq!(selections.len(), 1);
    }
}
