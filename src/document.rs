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
                        build_render_line(
                            row.line_number,
                            document.git_gutter_marker(row.line_number),
                            row.text,
                        )
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
                        build_render_line(row.line_number, None, row.text)
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

    pub fn indent_width(&self) -> usize {
        match self {
            Self::Editable(document) => document.indent_width(),
            Self::LargeFile(_) => 4,
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

    pub fn save(&mut self, path: &Path) -> Result<()> {
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

    pub fn insert_tab(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.insert_tab(
                display_row,
                display_column,
                content_width.max(1),
            )),
            Self::LargeFile(_) => None,
        }
    }

    pub fn delete_forward(
        &mut self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.delete_forward(
                display_row,
                display_column,
                content_width.max(1),
            ),
            Self::LargeFile(_) => None,
        }
    }

    pub fn next_git_marker_row(&self, current_row: usize, page_width: usize) -> Option<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.next_git_marker_row(current_row, content_width.max(1)),
            Self::LargeFile(_) => None,
        }
    }

    pub fn previous_git_marker_row(&self, current_row: usize, page_width: usize) -> Option<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                document.previous_git_marker_row(current_row, content_width.max(1))
            }
            Self::LargeFile(_) => None,
        }
    }

    pub fn display_line_width(&self, cursor_row: usize, page_width: usize) -> Result<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Ok(document.display_line_width(cursor_row, content_width.max(1))),
            Self::LargeFile(document) => {
                let page = document.read_page(cursor_row, 1, content_width.max(1))?;
                Ok(page
                    .rows
                    .first()
                    .map(|row| row.text.chars().count())
                    .unwrap_or(0))
            }
        }
    }

    pub fn display_line_text(&self, cursor_row: usize, page_width: usize) -> Result<String> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                Ok(document.display_line_text(cursor_row, content_width.max(1)))
            }
            Self::LargeFile(document) => {
                let page = document.read_page(cursor_row, 1, content_width.max(1))?;
                Ok(page
                    .rows
                    .first()
                    .map(|row| row.text.clone())
                    .unwrap_or_default())
            }
        }
    }

    pub fn jump_row_for_line_number(&self, line_number: usize, page_width: usize) -> Option<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                document.jump_row_for_line_number(line_number, content_width.max(1))
            }
            Self::LargeFile(_) => None,
        }
    }

    pub fn remove_display_range(
        &mut self,
        display_row: usize,
        start_column: usize,
        end_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.remove_display_range(
                display_row,
                start_column,
                end_column,
                content_width.max(1),
            ),
            Self::LargeFile(_) => None,
        }
    }

    pub fn open_below(&mut self, display_row: usize, page_width: usize) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.open_below(display_row, content_width.max(1))),
            Self::LargeFile(_) => None,
        }
    }

    pub fn current_line_text(&self, display_row: usize, page_width: usize) -> Option<String> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                Some(document.current_line_text(display_row, content_width.max(1)))
            }
            Self::LargeFile(_) => None,
        }
    }

    pub fn clear_current_line(
        &mut self,
        display_row: usize,
        page_width: usize,
    ) -> Option<(String, (usize, usize))> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                document.clear_current_line(display_row, content_width.max(1))
            }
            Self::LargeFile(_) => None,
        }
    }

    pub fn delete_current_line(
        &mut self,
        display_row: usize,
        page_width: usize,
    ) -> Option<(String, (usize, usize))> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                document.delete_current_line(display_row, content_width.max(1))
            }
            Self::LargeFile(_) => None,
        }
    }
}

fn build_status(page_width: usize, label: &str) -> String {
    let width = page_width.max(label.len());
    format!("{label}{}", "-".repeat(width.saturating_sub(label.len())))
}

fn build_render_line(line_number: usize, gutter_marker: Option<char>, text: String) -> DocumentRenderLine {
    DocumentRenderLine {
        diagnostic_marker: " ".to_owned(),
        line_number,
        gutter_marker: gutter_marker.unwrap_or(' ').to_string(),
        text,
    }
}
