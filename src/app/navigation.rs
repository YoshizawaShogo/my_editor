use crossterm::terminal;

use super::{App, ReplayableAction};

impl App {
    pub(super) fn push_jump_history(&mut self) {
        self.jump_history.push(super::JumpPosition {
            path: self.workspace.current_document_path().map(|path| path.to_path_buf()),
            row: self.cursor.row,
            column: self.cursor.column,
            viewport_row: self.viewport_row,
        });
        self.jump_forward_history.clear();
    }

    pub(super) fn jump_back(&mut self) {
        let Some(previous) = self.jump_history.pop() else {
            return;
        };

        self.jump_forward_history.push(super::JumpPosition {
            path: self.workspace.current_document_path().map(|path| path.to_path_buf()),
            row: self.cursor.row,
            column: self.cursor.column,
            viewport_row: self.viewport_row,
        });

        if let Some(path) = &previous.path {
            if let Some(index) = self.workspace.find_document_index(path) {
                self.make_document_current(index);
            } else if self.open_document(path.clone()).is_err() {
                return;
            }
        }
        self.cursor.row = previous.row;
        self.cursor.column = previous.column;
        self.viewport_row = previous.viewport_row;
        self.clamp_vertical_state();
    }

    pub(super) fn jump_forward(&mut self) {
        let Some(next) = self.jump_forward_history.pop() else {
            return;
        };

        self.jump_history.push(super::JumpPosition {
            path: self.workspace.current_document_path().map(|path| path.to_path_buf()),
            row: self.cursor.row,
            column: self.cursor.column,
            viewport_row: self.viewport_row,
        });

        if let Some(path) = &next.path {
            if let Some(index) = self.workspace.find_document_index(path) {
                self.make_document_current(index);
            } else if self.open_document(path.clone()).is_err() {
                return;
            }
        }
        self.cursor.row = next.row;
        self.cursor.column = next.column;
        self.viewport_row = next.viewport_row;
        self.clamp_vertical_state();
    }

    pub(super) fn jump_to_matching_bracket(&mut self) {
        if let Some((row, column)) = self.workspace.current_document().matching_bracket_position(
            self.cursor.row,
            self.cursor.column,
            self.current_page_width(),
        ) {
            self.push_jump_history();
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    pub(super) fn move_cursor_up(&mut self) {
        self.cursor.row = self.cursor.row.saturating_sub(1);
        self.clamp_vertical_state();
        self.clamp_cursor_column_to_current_line();
    }

    pub(super) fn move_cursor_left(&mut self) {
        self.cursor.column = self.cursor.column.saturating_sub(1);
    }

    pub(super) fn move_cursor_down(&mut self) {
        self.cursor.row = self.cursor.row.saturating_add(1);
        self.clamp_vertical_state();
        self.clamp_cursor_column_to_current_line();
    }

    pub(super) fn move_cursor_right(&mut self) {
        let Ok(line_width) = self
            .workspace
            .current_document()
            .display_line_width(self.cursor.row, self.current_page_width())
        else {
            return;
        };

        self.cursor.column = self.cursor.column.saturating_add(1).min(line_width);
    }

    pub(super) fn move_cursor_to_line_start(&mut self) {
        self.cursor.column = 0;
    }

    pub(super) fn move_cursor_to_line_end(&mut self) {
        let Ok(line_width) = self
            .workspace
            .current_document()
            .display_line_width(self.cursor.row, self.current_page_width())
        else {
            return;
        };

        self.cursor.column = line_width;
    }

    pub(super) fn page_down_half(&mut self) {
        let step = self.page_step() / 2;
        let previous_viewport_row = self.viewport_row;
        self.viewport_row = self.viewport_row.saturating_add(step.max(1));
        self.clamp_to_document_bounds();
        if self.viewport_row > previous_viewport_row {
            self.cursor.row = self.cursor.row.max(self.viewport_row);
        }
    }

    pub(super) fn page_down_full(&mut self) {
        let step = self.page_step();
        let previous_viewport_row = self.viewport_row;
        self.viewport_row = self.viewport_row.saturating_add(step.max(1));
        self.clamp_to_document_bounds();
        if self.viewport_row > previous_viewport_row {
            self.cursor.row = self.cursor.row.max(self.viewport_row);
        }
    }

    pub(super) fn page_up_half(&mut self) {
        let step = self.page_step() / 2;
        let previous_viewport_row = self.viewport_row;
        self.viewport_row = self.viewport_row.saturating_sub(step.max(1));
        self.clamp_to_document_bounds();
        if self.viewport_row < previous_viewport_row {
            self.cursor.row = self
                .cursor
                .row
                .min(self.viewport_row.saturating_add(self.page_step().saturating_sub(1)));
        }
    }

    pub(super) fn page_up_full(&mut self) {
        let step = self.page_step();
        let previous_viewport_row = self.viewport_row;
        self.viewport_row = self.viewport_row.saturating_sub(step.max(1));
        self.clamp_to_document_bounds();
        if self.viewport_row < previous_viewport_row {
            self.cursor.row = self
                .cursor
                .row
                .min(self.viewport_row.saturating_add(self.page_step().saturating_sub(1)));
        }
    }

    pub(super) fn page_step(&self) -> usize {
        terminal::size()
            .map(|(_, height)| height.saturating_sub(1) as usize)
            .unwrap_or(24)
            .max(1)
    }

    pub(super) fn sync_viewport_after_cursor_move(&mut self) {
        let visible_height = self.page_step();

        if self.cursor.row < self.viewport_row {
            self.viewport_row = self.cursor.row;
        } else if self.cursor.row >= self.viewport_row.saturating_add(visible_height) {
            self.viewport_row = self
                .cursor
                .row
                .saturating_sub(visible_height.saturating_sub(1));
        }
    }

    pub(super) fn clamp_vertical_state(&mut self) {
        self.clamp_to_document_bounds();
        self.sync_viewport_after_cursor_move();
    }

    pub(super) fn clamp_cursor_column_to_current_line(&mut self) {
        let Ok(line_width) = self
            .workspace
            .current_document()
            .display_line_width(self.cursor.row, self.current_page_width())
        else {
            return;
        };

        self.cursor.column = self.cursor.column.min(line_width);
    }

    pub(super) fn clamp_to_document_bounds(&mut self) {
        if let Some(total_rows) = self
            .workspace
            .current_document()
            .total_rows(self.current_page_width())
        {
            let visible_height = self.page_step();
            let last_row = total_rows.saturating_sub(1);
            let max_viewport_row = total_rows.saturating_sub(visible_height);

            self.cursor.row = self.cursor.row.min(last_row);
            self.viewport_row = self.viewport_row.min(max_viewport_row);
        }
    }

    pub(super) fn jump_to_top(&mut self) {
        self.push_jump_history();
        self.workspace.current_document_mut().jump_to_top();
        self.viewport_row = 0;
        self.cursor.row = 0;
    }

    pub(super) fn jump_to_bottom(&mut self) {
        let visible_height = self.page_step();
        let page_width = self.current_page_width();
        self.push_jump_history();
        if let Ok(Some(start_row)) = self
            .workspace
            .current_document_mut()
            .jump_to_bottom(visible_height, page_width)
        {
            self.viewport_row = start_row;
            self.cursor.row = start_row.saturating_add(visible_height.saturating_sub(1));
            return;
        }

        let Some(total_rows) = self
            .workspace
            .current_document()
            .total_rows(page_width)
        else {
            return;
        };

        self.cursor.row = total_rows.saturating_sub(1);
        self.viewport_row = total_rows.saturating_sub(visible_height);
    }

    pub(super) fn jump_to_next_git_marker(&mut self) {
        if let Some(row) = self
            .workspace
            .current_document()
            .next_git_marker_row(self.cursor.row, self.current_page_width())
        {
            self.push_jump_history();
            self.jump_with_context(row, self.current_page_width());
        }
    }

    pub(super) fn jump_to_previous_git_marker(&mut self) {
        if let Some(row) = self
            .workspace
            .current_document()
            .previous_git_marker_row(self.cursor.row, self.current_page_width())
        {
            self.push_jump_history();
            self.jump_with_context(row, self.current_page_width());
        }
    }

    pub(super) fn jump_to_next_diagnostic(&mut self, error_only: bool) {
        if let Some(row) = self
            .workspace
            .current_document()
            .next_diagnostic_row(self.cursor.row, self.current_page_width(), error_only)
        {
            self.push_jump_history();
            self.jump_with_context(row, self.current_page_width());
            self.last_replayable_action = Some(ReplayableAction::Diagnostic {
                error_only,
                forward: true,
            });
        }
    }

    pub(super) fn jump_to_previous_diagnostic(&mut self, error_only: bool) {
        if let Some(row) = self
            .workspace
            .current_document()
            .previous_diagnostic_row(self.cursor.row, self.current_page_width(), error_only)
        {
            self.push_jump_history();
            self.jump_with_context(row, self.current_page_width());
            self.last_replayable_action = Some(ReplayableAction::Diagnostic {
                error_only,
                forward: false,
            });
        }
    }

    pub(super) fn jump_with_context(&mut self, target_row: usize, page_width: usize) {
        let visible_height = self.page_step();

        self.cursor.row = target_row;
        self.viewport_row = target_row.saturating_sub(1);

        if let Some(total_rows) = self.workspace.current_document().total_rows(page_width) {
            self.cursor.row = self.cursor.row.min(total_rows.saturating_sub(1));
            self.viewport_row = self
                .viewport_row
                .min(total_rows.saturating_sub(visible_height));
        }

        self.clamp_to_document_bounds();
        self.clamp_cursor_column_to_current_line();
    }
}
