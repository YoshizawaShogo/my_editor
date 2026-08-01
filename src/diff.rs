//! Line alignment for the side-by-side diff view.
//!
//! Plain text in, plain rows out — no language server involved, so it works on
//! any two buffers regardless of language or whether a server is running.

/// Columns each diff row spends on its gutter before the line text: the
/// change marker, the right-aligned line number, and a separating space.
/// The renderer draws it and the editor hit-tests against it, so both read it
/// from here rather than each hard-coding 6.
pub const LINE_NUMBER_WIDTH: usize = 4;
pub const GUTTER_WIDTH: u16 = 1 + LINE_NUMBER_WIDTH as u16 + 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffKind {
    Equal,
    Added,
    Removed,
    Changed,
}

/// One rendered row: the line each side contributes, if any. A row with a hole
/// on one side is where that file has nothing to show against the other.
#[derive(Clone, Debug)]
pub struct DiffRow {
    pub left: Option<(usize, String)>,
    pub right: Option<(usize, String)>,
    pub kind: DiffKind,
}

impl DiffRow {
    pub fn is_change(&self) -> bool {
        self.kind != DiffKind::Equal
    }
}

pub fn rope_lines(text: &ropey::Rope) -> Vec<String> {
    text.lines()
        .map(|line| line.to_string().trim_end_matches(['\r', '\n']).to_owned())
        .collect()
}

/// A run of one line, flagged with whether it is part of what changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Segment {
    pub text: String,
    pub changed: bool,
}

/// Beyond this many token pairs the intra-line table is not worth building —
/// minified or generated lines would stall a frame for a result nobody reads.
const MAX_TOKEN_PAIRS: usize = 100_000;

/// Split the two versions of a changed line into runs, flagging the runs that
/// actually differ, so only those get tinted rather than the whole line.
///
/// Tokens, not characters: matching per character finds accidental letters in
/// common and produces confetti. Identifiers and numbers stay whole, runs of
/// whitespace stay whole (so re-indentation reads as one change), and each
/// punctuation mark stands alone so `foo(a)` against `foo(b)` narrows to `b`.
pub fn word_segments(before: &str, after: &str) -> (Vec<Segment>, Vec<Segment>) {
    let left = tokens(before);
    let right = tokens(after);
    if left.len().saturating_mul(right.len()) > MAX_TOKEN_PAIRS {
        return (whole_line(before), whole_line(after));
    }

    let width = right.len() + 1;
    let mut lcs = vec![0usize; (left.len() + 1) * width];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            lcs[i * width + j] = if left[i] == right[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }

    let (mut left_out, mut right_out) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    while i < left.len() || j < right.len() {
        if i < left.len() && j < right.len() && left[i] == right[j] {
            push_segment(&mut left_out, left[i], false);
            push_segment(&mut right_out, right[j], false);
            i += 1;
            j += 1;
        } else if j == right.len()
            || (i < left.len() && lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
        {
            push_segment(&mut left_out, left[i], true);
            i += 1;
        } else {
            push_segment(&mut right_out, right[j], true);
            j += 1;
        }
    }
    (left_out, right_out)
}

fn whole_line(line: &str) -> Vec<Segment> {
    if line.is_empty() {
        return Vec::new();
    }
    vec![Segment {
        text: line.to_owned(),
        changed: true,
    }]
}

/// Append `token`, merging into the previous run when it carries the same flag,
/// so the renderer emits one span per run instead of one per token.
fn push_segment(segments: &mut Vec<Segment>, token: &str, changed: bool) {
    match segments.last_mut() {
        Some(last) if last.changed == changed => last.text.push_str(token),
        _ => segments.push(Segment {
            text: token.to_owned(),
            changed,
        }),
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TokenClass {
    Word,
    Space,
    Symbol,
}

fn token_class(character: char) -> TokenClass {
    if character.is_alphanumeric() || character == '_' {
        TokenClass::Word
    } else if character.is_whitespace() {
        TokenClass::Space
    } else {
        TokenClass::Symbol
    }
}

fn tokens(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let mut characters = rest.char_indices();
        let (_, first) = characters.next().expect("rest is non-empty");
        let end = match token_class(first) {
            // One symbol per token: grouping `);` would drag an unchanged
            // bracket into a neighbouring change.
            TokenClass::Symbol => first.len_utf8(),
            class => characters
                .find(|(_, character)| token_class(*character) != class)
                .map_or(rest.len(), |(index, _)| index),
        };
        tokens.push(&rest[..end]);
        rest = &rest[end..];
    }
    tokens
}

/// First row of each run of consecutive changed rows — what "next difference"
/// steps through.
pub fn hunk_starts(rows: &[DiffRow]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(index, row)| {
            row.is_change()
                && index
                    .checked_sub(1)
                    .is_none_or(|prev| !rows[prev].is_change())
        })
        .map(|(index, _)| index)
        .collect()
}

pub fn aligned(left: &[String], right: &[String]) -> Vec<DiffRow> {
    if left.len().saturating_mul(right.len()) > 1_000_000 {
        return index_diff(left, right);
    }
    let width = right.len() + 1;
    let mut lcs = vec![0usize; (left.len() + 1) * width];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            lcs[i * width + j] = if left[i] == right[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let mut rows = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < left.len() || j < right.len() {
        if i < left.len() && j < right.len() && left[i] == right[j] {
            rows.push(DiffRow {
                left: Some((i, left[i].clone())),
                right: Some((j, right[j].clone())),
                kind: DiffKind::Equal,
            });
            i += 1;
            j += 1;
            continue;
        }
        let mut removed = Vec::new();
        let mut added = Vec::new();
        while i < left.len() || j < right.len() {
            if i < left.len() && j < right.len() && left[i] == right[j] {
                break;
            }
            if j == right.len()
                || (i < left.len() && lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
            {
                removed.push((i, left[i].clone()));
                i += 1;
            } else {
                added.push((j, right[j].clone()));
                j += 1;
            }
        }
        let count = removed.len().max(added.len());
        for index in 0..count {
            let left_row = removed.get(index).cloned();
            let right_row = added.get(index).cloned();
            rows.push(DiffRow {
                kind: match (left_row.is_some(), right_row.is_some()) {
                    (true, true) => DiffKind::Changed,
                    (true, false) => DiffKind::Removed,
                    (false, true) => DiffKind::Added,
                    (false, false) => unreachable!(),
                },
                left: left_row,
                right: right_row,
            });
        }
    }
    rows
}

/// Fallback for file pairs where the quadratic table would be too big: compare
/// line `n` against line `n`, so the view still renders rather than stalling.
fn index_diff(left: &[String], right: &[String]) -> Vec<DiffRow> {
    (0..left.len().max(right.len()))
        .map(|index| {
            let left_row = left.get(index).cloned().map(|line| (index, line));
            let right_row = right.get(index).cloned().map(|line| (index, line));
            let kind = match (&left_row, &right_row) {
                (Some((_, left)), Some((_, right))) if left == right => DiffKind::Equal,
                (Some(_), Some(_)) => DiffKind::Changed,
                (Some(_), None) => DiffKind::Removed,
                (None, Some(_)) => DiffKind::Added,
                (None, None) => unreachable!(),
            };
            DiffRow {
                left: left_row,
                right: right_row,
                kind,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insertions_and_changed_lines_are_aligned() {
        let left = vec!["same".to_owned(), "old".to_owned(), "tail".to_owned()];
        let right = vec![
            "same".to_owned(),
            "new".to_owned(),
            "added".to_owned(),
            "tail".to_owned(),
        ];

        let rows = aligned(&left, &right);

        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].kind, DiffKind::Equal);
        assert_eq!(rows[1].kind, DiffKind::Changed);
        assert_eq!(rows[2].kind, DiffKind::Added);
        assert!(rows[2].left.is_none());
        assert_eq!(rows[2].right.as_ref().unwrap().1, "added");
    }

    fn texts(segments: &[Segment]) -> Vec<(&str, bool)> {
        segments
            .iter()
            .map(|segment| (segment.text.as_str(), segment.changed))
            .collect()
    }

    #[test]
    fn word_segments_narrow_a_changed_line_to_the_token_that_moved() {
        let (left, right) = word_segments("let total = count + 1;", "let total = count + 2;");

        assert_eq!(
            texts(&left),
            vec![("let total = count + ", false), ("1", true), (";", false)]
        );
        assert_eq!(
            texts(&right),
            vec![("let total = count + ", false), ("2", true), (";", false)]
        );
    }

    #[test]
    fn word_segments_keep_punctuation_out_of_a_neighbouring_change() {
        // Grouping `);` into one token would tint the bracket along with `b`.
        let (left, right) = word_segments("foo(a);", "foo(b);");

        assert_eq!(
            texts(&left),
            vec![("foo(", false), ("a", true), (");", false)]
        );
        assert_eq!(
            texts(&right),
            vec![("foo(", false), ("b", true), (");", false)]
        );
    }

    #[test]
    fn word_segments_treat_reindentation_as_one_change() {
        let (left, right) = word_segments("  value", "\tvalue");

        assert_eq!(texts(&left), vec![("  ", true), ("value", false)]);
        assert_eq!(texts(&right), vec![("\t", true), ("value", false)]);
    }

    #[test]
    fn word_segments_mark_the_whole_line_when_nothing_is_shared() {
        let (left, right) = word_segments("alpha", "beta");

        assert_eq!(texts(&left), vec![("alpha", true)]);
        assert_eq!(texts(&right), vec![("beta", true)]);
    }

    #[test]
    fn word_segments_fall_back_to_a_single_run_on_very_long_lines() {
        let long = "x ".repeat(MAX_TOKEN_PAIRS);
        let (left, _) = word_segments(&long, "y");

        assert_eq!(left.len(), 1);
        assert!(left[0].changed);
    }

    #[test]
    fn hunk_starts_mark_each_run_of_changes_once() {
        let left = vec![
            "a".to_owned(),
            "b".to_owned(),
            "c".to_owned(),
            "d".to_owned(),
        ];
        let right = vec![
            "a".to_owned(),
            "B".to_owned(),
            "c".to_owned(),
            "D".to_owned(),
        ];

        let rows = aligned(&left, &right);

        // Two separate runs of change, each reported once however long it is.
        assert_eq!(hunk_starts(&rows), vec![1, 3]);
    }
}
