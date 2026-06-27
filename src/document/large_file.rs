use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

use crate::{config, error::Result};

pub struct LargeFileDocument {
    pub path: PathBuf,
    pub file_size_bytes: u64,
    pub viewport: LargeFileViewport,
}

pub struct LargeFileViewport {
    pub byte_offset: u64,
    pub line_number: usize,
    pub left_column: usize,
    pub line_number_origin: LineNumberOrigin,
}

pub enum LineNumberOrigin {
    FromTop,
    FromBottom,
}

pub struct LargeFilePage {
    pub rows: Vec<LargeFileRow>,
    pub next_byte_offset: u64,
}

pub struct LargeFileRow {
    pub line_number: usize,
    pub text: String,
}

impl LargeFileDocument {
    pub fn new(path: PathBuf, file_size_bytes: u64) -> Self {
        Self {
            path,
            file_size_bytes,
            viewport: LargeFileViewport::new(),
        }
    }

    pub fn read_page(
        &self,
        start_row: usize,
        page_height: usize,
        page_width: usize,
    ) -> Result<LargeFilePage> {
        let window = self.read_window(page_width)?;
        let total_rows = window.rows.len();
        let effective_start_row = if total_rows == 0 {
            0
        } else {
            start_row.min(total_rows.saturating_sub(page_height.max(1)))
        };
        let rows = window
            .rows
            .into_iter()
            .enumerate()
            .skip(effective_start_row)
            .take(page_height)
            .map(|(index, row)| LargeFileRow {
                line_number: match self.viewport.line_number_origin {
                    LineNumberOrigin::FromTop => row.line_number,
                    LineNumberOrigin::FromBottom => total_rows.saturating_sub(index),
                },
                text: row.text,
            })
            .collect();

        Ok(LargeFilePage {
            rows,
            next_byte_offset: window.next_byte_offset,
        })
    }

    pub fn jump_to_top(&mut self) {
        self.viewport.byte_offset = 0;
        self.viewport.line_number = 1;
        self.viewport.left_column = 0;
        self.viewport.line_number_origin = LineNumberOrigin::FromTop;
    }

    pub fn jump_to_bottom(&mut self, page_height: usize, page_width: usize) -> Result<usize> {
        self.viewport.byte_offset = self
            .file_size_bytes
            .saturating_sub(config::large_file_read_window_bytes() as u64);
        self.viewport.left_column = 0;
        self.viewport.line_number_origin = LineNumberOrigin::FromBottom;

        let window = self.read_window(page_width)?;
        Ok(window.rows.len().saturating_sub(page_height.max(1)))
    }
}

impl LargeFileViewport {
    pub fn new() -> Self {
        Self {
            byte_offset: 0,
            line_number: 1,
            left_column: 0,
            line_number_origin: LineNumberOrigin::FromTop,
        }
    }
}

struct LargeFileReadWindow {
    rows: Vec<LargeFileWindowRow>,
    next_byte_offset: u64,
}

struct LargeFileWindowRow {
    line_number: usize,
    text: String,
}

impl LargeFileDocument {
    fn read_window(&self, page_width: usize) -> Result<LargeFileReadWindow> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.viewport.byte_offset))?;

        // Cap each page read so a single very long line does not force unbounded work.
        let read_window_bytes = config::large_file_read_window_bytes();
        let mut buffer = vec![0; read_window_bytes];
        let bytes_read = file.read(&mut buffer)?;
        buffer.truncate(bytes_read);

        let chunk = String::from_utf8_lossy(&buffer);
        let mut rows = Vec::new();
        let mut line_number = self.viewport.line_number;
        let mut consumed_bytes = 0u64;
        let mut saw_newline = false;

        for segment in chunk.split_inclusive('\n') {
            let had_newline = segment.ends_with('\n');
            let line = segment.trim_end_matches('\n').trim_end_matches('\r');
            consumed_bytes += segment.len() as u64;
            saw_newline |= had_newline;

            for piece in wrap_visible_segments(line, self.viewport.left_column, page_width) {
                rows.push(LargeFileWindowRow {
                    line_number,
                    text: piece,
                });
            }

            if had_newline {
                line_number += 1;
            }
        }

        if rows.is_empty() {
            rows.push(LargeFileWindowRow {
                line_number,
                text: String::new(),
            });
        }

        let next_byte_offset = if saw_newline || bytes_read < read_window_bytes {
            self.viewport.byte_offset + consumed_bytes
        } else {
            self.viewport.byte_offset + bytes_read as u64
        };

        Ok(LargeFileReadWindow {
            rows,
            next_byte_offset,
        })
    }
}

fn wrap_visible_segments(line: &str, left_column: usize, page_width: usize) -> Vec<String> {
    if page_width == 0 {
        return Vec::new();
    }

    let visible: String = line.chars().skip(left_column).collect();
    if visible.is_empty() {
        return vec![String::new()];
    }

    let chars: Vec<char> = visible.chars().collect();
    chars
        .chunks(page_width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}
