use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::{error::Result, mode::Mode};

use super::{App, FindKind, PendingNormalAction, PendingOperator, ReplayableAction};

#[derive(Clone, Copy)]
enum NormalInput {
    Enter,
    F2,
    Up,
    Left,
    Down,
    Right,
    Home,
    End,
    Char(char),
    Ctrl(char),
    CtrlSpace,
    Unsupported,
}

enum NormalDecision {
    Ignore,
    Quit,
    SetPending(PendingNormalAction),
    Action(NormalAction),
}

enum NormalAction {
    OpenGoInput,
    OpenPicker,
    OpenSearch,
    OpenReplace,
    OpenDiagnosticPopup,
    OpenDiagnosticList { error_only: bool },
    OpenWorkspaceDiagnosticList { error_only: bool },
    OpenScratchTarget,
    OpenHoverPopup,
    OpenRenameInput,
    CloseCurrentBuffer,
    AdvanceLayoutOrFocus,
    CollapseToSinglePane,
    ToggleTerminalSplit,
    Save,
    EnterInsertAppend,
    EnterInsertAtCursor,
    JumpBack,
    JumpForward,
    PageDownHalf,
    PageUpHalf,
    MoveUp,
    MoveLeft,
    MoveDown,
    MoveRight,
    MoveToLineStart,
    MoveToLineEnd,
    JumpMatchingBracket,
    OpenLineBelow,
    Paste,
    PasteBefore,
    Replay { reverse: bool },
    Undo,
    Redo,
    JumpTop,
    JumpBottom,
    JumpNextGitHunk,
    JumpPreviousGitHunk,
    JumpNextDiagnostic { error_only: bool },
    JumpPreviousDiagnostic { error_only: bool },
    RepeatSearch { forward: bool },
    Goto { kind: super::GotoKind },
    ShowReferences,
    FindMotion { kind: FindKind, target: char },
    OperatorFind {
        operator: PendingOperator,
        find_kind: FindKind,
        target: char,
    },
    OperatorSelectionRange {
        operator: PendingOperator,
    },
    ChangeCurrentLine,
    DeleteCurrentLine,
    YankCurrentLine,
}

fn normalize_normal_input(key_event: KeyEvent) -> NormalInput {
    if key_event.modifiers.contains(KeyModifiers::CONTROL) {
        return match key_event.code {
            KeyCode::Null => NormalInput::CtrlSpace,
            KeyCode::Char(' ') => NormalInput::CtrlSpace,
            KeyCode::Char(ch) => NormalInput::Ctrl(ch),
            _ => NormalInput::Unsupported,
        };
    }

    match key_event.code {
        KeyCode::Up => NormalInput::Up,
        KeyCode::F(2) => NormalInput::F2,
        KeyCode::Enter => NormalInput::Enter,
        KeyCode::Left => NormalInput::Left,
        KeyCode::Down => NormalInput::Down,
        KeyCode::Right => NormalInput::Right,
        KeyCode::Home => NormalInput::Home,
        KeyCode::End => NormalInput::End,
        KeyCode::Char(ch) => NormalInput::Char(ch),
        _ => NormalInput::Unsupported,
    }
}

fn transition_normal_input(
    state: Option<PendingNormalAction>,
    input: NormalInput,
) -> NormalDecision {
    use NormalAction as Act;
    use NormalDecision as Dec;
    use NormalInput as In;
    use PendingNormalAction as State;
    use PendingOperator as Op;

    match (state, input) {
        (_, In::Ctrl('c')) => Dec::Ignore,
        (_, In::Ctrl('q')) => Dec::Quit,
        (None, In::Ctrl('j')) => Dec::Action(Act::OpenScratchTarget),
        (None, In::Ctrl('g')) => Dec::Action(Act::OpenGoInput),
        (None, In::Ctrl('p')) => Dec::Action(Act::OpenPicker),
        (None, In::Ctrl('f')) => Dec::Action(Act::OpenSearch),
        (None, In::Ctrl('h')) => Dec::Action(Act::OpenReplace),
        (None, In::Ctrl('w')) => Dec::Action(Act::CloseCurrentBuffer),
        (None, In::Ctrl('l')) => Dec::Action(Act::AdvanceLayoutOrFocus),
        (None, In::Ctrl('o')) => Dec::Action(Act::CollapseToSinglePane),
        (None, In::CtrlSpace) => Dec::Action(Act::ToggleTerminalSplit),
        (None, In::Ctrl('s')) => Dec::Action(Act::Save),
        (None, In::Ctrl('d')) => Dec::Action(Act::PageDownHalf),
        (None, In::Ctrl('u')) => Dec::Action(Act::PageUpHalf),
        (None, In::Ctrl('z')) | (None, In::Ctrl('y')) => Dec::Ignore,

        (None, In::F2) => Dec::Action(Act::OpenRenameInput),
        (None, In::Enter) => Dec::Action(Act::OpenScratchTarget),
        (None, In::Up) => Dec::Action(Act::MoveUp),
        (None, In::Left) => Dec::Action(Act::MoveLeft),
        (None, In::Down) => Dec::Action(Act::MoveDown),
        (None, In::Right) => Dec::Action(Act::MoveRight),
        (None, In::Home) => Dec::Action(Act::MoveToLineStart),
        (None, In::End) => Dec::Action(Act::MoveToLineEnd),

        (None, In::Char('a')) => Dec::Action(Act::EnterInsertAppend),
        (None, In::Char('h')) => Dec::Action(Act::EnterInsertAtCursor),
        (None, In::Char('b')) => Dec::Action(Act::JumpBack),
        (None, In::Char('B')) => Dec::Action(Act::JumpForward),
        (None, In::Char('e')) => Dec::SetPending(State::DiagnosticPrefix),
        (None, In::Char('K')) => Dec::Action(Act::OpenHoverPopup),
        (None, In::Char('i')) => Dec::Action(Act::MoveUp),
        (None, In::Char('j')) => Dec::Action(Act::MoveLeft),
        (None, In::Char('k')) => Dec::Action(Act::MoveDown),
        (None, In::Char('l')) => Dec::Action(Act::MoveRight),
        (None, In::Char('%')) => Dec::Action(Act::JumpMatchingBracket),
        (None, In::Char('o')) => Dec::Action(Act::OpenLineBelow),
        (None, In::Char('p')) => Dec::Action(Act::Paste),
        (None, In::Char('P')) => Dec::Action(Act::PasteBefore),
        (None, In::Char('r')) => Dec::Action(Act::Replay { reverse: false }),
        (None, In::Char('R')) => Dec::Action(Act::Replay { reverse: true }),
        (None, In::Char('u')) => Dec::Action(Act::Undo),
        (None, In::Char('U')) => Dec::Action(Act::Redo),

        (None, In::Char('g')) => Dec::SetPending(State::GoPrefix),
        (None, In::Char('f')) => Dec::SetPending(State::Find(FindKind::Forward)),
        (None, In::Char('F')) => Dec::SetPending(State::Find(FindKind::Backward)),
        (None, In::Char('t')) => Dec::SetPending(State::Find(FindKind::TillForward)),
        (None, In::Char('T')) => Dec::SetPending(State::Find(FindKind::TillBackward)),
        (None, In::Char('c')) => Dec::SetPending(State::Operator(Op::Change)),
        (None, In::Char('d')) => Dec::SetPending(State::Operator(Op::Delete)),
        (None, In::Char('y')) => Dec::SetPending(State::Operator(Op::Yank)),

        (Some(State::GoPrefix), In::Char('t')) => Dec::Action(Act::JumpTop),
        (Some(State::GoPrefix), In::Char('T')) => Dec::Action(Act::JumpBottom),
        (Some(State::GoPrefix), In::Char('g')) => Dec::Action(Act::JumpNextGitHunk),
        (Some(State::GoPrefix), In::Char('G')) => Dec::Action(Act::JumpPreviousGitHunk),
        (Some(State::GoPrefix), In::Char('w')) => {
            Dec::Action(Act::JumpNextDiagnostic { error_only: false })
        }
        (Some(State::GoPrefix), In::Char('W')) => {
            Dec::Action(Act::JumpPreviousDiagnostic { error_only: false })
        }
        (Some(State::GoPrefix), In::Char('e')) => {
            Dec::Action(Act::JumpNextDiagnostic { error_only: true })
        }
        (Some(State::GoPrefix), In::Char('E')) => {
            Dec::Action(Act::JumpPreviousDiagnostic { error_only: true })
        }
        (Some(State::GoPrefix), In::Char('f')) => Dec::Action(Act::RepeatSearch { forward: true }),
        (Some(State::GoPrefix), In::Char('F')) => Dec::Action(Act::RepeatSearch { forward: false }),
        (Some(State::GoPrefix), In::Char('d')) => {
            Dec::Action(Act::Goto { kind: super::GotoKind::Definition })
        }
        (Some(State::GoPrefix), In::Char('D')) => {
            Dec::Action(Act::Goto { kind: super::GotoKind::Declaration })
        }
        (Some(State::GoPrefix), In::Char('i')) => {
            Dec::Action(Act::Goto { kind: super::GotoKind::Implementation })
        }
        (Some(State::GoPrefix), In::Char('r')) => Dec::Action(Act::ShowReferences),

        (Some(State::DiagnosticPrefix), In::Char('d')) => Dec::Action(Act::OpenDiagnosticPopup),
        (Some(State::DiagnosticPrefix), In::Char('w')) => {
            Dec::Action(Act::OpenDiagnosticList { error_only: false })
        }
        (Some(State::DiagnosticPrefix), In::Char('e')) => {
            Dec::Action(Act::OpenDiagnosticList { error_only: true })
        }
        (Some(State::DiagnosticPrefix), In::Char('W')) => {
            Dec::Action(Act::OpenWorkspaceDiagnosticList { error_only: false })
        }
        (Some(State::DiagnosticPrefix), In::Char('E')) => {
            Dec::Action(Act::OpenWorkspaceDiagnosticList { error_only: true })
        }

        (Some(State::Find(kind)), In::Char(target)) => {
            Dec::Action(Act::FindMotion { kind, target })
        }

        (Some(State::Operator(Op::Change)), In::Char('c')) => Dec::Action(Act::ChangeCurrentLine),
        (Some(State::Operator(Op::Delete)), In::Char('d')) => Dec::Action(Act::DeleteCurrentLine),
        (Some(State::Operator(Op::Yank)), In::Char('y')) => Dec::Action(Act::YankCurrentLine),
        (Some(State::Operator(operator)), In::Char('f')) => {
            Dec::SetPending(State::OperatorFind(operator, FindKind::Forward))
        }
        (Some(State::Operator(operator)), In::Char('F')) => {
            Dec::SetPending(State::OperatorFind(operator, FindKind::Backward))
        }
        (Some(State::Operator(operator)), In::Char('t')) => {
            Dec::SetPending(State::OperatorFind(operator, FindKind::TillForward))
        }
        (Some(State::Operator(operator)), In::Char('T')) => {
            Dec::SetPending(State::OperatorFind(operator, FindKind::TillBackward))
        }
        (Some(State::Operator(operator)), In::Char('i')) => {
            Dec::Action(Act::OperatorSelectionRange { operator })
        }

        (Some(State::OperatorFind(operator, find_kind)), In::Char(target)) => {
            Dec::Action(Act::OperatorFind {
                operator,
                find_kind,
                target,
            })
        }

        _ => Dec::Ignore,
    }
}

impl App {
    pub(super) fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Key(key_event) => self.handle_key_event(key_event),
            Event::Mouse(_) => Ok(false),
            _ => Ok(false),
        }
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) -> Result<bool> {
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

        if self.focused_pane == super::FocusedPane::Right
            && matches!(
                self.layout_mode,
                super::LayoutMode::TerminalSplit | super::LayoutMode::Single
            )
        {
            return self.handle_shell_mode_key(key_event);
        }

        match self.mode {
            Mode::Normal => self.handle_normal_mode_key(key_event),
            Mode::Insert => self.handle_insert_mode_key(key_event),
            Mode::Shell => self.handle_shell_mode_key(key_event),
        }
    }

    fn handle_normal_mode_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if !self.workspace.has_documents() {
            return match transition_normal_input(None, normalize_normal_input(key_event)) {
                NormalDecision::Quit => Ok(true),
                NormalDecision::Action(action) => self.apply_normal_action(action),
                NormalDecision::SetPending(_) | NormalDecision::Ignore => {
                    self.pending_normal_action = None;
                    Ok(false)
                }
            };
        }

        let decision = transition_normal_input(
            self.pending_normal_action.take(),
            normalize_normal_input(key_event),
        );

        match decision {
            NormalDecision::Quit => Ok(true),
            NormalDecision::Ignore => Ok(false),
            NormalDecision::SetPending(next_state) => {
                self.pending_normal_action = Some(next_state);
                Ok(false)
            }
            NormalDecision::Action(action) => self.apply_normal_action(action),
        }
    }

    fn apply_normal_action(&mut self, action: NormalAction) -> Result<bool> {
        match action {
            NormalAction::OpenGoInput => self.open_go_input(),
            NormalAction::OpenPicker => self.open_or_cycle_picker()?,
            NormalAction::OpenSearch => self.open_or_cycle_search_input(),
            NormalAction::OpenReplace => self.open_or_cycle_replace_input(),
            NormalAction::OpenDiagnosticPopup => self.open_current_diagnostic_popup(),
            NormalAction::OpenDiagnosticList { error_only } => {
                self.open_diagnostic_list(error_only);
            }
            NormalAction::OpenWorkspaceDiagnosticList { error_only } => {
                self.request_workspace_diagnostic_list(error_only)?;
            }
            NormalAction::OpenScratchTarget => {
                if self.workspace.has_documents() && self.workspace.current_document().is_scratch() {
                    self.open_scratch_target_under_cursor()?;
                }
            }
            NormalAction::OpenHoverPopup => {
                if self.workspace.has_documents() {
                    self.open_hover_popup()?;
                }
            }
            NormalAction::OpenRenameInput => {
                if self.workspace.has_documents() {
                    self.open_rename_input();
                }
            }
            NormalAction::CloseCurrentBuffer => self.close_current_buffer(),
            NormalAction::AdvanceLayoutOrFocus => self.advance_layout_or_focus(),
            NormalAction::CollapseToSinglePane => self.collapse_to_single_pane(),
            NormalAction::ToggleTerminalSplit => self.toggle_terminal_split()?,
            NormalAction::Save => {
                if self.workspace.has_documents() {
                    self.save_current_document()?;
                }
            }
            NormalAction::EnterInsertAppend => {
                if self.workspace.has_documents() {
                    self.workspace.current_document_mut().begin_undo_group();
                    self.mode = Mode::Insert;
                    self.pending_insert_j = None;
                    self.move_cursor_right();
                    self.clamp_vertical_state();
                }
            }
            NormalAction::EnterInsertAtCursor => {
                if self.workspace.has_documents() {
                    self.workspace.current_document_mut().begin_undo_group();
                    self.mode = Mode::Insert;
                    self.pending_insert_j = None;
                }
            }
            NormalAction::JumpBack => self.jump_back(),
            NormalAction::JumpForward => self.jump_forward(),
            NormalAction::PageDownHalf => {
                if self.workspace.has_documents() {
                    self.page_down_half();
                }
            }
            NormalAction::PageUpHalf => {
                if self.workspace.has_documents() {
                    self.page_up_half();
                }
            }
            NormalAction::MoveUp => {
                if self.workspace.has_documents() {
                    self.move_cursor_up();
                }
            }
            NormalAction::MoveLeft => {
                if self.workspace.has_documents() {
                    self.move_cursor_left();
                }
            }
            NormalAction::MoveDown => {
                if self.workspace.has_documents() {
                    self.move_cursor_down();
                }
            }
            NormalAction::MoveRight => {
                if self.workspace.has_documents() {
                    self.move_cursor_right();
                }
            }
            NormalAction::MoveToLineStart => {
                if self.workspace.has_documents() {
                    self.move_cursor_to_line_start();
                }
            }
            NormalAction::MoveToLineEnd => {
                if self.workspace.has_documents() {
                    self.move_cursor_to_line_end();
                }
            }
            NormalAction::JumpMatchingBracket => {
                if self.workspace.has_documents() {
                    self.jump_to_matching_bracket();
                }
            }
            NormalAction::OpenLineBelow => {
                if self.workspace.has_documents() {
                    self.open_line_below();
                }
            }
            NormalAction::Paste => {
                if self.workspace.has_documents() {
                    self.paste_after_cursor()?;
                }
            }
            NormalAction::PasteBefore => {
                if self.workspace.has_documents() {
                    self.paste_before_cursor()?;
                }
            }
            NormalAction::Replay { reverse } => {
                if self.workspace.has_documents() {
                    self.replay_last_action(reverse)?;
                }
            }
            NormalAction::Undo => {
                if self.workspace.has_documents() {
                    self.undo_current_document();
                }
            }
            NormalAction::Redo => {
                if self.workspace.has_documents() {
                    self.redo_current_document();
                }
            }
            NormalAction::JumpTop => self.jump_to_top(),
            NormalAction::JumpBottom => self.jump_to_bottom(),
            NormalAction::JumpNextGitHunk => {
                self.jump_to_next_git_marker();
                self.last_replayable_action = Some(ReplayableAction::GitHunk { forward: true });
            }
            NormalAction::JumpPreviousGitHunk => {
                self.jump_to_previous_git_marker();
                self.last_replayable_action = Some(ReplayableAction::GitHunk { forward: false });
            }
            NormalAction::JumpNextDiagnostic { error_only } => {
                self.jump_to_next_diagnostic(error_only);
            }
            NormalAction::JumpPreviousDiagnostic { error_only } => {
                self.jump_to_previous_diagnostic(error_only);
            }
            NormalAction::RepeatSearch { forward } => {
                if forward {
                    self.repeat_search_forward()?;
                } else {
                    self.repeat_search_backward()?;
                }
            }
            NormalAction::Goto { kind } => {
                self.goto_symbol(kind)?;
            }
            NormalAction::ShowReferences => {
                self.show_references()?;
            }
            NormalAction::FindMotion { kind, target } => {
                self.run_find_motion(kind, target)?;
            }
            NormalAction::OperatorFind {
                operator,
                find_kind,
                target,
            } => {
                self.run_operator_find(operator, find_kind, target)?;
            }
            NormalAction::OperatorSelectionRange { operator } => {
                self.request_selection_range_operator(operator)?;
            }
            NormalAction::ChangeCurrentLine => {
                self.change_current_line()?;
            }
            NormalAction::DeleteCurrentLine => {
                self.delete_current_line()?;
            }
            NormalAction::YankCurrentLine => {
                self.yank_current_line()?;
            }
        }

        Ok(false)
    }

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
            _ => Ok(false),
        }
    }

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
            self.incremental_search_current_file();
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
            self.incremental_search_current_file();
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
                self.incremental_search_current_file();
                Ok(false)
            }
            KeyCode::Char(ch) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search_input.value.push(ch);
                self.incremental_search_current_file();
                Ok(false)
            }
            _ => Ok(false),
        }
    }

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

    fn handle_insert_mode_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.leave_insert_mode(true);
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('s'))
        {
            self.save_current_document()?;
            self.leave_insert_mode(true);
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('h'))
        {
            self.backspace_char();
            self.pending_insert_j = None;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('d'))
        {
            self.delete_forward_char();
            self.pending_insert_j = None;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('j'))
        {
            self.insert_newline();
            self.pending_insert_j = None;
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.leave_insert_mode(true);
                Ok(false)
            }
            KeyCode::Up => {
                self.move_cursor_up();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Left => {
                self.move_cursor_left();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Down => {
                self.move_cursor_down();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Right => {
                self.move_cursor_right();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Home => {
                self.move_cursor_to_line_start();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::End => {
                self.move_cursor_to_line_end();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Char('j') => {
                let now = Instant::now();
                if self
                    .pending_insert_j
                    .is_some_and(|previous| now.duration_since(previous) <= super::insert_escape_timeout())
                {
                    self.backspace_char();
                    self.leave_insert_mode(false);
                } else {
                    self.insert_char('j');
                    self.pending_insert_j = Some(now);
                }
                Ok(false)
            }
            KeyCode::Char('m') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_newline();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Enter => {
                self.insert_newline();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Char(ch) => {
                self.insert_char(ch);
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Tab => {
                self.insert_tab();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Backspace => {
                self.backspace_char();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Delete => {
                self.delete_forward_char();
                self.pending_insert_j = None;
                Ok(false)
            }
            _ => {
                self.pending_insert_j = None;
                Ok(false)
            }
        }
    }

    fn handle_picker_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.close_picker();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('p'))
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
