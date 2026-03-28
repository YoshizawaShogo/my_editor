use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
};

use lsp_server::{Message, Notification, Request, RequestId};
use lsp_types::{
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    GotoDefinitionParams, Hover, HoverContents, HoverParams, Location, Position,
    ReferenceContext, ReferenceParams, RenameParams, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, WorkDoneProgressParams, WorkspaceEdit, notification, request,
};
use lsp_types::notification::Notification as LspNotificationTrait;
use lsp_types::request::Request as LspRequestTrait;
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, ChildStdout, Command},
    runtime::Builder,
    select,
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
};

use crate::{
    document::{DiagnosticEntry, DiagnosticSeverity},
    error::{AppError, Result},
};

pub enum LspClientState {
    NotAvailable,
    Inactive,
    Ready(RustLspClient),
    Failed(String),
}

pub struct HoverPopupState {
    pub active: bool,
    pub lines: Vec<String>,
}

pub struct RenameInputState {
    pub active: bool,
    pub value: String,
}

#[derive(Clone, Copy)]
pub enum GotoKind {
    Definition,
    Declaration,
    Implementation,
}

impl GotoKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Definition => "[definition]",
            Self::Declaration => "[declaration]",
            Self::Implementation => "[implementation]",
        }
    }
}

#[derive(Clone)]
pub enum LspEvent {
    PublishDiagnostics {
        path: PathBuf,
        diagnostics: HashMap<usize, Vec<DiagnosticEntry>>,
    },
    GotoResult {
        kind: GotoKind,
        locations: Vec<Location>,
    },
    ReferencesResult {
        locations: Vec<Location>,
    },
    HoverResult {
        lines: Vec<String>,
    },
    RenameResult {
        edit: Option<WorkspaceEdit>,
    },
    Failed(String),
}

enum LspCommand {
    EnsureOpen {
        path: PathBuf,
        version: i32,
        text: String,
    },
    DidSave {
        path: PathBuf,
        text: String,
    },
    DidClose {
        path: PathBuf,
    },
    Goto {
        kind: GotoKind,
        path: PathBuf,
        position: Position,
    },
    References {
        path: PathBuf,
        position: Position,
    },
    Hover {
        path: PathBuf,
        position: Position,
    },
    Rename {
        path: PathBuf,
        position: Position,
        new_name: String,
    },
}

enum PendingRequest {
    Goto(GotoKind),
    References,
    Hover,
    Rename,
}

pub struct RustLspClient {
    tx: UnboundedSender<LspCommand>,
    rx: Receiver<LspEvent>,
    pending_events: Vec<LspEvent>,
    opened_documents: HashSet<PathBuf>,
}

impl RustLspClient {
    pub fn start(root_path: &Path) -> Result<Self> {
        let (tx, rx_commands) = unbounded_channel();
        let (tx_events, rx) = mpsc::channel();
        let root_path = root_path.to_path_buf();

        thread::spawn(move || {
            let runtime = Builder::new_current_thread().enable_all().build();
            let runtime = match runtime {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = tx_events.send(LspEvent::Failed(format!("failed to build tokio runtime: {error}")));
                    return;
                }
            };

            if let Err(error) = runtime.block_on(run_lsp_worker(root_path, rx_commands, tx_events.clone())) {
                let _ = tx_events.send(LspEvent::Failed(format!("{error:?}")));
            }
        });

        Ok(Self {
            tx,
            rx,
            pending_events: Vec::new(),
            opened_documents: HashSet::new(),
        })
    }

    pub fn poll(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            self.pending_events.push(event);
        }
    }

    pub fn take_events(&mut self) -> Vec<LspEvent> {
        std::mem::take(&mut self.pending_events)
    }

    pub fn ensure_open(&mut self, path: &Path, version: i32, text: &str) -> Result<()> {
        if self.opened_documents.contains(path) {
            return Ok(());
        }

        self.send(LspCommand::EnsureOpen {
            path: path.to_path_buf(),
            version,
            text: text.to_owned(),
        })?;
        self.opened_documents.insert(path.to_path_buf());
        Ok(())
    }

    pub fn did_save(&mut self, path: &Path, text: &str) -> Result<()> {
        self.send(LspCommand::DidSave {
            path: path.to_path_buf(),
            text: text.to_owned(),
        })
    }

    pub fn did_close(&mut self, path: &Path) -> Result<()> {
        if !self.opened_documents.remove(path) {
            return Ok(());
        }

        self.send(LspCommand::DidClose {
            path: path.to_path_buf(),
        })
    }

    pub fn goto(&mut self, kind: GotoKind, path: &Path, position: Position) -> Result<()> {
        self.send(LspCommand::Goto {
            kind,
            path: path.to_path_buf(),
            position,
        })
    }

    pub fn references(&mut self, path: &Path, position: Position) -> Result<()> {
        self.send(LspCommand::References {
            path: path.to_path_buf(),
            position,
        })
    }

    pub fn hover(&mut self, path: &Path, position: Position) -> Result<()> {
        self.send(LspCommand::Hover {
            path: path.to_path_buf(),
            position,
        })
    }

    pub fn rename(&mut self, path: &Path, position: Position, new_name: String) -> Result<()> {
        self.send(LspCommand::Rename {
            path: path.to_path_buf(),
            position,
            new_name,
        })
    }

    fn send(&self, command: LspCommand) -> Result<()> {
        self.tx
            .send(command)
            .map_err(|_| AppError::CommandFailed("LSP worker is not running".to_owned()))
    }
}

pub fn hover_lines(hover: &Hover) -> Vec<String> {
    match &hover.contents {
        HoverContents::Scalar(marked) => marked_string_lines(marked),
        HoverContents::Array(items) => items.iter().flat_map(marked_string_lines).collect(),
        HoverContents::Markup(markup) => markup.value.lines().map(ToOwned::to_owned).collect(),
    }
}

async fn run_lsp_worker(
    root_path: PathBuf,
    mut rx_commands: UnboundedReceiver<LspCommand>,
    tx_events: Sender<LspEvent>,
) -> Result<()> {
    let mut child = Command::new("rust-analyzer")
        .current_dir(&root_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::CommandFailed("failed to open rust-analyzer stdin".to_owned()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::CommandFailed("failed to open rust-analyzer stdout".to_owned()))?;

    let mut writer = stdin;
    let mut reader = BufReader::new(stdout);
    let mut next_request_id = 1;
    let mut pending_requests = HashMap::<RequestId, PendingRequest>::new();

    initialize_server(&mut writer, &mut reader, &root_path, &mut next_request_id).await?;

    loop {
        select! {
            maybe_command = rx_commands.recv() => {
                let Some(command) = maybe_command else {
                    break;
                };
                handle_command(
                    command,
                    &mut writer,
                    &mut next_request_id,
                    &mut pending_requests,
                ).await?;
            }
            message = read_message(&mut reader) => {
                let message = message?;
                process_message(message, &mut pending_requests, &tx_events)?;
            }
        }
    }

    let _ = child.start_kill();
    let _ = child.wait().await;
    Ok(())
}

async fn initialize_server(
    writer: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    root_path: &Path,
    next_request_id: &mut i32,
) -> Result<()> {
    let root_uri = path_to_uri(root_path)?;
    let workspace_folders = Some(vec![lsp_types::WorkspaceFolder {
        uri: root_uri,
        name: root_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(".")
            .to_owned(),
    }]);
    let initialize_params = lsp_types::InitializeParams {
        process_id: Some(std::process::id()),
        workspace_folders,
        capabilities: lsp_types::ClientCapabilities::default(),
        ..Default::default()
    };

    let request_id = next_request_id_value(next_request_id);
    send_request(
        writer,
        request_id.clone(),
        request::Initialize::METHOD,
        serde_json::to_value(initialize_params)
            .map_err(|error| AppError::CommandFailed(error.to_string()))?,
    )
    .await?;

    loop {
        match read_message(reader).await? {
            Message::Response(response) if response.id == request_id => break,
            Message::Notification(_) | Message::Request(_) | Message::Response(_) => {}
        }
    }

    send_notification(
        writer,
        notification::Initialized::METHOD,
        serde_json::json!({}),
    )
    .await?;

    Ok(())
}

async fn handle_command(
    command: LspCommand,
    writer: &mut ChildStdin,
    next_request_id: &mut i32,
    pending_requests: &mut HashMap<RequestId, PendingRequest>,
) -> Result<()> {
    match command {
        LspCommand::EnsureOpen {
            path,
            version,
            text,
        } => {
            let params = DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: path_to_uri(&path)?,
                    language_id: "rust".to_owned(),
                    version,
                    text,
                },
            };
            send_notification(
                writer,
                notification::DidOpenTextDocument::METHOD,
                serde_json::to_value(params)
                    .map_err(|error| AppError::CommandFailed(error.to_string()))?,
            )
            .await?;
        }
        LspCommand::DidSave { path, text } => {
            let params = DidSaveTextDocumentParams {
                text_document: TextDocumentIdentifier {
                    uri: path_to_uri(&path)?,
                },
                text: Some(text),
            };
            send_notification(
                writer,
                notification::DidSaveTextDocument::METHOD,
                serde_json::to_value(params)
                    .map_err(|error| AppError::CommandFailed(error.to_string()))?,
            )
            .await?;
        }
        LspCommand::DidClose { path } => {
            let params = DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier {
                    uri: path_to_uri(&path)?,
                },
            };
            send_notification(
                writer,
                notification::DidCloseTextDocument::METHOD,
                serde_json::to_value(params)
                    .map_err(|error| AppError::CommandFailed(error.to_string()))?,
            )
            .await?;
        }
        LspCommand::Goto {
            kind,
            path,
            position,
        } => {
            let method = match kind {
                GotoKind::Definition => request::GotoDefinition::METHOD,
                GotoKind::Declaration => request::GotoDeclaration::METHOD,
                GotoKind::Implementation => request::GotoImplementation::METHOD,
            };
            let params = GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: path_to_uri(&path)?,
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: Default::default(),
            };
            let id = next_request_id_value(next_request_id);
            pending_requests.insert(id.clone(), PendingRequest::Goto(kind));
            send_request(
                writer,
                id,
                method,
                serde_json::to_value(params)
                    .map_err(|error| AppError::CommandFailed(error.to_string()))?,
            )
            .await?;
        }
        LspCommand::References { path, position } => {
            let params = ReferenceParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: path_to_uri(&path)?,
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: Default::default(),
                context: ReferenceContext {
                    include_declaration: true,
                },
            };
            let id = next_request_id_value(next_request_id);
            pending_requests.insert(id.clone(), PendingRequest::References);
            send_request(
                writer,
                id,
                request::References::METHOD,
                serde_json::to_value(params)
                    .map_err(|error| AppError::CommandFailed(error.to_string()))?,
            )
            .await?;
        }
        LspCommand::Hover { path, position } => {
            let params = HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: path_to_uri(&path)?,
                    },
                    position,
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            };
            let id = next_request_id_value(next_request_id);
            pending_requests.insert(id.clone(), PendingRequest::Hover);
            send_request(
                writer,
                id,
                request::HoverRequest::METHOD,
                serde_json::to_value(params)
                    .map_err(|error| AppError::CommandFailed(error.to_string()))?,
            )
            .await?;
        }
        LspCommand::Rename {
            path,
            position,
            new_name,
        } => {
            let params = RenameParams {
                text_document_position: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: path_to_uri(&path)?,
                    },
                    position,
                },
                new_name,
                work_done_progress_params: WorkDoneProgressParams::default(),
            };
            let id = next_request_id_value(next_request_id);
            pending_requests.insert(id.clone(), PendingRequest::Rename);
            send_request(
                writer,
                id,
                request::Rename::METHOD,
                serde_json::to_value(params)
                    .map_err(|error| AppError::CommandFailed(error.to_string()))?,
            )
            .await?;
        }
    }

    Ok(())
}

fn process_message(
    message: Message,
    pending_requests: &mut HashMap<RequestId, PendingRequest>,
    tx_events: &Sender<LspEvent>,
) -> Result<()> {
    match message {
        Message::Notification(notification) => {
            if notification.method == notification::PublishDiagnostics::METHOD {
                let params = serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(notification.params)
                    .map_err(|error| AppError::CommandFailed(error.to_string()))?;
                if let Some(path) = uri_to_path(&params.uri) {
                    let mut diagnostics = HashMap::<usize, Vec<DiagnosticEntry>>::new();
                    for diagnostic in params.diagnostics {
                        let severity = match diagnostic.severity {
                            Some(lsp_types::DiagnosticSeverity::ERROR) => DiagnosticSeverity::Error,
                            Some(lsp_types::DiagnosticSeverity::WARNING) => DiagnosticSeverity::Warning,
                            _ => DiagnosticSeverity::Warning,
                        };
                        diagnostics
                            .entry(diagnostic.range.start.line as usize + 1)
                            .or_default()
                            .push(DiagnosticEntry {
                                severity,
                                message: diagnostic.message,
                            });
                    }
                    send_event(
                        tx_events,
                        LspEvent::PublishDiagnostics { path, diagnostics },
                    );
                }
            }
        }
        Message::Response(response) => {
            let Some(pending) = pending_requests.remove(&response.id) else {
                return Ok(());
            };

            if let Some(error) = response.error {
                send_event(
                    tx_events,
                    LspEvent::Failed(format!("LSP error {}: {}", error.code, error.message)),
                );
                return Ok(());
            }

            match pending {
                PendingRequest::Goto(kind) => {
                    let locations = parse_locations_response(response.result)?;
                    send_event(tx_events, LspEvent::GotoResult { kind, locations });
                }
                PendingRequest::References => {
                    let locations = serde_json::from_value(response.result.unwrap_or(Value::Null))
                        .map_err(|error| AppError::CommandFailed(error.to_string()))?;
                    send_event(tx_events, LspEvent::ReferencesResult { locations });
                }
                PendingRequest::Hover => {
                    let hover: Option<Hover> =
                        serde_json::from_value(response.result.unwrap_or(Value::Null))
                            .map_err(|error| AppError::CommandFailed(error.to_string()))?;
                    let lines = hover.map_or_else(Vec::new, |hover| hover_lines(&hover));
                    send_event(tx_events, LspEvent::HoverResult { lines });
                }
                PendingRequest::Rename => {
                    let edit = serde_json::from_value(response.result.unwrap_or(Value::Null))
                        .map_err(|error| AppError::CommandFailed(error.to_string()))?;
                    send_event(tx_events, LspEvent::RenameResult { edit });
                }
            }
        }
        Message::Request(_) => {}
    }

    Ok(())
}

fn send_event(tx_events: &Sender<LspEvent>, event: LspEvent) {
    let _ = tx_events.send(event);
}

async fn send_request(
    writer: &mut ChildStdin,
    id: RequestId,
    method: &str,
    params: Value,
) -> Result<()> {
    send_message(
        writer,
        Message::Request(Request {
            id,
            method: method.to_owned(),
            params,
        }),
    )
    .await
}

async fn send_notification(writer: &mut ChildStdin, method: &str, params: Value) -> Result<()> {
    send_message(
        writer,
        Message::Notification(Notification {
            method: method.to_owned(),
            params,
        }),
    )
    .await
}

async fn send_message(writer: &mut ChildStdin, message: Message) -> Result<()> {
    let body =
        serde_json::to_vec(&message).map_err(|error| AppError::CommandFailed(error.to_string()))?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
        .await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Message>
where
    R: AsyncRead + Unpin,
{
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).await?;
        if bytes == 0 {
            return Err(AppError::CommandFailed("unexpected EOF".to_owned()));
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length: ") {
            content_length = value.parse::<usize>().ok();
        }
    }

    let content_length =
        content_length.ok_or_else(|| AppError::CommandFailed("missing content length".to_owned()))?;
    let mut body = vec![0; content_length];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(|error| AppError::CommandFailed(error.to_string()))
}

fn next_request_id_value(next_request_id: &mut i32) -> RequestId {
    let id = *next_request_id;
    *next_request_id += 1;
    id.into()
}

fn marked_string_lines(marked: &lsp_types::MarkedString) -> Vec<String> {
    match marked {
        lsp_types::MarkedString::String(value) => value.lines().map(ToOwned::to_owned).collect(),
        lsp_types::MarkedString::LanguageString(code) => {
            code.value.lines().map(ToOwned::to_owned).collect()
        }
    }
}

fn parse_locations_response(result: Option<Value>) -> Result<Vec<Location>> {
    let Some(value) = result else {
        return Ok(Vec::new());
    };

    if value.is_null() {
        return Ok(Vec::new());
    }

    if let Ok(location) = serde_json::from_value::<Location>(value.clone()) {
        return Ok(vec![location]);
    }

    if let Ok(locations) = serde_json::from_value::<Vec<Location>>(value.clone()) {
        return Ok(locations);
    }

    if let Ok(location_links) = serde_json::from_value::<Vec<lsp_types::LocationLink>>(value) {
        return Ok(location_links
            .into_iter()
            .map(|link| Location {
                uri: link.target_uri,
                range: link.target_selection_range,
            })
            .collect());
    }

    Err(AppError::CommandFailed(
        "failed to parse LSP locations".to_owned(),
    ))
}

pub fn path_to_uri(path: &Path) -> Result<Uri> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let path_str = absolute.to_string_lossy().replace('\\', "/");
    let uri = if path_str.starts_with('/') {
        format!("file://{path_str}")
    } else {
        format!("file:///{path_str}")
    };
    uri.parse()
        .map_err(|error| AppError::CommandFailed(format!("failed to convert path to uri: {error}")))
}

pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let value = uri.as_str();
    let stripped = value.strip_prefix("file://")?;
    Some(PathBuf::from(stripped))
}
