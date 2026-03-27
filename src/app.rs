use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    terminal,
};

use crate::{
    config,
    document::{DiagnosticSeverity, DiagnosticSummary, Document},
    error::Result,
    mode::Mode,
    open_candidate::{
        OpenCandidate, ProjectFileCandidate, collect_project_file_candidates, collect_project_search_paths,
    },
    picker_match,
};

mod render;
mod terminal_session;
mod workspace;

use self::terminal_session::TerminalSession;

pub struct App {
    pub mode: Mode,
    pub workspace: Workspace,
    pub picker: PickerState,
    pub shell: ShellState,
    pub cursor: CursorState,
    pub viewport_row: usize,
    pub pending_normal_action: Option<PendingNormalAction>,
    pub pending_insert_j: Option<Instant>,
    pub last_replayable_action: Option<ReplayableAction>,
    pub go_input: GoInputState,
    pub search_input: SearchInputState,
    pub last_search: Option<SearchState>,
    pub yank_buffer: YankBuffer,
    pub jump_history: Vec<JumpPosition>,
    pub layout_mode: LayoutMode,
    pub focused_pane: FocusedPane,
    pub rust_support: RustSupportState,
    pub last_save_feedback: Option<String>,
}

#[derive(Clone, Copy)]
pub enum ReplayableAction {
    GitHunk { forward: bool },
    Find(FindKind, char),
    Diagnostic { error_only: bool, forward: bool },
    Search { forward: bool },
}

#[derive(Clone, Copy)]
pub enum PendingNormalAction {
    GoPrefix,
    Find(FindKind),
    Operator(PendingOperator),
    OperatorFind(PendingOperator, FindKind),
}

#[derive(Clone, Copy)]
pub enum PendingOperator {
    Change,
    Delete,
    Yank,
}

#[derive(Clone, Copy)]
pub enum FindKind {
    Forward,
    Backward,
    TillForward,
    TillBackward,
}

pub struct Workspace {
    pub documents: Vec<DocumentEntry>,
    pub current_index: usize,
}

pub struct DocumentEntry {
    pub path: PathBuf,
    pub document: Document,
    pub view_state: BufferViewState,
}

pub struct PickerState {
    pub active: bool,
    pub query: String,
    pub candidates: Vec<OpenCandidate>,
    pub scope: PickerScope,
}

pub struct ShellState {
    pub program: String,
}

pub struct CursorState {
    pub row: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Default)]
pub struct BufferViewState {
    pub row: usize,
    pub column: usize,
    pub viewport_row: usize,
}

pub struct GoInputState {
    pub active: bool,
    pub value: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    CurrentFile,
    OpenBuffers,
    Project,
}

pub struct SearchInputState {
    pub active: bool,
    pub value: String,
    pub scope: SearchScope,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PickerScope {
    All,
    Buffers,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Single,
    Dual,
    TerminalSplit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusedPane {
    Left,
    Right,
}

#[derive(Clone)]
pub struct SearchState {
    pub query: String,
    pub scope: SearchScope,
}

#[derive(Clone, Copy)]
pub struct JumpPosition {
    pub row: usize,
    pub column: usize,
    pub viewport_row: usize,
}

pub enum YankBuffer {
    Empty,
    Charwise(String),
    Linewise(String),
}

pub struct RustSupportState {
    pub rust_analyzer_available: bool,
    pub cargo_manifest_in_cwd: bool,
}

impl SearchScope {
    fn label(self) -> &'static str {
        match self {
            Self::CurrentFile => "file",
            Self::OpenBuffers => "buffers",
            Self::Project => "project",
        }
    }
}

impl App {
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let rust_support = RustSupportState {
            rust_analyzer_available: rust_analyzer_available(),
            cargo_manifest_in_cwd: Path::new("Cargo.toml").exists(),
        };
        let workspace = match path {
            Some(path) => Workspace {
                documents: vec![DocumentEntry {
                    path: path.to_path_buf(),
                    document: Document::open(path)?,
                    view_state: BufferViewState::default(),
                }],
                current_index: 0,
            },
            None => Workspace {
                documents: Vec::new(),
                current_index: 0,
            },
        };

        let mut app = Self {
            mode: Mode::Normal,
            workspace,
            picker: PickerState {
                active: false,
                query: String::new(),
                candidates: Vec::new(),
                scope: PickerScope::All,
            },
            shell: ShellState {
                program: config::shell_program().to_owned(),
            },
            cursor: CursorState { row: 0, column: 0 },
            viewport_row: 0,
            pending_normal_action: None,
            pending_insert_j: None,
            last_replayable_action: None,
            go_input: GoInputState {
                active: false,
                value: String::new(),
            },
            search_input: SearchInputState {
                active: false,
                value: String::new(),
                scope: SearchScope::CurrentFile,
            },
            last_search: None,
            yank_buffer: YankBuffer::Empty,
            jump_history: Vec::new(),
            layout_mode: LayoutMode::Single,
            focused_pane: FocusedPane::Left,
            rust_support,
            last_save_feedback: None,
        };
        app.refresh_picker_candidates()?;
        Ok(app)
    }

    pub fn run(&mut self) -> Result<()> {
        let mut terminal_session = TerminalSession::enter()?;

        loop {
            self.render_frame(terminal_session.terminal())?;

            if self.handle_event(event::read()?)? {
                break;
            }
        }

        terminal_session.leave()?;
        Ok(())
    }

    pub fn picker_matches(&self) -> Vec<OpenCandidate> {
        picker_match::sort_open_candidates(&self.picker.candidates, &self.picker.query)
    }

    pub fn refresh_picker_candidates(&mut self) -> Result<()> {
        let mut candidates = self.workspace.open_buffer_candidates();
        let open_paths: HashSet<PathBuf> = self
            .workspace
            .documents
            .iter()
            .map(|entry| entry.path.clone())
            .collect();

        for candidate in collect_project_file_candidates()? {
            if open_paths.contains(&candidate.path) {
                continue;
            }
            candidates.push(OpenCandidate::from_project_file(candidate));
        }

        self.picker.candidates = candidates;
        Ok(())
    }

    fn save_current_buffer_view_state(&mut self) {
        if let Some(entry) = self.workspace.documents.get_mut(self.workspace.current_index) {
            entry.view_state = BufferViewState {
                row: self.cursor.row,
                column: self.cursor.column,
                viewport_row: self.viewport_row,
            };
        }
    }

    fn restore_current_buffer_view_state(&mut self) {
        let Some(entry) = self.workspace.documents.get(self.workspace.current_index) else {
            self.cursor = CursorState { row: 0, column: 0 };
            self.viewport_row = 0;
            return;
        };

        self.cursor.row = entry.view_state.row;
        self.cursor.column = entry.view_state.column;
        self.viewport_row = entry.view_state.viewport_row;
        self.clamp_vertical_state();
    }

    fn make_document_current(&mut self, index: usize) {
        self.save_current_buffer_view_state();
        self.workspace.make_current(index);
        self.restore_current_buffer_view_state();
    }

    fn select_current_document(&mut self, index: usize) {
        self.save_current_buffer_view_state();
        self.workspace.select_current(index);
        self.restore_current_buffer_view_state();
    }

    fn open_document(&mut self, path: PathBuf) -> Result<()> {
        self.save_current_buffer_view_state();
        self.workspace.open_document(path)?;
        self.restore_current_buffer_view_state();
        Ok(())
    }

    fn handle_event(&mut self, event: Event) -> Result<bool> {
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

        if self.search_input.active {
            return self.handle_search_input_key(key_event);
        }

        if self.picker.active {
            return self.handle_picker_key(key_event);
        }

        match self.mode {
            Mode::Normal => self.handle_normal_mode_key(key_event),
            Mode::Insert => self.handle_insert_mode_key(key_event),
            Mode::Command | Mode::Shell => Ok(false),
        }
    }

    fn handle_normal_mode_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            self.pending_normal_action = None;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('q'))
        {
            return Ok(true);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('g'))
        {
            self.open_go_input();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('p'))
        {
            self.open_or_cycle_picker()?;
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('f'))
        {
            self.open_or_cycle_search_input();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('w'))
        {
            self.close_current_buffer();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('l'))
        {
            self.advance_layout_or_focus();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('o'))
        {
            self.collapse_to_single_pane();
            return Ok(false);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Null | KeyCode::Char(' '))
        {
            self.toggle_terminal_split();
            return Ok(false);
        }

        if !self.workspace.has_documents() {
            return Ok(false);
        }

        if let Some(pending_action) = self.pending_normal_action.take() {
            return self.handle_pending_normal_action(pending_action, key_event);
        }

        match key_event.code {
            KeyCode::Up => {
                self.move_cursor_up();
                Ok(false)
            }
            KeyCode::Left => {
                self.move_cursor_left();
                Ok(false)
            }
            KeyCode::Down => {
                self.move_cursor_down();
                Ok(false)
            }
            KeyCode::Right => {
                self.move_cursor_right();
                Ok(false)
            }
            KeyCode::Home => {
                self.move_cursor_to_line_start();
                Ok(false)
            }
            KeyCode::End => {
                self.move_cursor_to_line_end();
                Ok(false)
            }
            KeyCode::Char('s') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_current_document()?;
                Ok(false)
            }
            KeyCode::Char('z') if key_event.modifiers.contains(KeyModifiers::CONTROL) => Ok(false),
            KeyCode::Char('y') if key_event.modifiers.contains(KeyModifiers::CONTROL) => Ok(false),
            KeyCode::Char('a') => {
                self.workspace.current_document_mut().begin_undo_group();
                self.mode = Mode::Insert;
                self.pending_insert_j = None;
                self.move_cursor_right();
                self.clamp_vertical_state();
                Ok(false)
            }
            KeyCode::Char('b') => {
                self.jump_back();
                Ok(false)
            }
            KeyCode::Char('d') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.page_down_half();
                Ok(false)
            }
            KeyCode::Char('d') => {
                self.pending_normal_action = Some(PendingNormalAction::Operator(PendingOperator::Delete));
                Ok(false)
            }
            KeyCode::Char('f') => {
                self.pending_normal_action = Some(PendingNormalAction::Find(FindKind::Forward));
                Ok(false)
            }
            KeyCode::Char('F') => {
                self.pending_normal_action = Some(PendingNormalAction::Find(FindKind::Backward));
                Ok(false)
            }
            KeyCode::Char('g') => {
                self.pending_normal_action = Some(PendingNormalAction::GoPrefix);
                Ok(false)
            }
            KeyCode::Char('h') => {
                self.workspace.current_document_mut().begin_undo_group();
                self.mode = Mode::Insert;
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Char('i') => {
                self.move_cursor_up();
                Ok(false)
            }
            KeyCode::Char('j') => {
                self.move_cursor_left();
                Ok(false)
            }
            KeyCode::Char('k') => {
                self.move_cursor_down();
                Ok(false)
            }
            KeyCode::Char('l') => {
                self.move_cursor_right();
                Ok(false)
            }
            KeyCode::Char('c') => {
                self.pending_normal_action = Some(PendingNormalAction::Operator(PendingOperator::Change));
                Ok(false)
            }
            KeyCode::Char('%') => {
                self.jump_to_matching_bracket();
                Ok(false)
            }
            KeyCode::Char('o') => {
                self.open_line_below();
                Ok(false)
            }
            KeyCode::Char('p') => {
                self.paste_after_cursor()?;
                Ok(false)
            }
            KeyCode::Char('r') => {
                self.replay_last_action(false)?;
                Ok(false)
            }
            KeyCode::Char('R') => {
                self.replay_last_action(true)?;
                Ok(false)
            }
            KeyCode::Char('t') => {
                self.pending_normal_action = Some(PendingNormalAction::Find(FindKind::TillForward));
                Ok(false)
            }
            KeyCode::Char('T') => {
                self.pending_normal_action = Some(PendingNormalAction::Find(FindKind::TillBackward));
                Ok(false)
            }
            KeyCode::Char('u') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.page_up_half();
                Ok(false)
            }
            KeyCode::Char('u') => {
                self.undo_current_document();
                Ok(false)
            }
            KeyCode::Char('U') => {
                self.redo_current_document();
                Ok(false)
            }
            KeyCode::Char('y') => {
                self.pending_normal_action = Some(PendingNormalAction::Operator(PendingOperator::Yank));
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    fn handle_pending_normal_action(
        &mut self,
        pending_action: PendingNormalAction,
        key_event: KeyEvent,
    ) -> Result<bool> {
        match pending_action {
            PendingNormalAction::GoPrefix => match key_event.code {
                KeyCode::Char('t') => {
                    self.jump_to_top();
                    Ok(false)
                }
                KeyCode::Char('T') => {
                    self.jump_to_bottom();
                    Ok(false)
                }
                KeyCode::Char('g') => {
                    self.jump_to_next_git_marker();
                    self.last_replayable_action = Some(ReplayableAction::GitHunk { forward: true });
                    Ok(false)
                }
                KeyCode::Char('w') => {
                    self.jump_to_next_diagnostic(false);
                    Ok(false)
                }
                KeyCode::Char('W') => {
                    self.jump_to_previous_diagnostic(false);
                    Ok(false)
                }
                KeyCode::Char('e') => {
                    self.jump_to_next_diagnostic(true);
                    Ok(false)
                }
                KeyCode::Char('E') => {
                    self.jump_to_previous_diagnostic(true);
                    Ok(false)
                }
                KeyCode::Char('f') => {
                    self.repeat_search_forward()?;
                    Ok(false)
                }
                KeyCode::Char('F') => {
                    self.repeat_search_backward()?;
                    Ok(false)
                }
                KeyCode::Char('G') => {
                    self.jump_to_previous_git_marker();
                    self.last_replayable_action = Some(ReplayableAction::GitHunk { forward: false });
                    Ok(false)
                }
                _ => Ok(false),
            },
            PendingNormalAction::Find(find_kind) => match key_event.code {
                KeyCode::Char(target) => {
                    self.run_find_motion(find_kind, target)?;
                    Ok(false)
                }
                _ => Ok(false),
            },
            PendingNormalAction::Operator(operator) => match key_event.code {
                KeyCode::Char('c') if matches!(operator, PendingOperator::Change) => {
                    self.change_current_line()?;
                    Ok(false)
                }
                KeyCode::Char('d') if matches!(operator, PendingOperator::Delete) => {
                    self.delete_current_line()?;
                    Ok(false)
                }
                KeyCode::Char('f') => {
                    self.pending_normal_action =
                        Some(PendingNormalAction::OperatorFind(operator, FindKind::Forward));
                    Ok(false)
                }
                KeyCode::Char('F') => {
                    self.pending_normal_action =
                        Some(PendingNormalAction::OperatorFind(operator, FindKind::Backward));
                    Ok(false)
                }
                KeyCode::Char('t') => {
                    self.pending_normal_action =
                        Some(PendingNormalAction::OperatorFind(operator, FindKind::TillForward));
                    Ok(false)
                }
                KeyCode::Char('T') => {
                    self.pending_normal_action =
                        Some(PendingNormalAction::OperatorFind(operator, FindKind::TillBackward));
                    Ok(false)
                }
                KeyCode::Char('y') if matches!(operator, PendingOperator::Yank) => {
                    self.yank_current_line()?;
                    Ok(false)
                }
                _ => Ok(false),
            },
            PendingNormalAction::OperatorFind(operator, find_kind) => match key_event.code {
                KeyCode::Char(target) => {
                    self.run_operator_find(operator, find_kind, target)?;
                    Ok(false)
                }
                _ => Ok(false),
            },
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
                    .is_some_and(|previous| now.duration_since(previous) <= insert_escape_timeout())
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

    fn run_find_motion(&mut self, find_kind: FindKind, target: char) -> Result<()> {
        let Some((found_row, found_column)) = self.find_target_position(find_kind, target)? else {
            return Ok(());
        };

        self.cursor.row = found_row;
        self.cursor.column = motion_destination_column(find_kind, found_column);
        self.clamp_vertical_state();
        self.last_replayable_action = Some(ReplayableAction::Find(find_kind, target));
        Ok(())
    }

    fn run_operator_find(
        &mut self,
        operator: PendingOperator,
        find_kind: FindKind,
        target: char,
    ) -> Result<()> {
        let Some((found_row, found_column)) = self.find_target_position(find_kind, target)? else {
            return Ok(());
        };

        let Some((start_row, start_column, end_row, end_column)) =
            operator_range(
                self.cursor.row,
                self.cursor.column,
                found_row,
                found_column,
                find_kind,
            )
        else {
            return Ok(());
        };

        if matches!(operator, PendingOperator::Yank) {
            return self.yank_range(start_row, start_column, end_row, end_column);
        }

        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };

        self.workspace.current_document_mut().begin_undo_group();
        let Some((row, column)) = self.workspace.current_document_mut().remove_display_range(
            start_row,
            start_column,
            end_row,
            end_column,
            width as usize,
        ) else {
            self.workspace.current_document_mut().end_undo_group();
            return Ok(());
        };

        self.cursor.row = row;
        self.cursor.column = column;
        self.clamp_vertical_state();

        if matches!(operator, PendingOperator::Change) {
            self.mode = Mode::Insert;
            self.pending_insert_j = None;
        } else {
            self.workspace.current_document_mut().end_undo_group();
        }

        Ok(())
    }

    fn yank_range(
        &mut self,
        start_row: usize,
        start_column: usize,
        end_row: usize,
        end_column: usize,
    ) -> Result<()> {
        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };

        let document = self.workspace.current_document();
        let total_rows = document.total_rows(width as usize).unwrap_or(0);
        if total_rows == 0 {
            return Ok(());
        }

        let normalized_end_row = end_row.min(total_rows.saturating_sub(1));
        let mut collected = String::new();

        for row in start_row..=normalized_end_row {
            let line_text = document.display_line_text(row, width as usize)?;
            let line_len = line_text.chars().count();
            let slice_start = if row == start_row {
                start_column.min(line_len)
            } else {
                0
            };
            let slice_end = if row == normalized_end_row {
                end_column.min(line_len)
            } else {
                line_len
            };

            collected.extend(
                line_text
                    .chars()
                    .skip(slice_start)
                    .take(slice_end.saturating_sub(slice_start)),
            );

            if row != normalized_end_row {
                collected.push('\n');
            }
        }

        self.yank_buffer = YankBuffer::Charwise(collected);
        Ok(())
    }

    fn find_target_position(&self, find_kind: FindKind, target: char) -> Result<Option<(usize, usize)>> {
        let Ok((width, _)) = terminal::size() else {
            return Ok(None);
        };

        let document = self.workspace.current_document();
        let total_rows = document.total_rows(width as usize).unwrap_or(0);
        if total_rows == 0 {
            return Ok(None);
        }

        match find_kind {
            FindKind::Forward | FindKind::TillForward => {
                for row in self.cursor.row..total_rows {
                    let line_text = document.display_line_text(row, width as usize)?;
                    let line_chars: Vec<char> = line_text.chars().collect();
                    let start_column = if row == self.cursor.row {
                        self.cursor.column.saturating_add(1).min(line_chars.len())
                    } else {
                        0
                    };

                    if let Some(column) =
                        (start_column..line_chars.len()).find(|index| line_chars[*index] == target)
                    {
                        return Ok(Some((row, column)));
                    }
                }
            }
            FindKind::Backward | FindKind::TillBackward => {
                let first_row = self.cursor.row.min(total_rows.saturating_sub(1));
                for row in (0..=first_row).rev() {
                    let line_text = document.display_line_text(row, width as usize)?;
                    let line_chars: Vec<char> = line_text.chars().collect();
                    let end_column = if row == self.cursor.row {
                        self.cursor.column.min(line_chars.len())
                    } else {
                        line_chars.len()
                    };

                    if let Some(column) = (0..end_column).rev().find(|index| line_chars[*index] == target)
                    {
                        return Ok(Some((row, column)));
                    }
                }
            }
        }

        Ok(None)
    }

    fn replay_last_action(&mut self, reverse: bool) -> Result<()> {
        let Some(action) = self.last_replayable_action else {
            return Ok(());
        };

        match action {
            ReplayableAction::GitHunk { forward } => {
                if forward ^ reverse {
                    self.jump_to_next_git_marker();
                } else {
                    self.jump_to_previous_git_marker();
                }
            }
            ReplayableAction::Find(find_kind, target) => {
                self.run_find_motion(invert_find_kind(find_kind, reverse), target)?;
            }
            ReplayableAction::Diagnostic { error_only, forward } => {
                if forward ^ reverse {
                    self.jump_to_next_diagnostic(error_only);
                } else {
                    self.jump_to_previous_diagnostic(error_only);
                }
            }
            ReplayableAction::Search { forward } => {
                if forward ^ reverse {
                    self.repeat_search_forward()?;
                } else {
                    self.repeat_search_backward()?;
                }
            }
        }

        Ok(())
    }

    fn push_jump_history(&mut self) {
        self.jump_history.push(JumpPosition {
            row: self.cursor.row,
            column: self.cursor.column,
            viewport_row: self.viewport_row,
        });
    }

    fn jump_back(&mut self) {
        let Some(previous_position) = self.jump_history.pop() else {
            return;
        };

        self.cursor.row = previous_position.row;
        self.cursor.column = previous_position.column;
        self.viewport_row = previous_position.viewport_row;
        self.clamp_vertical_state();
    }

    fn jump_to_matching_bracket(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some((row, column)) = self
            .workspace
            .current_document()
            .matching_bracket_position(self.cursor.row, self.cursor.column, width as usize)
        {
            self.push_jump_history();
            self.cursor.column = column;
            self.jump_with_context(row, width as usize);
        }
    }

    fn paste_after_cursor(&mut self) -> Result<()> {
        self.workspace.current_document_mut().begin_undo_group();

        match self.yank_buffer_clone() {
            YankBuffer::Empty => {}
            YankBuffer::Charwise(yank_text) => {
                let Ok((width, _)) = terminal::size() else {
                    return Ok(());
                };

                let line_width = self
                    .workspace
                    .current_document()
                    .display_line_width(self.cursor.row, width as usize)?;
                let insertion_column = self.cursor.column.min(line_width);
                self.insert_text_at(self.cursor.row, insertion_column, &yank_text);
            }
            YankBuffer::Linewise(line_text) => {
                self.open_line_below_with_text(&line_text);
            }
        }

        self.workspace.current_document_mut().end_undo_group();
        Ok(())
    }

    fn insert_text_at(&mut self, mut row: usize, mut column: usize, text: &str) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        for ch in text.chars() {
            let next_position = if ch == '\n' {
                self.workspace
                    .current_document_mut()
                    .insert_newline(row, column, width as usize)
            } else {
                self.workspace
                    .current_document_mut()
                    .insert_char(row, column, width as usize, ch)
            };

            let Some((next_row, next_column)) = next_position else {
                return;
            };
            row = next_row;
            column = next_column;
        }

        self.cursor.row = row;
        self.cursor.column = column;
        self.clamp_vertical_state();
    }

    fn open_line_below(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        self.workspace.current_document_mut().begin_undo_group();
        if let Some((row, column)) = self
            .workspace
            .current_document_mut()
            .open_below(self.cursor.row, width as usize)
        {
            self.cursor.row = row;
            self.cursor.column = column;
            self.mode = Mode::Insert;
            self.pending_insert_j = None;
            self.clamp_vertical_state();
        }
    }

    fn open_line_below_with_text(&mut self, text: &str) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some((row, column)) = self
            .workspace
            .current_document_mut()
            .open_below(self.cursor.row, width as usize)
        {
            self.cursor.row = row;
            self.cursor.column = column;
            self.insert_text_at(row, column, text);
        }
    }

    fn yank_current_line(&mut self) -> Result<()> {
        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };

        if let Some(line_text) = self
            .workspace
            .current_document()
            .current_line_text(self.cursor.row, width as usize)
        {
            self.yank_buffer = YankBuffer::Linewise(line_text);
        }

        Ok(())
    }

    fn delete_current_line(&mut self) -> Result<()> {
        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };

        self.workspace.current_document_mut().begin_undo_group();
        if let Some((line_text, (row, column))) = self
            .workspace
            .current_document_mut()
            .delete_current_line(self.cursor.row, width as usize)
        {
            self.yank_buffer = YankBuffer::Linewise(line_text);
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
        self.workspace.current_document_mut().end_undo_group();

        Ok(())
    }

    fn change_current_line(&mut self) -> Result<()> {
        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };

        self.workspace.current_document_mut().begin_undo_group();
        if let Some((line_text, (row, column))) = self
            .workspace
            .current_document_mut()
            .clear_current_line(self.cursor.row, width as usize)
        {
            self.yank_buffer = YankBuffer::Linewise(line_text);
            self.cursor.row = row;
            self.cursor.column = column;
            self.mode = Mode::Insert;
            self.pending_insert_j = None;
            self.clamp_vertical_state();
        } else {
            self.workspace.current_document_mut().end_undo_group();
        }

        Ok(())
    }

    fn yank_buffer_clone(&self) -> YankBuffer {
        match &self.yank_buffer {
            YankBuffer::Empty => YankBuffer::Empty,
            YankBuffer::Charwise(text) => YankBuffer::Charwise(text.clone()),
            YankBuffer::Linewise(text) => YankBuffer::Linewise(text.clone()),
        }
    }

    fn undo_current_document(&mut self) {
        if self.workspace.current_document_mut().undo() {
            self.clamp_vertical_state();
        }
    }

    fn redo_current_document(&mut self) {
        if self.workspace.current_document_mut().redo() {
            self.clamp_vertical_state();
        }
    }

    fn open_go_input(&mut self) {
        self.go_input.active = true;
        self.go_input.value.clear();
    }

    fn close_go_input(&mut self) {
        self.go_input.active = false;
        self.go_input.value.clear();
    }

    fn submit_go_input(&mut self) -> Result<()> {
        let Ok((width, _)) = terminal::size() else {
            self.close_go_input();
            return Ok(());
        };

        if let Ok(line_number) = self.go_input.value.parse::<usize>() {
            if let Some(row) = self
                .workspace
                .current_document()
                .jump_row_for_line_number(line_number, width as usize)
            {
                self.push_jump_history();
                self.cursor.column = 0;
                self.jump_with_context(row, width as usize);
            }
        }

        self.close_go_input();
        Ok(())
    }

    fn open_or_cycle_search_input(&mut self) {
        if self.search_input.active {
            self.cycle_search_scope();
            return;
        }

        self.search_input.active = true;
        self.search_input.value.clear();
        self.search_input.scope = SearchScope::CurrentFile;
    }

    fn open_or_cycle_picker(&mut self) -> Result<()> {
        if self.picker.active {
            self.picker.scope = match self.picker.scope {
                PickerScope::All => PickerScope::Buffers,
                PickerScope::Buffers => PickerScope::All,
            };
        } else {
            self.picker.active = true;
            self.picker.query.clear();
            self.picker.scope = PickerScope::All;
        }

        self.refresh_picker_candidates()?;
        Ok(())
    }

    fn close_picker(&mut self) {
        self.picker.active = false;
        self.picker.query.clear();
        self.picker.scope = PickerScope::All;
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
            self.open_or_cycle_picker()?;
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

    fn filtered_picker_matches(&self) -> Vec<OpenCandidate> {
        self.ranked_picker_matches()
            .into_iter()
            .map(|matched| matched.candidate)
            .collect()
    }

    fn ranked_picker_matches(&self) -> Vec<picker_match::PickerMatch> {
        let candidates = match self.picker.scope {
            PickerScope::All => self.picker.candidates.clone(),
            PickerScope::Buffers => self
                .picker
                .candidates
                .iter()
                .filter(|candidate| matches!(candidate, OpenCandidate::OpenBuffer(_)))
                .cloned()
                .collect(),
        };

        picker_match::ranked_open_candidates(&candidates, &self.picker.query)
    }

    fn submit_picker_selection(&mut self) -> Result<()> {
        let matches = self.filtered_picker_matches();
        let Some(candidate) = matches.first().cloned() else {
            self.close_picker();
            return Ok(());
        };

        match candidate {
            OpenCandidate::OpenBuffer(candidate) => {
                if let Some(index) = self.workspace.find_document_index(&candidate.path) {
                    self.make_document_current(index);
                }
            }
            OpenCandidate::ProjectFile(candidate) => {
                self.open_document(candidate.path)?;
            }
        }

        self.close_picker();
        self.refresh_picker_candidates()?;
        Ok(())
    }

    fn cycle_search_scope(&mut self) {
        self.search_input.scope = match self.search_input.scope {
            SearchScope::CurrentFile => SearchScope::OpenBuffers,
            SearchScope::OpenBuffers => SearchScope::Project,
            SearchScope::Project => SearchScope::CurrentFile,
        };
    }

    fn incremental_search_current_file(&mut self) {
        if !self.search_input.active
            || self.search_input.scope != SearchScope::CurrentFile
            || self.search_input.value.is_empty()
            || !self.workspace.has_documents()
        {
            return;
        }

        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Ok(Some((document_index, row, column))) =
            self.search_current_file(&self.search_input.value, width as usize)
        {
            self.make_document_current(document_index);
            self.cursor.column = column;
            self.jump_with_context(row, width as usize);
        }
    }

    fn close_search_input(&mut self) {
        self.search_input.active = false;
        self.search_input.value.clear();
        self.search_input.scope = SearchScope::CurrentFile;
    }

    fn submit_search_input(&mut self) -> Result<()> {
        let query = self.search_input.value.clone();
        if query.is_empty() {
            self.close_search_input();
            return Ok(());
        }

        let Ok((width, _)) = terminal::size() else {
            self.close_search_input();
            return Ok(());
        };

        let result = match self.search_input.scope {
            SearchScope::CurrentFile => self.search_current_file(&query, width as usize)?,
            SearchScope::OpenBuffers => self.search_open_buffers(&query, width as usize)?,
            SearchScope::Project => self.search_project_files(&query, width as usize)?,
        };

        if let Some((document_index, row, column)) = result {
            if document_index != self.workspace.current_index {
                self.make_document_current(document_index);
            }
            self.push_jump_history();
            self.cursor.column = column;
            self.last_search = Some(SearchState {
                query,
                scope: self.search_input.scope,
            });
            self.jump_with_context(row, width as usize);
        }

        self.close_search_input();
        Ok(())
    }

    fn search_current_file(&self, query: &str, page_width: usize) -> Result<Option<(usize, usize, usize)>> {
        if !self.workspace.has_documents() {
            return Ok(None);
        }
        Ok(self
            .workspace
            .current_document()
            .first_match_position(query, page_width)
            .map(|(row, column)| (self.workspace.current_index, row, column)))
    }

    fn search_open_buffers(&self, query: &str, page_width: usize) -> Result<Option<(usize, usize, usize)>> {
        if !self.workspace.has_documents() {
            return Ok(None);
        }
        for (index, entry) in self.workspace.documents.iter().enumerate() {
            if let Some((row, column)) = entry.document.first_match_position(query, page_width) {
                return Ok(Some((index, row, column)));
            }
        }

        Ok(None)
    }

    fn search_project_files(&mut self, query: &str, page_width: usize) -> Result<Option<(usize, usize, usize)>> {
        if self.workspace.has_documents() {
        for (index, entry) in self.workspace.documents.iter().enumerate() {
            if let Some((row, column)) = entry.document.first_match_position(query, page_width) {
                return Ok(Some((index, row, column)));
            }
        }
        }

        for path in collect_project_search_paths()? {
            if self
                .workspace
                .documents
                .iter()
                .any(|entry| entry.path == path)
            {
                continue;
            }

            if let Some((line_number, column)) = first_matching_line_number(&path, query)? {
                self.open_document(path.clone())?;
                if let Some(row) = self.workspace.current_document().jump_row_for_line_number(line_number, page_width) {
                    return Ok(Some((self.workspace.current_index, row, column)));
                }
            }
        }

        Ok(None)
    }

    fn repeat_search_forward(&mut self) -> Result<()> {
        let Some(search_state) = self.last_search.clone() else {
            return Ok(());
        };
        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };
        let page_width = width as usize;

        match search_state.scope {
            SearchScope::CurrentFile => {
                if let Some((row, column)) = self.workspace.current_document().next_match_position(
                    &search_state.query,
                    self.cursor.row,
                    self.cursor.column.saturating_add(1),
                    page_width,
                ) {
                    self.push_jump_history();
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: true });
                }
            }
            SearchScope::OpenBuffers => {
                if let Some((document_index, row, column)) = self.search_open_buffers_from(
                    &search_state.query,
                    self.workspace.current_index,
                    self.cursor.row,
                    self.cursor.column.saturating_add(1),
                    page_width,
                    true,
                )? {
                    self.push_jump_history();
                    self.make_document_current(document_index);
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: true });
                }
            }
            SearchScope::Project => {
                if let Some((document_index, row, column)) = self.search_project_from(
                    &search_state.query,
                    self.workspace.current_index,
                    self.cursor.row,
                    self.cursor.column.saturating_add(1),
                    page_width,
                    true,
                )? {
                    self.push_jump_history();
                    self.make_document_current(document_index);
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: true });
                }
            }
        }

        Ok(())
    }

    fn repeat_search_backward(&mut self) -> Result<()> {
        let Some(search_state) = self.last_search.clone() else {
            return Ok(());
        };
        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };
        let page_width = width as usize;

        match search_state.scope {
            SearchScope::CurrentFile => {
                if let Some((row, column)) = self.workspace.current_document().previous_match_position(
                    &search_state.query,
                    self.cursor.row,
                    self.cursor.column,
                    page_width,
                ) {
                    self.push_jump_history();
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: false });
                }
            }
            SearchScope::OpenBuffers => {
                if let Some((document_index, row, column)) = self.search_open_buffers_from(
                    &search_state.query,
                    self.workspace.current_index,
                    self.cursor.row,
                    self.cursor.column,
                    page_width,
                    false,
                )? {
                    self.push_jump_history();
                    self.make_document_current(document_index);
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: false });
                }
            }
            SearchScope::Project => {
                if let Some((document_index, row, column)) = self.search_project_from(
                    &search_state.query,
                    self.workspace.current_index,
                    self.cursor.row,
                    self.cursor.column,
                    page_width,
                    false,
                )? {
                    self.push_jump_history();
                    self.make_document_current(document_index);
                    self.cursor.column = column;
                    self.jump_with_context(row, page_width);
                    self.last_replayable_action = Some(ReplayableAction::Search { forward: false });
                }
            }
        }

        Ok(())
    }

    fn search_open_buffers_from(
        &self,
        query: &str,
        start_document_index: usize,
        start_row: usize,
        start_column: usize,
        page_width: usize,
        forward: bool,
    ) -> Result<Option<(usize, usize, usize)>> {
        if forward {
            for (index, entry) in self.workspace.documents.iter().enumerate().skip(start_document_index) {
                let start = if index == start_document_index {
                    entry.document.next_match_position(query, start_row, start_column, page_width)
                } else {
                    entry.document.first_match_position(query, page_width)
                };
                if let Some((row, column)) = start {
                    return Ok(Some((index, row, column)));
                }
            }
        } else {
            for index in (0..=start_document_index).rev() {
                let entry = &self.workspace.documents[index];
                let found = if index == start_document_index {
                    entry
                        .document
                        .previous_match_position(query, start_row, start_column, page_width)
                } else {
                    last_match_in_document(&entry.document, query, page_width)
                };
                if let Some((row, column)) = found {
                    return Ok(Some((index, row, column)));
                }
            }
        }

        Ok(None)
    }

    fn search_project_from(
        &mut self,
        query: &str,
        start_document_index: usize,
        start_row: usize,
        start_column: usize,
        page_width: usize,
        forward: bool,
    ) -> Result<Option<(usize, usize, usize)>> {
        if let Some(found) = self.search_open_buffers_from(
            query,
            start_document_index,
            start_row,
            start_column,
            page_width,
            forward,
        )? {
            return Ok(Some(found));
        }

        if !forward {
            return Ok(None);
        }

        for path in collect_project_search_paths()? {
            if self.workspace.documents.iter().any(|entry| entry.path == path) {
                continue;
            }

            if let Some((line_number, column)) = first_matching_line_number(&path, query)? {
                self.open_document(path.clone())?;
                if let Some(row) = self
                    .workspace
                    .current_document()
                    .jump_row_for_line_number(line_number, page_width)
                {
                    return Ok(Some((self.workspace.current_index, row, column)));
                }
            }
        }

        Ok(None)
    }

    fn move_cursor_up(&mut self) {
        self.cursor.row = self.cursor.row.saturating_sub(1);
        self.clamp_vertical_state();
        self.clamp_cursor_column_to_current_line();
    }

    fn move_cursor_left(&mut self) {
        self.cursor.column = self.cursor.column.saturating_sub(1);
    }

    fn move_cursor_down(&mut self) {
        self.cursor.row = self.cursor.row.saturating_add(1);
        self.clamp_vertical_state();
        self.clamp_cursor_column_to_current_line();
    }

    fn move_cursor_right(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        let Ok(line_width) = self
            .workspace
            .current_document()
            .display_line_width(self.cursor.row, width as usize)
        else {
            return;
        };

        self.cursor.column = self
            .cursor
            .column
            .saturating_add(1)
            .min(line_width);
    }

    fn move_cursor_to_line_start(&mut self) {
        self.cursor.column = 0;
    }

    fn move_cursor_to_line_end(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        let Ok(line_width) = self
            .workspace
            .current_document()
            .display_line_width(self.cursor.row, width as usize)
        else {
            return;
        };

        self.cursor.column = line_width;
    }

    fn page_down_half(&mut self) {
        let step = self.page_step() / 2;
        let previous_viewport_row = self.viewport_row;
        self.viewport_row = self.viewport_row.saturating_add(step.max(1));
        self.clamp_to_document_bounds();
        if self.viewport_row > previous_viewport_row {
            self.cursor.row = self.cursor.row.max(self.viewport_row);
        }
    }

    fn page_down_full(&mut self) {
        let step = self.page_step();
        let previous_viewport_row = self.viewport_row;
        self.viewport_row = self.viewport_row.saturating_add(step.max(1));
        self.clamp_to_document_bounds();
        if self.viewport_row > previous_viewport_row {
            self.cursor.row = self.cursor.row.max(self.viewport_row);
        }
    }

    fn page_up_half(&mut self) {
        let step = self.page_step() / 2;
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

    fn page_up_full(&mut self) {
        let step = self.page_step();
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

    fn page_step(&self) -> usize {
        terminal::size()
            .map(|(_, height)| height.saturating_sub(1) as usize)
            .unwrap_or(24)
            .max(1)
    }

    fn sync_viewport_after_cursor_move(&mut self) {
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

    fn clamp_vertical_state(&mut self) {
        self.clamp_to_document_bounds();
        self.sync_viewport_after_cursor_move();
    }

    fn clamp_cursor_column_to_current_line(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        let Ok(line_width) = self
            .workspace
            .current_document()
            .display_line_width(self.cursor.row, width as usize)
        else {
            return;
        };

        self.cursor.column = self.cursor.column.min(line_width);
    }

    fn clamp_to_document_bounds(&mut self) {
        if let Ok((width, _)) = terminal::size() {
            if let Some(total_rows) = self.workspace.current_document().total_rows(width as usize) {
                let visible_height = self.page_step();
                let last_row = total_rows.saturating_sub(1);
                let max_viewport_row = total_rows.saturating_sub(visible_height);

                self.cursor.row = self.cursor.row.min(last_row);
                self.viewport_row = self.viewport_row.min(max_viewport_row);
            }
        }
    }

    fn jump_to_top(&mut self) {
        self.push_jump_history();
        self.workspace.current_document_mut().jump_to_top();
        self.viewport_row = 0;
        self.cursor.row = 0;
    }

    fn jump_to_bottom(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        let visible_height = self.page_step();
        self.push_jump_history();
        if let Ok(Some(start_row)) = self
            .workspace
            .current_document_mut()
            .jump_to_bottom(visible_height, width as usize)
        {
            self.viewport_row = start_row;
            self.cursor.row = start_row.saturating_add(visible_height.saturating_sub(1));
            return;
        }

        let Some(total_rows) = self.workspace.current_document().total_rows(width as usize) else {
            return;
        };

        self.cursor.row = total_rows.saturating_sub(1);
        self.viewport_row = total_rows.saturating_sub(visible_height);
    }

    fn jump_to_next_git_marker(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some(row) = self
            .workspace
            .current_document()
            .next_git_marker_row(self.cursor.row, width as usize)
        {
            self.push_jump_history();
            self.jump_with_context(row, width as usize);
        }
    }

    fn jump_to_previous_git_marker(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some(row) = self
            .workspace
            .current_document()
            .previous_git_marker_row(self.cursor.row, width as usize)
        {
            self.push_jump_history();
            self.jump_with_context(row, width as usize);
        }
    }

    fn jump_to_next_diagnostic(&mut self, error_only: bool) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some(row) = self
            .workspace
            .current_document()
            .next_diagnostic_row(self.cursor.row, width as usize, error_only)
        {
            self.push_jump_history();
            self.jump_with_context(row, width as usize);
            self.last_replayable_action = Some(ReplayableAction::Diagnostic {
                error_only,
                forward: true,
            });
        }
    }

    fn jump_to_previous_diagnostic(&mut self, error_only: bool) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some(row) = self
            .workspace
            .current_document()
            .previous_diagnostic_row(self.cursor.row, width as usize, error_only)
        {
            self.push_jump_history();
            self.jump_with_context(row, width as usize);
            self.last_replayable_action = Some(ReplayableAction::Diagnostic {
                error_only,
                forward: false,
            });
        }
    }

    fn refresh_rust_diagnostics(&mut self) -> Result<DiagnosticSummary> {
        let diagnostics_by_path = load_rust_diagnostics(Path::new("."))?;
        let mut summary = DiagnosticSummary::default();

        for entry in &mut self.workspace.documents {
            let diagnostics = diagnostics_by_path
                .get(&entry.path)
                .cloned()
                .unwrap_or_default();
            entry.document.set_rust_diagnostics(diagnostics);
            let document_summary = entry.document.diagnostic_summary();
            summary.errors += document_summary.errors;
            summary.warnings += document_summary.warnings;
        }

        Ok(summary)
    }

    fn jump_with_context(&mut self, target_row: usize, page_width: usize) {
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

    fn leave_insert_mode(&mut self, rewind_cursor: bool) {
        self.workspace.current_document_mut().end_undo_group();
        self.mode = Mode::Normal;
        self.pending_insert_j = None;
        if rewind_cursor {
            self.cursor.column = self.cursor.column.saturating_sub(1);
        }
    }

    fn save_current_document(&mut self) -> Result<()> {
        let Some(path) = self.workspace.current_document_path().map(ToOwned::to_owned) else {
            return Ok(());
        };
        self.workspace.current_document_mut().save(&path)?;

        if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            && self.rust_support.rust_analyzer_available
            && self.rust_support.cargo_manifest_in_cwd
        {
            let summary = self.refresh_rust_diagnostics()?;
            self.last_save_feedback = Some(format!("rust-analyzer E{} W{}", summary.errors, summary.warnings));
        } else {
            self.last_save_feedback = Some("saved".to_owned());
        }

        Ok(())
    }

    fn close_current_buffer(&mut self) {
        if !self.workspace.has_documents() {
            return;
        }

        self.save_current_buffer_view_state();
        self.workspace.close_current();
        let _ = self.refresh_picker_candidates();
        if !self.workspace.has_documents() {
            self.cursor = CursorState { row: 0, column: 0 };
            self.viewport_row = 0;
            self.layout_mode = LayoutMode::Single;
            self.focused_pane = FocusedPane::Left;
            self.mode = Mode::Normal;
        } else {
            self.restore_current_buffer_view_state();
        }
    }

    fn advance_layout_or_focus(&mut self) {
        if self.layout_mode == LayoutMode::Single {
            self.layout_mode = LayoutMode::Dual;
            self.focused_pane = FocusedPane::Left;
            return;
        }

        self.focused_pane = match self.focused_pane {
            FocusedPane::Left => FocusedPane::Right,
            FocusedPane::Right => FocusedPane::Left,
        };

        if self.layout_mode == LayoutMode::Dual && self.focused_pane == FocusedPane::Right {
            if let Some(other_index) = self.workspace.secondary_index() {
                self.select_current_document(other_index);
            } else {
                self.focused_pane = FocusedPane::Left;
            }
        } else if self.layout_mode == LayoutMode::Dual
            && self.focused_pane == FocusedPane::Left
            && self.workspace.current_index != 0
        {
            self.select_current_document(0);
        }
    }

    fn toggle_terminal_split(&mut self) {
        self.layout_mode = match self.layout_mode {
            LayoutMode::TerminalSplit => LayoutMode::Single,
            _ => LayoutMode::TerminalSplit,
        };
        self.focused_pane = FocusedPane::Right;
    }

    fn collapse_to_single_pane(&mut self) {
        if self.layout_mode == LayoutMode::Dual && self.focused_pane == FocusedPane::Right {
            if let Some(other_index) = self.workspace.secondary_index() {
                self.select_current_document(other_index);
            }
        }

        self.layout_mode = LayoutMode::Single;
        self.focused_pane = FocusedPane::Left;
    }

    fn insert_char(&mut self, ch: char) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some((row, column)) = self.workspace.current_document_mut().insert_char(
            self.cursor.row,
            self.cursor.column,
            width as usize,
            ch,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn insert_newline(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some((row, column)) = self.workspace.current_document_mut().insert_newline(
            self.cursor.row,
            self.cursor.column,
            width as usize,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn insert_tab(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some((row, column)) = self.workspace.current_document_mut().insert_tab(
            self.cursor.row,
            self.cursor.column,
            width as usize,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn backspace_char(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some((row, column)) = self.workspace.current_document_mut().backspace(
            self.cursor.row,
            self.cursor.column,
            width as usize,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn delete_forward_char(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some((row, column)) = self.workspace.current_document_mut().delete_forward(
            self.cursor.row,
            self.cursor.column,
            width as usize,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn insert_escape_timeout() -> Duration {
    Duration::from_millis(300)
}

fn motion_destination_column(find_kind: FindKind, found_column: usize) -> usize {
    match find_kind {
        FindKind::Forward => found_column.saturating_add(1),
        FindKind::Backward => found_column,
        FindKind::TillForward => found_column,
        FindKind::TillBackward => found_column.saturating_add(1),
    }
}

fn invert_find_kind(find_kind: FindKind, reverse: bool) -> FindKind {
    if !reverse {
        return find_kind;
    }

    match find_kind {
        FindKind::Forward => FindKind::Backward,
        FindKind::Backward => FindKind::Forward,
        FindKind::TillForward => FindKind::TillBackward,
        FindKind::TillBackward => FindKind::TillForward,
    }
}

fn operator_range(
    cursor_row: usize,
    cursor_column: usize,
    found_row: usize,
    found_column: usize,
    find_kind: FindKind,
) -> Option<(usize, usize, usize, usize)> {
    let (start_row, start_column, end_row, end_column) = match find_kind {
        FindKind::Forward => (
            cursor_row,
            cursor_column,
            found_row,
            found_column.saturating_add(1),
        ),
        FindKind::TillForward => (cursor_row, cursor_column, found_row, found_column),
        FindKind::Backward => (
            found_row,
            found_column,
            cursor_row,
            cursor_column,
        ),
        FindKind::TillBackward => (
            found_row,
            found_column.saturating_add(1),
            cursor_row,
            cursor_column,
        ),
    };

    (start_row < end_row || end_column > start_column)
        .then_some((start_row, start_column, end_row, end_column))
}

fn first_matching_line_number(path: &Path, query: &str) -> Result<Option<(usize, usize)>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return Err(error.into()),
    };
    let reader = BufReader::new(file);

    for (index, line) in reader.lines().enumerate() {
        let Ok(line) = line else {
            return Ok(None);
        };
        if let Some(column) = line.find(query) {
            return Ok(Some((index + 1, column)));
        }
    }

    Ok(None)
}

fn last_match_in_document(document: &Document, query: &str, page_width: usize) -> Option<(usize, usize)> {
    let total_rows = document.total_rows(page_width)?;
    document.previous_match_position(query, total_rows.saturating_sub(1), usize::MAX, page_width)
}

fn rust_analyzer_available() -> bool {
    Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn load_rust_diagnostics(project_root: &Path) -> Result<HashMap<PathBuf, HashMap<usize, DiagnosticSeverity>>> {
    let output = Command::new("rust-analyzer")
        .current_dir(project_root)
        .args(["-q", "diagnostics", ".", "--severity", "warning"])
        .output()?;

    if !output.status.success() {
        return Ok(HashMap::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut diagnostics_by_path: HashMap<PathBuf, HashMap<usize, DiagnosticSeverity>> = HashMap::new();

    for line in stdout.lines() {
        let Some((path, line_number, severity)) = parse_rust_analyzer_diagnostic_line(line) else {
            continue;
        };

        let entry = diagnostics_by_path.entry(path).or_default();
        match entry.get(&line_number).copied() {
            Some(DiagnosticSeverity::Error) => {}
            Some(DiagnosticSeverity::Warning) if severity == DiagnosticSeverity::Warning => {}
            _ => {
                entry.insert(line_number, severity);
            }
        }
    }

    Ok(diagnostics_by_path)
}

fn parse_rust_analyzer_diagnostic_line(line: &str) -> Option<(PathBuf, usize, DiagnosticSeverity)> {
    if !line.starts_with("at crate ") {
        return None;
    }

    let file_marker = ", file ";
    let file_start = line.find(file_marker)? + file_marker.len();
    let severity_marker = ": ";
    let severity_start = line[file_start..].find(severity_marker)? + file_start + severity_marker.len();

    let path = PathBuf::from(line[file_start..severity_start - severity_marker.len()].trim());
    let remainder = &line[severity_start..];
    let severity = if remainder.starts_with("Error") {
        DiagnosticSeverity::Error
    } else if remainder.starts_with("Warning") || remainder.starts_with("WeakWarning") {
        DiagnosticSeverity::Warning
    } else {
        return None;
    };

    let line_marker = "line: ";
    let line_pos = remainder.find(line_marker)? + line_marker.len();
    let line_digits: String = remainder[line_pos..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    let line_number: usize = line_digits.parse().ok()?;

    Some((path, line_number.saturating_add(1), severity))
}

#[allow(dead_code)]
fn _project_file_display_name(candidate: &ProjectFileCandidate) -> &str {
    &candidate.display_name
}
