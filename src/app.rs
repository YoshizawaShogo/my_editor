use std::cmp::min;
use std::fs;
use std::path::{Path, PathBuf};

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use ratatui::layout::Rect;
use walkdir::WalkDir;

use crate::theme::ThemePalette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    BufferSearch,
    FilePicker,
    BufferList,
    SymbolSearch,
    Diagnostics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    EnterInsert,
    EnterNormal,
    MoveCursor(Direction),
    MovePaneFocus(Direction),
    InsertChar(char),
    InsertNewline,
    DeleteBackward,
    OpenBufferSearch,
    OpenFilePicker,
    OpenBufferList,
    OpenSymbolSearch,
    OpenDiagnostics,
    PromptInput(char),
    PromptBackspace,
    PromptMoveSelection(Direction),
    ConfirmPrompt,
    CancelPrompt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    BufferSearch,
    FilePicker,
    BufferList,
    SymbolSearch,
    Diagnostics,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptItem {
    pub label: String,
    pub path: Option<PathBuf>,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub query: String,
    pub items: Vec<PromptItem>,
    pub selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PromptRefresh {
    items: Vec<PromptItem>,
    selected: usize,
    selected_item: Option<PromptItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    pub dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneState {
    pub buffer: Buffer,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scroll_row: usize,
    pub viewport_height: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorState {
    pub panes: Vec<PaneState>,
    pub focused_pane: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationLevel {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub level: NotificationLevel,
    pub message: String,
    pub remaining_ticks: u8,
}

#[derive(Debug)]
pub struct App {
    pub cwd: PathBuf,
    pub mode: Mode,
    pub theme: ThemePalette,
    pub editor: EditorState,
    pub prompt: Option<PromptState>,
    pub notification: Option<Notification>,
    pub should_quit: bool,
    files_cache: Vec<PathBuf>,
    open_buffers: Vec<PathBuf>,
}

impl App {
    pub fn new(cwd: PathBuf, initial_file: Option<PathBuf>, theme: ThemePalette) -> Self {
        let mut app = Self {
            cwd,
            mode: Mode::Normal,
            theme,
            editor: EditorState {
                panes: vec![PaneState::empty()],
                focused_pane: 0,
            },
            prompt: None,
            notification: None,
            should_quit: false,
            files_cache: Vec::new(),
            open_buffers: Vec::new(),
        };
        app.refresh_files_cache();
        if let Some(path) = initial_file {
            app.open_file_in_focused_pane(path);
        }
        app
    }

    pub fn tick(&mut self) {
        if let Some(notification) = &mut self.notification {
            notification.remaining_ticks = notification.remaining_ticks.saturating_sub(1);
            if notification.remaining_ticks == 0 {
                self.notification = None;
            }
        }
    }

    pub fn sync_viewports(&mut self, area: Rect) {
        let available_rows = area.height.saturating_sub(3) as usize;
        for pane in &mut self.editor.panes {
            pane.viewport_height = available_rows.max(1);
            pane.clamp_cursor();
            pane.ensure_cursor_visible();
        }
    }

    pub fn dispatch(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::EnterInsert => self.mode = Mode::Insert,
            Action::EnterNormal => {
                self.mode = Mode::Normal;
                self.prompt = None;
            }
            Action::MoveCursor(direction) => self.focused_pane_mut().move_cursor(direction),
            Action::MovePaneFocus(direction) => self.move_pane_focus(direction),
            Action::InsertChar(ch) => {
                if self.mode == Mode::Insert {
                    self.focused_pane_mut().insert_char(ch);
                } else {
                    self.handle_prompt_input(ch);
                }
            }
            Action::InsertNewline => {
                if self.mode == Mode::Insert {
                    self.focused_pane_mut().insert_newline();
                }
            }
            Action::DeleteBackward => {
                if self.mode == Mode::Insert {
                    self.focused_pane_mut().delete_backward();
                } else {
                    self.handle_prompt_backspace();
                }
            }
            Action::OpenBufferSearch => self.open_prompt(PromptKind::BufferSearch),
            Action::OpenFilePicker => self.open_prompt(PromptKind::FilePicker),
            Action::OpenBufferList => self.open_prompt(PromptKind::BufferList),
            Action::OpenSymbolSearch => self.open_prompt(PromptKind::SymbolSearch),
            Action::OpenDiagnostics => self.open_prompt(PromptKind::Diagnostics),
            Action::PromptInput(ch) => self.handle_prompt_input(ch),
            Action::PromptBackspace => self.handle_prompt_backspace(),
            Action::PromptMoveSelection(direction) => {
                if let Some(prompt) = &mut self.prompt {
                    if prompt.items.is_empty() {
                        prompt.selected = 0;
                    } else {
                        match direction {
                            Direction::Up => {
                                prompt.selected = prompt.selected.saturating_sub(1);
                            }
                            Direction::Down => {
                                prompt.selected = min(prompt.selected + 1, prompt.items.len() - 1);
                            }
                            Direction::Left | Direction::Right => {}
                        }
                    }
                }
            }
            Action::ConfirmPrompt => self.confirm_prompt(),
            Action::CancelPrompt => {
                self.prompt = None;
                self.mode = Mode::Normal;
            }
        }
    }

    fn open_prompt(&mut self, kind: PromptKind) {
        let mode = match kind {
            PromptKind::BufferSearch => Mode::BufferSearch,
            PromptKind::FilePicker => Mode::FilePicker,
            PromptKind::BufferList => Mode::BufferList,
            PromptKind::SymbolSearch => Mode::SymbolSearch,
            PromptKind::Diagnostics => Mode::Diagnostics,
        };
        self.mode = mode;
        self.prompt = Some(PromptState {
            kind,
            query: String::new(),
            items: Vec::new(),
            selected: 0,
        });
        self.refresh_prompt_items();
    }

    fn handle_prompt_input(&mut self, ch: char) {
        if let Some(prompt) = &mut self.prompt {
            prompt.query.push(ch);
            prompt.selected = 0;
            self.refresh_prompt_items();
        }
    }

    fn handle_prompt_backspace(&mut self) {
        if let Some(prompt) = &mut self.prompt {
            prompt.query.pop();
            prompt.selected = 0;
            self.refresh_prompt_items();
        }
    }

    fn refresh_prompt_items(&mut self) {
        let Some(prompt) = &self.prompt else {
            return;
        };
        let kind = prompt.kind;
        let query = prompt.query.clone();
        let selected = prompt.selected;

        if kind == PromptKind::FilePicker && self.files_cache.is_empty() {
            self.refresh_files_cache();
        }

        let refresh = match kind {
            PromptKind::BufferSearch => {
                compute_buffer_search_refresh(&self.focused_pane().buffer, &query, selected)
            }
            PromptKind::FilePicker => {
                compute_file_picker_refresh(&self.cwd, &self.files_cache, &query, selected)
            }
            PromptKind::BufferList => {
                compute_buffer_list_refresh(&self.cwd, &self.open_buffers, &query, selected)
            }
            PromptKind::SymbolSearch => {
                compute_symbol_search_refresh(&self.focused_pane().buffer, &query, selected)
            }
            PromptKind::Diagnostics => {
                compute_diagnostics_refresh(&self.focused_pane().buffer, &query, selected)
            }
        };

        if let Some(prompt) = &mut self.prompt {
            prompt.items = refresh.items;
            prompt.selected = refresh.selected;
        }

        if let Some(item) = refresh.selected_item {
            self.jump_to_prompt_item(&item);
        }
    }

    fn confirm_prompt(&mut self) {
        let Some(prompt) = &self.prompt else {
            return;
        };
        let item = prompt.items.get(prompt.selected).cloned();
        let kind = prompt.kind;

        match (kind, item) {
            (PromptKind::BufferSearch, Some(item)) => self.jump_to_prompt_item(&item),
            (PromptKind::FilePicker, Some(item)) => {
                if let Some(path) = item.path {
                    self.open_file_in_focused_pane(path);
                }
            }
            (PromptKind::BufferList, Some(item)) => {
                if let Some(path) = item.path {
                    self.open_file_in_focused_pane(path);
                }
            }
            (PromptKind::SymbolSearch, Some(item)) | (PromptKind::Diagnostics, Some(item)) => {
                self.jump_to_prompt_item(&item)
            }
            (_, None) => {}
        }

        self.prompt = None;
        self.mode = Mode::Normal;
    }

    fn jump_to_prompt_item(&mut self, item: &PromptItem) {
        let pane = self.focused_pane_mut();
        if let Some(row) = item.line {
            pane.cursor_row = min(row, pane.buffer.lines.len().saturating_sub(1));
            let line_len = pane.current_line_len();
            pane.cursor_col = min(item.column.unwrap_or(0), line_len);
            pane.ensure_cursor_visible();
        }
    }

    fn move_pane_focus(&mut self, direction: Direction) {
        match direction {
            Direction::Left => self.editor.focused_pane = 0,
            Direction::Right => {
                self.editor.focused_pane = min(1, self.editor.panes.len().saturating_sub(1))
            }
            Direction::Up | Direction::Down => {}
        }
    }

    fn refresh_files_cache(&mut self) {
        self.files_cache = collect_files(&self.cwd);
    }

    fn open_file_in_focused_pane(&mut self, path: PathBuf) {
        self.remember_open_buffer(&path);
        let buffer = Buffer::from_path(&path);
        let display_name = buffer.display_name();
        let pane = self.focused_pane_mut();
        pane.buffer = buffer;
        pane.cursor_row = 0;
        pane.cursor_col = 0;
        pane.scroll_row = 0;
        pane.ensure_cursor_visible();
        self.notify(NotificationLevel::Info, format!("Opened {display_name}"));
    }

    pub fn focused_pane(&self) -> &PaneState {
        &self.editor.panes[self.editor.focused_pane]
    }

    pub fn focused_pane_mut(&mut self) -> &mut PaneState {
        &mut self.editor.panes[self.editor.focused_pane]
    }

    fn remember_open_buffer(&mut self, path: &Path) {
        if let Some(index) = self
            .open_buffers
            .iter()
            .position(|existing| existing == path)
        {
            self.open_buffers.remove(index);
        }
        self.open_buffers.insert(0, path.to_path_buf());
    }

    fn notify(&mut self, level: NotificationLevel, message: String) {
        self.notification = Some(Notification {
            level,
            message,
            remaining_ticks: 30,
        });
    }
}

impl Buffer {
    pub fn empty() -> Self {
        Self {
            path: None,
            lines: vec![String::new()],
            dirty: false,
        }
    }

    pub fn from_path(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(content) => {
                let mut lines = content.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
                if content.ends_with('\n') {
                    lines.push(String::new());
                }
                if lines.is_empty() {
                    lines.push(String::new());
                }
                Self {
                    path: Some(path.to_path_buf()),
                    lines,
                    dirty: false,
                }
            }
            Err(_) => Self {
                path: Some(path.to_path_buf()),
                lines: vec![String::new()],
                dirty: false,
            },
        }
    }

    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "[No Name]".to_string())
    }
}

impl PaneState {
    pub fn empty() -> Self {
        Self {
            buffer: Buffer::empty(),
            cursor_row: 0,
            cursor_col: 0,
            scroll_row: 0,
            viewport_height: 8,
        }
    }

    pub fn move_cursor(&mut self, direction: Direction) {
        match direction {
            Direction::Left => {
                self.cursor_col = self.cursor_col.saturating_sub(1);
            }
            Direction::Right => {
                self.cursor_col = min(self.cursor_col + 1, self.current_line_len());
            }
            Direction::Up => {
                self.cursor_row = self.cursor_row.saturating_sub(1);
                self.cursor_col = min(self.cursor_col, self.current_line_len());
            }
            Direction::Down => {
                self.cursor_row = min(
                    self.cursor_row + 1,
                    self.buffer.lines.len().saturating_sub(1),
                );
                self.cursor_col = min(self.cursor_col, self.current_line_len());
            }
        }
        self.ensure_cursor_visible();
    }

    pub fn insert_char(&mut self, ch: char) {
        if self.cursor_row >= self.buffer.lines.len() {
            self.buffer.lines.push(String::new());
        }
        let line = &mut self.buffer.lines[self.cursor_row];
        let idx = char_to_byte_index(line, self.cursor_col);
        line.insert(idx, ch);
        self.cursor_col += 1;
        self.buffer.dirty = true;
        self.ensure_cursor_visible();
    }

    pub fn insert_newline(&mut self) {
        if self.cursor_row >= self.buffer.lines.len() {
            self.buffer.lines.push(String::new());
        }
        let line = &mut self.buffer.lines[self.cursor_row];
        let split_at = char_to_byte_index(line, self.cursor_col);
        let remainder = line.split_off(split_at);
        self.buffer.lines.insert(self.cursor_row + 1, remainder);
        self.cursor_row += 1;
        self.cursor_col = 0;
        self.buffer.dirty = true;
        self.ensure_cursor_visible();
    }

    pub fn delete_backward(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.buffer.lines[self.cursor_row];
            let end = char_to_byte_index(line, self.cursor_col);
            let start = char_to_byte_index(line, self.cursor_col - 1);
            line.replace_range(start..end, "");
            self.cursor_col -= 1;
            self.buffer.dirty = true;
        } else if self.cursor_row > 0 {
            let current = self.buffer.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            let prev_len = self.buffer.lines[self.cursor_row].chars().count();
            self.buffer.lines[self.cursor_row].push_str(&current);
            self.cursor_col = prev_len;
            self.buffer.dirty = true;
        }
        self.ensure_cursor_visible();
    }

    pub fn clamp_cursor(&mut self) {
        self.cursor_row = min(self.cursor_row, self.buffer.lines.len().saturating_sub(1));
        self.cursor_col = min(self.cursor_col, self.current_line_len());
    }

    pub fn ensure_cursor_visible(&mut self) {
        self.clamp_cursor();
        if self.cursor_row < self.scroll_row {
            self.scroll_row = self.cursor_row;
        } else if self.cursor_row >= self.scroll_row + self.viewport_height {
            self.scroll_row = self.cursor_row + 1 - self.viewport_height;
        }
    }

    pub fn current_line_len(&self) -> usize {
        self.buffer
            .lines
            .get(self.cursor_row)
            .map(|line| line.chars().count())
            .unwrap_or(0)
    }
}

fn char_to_byte_index(line: &str, char_idx: usize) -> usize {
    line.char_indices()
        .map(|(idx, _)| idx)
        .nth(char_idx)
        .unwrap_or(line.len())
}

fn search_in_buffer(buffer: &Buffer, query: &str) -> Vec<PromptItem> {
    if query.is_empty() {
        return Vec::new();
    }

    let mut items = Vec::new();
    for (line_idx, line) in buffer.lines.iter().enumerate() {
        if let Some(byte_idx) = line.find(query) {
            let column = line[..byte_idx].chars().count();
            items.push(PromptItem {
                label: format!("{}: {}", line_idx + 1, line),
                path: None,
                line: Some(line_idx),
                column: Some(column),
            });
        }
    }
    items
}

fn compute_buffer_search_refresh(buffer: &Buffer, query: &str, selected: usize) -> PromptRefresh {
    let items = search_in_buffer(buffer, query);
    let selected = clamp_prompt_selection(selected, &items);
    let selected_item = items.get(selected).cloned();
    PromptRefresh {
        items,
        selected,
        selected_item,
    }
}

fn collect_files(cwd: &Path) -> Vec<PathBuf> {
    WalkDir::new(cwd)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.path().strip_prefix(cwd).ok().map(Path::to_path_buf))
        .collect()
}

fn search_files(cwd: &Path, files: &[PathBuf], query: &str) -> Vec<PromptItem> {
    let matcher = SkimMatcherV2::default();
    let mut scored = files
        .iter()
        .filter_map(|path| {
            let display = path.to_string_lossy().to_string();
            let score = if query.is_empty() {
                Some(0)
            } else {
                matcher.fuzzy_match(&display, query)
            }?;
            Some((score, display, cwd.join(path)))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(10)
        .map(|(_, label, path)| PromptItem {
            label,
            path: Some(path),
            line: None,
            column: None,
        })
        .collect()
}

fn search_open_buffers(cwd: &Path, open_buffers: &[PathBuf], query: &str) -> Vec<PromptItem> {
    if query.is_empty() {
        return open_buffers
            .iter()
            .take(10)
            .map(|path| PromptItem {
                label: path
                    .strip_prefix(cwd)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .to_string(),
                path: Some(path.clone()),
                line: None,
                column: None,
            })
            .collect();
    }

    let relative_paths = open_buffers
        .iter()
        .map(|path| path.strip_prefix(cwd).unwrap_or(path).to_path_buf())
        .collect::<Vec<_>>();
    search_files(cwd, &relative_paths, query)
}

fn compute_file_picker_refresh(
    cwd: &Path,
    files: &[PathBuf],
    query: &str,
    selected: usize,
) -> PromptRefresh {
    let items = search_files(cwd, files, query);
    let selected = clamp_prompt_selection(selected, &items);
    PromptRefresh {
        items,
        selected,
        selected_item: None,
    }
}

fn compute_buffer_list_refresh(
    cwd: &Path,
    open_buffers: &[PathBuf],
    query: &str,
    selected: usize,
) -> PromptRefresh {
    let items = search_open_buffers(cwd, open_buffers, query);
    let selected = clamp_prompt_selection(selected, &items);
    PromptRefresh {
        items,
        selected,
        selected_item: None,
    }
}

fn compute_symbol_search_refresh(buffer: &Buffer, query: &str, selected: usize) -> PromptRefresh {
    let items = search_symbols(buffer, query);
    let selected = clamp_prompt_selection(selected, &items);
    let selected_item = items.get(selected).cloned();
    PromptRefresh {
        items,
        selected,
        selected_item,
    }
}

fn compute_diagnostics_refresh(buffer: &Buffer, query: &str, selected: usize) -> PromptRefresh {
    let items = search_diagnostics(buffer, query);
    let selected = clamp_prompt_selection(selected, &items);
    let selected_item = items.get(selected).cloned();
    PromptRefresh {
        items,
        selected,
        selected_item,
    }
}

fn clamp_prompt_selection(selected: usize, items: &[PromptItem]) -> usize {
    min(selected, items.len().saturating_sub(1))
}

fn search_symbols(buffer: &Buffer, query: &str) -> Vec<PromptItem> {
    let symbols = buffer
        .lines
        .iter()
        .enumerate()
        .filter_map(|(line_idx, line)| {
            let trimmed = line.trim_start();
            let kind = if trimmed.starts_with("fn ") {
                Some("fn")
            } else if trimmed.starts_with("struct ") {
                Some("struct")
            } else if trimmed.starts_with("enum ") {
                Some("enum")
            } else if trimmed.starts_with("trait ") {
                Some("trait")
            } else if trimmed.starts_with("impl ") {
                Some("impl")
            } else if trimmed.starts_with("mod ") {
                Some("mod")
            } else if trimmed.starts_with("const ") {
                Some("const")
            } else if trimmed.starts_with("static ") {
                Some("static")
            } else if trimmed.starts_with("type ") {
                Some("type")
            } else {
                None
            }?;

            Some(PromptItem {
                label: format!("{kind}: {}", trimmed),
                path: None,
                line: Some(line_idx),
                column: Some(0),
            })
        })
        .collect::<Vec<_>>();

    filter_prompt_items(&symbols, query)
}

fn search_diagnostics(buffer: &Buffer, query: &str) -> Vec<PromptItem> {
    let diagnostics = buffer
        .lines
        .iter()
        .enumerate()
        .filter_map(|(line_idx, line)| {
            if line.contains('\t') {
                Some(PromptItem {
                    label: format!("warning:{} contains tab indentation", line_idx + 1),
                    path: None,
                    line: Some(line_idx),
                    column: Some(line.find('\t').unwrap_or(0)),
                })
            } else if line.ends_with(' ') {
                Some(PromptItem {
                    label: format!("warning:{} trailing whitespace", line_idx + 1),
                    path: None,
                    line: Some(line_idx),
                    column: Some(line.trim_end_matches(' ').chars().count()),
                })
            } else if line.chars().count() > 120 {
                Some(PromptItem {
                    label: format!("info:{} line exceeds 120 characters", line_idx + 1),
                    path: None,
                    line: Some(line_idx),
                    column: Some(120),
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    filter_prompt_items(&diagnostics, query)
}

fn filter_prompt_items(items: &[PromptItem], query: &str) -> Vec<PromptItem> {
    let matcher = SkimMatcherV2::default();
    let mut scored = items
        .iter()
        .filter_map(|item| {
            let score = if query.is_empty() {
                Some(0)
            } else {
                matcher.fuzzy_match(&item.label, query)
            }?;
            Some((score, item.clone()))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
    scored.into_iter().map(|(_, item)| item).collect()
}

pub fn prompt_title(kind: PromptKind) -> &'static str {
    match kind {
        PromptKind::BufferSearch => "Buffer Search",
        PromptKind::FilePicker => "File Picker",
        PromptKind::BufferList => "Buffer List",
        PromptKind::SymbolSearch => "Symbol Search",
        PromptKind::Diagnostics => "Diagnostics",
    }
}

pub fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "NORMAL",
        Mode::Insert => "INSERT",
        Mode::BufferSearch => "BUFFER_SEARCH",
        Mode::FilePicker => "FILE_PICKER",
        Mode::BufferList => "BUFFER_LIST",
        Mode::SymbolSearch => "SYMBOL_SEARCH",
        Mode::Diagnostics => "DIAGNOSTICS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_buffer(lines: &[&str]) -> App {
        let mut app = App::new(
            std::env::temp_dir(),
            None,
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );
        app.editor.panes[0].buffer = Buffer {
            path: None,
            lines: lines.iter().map(|line| line.to_string()).collect(),
            dirty: false,
        };
        app
    }

    #[test]
    fn transitions_between_normal_and_insert() {
        let mut app = App::new(
            std::env::temp_dir(),
            None,
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );
        assert_eq!(app.mode, Mode::Normal);

        app.dispatch(Action::EnterInsert);
        assert_eq!(app.mode, Mode::Insert);

        app.dispatch(Action::EnterNormal);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn insert_mode_edits_buffer() {
        let mut app = App::new(
            std::env::temp_dir(),
            None,
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );
        app.dispatch(Action::EnterInsert);
        app.dispatch(Action::InsertChar('a'));
        app.dispatch(Action::InsertChar('b'));

        assert_eq!(app.focused_pane().buffer.lines, vec!["ab".to_string()]);
        assert!(app.focused_pane().buffer.dirty);
    }

    #[test]
    fn ctrl_f_style_prompt_moves_to_match() {
        let mut app = app_with_buffer(&["alpha", "beta", "alphabet"]);
        app.dispatch(Action::OpenBufferSearch);
        app.dispatch(Action::PromptInput('b'));
        app.dispatch(Action::PromptInput('e'));

        assert_eq!(app.mode, Mode::BufferSearch);
        assert_eq!(app.focused_pane().cursor_row, 1);
        assert_eq!(app.prompt.as_ref().unwrap().items.len(), 2);
    }

    #[test]
    fn prompt_cancel_returns_to_normal() {
        let mut app = App::new(
            std::env::temp_dir(),
            None,
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );
        app.dispatch(Action::EnterInsert);
        app.dispatch(Action::OpenFilePicker);
        assert_eq!(app.mode, Mode::FilePicker);

        app.dispatch(Action::CancelPrompt);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.prompt.is_none());
    }

    #[test]
    fn file_picker_opens_selected_file_in_focused_pane() {
        let root = std::env::temp_dir().join(format!(
            "my_editor_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let file_path = root.join("notes.rs");
        fs::write(&file_path, "fn main() {}\n").unwrap();

        let mut app = App::new(
            root.clone(),
            None,
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );
        app.dispatch(Action::OpenFilePicker);
        app.dispatch(Action::PromptInput('n'));
        app.dispatch(Action::PromptInput('o'));
        app.dispatch(Action::ConfirmPrompt);

        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.focused_pane().buffer.path.as_ref(), Some(&file_path));
        assert_eq!(app.focused_pane().buffer.lines[0], "fn main() {}");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delete_backward_merges_lines_at_line_start() {
        let mut pane = PaneState {
            buffer: Buffer {
                path: None,
                lines: vec!["abc".to_string(), "def".to_string()],
                dirty: false,
            },
            cursor_row: 1,
            cursor_col: 0,
            scroll_row: 0,
            viewport_height: 8,
        };

        pane.delete_backward();

        assert_eq!(pane.buffer.lines, vec!["abcdef".to_string()]);
        assert_eq!(pane.cursor_row, 0);
        assert_eq!(pane.cursor_col, 3);
    }

    #[test]
    fn empty_buffer_search_returns_no_items() {
        let buffer = Buffer {
            path: None,
            lines: vec!["alpha".to_string()],
            dirty: false,
        };

        let refresh = compute_buffer_search_refresh(&buffer, "", 3);

        assert!(refresh.items.is_empty());
        assert_eq!(refresh.selected, 0);
        assert!(refresh.selected_item.is_none());
    }

    #[test]
    fn app_starts_with_single_pane() {
        let app = App::new(
            std::env::temp_dir(),
            None,
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );

        assert_eq!(app.editor.panes.len(), 1);
        assert_eq!(app.editor.focused_pane, 0);
    }

    #[test]
    fn app_opens_initial_file_from_cli_argument() {
        let root = std::env::temp_dir().join(format!(
            "my_editor_initial_file_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let file_path = root.join("demo.rs");
        fs::write(&file_path, "let value = 1;\n").unwrap();

        let app = App::new(
            root.clone(),
            Some(file_path.clone()),
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );

        assert_eq!(app.focused_pane().buffer.path.as_ref(), Some(&file_path));
        assert_eq!(app.focused_pane().buffer.lines[0], "let value = 1;");
        assert_eq!(
            app.notification
                .as_ref()
                .map(|notification| notification.message.as_str()),
            Some("Opened demo.rs")
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn buffer_list_includes_previously_opened_files() {
        let root = std::env::temp_dir().join(format!(
            "my_editor_buffer_list_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let first = root.join("first.rs");
        let second = root.join("second.rs");
        fs::write(&first, "fn first() {}\n").unwrap();
        fs::write(&second, "fn second() {}\n").unwrap();

        let mut app = App::new(
            root.clone(),
            Some(first.clone()),
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );
        app.open_file_in_focused_pane(second.clone());
        app.dispatch(Action::OpenBufferList);

        let items = &app.prompt.as_ref().unwrap().items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].path.as_ref(), Some(&second));
        assert_eq!(items[1].path.as_ref(), Some(&first));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symbol_search_finds_named_declarations() {
        let buffer = Buffer {
            path: None,
            lines: vec!["struct Demo {}".to_string(), "fn run() {}".to_string()],
            dirty: false,
        };

        let refresh = compute_symbol_search_refresh(&buffer, "run", 0);

        assert_eq!(refresh.items.len(), 1);
        assert_eq!(refresh.items[0].label, "fn: fn run() {}");
        assert_eq!(refresh.selected_item.as_ref().unwrap().line, Some(1));
    }

    #[test]
    fn diagnostics_detect_tabs_and_trailing_whitespace() {
        let buffer = Buffer {
            path: None,
            lines: vec!["\tindented".to_string(), "trim me ".to_string()],
            dirty: false,
        };

        let refresh = compute_diagnostics_refresh(&buffer, "", 0);

        assert_eq!(refresh.items.len(), 2);
        assert!(refresh.items[0].label.contains("contains tab indentation"));
        assert!(refresh.items[1].label.contains("trailing whitespace"));
    }

    #[test]
    fn notification_expires_after_thirty_ticks() {
        let root = std::env::temp_dir().join(format!(
            "my_editor_notification_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let file_path = root.join("notify.rs");
        fs::write(&file_path, "fn notify() {}\n").unwrap();

        let mut app = App::new(
            root.clone(),
            Some(file_path),
            crate::theme::ThemePalette::resolve(crate::theme::ThemeOption::Classic),
        );
        assert!(app.notification.is_some());

        for _ in 0..30 {
            app.tick();
        }

        assert!(app.notification.is_none());

        fs::remove_dir_all(root).unwrap();
    }
}
