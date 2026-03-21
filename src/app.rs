use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, Instant},
    sync::{Arc, atomic::AtomicBool},
};

use crossterm::{
    cursor::{self, SetCursorStyle},
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph},
};
use signal_hook::{consts::signal::SIGTSTP, flag, low_level};

use crate::{
    config,
    color::AppColors,
    document::Document,
    error::Result,
    mode::Mode,
    open_candidate::{
        OpenBufferCandidate, OpenCandidate, ProjectFileCandidate, collect_project_file_candidates,
    },
    picker_match,
};

pub struct App {
    pub mode: Mode,
    pub workspace: Workspace,
    pub picker: PickerState,
    pub shell: ShellState,
    pub cursor: CursorState,
    pub viewport_row: usize,
    pub pending_g: bool,
    pub pending_insert_j: Option<Instant>,
}

pub struct Workspace {
    pub documents: Vec<DocumentEntry>,
    pub current_index: usize,
}

pub struct DocumentEntry {
    pub path: PathBuf,
    pub document: Document,
}

pub struct PickerState {
    pub query: String,
    pub candidates: Vec<OpenCandidate>,
}

pub struct ShellState {
    pub program: String,
}

pub struct CursorState {
    pub row: usize,
    pub column: usize,
}

impl App {
    pub fn open(path: &Path) -> Result<Self> {
        let document = Document::open(path)?;
        let workspace = Workspace {
            documents: vec![DocumentEntry {
                path: path.to_path_buf(),
                document,
            }],
            current_index: 0,
        };

        let mut app = Self {
            mode: Mode::Normal,
            workspace,
            picker: PickerState {
                query: String::new(),
                candidates: Vec::new(),
            },
            shell: ShellState {
                program: config::shell_program().to_owned(),
            },
            cursor: CursorState { row: 0, column: 0 },
            viewport_row: 0,
            pending_g: false,
            pending_insert_j: None,
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

    fn render_frame(&self, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
        terminal.draw(|frame| {
            let area = frame.area();
            let render = self
                .workspace
                .current_document()
                .render_first_page(self.viewport_row, area.height as usize, area.width as usize)
                .expect("document render should succeed during draw");

            let layout = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

            let background = Block::default().style(Style::default().bg(AppColors::BACKGROUND));
            let content = Paragraph::new(
                render
                    .lines
                    .iter()
                    .map(format_render_line)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .style(
                Style::default()
                    .fg(AppColors::FOREGROUND)
                    .bg(AppColors::BACKGROUND),
            );
            let footer = Paragraph::new(self.footer_line(&render.status)).style(
                Style::default().bg(AppColors::PANEL),
            );

            frame.render_widget(background, area);
            frame.render_widget(content, layout[0]);
            frame.render_widget(footer, layout[1]);

            let cursor_position = self.cursor_position(&render, layout[0]);
            frame.set_cursor_position(cursor_position);
        })?;
        Ok(())
    }

    fn handle_event(&mut self, event: Event) -> Result<bool> {
        match event {
            Event::Key(key_event) => self.handle_key_event(key_event),
            _ => Ok(false),
        }
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) -> Result<bool> {
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
            return Ok(true);
        }

        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('q'))
        {
            return Ok(true);
        }

        if self.pending_g {
            self.pending_g = false;
            if matches!(key_event.code, KeyCode::Char('g')) {
                self.jump_to_top();
                return Ok(false);
            }
        }

        match key_event.code {
            KeyCode::Char('s') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.save_current_document()?;
                Ok(false)
            }
            KeyCode::Char('z') if key_event.modifiers.contains(KeyModifiers::CONTROL) => Ok(false),
            KeyCode::Char('y') if key_event.modifiers.contains(KeyModifiers::CONTROL) => Ok(false),
            KeyCode::Char('a') => {
                self.mode = Mode::Insert;
                self.pending_insert_j = None;
                self.move_cursor_right();
                self.clamp_vertical_state();
                Ok(false)
            }
            KeyCode::Char('b') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.page_up_full();
                Ok(false)
            }
            KeyCode::Char('d') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.page_down_half();
                Ok(false)
            }
            KeyCode::Char('f') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.page_down_full();
                Ok(false)
            }
            KeyCode::Char('g') => {
                self.pending_g = true;
                Ok(false)
            }
            KeyCode::Char('h') => {
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
            KeyCode::Char('u') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.page_up_half();
                Ok(false)
            }
            KeyCode::Char('G') => {
                self.jump_to_bottom();
                Ok(false)
            }
            _ => {
                self.pending_g = false;
                Ok(false)
            }
        }
    }

    fn handle_insert_mode_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('s'))
        {
            self.save_current_document()?;
            self.leave_insert_mode();
            return Ok(false);
        }

        match key_event.code {
            KeyCode::Esc => {
                self.leave_insert_mode();
                Ok(false)
            }
            KeyCode::Char('j') => {
                let now = Instant::now();
                if self
                    .pending_insert_j
                    .is_some_and(|previous| now.duration_since(previous) <= insert_escape_timeout())
                {
                    self.backspace_char();
                    self.leave_insert_mode();
                } else {
                    self.insert_char('j');
                    self.pending_insert_j = Some(now);
                }
                Ok(false)
            }
            KeyCode::Char(ch) => {
                self.insert_char(ch);
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Enter => {
                self.insert_newline();
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Tab => {
                self.insert_char('\t');
                self.pending_insert_j = None;
                Ok(false)
            }
            KeyCode::Backspace => {
                self.backspace_char();
                self.pending_insert_j = None;
                Ok(false)
            }
            _ => {
                self.pending_insert_j = None;
                Ok(false)
            }
        }
    }

    fn footer_color(&self) -> ratatui::style::Color {
        match self.mode {
            Mode::Normal => AppColors::NORMAL_MODE,
            Mode::Insert => AppColors::INSERT_MODE,
            Mode::Command => AppColors::COMMAND_MODE,
            Mode::Shell => AppColors::SHELL_MODE,
        }
    }

    fn footer_line(&self, status: &str) -> Line<'static> {
        let mode = self.mode.label();
        let file_name = self.workspace.current_document_name();
        let mode_bg = self.footer_color();
        let footer_bg = AppColors::PANEL;

        Line::from(vec![
            powerline_segment(mode.to_owned(), AppColors::BACKGROUND, mode_bg),
            powerline_separator_left(mode_bg, footer_bg),
            powerline_segment(file_name, AppColors::ACCENT, footer_bg),
            powerline_separator_right(mode_bg),
            powerline_segment(status.to_owned(), AppColors::MUTED, footer_bg),
            powerline_separator_right(mode_bg),
        ])
    }

    fn cursor_position(
        &self,
        render: &crate::document::DocumentRender,
        area: ratatui::layout::Rect,
    ) -> ratatui::layout::Position {
        let relative_row = self.cursor.row.saturating_sub(self.viewport_row);
        let line_index = relative_row.min(render.lines.len().saturating_sub(1));
        let line = &render.lines[line_index];
        let text_width = line.text.chars().count();
        let column = self.cursor.column.min(text_width);

        let x = area.x.saturating_add(11).saturating_add(column as u16);
        let y = area.y.saturating_add(line_index as u16);

        ratatui::layout::Position::new(x, y)
    }

    fn move_cursor_up(&mut self) {
        self.cursor.row = self.cursor.row.saturating_sub(1);
        self.clamp_vertical_state();
    }

    fn move_cursor_left(&mut self) {
        self.cursor.column = self.cursor.column.saturating_sub(1);
    }

    fn move_cursor_down(&mut self) {
        self.cursor.row = self.cursor.row.saturating_add(1);
        self.clamp_vertical_state();
    }

    fn move_cursor_right(&mut self) {
        self.cursor.column = self.cursor.column.saturating_add(1);
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
        self.workspace.current_document_mut().jump_to_top();
        self.viewport_row = 0;
        self.cursor.row = 0;
    }

    fn jump_to_bottom(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        let visible_height = self.page_step();
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

    fn leave_insert_mode(&mut self) {
        self.mode = Mode::Normal;
        self.pending_insert_j = None;
        self.cursor.column = self.cursor.column.saturating_sub(1);
    }

    fn save_current_document(&mut self) -> Result<()> {
        let path = self.workspace.current_document_path().to_path_buf();
        self.workspace.current_document().save(&path)
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
}

impl Workspace {
    pub fn current_document(&self) -> &Document {
        &self.documents[self.current_index].document
    }

    pub fn current_document_mut(&mut self) -> &mut Document {
        &mut self.documents[self.current_index].document
    }

    pub fn open_buffer_candidates(&self) -> Vec<OpenCandidate> {
        self.documents
            .iter()
            .map(|entry| {
                OpenCandidate::OpenBuffer(OpenBufferCandidate::new(
                    entry.path.clone(),
                    display_name(&entry.path),
                ))
            })
            .collect()
    }

    pub fn current_document_name(&self) -> String {
        display_name(&self.documents[self.current_index].path)
    }

    pub fn current_document_path(&self) -> &Path {
        &self.documents[self.current_index].path
    }
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    suspend_signal_guard: SuspendSignalGuard,
    active: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        let mut stdout = std::io::stdout();
        terminal::enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, cursor::Show, SetCursorStyle::SteadyBar)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            suspend_signal_guard: SuspendSignalGuard::enter(),
            active: true,
        })
    }

    fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<std::io::Stdout>> {
        &mut self.terminal
    }

    fn leave(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }

        self.terminal.flush()?;
        execute!(self.terminal.backend_mut(), cursor::Show, LeaveAlternateScreen)?;
        terminal::disable_raw_mode()?;
        self.active = false;
        Ok(())
    }
}

struct SuspendSignalGuard {
    #[cfg(unix)]
    signal_id: signal_hook::SigId,
    _ignored: Arc<AtomicBool>,
}

impl SuspendSignalGuard {
    fn enter() -> Self {
        #[cfg(unix)]
        {
            let ignored = Arc::new(AtomicBool::new(false));
            let signal_id = flag::register(SIGTSTP, Arc::clone(&ignored))
                .expect("failed to register SIGTSTP handler");
            Self {
                signal_id,
                _ignored: ignored,
            }
        }

        #[cfg(not(unix))]
        {
            Self {}
        }
    }
}

impl Drop for SuspendSignalGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        low_level::unregister(self.signal_id);
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if self.active {
            let _ = self.terminal.flush();
            let _ = execute!(self.terminal.backend_mut(), cursor::Show, LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
        }
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

fn format_render_line(line: &crate::document::DocumentRenderLine) -> String {
    format!(
        "{diag:>1} {line_number:>6} {gutter:>1} {text}",
        diag = line.diagnostic_marker,
        line_number = line.line_number,
        gutter = line.gutter_marker,
        text = line.text,
    )
}

fn powerline_segment(
    text: String,
    foreground: ratatui::style::Color,
    background: ratatui::style::Color,
) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::default().fg(foreground).bg(background),
    )
}

fn powerline_separator_left(
    left_background: ratatui::style::Color,
    right_background: ratatui::style::Color,
) -> Span<'static> {
    Span::styled(
        "\u{e0b0}",
        Style::default()
            .fg(left_background)
            .bg(right_background),
    )
}

fn powerline_separator_right(foreground: ratatui::style::Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", '\u{e0b1}'),
        Style::default().fg(foreground).bg(AppColors::PANEL),
    )
}

fn insert_escape_timeout() -> Duration {
    Duration::from_millis(300)
}

#[allow(dead_code)]
fn _project_file_display_name(candidate: &ProjectFileCandidate) -> &str {
    &candidate.display_name
}
