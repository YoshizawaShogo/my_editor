use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{LineEnding, Revision};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PersistedHistory {
    pub path: PathBuf,
    pub base_hash: u64,
    pub line_ending: LineEnding,
    pub past: Vec<Revision>,
    pub future: Vec<Revision>,
}

pub fn content_hash(contents: &str) -> u64 {
    fnv1a(contents.as_bytes())
}

pub fn history_key(path: &Path) -> String {
    format!("{:016x}", fnv1a(path.to_string_lossy().as_bytes()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_hash_is_stable() {
        assert_eq!(content_hash("hello"), 0xa430d84680aabd0b);
        assert_eq!(history_key(Path::new("/tmp/a")), "6cc13bddf2746ce7");
    }
}
