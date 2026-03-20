use std::{fs, path::Path};

use crate::{config, error::Result};

pub mod editable;
pub mod large_file;

pub enum Document {
    Editable(editable::EditableDocument),
    LargeFile(large_file::LargeFileDocument),
}

const LINE_NUMBER_WIDTH: usize = 6;

pub struct DocumentRender {
    pub lines: Vec<String>,
    pub status: String,
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

    pub fn render_first_page(&self, page_height: usize, page_width: usize) -> Result<DocumentRender> {
        let content_height = page_height.saturating_sub(1);
        let content_width = page_width.saturating_sub(LINE_NUMBER_WIDTH + 1);

        match self {
            Self::Editable(document) => {
                let page = document.read_page(content_height, content_width)?;
                let lines = page
                    .rows
                    .into_iter()
                    .map(|row| {
                        format!(
                            "{:>width$} {}",
                            row.line_number,
                            row.text,
                            width = LINE_NUMBER_WIDTH
                        )
                    })
                    .collect();
                let status = build_status(page_width, "EDITOR");
                Ok(DocumentRender { lines, status })
            }
            Self::LargeFile(document) => {
                let page = document.read_page(content_height, content_width)?;
                let lines = page
                    .rows
                    .into_iter()
                    .map(|row| {
                        format!(
                            "{:>width$} {}",
                            row.line_number,
                            row.text,
                            width = LINE_NUMBER_WIDTH
                        )
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
}

fn build_status(page_width: usize, label: &str) -> String {
    let width = page_width.max(label.len());
    format!("{label}{}", "-".repeat(width.saturating_sub(label.len())))
}
