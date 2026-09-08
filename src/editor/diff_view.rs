//! The side-by-side diff view: hit-testing its gutter/badges, scrolling the
//! aligned rows together, and jumping between change hunks.

use super::{Focus, GitLineKind, Side, ToastLevel, split_left_width, split_right_width};
use crate::document::Document;
use crate::position::CharIdx;

impl super::Editor {
    /// The aligned rows the diff view is showing, or `None` when the right pane
    /// is not a diff (or either side has no editable text).
    pub(super) fn diff_rows(&self) -> Option<Vec<crate::diff::DiffRow>> {
        let (left, right, _) = self.layout.split().filter(|_| self.layout.is_diff())?;
        let left_text = self.documents.get(&left.view.doc)?.editable_opt()?.text();
        let right_text = self.documents.get(&right.view.doc)?.editable_opt()?.text();
        Some(crate::diff::aligned(
            &crate::diff::rope_lines(left_text),
            &crate::diff::rope_lines(right_text),
        ))
    }

    /// Did this click land on the diff navigator's arrows? `Some(true)` means
    /// next difference.
    pub(super) fn diff_navigator_click(&self, column: u16, row: u16) -> Option<bool> {
        let (current, total) = self.diff_hunk_position()?;
        let pane_x = split_left_width(self.terminal_size.0).saturating_add(1);
        crate::render::diff_navigator_hit(
            pane_x,
            self.terminal_size.0.saturating_sub(pane_x),
            current,
            total,
            column,
            row,
        )
    }

    /// Handle a left-click on a normal editor pane's off-screen-change badge
    /// (the top-right `↑` / bottom-right `↓` indicators summarising diagnostics
    /// and git changes above/below the viewport). Clicking a segment scrolls the
    /// pane to the nearest matching change in that direction, so the badges work
    /// like the diff navigator. Returns `true` when a jump was made.
    pub(super) fn edge_badge_click(&mut self, column: u16, row: u16) -> bool {
        use crate::lsp::DiagnosticSeverity::{Error, Warning};
        use crate::render::EdgeBadgeCategory;

        let total = self.terminal_size.0;
        let divider = split_left_width(total);
        // The split divider belongs to neither pane's badge.
        if self.layout.right.is_some() && column == divider {
            return false;
        }
        let has_right_editor = self.layout.right_editor().is_some();
        let side = if has_right_editor && column > divider {
            Side::Right
        } else {
            Side::Left
        };
        let (pane_x, pane_width) = match side {
            Side::Right => (divider.saturating_add(1), split_right_width(total)),
            Side::Left if self.layout.right.is_some() => (0, divider),
            Side::Left => (0, total),
        };
        let pane_height = self.terminal_size.1.saturating_sub(1);

        let pane = match side {
            Side::Left => &self.layout.left,
            Side::Right => match self.layout.right_editor() {
                Some(pane) => pane,
                None => return false,
            },
        };
        let doc = pane.view.doc;
        let Some(editable) = self.documents.get(&doc).and_then(Document::editable_opt) else {
            return false;
        };
        let text = editable.text();
        let start = pane.view.scroll.top_line;
        let end = (start + usize::from(pane_height) + pane.view.scroll.wrapped_row_offset)
            .min(text.len_lines());
        // A change is off-screen above the viewport when its line sits before the
        // first visible line, and below when at or past the last visible one.
        let off_screen = |line: usize, above: bool| {
            if above { line < start } else { line >= end }
        };
        let diagnostic_line = |diagnostic: &crate::document::ActiveDiagnostic| {
            text.char_to_line(diagnostic.range.start.0.min(text.len_chars()))
        };

        // The counts must match what the renderer drew, or the hit-test lands on
        // segments that aren't there.
        let errors = |above: bool| {
            editable
                .diagnostics
                .iter()
                .filter(|&d| d.severity == Error && off_screen(diagnostic_line(d), above))
                .count()
        };
        let warnings = |above: bool| {
            editable
                .diagnostics
                .iter()
                .filter(|&d| d.severity == Warning && off_screen(diagnostic_line(d), above))
                .count()
        };
        let git = |above: bool, kind: GitLineKind| {
            editable
                .git_lines
                .iter()
                .filter(|&g| g.kind == kind && off_screen(g.line, above))
                .count()
        };
        let hit = crate::render::edge_badge_hit(
            pane_x,
            pane_width,
            pane_height,
            true,
            errors(true),
            warnings(true),
            git(true, GitLineKind::Modified),
            git(true, GitLineKind::Added),
            column,
            row,
        )
        .or_else(|| {
            crate::render::edge_badge_hit(
                pane_x,
                pane_width,
                pane_height,
                false,
                errors(false),
                warnings(false),
                git(false, GitLineKind::Modified),
                git(false, GitLineKind::Added),
                column,
                row,
            )
        });
        let Some(hit) = hit else {
            return false;
        };

        let mut candidates = Vec::new();
        for diagnostic in &editable.diagnostics {
            let wanted = match hit.category {
                EdgeBadgeCategory::Error => diagnostic.severity == Error,
                EdgeBadgeCategory::Warning => diagnostic.severity == Warning,
                EdgeBadgeCategory::Any => matches!(diagnostic.severity, Error | Warning),
                _ => false,
            };
            if wanted {
                candidates.push(diagnostic_line(diagnostic));
            }
        }
        for git_line in &editable.git_lines {
            let wanted = match hit.category {
                EdgeBadgeCategory::Modified => git_line.kind == GitLineKind::Modified,
                EdgeBadgeCategory::Added => git_line.kind == GitLineKind::Added,
                EdgeBadgeCategory::Any => {
                    matches!(git_line.kind, GitLineKind::Modified | GitLineKind::Added)
                }
                _ => false,
            };
            if wanted {
                candidates.push(git_line.line);
            }
        }
        let target = if hit.above {
            candidates.into_iter().filter(|line| *line < start).max()
        } else {
            candidates.into_iter().filter(|line| *line >= end).min()
        };
        let Some(target) = target else {
            return false;
        };
        let head = CharIdx(text.line_to_char(target));

        self.focus = Focus::Editor(side);
        self.record_jump_origin();
        self.go_to_location(doc, head);
        true
    }

    pub(super) fn scroll_diff(&mut self, amount: isize) {
        // The exact row count needs the alignment; the sum of both line counts
        // bounds it, and the renderer clamps the last screenful precisely.
        let max_row = self
            .layout
            .split()
            .map(|(left, right, _)| {
                [left, right]
                    .iter()
                    .map(|pane| {
                        self.documents
                            .get(&pane.view.doc)
                            .and_then(Document::editable_opt)
                            .map_or(0, |editable| editable.text().len_lines())
                    })
                    .sum::<usize>()
            })
            .unwrap_or(0)
            .saturating_sub(1);
        let Some(diff) = self.layout.diff_mut() else {
            return;
        };
        diff.top_row = if amount < 0 {
            diff.top_row.saturating_sub(amount.unsigned_abs())
        } else {
            (diff.top_row + amount as usize).min(max_row)
        };
        self.dirty = true;
    }

    /// Scroll to the next (or previous) run of changed rows.
    pub(super) fn jump_diff_hunk(&mut self, forward: bool) {
        let Some(rows) = self.diff_rows() else {
            return;
        };
        let starts = crate::diff::hunk_starts(&rows);
        let Some(diff) = self.layout.diff_mut() else {
            return;
        };
        let current = diff.top_row;
        let target = if forward {
            starts.iter().find(|start| **start > current)
        } else {
            starts.iter().rev().find(|start| **start < current)
        };
        let Some(target) = target.copied() else {
            self.notify(
                ToastLevel::Info,
                if forward {
                    "最後の差分です"
                } else {
                    "最初の差分です"
                },
            );
            return;
        };
        diff.top_row = target;
        self.dirty = true;
    }

    /// `(current hunk 1-based, total)` for the diff navigator, where "current"
    /// is the last hunk at or above the top of the view.
    pub fn diff_hunk_position(&self) -> Option<(usize, usize)> {
        let rows = self.diff_rows()?;
        let starts = crate::diff::hunk_starts(&rows);
        let top = self.layout.diff()?.top_row;
        let index = starts.iter().filter(|start| **start <= top).count();
        Some((index, starts.len()))
    }

    pub fn diff_top_row(&self) -> usize {
        self.layout.diff().map_or(0, |diff| diff.top_row)
    }
}
