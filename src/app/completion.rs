use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use lsp_types::{Position, TextEdit};

#[derive(Clone)]
pub struct CompletionItem {
    pub label: String,
    pub insert_text: String,
    pub filter_text: String,
    pub sort_text: Option<String>,
    pub text_edit: Option<TextEdit>,
}

pub struct CompletionState {
    pub active: bool,
    pub query_start: usize,
    pub query: String,
    pub items: Vec<CompletionItem>,
    pub serial: u64,
    pub last_requested_serial: u64,
    pub pending_request_serial: Option<u64>,
    pub last_edit_at: Option<Instant>,
    pub path: Option<PathBuf>,
}

impl Default for CompletionState {
    fn default() -> Self {
        Self {
            active: false,
            query_start: 0,
            query: String::new(),
            items: Vec::new(),
            serial: 0,
            last_requested_serial: 0,
            pending_request_serial: None,
            last_edit_at: None,
            path: None,
        }
    }
}

impl CompletionState {
    pub fn invalidate(&mut self) {
        self.active = false;
        self.items.clear();
        self.pending_request_serial = None;
        self.last_edit_at = None;
        self.query.clear();
        self.query_start = 0;
        self.path = None;
        self.serial = self.serial.saturating_add(1);
    }
}

pub fn completion_prefix(line: &str, cursor_column: usize) -> (usize, String) {
    let chars: Vec<char> = line.chars().collect();
    let cursor = cursor_column.min(chars.len());
    let mut start = cursor;
    while start > 0 && is_completion_word_char(chars[start - 1]) {
        start -= 1;
    }
    let prefix = chars[start..cursor].iter().collect();
    (start, prefix)
}

pub fn has_empty_completion_trigger(line: &str, cursor_column: usize) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let cursor = cursor_column.min(chars.len());
    if cursor == 0 {
        return false;
    }

    if chars[cursor - 1] == '.' {
        return true;
    }

    cursor >= 2 && chars[cursor - 2] == ':' && chars[cursor - 1] == ':'
}

pub fn collect_fallback_items(text: &str, prefix: &str, max_items: usize) -> Vec<CompletionItem> {
    if prefix.is_empty() {
        return Vec::new();
    }

    let mut seen = HashSet::new();
    let mut items = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if is_completion_word_char(ch) {
            current.push(ch);
            continue;
        }

        push_fallback_word(&mut items, &mut seen, &current, prefix, max_items);
        if items.len() >= max_items {
            return items;
        }
        current.clear();
    }

    push_fallback_word(&mut items, &mut seen, &current, prefix, max_items);
    items
}

pub fn rank_completion_items(
    items: Vec<CompletionItem>,
    query: &str,
    max_items: usize,
) -> Vec<CompletionItem> {
    if query.is_empty() {
        let mut items = items;
        items.sort_by(|left, right| {
            left.sort_text
                .cmp(&right.sort_text)
                .then_with(|| left.label.cmp(&right.label))
        });
        items.truncate(max_items);
        return items;
    }

    let matcher = SkimMatcherV2::default();
    let mut ranked = items
        .into_iter()
        .filter_map(|item| {
            let haystack = if item.filter_text.is_empty() {
                item.label.as_str()
            } else {
                item.filter_text.as_str()
            };
            let (score, _) = matcher.fuzzy_indices(haystack, query)?;
            let exact_prefix_bonus = if haystack.starts_with(query) { 10_000 } else { 0 };
            Some((score + exact_prefix_bonus, item))
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.sort_text.cmp(&right.sort_text))
            .then_with(|| left.label.cmp(&right.label))
    });

    ranked
        .into_iter()
        .map(|(_, item)| item)
        .take(max_items)
        .collect()
}

pub fn text_end_position(start: Position, inserted_text: &str) -> Position {
    let mut line = start.line;
    let mut column = start.character;
    let mut chars_in_last_line = 0u32;
    let mut saw_newline = false;

    for ch in inserted_text.chars() {
        if ch == '\n' {
            line += 1;
            chars_in_last_line = 0;
            saw_newline = true;
        } else {
            chars_in_last_line += 1;
        }
    }

    if saw_newline {
        Position::new(line, chars_in_last_line)
    } else {
        column += chars_in_last_line;
        Position::new(line, column)
    }
}

fn push_fallback_word(
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    word: &str,
    prefix: &str,
    max_items: usize,
) {
    if items.len() >= max_items
        || word.is_empty()
        || word == prefix
        || !word.starts_with(prefix)
        || !seen.insert(word.to_owned())
    {
        return;
    }

    items.push(CompletionItem {
        label: word.to_owned(),
        insert_text: word.to_owned(),
        filter_text: word.to_owned(),
        sort_text: None,
        text_edit: None,
    });
}

fn is_completion_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::{
        collect_fallback_items, completion_prefix, has_empty_completion_trigger,
        rank_completion_items, CompletionItem,
    };

    #[test]
    fn completion_prefix_extracts_identifier_before_cursor() {
        let (start, prefix) = completion_prefix("let rust_analyzer_available", 27);
        assert_eq!(start, 4);
        assert_eq!(prefix, "rust_analyzer_available");
    }

    #[test]
    fn fallback_items_prefix_match_and_dedupe() {
        let items = collect_fallback_items(
            "alpha beta alphabet alpha_beta alpha",
            "alp",
            8,
        );
        let labels = items.into_iter().map(|item| item.label).collect::<Vec<_>>();
        assert_eq!(labels, vec!["alpha", "alphabet", "alpha_beta"]);
    }

    #[test]
    fn lsp_items_are_ranked_by_query() {
        let items = vec![
            CompletionItem {
                label: "alloc".to_owned(),
                insert_text: "alloc".to_owned(),
                filter_text: "alloc".to_owned(),
                sort_text: None,
                text_edit: None,
            },
            CompletionItem {
                label: "collections".to_owned(),
                insert_text: "collections".to_owned(),
                filter_text: "collections".to_owned(),
                sort_text: None,
                text_edit: None,
            },
        ];
        let ranked = rank_completion_items(items, "colle", 8);
        let labels = ranked.into_iter().map(|item| item.label).collect::<Vec<_>>();
        assert_eq!(labels, vec!["collections"]);
    }

    #[test]
    fn empty_completion_trigger_detects_double_colon() {
        assert!(has_empty_completion_trigger("use std::", 9));
        assert!(has_empty_completion_trigger("foo.", 4));
        assert!(!has_empty_completion_trigger("use std", 7));
    }

    #[test]
    fn empty_query_keeps_sorted_lsp_items() {
        let items = vec![
            CompletionItem {
                label: "collections".to_owned(),
                insert_text: "collections".to_owned(),
                filter_text: "collections".to_owned(),
                sort_text: Some("b".to_owned()),
                text_edit: None,
            },
            CompletionItem {
                label: "alloc".to_owned(),
                insert_text: "alloc".to_owned(),
                filter_text: "alloc".to_owned(),
                sort_text: Some("a".to_owned()),
                text_edit: None,
            },
        ];
        let labels = rank_completion_items(items, "", 8)
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert_eq!(labels, vec!["alloc", "collections"]);
    }
}
