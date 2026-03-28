use std::{fs, path::Path};

use lsp_types::{Position, TextEdit};

use crate::{config, error::Result};

pub mod editable;
pub mod large_file;
pub mod scratch;

pub use editable::{DiagnosticSeverity, DiagnosticSummary, SyntaxHighlightKind, SyntaxTokenSpan};
pub use scratch::{DiagnosticEntry, ScratchDocument, ScratchRow, ScratchTarget};

pub enum Document {
    Editable(editable::EditableDocument),
    LargeFile(large_file::LargeFileDocument),
    Scratch(scratch::ScratchDocument),
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
    pub syntax_spans: Vec<SyntaxTokenSpan>,
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
                            document.diagnostic_marker(row.line_number),
                            row.line_number,
                            document.git_gutter_marker(row.line_number),
                            row.text,
                            row.syntax_spans,
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
                        build_render_line(None, row.line_number, None, row.text, Vec::new())
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
            Self::Scratch(document) => Ok(document.render_first_page(viewport_row, page_height)),
        }
    }

    pub fn total_rows(&self, page_width: usize) -> Option<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.total_rows(content_width)),
            Self::LargeFile(_) => None,
            Self::Scratch(document) => Some(document.total_rows()),
        }
    }

    pub fn indent_width(&self) -> usize {
        match self {
            Self::Editable(document) => document.indent_width(),
            Self::LargeFile(_) => 4,
            Self::Scratch(_) => 4,
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
            Self::Scratch(document) => Ok(Some(document.total_rows().saturating_sub(page_height.max(1)))),
        }
    }

    pub fn save(&mut self, path: &Path) -> Result<()> {
        match self {
            Self::Editable(document) => document.save(path),
            Self::LargeFile(_) => Ok(()),
            Self::Scratch(_) => Ok(()),
        }
    }

    pub fn undo(&mut self) -> bool {
        match self {
            Self::Editable(document) => document.undo(),
            Self::LargeFile(_) => false,
            Self::Scratch(_) => false,
        }
    }

    pub fn redo(&mut self) -> bool {
        match self {
            Self::Editable(document) => document.redo(),
            Self::LargeFile(_) => false,
            Self::Scratch(_) => false,
        }
    }

    pub fn begin_undo_group(&mut self) {
        if let Self::Editable(document) = self {
            document.begin_undo_group();
        }
    }

    pub fn end_undo_group(&mut self) {
        if let Self::Editable(document) = self {
            document.end_undo_group();
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
            Self::Scratch(_) => None,
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
            Self::Scratch(_) => None,
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
            Self::Scratch(_) => None,
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
            Self::Scratch(_) => None,
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
            Self::Scratch(_) => None,
        }
    }

    pub fn next_git_marker_row(&self, current_row: usize, page_width: usize) -> Option<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.next_git_marker_row(current_row, content_width.max(1)),
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
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
            Self::Scratch(_) => None,
        }
    }

    pub fn next_diagnostic_row(
        &self,
        current_row: usize,
        page_width: usize,
        error_only: bool,
    ) -> Option<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                document.next_diagnostic_row(current_row, content_width.max(1), error_only)
            }
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
        }
    }

    pub fn previous_diagnostic_row(
        &self,
        current_row: usize,
        page_width: usize,
        error_only: bool,
    ) -> Option<usize> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                document.previous_diagnostic_row(current_row, content_width.max(1), error_only)
            }
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
        }
    }

    pub fn set_rust_diagnostics(
        &mut self,
        diagnostics: std::collections::HashMap<usize, Vec<DiagnosticEntry>>,
    ) {
        if let Self::Editable(document) = self {
            document.set_diagnostics(diagnostics);
        }
    }

    pub fn set_semantic_tokens(
        &mut self,
        semantic_tokens: std::collections::HashMap<usize, Vec<SyntaxTokenSpan>>,
    ) {
        if let Self::Editable(document) = self {
            document.set_semantic_tokens(semantic_tokens);
        }
    }

    pub fn diagnostic_summary(&self) -> DiagnosticSummary {
        match self {
            Self::Editable(document) => document.diagnostic_summary(),
            Self::LargeFile(_) => DiagnosticSummary::default(),
            Self::Scratch(_) => DiagnosticSummary::default(),
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
            Self::Scratch(document) => Ok(document.display_line_width(cursor_row)),
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
            Self::Scratch(document) => Ok(document.display_line_text(cursor_row)),
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
            Self::Scratch(document) => document.jump_row_for_line_number(line_number),
        }
    }

    pub fn first_match_row(&self, query: &str, page_width: usize) -> Option<usize> {
        self.first_match_position(query, page_width).map(|(row, _)| row)
    }

    pub fn first_match_position(&self, query: &str, page_width: usize) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.first_match_position(query, content_width.max(1)),
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
        }
    }

    pub fn next_match_position(
        &self,
        query: &str,
        start_row: usize,
        start_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.next_match_position(
                query,
                start_row,
                start_column,
                content_width.max(1),
            ),
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
        }
    }

    pub fn previous_match_position(
        &self,
        query: &str,
        start_row: usize,
        start_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.previous_match_position(
                query,
                start_row,
                start_column,
                content_width.max(1),
            ),
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
        }
    }

    pub fn remove_display_range(
        &mut self,
        start_row: usize,
        start_column: usize,
        end_row: usize,
        end_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.remove_display_range(
                start_row,
                start_column,
                end_row,
                end_column,
                content_width.max(1),
            ),
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
        }
    }

    pub fn open_below(&mut self, display_row: usize, page_width: usize) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.open_below(display_row, content_width.max(1))),
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
        }
    }

    pub fn open_above(&mut self, display_row: usize, page_width: usize) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.open_above(display_row, content_width.max(1))),
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
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
            Self::Scratch(_) => None,
        }
    }

    pub fn matching_bracket_position(
        &self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.matching_bracket_position(
                display_row,
                display_column,
                content_width.max(1),
            ),
            Self::LargeFile(_) => None,
            Self::Scratch(_) => None,
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
            Self::Scratch(_) => None,
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
            Self::Scratch(_) => None,
        }
    }

    pub fn is_scratch(&self) -> bool {
        matches!(self, Self::Scratch(_))
    }

    pub fn scratch_target_at_row(&self, row: usize) -> Option<ScratchTarget> {
        match self {
            Self::Scratch(document) => document.target_at_row(row),
            _ => None,
        }
    }

    pub fn diagnostics_for_display_row(
        &self,
        display_row: usize,
        page_width: usize,
    ) -> Vec<DiagnosticEntry> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => {
                document.diagnostics_for_display_row(display_row, content_width.max(1))
            }
            Self::LargeFile(_) | Self::Scratch(_) => Vec::new(),
        }
    }

    pub fn collect_diagnostics(&self) -> Vec<(usize, Vec<DiagnosticEntry>)> {
        match self {
            Self::Editable(document) => document.collect_diagnostics(),
            Self::LargeFile(_) | Self::Scratch(_) => Vec::new(),
        }
    }

    pub fn full_text(&self) -> Option<String> {
        match self {
            Self::Editable(document) => Some(document.full_text()),
            Self::LargeFile(_) | Self::Scratch(_) => None,
        }
    }

    pub fn lsp_position_for_display_position(
        &self,
        display_row: usize,
        display_column: usize,
        page_width: usize,
    ) -> Option<Position> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => Some(document.lsp_position_for_display_position(
                display_row,
                display_column,
                content_width.max(1),
            )),
            Self::LargeFile(_) | Self::Scratch(_) => None,
        }
    }

    pub fn display_position_for_lsp_position(
        &self,
        position: Position,
        page_width: usize,
    ) -> Option<(usize, usize)> {
        let content_width = page_width.saturating_sub(
            DIAGNOSTIC_WIDTH + 1 + LINE_NUMBER_WIDTH + 1 + GUTTER_WIDTH + 1,
        );

        match self {
            Self::Editable(document) => document.display_position_for_lsp_position(
                position.line,
                position.character,
                content_width.max(1),
            ),
            Self::LargeFile(_) | Self::Scratch(_) => None,
        }
    }

    pub fn apply_text_edits(&mut self, edits: &[TextEdit]) -> bool {
        match self {
            Self::Editable(document) => {
                document.apply_text_edits(edits);
                true
            }
            Self::LargeFile(_) | Self::Scratch(_) => false,
        }
    }

    pub fn replace_all(&mut self, find: &str, replace: &str) -> Option<usize> {
        match self {
            Self::Editable(document) => Some(document.replace_all(find, replace)),
            Self::LargeFile(_) | Self::Scratch(_) => None,
        }
    }
}

fn build_status(page_width: usize, label: &str) -> String {
    let width = page_width.max(label.len());
    format!("{label}{}", "-".repeat(width.saturating_sub(label.len())))
}

fn build_render_line(
    diagnostic_marker: Option<DiagnosticSeverity>,
    line_number: usize,
    gutter_marker: Option<char>,
    text: String,
    syntax_spans: Vec<SyntaxTokenSpan>,
) -> DocumentRenderLine {
    DocumentRenderLine {
        diagnostic_marker: match diagnostic_marker {
            Some(DiagnosticSeverity::Warning) => "W".to_owned(),
            Some(DiagnosticSeverity::Error) => "E".to_owned(),
            None => " ".to_owned(),
        },
        line_number,
        gutter_marker: gutter_marker.unwrap_or(' ').to_string(),
        text,
        syntax_spans,
    }
}
