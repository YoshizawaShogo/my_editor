mod command;
mod diff_view;
mod effect;
mod event;
mod focus;
mod layout;
mod navigate;
mod snippet_session;

pub use command::{Command, Direction, Unit, VerticalDirection};
pub use effect::Effect;
pub use event::{
    AppEvent, CtagsDefinitionEvent, FileScanEvent, GitEvent, GitInfo, GitLine, GitLineKind,
    GrepEvent, GrepHit, IoEvent, MouseInput, ShellcheckEvent, TerminalEvent,
};
pub use focus::{Focus, Side};
use layout::{DiffPane, RightPane};
pub use layout::{EditorPane, Layout};
use snippet_session::SnippetSession;

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
    /// Characters that trigger signature help; empty when unsupported.
    signature_help_triggers: Vec<String>,
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
            signature_help_triggers: Vec::new(),
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
        self.signature_help_triggers = Vec::new();
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
    /// The workspace root is under a `.gitignore` rule (e.g. a file opened from
    /// inside `target/` or `node_modules/`). Scanning such a tree with the ignore
    /// filters on yields nothing, so this forces file-scan and search to walk it
    /// regardless of `config.search.respect_ignore_files`.
    workspace_ignored: bool,
    next_scan_token: u64,
    next_grep_token: u64,
    next_shell_token: u64,
    /// Language name → server id; the index into `servers`.
    lsp_servers: HashMap<String, u64>,
    servers: HashMap<u64, LspServer>,
    next_server_id: u64,
    pending_lsp: HashMap<i64, PendingLsp>,
    next_lsp_request: i64,
    completion: Option<CompletionState>,
    /// The snippet currently being filled in, if any. Tab/Shift+Tab step through
    /// its stops (which live on the document as `snippet_stops`).
    snippet_session: Option<SnippetSession>,
    completion_suppressed: Option<(DocumentId, i32)>,
    rename_input: Option<String>,
    /// The Go-to-Line prompt: the pane side to jump in and the digits typed so
    /// far. `Some` means the prompt is open.
    goto_input: Option<(Side, String)>,
    confirm: Option<ConfirmState>,
    hover: Option<String>,
    /// The signature-help popup (argument hints). Like hover, it never takes
    /// focus — it is a plain overlay shown while typing a call.
    signature_help: Option<SignatureHelpState>,
    deferred_hover: Option<(DocumentId, CharIdx)>,
    nav_back: Vec<(DocumentId, CharIdx)>,
    nav_forward: Vec<(DocumentId, CharIdx)>,
    /// Caret positions to restore once a freshly opened document finishes loading,
    /// used by jumps that open a file whose text is not in memory yet.
    /// Where to put the caret once a freshly opened document loads. A range with
    /// start == end lands a bare caret; a wider one selects, so a jump to a search
    /// hit arrives with the match highlighted rather than silently placing a caret.
    pending_caret_jumps: HashMap<DocumentId, lsp_types::Range>,
    /// Where each document was last seen — caret, selections and scroll — so a
    /// buffer switched away from and back to reopens where it was left rather
    /// than at the top. Written only at the single exit of [`Self::update`], so
    /// none of the many switching paths (picker, reopen, close, jump) has to
    /// remember to save it.
    last_views: HashMap<DocumentId, View>,
    pending_self_disk_updates: HashMap<DocumentId, usize>,
    /// The shell, alive from the first Ctrl+O until it exits. Independent of
    /// [`RightPane::Shell`], which only says whether it is currently on screen:
    /// hiding the pane has to keep the session so reopening resumes it.
    shell: Option<ShellSession>,
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
            layout: Layout::new(View::new(id)),
            focus: Focus::Editor(Side::Left),
            clipboard: Register::default(),
            drag_anchor: None,
            picker: None,
            config: Config::default(),
            workspace_root: PathBuf::from("."),
            workspace_ignored: false,
            next_scan_token: 1,
            next_grep_token: 1,
            next_shell_token: 1,
            lsp_servers: HashMap::new(),
            servers: HashMap::new(),
            next_server_id: 1,
            pending_lsp: HashMap::new(),
            next_lsp_request: 1,
            completion: None,
            snippet_session: None,
            completion_suppressed: None,
            rename_input: None,
            goto_input: None,
            confirm: None,
            hover: None,
            signature_help: None,
            deferred_hover: None,
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            pending_caret_jumps: HashMap::new(),
            last_views: HashMap::new(),
            pending_self_disk_updates: HashMap::new(),
            shell: None,
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
        let effects = self.apply_event(event);
        // Recorded here rather than inside `apply_event`: its arms return early
        // in several places, and a position saved only on the fall-through
        // path would be skipped exactly when one of those switched buffers.
        self.remember_visible_views();
        effects
    }

    fn apply_event(&mut self, event: AppEvent) -> Vec<Effect> {
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
                self.signature_help_after_typing(character)
            }
            AppEvent::TextPaste(text) => {
                if self.focus == Focus::Shell {
                    if let Some(shell) = self.shell.as_mut() {
                        let bracketed = shell.parser.screen().bracketed_paste();
                        shell.parser.set_scrollback(0);
                        let mut bytes = text.into_bytes();
                        if bracketed {
                            bytes.splice(0..0, b"\x1b[200~".iter().copied());
                            bytes.extend_from_slice(b"\x1b[201~");
                        }
                        shell.selection = None;
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
                if let Some(shell) = self.shell.as_mut() {
                    let shell_cols = split_right_width(cols).max(1);
                    let shell_rows = rows.saturating_sub(1).max(1);
                    shell.parser.set_size(shell_rows, shell_cols);
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
                if self.search().is_some() {
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
                        MouseEventKind::Drag(MouseButton::Left)
                            if self.drag_search_selection(mouse.event.column, mouse.event.row) =>
                        {
                            return Vec::new();
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
                let over_terminal = self.layout.is_shell()
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
                if matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left))
                    && let Some(forward) =
                        self.diff_navigator_click(mouse.event.column, mouse.event.row)
                {
                    self.jump_diff_hunk(forward);
                    return Vec::new();
                }
                // The diff view has its own navigator above; here the badges sit
                // on ordinary editor panes.
                if matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left))
                    && !self.layout.is_diff()
                    && self.edge_badge_click(mouse.event.column, mouse.event.row)
                {
                    return Vec::new();
                }
                let on_split_divider = self.layout.right.is_some()
                    && mouse.event.column == split_left_width(self.terminal_size.0);
                // The diff is a comparison view, not a place to interrogate code:
                // clicking there must not fire hover or go-to-definition.
                let lsp_clickable = !on_split_divider && !self.layout.is_diff();
                let definition =
                    matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left))
                        && mouse.event.modifiers.contains(KeyModifiers::CONTROL)
                        && lsp_clickable;
                let hover = matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left))
                    && !definition
                    && lsp_clickable;
                let copy_shell_selection = self.focus == Focus::Shell
                    && matches!(mouse.event.kind, MouseEventKind::Up(MouseButton::Left));
                if matches!(mouse.event.kind, MouseEventKind::Down(MouseButton::Left)) {
                    self.dismiss_completion();
                    self.dismiss_signature_help();
                    // Remember where we are before the click moves the caret, so
                    // Ctrl+E returns there. This is recorded here for a Ctrl+click
                    // too — capturing the position you left, not the symbol you
                    // clicked — so Back after go-to-definition lands where you were
                    // reading rather than on the click point.
                    if matches!(self.focus, Focus::Editor(_)) {
                        self.record_jump_origin();
                    }
                }
                self.apply_mouse(mouse);
                if copy_shell_selection {
                    self.copy_shell_selection()
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
                        self.start_lsps_for_open_documents()
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
                if let Some(shell) = self.shell.as_mut() {
                    shell.selection = None;
                    shell.parser.set_scrollback(0);
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
            AppEvent::Shellcheck(event) => {
                if let Some(document) = self.documents.get_mut(&event.doc)
                    && let Some(editable) = document.editable_opt_mut()
                {
                    editable.set_diagnostics(event.diagnostics);
                }
                self.dirty = true;
                Vec::new()
            }
            AppEvent::CtagsDefinition(event) => match event.location {
                Some((path, line)) => {
                    let at = lsp_types::Position::new(line, 0);
                    self.open_path_at(path, lsp_types::Range::new(at, at))
                }
                None => {
                    self.notify(ToastLevel::Info, "定義が見つかりません");
                    Vec::new()
                }
            },
            AppEvent::Tick => {
                let toasts_before = self.notifications.len();
                self.notifications
                    .retain(|toast| toast.created.elapsed() < toast.ttl);
                // Redraw only when a toast is on screen (it needs to expire on
                // time) or one just did. Idle with nothing visible changing must
                // not force a repaint every tick — external edits still trigger a
                // redraw through DiskStateObserved.
                if !self.notifications.is_empty() || self.notifications.len() != toasts_before {
                    self.dirty = true;
                }
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

    /// Whether a toast is on screen. The runtime keeps its poll timer at the
    /// fast cadence while one is visible so it expires on time.
    pub fn has_notifications(&self) -> bool {
        !self.notifications.is_empty()
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

    /// Record where every visible pane is. Runs once per event, after it has been
    /// applied, so each document's entry holds its latest position and outlives
    /// the pane being pointed at another document.
    fn remember_visible_views(&mut self) {
        let views: Vec<View> = self
            .layout
            .panes_mut()
            .into_iter()
            .map(|pane| pane.view.clone())
            .collect();
        for view in views {
            self.last_views.insert(view.doc, view);
        }
    }

    /// The view to open `doc` with when switching to it: where it was last seen,
    /// or the top the first time. Clamped to the current text, since the document
    /// may have shrunk (an external reload) while it was off screen.
    fn view_for(&self, doc: DocumentId) -> View {
        let Some(mut view) = self.last_views.get(&doc).cloned() else {
            return View::new(doc);
        };
        if let Some(text) = self
            .documents
            .get(&doc)
            .and_then(Document::editable_opt)
            .map(|editable| editable.text())
        {
            let len = text.len_chars();
            let clamp = |index: CharIdx| CharIdx(index.0.min(len));
            let ranges = view
                .selections
                .iter()
                .map(|selection| Selection {
                    anchor: clamp(selection.anchor),
                    head: clamp(selection.head),
                })
                .collect();
            let primary = view.selections.primary_index();
            view.selections = crate::view::Selections::from_vec(ranges, primary);
            // Clamping can stack several carets on the end of the text; merge
            // them, or one keystroke would be applied once per duplicate.
            view.selections.normalize();
            let last_line = text.len_lines().saturating_sub(1);
            if view.scroll.top_line > last_line {
                view.scroll.top_line = last_line;
                view.scroll.wrapped_row_offset = 0;
            }
        }
        view
    }

    pub fn open_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> Vec<Effect> {
        let paths: Vec<_> = paths.into_iter().collect();
        if !paths.is_empty() {
            self.documents.remove(&DocumentId(0));
            self.last_views.remove(&DocumentId(0));
        }
        let mut effects = Vec::new();
        for path in paths {
            let (id, load) = self.document_for_path(path);
            let view = self.view_for(id);
            if self.layout.is_diff() {
                self.show_only(view);
                self.focus = Focus::Editor(Side::Left);
            } else if let Some(pane) = self.layout.active_editor_mut(self.focus) {
                pane.view = view;
            }
            effects.extend(load);
        }
        effects
    }

    /// The document for `path` and the effect that loads its text, without
    /// touching the layout — callers that place it somewhere other than the
    /// active pane (the diff picker) need the id without the side effect.
    fn document_for_path(&mut self, path: PathBuf) -> (DocumentId, Vec<Effect>) {
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
        let unsaved_edits = existing.is_some_and(|id| {
            self.documents
                .get(&id)
                .and_then(crate::document::Document::editable_opt)
                .is_some_and(|editable| editable.modified)
        });
        let effects = if unsaved_edits {
            Vec::new()
        } else {
            vec![Effect::ReadFile { id, path }]
        };
        (id, effects)
    }

    /// Open a single file and place the caret at `position` once its text loads.
    fn open_path_at(&mut self, path: PathBuf, range: lsp_types::Range) -> Vec<Effect> {
        let effects = self.open_paths([path.clone()]);
        let Some(id) = self
            .documents
            .iter()
            .find_map(|(id, document)| (document.path.as_ref() == Some(&path)).then_some(*id))
        else {
            return effects;
        };
        self.pending_caret_jumps.insert(id, range);
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
        let Some(range) = self.pending_caret_jumps.remove(&id) else {
            return;
        };
        let Some(document) = self.documents.get(&id) else {
            return;
        };
        let selection = document.editable_opt().map(|editable| {
            let at = |position: lsp_types::Position| {
                crate::position::lsp_position_to_char_idx(
                    editable.text(),
                    position.line as usize,
                    position.character as usize,
                )
            };
            let (start, end) = (at(range.start), at(range.end));
            if start == end {
                Selection::caret(start)
            } else {
                Selection {
                    anchor: start,
                    head: end,
                }
            }
        });
        for pane in self.layout.panes_mut() {
            if pane.view.doc != id {
                continue;
            }
            match selection {
                Some(selection) => pane.view.selections.set_single(selection),
                // Large files have no editable text to hold a caret; scroll the
                // target line into view instead.
                None => pane.view.scroll.top_line = range.start.line as usize,
            }
        }
        if selection.is_some() {
            self.reveal_caret_with_context();
        }
    }

    pub fn set_workspace_root(&mut self, root: PathBuf) {
        self.workspace_root = root;
    }

    /// Mark the workspace root as sitting under a `.gitignore` rule. When set,
    /// [`Self::respect_ignore_files`] returns false so the file picker and search
    /// can still see the tree.
    pub fn set_workspace_ignored(&mut self, ignored: bool) {
        self.workspace_ignored = ignored;
    }

    /// Whether file-scan and search should honor `.gitignore`/`.ignore` files.
    /// Forced off when the workspace root is itself ignored — otherwise the walk
    /// returns nothing — but otherwise follows the configured default.
    fn respect_ignore_files(&self) -> bool {
        !self.workspace_ignored && self.config.search.respect_ignore_files
    }

    /// Install `right` as the right pane, dropping whatever it displaced.
    ///
    /// Every path that opens a right pane goes through here, so the pane being
    /// replaced releases its state as a matter of course — the find pane's
    /// running grep in particular, which would otherwise keep reporting
    /// progress for a pane that is no longer on screen.
    fn set_right_pane(&mut self, right: Option<RightPane>) {
        if let Some(RightPane::Search(_)) = std::mem::replace(&mut self.layout.right, right) {
            self.finish_progress("grep");
        }
        // Focus must not outlive the pane it pointed at. `Overlay` belongs to the
        // find pane and `Side::Right` to whatever sits on the right, so either
        // one is stranded once that pane is replaced or removed — and a stranded
        // focus swallows every keystroke, since no overlay claims it and no
        // editor pane has it. Every right-pane change funnels through here, so
        // this is the one place the rule has to hold.
        let stranded = match &self.layout.right {
            None => !matches!(self.focus, Focus::Editor(Side::Left) | Focus::Shell),
            Some(RightPane::Search(_)) => false,
            Some(_) => self.focus == Focus::Overlay,
        };
        if stranded {
            self.focus = Focus::Editor(Side::Left);
        }
        self.dirty = true;
    }

    /// Collapse to a single pane, for tests that need to exercise what happens
    /// to focus when the right pane goes away underneath it.
    #[cfg(test)]
    pub(crate) fn test_show_only(&mut self, doc: DocumentId) {
        self.show_only(View::new(doc));
    }

    /// Show `view` in the left pane on its own, closing the right pane.
    fn show_only(&mut self, view: View) {
        self.layout.left = EditorPane { view };
        self.set_right_pane(None);
    }

    /// Take the find pane's state, leaving any other kind of right pane alone.
    fn take_search(&mut self) -> Option<SearchState> {
        let RightPane::Search(search) = self
            .layout
            .right
            .take_if(|right| matches!(right, RightPane::Search(_)))?
        else {
            return None;
        };
        self.finish_progress("grep");
        self.dirty = true;
        Some(*search)
    }

    /// Columns available to the focused file pane. Only a second file pane can
    /// hold the caret, so every other kind of right pane leaves the editor on
    /// the left half.
    fn active_pane_width(&self) -> u16 {
        let total = self.terminal_size.0;
        if self.layout.right.is_none() {
            return total;
        }
        if self.layout.right_editor().is_some() && matches!(self.focus, Focus::Editor(Side::Right))
        {
            return split_right_width(total);
        }
        split_left_width(total)
    }

    fn search(&self) -> Option<&SearchState> {
        self.layout.search()
    }

    fn search_mut(&mut self) -> Option<&mut SearchState> {
        self.layout.search_mut()
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
                PickerMode::Directory => "Open file · / ~ ../ でパス補完",
                PickerMode::Buffer => "Open buffer",
                PickerMode::Diff => "Compare with · buffer or file · / ~ ../ でパス補完",
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
        let search = self.search()?;
        // Locating the match inside a grep line needs the same pattern the search
        // ran with; built once rather than per hit.
        let pattern = search_pattern(&search.query, search.options).ok();
        // Each row is built as (location, text, match-within-text), then the
        // location column is padded to a common width so the dashed separator
        // forms a straight line even where line numbers differ in digits.
        let rows: Vec<(String, String, Option<std::ops::Range<usize>>)> = search
            .hits
            .iter()
            .take(500)
            .map(|hit| match hit {
                SearchHit::Buffer {
                    doc,
                    range,
                    preview,
                } => {
                    // Show the matched line's text instead of raw char offsets: a
                    // "foo.rs  120..125" tells the reader nothing about the match.
                    // The file path only earns its space when several buffers are
                    // in scope; for a single-buffer search it is just noise.
                    // Straight from the snapshot — reading the document here would
                    // drift as soon as it was edited.
                    let location = if search.scope == SearchScope::AllBuffers {
                        format!("{}:{}", self.document_label(*doc), preview.line + 1)
                    } else {
                        (preview.line + 1).to_string()
                    };
                    let limit = preview.text.chars().count();
                    let end = preview.column + range.end.saturating_sub(range.start);
                    let matched = (preview.column < limit).then(|| preview.column..end.min(limit));
                    (location, preview.text.clone(), matched)
                }
                SearchHit::Disk(hit) => {
                    // grep reports the line but not the column, so re-run the
                    // pattern over the line to place the highlight.
                    let line_text = hit.text.trim();
                    let location = format!("{}:{}", self.display_path(&hit.path), hit.line + 1);
                    let matched = pattern.as_ref().and_then(|pattern| {
                        let found = pattern.find(line_text)?;
                        let start = line_text[..found.start()].chars().count();
                        Some(start..start + found.as_str().chars().count())
                    });
                    (location, line_text.to_owned(), matched)
                }
            })
            .collect();
        let location_width = rows
            .iter()
            .map(|(location, _, _)| location.chars().count())
            .max()
            .unwrap_or(0);
        let items = rows
            .into_iter()
            .map(|(location, line_text, matched)| {
                let pad = location_width - location.chars().count();
                let prefix = format!("{location}{}{SEARCH_COLUMN_SEPARATOR}", " ".repeat(pad));
                let prefix_len = prefix.chars().count();
                SearchResultItem {
                    text: format!("{prefix}{line_text}"),
                    prefix_len,
                    matched: matched.map(|range| prefix_len + range.start..prefix_len + range.end),
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
            // The pane takes focus as an overlay; a picker on top steals it.
            focused: self.focus == Focus::Overlay && self.picker.is_none(),
            current: search.current,
            total: search.hits.len(),
            field_cursor: search.field_cursor,
            field_selection: self.search_selection(),
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

    pub fn goto_view(&self) -> Option<&str> {
        self.goto_input.as_ref().map(|(_, digits)| digits.as_str())
    }

    pub fn confirm_view(&self) -> Option<&str> {
        self.confirm
            .as_ref()
            .map(|confirm| confirm.message.as_str())
    }

    pub fn hover_view(&self) -> Option<&str> {
        self.hover.as_deref()
    }

    pub fn signature_help_view(&self) -> Option<SignatureHelpView<'_>> {
        let help = self.signature_help.as_ref()?;
        Some(SignatureHelpView {
            label: &help.label,
            active_parameter: help.active_parameter,
            anchor: help.anchor,
        })
    }

    pub fn terminal_contents(&self) -> Option<String> {
        self.shell
            .as_ref()
            .map(|shell| shell.parser.screen().contents())
    }

    pub fn terminal_screen(&self) -> Option<&vt100::Screen> {
        let shell = self.shell.as_ref()?;
        Some(
            shell
                .selection
                .as_ref()
                .map_or_else(|| shell.parser.screen(), |selection| &selection.snapshot),
        )
    }

    pub fn terminal_selection_view(&self) -> Option<TerminalSelectionView> {
        let selection = self.shell.as_ref()?.selection.as_ref()?;
        (selection.anchor != selection.head).then(|| {
            let (start, end) = ordered_terminal_points(selection.anchor, selection.head);
            TerminalSelectionView { start, end }
        })
    }

    pub fn shell_focused(&self) -> bool {
        self.focus == Focus::Shell
    }

    /// Whether the caret belongs in the document rather than in an overlay's own
    /// input. The find pane can hold focus while staying on screen, so a visible
    /// pane no longer implies the document has given up the caret.
    pub fn document_focused(&self) -> bool {
        matches!(self.focus, Focus::Editor(_) | Focus::Completion(_))
    }

    /// Whether anything occupies the right half, whatever kind of pane it is.
    /// The editor gets the left half in that case.
    pub fn is_split(&self) -> bool {
        self.layout.right.is_some()
    }

    pub fn shell_visible(&self) -> bool {
        self.layout.is_shell()
    }

    pub fn search_pane_visible(&self) -> bool {
        self.layout.search().is_some()
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

    /// Record a progress message; returns whether the displayed text changed. An
    /// unchanged repeat (servers re-send the same progress) should not repaint.
    fn set_progress(&mut self, key: impl Into<String>, text: impl Into<String>) -> bool {
        let (key, text) = (key.into(), text.into());
        if self.progress.get(&key) == Some(&text) {
            return false;
        }
        self.progress.insert(key, text);
        self.dirty = true;
        true
    }

    /// Clear a progress entry; returns whether anything was actually removed.
    fn finish_progress(&mut self, key: &str) -> bool {
        let removed = self.progress.remove(key).is_some();
        if removed {
            self.dirty = true;
        }
        removed
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
                            // Shell scripts have no language server; shellcheck
                            // fills that gap by relinting on each save. Only bash/sh
                            // (the "bash" language) — csh is its own language and
                            // shellcheck rejects it (SC1071: sh/bash/dash/ksh only).
                            if document.language.as_deref() == Some("bash") {
                                effects.push(Effect::RunShellcheck {
                                    doc: id,
                                    path: path.to_path_buf(),
                                });
                            }
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
                    let Some(state) = state else {
                        // No file on disk yet (e.g. a path opened to be created).
                        // Not an error: forget any prior state and clear the
                        // external-change flag without touching the status line.
                        document.disk_state = None;
                        document.external_changed = false;
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
                    self.show_only(View::new(id));
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
                signature_help_triggers,
            } => {
                self.finish_progress(&format!("lsp:{server}"));
                if let Some(entry) = self.server_mut(server) {
                    entry.spawned = true;
                    entry.ready = true;
                    entry.hover_capable = hover_provider;
                    entry.incremental_sync = incremental_sync;
                    entry.semantic_legend = semantic_tokens_legend;
                    entry.signature_help_triggers = signature_help_triggers;
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
                        // Skip the blanket repaint below when the server re-sent an
                        // identical diagnostic set (rust-analyzer does this while
                        // idle, which otherwise wakes a redraw every few seconds).
                        if !editable.set_diagnostics(diagnostics) {
                            return effects;
                        }
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
                let changed = if let Some(message) = message {
                    self.set_progress(key, message)
                } else {
                    self.finish_progress(&key)
                };
                // An unchanged progress notification should not force a repaint.
                if !changed {
                    return effects;
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
                        let language = self
                            .documents
                            .get(&doc)
                            .and_then(|document| document.language.clone());
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
                            let callable = kind_is_callable(item.kind, language.as_deref());
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
                                    snippet_body: None,
                                },
                            ))
                        })
                        .collect();
                        items.sort_by_key(|right| std::cmp::Reverse(right.0));
                        // Offer language snippets alongside the server's results,
                        // ranked to the top so `for`, `fn`, … are easy to reach.
                        let mut merged = language
                            .as_deref()
                            .map(|language| snippet_candidates(language, &prefix))
                            .unwrap_or_default();
                        merged.extend(items.into_iter().map(|(_, item)| item));
                        let items = merged;
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
                            // Origin was already recorded at the Ctrl+click, before
                            // the caret moved to the symbol.
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
                                self.show_only(view);
                                // The new view starts scrolled to the top; reveal the
                                // definition with context so its body isn't pushed
                                // just past the bottom edge.
                                self.reveal_caret_with_context();
                            } else {
                                // The file is not open yet, so its text is not loaded.
                                // Remember where to land and apply it once the read
                                // completes, otherwise the caret sits at the top.
                                effects.extend(self.open_path_at(
                                    path,
                                    lsp_types::Range::new(
                                        location.range.start,
                                        location.range.start,
                                    ),
                                ));
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
                        // Drop blank hover text: a server may answer with empty
                        // contents for a position it knows nothing about, and a
                        // lone "" would otherwise render as an empty popup box.
                        let mut parts = hover
                            .map(|hover| hover_text(hover.contents))
                            .filter(|text| !text.trim().is_empty())
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
                Some(PendingLsp::SignatureHelp { doc, anchor }) => {
                    // Focus-less like hover: only show it if the caret is still in
                    // the requesting document and no overlay has taken over.
                    if self
                        .layout
                        .active_editor(self.focus)
                        .is_some_and(|pane| pane.view.doc == doc)
                        && matches!(self.focus, Focus::Editor(_))
                    {
                        self.signature_help = result
                            .ok()
                            .and_then(|value| {
                                serde_json::from_value::<Option<lsp_types::SignatureHelp>>(value)
                                    .ok()
                            })
                            .flatten()
                            .and_then(|help| signature_help_state(help, anchor));
                        self.dirty = true;
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
        let words = ranked
            .into_iter()
            .take(50)
            .map(|(_, word)| CompletionCandidate {
                insert: word.to_owned(),
                cursor_back: 0,
                label: word.to_owned(),
                prefix_len,
                snippet_body: None,
            });
        // Snippets rank above buffer words so `for`, `if`, … stay reachable.
        let language = self
            .documents
            .get(&doc)
            .and_then(|document| document.language.clone());
        let mut items = language
            .as_deref()
            .map(|language| snippet_candidates(language, &prefix))
            .unwrap_or_default();
        items.extend(words);
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
            // No language server for this buffer — fall back to ctags.
            return self.request_ctags_definition();
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

    /// Languages ctags-based go-to-definition is offered for: the ones ctags
    /// indexes well and the editor targets. Keyed on the document's language, not
    /// its extension, so anything the config maps to one of them (`.pyi`, `.sdc`,
    /// `.cshrc`) qualifies without a second list to keep in step with the first.
    const CTAGS_LANGUAGES: &[&str] = &["rust", "c", "python", "bash", "csh", "tcl"];

    /// Resolve the identifier under the caret with ctags. The fallback path for
    /// buffers without a language server; a no-op unless the file is a ctags
    /// target and a symbol sits under the caret. The scan itself runs off-thread
    /// and is a no-op when ctags is not installed.
    fn request_ctags_definition(&mut self) -> Vec<Effect> {
        let pane = match self.layout.active_editor(self.focus) {
            Some(pane) => pane,
            None => return Vec::new(),
        };
        let Some(document) = self.documents.get(&pane.view.doc) else {
            return Vec::new();
        };
        let Some(editable) = document.editable_opt() else {
            return Vec::new();
        };
        let is_target = document
            .language
            .as_deref()
            .is_some_and(|language| Self::CTAGS_LANGUAGES.contains(&language));
        if !is_target {
            return Vec::new();
        }
        let Some(symbol) = word_at(editable.text(), pane.view.selections.primary().head) else {
            return Vec::new();
        };
        let doc = pane.view.doc;
        // Origin was already recorded at the Ctrl+click, before the caret moved.
        vec![Effect::CtagsDefinition {
            doc,
            symbol,
            root: self.workspace_root.clone(),
        }]
    }

    /// After a keystroke, open/refresh or close the signature-help popup. Fires
    /// on the active server's trigger characters (usually `(` / `,`), refreshes
    /// while the popup is already up so the active argument tracks the caret, and
    /// closes on the call's `)`.
    fn signature_help_after_typing(&mut self, character: char) -> Vec<Effect> {
        if character == ')' {
            self.dismiss_signature_help();
            return Vec::new();
        }
        if !self.is_signature_help_trigger(character) && self.signature_help.is_none() {
            return Vec::new();
        }
        let Some(index) = self
            .layout
            .active_editor(self.focus)
            .map(|pane| pane.view.selections.primary().head)
        else {
            return Vec::new();
        };
        self.request_signature_help_at(index)
    }

    fn is_signature_help_trigger(&self, character: char) -> bool {
        let mut buffer = [0u8; 4];
        let needle = character.encode_utf8(&mut buffer);
        self.layout
            .active_editor(self.focus)
            .and_then(|pane| self.documents.get(&pane.view.doc))
            .and_then(|document| document.language.as_deref())
            .and_then(|language| self.lsp_servers.get(language))
            .and_then(|id| self.servers.get(id))
            .filter(|server| server.ready)
            .is_some_and(|server| {
                server
                    .signature_help_triggers
                    .iter()
                    .any(|trigger| trigger == needle)
            })
    }

    fn request_signature_help_at(&mut self, index: CharIdx) -> Vec<Effect> {
        self.pending_lsp
            .retain(|_, pending| !matches!(pending, PendingLsp::SignatureHelp { .. }));
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
        self.pending_lsp
            .insert(id, PendingLsp::SignatureHelp { doc, anchor: index });
        vec![Effect::LspRequest {
            server,
            id,
            method: "textDocument/signatureHelp".to_owned(),
            params: serde_json::json!({
                "textDocument": {"uri": format!("file://{}", path.display())},
                "position": {"line": line, "character": character}
            })
            .to_string(),
        }]
    }

    fn dismiss_signature_help(&mut self) {
        if self.signature_help.take().is_some() {
            self.dirty = true;
        }
        self.pending_lsp
            .retain(|_, pending| !matches!(pending, PendingLsp::SignatureHelp { .. }));
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

    /// Start a language server for each language that is *actually open*, once the
    /// config that names those servers has loaded. Opening a `.md` file must not
    /// spawn rust-analyzer — a server only starts when a document of its language
    /// is present. Files opened later start their server lazily through
    /// [`Self::start_or_open_lsp`] on load.
    fn start_lsps_for_open_documents(&mut self) -> Vec<Effect> {
        let docs: Vec<DocumentId> = self.documents.keys().copied().collect();
        let mut effects = Vec::new();
        for doc in docs {
            effects.extend(self.start_or_open_lsp(doc));
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
        self.servers.entry(id).or_insert_with(|| {
            let mut server = LspServer::new(language.to_owned());
            // Default to a semantic-tokens-capable server (like rust-analyzer);
            // tests exercising a server without them (like pylsp) clear this.
            server.semantic_legend = Some(crate::lsp::SemanticTokensLegend {
                token_types: vec!["function".to_owned()],
                token_modifiers: Vec::new(),
            });
            server
        })
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
        let mut effects = vec![Effect::LspSend {
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
        }];
        // Only ask for semantic tokens if the server advertised them. pylsp, for
        // one, has no semanticTokensProvider, so requesting would draw an error
        // and the status would sit forever on "coloring" waiting for tokens that
        // never come.
        if self
            .server(server)
            .is_some_and(|server| server.semantic_legend.is_some())
        {
            let request = self.next_lsp_request;
            self.next_lsp_request += 1;
            self.pending_lsp
                .insert(request, PendingLsp::SemanticTokens { doc, version: 1 });
            effects.push(Effect::LspRequest {
                server,
                id: request,
                method: "textDocument/semanticTokens/full".to_owned(),
                params: serde_json::json!({
                    "textDocument": {"uri": format!("file://{}", path.display())}
                })
                .to_string(),
            });
        }
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
        // A server without a semantic-tokens legend (e.g. pylsp) never provides
        // them; don't send a request it will only reject.
        if self
            .server(server)
            .is_none_or(|server| server.semantic_legend.is_none())
        {
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
            let view = self.view_for(preferred_doc);
            self.show_only(view);
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
            Command::CollapseSelections => {
                // Esc also closes the focus-less signature-help popup.
                self.dismiss_signature_help();
                // The diff is display-only, so Esc backs out of it. Selections
                // there exist only for copying, and are not what you are trying
                // to escape from.
                if self.layout.is_diff() {
                    self.close_diff();
                } else {
                    self.collapse_selections();
                }
            }
            Command::AddCursor { direction } => self.add_cursor(direction),
            Command::SelectNextOccurrence => self.select_next_occurrence(),
            Command::Copy => return self.copy_active(true),
            Command::Cut => {
                // Deleting must key off "did we copy", not off the OSC 52 effect —
                // that effect is empty when OS-clipboard push is disabled, and a
                // no-selection Ctrl+X must still cut the current line.
                if self.copy_into_register(true) {
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
                    return self.osc52_effects(self.clipboard.osc52_text());
                }
                return Vec::new();
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
            Command::OpenBufferPicker => return self.open_picker(PickerMode::Buffer),
            Command::OpenDiffPicker => {
                // F6 over an open diff closes it instead of starting another
                // comparison, so the key that opens it also puts it away.
                if self.layout.is_diff() {
                    self.close_diff();
                } else {
                    return self.open_picker(PickerMode::Diff);
                }
            }
            Command::OpenCommandPalette => self.open_command_palette(),
            Command::OpenSearch => return self.open_search(false, SearchScope::CurrentBuffer),
            Command::OpenReplace => return self.open_search(true, SearchScope::CurrentBuffer),
            Command::OpenSearchInDirectory => {
                return self.open_search(false, SearchScope::Directory);
            }
            Command::CycleSearchScope => return self.cycle_search_scope(),
            Command::SearchCursorLeft => self.move_search_cursor(false),
            Command::SearchCursorRight => self.move_search_cursor(true),
            Command::SearchSelectAll => self.select_all_search_field(),
            Command::SearchSelectLeft => self.extend_search_selection(false),
            Command::SearchSelectRight => self.extend_search_selection(true),
            Command::SearchCopy => {
                if self.search().is_some() {
                    return self.copy_search_selection(false);
                }
                // The picker, go-to-line and rename overlays keep Ctrl+C as cancel.
                self.close_picker();
            }
            Command::SearchCut => return self.copy_search_selection(true),
            Command::SearchPaste => return self.paste_search_field(),
            Command::SearchUndo => return self.undo_search_field(false),
            Command::SearchRedo => return self.undo_search_field(true),
            Command::PickerUp => self.move_picker(-1),
            Command::PickerDown => self.move_picker(1),
            Command::PickerBackspace => {
                if let Some((_, digits)) = &mut self.goto_input {
                    digits.pop();
                    self.dirty = true;
                } else if let Some(rename) = &mut self.rename_input {
                    rename.pop();
                    self.dirty = true;
                } else if self.search().is_some() {
                    self.backspace_search_char();
                    return self.refresh_search();
                } else if let Some(picker) = &mut self.picker {
                    picker.query.pop();
                    return self.picker_query_changed();
                }
            }
            Command::PickerConfirm => return self.confirm_picker(),
            Command::PickerCancel => self.close_picker(),
            Command::Cancel => {
                if self.search().is_none() {
                    self.close_picker();
                }
            }
            Command::SearchToggleField => {
                if self.completion.is_some() {
                    return self.confirm_picker();
                }
                if let Some(search) = self.search_mut() {
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
                if let Some(search) = self.search_mut() {
                    search.filters.respect_ignore_files = !search.filters.respect_ignore_files;
                    return self.refresh_search();
                }
            }
            Command::SearchToggleHidden => {
                if let Some(search) = self.search_mut() {
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
            Command::GoToLine => {
                // Mirror the rename prompt: a modal single-line input on the
                // shared overlay focus. Remember the side so the jump lands in the
                // pane the caret was in, even in a split.
                self.hover = None;
                self.deferred_hover = None;
                self.completion = None;
                self.goto_input = Some((self.focused_side(), String::new()));
                self.focus = Focus::Overlay;
                self.dirty = true;
            }
            Command::Reload => {
                // Re-read the active file from disk. The editor already auto-reloads
                // unmodified buffers on external change; this covers the one case it
                // won't touch — a buffer with unsaved edits — by confirming before
                // discarding them.
                let Some(doc) = self
                    .layout
                    .active_editor(self.focus)
                    .map(|pane| pane.view.doc)
                else {
                    return Vec::new();
                };
                let Some(document) = self.documents.get(&doc) else {
                    return Vec::new();
                };
                let Some(path) = document.path.clone() else {
                    self.status = Some("再読込できるファイルがありません".to_owned());
                    return Vec::new();
                };
                let modified = document
                    .editable_opt()
                    .is_some_and(|editable| editable.modified);
                if modified {
                    self.confirm = Some(ConfirmState {
                        message: "未保存の変更を破棄して再読込しますか? [Enter / Esc]".to_owned(),
                        action: ConfirmAction::ReloadDiscard(doc),
                    });
                    self.focus = Focus::Overlay;
                    self.dirty = true;
                } else {
                    return vec![Effect::ReadFile { id: doc, path }];
                }
            }
            Command::Format => return self.request_formatting(),
            Command::ToggleShell => return self.toggle_shell(),
            Command::ToggleSplit => self.toggle_split(),
            Command::DiffNextHunk => self.jump_diff_hunk(true),
            Command::DiffPrevHunk => self.jump_diff_hunk(false),
            Command::CloseBuffer => return self.close_active_buffer(),
            Command::Indent => {
                // While filling in a snippet, Tab walks to the next stop; otherwise
                // it indents.
                if !self.advance_snippet_stop() {
                    self.indent_selected_lines(false);
                }
            }
            Command::Outdent => {
                if !self.retreat_snippet_stop() {
                    self.indent_selected_lines(true);
                }
            }
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
        if self.layout.is_shell() {
            // Hide the pane only. The session keeps running so that reopening
            // comes back to the same shell with its scrollback intact.
            self.set_right_pane(None);
            self.focus = Focus::Editor(Side::Left);
            return Vec::new();
        }
        self.set_right_pane(Some(RightPane::Shell));
        self.focus = Focus::Shell;
        self.dismiss_hover();
        if self.shell.is_some() {
            return Vec::new();
        }
        // Nothing to resume — either this is the first Ctrl+O or the last shell
        // exited, which drops the session precisely so this spawns a new one.
        //
        // A zero-sized grid panics inside vt100, and the size is still (0, 0)
        // until the first resize arrives.
        let rows = self.terminal_size.1.saturating_sub(1).max(1);
        let cols = split_right_width(self.terminal_size.0).max(1);
        let token = self.next_shell_token;
        self.next_shell_token += 1;
        self.shell = Some(ShellSession {
            token,
            parser: vt100::Parser::new(rows, cols, TERMINAL_SCROLLBACK_LINES),
            selection: None,
        });
        vec![Effect::SpawnShell {
            token,
            cols,
            rows,
            shell: self.config.editor.shell.clone(),
        }]
    }

    /// Leave the diff, back to the file that was on the left.
    fn close_diff(&mut self) {
        if !self.layout.is_diff() {
            return;
        }
        self.set_right_pane(None);
        self.focus = Focus::Editor(Side::Left);
    }

    fn toggle_split(&mut self) {
        if self.layout.is_editor_split() {
            self.set_right_pane(None);
            self.focus = Focus::Editor(Side::Left);
            return;
        }
        // Over any other right pane — the diff, find, the shell — this takes the
        // half over rather than merely dismissing it, the same way Ctrl+O and
        // Ctrl+F do. The diff has Esc and F6 for closing.
        let view = self.layout.left.view.clone();
        self.set_right_pane(Some(RightPane::Editor(EditorPane { view })));
        self.focus = Focus::Editor(Side::Right);
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
        self.last_views.remove(&id);
        if self.deferred_hover.is_some_and(|(doc, _)| doc == id) {
            self.deferred_hover = None;
        }
        self.pending_self_disk_updates.remove(&id);
        if let Some(next) = self.documents.keys().next().copied() {
            let view = self.view_for(next);
            self.show_only(view);
        } else {
            let id = DocumentId(self.next_doc_id);
            self.next_doc_id += 1;
            self.documents.insert(id, Document::scratch());
            self.show_only(View::new(id));
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
        // A replaced shell's reader thread reports its exit only after the
        // successor is already running, so events name the session they came
        // from and anything but the current one is ignored.
        if self
            .shell
            .as_ref()
            .is_none_or(|shell| shell.token != event.token())
        {
            return;
        }
        let dirty = match event {
            TerminalEvent::Output { bytes, .. } => {
                let shell = self.shell.as_mut().expect("checked above");
                shell.parser.process(&bytes);
                shell.selection.is_none()
            }
            TerminalEvent::Exited { error, .. } => {
                // Drop the session so the next Ctrl+O starts a new shell rather
                // than reopening a dead one.
                self.shell = None;
                if self.layout.is_shell() {
                    self.set_right_pane(None);
                    self.focus = Focus::Editor(Side::Left);
                }
                self.status = error;
                true
            }
        };
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
        // Esc also leaves snippet mode, so a later Tab indents again.
        self.clear_snippet_session();
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
        if self.copy_into_register(copy_caret_lines) {
            self.osc52_effects(self.clipboard.osc52_text())
        } else {
            Vec::new()
        }
    }

    /// Copy the active selection — or, when every cursor is a bare caret and
    /// `copy_caret_lines` is set, the caret lines — into the internal register.
    /// Returns whether anything was captured. Kept separate from OSC 52 emission
    /// so callers like Cut can act on "did we copy" regardless of the (optional)
    /// clipboard push.
    fn copy_into_register(&mut self, copy_caret_lines: bool) -> bool {
        let focus = self.focus;
        let Some(pane) = self.layout.active_editor(focus) else {
            return false;
        };
        let Some(document) = self.documents.get(&pane.view.doc) else {
            return false;
        };
        if let Some(large) = document.large() {
            let selection = pane.view.selections.primary();
            let start = selection.anchor.0.min(selection.head.0);
            let end = selection.anchor.0.max(selection.head.0);
            let fragments: Vec<_> = (start..=end).filter_map(|line| large.line(line)).collect();
            if fragments.is_empty() {
                return false;
            }
            self.clipboard.store(vec![fragments.join("\n")]);
            return true;
        }
        let Some(editable) = document.editable_opt() else {
            return false;
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
            return false;
        }
        if linewise {
            self.clipboard.store_linewise(fragments);
        } else {
            self.clipboard.store(fragments);
        }
        true
    }

    /// Wrap copied text in an OSC 52 effect only when the config enables it. The
    /// in-editor register is updated regardless (so copy/paste inside the editor
    /// always works); this only controls whether we also push to the host OS
    /// clipboard, which garbles terminals that don't support OSC 52.
    fn osc52_effects(&self, text: String) -> Vec<Effect> {
        if self.config.editor.osc52_clipboard {
            vec![Effect::ClipboardOsc52(text)]
        } else {
            Vec::new()
        }
    }

    fn copy_shell_selection(&self) -> Vec<Effect> {
        let Some(selection) = self
            .shell
            .as_ref()
            .and_then(|shell| shell.selection.as_ref())
        else {
            return Vec::new();
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
        if text.is_empty() {
            Vec::new()
        } else {
            self.osc52_effects(text)
        }
    }

    fn terminal_mouse_position(&self, column: u16, row: u16) -> Option<(u16, u16)> {
        if !self.layout.is_shell() {
            return None;
        }
        let start = split_left_width(self.terminal_size.0).saturating_add(1);
        let screen = self.shell.as_ref()?.parser.screen();
        let (rows, cols) = screen.size();
        if column < start || row >= rows || cols == 0 {
            return None;
        }
        Some((row, column.saturating_sub(start).min(cols - 1)))
    }

    /// Lines advanced per mouse-wheel notch. Larger covers the same distance in
    /// fewer redraws — cheaper over SSH, where each redraw is a round-trip.
    const MOUSE_WHEEL_SCROLL_LINES: isize = 5;

    fn apply_mouse(&mut self, input: MouseInput) {
        let mouse = input.event;
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.scroll_at(mouse.column, -Self::MOUSE_WHEEL_SCROLL_LINES)
            }
            MouseEventKind::ScrollDown => {
                self.scroll_at(mouse.column, Self::MOUSE_WHEEL_SCROLL_LINES)
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let divider = split_left_width(self.terminal_size.0);
                if self.layout.right.is_some() && mouse.column == divider {
                    return;
                }
                let right_half = mouse.column > divider;
                if self.layout.right_editor().is_some() {
                    self.focus = Focus::Editor(if right_half { Side::Right } else { Side::Left });
                } else if self.search().is_some() {
                    // The find pane holds focus as an overlay. Clicking the
                    // document has to hand focus back, or keystrokes keep going
                    // to the query box. (A click inside the pane never reaches
                    // here — `search_pane_click` consumes it.)
                    self.focus = Focus::Editor(Side::Left);
                } else if self.layout.is_shell() {
                    if right_half {
                        self.focus = Focus::Shell;
                        self.dismiss_hover();
                        let point = self.terminal_mouse_position(mouse.column, mouse.row);
                        if let Some(shell) = self.shell.as_mut()
                            && let Some(point) = point
                        {
                            shell.selection = Some(TerminalSelection {
                                anchor: point,
                                head: point,
                                snapshot: shell.parser.screen().clone(),
                            });
                        }
                        self.dirty = true;
                        return;
                    }
                    self.focus = Focus::Editor(Side::Left);
                    if let Some(shell) = self.shell.as_mut() {
                        shell.selection = None;
                    }
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
                    let point = self.terminal_mouse_position(mouse.column, mouse.row);
                    if let Some(point) = point
                        && let Some(selection) = self
                            .shell
                            .as_mut()
                            .and_then(|shell| shell.selection.as_mut())
                    {
                        selection.head = point;
                        self.dirty = true;
                    }
                    return;
                }
                let Some(anchor) = self.drag_anchor else {
                    return;
                };
                // Extend past the viewport: when the drag reaches the top or bottom
                // edge of the text area, scroll a line at a time so the selection can
                // grow beyond what's on screen. Terminals only emit drag events on
                // movement, so holding still at the edge just stops scrolling — no
                // timer, and no runaway once the document end is reached (scroll_at
                // clamps and mouse_position then lands on the last position).
                let text_bottom = self.terminal_size.1.saturating_sub(2);
                let head = if mouse.row == 0 {
                    self.scroll_at(mouse.column, -1);
                    self.mouse_position(mouse.column, 0)
                } else if mouse.row >= text_bottom {
                    self.scroll_at(mouse.column, 1);
                    self.mouse_position(mouse.column, text_bottom)
                } else {
                    self.mouse_position(mouse.column, mouse.row)
                };
                let Some(head) = head else {
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
                    && let Some(shell) = self.shell.as_mut()
                    && shell
                        .selection
                        .as_ref()
                        .is_some_and(|selection| selection.anchor == selection.head)
                {
                    shell.selection = None;
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn open_picker(&mut self, mode: PickerMode) -> Vec<Effect> {
        if self
            .picker
            .as_ref()
            .is_some_and(|picker| picker.mode == mode)
        {
            self.close_picker();
            return Vec::new();
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
            return Vec::new();
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
        // Diff also offers files that are not open yet, so an empty buffer list
        // is not a dead end there.
        if candidates.is_empty() && mode != PickerMode::Diff {
            self.status = Some("他に開いているバッファがありません".to_owned());
            self.dirty = true;
            return Vec::new();
        }
        let scan_token = (mode == PickerMode::Diff).then(|| {
            let token = self.next_scan_token;
            self.next_scan_token += 1;
            token
        });
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
            scan_token,
            path_listing_start: None,
        });
        self.focus = Focus::Overlay;
        self.dirty = true;
        scan_token
            .map(|token| {
                self.set_progress("file-scan", "ファイルを走査中…");
                vec![Effect::StartFileScan {
                    root: self.workspace_root.clone(),
                    respect_ignore_files: self.respect_ignore_files(),
                    token,
                }]
            })
            .unwrap_or_default()
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
            path_listing_start: None,
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
            path_listing_start: None,
        });
        self.focus = Focus::Overlay;
        self.status = Some("ファイルを走査中…".to_owned());
        self.set_progress("file-scan", "ファイルを走査中…");
        self.dirty = true;
        vec![Effect::StartFileScan {
            root: self.workspace_root.clone(),
            respect_ignore_files: self.respect_ignore_files(),
            token,
        }]
    }

    fn dismiss_search_for_overlay(&mut self) {
        self.take_search();
    }

    fn apply_file_scan(&mut self, event: FileScanEvent) {
        match event {
            FileScanEvent::Batch { token, paths } => {
                if self.picker.as_ref().and_then(|picker| picker.scan_token) == Some(token) {
                    // The diff picker lists open buffers as candidates first, so a
                    // scanned file that is already open would appear twice — drop the
                    // duplicate there. The file picker (Ctrl+T) starts empty and lists
                    // every file, so it must not hide files just because they are open.
                    let open: HashSet<_> = if self.picker.as_ref().map(|picker| picker.mode)
                        == Some(PickerMode::Diff)
                    {
                        self.documents
                            .values()
                            .filter_map(|document| document.path.clone())
                            .collect()
                    } else {
                        HashSet::new()
                    };
                    let picker = self.picker.as_mut().expect("checked above");
                    let start = picker.candidates.len();
                    picker.candidates.extend(
                        paths
                            .into_iter()
                            .filter(|path| !open.contains(path))
                            .map(PickerCandidate::Path),
                    );
                    picker.ranking_cache.clear();
                    if picker.query.is_empty() {
                        picker.filtered.extend(start..picker.candidates.len());
                    } else {
                        self.refresh_picker();
                    }
                }
            }
            FileScanEvent::PathCompletions { token, paths } => {
                let Some(picker) = &mut self.picker else {
                    return;
                };
                if picker.scan_token != Some(token) {
                    return;
                }
                // The whole answer for the path typed so far, so it replaces the
                // previous listing rather than adding to it. The picker's own
                // candidates stay underneath for when the query stops being a path.
                let start = picker.path_listing_start.unwrap_or(picker.candidates.len());
                picker.candidates.truncate(start);
                picker
                    .candidates
                    .extend(paths.into_iter().map(PickerCandidate::Path));
                picker.path_listing_start = Some(start);
                picker.filtered = (start..picker.candidates.len()).collect();
                picker.ranking_cache.clear();
                picker.selected = 0;
                picker.scan_token = None;
                self.finish_progress("file-scan");
                self.dirty = true;
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
        if self.layout.search().is_some() {
            // Ctrl+F only toggles the pane's visibility; scope and options are
            // adjusted with the mouse inside the pane.
            return self.close_search_pane();
        }
        // Replace is opt-in via the checkbox; a fresh pane starts as find-only.
        let _ = replace;
        self.picker = None;
        self.dismiss_hover();
        let search = SearchState {
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
                respect_ignore_files: self.respect_ignore_files(),
                include_hidden: self.config.search.include_hidden,
            },
            hits: Vec::new(),
            current: None,
            grep_token: None,
            field_cursor: 0,
            field_anchor: None,
            field_undo: Vec::new(),
            field_redo: Vec::new(),
            results_scroll: 0,
        };
        self.set_right_pane(Some(RightPane::Search(Box::new(search))));
        self.focus = Focus::Overlay;
        Vec::new()
    }

    fn toggle_replace_field(&mut self) {
        if let Some(search) = self.search_mut() {
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

    /// Jump to the file for the given result index, leaving the pane open. The
    /// match lands selected, so a click has a visible result at the destination.
    pub(crate) fn open_search_hit(&mut self, index: usize) -> Vec<Effect> {
        let Some((hit, options, query)) = self.search().and_then(|search| {
            Some((
                search.hits.get(index).cloned()?,
                search.options,
                search.query.clone(),
            ))
        }) else {
            return Vec::new();
        };
        // Mark the row as the current hit so the pane shows which result the
        // editor is parked on.
        if let Some(search) = self.search_mut() {
            search.current = Some(index);
        }
        self.record_jump_origin();
        // Keep focus in the find pane (it holds focus as an overlay): the pane
        // stays open, its caret stays drawn, and typing has to keep going to the
        // query. Moving focus to the buffer here left the caret sitting in the
        // find field while keystrokes landed in the document. The jump still
        // lands — `active_editor_mut` resolves Overlay to the left pane.
        self.focus = Focus::Overlay;
        let effects = match hit {
            SearchHit::Buffer { doc, range, .. } => {
                let mut view = View::new(doc);
                view.selections.set_single(Selection {
                    anchor: CharIdx(range.start),
                    head: CharIdx(range.end),
                });
                // Show the hit in the left pane but keep the find pane open, so a
                // list of matches can be walked one click at a time.
                self.layout.left = EditorPane { view };
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
                    self.layout.left = EditorPane { view };
                    Vec::new()
                } else {
                    // Not open yet: land on the matched line once it loads instead
                    // of opening at the top of the file. grep reports the line but
                    // not the column, so re-run the pattern to select the match
                    // itself — landing on a bare column 0 gives no sign of a hit.
                    let columns = search_pattern(&query, options)
                        .ok()
                        .and_then(|pattern| pattern.find(&hit.text))
                        .map_or((0, 0), |found| {
                            let start = hit.text[..found.start()].chars().count() as u32;
                            (start, start + found.as_str().chars().count() as u32)
                        });
                    let line = hit.line as u32;
                    self.open_path_at(
                        hit.path,
                        lsp_types::Range::new(
                            lsp_types::Position::new(line, columns.0),
                            lsp_types::Position::new(line, columns.1),
                        ),
                    )
                }
            }
        };
        self.dirty = true;
        effects
    }

    /// The "Run Replace" button: replace every match in one shot.
    fn run_replace(&mut self) -> Vec<Effect> {
        let Some(search) = self.take_search() else {
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
            if let SearchHit::Buffer { doc, range, .. } = hit {
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
        if self.take_search().is_some() {
            self.focus = Focus::Editor(self.focused_side());
        }
        Vec::new()
    }

    fn set_search_scope(&mut self, scope: SearchScope) -> Vec<Effect> {
        if let Some(search) = self.search_mut() {
            if search.scope == scope {
                return Vec::new();
            }
            search.scope = scope;
        }
        self.refresh_search()
    }

    /// The right-half rectangle occupied by the search pane: `(x, y, width, height)`.
    pub(crate) fn search_pane_rect(&self) -> (u16, u16, u16, u16) {
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
        // Any click inside the pane hands it focus. The document can hold focus
        // while the pane stays open, so clicking back into it has to take the
        // caret back — otherwise the field looks active but keystrokes keep
        // going to the buffer. This is the counterpart to `apply_mouse` giving
        // focus to the document when a click lands outside the pane.
        self.focus = Focus::Overlay;
        let search = self.search()?;
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
            self.focus_search_field(None, false, Some(self.search_field_index_at(column)));
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
            self.focus_search_field(None, true, Some(self.search_field_index_at(column)));
            return Some(Vec::new());
        }
        if let Some(top) = layout.include_top
            && in_box(relative, top)
        {
            self.focus_search_field(
                Some(SearchFilterField::Include),
                false,
                Some(self.search_field_index_at(column)),
            );
            return Some(Vec::new());
        }
        if let Some(top) = layout.exclude_top
            && in_box(relative, top)
        {
            self.focus_search_field(
                Some(SearchFilterField::Exclude),
                false,
                Some(self.search_field_index_at(column)),
            );
            return Some(Vec::new());
        }
        if relative == layout.results_top {
            // The header row carries the hit count and the reload button.
            let (start, end) = search_reload_button_range(pane_x, pane_width);
            if column >= start && column < end {
                return Some(self.refresh_search());
            }
            return Some(Vec::new());
        }
        if relative > layout.results_top {
            let index = usize::from(relative - layout.results_top - 1)
                + self.search().map_or(0, |search| search.results_scroll);
            return Some(self.open_search_hit(index));
        }
        Some(Vec::new())
    }

    /// Focus one of the pane's fields. `cursor` is the caret's character index —
    /// where a click landed — or None to sit at the end. The caret doubles as the
    /// anchor so a drag from here extends a selection; equal anchor and caret
    /// read as no selection, so a plain click just moves the caret.
    fn focus_search_field(
        &mut self,
        filter: Option<SearchFilterField>,
        replace: bool,
        cursor: Option<usize>,
    ) {
        if let Some(search) = self.search_mut() {
            let replace = replace && search.replacement.is_some();
            // The undo history holds snapshots of one field's text; carrying it
            // across a switch would restore it into the wrong field.
            if (search.editing_filter, search.editing_replace) != (filter, replace) {
                search.field_undo.clear();
                search.field_redo.clear();
            }
            search.editing_filter = filter;
            search.editing_replace = replace;
            let at = cursor.unwrap_or(usize::MAX).min(search_field_len(search));
            search.field_cursor = at;
            search.field_anchor = Some(at);
            self.dirty = true;
        }
    }

    /// Character index in the active field for a click at `column`, measured from
    /// the field box's inner edge.
    fn search_field_index_at(&self, column: u16) -> usize {
        let (pane_x, _, _, _) = self.search_pane_rect();
        usize::from(column.saturating_sub(pane_x + 1))
    }

    /// Shift+Left / Shift+Right: move the caret, keeping (or starting) a
    /// selection anchored where it was.
    fn extend_search_selection(&mut self, right: bool) {
        if let Some(search) = self.search_mut() {
            let len = search_field_len(search);
            let anchor = search.field_anchor.unwrap_or(search.field_cursor);
            search.field_anchor = Some(anchor);
            search.field_cursor = if right {
                (search.field_cursor + 1).min(len)
            } else {
                search.field_cursor.saturating_sub(1)
            };
            self.dirty = true;
        }
    }

    /// Dragging inside the active field sweeps a selection out from the press
    /// point. Returns false when the drag is not over that field, so a drag
    /// across the results does not move the caret.
    fn drag_search_selection(&mut self, column: u16, row: u16) -> bool {
        let (pane_x, pane_y, pane_width, _) = self.search_pane_rect();
        if column < pane_x || column >= pane_x + pane_width || row < pane_y {
            return false;
        }
        let Some(search) = self.search() else {
            return false;
        };
        let layout = search_pane_layout(
            search.scope == SearchScope::Directory,
            search.replacement.is_some(),
        );
        // The box the caret currently lives in.
        let top = match (search.editing_filter, search.editing_replace) {
            (Some(SearchFilterField::Include), _) => layout.include_top,
            (Some(SearchFilterField::Exclude), _) => layout.exclude_top,
            (None, true) => layout.replace_top,
            (None, false) => Some(layout.find_top),
        };
        let relative = row - pane_y;
        // A 3-row box; the value sits on its middle row.
        if top.is_none_or(|top| relative != top + 1) {
            return false;
        }
        let at = self.search_field_index_at(column);
        if let Some(search) = self.search_mut() {
            let len = search_field_len(search);
            search.field_cursor = at.min(len);
            self.dirty = true;
        }
        true
    }

    fn cycle_search_scope(&mut self) -> Vec<Effect> {
        if let Some(search) = self.search_mut() {
            search.scope = match search.scope {
                SearchScope::CurrentBuffer => SearchScope::AllBuffers,
                SearchScope::AllBuffers => SearchScope::Directory,
                SearchScope::Directory => SearchScope::CurrentBuffer,
            };
        }
        self.refresh_search()
    }

    fn refresh_search(&mut self) -> Vec<Effect> {
        let Some(search) = self.search() else {
            return Vec::new();
        };
        let query = search.query.clone();
        let scope = search.scope;
        let options = search.options;
        let mut filters = search.filters.clone();
        filters.include = split_globs(&search.include_input);
        filters.exclude = split_globs(&search.exclude_input);
        if query.is_empty() {
            if let Some(search) = self.search_mut() {
                search.hits.clear();
                search.current = None;
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
            if let Some(search) = self.search_mut() {
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
                // Snapshot the row now, while `range` still describes this text.
                let (line, column) =
                    crate::position::char_idx_to_line_col(editable.text(), CharIdx(start));
                let raw = editable.text().line(line).to_string();
                let trimmed_away = raw.chars().take_while(|c| c.is_whitespace()).count();
                hits.push(SearchHit::Buffer {
                    doc: *id,
                    range: start..end,
                    preview: HitPreview {
                        line,
                        text: raw.trim().to_owned(),
                        column: column.saturating_sub(trimmed_away),
                    },
                });
            }
        }
        if let Some(search) = self.search_mut() {
            search.hits = hits;
            search.current = search
                .current
                .filter(|current| *current < search.hits.len());
        }
        self.dirty = true;
        Vec::new()
    }

    fn toggle_search_option(&mut self, toggle: impl FnOnce(&mut SearchOptions)) -> Vec<Effect> {
        if let Some(search) = self.search_mut() {
            toggle(&mut search.options);
            return self.refresh_search();
        }
        Vec::new()
    }

    fn apply_grep(&mut self, event: GrepEvent) {
        let Some(search) = self.search_mut() else {
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
        let search = self.search_mut()?;
        match search.editing_filter {
            Some(SearchFilterField::Include) => Some(&mut search.include_input),
            Some(SearchFilterField::Exclude) => Some(&mut search.exclude_input),
            None if search.editing_replace => search.replacement.as_mut(),
            None => Some(&mut search.query),
        }
    }

    fn active_search_field(&self) -> Option<&str> {
        let search = self.search()?;
        match search.editing_filter {
            Some(SearchFilterField::Include) => Some(search.include_input.as_str()),
            Some(SearchFilterField::Exclude) => Some(search.exclude_input.as_str()),
            None if search.editing_replace => search.replacement.as_deref(),
            None => Some(search.query.as_str()),
        }
    }

    /// The active field's selection as an ordered char range, if one is active.
    fn search_selection(&self) -> Option<std::ops::Range<usize>> {
        let search = self.search()?;
        let anchor = search.field_anchor?;
        let cursor = search.field_cursor;
        (anchor != cursor).then(|| anchor.min(cursor)..anchor.max(cursor))
    }

    fn search_selected_text(&self) -> Option<String> {
        let range = self.search_selection()?;
        let field = self.active_search_field()?;
        Some(
            field
                .chars()
                .skip(range.start)
                .take(range.len())
                .collect::<String>(),
        )
    }

    /// Snapshot the active field before mutating it, so Ctrl+Z can come back.
    /// Any edit invalidates the redo stack, as in the buffer's own history.
    fn push_search_undo(&mut self) {
        let Some(text) = self.active_search_field().map(str::to_owned) else {
            return;
        };
        let Some(search) = self.search_mut() else {
            return;
        };
        let cursor = search.field_cursor;
        search.field_undo.push(FieldSnapshot { text, cursor });
        search.field_redo.clear();
    }

    /// Replace the selection (or insert at the caret when there is none) and
    /// leave the caret after the inserted text. Used by typing, paste and cut.
    fn replace_search_selection(&mut self, with: &str) {
        let selection = self.search_selection();
        let cursor = self.search().map_or(0, |search| search.field_cursor);
        let range = selection.unwrap_or(cursor..cursor);
        let Some(field) = self.active_search_field_mut() else {
            return;
        };
        let start = char_byte_index(field, range.start);
        let end = char_byte_index(field, range.end);
        field.replace_range(start..end, with);
        if let Some(search) = self.search_mut() {
            search.field_cursor = range.start + with.chars().count();
            search.field_anchor = None;
        }
    }

    fn insert_search_char(&mut self, character: char) {
        self.push_search_undo();
        let mut buffer = [0u8; 4];
        self.replace_search_selection(character.encode_utf8(&mut buffer));
    }

    fn backspace_search_char(&mut self) {
        // With a selection, Backspace deletes it rather than one character.
        if self.search_selection().is_some() {
            self.push_search_undo();
            self.replace_search_selection("");
            return;
        }
        let cursor = self.search().map_or(0, |search| search.field_cursor);
        if cursor == 0 {
            return;
        }
        self.push_search_undo();
        if let Some(field) = self.active_search_field_mut() {
            let end = char_byte_index(field, cursor);
            let start = char_byte_index(field, cursor - 1);
            field.replace_range(start..end, "");
        }
        if let Some(search) = self.search_mut() {
            search.field_cursor = cursor - 1;
            search.field_anchor = None;
        }
    }

    fn move_search_cursor(&mut self, right: bool) {
        if let Some(search) = self.search_mut() {
            let len = search_field_len(search);
            search.field_cursor = if right {
                (search.field_cursor + 1).min(len)
            } else {
                search.field_cursor.saturating_sub(1)
            };
            // Moving the caret drops the selection, as in the buffer.
            search.field_anchor = None;
            self.dirty = true;
        }
    }

    fn select_all_search_field(&mut self) {
        if let Some(search) = self.search_mut() {
            let len = search_field_len(search);
            search.field_anchor = Some(0);
            search.field_cursor = len;
            self.dirty = true;
        }
    }

    /// Ctrl+C / Ctrl+X in the find pane. Copies the selection into the same
    /// register the buffer uses, so it can be pasted either side.
    fn copy_search_selection(&mut self, cut: bool) -> Vec<Effect> {
        let Some(text) = self.search_selected_text() else {
            return Vec::new();
        };
        self.clipboard.store(vec![text]);
        if cut {
            self.push_search_undo();
            self.replace_search_selection("");
            self.dirty = true;
            return self.refresh_search();
        }
        self.dirty = true;
        Vec::new()
    }

    fn paste_search_field(&mut self) -> Vec<Effect> {
        let fragments = self.clipboard.fragments().to_vec();
        if fragments.is_empty() {
            return Vec::new();
        }
        // The fields are single-line; a multi-line register joins with spaces
        // rather than pasting newlines that the field cannot show.
        let text = fragments.join(" ").replace(['\n', '\r'], " ");
        self.push_search_undo();
        self.replace_search_selection(&text);
        self.dirty = true;
        self.refresh_search()
    }

    fn undo_search_field(&mut self, redo: bool) -> Vec<Effect> {
        let Some(current) = self.active_search_field().map(str::to_owned) else {
            return Vec::new();
        };
        let Some(search) = self.search_mut() else {
            return Vec::new();
        };
        let cursor = search.field_cursor;
        let stack = if redo {
            &mut search.field_redo
        } else {
            &mut search.field_undo
        };
        let Some(snapshot) = stack.pop() else {
            return Vec::new();
        };
        let restored = snapshot.clone();
        let opposite = FieldSnapshot {
            text: current,
            cursor,
        };
        if redo {
            search.field_undo.push(opposite);
        } else {
            search.field_redo.push(opposite);
        }
        search.field_cursor = restored.cursor;
        search.field_anchor = None;
        if let Some(field) = self.active_search_field_mut() {
            *field = restored.text;
        }
        self.dirty = true;
        self.refresh_search()
    }

    /// Walk the result list with the arrow keys, opening each hit as it is
    /// reached so Up/Down previews matches the way clicking a row does.
    fn step_search_result(&mut self, amount: isize) {
        let Some(search) = self.search() else { return };
        let count = search.hits.len();
        if count == 0 {
            return;
        }
        let next = match search.current {
            // Nothing opened yet: the first step lands on an end of the list.
            None if amount < 0 => count - 1,
            None => 0,
            Some(current) if amount < 0 => current.saturating_sub(amount.unsigned_abs()),
            Some(current) => (current + amount as usize).min(count - 1),
        };
        self.open_search_hit(next);
        self.scroll_result_into_view(next);
    }

    /// Keep the current row inside the visible slice of the result list.
    fn scroll_result_into_view(&mut self, index: usize) {
        let (_, _, _, pane_height) = self.search_pane_rect();
        let Some(search) = self.search() else { return };
        let layout = search_pane_layout(
            search.scope == SearchScope::Directory,
            search.replacement.is_some(),
        );
        // The list starts below its own border and ends at the pane's bottom.
        let visible = usize::from(
            pane_height
                .saturating_sub(layout.results_top)
                .saturating_sub(1),
        )
        .max(1);
        if let Some(search) = self.search_mut() {
            if index < search.results_scroll {
                search.results_scroll = index;
            } else if index >= search.results_scroll + visible {
                search.results_scroll = index + 1 - visible;
            }
            self.dirty = true;
        }
    }

    fn scroll_search_results(&mut self, delta: isize) {
        if let Some(search) = self.search_mut() {
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
        if let Some((_, digits)) = &mut self.goto_input {
            // A line number: ignore everything but digits.
            if character.is_ascii_digit() {
                digits.push(character);
                self.dirty = true;
            }
            return Vec::new();
        }
        if let Some(rename) = &mut self.rename_input {
            rename.push(character);
            self.dirty = true;
            return Vec::new();
        }
        if self.search().is_some() {
            self.insert_search_char(character);
            return self.refresh_search();
        }
        if let Some(picker) = &mut self.picker {
            picker.query.push(character);
            self.dirty = true;
            return self.picker_query_changed();
        }
        Vec::new()
    }

    /// Re-rank after the query changed, and ask for a directory listing when the
    /// query became a path — the workspace scan cannot answer those.
    fn picker_query_changed(&mut self) -> Vec<Effect> {
        let Some(picker) = &mut self.picker else {
            return Vec::new();
        };
        if !picker.accepts_paths() || !is_path_query(&picker.query) {
            // Back to a fuzzy pattern: drop the listing so the picker's own
            // candidates are what gets ranked.
            if let Some(start) = picker.path_listing_start.take() {
                picker.candidates.truncate(start);
                picker.ranking_cache.clear();
                picker.selected = 0;
            }
            self.refresh_picker();
            return Vec::new();
        }
        let input = picker.query.clone();
        let token = self.next_scan_token;
        self.next_scan_token += 1;
        self.picker.as_mut().expect("picker exists").scan_token = Some(token);
        self.refresh_picker();
        vec![Effect::ListPathCompletions {
            input,
            root: self.workspace_root.clone(),
            token,
        }]
    }

    fn refresh_picker(&mut self) {
        let Some(picker) = &self.picker else {
            return;
        };
        let query = picker.query.clone();
        if let Some(start) = picker.path_listing_start {
            // Already prefix-filtered by the directory listing; fuzzy-matching a
            // path against workspace-relative labels would only throw it away.
            let ranking: Vec<_> = (start..picker.candidates.len()).collect();
            let picker = self.picker.as_mut().expect("picker exists");
            picker.selected = picker.selected.min(ranking.len().saturating_sub(1));
            picker.filtered = ranking;
            self.dirty = true;
            return;
        }
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
                matcher.fuzzy_match(&label, &query).map(|score| {
                    (
                        index,
                        score,
                        matches!(candidate, PickerCandidate::Document(_)),
                    )
                })
            })
            .collect();
        // Equal scores go to the open buffer: the diff picker offers unopened
        // files too, and what you already have open is the likelier target.
        scored.sort_by_key(|(_, score, is_open_buffer)| {
            (
                std::cmp::Reverse(*score),
                std::cmp::Reverse(*is_open_buffer),
            )
        });
        let picker = self.picker.as_mut().expect("picker exists");
        picker.filtered = scored.into_iter().map(|(index, _, _)| index).collect();
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
        if self.search().is_some() {
            self.step_search_result(amount);
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
                ConfirmAction::ReloadDiscard(doc) => {
                    let Some(path) = self.documents.get(&doc).and_then(|d| d.path.clone()) else {
                        return Vec::new();
                    };
                    return vec![Effect::ReadFile { id: doc, path }];
                }
                ConfirmAction::QuitDiscard => {
                    self.quit = true;
                    return vec![Effect::Quit];
                }
            }
        }
        if let Some((side, digits)) = self.goto_input.take() {
            self.focus = Focus::Editor(side);
            // Line numbers are 1-based; clamp to the last line so a too-large
            // number lands at the end rather than doing nothing.
            let target = digits
                .parse::<usize>()
                .ok()
                .filter(|line| *line >= 1)
                .and_then(|line| {
                    let doc = self
                        .layout
                        .active_editor(self.focus)
                        .map(|pane| pane.view.doc)?;
                    let editable = self.documents.get(&doc).and_then(Document::editable_opt)?;
                    let text = editable.text();
                    let last = text.len_lines().saturating_sub(1);
                    Some((doc, CharIdx(text.line_to_char((line - 1).min(last)))))
                });
            if let Some((doc, head)) = target {
                self.record_jump_origin();
                self.go_to_location(doc, head);
            } else {
                self.dirty = true;
            }
            return Vec::new();
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
                let prefix_len = candidate.prefix_len;
                self.focus = Focus::Editor(completion.return_side);
                if let Some(body) = candidate.snippet_body.clone() {
                    self.expand_snippet_body(&body, prefix_len);
                } else {
                    let insert = candidate.insert.clone();
                    let cursor_back = candidate.cursor_back;
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
            }
            return Vec::new();
        }
        if self.search().is_some() {
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
                let view = self.view_for(target);
                if let Some(pane) = self.layout.active_editor_mut(self.focus) {
                    pane.view = view;
                } else {
                    self.show_only(view);
                    final_side = Side::Left;
                }
            }
            (PickerMode::Diff, PickerCandidate::Path(path)) => {
                let (target, load) = self.document_for_path(path);
                effects.extend(load);
                self.layout.left = EditorPane {
                    view: View::new(picker.base),
                };
                self.set_right_pane(Some(RightPane::Diff(DiffPane {
                    pane: EditorPane {
                        view: View::new(target),
                    },
                    top_row: 0,
                })));
                final_side = Side::Left;
            }
            (PickerMode::Diff, PickerCandidate::Document(target)) => {
                self.layout.left = EditorPane {
                    view: View::new(picker.base),
                };
                self.set_right_pane(Some(RightPane::Diff(DiffPane {
                    pane: EditorPane {
                        view: View::new(target),
                    },
                    top_row: 0,
                })));
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
        let scanning = self
            .picker
            .as_ref()
            .is_some_and(|picker| picker.scan_token.is_some());
        let return_side = self
            .completion
            .as_ref()
            .map(|completion| completion.return_side)
            .or_else(|| self.picker.as_ref().map(|picker| picker.return_side))
            .unwrap_or(Side::Left);
        self.picker = None;
        self.take_search();
        self.completion = None;
        self.rename_input = None;
        self.goto_input = None;
        self.confirm = None;
        if scanning {
            self.finish_progress("file-scan");
            if self.status.as_deref() == Some("ファイルを走査中…") {
                self.status = None;
            }
        }
        self.focus = Focus::Editor(return_side);
        self.dirty = true;
    }

    /// How a document is named in pickers and search results: relative to the
    /// workspace root, so a list of hits stays readable instead of repeating the
    /// same long absolute prefix on every row.
    fn document_label(&self, id: DocumentId) -> String {
        self.documents
            .get(&id)
            .and_then(|document| document.path.as_ref())
            .map_or_else(
                || format!("Untitled {}", id.0),
                |path| self.display_path(path),
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
        // Only wait on semantic tokens if the server actually provides them;
        // otherwise the status would sit on "coloring" forever (e.g. pylsp).
        if server.semantic_legend.is_some() {
            match lsp.semantic_ready_version() {
                None => return format!("<lsp> {language}: coloring"),
                Some(version) if version < lsp.version() => {
                    return format!("<lsp> {language}: updating");
                }
                Some(_) => {}
            }
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
        let pane_width = self.active_pane_width().max(1);
        let text_width = pane_width.saturating_sub(gutter_width as u16).max(1);
        let local_column = if matches!(self.focus, Focus::Editor(Side::Right)) {
            column.saturating_sub(split_left_width(self.terminal_size.0).saturating_add(1))
        } else {
            column
        };
        if self.layout.is_diff() {
            // Screen rows are aligned diff rows, which carry the line number
            // each side contributes — the panes scroll together and neither
            // side's line numbers run consecutively down the screen.
            let rows = self.diff_rows()?;
            let height = usize::from(self.terminal_size.1.saturating_sub(1));
            let start = self.diff_top_row().min(rows.len().saturating_sub(height));
            let entry = rows.get(start + usize::from(row))?;
            let side = if matches!(self.focus, Focus::Editor(Side::Right)) {
                &entry.right
            } else {
                &entry.left
            };
            // A blank row is where this side has no line at all; nothing to aim at.
            let (line, _) = side.as_ref()?;
            let display_col = usize::from(local_column.saturating_sub(crate::diff::GUTTER_WIDTH));
            return Some(display_col_to_char_idx(text, *line, display_col, tab_size));
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

    /// Scroll the pane the mouse is over, not the focused one — hovering a split
    /// and spinning the wheel scrolls whichever side the cursor sits on, which is
    /// what other editors do. Focus is left untouched.
    fn scroll_at(&mut self, column: u16, amount: isize) {
        if self.layout.is_diff() {
            self.scroll_diff(amount);
            return;
        }
        let over_right =
            self.layout.right_editor().is_some() && column > split_left_width(self.terminal_size.0);
        let focus = Focus::Editor(if over_right { Side::Right } else { Side::Left });
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
        let Some(shell) = self.shell.as_mut() else {
            return;
        };
        let current = shell.parser.screen().scrollback();
        let target = if amount < 0 {
            current.saturating_sub(amount.unsigned_abs())
        } else {
            current.saturating_add(amount as usize)
        };
        shell.parser.set_scrollback(target);
        shell.selection = None;
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
        self.dismiss_signature_help();
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
        let pane_cols = usize::from(self.active_pane_width()).max(1);
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

/// Completion candidates for the language's snippets whose prefix starts with
/// what has been typed. Empty when nothing is typed or the language has none.
fn snippet_candidates(language: &str, prefix: &str) -> Vec<CompletionCandidate> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let prefix_lower = prefix.to_lowercase();
    let prefix_len = prefix.chars().count();
    crate::snippet::snippets_for(language)
        .iter()
        .filter(|snippet| snippet.prefix.to_lowercase().starts_with(&prefix_lower))
        .map(|snippet| CompletionCandidate {
            label: format!("{}  (snippet)", snippet.prefix),
            insert: String::new(),
            prefix_len,
            cursor_back: 0,
            snippet_body: Some(snippet.body.to_owned()),
        })
        .collect()
}

/// The whole word the caret sits on or immediately after, or None when the caret
/// is not touching a word. Used as the ctags lookup symbol.
fn word_at(text: &ropey::Rope, caret: CharIdx) -> Option<String> {
    let len = text.len_chars();
    let cursor = caret.0.min(len);
    let adjacent = if cursor < len && is_word(text.char(cursor)) {
        cursor
    } else if cursor > 0 && is_word(text.char(cursor - 1)) {
        cursor - 1
    } else {
        return None;
    };
    let mut start = adjacent;
    while start > 0 && is_word(text.char(start - 1)) {
        start -= 1;
    }
    let mut end = adjacent + 1;
    while end < len && is_word(text.char(end)) {
        end += 1;
    }
    Some(text.slice(start..end).to_string())
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
        description: "比較対象を選び左右diff表示。表示中はEsc/F6で閉じる",
        command: Command::OpenDiffPicker,
    },
    CommandPaletteEntry {
        key: "F7",
        name: "Prev Diff / 前の差分",
        description: "diff表示で前の差分位置へスクロール",
        command: Command::DiffPrevHunk,
    },
    CommandPaletteEntry {
        key: "F8",
        name: "Next Diff / 次の差分",
        description: "diff表示で次の差分位置へスクロール",
        command: Command::DiffNextHunk,
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
        key: "Ctrl+N",
        name: "Go to Line / 行番号ジャンプ",
        description: "指定した行番号へカーソルを移動",
        command: Command::GoToLine,
    },
    CommandPaletteEntry {
        key: "F5",
        name: "Reload File / 再読込",
        description: "ファイルをディスクから読み直す（未保存時は確認）",
        command: Command::Reload,
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
    /// Where the current directory listing starts in `candidates`. Everything
    /// from here on answers a path-shaped query and is dropped once the query
    /// stops being one, restoring the picker's own candidates underneath.
    path_listing_start: Option<usize>,
}

impl PickerState {
    /// Whether this picker offers files at all — the command palette and the
    /// buffer list do not, so a path typed into them stays a fuzzy pattern.
    fn accepts_paths(&self) -> bool {
        matches!(self.mode, PickerMode::Directory | PickerMode::Diff)
    }
}

/// Does this query name a filesystem path rather than a fuzzy pattern? The
/// workspace scan only knows files under the root, so `/`, `~/` and `../`
/// queries would otherwise match nothing at all.
///
/// A bare leading dot is deliberately not enough: `.rs` is a useful fuzzy
/// pattern, and only `./` or `..` commit to being a path.
fn is_path_query(query: &str) -> bool {
    query.starts_with(['/', '~']) || query.starts_with("./") || query.starts_with("..")
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
        preview: HitPreview,
    },
    Disk(GrepHit),
}

/// What a buffer hit looked like when the search ran. Captured here rather than
/// re-read from the document at draw time: `range` is frozen at search time, so
/// reading live text would pair a moved line with a stale offset and slide the
/// highlight off the match as soon as the buffer was edited. Grep hits already
/// carry their own snapshot in `GrepHit::text`.
#[derive(Clone, Debug)]
struct HitPreview {
    /// 0-based line the match sits on.
    line: usize,
    /// The line, trimmed — as shown in the results.
    text: String,
    /// Match start within `text`, in characters.
    column: usize,
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
    /// Index of the hit the editor is parked on, once one has been opened. None
    /// until then, so the list does not claim a row is focused before it is.
    current: Option<usize>,
    grep_token: Option<u64>,
    field_cursor: usize,
    /// Where a selection started, when one is active. The selection runs between
    /// this and `field_cursor`, in characters.
    field_anchor: Option<usize>,
    /// Undo/redo for the field being edited. Snapshots rather than a diff: the
    /// fields are one line long, so whole-value history costs nothing and keeps
    /// undo correct across a paste, a cut and a select-all overwrite alike.
    field_undo: Vec<FieldSnapshot>,
    field_redo: Vec<FieldSnapshot>,
    results_scroll: usize,
}

/// A find-pane field and caret, captured before an edit so it can be restored.
#[derive(Clone, Debug)]
struct FieldSnapshot {
    text: String,
    cursor: usize,
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
    ReloadDiscard(DocumentId),
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

/// Re-runs the search on demand. Results are a snapshot: they do not follow the
/// buffer, and edits made outside the editor cannot be noticed at all, so the
/// refresh is a button rather than something inferred from a document changing.
pub(crate) const SEARCH_RELOAD_BUTTON: &str = "[ Reload ]";

/// Sits flush with the right edge of the result list's header row. ASCII like
/// the other buttons: a symbol such as `⟳` is double-width in some terminals,
/// which would slide the drawn button out from under this hit range.
pub(crate) fn search_reload_button_range(pane_x: u16, pane_width: u16) -> (u16, u16) {
    let width = SEARCH_RELOAD_BUTTON.chars().count() as u16;
    let start = pane_x + pane_width.saturating_sub(width);
    (start, start + width)
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

/// Divides a result row's location column from its text. A dashed vertical rules
/// it off clearly while staying distinct from the solid `│` of a pane border.
pub const SEARCH_COLUMN_SEPARATOR: &str = " ┆ ";

/// One row of the find results: the whole line, how much of it is the location
/// column (so the renderer can dim it), and where the match sits (to highlight).
/// Both ranges are in characters.
pub struct SearchResultItem {
    pub text: String,
    pub prefix_len: usize,
    pub matched: Option<std::ops::Range<usize>>,
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
    pub items: Vec<SearchResultItem>,
    /// Whether the pane holds input focus. The caret is only drawn when it does,
    /// so it never sits in the query box while keystrokes go to the buffer.
    pub focused: bool,
    pub current: Option<usize>,
    pub total: usize,
    pub field_cursor: usize,
    /// Selected range in the active field, in characters, when one is active.
    pub field_selection: Option<std::ops::Range<usize>>,
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
    SignatureHelp {
        doc: DocumentId,
        anchor: CharIdx,
    },
    SemanticTokens {
        doc: DocumentId,
        version: i32,
    },
}

/// Whether picking a completion of this kind should insert `()` and put the
/// caret between them.
///
/// Functions, methods and constructors are called by name in every language the
/// editor targets. A class is only *called* by name where the class name is the
/// constructor — true in Python (`enumerate(…)`, `range(…)`, `dict()`), but not
/// in Rust, where a struct is built with `Foo::new()` or `Foo { … }` and `Foo()`
/// would be wrong. Servers report Python builtins like `enumerate` as `CLASS`,
/// so without the language check they would never get parentheses.
fn kind_is_callable(kind: Option<lsp_types::CompletionItemKind>, language: Option<&str>) -> bool {
    use lsp_types::CompletionItemKind as Kind;
    match kind {
        Some(Kind::FUNCTION | Kind::METHOD | Kind::CONSTRUCTOR) => true,
        Some(Kind::CLASS) => language == Some("python"),
        _ => false,
    }
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

/// A running shell and the screen it has drawn so far. The child process lives
/// in the runtime; this is the editor's view of it.
struct ShellSession {
    /// Distinguishes this session from one it replaced, whose reader thread
    /// reports its exit only after the successor is already running.
    token: u64,
    parser: vt100::Parser,
    selection: Option<TerminalSelection>,
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

/// Reduce an LSP `SignatureHelp` to the one signature line to show and the byte
/// range of the active parameter within it. Returns `None` when there is nothing
/// to show, which closes the popup.
fn signature_help_state(
    help: lsp_types::SignatureHelp,
    anchor: CharIdx,
) -> Option<SignatureHelpState> {
    let active = help.active_signature.unwrap_or(0) as usize;
    let signature = help
        .signatures
        .get(active)
        .or_else(|| help.signatures.first())?;
    let label = signature.label.clone();
    let active_index = signature.active_parameter.or(help.active_parameter);
    let active_parameter = active_index.and_then(|index| {
        let parameter = signature.parameters.as_ref()?.get(index as usize)?;
        parameter_byte_range(&label, &parameter.label)
    });
    Some(SignatureHelpState {
        label,
        active_parameter,
        anchor,
    })
}

/// The byte range of a parameter within its signature label. Simple labels are
/// matched as substrings; offset labels are UTF-16 code-unit offsets per the LSP
/// spec, converted to byte offsets here.
fn parameter_byte_range(
    label: &str,
    parameter: &lsp_types::ParameterLabel,
) -> Option<(usize, usize)> {
    match parameter {
        lsp_types::ParameterLabel::Simple(text) => {
            let start = label.find(text.as_str())?;
            Some((start, start + text.len()))
        }
        lsp_types::ParameterLabel::LabelOffsets(offsets) => {
            let start = utf16_offset_to_byte(label, offsets[0] as usize)?;
            let end = utf16_offset_to_byte(label, offsets[1] as usize)?;
            (start <= end).then_some((start, end))
        }
    }
}

fn utf16_offset_to_byte(text: &str, target: usize) -> Option<usize> {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 == target {
            return Some(byte);
        }
        utf16 += character.len_utf16();
    }
    (utf16 == target).then_some(text.len())
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
    /// When set, confirming expands this snippet body instead of inserting
    /// `insert` verbatim (see [`Editor::expand_snippet_body`]).
    snippet_body: Option<String>,
}

pub struct CompletionView {
    pub items: Vec<String>,
    pub selected: usize,
    pub anchor: CharIdx,
}

/// The signature-help popup: one signature line and the byte range within it to
/// highlight as the active parameter. `anchor` positions it at the caret like the
/// completion popup.
#[derive(Debug)]
struct SignatureHelpState {
    label: String,
    active_parameter: Option<(usize, usize)>,
    anchor: CharIdx,
}

pub struct SignatureHelpView<'a> {
    pub label: &'a str,
    pub active_parameter: Option<(usize, usize)>,
    pub anchor: CharIdx,
}

#[cfg(test)]
mod tests;
