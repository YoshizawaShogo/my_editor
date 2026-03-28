use std::path::PathBuf;

use crate::document::{DocumentRender, DocumentRenderLine};

use super::DiagnosticSeverity;

#[derive(Clone)]
pub struct DiagnosticEntry {
    pub severity: DiagnosticSeverity,
    pub message: String,
}

#[derive(Clone)]
pub struct ScratchTarget {
    pub path: PathBuf,
    pub line_number: usize,
    pub column: usize,
}

#[derive(Clone)]
pub struct ScratchRow {
    pub text: String,
    pub target: Option<ScratchTarget>,
}

pub struct ScratchDocument {
    title: String,
    rows: Vec<ScratchRow>,
}

impl ScratchDocument {
    pub fn new(title: impl Into<String>, rows: Vec<ScratchRow>) -> Self {
        Self {
            title: title.into(),
            rows,
        }
    }

    pub fn render_first_page(&self, viewport_row: usize, page_height: usize) -> DocumentRender {
        let content_height = page_height.saturating_sub(1);
        let end_row = viewport_row.saturating_add(content_height).min(self.rows.len());
        let mut lines = self.rows[viewport_row.min(self.rows.len())..end_row]
            .iter()
            .enumerate()
            .map(|(offset, row)| DocumentRenderLine {
                diagnostic_marker: " ".to_owned(),
                line_number: viewport_row.saturating_add(offset).saturating_add(1),
                gutter_marker: " ".to_owned(),
                text: row.text.clone(),
                syntax_spans: Vec::new(),
            })
            .collect::<Vec<_>>();

        if lines.is_empty() {
            lines.push(DocumentRenderLine {
                diagnostic_marker: " ".to_owned(),
                line_number: 1,
                gutter_marker: " ".to_owned(),
                text: String::new(),
                syntax_spans: Vec::new(),
            });
        }

        DocumentRender {
            lines,
            status: format!("SCRATCH {}", self.title),
        }
    }

    pub fn total_rows(&self) -> usize {
        self.rows.len().max(1)
    }

    pub fn display_line_width(&self, display_row: usize) -> usize {
        self.rows
            .get(display_row)
            .map(|row| row.text.chars().count())
            .unwrap_or(0)
    }

    pub fn display_line_text(&self, display_row: usize) -> String {
        self.rows
            .get(display_row)
            .map(|row| row.text.clone())
            .unwrap_or_default()
    }

    pub fn jump_row_for_line_number(&self, line_number: usize) -> Option<usize> {
        if line_number == 0 || line_number > self.rows.len() {
            return None;
        }

        Some(line_number - 1)
    }

    pub fn target_at_row(&self, display_row: usize) -> Option<ScratchTarget> {
        self.rows
            .get(display_row)
            .and_then(|row| row.target.clone())
    }
}
