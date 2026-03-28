use std::{
    fs::File,
    io::{self, Read, Write},
    os::fd::AsRawFd,
    os::unix::process::CommandExt,
    sync::mpsc,
    thread,
};

use crossterm::{
    event::{KeyCode, KeyEvent, KeyModifiers},
    terminal,
};
use nix::{
    libc,
    pty::{Winsize, openpty},
    unistd::{dup2_stderr, dup2_stdin, dup2_stdout, getpid, setsid, tcsetpgrp},
};

use crate::{error::Result, mode::Mode};

use super::{App, FocusedPane, LayoutMode};

impl App {
    pub(super) fn ensure_shell_started(&mut self) -> Result<()> {
        if self.shell.child.is_some() {
            return Ok(());
        }

        let (rows, cols) = shell_size_for_layout(LayoutMode::TerminalSplit);
        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = openpty(Some(&winsize), None).map_err(io::Error::from)?;
        let master = File::from(pty.master);
        let reader = master.try_clone()?;
        let writer = master.try_clone()?;
        let slave = File::from(pty.slave);
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let mut reader = reader;
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if tx.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });

        let mut command = std::process::Command::new(&self.shell.program);
        command.arg("-i");
        unsafe {
            command.pre_exec(move || {
                setsid().map_err(io::Error::from)?;
                if libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                dup2_stdin(&slave).map_err(io::Error::from)?;
                dup2_stdout(&slave).map_err(io::Error::from)?;
                dup2_stderr(&slave).map_err(io::Error::from)?;
                tcsetpgrp(&slave, getpid()).map_err(io::Error::from)?;
                Ok(())
            });
        }
        let child = command.spawn()?;

        self.shell.child = Some(child);
        self.shell.pty = Some(writer);
        self.shell.output_rx = Some(rx);
        self.shell.parser = Some(vt100::Parser::new(rows, cols, 0));
        self.shell.rows = rows;
        self.shell.cols = cols;
        self.show_toast(format!("Started shell {}", self.shell.program));
        Ok(())
    }

    pub(super) fn poll_shell_output(&mut self) {
        let Some(rx) = &self.shell.output_rx else {
            return;
        };

        let mut chunks = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            chunks.push(chunk);
        }

        for chunk in chunks {
            if let Some(parser) = &mut self.shell.parser {
                parser.process(&chunk);
            }
        }
    }

    pub(super) fn shutdown_shell(&mut self) {
        self.shell.pty.take();
        if let Some(mut child) = self.shell.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.shell.output_rx = None;
        self.shell.parser = None;
    }

    pub(super) fn handle_shell_mode_key(&mut self, key_event: KeyEvent) -> Result<bool> {
        if key_event.modifiers.contains(KeyModifiers::CONTROL) {
            match key_event.code {
                KeyCode::Char('l') => {
                    self.advance_layout_or_focus();
                    return Ok(false);
                }
                KeyCode::Char('o') => {
                    self.collapse_to_single_pane();
                    return Ok(false);
                }
                KeyCode::Null | KeyCode::Char(' ') => {
                    self.toggle_terminal_split()?;
                    return Ok(false);
                }
                KeyCode::Char(ch) => {
                    let control = (ch.to_ascii_lowercase() as u8) & 0x1f;
                    self.write_shell_input(&[control])?;
                    return Ok(false);
                }
                _ => {}
            }
        }

        match key_event.code {
            KeyCode::Esc => {
                self.focused_pane = FocusedPane::Left;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                self.write_shell_input(b"\r")?;
            }
            KeyCode::Backspace => {
                self.write_shell_input(&[0x7f])?;
            }
            KeyCode::Tab => {
                self.write_shell_input(b"\t")?;
            }
            KeyCode::Delete => {
                self.write_shell_input(b"\x1b[3~")?;
            }
            KeyCode::Up => {
                self.write_shell_input(b"\x1b[A")?;
            }
            KeyCode::Down => {
                self.write_shell_input(b"\x1b[B")?;
            }
            KeyCode::Right => {
                self.write_shell_input(b"\x1b[C")?;
            }
            KeyCode::Left => {
                self.write_shell_input(b"\x1b[D")?;
            }
            KeyCode::Home => {
                self.write_shell_input(b"\x1b[H")?;
            }
            KeyCode::End => {
                self.write_shell_input(b"\x1b[F")?;
            }
            KeyCode::Char(ch) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                let mut buffer = [0_u8; 4];
                let encoded = ch.encode_utf8(&mut buffer);
                self.write_shell_input(encoded.as_bytes())?;
            }
            _ => {}
        }

        Ok(false)
    }

    pub(super) fn toggle_terminal_split(&mut self) -> Result<()> {
        self.ensure_shell_started()?;
        self.layout_mode = match self.layout_mode {
            LayoutMode::TerminalSplit => LayoutMode::Single,
            _ => LayoutMode::TerminalSplit,
        };
        self.focused_pane = match self.layout_mode {
            LayoutMode::TerminalSplit => FocusedPane::Right,
            _ => FocusedPane::Left,
        };
        self.sync_shell_size()?;
        Ok(())
    }

    pub(super) fn sync_shell_size(&mut self) -> Result<()> {
        let Some(pty) = &self.shell.pty else {
            return Ok(());
        };

        let target_layout = if self.focused_pane == FocusedPane::Right && self.layout_mode == LayoutMode::Single {
            LayoutMode::Single
        } else {
            LayoutMode::TerminalSplit
        };
        let (rows, cols) = shell_size_for_layout(target_layout);
        if self.shell.rows == rows && self.shell.cols == cols {
            return Ok(());
        }

        let winsize = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe { libc::ioctl(pty.as_raw_fd(), libc::TIOCSWINSZ, &winsize) };
        if result == -1 {
            return Err(io::Error::last_os_error().into());
        }

        if let Some(parser) = &mut self.shell.parser {
            parser.set_size(rows, cols);
        }
        self.shell.rows = rows;
        self.shell.cols = cols;
        Ok(())
    }

    fn write_shell_input(&mut self, bytes: &[u8]) -> Result<()> {
        if let Some(pty) = &mut self.shell.pty {
            pty.write_all(bytes)?;
            pty.flush()?;
        }
        Ok(())
    }
}

fn shell_size_for_layout(layout_mode: LayoutMode) -> (u16, u16) {
    let (columns, rows) = terminal::size().unwrap_or((120, 40));
    let content_rows = rows.saturating_sub(1).max(1);
    let content_cols = match layout_mode {
        LayoutMode::Single => columns.max(1),
        LayoutMode::Dual | LayoutMode::TerminalSplit => {
            (columns.saturating_sub(1) / 2).max(1)
        }
    };
    (content_rows, content_cols)
}
