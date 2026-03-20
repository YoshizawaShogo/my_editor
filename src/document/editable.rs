use std::{fs::File, path::Path};

use ropey::Rope;

use crate::error::Result;

pub struct EditableDocument {
    pub rope: Rope,
}

impl EditableDocument {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let rope = Rope::from_reader(file)?;
        Ok(Self { rope })
    }

    pub fn read_page(&self, page_height: usize, page_width: usize) -> Result<EditablePage> {
        let mut rows = Vec::with_capacity(page_height);

        for (line_index, line) in self.rope.lines().enumerate().take(page_height) {
            let text = line.to_string();
            let line = text.trim_end_matches('\n').trim_end_matches('\r');

            for piece in wrap_visible_segments(line, page_width) {
                rows.push(EditableRow {
                    line_number: line_index + 1,
                    text: piece,
                });
                if rows.len() >= page_height {
                    return Ok(EditablePage { rows });
                }
            }
        }

        if rows.is_empty() {
            rows.push(EditableRow {
                line_number: 1,
                text: String::new(),
            });
        }

        Ok(EditablePage { rows })
    }
}

pub struct EditablePage {
    pub rows: Vec<EditableRow>,
}

pub struct EditableRow {
    pub line_number: usize,
    pub text: String,
}

fn wrap_visible_segments(line: &str, page_width: usize) -> Vec<String> {
    if page_width == 0 {
        return Vec::new();
    }

    if line.is_empty() {
        return vec![String::new()];
    }

    let chars: Vec<char> = line.chars().collect();
    chars
        .chunks(page_width)
        .map(|chunk| chunk.iter().collect())
        .collect()
}
