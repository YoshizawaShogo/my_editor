use std::io::{self, Stdout, Write};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::{
    cursor::{Hide, SetCursorStyle, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};

use crate::Result;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

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
            EnableMouseCapture,
            SetCursorStyle::SteadyBar,
            Hide
        ) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }

        match Terminal::new(CrosstermBackend::new(stdout)) {
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

    pub fn draw(&mut self, render: impl FnOnce(&mut Frame<'_>)) -> Result<()> {
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
        self.terminal.backend_mut().flush()?;
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
        DisableMouseCapture,
        LeaveAlternateScreen
    );
}
