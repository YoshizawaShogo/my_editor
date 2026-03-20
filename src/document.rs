use std::{fs, io::Write, path::Path};

use crate::{config, error::Result};

pub mod editable;
pub mod large_file;

pub enum Document {
    Editable(editable::EditableDocument),
    LargeFile(large_file::LargeFileDocument),
}

const LINE_NUMBER_WIDTH: usize = 6;

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

    pub fn render_first_page_to<W: Write>(
        &self,
        writer: &mut W,
        page_height: usize,
        page_width: usize,
    ) -> Result<()> {
        let content_height = page_height.saturating_sub(1);
        let content_width = page_width.saturating_sub(LINE_NUMBER_WIDTH + 1);

        match self {
            Self::Editable(document) => {
                let page = document.read_page(content_height, content_width)?;
                for row in page.rows {
                    writeln!(
                        writer,
                        "{:>width$} {}",
                        row.line_number,
                        row.text,
                        width = LINE_NUMBER_WIDTH
                    )?;
                }
                write_footer(writer, page_width, "EDITOR")?;
            }
            Self::LargeFile(document) => {
                let page = document.read_page(content_height, content_width)?;
                for row in page.rows {
                    writeln!(
                        writer,
                        "{:>width$} {}",
                        row.line_number,
                        row.text,
                        width = LINE_NUMBER_WIDTH
                    )?;
                }
                let status = if page.next_byte_offset >= document.file_size_bytes {
                    "VIEWER END"
                } else {
                    "VIEWER"
                };
                write_footer(writer, page_width, status)?;
            }
        }
        Ok(())
    }
}

fn write_footer<W: Write>(writer: &mut W, page_width: usize, label: &str) -> Result<()> {
    let width = page_width.max(label.len());
    let footer = format!("{label}{}", "-".repeat(width.saturating_sub(label.len())));
    writeln!(writer, "{footer}")?;
    Ok(())
}
