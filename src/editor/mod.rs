mod command;
mod effect;
mod event;
mod focus;
mod layout;

pub use command::{Command, Direction, Unit, VerticalDirection};
pub use effect::Effect;
pub use event::{
    AppEvent, FileScanEvent, GitEvent, GitLine, GitLineKind, GrepEvent, GrepHit, IoEvent,
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
    document::{Document, DocumentId, LargeFile, content_hash},
    lsp::LspEvent,
    position::{CharIdx, char_idx_to_display_pos, display_col_to_char_idx},
    view::{Selection, View, is_word, move_head},
};

const TAB_SIZE: usize = 4;

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
    lsp_ready: HashSet<u64>,
    lsp_restart_counts: HashMap<u64, u8>,
    next_server_id: u64,
    pending_lsp: HashMap<i64, PendingLsp>,
    next_lsp_request: i64,
    completion: Option<CompletionState>,
    rename_input: Option<String>,
    confirm: Option<ConfirmState>,
    hover: Option<String>,
    pending_lsp_sync: HashSet<DocumentId>,
    document_versions: HashMap<DocumentId, i32>,
    terminal: Option<vt100::Parser>,
    hint_guide: bool,
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
            lsp_ready: HashSet::new(),
            lsp_restart_counts: HashMap::new(),
            next_server_id: 1,
            pending_lsp: HashMap::new(),
            next_lsp_request: 1,
            completion: None,
            rename_input: None,
            confirm: None,
            hover: None,
            pending_lsp_sync: HashSet::new(),
            document_versions: HashMap::new(),
            terminal: None,
            hint_guide: false,
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
                    let shell_cols = (cols / 2).max(1);
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
                let definition =
                    matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left))
                        && mouse.event.modifiers.contains(KeyModifiers::CONTROL);
                let hover_index = matches!(mouse.event.kind, MouseEventKind::Moved)
                    .then(|| self.mouse_position(mouse.event.column, mouse.event.row))
                    .flatten();
                self.apply_mouse(mouse);
                if definition {
                    self.request_definition()
                } else if let Some(index) = hover_index {
                    self.request_hover_at(index)
                } else {
                    Vec::new()
                }
            }
            AppEvent::Io(event) => self.apply_io(event),
            AppEvent::ConfigLoaded(result) => {
                match result {
                    Ok(config) => {
                        self.config = config;
                        self.refresh_languages();
                    }
                    Err(error) => self.status = Some(error),
                }
                self.dirty = true;
                Vec::new()
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
            AppEvent::TerminalInput(bytes) => vec![Effect::TerminalInput(bytes)],
            AppEvent::Git(event) => {
                if let Ok(lines) = event.result
                    && let Some(document) = self.documents.get_mut(&event.doc)
                    && let crate::document::DocumentKind::Editable(editable) = &mut document.kind
                {
                    editable.git_lines = lines;
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
        effects.extend(self.take_lsp_sync_effects());
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
        let pane = self.layout.active_editor(self.focus)?;
        let document = self.documents.get(&pane.view.doc)?;
        let editable = document.editable_opt()?;
        Some(ActiveBuffer {
            name: document
                .path
                .as_ref()
                .map_or_else(|| "Untitled".to_owned(), |path| path.display().to_string()),
            text: editable.text(),
            view: &pane.view,
            modified: editable.modified,
            external_changed: document.external_changed,
            language: document.language.as_deref(),
            diagnostics: &editable.diagnostics,
            git_lines: &editable.git_lines,
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
        let pane = self.layout.active_editor(self.focus)?;
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
            if let Some(pane) = self.layout.active_editor_mut(self.focus) {
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
                    .map_or_else(|| "Untitled".to_owned(), |path| path.display().to_string()),
                text: left_document.text(),
                view: &left.view,
                modified: left_document.modified,
                external_changed: left_doc.external_changed,
                language: None,
                diagnostics: &left_document.diagnostics,
                git_lines: &left_document.git_lines,
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
                    .map_or_else(|| "Untitled".to_owned(), |path| path.display().to_string()),
                text: right_document.text(),
                view: &right.view,
                modified: right_document.modified,
                external_changed: right_doc.external_changed,
                language: None,
                diagnostics: &right_document.diagnostics,
                git_lines: &right_document.git_lines,
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
        let items = picker
            .filtered
            .iter()
            .filter_map(|index| picker.candidates.get(*index))
            .map(|candidate| self.candidate_label(candidate))
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
            selected: picker.selected,
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

    pub fn shell_visible(&self) -> bool {
        matches!(self.layout, Layout::EditorAndShell { .. })
    }

    pub fn hint_guide_visible(&self) -> bool {
        self.hint_guide
    }

    pub fn focused_side(&self) -> Side {
        match self.focus {
            Focus::Editor(side) => side,
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
                            effects.push(Effect::LoadUndoHistory { id, path });
                            effects.push(Effect::ComputeGitStatus {
                                doc: id,
                                path: document.path.clone().expect("path exists"),
                            });
                        }
                    }
                    if let Some(language) = self
                        .documents
                        .get(&id)
                        .and_then(|document| document.language.clone())
                        && !self.lsp_servers.contains_key(&language)
                        && let Some(command) = self
                            .config
                            .language
                            .iter()
                            .find(|config| config.name == language)
                            .and_then(|config| config.lsp.clone())
                    {
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
                            effects.push(Effect::SaveUndoHistory(
                                document.editable().persisted_history(path),
                            ));
                            self.status = Some(format!("保存しました: {}", path.display()));
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
            IoEvent::UndoHistoryLoaded { id, result } => match result {
                Ok(Some(history)) => {
                    if let Some(document) = self.documents.get_mut(&id) {
                        let current_hash = content_hash(&document.editable().text().to_string());
                        if history.path == document.path.clone().unwrap_or_default()
                            && history.base_hash == current_hash
                        {
                            document.editable_mut().restore_history(history);
                        } else {
                            self.status = Some("undo履歴を破棄（ファイルが外部変更）".to_owned());
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => self.status = Some(error),
            },
            IoEvent::UndoHistorySaved { result } => {
                if let Err(error) = result {
                    self.status = Some(error);
                }
            }
            IoEvent::DiskStateObserved { id, result } => match result {
                Ok(state) => {
                    let Some(document) = self.documents.get_mut(&id) else {
                        return effects;
                    };
                    let changed = document.disk_state.is_some_and(|old| old != state);
                    document.disk_state = Some(state);
                    if changed {
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
                self.notify(ToastLevel::Info, format!("{language} LSPを起動しました"));
                self.set_progress(format!("lsp:{server}"), format!("{language}: 初期化中"));
                self.status = Some(format!("{language} LSP: 初期化中"));
            }
            LspEvent::Initialized { server } => {
                self.finish_progress(&format!("lsp:{server}"));
                self.lsp_ready.insert(server);
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
                    for (doc_id, document) in &self.documents {
                        if document.language.as_deref() != Some(&language) {
                            continue;
                        }
                        let (Some(path), Some(editable)) =
                            (document.path.as_ref(), document.editable_opt())
                        else {
                            continue;
                        };
                        effects.push(Effect::LspSend {
                            server,
                            message: serde_json::json!({
                                "jsonrpc": "2.0",
                                "method": "textDocument/didOpen",
                                "params": {
                                    "textDocument": {
                                        "uri": format!("file://{}", path.display()),
                                        "languageId": language,
                                        "version": 1,
                                        "text": editable.text().to_string()
                                    }
                                }
                            })
                            .to_string(),
                        });
                        let id = self.next_lsp_request;
                        self.next_lsp_request += 1;
                        self.pending_lsp
                            .insert(id, PendingLsp::SemanticTokens { doc: *doc_id });
                        effects.push(Effect::LspRequest {
                            server,
                            id,
                            method: "textDocument/semanticTokens/full".to_owned(),
                            params: serde_json::json!({
                                "textDocument": {"uri": format!("file://{}", path.display())}
                            })
                            .to_string(),
                        });
                    }
                    self.status = Some(format!("{language} LSP: 準備完了"));
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
            LspEvent::Progress { server, message } => {
                self.set_progress(format!("lsp:{server}"), message.clone());
                self.status = Some(message);
            }
            LspEvent::Response { id, result } => match self.pending_lsp.remove(&id) {
                Some(PendingLsp::Completion) => match result.and_then(|value| {
                    serde_json::from_value::<lsp_types::CompletionResponse>(value)
                        .map_err(|error| error.to_string())
                }) {
                    Ok(response) => {
                        let items = match response {
                            lsp_types::CompletionResponse::Array(items) => items,
                            lsp_types::CompletionResponse::List(list) => list.items,
                        }
                        .into_iter()
                        .map(|item| CompletionCandidate {
                            insert: item
                                .insert_text
                                .clone()
                                .unwrap_or_else(|| item.label.clone()),
                            label: item.label,
                        })
                        .collect();
                        self.completion = Some(CompletionState { items, selected: 0 });
                        self.focus = Focus::Overlay;
                    }
                    Err(error) => self.status = Some(format!("補完に失敗: {error}")),
                },
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
                    Err(error) => self.status = Some(format!("hoverに失敗: {error}")),
                },
                Some(PendingLsp::SemanticTokens { doc }) => {
                    if let Ok(value) = result
                        && let Ok(Some(tokens)) =
                            serde_json::from_value::<Option<lsp_types::SemanticTokensResult>>(value)
                    {
                        self.apply_semantic_tokens(doc, tokens);
                    }
                }
                None => {}
            },
            LspEvent::Exited { server, error } => {
                self.finish_progress(&format!("lsp:{server}"));
                let message = error.unwrap_or_else(|| "LSPが終了しました".to_owned());
                self.notify(ToastLevel::Error, message.clone());
                self.status = Some(message);
                self.lsp_ready.remove(&server);
                let attempts = self.lsp_restart_counts.entry(server).or_insert(0);
                if *attempts < 3 {
                    let delay_ms = 500u64 * (1u64 << *attempts);
                    *attempts += 1;
                    effects.push(Effect::ScheduleLspRestart { server, delay_ms });
                } else {
                    self.status = Some("LSP再起動上限に達しました".to_owned());
                }
            }
            LspEvent::RestartDue { server } => {
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
        }
        self.dirty = true;
        effects
    }

    fn toggle_completion(&mut self) -> Vec<Effect> {
        if self.completion.take().is_some() {
            self.focus = Focus::Editor(Side::Left);
            self.dirty = true;
            return Vec::new();
        }
        let Some((server, path, line, character)) = self.active_lsp_context() else {
            self.status = Some("このバッファではLSP補完を利用できません".to_owned());
            self.dirty = true;
            return Vec::new();
        };
        let id = self.next_lsp_request;
        self.next_lsp_request += 1;
        self.pending_lsp.insert(id, PendingLsp::Completion);
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
        if self
            .pending_lsp
            .values()
            .any(|pending| matches!(pending, PendingLsp::Hover { .. }))
        {
            return Vec::new();
        }
        let Some((server, path, line, character)) = self.active_lsp_context_at(index) else {
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

    fn apply_semantic_tokens(&mut self, doc: DocumentId, result: lsp_types::SemanticTokensResult) {
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
        match command {
            Command::InsertNewline => self.edit_active(|document, view| {
                document.editable_mut().insert_newline(&mut view.selections);
            }),
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
            Command::Copy => return self.copy_active(),
            Command::Cut => {
                let effects = self.copy_active();
                if !effects.is_empty() {
                    self.edit_active(|document, view| {
                        document
                            .editable_mut()
                            .delete_backward(&mut view.selections);
                    });
                }
                return effects;
            }
            Command::Paste => {
                let fragments = self.clipboard.fragments().to_vec();
                if !fragments.is_empty() {
                    self.edit_active(|document, view| {
                        document
                            .editable_mut()
                            .insert_fragments(&mut view.selections, &fragments);
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
            Command::ToggleHintGuide => {
                self.hint_guide = !self.hint_guide;
                self.dirty = true;
            }
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
                self.terminal = Some(vt100::Parser::new(
                    self.terminal_size.1.saturating_sub(1),
                    self.terminal_size.0 / 2,
                    0,
                ));
                self.focus = Focus::Shell;
                self.dirty = true;
                vec![Effect::SpawnShell {
                    cols: (self.terminal_size.0 / 2).max(1),
                    rows: self.terminal_size.1.saturating_sub(1).max(1),
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
        self.edit_active(|document, view| {
            let editable = document.editable_opt().expect("editable document");
            let lines = selected_lines(editable.text(), &view.selections);
            let mut edits = Vec::new();
            let mut fragments = Vec::new();
            for line in lines {
                let start = editable.text().line_to_char(line);
                if outdent {
                    let text = editable.text().line(line).to_string();
                    let length = if text.starts_with('\t') {
                        1
                    } else {
                        text.chars()
                            .take_while(|character| *character == ' ')
                            .take(TAB_SIZE)
                            .count()
                    };
                    if length == 0 {
                        continue;
                    }
                    edits.push(Selection {
                        anchor: CharIdx(start),
                        head: CharIdx(start + length),
                    });
                    fragments.push(String::new());
                } else {
                    edits.push(Selection::caret(CharIdx(start)));
                    fragments.push(" ".repeat(TAB_SIZE));
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

    fn apply_terminal(&mut self, event: TerminalEvent) {
        match event {
            TerminalEvent::Output(bytes) => {
                if let Some(parser) = &mut self.terminal {
                    parser.process(&bytes);
                }
            }
            TerminalEvent::Exited(error) => {
                if let Layout::EditorAndShell { editor } = &self.layout {
                    self.layout = Layout::EditorFull(EditorPane {
                        view: editor.view.clone(),
                    });
                }
                self.focus = Focus::Editor(Side::Left);
                self.terminal = None;
                self.status = Some(error.unwrap_or_else(|| "シェルが終了しました".to_owned()));
            }
        }
        self.dirty = true;
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

    fn copy_active(&mut self) -> Vec<Effect> {
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
        let fragments = editable.selected_texts(&pane.view.selections);
        if fragments.iter().all(String::is_empty) {
            return Vec::new();
        }
        self.clipboard.store(fragments);
        vec![Effect::ClipboardOsc52(self.clipboard.osc52_text())]
    }

    fn apply_mouse(&mut self, input: MouseInput) {
        let mouse = input.event;
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_active(-3),
            MouseEventKind::ScrollDown => self.scroll_active(3),
            MouseEventKind::Down(MouseButton::Left) => {
                let right_half = mouse.column >= self.terminal_size.0 / 2;
                match &self.layout {
                    Layout::EditorAndEditor { .. } => {
                        self.focus =
                            Focus::Editor(if right_half { Side::Right } else { Side::Left });
                    }
                    Layout::EditorAndShell { .. } if right_half => {
                        self.focus = Focus::Shell;
                        self.dirty = true;
                        return;
                    }
                    Layout::EditorAndShell { .. } => self.focus = Focus::Editor(Side::Left),
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
            MouseEventKind::Up(MouseButton::Left) => self.drag_anchor = None,
            _ => {}
        }
    }

    fn open_picker(&mut self, mode: PickerMode) {
        let Some(base) = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
        else {
            return;
        };
        let candidates: Vec<_> = self
            .documents
            .keys()
            .copied()
            .filter(|id| mode == PickerMode::Buffer || *id != base)
            .map(PickerCandidate::Document)
            .collect();
        if candidates.is_empty() {
            self.status = Some("比較できる別のバッファがありません".to_owned());
            self.dirty = true;
            return;
        }
        self.picker = Some(PickerState {
            mode,
            base,
            return_side: match self.focus {
                Focus::Editor(side) => side,
                Focus::Shell | Focus::Overlay => Side::Left,
            },
            filtered: (0..candidates.len()).collect(),
            candidates,
            query: String::new(),
            selected: 0,
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
    }

    fn open_command_palette(&mut self) {
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
                Focus::Editor(side) => side,
                Focus::Shell | Focus::Overlay => Side::Left,
            },
            filtered: (0..candidates.len()).collect(),
            candidates,
            query: String::new(),
            selected: 0,
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
    }

    fn open_directory_picker(&mut self) -> Vec<Effect> {
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
                Focus::Editor(side) => side,
                Focus::Shell | Focus::Overlay => Side::Left,
            },
            query: String::new(),
            candidates: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
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
                    picker
                        .candidates
                        .extend(paths.into_iter().map(PickerCandidate::Path));
                    self.refresh_picker();
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
        let candidates = picker.candidates.clone();
        let matcher = SkimMatcherV2::default();
        let mut scored: Vec<_> = candidates
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| {
                let label = self.candidate_label(candidate);
                if query.is_empty() {
                    Some((index, 0))
                } else {
                    matcher
                        .fuzzy_match(&label, &query)
                        .map(|score| (index, score))
                }
            })
            .collect();
        scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
        let picker = self.picker.as_mut().expect("picker exists");
        picker.filtered = scored.into_iter().map(|(index, _)| index).collect();
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
                self.focus = Focus::Editor(Side::Left);
                self.edit_active(|document, view| {
                    document
                        .editable_mut()
                        .insert(&mut view.selections, &insert);
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
                self.focus = Focus::Editor(Side::Left);
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
        match (picker.mode, candidate) {
            (PickerMode::Directory, PickerCandidate::Path(path)) => {
                effects.extend(self.open_paths([path]));
            }
            (PickerMode::Buffer, PickerCandidate::Document(target)) => {
                self.layout = Layout::EditorFull(EditorPane {
                    view: View::new(target),
                });
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
            }
            (PickerMode::Command, PickerCandidate::Command(index)) => {
                self.focus = Focus::Editor(picker.return_side);
                self.dirty = true;
                return self.apply_command(COMMAND_PALETTE[index].command);
            }
            _ => {}
        }
        self.focus = Focus::Editor(Side::Left);
        self.dirty = true;
        effects
    }

    fn close_picker(&mut self) {
        self.picker = None;
        self.search = None;
        self.completion = None;
        self.rename_input = None;
        self.confirm = None;
        self.focus = Focus::Editor(Side::Left);
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
        let line =
            (pane.view.scroll.top_line + usize::from(row)).min(text.len_lines().saturating_sub(1));
        let gutter_width = text.len_lines().max(1).to_string().len().max(2) + 3;
        let local_column = if matches!(self.focus, Focus::Editor(Side::Right)) {
            column.saturating_sub(self.terminal_size.0 / 2)
        } else {
            column
        };
        let display_col = usize::from(local_column)
            .saturating_sub(gutter_width)
            .saturating_add(pane.view.scroll.left_col);
        Some(display_col_to_char_idx(text, line, display_col, TAB_SIZE))
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
        self.dirty = true;
    }

    fn edit_active(&mut self, edit: impl FnOnce(&mut Document, &mut View)) {
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
        self.pending_lsp_sync.insert(pane.view.doc);
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn take_lsp_sync_effects(&mut self) -> Vec<Effect> {
        let pending: Vec<_> = self.pending_lsp_sync.drain().collect();
        let mut effects = Vec::new();
        for id in pending {
            let Some(document) = self.documents.get(&id) else {
                continue;
            };
            let (Some(language), Some(path), Some(editable)) = (
                document.language.as_ref(),
                document.path.as_ref(),
                document.editable_opt(),
            ) else {
                continue;
            };
            let Some(server) = self.lsp_servers.get(language).copied() else {
                continue;
            };
            let version = self.document_versions.entry(id).or_insert(1);
            *version += 1;
            effects.push(Effect::LspSend {
                server,
                message: serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "textDocument/didChange",
                    "params": {
                        "textDocument": {
                            "uri": format!("file://{}", path.display()),
                            "version": *version
                        },
                        "contentChanges": [{"text": editable.text().to_string()}]
                    }
                })
                .to_string(),
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
        let rows = usize::from(self.terminal_size.1.saturating_sub(1)).max(1);
        let cols = usize::from(self.terminal_size.0);
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
        let position = char_idx_to_display_pos(
            editable.text(),
            pane.view.selections.primary().head,
            TAB_SIZE,
        );
        let gutter_width = editable.text().len_lines().max(1).to_string().len().max(2) + 3;
        let visible_cols = cols.saturating_sub(gutter_width).max(1);
        if position.line < pane.view.scroll.top_line {
            pane.view.scroll.top_line = position.line;
        } else if position.line >= pane.view.scroll.top_line + rows {
            pane.view.scroll.top_line = position.line + 1 - rows;
        } else {
            pane.view.scroll.top_line = pane
                .view
                .scroll
                .top_line
                .min((position.line + 1).saturating_sub(rows));
        }
        if position.col < pane.view.scroll.left_col {
            pane.view.scroll.left_col = position.col;
        } else if position.col >= pane.view.scroll.left_col + visible_cols {
            pane.view.scroll.left_col = position.col + 1 - visible_cols;
        } else {
            pane.view.scroll.left_col = pane
                .view
                .scroll
                .left_col
                .min((position.col + 1).saturating_sub(visible_cols));
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

pub struct ActiveBuffer<'a> {
    pub name: String,
    pub text: &'a Rope,
    pub view: &'a View,
    pub modified: bool,
    pub external_changed: bool,
    pub language: Option<&'a str>,
    pub diagnostics: &'a [crate::lsp::Diagnostic],
    pub git_lines: &'a [GitLine],
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
        key: "Ctrl+Space",
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
        key: "Ctrl+\\",
        name: "Split Editor / 左右分割",
        description: "エディタの左右分割を切り替える",
        command: Command::ToggleSplit,
    },
    CommandPaletteEntry {
        key: "Ctrl+@",
        name: "Terminal / シェル",
        description: "統合ターミナルを切り替える",
        command: Command::ToggleShell,
    },
    CommandPaletteEntry {
        key: "Alt+H",
        name: "Shortcut Guide / キーガイド",
        description: "操作ヒントの表示を切り替える",
        command: Command::ToggleHintGuide,
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
    pub items: Vec<String>,
    pub selected: usize,
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
    Completion,
    Definition,
    Rename { doc: DocumentId },
    Formatting { doc: DocumentId },
    Hover { doc: DocumentId, line: usize },
    SemanticTokens { doc: DocumentId },
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

fn marked_string(marked: lsp_types::MarkedString) -> String {
    match marked {
        lsp_types::MarkedString::String(value) => value,
        lsp_types::MarkedString::LanguageString(value) => value.value,
    }
}

#[derive(Debug)]
struct CompletionState {
    items: Vec<CompletionCandidate>,
    selected: usize,
}

#[derive(Debug)]
struct CompletionCandidate {
    label: String,
    insert: String,
}

pub struct CompletionView {
    pub items: Vec<String>,
    pub selected: usize,
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
    fn command_palette_searches_labels_and_executes_selected_command() {
        let mut editor = Editor::default();
        editor.update(Command::OpenCommandPalette.into());
        for character in "find file".chars() {
            editor.update(AppEvent::TextInput(character));
        }

        let palette = editor.picker_view().unwrap();
        assert!(palette.items[0].contains("Ctrl+T"));
        assert!(palette.items[0].contains("Find File"));

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
        assert!(palette.items[0].contains("F6"));
        assert!(palette.items[0].contains("Diff"));
    }

    #[test]
    fn command_palette_restores_the_originating_pane_focus() {
        let mut editor = Editor::default();
        editor.update(Command::ToggleSplit.into());
        assert_eq!(editor.focus, Focus::Editor(Side::Right));
        editor.update(Command::OpenCommandPalette.into());
        for character in "shortcut guide".chars() {
            editor.update(AppEvent::TextInput(character));
        }

        editor.update(Command::PickerConfirm.into());

        assert_eq!(editor.focus, Focus::Editor(Side::Right));
        assert!(editor.hint_guide_visible());
    }

    #[test]
    fn save_emits_write_effect_and_success_emits_history_effect() {
        let mut editor = Editor::default();
        let path = PathBuf::from("/tmp/save-test.txt");
        editor.open_paths([path.clone()]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("old\n".to_owned()),
        }));
        editor.update(AppEvent::TextInput('x'));

        let effects = editor.update(Command::Save.into());
        assert_eq!(
            effects,
            vec![Effect::WriteFile {
                doc: DocumentId(1),
                path: path.clone(),
                contents: "xold\n".to_owned(),
                expected: None,
            }]
        );

        let effects = editor.update(AppEvent::Io(IoEvent::FileSaved {
            id: DocumentId(1),
            result: Ok(()),
        }));
        assert!(matches!(effects.as_slice(), [Effect::SaveUndoHistory(_)]));
        assert!(!editor.active_buffer().unwrap().modified);
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

        let effects = editor.update(Command::Format.into());

        assert!(matches!(
            effects.as_slice(),
            [Effect::LspRequest { server: 7, method, .. }]
                if method == "textDocument/formatting"
        ));
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
}
