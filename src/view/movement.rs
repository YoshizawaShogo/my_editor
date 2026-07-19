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
    let target = match direction {
        Direction::Up => line.saturating_sub(1),
        Direction::Down => (line + 1).min(text.len_lines().saturating_sub(1)),
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

fn move_word(text: &Rope, index: CharIdx, direction: Direction) -> CharIdx {
    // Word motion stops at every run boundary rather than jumping a whole word
    // plus its trailing gap in one step: a press lands on the end of the current
    // word, the next press skips the following symbols/whitespace to the next
    // word's start. Whether the current run is word characters or not decides
    // which kind of run we consume this step.
    match direction {
        Direction::Left | Direction::Up => {
            let mut cursor = index.0;
            if cursor == 0 {
                return CharIdx(0);
            }
            let in_word = is_word(text.char(cursor - 1));
            while cursor > 0 && is_word(text.char(cursor - 1)) == in_word {
                cursor -= 1;
            }
            CharIdx(cursor)
        }
        Direction::Right | Direction::Down => {
            let len = text.len_chars();
            let mut cursor = index.0;
            if cursor >= len {
                return CharIdx(len);
            }
            let in_word = is_word(text.char(cursor));
            while cursor < len && is_word(text.char(cursor)) == in_word {
                cursor += 1;
            }
            CharIdx(cursor)
        }
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
    fn word_movement_uses_identifier_boundaries() {
        let text = Rope::from_str("one  two_three");
        let selection = Selection::caret(CharIdx(0));

        let moved = move_head(&text, selection, Direction::Right, Unit::Word, false);

        assert_eq!(moved.head, CharIdx(3));
    }

    #[test]
    fn word_movement_right_stops_at_each_run_boundary() {
        let text = Rope::from_str("one  two");
        let stops: Vec<usize> = std::iter::successors(Some(CharIdx(0)), |index| {
            let next = move_head(
                &text,
                Selection::caret(*index),
                Direction::Right,
                Unit::Word,
                false,
            )
            .head;
            (next != *index).then_some(next)
        })
        .map(|index| index.0)
        .collect();

        // End of "one", start of "two", end of "two" — the gap is its own stop.
        assert_eq!(stops, vec![0, 3, 5, 8]);
    }

    #[test]
    fn word_movement_left_mirrors_run_boundaries() {
        let text = Rope::from_str("one  two");
        let stops: Vec<usize> = std::iter::successors(Some(CharIdx(8)), |index| {
            let next = move_head(
                &text,
                Selection::caret(*index),
                Direction::Left,
                Unit::Word,
                false,
            )
            .head;
            (next != *index).then_some(next)
        })
        .map(|index| index.0)
        .collect();

        assert_eq!(stops, vec![8, 5, 3, 0]);
    }
}
