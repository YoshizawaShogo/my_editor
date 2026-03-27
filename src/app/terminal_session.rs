use std::sync::{Arc, atomic::AtomicBool};

use crossterm::{
    cursor::{self, SetCursorStyle},
    event::DisableMouseCapture,
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use signal_hook::{consts::signal::SIGTSTP, flag, low_level};

use crate::error::Result;

pub(super) struct TerminalSession {
    terminal: Terminal<CrosstermBackend<std::io::Stdout>>,
    _suspend_signal_guard: SuspendSignalGuard,
    active: bool,
}

impl TerminalSession {
    pub(super) fn enter() -> Result<Self> {
        let mut stdout = std::io::stdout();
        terminal::enable_raw_mode()?;
        execute!(
            stdout,
            EnterAlternateScreen,
            DisableMouseCapture,
            cursor::Show,
            SetCursorStyle::SteadyBar
        )?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            _suspend_signal_guard: SuspendSignalGuard::enter(),
            active: true,
        })
    }

    pub(super) fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<std::io::Stdout>> {
        &mut self.terminal
    }

    pub(super) fn leave(&mut self) -> Result<()> {
        if !self.active {
            return Ok(());
        }

        self.terminal.flush()?;
        execute!(
            self.terminal.backend_mut(),
            DisableMouseCapture,
            cursor::Show,
            LeaveAlternateScreen
        )?;
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
            let signal_id =
                flag::register(SIGTSTP, Arc::clone(&ignored)).expect("failed to register SIGTSTP handler");
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
            let _ = execute!(
                self.terminal.backend_mut(),
                DisableMouseCapture,
                cursor::Show,
                LeaveAlternateScreen
            );
            let _ = terminal::disable_raw_mode();
        }
    }
}
