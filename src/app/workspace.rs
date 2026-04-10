/*
use std::path::{Path, PathBuf};

use crate::{
    document::Document,
    error::Result,
    open_candidate::{OpenBufferCandidate, OpenCandidate},
};

use super::{BufferViewState, DocumentEntry, Workspace};

impl Workspace {
    /// ドキュメントが1つ以上存在するかを返す
    pub fn has_documents(&self) -> bool {
        !self.documents.is_empty()
    }

    /// カレントドキュメントへの不変参照を返す
    pub fn current_document(&self) -> &Document {
        &self.documents[self.current_index].document
    }

    /// カレントドキュメントへの可変参照を返す
    pub fn current_document_mut(&mut self) -> &mut Document {
        &mut self.documents[self.current_index].document
    }

    /// カレントドキュメントへの不変参照をOptionで返す
    pub fn try_current_document(&self) -> Option<&Document> {
        self.documents.get(self.current_index).map(|entry| &entry.document)
    }

    /// カレントドキュメントへの可変参照をOptionで返す
    pub fn try_current_document_mut(&mut self) -> Option<&mut Document> {
        self.documents
            .get_mut(self.current_index)
            .map(|entry| &mut entry.document)
    }

    /// 開いているバッファをOpenCandidate::OpenBufferのリストとして返す
    pub fn open_buffer_candidates(&self) -> Vec<OpenCandidate> {
        self.documents
            .iter()
            .filter(|entry| !entry.document.is_scratch())
            .map(|entry| {
                OpenCandidate::OpenBuffer(OpenBufferCandidate::new(
                    entry.path.clone(),
                    super::display_name(&entry.path),
                ))
            })
            .collect()
    }

    /// カレントドキュメントのファイル名を返す
    pub fn current_document_name(&self) -> Option<String> {
        self.documents
            .get(self.current_index)
            .map(|entry| super::display_name(&entry.path))
    }

    /// カレントドキュメントのパスを返す
    pub fn current_document_path(&self) -> Option<&Path> {
        self.documents
            .get(self.current_index)
            .map(|entry| entry.path.as_path())
    }

    /// パスに一致するドキュメントのインデックスを返す
    pub fn find_document_index(&self, path: &Path) -> Option<usize> {
        let normalized = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().ok()?.join(path)
        };
        self.documents
            .iter()
            .position(|entry| entry.path == normalized)
    }

    /// 指定インデックスのドキュメントを先頭に移動してカレントにする
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

    /// 指定インデックスをcurrent_indexとして選択する
    pub fn select_current(&mut self, index: usize) {
        if index >= self.documents.len() {
            return;
        }
        self.current_index = index;
    }

    /// パスのドキュメントを開いてリストの先頭に挿入しカレントにする
    pub fn open_document(&mut self, path: PathBuf) -> Result<()> {
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()?.join(path)
        };
        let document = Document::open(&path)?;
        self.documents.insert(
            0,
            DocumentEntry {
                path,
                document,
                view_state: BufferViewState::default(),
                version: 1,
                lsp_open: false,
            },
        );
        self.current_index = 0;
        Ok(())
    }

    /// カレントドキュメントを閉じてインデックスを調整する
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

    /// カレント以外のドキュメントのインデックスを返す
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
*/
