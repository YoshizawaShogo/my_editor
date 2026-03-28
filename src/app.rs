use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use crossterm::{event, terminal};

use crate::{
    config,
    document::{
        DiagnosticEntry, DiagnosticSeverity, DiagnosticSummary, Document, ScratchDocument,
        ScratchRow, ScratchTarget,
    },
    error::Result,
    mode::Mode,
    open_candidate::{
        OpenCandidate, ProjectFileCandidate, collect_project_file_candidates,
    },
    picker_match,
};

mod action;
mod keymap;
mod navigation;
mod render;
mod search;
mod terminal_session;
mod workspace;

use self::action::{FindKind, PendingNormalAction, PendingOperator, ReplayableAction};
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
    pub diagnostic_popup: DiagnosticPopupState,
    pub last_search: Option<SearchState>,
    pub yank_buffer: YankBuffer,
    pub jump_history: Vec<JumpPosition>,
    pub layout_mode: LayoutMode,
    pub focused_pane: FocusedPane,
    pub rust_support: RustSupportState,
    pub last_save_feedback: Option<String>,
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

pub struct DiagnosticPopupState {
    pub active: bool,
    pub lines: Vec<String>,
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
            diagnostic_popup: DiagnosticPopupState {
                active: false,
                lines: Vec::new(),
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

    fn current_page_width(&self) -> usize {
        let Ok((terminal_width, _)) = terminal::size() else {
            return 80;
        };

        match self.layout_mode {
            LayoutMode::Single => terminal_width.max(1) as usize,
            LayoutMode::Dual | LayoutMode::TerminalSplit => {
                let usable_width = terminal_width.saturating_sub(1) as usize;
                let left_width = (usable_width / 2).max(1);
                let right_width = usable_width.saturating_sub(left_width).max(1);
                match self.focused_pane {
                    FocusedPane::Left => left_width,
                    FocusedPane::Right => right_width,
                }
            }
        }
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

        let page_width = self.current_page_width();
        self.workspace.current_document_mut().begin_undo_group();
        let Some((row, column)) = self.workspace.current_document_mut().remove_display_range(
            start_row,
            start_column,
            end_row,
            end_column,
            page_width,
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
        let page_width = self.current_page_width();
        let document = self.workspace.current_document();
        let total_rows = document.total_rows(page_width).unwrap_or(0);
        if total_rows == 0 {
            return Ok(());
        }

        let normalized_end_row = end_row.min(total_rows.saturating_sub(1));
        let mut collected = String::new();

        for row in start_row..=normalized_end_row {
            let line_text = document.display_line_text(row, page_width)?;
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
        let page_width = self.current_page_width();
        let document = self.workspace.current_document();
        let total_rows = document.total_rows(page_width).unwrap_or(0);
        if total_rows == 0 {
            return Ok(None);
        }

        match find_kind {
            FindKind::Forward | FindKind::TillForward => {
                for row in self.cursor.row..total_rows {
                    let line_text = document.display_line_text(row, page_width)?;
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
                    let line_text = document.display_line_text(row, page_width)?;
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

    fn paste_after_cursor(&mut self) -> Result<()> {
        self.workspace.current_document_mut().begin_undo_group();

        match self.yank_buffer_clone() {
            YankBuffer::Empty => {}
            YankBuffer::Charwise(yank_text) => {
                let page_width = self.current_page_width();
                let line_width = self
                    .workspace
                    .current_document()
                    .display_line_width(self.cursor.row, page_width)?;
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
        let page_width = self.current_page_width();
        for ch in text.chars() {
            let next_position = if ch == '\n' {
                self.workspace
                    .current_document_mut()
                    .insert_newline(row, column, page_width)
            } else {
                self.workspace
                    .current_document_mut()
                    .insert_char(row, column, page_width, ch)
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
        let page_width = self.current_page_width();
        self.workspace.current_document_mut().begin_undo_group();
        if let Some((row, column)) = self
            .workspace
            .current_document_mut()
            .open_below(self.cursor.row, page_width)
        {
            self.cursor.row = row;
            self.cursor.column = column;
            self.mode = Mode::Insert;
            self.pending_insert_j = None;
            self.clamp_vertical_state();
        }
    }

    fn open_line_below_with_text(&mut self, text: &str) {
        let page_width = self.current_page_width();
        if let Some((row, column)) = self
            .workspace
            .current_document_mut()
            .open_below(self.cursor.row, page_width)
        {
            self.cursor.row = row;
            self.cursor.column = column;
            self.insert_text_at(row, column, text);
        }
    }

    fn yank_current_line(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        if let Some(line_text) = self
            .workspace
            .current_document()
            .current_line_text(self.cursor.row, page_width)
        {
            self.yank_buffer = YankBuffer::Linewise(line_text);
        }

        Ok(())
    }

    fn delete_current_line(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        self.workspace.current_document_mut().begin_undo_group();
        if let Some((line_text, (row, column))) = self
            .workspace
            .current_document_mut()
            .delete_current_line(self.cursor.row, page_width)
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
        let page_width = self.current_page_width();
        self.workspace.current_document_mut().begin_undo_group();
        if let Some((line_text, (row, column))) = self
            .workspace
            .current_document_mut()
            .clear_current_line(self.cursor.row, page_width)
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

    fn open_current_diagnostic_popup(&mut self) {
        if !self.workspace.has_documents() {
            return;
        }

        let diagnostics = self
            .workspace
            .current_document()
            .diagnostics_for_display_row(self.cursor.row, self.current_page_width());
        if diagnostics.is_empty() {
            self.close_diagnostic_popup();
            return;
        }

        self.diagnostic_popup.active = true;
        self.diagnostic_popup.lines = diagnostics
            .into_iter()
            .map(|entry| format!("{} {}", diagnostic_label(entry.severity), entry.message))
            .collect();
    }

    fn close_diagnostic_popup(&mut self) {
        self.diagnostic_popup.active = false;
        self.diagnostic_popup.lines.clear();
    }

    fn open_diagnostic_list(&mut self, error_only: bool) {
        let mut rows = Vec::new();

        for entry in &self.workspace.documents {
            if entry.document.is_scratch() {
                continue;
            }

            for (line_number, diagnostics) in entry.document.collect_diagnostics() {
                for diagnostic in diagnostics {
                    if error_only && diagnostic.severity != DiagnosticSeverity::Error {
                        continue;
                    }

                    rows.push(ScratchRow {
                        text: format!(
                            "{:<7} {}:{}:{} {}",
                            diagnostic_label(diagnostic.severity),
                            entry.path.display(),
                            line_number,
                            1,
                            diagnostic.message
                        ),
                        target: Some(ScratchTarget {
                            path: entry.path.clone(),
                            line_number,
                            column: 0,
                        }),
                    });
                }
            }
        }

        let title = if error_only {
            "[diagnostics] errors"
        } else {
            "[diagnostics] warnings+errors"
        };

        self.save_current_buffer_view_state();
        self.workspace.documents.insert(
            0,
            DocumentEntry {
                path: PathBuf::from(title),
                document: Document::Scratch(ScratchDocument::new(title, rows)),
                view_state: BufferViewState::default(),
            },
        );
        self.workspace.current_index = 0;
        self.restore_current_buffer_view_state();
        self.close_diagnostic_popup();
    }

    fn open_scratch_target_under_cursor(&mut self) -> Result<()> {
        let Some(target) = self
            .workspace
            .current_document()
            .scratch_target_at_row(self.cursor.row)
        else {
            return Ok(());
        };

        self.push_jump_history();
        if let Some(index) = self.workspace.find_document_index(&target.path) {
            self.make_document_current(index);
        } else {
            self.open_document(target.path.clone())?;
        }

        if let Some(row) = self
            .workspace
            .current_document()
            .jump_row_for_line_number(target.line_number, self.current_page_width())
        {
            self.cursor.column = target.column;
            self.jump_with_context(row, self.current_page_width());
        }

        Ok(())
    }

    fn submit_go_input(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        if let Ok(line_number) = self.go_input.value.parse::<usize>() {
            if let Some(row) = self
                .workspace
                .current_document()
                .jump_row_for_line_number(line_number, page_width)
            {
                self.push_jump_history();
                self.cursor.column = 0;
                self.jump_with_context(row, page_width);
            }
        }

        self.close_go_input();
        Ok(())
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

    fn refresh_rust_diagnostics(&mut self) -> Result<DiagnosticSummary> {
        let diagnostics_by_path = load_rust_diagnostics(Path::new("."))?;
        let mut summary = DiagnosticSummary::default();

        for entry in &mut self.workspace.documents {
            if entry.document.is_scratch() {
                continue;
            }
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
        let page_width = self.current_page_width();
        if let Some((row, column)) = self.workspace.current_document_mut().insert_char(
            self.cursor.row,
            self.cursor.column,
            page_width,
            ch,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn insert_newline(&mut self) {
        let page_width = self.current_page_width();
        if let Some((row, column)) = self.workspace.current_document_mut().insert_newline(
            self.cursor.row,
            self.cursor.column,
            page_width,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn insert_tab(&mut self) {
        let page_width = self.current_page_width();
        if let Some((row, column)) = self.workspace.current_document_mut().insert_tab(
            self.cursor.row,
            self.cursor.column,
            page_width,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn backspace_char(&mut self) {
        let page_width = self.current_page_width();
        if let Some((row, column)) = self.workspace.current_document_mut().backspace(
            self.cursor.row,
            self.cursor.column,
            page_width,
        ) {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn delete_forward_char(&mut self) {
        let page_width = self.current_page_width();
        if let Some((row, column)) = self.workspace.current_document_mut().delete_forward(
            self.cursor.row,
            self.cursor.column,
            page_width,
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

fn rust_analyzer_available() -> bool {
    Command::new("rust-analyzer")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn load_rust_diagnostics(
    project_root: &Path,
) -> Result<HashMap<PathBuf, HashMap<usize, Vec<DiagnosticEntry>>>> {
    let output = Command::new("rust-analyzer")
        .current_dir(project_root)
        .args(["-q", "diagnostics", ".", "--severity", "warning"])
        .output()?;

    if !output.status.success() {
        return Ok(HashMap::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut diagnostics_by_path: HashMap<PathBuf, HashMap<usize, Vec<DiagnosticEntry>>> =
        HashMap::new();

    for line in stdout.lines() {
        let Some((path, line_number, diagnostic)) = parse_rust_analyzer_diagnostic_line(line) else {
            continue;
        };

        diagnostics_by_path
            .entry(path)
            .or_default()
            .entry(line_number)
            .or_default()
            .push(diagnostic);
    }

    Ok(diagnostics_by_path)
}

fn parse_rust_analyzer_diagnostic_line(line: &str) -> Option<(PathBuf, usize, DiagnosticEntry)> {
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

    Some((
        path,
        line_number.saturating_add(1),
        DiagnosticEntry {
            severity,
            message: remainder.trim().to_owned(),
        },
    ))
}

fn diagnostic_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Error => "Error",
    }
}

#[allow(dead_code)]
fn _project_file_display_name(candidate: &ProjectFileCandidate) -> &str {
    &candidate.display_name
}
