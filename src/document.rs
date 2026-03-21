use std::{fs, path::Path};

use crate::{config, error::Result};

pub mod editable;
pub mod large_file;

pub enum Document {
    Editable(editable::EditableDocument),
    LargeFile(large_file::LargeFileDocument),
}

const DIAGNOSTIC_WIDTH: usize = 1;
const LINE_NUMBER_WIDTH: usize = 6;
const GUTTER_WIDTH: usize = 1;

pub struct DocumentRender {
    pub lines: Vec<DocumentRenderLine>,
    pub status: String,
}

pub struct DocumentRenderLine {
    pub diagnostic_marker: String,
    pub line_number: usize,
    pub gutter_marker: String,
    pub text: String,
}

impl Document {
    pub fn open(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        let file_size_bytes = metadata.len();

        if file_size_bytes > config::large_file_threshold_bytes() {
            return Ok(Self::LargeFile(large_file::LargeFileDocument::new(
                path.to_path_buf(),
                file_size_bytes,
            )));
        }

        Ok(Self::Editable(editable::EditableDocument::open(path)?))
    }

    pub fn render_first_page(
        &self,
        viewport_row: usize,
        page_height: usize,
        page_width: usize,
    ) -> Result<DocumentRender> {
        let content_height = page_height.saturating_sub(1);
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                let page = document.read_page(viewport_row, content_height, content_width)?;
                let lines = page
                    .rows
                    .into_iter()
                    .map(|row| {
                        build_render_line(row.line_number, row.text)
                    })
                    .collect();
                let status = build_status(page_width, "EDITOR");
                Ok(DocumentRender { lines, status })
            }
            Self::LargeFile(document) => {
                let page = document.read_page(viewport_row, content_height, content_width)?;
                let lines = page
                    .rows
                    .into_iter()
                    .map(|row| {
                        build_render_line(row.line_number, row.text)
                    })
                    .collect();
                let status = if page.next_byte_offset >= document.file_size_bytes {
                    "VIEWER END"
                } else {
                    "VIEWER"
                };
                let status = build_status(page_width, status);
                Ok(DocumentRender { lines, status })
            }
        }
    }

    pub fn total_rows(&self, page_width: usize) -> Option<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.total_rows(content_width)),
            Self::LargeFile(_) => None,
        }
    }

    pub fn jump_to_top(&mut self) {
        if let Self::LargeFile(document) = self {
            document.jump_to_top();
        }
    }

    pub fn jump_to_bottom(&mut self, page_height: usize, page_width: usize) -> Result<Option<usize>> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(_) => Ok(None),
            Self::LargeFile(document) => Ok(Some(document.jump_to_bottom(page_height, content_width)?)),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        match self {
            Self::Editable(document) => document.save(path),
            Self::LargeFile(_) => Ok(()),
        }
    }

    pub fn insert_char(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
        ch: char,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.insert_char(
                display_row,
                display_column,
                content_width.max(1),
                ch,
            )),
            Self::LargeFile(_) => None,
        }
    }

    pub fn insert_newline(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.insert_newline(
                display_row,
                display_column,
                content_width.max(1),
            )),
            Self::LargeFile(_) => None,
        }
    }

    pub fn backspace(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.backspace(
                display_row,
                display_column,
                content_width.max(1),
            ),
            Self::LargeFile(_) => None,
        }
    }
}

fn build_status(page_width: usize, label: &str) -> String {
    let width = page_width.max(label.len());
    format!("{label}{}", "-".repeat(width.saturating_sub(label.len())))
}

fn build_render_line(line_number: usize, text: String) -> DocumentRenderLine {
    DocumentRenderLine {
        diagnostic_marker: " ".to_owned(),
        line_number,
        gutter_marker: " ".to_owned(),
        text,
    }
}
