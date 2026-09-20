use std::{cell::RefCell, fs::File, path::Path};

use memmap2::{Mmap, MmapOptions};

pub struct LargeFile {
    mmap: Mmap,
    index: RefCell<LineIndex>,
}

impl LargeFile {
    pub fn open(path: &Path) -> std::result::Result<Self, String> {
        let file = File::open(path)
            .map_err(|error| format!("大容量ファイルを開けません {}: {error}", path.display()))?;
        // SAFETY: The mapping is read-only. External truncation remains the documented SIGBUS risk.
        let mmap = unsafe { MmapOptions::new().map(&file) }
            .map_err(|error| format!("mmapできません {}: {error}", path.display()))?;
        Ok(Self {
            mmap,
            index: RefCell::new(LineIndex::default()),
        })
    }

    pub fn len_bytes(&self) -> usize {
        self.mmap.len()
    }

    pub fn validate_text(&self) -> bool {
        super::looks_like_text(&self.mmap)
    }

    pub fn line(&self, line: usize) -> Option<String> {
        self.ensure_line(line + 1);
        let index = self.index.borrow();
        let start = *index.starts.get(line)? as usize;
        let end = index
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.mmap.len() as u64) as usize;
        let bytes = self.mmap.get(start..end)?;
        Some(
            String::from_utf8_lossy(bytes)
                .trim_end_matches(['\r', '\n'])
                .to_owned(),
        )
    }

    /// The last line that exists, or `None` while the index has not reached the
    /// end of the file yet. Scrolling clamps with it: the index is built lazily,
    /// so the answer is only known once a scan has run past the end.
    pub fn last_line(&self) -> Option<usize> {
        let index = self.index.borrow();
        if !index.complete {
            return None;
        }
        let last = index.starts.len().saturating_sub(1);
        // A file ending in a newline leaves a start at EOF — an empty line that
        // no reader should be able to scroll onto.
        let trailing_newline = index.starts.last().copied() == Some(self.mmap.len() as u64);
        Some(if trailing_newline && last > 0 {
            last - 1
        } else {
            last
        })
    }

    pub fn ensure_line(&self, target: usize) {
        let mut index = self.index.borrow_mut();
        while index.starts.len() <= target && !index.complete {
            let start = index.scanned_to as usize;
            let Some(relative) = self.mmap[start..].iter().position(|byte| *byte == b'\n') else {
                index.scanned_to = self.mmap.len() as u64;
                index.complete = true;
                break;
            };
            let next = start + relative + 1;
            index.starts.push(next as u64);
            index.scanned_to = next as u64;
            if next == self.mmap.len() {
                index.complete = true;
            }
        }
    }
}

impl std::fmt::Debug for LargeFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LargeFile")
            .field("len", &self.mmap.len())
            .field("index", &self.index.borrow())
            .finish()
    }
}

impl PartialEq for LargeFile {
    fn eq(&self, other: &Self) -> bool {
        self.mmap[..] == other.mmap[..]
    }
}

impl Eq for LargeFile {}

#[derive(Debug)]
struct LineIndex {
    starts: Vec<u64>,
    scanned_to: u64,
    complete: bool,
}

impl Default for LineIndex {
    fn default() -> Self {
        Self {
            starts: vec![0],
            scanned_to: 0,
            complete: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write};

    use super::*;

    #[test]
    fn line_index_is_built_lazily() {
        let path = std::env::temp_dir().join(format!(
            "my_editor_large_file_{}_{}.txt",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let mut file = File::create(&path).unwrap();
        file.write_all(b"zero\none\ntwo\n").unwrap();
        file.sync_all().unwrap();
        let large = LargeFile::open(&path).unwrap();

        assert_eq!(large.index.borrow().starts, vec![0]);
        assert!(large.last_line().is_none());
        assert_eq!(large.line(2).as_deref(), Some("two"));
        assert_eq!(large.index.borrow().starts, vec![0, 5, 9, 13]);
        // The newline ending the file must not count as a fourth line.
        assert_eq!(large.last_line(), Some(2));

        drop(large);
        fs::remove_file(path).unwrap();
    }
}
