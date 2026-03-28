use std::{
    collections::HashSet,
    fs::File,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use crossterm::{event, terminal};
use lsp_types::{Location, Position, TextEdit};

use crate::{
    config,
    document::{
        DiagnosticSeverity, DiagnosticSummary, Document, ScratchDocument, ScratchRow,
        ScratchTarget,
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
mod lsp;
mod navigation;
mod replace;
mod render;
mod search;
mod shell;
mod terminal_session;
mod workspace;

use self::action::{FindKind, PendingNormalAction, PendingOperator, ReplayableAction};
use self::lsp::{
    GotoKind, HoverPopupState, LspClientState, LspEvent, RenameInputState, RustLspClient,
    WorkspaceDiagnosticItem, uri_to_path,
};
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
    pub replace_input: ReplaceInputState,
    pub selection_input: SelectionInputState,
    pub diagnostic_popup: DiagnosticPopupState,
    pub last_search: Option<SearchState>,
    pub yank_buffer: YankBuffer,
    pub jump_history: Vec<JumpPosition>,
    pub jump_forward_history: Vec<JumpPosition>,
    pub layout_mode: LayoutMode,
    pub focused_pane: FocusedPane,
    pub last_save_feedback: Option<String>,
    pub hover_popup: HoverPopupState,
    pub rename_input: RenameInputState,
    pub lsp: LspClientState,
    pub toast: ToastState,
    pub workspace_diagnostics_cache: WorkspaceDiagnosticsCache,
}

pub struct Workspace {
    pub documents: Vec<DocumentEntry>,
    pub current_index: usize,
}

pub struct DocumentEntry {
    pub path: PathBuf,
    pub document: Document,
    pub view_state: BufferViewState,
    pub version: i32,
    pub lsp_open: bool,
}

pub struct PickerState {
    pub active: bool,
    pub query: String,
    pub candidates: Vec<OpenCandidate>,
    pub scope: PickerScope,
}

pub struct ShellState {
    pub program: String,
    pub parser: Option<vt100::Parser>,
    pub rows: u16,
    pub cols: u16,
    child: Option<Child>,
    pty: Option<File>,
    output_rx: Option<Receiver<Vec<u8>>>,
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

pub struct ReplaceInputState {
    pub active: bool,
    pub find: String,
    pub replace: String,
    pub scope: SearchScope,
    pub field: ReplaceField,
}

pub struct SelectionInputState {
    pub active: bool,
    pub operator: Option<PendingOperator>,
    pub ranges: Vec<DisplayRange>,
    pub current_index: usize,
}

#[derive(Clone, Copy)]
pub struct DisplayRange {
    pub start_row: usize,
    pub start_column: usize,
    pub end_row: usize,
    pub end_column: usize,
}

impl SelectionInputState {
    pub fn current_range(&self) -> Option<DisplayRange> {
        self.ranges.get(self.current_index).copied()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReplaceField {
    Find,
    Replace,
}

pub struct DiagnosticPopupState {
    pub active: bool,
    pub lines: Vec<String>,
}

pub struct ToastState {
    pub message: Option<String>,
    pub expires_at: Option<Instant>,
}

pub struct WorkspaceDiagnosticsCache {
    pub rust_files: Option<Vec<PathBuf>>,
    pub diagnostics: std::collections::HashMap<PathBuf, std::collections::HashMap<usize, Vec<crate::document::DiagnosticEntry>>>,
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

#[derive(Clone)]
pub struct JumpPosition {
    pub path: Option<PathBuf>,
    pub row: usize,
    pub column: usize,
    pub viewport_row: usize,
}

pub enum YankBuffer {
    Empty,
    Charwise(String),
    Linewise(String),
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
        let rust_analyzer_available = rust_analyzer_available();
        let cargo_manifest_in_cwd = Path::new("Cargo.toml").exists();
        let workspace = match path {
            Some(path) => {
                let path = normalize_workspace_path(path)?;
                Workspace {
                    documents: vec![DocumentEntry {
                        path: path.clone(),
                        document: Document::open(&path)?,
                        view_state: BufferViewState::default(),
                        version: 1,
                        lsp_open: false,
                    }],
                    current_index: 0,
                }
            }
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
                parser: None,
                rows: 0,
                cols: 0,
                child: None,
                pty: None,
                output_rx: None,
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
            replace_input: ReplaceInputState {
                active: false,
                find: String::new(),
                replace: String::new(),
                scope: SearchScope::CurrentFile,
                field: ReplaceField::Find,
            },
            selection_input: SelectionInputState {
                active: false,
                operator: None,
                ranges: Vec::new(),
                current_index: 0,
            },
            diagnostic_popup: DiagnosticPopupState {
                active: false,
                lines: Vec::new(),
            },
            last_search: None,
            yank_buffer: YankBuffer::Empty,
            jump_history: Vec::new(),
            jump_forward_history: Vec::new(),
            layout_mode: LayoutMode::Single,
            focused_pane: FocusedPane::Left,
            last_save_feedback: None,
            hover_popup: HoverPopupState {
                active: false,
                lines: Vec::new(),
            },
            rename_input: RenameInputState {
                active: false,
                value: String::new(),
            },
            lsp: if rust_analyzer_available && cargo_manifest_in_cwd {
                LspClientState::Inactive
            } else {
                LspClientState::NotAvailable
            },
            toast: ToastState {
                message: None,
                expires_at: None,
            },
            workspace_diagnostics_cache: WorkspaceDiagnosticsCache {
                rust_files: None,
                diagnostics: std::collections::HashMap::new(),
            },
        };
        let _ = app.ensure_lsp_for_current_document();
        app.refresh_picker_candidates()?;
        Ok(app)
    }

    pub fn run(&mut self) -> Result<()> {
        let mut terminal_session = TerminalSession::enter()?;

        loop {
            self.prune_toast();
            self.poll_lsp();
            self.poll_shell_output();
            self.sync_shell_size()?;
            self.render_frame(terminal_session.terminal())?;

            if event::poll(Duration::from_millis(50))? {
                if self.handle_event(event::read()?)? {
                    break;
                }
            }
        }

        self.shutdown_shell();
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
        let _ = self.ensure_lsp_for_current_document();
    }

    fn select_current_document(&mut self, index: usize) {
        self.save_current_buffer_view_state();
        self.workspace.select_current(index);
        self.restore_current_buffer_view_state();
        let _ = self.ensure_lsp_for_current_document();
    }

    fn open_document(&mut self, path: PathBuf) -> Result<()> {
        self.save_current_buffer_view_state();
        self.workspace.open_document(path)?;
        self.restore_current_buffer_view_state();
        self.ensure_lsp_for_current_document()?;
        Ok(())
    }

    fn ensure_lsp_for_current_document(&mut self) -> Result<()> {
        let Some(path) = self.workspace.current_document_path().map(ToOwned::to_owned) else {
            return Ok(());
        };
        if !is_rust_source_path(&path) {
            return Ok(());
        }

        if matches!(self.lsp, LspClientState::NotAvailable) {
            return Ok(());
        }

        let started_now = matches!(self.lsp, LspClientState::Inactive);
        if matches!(self.lsp, LspClientState::Inactive) {
            self.lsp = match RustLspClient::start(Path::new(".")) {
                Ok(client) => LspClientState::Ready(client),
                Err(error) => LspClientState::Failed(format!("{error:?}")),
            };
        }

        if started_now {
            let rust_docs = self
                .workspace
                .documents
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    if !is_rust_source_path(&entry.path) {
                        return None;
                    }
                    Some((index, entry.path.clone(), entry.document.full_text()?, entry.version))
                })
                .collect::<Vec<_>>();
            if let LspClientState::Ready(client) = &mut self.lsp {
                for (index, path, text, version) in rust_docs {
                    client.ensure_open(&path, version, &text)?;
                    let _ = client.did_save(&path, &text);
                    self.workspace.documents[index].lsp_open = true;
                }
            }
            let _ = self.refresh_workspace_diagnostic_cache();
        }

        self.ensure_current_document_open_for_lsp()
    }

    fn ensure_current_document_open_for_lsp(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        let Some(path) = self.workspace.current_document_path().map(ToOwned::to_owned) else {
            return Ok(());
        };
        if !is_rust_source_path(&path) {
            return Ok(());
        }

        let Some(text) = self.workspace.current_document().full_text() else {
            return Ok(());
        };
        let current_index = self.workspace.current_index;
        let version = self.workspace.documents[current_index].version;

        if let LspClientState::Ready(client) = &mut self.lsp {
            client.ensure_open(&path, version, &text)?;
            let _ = client.did_save(&path, &text);
            self.workspace.documents[current_index].lsp_open = true;
        }

        let _ = page_width;
        Ok(())
    }

    fn poll_lsp(&mut self) {
        let events = match &mut self.lsp {
            LspClientState::Ready(client) => {
                client.poll();
                client.take_events()
            }
            _ => Vec::new(),
        };

        for event in events {
            match event {
                LspEvent::PublishDiagnostics { path, diagnostics } => {
                    self.workspace_diagnostics_cache
                        .diagnostics
                        .insert(path.clone(), diagnostics.clone());
                    if let Some(index) = self.workspace.find_document_index(&path) {
                        self.workspace.documents[index]
                            .document
                            .set_rust_diagnostics(diagnostics);
                    }
                }
                LspEvent::PublishSemanticTokens { path, tokens } => {
                    if let Some(index) = self.workspace.find_document_index(&path) {
                        self.workspace.documents[index]
                            .document
                            .set_semantic_tokens(tokens);
                    }
                }
                LspEvent::WorkspaceDiagnosticsResult { error_only, items } => {
                    self.open_workspace_diagnostic_list(error_only, items);
                }
                LspEvent::GotoResult { kind, locations } => {
                    self.show_toast(format!("LSP {}", kind.title()));
                    let _ = self.open_location_results(kind.title(), locations);
                }
                LspEvent::ReferencesResult { locations } => {
                    self.show_toast("LSP [references]");
                    let _ = self.open_location_results("[references]", locations);
                }
                LspEvent::HoverResult { lines } => {
                    if self.hover_popup.active {
                        self.hover_popup.lines = if lines.is_empty() {
                            vec!["No information.".to_owned()]
                        } else {
                            lines
                        };
                    }
                }
                LspEvent::RenameResult { edit } => {
                    self.show_toast("LSP rename");
                    if let Some(edit) = edit {
                        let _ = self.apply_workspace_edit(edit);
                    }
                }
                LspEvent::SelectionRangeResult { operator, ranges } => {
                    let _ = self.open_selection_input(operator, ranges);
                }
                LspEvent::Failed(message) => {
                    self.last_save_feedback = Some(message.clone());
                    self.lsp = LspClientState::Failed(message);
                }
            }
        }
    }

    fn current_diagnostic_summary(&self) -> DiagnosticSummary {
        let mut summary = DiagnosticSummary::default();
        for entry in &self.workspace.documents {
            let document_summary = entry.document.diagnostic_summary();
            summary.errors += document_summary.errors;
            summary.warnings += document_summary.warnings;
        }
        summary
    }

    fn sync_current_document_save(&mut self) {
        let Some(path) = self.workspace.current_document_path().map(ToOwned::to_owned) else {
            return;
        };
        if !is_rust_source_path(&path) {
            return;
        }
        if self.ensure_lsp_for_current_document().is_err() {
            return;
        }
        let Some(text) = self.workspace.current_document().full_text() else {
            return;
        };
        if let LspClientState::Ready(client) = &mut self.lsp {
            let _ = client.did_save(&path, &text);
        }
    }

    fn sync_current_document_close(&mut self) {
        let Some(entry) = self.workspace.documents.get(self.workspace.current_index) else {
            return;
        };
        if !entry.lsp_open || !is_rust_source_path(&entry.path) {
            return;
        }
        if let LspClientState::Ready(client) = &mut self.lsp {
            let _ = client.did_close(&entry.path);
        }
    }

    fn refresh_workspace_diagnostic_cache(&mut self) -> Result<()> {
        self.ensure_workspace_rust_files()?;

        let Some(rust_files) = self.workspace_diagnostics_cache.rust_files.clone() else {
            return Ok(());
        };

        let LspClientState::Ready(client) = &mut self.lsp else {
            return Ok(());
        };

        for path in rust_files {
            if let Some(index) = self.workspace.find_document_index(&path) {
                if let Some(text) = self.workspace.documents[index].document.full_text() {
                    let version = self.workspace.documents[index].version;
                    client.ensure_open(&path, version, &text)?;
                    let _ = client.did_save(&path, &text);
                    self.workspace.documents[index].lsp_open = true;
                }
                continue;
            }

            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            client.ensure_open(&path, 1, &text)?;
            let _ = client.did_save(&path, &text);
        }

        Ok(())
    }

    fn ensure_workspace_rust_files(&mut self) -> Result<()> {
        if self.workspace_diagnostics_cache.rust_files.is_some() {
            return Ok(());
        }

        let src_dir = normalize_workspace_path(Path::new("src"))?;
        let mut rust_files = Vec::new();
        collect_rust_files_under(&src_dir, &mut rust_files)?;
        rust_files.sort();
        self.workspace_diagnostics_cache.rust_files = Some(rust_files);
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

        self.last_replayable_action = Some(action);

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

    fn paste_before_cursor(&mut self) -> Result<()> {
        self.workspace.current_document_mut().begin_undo_group();

        match self.yank_buffer_clone() {
            YankBuffer::Empty => {}
            YankBuffer::Charwise(yank_text) => {
                self.insert_text_at(self.cursor.row, self.cursor.column, &yank_text);
            }
            YankBuffer::Linewise(line_text) => {
                self.open_line_above_with_text(&line_text);
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

    fn open_line_above_with_text(&mut self, text: &str) {
        let page_width = self.current_page_width();
        if let Some((row, column)) = self
            .workspace
            .current_document_mut()
            .open_above(self.cursor.row, page_width)
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
                            workspace_relative_display(&entry.path),
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
                version: 1,
                lsp_open: false,
            },
        );
        self.workspace.current_index = 0;
        self.restore_current_buffer_view_state();
        self.close_diagnostic_popup();
    }

    fn request_workspace_diagnostic_list(&mut self, error_only: bool) -> Result<()> {
        let _ = self.ensure_lsp_for_current_document();
        let supported = matches!(
            &self.lsp,
            LspClientState::Ready(client) if client.supports_workspace_diagnostics()
        );
        if !supported {
            self.refresh_workspace_diagnostic_cache()?;
            self.poll_lsp();
            self.open_cached_workspace_diagnostic_list(error_only);
            self.close_diagnostic_popup();
            return Ok(());
        }
        self.show_toast(if error_only {
            "LSP workspace errors"
        } else {
            "LSP workspace warnings+errors"
        });
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.workspace_diagnostics(error_only)?;
        }
        self.close_diagnostic_popup();
        Ok(())
    }

    fn open_cached_workspace_diagnostic_list(&mut self, error_only: bool) {
        let mut rows = Vec::new();

        let mut paths = self
            .workspace_diagnostics_cache
            .diagnostics
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        paths.sort();

        for path in paths {
            let Some(per_line) = self.workspace_diagnostics_cache.diagnostics.get(&path) else {
                continue;
            };
            let mut line_numbers = per_line.keys().copied().collect::<Vec<_>>();
            line_numbers.sort_unstable();

            for line_number in line_numbers {
                let Some(entries) = per_line.get(&line_number) else {
                    continue;
                };
                for entry in entries {
                    if error_only && entry.severity != DiagnosticSeverity::Error {
                        continue;
                    }
                    rows.push(ScratchRow {
                        text: format!(
                            "{:<7} {}:{}:{} {}",
                            diagnostic_label(entry.severity),
                            workspace_relative_display(&path),
                            line_number,
                            1,
                            entry.message
                        ),
                        target: Some(ScratchTarget {
                            path: path.clone(),
                            line_number,
                            column: 0,
                        }),
                    });
                }
            }
        }

        if rows.is_empty() {
            self.show_toast(if error_only {
                "No cached workspace errors"
            } else {
                "No cached workspace diagnostics"
            });
            return;
        }

        let title = if error_only {
            "[diagnostics] cached workspace errors"
        } else {
            "[diagnostics] cached workspace warnings+errors"
        };

        self.save_current_buffer_view_state();
        self.workspace.documents.insert(
            0,
            DocumentEntry {
                path: PathBuf::from(title),
                document: Document::Scratch(ScratchDocument::new(title, rows)),
                view_state: BufferViewState::default(),
                version: 1,
                lsp_open: false,
            },
        );
        self.workspace.current_index = 0;
        self.restore_current_buffer_view_state();
    }

    fn open_workspace_diagnostic_list(
        &mut self,
        error_only: bool,
        items: Vec<WorkspaceDiagnosticItem>,
    ) {
        if items.is_empty() {
            self.show_toast(if error_only {
                "No workspace errors"
            } else {
                "No workspace diagnostics"
            });
            return;
        }

        let rows = items
            .into_iter()
            .map(|item| ScratchRow {
                text: format!(
                    "{:<7} {}:{}:{} {}",
                    diagnostic_label(item.severity),
                    workspace_relative_display(&item.path),
                    item.line_number,
                    item.column + 1,
                    item.message
                ),
                target: Some(ScratchTarget {
                    path: item.path,
                    line_number: item.line_number,
                    column: item.column,
                }),
            })
            .collect();

        let title = if error_only {
            "[diagnostics] workspace errors"
        } else {
            "[diagnostics] workspace warnings+errors"
        };

        self.save_current_buffer_view_state();
        self.workspace.documents.insert(
            0,
            DocumentEntry {
                path: PathBuf::from(title),
                document: Document::Scratch(ScratchDocument::new(title, rows)),
                view_state: BufferViewState::default(),
                version: 1,
                lsp_open: false,
            },
        );
        self.workspace.current_index = 0;
        self.restore_current_buffer_view_state();
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

    fn open_hover_popup(&mut self) -> Result<()> {
        self.ensure_lsp_for_current_document()?;
        let Some((path, position)) = self.current_rust_lsp_position() else {
            return Ok(());
        };
        self.show_toast("LSP hover");
        if let LspClientState::Ready(client) = &mut self.lsp {
            self.hover_popup.active = true;
            self.hover_popup.lines = vec!["Loading...".to_owned()];
            client.hover(&path, position)?;
        }
        Ok(())
    }

    fn close_hover_popup(&mut self) {
        self.hover_popup.active = false;
        self.hover_popup.lines.clear();
    }

    fn open_rename_input(&mut self) {
        self.rename_input.active = true;
        self.rename_input.value.clear();
    }

    fn close_rename_input(&mut self) {
        self.rename_input.active = false;
        self.rename_input.value.clear();
    }

    fn submit_rename_input(&mut self) -> Result<()> {
        let new_name = self.rename_input.value.trim().to_owned();
        if new_name.is_empty() {
            self.close_rename_input();
            return Ok(());
        }

        self.ensure_lsp_for_current_document()?;
        let Some((path, position)) = self.current_rust_lsp_position() else {
            self.close_rename_input();
            return Ok(());
        };

        self.show_toast("LSP rename");
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.rename(&path, position, new_name)?;
        }
        self.close_rename_input();
        Ok(())
    }

    fn goto_symbol(&mut self, kind: GotoKind) -> Result<()> {
        self.ensure_lsp_for_current_document()?;
        let Some((path, position)) = self.current_rust_lsp_position() else {
            return Ok(());
        };
        self.show_toast(format!("LSP {}", kind.title()));
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.goto(kind, &path, position)?;
        }
        Ok(())
    }

    fn show_references(&mut self) -> Result<()> {
        self.ensure_lsp_for_current_document()?;
        let Some((path, position)) = self.current_rust_lsp_position() else {
            return Ok(());
        };
        self.show_toast("LSP [references]");
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.references(&path, position)?;
        }
        Ok(())
    }

    fn open_location_results(&mut self, title: &str, locations: Vec<Location>) -> Result<()> {
        if locations.is_empty() {
            return Ok(());
        }

        if locations.len() == 1 {
            return self.jump_to_location(&locations[0]);
        }

        let rows = locations
            .into_iter()
            .filter_map(|location| {
                let path = uri_to_path(&location.uri)?;
                Some(ScratchRow {
                    text: format!(
                        "{}:{}:{}",
                        workspace_relative_display(&path),
                        location.range.start.line + 1,
                        location.range.start.character + 1
                    ),
                    target: Some(ScratchTarget {
                        path,
                        line_number: location.range.start.line as usize + 1,
                        column: location.range.start.character as usize,
                    }),
                })
            })
            .collect();

        self.save_current_buffer_view_state();
        self.workspace.documents.insert(
            0,
            DocumentEntry {
                path: PathBuf::from(title),
                document: Document::Scratch(ScratchDocument::new(title, rows)),
                view_state: BufferViewState::default(),
                version: 1,
                lsp_open: false,
            },
        );
        self.workspace.current_index = 0;
        self.restore_current_buffer_view_state();
        Ok(())
    }

    fn jump_to_location(&mut self, location: &Location) -> Result<()> {
        let Some(path) = uri_to_path(&location.uri) else {
            return Ok(());
        };

        self.push_jump_history();
        if let Some(index) = self.workspace.find_document_index(&path) {
            self.make_document_current(index);
        } else {
                    self.open_document(path.clone())?;
        }

        if let Some((row, column)) = self
            .workspace
            .current_document()
            .display_position_for_lsp_position(location.range.start, self.current_page_width())
        {
            self.cursor.column = column;
            self.jump_with_context(row, self.current_page_width());
        }

        Ok(())
    }

    fn current_rust_lsp_position(&self) -> Option<(PathBuf, Position)> {
        let path = self.workspace.current_document_path()?.to_path_buf();
        if !is_rust_source_path(&path) {
            return None;
        }
        let position = self.workspace.current_document().lsp_position_for_display_position(
            self.cursor.row,
            self.cursor.column,
            self.current_page_width(),
        )?;
        Some((path, position))
    }

    fn request_selection_range_operator(&mut self, operator: PendingOperator) -> Result<()> {
        self.ensure_lsp_for_current_document()?;
        let Some((path, position)) = self.current_rust_lsp_position() else {
            self.show_toast("LSP syntax range unavailable");
            return Ok(());
        };

        self.show_toast("LSP syntax range");
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.selection_range(&path, position, operator)?;
        } else {
            self.show_toast("LSP syntax range unavailable");
        }
        Ok(())
    }

    fn open_selection_input(
        &mut self,
        operator: PendingOperator,
        ranges: Vec<lsp_types::Range>,
    ) -> Result<()> {
        let page_width = self.current_page_width();
        let display_ranges = ranges
            .into_iter()
            .filter_map(|range| {
                let (start_row, start_column) = self
                    .workspace
                    .current_document()
                    .display_position_for_lsp_position(range.start, page_width)?;
                let (end_row, end_column) = self
                    .workspace
                    .current_document()
                    .display_position_for_lsp_position(range.end, page_width)?;
                Some(DisplayRange {
                    start_row,
                    start_column,
                    end_row,
                    end_column,
                })
            })
            .collect::<Vec<_>>();

        if display_ranges.is_empty() {
            self.show_toast("No syntax range");
            return Ok(());
        }

        self.selection_input.active = true;
        self.selection_input.operator = Some(operator);
        self.selection_input.ranges = display_ranges;
        self.selection_input.current_index = 0;
        self.show_toast("Syntax range: i expand, Enter confirm");
        Ok(())
    }

    fn close_selection_input(&mut self) {
        self.selection_input.active = false;
        self.selection_input.operator = None;
        self.selection_input.ranges.clear();
        self.selection_input.current_index = 0;
    }

    fn expand_selection_input(&mut self) {
        if !self.selection_input.active {
            return;
        }
        let last = self.selection_input.ranges.len().saturating_sub(1);
        self.selection_input.current_index = (self.selection_input.current_index + 1).min(last);
    }

    fn submit_selection_input(&mut self) -> Result<()> {
        let Some(operator) = self.selection_input.operator else {
            self.close_selection_input();
            return Ok(());
        };
        let Some(range) = self.selection_input.current_range() else {
            self.close_selection_input();
            self.show_toast("No syntax range");
            return Ok(());
        };
        self.close_selection_input();

        if matches!(operator, PendingOperator::Yank) {
            self.yank_range(
                range.start_row,
                range.start_column,
                range.end_row,
                range.end_column,
            )?;
            self.show_toast("Yanked syntax range");
            return Ok(());
        }

        let page_width = self.current_page_width();
        self.workspace.current_document_mut().begin_undo_group();
        let Some((row, column)) = self.workspace.current_document_mut().remove_display_range(
            range.start_row,
            range.start_column,
            range.end_row,
            range.end_column,
            page_width,
        ) else {
            self.workspace.current_document_mut().end_undo_group();
            self.show_toast("Empty syntax range");
            return Ok(());
        };

        self.cursor.row = row;
        self.cursor.column = column;
        self.clamp_vertical_state();

        if matches!(operator, PendingOperator::Change) {
            self.mode = Mode::Insert;
            self.pending_insert_j = None;
            self.show_toast("Changed syntax range");
        } else {
            self.workspace.current_document_mut().end_undo_group();
            self.show_toast("Deleted syntax range");
        }
        Ok(())
    }

    fn apply_workspace_edit(&mut self, edit: lsp_types::WorkspaceEdit) -> Result<()> {
        if let Some(changes) = edit.changes {
            for (uri, edits) in changes {
                if let Some(path) = uri_to_path(&uri) {
                    self.apply_text_edits_to_path(path, &edits)?;
                }
            }
        }

        if let Some(document_changes) = edit.document_changes {
            match document_changes {
                lsp_types::DocumentChanges::Edits(edits) => {
                    for change in edits {
                        if let Some(path) = uri_to_path(&change.text_document.uri) {
                            self.apply_text_edits_to_path(path, &change.edits.into_iter().map(|edit| match edit {
                                lsp_types::OneOf::Left(text_edit) => text_edit,
                                lsp_types::OneOf::Right(annotated) => annotated.text_edit,
                            }).collect::<Vec<_>>())?;
                        }
                    }
                }
                lsp_types::DocumentChanges::Operations(_) => {}
            }
        }

        Ok(())
    }

    fn apply_text_edits_to_path(&mut self, path: PathBuf, edits: &[TextEdit]) -> Result<()> {
        if let Some(index) = self.workspace.find_document_index(&path) {
            self.workspace.documents[index].document.apply_text_edits(edits);
            self.workspace.documents[index].version += 1;
            if index == self.workspace.current_index {
                self.clamp_vertical_state();
            }
            return Ok(());
        }

        self.open_document(path.clone())?;
        let index = self.workspace.current_index;
        self.workspace.documents[index].document.apply_text_edits(edits);
        self.workspace.documents[index].version += 1;
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
                self.show_toast(format!("Go to line {line_number}"));
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
            self.show_toast(format!("Open [{}]", match self.picker.scope {
                PickerScope::All => "all",
                PickerScope::Buffers => "buffers",
            }));
        } else {
            self.picker.active = true;
            self.picker.query.clear();
            self.picker.scope = PickerScope::All;
            self.show_toast("Open [all]");
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
                    self.show_toast(format!("Open {}", display_name(&candidate.path)));
                }
            }
            OpenCandidate::ProjectFile(candidate) => {
                self.show_toast(format!("Open {}", display_name(&candidate.path)));
                self.open_document(candidate.path)?;
            }
        }

        self.close_picker();
        self.refresh_picker_candidates()?;
        Ok(())
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
        if is_rust_source_path(&path) {
            self.sync_current_document_save();
            self.poll_lsp();
            let summary = self.current_diagnostic_summary();
            self.last_save_feedback =
                Some(format!("rust-analyzer E{} W{}", summary.errors, summary.warnings));
        } else {
            self.last_save_feedback = Some("saved".to_owned());
        }
        self.show_toast(format!("Saved {}", display_name(&path)));

        Ok(())
    }

    fn show_toast(&mut self, message: impl Into<String>) {
        self.toast.message = Some(message.into());
        self.toast.expires_at = Some(Instant::now() + Duration::from_secs(3));
    }

    fn prune_toast(&mut self) {
        if let Some(expires_at) = self.toast.expires_at
            && Instant::now() >= expires_at
        {
            self.toast.message = None;
            self.toast.expires_at = None;
        }
    }

    fn close_current_buffer(&mut self) {
        if !self.workspace.has_documents() {
            return;
        }

        self.save_current_buffer_view_state();
        self.sync_current_document_close();
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

    fn collapse_to_single_pane(&mut self) {
        if self.focused_pane == FocusedPane::Right && self.layout_mode == LayoutMode::TerminalSplit {
            self.layout_mode = LayoutMode::Single;
            return;
        }

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

fn workspace_relative_display(path: &Path) -> String {
    let Ok(current_dir) = std::env::current_dir() else {
        return path.display().to_string();
    };

    path.strip_prefix(&current_dir)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn normalize_workspace_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn collect_rust_files_under(dir: &Path, rust_files: &mut Vec<PathBuf>) -> Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files_under(&path, rust_files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            rust_files.push(normalize_workspace_path(&path)?);
        }
    }

    Ok(())
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

fn diagnostic_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Error => "Error",
    }
}

fn is_rust_source_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("rs")
}

#[allow(dead_code)]
fn _project_file_display_name(candidate: &ProjectFileCandidate) -> &str {
    &candidate.display_name
}
