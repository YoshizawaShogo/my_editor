use crossterm::terminal;

use super::{App, ReplayableAction};

impl App {
    /// 現在のカーソル位置をJumpPositionとして構築して返す
    fn current_jump_position(&self) -> super::JumpPosition {
        super::JumpPosition {
            path: self
                .workspace
                .current_document_path()
                .map(|path| path.to_path_buf()),
            row: self.cursor.row,
            column: self.cursor.column,
            viewport_row: self.viewport_row,
        }
    }

    /// 現在位置をジャンプ履歴に積み、前進履歴をクリアする
    pub(super) fn push_jump_history(&mut self) {
        self.jump_history.push(self.current_jump_position());
        self.jump_forward_history.clear();
    }

    /// ジャンプ履歴を一つ前に戻り、現在位置を前進履歴に積む
    pub(super) fn jump_back(&mut self) {
        let Some(previous) = self.jump_history.pop() else {
            return;
        };

        self.jump_forward_history.push(self.current_jump_position());
        self.navigate_to_jump_position(previous);
    }

    /// ジャンプ前進履歴を一つ進め、現在位置をジャンプ履歴に積む
    pub(super) fn jump_forward(&mut self) {
        let Some(next) = self.jump_forward_history.pop() else {
            return;
        };

        self.jump_history.push(self.current_jump_position());
        self.navigate_to_jump_position(next);
    }

    /// 保存済みジャンプ位置のドキュメントを開き、カーソルを移動する
    fn navigate_to_jump_position(&mut self, position: super::JumpPosition) {
        if let Some(path) = &position.path {
            if let Some(index) = self.workspace.find_document_index(path) {
                self.make_document_current(index);
            } else if self.open_document(path.clone()).is_err() {
                return;
            }
        }
        self.cursor.row = position.row;
        self.cursor.column = position.column;
        self.viewport_row = position.viewport_row;
        self.clamp_vertical_state();
    }

    /// 対応するブラケットの位置にカーソルをジャンプする
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

    /// カーソルを1行上に移動し、列をクランプする
    pub(super) fn move_cursor_up(&mut self) {
        self.cursor.row = self.cursor.row.saturating_sub(1);
        self.clamp_vertical_state();
        self.clamp_cursor_column_to_current_line();
    }

    /// カーソルを1文字左に移動する
    pub(super) fn move_cursor_left(&mut self) {
        self.cursor.column = self.cursor.column.saturating_sub(1);
    }

    /// カーソルを1行下に移動し、列をクランプする
    pub(super) fn move_cursor_down(&mut self) {
        self.cursor.row = self.cursor.row.saturating_add(1);
        self.clamp_vertical_state();
        self.clamp_cursor_column_to_current_line();
    }

    /// カーソルを1文字右に移動する（行末を超えない）
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

    /// カーソルを行頭に移動する
    pub(super) fn move_cursor_to_line_start(&mut self) {
        self.cursor.column = 0;
    }

    /// カーソルを行末に移動する
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

    /// 半ページ分だけ下にスクロールする
    pub(super) fn page_down_half(&mut self) {
        self.scroll_down(self.page_step() / 2);
    }

    /// 半ページ分だけ上にスクロールする
    pub(super) fn page_up_half(&mut self) {
        self.scroll_up(self.page_step() / 2);
    }

    /// ビューポートを指定ステップ数だけ下にスクロールし、カーソルを追随させる
    fn scroll_down(&mut self, step: usize) {
        let previous_viewport_row = self.viewport_row;
        self.viewport_row = self.viewport_row.saturating_add(step.max(1));
        self.clamp_to_document_bounds();
        if self.viewport_row > previous_viewport_row {
            self.cursor.row = self.cursor.row.max(self.viewport_row);
        }
    }

    /// ビューポートを指定ステップ数だけ上にスクロールし、カーソルを追随させる
    fn scroll_up(&mut self, step: usize) {
        let previous_viewport_row = self.viewport_row;
        self.viewport_row = self.viewport_row.saturating_sub(step.max(1));
        self.clamp_to_document_bounds();
        if self.viewport_row < previous_viewport_row {
            self.cursor.row = self.cursor.row.min(
                self.viewport_row
                    .saturating_add(self.page_step().saturating_sub(1)),
            );
        }
    }

    /// ターミナルの高さから1ページあたりの行数を返す
    pub(super) fn page_step(&self) -> usize {
        terminal::size()
            .map(|(_, height)| height.saturating_sub(1) as usize)
            .unwrap_or(24)
            .max(1)
    }

    /// カーソル行がビューポート内に収まるようビューポートを調整する
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

    /// カーソルとビューポートをドキュメント範囲内に収め、ビューポートを同期する
    pub(super) fn clamp_vertical_state(&mut self) {
        self.clamp_to_document_bounds();
        self.sync_viewport_after_cursor_move();
    }

    /// カーソルの列を現在行の幅内に収める
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

    /// カーソル行とビューポート行をドキュメントの行数範囲内に制限する
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

    /// ドキュメント先頭にジャンプする
    pub(super) fn jump_to_top(&mut self) {
        self.push_jump_history();
        self.workspace.current_document_mut().jump_to_top();
        self.viewport_row = 0;
        self.cursor.row = 0;
    }

    /// ドキュメント末尾にジャンプする
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

        let Some(total_rows) = self.workspace.current_document().total_rows(page_width) else {
            return;
        };

        self.cursor.row = total_rows.saturating_sub(1);
        self.viewport_row = total_rows.saturating_sub(visible_height);
    }

    /// 次のgit変更マーカー行にジャンプする
    pub(super) fn jump_to_next_git_marker(&mut self) {
        self.jump_to_git_marker(true);
    }

    /// 前のgit変更マーカー行にジャンプする
    pub(super) fn jump_to_previous_git_marker(&mut self) {
        self.jump_to_git_marker(false);
    }

    /// 指定方向のgit変更マーカー行にジャンプする
    fn jump_to_git_marker(&mut self, forward: bool) {
        let page_width = self.current_page_width();
        let row = if forward {
            self.workspace
                .current_document()
                .next_git_marker_row(self.cursor.row, page_width)
        } else {
            self.workspace
                .current_document()
                .previous_git_marker_row(self.cursor.row, page_width)
        };
        if let Some(row) = row {
            self.push_jump_history();
            self.jump_with_context(row, page_width);
        }
    }

    /// 次の診断マーカー行にジャンプし、リプレイ可能アクションとして記録する
    pub(super) fn jump_to_next_diagnostic(&mut self, error_only: bool) {
        self.jump_to_diagnostic(error_only, true);
    }

    /// 前の診断マーカー行にジャンプし、リプレイ可能アクションとして記録する
    pub(super) fn jump_to_previous_diagnostic(&mut self, error_only: bool) {
        self.jump_to_diagnostic(error_only, false);
    }

    /// 指定方向の診断マーカー行にジャンプし、リプレイ可能アクションとして記録する
    fn jump_to_diagnostic(&mut self, error_only: bool, forward: bool) {
        let page_width = self.current_page_width();
        let row = if forward {
            self.workspace.current_document().next_diagnostic_row(
                self.cursor.row,
                page_width,
                error_only,
            )
        } else {
            self.workspace.current_document().previous_diagnostic_row(
                self.cursor.row,
                page_width,
                error_only,
            )
        };
        if let Some(row) = row {
            self.push_jump_history();
            self.jump_with_context(row, page_width);
            self.last_replayable_action = Some(ReplayableAction::Diagnostic {
                error_only,
                forward,
            });
        }
    }

    /// 指定行にジャンプし、ビューポートをその行の周辺に配置する
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
