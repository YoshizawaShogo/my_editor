//! Static, per-language snippet tables and a small placeholder-expansion engine.
//!
//! Snippets are held as compile-time `const` data — the language differences are
//! absorbed before build time, keeping the runtime engine language-agnostic. The
//! body uses LSP snippet syntax (`$1`, `${1:default}`, `$0`) so the same engine
//! could later also expand snippets an LSP sends.

use std::ops::Range;

/// One snippet: the word that triggers it and the body it expands to.
pub struct Snippet {
    pub prefix: &'static str,
    pub body: &'static str,
}

/// Snippets available for `language` (the editor's language name), empty for a
/// language with none. csh has its own set because its control flow (`foreach`,
/// `if … then … endif`) is not the POSIX/bash shell syntax.
pub fn snippets_for(language: &str) -> &'static [Snippet] {
    match language {
        "rust" => RUST,
        "bash" => BASH,
        "csh" => CSH,
        "c" => C,
        "python" => PYTHON,
        "tcl" => TCL,
        _ => &[],
    }
}

const RUST: &[Snippet] = &[
    Snippet {
        prefix: "for",
        body: "for ${1:item} in ${2:iter} {\n    $0\n}",
    },
    Snippet {
        prefix: "fn",
        body: "fn ${1:name}(${2}) {\n    $0\n}",
    },
    Snippet {
        prefix: "if",
        body: "if ${1:cond} {\n    $0\n}",
    },
    Snippet {
        prefix: "while",
        body: "while ${1:cond} {\n    $0\n}",
    },
    Snippet {
        prefix: "match",
        body: "match ${1:expr} {\n    ${2:pattern} => $0,\n}",
    },
];

const BASH: &[Snippet] = &[
    Snippet {
        prefix: "for",
        body: "for ${1:x} in ${2:list}; do\n    $0\ndone",
    },
    Snippet {
        prefix: "if",
        body: "if ${1:cond}; then\n    $0\nfi",
    },
    Snippet {
        prefix: "while",
        body: "while ${1:cond}; do\n    $0\ndone",
    },
    Snippet {
        prefix: "case",
        body: "case ${1:word} in\n    ${2:pattern})\n        $0\n        ;;\nesac",
    },
    Snippet {
        prefix: "fn",
        body: "${1:name}() {\n    $0\n}",
    },
];

const CSH: &[Snippet] = &[
    Snippet {
        prefix: "foreach",
        body: "foreach ${1:i} (${2:list})\n    $0\nend",
    },
    Snippet {
        prefix: "if",
        body: "if (${1:cond}) then\n    $0\nendif",
    },
    Snippet {
        prefix: "while",
        body: "while (${1:cond})\n    $0\nend",
    },
    Snippet {
        prefix: "switch",
        body: "switch (${1:word})\n    case ${2:pat}:\n        $0\n        breaksw\nendsw",
    },
];

const C: &[Snippet] = &[
    Snippet {
        prefix: "for",
        body: "for (${1:int i = 0}; ${2:i < n}; ${3:i++}) {\n    $0\n}",
    },
    Snippet {
        prefix: "if",
        body: "if (${1:cond}) {\n    $0\n}",
    },
    Snippet {
        prefix: "while",
        body: "while (${1:cond}) {\n    $0\n}",
    },
    Snippet {
        prefix: "fn",
        body: "${1:void} ${2:name}(${3}) {\n    $0\n}",
    },
    Snippet {
        prefix: "inc",
        body: "#include <${1:stdio.h}>$0",
    },
];

const PYTHON: &[Snippet] = &[
    Snippet {
        prefix: "for",
        body: "for ${1:item} in ${2:iterable}:\n    $0",
    },
    Snippet {
        prefix: "if",
        body: "if ${1:cond}:\n    $0",
    },
    Snippet {
        prefix: "while",
        body: "while ${1:cond}:\n    $0",
    },
    Snippet {
        prefix: "def",
        body: "def ${1:name}(${2}):\n    $0",
    },
    Snippet {
        prefix: "class",
        body: "class ${1:Name}:\n    $0",
    },
];

const TCL: &[Snippet] = &[
    Snippet {
        prefix: "for",
        body: "for {${1:set i 0}} {${2:$i < $n}} {${3:incr i}} {\n    $0\n}",
    },
    Snippet {
        prefix: "if",
        body: "if {${1:cond}} {\n    $0\n}",
    },
    Snippet {
        prefix: "while",
        body: "while {${1:cond}} {\n    $0\n}",
    },
    Snippet {
        prefix: "proc",
        body: "proc ${1:name} {${2:args}} {\n    $0\n}",
    },
    Snippet {
        prefix: "foreach",
        body: "foreach ${1:x} ${2:list} {\n    $0\n}",
    },
];

/// An expanded snippet ready to insert: the literal `text`, and the tab stops as
/// char ranges within that text, ordered `$1, $2, …` with `$0` last. A range with
/// `start == end` is an empty stop (a bare caret); otherwise it spans a default
/// the caret should select so typing replaces it.
pub struct Expansion {
    pub text: String,
    pub stops: Vec<Range<usize>>,
}

/// Expand `body` (LSP snippet syntax) into insertable text. `base_indent` is
/// prepended after every newline so a multi-line body stays aligned with the
/// line it is inserted on. Unrecognised `$` sequences are kept literally.
pub fn expand(body: &str, base_indent: &str) -> Expansion {
    let mut text = String::new();
    let mut len = 0usize; // chars pushed to `text` so far
    let mut stops: Vec<(u32, usize, usize)> = Vec::new();
    let mut chars = body.chars().peekable();

    while let Some(character) = chars.next() {
        match character {
            '\n' => {
                text.push('\n');
                len += 1;
                for indent in base_indent.chars() {
                    text.push(indent);
                    len += 1;
                }
            }
            '$' if chars.peek() == Some(&'{') => {
                chars.next(); // consume '{'
                let index = take_number(&mut chars);
                let mut default = String::new();
                if chars.peek() == Some(&':') {
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        if next == '}' {
                            break;
                        }
                        default.push(next);
                        chars.next();
                    }
                }
                if chars.peek() == Some(&'}') {
                    chars.next();
                }
                let start = len;
                for default_char in default.chars() {
                    text.push(default_char);
                    len += 1;
                }
                stops.push((index, start, len));
            }
            '$' if chars.peek().is_some_and(char::is_ascii_digit) => {
                let index = take_number(&mut chars);
                stops.push((index, len, len));
            }
            other => {
                text.push(other);
                len += 1;
            }
        }
    }

    // $0 is the terminal stop, so it sorts after every numbered stop.
    stops.sort_by_key(|(index, _, _)| if *index == 0 { u32::MAX } else { *index });
    Expansion {
        text,
        stops: stops
            .into_iter()
            .map(|(_, start, end)| start..end)
            .collect(),
    }
}

fn take_number(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u32 {
    let mut digits = String::new();
    while let Some(&next) = chars.peek() {
        if next.is_ascii_digit() {
            digits.push(next);
            chars.next();
        } else {
            break;
        }
    }
    digits.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_inlines_defaults_and_orders_stops_with_zero_last() {
        let expansion = expand("for ${1:x} in ${2:y} {\n$0\n}", "");

        assert_eq!(expansion.text, "for x in y {\n\n}");
        // Stops are $1, $2, then $0 (the final caret) — three in that order.
        assert_eq!(expansion.stops.len(), 3);
        // $1 selects the "x" default (chars 4..5).
        assert_eq!(expansion.stops[0], 4..5);
        assert_eq!(&expansion.text[4..5], "x");
        // $2 selects "y".
        assert_eq!(&expansion.text[expansion.stops[1].clone()], "y");
        // $0 is an empty caret stop (on the blank middle line).
        assert!(expansion.stops[2].is_empty());
    }

    #[test]
    fn expand_prepends_base_indent_after_newlines() {
        let expansion = expand("if ${1:c} {\n    $0\n}", "  ");

        // Each continuation line gains the two-space base indent.
        assert_eq!(expansion.text, "if c {\n      \n  }");
    }

    #[test]
    fn expand_keeps_dollar_signs_inside_defaults_literal() {
        // Tcl defaults contain '$', which must not be read as another tab stop.
        let expansion = expand("while {${1:$i < $n}} {}", "");
        assert_eq!(expansion.text, "while {$i < $n} {}");
        assert_eq!(expansion.stops.len(), 1);
        assert_eq!(&expansion.text[expansion.stops[0].clone()], "$i < $n");
    }

    #[test]
    fn snippets_are_defined_for_every_targeted_language() {
        for language in ["rust", "bash", "csh", "c", "python", "tcl"] {
            assert!(
                !snippets_for(language).is_empty(),
                "{language} should have snippets"
            );
        }
        assert!(snippets_for("json").is_empty());
    }
}
