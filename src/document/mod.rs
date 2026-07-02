mod editable;
mod history;
mod large_file;
mod persist;

pub use editable::Editable;
pub use history::{Change, History, Revision};
pub use large_file::LargeFile;
pub use persist::{PersistedHistory, content_hash, history_key};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DocumentId(pub u64);

#[derive(Debug)]
pub struct Document {
    pub path: Option<PathBuf>,
    pub language: Option<String>,
    pub disk_state: Option<DiskState>,
    pub external_changed: bool,
    pub kind: DocumentKind,
}

impl Document {
    pub fn scratch() -> Self {
        Self {
            path: None,
            language: None,
            disk_state: None,
            external_changed: false,
            kind: DocumentKind::Editable(Editable::default()),
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
