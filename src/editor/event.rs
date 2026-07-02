use crossterm::event::MouseEvent;

use crate::config::Config;
use crate::document::{DiskState, DocumentId, LargeFile};
use crate::lsp::LspEvent;

use super::Command;

#[derive(Debug, Eq, PartialEq)]
pub enum AppEvent {
    Command(Command),
    TextInput(char),
    TextInputAt {
        character: char,
        at: std::time::Instant,
    },
    TextPaste(String),
    Mouse(MouseInput),
    Io(IoEvent),
    ConfigLoaded(Result<Config, String>),
    FileScan(FileScanEvent),
    Grep(GrepEvent),
    Lsp(LspEvent),
    Terminal(TerminalEvent),
    TerminalInput(Vec<u8>),
    Git(GitEvent),
    Resize {
        cols: u16,
        rows: u16,
    },
    Tick,
    Error(String),
}

#[derive(Debug, Eq, PartialEq)]
pub struct GitEvent {
    pub doc: DocumentId,
    pub result: Result<GitInfo, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitInfo {
    pub lines: Vec<GitLine>,
    pub branch: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitLine {
    pub line: usize,
    pub kind: GitLineKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitLineKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    Exited(Option<String>),
}

#[derive(Debug, Eq, PartialEq)]
pub enum GrepEvent {
    Hits { token: u64, hits: Vec<GrepHit> },
    Done { token: u64 },
    Failed { token: u64, error: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrepHit {
    pub path: std::path::PathBuf,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Eq, PartialEq)]
pub enum FileScanEvent {
    Batch {
        token: u64,
        paths: Vec<std::path::PathBuf>,
    },
    Done {
        token: u64,
    },
    Failed {
        token: u64,
        error: String,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub enum IoEvent {
    FileLoaded {
        id: DocumentId,
        result: Result<String, String>,
    },
    LargeFileLoaded {
        id: DocumentId,
        result: Result<LargeFile, String>,
    },
    FileSaved {
        id: DocumentId,
        result: Result<(), String>,
    },
    SaveConflict {
        id: DocumentId,
        path: std::path::PathBuf,
    },
    DirectoryReplaceFinished {
        result: Result<usize, String>,
    },
    ExternalEditsFinished {
        result: Result<std::path::PathBuf, String>,
    },
    DiskStateObserved {
        id: DocumentId,
        result: Result<DiskState, String>,
    },
    DirectPathResolved {
        path: std::path::PathBuf,
        exists: bool,
        parent_exists: bool,
        inside_root: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseInput {
    pub event: MouseEvent,
    pub clicks: u8,
}

impl From<Command> for AppEvent {
    fn from(command: Command) -> Self {
        Self::Command(command)
    }
}
