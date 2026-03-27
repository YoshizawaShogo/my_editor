use std::path::{Path, PathBuf};

use crate::{
    document::Document,
    error::Result,
    open_candidate::{OpenBufferCandidate, OpenCandidate},
};

use super::{BufferViewState, DocumentEntry, Workspace};

impl Workspace {
    pub fn has_documents(&self) -> bool {
        !self.documents.is_empty()
    }

    pub fn current_document(&self) -> &Document {
        &self.documents[self.current_index].document
    }

    pub fn current_document_mut(&mut self) -> &mut Document {
        &mut self.documents[self.current_index].document
    }

    pub fn try_current_document(&self) -> Option<&Document> {
        self.documents.get(self.current_index).map(|entry| &entry.document)
    }

    pub fn try_current_document_mut(&mut self) -> Option<&mut Document> {
        self.documents
            .get_mut(self.current_index)
            .map(|entry| &mut entry.document)
    }

    pub fn open_buffer_candidates(&self) -> Vec<OpenCandidate> {
        self.documents
            .iter()
            .map(|entry| {
                OpenCandidate::OpenBuffer(OpenBufferCandidate::new(
                    entry.path.clone(),
                    super::display_name(&entry.path),
                ))
            })
            .collect()
    }

    pub fn current_document_name(&self) -> Option<String> {
        self.documents
            .get(self.current_index)
            .map(|entry| super::display_name(&entry.path))
    }

    pub fn current_document_path(&self) -> Option<&Path> {
        self.documents
            .get(self.current_index)
            .map(|entry| entry.path.as_path())
    }

    pub fn find_document_index(&self, path: &Path) -> Option<usize> {
        self.documents.iter().position(|entry| entry.path == path)
    }

    pub fn make_current(&mut self, index: usize) {
        if index >= self.documents.len() {
            return;
        }
        if index != 0 {
            let entry = self.documents.remove(index);
            self.documents.insert(0, entry);
        }
        self.current_index = 0;
    }

    pub fn select_current(&mut self, index: usize) {
        if index >= self.documents.len() {
            return;
        }
        self.current_index = index;
    }

    pub fn open_document(&mut self, path: PathBuf) -> Result<()> {
        let document = Document::open(&path)?;
        self.documents.insert(
            0,
            DocumentEntry {
                path,
                document,
                view_state: BufferViewState::default(),
            },
        );
        self.current_index = 0;
        Ok(())
    }

    pub fn close_current(&mut self) {
        if self.documents.is_empty() {
            return;
        }
        self.documents.remove(self.current_index);
        if self.documents.is_empty() {
            self.current_index = 0;
        } else {
            self.current_index = self.current_index.min(self.documents.len().saturating_sub(1));
        }
    }

    pub fn secondary_index(&self) -> Option<usize> {
        if self.documents.len() < 2 {
            None
        } else if self.current_index == 0 {
            Some(1)
        } else {
            Some(0)
        }
    }
}
