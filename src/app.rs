use std::{
    collections::{HashSet, VecDeque},
    fs,
    fs::File,
    path::{Path, PathBuf},
    process::Child,
    sync::mpsc::Receiver,
    time::{Duration, Instant, SystemTime},
};

use crossterm::{event, terminal};
use lsp_types::{Location, Position, TextEdit};

use crate::{
    config,
    document::{
        DiagnosticSeverity, DiagnosticSummary, Document, ScratchDocument, ScratchRow,
        ScratchTarget, SyntaxTokenSpan,
    },
    error::Result,
    mode::Mode,
    open_candidate::{OpenCandidate, ProjectFileCandidate, collect_project_file_candidates},
    picker_match,
};

mod action;
mod completion;
mod keymap;
mod lsp;
mod navigation;
mod render;
mod replace;
mod search;
pub(crate) mod semantic;
mod shell;
mod terminal_session;
mod workspace;

use self::action::{FindKind, PendingNormalAction, PendingOperator, ReplayableAction};
use self::completion::{
    CompletionItem, CompletionState, collect_fallback_items, completion_prefix,
    has_empty_completion_trigger, rank_completion_items, text_end_position,
};
use self::lsp::{
    GotoKind, HoverPopupState, LspClient, LspClientState, LspEvent, RenameInputState,
    WorkspaceDiagnosticItem, uri_to_path,
};
use self::terminal_session::TerminalSession;
use crate::language;

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
    pub completion: CompletionState,
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
    pub pending_semantic_tokens_path: Option<PathBuf>,
    pub lsp_document_cache: std::collections::HashMap<PathBuf, CachedLspDocumentState>,
    pub editor_rc: language::EditorRc,
    pub silent: bool,
    pub wrap: bool,
    pub last_file_check: Instant,
    pub selection_anchor: Option<CursorState>,
    pub extra_cursors: Vec<CursorState>,
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

#[derive(Default)]
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

#[derive(Clone, Copy, Default)]
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

#[derive(Default)]
pub struct GoInputState {
    pub active: bool,
    pub value: String,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchScope {
    #[default]
    CurrentFile,
    OpenBuffers,
    Project,
}

#[derive(Default)]
pub struct SearchInputState {
    pub active: bool,
    pub value: String,
    pub scope: SearchScope,
    pub options: crate::search_options::SearchOptions,
}

#[derive(Default)]
pub struct ReplaceInputState {
    pub active: bool,
    pub find: String,
    pub replace: String,
    pub scope: SearchScope,
    pub field: ReplaceField,
    pub options: crate::search_options::SearchOptions,
}

#[derive(Default)]
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

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ReplaceField {
    #[default]
    Find,
    Replace,
}

#[derive(Default)]
pub struct DiagnosticPopupState {
    pub active: bool,
    pub lines: Vec<String>,
}

#[derive(Default)]
pub struct ToastState {
    pub transient_messages: VecDeque<ToastMessage>,
    pub persistent_message: Option<String>,
}

pub struct ToastMessage {
    pub message: String,
    pub expires_at: Instant,
}

#[derive(Default)]
pub struct WorkspaceDiagnosticsCache {
    pub rust_files: Option<Vec<PathBuf>>,
    pub diagnostics: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<usize, Vec<crate::document::DiagnosticEntry>>,
    >,
}

pub struct CachedLspDocumentState {
    pub modified: SystemTime,
    pub diagnostics:
        Option<std::collections::HashMap<usize, Vec<crate::document::DiagnosticEntry>>>,
    pub semantic_tokens: Option<std::collections::HashMap<usize, Vec<SyntaxTokenSpan>>>,
}

impl CachedLspDocumentState {
    pub fn with_diagnostics(
        modified: SystemTime,
        diagnostics: std::collections::HashMap<usize, Vec<crate::document::DiagnosticEntry>>,
    ) -> Self {
        Self {
            modified,
            diagnostics: Some(diagnostics),
            semantic_tokens: None,
        }
    }

    pub fn with_semantic_tokens(
        modified: SystemTime,
        semantic_tokens: std::collections::HashMap<usize, Vec<SyntaxTokenSpan>>,
    ) -> Self {
        Self {
            modified,
            diagnostics: None,
            semantic_tokens: Some(semantic_tokens),
        }
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum PickerScope {
    #[default]
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
    pub options: crate::search_options::SearchOptions,
}

#[derive(Clone)]
pub struct JumpPosition {
    pub path: Option<PathBuf>,
    pub row: usize,
    pub column: usize,
    pub viewport_row: usize,
}

#[derive(Clone, Default)]
pub enum YankBuffer {
    #[default]
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
    /// アプリを初期化してLSPとピッカー候補を準備する
    pub fn new() -> Result<Self> {
        let workspace = Workspace {
            documents: Vec::new(),
            current_index: 0,
        };

        let mut app = Self {
            mode: Mode::Insert,
            workspace,
            picker: PickerState::default(),
            shell: ShellState {
                program: config::shell_program().to_owned(),
                parser: None,
                rows: 0,
                cols: 0,
                child: None,
                pty: None,
                output_rx: None,
            },
            cursor: CursorState::default(),
            viewport_row: 0,
            pending_normal_action: None,
            pending_insert_j: None,
            last_replayable_action: None,
            go_input: GoInputState::default(),
            search_input: SearchInputState::default(),
            replace_input: ReplaceInputState::default(),
            completion: CompletionState::default(),
            selection_input: SelectionInputState::default(),
            diagnostic_popup: DiagnosticPopupState::default(),
            last_search: None,
            yank_buffer: YankBuffer::default(),
            jump_history: Vec::new(),
            jump_forward_history: Vec::new(),
            layout_mode: LayoutMode::Single,
            focused_pane: FocusedPane::Left,
            last_save_feedback: None,
            hover_popup: HoverPopupState::default(),
            rename_input: RenameInputState::default(),
            lsp: LspClientState::Inactive,
            toast: ToastState::default(),
            workspace_diagnostics_cache: WorkspaceDiagnosticsCache::default(),
            pending_semantic_tokens_path: None,
            lsp_document_cache: std::collections::HashMap::new(),
            editor_rc: language::EditorRc::load(),
            silent: false,
            wrap: true,
            last_file_check: Instant::now(),
            selection_anchor: None,
            extra_cursors: Vec::new(),
        };
        let _ = app.ensure_lsp_for_current_document();
        app.refresh_picker_candidates()?;
        Ok(app)
    }

    /// パスを指定してアプリを初期化し、ファイルを開く
    pub fn open_path(path: &Path) -> Result<Self> {
        let mut app = Self::new()?;
        let path = normalize_workspace_path(path)?;
        app.workspace.open_document(path)?;
        let _ = app.ensure_lsp_for_current_document();
        app.refresh_picker_candidates()?;
        Ok(app)
    }

    /// TUIのイベントループを実行する
    pub fn run(&mut self) -> Result<()> {
        let mut terminal_session = TerminalSession::enter()?;
        let mut needs_redraw = true;

        loop {
            let mut changed = false;
            changed |= self.prune_toast();
            changed |= self.poll_lsp();
            changed |= self.poll_shell_output();
            changed |= self.poll_completion();
            changed |= self.poll_external_file_changes();
            needs_redraw |= changed;

            if needs_redraw {
                self.sync_shell_size()?;
                self.render_frame(terminal_session.terminal())?;
                needs_redraw = false;
            }

            if event::poll(Duration::from_millis(50))? {
                if self.handle_event(event::read()?)? {
                    break;
                }
                needs_redraw = true;
            }
        }

        self.shutdown_shell();
        terminal_session.leave()?;
        Ok(())
    }

    /// ピッカーの候補リストをバッファとプロジェクトファイルから更新する
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

    /// 現在のカーソル・ビューポート状態をバッファエントリに保存する
    fn save_current_buffer_view_state(&mut self) {
        if let Some(entry) = self
            .workspace
            .documents
            .get_mut(self.workspace.current_index)
        {
            entry.view_state = BufferViewState {
                row: self.cursor.row,
                column: self.cursor.column,
                viewport_row: self.viewport_row,
            };
        }
    }

    /// バッファエントリに保存されたカーソル・ビューポート状態を復元する
    fn restore_current_buffer_view_state(&mut self) {
        let Some(entry) = self.workspace.documents.get(self.workspace.current_index) else {
            self.cursor = CursorState::default();
            self.viewport_row = 0;
            return;
        };

        self.cursor.row = entry.view_state.row;
        self.cursor.column = entry.view_state.column;
        self.viewport_row = entry.view_state.viewport_row;
        self.clamp_vertical_state();
    }

    /// 右ペインがターミナルとしてフォーカス中かどうかを返す
    pub(crate) fn is_terminal_pane_focused(&self) -> bool {
        self.focused_pane == FocusedPane::Right
            && matches!(
                self.layout_mode,
                LayoutMode::TerminalSplit | LayoutMode::Single
            )
    }

    /// 現在のレイアウトとフォーカスに基づいてページ幅を返す
    fn current_page_width(&self) -> usize {
        if !self.wrap {
            return u16::MAX as usize;
        }
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

    /// 指定インデックスのドキュメントをカレントに切り替え、ビュー状態とLSPを更新する
    fn make_document_current(&mut self, index: usize) {
        self.close_completion();
        self.save_current_buffer_view_state();
        self.workspace.make_current(index);
        self.restore_current_buffer_view_state();
        let _ = self.ensure_lsp_for_current_document();
    }

    /// 指定インデックスのドキュメントをセカンダリとして選択し、ビュー状態とLSPを更新する
    fn select_current_document(&mut self, index: usize) {
        self.close_completion();
        self.save_current_buffer_view_state();
        self.workspace.select_current(index);
        self.restore_current_buffer_view_state();
        let _ = self.ensure_lsp_for_current_document();
    }

    /// パスのドキュメントを開いてLSP状態を復元し、LSPを確保する
    fn open_document(&mut self, path: PathBuf) -> Result<()> {
        let path = normalize_workspace_path(&path)?;
        self.close_completion();
        self.save_current_buffer_view_state();
        self.workspace.open_document(path.clone())?;
        self.restore_cached_lsp_state(&path);
        self.restore_current_buffer_view_state();
        self.ensure_lsp_for_current_document()?;
        Ok(())
    }

    /// ファイル更新時刻が一致する場合、キャッシュされたLSP状態をドキュメントに適用する
    fn restore_cached_lsp_state(&mut self, path: &Path) {
        let Some(modified) = path_modified_time(path) else {
            return;
        };
        let Some(cached) = self.lsp_document_cache.get(path) else {
            return;
        };
        if cached.modified != modified {
            return;
        }
        let Some(index) = self.workspace.find_document_index(path) else {
            return;
        };
        if let Some(diagnostics) = &cached.diagnostics {
            self.workspace.documents[index]
                .document
                .set_rust_diagnostics(diagnostics.clone());
        }
        if let Some(tokens) = &cached.semantic_tokens {
            self.workspace.documents[index]
                .document
                .set_semantic_tokens(tokens.clone());
        }
    }

    /// 言語設定からパスに対応する LSP コマンド情報を返す
    fn lsp_command_for_path(&self, path: &Path) -> Option<(String, Vec<String>)> {
        self.editor_rc.lsp_for_path(path)
    }

    /// パスに対応する LSP の languageId を返す。未設定の場合は "plaintext"
    fn lsp_language_id_for_path(&self, path: &Path) -> String {
        self.editor_rc
            .language_id_for_path(path)
            .unwrap_or_else(|| "plaintext".to_owned())
    }

    /// 現在のドキュメントに対してLSPが動作中か確認し、必要なら起動してドキュメントを登録する
    fn ensure_lsp_for_current_document(&mut self) -> Result<()> {
        let Some(path) = self
            .workspace
            .current_document_path()
            .map(ToOwned::to_owned)
        else {
            return Ok(());
        };
        if self.lsp_command_for_path(&path).is_none() {
            return Ok(());
        }

        self.restore_cached_lsp_state(&path);
        if self.has_fresh_cached_lsp_state(&path) {
            return Ok(());
        }

        if matches!(
            self.lsp,
            LspClientState::NotAvailable | LspClientState::Starting(_)
        ) {
            return Ok(());
        }

        if matches!(self.lsp, LspClientState::Inactive) {
            let lsp_info = self.lsp_command_for_path(&path);
            self.lsp = match lsp_info {
                Some((cmd, args)) => match LspClient::start(Path::new("."), cmd, args) {
                    Ok(client) => LspClientState::Starting(client),
                    Err(error) => {
                        self.show_toast(format!("LSP failed: {error:?}"));
                        LspClientState::Failed(())
                    }
                },
                None => LspClientState::NotAvailable,
            };
            return Ok(());
        }

        self.ensure_current_document_open_for_lsp()
    }

    /// パスのLSPキャッシュが現在のファイル更新時刻と一致して完全かどうかを返す
    fn has_fresh_cached_lsp_state(&self, path: &Path) -> bool {
        let Some(modified) = path_modified_time(path) else {
            return false;
        };
        self.lsp_document_cache.get(path).is_some_and(|cached| {
            cached.modified == modified
                && cached.diagnostics.is_some()
                && cached.semantic_tokens.is_some()
        })
    }

    /// 現在のドキュメントをLSPに通知してセマンティックトークンをスケジュールする
    fn ensure_current_document_open_for_lsp(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        let Some(path) = self
            .workspace
            .current_document_path()
            .map(ToOwned::to_owned)
        else {
            return Ok(());
        };
        if self.lsp_command_for_path(&path).is_none() {
            return Ok(());
        }

        let Some(text) = self.workspace.current_document().full_text() else {
            return Ok(());
        };
        let current_index = self.workspace.current_index;
        let version = self.workspace.documents[current_index].version;

        self.show_lsp_sync_toast(&path, "sync");
        let language_id = self.lsp_language_id_for_path(&path);
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.ensure_open(&path, &language_id, version, &text)?;
            let _ = client.did_save(&path, &text);
            self.workspace.documents[current_index].lsp_open = true;
        }
        self.schedule_semantic_tokens_request(&path);

        let _ = page_width;
        Ok(())
    }

    /// LSPイベントを処理してドキュメントに反映し、変更があればtrueを返す
    fn poll_lsp(&mut self) -> bool {
        // Starting 状態のクライアントもポーリングし、Initialized が届いたら Ready へ遷移する
        if let LspClientState::Starting(client) = &mut self.lsp {
            client.poll();
            let initialized = client
                .pending_events
                .iter()
                .position(|e| matches!(e, LspEvent::Initialized { .. }));
            if let Some(idx) = initialized {
                let event = client.pending_events.remove(idx);
                if let (
                    LspEvent::Initialized {
                        workspace_diagnostics_supported,
                    },
                    LspClientState::Starting(mut client),
                ) = (
                    event,
                    std::mem::replace(&mut self.lsp, LspClientState::Inactive),
                ) {
                    client.workspace_diagnostics_supported = workspace_diagnostics_supported;
                    self.lsp = LspClientState::Ready(client);
                    let _ = self.ensure_lsp_for_current_document();
                }
                return true;
            }
            return false;
        }

        let events = match &mut self.lsp {
            LspClientState::Ready(client) => {
                client.poll();
                client.take_events()
            }
            _ => Vec::new(),
        };
        if events.is_empty() {
            return false;
        }

        for event in events {
            match event {
                LspEvent::PublishDiagnostics { path, diagnostics } => {
                    if let Some(modified) = path_modified_time(&path) {
                        self.lsp_document_cache
                            .entry(path.clone())
                            .and_modify(|cached| {
                                cached.modified = modified;
                                cached.diagnostics = Some(diagnostics.clone());
                            })
                            .or_insert_with(|| {
                                CachedLspDocumentState::with_diagnostics(
                                    modified,
                                    diagnostics.clone(),
                                )
                            });
                    }
                    self.workspace_diagnostics_cache
                        .diagnostics
                        .insert(path.clone(), diagnostics.clone());
                    self.workspace.apply_lsp_diagnostics(&path, diagnostics);
                    if self
                        .pending_semantic_tokens_path
                        .as_ref()
                        .is_some_and(|pending| pending == &path)
                    {
                        if let LspClientState::Ready(client) = &mut self.lsp {
                            let _ = client.request_semantic_tokens(&path);
                        }
                        self.show_lsp_sync_toast(&path, "syntax");
                        self.pending_semantic_tokens_path = None;
                    }
                }
                LspEvent::PublishSemanticTokens { path, tokens } => {
                    if let Some(modified) = path_modified_time(&path) {
                        self.lsp_document_cache
                            .entry(path.clone())
                            .and_modify(|cached| {
                                cached.modified = modified;
                                cached.semantic_tokens = Some(tokens.clone());
                            })
                            .or_insert_with(|| {
                                CachedLspDocumentState::with_semantic_tokens(
                                    modified,
                                    tokens.clone(),
                                )
                            });
                    }
                    if let Some(index) = self.workspace.find_document_index(&path) {
                        self.workspace.documents[index]
                            .document
                            .set_semantic_tokens(tokens);
                    }
                    if self
                        .workspace
                        .current_document_path()
                        .is_some_and(|current| current == path)
                    {
                        self.clear_persistent_toast();
                    }
                }
                LspEvent::WorkspaceDiagnosticsResult { error_only, items } => {
                    self.open_workspace_diagnostic_list(error_only, items);
                }
                LspEvent::GotoResult { kind, locations } => {
                    self.clear_persistent_toast();
                    let _ = self.open_location_results(kind.title(), locations);
                }
                LspEvent::ReferencesResult { locations } => {
                    self.clear_persistent_toast();
                    let _ = self.open_location_results("[references]", locations);
                }
                LspEvent::HoverResult { lines } => {
                    self.clear_persistent_toast();
                    if self.hover_popup.active {
                        self.hover_popup.lines = if lines.is_empty() {
                            vec!["No information.".to_owned()]
                        } else {
                            lines
                        };
                    }
                }
                LspEvent::RenameResult { edit } => {
                    self.clear_persistent_toast();
                    if let Some(edit) = edit {
                        let _ = self.apply_workspace_edit(edit);
                    }
                }
                LspEvent::SelectionRangeResult { operator, ranges } => {
                    self.clear_persistent_toast();
                    let _ = self.open_selection_input(operator, ranges);
                }
                LspEvent::CompletionResult {
                    path,
                    serial,
                    items,
                } => {
                    self.handle_completion_result(path, serial, items);
                }
                LspEvent::SemanticTokensFailed { path } => {
                    if self
                        .workspace
                        .current_document_path()
                        .is_some_and(|current| current == path)
                    {
                        self.clear_persistent_toast();
                    }
                }
                LspEvent::Failed(message) => {
                    self.clear_persistent_toast();
                    self.last_save_feedback = Some(message);
                    self.lsp = LspClientState::Failed(());
                }
                // Starting 状態で処理されなかった場合のフォールバック
                LspEvent::Initialized { .. } => {}
            }
        }
        true
    }

    /// 全ドキュメントの診断エラー・警告件数を集計して返す
    fn current_diagnostic_summary(&self) -> DiagnosticSummary {
        let mut summary = DiagnosticSummary::default();
        for entry in &self.workspace.documents {
            let document_summary = entry.document.diagnostic_summary();
            summary.errors += document_summary.errors;
            summary.warnings += document_summary.warnings;
        }
        summary
    }

    /// 外部変更を検出し、未変更なら自動リロード、変更済みなら通知する
    fn poll_external_file_changes(&mut self) -> bool {
        if self.last_file_check.elapsed() < Duration::from_secs(1) {
            return false;
        }
        self.last_file_check = Instant::now();

        let paths: Vec<PathBuf> = self
            .workspace
            .documents
            .iter()
            .map(|e| e.path.clone())
            .collect();

        let mut changed = false;
        for path in paths {
            let Some(current_mtime) = path_modified_time(&path) else {
                continue;
            };
            let Some(doc) = self
                .workspace
                .documents
                .iter_mut()
                .find(|e| e.path == path)
                .and_then(|e| e.document.as_editable_mut())
            else {
                continue;
            };
            let Some(disk_mtime) = doc.disk_mtime else {
                continue;
            };
            if current_mtime == disk_mtime {
                continue;
            }

            if !doc.is_dirty {
                if doc.reload().is_ok() {
                    let name = display_name(&path);
                    self.show_toast(format!("{name}: 外部変更を検出、再読み込みしました"));
                    changed = true;
                    // LSP へ変更を通知
                    if let Some(entry) =
                        self.workspace.documents.iter_mut().find(|e| e.path == path)
                    {
                        entry.version += 1;
                        let version = entry.version;
                        if let (Some(text), LspClientState::Ready(client)) =
                            (entry.document.full_text(), &mut self.lsp)
                        {
                            let _ = client.did_change(&path, version, &text);
                        }
                    }
                }
            } else {
                // 未保存の変更がある場合はディスク mtime だけ更新して繰り返し通知を抑制
                doc.disk_mtime = Some(current_mtime);
                let name = display_name(&path);
                self.show_toast(format!(
                    "{name}: 外部変更を検出（未保存の変更があるため再読み込みしていません）"
                ));
                changed = true;
            }
        }
        changed
    }

    /// 現在のドキュメントの保存をLSPに通知してセマンティックトークンをスケジュールする
    fn sync_current_document_save(&mut self) {
        let Some(path) = self
            .workspace
            .current_document_path()
            .map(ToOwned::to_owned)
        else {
            return;
        };
        if self.lsp_command_for_path(&path).is_none() {
            return;
        }
        if self.ensure_lsp_for_current_document().is_err() {
            return;
        }
        let Some(text) = self.workspace.current_document().full_text() else {
            return;
        };
        self.show_lsp_sync_toast(&path, "updating");
        if let LspClientState::Ready(client) = &mut self.lsp {
            let _ = client.did_save(&path, &text);
        }
        self.schedule_semantic_tokens_request(&path);
    }

    /// セマンティックトークンリクエストを診断待機またはLSPへの即時送信としてスケジュールする
    fn schedule_semantic_tokens_request(&mut self, path: &std::path::Path) {
        if should_wait_for_diagnostics_before_semantic(path) {
            self.pending_semantic_tokens_path = Some(path.to_path_buf());
        } else {
            self.pending_semantic_tokens_path = None;
            if let LspClientState::Ready(client) = &mut self.lsp {
                let _ = client.request_semantic_tokens(path);
            }
        }
    }

    /// ファイル名とverbを組み合わせたLSP同期中トーストを永続表示する
    fn show_lsp_sync_toast(&mut self, path: &Path, verb: &str) {
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Rust file");
        self.show_persistent_toast(format!("LSP {verb} {label}..."));
    }

    /// 現在のドキュメントのクローズをLSPに通知する
    fn sync_current_document_close(&mut self) {
        let Some(entry) = self.workspace.documents.get(self.workspace.current_index) else {
            return;
        };
        if !entry.lsp_open || self.lsp_command_for_path(&entry.path).is_none() {
            return;
        }
        if let LspClientState::Ready(client) = &mut self.lsp {
            let _ = client.did_close(&entry.path);
        }
    }

    /// ワークスペース内の全RustファイルをLSPに通知して診断を更新する
    fn refresh_workspace_diagnostic_cache(&mut self) -> Result<()> {
        self.ensure_workspace_rust_files()?;

        let Some(rust_files) = self.workspace_diagnostics_cache.rust_files.clone() else {
            return Ok(());
        };

        let LspClientState::Ready(client) = &mut self.lsp else {
            return Ok(());
        };

        for path in rust_files {
            let language_id = self
                .editor_rc
                .language_id_for_path(&path)
                .unwrap_or_else(|| "plaintext".to_owned());
            if let Some(index) = self.workspace.find_document_index(&path) {
                if let Some(text) = self.workspace.documents[index].document.full_text() {
                    let version = self.workspace.documents[index].version;
                    client.ensure_open(&path, &language_id, version, &text)?;
                    let _ = client.did_save(&path, &text);
                    self.workspace.documents[index].lsp_open = true;
                }
                continue;
            }

            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            client.ensure_open(&path, &language_id, 1, &text)?;
            let _ = client.did_save(&path, &text);
        }

        Ok(())
    }

    /// ワークスペースのRustファイルリストが未キャッシュなら収集してキャッシュする
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

    /// 検索モーションを実行してカーソルを対象文字位置に移動する
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

    #[allow(dead_code)]
    /// 検索モーションとオペレータを組み合わせて範囲を削除またはチェンジする
    fn run_operator_find(
        &mut self,
        operator: PendingOperator,
        find_kind: FindKind,
        target: char,
    ) -> Result<()> {
        let Some((found_row, found_column)) = self.find_target_position(find_kind, target)? else {
            return Ok(());
        };

        let Some((start_row, start_column, end_row, end_column)) = operator_range(
            self.cursor.row,
            self.cursor.column,
            found_row,
            found_column,
            find_kind,
        ) else {
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

    /// 指定範囲のテキストをヤンクバッファにcharwiseコピーする
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

    /// 指定方向で対象文字の最初のマッチ位置を返す
    fn find_target_position(
        &self,
        find_kind: FindKind,
        target: char,
    ) -> Result<Option<(usize, usize)>> {
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

                    if let Some(column) = (0..end_column)
                        .rev()
                        .find(|index| line_chars[*index] == target)
                    {
                        return Ok(Some((row, column)));
                    }
                }
            }
        }

        Ok(None)
    }

    /// 最後に実行したリプレイ可能なアクションを同方向または逆方向に再実行する
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
            ReplayableAction::Diagnostic {
                error_only,
                forward,
            } => {
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

    /// ヤンクバッファの内容をカーソルの後にペーストする
    fn paste_after_cursor(&mut self) -> Result<()> {
        self.workspace.current_document_mut().begin_undo_group();

        match self.yank_buffer.clone() {
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
                self.open_line_above_with_text(&line_text);
            }
        }

        self.workspace.current_document_mut().end_undo_group();
        Ok(())
    }

    #[allow(dead_code)]
    /// ヤンクバッファの内容をカーソルの前にペーストする
    fn paste_before_cursor(&mut self) -> Result<()> {
        self.workspace.current_document_mut().begin_undo_group();

        match self.yank_buffer.clone() {
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

    /// 指定位置にテキストを1文字ずつ挿入してカーソルを末尾に移動する
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

    /// 現在行の下に空行を開いてインサートモードに入る
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

    /// 現在行の下に空行を開いてテキストを挿入する
    #[allow(dead_code)]
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

    /// 現在行の上に空行を開いてテキストを挿入する
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

    #[allow(dead_code)]
    /// 現在行をlinewiseヤンクバッファにコピーする
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

    /// 現在行を削除してlinewiseヤンクバッファに保存する
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

    #[allow(dead_code)]
    /// 現在行をクリアしてヤンクバッファに保存しインサートモードに入る
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

    /// 現在のドキュメントのアンドゥを実行してカーソルをクランプする
    fn undo_current_document(&mut self) {
        if self.workspace.current_document_mut().undo() {
            self.clamp_vertical_state();
        }
    }

    /// 現在のドキュメントのリドゥを実行してカーソルをクランプする
    fn redo_current_document(&mut self) {
        if self.workspace.current_document_mut().redo() {
            self.clamp_vertical_state();
        }
    }

    /// スクラッチドキュメントを最前面に挿入してカレントにする
    fn open_scratch_document(&mut self, title: &str, rows: Vec<ScratchRow>) {
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

    /// 行番号入力UIを開く
    fn open_go_input(&mut self) {
        self.go_input = GoInputState {
            active: true,
            ..Default::default()
        };
    }

    /// 行番号入力UIを閉じてデフォルト状態にリセットする
    fn close_go_input(&mut self) {
        self.go_input = GoInputState::default();
    }

    /// カーソル行の診断をポップアップで表示する
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

    /// 診断ポップアップを閉じてデフォルト状態にリセットする
    fn close_diagnostic_popup(&mut self) {
        self.diagnostic_popup = DiagnosticPopupState::default();
    }

    /// 開いているバッファの診断一覧をスクラッチドキュメントとして開く
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

        self.open_scratch_document(title, rows);
        self.close_diagnostic_popup();
    }

    /// LSPまたはキャッシュからワークスペース診断一覧を取得して表示する
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

    /// キャッシュされたワークスペース診断をスクラッチドキュメントとして開く
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

        self.open_scratch_document(title, rows);
    }

    /// LSPから受け取ったワークスペース診断をスクラッチドキュメントとして開く
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

        self.open_scratch_document(title, rows);
    }

    #[allow(dead_code)]
    /// カーソル行のスクラッチターゲットを開いてその位置にジャンプする
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

    /// カーソル位置のLSPホバー情報を要求してポップアップを開く
    fn open_hover_popup(&mut self) -> Result<()> {
        self.ensure_lsp_for_current_document()?;
        self.sync_current_document_saved_state_for_lsp();
        let Some((path, position)) = self.current_rust_lsp_position() else {
            return Ok(());
        };
        self.show_persistent_toast("LSP hover...");
        if let LspClientState::Ready(client) = &mut self.lsp {
            self.hover_popup.active = true;
            self.hover_popup.lines = vec!["Loading...".to_owned()];
            client.hover(&path, position)?;
        }
        Ok(())
    }

    /// ホバーポップアップを非アクティブ化して内容をクリアする
    fn close_hover_popup(&mut self) {
        self.hover_popup.active = false;
        self.hover_popup.lines.clear();
    }

    /// リネーム入力UIを開いて入力値をクリアする
    fn open_rename_input(&mut self) {
        self.rename_input.active = true;
        self.rename_input.value.clear();
    }

    /// リネーム入力UIを閉じて入力値をクリアする
    fn close_rename_input(&mut self) {
        self.rename_input.active = false;
        self.rename_input.value.clear();
    }

    /// リネーム入力を確定してLSPにリネームを要求する
    fn submit_rename_input(&mut self) -> Result<()> {
        let new_name = self.rename_input.value.trim().to_owned();
        if new_name.is_empty() {
            self.close_rename_input();
            return Ok(());
        }

        self.ensure_lsp_for_current_document()?;
        self.sync_current_document_saved_state_for_lsp();
        let Some((path, position)) = self.current_rust_lsp_position() else {
            self.close_rename_input();
            return Ok(());
        };

        self.show_persistent_toast("LSP rename...");
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.rename(&path, position, new_name)?;
        }
        self.close_rename_input();
        Ok(())
    }

    /// LSPにシンボル定義・宣言・実装へのジャンプを要求する
    fn goto_symbol(&mut self, kind: GotoKind) -> Result<()> {
        self.ensure_lsp_for_current_document()?;
        self.sync_current_document_saved_state_for_lsp();
        let Some((path, position)) = self.current_rust_lsp_position() else {
            return Ok(());
        };
        self.show_persistent_toast(format!("LSP {}...", kind.title()));
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.goto(kind, &path, position)?;
        }
        Ok(())
    }

    /// LSPにカーソル位置のシンボル参照一覧を要求する
    fn show_references(&mut self) -> Result<()> {
        self.ensure_lsp_for_current_document()?;
        self.sync_current_document_saved_state_for_lsp();
        let Some((path, position)) = self.current_rust_lsp_position() else {
            return Ok(());
        };
        self.show_persistent_toast("LSP [references]...");
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.references(&path, position)?;
        }
        Ok(())
    }

    /// 単一位置なら直接ジャンプし、複数なら選択リストをスクラッチドキュメントとして開く
    fn open_location_results(&mut self, title: &str, locations: Vec<Location>) -> Result<()> {
        if locations.is_empty() {
            self.show_toast(format!("{title} not found"));
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

        self.open_scratch_document(title, rows);
        Ok(())
    }

    /// LSP位置のドキュメントを開いてカーソルをその位置に移動する
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

    /// 現在カーソルのLSP対象ファイルパスとLSP位置を返す
    fn current_rust_lsp_position(&self) -> Option<(PathBuf, Position)> {
        let path = self.workspace.current_document_path()?.to_path_buf();
        self.lsp_command_for_path(&path)?;
        let position = self
            .workspace
            .current_document()
            .lsp_position_for_display_position(
                self.cursor.row,
                self.cursor.column,
                self.current_page_width(),
            )?;
        Some((path, position))
    }

    /// ディスク上の現在ドキュメント内容をLSPに同期する
    fn sync_current_document_saved_state_for_lsp(&mut self) {
        let Some(path) = self
            .workspace
            .current_document_path()
            .map(ToOwned::to_owned)
        else {
            return;
        };
        if self.lsp_command_for_path(&path).is_none() {
            return;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            return;
        };
        let current_index = self.workspace.current_index;
        self.workspace.documents[current_index].version += 1;
        let version = self.workspace.documents[current_index].version;
        let language_id = self.lsp_language_id_for_path(&path);
        if let LspClientState::Ready(client) = &mut self.lsp {
            let _ = client.ensure_open(&path, &language_id, version, &text);
            let _ = client.did_change(&path, version, &text);
            self.workspace.documents[current_index].lsp_open = true;
        }
    }

    #[allow(dead_code)]
    /// LSPにシンタックス選択範囲を要求してオペレータを紐付ける
    fn request_selection_range_operator(&mut self, operator: PendingOperator) -> Result<()> {
        self.ensure_lsp_for_current_document()?;
        self.sync_current_document_saved_state_for_lsp();
        let Some((path, position)) = self.current_rust_lsp_position() else {
            self.show_toast("LSP syntax range unavailable");
            return Ok(());
        };

        self.show_persistent_toast("LSP syntax range...");
        if let LspClientState::Ready(client) = &mut self.lsp {
            client.selection_range(&path, position, operator)?;
        } else {
            self.show_toast("LSP syntax range unavailable");
        }
        Ok(())
    }

    /// LSP選択範囲をDisplayRangeに変換して選択入力UIを開く
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

    /// 選択入力UIを閉じてデフォルト状態にリセットする
    fn close_selection_input(&mut self) {
        self.selection_input = SelectionInputState::default();
    }

    /// 選択範囲を一段階拡大する
    fn expand_selection_input(&mut self) {
        if !self.selection_input.active {
            return;
        }
        let last = self.selection_input.ranges.len().saturating_sub(1);
        self.selection_input.current_index = (self.selection_input.current_index + 1).min(last);
    }

    /// 現在の選択範囲を確定してオペレータ（ヤンク・削除・チェンジ）を実行する
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

    /// LSPのワークスペース編集を各パスに適用する
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
                            self.apply_text_edits_to_path(
                                path,
                                &change
                                    .edits
                                    .into_iter()
                                    .map(|edit| match edit {
                                        lsp_types::OneOf::Left(text_edit) => text_edit,
                                        lsp_types::OneOf::Right(annotated) => annotated.text_edit,
                                    })
                                    .collect::<Vec<_>>(),
                            )?;
                        }
                    }
                }
                lsp_types::DocumentChanges::Operations(_) => {}
            }
        }

        Ok(())
    }

    /// 指定パスのドキュメントにテキスト編集を適用し、必要なら開いてから適用する
    fn apply_text_edits_to_path(&mut self, path: PathBuf, edits: &[TextEdit]) -> Result<()> {
        if let Some(index) = self.workspace.find_document_index(&path) {
            self.workspace.documents[index]
                .document
                .apply_text_edits(edits);
            self.workspace.documents[index].version += 1;
            if index == self.workspace.current_index {
                self.clamp_vertical_state();
            }
            return Ok(());
        }

        self.open_document(path.clone())?;
        let index = self.workspace.current_index;
        self.workspace.documents[index]
            .document
            .apply_text_edits(edits);
        self.workspace.documents[index].version += 1;
        Ok(())
    }

    /// 行番号入力を確定して指定行にジャンプする
    fn submit_go_input(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        if let Ok(line_number) = self.go_input.value.parse::<usize>()
            && let Some(row) = self
                .workspace
                .current_document()
                .jump_row_for_line_number(line_number, page_width)
        {
            self.push_jump_history();
            self.cursor.column = 0;
            self.jump_with_context(row, page_width);
            self.show_toast(format!("Go to line {line_number}"));
        }

        self.close_go_input();
        Ok(())
    }

    /// ピッカーを開くか、すでに開いている場合はスコープを循環させる
    fn open_or_cycle_picker(&mut self) -> Result<()> {
        if self.picker.active {
            self.picker.scope = match self.picker.scope {
                PickerScope::All => PickerScope::Buffers,
                PickerScope::Buffers => PickerScope::All,
            };
            self.show_toast(format!(
                "Open [{}]",
                match self.picker.scope {
                    PickerScope::All => "all",
                    PickerScope::Buffers => "buffers",
                }
            ));
        } else {
            self.picker.active = true;
            self.picker.query.clear();
            self.picker.scope = PickerScope::All;
            self.show_toast("Open [all]");
        }

        self.refresh_picker_candidates()?;
        Ok(())
    }

    /// ピッカーを非アクティブ化してクエリとスコープをリセットする
    fn close_picker(&mut self) {
        self.picker.active = false;
        self.picker.query.clear();
        self.picker.scope = PickerScope::All;
    }

    /// スコープとクエリでフィルタした一致候補を返す
    fn filtered_picker_matches(&self) -> Vec<OpenCandidate> {
        self.ranked_picker_matches()
            .into_iter()
            .map(|matched| matched.candidate)
            .collect()
    }

    /// スコープとクエリでランク付けしたピッカー一致候補を返す
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

    /// ピッカーの先頭候補を選択してドキュメントを開きピッカーを閉じる
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

    #[allow(dead_code)]
    /// インサートモードを終了してノーマルモードに戻り、セマンティックトークンを更新する
    fn leave_insert_mode(&mut self, rewind_cursor: bool) {
        self.workspace.current_document_mut().end_undo_group();
        self.close_completion();
        self.mode = Mode::Normal;
        self.pending_insert_j = None;
        if rewind_cursor {
            self.cursor.column = self.cursor.column.saturating_sub(1);
        }
        self.refresh_current_document_semantic_tokens();
    }

    #[allow(dead_code)]
    /// 現在のドキュメントの変更内容をLSPに送信してセマンティックトークンを再取得する
    fn refresh_current_document_semantic_tokens(&mut self) {
        let Some(path) = self
            .workspace
            .current_document_path()
            .map(ToOwned::to_owned)
        else {
            return;
        };
        if self.lsp_command_for_path(&path).is_none() {
            return;
        }
        let Some(text) = self.workspace.current_document().full_text() else {
            return;
        };

        if matches!(self.lsp, LspClientState::Inactive) {
            let lsp_info = self.lsp_command_for_path(&path);
            self.lsp = match lsp_info {
                Some((cmd, args)) => match LspClient::start(Path::new("."), cmd, args) {
                    Ok(client) => LspClientState::Ready(client),
                    Err(_) => return,
                },
                None => return,
            };
        }

        let current_index = self.workspace.current_index;
        self.workspace.documents[current_index].version += 1;
        let version = self.workspace.documents[current_index].version;
        let language_id = self.lsp_language_id_for_path(&path);

        if let LspClientState::Ready(client) = &mut self.lsp {
            if client
                .ensure_open(&path, &language_id, version, &text)
                .is_err()
            {
                return;
            }
            self.workspace.documents[current_index].lsp_open = true;
            if client.did_change(&path, version, &text).is_err() {
                return;
            }
            let _ = client.request_semantic_tokens(&path);
        }
    }

    /// 現在のドキュメントをディスクに保存してLSPへ通知しトーストを表示する
    fn save_current_document(&mut self) -> Result<()> {
        let Some(path) = self
            .workspace
            .current_document_path()
            .map(ToOwned::to_owned)
        else {
            return Ok(());
        };
        self.workspace.current_document_mut().save(&path)?;
        if self.lsp_command_for_path(&path).is_some() {
            self.sync_current_document_save();
            self.poll_lsp();
            let summary = self.current_diagnostic_summary();
            self.last_save_feedback =
                Some(format!("LSP E{} W{}", summary.errors, summary.warnings));
        } else {
            self.last_save_feedback = Some("saved".to_owned());
        }
        self.show_toast(format!("Saved {}", display_name(&path)));

        Ok(())
    }

    /// 一時的なトーストメッセージを追加する
    fn show_toast(&mut self, message: impl Into<String>) {
        self.toast.transient_messages.push_back(ToastMessage {
            message: message.into(),
            expires_at: Instant::now() + Duration::from_secs(5),
        });
    }

    /// 永続的なトーストメッセージを設定する
    fn show_persistent_toast(&mut self, message: impl Into<String>) {
        self.toast.persistent_message = Some(message.into());
    }

    /// 永続的なトーストをクリアする
    fn clear_persistent_toast(&mut self) {
        self.toast.persistent_message = None;
    }

    /// 期限切れのトーストを削除し、変更があればtrueを返す
    fn prune_toast(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        while self
            .toast
            .transient_messages
            .front()
            .is_some_and(|toast| now >= toast.expires_at)
        {
            self.toast.transient_messages.pop_front();
            changed = true;
        }
        changed
    }

    /// 現在のバッファを閉じてワークスペースとUI状態を更新する
    fn close_current_buffer(&mut self) {
        if !self.workspace.has_documents() {
            return;
        }

        self.close_completion();
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

    /// レイアウトを次の状態に進めるかフォーカスを次のペインに移動する
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

    /// シングルペインレイアウトに戻してフォーカスを左ペインにする
    fn collapse_to_single_pane(&mut self) {
        if self.focused_pane == FocusedPane::Right && self.layout_mode == LayoutMode::TerminalSplit
        {
            self.layout_mode = LayoutMode::Single;
            return;
        }

        if self.layout_mode == LayoutMode::Dual
            && self.focused_pane == FocusedPane::Right
            && let Some(other_index) = self.workspace.secondary_index()
        {
            self.select_current_document(other_index);
        }

        self.layout_mode = LayoutMode::Single;
        self.focused_pane = FocusedPane::Left;
    }

    /// インサートモードで文字を挿入してカーソルを移動し補完を更新する
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
            self.schedule_completion_refresh();
        }
    }

    /// インサートモードで改行を挿入してカーソルを移動する
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
            self.close_completion();
        }
    }

    /// インサートモードでタブを挿入してカーソルを移動する
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
            self.close_completion();
        }
    }

    /// インサートモードでバックスペースしてカーソルを移動し補完を更新する
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
            self.schedule_completion_refresh();
        }
    }

    /// インサートモードでカーソル前方の文字を削除して補完を更新する
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
            self.schedule_completion_refresh();
        }
    }

    /// 補完UIを無効化してクリアする
    fn close_completion(&mut self) {
        self.completion.invalidate();
    }

    /// 補完の更新をスケジュールし、条件を満たさない場合は補完を閉じる
    fn schedule_completion_refresh(&mut self) {
        let Some(path) = self
            .workspace
            .current_document_path()
            .map(ToOwned::to_owned)
        else {
            self.close_completion();
            return;
        };
        if self.lsp_command_for_path(&path).is_none() {
            self.close_completion();
            return;
        }

        let page_width = self.current_page_width();
        let line = self
            .workspace
            .current_document()
            .display_line_text(self.cursor.row, page_width)
            .unwrap_or_default();
        let (query_start, query) = completion_prefix(&line, self.cursor.column);
        if query.is_empty() && !has_empty_completion_trigger(&line, self.cursor.column) {
            self.close_completion();
            return;
        }

        self.completion.active = false;
        self.completion.items.clear();
        self.completion.query_start = query_start;
        self.completion.query = query;
        self.completion.path = Some(path);
        self.completion.serial = self.completion.serial.saturating_add(1);
        let serial = self.completion.serial;
        self.completion.last_edit_at = None;
        self.completion.last_requested_serial = serial;
        self.completion.pending_request_serial = Some(serial);
        self.request_completion(serial);
    }

    /// 遅延補完リクエストの送信タイミングを確認して送信し、変更があればtrueを返す
    fn poll_completion(&mut self) -> bool {
        if self.mode != Mode::Insert
            || self.picker.active
            || self.search_input.active
            || self.replace_input.active
        {
            return false;
        }

        let Some(last_edit_at) = self.completion.last_edit_at else {
            return false;
        };
        if Instant::now().duration_since(last_edit_at) < Duration::from_millis(120) {
            return false;
        }

        if self.completion.serial == self.completion.last_requested_serial {
            return false;
        }

        let serial = self.completion.serial;
        self.completion.last_requested_serial = serial;
        self.completion.pending_request_serial = Some(serial);
        self.request_completion(serial);
        true
    }

    /// LSPまたはフォールバックで補完を要求する
    fn request_completion(&mut self, serial: u64) {
        let Some(path) = self
            .workspace
            .current_document_path()
            .map(ToOwned::to_owned)
        else {
            self.apply_completion_fallback(serial);
            return;
        };
        if self.lsp_command_for_path(&path).is_none() {
            self.close_completion();
            return;
        }

        let page_width = self.current_page_width();
        let Some(position) = self
            .workspace
            .current_document()
            .lsp_position_for_display_position(self.cursor.row, self.cursor.column, page_width)
        else {
            self.apply_completion_fallback(serial);
            return;
        };

        let Some(text) = self.workspace.current_document().full_text() else {
            self.apply_completion_fallback(serial);
            return;
        };

        match &mut self.lsp {
            LspClientState::NotAvailable
            | LspClientState::Failed(_)
            | LspClientState::Starting(_) => {
                self.apply_completion_fallback(serial);
            }
            LspClientState::Inactive => {
                let lsp_info = self.lsp_command_for_path(&path);
                self.lsp = match lsp_info {
                    Some((cmd, args)) => match LspClient::start(Path::new("."), cmd, args) {
                        Ok(client) => LspClientState::Starting(client),
                        Err(_) => {
                            self.apply_completion_fallback(serial);
                            return;
                        }
                    },
                    None => {
                        self.apply_completion_fallback(serial);
                        return;
                    }
                };
                self.apply_completion_fallback(serial);
            }
            LspClientState::Ready(_) => {
                if self
                    .request_completion_after_start(path, text, position, serial)
                    .is_err()
                {
                    self.apply_completion_fallback(serial);
                }
            }
        }
    }

    /// LSP起動済み状態でドキュメントを同期して補完リクエストを送信する
    fn request_completion_after_start(
        &mut self,
        path: PathBuf,
        text: String,
        position: Position,
        serial: u64,
    ) -> Result<()> {
        let current_index = self.workspace.current_index;
        self.workspace.documents[current_index].version += 1;
        let version = self.workspace.documents[current_index].version;
        let language_id = self.lsp_language_id_for_path(&path);
        let LspClientState::Ready(client) = &mut self.lsp else {
            self.apply_completion_fallback(serial);
            return Ok(());
        };
        client.ensure_open(&path, &language_id, version, &text)?;
        self.workspace.documents[current_index].lsp_open = true;
        client.did_change(&path, version, &text)?;
        client.completion(&path, position, serial)?;
        Ok(())
    }

    /// LSPから受け取った補完結果をランク付けして補完リストに反映する
    fn handle_completion_result(&mut self, path: PathBuf, serial: u64, items: Vec<CompletionItem>) {
        if self.mode != Mode::Insert {
            return;
        }
        if self.completion.serial != serial {
            return;
        }
        if self
            .completion
            .path
            .as_ref()
            .is_some_and(|current| current != &path)
        {
            return;
        }

        self.completion.pending_request_serial = None;
        let ranked = rank_completion_items(items, &self.completion.query, 8);
        if ranked.is_empty() {
            self.apply_completion_fallback(serial);
            return;
        }

        self.completion.items = ranked;
        self.completion.active = true;
    }

    /// LSP補完が得られない場合にドキュメント内テキストでフォールバック補完を適用する
    fn apply_completion_fallback(&mut self, serial: u64) {
        if self.completion.serial != serial {
            return;
        }
        let Some(text) = self.workspace.current_document().full_text() else {
            self.close_completion();
            return;
        };
        let items = collect_fallback_items(&text, &self.completion.query, 8);
        self.completion.items = items;
        self.completion.active = !self.completion.items.is_empty();
    }

    /// 先頭の補完候補をテキスト編集として適用してカーソルを移動する
    fn submit_completion(&mut self) -> bool {
        let Some(item) = self.completion.items.first().cloned() else {
            return false;
        };

        let page_width = self.current_page_width();
        let prefix_start = self.completion.query_start;
        let Some(start_position) = self
            .workspace
            .current_document()
            .lsp_position_for_display_position(self.cursor.row, prefix_start, page_width)
        else {
            self.close_completion();
            return false;
        };

        let edit = item.text_edit.clone().unwrap_or_else(|| {
            let end = self
                .workspace
                .current_document()
                .lsp_position_for_display_position(self.cursor.row, self.cursor.column, page_width)
                .unwrap_or(start_position);
            TextEdit {
                range: lsp_types::Range::new(start_position, end),
                new_text: item.insert_text.clone(),
            }
        });

        let cursor_position = text_end_position(edit.range.start, &edit.new_text);
        let applied = self
            .workspace
            .current_document_mut()
            .apply_text_edits(&[edit]);
        if applied {
            self.workspace.documents[self.workspace.current_index].version += 1;
            if let Some((row, column)) = self
                .workspace
                .current_document()
                .display_position_for_lsp_position(cursor_position, page_width)
            {
                self.cursor.row = row;
                self.cursor.column = column;
                self.clamp_vertical_state();
            }
        }
        self.close_completion();
        true
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    fn copy_selection_or_line(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        if let Some(anchor) = self.selection_anchor {
            let cursor_row = self.cursor.row;
            let cursor_col = self.cursor.column;
            let (sr, sc, er, ec) =
                normalize_selection(anchor.row, anchor.column, cursor_row, cursor_col);
            let text = self
                .workspace
                .current_document()
                .text_for_display_range(sr, sc, er, ec, page_width);
            self.yank_buffer = YankBuffer::Charwise(text);
            self.selection_anchor = None;
        } else if let Some(line_text) = self
            .workspace
            .current_document()
            .current_line_text(self.cursor.row, page_width)
        {
            self.yank_buffer = YankBuffer::Linewise(line_text);
        }
        Ok(())
    }

    fn cut_selection_or_line(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        if let Some(anchor) = self.selection_anchor {
            let cursor_row = self.cursor.row;
            let cursor_col = self.cursor.column;
            let (sr, sc, er, ec) =
                normalize_selection(anchor.row, anchor.column, cursor_row, cursor_col);
            let text = self
                .workspace
                .current_document()
                .text_for_display_range(sr, sc, er, ec, page_width);
            self.yank_buffer = YankBuffer::Charwise(text);
            self.selection_anchor = None;
            if let Some((row, col)) = self
                .workspace
                .current_document_mut()
                .remove_display_range(sr, sc, er, ec, page_width)
            {
                self.cursor.row = row;
                self.cursor.column = col;
            }
            self.clamp_vertical_state();
        } else {
            self.delete_current_line()?;
        }
        Ok(())
    }

    fn delete_selection(&mut self) -> Result<()> {
        let Some(anchor) = self.selection_anchor.take() else {
            return Ok(());
        };
        let page_width = self.current_page_width();
        let (sr, sc, er, ec) = normalize_selection(
            anchor.row,
            anchor.column,
            self.cursor.row,
            self.cursor.column,
        );
        if let Some((row, col)) = self
            .workspace
            .current_document_mut()
            .remove_display_range(sr, sc, er, ec, page_width)
        {
            self.cursor.row = row;
            self.cursor.column = col;
        }
        self.clamp_vertical_state();
        Ok(())
    }

    fn delete_selection_if_any(&mut self) -> Result<()> {
        if self.selection_anchor.is_some() {
            self.delete_selection()?;
        }
        Ok(())
    }

    fn select_all(&mut self) {
        let page_width = self.current_page_width();
        let total_rows = self
            .workspace
            .current_document()
            .total_rows(page_width)
            .unwrap_or(0);
        if total_rows == 0 {
            return;
        }
        self.selection_anchor = Some(CursorState { row: 0, column: 0 });
        let last_row = total_rows.saturating_sub(1);
        let last_line_width = self
            .workspace
            .current_document()
            .display_line_width(last_row, page_width)
            .unwrap_or(0);
        self.cursor.row = last_row;
        self.cursor.column = last_line_width;
        self.clamp_vertical_state();
    }

    fn select_word_or_next_occurrence(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        if let Some(anchor) = self.selection_anchor {
            let cursor_row = self.cursor.row;
            let cursor_col = self.cursor.column;
            let (sr, sc, er, ec) =
                normalize_selection(anchor.row, anchor.column, cursor_row, cursor_col);
            let selected_text = self
                .workspace
                .current_document()
                .text_for_display_range(sr, sc, er, ec, page_width);
            if selected_text.is_empty() {
                return Ok(());
            }
            let total_rows = self
                .workspace
                .current_document()
                .total_rows(page_width)
                .unwrap_or(0);
            for row in er..total_rows {
                let row_line = self
                    .workspace
                    .current_document()
                    .display_line_text(row, page_width)
                    .unwrap_or_default();
                let start_col = if row == er { ec } else { 0 };
                if let Some(byte_offset) = row_line.find(&selected_text) {
                    let char_col = row_line[..byte_offset].chars().count();
                    if char_col >= start_col || row > er {
                        self.selection_anchor = Some(CursorState {
                            row,
                            column: char_col,
                        });
                        self.cursor.row = row;
                        self.cursor.column = char_col + selected_text.chars().count();
                        self.clamp_vertical_state();
                        return Ok(());
                    }
                }
            }
            return Ok(());
        }

        let line = self
            .workspace
            .current_document()
            .display_line_text(self.cursor.row, page_width)
            .unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            return Ok(());
        }
        let col = self.cursor.column.min(chars.len().saturating_sub(1));
        let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        if !is_word(chars.get(col).copied().unwrap_or(' ')) {
            return Ok(());
        }
        let mut start = col;
        while start > 0 && is_word(chars[start - 1]) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < chars.len() && is_word(chars[end]) {
            end += 1;
        }
        self.selection_anchor = Some(CursorState {
            row: self.cursor.row,
            column: start,
        });
        self.cursor.column = end;
        Ok(())
    }

    fn toggle_comment(&mut self) -> Result<()> {
        let page_width = self.current_page_width();
        let comment_prefix = self
            .workspace
            .current_document_path()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(|ext| match ext {
                "toml" | "py" | "sh" | "yaml" | "yml" | "rb" => "# ",
                _ => "// ",
            })
            .unwrap_or("// ");

        let (start_row, end_row) = if let Some(anchor) = self.selection_anchor {
            let cursor_row = self.cursor.row;
            let (ar, _, er, _) =
                normalize_selection(anchor.row, anchor.column, cursor_row, self.cursor.column);
            (ar, er)
        } else {
            (self.cursor.row, self.cursor.row)
        };

        let all_commented = (start_row..=end_row).all(|row| {
            let text = self
                .workspace
                .current_document()
                .display_line_text(row, page_width)
                .unwrap_or_default();
            text.starts_with(comment_prefix)
        });

        for row in start_row..=end_row {
            let text = self
                .workspace
                .current_document()
                .display_line_text(row, page_width)
                .unwrap_or_default();
            if all_commented {
                let new_text = text
                    .strip_prefix(comment_prefix)
                    .unwrap_or(&text)
                    .to_owned();
                self.workspace
                    .current_document_mut()
                    .replace_line_display(row, &new_text, page_width);
            } else {
                let new_text = format!("{comment_prefix}{text}");
                self.workspace
                    .current_document_mut()
                    .replace_line_display(row, &new_text, page_width);
            }
        }
        self.selection_anchor = None;
        Ok(())
    }

    fn add_extra_cursor_above(&mut self) {
        let row = if let Some(last) = self.extra_cursors.last() {
            last.row.saturating_sub(1)
        } else {
            self.cursor.row.saturating_sub(1)
        };
        if !self.extra_cursors.iter().any(|c| c.row == row) && row < self.cursor.row {
            self.extra_cursors.push(CursorState {
                row,
                column: self.cursor.column,
            });
        }
    }

    fn add_extra_cursor_below(&mut self) {
        let page_width = self.current_page_width();
        let total = self
            .workspace
            .current_document()
            .total_rows(page_width)
            .unwrap_or(0);
        let row = if let Some(last) = self.extra_cursors.last() {
            last.row.saturating_add(1)
        } else {
            self.cursor.row.saturating_add(1)
        };
        if row < total && !self.extra_cursors.iter().any(|c| c.row == row) {
            self.extra_cursors.push(CursorState {
                row,
                column: self.cursor.column,
            });
        }
    }

    fn open_line_above(&mut self) {
        let page_width = self.current_page_width();
        self.workspace.current_document_mut().begin_undo_group();
        if let Some((row, column)) = self
            .workspace
            .current_document_mut()
            .open_above(self.cursor.row, page_width)
        {
            self.cursor.row = row;
            self.cursor.column = column;
            self.clamp_vertical_state();
        }
    }

    fn insert_char_for_all_cursors(&mut self, ch: char) {
        self.insert_char(ch);
        self.extra_cursors.clear();
    }

    fn backspace_char_for_all_cursors(&mut self) {
        self.backspace_char();
        self.extra_cursors.clear();
    }

    fn insert_newline_for_all_cursors(&mut self) {
        self.insert_newline();
        self.extra_cursors.clear();
    }
}

pub(super) fn normalize_selection(
    ar: usize,
    ac: usize,
    cr: usize,
    cc: usize,
) -> (usize, usize, usize, usize) {
    if ar < cr || (ar == cr && ac <= cc) {
        (ar, ac, cr, cc)
    } else {
        (cr, cc, ar, ac)
    }
}

/// パスからファイル名部分を返す
fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

/// パスをカレントディレクトリからの相対表示に変換する
fn workspace_relative_display(path: &Path) -> String {
    let Ok(current_dir) = std::env::current_dir() else {
        return path.display().to_string();
    };

    path.strip_prefix(&current_dir)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// パスのファイル最終更新時刻を返す
fn path_modified_time(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

/// 相対パスをカレントディレクトリを基準に絶対パスへ変換し、可能ならcanonicalizeする。
///
/// LSPサーバー（rust-analyzer等）はシンボリックリンクを解決した正規パスでURIを返すため、
/// ドキュメントのキーも同じ正規形に揃えておかないと診断・セマンティックトークンが一致しない。
/// 未作成ファイルなどcanonicalizeできない場合は絶対パスのまま返す。
fn normalize_workspace_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.canonicalize().unwrap_or(absolute))
}

/// 指定ディレクトリ配下の.rsファイルを再帰的に収集する
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

#[allow(dead_code)]
/// インサートモードのjjエスケープ待機タイムアウトを返す
fn insert_escape_timeout() -> Duration {
    Duration::from_millis(300)
}

/// FindKindと発見列からカーソルの移動先列を計算する
fn motion_destination_column(find_kind: FindKind, found_column: usize) -> usize {
    match find_kind {
        FindKind::Forward => found_column.saturating_add(1),
        FindKind::Backward => found_column,
        FindKind::TillForward => found_column,
        FindKind::TillBackward => found_column.saturating_add(1),
    }
}

/// reverseフラグに応じてFindKindの方向を反転させる
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

#[allow(dead_code)]
/// カーソルと発見位置からFindKindに応じたオペレータ操作範囲を計算する
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
        FindKind::Backward => (found_row, found_column, cursor_row, cursor_column),
        FindKind::TillBackward => (
            found_row,
            found_column.saturating_add(1),
            cursor_row,
            cursor_column,
        ),
    };

    (start_row < end_row || end_column > start_column).then_some((
        start_row,
        start_column,
        end_row,
        end_column,
    ))
}

/// 診断の重大度を表示ラベル文字列に変換する
fn diagnostic_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Error => "Error",
    }
}

/// セマンティックトークンリクエストを診断の後まで遅延すべきかを返す
fn should_wait_for_diagnostics_before_semantic(path: &Path) -> bool {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| cwd.canonicalize().ok())
        .is_some_and(|cwd| path.starts_with(cwd))
}

#[allow(dead_code)]
fn _project_file_display_name(candidate: &ProjectFileCandidate) -> &str {
    &candidate.display_name
}
