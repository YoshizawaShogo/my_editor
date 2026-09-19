use ropey::Rope;

use crate::{
    editor::{Direction, Unit},
    position::{CharIdx, char_idx_to_line_col, line_col_to_char_idx},
    view::Selection,
};

pub fn move_head(
    text: &Rope,
    selection: Selection,
    direction: Direction,
    unit: Unit,
    extend: bool,
) -> Selection {
    let head = match unit {
        Unit::Character => move_character(text, selection.head, direction),
        Unit::Line => move_line(text, selection.head, direction),
        Unit::LineStartSmart => smart_home(text, selection.head),
        Unit::Document => move_document(text, direction),
        Unit::Word => move_word(text, selection.head, direction),
    };
    Selection {
        anchor: if extend { selection.anchor } else { head },
        head,
    }
}

fn move_character(text: &Rope, index: CharIdx, direction: Direction) -> CharIdx {
    match direction {
        Direction::Left => CharIdx(index.0.saturating_sub(1)),
        Direction::Right => CharIdx((index.0 + 1).min(text.len_chars())),
        Direction::Up | Direction::Down => move_line(text, index, direction),
    }
}

fn move_line(text: &Rope, index: CharIdx, direction: Direction) -> CharIdx {
    let (line, col) = char_idx_to_line_col(text, index);
    let last_line = text.len_lines().saturating_sub(1);
    let target = match direction {
        // At the edges of the document there is no line to move to, so the press
        // goes to the edge of the line instead of doing nothing: Up on the first
        // line lands on its start, Down on the last on its end. Not the smart-home
        // toggle that the Home key does — an arrow that bounced between two spots
        // on repeated presses would be its own surprise.
        Direction::Up if line == 0 => return line_col_to_char_idx(text, 0, 0),
        Direction::Down if line == last_line => {
            return line_col_to_char_idx(text, last_line, usize::MAX);
        }
        Direction::Up => line - 1,
        Direction::Down => line + 1,
        Direction::Left => return line_col_to_char_idx(text, line, 0),
        Direction::Right => return line_col_to_char_idx(text, line, usize::MAX),
    };
    line_col_to_char_idx(text, target, col)
}

fn move_document(text: &Rope, direction: Direction) -> CharIdx {
    match direction {
        Direction::Left | Direction::Up => CharIdx(0),
        Direction::Right | Direction::Down => CharIdx(text.len_chars()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Regular,
    Whitespace,
    Separator,
}

fn classify(character: char) -> CharClass {
    if is_word(character) {
        CharClass::Regular
    } else if character.is_whitespace() {
        CharClass::Whitespace
    } else {
        CharClass::Separator
    }
}

/// Word motion matching VS Code's default `cursorWordLeft`/`cursorWordRight`:
/// moving right lands on the *end* of the next word, moving left on the *start*
/// of the previous word. Whitespace is skipped, and a lone separator wedged
/// between two words (`foo.bar`) is passed over transparently, while a run of two
/// or more separators (`foo::bar`, `a === b`) is a stop of its own.
fn move_word(text: &Rope, index: CharIdx, direction: Direction) -> CharIdx {
    let len = text.len_chars();
    match direction {
        Direction::Right | Direction::Down => {
            let mut cursor = index.0;
            while cursor < len && classify(text.char(cursor)) == CharClass::Whitespace {
                cursor += 1;
            }
            if cursor >= len {
                return CharIdx(len);
            }
            if classify(text.char(cursor)) == CharClass::Regular {
                while cursor < len && classify(text.char(cursor)) == CharClass::Regular {
                    cursor += 1;
                }
                return CharIdx(cursor);
            }
            // A separator run: stop at its end, unless it is a single separator
            // immediately followed by a word, which is skipped through to that
            // word's end.
            let start = cursor;
            while cursor < len && classify(text.char(cursor)) == CharClass::Separator {
                cursor += 1;
            }
            if cursor - start == 1
                && cursor < len
                && classify(text.char(cursor)) == CharClass::Regular
            {
                while cursor < len && classify(text.char(cursor)) == CharClass::Regular {
                    cursor += 1;
                }
            }
            CharIdx(cursor)
        }
        Direction::Left | Direction::Up => {
            let mut cursor = index.0;
            while cursor > 0 && classify(text.char(cursor - 1)) == CharClass::Whitespace {
                cursor -= 1;
            }
            if cursor == 0 {
                return CharIdx(0);
            }
            if classify(text.char(cursor - 1)) == CharClass::Regular {
                while cursor > 0 && classify(text.char(cursor - 1)) == CharClass::Regular {
                    cursor -= 1;
                }
                return CharIdx(cursor);
            }
            let end = cursor;
            while cursor > 0 && classify(text.char(cursor - 1)) == CharClass::Separator {
                cursor -= 1;
            }
            if end - cursor == 1
                && cursor > 0
                && classify(text.char(cursor - 1)) == CharClass::Regular
            {
                while cursor > 0 && classify(text.char(cursor - 1)) == CharClass::Regular {
                    cursor -= 1;
                }
            }
            CharIdx(cursor)
        }
    }
}

/// Smart Home: jump to the first non-blank character of the line, or to column 0
/// when the caret is already sitting on it. Toggling between the two is what makes
/// a single Home key do both.
fn smart_home(text: &Rope, index: CharIdx) -> CharIdx {
    let head = index.0.min(text.len_chars());
    let line = text.char_to_line(head);
    let line_start = text.line_to_char(line);
    let indent = text
        .line(line)
        .chars()
        .take_while(|character| *character == ' ' || *character == '\t')
        .count();
    let first_non_blank = line_start + indent;
    if head == first_non_blank {
        CharIdx(line_start)
    } else {
        CharIdx(first_non_blank)
    }
}

pub fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertical_movement_clamps_to_shorter_line() {
        let text = Rope::from_str("abcdef\nxy\n");
        let selection = Selection::caret(CharIdx(5));

        let moved = move_head(&text, selection, Direction::Down, Unit::Character, false);

        assert_eq!(moved, Selection::caret(CharIdx(9)));
    }

    #[test]
    fn smart_home_toggles_between_first_non_blank_and_column_zero() {
        let text = Rope::from_str("    let x = 1;\n");
        // From the end of the line, Home lands on the first non-blank ("let").
        let from_end = move_head(
            &text,
            Selection::caret(CharIdx(13)),
            Direction::Left,
            Unit::LineStartSmart,
            false,
        );
        assert_eq!(from_end.head, CharIdx(4));

        // Pressing it again, already on the first non-blank, goes to column 0.
        let from_indent = move_head(
            &text,
            Selection::caret(CharIdx(4)),
            Direction::Left,
            Unit::LineStartSmart,
            false,
        );
        assert_eq!(from_indent.head, CharIdx(0));

        // And once more toggles back to the first non-blank.
        let from_start = move_head(
            &text,
            Selection::caret(CharIdx(0)),
            Direction::Left,
            Unit::LineStartSmart,
            false,
        );
        assert_eq!(from_start.head, CharIdx(4));
    }

    #[test]
    fn vertical_movement_at_the_document_edges_goes_to_the_line_edge() {
        let text = Rope::from_str("first line\nsecond");
        let up = |from, extend| {
            move_head(
                &text,
                Selection::caret(CharIdx(from)),
                Direction::Up,
                Unit::Character,
                extend,
            )
        };

        // Up on the first line: nowhere to go, so it lands on the line start.
        assert_eq!(up(5, false).head, CharIdx(0));
        // Repeating it stays there rather than toggling like Home does.
        assert_eq!(up(0, false).head, CharIdx(0));
        // Shift+Up keeps the anchor, so the run to the start is selected.
        let extended = up(5, true);
        assert_eq!((extended.anchor, extended.head), (CharIdx(5), CharIdx(0)));

        // Down on the last line lands on its end.
        let down = move_head(
            &text,
            Selection::caret(CharIdx(13)),
            Direction::Down,
            Unit::Character,
            false,
        );
        assert_eq!(down.head, CharIdx(17));

        // A line in the middle still moves by line, keeping the column.
        let middle = move_head(
            &text,
            Selection::caret(CharIdx(3)),
            Direction::Down,
            Unit::Character,
            false,
        );
        assert_eq!(middle.head, CharIdx(14));
    }

    #[test]
    fn smart_home_extends_the_selection_when_asked() {
        let text = Rope::from_str("    value\n");
        let moved = move_head(
            &text,
            Selection::caret(CharIdx(9)),
            Direction::Left,
            Unit::LineStartSmart,
            true,
        );
        assert_eq!(moved.anchor, CharIdx(9));
        assert_eq!(moved.head, CharIdx(4));
    }

    #[test]
    fn word_movement_uses_identifier_boundaries() {
        let text = Rope::from_str("one  two_three");
        let selection = Selection::caret(CharIdx(0));

        let moved = move_head(&text, selection, Direction::Right, Unit::Word, false);

        assert_eq!(moved.head, CharIdx(3));
    }

    fn right_stops(text: &str, from: usize) -> Vec<usize> {
        let rope = Rope::from_str(text);
        std::iter::successors(Some(CharIdx(from)), |index| {
            let next = move_head(
                &rope,
                Selection::caret(*index),
                Direction::Right,
                Unit::Word,
                false,
            )
            .head;
            (next != *index).then_some(next)
        })
        .map(|index| index.0)
        .collect()
    }

    fn left_stops(text: &str, from: usize) -> Vec<usize> {
        let rope = Rope::from_str(text);
        std::iter::successors(Some(CharIdx(from)), |index| {
            let next = move_head(
                &rope,
                Selection::caret(*index),
                Direction::Left,
                Unit::Word,
                false,
            )
            .head;
            (next != *index).then_some(next)
        })
        .map(|index| index.0)
        .collect()
    }

    #[test]
    fn word_movement_right_lands_on_word_ends() {
        // VS Code style: end of each word, skipping whitespace between them — no
        // separate stop on the gap.
        assert_eq!(right_stops("one  two", 0), vec![0, 3, 8]);
    }

    #[test]
    fn word_movement_left_lands_on_word_starts() {
        assert_eq!(left_stops("one  two", 8), vec![8, 5, 0]);
    }

    #[test]
    fn word_movement_skips_a_lone_separator_but_stops_on_a_run() {
        // "foo.bar baz(qux)": a single '.' or '(' between words is transparent, so
        // the stops are the word ends (right) / starts (left).
        assert_eq!(
            right_stops("foo.bar baz(qux)", 0),
            vec![0, 3, 7, 11, 15, 16]
        );
        assert_eq!(left_stops("foo.bar baz(qux)", 16), vec![16, 12, 8, 4, 0]);
        // A run of two separators is its own stop.
        assert_eq!(right_stops("foo::bar", 0), vec![0, 3, 5, 8]);
    }
}
