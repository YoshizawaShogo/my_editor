//! The lines pinned over the top of a file pane: the ones that open the blocks
//! the visible code sits in — the `fn`, the `impl`, the `if` — so which
//! function or block is on screen stays readable after its first line has
//! scrolled away. Indentation is the whole model: no grammar is consulted, so it
//! works for any language that indents its blocks.

use ropey::Rope;

/// The most lines pinned at once. Past this the header starts to crowd out the
/// code it is there to explain.
pub(crate) const STICKY_MAX_LINES: usize = 5;

/// The lines to pin over a pane whose first visible line is `top_line`,
/// outermost first: each one less indented than the one after it.
///
/// A line pinned on row `r` covers whatever is on that row, so it is pinned
/// only while the block it opens still runs on past that row — a block whose
/// last line would sit under its own header has nothing left to explain. At
/// most `limit` lines, keeping the innermost (they say the most about where you
/// are), and never so many that they would cover `caret_row`, the caret's row in
/// the pane, since a caret hidden under the header cannot be seen to type at.
pub(crate) fn sticky_lines(
    text: &Rope,
    top_line: usize,
    tab_size: usize,
    limit: usize,
    caret_row: Option<usize>,
) -> Vec<usize> {
    let limit = caret_row.map_or(limit, |row| limit.min(row));
    if limit == 0 {
        return Vec::new();
    }
    let horizon = top_line + STICKY_MAX_LINES + 1;
    let mut pinned = Vec::new();
    for opener in enclosing_lines(text, top_line, tab_size) {
        // Outer blocks run at least as far as inner ones and sit on earlier
        // rows, so once one stops earning its row, none after it can.
        if block_end(text, opener, top_line, horizon, tab_size) <= top_line + pinned.len() {
            break;
        }
        pinned.push(opener);
    }
    if pinned.len() > limit {
        pinned.drain(..pinned.len() - limit);
    }
    pinned
}

/// The lines above `line` that open the blocks it sits in, outermost first.
fn enclosing_lines(text: &Rope, line: usize, tab_size: usize) -> Vec<usize> {
    let total = text.len_lines();
    // A blank line has no indent of its own; it belongs to the code after it.
    let Some(mut reference) = (line..total).find_map(|line| indent(text, line, tab_size)) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for above in (0..line.min(total)).rev() {
        if reference == 0 {
            break;
        }
        if let Some(width) = indent(text, above, tab_size)
            && width < reference
        {
            found.push(heading_start(text, above, width, tab_size));
            reference = width;
        }
    }
    found.reverse();
    found
}

/// Where the heading that ends on `line` begins. A signature too long for one
/// line ends in `) -> Vec<usize> {`, which is the line that opens the block but
/// says nothing about it; the line its parameters hang from — `fn name(` —
/// is the one to pin. A line not starting with `)` or `]` is its own heading.
fn heading_start(text: &Rope, line: usize, width: usize, tab_size: usize) -> usize {
    let content = text.line(line).to_string();
    if !content.trim_start().starts_with([')', ']']) {
        return line;
    }
    (0..line)
        .rev()
        .find(|above| indent(text, *above, tab_size).is_some_and(|indent| indent <= width))
        .unwrap_or(line)
}

/// The last line of the block `opener` begins, looking on from `from` (a line
/// inside it) no further than `horizon` — a block still going there is going
/// far enough. The block runs while lines are indented deeper than its opener;
/// a closing bracket or keyword back at the opener's indent still belongs to it.
fn block_end(text: &Rope, opener: usize, from: usize, horizon: usize, tab_size: usize) -> usize {
    let Some(width) = indent(text, opener, tab_size) else {
        return from;
    };
    let total = text.len_lines();
    let stop = (horizon + 1).min(total);
    let mut last = from;
    for line in from + 1..stop {
        match indent(text, line, tab_size) {
            None => {}
            Some(deeper) if deeper > width => last = line,
            Some(_) => {
                if closes_a_block(text, line) {
                    last = line;
                }
                return last;
            }
        }
    }
    if stop < total { horizon } else { last }
}

/// Whether `line` begins by closing a block: a bracket, or one of the words
/// shells and Tcl-likes end blocks with.
fn closes_a_block(text: &Rope, line: usize) -> bool {
    let content = text.line(line).to_string();
    let content = content.trim_start();
    if content.starts_with(['}', ')', ']']) {
        return true;
    }
    let word: String = content
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    matches!(
        word.as_str(),
        "end" | "fi" | "done" | "esac" | "endif" | "endsw" | "endwhile" | "endforeach"
    )
}

/// The display width of `line`'s leading whitespace, tabs to the next stop, or
/// `None` when the line is blank.
fn indent(text: &Rope, line: usize, tab_size: usize) -> Option<usize> {
    let tab_size = tab_size.max(1);
    let mut width = 0;
    for character in text.line(line).chars() {
        match character {
            ' ' => width += 1,
            '\t' => width += tab_size - width % tab_size,
            '\r' | '\n' => return None,
            _ => return Some(width),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "\
impl Editor {
    fn update(&mut self) {
        if ready {
            first();

            second();
        }
        after();
        more();
        last();
    }
}
";

    fn pinned(top: usize) -> Vec<usize> {
        sticky_lines(&Rope::from_str(SOURCE), top, 4, STICKY_MAX_LINES, None)
    }

    #[test]
    fn the_blocks_around_the_top_line_are_pinned_outermost_first() {
        // Scrolled to `first();`: it sits in the `if`, in `update`, in the impl.
        assert_eq!(pinned(3), vec![0, 1, 2]);
    }

    #[test]
    fn a_closed_block_is_no_longer_pinned() {
        // `after();` is past the `if`: only `update` and the impl hold it.
        assert_eq!(pinned(7), vec![0, 1]);
    }

    #[test]
    fn a_block_ending_under_its_own_header_is_not_pinned() {
        // From `second();` the `if` closes two lines on, on the very row its
        // header would take; nothing of it would be left to explain.
        assert_eq!(pinned(5), vec![0, 1]);
    }

    #[test]
    fn the_innermost_are_kept_when_there_is_room_for_fewer() {
        let text = Rope::from_str(SOURCE);
        assert_eq!(sticky_lines(&text, 3, 4, 2, None), vec![1, 2]);
        // Nor may the header cover the caret: on the pane's second row, only one
        // line fits above it.
        assert_eq!(
            sticky_lines(&text, 3, 4, STICKY_MAX_LINES, Some(1)),
            vec![2]
        );
    }

    #[test]
    fn a_signature_spread_over_lines_is_pinned_by_its_first() {
        let source = "\
pub fn sticky_lines(
    text: &Rope,
    limit: usize,
) -> Vec<usize> {
    let a = 1;
    let b = 2;
    let c = 3;
    let d = 4;
}
";
        // Inside the body, the line that opens it is `) -> Vec<usize> {`; the
        // one that names the function is pinned instead.
        assert_eq!(
            sticky_lines(&Rope::from_str(source), 5, 4, STICKY_MAX_LINES, None),
            vec![0]
        );
    }

    #[test]
    fn code_at_the_left_margin_needs_no_header() {
        assert!(pinned(0).is_empty());
        assert!(pinned(11).is_empty());
    }
}
