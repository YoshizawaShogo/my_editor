mod command;
mod effect;
mod event;
mod focus;
mod layout;

pub use command::{Command, Direction, Unit, VerticalDirection};
pub use effect::Effect;
pub use event::{
    AppEvent, FileScanEvent, GitEvent, GitInfo, GitLine, GitLineKind, GrepEvent, GrepHit, IoEvent,
    MouseInput, TerminalEvent,
};
pub use focus::{Focus, Side};
pub use layout::{EditorPane, Layout};

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};

use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use ropey::Rope;

use crate::{
    clipboard::Register,
    config::Config,
    document::{Document, DocumentId, LargeFile},
    lsp::LspEvent,
    position::{CharIdx, char_idx_to_display_pos, display_col_after, display_col_to_char_idx},
    view::{Selection, View, is_word, move_head},
};

pub struct Editor {
    documents: HashMap<DocumentId, Document>,
    next_doc_id: u64,
    layout: Layout,
    focus: Focus,
    clipboard: Register,
    drag_anchor: Option<CharIdx>,
    picker: Option<PickerState>,
    config: Config,
    workspace_root: PathBuf,
    next_scan_token: u64,
    search: Option<SearchState>,
    next_grep_token: u64,
    lsp_servers: HashMap<String, u64>,
    lsp_spawned: HashSet<u64>,
    lsp_ready: HashSet<u64>,
    lsp_hover_capable: HashSet<u64>,
    lsp_incremental_sync: HashSet<u64>,
    lsp_errors: HashMap<u64, String>,
    lsp_opened_documents: HashSet<DocumentId>,
    semantic_ready_versions: HashMap<DocumentId, i32>,
    hover_ready_documents: HashSet<DocumentId>,
    hover_probe_successes: HashMap<DocumentId, usize>,
    hover_probe_attempts: HashMap<DocumentId, usize>,
    lsp_restart_counts: HashMap<u64, u8>,
    next_server_id: u64,
    pending_lsp: HashMap<i64, PendingLsp>,
    next_lsp_request: i64,
    completion: Option<CompletionState>,
    completion_suppressed: Option<(DocumentId, i32)>,
    rename_input: Option<String>,
    confirm: Option<ConfirmState>,
    hover: Option<String>,
    deferred_hover: Option<(DocumentId, CharIdx)>,
    pending_lsp_sync: HashSet<DocumentId>,
    document_versions: HashMap<DocumentId, i32>,
    pending_self_disk_updates: HashMap<DocumentId, usize>,
    terminal: Option<vt100::Parser>,
    terminal_selection: Option<TerminalSelection>,
    terminal_size: (u16, u16),
    status: Option<String>,
    notifications: Vec<Toast>,
    progress: HashMap<String, String>,
    dirty: bool,
    quit: bool,
}

impl Default for Editor {
    fn default() -> Self {
        let id = DocumentId(0);
        let documents = HashMap::from([(id, Document::scratch())]);
        Self {
            documents,
            next_doc_id: 1,
            layout: Layout::EditorFull(EditorPane {
                view: View::new(id),
            }),
            focus: Focus::Editor(Side::Left),
            clipboard: Register::default(),
            drag_anchor: None,
            picker: None,
            config: Config::default(),
            workspace_root: PathBuf::from("."),
            next_scan_token: 1,
            search: None,
            next_grep_token: 1,
            lsp_servers: HashMap::new(),
            lsp_spawned: HashSet::new(),
            lsp_ready: HashSet::new(),
            lsp_hover_capable: HashSet::new(),
            lsp_incremental_sync: HashSet::new(),
            lsp_errors: HashMap::new(),
            lsp_opened_documents: HashSet::new(),
            semantic_ready_versions: HashMap::new(),
            hover_ready_documents: HashSet::new(),
            hover_probe_successes: HashMap::new(),
            hover_probe_attempts: HashMap::new(),
            lsp_restart_counts: HashMap::new(),
            next_server_id: 1,
            pending_lsp: HashMap::new(),
            next_lsp_request: 1,
            completion: None,
            completion_suppressed: None,
            rename_input: None,
            confirm: None,
            hover: None,
            deferred_hover: None,
            pending_lsp_sync: HashSet::new(),
            document_versions: HashMap::new(),
            pending_self_disk_updates: HashMap::new(),
            terminal: None,
            terminal_selection: None,
            terminal_size: (0, 0),
            status: None,
            notifications: Vec::new(),
            progress: HashMap::new(),
            dirty: true,
            quit: false,
        }
    }
}

impl Editor {
    pub fn update(&mut self, event: AppEvent) -> Vec<Effect> {
        let mut effects = match event {
            AppEvent::Command(command) => self.apply_command(command),
            AppEvent::TextInput(character) => {
                if self.focus == Focus::Overlay {
                    return self.overlay_input(character);
                }
                self.edit_active(|document, view| {
                    document
                        .editable_mut()
                        .insert(&mut view.selections, &character.to_string());
                });
                Vec::new()
            }
            AppEvent::TextInputAt { character, at } => {
                if self.focus == Focus::Overlay {
                    return self.overlay_input(character);
                }
                self.edit_active(|document, view| {
                    document.editable_mut().insert_timed(
                        &mut view.selections,
                        &character.to_string(),
                        at,
                    );
                });
                Vec::new()
            }
            AppEvent::TextPaste(text) => {
                if self.focus == Focus::Overlay {
                    let mut effects = Vec::new();
                    for character in text.chars() {
                        effects.extend(self.overlay_input(character));
                    }
                    return effects;
                }
                self.edit_active(|document, view| {
                    document.editable_mut().insert(&mut view.selections, &text);
                });
                Vec::new()
            }
            AppEvent::Resize { cols, rows } => {
                self.terminal_size = (cols, rows);
                let mut effects = Vec::new();
                if let Some(parser) = &mut self.terminal {
                    let shell_cols = split_right_width(cols).max(1);
                    let shell_rows = rows.saturating_sub(1).max(1);
                    parser.set_size(shell_rows, shell_cols);
                    effects.push(Effect::TerminalResize {
                        cols: shell_cols,
                        rows: shell_rows,
                    });
                }
                self.ensure_cursor_visible();
                self.dirty = true;
                effects
            }
            AppEvent::Mouse(mouse) => {
                let on_split_divider = matches!(
                    self.layout,
                    Layout::EditorAndEditor { .. } | Layout::EditorAndShell { .. }
                ) && mouse.event.column
                    == split_left_width(self.terminal_size.0);
                let definition =
                    matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left))
                        && mouse.event.modifiers.contains(KeyModifiers::CONTROL)
                        && !on_split_divider;
                let hover = matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left))
                    && !definition
                    && !on_split_divider;
                let copy_shell_selection = self.focus == Focus::Shell
                    && matches!(mouse.event.kind, MouseEventKind::Up(MouseButton::Left));
                self.apply_mouse(mouse);
                if copy_shell_selection {
                    self.copy_shell_selection(false)
                } else if definition {
                    self.request_definition()
                } else if hover
                    && self.layout.active_editor(self.focus).is_some_and(|pane| {
                        pane.view
                            .selections
                            .iter()
                            .all(|selection| selection.is_caret())
                    })
                {
                    let index = self
                        .layout
                        .active_editor(self.focus)
                        .map(|pane| pane.view.selections.primary().head);
                    index.map_or_else(Vec::new, |index| self.request_hover_at(index))
                } else {
                    Vec::new()
                }
            }
            AppEvent::Io(event) => self.apply_io(event),
            AppEvent::ConfigLoaded(result) => {
                let effects = match result {
                    Ok(config) => {
                        self.config = config;
                        crate::highlight::warm_hover_highlighting();
                        self.refresh_languages();
                        self.start_workspace_lsps()
                    }
                    Err(error) => {
                        self.status = Some(error);
                        Vec::new()
                    }
                };
                self.dirty = true;
                effects
            }
            AppEvent::FileScan(event) => {
                self.apply_file_scan(event);
                Vec::new()
            }
            AppEvent::Grep(event) => {
                self.apply_grep(event);
                Vec::new()
            }
            AppEvent::Lsp(event) => self.apply_lsp(event),
            AppEvent::Terminal(event) => {
                self.apply_terminal(event);
                Vec::new()
            }
            AppEvent::TerminalInput(bytes) => {
                self.terminal_selection = None;
                self.dirty = true;
                vec![Effect::TerminalInput(bytes)]
            }
            AppEvent::Git(event) => {
                if let Ok(info) = event.result
                    && let Some(document) = self.documents.get_mut(&event.doc)
                {
                    document.git_branch = info.branch;
                    document.git_status = info.status;
                    if let crate::document::DocumentKind::Editable(editable) = &mut document.kind {
                        editable.git_lines = info.lines;
                    }
                }
                self.dirty = true;
                Vec::new()
            }
            AppEvent::Tick => {
                self.notifications
                    .retain(|toast| toast.created.elapsed() < toast.ttl);
                self.dirty = true;
                let files = self
                    .documents
                    .iter()
                    .filter_map(|(id, document)| document.path.clone().map(|path| (*id, path)))
                    .collect();
                vec![Effect::CheckDiskStates(files)]
            }
            AppEvent::Error(message) => {
                self.notify(ToastLevel::Error, message.clone());
                self.status = Some(message);
                self.dirty = true;
                Vec::new()
            }
        };
        let has_selection = self.layout.active_editor(self.focus).is_some_and(|pane| {
            pane.view
                .selections
                .iter()
                .any(|selection| !selection.is_caret())
        });
        if has_selection {
            if self.hover.take().is_some() {
                self.dirty = true;
            }
            self.deferred_hover = None;
        }
        effects.extend(self.take_lsp_sync_effects());
        effects.extend(self.retry_deferred_hover());
        effects
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn terminal_size(&self) -> (u16, u16) {
        self.terminal_size
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }

    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn active_buffer(&self) -> Option<ActiveBuffer<'_>> {
        let focus = if self.focus == Focus::Shell {
            Focus::Editor(Side::Left)
        } else {
            self.focus
        };
        let pane = self.layout.active_editor(focus)?;
        let document = self.documents.get(&pane.view.doc)?;
        let editable = document.editable_opt()?;
        Some(ActiveBuffer {
            name: document
                .path
                .as_ref()
                .map_or_else(|| "Untitled".to_owned(), |path| self.display_path(path)),
            text: editable.text(),
            view: &pane.view,
            modified: editable.modified,
            external_changed: document.external_changed,
            language: document.language.as_deref(),
            tab_size: self
                .config
                .indentation_for_language(document.language.as_deref())
                .0,
            language_status: self.document_language_status(pane.view.doc, document),
            diagnostics: &editable.diagnostics,
            git_lines: &editable.git_lines,
            git_branch: document.git_branch.as_deref(),
            git_status: document.git_status.as_deref(),
            semantic_spans: &editable.semantic_spans,
            syntax_spans: editable
                .syntax
                .as_ref()
                .map_or(&[], crate::highlight::IncrementalHighlighter::spans),
        })
    }

    pub fn show_start_page(&self) -> bool {
        self.documents.len() == 1
            && self.documents.values().next().is_some_and(|document| {
                document.path.is_none()
                    && document.editable_opt().is_some_and(|editable| {
                        editable.text().len_chars() == 0 && !editable.modified
                    })
            })
    }

    pub fn active_large_buffer(&self) -> Option<LargeBuffer<'_>> {
        let focus = if self.focus == Focus::Shell {
            Focus::Editor(Side::Left)
        } else {
            self.focus
        };
        let pane = self.layout.active_editor(focus)?;
        let document = self.documents.get(&pane.view.doc)?;
        Some(LargeBuffer {
            file: document.large()?,
            view: &pane.view,
        })
    }

    pub fn open_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<Effect> {
        let paths: Vec<_> = paths.into_iter().collect();
        if !paths.is_empty() {
            self.documents.remove(&DocumentId(0));
        }
        let mut effects = Vec::new();
        for path in paths {
            let id = DocumentId(self.next_doc_id);
            self.next_doc_id += 1;
            let mut document = Document::scratch();
            document.path = Some(path.clone());
            document.language = self
                .config
                .language_for_path(&path)
                .map(|language| language.name.clone());
            self.documents.insert(id, document);
            if matches!(self.layout, Layout::EditorAndEditor { diff: true, .. }) {
                self.layout = Layout::EditorFull(EditorPane {
                    view: View::new(id),
                });
                self.focus = Focus::Editor(Side::Left);
            } else if let Some(pane) = self.layout.active_editor_mut(self.focus) {
                pane.view = View::new(id);
            }
            effects.push(Effect::ReadFile { id, path });
        }
        effects
    }

    pub fn set_workspace_root(&mut self, root: PathBuf) {
        self.workspace_root = root;
    }

    pub fn split_buffers(&self) -> Option<(ActiveBuffer<'_>, ActiveBuffer<'_>, bool)> {
        let (left, right, diff) = self.layout.split()?;
        let left_doc = self.documents.get(&left.view.doc)?;
        let right_doc = self.documents.get(&right.view.doc)?;
        let left_document = left_doc.editable_opt()?;
        let right_document = right_doc.editable_opt()?;
        Some((
            ActiveBuffer {
                name: left_doc
                    .path
                    .as_ref()
                    .map_or_else(|| "Untitled".to_owned(), |path| self.display_path(path)),
                text: left_document.text(),
                view: &left.view,
                modified: left_document.modified,
                external_changed: left_doc.external_changed,
                language: None,
                tab_size: self
                    .config
                    .indentation_for_language(left_doc.language.as_deref())
                    .0,
                language_status: self.document_language_status(left.view.doc, left_doc),
                diagnostics: &left_document.diagnostics,
                git_lines: &left_document.git_lines,
                git_branch: left_doc.git_branch.as_deref(),
                git_status: left_doc.git_status.as_deref(),
                semantic_spans: &left_document.semantic_spans,
                syntax_spans: left_document
                    .syntax
                    .as_ref()
                    .map_or(&[], crate::highlight::IncrementalHighlighter::spans),
            },
            ActiveBuffer {
                name: right_doc
                    .path
                    .as_ref()
                    .map_or_else(|| "Untitled".to_owned(), |path| self.display_path(path)),
                text: right_document.text(),
                view: &right.view,
                modified: right_document.modified,
                external_changed: right_doc.external_changed,
                language: None,
                tab_size: self
                    .config
                    .indentation_for_language(right_doc.language.as_deref())
                    .0,
                language_status: self.document_language_status(right.view.doc, right_doc),
                diagnostics: &right_document.diagnostics,
                git_lines: &right_document.git_lines,
                git_branch: right_doc.git_branch.as_deref(),
                git_status: right_doc.git_status.as_deref(),
                semantic_spans: &right_document.semantic_spans,
                syntax_spans: right_document
                    .syntax
                    .as_ref()
                    .map_or(&[], crate::highlight::IncrementalHighlighter::spans),
            },
            diff,
        ))
    }

    pub fn picker_view(&self) -> Option<PickerView> {
        let picker = self.picker.as_ref()?;
        const VIEW_WINDOW: usize = 5;
        let start = picker
            .selected
            .saturating_sub(VIEW_WINDOW / 2)
            .min(picker.filtered.len().saturating_sub(VIEW_WINDOW));
        let matcher = SkimMatcherV2::default();
        let items = picker
            .filtered
            .iter()
            .skip(start)
            .take(VIEW_WINDOW)
            .filter_map(|index| picker.candidates.get(*index))
            .map(|candidate| {
                let label = self.candidate_label(candidate);
                let matched = if picker.query.is_empty() {
                    Vec::new()
                } else {
                    matcher
                        .fuzzy_indices(&label, &picker.query)
                        .map_or_else(Vec::new, |(_, indices)| indices)
                };
                PickerViewItem { label, matched }
            })
            .collect();
        Some(PickerView {
            title: match picker.mode {
                PickerMode::Directory => "Open file",
                PickerMode::Buffer => "Open buffer",
                PickerMode::Diff => "Compare with buffer",
                PickerMode::Command => "Command Palette · key / command / description",
            },
            query: picker.query.clone(),
            items,
            selected: picker.selected.saturating_sub(start),
            has_before: start > 0,
            has_after: start + VIEW_WINDOW < picker.filtered.len(),
            total: picker.filtered.len(),
        })
    }

    pub fn search_view(&self) -> Option<SearchView> {
        let search = self.search.as_ref()?;
        let items = search
            .hits
            .iter()
            .take(12)
            .map(|hit| match hit {
                SearchHit::Buffer { doc, range } => {
                    format!(
                        "{}  {}..{}",
                        self.document_label(*doc),
                        range.start,
                        range.end
                    )
                }
                SearchHit::Disk(hit) => {
                    format!(
                        "{}:{}  {}",
                        hit.path.display(),
                        hit.line + 1,
                        hit.text.trim()
                    )
                }
            })
            .collect();
        Some(SearchView {
            query: search.query.clone(),
            replacement: search.replacement.clone(),
            editing_replace: search.editing_replace,
            editing_filter: search.editing_filter,
            scope: search.scope,
            options: search.options,
            include: search.include_input.clone(),
            exclude: search.exclude_input.clone(),
            filters: search.filters.clone(),
            items,
            current: search.current,
        })
    }

    pub fn completion_view(&self) -> Option<CompletionView> {
        let completion = self.completion.as_ref()?;
        Some(CompletionView {
            items: completion
                .items
                .iter()
                .take(12)
                .map(|item| item.label.clone())
                .collect(),
            selected: completion.selected,
            anchor: completion.anchor,
        })
    }

    pub fn rename_view(&self) -> Option<&str> {
        self.rename_input.as_deref()
    }

    pub fn confirm_view(&self) -> Option<&str> {
        self.confirm
            .as_ref()
            .map(|confirm| confirm.message.as_str())
    }

    pub fn hover_view(&self) -> Option<&str> {
        self.hover.as_deref()
    }

    pub fn terminal_contents(&self) -> Option<String> {
        self.terminal
            .as_ref()
            .map(|parser| parser.screen().contents())
    }

    pub fn terminal_screen(&self) -> Option<&vt100::Screen> {
        self.terminal_selection
            .as_ref()
            .map(|selection| &selection.snapshot)
            .or_else(|| self.terminal.as_ref().map(vt100::Parser::screen))
    }

    pub fn terminal_selection_view(&self) -> Option<TerminalSelectionView> {
        let selection = self.terminal_selection.as_ref()?;
        (selection.anchor != selection.head).then(|| {
            let (start, end) = ordered_terminal_points(selection.anchor, selection.head);
            TerminalSelectionView { start, end }
        })
    }

    pub fn shell_focused(&self) -> bool {
        self.focus == Focus::Shell
    }

    pub fn shell_visible(&self) -> bool {
        matches!(self.layout, Layout::EditorAndShell { .. })
    }

    pub fn focused_side(&self) -> Side {
        match self.focus {
            Focus::Editor(side) | Focus::Completion(side) => side,
            Focus::Shell | Focus::Overlay => Side::Left,
        }
    }

    pub fn notification_views(&self) -> Vec<NotificationView<'_>> {
        self.progress
            .values()
            .map(|text| NotificationView {
                level: ToastLevel::Info,
                text: text.as_str(),
            })
            .chain(
                self.notifications
                    .iter()
                    .rev()
                    .map(|toast| NotificationView {
                        level: toast.level,
                        text: &toast.text,
                    }),
            )
            .take(4)
            .collect()
    }

    fn notify(&mut self, level: ToastLevel, text: impl Into<String>) {
        let ttl = match level {
            ToastLevel::Error => Duration::from_secs(8),
            ToastLevel::Warn => Duration::from_secs(6),
            ToastLevel::Info | ToastLevel::Success => Duration::from_secs(4),
        };
        self.notifications.push(Toast {
            level,
            text: text.into(),
            created: Instant::now(),
            ttl,
        });
        self.dirty = true;
    }

    fn set_progress(&mut self, key: impl Into<String>, text: impl Into<String>) {
        self.progress.insert(key.into(), text.into());
        self.dirty = true;
    }

    fn finish_progress(&mut self, key: &str) {
        self.progress.remove(key);
        self.dirty = true;
    }

    fn apply_io(&mut self, event: IoEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        match event {
            IoEvent::FileLoaded { id, result } => match result {
                Ok(contents) => {
                    if let Some(document) = self.documents.get_mut(&id) {
                        document.load_text(&contents);
                        document.external_changed = false;
                        if let Some(language) = document.language.clone() {
                            document.editable_mut().enable_highlight(&language);
                        }
                        self.status = None;
                        if let Some(path) = document.path.clone() {
                            effects.push(Effect::ComputeGitStatus { doc: id, path });
                        }
                    }
                    effects.extend(self.start_or_open_lsp(id));
                }
                Err(error) => self.status = Some(error),
            },
            IoEvent::LargeFileLoaded { id, result } => match result {
                Ok(large) => {
                    if let Some(document) = self.documents.get_mut(&id) {
                        document.load_large(large);
                        self.status = Some("大容量ファイルを読み取り専用で開きました".to_owned());
                    }
                }
                Err(error) => self.status = Some(error),
            },
            IoEvent::FileSaved { id, result } => match result {
                Ok(()) => {
                    self.notify(ToastLevel::Success, "保存しました");
                    if let Some(document) = self.documents.get_mut(&id) {
                        document.editable_mut().mark_saved();
                        document.external_changed = false;
                        if let Some(path) = document.path.as_deref() {
                            effects.push(Effect::ComputeGitStatus {
                                doc: id,
                                path: path.to_path_buf(),
                            });
                            if let Some(server) = document
                                .language
                                .as_ref()
                                .and_then(|language| self.lsp_servers.get(language))
                            {
                                effects.push(Effect::LspSend {
                                    server: *server,
                                    message: serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "method": "textDocument/didSave",
                                        "params": {"textDocument": {
                                            "uri": format!("file://{}", path.display())
                                        }}
                                    })
                                    .to_string(),
                                });
                            }
                        }
                    }
                    *self.pending_self_disk_updates.entry(id).or_insert(0) += 1;
                }
                Err(error) => {
                    self.notify(ToastLevel::Error, error.clone());
                    self.status = Some(error);
                }
            },
            IoEvent::SaveConflict { id, path } => {
                if let Some(document) = self.documents.get_mut(&id) {
                    document.external_changed = true;
                }
                self.notify(ToastLevel::Warn, "外部変更との保存競合を検出しました");
                self.confirm = Some(ConfirmState {
                    message: format!(
                        "外部変更があります。上書きしますか? {}  [Enter: 上書き / Esc: 中止]",
                        path.display()
                    ),
                    action: ConfirmAction::Overwrite(id),
                });
                self.focus = Focus::Overlay;
            }
            IoEvent::DirectoryReplaceFinished { result } => match result {
                Ok(files) => {
                    let message = format!("ディレクトリ置換完了: {files}ファイル");
                    self.notify(ToastLevel::Success, message.clone());
                    self.status = Some(message);
                }
                Err(error) => {
                    self.notify(ToastLevel::Error, error.clone());
                    self.status = Some(error);
                }
            },
            IoEvent::ExternalEditsFinished { result } => match result {
                Ok(path) => {
                    let message = format!("LSP変更を保存しました: {}", path.display());
                    self.notify(ToastLevel::Success, message.clone());
                    self.status = Some(message);
                }
                Err(error) => {
                    self.notify(ToastLevel::Error, error.clone());
                    self.status = Some(error);
                }
            },
            IoEvent::DiskStateObserved { id, result } => match result {
                Ok(state) => {
                    let self_saved =
                        if let Some(count) = self.pending_self_disk_updates.get_mut(&id) {
                            *count -= 1;
                            let finished = *count == 0;
                            if finished {
                                self.pending_self_disk_updates.remove(&id);
                            }
                            true
                        } else {
                            false
                        };
                    let Some(document) = self.documents.get_mut(&id) else {
                        return effects;
                    };
                    let changed = document.disk_state.is_some_and(|old| old != state);
                    document.disk_state = Some(state);
                    if changed && !self_saved {
                        if document
                            .editable_opt()
                            .is_some_and(|editable| editable.modified)
                        {
                            document.external_changed = true;
                            self.status = Some(format!(
                                "外部変更を検出しました（編集中）: {}",
                                document.path.as_deref().map_or_else(
                                    || "?".to_owned(),
                                    |path| path.display().to_string()
                                )
                            ));
                        } else if let Some(path) = document.path.clone() {
                            effects.push(Effect::ReadFile { id, path });
                            self.status = Some("外部変更を再読込しました".to_owned());
                        }
                    }
                }
                Err(error) => self.status = Some(error),
            },
            IoEvent::DirectPathResolved {
                path,
                exists,
                parent_exists,
                inside_root,
            } => {
                if exists {
                    effects.extend(self.open_paths([path]));
                } else if inside_root && parent_exists {
                    let id = DocumentId(self.next_doc_id);
                    self.next_doc_id += 1;
                    let mut document = Document::scratch();
                    document.path = Some(path.clone());
                    document.language = self
                        .config
                        .language_for_path(&path)
                        .map(|language| language.name.clone());
                    self.documents.insert(id, document);
                    self.layout = Layout::EditorFull(EditorPane {
                        view: View::new(id),
                    });
                    self.status = Some(format!("新規ファイル: {}", path.display()));
                    effects.extend(self.start_or_open_lsp(id));
                } else if inside_root {
                    self.status = Some("ディレクトリが存在しません".to_owned());
                } else {
                    self.status = Some("ワークスペース外には新規作成できません".to_owned());
                }
                self.focus = Focus::Editor(Side::Left);
            }
        }
        self.dirty = true;
        effects
    }

    fn apply_lsp(&mut self, event: LspEvent) -> Vec<Effect> {
        let mut effects = Vec::new();
        match event {
            LspEvent::Spawned { server, language } => {
                self.lsp_spawned.insert(server);
                self.lsp_errors.remove(&server);
                self.notify(ToastLevel::Info, format!("{language} LSPを起動しました"));
            }
            LspEvent::Initialized {
                server,
                incremental_sync,
                hover_provider,
            } => {
                self.lsp_spawned.insert(server);
                self.finish_progress(&format!("lsp:{server}"));
                self.lsp_ready.insert(server);
                self.lsp_errors.remove(&server);
                if hover_provider {
                    self.lsp_hover_capable.insert(server);
                } else {
                    self.lsp_errors
                        .insert(server, "hover is not supported".to_owned());
                }
                if incremental_sync {
                    self.lsp_incremental_sync.insert(server);
                }
                self.lsp_restart_counts.remove(&server);
                effects.push(Effect::LspSend {
                    server,
                    message: serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "initialized",
                        "params": {}
                    })
                    .to_string(),
                });
                let language = self
                    .lsp_servers
                    .iter()
                    .find_map(|(language, id)| (*id == server).then(|| language.clone()));
                if let Some(language) = language {
                    let documents: Vec<_> = self
                        .documents
                        .iter()
                        .filter_map(|(id, document)| {
                            (document.language.as_deref() == Some(&language)).then_some(*id)
                        })
                        .collect();
                    for doc in documents {
                        effects.extend(self.open_lsp_document(doc, server));
                    }
                }
            }
            LspEvent::Diagnostics { uri, diagnostics } => {
                for document in self.documents.values_mut() {
                    let Some(path) = &document.path else { continue };
                    if uri == format!("file://{}", path.display())
                        && let Some(editable) = match &mut document.kind {
                            crate::document::DocumentKind::Editable(editable) => Some(editable),
                            crate::document::DocumentKind::Large(_) => None,
                        }
                    {
                        editable.diagnostics = diagnostics;
                        break;
                    }
                }
            }
            LspEvent::Progress {
                server,
                token,
                message,
            } => {
                let key = format!("lsp:{server}:{token}");
                if let Some(message) = message {
                    self.set_progress(key, message);
                } else {
                    self.finish_progress(&key);
                }
            }
            LspEvent::Response { id, result } => match self.pending_lsp.remove(&id) {
                Some(PendingLsp::Completion {
                    doc,
                    version,
                    prefix,
                    side,
                    anchor,
                }) if self.document_versions.get(&doc) == Some(&version)
                    && self
                        .layout
                        .active_editor(self.focus)
                        .is_some_and(|pane| pane.view.doc == doc)
                    && matches!(self.focus, Focus::Editor(_)) =>
                {
                    match result.and_then(|value| {
                        serde_json::from_value::<lsp_types::CompletionResponse>(value)
                            .map_err(|error| error.to_string())
                    }) {
                        Ok(response) => {
                            let matcher = SkimMatcherV2::default();
                            let mut items: Vec<_> = match response {
                                lsp_types::CompletionResponse::Array(items) => items,
                                lsp_types::CompletionResponse::List(list) => list.items,
                            }
                            .into_iter()
                            .filter_map(|item| {
                                let filter = item
                                    .filter_text
                                    .clone()
                                    .unwrap_or_else(|| item.label.clone());
                                if !prefix.is_empty()
                                    && (filter.eq_ignore_ascii_case(&prefix)
                                        || item.label.eq_ignore_ascii_case(&prefix))
                                {
                                    return None;
                                }
                                let score = if prefix.is_empty() {
                                    0
                                } else {
                                    matcher.fuzzy_match(&filter, &prefix)?
                                };
                                Some((score, {
                                    let mut insert = item
                                        .insert_text
                                        .clone()
                                        .unwrap_or_else(|| item.label.clone());
                                    let callable = matches!(
                                        item.kind,
                                        Some(
                                            lsp_types::CompletionItemKind::FUNCTION
                                                | lsp_types::CompletionItemKind::METHOD
                                        )
                                    );
                                    if callable && !insert.contains('(') {
                                        insert.push_str("()");
                                    }
                                    let cursor_back =
                                        usize::from(callable && insert.trim_end().ends_with("()"));
                                    CompletionCandidate {
                                        insert,
                                        cursor_back,
                                        label: item.label,
                                        prefix_len: prefix.chars().count(),
                                    }
                                }))
                            })
                            .collect();
                            items.sort_by(|left, right| right.0.cmp(&left.0));
                            let items = items.into_iter().map(|(_, item)| item).collect::<Vec<_>>();
                            if !items.is_empty() {
                                self.completion = Some(CompletionState {
                                    items,
                                    selected: 0,
                                    return_side: side,
                                    anchor,
                                });
                                self.focus = Focus::Completion(side);
                            }
                        }
                        Err(error) => self.status = Some(format!("補完に失敗: {error}")),
                    }
                }
                Some(PendingLsp::Completion { .. }) => {}
                Some(PendingLsp::Definition) => match result.and_then(|value| {
                    serde_json::from_value::<lsp_types::GotoDefinitionResponse>(value)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(response) => {
                        let location = match response {
                            lsp_types::GotoDefinitionResponse::Scalar(location) => Some(location),
                            lsp_types::GotoDefinitionResponse::Array(locations) => {
                                locations.into_iter().next()
                            }
                            lsp_types::GotoDefinitionResponse::Link(links) => {
                                links.into_iter().next().map(|link| lsp_types::Location {
                                    uri: link.target_uri,
                                    range: link.target_selection_range,
                                })
                            }
                        };
                        if let Some(location) = location {
                            let path =
                                PathBuf::from(location.uri.as_str().trim_start_matches("file://"));
                            if let Some((doc, document)) = self
                                .documents
                                .iter()
                                .find(|(_, document)| document.path.as_ref() == Some(&path))
                            {
                                let mut view = View::new(*doc);
                                if let Some(editable) = document.editable_opt() {
                                    let index = crate::position::line_col_to_char_idx(
                                        editable.text(),
                                        location.range.start.line as usize,
                                        location.range.start.character as usize,
                                    );
                                    view.selections.set_single(Selection::caret(index));
                                }
                                self.layout = Layout::EditorFull(EditorPane { view });
                            } else {
                                effects.extend(self.open_paths([path]));
                            }
                        }
                    }
                    Err(error) => self.status = Some(format!("定義ジャンプに失敗: {error}")),
                },
                Some(PendingLsp::Rename { doc }) => match result.and_then(|value| {
                    serde_json::from_value::<lsp_types::WorkspaceEdit>(value)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(edit) => {
                        effects.extend(self.apply_workspace_edit(doc, edit));
                        self.status = Some("リネームを適用しました".to_owned());
                    }
                    Err(error) => self.status = Some(format!("リネームに失敗: {error}")),
                },
                Some(PendingLsp::Formatting { doc }) => match result.and_then(|value| {
                    serde_json::from_value::<Option<Vec<lsp_types::TextEdit>>>(value)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(Some(edits)) => {
                        self.apply_text_edits(doc, edits);
                        self.status = Some("整形を適用しました".to_owned());
                    }
                    Ok(None) => self.status = Some("整形による変更はありません".to_owned()),
                    Err(error) => self.status = Some(format!("整形に失敗: {error}")),
                },
                Some(PendingLsp::Hover { doc, line }) => match result.and_then(|value| {
                    serde_json::from_value::<Option<lsp_types::Hover>>(value)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(hover) => {
                        if hover.is_some() {
                            self.hover_ready_documents.insert(doc);
                        }
                        let mut parts = hover
                            .map(|hover| hover_text(hover.contents))
                            .into_iter()
                            .collect::<Vec<_>>();
                        if let Some(message) = self
                            .documents
                            .get(&doc)
                            .and_then(Document::editable_opt)
                            .and_then(|editable| {
                                editable
                                    .diagnostics
                                    .iter()
                                    .find(|diagnostic| diagnostic.line as usize == line)
                            })
                            .map(|diagnostic| diagnostic.message.clone())
                        {
                            parts.push(format!("診断: {message}"));
                        }
                        self.hover = (!parts.is_empty()).then(|| parts.join("\n\n"));
                    }
                    Err(_) => self.hover = None,
                },
                Some(PendingLsp::HoverProbe { doc }) => {
                    match result.and_then(|value| {
                        serde_json::from_value::<Option<lsp_types::Hover>>(value)
                            .map_err(|error| error.to_string())
                    }) {
                        Ok(Some(_)) => {
                            let successes = self.hover_probe_successes.entry(doc).or_default();
                            *successes += 1;
                            let attempts = self.hover_probe_attempts.entry(doc).or_default();
                            *attempts += 1;
                            effects.push(Effect::ScheduleHoverProbe { doc, delay_ms: 50 });
                        }
                        Ok(None) => {
                            let attempts = self.hover_probe_attempts.entry(doc).or_default();
                            *attempts += 1;
                            effects.push(Effect::ScheduleHoverProbe { doc, delay_ms: 50 });
                        }
                        Err(_) => effects.push(Effect::ScheduleHoverProbe { doc, delay_ms: 500 }),
                    }
                }
                Some(PendingLsp::SemanticTokens { doc, version }) => {
                    if self.document_versions.get(&doc) == Some(&version)
                        && let Ok(value) = result
                        && let Ok(Some(tokens)) =
                            serde_json::from_value::<Option<lsp_types::SemanticTokensResult>>(value)
                    {
                        self.apply_semantic_tokens(doc, version, tokens);
                    }
                }
                None => {}
            },
            LspEvent::Exited { server, error }
            | LspEvent::InitializationFailed { server, error } => {
                let progress_prefix = format!("lsp:{server}:");
                self.progress
                    .retain(|key, _| !key.starts_with(&progress_prefix));
                let message = error.unwrap_or_else(|| "LSPが終了しました".to_owned());
                self.lsp_errors.insert(server, message.clone());
                self.notify(ToastLevel::Error, message.clone());
                self.lsp_ready.remove(&server);
                self.lsp_spawned.remove(&server);
                self.lsp_incremental_sync.remove(&server);
                self.lsp_hover_capable.remove(&server);
                if let Some(language) = self
                    .lsp_servers
                    .iter()
                    .find_map(|(language, id)| (*id == server).then(|| language.clone()))
                {
                    self.lsp_opened_documents.retain(|doc| {
                        self.documents
                            .get(doc)
                            .is_none_or(|document| document.language.as_deref() != Some(&language))
                    });
                    self.semantic_ready_versions.retain(|doc, _| {
                        self.documents
                            .get(doc)
                            .is_none_or(|document| document.language.as_deref() != Some(&language))
                    });
                    self.hover_ready_documents.retain(|doc| {
                        self.documents
                            .get(doc)
                            .is_none_or(|document| document.language.as_deref() != Some(&language))
                    });
                    self.hover_probe_successes.retain(|doc, _| {
                        self.documents
                            .get(doc)
                            .is_none_or(|document| document.language.as_deref() != Some(&language))
                    });
                    self.hover_probe_attempts.retain(|doc, _| {
                        self.documents
                            .get(doc)
                            .is_none_or(|document| document.language.as_deref() != Some(&language))
                    });
                }
                let attempts = self.lsp_restart_counts.entry(server).or_insert(0);
                if *attempts < 3 {
                    let delay_ms = 500u64 * (1u64 << *attempts);
                    *attempts += 1;
                    effects.push(Effect::ScheduleLspRestart { server, delay_ms });
                }
            }
            LspEvent::RestartDue { server } => {
                self.lsp_spawned.remove(&server);
                self.lsp_errors.remove(&server);
                if let Some(language) = self
                    .lsp_servers
                    .iter()
                    .find_map(|(language, id)| (*id == server).then(|| language.clone()))
                    && let Some(command) = self
                        .config
                        .language
                        .iter()
                        .find(|config| config.name == language)
                        .and_then(|config| config.lsp.clone())
                {
                    effects.push(Effect::SpawnLsp {
                        server,
                        language,
                        command,
                        root: self.workspace_root.clone(),
                    });
                }
            }
            LspEvent::SemanticRefreshDue { doc, version } => {
                if self.document_versions.get(&doc) == Some(&version) {
                    effects.extend(self.request_semantic_tokens(doc, version));
                }
            }
            LspEvent::CompletionRefreshDue { doc, version } => {
                let active_doc = self
                    .layout
                    .active_editor(self.focus)
                    .map(|pane| pane.view.doc);
                if self.document_versions.get(&doc) == Some(&version)
                    && active_doc == Some(doc)
                    && self.completion_suppressed != Some((doc, version))
                    && matches!(self.focus, Focus::Editor(_))
                {
                    effects.extend(self.request_completion(false));
                }
            }
            LspEvent::HoverProbeDue { doc } => {
                if !self.hover_ready_documents.contains(&doc) {
                    effects.extend(self.request_hover_probe(doc));
                }
            }
        }
        self.dirty = true;
        effects
    }

    fn toggle_completion(&mut self) -> Vec<Effect> {
        if let Some(completion) = self.completion.take() {
            if let Some(doc) = self
                .layout
                .active_editor(self.focus)
                .map(|pane| pane.view.doc)
                && let Some(version) = self.document_versions.get(&doc)
            {
                self.completion_suppressed = Some((doc, *version));
            }
            self.focus = Focus::Editor(completion.return_side);
            self.dirty = true;
            return Vec::new();
        }
        self.request_completion(true)
    }

    fn request_completion(&mut self, manual: bool) -> Vec<Effect> {
        let side = match self.focus {
            Focus::Editor(side) | Focus::Completion(side) => side,
            Focus::Shell | Focus::Overlay => Side::Left,
        };
        let Some((server, path, line, character)) = self.active_lsp_context() else {
            if manual {
                self.status = Some("このバッファではLSP補完を利用できません".to_owned());
                self.dirty = true;
            }
            return Vec::new();
        };
        let Some((doc, version, prefix, anchor)) = self.completion_context() else {
            return Vec::new();
        };
        if !manual && prefix.is_empty() {
            return Vec::new();
        }
        let id = self.next_lsp_request;
        self.next_lsp_request += 1;
        self.pending_lsp
            .retain(|_, pending| !matches!(pending, PendingLsp::Completion { .. }));
        self.pending_lsp.insert(
            id,
            PendingLsp::Completion {
                doc,
                version,
                prefix,
                side,
                anchor,
            },
        );
        vec![Effect::LspRequest {
            server,
            id,
            method: "textDocument/completion".to_owned(),
            params: serde_json::json!({
                "textDocument": {"uri": format!("file://{}", path.display())},
                "position": {"line": line, "character": character}
            })
            .to_string(),
        }]
    }

    fn completion_context(&self) -> Option<(DocumentId, i32, String, CharIdx)> {
        let pane = self.layout.active_editor(self.focus)?;
        let document = self.documents.get(&pane.view.doc)?;
        let editable = document.editable_opt()?;
        let head = pane
            .view
            .selections
            .primary()
            .head
            .0
            .min(editable.text().len_chars());
        let mut start = head;
        while start > 0 && is_word(editable.text().char(start - 1)) {
            start -= 1;
        }
        Some((
            pane.view.doc,
            *self.document_versions.get(&pane.view.doc).unwrap_or(&1),
            editable.text().slice(start..head).to_string(),
            CharIdx(start),
        ))
    }

    fn request_definition(&mut self) -> Vec<Effect> {
        let Some((server, path, line, character)) = self.active_lsp_context() else {
            return Vec::new();
        };
        let id = self.next_lsp_request;
        self.next_lsp_request += 1;
        self.pending_lsp.insert(id, PendingLsp::Definition);
        vec![Effect::LspRequest {
            server,
            id,
            method: "textDocument/definition".to_owned(),
            params: serde_json::json!({
                "textDocument": {"uri": format!("file://{}", path.display())},
                "position": {"line": line, "character": character}
            })
            .to_string(),
        }]
    }

    fn request_hover_at(&mut self, index: CharIdx) -> Vec<Effect> {
        self.pending_lsp
            .retain(|_, pending| !matches!(pending, PendingLsp::Hover { .. }));
        self.hover = None;
        let Some(doc) = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
        else {
            return Vec::new();
        };
        let Some((server, path, line, character)) = self.active_lsp_context_at(index) else {
            let has_lsp = self
                .documents
                .get(&doc)
                .and_then(|document| document.language.as_deref())
                .and_then(|language| {
                    self.config
                        .language
                        .iter()
                        .find(|config| config.name == language)
                })
                .is_some_and(|config| config.lsp.is_some());
            if has_lsp {
                self.deferred_hover = Some((doc, index));
            }
            return Vec::new();
        };
        self.deferred_hover = None;
        let id = self.next_lsp_request;
        self.next_lsp_request += 1;
        self.pending_lsp.insert(id, PendingLsp::Hover { doc, line });
        vec![Effect::LspRequest {
            server,
            id,
            method: "textDocument/hover".to_owned(),
            params: serde_json::json!({
                "textDocument": {"uri": format!("file://{}", path.display())},
                "position": {"line": line, "character": character}
            })
            .to_string(),
        }]
    }

    fn retry_deferred_hover(&mut self) -> Vec<Effect> {
        let Some((doc, index)) = self.deferred_hover else {
            return Vec::new();
        };
        let active = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc);
        if active != Some(doc) {
            return Vec::new();
        }
        let ready = self
            .documents
            .get(&doc)
            .and_then(|document| document.language.as_ref())
            .and_then(|language| self.lsp_servers.get(language))
            .is_some_and(|server| {
                self.lsp_ready.contains(server) && self.lsp_opened_documents.contains(&doc)
            });
        if !ready {
            return Vec::new();
        }
        self.deferred_hover = None;
        self.request_hover_at(index)
    }

    fn request_formatting(&mut self) -> Vec<Effect> {
        let Some((server, path, _, _)) = self.active_lsp_context() else {
            self.status = Some("このバッファでは整形を利用できません".to_owned());
            return Vec::new();
        };
        let Some(doc) = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
        else {
            return Vec::new();
        };
        let id = self.next_lsp_request;
        self.next_lsp_request += 1;
        self.pending_lsp.insert(id, PendingLsp::Formatting { doc });
        vec![Effect::LspRequest {
            server,
            id,
            method: "textDocument/formatting".to_owned(),
            params: serde_json::json!({
                "textDocument": {"uri": format!("file://{}", path.display())},
                "options": {"tabSize": 4, "insertSpaces": true}
            })
            .to_string(),
        }]
    }

    fn active_lsp_context(&self) -> Option<(u64, PathBuf, usize, usize)> {
        let pane = self.layout.active_editor(self.focus)?;
        self.active_lsp_context_at(pane.view.selections.primary().head)
    }

    fn active_lsp_context_at(&self, index: CharIdx) -> Option<(u64, PathBuf, usize, usize)> {
        let pane = self.layout.active_editor(self.focus)?;
        let document = self.documents.get(&pane.view.doc)?;
        let editable = document.editable_opt()?;
        let language = document.language.as_ref()?;
        let server = *self.lsp_servers.get(language)?;
        if !self.lsp_ready.contains(&server) || !self.lsp_opened_documents.contains(&pane.view.doc)
        {
            return None;
        }
        let path = document.path.clone()?;
        let (line, char_col) = crate::position::char_idx_to_line_col(editable.text(), index);
        let utf16 = editable
            .text()
            .line(line)
            .chars()
            .take(char_col)
            .map(char::len_utf16)
            .sum();
        Some((server, path, line, utf16))
    }

    fn start_or_open_lsp(&mut self, doc: DocumentId) -> Vec<Effect> {
        let Some(language) = self
            .documents
            .get(&doc)
            .and_then(|document| document.language.clone())
        else {
            return Vec::new();
        };
        if let Some(server) = self.lsp_servers.get(&language).copied() {
            return self.open_lsp_document(doc, server);
        }
        let Some(command) = self
            .config
            .language
            .iter()
            .find(|config| config.name == language)
            .and_then(|config| config.lsp.clone())
        else {
            return Vec::new();
        };
        let server = self.next_server_id;
        self.next_server_id += 1;
        self.lsp_servers.insert(language.clone(), server);
        vec![Effect::SpawnLsp {
            server,
            language,
            command,
            root: self.workspace_root.clone(),
        }]
    }

    fn start_workspace_lsps(&mut self) -> Vec<Effect> {
        let servers: Vec<_> = self
            .config
            .language
            .iter()
            .filter_map(|language| {
                language
                    .lsp
                    .clone()
                    .map(|command| (language.name.clone(), command))
            })
            .collect();
        let mut effects = Vec::new();
        for (language, command) in servers {
            if self.lsp_servers.contains_key(&language) {
                continue;
            }
            let server = self.next_server_id;
            self.next_server_id += 1;
            self.lsp_servers.insert(language.clone(), server);
            effects.push(Effect::SpawnLsp {
                server,
                language,
                command,
                root: self.workspace_root.clone(),
            });
        }
        effects
    }

    fn open_lsp_document(&mut self, doc: DocumentId, server: u64) -> Vec<Effect> {
        if !self.lsp_ready.contains(&server) || self.lsp_opened_documents.contains(&doc) {
            return Vec::new();
        }
        let Some((language, path, text)) = self.documents.get(&doc).and_then(|document| {
            Some((
                document.language.clone()?,
                document.path.clone()?,
                document.editable_opt()?.text().to_string(),
            ))
        }) else {
            return Vec::new();
        };
        if self.lsp_servers.get(&language) != Some(&server) {
            return Vec::new();
        }
        self.lsp_opened_documents.insert(doc);
        self.document_versions.insert(doc, 1);
        let request = self.next_lsp_request;
        self.next_lsp_request += 1;
        self.pending_lsp
            .insert(request, PendingLsp::SemanticTokens { doc, version: 1 });
        let mut effects = vec![
            Effect::LspSend {
                server,
                message: serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didOpen",
                    "params": {
                        "textDocument": {
                            "uri": format!("file://{}", path.display()),
                            "languageId": language,
                            "version": 1,
                            "text": text
                        }
                    }
                })
                .to_string(),
            },
            Effect::LspRequest {
                server,
                id: request,
                method: "textDocument/semanticTokens/full".to_owned(),
                params: serde_json::json!({
                    "textDocument": {"uri": format!("file://{}", path.display())}
                })
                .to_string(),
            },
        ];
        effects.extend(self.request_hover_probe(doc));
        effects
    }

    fn request_hover_probe(&mut self, doc: DocumentId) -> Vec<Effect> {
        if self.hover_ready_documents.contains(&doc)
            || self.pending_lsp.values().any(
                |pending| matches!(pending, PendingLsp::HoverProbe { doc: pending } if *pending == doc),
            )
        {
            return Vec::new();
        }
        let Some((server, path, candidate)) = self.documents.get(&doc).and_then(|document| {
            let language = document.language.as_ref()?;
            let editable = document.editable_opt()?;
            let attempt = self.hover_probe_attempts.get(&doc).copied().unwrap_or(0);
            let candidate = sampled_hover_probe_indices(editable.text(), 12)
                .get(attempt)
                .copied()
                .map(|index| {
                    let (line, char_col) =
                        crate::position::char_idx_to_line_col(editable.text(), index);
                    let character = editable
                        .text()
                        .line(line)
                        .chars()
                        .take(char_col)
                        .map(char::len_utf16)
                        .sum::<usize>();
                    (line, character)
                });
            Some((
                *self.lsp_servers.get(language)?,
                document.path.clone()?,
                candidate,
            ))
        }) else {
            return Vec::new();
        };
        if !self.lsp_ready.contains(&server)
            || !self.lsp_hover_capable.contains(&server)
            || !self.lsp_opened_documents.contains(&doc)
        {
            return Vec::new();
        }
        let Some((line, character)) = candidate else {
            if self.hover_probe_successes.get(&doc).copied().unwrap_or(0) > 0 {
                self.hover_ready_documents.insert(doc);
                self.dirty = true;
            }
            return Vec::new();
        };
        let request = self.next_lsp_request;
        self.next_lsp_request += 1;
        self.pending_lsp
            .insert(request, PendingLsp::HoverProbe { doc });
        vec![Effect::LspRequest {
            server,
            id: request,
            method: "textDocument/hover".to_owned(),
            params: serde_json::json!({
                "textDocument": {"uri": format!("file://{}", path.display())},
                "position": {"line": line, "character": character}
            })
            .to_string(),
        }]
    }

    fn request_semantic_tokens(&mut self, doc: DocumentId, version: i32) -> Vec<Effect> {
        if self.pending_lsp.values().any(|pending| {
            matches!(
                pending,
                PendingLsp::SemanticTokens {
                    doc: pending_doc,
                    version: pending_version,
                } if *pending_doc == doc && *pending_version == version
            )
        }) {
            return Vec::new();
        }
        let Some((server, path)) = self.documents.get(&doc).and_then(|document| {
            let language = document.language.as_ref()?;
            Some((*self.lsp_servers.get(language)?, document.path.clone()?))
        }) else {
            return Vec::new();
        };
        if !self.lsp_ready.contains(&server) || !self.lsp_opened_documents.contains(&doc) {
            return Vec::new();
        }
        let request = self.next_lsp_request;
        self.next_lsp_request += 1;
        self.pending_lsp
            .insert(request, PendingLsp::SemanticTokens { doc, version });
        vec![Effect::LspRequest {
            server,
            id: request,
            method: "textDocument/semanticTokens/full".to_owned(),
            params: serde_json::json!({
                "textDocument": {"uri": format!("file://{}", path.display())}
            })
            .to_string(),
        }]
    }

    fn apply_workspace_edit(
        &mut self,
        preferred_doc: DocumentId,
        edit: lsp_types::WorkspaceEdit,
    ) -> Vec<Effect> {
        let mut edits_by_doc: HashMap<DocumentId, Vec<lsp_types::TextEdit>> = HashMap::new();
        let mut external_edits: Vec<(PathBuf, Vec<lsp_types::TextEdit>)> = Vec::new();
        if let Some(changes) = edit.changes {
            for (uri, edits) in changes {
                if let Some(doc) = self.document_for_uri(uri.as_str()) {
                    edits_by_doc.entry(doc).or_default().extend(edits);
                } else if let Some(path) = file_uri_path(uri.as_str()) {
                    external_edits.push((path, edits));
                }
            }
        }
        if let Some(lsp_types::DocumentChanges::Edits(changes)) = edit.document_changes {
            for change in changes {
                let uri = change.text_document.uri.as_str();
                let edits: Vec<_> = change
                    .edits
                    .into_iter()
                    .map(|edit| match edit {
                        lsp_types::OneOf::Left(edit) => edit,
                        lsp_types::OneOf::Right(edit) => edit.text_edit,
                    })
                    .collect();
                if let Some(doc) = self.document_for_uri(uri) {
                    edits_by_doc.entry(doc).or_default().extend(edits);
                } else if let Some(path) = file_uri_path(uri) {
                    external_edits.push((path, edits));
                }
            }
        }
        for (doc, edits) in edits_by_doc {
            self.apply_text_edits(doc, edits);
        }
        if self.documents.contains_key(&preferred_doc) {
            self.layout = Layout::EditorFull(EditorPane {
                view: View::new(preferred_doc),
            });
        }
        if !external_edits.is_empty() {
            self.notify(
                ToastLevel::Warn,
                format!(
                    "未オープンの{}ファイルにも変更を適用します",
                    external_edits.len()
                ),
            );
        }
        external_edits
            .into_iter()
            .filter_map(|(path, edits)| {
                serde_json::to_string(&edits)
                    .ok()
                    .map(|edits_json| Effect::ApplyFileEdits { path, edits_json })
            })
            .collect()
    }

    fn apply_text_edits(&mut self, doc: DocumentId, edits: Vec<lsp_types::TextEdit>) {
        let Some(document) = self.documents.get_mut(&doc) else {
            return;
        };
        let Some(editable) = document.editable_opt() else {
            return;
        };
        let selections: Vec<_> = edits
            .iter()
            .map(|edit| Selection {
                anchor: crate::position::lsp_position_to_char_idx(
                    editable.text(),
                    edit.range.start.line as usize,
                    edit.range.start.character as usize,
                ),
                head: crate::position::lsp_position_to_char_idx(
                    editable.text(),
                    edit.range.end.line as usize,
                    edit.range.end.character as usize,
                ),
            })
            .collect();
        if selections.is_empty() {
            return;
        }
        let replacements: Vec<_> = edits.into_iter().map(|edit| edit.new_text).collect();
        let mut selections = crate::view::Selections::from_vec(selections, 0);
        document
            .editable_mut()
            .insert_fragments(&mut selections, &replacements);
        self.pending_lsp_sync.insert(doc);
    }

    fn document_for_uri(&self, uri: &str) -> Option<DocumentId> {
        let path = file_uri_path(uri)?;
        self.documents
            .iter()
            .find_map(|(id, document)| (document.path.as_ref() == Some(&path)).then_some(*id))
    }

    fn apply_semantic_tokens(
        &mut self,
        doc: DocumentId,
        version: i32,
        result: lsp_types::SemanticTokensResult,
    ) {
        let tokens = match result {
            lsp_types::SemanticTokensResult::Tokens(tokens) => tokens.data,
            lsp_types::SemanticTokensResult::Partial(partial) => partial.data,
        };
        let Some(document) = self.documents.get_mut(&doc) else {
            return;
        };
        let Some(editable) = document.editable_opt() else {
            return;
        };
        let mut line = 0u32;
        let mut start = 0u32;
        let mut spans = Vec::with_capacity(tokens.len());
        for token in tokens {
            line += token.delta_line;
            start = if token.delta_line == 0 {
                start + token.delta_start
            } else {
                token.delta_start
            };
            let begin = crate::position::lsp_position_to_char_idx(
                editable.text(),
                line as usize,
                start as usize,
            );
            let end = crate::position::lsp_position_to_char_idx(
                editable.text(),
                line as usize,
                (start + token.length) as usize,
            );
            spans.push(crate::lsp::SemanticSpan {
                start: begin,
                end,
                token_type: token.token_type,
            });
        }
        document.editable_mut().semantic_spans = spans;
        self.semantic_ready_versions.insert(doc, version);
    }

    fn refresh_languages(&mut self) {
        for document in self.documents.values_mut() {
            document.language = document
                .path
                .as_deref()
                .and_then(|path| self.config.language_for_path(path))
                .map(|language| language.name.clone());
            if let Some(language) = document.language.clone()
                && let crate::document::DocumentKind::Editable(editable) = &mut document.kind
            {
                editable.enable_highlight(&language);
            }
        }
    }

    fn apply_command(&mut self, command: Command) -> Vec<Effect> {
        if let Focus::Completion(side) = self.focus
            && !matches!(
                command,
                Command::PickerUp
                    | Command::PickerDown
                    | Command::PickerConfirm
                    | Command::PickerCancel
                    | Command::ToggleCompletion
            )
        {
            self.completion = None;
            self.focus = Focus::Editor(side);
        }
        match command {
            Command::InsertNewline => {
                let (tab_size, insert_spaces) = self.active_indentation_settings();
                let line_comment = self
                    .layout
                    .active_editor(self.focus)
                    .and_then(|pane| self.documents.get(&pane.view.doc))
                    .and_then(|document| document.language.as_deref())
                    .and_then(|language| {
                        self.config
                            .language
                            .iter()
                            .find(|config| config.name == language)
                    })
                    .and_then(|config| config.line_comment.clone());
                self.edit_active(|document, view| {
                    document.editable_mut().insert_newline(
                        &mut view.selections,
                        line_comment.as_deref(),
                        tab_size,
                        insert_spaces,
                    );
                });
            }
            Command::DeleteBackward => self.edit_active(|document, view| {
                document
                    .editable_mut()
                    .delete_backward(&mut view.selections);
            }),
            Command::DeleteForward => self.edit_active(|document, view| {
                document.editable_mut().delete_forward(&mut view.selections);
            }),
            Command::Move {
                direction,
                unit,
                extend,
            } => self.move_active(direction, unit, extend),
            Command::SelectAll => self.select_all(),
            Command::CollapseSelections => self.collapse_selections(),
            Command::AddCursor { direction } => self.add_cursor(direction),
            Command::SelectNextOccurrence => self.select_next_occurrence(),
            Command::Copy => return self.copy_active(true),
            Command::CopyShellSelection => return self.copy_shell_selection(true),
            Command::Cut => {
                let effects = self.copy_active(true);
                if !effects.is_empty() {
                    let linewise = self.clipboard.is_linewise();
                    self.edit_active(|document, view| {
                        if linewise {
                            document.editable_mut().delete_lines(&mut view.selections);
                        } else {
                            document
                                .editable_mut()
                                .delete_backward(&mut view.selections);
                        }
                    });
                }
                return effects;
            }
            Command::Paste => {
                let fragments = self.clipboard.fragments().to_vec();
                let linewise = self.clipboard.is_linewise();
                if !fragments.is_empty() {
                    self.edit_active(|document, view| {
                        if linewise {
                            document
                                .editable_mut()
                                .insert_linewise_fragments(&mut view.selections, &fragments);
                        } else {
                            document
                                .editable_mut()
                                .insert_fragments(&mut view.selections, &fragments);
                        }
                    });
                }
            }
            Command::Save => return self.save_active(),
            Command::OpenDirectoryPicker => return self.open_directory_picker(),
            Command::OpenBufferPicker => self.open_picker(PickerMode::Buffer),
            Command::OpenDiffPicker => self.open_picker(PickerMode::Diff),
            Command::OpenCommandPalette => self.open_command_palette(),
            Command::OpenSearch => return self.open_search(false, SearchScope::CurrentBuffer),
            Command::OpenReplace => return self.open_search(true, SearchScope::CurrentBuffer),
            Command::OpenSearchInDirectory => {
                return self.open_search(false, SearchScope::Directory);
            }
            Command::CycleSearchScope => return self.cycle_search_scope(),
            Command::PickerUp => self.move_picker(-1),
            Command::PickerDown => self.move_picker(1),
            Command::PickerBackspace => {
                if let Some(rename) = &mut self.rename_input {
                    rename.pop();
                    self.dirty = true;
                } else if let Some(search) = &mut self.search {
                    if let Some(field) = search.editing_filter {
                        match field {
                            SearchFilterField::Include => {
                                search.include_input.pop();
                            }
                            SearchFilterField::Exclude => {
                                search.exclude_input.pop();
                            }
                        }
                    } else if search.editing_replace {
                        if let Some(replacement) = &mut search.replacement {
                            replacement.pop();
                        }
                    } else {
                        search.query.pop();
                    }
                    return self.refresh_search();
                } else if let Some(picker) = &mut self.picker {
                    picker.query.pop();
                    self.refresh_picker();
                }
            }
            Command::PickerConfirm => return self.confirm_picker(),
            Command::PickerCancel => self.close_picker(),
            Command::SearchToggleField => {
                if self.completion.is_some() {
                    return self.confirm_picker();
                }
                if let Some(search) = &mut self.search {
                    match search.editing_filter {
                        Some(SearchFilterField::Include) => {
                            search.editing_filter = Some(SearchFilterField::Exclude);
                        }
                        Some(SearchFilterField::Exclude) => {
                            search.editing_filter = None;
                            search.editing_replace = false;
                        }
                        None if search.editing_replace
                            && search.scope == SearchScope::Directory =>
                        {
                            search.editing_replace = false;
                            search.editing_filter = Some(SearchFilterField::Include);
                        }
                        None if search.editing_replace => search.editing_replace = false,
                        None if search.replacement.is_some() => search.editing_replace = true,
                        None if search.scope == SearchScope::Directory => {
                            search.editing_filter = Some(SearchFilterField::Include);
                        }
                        None => {}
                    }
                    self.dirty = true;
                }
            }
            Command::SearchToggleCase => {
                return self.toggle_search_option(|options| {
                    options.case_sensitive = !options.case_sensitive;
                });
            }
            Command::SearchToggleWholeWord => {
                return self.toggle_search_option(|options| {
                    options.whole_word = !options.whole_word;
                });
            }
            Command::SearchToggleRegex => {
                return self.toggle_search_option(|options| {
                    options.regex = !options.regex;
                });
            }
            Command::SearchToggleIgnore => {
                if let Some(search) = &mut self.search {
                    search.filters.respect_ignore_files = !search.filters.respect_ignore_files;
                    return self.refresh_search();
                }
            }
            Command::SearchToggleHidden => {
                if let Some(search) = &mut self.search {
                    search.filters.include_hidden = !search.filters.include_hidden;
                    return self.refresh_search();
                }
            }
            Command::ToggleCompletion => return self.toggle_completion(),
            Command::Rename => {
                if self.active_lsp_context().is_some() {
                    self.rename_input = Some(String::new());
                    self.focus = Focus::Overlay;
                    self.dirty = true;
                } else {
                    self.status = Some("このバッファではリネームを利用できません".to_owned());
                }
            }
            Command::Format => return self.request_formatting(),
            Command::ToggleShell => return self.toggle_shell(),
            Command::ToggleSplit => self.toggle_split(),
            Command::CloseBuffer => return self.close_active_buffer(),
            Command::Indent => self.indent_selected_lines(false),
            Command::Outdent => self.indent_selected_lines(true),
            Command::ToggleComment => self.toggle_comment(),
            Command::Undo => self.edit_active(|document, view| {
                document.editable_mut().undo(&mut view.selections);
            }),
            Command::Redo => self.edit_active(|document, view| {
                document.editable_mut().redo(&mut view.selections);
            }),
            Command::Quit => {
                if self.documents.values().any(|document| {
                    document
                        .editable_opt()
                        .is_some_and(|editable| editable.modified)
                }) {
                    self.confirm = Some(ConfirmState {
                        message: "未保存の変更を破棄して終了しますか? [Enter / Esc]".to_owned(),
                        action: ConfirmAction::QuitDiscard,
                    });
                    self.focus = Focus::Overlay;
                    self.dirty = true;
                    return Vec::new();
                }
                self.quit = true;
                return vec![Effect::Quit];
            }
        }
        Vec::new()
    }

    fn toggle_shell(&mut self) -> Vec<Effect> {
        match &self.layout {
            Layout::EditorAndShell { editor } => {
                self.layout = Layout::EditorFull(EditorPane {
                    view: editor.view.clone(),
                });
                self.focus = Focus::Editor(Side::Left);
                self.terminal_selection = None;
                self.dirty = true;
                Vec::new()
            }
            _ => {
                let Some(view) = self
                    .layout
                    .active_editor(self.focus)
                    .map(|pane| pane.view.clone())
                else {
                    return Vec::new();
                };
                self.layout = Layout::EditorAndShell {
                    editor: EditorPane { view },
                };
                self.focus = Focus::Shell;
                self.hover = None;
                self.deferred_hover = None;
                self.pending_lsp
                    .retain(|_, pending| !matches!(pending, PendingLsp::Hover { .. }));
                self.dirty = true;
                if self.terminal.is_some() {
                    return Vec::new();
                }
                self.terminal = Some(vt100::Parser::new(
                    self.terminal_size.1.saturating_sub(1),
                    split_right_width(self.terminal_size.0),
                    0,
                ));
                vec![Effect::SpawnShell {
                    cols: split_right_width(self.terminal_size.0).max(1),
                    rows: self.terminal_size.1.saturating_sub(1).max(1),
                    shell: self.config.editor.shell.clone(),
                }]
            }
        }
    }

    fn toggle_split(&mut self) {
        match &self.layout {
            Layout::EditorAndEditor { left, .. } => {
                self.layout = Layout::EditorFull(EditorPane {
                    view: left.view.clone(),
                });
                self.focus = Focus::Editor(Side::Left);
            }
            _ => {
                let Some(view) = self
                    .layout
                    .active_editor(self.focus)
                    .map(|pane| pane.view.clone())
                else {
                    return;
                };
                self.layout = Layout::EditorAndEditor {
                    left: EditorPane { view: view.clone() },
                    right: EditorPane { view },
                    diff: false,
                };
                self.focus = Focus::Editor(Side::Right);
            }
        }
        self.dirty = true;
    }

    fn close_active_buffer(&mut self) -> Vec<Effect> {
        let Some(pane) = self.layout.active_editor(self.focus) else {
            return Vec::new();
        };
        let id = pane.view.doc;
        if self
            .documents
            .get(&id)
            .and_then(Document::editable_opt)
            .is_some_and(|editable| editable.modified)
        {
            self.confirm = Some(ConfirmState {
                message: "未保存の変更を破棄してバッファを閉じますか? [Enter / Esc]".to_owned(),
                action: ConfirmAction::CloseDiscard(id),
            });
            self.focus = Focus::Overlay;
            self.dirty = true;
            return Vec::new();
        }
        self.close_document(id)
    }

    fn close_document(&mut self, id: DocumentId) -> Vec<Effect> {
        let did_close = self.documents.get(&id).and_then(|document| {
            let path = document.path.as_ref()?;
            let server = *self.lsp_servers.get(document.language.as_ref()?)?;
            Some(Effect::LspSend {
                server,
                message: serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didClose",
                    "params": {"textDocument": {
                        "uri": format!("file://{}", path.display())
                    }}
                })
                .to_string(),
            })
        });
        self.documents.remove(&id);
        self.lsp_opened_documents.remove(&id);
        self.semantic_ready_versions.remove(&id);
        self.hover_ready_documents.remove(&id);
        self.hover_probe_successes.remove(&id);
        self.hover_probe_attempts.remove(&id);
        if self.deferred_hover.is_some_and(|(doc, _)| doc == id) {
            self.deferred_hover = None;
        }
        self.document_versions.remove(&id);
        self.pending_self_disk_updates.remove(&id);
        if let Some(next) = self.documents.keys().next().copied() {
            self.layout = Layout::EditorFull(EditorPane {
                view: View::new(next),
            });
        } else {
            let id = DocumentId(self.next_doc_id);
            self.next_doc_id += 1;
            self.documents.insert(id, Document::scratch());
            self.layout = Layout::EditorFull(EditorPane {
                view: View::new(id),
            });
        }
        self.focus = Focus::Editor(Side::Left);
        self.dirty = true;
        did_close.into_iter().collect()
    }

    fn toggle_comment(&mut self) {
        let language = self
            .layout
            .active_editor(self.focus)
            .and_then(|pane| self.documents.get(&pane.view.doc))
            .and_then(|document| document.language.as_deref())
            .and_then(|language| {
                self.config
                    .language
                    .iter()
                    .find(|config| config.name == language)
            })
            .and_then(|config| config.line_comment.clone());
        let Some(comment) = language else {
            self.status = Some("この言語のコメント記号は未設定です".to_owned());
            self.dirty = true;
            return;
        };
        self.edit_active(|document, view| {
            let editable = document.editable_opt().expect("editable document");
            let lines = selected_lines(editable.text(), &view.selections);
            let uncomment = lines.iter().all(|line| {
                editable
                    .text()
                    .line(*line)
                    .to_string()
                    .trim_start()
                    .starts_with(&comment)
            });
            let mut edits = Vec::new();
            let mut fragments = Vec::new();
            for line in lines {
                let text = editable.text().line(line).to_string();
                let indent = text
                    .chars()
                    .take_while(|character| character.is_whitespace() && *character != '\n')
                    .count();
                let start = editable.text().line_to_char(line) + indent;
                if uncomment {
                    let suffix_space = text[indent..]
                        .strip_prefix(&comment)
                        .is_some_and(|rest| rest.starts_with(' '));
                    let length = comment.chars().count() + usize::from(suffix_space);
                    edits.push(Selection {
                        anchor: CharIdx(start),
                        head: CharIdx(start + length),
                    });
                    fragments.push(String::new());
                } else {
                    edits.push(Selection::caret(CharIdx(start)));
                    fragments.push(format!("{comment} "));
                }
            }
            if !edits.is_empty() {
                let mut edits = crate::view::Selections::from_vec(edits, 0);
                document
                    .editable_mut()
                    .insert_fragments(&mut edits, &fragments);
            }
        });
    }

    fn indent_selected_lines(&mut self, outdent: bool) {
        let (tab_size, insert_spaces) = self.active_indentation_settings();
        let indentation = if insert_spaces {
            " ".repeat(tab_size)
        } else {
            "\t".to_owned()
        };
        self.edit_active(move |document, view| {
            let has_range = view
                .selections
                .iter()
                .any(|selection| !selection.is_caret());
            if !outdent && !has_range {
                let fragments = vec![indentation.clone(); view.selections.len()];
                document
                    .editable_mut()
                    .insert_fragments(&mut view.selections, &fragments);
            } else {
                document.editable_mut().indent_lines(
                    &mut view.selections,
                    &indentation,
                    tab_size,
                    outdent,
                );
            }
        });
    }

    fn active_indentation_settings(&self) -> (usize, bool) {
        let language = self
            .layout
            .active_editor(self.focus)
            .and_then(|pane| self.documents.get(&pane.view.doc))
            .and_then(|document| document.language.as_deref());
        self.config.indentation_for_language(language)
    }

    fn apply_terminal(&mut self, event: TerminalEvent) {
        let mut dirty = true;
        match event {
            TerminalEvent::Output(bytes) => {
                if let Some(parser) = &mut self.terminal {
                    parser.process(&bytes);
                }
                dirty = self.terminal_selection.is_none();
            }
            TerminalEvent::Exited(error) => {
                if let Layout::EditorAndShell { editor } = &self.layout {
                    self.layout = Layout::EditorFull(EditorPane {
                        view: editor.view.clone(),
                    });
                }
                self.focus = Focus::Editor(Side::Left);
                self.terminal = None;
                self.terminal_selection = None;
                self.status = error;
            }
        }
        self.dirty |= dirty;
    }

    fn save_active(&mut self) -> Vec<Effect> {
        let Some(pane) = self.layout.active_editor(self.focus) else {
            return Vec::new();
        };
        let Some(document) = self.documents.get(&pane.view.doc) else {
            return Vec::new();
        };
        let Some(path) = document.path.clone() else {
            self.status = Some("無名バッファは保存できません".to_owned());
            self.dirty = true;
            return Vec::new();
        };
        let Some(editable) = document.editable_opt() else {
            self.status = Some("大容量ファイルは読み取り専用です".to_owned());
            self.dirty = true;
            return Vec::new();
        };
        vec![Effect::WriteFile {
            doc: pane.view.doc,
            path,
            contents: editable.contents_for_save(),
            expected: document.disk_state,
        }]
    }

    fn select_all(&mut self) {
        let focus = self.focus;
        let (documents, layout) = (&mut self.documents, &mut self.layout);
        let Some(pane) = layout.active_editor_mut(focus) else {
            return;
        };
        let Some(document) = documents.get_mut(&pane.view.doc) else {
            return;
        };
        if let Some(editable) = match &mut document.kind {
            crate::document::DocumentKind::Editable(editable) => Some(editable),
            crate::document::DocumentKind::Large(_) => None,
        } {
            editable.break_history_group();
        }
        let Some(editable) = document.editable_opt() else {
            return;
        };
        pane.view.selections.set_single(Selection {
            anchor: crate::position::CharIdx(0),
            head: crate::position::CharIdx(editable.text().len_chars()),
        });
        self.dirty = true;
    }

    fn collapse_selections(&mut self) {
        let Some(pane) = self.layout.active_editor_mut(self.focus) else {
            return;
        };
        let head = pane.view.selections.primary().head;
        pane.view.selections.set_single(Selection::caret(head));
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn add_cursor(&mut self, direction: VerticalDirection) {
        let focus = self.focus;
        let (documents, layout) = (&self.documents, &mut self.layout);
        let Some(pane) = layout.active_editor_mut(focus) else {
            return;
        };
        let Some(document) = documents.get(&pane.view.doc) else {
            return;
        };
        let Some(editable) = document.editable_opt() else {
            return;
        };
        let existing: Vec<_> = pane.view.selections.iter().copied().collect();
        for selection in existing {
            let moved = move_head(
                editable.text(),
                Selection::caret(selection.head),
                match direction {
                    VerticalDirection::Up => Direction::Up,
                    VerticalDirection::Down => Direction::Down,
                },
                Unit::Character,
                false,
            );
            pane.view.selections.add(moved, true);
        }
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn select_next_occurrence(&mut self) {
        let focus = self.focus;
        let (documents, layout) = (&self.documents, &mut self.layout);
        let Some(pane) = layout.active_editor_mut(focus) else {
            return;
        };
        let Some(document) = documents.get(&pane.view.doc) else {
            return;
        };
        let Some(editable) = document.editable_opt() else {
            return;
        };
        let text = editable.text();
        let primary = pane.view.selections.primary();
        if primary.is_caret() {
            let mut start = primary.head.0;
            let mut end = primary.head.0;
            while start > 0 && is_word(text.char(start - 1)) {
                start -= 1;
            }
            while end < text.len_chars() && is_word(text.char(end)) {
                end += 1;
            }
            if start != end {
                pane.view.selections.set_single(Selection {
                    anchor: crate::position::CharIdx(start),
                    head: crate::position::CharIdx(end),
                });
                self.dirty = true;
            }
            return;
        }

        let needle: Vec<_> = text.slice(primary.range()).chars().collect();
        let haystack: Vec<_> = text.chars().collect();
        let start = pane
            .view
            .selections
            .iter()
            .map(|selection| selection.range().end)
            .max()
            .unwrap_or(0);
        let existing: Vec<_> = pane
            .view
            .selections
            .iter()
            .map(|selection| selection.range())
            .collect();
        let found = find_occurrence(&haystack, &needle, start)
            .or_else(|| find_occurrence(&haystack, &needle, 0))
            .filter(|candidate| {
                let candidate = *candidate..*candidate + needle.len();
                existing
                    .iter()
                    .all(|range| candidate.end <= range.start || candidate.start >= range.end)
            });
        if let Some(found) = found {
            pane.view.selections.add(
                Selection {
                    anchor: crate::position::CharIdx(found),
                    head: crate::position::CharIdx(found + needle.len()),
                },
                true,
            );
            self.ensure_cursor_visible();
            self.dirty = true;
        }
    }

    fn copy_active(&mut self, copy_caret_lines: bool) -> Vec<Effect> {
        let focus = self.focus;
        let Some(pane) = self.layout.active_editor(focus) else {
            return Vec::new();
        };
        let Some(document) = self.documents.get(&pane.view.doc) else {
            return Vec::new();
        };
        if let Some(large) = document.large() {
            let selection = pane.view.selections.primary();
            let start = selection.anchor.0.min(selection.head.0);
            let end = selection.anchor.0.max(selection.head.0);
            let fragments: Vec<_> = (start..=end).filter_map(|line| large.line(line)).collect();
            if fragments.is_empty() {
                return Vec::new();
            }
            self.clipboard.store(vec![fragments.join("\n")]);
            return vec![Effect::ClipboardOsc52(self.clipboard.osc52_text())];
        }
        let Some(editable) = document.editable_opt() else {
            return Vec::new();
        };
        let linewise = copy_caret_lines
            && pane
                .view
                .selections
                .iter()
                .all(|selection| selection.is_caret());
        let fragments = if linewise {
            pane.view
                .selections
                .iter()
                .map(|selection| {
                    let line = editable
                        .text()
                        .char_to_line(selection.head.0.min(editable.text().len_chars()));
                    editable
                        .text()
                        .line(line)
                        .chars()
                        .take_while(|character| !matches!(character, '\r' | '\n'))
                        .collect()
                })
                .collect()
        } else {
            editable.selected_texts(&pane.view.selections)
        };
        if fragments.iter().all(String::is_empty) && !linewise {
            return Vec::new();
        }
        if linewise {
            self.clipboard.store_linewise(fragments);
        } else {
            self.clipboard.store(fragments);
        }
        vec![Effect::ClipboardOsc52(self.clipboard.osc52_text())]
    }

    fn copy_shell_selection(&self, send_interrupt_if_empty: bool) -> Vec<Effect> {
        let Some(selection) = &self.terminal_selection else {
            return if self.focus == Focus::Shell && send_interrupt_if_empty {
                vec![Effect::TerminalInput(vec![3])]
            } else {
                Vec::new()
            };
        };
        if selection.anchor == selection.head {
            return Vec::new();
        }
        let (start, end) = ordered_terminal_points(selection.anchor, selection.head);
        let (_, cols) = selection.snapshot.size();
        let text = selection.snapshot.contents_between(
            start.0,
            start.1,
            end.0,
            end.1.saturating_add(1).min(cols),
        );
        (!text.is_empty())
            .then_some(Effect::ClipboardOsc52(text))
            .into_iter()
            .collect()
    }

    fn terminal_mouse_position(&self, column: u16, row: u16) -> Option<(u16, u16)> {
        if !matches!(self.layout, Layout::EditorAndShell { .. }) {
            return None;
        }
        let start = split_left_width(self.terminal_size.0).saturating_add(1);
        let screen = self.terminal.as_ref()?.screen();
        let (rows, cols) = screen.size();
        if column < start || row >= rows || cols == 0 {
            return None;
        }
        Some((row, column.saturating_sub(start).min(cols - 1)))
    }

    fn apply_mouse(&mut self, input: MouseInput) {
        let mouse = input.event;
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_active(-3),
            MouseEventKind::ScrollDown => self.scroll_active(3),
            MouseEventKind::Down(MouseButton::Back) => {
                self.edit_active(|document, view| {
                    document.editable_mut().undo(&mut view.selections);
                });
            }
            MouseEventKind::Down(MouseButton::Forward) => {
                self.edit_active(|document, view| {
                    document.editable_mut().redo(&mut view.selections);
                });
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let divider = split_left_width(self.terminal_size.0);
                if matches!(
                    self.layout,
                    Layout::EditorAndEditor { .. } | Layout::EditorAndShell { .. }
                ) && mouse.column == divider
                {
                    return;
                }
                let right_half = mouse.column > divider;
                match &self.layout {
                    Layout::EditorAndEditor { .. } => {
                        self.focus =
                            Focus::Editor(if right_half { Side::Right } else { Side::Left });
                    }
                    Layout::EditorAndShell { .. } if right_half => {
                        self.focus = Focus::Shell;
                        self.hover = None;
                        self.deferred_hover = None;
                        self.pending_lsp
                            .retain(|_, pending| !matches!(pending, PendingLsp::Hover { .. }));
                        if let Some(point) = self.terminal_mouse_position(mouse.column, mouse.row)
                            && let Some(parser) = &self.terminal
                        {
                            self.terminal_selection = Some(TerminalSelection {
                                anchor: point,
                                head: point,
                                snapshot: parser.screen().clone(),
                            });
                        }
                        self.dirty = true;
                        return;
                    }
                    Layout::EditorAndShell { .. } => {
                        self.focus = Focus::Editor(Side::Left);
                        self.terminal_selection = None;
                    }
                    Layout::EditorFull(_) => {}
                }
                let Some(index) = self.mouse_position(mouse.column, mouse.row) else {
                    return;
                };
                let Some(pane) = self.layout.active_editor_mut(self.focus) else {
                    return;
                };
                if mouse.modifiers.contains(KeyModifiers::ALT) {
                    pane.view.selections.add(Selection::caret(index), true);
                } else if input.clicks >= 3 {
                    let document = self.documents.get(&pane.view.doc).expect("active document");
                    let text = document.editable().text();
                    let line = text.char_to_line(index.0.min(text.len_chars()));
                    let start = text.line_to_char(line);
                    let end = if line + 1 < text.len_lines() {
                        text.line_to_char(line + 1)
                    } else {
                        text.len_chars()
                    };
                    pane.view.selections.set_single(Selection {
                        anchor: CharIdx(start),
                        head: CharIdx(end),
                    });
                } else if input.clicks == 2 {
                    let document = self.documents.get(&pane.view.doc).expect("active document");
                    let text = document.editable().text();
                    let mut start = index.0;
                    let mut end = index.0;
                    if end < text.len_chars() && is_word(text.char(end)) {
                        while start > 0 && is_word(text.char(start - 1)) {
                            start -= 1;
                        }
                        while end < text.len_chars() && is_word(text.char(end)) {
                            end += 1;
                        }
                    }
                    pane.view.selections.set_single(Selection {
                        anchor: CharIdx(start),
                        head: CharIdx(end),
                    });
                } else {
                    pane.view.selections.set_single(Selection::caret(index));
                }
                self.drag_anchor = Some(index);
                self.ensure_cursor_visible();
                self.dirty = true;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.focus == Focus::Shell {
                    if let Some(point) = self.terminal_mouse_position(mouse.column, mouse.row)
                        && let Some(selection) = &mut self.terminal_selection
                    {
                        selection.head = point;
                        self.dirty = true;
                    }
                    return;
                }
                let Some(anchor) = self.drag_anchor else {
                    return;
                };
                let Some(head) = self.mouse_position(mouse.column, mouse.row) else {
                    return;
                };
                let Some(pane) = self.layout.active_editor_mut(self.focus) else {
                    return;
                };
                pane.view.selections.set_single(Selection { anchor, head });
                self.ensure_cursor_visible();
                self.dirty = true;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag_anchor = None;
                if self.focus == Focus::Shell
                    && self
                        .terminal_selection
                        .as_ref()
                        .is_some_and(|selection| selection.anchor == selection.head)
                {
                    self.terminal_selection = None;
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn open_picker(&mut self, mode: PickerMode) {
        self.hover = None;
        let Some(base) = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
        else {
            return;
        };
        let base_path = self
            .documents
            .get(&base)
            .and_then(|document| document.path.as_ref());
        let candidates: Vec<_> = self
            .documents
            .iter()
            .filter(|(id, document)| {
                **id != base
                    && (mode != PickerMode::Diff
                        || base_path.is_none()
                        || document.path.as_ref() != base_path)
            })
            .map(|(id, _)| *id)
            .map(PickerCandidate::Document)
            .collect();
        if candidates.is_empty() {
            self.status = Some(match mode {
                PickerMode::Buffer => "他に開いているバッファがありません".to_owned(),
                PickerMode::Diff => "比較できる別のバッファがありません".to_owned(),
                PickerMode::Directory | PickerMode::Command => unreachable!(),
            });
            self.dirty = true;
            return;
        }
        self.picker = Some(PickerState {
            mode,
            base,
            return_side: match self.focus {
                Focus::Editor(side) | Focus::Completion(side) => side,
                Focus::Shell | Focus::Overlay => Side::Left,
            },
            filtered: (0..candidates.len()).collect(),
            candidates,
            query: String::new(),
            selected: 0,
            ranking_cache: Vec::new(),
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
    }

    fn open_command_palette(&mut self) {
        self.hover = None;
        let base = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
            .unwrap_or(DocumentId(0));
        let candidates: Vec<_> = (0..COMMAND_PALETTE.len())
            .map(PickerCandidate::Command)
            .collect();
        self.picker = Some(PickerState {
            mode: PickerMode::Command,
            base,
            return_side: match self.focus {
                Focus::Editor(side) | Focus::Completion(side) => side,
                Focus::Shell | Focus::Overlay => Side::Left,
            },
            filtered: (0..candidates.len()).collect(),
            candidates,
            query: String::new(),
            selected: 0,
            ranking_cache: Vec::new(),
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
    }

    fn open_directory_picker(&mut self) -> Vec<Effect> {
        self.hover = None;
        let base = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
            .unwrap_or(DocumentId(0));
        let token = self.next_scan_token;
        self.next_scan_token += 1;
        self.picker = Some(PickerState {
            mode: PickerMode::Directory,
            base,
            return_side: match self.focus {
                Focus::Editor(side) | Focus::Completion(side) => side,
                Focus::Shell | Focus::Overlay => Side::Left,
            },
            query: String::new(),
            candidates: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            ranking_cache: Vec::new(),
        });
        self.focus = Focus::Overlay;
        self.status = Some("ファイルを走査中…".to_owned());
        self.set_progress("file-scan", "ファイルを走査中…");
        self.dirty = true;
        vec![Effect::StartFileScan {
            root: self.workspace_root.clone(),
            token,
        }]
    }

    fn apply_file_scan(&mut self, event: FileScanEvent) {
        match event {
            FileScanEvent::Batch { paths, .. } => {
                if let Some(picker) = &mut self.picker
                    && picker.mode == PickerMode::Directory
                {
                    let start = picker.candidates.len();
                    picker
                        .candidates
                        .extend(paths.into_iter().map(PickerCandidate::Path));
                    picker.ranking_cache.clear();
                    if picker.query.is_empty() {
                        picker.filtered.extend(start..picker.candidates.len());
                    } else {
                        self.refresh_picker();
                    }
                }
            }
            FileScanEvent::Done { .. } => {
                self.status = None;
                self.finish_progress("file-scan");
            }
            FileScanEvent::Failed { error, .. } => {
                self.status = Some(error);
                self.finish_progress("file-scan");
            }
        }
        self.dirty = true;
    }

    fn open_search(&mut self, replace: bool, scope: SearchScope) -> Vec<Effect> {
        if self.search.is_some() {
            return self.cycle_search_scope();
        }
        self.picker = None;
        self.search = Some(SearchState {
            query: String::new(),
            replacement: replace.then(String::new),
            editing_replace: false,
            editing_filter: None,
            scope,
            options: SearchOptions::default(),
            include_input: String::new(),
            exclude_input: self.config.search.exclude.join(","),
            filters: SearchFilters {
                include: Vec::new(),
                exclude: self.config.search.exclude.clone(),
                respect_ignore_files: self.config.search.respect_ignore_files,
                include_hidden: self.config.search.include_hidden,
            },
            hits: Vec::new(),
            current: 0,
            grep_token: None,
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
        Vec::new()
    }

    fn cycle_search_scope(&mut self) -> Vec<Effect> {
        if let Some(search) = &mut self.search {
            search.scope = match search.scope {
                SearchScope::CurrentBuffer => SearchScope::AllBuffers,
                SearchScope::AllBuffers => SearchScope::Directory,
                SearchScope::Directory => SearchScope::CurrentBuffer,
            };
        }
        self.refresh_search()
    }

    fn refresh_search(&mut self) -> Vec<Effect> {
        let Some(search) = &self.search else {
            return Vec::new();
        };
        let query = search.query.clone();
        let scope = search.scope;
        let options = search.options;
        let mut filters = search.filters.clone();
        filters.include = split_globs(&search.include_input);
        filters.exclude = split_globs(&search.exclude_input);
        if query.is_empty() {
            if let Some(search) = &mut self.search {
                search.hits.clear();
                search.current = 0;
            }
            self.dirty = true;
            return Vec::new();
        }
        let large_path = (scope == SearchScope::CurrentBuffer)
            .then(|| {
                self.layout
                    .active_editor(Focus::Overlay)
                    .and_then(|pane| self.documents.get(&pane.view.doc))
                    .filter(|document| document.large().is_some())
                    .and_then(|document| document.path.clone())
            })
            .flatten();
        if scope == SearchScope::Directory || large_path.is_some() {
            let token = self.next_grep_token;
            self.next_grep_token += 1;
            if let Some(search) = &mut self.search {
                search.hits.clear();
                search.grep_token = Some(token);
            }
            self.dirty = true;
            self.set_progress("grep", "検索中…");
            let Ok(pattern) = search_pattern(&query, options) else {
                self.status = Some("検索式が不正です".to_owned());
                return Vec::new();
            };
            return vec![Effect::StartGrep {
                pattern: pattern.to_string(),
                filters: if large_path.is_some() {
                    SearchFilters {
                        respect_ignore_files: true,
                        ..SearchFilters::default()
                    }
                } else {
                    filters
                },
                root: large_path.unwrap_or_else(|| self.workspace_root.clone()),
                token,
            }];
        }
        let pattern = search_pattern(&query, options);
        let Ok(pattern) = pattern else {
            self.status = Some("検索式が不正です".to_owned());
            return Vec::new();
        };
        let active = self
            .layout
            .active_editor(Focus::Overlay)
            .map(|pane| pane.view.doc);
        let mut hits = Vec::new();
        for (id, document) in &self.documents {
            if scope == SearchScope::CurrentBuffer && Some(*id) != active {
                continue;
            }
            let Some(editable) = document.editable_opt() else {
                continue;
            };
            let contents = editable.text().to_string();
            for matched in pattern.find_iter(&contents) {
                let start = contents[..matched.start()].chars().count();
                let end = start + matched.as_str().chars().count();
                hits.push(SearchHit::Buffer {
                    doc: *id,
                    range: start..end,
                });
            }
        }
        if let Some(search) = &mut self.search {
            search.hits = hits;
            search.current = search.current.min(search.hits.len().saturating_sub(1));
        }
        self.dirty = true;
        Vec::new()
    }

    fn toggle_search_option(&mut self, toggle: impl FnOnce(&mut SearchOptions)) -> Vec<Effect> {
        if let Some(search) = &mut self.search {
            toggle(&mut search.options);
            return self.refresh_search();
        }
        Vec::new()
    }

    fn apply_grep(&mut self, event: GrepEvent) {
        let Some(search) = &mut self.search else {
            return;
        };
        let finished = match event {
            GrepEvent::Hits { token, hits } if search.grep_token == Some(token) => {
                search.hits.extend(hits.into_iter().map(SearchHit::Disk));
                false
            }
            GrepEvent::Done { token } if search.grep_token == Some(token) => {
                self.status = Some(format!("検索完了: {}件", search.hits.len()));
                true
            }
            GrepEvent::Failed { token, error } if search.grep_token == Some(token) => {
                self.status = Some(error);
                true
            }
            _ => false,
        };
        if finished {
            self.finish_progress("grep");
        }
        self.dirty = true;
    }

    fn confirm_search(&mut self) -> Vec<Effect> {
        let Some(search) = self.search.take() else {
            return Vec::new();
        };
        if let Some(replacement) = search.replacement {
            let Ok(pattern) = search_pattern(&search.query, search.options) else {
                self.status = Some("検索式が不正です".to_owned());
                return Vec::new();
            };
            if search.scope == SearchScope::Directory {
                let paths: HashSet<_> = search
                    .hits
                    .into_iter()
                    .filter_map(|hit| match hit {
                        SearchHit::Disk(hit) => Some(hit.path),
                        SearchHit::Buffer { .. } => None,
                    })
                    .collect();
                self.confirm = Some(ConfirmState {
                    message: format!(
                        "{}ファイルをディスク上で置換します。続行しますか? [Enter / Esc]",
                        paths.len()
                    ),
                    action: ConfirmAction::DirectoryReplace {
                        paths: paths.into_iter().collect(),
                        pattern: pattern.as_str().to_owned(),
                        replacement,
                    },
                });
                self.focus = Focus::Overlay;
                self.dirty = true;
                return Vec::new();
            }
            let mut by_document: HashMap<DocumentId, Vec<Selection>> = HashMap::new();
            for hit in search.hits {
                if let SearchHit::Buffer { doc, range } = hit {
                    by_document.entry(doc).or_default().push(Selection {
                        anchor: CharIdx(range.start),
                        head: CharIdx(range.end),
                    });
                }
            }
            for (id, selections) in by_document {
                let Some(document) = self.documents.get_mut(&id) else {
                    continue;
                };
                let Some(editable) = document.editable_opt() else {
                    continue;
                };
                let fragments: Vec<_> = selections
                    .iter()
                    .map(|selection| {
                        let range = selection.range();
                        let matched = editable.text().slice(range).to_string();
                        pattern.replace(&matched, replacement.as_str()).into_owned()
                    })
                    .collect();
                let mut selections = crate::view::Selections::from_vec(selections, 0);
                document
                    .editable_mut()
                    .insert_fragments(&mut selections, &fragments);
            }
            self.status = Some("置換を適用しました".to_owned());
        } else if let Some(hit) = search.hits.get(search.current) {
            match hit {
                SearchHit::Buffer { doc, range } => {
                    let mut view = View::new(*doc);
                    view.selections.set_single(Selection {
                        anchor: CharIdx(range.start),
                        head: CharIdx(range.end),
                    });
                    self.layout = Layout::EditorFull(EditorPane { view });
                }
                SearchHit::Disk(hit) => {
                    let path = hit.path.clone();
                    if let Some((doc, _)) = self.documents.iter().find(|(_, document)| {
                        document.path.as_ref() == Some(&path) && document.large().is_some()
                    }) {
                        let mut view = View::new(*doc);
                        view.scroll.top_line = hit.line;
                        self.layout = Layout::EditorFull(EditorPane { view });
                        self.focus = Focus::Editor(Side::Left);
                        self.dirty = true;
                        return Vec::new();
                    }
                    self.focus = Focus::Editor(Side::Left);
                    return self.open_paths([path]);
                }
            }
        }
        self.focus = Focus::Editor(Side::Left);
        self.dirty = true;
        Vec::new()
    }

    fn overlay_input(&mut self, character: char) -> Vec<Effect> {
        if let Some(rename) = &mut self.rename_input {
            rename.push(character);
            self.dirty = true;
            return Vec::new();
        }
        if let Some(search) = &mut self.search {
            if let Some(field) = search.editing_filter {
                match field {
                    SearchFilterField::Include => search.include_input.push(character),
                    SearchFilterField::Exclude => search.exclude_input.push(character),
                }
            } else if search.editing_replace {
                if let Some(replacement) = &mut search.replacement {
                    replacement.push(character);
                }
            } else {
                search.query.push(character);
            }
            return self.refresh_search();
        }
        if let Some(picker) = &mut self.picker {
            picker.query.push(character);
            self.refresh_picker();
            self.dirty = true;
        }
        Vec::new()
    }

    fn refresh_picker(&mut self) {
        let Some(picker) = &self.picker else {
            return;
        };
        let query = picker.query.clone();
        if let Some(cached) = picker
            .ranking_cache
            .iter()
            .find_map(|(cached_query, ranking)| (cached_query == &query).then(|| ranking.clone()))
        {
            let picker = self.picker.as_mut().expect("picker exists");
            picker.filtered = cached;
            picker.selected = picker.selected.min(picker.filtered.len().saturating_sub(1));
            self.dirty = true;
            return;
        }
        if query.is_empty() {
            let ranking: Vec<_> = (0..picker.candidates.len()).collect();
            let picker = self.picker.as_mut().expect("picker exists");
            picker.filtered = ranking;
            picker.selected = picker.selected.min(picker.filtered.len().saturating_sub(1));
            self.dirty = true;
            return;
        }
        let matcher = SkimMatcherV2::default();
        let mut scored: Vec<_> = picker
            .candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let label = self.candidate_label(candidate);
                matcher
                    .fuzzy_match(&label, &query)
                    .map(|score| (index, score))
            })
            .collect();
        scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        let picker = self.picker.as_mut().expect("picker exists");
        picker.filtered = scored.into_iter().map(|(index, _)| index).collect();
        picker
            .ranking_cache
            .retain(|(cached_query, _)| cached_query != &query);
        picker.ranking_cache.push((query, picker.filtered.clone()));
        if picker.ranking_cache.len() > 12 {
            picker.ranking_cache.remove(0);
        }
        picker.selected = picker.selected.min(picker.filtered.len().saturating_sub(1));
    }

    fn move_picker(&mut self, amount: isize) {
        if let Some(completion) = &mut self.completion {
            let max = completion.items.len().saturating_sub(1);
            completion.selected = if amount < 0 {
                completion.selected.saturating_sub(amount.unsigned_abs())
            } else {
                (completion.selected + amount as usize).min(max)
            };
            self.dirty = true;
            return;
        }
        if let Some(search) = &mut self.search {
            let max = search.hits.len().saturating_sub(1);
            search.current = if amount < 0 {
                search.current.saturating_sub(amount.unsigned_abs())
            } else {
                (search.current + amount as usize).min(max)
            };
            self.dirty = true;
            return;
        }
        let Some(picker) = &mut self.picker else {
            return;
        };
        let max = picker.filtered.len().saturating_sub(1);
        picker.selected = if amount < 0 {
            picker.selected.saturating_sub(amount.unsigned_abs())
        } else {
            (picker.selected + amount as usize).min(max)
        };
        self.dirty = true;
    }

    fn confirm_picker(&mut self) -> Vec<Effect> {
        if let Some(confirm) = self.confirm.take() {
            self.focus = Focus::Editor(Side::Left);
            match confirm.action {
                ConfirmAction::Overwrite(doc) => {
                    let Some(document) = self.documents.get(&doc) else {
                        return Vec::new();
                    };
                    let (Some(path), Some(editable)) =
                        (document.path.clone(), document.editable_opt())
                    else {
                        return Vec::new();
                    };
                    return vec![Effect::WriteFile {
                        doc,
                        path,
                        contents: editable.contents_for_save(),
                        expected: None,
                    }];
                }
                ConfirmAction::DirectoryReplace {
                    paths,
                    pattern,
                    replacement,
                } => {
                    return vec![Effect::ReplaceFiles {
                        paths,
                        pattern,
                        replacement,
                    }];
                }
                ConfirmAction::CloseDiscard(doc) => return self.close_document(doc),
                ConfirmAction::QuitDiscard => {
                    self.quit = true;
                    return vec![Effect::Quit];
                }
            }
        }
        if let Some(new_name) = self.rename_input.take() {
            let Some((server, path, line, character)) = self.active_lsp_context() else {
                self.close_picker();
                return Vec::new();
            };
            let Some(doc) = self
                .layout
                .active_editor(self.focus)
                .map(|pane| pane.view.doc)
            else {
                return Vec::new();
            };
            let id = self.next_lsp_request;
            self.next_lsp_request += 1;
            self.pending_lsp.insert(id, PendingLsp::Rename { doc });
            self.focus = Focus::Editor(Side::Left);
            return vec![Effect::LspRequest {
                server,
                id,
                method: "textDocument/rename".to_owned(),
                params: serde_json::json!({
                    "textDocument": {"uri": format!("file://{}", path.display())},
                    "position": {"line": line, "character": character},
                    "newName": new_name
                })
                .to_string(),
            }];
        }
        if let Some(completion) = self.completion.take() {
            if let Some(candidate) = completion.items.get(completion.selected) {
                let insert = candidate.insert.clone();
                let prefix_len = candidate.prefix_len;
                let cursor_back = candidate.cursor_back;
                self.focus = Focus::Editor(completion.return_side);
                self.edit_active(|document, view| {
                    let head = view.selections.primary().head.0;
                    view.selections.set_single(Selection {
                        anchor: CharIdx(head.saturating_sub(prefix_len)),
                        head: CharIdx(head),
                    });
                    document
                        .editable_mut()
                        .insert(&mut view.selections, &insert);
                    if cursor_back > 0 {
                        let head = view.selections.primary().head.0;
                        view.selections
                            .set_single(Selection::caret(CharIdx(head - cursor_back)));
                    }
                });
            }
            return Vec::new();
        }
        if self.search.is_some() {
            return self.confirm_search();
        }
        let Some(picker) = self.picker.take() else {
            return Vec::new();
        };
        let Some(candidate_index) = picker.filtered.get(picker.selected) else {
            if picker.mode == PickerMode::Directory && !picker.query.is_empty() {
                self.focus = Focus::Editor(picker.return_side);
                return vec![Effect::ResolveDirectPath {
                    input: picker.query,
                    root: self.workspace_root.clone(),
                }];
            }
            self.close_picker();
            return Vec::new();
        };
        let candidate = picker.candidates[*candidate_index].clone();
        let mut effects = Vec::new();
        let mut final_side = picker.return_side;
        match (picker.mode, candidate) {
            (PickerMode::Directory, PickerCandidate::Path(path)) => {
                self.focus = Focus::Editor(picker.return_side);
                effects.extend(self.open_paths([path]));
            }
            (PickerMode::Buffer, PickerCandidate::Document(target)) => {
                self.focus = Focus::Editor(picker.return_side);
                if let Some(pane) = self.layout.active_editor_mut(self.focus) {
                    pane.view = View::new(target);
                } else {
                    self.layout = Layout::EditorFull(EditorPane {
                        view: View::new(target),
                    });
                    final_side = Side::Left;
                }
            }
            (PickerMode::Diff, PickerCandidate::Document(target)) => {
                self.layout = Layout::EditorAndEditor {
                    left: EditorPane {
                        view: View::new(picker.base),
                    },
                    right: EditorPane {
                        view: View::new(target),
                    },
                    diff: true,
                };
                final_side = Side::Left;
            }
            (PickerMode::Command, PickerCandidate::Command(index)) => {
                self.focus = Focus::Editor(picker.return_side);
                self.dirty = true;
                return self.apply_command(COMMAND_PALETTE[index].command);
            }
            _ => {}
        }
        self.focus = Focus::Editor(final_side);
        self.dirty = true;
        effects
    }

    fn close_picker(&mut self) {
        let return_side = self
            .completion
            .as_ref()
            .map(|completion| completion.return_side)
            .or_else(|| self.picker.as_ref().map(|picker| picker.return_side))
            .unwrap_or(Side::Left);
        self.picker = None;
        self.search = None;
        self.completion = None;
        self.rename_input = None;
        self.confirm = None;
        self.focus = Focus::Editor(return_side);
        self.dirty = true;
    }

    fn document_label(&self, id: DocumentId) -> String {
        self.documents
            .get(&id)
            .and_then(|document| document.path.as_ref())
            .map_or_else(
                || format!("Untitled {}", id.0),
                |path| path.display().to_string(),
            )
    }

    fn display_path(&self, path: &std::path::Path) -> String {
        path.strip_prefix(&self.workspace_root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    fn document_language_status(&self, doc: DocumentId, document: &Document) -> String {
        let Some(language) = document.language.as_deref() else {
            return "<syntax> text: plain".to_owned();
        };
        let has_lsp = self
            .config
            .language
            .iter()
            .find(|config| config.name == language)
            .is_some_and(|config| config.lsp.is_some());
        if !has_lsp {
            return format!("<syntax> {language}");
        }
        let Some(server) = self.lsp_servers.get(language) else {
            return format!("<lsp> {language}: starting");
        };
        if let Some(error) = self.lsp_errors.get(server) {
            let state = if error.to_ascii_lowercase().contains("not found") {
                "not found"
            } else {
                "error"
            };
            return format!("<lsp> {language}: {state}");
        }
        if !self.lsp_spawned.contains(server) {
            return format!("<lsp> {language}: starting");
        }
        let progress_prefix = format!("lsp:{server}:");
        let progress = self
            .progress
            .iter()
            .find_map(|(key, message)| key.starts_with(&progress_prefix).then_some(message));
        if !self.lsp_ready.contains(server) {
            return progress.map_or_else(
                || format!("<lsp> {language}: initializing"),
                |message| format!("<lsp> {language}: initializing ({message})"),
            );
        }
        if let Some(message) = progress {
            return format!("<lsp> {language}: updating ({message})");
        }
        if !self.lsp_opened_documents.contains(&doc) {
            return format!("<lsp> {language}: opening");
        }
        let current_version = self.document_versions.get(&doc).copied().unwrap_or(1);
        match self.semantic_ready_versions.get(&doc).copied() {
            None => return format!("<lsp> {language}: coloring"),
            Some(version) if version < current_version => {
                return format!("<lsp> {language}: updating");
            }
            Some(_) => {}
        }
        if !self.hover_ready_documents.contains(&doc) {
            return format!("<lsp> {language}: checking hover");
        }
        format!("<lsp> {language}: ready")
    }

    fn candidate_label(&self, candidate: &PickerCandidate) -> String {
        match candidate {
            PickerCandidate::Document(id) => self.document_label(*id),
            PickerCandidate::Path(path) => path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(path)
                .display()
                .to_string(),
            PickerCandidate::Command(index) => COMMAND_PALETTE[*index].label(),
        }
    }

    fn mouse_position(&self, column: u16, row: u16) -> Option<CharIdx> {
        if row >= self.terminal_size.1.saturating_sub(1) {
            return None;
        }
        let pane = self.layout.active_editor(self.focus)?;
        let document = self.documents.get(&pane.view.doc)?;
        let text = document.editable_opt()?.text();
        let tab_size = self
            .config
            .indentation_for_language(document.language.as_deref())
            .0;
        let gutter_width = text.len_lines().max(1).to_string().len().max(2) + 3;
        let pane_width = match self.layout {
            Layout::EditorFull(_) => self.terminal_size.0,
            Layout::EditorAndShell { .. } | Layout::EditorAndEditor { .. }
                if matches!(self.focus, Focus::Editor(Side::Right)) =>
            {
                split_right_width(self.terminal_size.0)
            }
            Layout::EditorAndShell { .. } | Layout::EditorAndEditor { .. } => {
                split_left_width(self.terminal_size.0)
            }
        }
        .max(1);
        let text_width = pane_width.saturating_sub(gutter_width as u16).max(1);
        let local_column = if matches!(self.focus, Focus::Editor(Side::Right)) {
            column.saturating_sub(split_left_width(self.terminal_size.0).saturating_add(1))
        } else {
            column
        };
        if matches!(self.layout, Layout::EditorAndEditor { diff: true, .. }) {
            let line = (pane.view.scroll.top_line + usize::from(row))
                .min(text.len_lines().saturating_sub(1));
            let display_col = usize::from(local_column).saturating_sub(6);
            return Some(display_col_to_char_idx(text, line, display_col, tab_size));
        }
        let mut visual_row = usize::from(row) + pane.view.scroll.wrapped_row_offset;
        let mut line = pane.view.scroll.top_line;
        while line < text.len_lines() {
            let line_rows = editor_wrapped_line_rows(text, line, usize::from(text_width), tab_size);
            if visual_row < line_rows {
                let text_column = local_column.saturating_sub(gutter_width as u16);
                let display_col = visual_row * usize::from(text_width) + usize::from(text_column);
                return Some(display_col_to_char_idx(text, line, display_col, tab_size));
            }
            visual_row -= line_rows;
            line += 1;
        }
        Some(CharIdx(text.len_chars()))
    }

    fn scroll_active(&mut self, amount: isize) {
        let focus = self.focus;
        let (documents, layout) = (&self.documents, &mut self.layout);
        let Some(pane) = layout.active_editor_mut(focus) else {
            return;
        };
        let Some(document) = documents.get(&pane.view.doc) else {
            return;
        };
        let max_top = document.editable_opt().map_or(usize::MAX, |editable| {
            editable.text().len_lines().saturating_sub(1)
        });
        pane.view.scroll.top_line = if amount < 0 {
            pane.view
                .scroll
                .top_line
                .saturating_sub(amount.unsigned_abs())
        } else {
            (pane.view.scroll.top_line + amount as usize).min(max_top)
        };
        pane.view.scroll.wrapped_row_offset = 0;
        self.dirty = true;
    }

    fn edit_active(&mut self, edit: impl FnOnce(&mut Document, &mut View)) {
        if let Focus::Completion(side) = self.focus {
            self.completion = None;
            self.focus = Focus::Editor(side);
        }
        let focus = self.focus;
        let (documents, layout) = (&mut self.documents, &mut self.layout);
        let Some(pane) = layout.active_editor_mut(focus) else {
            return;
        };
        let Some(document) = documents.get_mut(&pane.view.doc) else {
            return;
        };
        if document.large().is_some() {
            self.status = Some("大容量ファイルは読み取り専用です".to_owned());
            self.dirty = true;
            return;
        }
        edit(document, &mut pane.view);
        self.completion_suppressed = None;
        self.pending_lsp_sync.insert(pane.view.doc);
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn take_lsp_sync_effects(&mut self) -> Vec<Effect> {
        let pending: Vec<_> = self.pending_lsp_sync.drain().collect();
        let mut effects = Vec::new();
        for id in pending {
            let Some(document) = self.documents.get_mut(&id) else {
                continue;
            };
            let language = document.language.clone();
            let path = document.path.clone();
            let (changes, text) = match &mut document.kind {
                crate::document::DocumentKind::Editable(editable) => {
                    (editable.take_lsp_changes(), editable.text().to_string())
                }
                crate::document::DocumentKind::Large(_) => continue,
            };
            let (Some(language), Some(path)) = (language, path) else {
                continue;
            };
            if changes.is_empty() {
                continue;
            }
            let Some(server) = self.lsp_servers.get(&language).copied() else {
                continue;
            };
            if !self.lsp_ready.contains(&server) || !self.lsp_opened_documents.contains(&id) {
                continue;
            }
            let content_changes = if self.lsp_incremental_sync.contains(&server) {
                serde_json::to_value(changes).unwrap_or_else(|_| serde_json::json!([]))
            } else {
                serde_json::json!([{"text": text}])
            };
            let version = self.document_versions.entry(id).or_insert(1);
            *version += 1;
            let version = *version;
            effects.push(Effect::LspSend {
                server,
                message: serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {
                            "uri": format!("file://{}", path.display()),
                            "version": version
                        },
                        "contentChanges": content_changes
                    }
                })
                .to_string(),
            });
            effects.push(Effect::ScheduleSemanticRefresh {
                doc: id,
                version,
                delay_ms: 150,
            });
            effects.push(Effect::ScheduleCompletionRefresh {
                doc: id,
                version,
                delay_ms: 100,
            });
        }
        effects
    }

    fn move_active(&mut self, direction: Direction, unit: Unit, extend: bool) {
        let focus = self.focus;
        let (documents, layout) = (&mut self.documents, &mut self.layout);
        let Some(pane) = layout.active_editor_mut(focus) else {
            return;
        };
        let Some(document) = documents.get_mut(&pane.view.doc) else {
            return;
        };
        if let crate::document::DocumentKind::Editable(editable) = &mut document.kind {
            editable.break_history_group();
        }
        if let Some(large) = document.large() {
            let current = pane.view.selections.primary().head.0;
            let target = match (direction, unit) {
                (Direction::Left | Direction::Up, Unit::Document) => 0,
                (Direction::Left | Direction::Up, _) => current.saturating_sub(1),
                (Direction::Right | Direction::Down, _) => current.saturating_add(1),
            };
            let target = if large.line(target).is_some() {
                target
            } else {
                current
            };
            let anchor = if extend {
                pane.view.selections.primary().anchor
            } else {
                CharIdx(target)
            };
            pane.view.selections.set_single(Selection {
                anchor,
                head: CharIdx(target),
            });
            let rows = usize::from(self.terminal_size.1.saturating_sub(1)).max(1);
            if target < pane.view.scroll.top_line {
                pane.view.scroll.top_line = target;
            } else if target >= pane.view.scroll.top_line + rows {
                pane.view.scroll.top_line = target + 1 - rows;
            }
            self.dirty = true;
            return;
        }
        let Some(editable) = document.editable_opt() else {
            return;
        };
        let moved = pane
            .view
            .selections
            .iter()
            .map(|selection| move_head(editable.text(), *selection, direction, unit, extend))
            .collect();
        pane.view.selections.replace_all(moved);
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn ensure_cursor_visible(&mut self) {
        if self.terminal_size.0 == 0 || self.terminal_size.1 == 0 {
            return;
        }
        let rows = usize::from(self.terminal_size.1.saturating_sub(1)).max(1);
        let pane_cols = match self.layout {
            Layout::EditorFull(_) => usize::from(self.terminal_size.0),
            Layout::EditorAndShell { .. } | Layout::EditorAndEditor { .. }
                if matches!(self.focus, Focus::Editor(Side::Right)) =>
            {
                usize::from(split_right_width(self.terminal_size.0))
            }
            Layout::EditorAndShell { .. } | Layout::EditorAndEditor { .. } => {
                usize::from(split_left_width(self.terminal_size.0))
            }
        }
        .max(1);
        let focus = self.focus;
        let tab_size = self
            .layout
            .active_editor(focus)
            .and_then(|pane| self.documents.get(&pane.view.doc))
            .map(|document| {
                self.config
                    .indentation_for_language(document.language.as_deref())
                    .0
            })
            .unwrap_or_else(|| self.config.editor.tab_size.max(1));
        let (documents, layout) = (&self.documents, &mut self.layout);
        let Some(pane) = layout.active_editor_mut(focus) else {
            return;
        };
        let Some(document) = documents.get(&pane.view.doc) else {
            return;
        };
        let Some(editable) = document.editable_opt() else {
            return;
        };
        let position = char_idx_to_display_pos(
            editable.text(),
            pane.view.selections.primary().head,
            tab_size,
        );
        let gutter_width = editable.text().len_lines().max(1).to_string().len().max(2) + 3;
        let text_cols = pane_cols.saturating_sub(gutter_width).max(1);
        let previous_top = pane.view.scroll.top_line;
        if position.line < pane.view.scroll.top_line {
            pane.view.scroll.top_line = position.line;
        } else if position.line >= pane.view.scroll.top_line + rows {
            pane.view.scroll.top_line = position.line + 1 - rows;
        }
        if pane.view.scroll.top_line != previous_top {
            pane.view.scroll.wrapped_row_offset = 0;
        }
        let visual_row = (pane.view.scroll.top_line..position.line)
            .map(|line| editor_wrapped_line_rows(editable.text(), line, text_cols, tab_size))
            .sum::<usize>()
            + position.col / text_cols;
        if visual_row < pane.view.scroll.wrapped_row_offset {
            pane.view.scroll.wrapped_row_offset = visual_row;
        } else if visual_row >= pane.view.scroll.wrapped_row_offset + rows {
            pane.view.scroll.wrapped_row_offset = visual_row + 1 - rows;
        }
    }
}

fn find_occurrence(haystack: &[char], needle: &[char], start: usize) -> Option<usize> {
    if needle.is_empty() || start > haystack.len().saturating_sub(needle.len()) {
        return None;
    }
    haystack[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

fn selected_lines(text: &Rope, selections: &crate::view::Selections) -> Vec<usize> {
    let mut lines = BTreeSet::new();
    for selection in selections.iter() {
        let range = selection.range();
        let start = text.char_to_line(range.start.min(text.len_chars()));
        let last_char = if range.end > range.start {
            range.end.saturating_sub(1)
        } else {
            range.end
        };
        let end = text.char_to_line(last_char.min(text.len_chars()));
        lines.extend(start..=end);
    }
    lines.into_iter().collect()
}

fn editor_wrapped_line_rows(text: &Rope, line: usize, width: usize, tab_size: usize) -> usize {
    let display_width = text
        .line(line.min(text.len_lines().saturating_sub(1)))
        .chars()
        .take_while(|character| !matches!(character, '\r' | '\n'))
        .fold(0, |column, character| {
            display_col_after(column, character, tab_size)
        });
    display_width.max(1).div_ceil(width.max(1))
}

pub struct ActiveBuffer<'a> {
    pub name: String,
    pub text: &'a Rope,
    pub view: &'a View,
    pub modified: bool,
    pub external_changed: bool,
    pub language: Option<&'a str>,
    pub tab_size: usize,
    pub language_status: String,
    pub diagnostics: &'a [crate::lsp::Diagnostic],
    pub git_lines: &'a [GitLine],
    pub git_branch: Option<&'a str>,
    pub git_status: Option<&'a str>,
    pub semantic_spans: &'a [crate::lsp::SemanticSpan],
    pub syntax_spans: &'a [crate::highlight::HighlightSpan],
}

pub struct LargeBuffer<'a> {
    pub file: &'a LargeFile,
    pub view: &'a View,
}

#[derive(Clone, Copy, Debug)]
struct CommandPaletteEntry {
    key: &'static str,
    name: &'static str,
    description: &'static str,
    command: Command,
}

impl CommandPaletteEntry {
    fn label(self) -> String {
        format!("{:<14} {:<25} — {}", self.key, self.name, self.description)
    }
}

const COMMAND_PALETTE: &[CommandPaletteEntry] = &[
    CommandPaletteEntry {
        key: "Ctrl+F",
        name: "Find / 検索",
        description: "現在→全バッファ→ディレクトリを検索",
        command: Command::OpenSearch,
    },
    CommandPaletteEntry {
        key: "Ctrl+Shift+F",
        name: "Find in Files / 全体検索",
        description: "ディレクトリを直接検索",
        command: Command::OpenSearchInDirectory,
    },
    CommandPaletteEntry {
        key: "Ctrl+H",
        name: "Replace / 置換",
        description: "検索と置換を開く",
        command: Command::OpenReplace,
    },
    CommandPaletteEntry {
        key: "F6",
        name: "Diff / バッファ比較",
        description: "比較対象を選び左右diff表示",
        command: Command::OpenDiffPicker,
    },
    CommandPaletteEntry {
        key: "Ctrl+T",
        name: "Find File / ファイル検索",
        description: "ワークスペースのファイルを開く",
        command: Command::OpenDirectoryPicker,
    },
    CommandPaletteEntry {
        key: "Ctrl+G",
        name: "Find Buffer / バッファ検索",
        description: "開いているバッファを選ぶ",
        command: Command::OpenBufferPicker,
    },
    CommandPaletteEntry {
        key: "Ctrl+P",
        name: "Command Palette / コマンド検索",
        description: "コマンドとキーバインドを検索",
        command: Command::OpenCommandPalette,
    },
    CommandPaletteEntry {
        key: "Ctrl+S",
        name: "Save / 保存",
        description: "現在のファイルを安全に保存",
        command: Command::Save,
    },
    CommandPaletteEntry {
        key: "Ctrl+W",
        name: "Close Buffer / 閉じる",
        description: "現在のバッファを閉じる",
        command: Command::CloseBuffer,
    },
    CommandPaletteEntry {
        key: "Ctrl+Z",
        name: "Undo / 元に戻す",
        description: "直前の編集を元に戻す",
        command: Command::Undo,
    },
    CommandPaletteEntry {
        key: "Ctrl+Y",
        name: "Redo / やり直す",
        description: "元に戻した編集をやり直す",
        command: Command::Redo,
    },
    CommandPaletteEntry {
        key: "Ctrl+C",
        name: "Copy / コピー",
        description: "選択範囲をコピー",
        command: Command::Copy,
    },
    CommandPaletteEntry {
        key: "Ctrl+X",
        name: "Cut / 切り取り",
        description: "選択範囲を切り取る",
        command: Command::Cut,
    },
    CommandPaletteEntry {
        key: "Ctrl+V",
        name: "Paste / 貼り付け",
        description: "クリップボードを貼り付ける",
        command: Command::Paste,
    },
    CommandPaletteEntry {
        key: "Ctrl+A",
        name: "Select All / 全選択",
        description: "現在のバッファをすべて選択",
        command: Command::SelectAll,
    },
    CommandPaletteEntry {
        key: "Ctrl+D",
        name: "Select Next / 次を選択",
        description: "次の同一語へカーソルを追加",
        command: Command::SelectNextOccurrence,
    },
    CommandPaletteEntry {
        key: "Tab",
        name: "Indent / インデント",
        description: "選択行をインデント",
        command: Command::Indent,
    },
    CommandPaletteEntry {
        key: "Shift+Tab",
        name: "Outdent / アンインデント",
        description: "選択行のインデントを戻す",
        command: Command::Outdent,
    },
    CommandPaletteEntry {
        key: "Ctrl+/ · Ctrl+Q",
        name: "Toggle Comment / コメント",
        description: "選択行のコメントを切り替える",
        command: Command::ToggleComment,
    },
    CommandPaletteEntry {
        key: "Ctrl+@ · Ctrl+Space",
        name: "Completion / 補完",
        description: "LSP補完候補を表示",
        command: Command::ToggleCompletion,
    },
    CommandPaletteEntry {
        key: "F2",
        name: "Rename Symbol / リネーム",
        description: "LSPでシンボル名を変更",
        command: Command::Rename,
    },
    CommandPaletteEntry {
        key: "—",
        name: "Format Document / 整形",
        description: "LSPで文書を整形",
        command: Command::Format,
    },
    CommandPaletteEntry {
        key: "Ctrl+]",
        name: "Split Editor / 左右分割",
        description: "エディタの左右分割を切り替える",
        command: Command::ToggleSplit,
    },
    CommandPaletteEntry {
        key: "Ctrl+O",
        name: "Terminal / シェル",
        description: "統合ターミナルを切り替える",
        command: Command::ToggleShell,
    },
    CommandPaletteEntry {
        key: "F4",
        name: "Quit / 終了",
        description: "エディタを終了",
        command: Command::Quit,
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PickerMode {
    Directory,
    Buffer,
    Diff,
    Command,
}

#[derive(Debug)]
struct PickerState {
    mode: PickerMode,
    base: DocumentId,
    return_side: Side,
    query: String,
    candidates: Vec<PickerCandidate>,
    filtered: Vec<usize>,
    selected: usize,
    ranking_cache: Vec<(String, Vec<usize>)>,
}

#[derive(Clone, Debug)]
enum PickerCandidate {
    Document(DocumentId),
    Path(PathBuf),
    Command(usize),
}

pub struct PickerView {
    pub title: &'static str,
    pub query: String,
    pub items: Vec<PickerViewItem>,
    pub selected: usize,
    pub has_before: bool,
    pub has_after: bool,
    pub total: usize,
}

pub struct PickerViewItem {
    pub label: String,
    pub matched: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchScope {
    CurrentBuffer,
    AllBuffers,
    Directory,
}

#[derive(Clone, Debug)]
enum SearchHit {
    Buffer {
        doc: DocumentId,
        range: std::ops::Range<usize>,
    },
    Disk(GrepHit),
}

#[derive(Debug)]
struct SearchState {
    query: String,
    replacement: Option<String>,
    editing_replace: bool,
    editing_filter: Option<SearchFilterField>,
    scope: SearchScope,
    options: SearchOptions,
    include_input: String,
    exclude_input: String,
    filters: SearchFilters,
    hits: Vec<SearchHit>,
    current: usize,
    grep_token: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchFilterField {
    Include,
    Exclude,
}

#[derive(Debug)]
struct ConfirmState {
    message: String,
    action: ConfirmAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ConfirmAction {
    Overwrite(DocumentId),
    DirectoryReplace {
        paths: Vec<PathBuf>,
        pattern: String,
        replacement: String,
    },
    CloseDiscard(DocumentId),
    QuitDiscard,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToastLevel {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug)]
struct Toast {
    level: ToastLevel,
    text: String,
    created: Instant,
    ttl: Duration,
}

pub struct NotificationView<'a> {
    pub level: ToastLevel,
    pub text: &'a str,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SearchFilters {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    pub respect_ignore_files: bool,
    pub include_hidden: bool,
}

fn split_globs(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|glob| !glob.is_empty())
        .map(str::to_owned)
        .collect()
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

fn search_pattern(query: &str, options: SearchOptions) -> Result<regex::Regex, regex::Error> {
    let mut source = if options.regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    if options.whole_word {
        source = format!(r"\b(?:{source})\b");
    }
    if !options.case_sensitive {
        source = format!("(?i){source}");
    }
    regex::Regex::new(&source)
}

pub struct SearchView {
    pub query: String,
    pub replacement: Option<String>,
    pub editing_replace: bool,
    pub editing_filter: Option<SearchFilterField>,
    pub scope: SearchScope,
    pub options: SearchOptions,
    pub include: String,
    pub exclude: String,
    pub filters: SearchFilters,
    pub items: Vec<String>,
    pub current: usize,
}

#[derive(Debug)]
enum PendingLsp {
    Completion {
        doc: DocumentId,
        version: i32,
        prefix: String,
        side: Side,
        anchor: CharIdx,
    },
    Definition,
    Rename {
        doc: DocumentId,
    },
    Formatting {
        doc: DocumentId,
    },
    Hover {
        doc: DocumentId,
        line: usize,
    },
    HoverProbe {
        doc: DocumentId,
    },
    SemanticTokens {
        doc: DocumentId,
        version: i32,
    },
}

fn hover_text(contents: lsp_types::HoverContents) -> String {
    match contents {
        lsp_types::HoverContents::Scalar(marked) => marked_string(marked),
        lsp_types::HoverContents::Array(items) => items
            .into_iter()
            .map(marked_string)
            .collect::<Vec<_>>()
            .join("\n"),
        lsp_types::HoverContents::Markup(markup) => markup.value,
    }
}

fn split_left_width(total: u16) -> u16 {
    total / 2
}

fn split_right_width(total: u16) -> u16 {
    total
        .saturating_sub(split_left_width(total))
        .saturating_sub(1)
}

#[derive(Clone, Debug)]
struct TerminalSelection {
    anchor: (u16, u16),
    head: (u16, u16),
    snapshot: vt100::Screen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSelectionView {
    pub start: (u16, u16),
    pub end: (u16, u16),
}

fn ordered_terminal_points(first: (u16, u16), second: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    if first <= second {
        (first, second)
    } else {
        (second, first)
    }
}

fn next_hover_probe_index(text: &Rope, from: usize) -> Option<(CharIdx, usize)> {
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ];
    let len = text.len_chars();
    let mut index = from.min(len);
    if index > 0 && index < len && is_word(text.char(index - 1)) {
        while index < len && is_word(text.char(index)) {
            index += 1;
        }
    }
    while index < len {
        while index < len && !is_word(text.char(index)) {
            index += 1;
        }
        let start = index;
        while index < len && is_word(text.char(index)) {
            index += 1;
        }
        if start == index {
            break;
        }
        let word = text.slice(start..index).to_string();
        if word
            .chars()
            .next()
            .is_some_and(|character| character == '_' || character.is_alphabetic())
            && !KEYWORDS.contains(&word.as_str())
        {
            return Some((CharIdx(start), index));
        }
    }
    None
}

fn sampled_hover_probe_indices(text: &Rope, limit: usize) -> Vec<CharIdx> {
    if limit == 0 || text.len_chars() == 0 {
        return Vec::new();
    }
    let mut indices = Vec::with_capacity(limit);
    for segment in 0..limit {
        let from = text.len_chars().saturating_mul(segment) / limit;
        if let Some((index, _)) = next_hover_probe_index(text, from)
            && indices.last() != Some(&index)
        {
            indices.push(index);
        }
    }
    indices
}

fn marked_string(marked: lsp_types::MarkedString) -> String {
    match marked {
        lsp_types::MarkedString::String(value) => value,
        lsp_types::MarkedString::LanguageString(value) => {
            format!("```{}\n{}\n```", value.language, value.value)
        }
    }
}

#[derive(Debug)]
struct CompletionState {
    items: Vec<CompletionCandidate>,
    selected: usize,
    return_side: Side,
    anchor: CharIdx,
}

#[derive(Debug)]
struct CompletionCandidate {
    label: String,
    insert: String,
    prefix_len: usize,
    cursor_back: usize,
}

pub struct CompletionView {
    pub items: Vec<String>,
    pub selected: usize,
    pub anchor: CharIdx,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::CharIdx;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    #[test]
    fn quit_is_a_pure_state_transition_with_an_effect() {
        let mut editor = Editor::default();

        let effects = editor.update(Command::Quit.into());

        assert!(editor.should_quit());
        assert_eq!(effects, vec![Effect::Quit]);
    }

    #[test]
    fn modified_buffer_uses_confirm_before_quit() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextInput('x'));

        assert!(editor.update(Command::Quit.into()).is_empty());
        assert!(!editor.should_quit());
        assert!(editor.confirm_view().is_some());

        assert_eq!(
            editor.update(Command::PickerConfirm.into()),
            vec![Effect::Quit]
        );
        assert!(editor.should_quit());
    }

    #[test]
    fn resize_marks_the_editor_dirty() {
        let mut editor = Editor::default();
        assert!(editor.take_dirty());

        editor.update(AppEvent::Resize {
            cols: 120,
            rows: 40,
        });

        assert_eq!(editor.terminal_size(), (120, 40));
        assert!(editor.take_dirty());
        assert!(!editor.take_dirty());
    }

    #[test]
    fn shell_focus_keeps_the_left_editor_available_for_rendering() {
        let mut editor = Editor::default();
        editor.config.editor.shell = Some("/configured/shell".to_owned());
        editor.update(AppEvent::TextPaste("visible".to_owned()));
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.hover = Some("old hover".to_owned());

        let effects = editor.update(Command::ToggleShell.into());

        assert!(editor.shell_focused());
        assert!(editor.hover_view().is_none());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "visible");
        assert!(matches!(
            effects.as_slice(),
            [Effect::SpawnShell { shell, .. }]
                if shell.as_deref() == Some("/configured/shell")
        ));

        assert!(editor.update(Command::ToggleShell.into()).is_empty());
        assert!(!editor.shell_visible());
        assert!(editor.terminal.is_some());

        assert!(editor.update(Command::ToggleShell.into()).is_empty());
        assert!(editor.shell_visible());
        assert!(editor.shell_focused());

        editor.update(AppEvent::Terminal(TerminalEvent::Exited(None)));
        assert!(!editor.shell_visible());
        assert!(editor.terminal.is_none());
        assert_eq!(editor.status(), None);
    }

    #[test]
    fn shell_drag_selection_copies_a_stable_snapshot_and_clears_hover() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 20, rows: 6 });
        editor.update(Command::ToggleShell.into());
        editor.update(AppEvent::Terminal(TerminalEvent::Output(b"hello".to_vec())));
        editor.hover = Some("old hover".to_owned());

        let mouse = |kind, column| {
            AppEvent::Mouse(MouseInput {
                event: MouseEvent {
                    kind,
                    column,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                },
                clicks: 1,
            })
        };
        editor.update(mouse(MouseEventKind::Down(MouseButton::Left), 11));
        editor.update(mouse(MouseEventKind::Drag(MouseButton::Left), 15));
        let effects = editor.update(mouse(MouseEventKind::Up(MouseButton::Left), 15));

        assert_eq!(effects, vec![Effect::ClipboardOsc52("hello".to_owned())]);
        assert_eq!(
            editor.terminal_selection_view(),
            Some(TerminalSelectionView {
                start: (0, 0),
                end: (0, 4),
            })
        );
        assert!(editor.hover.is_none());

        editor.update(AppEvent::Terminal(TerminalEvent::Output(
            b"\rXXXXX".to_vec(),
        )));
        assert!(
            editor
                .terminal_screen()
                .unwrap()
                .contents()
                .contains("hello")
        );

        editor.update(AppEvent::TerminalInput(b"x".to_vec()));
        assert!(editor.terminal_selection.is_none());
    }

    #[test]
    fn ctrl_c_without_a_shell_selection_still_sends_interrupt() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 20, rows: 6 });
        editor.update(Command::ToggleShell.into());

        assert_eq!(
            editor.update(Command::CopyShellSelection.into()),
            vec![Effect::TerminalInput(vec![3])]
        );
    }

    #[test]
    fn event_sequence_edits_moves_and_undoes() {
        let mut editor = Editor::default();

        editor.update(AppEvent::TextInput('a'));
        editor.update(AppEvent::TextInput('b'));
        editor.update(AppEvent::Command(Command::Move {
            direction: Direction::Left,
            unit: Unit::Character,
            extend: false,
        }));
        editor.update(AppEvent::Command(Command::DeleteForward));

        let buffer = editor.active_buffer().unwrap();
        assert_eq!(buffer.text.to_string(), "a");
        assert_eq!(buffer.view.selections.primary().head, CharIdx(1));

        editor.update(AppEvent::Command(Command::Undo));
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "ab");
    }

    #[test]
    fn mouse_side_buttons_undo_and_redo() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextInput('a'));
        editor.update(AppEvent::Command(Command::Move {
            direction: Direction::Right,
            unit: Unit::Character,
            extend: false,
        }));

        let side_button = |button| {
            AppEvent::Mouse(MouseInput {
                event: MouseEvent {
                    kind: MouseEventKind::Down(button),
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                },
                clicks: 1,
            })
        };

        editor.update(side_button(MouseButton::Back));
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "");

        editor.update(side_button(MouseButton::Forward));
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "a");
    }

    #[test]
    fn select_next_occurrence_edits_every_selection() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("one one".to_owned()));

        editor.update(Command::SelectNextOccurrence.into());
        editor.update(Command::SelectNextOccurrence.into());
        editor.update(AppEvent::TextInput('X'));

        let buffer = editor.active_buffer().unwrap();
        assert_eq!(buffer.text.to_string(), "X X");
        assert_eq!(buffer.view.selections.len(), 2);
    }

    #[test]
    fn vertical_cursor_addition_edits_both_lines() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("a\nb".to_owned()));
        editor.update(
            Command::Move {
                direction: Direction::Left,
                unit: Unit::Document,
                extend: false,
            }
            .into(),
        );

        editor.update(
            Command::AddCursor {
                direction: VerticalDirection::Down,
            }
            .into(),
        );
        editor.update(AppEvent::TextInput('X'));

        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "Xa\nXb");
    }

    #[test]
    fn copy_updates_register_and_emits_osc52_effect() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("copy me".to_owned()));
        editor.update(Command::SelectAll.into());

        let effects = editor.update(Command::Copy.into());

        assert_eq!(effects, vec![Effect::ClipboardOsc52("copy me".to_owned())]);
    }

    #[test]
    fn copy_without_a_selection_copies_and_pastes_the_current_line() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("one\ntwo".to_owned()));

        let effects = editor.update(Command::Copy.into());

        assert_eq!(effects, vec![Effect::ClipboardOsc52("two\n".to_owned())]);
        editor.update(Command::Paste.into());
        assert_eq!(
            editor.active_buffer().unwrap().text.to_string(),
            "one\ntwo\ntwo"
        );
    }

    #[test]
    fn cut_without_a_selection_cuts_and_restores_the_current_line() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("one\ntwo".to_owned()));
        editor.update(
            Command::Move {
                direction: Direction::Left,
                unit: Unit::Document,
                extend: false,
            }
            .into(),
        );

        let effects = editor.update(Command::Cut.into());

        assert_eq!(effects, vec![Effect::ClipboardOsc52("one\n".to_owned())]);
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "two");

        editor.update(Command::Undo.into());
        let buffer = editor.active_buffer().unwrap();
        assert_eq!(buffer.text.to_string(), "one\ntwo");
        assert!(buffer.view.selections.primary().is_caret());

        editor.update(Command::Redo.into());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "two");
        editor.update(Command::Paste.into());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "one\ntwo");
    }

    #[test]
    fn mouse_click_uses_text_coordinates_after_the_gutter() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("abc".to_owned()));
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });

        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 6,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));

        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .head,
            CharIdx(1)
        );
    }

    #[test]
    fn mouse_hit_testing_follows_soft_wrapped_rows() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 10, rows: 8 });
        editor.update(AppEvent::TextPaste("abcdefghij".to_owned()));

        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));

        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .head,
            CharIdx(5)
        );
    }

    #[test]
    fn lsp_hover_is_requested_on_click_not_mouse_move() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("value".to_owned()));
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("/tmp/hover.rs"));
        document.language = Some("rust".to_owned());
        editor.lsp_servers.insert("rust".to_owned(), 1);
        editor.lsp_ready.insert(1);
        editor.lsp_opened_documents.insert(DocumentId(0));
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 6,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        assert!(
            editor
                .update(AppEvent::Mouse(MouseInput {
                    event: moved,
                    clicks: 0
                }))
                .is_empty()
        );
        assert!(
            !editor
                .pending_lsp
                .values()
                .any(|pending| matches!(pending, PendingLsp::Hover { .. }))
        );

        let effects = editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                ..moved
            },
            clicks: 1,
        }));
        assert!(matches!(
            effects.as_slice(),
            [Effect::LspRequest { method, .. }] if method == "textDocument/hover"
        ));
    }

    #[test]
    fn click_before_lsp_open_is_deferred_and_sent_when_hover_becomes_available() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("value".to_owned()));
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("/tmp/deferred-hover.rs"));
        document.language = Some("rust".to_owned());
        editor.lsp_servers.insert("rust".to_owned(), 1);

        let effects = editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 6,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));
        assert!(effects.is_empty());
        assert_eq!(editor.deferred_hover, Some((DocumentId(0), CharIdx(1))));

        let effects = editor.update(AppEvent::Lsp(LspEvent::Initialized {
            server: 1,
            incremental_sync: true,
            hover_provider: true,
        }));

        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::LspRequest { id, method, .. }
                if method == "textDocument/hover"
                    && matches!(editor.pending_lsp.get(id), Some(PendingLsp::Hover { .. }))
        )));
        assert!(editor.deferred_hover.is_none());
    }

    #[test]
    fn selecting_a_range_dismisses_hover_without_requesting_another() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("value".to_owned()));
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.hover = Some("old hover".to_owned());

        let effects = editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 6,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 2,
        }));

        assert!(effects.is_empty());
        assert!(editor.hover_view().is_none());
        assert!(
            !editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .is_caret()
        );
    }

    #[test]
    fn edits_preserve_shifted_semantic_colors_and_ignore_old_responses() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("/tmp/colors.rs"));
        document.language = Some("rust".to_owned());
        document
            .editable_mut()
            .semantic_spans
            .push(crate::lsp::SemanticSpan {
                start: CharIdx(0),
                end: CharIdx(1),
                token_type: 0,
            });
        editor.lsp_servers.insert("rust".to_owned(), 1);
        editor.lsp_ready.insert(1);
        editor.lsp_incremental_sync.insert(1);
        editor.lsp_opened_documents.insert(DocumentId(0));
        editor.document_versions.insert(DocumentId(0), 1);

        let effects = editor.update(AppEvent::TextInput('a'));
        let sync_message = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::LspSend { message, .. } if message.contains("textDocument/didChange") => {
                    Some(serde_json::from_str::<serde_json::Value>(message).unwrap())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(
            sync_message["params"]["contentChanges"][0]["range"]["start"],
            serde_json::json!({"line": 0, "character": 0})
        );
        assert_eq!(sync_message["params"]["contentChanges"][0]["text"], "a");
        let version = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::ScheduleSemanticRefresh { version, .. } => Some(*version),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            editor.active_buffer().unwrap().semantic_spans[0].start,
            CharIdx(1)
        );
        let due = editor.update(AppEvent::Lsp(LspEvent::SemanticRefreshDue {
            doc: DocumentId(0),
            version,
        }));
        let old_request = due
            .iter()
            .find_map(|effect| match effect {
                Effect::LspRequest { id, method, .. }
                    if method == "textDocument/semanticTokens/full" =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .unwrap();

        editor.update(AppEvent::TextInput('b'));
        editor.update(AppEvent::Lsp(LspEvent::Response {
            id: old_request,
            result: Ok(serde_json::json!({"data": [0, 0, 1, 0, 0]})),
        }));

        let span = &editor.active_buffer().unwrap().semantic_spans[0];
        assert_eq!((span.start, span.end), (CharIdx(2), CharIdx(3)));
    }

    #[test]
    fn double_and_triple_click_select_word_and_line() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("one two\nnext".to_owned()));
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 9,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        editor.update(AppEvent::Mouse(MouseInput { event, clicks: 2 }));
        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .range(),
            4..7
        );

        editor.update(AppEvent::Mouse(MouseInput { event, clicks: 3 }));
        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .range(),
            0..8
        );
    }

    #[test]
    fn diff_picker_opens_current_and_selected_buffers_side_by_side() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("same\nleft".to_owned()),
        }));
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(2),
            result: Ok("same\nright".to_owned()),
        }));

        editor.update(Command::OpenDiffPicker.into());
        assert_eq!(editor.focus(), Focus::Overlay);
        editor.update(Command::PickerConfirm.into());

        let (left, right, diff) = editor.split_buffers().unwrap();
        assert!(diff);
        assert_eq!(left.text.to_string(), "same\nright");
        assert_eq!(right.text.to_string(), "same\nleft");
    }

    #[test]
    fn mouse_click_switches_the_active_split_buffer() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("left".to_owned()),
        }));
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(2),
            result: Ok("right".to_owned()),
        }));
        editor.update(Command::OpenDiffPicker.into());
        editor.update(Command::PickerConfirm.into());

        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 60,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));
        assert_eq!(editor.focus, Focus::Editor(Side::Right));
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "left");
        editor.update(AppEvent::TextInput('X'));
        let (left, right, _) = editor.split_buffers().unwrap();
        assert_eq!(left.text.to_string(), "right");
        assert_eq!(right.text.to_string(), "leftX");

        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 10,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));
        assert_eq!(editor.focus, Focus::Editor(Side::Left));
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "right");
    }

    #[test]
    fn opening_a_file_from_picker_exits_diff_mode() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("left".to_owned()),
        }));
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(2),
            result: Ok("right".to_owned()),
        }));
        editor.update(Command::OpenDiffPicker.into());
        editor.update(Command::PickerConfirm.into());
        assert!(editor.split_buffers().is_some_and(|(_, _, diff)| diff));

        editor.update(Command::OpenDirectoryPicker.into());
        editor.update(AppEvent::FileScan(FileScanEvent::Batch {
            token: 1,
            paths: vec![PathBuf::from("next.txt")],
        }));
        let effects = editor.update(Command::PickerConfirm.into());

        assert!(editor.split_buffers().is_none());
        assert!(
            matches!(effects.as_slice(), [Effect::ReadFile { path, .. }] if path == &PathBuf::from("next.txt"))
        );
    }

    #[test]
    fn buffer_picker_replaces_the_mouse_focused_pane_without_closing_split() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.open_paths([PathBuf::from("left.txt"), PathBuf::from("right.txt")]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("left".to_owned()),
        }));
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(2),
            result: Ok("right".to_owned()),
        }));
        editor.update(Command::ToggleSplit.into());
        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 10,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));
        editor.update(Command::OpenBufferPicker.into());
        for character in "left.txt".chars() {
            editor.update(AppEvent::TextInput(character));
        }
        editor.update(Command::PickerConfirm.into());

        let (left, right, diff) = editor.split_buffers().unwrap();
        assert!(!diff);
        assert_eq!(left.text.to_string(), "left");
        assert_eq!(right.text.to_string(), "right");
        assert_eq!(editor.focus, Focus::Editor(Side::Left));
    }

    #[test]
    fn buffer_picker_excludes_the_current_buffer() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("other.txt"), PathBuf::from("current.txt")]);

        editor.update(Command::OpenBufferPicker.into());

        let picker = editor.picker_view().unwrap();
        assert_eq!(picker.items.len(), 1);
        assert!(picker.items[0].label.contains("other.txt"));
        assert!(!picker.items[0].label.contains("current.txt"));
    }

    #[test]
    fn diff_picker_excludes_another_document_for_the_same_file() {
        let mut editor = Editor::default();
        editor.open_paths([
            PathBuf::from("same.txt"),
            PathBuf::from("other.txt"),
            PathBuf::from("same.txt"),
        ]);

        editor.update(Command::OpenDiffPicker.into());

        let picker = editor.picker_view().unwrap();
        assert_eq!(picker.items.len(), 1);
        assert!(picker.items[0].label.contains("other.txt"));
        assert!(!picker.items[0].label.contains("same.txt"));
    }

    #[test]
    fn command_palette_searches_labels_and_executes_selected_command() {
        let mut editor = Editor::default();
        editor.update(Command::OpenCommandPalette.into());
        for character in "find file".chars() {
            editor.update(AppEvent::TextInput(character));
        }

        let palette = editor.picker_view().unwrap();
        assert!(palette.items[0].label.contains("Ctrl+T"));
        assert!(palette.items[0].label.contains("Find File"));

        let effects = editor.update(Command::PickerConfirm.into());
        assert!(matches!(effects.as_slice(), [Effect::StartFileScan { .. }]));
    }

    #[test]
    fn command_palette_exposes_diff_keybinding() {
        let mut editor = Editor::default();
        editor.update(Command::OpenCommandPalette.into());
        for character in "diff".chars() {
            editor.update(AppEvent::TextInput(character));
        }

        let palette = editor.picker_view().unwrap();
        assert!(palette.items[0].label.contains("F6"));
        assert!(palette.items[0].label.contains("Diff"));
    }

    #[test]
    fn opening_any_picker_dismisses_hover() {
        let mut editor = Editor::default();
        for command in [
            Command::OpenCommandPalette,
            Command::OpenDirectoryPicker,
            Command::OpenBufferPicker,
            Command::OpenDiffPicker,
        ] {
            editor.hover = Some("hover text".to_owned());
            editor.update(command.into());
            assert!(editor.hover_view().is_none());
            editor.update(Command::PickerCancel.into());
        }
    }

    #[test]
    fn command_palette_restores_the_originating_pane_focus() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("abc".to_owned()));
        editor.update(Command::ToggleSplit.into());
        assert_eq!(editor.focus, Focus::Editor(Side::Right));
        editor.update(Command::OpenCommandPalette.into());
        for character in "select all".chars() {
            editor.update(AppEvent::TextInput(character));
        }

        editor.update(Command::PickerConfirm.into());

        assert_eq!(editor.focus, Focus::Editor(Side::Right));
        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .range(),
            0..3
        );
    }

    #[test]
    fn picker_view_virtualizes_large_candidate_lists() {
        let mut editor = Editor::default();
        editor.open_directory_picker();
        let picker = editor.picker.as_mut().unwrap();
        picker.candidates = (0..10_000)
            .map(|index| PickerCandidate::Path(PathBuf::from(format!("file-{index}.txt"))))
            .collect();
        picker.filtered = (0..picker.candidates.len()).collect();
        picker.selected = 9_000;

        let view = editor.picker_view().unwrap();

        assert_eq!(view.items.len(), 5);
        assert!(view.selected < view.items.len());
        assert!(view.items[view.selected].label.contains("file-9000.txt"));
        assert!(view.has_before);
        assert!(view.has_after);
    }

    #[test]
    fn picker_backspace_restores_cached_prefix_ranking() {
        let mut editor = Editor::default();
        editor.open_directory_picker();
        let picker = editor.picker.as_mut().unwrap();
        picker.candidates = (0..10_000)
            .map(|index| PickerCandidate::Path(PathBuf::from(format!("file-{index}.txt"))))
            .collect();
        picker.filtered = (0..picker.candidates.len()).collect();
        for character in "file-9".chars() {
            editor.update(AppEvent::TextInput(character));
        }
        let cache_before = editor.picker.as_ref().unwrap().ranking_cache.clone();

        editor.update(Command::PickerBackspace.into());

        let picker = editor.picker.as_ref().unwrap();
        assert_eq!(picker.query, "file-");
        assert_eq!(picker.ranking_cache, cache_before);
        assert_eq!(
            picker.filtered,
            cache_before
                .iter()
                .find(|(query, _)| query == "file-")
                .unwrap()
                .1
        );
    }

    #[test]
    fn save_emits_write_effect_and_marks_the_document_saved() {
        let mut editor = Editor::default();
        let path = PathBuf::from("/tmp/save-test.txt");
        editor.open_paths([path.clone()]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("old\n".to_owned()),
        }));
        let before = crate::document::DiskState {
            size: 4,
            modified_nanos: 1,
        };
        editor.update(AppEvent::Io(IoEvent::DiskStateObserved {
            id: DocumentId(1),
            result: Ok(before),
        }));
        editor.update(AppEvent::TextInput('x'));

        let effects = editor.update(Command::Save.into());
        assert_eq!(
            effects,
            vec![Effect::WriteFile {
                doc: DocumentId(1),
                path: path.clone(),
                contents: "xold\n".to_owned(),
                expected: Some(before),
            }]
        );

        let effects = editor.update(AppEvent::Io(IoEvent::FileSaved {
            id: DocumentId(1),
            result: Ok(()),
        }));
        assert!(matches!(
            effects.as_slice(),
            [Effect::ComputeGitStatus {
                doc: DocumentId(1),
                path: git_path,
            }] if git_path == &path
        ));
        assert!(!editor.active_buffer().unwrap().modified);

        let effects = editor.update(AppEvent::Io(IoEvent::DiskStateObserved {
            id: DocumentId(1),
            result: Ok(crate::document::DiskState {
                size: 5,
                modified_nanos: 2,
            }),
        }));
        assert!(effects.is_empty());

        editor.update(AppEvent::TextInput('y'));
        editor.update(Command::Undo.into());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "xold\n");
        assert!(!editor.active_buffer().unwrap().modified);

        editor.update(Command::Undo.into());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "old\n");
        assert!(editor.active_buffer().unwrap().modified);
    }

    #[test]
    fn buffer_search_and_replace_use_overlay_state() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste("one two one".to_owned()));
        editor.update(Command::OpenReplace.into());
        for character in "one".chars() {
            editor.update(AppEvent::TextInput(character));
        }
        assert_eq!(editor.search_view().unwrap().items.len(), 2);
        editor.update(Command::SearchToggleField.into());
        editor.update(AppEvent::TextInput('X'));

        editor.update(Command::PickerConfirm.into());

        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "X two X");
    }

    #[test]
    fn directory_search_emits_grep_effect() {
        let mut editor = Editor::default();
        editor.update(Command::OpenSearchInDirectory.into());

        let effects = editor.update(AppEvent::TextInput('x'));

        assert!(
            matches!(effects.as_slice(), [Effect::StartGrep { pattern, .. }] if pattern == "(?i)x")
        );
    }

    #[test]
    fn search_pattern_honors_case_word_and_regex_options() {
        let literal = search_pattern(
            "Foo",
            SearchOptions {
                case_sensitive: true,
                whole_word: true,
                regex: false,
            },
        )
        .unwrap();
        assert!(literal.is_match("Foo"));
        assert!(!literal.is_match("foo"));
        assert!(!literal.is_match("xFooy"));

        let regex = search_pattern(
            "foo.+bar",
            SearchOptions {
                regex: true,
                ..SearchOptions::default()
            },
        )
        .unwrap();
        assert!(regex.is_match("FOO xxx BAR"));
    }

    #[test]
    fn save_conflict_requires_explicit_overwrite_confirmation() {
        let mut editor = Editor::default();
        let path = PathBuf::from("/tmp/conflicted.txt");
        editor.open_paths([path.clone()]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("old".to_owned()),
        }));
        editor.update(AppEvent::TextInput('x'));

        editor.update(AppEvent::Io(IoEvent::SaveConflict {
            id: DocumentId(1),
            path: path.clone(),
        }));
        assert!(editor.confirm_view().is_some());
        let effects = editor.update(Command::PickerConfirm.into());

        assert!(matches!(
            effects.as_slice(),
            [Effect::WriteFile {
                doc: DocumentId(1),
                expected: None,
                ..
            }]
        ));
    }

    #[test]
    fn directory_replace_is_confirmed_before_disk_effect() {
        let mut editor = Editor::default();
        editor.update(Command::OpenReplace.into());
        editor.update(Command::OpenReplace.into());
        editor.update(Command::OpenReplace.into());
        editor.update(AppEvent::TextInput('o'));
        editor.update(Command::SearchToggleField.into());
        editor.update(AppEvent::TextInput('X'));
        let token = editor.search.as_ref().unwrap().grep_token.unwrap();
        editor.update(AppEvent::Grep(GrepEvent::Hits {
            token,
            hits: vec![GrepHit {
                path: PathBuf::from("/tmp/a.txt"),
                line: 0,
                text: "one".to_owned(),
            }],
        }));

        assert!(editor.update(Command::PickerConfirm.into()).is_empty());
        assert!(editor.confirm_view().is_some());
        let effects = editor.update(Command::PickerConfirm.into());
        assert!(matches!(
            effects.as_slice(),
            [Effect::ReplaceFiles { paths, replacement, .. }]
                if paths == &[PathBuf::from("/tmp/a.txt")] && replacement == "X"
        ));
    }

    #[test]
    fn formatting_command_sends_lsp_request() {
        let mut editor = Editor::default();
        let path = PathBuf::from("/tmp/format.rs");
        editor.open_paths([path]);
        editor.documents.get_mut(&DocumentId(1)).unwrap().language = Some("rust".to_owned());
        editor.lsp_servers.insert("rust".to_owned(), 7);
        editor.lsp_ready.insert(7);
        editor.lsp_opened_documents.insert(DocumentId(1));

        let effects = editor.update(Command::Format.into());

        assert!(matches!(
            effects.as_slice(),
            [Effect::LspRequest { server: 7, method, .. }]
                if method == "textDocument/formatting"
        ));
    }

    #[test]
    fn file_opened_after_lsp_initialization_gets_did_open_and_semantic_tokens() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("/tmp/first.rs")]);
        let startup = editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("fn first() {}".to_owned()),
        }));
        assert!(matches!(
            startup.last(),
            Some(Effect::SpawnLsp { server: 1, .. })
        ));
        editor.update(AppEvent::Lsp(LspEvent::Initialized {
            server: 1,
            incremental_sync: true,
            hover_provider: true,
        }));

        editor.open_paths([PathBuf::from("/tmp/second.rs")]);
        let effects = editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(2),
            result: Ok("fn second() {}".to_owned()),
        }));

        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::LspSend { server: 1, message }
                if message.contains("textDocument/didOpen") && message.contains("second.rs")
        )));
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::LspRequest { server: 1, method, params, .. }
                if method == "textDocument/semanticTokens/full" && params.contains("second.rs")
        )));
    }

    #[test]
    fn loading_config_starts_workspace_lsp_before_a_language_file_is_opened() {
        let mut editor = Editor::default();
        editor.set_workspace_root(PathBuf::from("/workspace"));

        let effects = editor.update(AppEvent::ConfigLoaded(Ok(Config::default())));

        assert!(matches!(
            effects.as_slice(),
            [Effect::SpawnLsp {
                language,
                root,
                ..
            }] if language == "rust" && root == &PathBuf::from("/workspace")
        ));
    }

    #[test]
    fn status_reports_language_and_lsp_lifecycle() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("main.rs")]);
        editor
            .documents
            .get_mut(&DocumentId(1))
            .unwrap()
            .load_text("fn main() { let alpha = beta; gamma(delta); epsilon(); }");
        editor.update(AppEvent::ConfigLoaded(Ok(Config::default())));
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: starting"
        );

        editor.update(AppEvent::Lsp(LspEvent::Spawned {
            server: 1,
            language: "rust".to_owned(),
        }));
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: initializing"
        );

        let effects = editor.update(AppEvent::Lsp(LspEvent::Initialized {
            server: 1,
            incremental_sync: true,
            hover_provider: true,
        }));
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: coloring"
        );

        let semantic_request = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::LspRequest { id, method, .. }
                    if method == "textDocument/semanticTokens/full" =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .unwrap();
        let hover_probe = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::LspRequest { id, method, .. } if method == "textDocument/hover" => {
                    Some(*id)
                }
                _ => None,
            })
            .unwrap();
        editor.update(AppEvent::Lsp(LspEvent::Response {
            id: semantic_request,
            result: Ok(serde_json::json!({"data": [0, 0, 1, 0, 0]})),
        }));
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: checking hover"
        );
        let mut probe_effects = editor.update(AppEvent::Lsp(LspEvent::Response {
            id: hover_probe,
            result: Ok(serde_json::json!({
                "contents": {"kind": "markdown", "value": "fn main()"}
            })),
        }));
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: checking hover"
        );
        for _ in 0..20 {
            if editor.active_buffer().unwrap().language_status == "<lsp> rust: ready" {
                break;
            }
            assert!(probe_effects.iter().any(|effect| matches!(
                effect,
                Effect::ScheduleHoverProbe {
                    doc: DocumentId(1),
                    ..
                }
            )));
            let request_effects = editor.update(AppEvent::Lsp(LspEvent::HoverProbeDue {
                doc: DocumentId(1),
            }));
            let Some(request) = request_effects.iter().find_map(|effect| match effect {
                Effect::LspRequest { id, method, .. } if method == "textDocument/hover" => {
                    Some(*id)
                }
                _ => None,
            }) else {
                continue;
            };
            probe_effects = editor.update(AppEvent::Lsp(LspEvent::Response {
                id: request,
                result: Ok(serde_json::json!({
                    "contents": {"kind": "markdown", "value": "hover"}
                })),
            }));
        }
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: ready"
        );

        editor.update(AppEvent::Lsp(LspEvent::Progress {
            server: 1,
            token: "index".to_owned(),
            message: Some("Indexing 20%".to_owned()),
        }));
        editor.update(AppEvent::Lsp(LspEvent::Progress {
            server: 1,
            token: "check".to_owned(),
            message: Some("Checking".to_owned()),
        }));
        editor.update(AppEvent::Lsp(LspEvent::Progress {
            server: 1,
            token: "index".to_owned(),
            message: None,
        }));
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: updating (Checking)"
        );
        editor.update(AppEvent::Lsp(LspEvent::Progress {
            server: 1,
            token: "check".to_owned(),
            message: None,
        }));

        editor.update(AppEvent::TextInput('x'));
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: updating"
        );

        editor.update(AppEvent::Lsp(LspEvent::Exited {
            server: 1,
            error: Some("not found".to_owned()),
        }));
        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: not found"
        );
    }

    #[test]
    fn crashed_lsp_restarts_three_times_with_exponential_backoff() {
        let mut editor = Editor::default();
        for expected_delay in [500, 1_000, 2_000] {
            assert_eq!(
                editor.update(AppEvent::Lsp(LspEvent::Exited {
                    server: 1,
                    error: Some("crashed".to_owned()),
                })),
                vec![Effect::ScheduleLspRestart {
                    server: 1,
                    delay_ms: expected_delay,
                }]
            );
        }

        assert!(
            editor
                .update(AppEvent::Lsp(LspEvent::Exited {
                    server: 1,
                    error: Some("crashed".to_owned()),
                }))
                .is_empty()
        );
        assert_eq!(
            editor.lsp_errors.get(&1).map(String::as_str),
            Some("crashed")
        );
    }

    #[test]
    fn failed_lsp_initialization_never_reports_ready() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("main.rs")]);
        editor.update(AppEvent::ConfigLoaded(Ok(Config::default())));
        editor.update(AppEvent::Lsp(LspEvent::Spawned {
            server: 1,
            language: "rust".to_owned(),
        }));

        editor.update(AppEvent::Lsp(LspEvent::InitializationFailed {
            server: 1,
            error: Some("initialize rejected".to_owned()),
        }));

        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<lsp> rust: error"
        );
        assert!(!editor.lsp_ready.contains(&1));
    }

    #[test]
    fn markdown_status_does_not_report_the_workspace_rust_lsp() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("README.md")]);
        editor.update(AppEvent::ConfigLoaded(Ok(Config::default())));

        assert_eq!(
            editor.active_buffer().unwrap().language_status,
            "<syntax> markdown"
        );
    }

    #[test]
    fn completion_excludes_the_candidate_identical_to_the_typed_prefix() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("main.rs"));
        document.language = Some("rust".to_owned());
        editor.lsp_servers.insert("rust".to_owned(), 1);
        editor.lsp_spawned.insert(1);
        editor.lsp_ready.insert(1);
        editor.lsp_opened_documents.insert(DocumentId(0));
        editor.document_versions.insert(DocumentId(0), 1);

        editor.update(AppEvent::TextPaste("let collections".to_owned()));
        let effects = editor.request_completion(true);
        let request = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::LspRequest { id, .. } => Some(*id),
                _ => None,
            })
            .unwrap();
        editor.update(AppEvent::Lsp(LspEvent::Response {
            id: request,
            result: Ok(serde_json::json!([
                {"label": "collections"},
                {"label": "collections_mut"}
            ])),
        }));

        let completion = editor.completion_view().unwrap();
        assert_eq!(completion.items, vec!["collections_mut"]);
        assert_eq!(completion.anchor, CharIdx(4));
    }

    #[test]
    fn function_completion_inserts_parentheses_and_places_caret_inside() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("main.rs"));
        document.language = Some("rust".to_owned());
        editor.lsp_servers.insert("rust".to_owned(), 1);
        editor.lsp_ready.insert(1);
        editor.lsp_opened_documents.insert(DocumentId(0));
        editor.document_versions.insert(DocumentId(0), 1);
        editor.update(AppEvent::TextPaste("cur".to_owned()));

        let request = editor
            .request_completion(true)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::LspRequest { id, .. } => Some(id),
                _ => None,
            })
            .unwrap();
        editor.update(AppEvent::Lsp(LspEvent::Response {
            id: request,
            result: Ok(serde_json::json!([{
                "label": "current_dir",
                "insertText": "current_dir",
                "kind": 3
            }])),
        }));
        editor.update(Command::PickerConfirm.into());

        let buffer = editor.active_buffer().unwrap();
        assert_eq!(buffer.text.to_string(), "current_dir()");
        assert_eq!(
            buffer.text.char(buffer.view.selections.primary().head.0),
            ')'
        );
    }

    #[test]
    fn indent_and_comment_apply_to_every_selected_line() {
        let mut editor = Editor::default();
        editor.documents.get_mut(&DocumentId(0)).unwrap().language = Some("rust".to_owned());
        editor.update(AppEvent::TextPaste("one\ntwo".to_owned()));
        editor.update(Command::SelectAll.into());

        editor.update(Command::Indent.into());
        assert_eq!(
            editor.active_buffer().unwrap().text.to_string(),
            "    one\n    two"
        );

        editor.update(Command::SelectAll.into());
        editor.update(Command::ToggleComment.into());
        assert_eq!(
            editor.active_buffer().unwrap().text.to_string(),
            "    // one\n    // two"
        );
        editor.update(Command::SelectAll.into());
        editor.update(Command::ToggleComment.into());
        assert_eq!(
            editor.active_buffer().unwrap().text.to_string(),
            "    one\n    two"
        );
    }

    #[test]
    fn tab_without_a_selection_inserts_configured_spaces_at_the_caret() {
        let mut editor = Editor::default();
        editor.config.editor.tab_size = 3;
        editor.update(AppEvent::TextPaste("ab".to_owned()));
        editor.update(
            Command::Move {
                direction: Direction::Left,
                unit: Unit::Character,
                extend: false,
            }
            .into(),
        );

        editor.update(Command::Indent.into());

        let buffer = editor.active_buffer().unwrap();
        assert_eq!(buffer.text.to_string(), "a   b");
        assert_eq!(buffer.view.selections.primary().head, CharIdx(4));
    }

    #[test]
    fn tab_then_newline_has_exactly_one_newline_and_no_tab_in_space_mode() {
        let mut editor = Editor::default();
        editor.documents.get_mut(&DocumentId(0)).unwrap().language = Some("rust".to_owned());

        editor.update(Command::Indent.into());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "    ");
        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .head,
            CharIdx(4)
        );

        editor.update(Command::InsertNewline.into());

        let buffer = editor.active_buffer().unwrap();
        assert_eq!(buffer.text.to_string(), "    \n    ");
        assert_eq!(
            buffer
                .text
                .chars()
                .filter(|character| *character == '\n')
                .count(),
            1
        );
        assert!(!buffer.text.to_string().contains('\t'));
        assert_eq!(buffer.view.selections.primary().head, CharIdx(9));
    }

    #[test]
    fn selected_lines_indent_together_and_outdent_clamps_to_existing_space() {
        let mut editor = Editor::default();
        editor.update(AppEvent::TextPaste(" a\n    b".to_owned()));
        editor.update(Command::SelectAll.into());

        editor.update(Command::Outdent.into());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "a\nb");

        editor.update(Command::Undo.into());
        assert_eq!(
            editor.active_buffer().unwrap().text.to_string(),
            " a\n    b"
        );
        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .range(),
            0..8
        );
    }

    #[test]
    fn makefile_tab_inserts_a_real_tab() {
        let mut editor = Editor::default();
        editor.documents.get_mut(&DocumentId(0)).unwrap().language = Some("make".to_owned());

        editor.update(Command::Indent.into());

        let buffer = editor.active_buffer().unwrap();
        assert_eq!(buffer.text.to_string(), "\t");
        assert_eq!(buffer.view.selections.primary().head, CharIdx(1));
        assert_eq!(buffer.tab_size, 4);
    }
}
