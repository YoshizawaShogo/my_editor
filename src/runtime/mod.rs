mod input;

use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    os::unix::{io::AsRawFd, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use lsp_server::{Message, Notification, Request, RequestId};
use nix::pty::{Winsize, openpty};
use tokio::sync::mpsc;

use crate::{
    Result,
    config::Config,
    document::{DiskState, LargeFile, PersistedHistory, history_key},
    editor::{
        AppEvent, Editor, Effect, FileScanEvent, GitEvent, GitLine, GitLineKind, GrepEvent,
        GrepHit, IoEvent, TerminalEvent,
    },
    input::{KeyChordState, RawInput, translate},
    render,
    terminal::TerminalSession,
};

pub struct Runtime {
    editor: Editor,
    tx: mpsc::UnboundedSender<AppEvent>,
    rx: mpsc::UnboundedReceiver<AppEvent>,
    raw_tx: mpsc::UnboundedSender<RawInput>,
    raw_rx: mpsc::UnboundedReceiver<RawInput>,
    pending_keys: KeyChordState,
    startup_effects: Vec<Effect>,
    terminal: TerminalSession,
    lsp: std::collections::HashMap<u64, LspHandle>,
    shell: Option<ShellHandle>,
}

impl Runtime {
    pub fn new(terminal: TerminalSession, paths: Vec<PathBuf>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (raw_tx, raw_rx) = mpsc::unbounded_channel();
        let mut editor = Editor::default();
        editor.set_workspace_root(find_workspace_root());
        let mut startup_effects = vec![Effect::LoadConfig];
        startup_effects.extend(editor.open_paths(paths));
        Self {
            editor,
            tx,
            rx,
            raw_tx,
            raw_rx,
            pending_keys: KeyChordState::default(),
            startup_effects,
            terminal,
            lsp: std::collections::HashMap::new(),
            shell: None,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        let input_task = input::spawn(self.raw_tx.clone());
        let size = self.terminal.terminal_mut().size()?;
        self.editor.update(AppEvent::Resize {
            cols: size.width,
            rows: size.height,
        });
        self.draw()?;
        self.editor.take_dirty();
        for effect in std::mem::take(&mut self.startup_effects) {
            self.execute(effect).await?;
        }
        let mut disk_check = tokio::time::interval_at(
            tokio::time::Instant::now() + std::time::Duration::from_secs(2),
            std::time::Duration::from_secs(2),
        );

        while !self.editor.should_quit() {
            tokio::select! {
                Some(raw) = self.raw_rx.recv() => {
                    self.enqueue_raw_input(raw);
                }
                Some(event) = self.rx.recv() => {
                    self.process_events(event).await?;
                }
                _ = disk_check.tick() => {
                    let _ = self.tx.send(AppEvent::Tick);
                }
                else => break,
            }
        }

        input_task.abort();
        Ok(())
    }

    fn enqueue_raw_input(&mut self, raw: RawInput) {
        if let Some(event) = translate(raw, &self.editor.focus(), &mut self.pending_keys) {
            let _ = self.tx.send(event);
        }
    }

    async fn process_events(&mut self, first: AppEvent) -> Result<()> {
        let mut effects = self.editor.update(first);
        while let Ok(event) = self.rx.try_recv() {
            effects.extend(self.editor.update(event));
        }

        for effect in effects {
            self.execute(effect).await?;
        }

        if self.editor.take_dirty() {
            self.draw()?;
        }
        Ok(())
    }

    async fn execute(&mut self, effect: Effect) -> Result<()> {
        match effect {
            Effect::LoadConfig => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = load_config();
                    let _ = tx.send(AppEvent::ConfigLoaded(result));
                });
            }
            Effect::ReadFile { id, path } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    const LARGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024;
                    let is_large = fs::metadata(&path)
                        .map(|metadata| metadata.len() > LARGE_FILE_THRESHOLD)
                        .unwrap_or(false);
                    let event = if is_large {
                        let result = LargeFile::open(&path).and_then(|large| {
                            if large.validate_text() {
                                Ok(large)
                            } else {
                                Err(format!(
                                    "バイナリ/非UTF-8のため開けません: {}",
                                    path.display()
                                ))
                            }
                        });
                        IoEvent::LargeFileLoaded { id, result }
                    } else {
                        IoEvent::FileLoaded {
                            id,
                            result: read_utf8_file(&path),
                        }
                    };
                    let _ = tx.send(AppEvent::Io(event));
                    let _ = tx.send(AppEvent::Io(IoEvent::DiskStateObserved {
                        id,
                        result: disk_state(&path),
                    }));
                });
            }
            Effect::WriteFile {
                doc,
                path,
                contents,
                expected,
            } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = match (expected, disk_state(&path)) {
                        (Some(expected), Ok(current)) if expected != current => {
                            let _ = tx.send(AppEvent::Io(IoEvent::SaveConflict { id: doc, path }));
                            return;
                        }
                        (_, Err(error)) if path.exists() => Err(error),
                        _ => atomic_write(&path, contents.as_bytes()),
                    };
                    let saved = result.is_ok();
                    let _ = tx.send(AppEvent::Io(IoEvent::FileSaved { id: doc, result }));
                    if saved && let Ok(state) = disk_state(&path) {
                        let _ = tx.send(AppEvent::Io(IoEvent::DiskStateObserved {
                            id: doc,
                            result: Ok(state),
                        }));
                    }
                });
            }
            Effect::LoadUndoHistory { id, path } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = load_undo_history(&path);
                    let _ = tx.send(AppEvent::Io(IoEvent::UndoHistoryLoaded { id, result }));
                });
            }
            Effect::SaveUndoHistory(history) => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = save_undo_history(&history);
                    let _ = tx.send(AppEvent::Io(IoEvent::UndoHistorySaved { result }));
                });
            }
            Effect::StartFileScan { root, token } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let mut paths = Vec::new();
                    for entry in ignore::WalkBuilder::new(&root)
                        .hidden(true)
                        .git_ignore(true)
                        .ignore(true)
                        .build()
                    {
                        match entry {
                            Ok(entry) if entry.file_type().is_some_and(|kind| kind.is_file()) => {
                                paths.push(entry.into_path());
                                if paths.len() >= 256 {
                                    let batch = std::mem::take(&mut paths);
                                    let _ = tx.send(AppEvent::FileScan(FileScanEvent::Batch {
                                        token,
                                        paths: batch,
                                    }));
                                }
                            }
                            Ok(_) => {}
                            Err(error) => {
                                let _ = tx.send(AppEvent::FileScan(FileScanEvent::Failed {
                                    token,
                                    error: format!("ファイル走査に失敗: {error}"),
                                }));
                            }
                        }
                    }
                    if !paths.is_empty() {
                        let _ = tx.send(AppEvent::FileScan(FileScanEvent::Batch { token, paths }));
                    }
                    let _ = tx.send(AppEvent::FileScan(FileScanEvent::Done { token }));
                });
            }
            Effect::StartGrep {
                pattern,
                filters,
                root,
                token,
            } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let pattern = match regex::Regex::new(&pattern) {
                        Ok(pattern) => pattern,
                        Err(error) => {
                            let _ = tx.send(AppEvent::Grep(GrepEvent::Failed {
                                token,
                                error: format!("検索式が不正です: {error}"),
                            }));
                            return;
                        }
                    };
                    let mut walker = ignore::WalkBuilder::new(&root);
                    walker
                        .hidden(!filters.include_hidden)
                        .git_ignore(filters.respect_ignore_files)
                        .git_exclude(filters.respect_ignore_files)
                        .ignore(filters.respect_ignore_files);
                    let mut overrides = ignore::overrides::OverrideBuilder::new(&root);
                    for include in &filters.include {
                        if let Err(error) = overrides.add(include) {
                            let _ = tx.send(AppEvent::Grep(GrepEvent::Failed {
                                token,
                                error: format!("include globが不正です: {error}"),
                            }));
                            return;
                        }
                    }
                    for exclude in &filters.exclude {
                        if let Err(error) = overrides.add(&format!("!{exclude}")) {
                            let _ = tx.send(AppEvent::Grep(GrepEvent::Failed {
                                token,
                                error: format!("exclude globが不正です: {error}"),
                            }));
                            return;
                        }
                    }
                    if let Ok(overrides) = overrides.build() {
                        walker.overrides(overrides);
                    }
                    let mut hits = Vec::new();
                    for entry in walker.build().flatten() {
                        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                            continue;
                        }
                        let Ok(file) = File::open(entry.path()) else {
                            continue;
                        };
                        for (line, result) in BufReader::new(file).lines().enumerate() {
                            let Ok(text) = result else { break };
                            if pattern.is_match(&text) {
                                hits.push(GrepHit {
                                    path: entry.path().to_path_buf(),
                                    line,
                                    text,
                                });
                                if hits.len() >= 128 {
                                    let batch = std::mem::take(&mut hits);
                                    let _ = tx.send(AppEvent::Grep(GrepEvent::Hits {
                                        token,
                                        hits: batch,
                                    }));
                                }
                            }
                        }
                    }
                    if !hits.is_empty() {
                        let _ = tx.send(AppEvent::Grep(GrepEvent::Hits { token, hits }));
                    }
                    let _ = tx.send(AppEvent::Grep(GrepEvent::Done { token }));
                });
            }
            Effect::ReplaceFiles {
                paths,
                pattern,
                replacement,
            } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = (|| {
                        let pattern = regex::Regex::new(&pattern)
                            .map_err(|error| format!("検索式が不正です: {error}"))?;
                        let mut changed = 0;
                        for path in paths {
                            let contents = read_utf8_file(&path)?;
                            let replaced = pattern.replace_all(&contents, replacement.as_str());
                            if replaced != contents {
                                atomic_write(&path, replaced.as_bytes())?;
                                changed += 1;
                            }
                        }
                        Ok(changed)
                    })();
                    let _ = tx.send(AppEvent::Io(IoEvent::DirectoryReplaceFinished { result }));
                });
            }
            Effect::ApplyFileEdits { path, edits_json } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = apply_file_edits(&path, &edits_json).map(|()| path);
                    let _ = tx.send(AppEvent::Io(IoEvent::ExternalEditsFinished { result }));
                });
            }
            Effect::ComputeGitStatus { doc, path } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = Command::new("git")
                        .args(["diff", "--unified=0", "HEAD", "--"])
                        .arg(&path)
                        .output()
                        .map_err(|error| format!("git diffを実行できません: {error}"))
                        .map(|output| parse_git_diff(&String::from_utf8_lossy(&output.stdout)));
                    let _ = tx.send(AppEvent::Git(GitEvent { doc, result }));
                });
            }
            Effect::SpawnLsp {
                server,
                language,
                command,
                root,
            } => self.spawn_lsp(server, language, command, root),
            Effect::LspSend { server, message } => {
                let parsed: Message = serde_json::from_str(&message)
                    .map_err(|error| crate::Error::Io(std::io::Error::other(error)))?;
                if let Some(handle) = self.lsp.get(&server) {
                    let _ = handle.sender.send(parsed);
                }
            }
            Effect::LspRequest {
                server,
                id,
                method,
                params,
            } => {
                let params = serde_json::from_str(&params)
                    .map_err(|error| crate::Error::Io(std::io::Error::other(error)))?;
                if let Some(handle) = self.lsp.get(&server) {
                    let _ = handle.sender.send(Message::Request(Request {
                        id: RequestId::from(id as i32),
                        method,
                        params,
                    }));
                }
            }
            Effect::ScheduleLspRestart { server, delay_ms } => {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    let _ = tx.send(AppEvent::Lsp(crate::lsp::LspEvent::RestartDue { server }));
                });
            }
            Effect::CheckDiskStates(files) => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    for (id, path) in files {
                        let result = disk_state(&path);
                        let _ = tx.send(AppEvent::Io(IoEvent::DiskStateObserved { id, result }));
                    }
                });
            }
            Effect::ResolveDirectPath { input, root } => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let expanded = if let Some(rest) = input.strip_prefix("~/") {
                        std::env::var_os("HOME")
                            .map(PathBuf::from)
                            .unwrap_or_else(|| root.clone())
                            .join(rest)
                    } else {
                        let path = PathBuf::from(&input);
                        if path.is_absolute() {
                            path
                        } else {
                            root.join(path)
                        }
                    };
                    let path = if expanded.exists() {
                        fs::canonicalize(&expanded).unwrap_or(expanded)
                    } else {
                        normalize_path(&expanded)
                    };
                    let canonical_root = fs::canonicalize(&root).unwrap_or(root);
                    let exists = path.is_file();
                    let parent_exists = path.parent().is_some_and(Path::is_dir);
                    let inside_root = path.starts_with(canonical_root);
                    let _ = tx.send(AppEvent::Io(IoEvent::DirectPathResolved {
                        path,
                        exists,
                        parent_exists,
                        inside_root,
                    }));
                });
            }
            Effect::SpawnShell { cols, rows } => self.spawn_shell(cols, rows),
            Effect::TerminalInput(bytes) => {
                if let Some(shell) = &mut self.shell {
                    shell.writer.write_all(&bytes)?;
                    shell.writer.flush()?;
                }
            }
            Effect::TerminalResize { cols, rows } => {
                if let Some(shell) = &self.shell {
                    let size = Winsize {
                        ws_row: rows,
                        ws_col: cols,
                        ws_xpixel: 0,
                        ws_ypixel: 0,
                    };
                    // SAFETY: ioctl receives a valid PTY master fd and Winsize pointer.
                    unsafe {
                        nix::libc::ioctl(shell.writer.as_raw_fd(), nix::libc::TIOCSWINSZ, &size);
                    }
                }
            }
            Effect::ClipboardOsc52(text) => self.terminal.copy_osc52(&text)?,
            Effect::Quit => {}
        }
        Ok(())
    }

    fn spawn_lsp(&mut self, server: u64, language: String, command: Vec<String>, root: PathBuf) {
        if let Some(mut previous) = self.lsp.remove(&server) {
            let _ = previous.child.kill();
            let _ = previous.child.wait();
        }
        let Some(program) = command.first() else {
            return;
        };
        let mut child = match Command::new(program)
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = self.tx.send(AppEvent::Lsp(crate::lsp::LspEvent::Exited {
                    server,
                    error: Some(format!("{language} LSPを起動できません: {error}")),
                }));
                return;
            }
        };
        let Some(stdin) = child.stdin.take() else {
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            return;
        };
        let (sender, receiver) = std::sync::mpsc::channel::<Message>();
        std::thread::spawn(move || {
            let mut stdin = stdin;
            while let Ok(message) = receiver.recv() {
                if message.write(&mut stdin).is_err() {
                    break;
                }
            }
        });
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut stdout = BufReader::new(stdout);
            loop {
                match Message::read(&mut stdout) {
                    Ok(Some(Message::Response(response))) if response.id.to_string() == "0" => {
                        let _ =
                            tx.send(AppEvent::Lsp(crate::lsp::LspEvent::Initialized { server }));
                    }
                    Ok(Some(Message::Response(response))) => {
                        if let Ok(id) = response.id.to_string().parse::<i64>() {
                            let result = response.error.map_or_else(
                                || Ok(response.result.unwrap_or_default()),
                                |error| Err(error.message),
                            );
                            let _ = tx
                                .send(AppEvent::Lsp(crate::lsp::LspEvent::Response { id, result }));
                        }
                    }
                    Ok(Some(Message::Notification(notification))) => {
                        handle_lsp_notification(server, notification, &tx);
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        let _ = tx.send(AppEvent::Lsp(crate::lsp::LspEvent::Exited {
                            server,
                            error: None,
                        }));
                        break;
                    }
                    Err(error) => {
                        let _ = tx.send(AppEvent::Lsp(crate::lsp::LspEvent::Exited {
                            server,
                            error: Some(format!("LSP read error: {error}")),
                        }));
                        break;
                    }
                }
            }
        });
        let initialize = Message::Request(Request {
            id: RequestId::from(0i32),
            method: "initialize".to_owned(),
            params: serde_json::json!({
                "processId": std::process::id(),
                "rootUri": format!("file://{}", root.display()),
                "capabilities": {},
                "clientInfo": {"name": "my_editor", "version": env!("CARGO_PKG_VERSION")}
            }),
        });
        let _ = sender.send(initialize);
        self.lsp.insert(server, LspHandle { sender, child });
        let _ = self.tx.send(AppEvent::Lsp(crate::lsp::LspEvent::Spawned {
            server,
            language,
        }));
    }

    fn spawn_shell(&mut self, cols: u16, rows: u16) {
        if let Some(mut existing) = self.shell.take() {
            let _ = existing.child.kill();
            let _ = existing.child.wait();
        }
        let size = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = match openpty(&size, None) {
            Ok(pty) => pty,
            Err(error) => {
                let _ = self
                    .tx
                    .send(AppEvent::Terminal(TerminalEvent::Exited(Some(format!(
                        "PTYを作成できません: {error}"
                    )))));
                return;
            }
        };
        let stdin = match pty.slave.try_clone() {
            Ok(fd) => Stdio::from(fd),
            Err(_) => return,
        };
        let stdout = match pty.slave.try_clone() {
            Ok(fd) => Stdio::from(fd),
            Err(_) => return,
        };
        let slave_fd = pty.slave.as_raw_fd();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
        let mut shell_command = Command::new(shell);
        shell_command
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::from(pty.slave));
        // SAFETY: This closure only performs async-signal-safe session/ioctl setup before exec.
        unsafe {
            shell_command.pre_exec(move || {
                nix::unistd::setsid().map_err(std::io::Error::other)?;
                if nix::libc::ioctl(slave_fd, nix::libc::TIOCSCTTY, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = match shell_command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = self
                    .tx
                    .send(AppEvent::Terminal(TerminalEvent::Exited(Some(format!(
                        "シェルを起動できません: {error}"
                    )))));
                return;
            }
        };
        let master = File::from(pty.master);
        let mut reader = match master.try_clone() {
            Ok(reader) => reader,
            Err(_) => return,
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = tx.send(AppEvent::Terminal(TerminalEvent::Exited(None)));
                        break;
                    }
                    Ok(read) => {
                        let _ = tx.send(AppEvent::Terminal(TerminalEvent::Output(
                            buffer[..read].to_vec(),
                        )));
                    }
                    Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => {
                        let _ = tx.send(AppEvent::Terminal(TerminalEvent::Exited(None)));
                        break;
                    }
                    Err(error) => {
                        let _ = tx.send(AppEvent::Terminal(TerminalEvent::Exited(Some(format!(
                            "PTY read error: {error}"
                        )))));
                        break;
                    }
                }
            }
        });
        self.shell = Some(ShellHandle {
            writer: master,
            child,
        });
    }

    fn draw(&mut self) -> Result<()> {
        self.terminal
            .terminal_mut()
            .draw(|frame| render::draw(frame, &self.editor))?;
        Ok(())
    }
}

struct LspHandle {
    sender: std::sync::mpsc::Sender<Message>,
    child: Child,
}

struct ShellHandle {
    writer: File,
    child: Child,
}

impl Drop for Runtime {
    fn drop(&mut self) {
        for handle in self.lsp.values_mut() {
            let _ = handle.child.kill();
            let _ = handle.child.wait();
        }
        if let Some(shell) = &mut self.shell {
            let _ = shell.child.kill();
            let _ = shell.child.wait();
        }
    }
}

fn handle_lsp_notification(
    server: u64,
    notification: Notification,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    if notification.method == "textDocument/publishDiagnostics"
        && let Ok(params) = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(
            notification.params.clone(),
        )
    {
        let diagnostics = params
            .diagnostics
            .into_iter()
            .map(|diagnostic| crate::lsp::Diagnostic {
                line: diagnostic.range.start.line,
                character: diagnostic.range.start.character,
                severity: match diagnostic.severity {
                    Some(lsp_types::DiagnosticSeverity::ERROR) => {
                        crate::lsp::DiagnosticSeverity::Error
                    }
                    Some(lsp_types::DiagnosticSeverity::WARNING) => {
                        crate::lsp::DiagnosticSeverity::Warning
                    }
                    Some(lsp_types::DiagnosticSeverity::INFORMATION) => {
                        crate::lsp::DiagnosticSeverity::Information
                    }
                    _ => crate::lsp::DiagnosticSeverity::Hint,
                },
                message: diagnostic.message,
            })
            .collect();
        let _ = tx.send(AppEvent::Lsp(crate::lsp::LspEvent::Diagnostics {
            uri: params.uri.as_str().to_owned(),
            diagnostics,
        }));
    } else if notification.method == "$/progress" {
        let _ = tx.send(AppEvent::Lsp(crate::lsp::LspEvent::Progress {
            server,
            message: notification.params.to_string(),
        }));
    }
}

fn read_utf8_file(path: &std::path::Path) -> std::result::Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("ファイルを開けません {}: {error}", path.display()))?;
    let sample = &bytes[..bytes.len().min(8192)];
    if sample.contains(&0) || std::str::from_utf8(sample).is_err() {
        return Err(format!(
            "バイナリ/非UTF-8のため開けません: {}",
            path.display()
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| format!("バイナリ/非UTF-8のため開けません: {}", path.display()))
}

fn apply_file_edits(path: &Path, edits_json: &str) -> std::result::Result<(), String> {
    let edits: Vec<lsp_types::TextEdit> = serde_json::from_str(edits_json)
        .map_err(|error| format!("LSP編集を解釈できません: {error}"))?;
    let mut contents = read_utf8_file(path)?;
    let rope = ropey::Rope::from_str(&contents);
    let mut replacements: Vec<_> = edits
        .into_iter()
        .map(|edit| {
            let start = crate::position::lsp_position_to_char_idx(
                &rope,
                edit.range.start.line as usize,
                edit.range.start.character as usize,
            );
            let end = crate::position::lsp_position_to_char_idx(
                &rope,
                edit.range.end.line as usize,
                edit.range.end.character as usize,
            );
            (
                rope.char_to_byte(start.0),
                rope.char_to_byte(end.0),
                edit.new_text,
            )
        })
        .collect();
    replacements.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, replacement) in replacements {
        contents.replace_range(start..end, &replacement);
    }
    atomic_write(path, contents.as_bytes())
}

fn disk_state(path: &Path) -> std::result::Result<DiskState, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("ファイル状態を取得できません {}: {error}", path.display()))?;
    let modified = metadata
        .modified()
        .map_err(|error| format!("mtimeを取得できません {}: {error}", path.display()))?;
    let modified_nanos = modified
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(DiskState {
        size: metadata.len(),
        modified_nanos,
    })
}

fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn find_workspace_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.ancestors()
        .find(|directory| directory.join(".git").exists())
        .map_or(cwd.clone(), Path::to_path_buf)
}

fn parse_git_diff(diff: &str) -> Vec<GitLine> {
    let mut lines = Vec::new();
    for line in diff.lines() {
        let Some(hunk) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(plus) = hunk.split_whitespace().find(|part| part.starts_with('+')) else {
            continue;
        };
        let mut parts = plus.trim_start_matches('+').split(',');
        let start = parts
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1);
        let count = parts
            .next()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1);
        if count == 0 {
            lines.push(GitLine {
                line: start.saturating_sub(1),
                kind: GitLineKind::Deleted,
            });
        } else {
            for current in start..start + count {
                lines.push(GitLine {
                    line: current.saturating_sub(1),
                    kind: GitLineKind::Modified,
                });
            }
        }
    }
    lines
}

fn load_config() -> std::result::Result<Config, String> {
    let Some(home) = std::env::var_os("HOME") else {
        return Ok(Config::default());
    };
    let path = PathBuf::from(home).join(".my_editor.toml");
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(error) => return Err(format!("設定を読めません {}: {error}", path.display())),
    };
    toml::from_str(&contents).map_err(|error| format!("設定が不正です {}: {error}", path.display()))
}

fn atomic_write(path: &Path, contents: &[u8]) -> std::result::Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(format!("ディレクトリが存在しません: {}", parent.display()));
    }
    let existing_permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut temporary = None;
    for attempt in 0..100u32 {
        let name = format!(".my_editor.{}.{}.tmp", std::process::id(), attempt);
        let candidate = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("一時ファイルを作成できません: {error}")),
        }
    }
    let Some((temporary_path, mut file)) = temporary else {
        return Err("保存用一時ファイル名を確保できません".to_owned());
    };
    let write_result = (|| {
        file.write_all(contents)?;
        file.sync_all()?;
        if let Some(permissions) = existing_permissions {
            fs::set_permissions(&temporary_path, permissions)?;
        }
        fs::rename(&temporary_path, path)?;
        File::open(parent)?.sync_all()?;
        Ok::<(), std::io::Error>(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(format!("保存できません {}: {error}", path.display()));
    }
    Ok(())
}

fn undo_directory() -> std::result::Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME が設定されていません".to_owned())?;
    Ok(PathBuf::from(home).join("my_editor_undo_history"))
}

fn undo_history_path(path: &Path) -> std::result::Result<PathBuf, String> {
    Ok(undo_directory()?.join(format!("{}.json", history_key(path))))
}

fn load_undo_history(
    document_path: &Path,
) -> std::result::Result<Option<PersistedHistory>, String> {
    let path = undo_history_path(document_path)?;
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("undo履歴を読めません {}: {error}", path.display())),
    };
    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| format!("undo履歴が壊れています {}: {error}", path.display()))
}

fn save_undo_history(history: &PersistedHistory) -> std::result::Result<(), String> {
    let directory = undo_directory()?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("undo履歴ディレクトリを作れません: {error}"))?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("undo履歴ディレクトリの権限を設定できません: {error}"))?;
    let path = undo_history_path(&history.path)?;
    let contents = serde_json::to_vec(history)
        .map_err(|error| format!("undo履歴を変換できません: {error}"))?;
    atomic_write(&path, &contents)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("undo履歴の権限を設定できません: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_contents_and_preserves_permissions() {
        let directory = std::env::temp_dir().join(format!(
            "my_editor_atomic_write_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("file.txt");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        atomic_write(&path, b"new contents").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new contents");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn file_edits_use_lsp_utf16_positions() {
        let directory = std::env::temp_dir().join(format!(
            "my_editor_lsp_edit_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("file.txt");
        fs::write(&path, "a😀b\n").unwrap();
        let edits = vec![lsp_types::TextEdit {
            range: lsp_types::Range {
                start: lsp_types::Position::new(0, 1),
                end: lsp_types::Position::new(0, 3),
            },
            new_text: "X".to_owned(),
        }];

        apply_file_edits(&path, &serde_json::to_string(&edits).unwrap()).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "aXb\n");
        fs::remove_dir_all(directory).unwrap();
    }
}
