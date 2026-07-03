use ropey::Rope;
use std::sync::OnceLock;
use tree_sitter::{
    InputEdit, Language, Parser, Point, Query, QueryCursor, StreamingIterator, Tree,
};

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
        grammar(language_name)?;
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
    let slot = match language_name {
        "json" => &JSON,
        "toml" => &TOML,
        "markdown" => &MARKDOWN,
        "rust" => &RUST,
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
}
