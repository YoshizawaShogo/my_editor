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

/// Number of picker candidates shown at once (and the mouse hit-test window).
const PICKER_VIEW_WINDOW: usize = 20;
const TERMINAL_SCROLLBACK_LINES: usize = 10_000;

/// One language server's lifecycle and negotiated capabilities.
///
/// These used to be eight parallel maps keyed by the server id, so every
/// lifecycle transition had to touch all of them by hand — an easy place to
/// leave a stale flag when a server died. Bundled here, spawn/exit is a single
/// state change and the entry lives as long as the language binding (a restart
/// reuses the id), so [`Self::mark_down`] just clears capabilities in place.
#[derive(Debug)]
struct LspServer {
    language: String,
    spawned: bool,
    ready: bool,
    hover_capable: bool,
    incremental_sync: bool,
    semantic_legend: Option<crate::lsp::SemanticTokensLegend>,
    error: Option<String>,
    restart_count: u8,
}

impl LspServer {
    fn new(language: String) -> Self {
        Self {
            language,
            spawned: false,
            ready: false,
            hover_capable: false,
            incremental_sync: false,
            semantic_legend: None,
            error: None,
            restart_count: 0,
        }
    }

    /// The process went down (crash or failed initialization): clear every
    /// negotiated capability so a respawn re-handshakes, and record why.
    fn mark_down(&mut self, error: String) {
        self.spawned = false;
        self.ready = false;
        self.hover_capable = false;
        self.incremental_sync = false;
        self.semantic_legend = None;
        self.error = Some(error);
    }
}

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
    /// Language name → server id; the index into `servers`.
    lsp_servers: HashMap<String, u64>,
    servers: HashMap<u64, LspServer>,
    next_server_id: u64,
    pending_lsp: HashMap<i64, PendingLsp>,
    next_lsp_request: i64,
    completion: Option<CompletionState>,
    completion_suppressed: Option<(DocumentId, i32)>,
    rename_input: Option<String>,
    confirm: Option<ConfirmState>,
    hover: Option<String>,
    deferred_hover: Option<(DocumentId, CharIdx)>,
    nav_back: Vec<(DocumentId, CharIdx)>,
    nav_forward: Vec<(DocumentId, CharIdx)>,
    /// Caret positions to restore once a freshly opened document finishes loading,
    /// used by jumps that open a file whose text is not in memory yet.
    pending_caret_jumps: HashMap<DocumentId, lsp_types::Position>,
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
            servers: HashMap::new(),
            next_server_id: 1,
            pending_lsp: HashMap::new(),
            next_lsp_request: 1,
            completion: None,
            completion_suppressed: None,
            rename_input: None,
            confirm: None,
            hover: None,
            deferred_hover: None,
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            pending_caret_jumps: HashMap::new(),
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
                self.insert_typed_character(character, None);
                Vec::new()
            }
            AppEvent::TextInputAt { character, at } => {
                if self.focus == Focus::Overlay {
                    return self.overlay_input(character);
                }
                self.insert_typed_character(character, Some(at));
                self.autocomplete_after_typing(character);
                Vec::new()
            }
            AppEvent::TextPaste(text) => {
                if self.focus == Focus::Shell {
                    if let Some(parser) = &mut self.terminal {
                        let bracketed = parser.screen().bracketed_paste();
                        parser.set_scrollback(0);
                        let mut bytes = text.into_bytes();
                        if bracketed {
                            bytes.splice(0..0, b"\x1b[200~".iter().copied());
                            bytes.extend_from_slice(b"\x1b[201~");
                        }
                        self.terminal_selection = None;
                        self.dirty = true;
                        return vec![Effect::TerminalInput(bytes)];
                    }
                    return Vec::new();
                }
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
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
                if self.picker.is_some()
                    && matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left))
                {
                    if !self.picker_contains(mouse.event.column, mouse.event.row) {
                        self.close_picker();
                    }
                    return Vec::new();
                }
                if self.search.is_some() {
                    let (pane_x, _, pane_width, _) = self.search_pane_rect();
                    let over_pane =
                        mouse.event.column >= pane_x && mouse.event.column < pane_x + pane_width;
                    match mouse.event.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if let Some(effects) =
                                self.search_pane_click(mouse.event.column, mouse.event.row)
                            {
                                return effects;
                            }
                        }
                        MouseEventKind::ScrollDown if over_pane => {
                            self.scroll_search_results(3);
                            return Vec::new();
                        }
                        MouseEventKind::ScrollUp if over_pane => {
                            self.scroll_search_results(-3);
                            return Vec::new();
                        }
                        _ => {}
                    }
                }
                let over_terminal = matches!(self.layout, Layout::EditorAndShell { .. })
                    && mouse.event.column > split_left_width(self.terminal_size.0);
                if over_terminal {
                    match mouse.event.kind {
                        MouseEventKind::ScrollUp => {
                            self.scroll_terminal(3);
                            return Vec::new();
                        }
                        MouseEventKind::ScrollDown => {
                            self.scroll_terminal(-3);
                            return Vec::new();
                        }
                        _ => {}
                    }
                }
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
                if matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left)) {
                    self.dismiss_completion();
                    // A plain click is a jump; remember where we were so Ctrl+E can
                    // return. Ctrl+click goes to definition, which records its own origin.
                    if !mouse.event.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(self.focus, Focus::Editor(_))
                    {
                        self.record_jump_origin();
                    }
                }
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
                if let Some(parser) = &mut self.terminal {
                    parser.set_scrollback(0);
                }
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
            // 既に開いているファイルはそのバッファへ切り替える。複製すると同じ
            // URIを指す文書が増え、古い方を編集したときにLSPサーバーのテキスト
            // と食い違ってハイライトや診断がズレる。
            let existing = self
                .documents
                .iter()
                .find_map(|(id, document)| (document.path.as_ref() == Some(&path)).then_some(*id));
            let id = existing.unwrap_or_else(|| {
                let id = DocumentId(self.next_doc_id);
                self.next_doc_id += 1;
                let mut document = Document::scratch();
                document.path = Some(path.clone());
                document.language = self
                    .config
                    .language_for_path(&path)
                    .map(|language| language.name.clone());
                self.documents.insert(id, document);
                id
            });
            if matches!(self.layout, Layout::EditorAndEditor { diff: true, .. }) {
                self.layout = Layout::EditorFull(EditorPane {
                    view: View::new(id),
                });
                self.focus = Focus::Editor(Side::Left);
            } else if let Some(pane) = self.layout.active_editor_mut(self.focus) {
                pane.view = View::new(id);
            }
            let unsaved_edits = existing.is_some_and(|id| {
                self.documents
                    .get(&id)
                    .and_then(crate::document::Document::editable_opt)
                    .is_some_and(|editable| editable.modified)
            });
            if !unsaved_edits {
                effects.push(Effect::ReadFile { id, path });
            }
        }
        effects
    }

    /// Open a single file and place the caret at `position` once its text loads.
    fn open_path_at(&mut self, path: PathBuf, position: lsp_types::Position) -> Vec<Effect> {
        let effects = self.open_paths([path.clone()]);
        let Some(id) = self
            .documents
            .iter()
            .find_map(|(id, document)| (document.path.as_ref() == Some(&path)).then_some(*id))
        else {
            return effects;
        };
        self.pending_caret_jumps.insert(id, position);
        // 再読込が走らないケース(未保存編集のある既存バッファ)は FileLoaded が
        // 来ないので、その場でジャンプを適用する。
        if !effects
            .iter()
            .any(|effect| matches!(effect, Effect::ReadFile { id: read_id, .. } if *read_id == id))
        {
            self.apply_pending_caret_jump(id);
        }
        effects
    }

    /// Move the caret to the position recorded by [`Self::open_path_at`] now that
    /// the document `id` has loaded and can resolve the UTF-16 LSP column.
    fn apply_pending_caret_jump(&mut self, id: DocumentId) {
        let Some(position) = self.pending_caret_jumps.remove(&id) else {
            return;
        };
        let Some(document) = self.documents.get(&id) else {
            return;
        };
        let caret = document.editable_opt().map(|editable| {
            crate::position::lsp_position_to_char_idx(
                editable.text(),
                position.line as usize,
                position.character as usize,
            )
        });
        for pane in self.layout.panes_mut() {
            if pane.view.doc != id {
                continue;
            }
            match caret {
                Some(index) => pane.view.selections.set_single(Selection::caret(index)),
                // Large files have no editable text to hold a caret; scroll the
                // target line into view instead.
                None => pane.view.scroll.top_line = position.line as usize,
            }
        }
        if caret.is_some() {
            self.reveal_caret_with_context();
        }
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
        let start = picker
            .selected
            .saturating_sub(PICKER_VIEW_WINDOW / 2)
            .min(picker.filtered.len().saturating_sub(PICKER_VIEW_WINDOW));
        let matcher = SkimMatcherV2::default();
        let items = picker
            .filtered
            .iter()
            .skip(start)
            .take(PICKER_VIEW_WINDOW)
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
            has_after: start + PICKER_VIEW_WINDOW < picker.filtered.len(),
            total: picker.filtered.len(),
        })
    }

    pub fn search_view(&self) -> Option<SearchView> {
        let search = self.search.as_ref()?;
        let items = search
            .hits
            .iter()
            .take(500)
            .map(|hit| match hit {
                SearchHit::Buffer { doc, range } => {
                    // Show the matched line's text instead of raw char offsets: a
                    // "foo.rs  120..125" tells the reader nothing about the match.
                    // The file path only earns its space when several buffers are
                    // in scope; for a single-buffer search it is just noise.
                    match self
                        .documents
                        .get(doc)
                        .and_then(|document| document.editable_opt())
                    {
                        Some(editable) => {
                            let (line, _) = crate::position::char_idx_to_line_col(
                                editable.text(),
                                CharIdx(range.start),
                            );
                            let line_text =
                                editable.text().line(line).to_string().trim().to_owned();
                            if search.scope == SearchScope::AllBuffers {
                                format!("{}:{}  {}", self.document_label(*doc), line + 1, line_text)
                            } else {
                                format!("{}  {}", line + 1, line_text)
                            }
                        }
                        None => format!(
                            "{}  {}..{}",
                            self.document_label(*doc),
                            range.start,
                            range.end
                        ),
                    }
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
            total: search.hits.len(),
            field_cursor: search.field_cursor,
            results_scroll: search.results_scroll,
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

    pub fn search_pane_visible(&self) -> bool {
        self.search.is_some()
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
                        // 再読込はエディタ内の編集を経由しないため、LSPサーバーが
                        // 開いている文書なら全文を送り直してテキストを揃える。
                        // これを怠ると以後の差分didChangeが古いテキストに適用され、
                        // セマンティックトークンが恒久的にズレる。
                        if document.lsp.is_opened() {
                            document.editable_mut().record_full_lsp_sync();
                            document.lsp.mark_dirty();
                        }
                        self.status = None;
                        if let Some(path) = document.path.clone() {
                            effects.push(Effect::ComputeGitStatus { doc: id, path });
                        }
                    }
                    self.apply_pending_caret_jump(id);
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
                    self.apply_pending_caret_jump(id);
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
                if let Some(server) = self.server_mut(server) {
                    server.spawned = true;
                    server.error = None;
                }
                self.notify(ToastLevel::Info, format!("{language} LSPを起動しました"));
            }
            LspEvent::Initialized {
                server,
                incremental_sync,
                hover_provider,
                semantic_tokens_legend,
            } => {
                self.finish_progress(&format!("lsp:{server}"));
                if let Some(entry) = self.server_mut(server) {
                    entry.spawned = true;
                    entry.ready = true;
                    entry.hover_capable = hover_provider;
                    entry.incremental_sync = incremental_sync;
                    entry.semantic_legend = semantic_tokens_legend;
                    entry.restart_count = 0;
                    entry.error = (!hover_provider).then(|| "hover is not supported".to_owned());
                }
                effects.push(Effect::LspSend {
                    server,
                    message: serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "initialized",
                        "params": {}
                    })
                    .to_string(),
                });
                if let Some(language) = self
                    .servers
                    .get(&server)
                    .map(|entry| entry.language.clone())
                {
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
                        editable.set_diagnostics(diagnostics);
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
                    add_parentheses,
                }) if self.doc_version(doc) == Some(version)
                    && self
                        .layout
                        .active_editor(self.focus)
                        .is_some_and(|pane| pane.view.doc == doc)
                    && matches!(self.focus, Focus::Editor(_)) =>
                {
                    if let Ok(response) = result.and_then(|value| {
                        serde_json::from_value::<lsp_types::CompletionResponse>(value)
                            .map_err(|error| error.to_string())
                    }) {
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
                            let score = if prefix.is_empty() {
                                0
                            } else {
                                matcher.fuzzy_match(&filter, &prefix)?
                            };
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
                            if callable && add_parentheses && !insert.contains('(') {
                                insert.push_str("()");
                            }
                            // Drop a candidate only when picking it would change
                            // nothing — its insertion equals what's already typed.
                            // A method like `push` completing to `push()` still
                            // edits the buffer, so it stays even beside `push_str`.
                            if !prefix.is_empty() && insert.eq_ignore_ascii_case(&prefix) {
                                return None;
                            }
                            let cursor_back = usize::from(
                                callable && add_parentheses && insert.trim_end().ends_with("()"),
                            );
                            Some((
                                score,
                                CompletionCandidate {
                                    insert,
                                    cursor_back,
                                    label: item.label,
                                    prefix_len: prefix.chars().count(),
                                },
                            ))
                        })
                        .collect();
                        items.sort_by_key(|right| std::cmp::Reverse(right.0));
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
                            self.record_jump_origin();
                            let path =
                                PathBuf::from(location.uri.as_str().trim_start_matches("file://"));
                            if let Some((doc, document)) = self
                                .documents
                                .iter()
                                .find(|(_, document)| document.path.as_ref() == Some(&path))
                            {
                                let mut view = View::new(*doc);
                                if let Some(editable) = document.editable_opt() {
                                    // LSP columns are UTF-16 units, not char indices;
                                    // using the plain converter drifts the caret on
                                    // lines with non-ASCII text before the target.
                                    let index = crate::position::lsp_position_to_char_idx(
                                        editable.text(),
                                        location.range.start.line as usize,
                                        location.range.start.character as usize,
                                    );
                                    view.selections.set_single(Selection::caret(index));
                                }
                                self.layout = Layout::EditorFull(EditorPane { view });
                                // The new view starts scrolled to the top; reveal the
                                // definition with context so its body isn't pushed
                                // just past the bottom edge.
                                self.reveal_caret_with_context();
                            } else {
                                // The file is not open yet, so its text is not loaded.
                                // Remember where to land and apply it once the read
                                // completes, otherwise the caret sits at the top.
                                effects.extend(self.open_path_at(path, location.range.start));
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
                        if hover.is_some()
                            && let Some(lsp) = self.doc_lsp_mut(doc)
                        {
                            lsp.mark_hover_ready();
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
                                let text = editable.text();
                                editable.diagnostics.iter().find(|diagnostic| {
                                    let len = text.len_chars();
                                    text.char_to_line(diagnostic.range.start.0.min(len)) == line
                                })
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
                        // 一度でも hover が返ればサーバーは応答可能。全候補を
                        // 巡回してから ready にすると往復×候補数ぶん待たされる。
                        Ok(Some(_)) => {
                            if let Some(lsp) = self.doc_lsp_mut(doc) {
                                lsp.mark_hover_ready();
                            }
                            self.dirty = true;
                        }
                        Ok(None) => {
                            if let Some(lsp) = self.doc_lsp_mut(doc) {
                                lsp.record_hover_probe_attempt();
                            }
                            effects.push(Effect::ScheduleHoverProbe { doc, delay_ms: 50 });
                        }
                        Err(_) => effects.push(Effect::ScheduleHoverProbe { doc, delay_ms: 500 }),
                    }
                }
                Some(PendingLsp::SemanticTokens { doc, version }) => {
                    if self.doc_version(doc) == Some(version)
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
                self.notify(ToastLevel::Error, message.clone());
                let language = self
                    .servers
                    .get(&server)
                    .map(|entry| entry.language.clone());
                let restart_count = self.server_mut(server).map_or(0, |entry| {
                    entry.mark_down(message);
                    entry.restart_count
                });
                if let Some(language) = language {
                    self.reset_documents_for_server_loss(&language);
                }
                if restart_count < 3 {
                    let delay_ms = 500u64 * (1u64 << restart_count);
                    if let Some(entry) = self.server_mut(server) {
                        entry.restart_count += 1;
                    }
                    effects.push(Effect::ScheduleLspRestart { server, delay_ms });
                }
            }
            LspEvent::RestartDue { server } => {
                if let Some(entry) = self.server_mut(server) {
                    entry.spawned = false;
                    entry.error = None;
                }
                if let Some(language) = self
                    .servers
                    .get(&server)
                    .map(|entry| entry.language.clone())
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
                if self.doc_version(doc) == Some(version) {
                    effects.extend(self.request_semantic_tokens(doc, version));
                }
            }
            LspEvent::CompletionRefreshDue { doc, version } => {
                let active_doc = self
                    .layout
                    .active_editor(self.focus)
                    .map(|pane| pane.view.doc);
                if self.doc_version(doc) == Some(version)
                    && active_doc == Some(doc)
                    && self.completion_suppressed != Some((doc, version))
                    && matches!(self.focus, Focus::Editor(_))
                {
                    effects.extend(self.request_completion(false));
                }
            }
            LspEvent::HoverProbeDue { doc } => {
                if !self.doc_is_hover_ready(doc) {
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
                && let Some(version) = self.doc_version(doc)
            {
                self.completion_suppressed = Some((doc, version));
            }
            self.focus = Focus::Editor(completion.return_side);
            self.dirty = true;
            return Vec::new();
        }
        self.request_completion(true)
    }

    fn current_location(&self) -> Option<(DocumentId, CharIdx)> {
        let pane = self.layout.active_editor(self.focus)?;
        Some((pane.view.doc, pane.view.selections.primary().head))
    }

    /// Remember the caret's current spot before a jump so Ctrl+E can return to it.
    fn record_jump_origin(&mut self) {
        if let Some(location) = self.current_location() {
            if self.nav_back.last() == Some(&location) {
                return;
            }
            self.nav_back.push(location);
            if self.nav_back.len() > 200 {
                self.nav_back.remove(0);
            }
            self.nav_forward.clear();
        }
    }

    /// Ctrl+E / Ctrl+R: step back and forward through visited caret locations.
    fn navigate_history(&mut self, back: bool) {
        let Some(current) = self.current_location() else {
            return;
        };
        let target = if back {
            self.nav_back.pop()
        } else {
            self.nav_forward.pop()
        };
        let Some((doc, head)) = target else {
            self.status = Some(if back {
                "戻る履歴がありません".to_owned()
            } else {
                "進む履歴がありません".to_owned()
            });
            self.dirty = true;
            return;
        };
        if !self.documents.contains_key(&doc) {
            // The document was closed; drop the stale entry and retry.
            return self.navigate_history(back);
        }
        if back {
            self.nav_forward.push(current);
        } else {
            self.nav_back.push(current);
        }
        self.go_to_location(doc, head);
    }

    fn go_to_location(&mut self, doc: DocumentId, head: CharIdx) {
        let clamped = self
            .documents
            .get(&doc)
            .and_then(Document::editable_opt)
            .map_or(head, |editable| {
                CharIdx(head.0.min(editable.text().len_chars()))
            });
        let focus = self.focus;
        if let Some(pane) = self.layout.active_editor_mut(focus) {
            if pane.view.doc != doc {
                pane.view = View::new(doc);
            }
            pane.view.selections.set_single(Selection::caret(clamped));
        }
        self.reveal_caret_with_context();
        self.dirty = true;
    }

    /// Close the completion popup (e.g. when the caret moves away). Restores editor
    /// focus if the popup currently holds it.
    fn dismiss_completion(&mut self) {
        if let Some(completion) = self.completion.take() {
            if matches!(self.focus, Focus::Completion(_)) {
                self.focus = Focus::Editor(completion.return_side);
            }
            self.dirty = true;
        }
    }

    /// Hide the hover popup and cancel any in-flight or deferred hover request.
    /// Opening any other pane or window dismisses hover, since the newest surface
    /// takes priority.
    fn dismiss_hover(&mut self) {
        self.hover = None;
        self.deferred_hover = None;
        self.pending_lsp
            .retain(|_, pending| !matches!(pending, PendingLsp::Hover { .. }));
    }

    fn request_completion(&mut self, manual: bool) -> Vec<Effect> {
        let side = match self.focus {
            Focus::Editor(side) | Focus::Completion(side) => side,
            Focus::Shell | Focus::Overlay => Side::Left,
        };
        let Some((server, path, line, character)) = self.active_lsp_context() else {
            // No LSP completion for this buffer: fall back to the words already
            // present in the file.
            return self.word_completion(manual);
        };
        let Some((doc, version, prefix, anchor)) = self.completion_context() else {
            return Vec::new();
        };
        if !manual && prefix.is_empty() {
            return Vec::new();
        }
        let id = self.next_lsp_request;
        let add_parentheses = self.completion_adds_parentheses(doc, anchor);
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
                add_parentheses,
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

    /// After typing a word character in a buffer without LSP completion, pop up
    /// word-based suggestions. LSP buffers use the debounced didChange path instead.
    fn autocomplete_after_typing(&mut self, character: char) {
        if is_word(character) && self.active_lsp_context().is_none() {
            self.word_completion(false);
        }
    }

    /// Completion fallback for buffers without an LSP: offer the identifiers that
    /// already appear in the file, ranked by frequency.
    fn word_completion(&mut self, manual: bool) -> Vec<Effect> {
        let side = match self.focus {
            Focus::Editor(side) | Focus::Completion(side) => side,
            Focus::Shell | Focus::Overlay => Side::Left,
        };
        let Some((doc, _version, prefix, anchor)) = self.completion_context() else {
            return Vec::new();
        };
        if prefix.is_empty() {
            if manual {
                self.status = Some("補完候補がありません".to_owned());
                self.dirty = true;
            }
            return Vec::new();
        }
        let Some(text) = self
            .documents
            .get(&doc)
            .and_then(Document::editable_opt)
            .map(|editable| editable.text().to_string())
        else {
            return Vec::new();
        };
        let prefix_lower = prefix.to_lowercase();
        let prefix_len = prefix.chars().count();
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for word in text.split(|character| !is_word(character)) {
            if word.chars().count() <= prefix_len {
                continue;
            }
            if word.to_lowercase().starts_with(&prefix_lower) {
                *counts.entry(word).or_insert(0) += 1;
            }
        }
        let mut ranked: Vec<(usize, &str)> = counts
            .into_iter()
            .map(|(word, count)| (count, word))
            .collect();
        ranked.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.len().cmp(&right.1.len()))
                .then_with(|| left.1.cmp(right.1))
        });
        let items: Vec<_> = ranked
            .into_iter()
            .take(50)
            .map(|(_, word)| CompletionCandidate {
                insert: word.to_owned(),
                cursor_back: 0,
                label: word.to_owned(),
                prefix_len,
            })
            .collect();
        if items.is_empty() {
            if manual {
                self.status = Some("補完候補がありません".to_owned());
                self.dirty = true;
            }
            return Vec::new();
        }
        self.completion = Some(CompletionState {
            items,
            selected: 0,
            return_side: side,
            anchor,
        });
        self.focus = Focus::Completion(side);
        self.dirty = true;
        Vec::new()
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
            self.doc_version(pane.view.doc).unwrap_or(1),
            editable.text().slice(start..head).to_string(),
            CharIdx(start),
        ))
    }

    fn completion_adds_parentheses(&self, doc: DocumentId, anchor: CharIdx) -> bool {
        let Some(text) = self
            .documents
            .get(&doc)
            .and_then(Document::editable_opt)
            .map(|editable| editable.text())
        else {
            return true;
        };
        let anchor = anchor.0.min(text.len_chars());
        let line = text.char_to_line(anchor);
        let line_start = text.line_to_char(line);
        let prefix = text.slice(line_start..anchor).to_string();
        let keywords = prefix
            .split(|character: char| !is_word(character))
            .filter(|word| !word.is_empty());
        !keywords
            .into_iter()
            .any(|word| matches!(word, "use" | "import" | "from"))
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
            .is_some_and(|server| self.server_ready(*server) && self.doc_is_opened(doc));
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
        if !self.server_ready(server) || !document.lsp.is_opened() {
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
        let server = self.register_server(language.clone());
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
            let server = self.register_server(language.clone());
            effects.push(Effect::SpawnLsp {
                server,
                language,
                command,
                root: self.workspace_root.clone(),
            });
        }
        effects
    }

    fn doc_version(&self, doc: DocumentId) -> Option<i32> {
        self.documents
            .get(&doc)
            .map(|document| document.lsp.version())
    }

    fn doc_is_opened(&self, doc: DocumentId) -> bool {
        self.documents
            .get(&doc)
            .is_some_and(|document| document.lsp.is_opened())
    }

    fn doc_is_hover_ready(&self, doc: DocumentId) -> bool {
        self.documents
            .get(&doc)
            .is_some_and(|document| document.lsp.is_hover_ready())
    }

    fn doc_lsp_mut(&mut self, doc: DocumentId) -> Option<&mut crate::document::DocumentLsp> {
        self.documents
            .get_mut(&doc)
            .map(|document| &mut document.lsp)
    }

    fn server_id_for_language(&self, language: &str) -> Option<u64> {
        self.lsp_servers.get(language).copied()
    }

    fn server(&self, id: u64) -> Option<&LspServer> {
        self.servers.get(&id)
    }

    /// Allocate a server id for `language` and register it in both the index and
    /// the server table. The two must move together, so nobody does it by hand.
    fn register_server(&mut self, language: String) -> u64 {
        let id = self.next_server_id;
        self.next_server_id += 1;
        self.lsp_servers.insert(language.clone(), id);
        self.servers.insert(id, LspServer::new(language));
        id
    }

    fn server_mut(&mut self, id: u64) -> Option<&mut LspServer> {
        self.servers.get_mut(&id)
    }

    fn server_ready(&self, id: u64) -> bool {
        self.servers.get(&id).is_some_and(|server| server.ready)
    }

    /// Register a language server for tests and hand back its entry so the test
    /// can flip whichever capabilities it needs.
    #[cfg(test)]
    fn test_register_server(&mut self, language: &str, id: u64) -> &mut LspServer {
        self.lsp_servers.insert(language.to_owned(), id);
        self.servers
            .entry(id)
            .or_insert_with(|| LspServer::new(language.to_owned()))
    }

    /// Stand a document up as already opened at `version`, for tests that skip
    /// the real didOpen handshake.
    #[cfg(test)]
    fn test_open_doc(&mut self, doc: DocumentId, version: i32) {
        if let Some(document) = self.documents.get_mut(&doc) {
            document.lsp = crate::document::DocumentLsp::test_opened(version);
        }
    }

    /// Flag `doc` as owing a `didChange` sync to its server. Called from every
    /// edit path so the flush in [`Self::take_lsp_sync_effects`] picks it up.
    fn mark_doc_dirty(&mut self, doc: DocumentId) {
        if let Some(lsp) = self.doc_lsp_mut(doc) {
            lsp.mark_dirty();
        }
    }

    /// The server for `language` died or is restarting: drop every document's
    /// server-derived state so a respawn re-opens them from scratch.
    fn reset_documents_for_server_loss(&mut self, language: &str) {
        for document in self.documents.values_mut() {
            if document.language.as_deref() == Some(language) {
                document.lsp.reset_for_server_loss();
            }
        }
    }

    fn open_lsp_document(&mut self, doc: DocumentId, server: u64) -> Vec<Effect> {
        if !self.server_ready(server) || self.doc_is_opened(doc) {
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
        if let Some(lsp) = self.doc_lsp_mut(doc) {
            lsp.mark_opened();
        }
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
        if self.doc_is_hover_ready(doc)
            || self.pending_lsp.values().any(
                |pending| matches!(pending, PendingLsp::HoverProbe { doc: pending } if *pending == doc),
            )
        {
            return Vec::new();
        }
        let Some((server, path, candidate)) = self.documents.get(&doc).and_then(|document| {
            let language = document.language.as_ref()?;
            let editable = document.editable_opt()?;
            let attempt = document.lsp.hover_probe_attempts();
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
        let hover_capable = self
            .server(server)
            .is_some_and(|entry| entry.ready && entry.hover_capable);
        if !hover_capable || !self.doc_is_opened(doc) {
            return Vec::new();
        }
        let Some((line, character)) = candidate else {
            // 候補を使い切っても hover が一度も返らなかった。サーバーの準備が
            // 遅れているだけの可能性が高いので、間を置いて最初からやり直す。
            if let Some(lsp) = self.doc_lsp_mut(doc) {
                lsp.reset_hover_probe_attempts();
            }
            return vec![Effect::ScheduleHoverProbe { doc, delay_ms: 500 }];
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
        self.request_semantic_tokens_inner(doc, version, false)
    }

    fn request_semantic_tokens_force(&mut self, doc: DocumentId, version: i32) -> Vec<Effect> {
        self.request_semantic_tokens_inner(doc, version, true)
    }

    fn request_semantic_tokens_inner(
        &mut self,
        doc: DocumentId,
        version: i32,
        force: bool,
    ) -> Vec<Effect> {
        if force {
            self.pending_lsp.retain(|_, pending| {
                !matches!(pending, PendingLsp::SemanticTokens { doc: pending_doc, .. } if *pending_doc == doc)
            });
        } else if self.pending_lsp.values().any(|pending| {
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
        if !self.server_ready(server) || !self.doc_is_opened(doc) {
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
        self.mark_doc_dirty(doc);
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
        let legend = self
            .documents
            .get(&doc)
            .and_then(|document| document.language.as_deref())
            .and_then(|language| self.server_id_for_language(language))
            .and_then(|server| self.server(server))
            .and_then(|server| server.semantic_legend.clone());
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
                token_kind: legend
                    .as_ref()
                    .and_then(|legend| legend.token_types.get(token.token_type as usize))
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_owned()),
                token_modifiers: legend.as_ref().map_or_else(Vec::new, |legend| {
                    legend
                        .token_modifiers
                        .iter()
                        .enumerate()
                        .filter_map(|(index, modifier)| {
                            let bit = 1u32.checked_shl(index as u32)?;
                            if token.token_modifiers_bitset & bit != 0 {
                                Some(modifier.clone())
                            } else {
                                None
                            }
                        })
                        .collect()
                }),
            });
        }
        document.editable_mut().semantic_spans = spans;
        if let Some(lsp) = self.doc_lsp_mut(doc) {
            lsp.set_semantic_ready(version);
        }
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
            Command::DeleteBackward => {
                let (tab_size, insert_spaces) = self.active_indentation_settings();
                self.edit_active(|document, view| {
                    document.editable_mut().delete_backward_smart(
                        &mut view.selections,
                        tab_size,
                        insert_spaces,
                    );
                });
            }
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
            Command::SearchCursorLeft => self.move_search_cursor(false),
            Command::SearchCursorRight => self.move_search_cursor(true),
            Command::PickerUp => self.move_picker(-1),
            Command::PickerDown => self.move_picker(1),
            Command::PickerBackspace => {
                if let Some(rename) = &mut self.rename_input {
                    rename.pop();
                    self.dirty = true;
                } else if self.search.is_some() {
                    self.backspace_search_char();
                    return self.refresh_search();
                } else if let Some(picker) = &mut self.picker {
                    picker.query.pop();
                    self.refresh_picker();
                }
            }
            Command::PickerConfirm => return self.confirm_picker(),
            Command::PickerCancel => self.close_picker(),
            Command::Cancel => {
                if self.search.is_none() {
                    self.close_picker();
                }
            }
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
                    search.field_cursor = search_field_len(search);
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
                    // Clear the hover/diagnostic popup and completion so they don't
                    // linger beside or under the modal rename prompt.
                    self.hover = None;
                    self.deferred_hover = None;
                    self.completion = None;
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
            Command::NavigateBack => self.navigate_history(true),
            Command::NavigateForward => self.navigate_history(false),
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
                self.dismiss_hover();
                self.dirty = true;
                if self.terminal.is_some() {
                    return Vec::new();
                }
                self.terminal = Some(vt100::Parser::new(
                    self.terminal_size.1.saturating_sub(1),
                    split_right_width(self.terminal_size.0),
                    TERMINAL_SCROLLBACK_LINES,
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
        // Removing the document drops its DocumentLsp with it, so the per-document
        // LSP state (version, opened, semantic/hover readiness) needs no separate
        // cleanup here — that inseparability is the point of storing it inline.
        self.documents.remove(&id);
        if self.deferred_hover.is_some_and(|(doc, _)| doc == id) {
            self.deferred_hover = None;
        }
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

    fn insert_typed_character(&mut self, character: char, at: Option<Instant>) {
        match character {
            ')' | ']' => self.edit_active(|document, view| {
                if !document
                    .editable_mut()
                    .skip_closing_character(&mut view.selections, character)
                {
                    match at {
                        Some(at) => document.editable_mut().insert_timed(
                            &mut view.selections,
                            &character.to_string(),
                            at,
                        ),
                        None => document
                            .editable_mut()
                            .insert(&mut view.selections, &character.to_string()),
                    }
                }
            }),
            '(' | '[' | '{' | '\'' | '"' | '`' => {
                let closing = match character {
                    '(' => ')',
                    '[' => ']',
                    '{' => '}',
                    quote => quote,
                };
                self.edit_active(|document, view| {
                    if !document
                        .editable_mut()
                        .skip_closing_character(&mut view.selections, closing)
                    {
                        document.editable_mut().insert_pair(
                            &mut view.selections,
                            character,
                            closing,
                            at,
                        );
                    }
                });
            }
            '}' => self.edit_active(|document, view| {
                if !document
                    .editable_mut()
                    .skip_closing_character(&mut view.selections, '}')
                {
                    document
                        .editable_mut()
                        .insert_closing_brace(&mut view.selections, at);
                }
            }),
            _ => self.edit_active(|document, view| match at {
                Some(at) => document.editable_mut().insert_timed(
                    &mut view.selections,
                    &character.to_string(),
                    at,
                ),
                None => document
                    .editable_mut()
                    .insert(&mut view.selections, &character.to_string()),
            }),
        }
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
        let doc = pane.view.doc;
        let Some((path, contents, expected, language)) =
            self.documents.get(&doc).and_then(|document| {
                let path = match document.path.clone() {
                    Some(path) => path,
                    None => {
                        self.status = Some("無名バッファは保存できません".to_owned());
                        self.dirty = true;
                        return None;
                    }
                };
                let Some(editable) = document.editable_opt() else {
                    self.status = Some("大容量ファイルは読み取り専用です".to_owned());
                    self.dirty = true;
                    return None;
                };
                Some((
                    path,
                    editable.contents_for_save(),
                    document.disk_state,
                    document.language.clone(),
                ))
            })
        else {
            return Vec::new();
        };
        let version = self.doc_version(doc).unwrap_or(1);
        let mut effects = Vec::new();
        if let Some(language) = language
            && let Some(document) = self.documents.get_mut(&doc)
            && let Some(editable) = document.editable_opt_mut()
        {
            editable.refresh_highlight(&language);
            self.dirty = true;
        }
        effects.extend(self.request_semantic_tokens_force(doc, version));
        effects.push(Effect::WriteFile {
            doc,
            path,
            contents,
            expected,
        });
        effects
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
                        self.dismiss_hover();
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
        if mode == PickerMode::Buffer
            && self
                .picker
                .as_ref()
                .is_some_and(|picker| picker.mode == PickerMode::Buffer)
        {
            self.close_picker();
            return;
        }
        if self.picker.is_some() {
            self.close_picker();
        }
        self.dismiss_hover();
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
        self.dismiss_search_for_overlay();
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
            scan_token: None,
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
    }

    fn open_command_palette(&mut self) {
        if self.picker.is_some() {
            self.close_picker();
        }
        self.dismiss_hover();
        let base = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
            .unwrap_or(DocumentId(0));
        let candidates: Vec<_> = (0..COMMAND_PALETTE.len())
            .map(PickerCandidate::Command)
            .collect();
        self.dismiss_search_for_overlay();
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
            scan_token: None,
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
    }

    fn open_directory_picker(&mut self) -> Vec<Effect> {
        if self
            .picker
            .as_ref()
            .is_some_and(|picker| picker.mode == PickerMode::Directory)
        {
            self.close_picker();
            return Vec::new();
        }
        if self.picker.is_some() {
            self.close_picker();
        }
        self.dismiss_hover();
        let base = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.doc)
            .unwrap_or(DocumentId(0));
        let token = self.next_scan_token;
        self.next_scan_token += 1;
        self.dismiss_search_for_overlay();
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
            scan_token: Some(token),
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

    fn dismiss_search_for_overlay(&mut self) {
        if self.search.take().is_some() {
            self.finish_progress("grep");
        }
    }

    fn apply_file_scan(&mut self, event: FileScanEvent) {
        match event {
            FileScanEvent::Batch { token, paths } => {
                if let Some(picker) = &mut self.picker
                    && picker.mode == PickerMode::Directory
                    && picker.scan_token == Some(token)
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
            FileScanEvent::Done { token } => {
                if let Some(picker) = &mut self.picker
                    && picker.scan_token == Some(token)
                {
                    picker.scan_token = None;
                    self.status = None;
                    self.finish_progress("file-scan");
                }
            }
            FileScanEvent::Failed { token, error } => {
                if let Some(picker) = &mut self.picker
                    && picker.scan_token == Some(token)
                {
                    picker.scan_token = None;
                    self.status = Some(error);
                    self.finish_progress("file-scan");
                }
            }
        }
        self.dirty = true;
    }

    fn open_search(&mut self, replace: bool, scope: SearchScope) -> Vec<Effect> {
        if self.search.is_some() {
            // Ctrl+F only toggles the pane's visibility; scope and options are
            // adjusted with the mouse inside the pane.
            return self.close_search_pane();
        }
        // Replace is opt-in via the checkbox; a fresh pane starts as find-only.
        let _ = replace;
        self.picker = None;
        self.dismiss_hover();
        self.search = Some(SearchState {
            query: String::new(),
            replacement: None,
            editing_replace: false,
            editing_filter: None,
            scope,
            options: SearchOptions::default(),
            include_input: String::new(),
            exclude_input: String::new(),
            filters: SearchFilters {
                include: Vec::new(),
                exclude: Vec::new(),
                exclude_dirs: self.config.search.exclude.clone(),
                respect_ignore_files: self.config.search.respect_ignore_files,
                include_hidden: self.config.search.include_hidden,
            },
            hits: Vec::new(),
            current: 0,
            grep_token: None,
            field_cursor: 0,
            results_scroll: 0,
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
        Vec::new()
    }

    fn toggle_replace_field(&mut self) {
        if let Some(search) = &mut self.search {
            if search.replacement.is_some() {
                search.replacement = None;
                search.editing_replace = false;
            } else {
                search.replacement = Some(String::new());
                search.editing_filter = None;
                search.editing_replace = true;
                search.field_cursor = 0;
            }
            self.dirty = true;
        }
    }

    /// Jump to and open the file for the given result index, closing the pane.
    fn open_search_hit(&mut self, index: usize) -> Vec<Effect> {
        let Some(hit) = self
            .search
            .as_ref()
            .and_then(|search| search.hits.get(index).cloned())
        else {
            return Vec::new();
        };
        self.record_jump_origin();
        self.focus = Focus::Editor(Side::Left);
        let effects = match hit {
            SearchHit::Buffer { doc, range } => {
                let mut view = View::new(doc);
                view.selections.set_single(Selection {
                    anchor: CharIdx(range.start),
                    head: CharIdx(range.end),
                });
                self.layout = Layout::EditorFull(EditorPane { view });
                // The fresh view is scrolled to the top; reveal the match with
                // surrounding context rather than pinning it to the bottom edge.
                self.reveal_caret_with_context();
                Vec::new()
            }
            SearchHit::Disk(hit) => {
                if let Some((doc, _)) = self.documents.iter().find(|(_, document)| {
                    document.path.as_ref() == Some(&hit.path) && document.large().is_some()
                }) {
                    let mut view = View::new(*doc);
                    view.scroll.top_line = hit.line;
                    self.layout = Layout::EditorFull(EditorPane { view });
                    Vec::new()
                } else {
                    // Not open yet: land on the matched line once it loads instead
                    // of opening at the top of the file.
                    self.open_path_at(hit.path, lsp_types::Position::new(hit.line as u32, 0))
                }
            }
        };
        self.search = None;
        self.dirty = true;
        effects
    }

    /// The "Run Replace" button: replace every match in one shot.
    fn run_replace(&mut self) -> Vec<Effect> {
        let Some(search) = self.search.take() else {
            return Vec::new();
        };
        let Some(replacement) = search.replacement else {
            return Vec::new();
        };
        let Ok(pattern) = search_pattern(&search.query, search.options) else {
            self.status = Some("検索式が不正です".to_owned());
            self.focus = Focus::Editor(Side::Left);
            self.dirty = true;
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
                    let matched = editable.text().slice(selection.range()).to_string();
                    pattern.replace(&matched, replacement.as_str()).into_owned()
                })
                .collect();
            let mut selections = crate::view::Selections::from_vec(selections, 0);
            document
                .editable_mut()
                .insert_fragments(&mut selections, &fragments);
            self.mark_doc_dirty(id);
        }
        self.status = Some("置換を適用しました".to_owned());
        self.focus = Focus::Editor(Side::Left);
        self.dirty = true;
        Vec::new()
    }

    fn close_search_pane(&mut self) -> Vec<Effect> {
        if self.search.take().is_some() {
            self.finish_progress("grep");
            self.focus = Focus::Editor(self.focused_side());
            self.dirty = true;
        }
        Vec::new()
    }

    fn set_search_scope(&mut self, scope: SearchScope) -> Vec<Effect> {
        if let Some(search) = &mut self.search {
            if search.scope == scope {
                return Vec::new();
            }
            search.scope = scope;
        }
        self.refresh_search()
    }

    /// The right-half rectangle occupied by the search pane: `(x, y, width, height)`.
    fn search_pane_rect(&self) -> (u16, u16, u16, u16) {
        let (cols, rows) = self.terminal_size;
        let content_height = rows.saturating_sub(1);
        let x = split_left_width(cols).saturating_add(1);
        (x, 0, cols.saturating_sub(x), content_height)
    }

    /// Handle a left-click while the search pane is open. Returns `Some` when the
    /// click landed inside the pane (and was consumed), `None` otherwise.
    fn search_pane_click(&mut self, column: u16, row: u16) -> Option<Vec<Effect>> {
        let (pane_x, pane_y, pane_width, pane_height) = self.search_pane_rect();
        if column < pane_x
            || column >= pane_x + pane_width
            || row < pane_y
            || row >= pane_y + pane_height
        {
            return None;
        }
        let search = self.search.as_ref()?;
        let directory = search.scope == SearchScope::Directory;
        let replace_enabled = search.replacement.is_some();
        let layout = search_pane_layout(directory, replace_enabled);
        let relative = row - pane_y;
        let inner_x = pane_x + 1;

        if relative == layout.scope_row {
            let scopes = [
                SearchScope::CurrentBuffer,
                SearchScope::AllBuffers,
                SearchScope::Directory,
            ];
            for (index, (start, end)) in search_scope_tab_ranges(inner_x).into_iter().enumerate() {
                if column >= start && column < end {
                    return Some(self.set_search_scope(scopes[index]));
                }
            }
            return Some(Vec::new());
        }
        if relative == layout.toggle_row {
            for (index, (start, end)) in search_toggle_click_ranges(inner_x).into_iter().enumerate()
            {
                if column >= start && column < end {
                    return Some(self.toggle_search_option(move |options| match index {
                        0 => options.case_sensitive = !options.case_sensitive,
                        1 => options.whole_word = !options.whole_word,
                        _ => options.regex = !options.regex,
                    }));
                }
            }
            return Some(Vec::new());
        }
        if in_box(relative, layout.find_top) {
            self.focus_search_field(None, false);
            return Some(Vec::new());
        }
        if relative == layout.replace_checkbox_row {
            // The run button lives to the right of a ticked checkbox on this row.
            let (button_start, button_end) = search_run_button_range(inner_x);
            if replace_enabled && column >= button_start && column < button_end {
                return Some(self.run_replace());
            }
            self.toggle_replace_field();
            return Some(Vec::new());
        }
        if let Some(top) = layout.replace_top
            && in_box(relative, top)
        {
            self.focus_search_field(None, true);
            return Some(Vec::new());
        }
        if let Some(top) = layout.include_top
            && in_box(relative, top)
        {
            self.focus_search_field(Some(SearchFilterField::Include), false);
            return Some(Vec::new());
        }
        if let Some(top) = layout.exclude_top
            && in_box(relative, top)
        {
            self.focus_search_field(Some(SearchFilterField::Exclude), false);
            return Some(Vec::new());
        }
        if relative > layout.results_top {
            let index = usize::from(relative - layout.results_top - 1)
                + self
                    .search
                    .as_ref()
                    .map_or(0, |search| search.results_scroll);
            return Some(self.open_search_hit(index));
        }
        Some(Vec::new())
    }

    fn focus_search_field(&mut self, filter: Option<SearchFilterField>, replace: bool) {
        if let Some(search) = &mut self.search {
            search.editing_filter = filter;
            search.editing_replace = replace && search.replacement.is_some();
            search.field_cursor = search_field_len(search);
            self.dirty = true;
        }
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

    fn active_search_field_mut(&mut self) -> Option<&mut String> {
        let search = self.search.as_mut()?;
        match search.editing_filter {
            Some(SearchFilterField::Include) => Some(&mut search.include_input),
            Some(SearchFilterField::Exclude) => Some(&mut search.exclude_input),
            None if search.editing_replace => search.replacement.as_mut(),
            None => Some(&mut search.query),
        }
    }

    fn insert_search_char(&mut self, character: char) {
        let cursor = self.search.as_ref().map_or(0, |search| search.field_cursor);
        let Some(field) = self.active_search_field_mut() else {
            return;
        };
        let byte = char_byte_index(field, cursor);
        field.insert(byte, character);
        if let Some(search) = &mut self.search {
            search.field_cursor = cursor + 1;
        }
    }

    fn backspace_search_char(&mut self) {
        let cursor = self.search.as_ref().map_or(0, |search| search.field_cursor);
        if cursor == 0 {
            return;
        }
        if let Some(field) = self.active_search_field_mut() {
            let end = char_byte_index(field, cursor);
            let start = char_byte_index(field, cursor - 1);
            field.replace_range(start..end, "");
        }
        if let Some(search) = &mut self.search {
            search.field_cursor = cursor - 1;
        }
    }

    fn move_search_cursor(&mut self, right: bool) {
        if let Some(search) = &mut self.search {
            let len = search_field_len(search);
            search.field_cursor = if right {
                (search.field_cursor + 1).min(len)
            } else {
                search.field_cursor.saturating_sub(1)
            };
            self.dirty = true;
        }
    }

    fn scroll_search_results(&mut self, delta: isize) {
        if let Some(search) = &mut self.search {
            let max = search.hits.len().saturating_sub(1);
            search.results_scroll = if delta < 0 {
                search.results_scroll.saturating_sub(delta.unsigned_abs())
            } else {
                (search.results_scroll + delta as usize).min(max)
            };
            self.dirty = true;
        }
    }

    fn overlay_input(&mut self, character: char) -> Vec<Effect> {
        if let Some(rename) = &mut self.rename_input {
            rename.push(character);
            self.dirty = true;
            return Vec::new();
        }
        if self.search.is_some() {
            self.insert_search_char(character);
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
        if self.search.is_some() {
            // Results are browsed with the mouse wheel, not the keyboard.
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
            // The search pane is entirely mouse-driven: results open on click and
            // replacement runs from the button, so Enter does nothing here.
            return Vec::new();
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

    fn picker_contains(&self, column: u16, row: u16) -> bool {
        let Some(picker) = self.picker_view() else {
            return false;
        };
        let viewport_width = self.terminal_size.0;
        let viewport_height = self.terminal_size.1.saturating_sub(1);
        let width = viewport_width.saturating_sub(4).clamp(1, 70);
        let available_height = viewport_height.saturating_sub(1).max(1);
        let ellipsis_rows = u16::from(picker.has_before) + u16::from(picker.has_after);
        let height = (picker.items.len() as u16 + 3 + ellipsis_rows)
            .min(available_height)
            .max(1);
        let x = viewport_width.saturating_sub(width) / 2;
        let y = 1;
        column >= x && column < x.saturating_add(width) && row >= y && row < y + height
    }

    fn close_picker(&mut self) {
        let closing_directory = self
            .picker
            .as_ref()
            .is_some_and(|picker| picker.mode == PickerMode::Directory);
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
        if closing_directory {
            self.finish_progress("file-scan");
            if self.status.as_deref() == Some("ファイルを走査中…") {
                self.status = None;
            }
        }
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
        let Some(server_id) = self.server_id_for_language(language) else {
            return format!("<lsp> {language}: starting");
        };
        let Some(server) = self.server(server_id) else {
            return format!("<lsp> {language}: starting");
        };
        if let Some(error) = &server.error {
            let state = if error.to_ascii_lowercase().contains("not found") {
                "not found"
            } else {
                "error"
            };
            return format!("<lsp> {language}: {state}");
        }
        if !server.spawned {
            return format!("<lsp> {language}: starting");
        }
        let progress_prefix = format!("lsp:{server_id}:");
        let progress = self
            .progress
            .iter()
            .find_map(|(key, message)| key.starts_with(&progress_prefix).then_some(message));
        if !server.ready {
            return progress.map_or_else(
                || format!("<lsp> {language}: initializing"),
                |message| format!("<lsp> {language}: initializing ({message})"),
            );
        }
        if let Some(message) = progress {
            return format!("<lsp> {language}: updating ({message})");
        }
        let Some(lsp) = self.documents.get(&doc).map(|document| &document.lsp) else {
            return format!("<lsp> {language}: opening");
        };
        if !lsp.is_opened() {
            return format!("<lsp> {language}: opening");
        }
        match lsp.semantic_ready_version() {
            None => return format!("<lsp> {language}: coloring"),
            Some(version) if version < lsp.version() => {
                return format!("<lsp> {language}: updating");
            }
            Some(_) => {}
        }
        if !lsp.is_hover_ready() {
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
        let pane_width = if self.search.is_some() {
            // The search pane occupies the right half; the editor is on the left.
            split_left_width(self.terminal_size.0)
        } else {
            match self.layout {
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

    fn scroll_terminal(&mut self, amount: isize) {
        let Some(parser) = &mut self.terminal else {
            return;
        };
        let current = parser.screen().scrollback();
        let target = if amount < 0 {
            current.saturating_sub(amount.unsigned_abs())
        } else {
            current.saturating_add(amount as usize)
        };
        parser.set_scrollback(target);
        self.terminal_selection = None;
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
        let doc = pane.view.doc;
        edit(document, &mut pane.view);
        self.completion_suppressed = None;
        self.mark_doc_dirty(doc);
        self.ensure_cursor_visible();
        self.dirty = true;
    }

    fn take_lsp_sync_effects(&mut self) -> Vec<Effect> {
        let pending: Vec<_> = self
            .documents
            .iter_mut()
            .filter_map(|(id, document)| document.lsp.take_needs_sync().then_some(*id))
            .collect();
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
            if !self.server_ready(server) || !self.doc_is_opened(id) {
                continue;
            }
            let content_changes = if self
                .server(server)
                .is_some_and(|entry| entry.incremental_sync)
            {
                serde_json::to_value(changes).unwrap_or_else(|_| serde_json::json!([]))
            } else {
                serde_json::json!([{"text": text}])
            };
            let version = self
                .doc_lsp_mut(id)
                .map_or(1, crate::document::DocumentLsp::bump_version);
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
        self.dismiss_completion();
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

    /// Reveal the caret with room above and below, for jumps (definition,
    /// navigation, search hit) where the content you jumped to — a definition
    /// body, the lines around a match — sits *below* the caret. Plain
    /// [`Self::ensure_cursor_visible`] only guarantees the caret line itself, so on
    /// a downward jump it pins that line to the bottom edge and leaves the body
    /// off screen. This first parks the caret about a third of the way down, then
    /// defers to `ensure_cursor_visible` to clamp and finalise the wrapped offset.
    fn reveal_caret_with_context(&mut self) {
        if self.terminal_size.1 != 0 {
            let rows = usize::from(self.terminal_size.1.saturating_sub(1)).max(1);
            let focus = self.focus;
            let (documents, layout) = (&self.documents, &mut self.layout);
            if let Some(pane) = layout.active_editor_mut(focus)
                && let Some(editable) = documents
                    .get(&pane.view.doc)
                    .and_then(Document::editable_opt)
            {
                let head = pane
                    .view
                    .selections
                    .primary()
                    .head
                    .0
                    .min(editable.text().len_chars());
                let line = editable.text().char_to_line(head);
                pane.view.scroll.top_line = line.saturating_sub(rows / 3);
                pane.view.scroll.wrapped_row_offset = 0;
            }
        }
        self.ensure_cursor_visible();
    }

    fn ensure_cursor_visible(&mut self) {
        if self.terminal_size.0 == 0 || self.terminal_size.1 == 0 {
            return;
        }
        let rows = usize::from(self.terminal_size.1.saturating_sub(1)).max(1);
        let pane_cols = if self.search.is_some() {
            usize::from(split_left_width(self.terminal_size.0))
        } else {
            match self.layout {
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
    pub diagnostics: &'a [crate::document::ActiveDiagnostic],
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
        name: "Find & Replace / 検索・置換",
        description: "右ペインを開く。再押下で現在→全バッファ→ディレクトリと範囲切替",
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
        key: "Ctrl+E",
        name: "Go Back / 戻る",
        description: "直前のカーソル位置へ戻る",
        command: Command::NavigateBack,
    },
    CommandPaletteEntry {
        key: "Ctrl+R",
        name: "Go Forward / 進む",
        description: "戻る前のカーソル位置へ進む",
        command: Command::NavigateForward,
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
    scan_token: Option<u64>,
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
    field_cursor: usize,
    results_scroll: usize,
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
    /// Directory names pruned from the walk regardless of the exclude field.
    pub exclude_dirs: Vec<String>,
    pub respect_ignore_files: bool,
    pub include_hidden: bool,
}

fn char_byte_index(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

fn search_field_len(search: &SearchState) -> usize {
    match search.editing_filter {
        Some(SearchFilterField::Include) => search.include_input.chars().count(),
        Some(SearchFilterField::Exclude) => search.exclude_input.chars().count(),
        None if search.editing_replace => {
            search.replacement.as_deref().unwrap_or("").chars().count()
        }
        None => search.query.chars().count(),
    }
}

/// Split an include/exclude field into `-name` patterns. Patterns are separated by
/// whitespace, so `*.rs *.md` matches either extension.
fn split_globs(value: &str) -> Vec<String> {
    value.split_whitespace().map(str::to_owned).collect()
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

// Geometry of the search pane, shared between rendering and mouse hit-testing so
// clicks land on the same controls that are drawn. Rows are relative to the pane
// top; input boxes are three rows tall (border / text / border). The replace box
// and directory filters appear conditionally, so row positions are computed rather
// than fixed.
pub(crate) const SEARCH_SCOPE_LABELS: [&str; 3] = [" file ", " buffers ", " dir "];
pub(crate) const SEARCH_TOGGLE_LABELS: [&str; 3] = ["[Aa]", "[W]", "[.*]"];
/// The checkbox + label drawn at the start of the replace row.
pub(crate) const SEARCH_REPLACE_CHECKBOX: &str = "[ ] Replace";
/// The "Run Replace" button drawn to the right of a ticked checkbox.
pub(crate) const SEARCH_RUN_BUTTON: &str = "[ Run Replace ]";

#[derive(Clone, Copy, Debug)]
pub(crate) struct SearchPaneLayout {
    pub scope_row: u16,
    pub toggle_row: u16,
    pub find_top: u16,
    pub replace_checkbox_row: u16,
    pub replace_top: Option<u16>,
    pub include_top: Option<u16>,
    pub exclude_top: Option<u16>,
    pub results_top: u16,
}

/// Column range of the "Run Replace" button on the checkbox row (a gap after the
/// checkbox label).
pub(crate) fn search_run_button_range(inner_x: u16) -> (u16, u16) {
    let start = inner_x + SEARCH_REPLACE_CHECKBOX.chars().count() as u16 + 2;
    (start, start + SEARCH_RUN_BUTTON.chars().count() as u16)
}

pub(crate) fn search_pane_layout(directory: bool, replace_enabled: bool) -> SearchPaneLayout {
    let find_top = 2;
    let replace_checkbox_row = find_top + 3;
    let mut next = replace_checkbox_row + 1;
    let replace_top = if replace_enabled {
        let top = next;
        next += 3; // 3-row box; the run button shares the checkbox row
        Some(top)
    } else {
        None
    };
    let (include_top, exclude_top) = if directory {
        let include = next;
        next += 6; // two 3-row boxes
        (Some(include), Some(include + 3))
    } else {
        (None, None)
    };
    SearchPaneLayout {
        scope_row: 0,
        toggle_row: 1,
        find_top,
        replace_checkbox_row,
        replace_top,
        include_top,
        exclude_top,
        results_top: next,
    }
}

/// Whether a pane-relative row falls inside a three-row bordered input box.
pub(crate) fn in_box(relative: u16, top: u16) -> bool {
    relative >= top && relative < top + 3
}

pub(crate) fn search_scope_tab_ranges(inner_x: u16) -> [(u16, u16); 3] {
    let mut x = inner_x;
    let mut ranges = [(0, 0); 3];
    for (index, label) in SEARCH_SCOPE_LABELS.iter().enumerate() {
        let width = label.chars().count() as u16;
        ranges[index] = (x, x + width);
        x += width;
    }
    ranges
}

/// Rendered x-position of each toggle label (contiguous label, then two spaces).
pub(crate) fn search_toggle_label_starts(inner_x: u16) -> [u16; 3] {
    let mut x = inner_x;
    let mut starts = [0; 3];
    for (index, label) in SEARCH_TOGGLE_LABELS.iter().enumerate() {
        starts[index] = x;
        x += label.chars().count() as u16 + 2;
    }
    starts
}

/// Clickable ranges for the toggles, tiled so a click in the gap between two
/// toggles selects the nearer one.
pub(crate) fn search_toggle_click_ranges(inner_x: u16) -> [(u16, u16); 3] {
    let starts = search_toggle_label_starts(inner_x);
    let widths: [u16; 3] = [
        SEARCH_TOGGLE_LABELS[0].chars().count() as u16,
        SEARCH_TOGGLE_LABELS[1].chars().count() as u16,
        SEARCH_TOGGLE_LABELS[2].chars().count() as u16,
    ];
    // Boundaries sit at the midpoint of each gap between adjacent labels.
    let split0 = (starts[0] + widths[0] + starts[1]) / 2;
    let split1 = (starts[1] + widths[1] + starts[2]) / 2;
    let end = starts[2] + widths[2];
    [(inner_x, split0), (split0, split1), (split1, end)]
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
    pub total: usize,
    pub field_cursor: usize,
    pub results_scroll: usize,
}

#[derive(Debug)]
enum PendingLsp {
    Completion {
        doc: DocumentId,
        version: i32,
        prefix: String,
        side: Side,
        anchor: CharIdx,
        add_parentheses: bool,
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
    fn word_completion_offers_file_words_without_an_lsp() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.update(AppEvent::TextPaste("hello helper\nhel".to_owned()));

        editor.update(Command::ToggleCompletion.into());

        let view = editor.completion_view().expect("completion popup");
        assert!(
            view.items.iter().any(|item| item == "hello"),
            "{:?}",
            view.items
        );
        assert!(
            view.items.iter().any(|item| item == "helper"),
            "{:?}",
            view.items
        );
        // The word being typed is not offered as its own completion.
        assert!(!view.items.iter().any(|item| item == "hel"));
    }

    #[test]
    fn typing_a_word_character_pops_word_completion_without_lsp() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.update(AppEvent::TextPaste("hello\n".to_owned()));

        editor.update(AppEvent::TextInputAt {
            character: 'h',
            at: std::time::Instant::now(),
        });

        let view = editor.completion_view().expect("completion popup");
        assert!(
            view.items.iter().any(|item| item == "hello"),
            "{:?}",
            view.items
        );
    }

    #[test]
    fn moving_the_caret_dismisses_the_completion_popup() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.update(AppEvent::TextPaste("hello\nhel".to_owned()));
        editor.update(Command::ToggleCompletion.into());
        assert!(editor.completion_view().is_some());

        editor.update(
            Command::Move {
                direction: Direction::Left,
                unit: Unit::Character,
                extend: false,
            }
            .into(),
        );

        assert!(editor.completion_view().is_none());
    }

    #[test]
    fn navigation_history_returns_to_the_pre_click_caret() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 40, rows: 10 });
        editor.update(AppEvent::TextPaste("abcdefghij".to_owned()));
        assert_eq!(
            editor.current_location(),
            Some((DocumentId(0), CharIdx(10)))
        );

        // Click into the middle of the line (gutter is 5 wide, so column 8 is
        // display column 3).
        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 8,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));
        assert_eq!(editor.current_location(), Some((DocumentId(0), CharIdx(3))));

        editor.update(Command::NavigateBack.into());
        assert_eq!(
            editor.current_location(),
            Some((DocumentId(0), CharIdx(10)))
        );

        editor.update(Command::NavigateForward.into());
        assert_eq!(editor.current_location(), Some((DocumentId(0), CharIdx(3))));
    }

    #[test]
    fn replace_is_off_until_the_checkbox_is_ticked() {
        let mut editor = Editor::default();
        editor.update(Command::OpenReplace.into());
        assert!(editor.search_view().unwrap().replacement.is_none());

        editor.toggle_replace_field();
        assert!(editor.search_view().unwrap().replacement.is_some());

        editor.toggle_replace_field();
        assert!(editor.search_view().unwrap().replacement.is_none());
    }

    #[test]
    fn clicking_the_gap_between_toggles_flips_the_nearer_one() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 40, rows: 24 });
        editor.update(Command::OpenReplace.into());
        assert!(!editor.search.as_ref().unwrap().options.case_sensitive);

        // Column 26 is the blank just after the "[Aa]" label; the nearer toggle
        // is still case-sensitivity.
        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 26,
                row: 1,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));

        assert!(editor.search.as_ref().unwrap().options.case_sensitive);
    }

    #[test]
    fn clicking_a_result_opens_it_and_closes_the_pane() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 40, rows: 24 });
        editor.update(AppEvent::TextPaste("foo foo".to_owned()));
        editor.update(Command::OpenReplace.into());
        for character in "foo".chars() {
            editor.update(AppEvent::TextInput(character));
        }
        assert_eq!(editor.search_view().unwrap().total, 2);

        // Results start at row 7 (find box + replace checkbox, then the list border).
        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 25,
                row: 7,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));

        assert!(editor.search_view().is_none());
        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .head,
            CharIdx(3)
        );
    }

    #[test]
    fn clicking_the_run_button_replaces_every_match() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 40, rows: 24 });
        editor.update(AppEvent::TextPaste("one two one".to_owned()));
        editor.update(Command::OpenReplace.into());
        for character in "one".chars() {
            editor.update(AppEvent::TextInput(character));
        }
        editor.toggle_replace_field();
        editor.update(AppEvent::TextInput('X'));

        // The run button sits on the checkbox row (row 5), right of "[x] Replace".
        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 36,
                row: 5,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));

        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "X two X");
    }

    #[test]
    fn glob_fields_split_into_multiple_patterns_on_whitespace() {
        assert_eq!(
            split_globs("*.rs *.md"),
            vec!["*.rs".to_owned(), "*.md".to_owned()]
        );
        assert_eq!(
            split_globs("   *.rs    *.md   "),
            vec!["*.rs".to_owned(), "*.md".to_owned()]
        );
        assert!(split_globs("").is_empty());
    }

    #[test]
    fn exclude_field_is_empty_with_default_directories_pruned_behind_the_scenes() {
        let mut editor = Editor::default();
        editor.update(Command::OpenReplace.into());

        assert_eq!(editor.search_view().unwrap().exclude, "");
        assert!(
            editor
                .search
                .as_ref()
                .unwrap()
                .filters
                .exclude_dirs
                .contains(&".git".to_owned())
        );
    }

    #[test]
    fn search_field_supports_horizontal_cursor_editing() {
        let mut editor = Editor::default();
        editor.update(Command::OpenReplace.into());
        for character in "abc".chars() {
            editor.update(AppEvent::TextInput(character));
        }
        editor.update(Command::SearchCursorLeft.into());
        editor.update(AppEvent::TextInput('X'));

        assert_eq!(editor.search_view().unwrap().query, "abXc");
    }

    #[test]
    fn clicking_a_scope_tab_switches_the_search_scope() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 40, rows: 24 });
        editor.update(Command::OpenReplace.into());
        assert_eq!(
            editor.search.as_ref().unwrap().scope,
            SearchScope::CurrentBuffer
        );

        // The pane starts at column split_left_width(40)+1 = 21, so its inner
        // content begins at 22 and the " dir " tab spans columns 37..42.
        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 38,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));

        assert_eq!(
            editor.search.as_ref().unwrap().scope,
            SearchScope::Directory
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
        editor.test_register_server("rust", 1).ready = true;
        editor.test_open_doc(DocumentId(0), 1);
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
        editor.test_register_server("rust", 1);

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
            semantic_tokens_legend: None,
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
                token_kind: "function".to_owned(),
                token_modifiers: Vec::new(),
            });
        let server = editor.test_register_server("rust", 1);
        server.ready = true;
        server.incremental_sync = true;
        editor.test_open_doc(DocumentId(0), 1);

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
    fn semantic_tokens_use_server_legend_names_instead_of_numeric_slots() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("/tmp/legend.rs"));
        document.language = Some("rust".to_owned());
        document.load_text("fn main() {}");
        editor.test_register_server("rust", 1).semantic_legend =
            Some(crate::lsp::SemanticTokensLegend {
                token_types: vec!["unresolvedReference".to_owned(), "function".to_owned()],
                token_modifiers: vec!["deprecated".to_owned()],
            });

        editor.apply_semantic_tokens(
            DocumentId(0),
            2,
            lsp_types::SemanticTokensResult::Tokens(lsp_types::SemanticTokens {
                result_id: None,
                data: vec![lsp_types::SemanticToken {
                    delta_line: 0,
                    delta_start: 3,
                    length: 4,
                    token_type: 1,
                    token_modifiers_bitset: 1,
                }],
            }),
        );

        let span = &editor.active_buffer().unwrap().semantic_spans[0];
        assert_eq!(span.token_kind, "function");
        assert_eq!(span.token_modifiers, vec!["deprecated"]);
        assert_eq!((span.start, span.end), (CharIdx(3), CharIdx(7)));
    }

    #[test]
    fn definition_jump_to_unopened_file_lands_on_utf16_position() {
        let mut editor = Editor::default();
        // Stand in for a pending textDocument/definition request.
        editor.pending_lsp.insert(7, PendingLsp::Definition);

        let path = std::path::PathBuf::from("/tmp/def_target.rs");
        editor.update(AppEvent::Lsp(LspEvent::Response {
            id: 7,
            result: Ok(serde_json::json!({
                "uri": format!("file://{}", path.display()),
                "range": {
                    "start": {"line": 0, "character": 7},
                    "end": {"line": 0, "character": 7},
                },
            })),
        }));

        // The response opens the file asynchronously, so the caret can only be
        // placed once the text arrives. An emoji before the target column makes
        // the UTF-16 column diverge from the char index (char 6, not 7).
        let id = DocumentId(editor.next_doc_id - 1);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id,
            result: Ok("let 😀 = value;".to_owned()),
        }));

        let caret = editor
            .layout
            .active_editor(editor.focus)
            .unwrap()
            .view
            .selections
            .primary()
            .head;
        assert_eq!(caret, CharIdx(6));
        assert!(editor.pending_caret_jumps.is_empty());
    }

    #[test]
    fn definition_jump_scrolls_the_target_line_into_view() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 10 });
        editor.pending_lsp.insert(7, PendingLsp::Definition);

        let path = std::path::PathBuf::from("/tmp/far_target.rs");
        editor.update(AppEvent::Lsp(LspEvent::Response {
            id: 7,
            result: Ok(serde_json::json!({
                "uri": format!("file://{}", path.display()),
                "range": {
                    "start": {"line": 30, "character": 0},
                    "end": {"line": 30, "character": 0},
                },
            })),
        }));

        let id = DocumentId(editor.next_doc_id - 1);
        let body = (0..40)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id,
            result: Ok(body),
        }));

        // The definition sits on line 30, far below the 9-row text area (10 rows
        // minus the status bar). The view must scroll it into sight AND leave the
        // definition body below it visible. Bottom-pinning would give top_line
        // 30 + 1 - 9 = 22 (target on the last row, nothing below); revealing with
        // context means a larger top_line, so the target sits higher on screen.
        let scroll = editor
            .layout
            .active_editor(editor.focus)
            .unwrap()
            .view
            .scroll
            .top_line;
        assert!(scroll > 0, "expected the view to scroll, got {scroll}");
        assert!(
            scroll > 22,
            "target pinned to the bottom edge with no context below: {scroll}"
        );
        assert!(scroll <= 30);
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
    fn directory_picker_replaces_the_find_pane_and_receives_keyboard_input() {
        let mut editor = Editor::default();
        editor.update(Command::OpenSearch.into());
        assert!(editor.search_view().is_some());

        let effects = editor.update(Command::OpenDirectoryPicker.into());
        editor.update(AppEvent::TextInput('m'));

        assert!(matches!(effects.as_slice(), [Effect::StartFileScan { .. }]));
        assert!(editor.search_view().is_none());
        assert_eq!(editor.picker_view().unwrap().query, "m");
    }

    #[test]
    fn ctrl_t_and_ctrl_g_toggle_their_picker_closed() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });

        assert!(matches!(
            editor
                .update(Command::OpenDirectoryPicker.into())
                .as_slice(),
            [Effect::StartFileScan { .. }]
        ));
        assert!(editor.picker_view().is_some());
        assert!(
            editor
                .update(Command::OpenDirectoryPicker.into())
                .is_empty()
        );
        assert!(editor.picker_view().is_none());

        editor.open_paths([PathBuf::from("one.txt"), PathBuf::from("two.txt")]);
        editor.update(Command::OpenBufferPicker.into());
        assert!(editor.picker_view().is_some());
        editor.update(Command::OpenBufferPicker.into());
        assert!(editor.picker_view().is_none());
    }

    #[test]
    fn clicking_outside_the_picker_closes_it() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.update(Command::OpenCommandPalette.into());

        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));

        assert!(editor.picker_view().is_none());
        assert_eq!(editor.focus(), Focus::Editor(Side::Left));
    }

    #[test]
    fn stale_file_scan_batches_do_not_enter_a_reopened_picker() {
        let mut editor = Editor::default();
        let first = editor
            .update(Command::OpenDirectoryPicker.into())
            .into_iter()
            .find_map(|effect| match effect {
                Effect::StartFileScan { token, .. } => Some(token),
                _ => None,
            })
            .unwrap();
        editor.update(Command::OpenDirectoryPicker.into());
        let second = editor
            .update(Command::OpenDirectoryPicker.into())
            .into_iter()
            .find_map(|effect| match effect {
                Effect::StartFileScan { token, .. } => Some(token),
                _ => None,
            })
            .unwrap();

        editor.update(AppEvent::FileScan(FileScanEvent::Batch {
            token: first,
            paths: vec![PathBuf::from("stale.txt")],
        }));
        assert_eq!(editor.picker_view().unwrap().total, 0);

        editor.update(AppEvent::FileScan(FileScanEvent::Batch {
            token: second,
            paths: vec![PathBuf::from("current.txt")],
        }));
        assert_eq!(editor.picker_view().unwrap().total, 1);
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

        assert_eq!(view.items.len(), PICKER_VIEW_WINDOW);
        assert!(view.selected < view.items.len());
        assert!(view.items[view.selected].label.contains("file-9000.txt"));
        assert!(view.has_before);
        assert!(view.has_after);
    }

    #[test]
    fn picker_ignores_mouse_input() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 80, rows: 24 });
        editor.open_command_palette();
        let selected = editor.picker.as_ref().unwrap().selected;

        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 10,
                row: 5,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 1,
        }));

        // The picker is keyboard-only; the click neither closes it nor moves it.
        assert!(editor.picker.is_some());
        assert_eq!(editor.picker.as_ref().unwrap().selected, selected);
    }

    #[test]
    fn opening_the_find_pane_dismisses_the_hover_popup() {
        let mut editor = Editor {
            hover: Some("docs".to_owned()),
            ..Default::default()
        };

        editor.update(Command::OpenReplace.into());

        assert!(editor.hover_view().is_none());
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
    fn save_refreshes_current_buffer_highlighting_and_semantic_tokens() {
        let mut editor = Editor::default();
        let path = PathBuf::from("/tmp/save-test.rs");
        editor.open_paths([path.clone()]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("fn main() {}\n".to_owned()),
        }));
        editor.test_register_server("rust", 7).ready = true;
        editor.test_open_doc(DocumentId(1), 3);
        editor.pending_lsp.insert(
            99,
            PendingLsp::SemanticTokens {
                doc: DocumentId(1),
                version: 3,
            },
        );
        let document = editor.documents.get_mut(&DocumentId(1)).unwrap();
        let editable = document.editable_mut();
        editable.semantic_spans = vec![crate::lsp::SemanticSpan {
            start: CharIdx(0),
            end: CharIdx(2),
            token_kind: "comment".to_owned(),
            token_modifiers: Vec::new(),
        }];
        editable.syntax = crate::highlight::IncrementalHighlighter::new("rust", "// stale\n");

        let effects = editor.update(Command::Save.into());

        // 保存で構文ハイライトは全体再パースされる。セマンティックスパンは
        // 次の応答が届くまで既存の色を保つ(消すとフラットな色に落ちて見える)。
        assert!(!editor.active_buffer().unwrap().semantic_spans.is_empty());
        assert_eq!(
            editor.active_buffer().unwrap().syntax_spans,
            crate::highlight::highlight("rust", "fn main() {}\n").as_slice()
        );
        assert!(!editor.pending_lsp.contains_key(&99));
        assert!(effects.iter().any(|effect| {
            matches!(
                effect,
                Effect::LspRequest {
                    server: 7,
                    method,
                    ..
                } if method == "textDocument/semanticTokens/full"
            )
        }));
        assert!(effects.iter().any(|effect| {
            matches!(
                effect,
                Effect::WriteFile {
                    doc: DocumentId(1),
                    path: written_path,
                    contents,
                    ..
                } if written_path == &path && contents == "fn main() {}\n"
            )
        }));
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
        // Enable replace via the checkbox, type the replacement, run it.
        editor.toggle_replace_field();
        editor.update(AppEvent::TextInput('X'));
        editor.run_replace();

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
        editor.update(Command::CycleSearchScope.into());
        editor.update(Command::CycleSearchScope.into());
        editor.update(AppEvent::TextInput('o'));
        editor.toggle_replace_field();
        editor.update(AppEvent::TextInput('X'));
        // Each keystroke restarts the grep, so feed hits for the latest token.
        let token = editor.search.as_ref().unwrap().grep_token.unwrap();
        editor.update(AppEvent::Grep(GrepEvent::Hits {
            token,
            hits: vec![GrepHit {
                path: PathBuf::from("/tmp/a.txt"),
                line: 0,
                text: "one".to_owned(),
            }],
        }));

        // Running replace over a directory asks for confirmation first.
        assert!(editor.run_replace().is_empty());
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
        editor.test_register_server("rust", 7).ready = true;
        editor.test_open_doc(DocumentId(1), 1);

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
            semantic_tokens_legend: None,
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
    fn reopening_an_open_file_reuses_the_existing_document() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("/tmp/dup.rs")]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("fn main() {}\n".to_owned()),
        }));
        editor.update(AppEvent::TextInput('x'));
        assert!(editor.active_buffer().unwrap().modified);

        let effects = editor.open_paths([PathBuf::from("/tmp/dup.rs")]);

        // 複製バッファを作らず、未保存編集をディスク内容で潰さない。
        assert_eq!(editor.documents.len(), 1);
        assert!(
            !effects
                .iter()
                .any(|effect| matches!(effect, Effect::ReadFile { .. }))
        );
        assert_eq!(
            editor
                .layout
                .active_editor(editor.focus)
                .map(|pane| pane.view.doc),
            Some(DocumentId(1))
        );

        // 保存済みならディスクから同じ文書IDへ再読込する。
        editor
            .documents
            .get_mut(&DocumentId(1))
            .unwrap()
            .editable_mut()
            .mark_saved();
        let effects = editor.open_paths([PathBuf::from("/tmp/dup.rs")]);
        assert!(effects.iter().any(|effect| matches!(
            effect,
            Effect::ReadFile {
                id: DocumentId(1),
                ..
            }
        )));
        assert_eq!(editor.documents.len(), 1);
    }

    #[test]
    fn external_reload_resyncs_full_text_to_lsp_server() {
        let mut editor = Editor::default();
        editor.open_paths([PathBuf::from("/tmp/reload.rs")]);
        editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("fn before() {}\n".to_owned()),
        }));
        editor.update(AppEvent::Lsp(LspEvent::Initialized {
            server: 1,
            incremental_sync: true,
            hover_provider: true,
            semantic_tokens_legend: None,
        }));
        assert!(editor.doc_is_opened(DocumentId(1)));
        let version_before = editor.doc_version(DocumentId(1)).unwrap();

        // 外部ツールがディスク上のファイルを書き換えた後の自動再読込。
        let effects = editor.update(AppEvent::Io(IoEvent::FileLoaded {
            id: DocumentId(1),
            result: Ok("// external\nfn after() {}\n".to_owned()),
        }));

        let did_change = effects
            .iter()
            .find_map(|effect| match effect {
                Effect::LspSend { server: 1, message }
                    if message.contains("textDocument/didChange") =>
                {
                    Some(message.clone())
                }
                _ => None,
            })
            .expect("再読込後は全文didChangeでサーバーと同期し直す");
        assert!(did_change.contains("fn after() {}"));
        assert_eq!(
            editor.doc_version(DocumentId(1)).unwrap(),
            version_before + 1
        );
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
            semantic_tokens_legend: None,
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
        // hover が一度返った時点で ready(候補を巡回し続けて待たせない)。
        editor.update(AppEvent::Lsp(LspEvent::Response {
            id: hover_probe,
            result: Ok(serde_json::json!({
                "contents": {"kind": "markdown", "value": "fn main()"}
            })),
        }));
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
        editor.test_register_server("rust", 1);
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
            editor.server(1).and_then(|server| server.error.as_deref()),
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
        assert!(!editor.server_ready(1));
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
        let server = editor.test_register_server("rust", 1);
        server.spawned = true;
        server.ready = true;
        editor.test_open_doc(DocumentId(0), 1);

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
    fn method_matching_the_prefix_stays_because_it_completes_to_a_call() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("main.rs"));
        document.language = Some("rust".to_owned());
        let server = editor.test_register_server("rust", 1);
        server.spawned = true;
        server.ready = true;
        editor.test_open_doc(DocumentId(0), 1);

        editor.update(AppEvent::TextPaste("s.push".to_owned()));
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
            // Both are methods; `push` equals the prefix but completes to `push()`.
            result: Ok(serde_json::json!([
                {"label": "push", "kind": 2},
                {"label": "push_str", "kind": 2}
            ])),
        }));

        let completion = editor.completion_view().unwrap();
        assert!(
            completion.items.contains(&"push".to_owned()),
            "push was dropped: {:?}",
            completion.items
        );
        assert!(completion.items.contains(&"push_str".to_owned()));
    }

    #[test]
    fn starting_rename_dismisses_the_hover_and_completion_popups() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("main.rs"));
        document.language = Some("rust".to_owned());
        editor.test_register_server("rust", 1).ready = true;
        editor.test_open_doc(DocumentId(0), 1);
        editor.update(AppEvent::TextPaste("value".to_owned()));
        editor.hover = Some("診断: unused variable".to_owned());

        editor.update(Command::Rename.into());

        assert!(editor.rename_view().is_some(), "rename prompt should open");
        assert!(
            editor.hover_view().is_none(),
            "the hover/diagnostic popup should be cleared so it can't overlap rename"
        );
        assert!(editor.completion_view().is_none());
    }

    #[test]
    fn function_completion_inserts_parentheses_and_places_caret_inside() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("main.rs"));
        document.language = Some("rust".to_owned());
        editor.test_register_server("rust", 1).ready = true;
        editor.test_open_doc(DocumentId(0), 1);
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
    fn function_completion_in_an_import_statement_does_not_add_parentheses() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("main.rs"));
        document.language = Some("rust".to_owned());
        editor.test_register_server("rust", 1).ready = true;
        editor.test_open_doc(DocumentId(0), 1);
        editor.update(AppEvent::TextPaste("use crate::cur".to_owned()));

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

        assert_eq!(
            editor.active_buffer().unwrap().text.to_string(),
            "use crate::current_dir"
        );
    }

    #[test]
    fn malformed_automatic_completion_response_does_not_replace_the_status() {
        let mut editor = Editor::default();
        let document = editor.documents.get_mut(&DocumentId(0)).unwrap();
        document.path = Some(PathBuf::from("main.rs"));
        document.language = Some("rust".to_owned());
        editor.test_register_server("rust", 1).ready = true;
        editor.test_open_doc(DocumentId(0), 1);
        editor.update(AppEvent::TextPaste("cur".to_owned()));
        editor.status = Some("保存しました".to_owned());
        let request = editor
            .request_completion(false)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::LspRequest { id, .. } => Some(id),
                _ => None,
            })
            .unwrap();

        editor.update(AppEvent::Lsp(LspEvent::Response {
            id: request,
            result: Err("server cancelled request".to_owned()),
        }));

        assert_eq!(editor.status(), Some("保存しました"));
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

        editor.update(Command::DeleteBackward.into());
        let buffer = editor.active_buffer().unwrap();
        assert_eq!(buffer.text.to_string(), "a  b");
        assert_eq!(buffer.view.selections.primary().head, CharIdx(3));
    }

    #[test]
    fn bracketed_multiline_paste_is_inserted_literally_and_normalizes_line_endings() {
        let mut editor = Editor::default();

        editor.update(AppEvent::TextPaste("if ready {\r\n  value\r\n}".to_owned()));

        assert_eq!(
            editor.active_buffer().unwrap().text.to_string(),
            "if ready {\n  value\n}"
        );
    }

    #[test]
    fn typed_opening_delimiters_insert_pairs_without_treating_paste_as_typing() {
        let mut editor = Editor::default();

        editor.update(AppEvent::TextInput('['));
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "[]");
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
        editor.update(Command::DeleteBackward.into());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "");

        editor.update(AppEvent::TextInput('{'));
        editor.update(AppEvent::TextInput('}'));
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "{}");
        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .head,
            CharIdx(2)
        );
        editor.update(Command::DeleteBackward.into());
        editor.update(Command::DeleteBackward.into());
        assert_eq!(editor.active_buffer().unwrap().text.to_string(), "");

        editor.update(AppEvent::TextPaste("[literal]".to_owned()));
        assert_eq!(
            editor.active_buffer().unwrap().text.to_string(),
            "[literal]"
        );
    }

    #[test]
    fn ctrl_c_keeps_the_find_pane_open() {
        let mut editor = Editor::default();
        editor.update(Command::OpenSearch.into());

        editor.update(Command::Cancel.into());

        assert!(editor.search_view().is_some());
        assert_eq!(editor.focus(), Focus::Overlay);
    }

    #[test]
    fn mouse_wheel_scrolls_the_terminal_pane_scrollback() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 40, rows: 6 });
        editor.update(Command::ToggleShell.into());
        editor.update(AppEvent::Terminal(TerminalEvent::Output(
            (0..20)
                .map(|line| format!("line {line}\r\n"))
                .collect::<String>()
                .into_bytes(),
        )));
        assert_eq!(editor.terminal.as_ref().unwrap().screen().scrollback(), 0);

        editor.update(AppEvent::Mouse(MouseInput {
            event: MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 30,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            clicks: 0,
        }));

        assert_eq!(editor.terminal.as_ref().unwrap().screen().scrollback(), 3);
    }

    #[test]
    fn shell_paste_preserves_child_bracketed_paste_mode() {
        let mut editor = Editor::default();
        editor.update(AppEvent::Resize { cols: 40, rows: 6 });
        editor.update(Command::ToggleShell.into());
        editor.update(AppEvent::Terminal(TerminalEvent::Output(
            b"\x1b[?2004h".to_vec(),
        )));

        let effects = editor.update(AppEvent::TextPaste("one\ntwo".to_owned()));

        assert_eq!(
            effects,
            vec![Effect::TerminalInput(
                b"\x1b[200~one\ntwo\x1b[201~".to_vec()
            )]
        );
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
