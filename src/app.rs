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
    layout::{Constraint, Layout, Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
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
    pub pending_normal_action: Option<PendingNormalAction>,
    pub pending_insert_j: Option<Instant>,
    pub last_replayable_action: Option<ReplayableAction>,
    pub go_input: GoInputState,
    pub yank_buffer: YankBuffer,
}

#[derive(Clone, Copy)]
pub enum ReplayableAction {
    NextGitHunk,
    PreviousGitHunk,
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

pub struct GoInputState {
    pub active: bool,
    pub value: String,
}

pub enum YankBuffer {
    Empty,
    Charwise(String),
    Linewise(String),
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
            pending_normal_action: None,
            pending_insert_j: None,
            last_replayable_action: None,
            go_input: GoInputState {
                active: false,
                value: String::new(),
            },
            yank_buffer: YankBuffer::Empty,
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
            let document = self.workspace.current_document();
            let indent_width = document.indent_width();
            let render = document
                .render_first_page(self.viewport_row, area.height as usize, area.width as usize)
                .expect("document render should succeed during draw");

            let layout = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

            let background = Block::default().style(Style::default().bg(AppColors::BACKGROUND));
            let content = Paragraph::new(
                format_render_lines(&render.lines, indent_width),
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

            if self.go_input.active {
                let popup = centered_rect(24, 3, area);
                let input = Paragraph::new(format!("Go: {}", self.go_input.value))
                    .block(
                        Block::default()
                            .title(" Go ")
                            .borders(Borders::ALL)
                            .style(Style::default().bg(AppColors::PANEL).fg(AppColors::ACCENT)),
                    )
                    .style(Style::default().bg(AppColors::PANEL).fg(AppColors::FOREGROUND));
                frame.render_widget(Clear, popup);
                frame.render_widget(input, popup);
            }

            let cursor_position = if self.go_input.active {
                self.go_input_cursor_position(area)
            } else {
                self.cursor_position(&render, layout[0])
            };
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
        if self.go_input.active {
            return self.handle_go_input_key(key_event);
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
            return Ok(true);
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

        if let Some(pending_action) = self.pending_normal_action.take() {
            return self.handle_pending_normal_action(pending_action, key_event);
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
            KeyCode::Char('d') => {
                self.pending_normal_action = Some(PendingNormalAction::Operator(PendingOperator::Delete));
                Ok(false)
            }
            KeyCode::Char('f') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.page_down_full();
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
            KeyCode::Char('o') => {
                self.open_line_below();
                Ok(false)
            }
            KeyCode::Char('p') => {
                self.paste_after_cursor()?;
                Ok(false)
            }
            KeyCode::Char('r') => {
                self.replay_last_action()?;
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
            KeyCode::Char('y') => {
                self.pending_normal_action = Some(PendingNormalAction::Operator(PendingOperator::Yank));
                Ok(false)
            }
            KeyCode::Char('u') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.page_up_half();
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
                    self.last_replayable_action = Some(ReplayableAction::NextGitHunk);
                    Ok(false)
                }
                KeyCode::Char('G') => {
                    self.jump_to_previous_git_marker();
                    self.last_replayable_action = Some(ReplayableAction::PreviousGitHunk);
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

    fn handle_insert_mode_key(&mut self, key_event: KeyEvent) -> Result<bool> {
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

        match key_event.code {
            KeyCode::Esc => {
                self.leave_insert_mode(true);
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
                self.insert_tab();
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

    fn run_find_motion(&mut self, find_kind: FindKind, target: char) -> Result<()> {
        let Some(found_column) = self.find_target_column(find_kind, target)? else {
            return Ok(());
        };

        self.cursor.column = motion_destination_column(find_kind, found_column);
        Ok(())
    }

    fn run_operator_find(
        &mut self,
        operator: PendingOperator,
        find_kind: FindKind,
        target: char,
    ) -> Result<()> {
        let Some(found_column) = self.find_target_column(find_kind, target)? else {
            return Ok(());
        };

        let Some((start_column, end_column)) =
            operator_range(self.cursor.column, found_column, find_kind)
        else {
            return Ok(());
        };

        if matches!(operator, PendingOperator::Yank) {
            return self.yank_range(start_column, end_column);
        }

        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };

        let Some((row, column)) = self.workspace.current_document_mut().remove_display_range(
            self.cursor.row,
            start_column,
            end_column,
            width as usize,
        ) else {
            return Ok(());
        };

        self.cursor.row = row;
        self.cursor.column = column;
        self.clamp_vertical_state();

        if matches!(operator, PendingOperator::Change) {
            self.mode = Mode::Insert;
            self.pending_insert_j = None;
        }

        Ok(())
    }

    fn yank_range(&mut self, start_column: usize, end_column: usize) -> Result<()> {
        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };

        let line_text = self
            .workspace
            .current_document()
            .display_line_text(self.cursor.row, width as usize)?;
        let char_count = line_text.chars().count();
        let start_column = start_column.min(char_count);
        let end_column = end_column.min(char_count);

        self.yank_buffer = YankBuffer::Charwise(
            line_text
                .chars()
                .skip(start_column)
                .take(end_column.saturating_sub(start_column))
                .collect(),
        );
        Ok(())
    }

    fn find_target_column(&self, find_kind: FindKind, target: char) -> Result<Option<usize>> {
        let Ok((width, _)) = terminal::size() else {
            return Ok(None);
        };

        let line_text = self
            .workspace
            .current_document()
            .display_line_text(self.cursor.row, width as usize)?;
        let line_chars: Vec<char> = line_text.chars().collect();
        let cursor_column = self.cursor.column.min(line_chars.len());

        let found = match find_kind {
            FindKind::Forward | FindKind::TillForward => (cursor_column.saturating_add(1)
                ..line_chars.len())
                .find(|index| line_chars[*index] == target),
            FindKind::Backward | FindKind::TillBackward => {
                (0..cursor_column).rev().find(|index| line_chars[*index] == target)
            }
        };

        Ok(found)
    }

    fn replay_last_action(&mut self) -> Result<()> {
        let Some(action) = self.last_replayable_action else {
            return Ok(());
        };

        match action {
            ReplayableAction::NextGitHunk => self.jump_to_next_git_marker(),
            ReplayableAction::PreviousGitHunk => self.jump_to_previous_git_marker(),
        }

        Ok(())
    }

    fn paste_after_cursor(&mut self) -> Result<()> {
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

        Ok(())
    }

    fn change_current_line(&mut self) -> Result<()> {
        let Ok((width, _)) = terminal::size() else {
            return Ok(());
        };

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
                self.cursor.column = 0;
                self.jump_with_context(row, width as usize);
            }
        }

        self.close_go_input();
        Ok(())
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
    ) -> Position {
        let relative_row = self.cursor.row.saturating_sub(self.viewport_row);
        let line_index = relative_row.min(render.lines.len().saturating_sub(1));
        let line = &render.lines[line_index];
        let text_width = line.text.chars().count();
        let column = self.cursor.column.min(text_width);

        let x = area.x.saturating_add(11).saturating_add(column as u16);
        let y = area.y.saturating_add(line_index as u16);

        Position::new(x, y)
    }

    fn go_input_cursor_position(&self, area: Rect) -> Position {
        let popup = centered_rect(24, 3, area);
        Position::new(
            popup.x.saturating_add(5 + self.go_input.value.chars().count() as u16),
            popup.y.saturating_add(1),
        )
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

    fn jump_to_next_git_marker(&mut self) {
        let Ok((width, _)) = terminal::size() else {
            return;
        };

        if let Some(row) = self
            .workspace
            .current_document()
            .next_git_marker_row(self.cursor.row, width as usize)
        {
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
            self.jump_with_context(row, width as usize);
        }
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
    }

    fn leave_insert_mode(&mut self, rewind_cursor: bool) {
        self.mode = Mode::Normal;
        self.pending_insert_j = None;
        if rewind_cursor {
            self.cursor.column = self.cursor.column.saturating_sub(1);
        }
    }

    fn save_current_document(&mut self) -> Result<()> {
        let path = self.workspace.current_document_path().to_path_buf();
        self.workspace.current_document_mut().save(&path)
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

fn format_render_lines(
    lines: &[crate::document::DocumentRenderLine],
    indent_width: usize,
) -> Vec<Line<'static>> {
    let mut formatted_lines = Vec::with_capacity(lines.len());
    let mut previous_guide_width = 0usize;

    for line in lines {
        let current_guide_width = if line.text.is_empty() {
            previous_guide_width
        } else {
            line.text.chars().take_while(|ch| *ch == ' ').count()
        };

        formatted_lines.push(format_render_line(
            line,
            indent_width,
            current_guide_width,
        ));
        previous_guide_width = current_guide_width;
    }

    formatted_lines
}

fn format_render_line(
    line: &crate::document::DocumentRenderLine,
    indent_width: usize,
    empty_line_guide_width: usize,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{:>1}", line.diagnostic_marker),
            Style::default().fg(AppColors::MUTED),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:>6}", line.line_number),
            Style::default().fg(AppColors::MUTED),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:>1}", line.gutter_marker),
            Style::default().fg(git_gutter_color(&line.gutter_marker)),
        ),
        Span::raw(" "),
    ];
    spans.extend(render_text_with_indent_guides(
        &line.text,
        indent_width,
        empty_line_guide_width,
    ));
    Line::from(spans)
}

fn render_text_with_indent_guides(
    text: &str,
    indent_width: usize,
    empty_line_guide_width: usize,
) -> Vec<Span<'static>> {
    let leading_spaces = text.chars().take_while(|ch| *ch == ' ').count();
    let guide_width = indent_width.max(1);

    if leading_spaces == 0 && empty_line_guide_width == 0 {
        return vec![Span::styled(
            text.to_owned(),
            Style::default().fg(AppColors::FOREGROUND),
        )];
    }

    let mut spans = Vec::new();
    let visual_indent_width = if leading_spaces == 0 {
        empty_line_guide_width
    } else {
        leading_spaces
    };
    let guide_count = visual_indent_width / guide_width;
    let trailing_spaces = visual_indent_width % guide_width;

    for _ in 0..guide_count {
        spans.push(Span::styled(
            format!("\u{2502}{}", " ".repeat(guide_width.saturating_sub(1))),
            Style::default().fg(AppColors::INDENT_GUIDE),
        ));
    }

    if trailing_spaces > 0 {
        spans.push(Span::raw(" ".repeat(trailing_spaces)));
    }

    if !text.is_empty() {
        spans.push(Span::styled(
            text.chars().skip(leading_spaces).collect::<String>(),
            Style::default().fg(AppColors::FOREGROUND),
        ));
    }

    spans
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

fn git_gutter_color(marker: &str) -> ratatui::style::Color {
    match marker {
        "A" => AppColors::GIT_ADDED,
        "M" => AppColors::GIT_MODIFIED,
        "D" => AppColors::GIT_REMOVED,
        _ => AppColors::MUTED,
    }
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

fn motion_destination_column(find_kind: FindKind, found_column: usize) -> usize {
    match find_kind {
        FindKind::Forward | FindKind::Backward => found_column,
        FindKind::TillForward => found_column.saturating_sub(1),
        FindKind::TillBackward => found_column.saturating_add(1),
    }
}

fn operator_range(
    cursor_column: usize,
    found_column: usize,
    find_kind: FindKind,
) -> Option<(usize, usize)> {
    let (start_column, end_column) = match find_kind {
        FindKind::Forward => (cursor_column, found_column.saturating_add(1)),
        FindKind::TillForward => (cursor_column, found_column),
        FindKind::Backward => (found_column, cursor_column.saturating_add(1)),
        FindKind::TillBackward => (found_column.saturating_add(1), cursor_column.saturating_add(1)),
    };

    (end_column > start_column).then_some((start_column, end_column))
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area.y.saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

#[allow(dead_code)]
fn _project_file_display_name(candidate: &ProjectFileCandidate) -> &str {
    &candidate.display_name
}
