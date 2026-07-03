use std::path::PathBuf;

use crate::document::{DiskState, DocumentId};

use super::SearchFilters;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    ReadFile {
        id: DocumentId,
        path: PathBuf,
    },
    WriteFile {
        doc: DocumentId,
        path: PathBuf,
        contents: String,
        expected: Option<DiskState>,
    },
    LoadConfig,
    StartFileScan {
        root: PathBuf,
        token: u64,
    },
    StartGrep {
        pattern: String,
        filters: SearchFilters,
        root: PathBuf,
        token: u64,
    },
    ReplaceFiles {
        paths: Vec<PathBuf>,
        pattern: String,
        replacement: String,
    },
    ApplyFileEdits {
        path: PathBuf,
        edits_json: String,
    },
    ComputeGitStatus {
        doc: DocumentId,
        path: PathBuf,
    },
    SpawnLsp {
        server: u64,
        language: String,
        command: Vec<String>,
        root: PathBuf,
    },
    LspSend {
        server: u64,
        message: String,
    },
    LspRequest {
        server: u64,
        id: i64,
        method: String,
        params: String,
    },
    ScheduleLspRestart {
        server: u64,
        delay_ms: u64,
    },
    ScheduleSemanticRefresh {
        doc: DocumentId,
        version: i32,
        delay_ms: u64,
    },
    ScheduleCompletionRefresh {
        doc: DocumentId,
        version: i32,
        delay_ms: u64,
    },
    CheckDiskStates(Vec<(DocumentId, PathBuf)>),
    ResolveDirectPath {
        input: String,
        root: PathBuf,
    },
    SpawnShell {
        cols: u16,
        rows: u16,
        shell: Option<String>,
    },
    TerminalInput(Vec<u8>),
    TerminalResize {
        cols: u16,
        rows: u16,
    },
    ClipboardOsc52(String),
    Quit,
}
