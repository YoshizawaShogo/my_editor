use ropey::Rope;
use std::sync::OnceLock;
use tree_sitter::{
    InputEdit, Language, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

mod csh;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HighlightSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub kind: String,
}

pub struct IncrementalHighlighter {
    language_name: String,
    tree: Option<Tree>,
    spans: Vec<HighlightSpan>,
}

impl std::fmt::Debug for IncrementalHighlighter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IncrementalHighlighter")
            .field("language_name", &self.language_name)
            .field("spans", &self.spans.len())
            .finish()
    }
}

impl IncrementalHighlighter {
    pub fn new(language_name: &str, source: &str) -> Option<Self> {
        // csh has no tree-sitter grammar; it is highlighted by a regex pass that
        // recomputes on every reparse, so there is no tree to maintain.
        if language_name != "csh" {
            grammar(language_name)?;
        }
        let mut highlighter = Self {
            language_name: language_name.to_owned(),
            tree: None,
            spans: Vec::new(),
        };
        highlighter.reparse(source, false);
        Some(highlighter)
    }

    pub fn edit(&mut self, text: &Rope, range: std::ops::Range<usize>, inserted: &str) {
        let Some(tree) = &mut self.tree else { return };
        let start_byte = text.char_to_byte(range.start);
        let old_end_byte = text.char_to_byte(range.end);
        let start_position = point_for_char(text, range.start);
        let old_end_position = point_for_char(text, range.end);
        let new_end_position = inserted_end_point(start_position, inserted);
        tree.edit(&InputEdit {
            start_byte,
            old_end_byte,
            new_end_byte: start_byte + inserted.len(),
            start_position,
            old_end_position,
            new_end_position,
        });
    }

    pub fn reparse(&mut self, source: &str, incremental: bool) {
        if self.language_name == "csh" {
            self.spans = csh::highlight(source);
            return;
        }
        let Some((language, query_source)) = grammar(&self.language_name) else {
            return;
        };
        let mut parser = Parser::new();
        if parser.set_language(&language).is_err() {
            return;
        }
        self.tree = parser.parse(source, incremental.then_some(self.tree.as_ref()).flatten());
        self.spans = self.tree.as_ref().map_or_else(Vec::new, |tree| {
            query_spans(&self.language_name, &language, query_source, tree, source)
        });
    }

    pub fn spans(&self) -> &[HighlightSpan] {
        &self.spans
    }
}

pub fn highlight(language_name: &str, source: &str) -> Vec<HighlightSpan> {
    if language_name == "csh" {
        return csh::highlight(source);
    }
    let Some((language, query_source)) = grammar(language_name) else {
        return Vec::new();
    };
    let mut parser = Parser::new();
    if parser.set_language(&language).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };
    query_spans(language_name, &language, query_source, &tree, source)
}

pub fn warm_hover_highlighting() {
    let _ = highlight("markdown", "Hover warmup");
    let _ = highlight("rust", "fn hover_warmup() {}");
}

fn query_spans(
    language_name: &str,
    language: &Language,
    query_source: &str,
    tree: &Tree,
    source: &str,
) -> Vec<HighlightSpan> {
    let Some(query) = cached_query(language_name, language, query_source) else {
        return Vec::new();
    };
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
    let mut spans = Vec::new();
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            spans.push(HighlightSpan {
                start_byte: capture.node.start_byte(),
                end_byte: capture.node.end_byte(),
                kind: names[capture.index as usize].to_string(),
            });
        }
    }
    spans.sort_by_key(|span| (span.start_byte, span.end_byte));
    spans
}

fn cached_query(
    language_name: &str,
    language: &Language,
    query_source: &str,
) -> Option<&'static Query> {
    static JSON: OnceLock<Option<Query>> = OnceLock::new();
    static TOML: OnceLock<Option<Query>> = OnceLock::new();
    static MARKDOWN: OnceLock<Option<Query>> = OnceLock::new();
    static RUST: OnceLock<Option<Query>> = OnceLock::new();
    static BASH: OnceLock<Option<Query>> = OnceLock::new();
    static TCL: OnceLock<Option<Query>> = OnceLock::new();
    let slot = match language_name {
        "json" => &JSON,
        "toml" => &TOML,
        "markdown" => &MARKDOWN,
        "rust" => &RUST,
        "bash" => &BASH,
        "tcl" => &TCL,
        _ => return None,
    };
    slot.get_or_init(|| Query::new(language, query_source).ok())
        .as_ref()
}

fn point_for_char(text: &Rope, index: usize) -> Point {
    let index = index.min(text.len_chars());
    let row = text.char_to_line(index);
    let line_start = text.line_to_char(row);
    let column = text.char_to_byte(index) - text.char_to_byte(line_start);
    Point::new(row, column)
}

fn inserted_end_point(start: Point, inserted: &str) -> Point {
    let lines = inserted.bytes().filter(|byte| *byte == b'\n').count();
    if lines == 0 {
        Point::new(start.row, start.column + inserted.len())
    } else {
        Point::new(
            start.row + lines,
            inserted.rsplit('\n').next().map_or(0, str::len),
        )
    }
}

fn grammar(name: &str) -> Option<(Language, &'static str)> {
    match name {
        "json" => Some((
            tree_sitter_json::LANGUAGE.into(),
            tree_sitter_json::HIGHLIGHTS_QUERY,
        )),
        "toml" => Some((
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
        )),
        "markdown" => Some((
            tree_sitter_md_025::LANGUAGE.into(),
            tree_sitter_md_025::HIGHLIGHT_QUERY_BLOCK,
        )),
        "rust" => Some((
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        )),
        "bash" => Some((
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY,
        )),
        "tcl" => Some((
            tree_sitter_tcl::LANGUAGE.into(),
            include_str!("tcl_highlights.scm"),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_strings_and_numbers_are_highlighted() {
        let spans = highlight("json", r#"{"value": 42}"#);

        assert!(spans.iter().any(|span| span.kind.contains("string")));
        assert!(spans.iter().any(|span| span.kind.contains("number")));
    }

    #[test]
    fn bash_comments_strings_and_keywords_are_highlighted() {
        let spans = highlight("bash", "# note\nif true; then\n  echo \"hi\"\nfi\n");

        assert!(spans.iter().any(|span| span.kind.contains("comment")));
        assert!(spans.iter().any(|span| span.kind.contains("string")));
        assert!(spans.iter().any(|span| span.kind.contains("keyword")));
    }

    #[test]
    fn tcl_comments_strings_numbers_keywords_and_procs_are_highlighted() {
        // The number lives inside an `expr`: Tcl only parses digits as a number
        // node there, not as a bare command argument.
        let source = "# greet\nproc greet {name} {\n    set count [expr {3 + 1}]\n    puts \"hi $name\"\n}\n";
        let spans = highlight("tcl", source);
        let kinds = |needle: &str| spans.iter().any(|span| span.kind.contains(needle));

        assert!(kinds("comment"), "no comment span: {spans:?}");
        assert!(kinds("string"), "no string span: {spans:?}");
        assert!(kinds("number"), "no number span: {spans:?}");
        assert!(kinds("keyword"), "no keyword span: {spans:?}");
        assert!(kinds("function"), "no function span: {spans:?}");
        // The proc's name is coloured as a function.
        let greet = source.match_indices("greet").nth(1).unwrap().0;
        assert!(
            spans.iter().any(|span| span.kind.contains("function")
                && span.start_byte <= greet
                && greet < span.end_byte),
            "proc name not highlighted as function: {spans:?}"
        );
    }

    #[test]
    fn csh_keywords_comments_strings_and_numbers_are_highlighted() {
        let source = "#!/bin/csh\nset name = \"world\"\nif ( $name == \"world\" ) then\n    echo \"hi $name\"\nendif\nforeach item ( a b c )\n    echo $item 42\nend\n";
        let spans = highlight("csh", source);

        let keywords: Vec<_> = spans
            .iter()
            .filter(|span| span.kind.contains("keyword"))
            .map(|span| &source[span.start_byte..span.end_byte])
            .collect();

        assert!(spans.iter().any(|span| span.kind.contains("comment")));
        assert!(spans.iter().any(|span| span.kind.contains("string")));
        assert!(spans.iter().any(|span| span.kind.contains("number")));
        // The csh control-flow words the bash grammar used to miss now colour.
        for expected in ["set", "if", "then", "endif", "foreach", "end"] {
            assert!(
                keywords.contains(&expected),
                "expected csh keyword {expected:?} highlighted, got {keywords:?}"
            );
        }
    }

    #[test]
    fn csh_keywords_inside_comments_and_strings_are_not_highlighted() {
        let source = "# set foreach\necho \"set if endif\"\n";
        let spans = highlight("csh", source);

        assert!(
            !spans.iter().any(|span| span.kind.contains("keyword")),
            "keyword coloured inside comment/string: {spans:?}"
        );
    }

    #[test]
    fn csh_incremental_highlighter_recomputes_on_reparse() {
        let text = Rope::from_str("set x = 1\n");
        let mut highlighter = IncrementalHighlighter::new("csh", &text.to_string()).unwrap();
        assert!(
            highlighter
                .spans()
                .iter()
                .any(|s| s.kind.contains("keyword"))
        );

        let updated = "set x = 1\nforeach i ( a )\nend\n";
        highlighter.reparse(updated, true);
        let keywords: Vec<_> = highlighter
            .spans()
            .iter()
            .filter(|s| s.kind.contains("keyword"))
            .map(|s| &updated[s.start_byte..s.end_byte])
            .collect();
        assert!(keywords.contains(&"foreach"), "got {keywords:?}");
        assert!(keywords.contains(&"end"), "got {keywords:?}");
    }

    #[test]
    fn incremental_edit_updates_tree_and_spans() {
        let mut text = Rope::from_str(r#"{"value": 1}"#);
        let mut highlighter = IncrementalHighlighter::new("json", &text.to_string()).unwrap();
        let end = text.len_chars() - 1;

        highlighter.edit(&text, end..end, ", \"next\": 2");
        text.insert(end, ", \"next\": 2");
        highlighter.reparse(&text.to_string(), true);

        assert!(
            highlighter
                .spans()
                .iter()
                .filter(|span| span.kind.contains("number"))
                .count()
                >= 2
        );
    }

    #[test]
    fn rust_comment_newline_incremental_highlight_matches_full_reparse() {
        let source = "// comment\nfn solve() {}\n";
        let mut text = Rope::from_str(source);
        let mut highlighter = IncrementalHighlighter::new("rust", source).unwrap();
        let insertion = "// comment".chars().count();

        highlighter.edit(&text, insertion..insertion, "\n// ");
        text.insert(insertion, "\n// ");
        let edited = text.to_string();
        highlighter.reparse(&edited, true);

        assert_eq!(highlighter.spans(), highlight("rust", &edited).as_slice());
    }

    #[test]
    fn repeated_rust_comment_newlines_do_not_leak_comment_highlight_to_code() {
        let source = "//\nfn solve(s: &mut String) {\n    s.push(\"d\");\n}\n";
        let mut text = Rope::from_str(source);
        let mut highlighter = IncrementalHighlighter::new("rust", source).unwrap();

        for _ in 0..8 {
            let insertion = text.line_to_char(0) + 2;
            highlighter.edit(&text, insertion..insertion, "\n// ");
            text.insert(insertion, "\n// ");
            highlighter.reparse(&text.to_string(), true);
        }

        let edited = text.to_string();
        let spans = highlighter.spans();
        assert_eq!(spans, highlight("rust", &edited).as_slice());
        let solve_start = edited.find("solve").unwrap();
        let solve_span = spans
            .iter()
            .rev()
            .find(|span| span.start_byte <= solve_start && solve_start < span.end_byte)
            .unwrap();
        assert!(
            !solve_span.kind.contains("comment"),
            "solve highlighted as {:?}",
            solve_span
        );
    }
}
