use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Diagnostic {
    pub line: u32,
    pub character: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticSpan {
    pub start: crate::position::CharIdx,
    pub end: crate::position::CharIdx,
    pub token_type: u32,
}

#[derive(Debug, Eq, PartialEq)]
pub enum LspEvent {
    Spawned {
        server: u64,
        language: String,
    },
    Initialized {
        server: u64,
        incremental_sync: bool,
    },
    Diagnostics {
        uri: String,
        diagnostics: Vec<Diagnostic>,
    },
    Progress {
        server: u64,
        message: Option<String>,
    },
    Response {
        id: i64,
        result: Result<serde_json::Value, String>,
    },
    Exited {
        server: u64,
        error: Option<String>,
    },
    RestartDue {
        server: u64,
    },
    SemanticRefreshDue {
        doc: crate::document::DocumentId,
        version: i32,
    },
    CompletionRefreshDue {
        doc: crate::document::DocumentId,
        version: i32,
    },
}
