use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use crossterm::{
    cursor,
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
                .render_first_page(area.height as usize, area.width as usize)
                .expect("document render should succeed during draw");

            let layout = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

            let background = Block::default().style(Style::default().bg(AppColors::BACKGROUND));
            let content = Paragraph::new(render.lines.join("\n")).style(
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
            Mode::Insert | Mode::Command | Mode::Shell => Ok(false),
        }
    }

    fn handle_normal_mode_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key_event.code, KeyCode::Char('c'))
        {
            return Ok(true);
        }

        match key_event.code {
            KeyCode::Char('q') => Ok(true),
            _ => Ok(false),
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
}

impl Workspace {
    pub fn current_document(&self) -> &Document {
        &self.documents[self.current_index].document
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
}

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    active: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        let mut stdout = std::io::stdout();
        terminal::enable_raw_mode()?;
        execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
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

#[allow(dead_code)]
fn _project_file_display_name(candidate: &ProjectFileCandidate) -> &str {
    &candidate.display_name
}
