mod editable;
mod hash;
mod history;
mod large_file;

pub use editable::{ActiveDiagnostic, Editable};
pub use hash::content_hash;
pub use history::{Change, History, Revision};
pub use large_file::LargeFile;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DocumentId(pub u64);

/// A document's synchronization state with its language server.
///
/// These fields used to live in the editor as six parallel maps keyed by
/// [`DocumentId`]. Splitting the invariant across maps is exactly what let edits
/// bump one without the others and desynchronize the buffer from the server.
/// Bundling them here keeps the invariant in one place and ties its lifetime to
/// the document: dropping the document drops all of it, so no stale entry can
/// outlive the buffer it described.
#[derive(Debug)]
pub struct DocumentLsp {
    version: i32,
    opened: bool,
    needs_sync: bool,
    semantic_ready_version: Option<i32>,
    hover_ready: bool,
    hover_probe_attempts: usize,
}

impl Default for DocumentLsp {
    fn default() -> Self {
        Self {
            // Servers number the first `didOpen` as version 1, so a document
            // that has never synced already sits at 1.
            version: 1,
            opened: false,
            needs_sync: false,
            semantic_ready_version: None,
            hover_ready: false,
            hover_probe_attempts: 0,
        }
    }
}

impl DocumentLsp {
    pub fn version(&self) -> i32 {
        self.version
    }

    pub fn is_opened(&self) -> bool {
        self.opened
    }

    pub fn is_hover_ready(&self) -> bool {
        self.hover_ready
    }

    pub fn semantic_ready_version(&self) -> Option<i32> {
        self.semantic_ready_version
    }

    pub fn hover_probe_attempts(&self) -> usize {
        self.hover_probe_attempts
    }

    /// The server has accepted `didOpen` for this document. Reset the version
    /// stream and clear staleness so a freshly (re)opened buffer probes anew.
    pub fn mark_opened(&mut self) {
        *self = Self {
            opened: true,
            ..Self::default()
        };
    }

    /// Record that the buffer changed and a `didChange` is owed to the server.
    pub fn mark_dirty(&mut self) {
        self.needs_sync = true;
    }

    /// Consume the dirty flag; `true` means a `didChange` sync is owed. Clearing
    /// unconditionally mirrors draining a pending-set: a skipped flush (no server,
    /// no real change) still clears, and the version only advances when we send.
    pub fn take_needs_sync(&mut self) -> bool {
        std::mem::take(&mut self.needs_sync)
    }

    /// Advance to the next document version for an outgoing `didChange`.
    pub fn bump_version(&mut self) -> i32 {
        self.version += 1;
        self.version
    }

    pub fn set_semantic_ready(&mut self, version: i32) {
        self.semantic_ready_version = Some(version);
    }

    /// A hover probe succeeded: the server answers requests, so stop probing.
    pub fn mark_hover_ready(&mut self) {
        self.hover_ready = true;
        self.hover_probe_attempts = 0;
    }

    pub fn record_hover_probe_attempt(&mut self) {
        self.hover_probe_attempts += 1;
    }

    pub fn reset_hover_probe_attempts(&mut self) {
        self.hover_probe_attempts = 0;
    }

    /// The server for this document's language died or is restarting: forget
    /// everything that only held while it was alive so a respawn re-opens cleanly.
    pub fn reset_for_server_loss(&mut self) {
        self.opened = false;
        self.semantic_ready_version = None;
        self.hover_ready = false;
        self.hover_probe_attempts = 0;
    }

    /// An already-opened document sitting at `version`, for tests that need to
    /// stand in for a server that has handshaked without driving the full flow.
    #[cfg(test)]
    pub fn test_opened(version: i32) -> Self {
        Self {
            version,
            opened: true,
            ..Self::default()
        }
    }
}

#[derive(Debug)]
pub struct Document {
    pub path: Option<PathBuf>,
    pub language: Option<String>,
    pub disk_state: Option<DiskState>,
    pub external_changed: bool,
    pub git_branch: Option<String>,
    pub git_status: Option<String>,
    pub kind: DocumentKind,
    pub lsp: DocumentLsp,
}

impl Document {
    pub fn scratch() -> Self {
        Self {
            path: None,
            language: None,
            disk_state: None,
            external_changed: false,
            git_branch: None,
            git_status: None,
            kind: DocumentKind::Editable(Editable::default()),
            lsp: DocumentLsp::default(),
        }
    }

    pub fn editable(&self) -> &Editable {
        match &self.kind {
            DocumentKind::Editable(editable) => editable,
            DocumentKind::Large(_) => panic!("large files are not editable"),
        }
    }

    pub fn editable_mut(&mut self) -> &mut Editable {
        match &mut self.kind {
            DocumentKind::Editable(editable) => editable,
            DocumentKind::Large(_) => panic!("large files are not editable"),
        }
    }

    pub fn editable_opt(&self) -> Option<&Editable> {
        match &self.kind {
            DocumentKind::Editable(editable) => Some(editable),
            DocumentKind::Large(_) => None,
        }
    }

    pub fn editable_opt_mut(&mut self) -> Option<&mut Editable> {
        match &mut self.kind {
            DocumentKind::Editable(editable) => Some(editable),
            DocumentKind::Large(_) => None,
        }
    }

    pub fn large(&self) -> Option<&LargeFile> {
        match &self.kind {
            DocumentKind::Editable(_) => None,
            DocumentKind::Large(large) => Some(large),
        }
    }

    pub fn load_large(&mut self, large: LargeFile) {
        self.kind = DocumentKind::Large(large);
    }

    pub fn load_text(&mut self, contents: &str) {
        let line_ending = if contents.contains("\r\n") {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        };
        let normalized = contents.replace("\r\n", "\n");
        let mut editable = Editable::new(&normalized);
        editable.line_ending = line_ending;
        self.kind = DocumentKind::Editable(editable);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiskState {
    pub size: u64,
    pub modified_nanos: u128,
}

#[derive(Debug)]
pub enum DocumentKind {
    Editable(Editable),
    Large(LargeFile),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum LineEnding {
    #[default]
    Lf,
    Crlf,
}
