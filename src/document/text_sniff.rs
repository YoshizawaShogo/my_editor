/// How much of a file is inspected before deciding whether it is text.
///
/// Bounded so that opening a multi-gigabyte file does not have to touch all of
/// it — for the memory-mapped path in particular, scanning the whole mapping
/// would fault in the entire file just to answer "can I display this?".
const SAMPLE_BYTES: usize = 8192;

/// Whether `bytes` reads as UTF-8 text, judged from its first [`SAMPLE_BYTES`].
///
/// A NUL byte is the binary signal: text files essentially never contain one,
/// while almost every binary format does within its header.
///
/// The subtlety is the sample boundary. Cutting at a fixed byte count lands in
/// the middle of a multi-byte character roughly two times in three for CJK text,
/// and `from_utf8` reports that truncation as an encoding error like any other.
/// Treating it as one would misreport most non-ASCII files as binary, so a
/// sequence that is merely *unfinished* at the cut is accepted — but only when
/// there are more bytes after the sample to finish it. When the sample is the
/// whole file, an unfinished sequence is genuinely malformed and stays a
/// rejection.
pub fn looks_like_text(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(SAMPLE_BYTES)];
    if sample.contains(&0) {
        return false;
    }
    match std::str::from_utf8(sample) {
        Ok(_) => true,
        // `error_len() == None` is `from_utf8`'s "input ended part-way through
        // a character", as opposed to `Some(n)` for bytes that cannot start or
        // continue one.
        Err(error) => error.error_len().is_none() && sample.len() < bytes.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a file whose `SAMPLE_BYTES` cut falls inside a multi-byte
    /// character: one leading ASCII byte offsets the 3-byte characters so the
    /// boundary splits one.
    fn multibyte_split_at_sample_boundary() -> Vec<u8> {
        let mut bytes = b"x".to_vec();
        while bytes.len() < SAMPLE_BYTES + 64 {
            bytes.extend_from_slice("あ".as_bytes());
        }
        assert!(std::str::from_utf8(&bytes[..SAMPLE_BYTES]).is_err());
        bytes
    }

    #[test]
    fn a_character_split_by_the_sample_boundary_is_still_text() {
        assert!(looks_like_text(&multibyte_split_at_sample_boundary()));
    }

    #[test]
    fn a_truncated_character_at_the_end_of_the_file_is_not_text() {
        // Same bytes, cut short so nothing follows the sample to complete the
        // character — now the file really is malformed.
        let bytes = multibyte_split_at_sample_boundary();
        assert!(!looks_like_text(&bytes[..SAMPLE_BYTES]));
    }

    #[test]
    fn a_nul_byte_marks_the_file_binary() {
        assert!(!looks_like_text(b"ELF\0\x02\x01"));
    }

    #[test]
    fn invalid_utf8_inside_the_sample_is_not_text() {
        assert!(!looks_like_text(&[0x41, 0xff, 0xfe, 0x41]));
    }
}
