use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::{error::Result, mode::Mode};

use super::{
    App,
    action::{PendingNormalAction, ReplayableAction},
    lsp::GotoKind,
};

impl App {
    /// crossterm Eventを受け取りキーイベントのみを処理する
    pub(super) fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Key(key_event) => self.handle_key_event(key_event),
            Event::Mouse(_) => Ok(false),
            _ => Ok(false),
        }
    }

    /// アクティブなUIとモードに応じてキーイベントを適切なハンドラに振り分ける
    fn handle_key_event(&mut self, key_event: KeyEvent) -> Result<bool> {
        // Popups and special modes take priority
        if self.go_input.active {
            return self.handle_go_input_key(key_event);
        }
        if self.rename_input.active {
            return self.handle_rename_input_key(key_event);
        }
        if self.hover_popup.active {
            return self.handle_hover_popup_key(key_event);
        }
        if self.selection_input.active {
            return self.handle_selection_input_key(key_event);
        }
        if self.diagnostic_popup.active {
            return self.handle_diagnostic_popup_key(key_event);
        }
        if self.search_input.active {
            return self.handle_search_input_key(key_event);
        }
        if self.replace_input.active {
            return self.handle_replace_input_key(key_event);
        }
        if self.picker.active {
            return self.handle_picker_key(key_event);
        }
        if self.is_terminal_pane_focused() {
            return self.handle_shell_mode_key(key_event);
        }

        match self.mode {
            Mode::Normal | Mode::Insert => self.handle_edit_mode_key(key_event),
            Mode::Shell => self.handle_shell_mode_key(key_event),
        }
    }

    fn handle_edit_mode_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        let ctrl = key_event.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key_event.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key_event.modifiers.contains(KeyModifiers::ALT);

        // Undo group management: group consecutive plain character inputs into one undo step
        if self.workspace.has_documents() {
            let is_plain_char = !ctrl && !alt && matches!(key_event.code, KeyCode::Char(_));
            if is_plain_char {
                if !self.workspace.current_document().is_in_undo_group() {
                    self.workspace.current_document_mut().begin_undo_group();
                }
            } else {
                self.workspace.current_document_mut().end_undo_group();
            }
        }

        // Handle c-j prefix state
        if let Some(PendingNormalAction::JumpPrefix) = self.pending_normal_action {
            self.pending_normal_action = None;
            return self.handle_jump_prefix_key(key_event);
        }

        // Alt+H = toggle hints (global)
        if alt && !ctrl && matches!(key_event.code, KeyCode::Char('h')) {
            self.silent = !self.silent;
            return Ok(false);
        }

        // Ctrl+Shift combos
        if ctrl && shift {
            match key_event.code {
                KeyCode::Char('w') | KeyCode::Char('W') => return Ok(true), // Quit
                KeyCode::Enter => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.open_line_above();
                    }
                    return Ok(false);
                }
                KeyCode::Left => {
                    if self.workspace.has_documents() {
                        self.extend_selection_word_left();
                    }
                    return Ok(false);
                }
                KeyCode::Right => {
                    if self.workspace.has_documents() {
                        self.extend_selection_word_right();
                    }
                    return Ok(false);
                }
                _ => return Ok(false),
            }
        }

        // Ctrl+Alt combos
        if ctrl && alt {
            match key_event.code {
                KeyCode::Up => {
                    if self.workspace.has_documents() {
                        self.add_extra_cursor_above();
                    }
                    return Ok(false);
                }
                KeyCode::Down => {
                    if self.workspace.has_documents() {
                        self.add_extra_cursor_below();
                    }
                    return Ok(false);
                }
                _ => return Ok(false),
            }
        }

        // Alt combos (no ctrl)
        if alt && !ctrl {
            match key_event.code {
                KeyCode::Left => {
                    self.clear_selection();
                    self.jump_back();
                    return Ok(false);
                }
                KeyCode::Right => {
                    self.clear_selection();
                    self.jump_forward();
                    return Ok(false);
                }
                KeyCode::Char('k') | KeyCode::Char('K') => {
                    if self.workspace.has_documents() {
                        self.open_hover_popup()?;
                    }
                    return Ok(false);
                }
                _ => return Ok(false),
            }
        }

        // Ctrl combos (no shift, no alt)
        if ctrl && !shift && !alt {
            match key_event.code {
                KeyCode::Char('z') => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.extra_cursors.clear();
                        self.undo_current_document();
                    }
                    return Ok(false);
                }
                KeyCode::Char('y') => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.extra_cursors.clear();
                        self.redo_current_document();
                    }
                    return Ok(false);
                }
                KeyCode::Char('c') => {
                    if self.workspace.has_documents() {
                        self.copy_selection_or_line()?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('x') => {
                    if self.workspace.has_documents() {
                        self.cut_selection_or_line()?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('v') => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.extra_cursors.clear();
                        self.paste_after_cursor()?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('a') => {
                    if self.workspace.has_documents() {
                        self.select_all();
                    }
                    return Ok(false);
                }
                KeyCode::Char('d') => {
                    if self.workspace.has_documents() {
                        self.select_word_or_next_occurrence()?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('q') => {
                    if self.workspace.has_documents() {
                        self.toggle_comment()?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('s') => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.save_current_document()?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('w') => {
                    self.close_current_buffer();
                    return Ok(false);
                }
                KeyCode::Char('f') => {
                    self.open_or_cycle_search_input();
                    return Ok(false);
                }
                KeyCode::Char('h') => {
                    self.open_or_cycle_replace_input();
                    return Ok(false);
                }
                KeyCode::Char('j') => {
                    if self.workspace.has_documents() {
                        self.pending_normal_action = Some(PendingNormalAction::JumpPrefix);
                    }
                    return Ok(false);
                }
                KeyCode::Char('n') => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.replay_last_action(false)?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('p') => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.replay_last_action(true)?;
                    }
                    return Ok(false);
                }
                KeyCode::Char('t') => {
                    self.open_or_cycle_picker()?;
                    return Ok(false);
                }
                KeyCode::Char('g') => {
                    self.open_go_input();
                    return Ok(false);
                }
                KeyCode::Char('l') => {
                    self.advance_layout_or_focus();
                    return Ok(false);
                }
                KeyCode::Char('o') => {
                    self.collapse_to_single_pane();
                    return Ok(false);
                }
                KeyCode::Null | KeyCode::Char(' ') => {
                    self.toggle_terminal_split()?;
                    return Ok(false);
                }
                KeyCode::Enter => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.extra_cursors.clear();
                        self.open_line_below();
                    }
                    return Ok(false);
                }
                KeyCode::Char('m') => {
                    // c-m = enter (legacy support)
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.insert_newline();
                    }
                    return Ok(false);
                }
                KeyCode::Left => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.close_completion();
                        self.move_cursor_word_left();
                    }
                    return Ok(false);
                }
                KeyCode::Right => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.close_completion();
                        self.move_cursor_word_right();
                    }
                    return Ok(false);
                }
                KeyCode::Up => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.page_up_half();
                    }
                    return Ok(false);
                }
                KeyCode::Down => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.page_down_half();
                    }
                    return Ok(false);
                }
                KeyCode::Home => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.jump_to_top();
                    }
                    return Ok(false);
                }
                KeyCode::End => {
                    if self.workspace.has_documents() {
                        self.clear_selection();
                        self.jump_to_bottom();
                    }
                    return Ok(false);
                }
                _ => return Ok(false),
            }
        }

        // Non-modifier keys (possibly with Shift)
        match key_event.code {
            KeyCode::Esc => {
                self.close_completion();
                self.clear_selection();
                self.extra_cursors.clear();
                self.pending_normal_action = None;
                if self.workspace.has_documents() && self.workspace.current_document().is_scratch()
                {
                    self.close_current_buffer();
                }
                Ok(false)
            }

            KeyCode::F(2) => {
                if self.workspace.has_documents() {
                    self.open_rename_input();
                }
                Ok(false)
            }

            KeyCode::F(4) => Ok(true), // Quit

            KeyCode::F(8) => {
                self.wrap = !self.wrap;
                Ok(false)
            }

            KeyCode::Up => {
                self.close_completion();
                if self.workspace.has_documents() {
                    if shift {
                        self.extend_selection_up();
                    } else {
                        self.clear_selection();
                        self.move_cursor_up();
                    }
                }
                Ok(false)
            }
            KeyCode::Down => {
                self.close_completion();
                if self.workspace.has_documents() {
                    if shift {
                        self.extend_selection_down();
                    } else {
                        self.clear_selection();
                        self.move_cursor_down();
                    }
                }
                Ok(false)
            }
            KeyCode::Left => {
                self.close_completion();
                if self.workspace.has_documents() {
                    if shift {
                        self.extend_selection_left();
                    } else {
                        self.clear_selection();
                        self.move_cursor_left();
                    }
                }
                Ok(false)
            }
            KeyCode::Right => {
                self.close_completion();
                if self.workspace.has_documents() {
                    if shift {
                        self.extend_selection_right();
                    } else {
                        self.clear_selection();
                        self.move_cursor_right();
                    }
                }
                Ok(false)
            }
            KeyCode::Home => {
                self.close_completion();
                if self.workspace.has_documents() {
                    if shift {
                        self.extend_selection_to_line_start();
                    } else {
                        self.clear_selection();
                        self.move_cursor_to_line_start();
                    }
                }
                Ok(false)
            }
            KeyCode::End => {
                self.close_completion();
                if self.workspace.has_documents() {
                    if shift {
                        self.extend_selection_to_line_end();
                    } else {
                        self.clear_selection();
                        self.move_cursor_to_line_end();
                    }
                }
                Ok(false)
            }

            KeyCode::Enter => {
                if self.workspace.has_documents() {
                    self.delete_selection_if_any()?;
                    self.insert_newline_for_all_cursors();
                }
                Ok(false)
            }

            KeyCode::Tab => {
                if !self.submit_completion() {
                    self.close_completion();
                    if self.workspace.has_documents() {
                        self.delete_selection_if_any()?;
                        self.insert_tab();
                    }
                }
                Ok(false)
            }

            KeyCode::BackTab => {
                // Shift+Tab = close completion for now
                self.close_completion();
                Ok(false)
            }

            KeyCode::Backspace => {
                if self.workspace.has_documents() {
                    if self.selection_anchor.is_some() {
                        self.delete_selection()?;
                    } else {
                        self.backspace_char_for_all_cursors();
                    }
                }
                Ok(false)
            }

            KeyCode::Delete => {
                if self.workspace.has_documents() {
                    if self.selection_anchor.is_some() {
                        self.delete_selection()?;
                    } else {
                        self.delete_forward_char();
                    }
                }
                Ok(false)
            }

            KeyCode::Char(ch) => {
                if self.workspace.has_documents() {
                    self.delete_selection_if_any()?;
                    self.insert_char_for_all_cursors(ch);
                }
                Ok(false)
            }

            _ => Ok(false),
        }
    }

    fn handle_jump_prefix_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        match key_event.code {
            KeyCode::Char('d') => {
                self.goto_symbol(GotoKind::Definition)?;
            }
            KeyCode::Char('i') => {
                self.goto_symbol(GotoKind::Implementation)?;
            }
            KeyCode::Char('r') => {
                self.show_references()?;
            }
            KeyCode::Char('D') => {
                self.goto_symbol(GotoKind::Declaration)?;
            }
            KeyCode::Char('e') => {
                self.open_current_diagnostic_popup();
            }
            KeyCode::Char('w') => {
                self.jump_to_next_diagnostic(false);
            }
            KeyCode::Char('W') => {
                self.jump_to_previous_diagnostic(false);
            }
            KeyCode::Char('n') => {
                self.jump_to_next_diagnostic(true);
            }
            KeyCode::Char('N') => {
                self.jump_to_previous_diagnostic(true);
            }
            KeyCode::Char('g') => {
                self.jump_to_next_git_marker();
                self.last_replayable_action = Some(ReplayableAction::GitHunk { forward: true });
            }
            KeyCode::Char('G') => {
                self.jump_to_previous_git_marker();
                self.last_replayable_action = Some(ReplayableAction::GitHunk { forward: false });
            }
            KeyCode::Char('t') => {
                self.jump_to_top();
            }
            KeyCode::Char('b') => {
                self.jump_to_bottom();
            }
            KeyCode::Char('f') => {
                self.repeat_search_forward()?;
            }
            KeyCode::Char('F') => {
                self.repeat_search_backward()?;
            }
            KeyCode::Esc => {} // cancel
            _ => {}            // ignore unknown
        }
        Ok(false)
    }

    /// 診断ポップアップ表示中のキー入力を処理する
    fn handle_diagnostic_popup_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_diagnostic_popup();
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close_diagnostic_popup();
                Ok(false)
            }
            KeyCode::Char('w') if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_diagnostic_list(false);
                Ok(false)
            }
            KeyCode::Char('e') if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_diagnostic_list(true);
                Ok(false)
            }
            KeyCode::Char('W') if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.request_workspace_diagnostic_list(false)?;
                Ok(false)
            }
            KeyCode::Char('E') if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.request_workspace_diagnostic_list(true)?;
                Ok(false)
            }
            _ => {
                self.close_diagnostic_popup();
                Ok(false)
            }
        }
    }

    /// 行番号入力中のキー入力を処理する
    fn handle_go_input_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_go_input();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('j'))
        {
            self.submit_go_input()?;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('m'))
        {
            self.submit_go_input()?;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('h'))
        {
            self.go_input.value.pop();
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close_go_input();
                Ok(false)
            }
            KeyCode::Enter => {
                self.submit_go_input()?;
                Ok(false)
            }
            KeyCode::Backspace => {
                self.go_input.value.pop();
                Ok(false)
            }
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                self.go_input.value.push(ch);
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// 検索入力中のキー入力を処理する
    fn handle_search_input_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_search_input();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('f'))
        {
            self.cycle_search_scope();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('j'))
        {
            self.submit_search_input()?;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('m'))
        {
            self.submit_search_input()?;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('h'))
        {
            self.search_input.value.pop();
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close_search_input();
                Ok(false)
            }
            KeyCode::Enter => {
                self.submit_search_input()?;
                Ok(false)
            }
            KeyCode::Backspace => {
                self.search_input.value.pop();
                Ok(false)
            }
            KeyCode::Char(ch) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_input.value.push(ch);
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// 置換入力中のキー入力を処理する
    fn handle_replace_input_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_replace_input();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('h'))
        {
            self.cycle_replace_scope();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('j') | KeyCode::Char('m'))
        {
            self.submit_replace_input()?;
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close_replace_input();
                Ok(false)
            }
            KeyCode::Enter => {
                self.submit_replace_input()?;
                Ok(false)
            }
            KeyCode::Tab => {
                self.switch_replace_field();
                Ok(false)
            }
            KeyCode::Backspace => {
                self.pop_replace_char();
                Ok(false)
            }
            KeyCode::Char(ch) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.append_replace_char(ch);
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// ピッカー表示中のキー入力を処理する
    fn handle_picker_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_picker();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('t'))
        {
            self.close_picker();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('j'))
        {
            self.submit_picker_selection()?;
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close_picker();
                Ok(false)
            }
            KeyCode::Backspace => {
                self.picker.query.pop();
                Ok(false)
            }
            KeyCode::Enter => {
                self.submit_picker_selection()?;
                Ok(false)
            }
            KeyCode::Char('w') if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.close_picker();
                Ok(false)
            }
            KeyCode::Char(ch) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.picker.query.push(ch);
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// ホバーポップアップ表示中のキー入力を処理する
    fn handle_hover_popup_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_hover_popup();
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close_hover_popup();
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// 選択範囲入力中のキー入力を処理する
    fn handle_selection_input_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_selection_input();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('j') | KeyCode::Char('m'))
        {
            self.submit_selection_input()?;
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close_selection_input();
                Ok(false)
            }
            KeyCode::Enter => {
                self.submit_selection_input()?;
                Ok(false)
            }
            KeyCode::Char('i') if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.expand_selection_input();
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// リネーム入力中のキー入力を処理する
    fn handle_rename_input_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_rename_input();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('j'))
        {
            self.submit_rename_input()?;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('h'))
        {
            self.rename_input.value.pop();
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.close_rename_input();
                Ok(false)
            }
            KeyCode::Enter => {
                self.submit_rename_input()?;
                Ok(false)
            }
            KeyCode::Backspace => {
                self.rename_input.value.pop();
                Ok(false)
            }
            KeyCode::Char(ch) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.rename_input.value.push(ch);
                Ok(false)
            }
            _ => Ok(false),
        }
    }
}
