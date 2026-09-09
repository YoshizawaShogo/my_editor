//! Regex-based highlighter for csh/tcsh.
//!
//! csh has no mature tree-sitter grammar, and its control flow diverges enough
//! from bash that reusing the bash grammar left its own keywords (`set`,
//! `foreach`, `endif`, `end`, ...) uncoloured. The dialect is small, so a
//! single forward scan does the job: comments and quoted strings are carved out
//! first, then keywords, numbers, variables and operators are matched only over
//! the remaining code so nothing inside a string or comment is recoloured.

use std::sync::OnceLock;

use regex::Regex;

use super::HighlightSpan;

/// Control-flow words and the builtins csh users read as keywords.
const KEYWORDS: &[&str] = &[
    "if", "then", "else", "endif", "while", "end", "foreach", "switch", "case", "default", "endsw",
    "breaksw", "break", "continue", "goto", "repeat", "onintr", "set", "setenv", "unset",
    "unsetenv", "alias", "unalias", "source", "exit", "eval", "shift", "exec", "umask", "limit",
    "glob", "rehash", "unhash", "login", "logout",
];

pub fn highlight(source: &str) -> Vec<HighlightSpan> {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut spans = Vec::new();
    let mut index = 0;
    let mut code_start = 0;

    while index < len {
        match bytes[index] {
            b'#' if at_word_start(bytes, index) => {
                scan_code(source, code_start, index, &mut spans);
                let end = line_end(bytes, index);
                spans.push(span(index, end, "comment"));
                index = end;
                code_start = index;
            }
            quote @ (b'\'' | b'"') => {
                scan_code(source, code_start, index, &mut spans);
                let end = string_end(bytes, index, quote);
                spans.push(span(index, end, "string"));
                index = end;
                code_start = index;
            }
            _ => index += 1,
        }
    }
    scan_code(source, code_start, len, &mut spans);

    spans.sort_by_key(|span| (span.start_byte, span.end_byte));
    spans
}

/// A `#` opens a comment only at the start of a word: column zero or right
/// after whitespace. This keeps `$#argv` and `arr#2` out of comment colour.
fn at_word_start(bytes: &[u8], index: usize) -> bool {
    index == 0 || bytes[index - 1].is_ascii_whitespace()
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| from + offset)
}

/// End (exclusive) of a quoted string that opened at `from`. Runs to the
/// closing quote, or to end-of-line/input if the quote is never closed. Inside
/// double quotes a backslash escapes the next byte; single quotes take the
/// next bare quote as the close, matching csh.
fn string_end(bytes: &[u8], from: usize, quote: u8) -> usize {
    let mut index = from + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => return index,
            b'\\' if quote == b'"' && index + 1 < bytes.len() => index += 2,
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// Match keywords, numbers, variables and operators over one code run and push
/// their spans with absolute offsets. Variable ranges are found first so a
/// `$set` substitution is not also lit as the `set` keyword.
fn scan_code(source: &str, start: usize, end: usize, spans: &mut Vec<HighlightSpan>) {
    if start >= end {
        return;
    }
    let code = &source[start..end];

    let variables: Vec<_> = variable_regex()
        .find_iter(code)
        .map(|matched| matched.range())
        .collect();
    for range in &variables {
        spans.push(span(start + range.start, start + range.end, "variable"));
    }
    let in_variable = |range: &std::ops::Range<usize>| {
        variables
            .iter()
            .any(|variable| range.start < variable.end && variable.start < range.end)
    };

    for matched in keyword_regex().find_iter(code) {
        if !in_variable(&matched.range()) {
            spans.push(span(
                start + matched.start(),
                start + matched.end(),
                "keyword",
            ));
        }
    }
    for matched in number_regex().find_iter(code) {
        if !in_variable(&matched.range()) {
            spans.push(span(
                start + matched.start(),
                start + matched.end(),
                "number",
            ));
        }
    }
    for matched in operator_regex().find_iter(code) {
        if !in_variable(&matched.range()) {
            spans.push(span(
                start + matched.start(),
                start + matched.end(),
                "operator",
            ));
        }
    }
}

fn span(start_byte: usize, end_byte: usize, kind: &str) -> HighlightSpan {
    HighlightSpan {
        start_byte,
        end_byte,
        kind: kind.to_owned(),
    }
}

fn keyword_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        let alternation = KEYWORDS.join("|");
        Regex::new(&format!(r"\b(?:{alternation})\b")).expect("valid keyword regex")
    })
}

fn number_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"\b\d+(?:\.\d+)?\b").expect("valid number regex"))
}

fn variable_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    // $name, ${name}, $#name, $?name, $<, $$, and an optional [..] index.
    REGEX.get_or_init(|| {
        Regex::new(r"\$(?:\{[^}]*\}|[#?]?[A-Za-z_][A-Za-z0-9_]*(?:\[[^\]]*\])?|[<$*#?])")
            .expect("valid variable regex")
    })
}

fn operator_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"==|!=|=~|!~|<=|>=|&&|\|\||<<|>>|[-+*/%<>=!~|&]").expect("valid operator regex")
    })
}
