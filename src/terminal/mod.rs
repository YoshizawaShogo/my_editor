use std::io::{self, Stdout, Write};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::{
    cursor::{Hide, SetCursorStyle, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, ClearType, CrosstermBackend, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
};

use crate::Result;

pub type Tui = Terminal<StableCrosstermBackend>;

pub struct StableCrosstermBackend {
    inner: CrosstermBackend<Stdout>,
    bottom_up: bool,
    show_cursor_on_flush: bool,
}

impl StableCrosstermBackend {
    fn new(stdout: Stdout) -> Self {
        Self {
            inner: CrosstermBackend::new(stdout),
            bottom_up: false,
            show_cursor_on_flush: false,
        }
    }
}

impl Write for StableCrosstermBackend {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(&mut self.inner)
    }
}

impl Backend for StableCrosstermBackend {
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut updates = content.collect::<Vec<_>>();
        if self.bottom_up {
            updates.sort_by_key(|(x, y, _)| (std::cmp::Reverse(*y), *x));
        }
        self.inner.draw(updates.into_iter())
    }

    fn append_lines(&mut self, count: u16) -> io::Result<()> {
        self.inner.append_lines(count)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.show_cursor_on_flush = false;
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        // ratatui calls show_cursor before set_cursor_position. Deferring the show
        // avoids exposing the cursor at the final diff cell for one terminal paint.
        self.show_cursor_on_flush = true;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.show_cursor_on_flush {
            self.inner.show_cursor()?;
            self.show_cursor_on_flush = false;
        }
        Backend::flush(&mut self.inner)
    }
}

pub struct TerminalSession {
    terminal: Tui,
}

impl TerminalSession {
    pub fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
            SetCursorStyle::BlinkingBar,
            Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }

        match Terminal::new(StableCrosstermBackend::new(stdout)) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                restore_terminal();
                Err(error.into())
            }
        }
    }

    pub fn terminal_mut(&mut self) -> &mut Tui {
        &mut self.terminal
    }

    pub fn draw(&mut self, bottom_up: bool, render: impl FnOnce(&mut Frame<'_>)) -> Result<()> {
        self.terminal.backend_mut().bottom_up = bottom_up;
        let draw_result = self
            .terminal
            .try_draw(|frame| {
                render(frame);
                // Start the synchronized window after CPU-side rendering. VTE terminals
                // automatically time out long synchronized updates, which otherwise exposes
                // ratatui's row-by-row diff while a newline shifts the following rows.
                execute!(io::stdout(), BeginSynchronizedUpdate)
            })
            .map(|_| ());
        let end_result = execute!(self.terminal.backend_mut(), EndSynchronizedUpdate);
        draw_result?;
        end_result?;
        Ok(())
    }

    pub fn copy_osc52(&mut self, text: &str) -> Result<()> {
        let encoded = STANDARD.encode(text.as_bytes());
        write!(self.terminal.backend_mut(), "\x1b]52;c;{encoded}\x07")?;
        Write::flush(self.terminal.backend_mut())?;
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
        let _ = execute!(
            self.terminal.backend_mut(),
            Show,
            SetCursorStyle::DefaultUserShape,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
        let _ = disable_raw_mode();
    }
}

pub fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stderr(),
        Show,
        SetCursorStyle::DefaultUserShape,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
}
