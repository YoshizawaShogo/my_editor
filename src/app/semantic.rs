use std::collections::HashMap;

use lsp_types::{SemanticToken, SemanticTokens};
use serde_json::Value;

use crate::{
    document::{SyntaxHighlightKind, SyntaxTokenSpan},
    error::{AppError, Result},
};

pub fn decode_semantic_tokens_response(
    result: Option<Value>,
    legend: &[String],
) -> Result<HashMap<usize, Vec<SyntaxTokenSpan>>> {
    let Some(value) = result else {
        return Ok(HashMap::new());
    };
    if value.is_null() {
        return Ok(HashMap::new());
    }

    let semantic_tokens = match serde_json::from_value::<SemanticTokens>(value.clone()) {
        Ok(tokens) => tokens,
        Err(_) => {
            let result = serde_json::from_value::<lsp_types::SemanticTokensResult>(value)
                .map_err(|error| AppError::CommandFailed(error.to_string()))?;
            match result {
                lsp_types::SemanticTokensResult::Tokens(tokens) => tokens,
                lsp_types::SemanticTokensResult::Partial(partial) => SemanticTokens {
                    result_id: None,
                    data: partial.data,
                },
            }
        }
    };

    Ok(decode_semantic_tokens_data(&semantic_tokens.data, legend))
}

pub fn decode_semantic_tokens_data(
    data: &[SemanticToken],
    legend: &[String],
) -> HashMap<usize, Vec<SyntaxTokenSpan>> {
    let mut by_line = HashMap::<usize, Vec<SyntaxTokenSpan>>::new();
    let mut line = 0u32;
    let mut start = 0u32;

    for token in data {
        line = line.saturating_add(token.delta_line);
        if token.delta_line == 0 {
            start = start.saturating_add(token.delta_start);
        } else {
            start = token.delta_start;
        }

        let Some(kind_name) = legend.get(token.token_type as usize) else {
            continue;
        };
        let Some(kind) = map_semantic_kind(kind_name) else {
            continue;
        };

        by_line
            .entry(line as usize + 1)
            .or_default()
            .push(SyntaxTokenSpan {
                start: start as usize,
                length: token.length as usize,
                kind,
            });
    }

    by_line
}

pub fn slice_wrapped_syntax_spans(
    line_tokens: &[SyntaxTokenSpan],
    piece_start: usize,
    piece_len: usize,
) -> Vec<SyntaxTokenSpan> {
    let piece_end = piece_start.saturating_add(piece_len);

    line_tokens
        .iter()
        .filter_map(|token| {
            let token_start = token.start;
            let token_end = token.start.saturating_add(token.length);
            let overlap_start = token_start.max(piece_start);
            let overlap_end = token_end.min(piece_end);
            (overlap_start < overlap_end).then(|| SyntaxTokenSpan {
                start: overlap_start.saturating_sub(piece_start),
                length: overlap_end.saturating_sub(overlap_start),
                kind: token.kind,
            })
        })
        .collect()
}

pub fn map_semantic_kind(kind: &str) -> Option<SyntaxHighlightKind> {
    Some(match kind {
        "keyword" | "selfKeyword" | "boolean" => SyntaxHighlightKind::Keyword,
        "string" | "character" | "escapeSequence" | "formatSpecifier" => {
            SyntaxHighlightKind::String
        }
        "comment" => SyntaxHighlightKind::Comment,
        "type"
        | "struct"
        | "enum"
        | "interface"
        | "typeParameter"
        | "typeAlias"
        | "builtinType"
        | "selfType" => SyntaxHighlightKind::Type,
        "function" | "method" => SyntaxHighlightKind::Function,
        "variable" => SyntaxHighlightKind::Variable,
        "parameter" | "lifetime" => SyntaxHighlightKind::Parameter,
        "number" => SyntaxHighlightKind::Number,
        "operator" => SyntaxHighlightKind::Operator,
        "macro" | "attribute" | "derive" | "decorator" => SyntaxHighlightKind::Macro,
        "namespace" => SyntaxHighlightKind::Namespace,
        "property" | "field" | "enumMember" => SyntaxHighlightKind::Property,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::SyntaxHighlightKind;
    use serde_json::json;

    #[test]
    fn decodes_relative_semantic_tokens_into_one_based_lines() {
        let legend = vec![
            "keyword".to_owned(),
            "variable".to_owned(),
            "function".to_owned(),
        ];
        let data = vec![
            SemanticToken {
                delta_line: 0,
                delta_start: 0,
                length: 3,
                token_type: 0,
                token_modifiers_bitset: 0,
            },
            SemanticToken {
                delta_line: 0,
                delta_start: 4,
                length: 3,
                token_type: 1,
                token_modifiers_bitset: 0,
            },
            SemanticToken {
                delta_line: 1,
                delta_start: 4,
                length: 5,
                token_type: 2,
                token_modifiers_bitset: 0,
            },
        ];

        let decoded = decode_semantic_tokens_data(&data, &legend);

        let line1 = decoded.get(&1).expect("line 1");
        assert_eq!(line1.len(), 2);
        assert_eq!(line1[0].start, 0);
        assert_eq!(line1[0].length, 3);
        assert!(matches!(line1[0].kind, SyntaxHighlightKind::Keyword));
        assert_eq!(line1[1].start, 4);
        assert_eq!(line1[1].length, 3);
        assert!(matches!(line1[1].kind, SyntaxHighlightKind::Variable));

        let line2 = decoded.get(&2).expect("line 2");
        assert_eq!(line2.len(), 1);
        assert_eq!(line2[0].start, 4);
        assert_eq!(line2[0].length, 5);
        assert!(matches!(line2[0].kind, SyntaxHighlightKind::Function));
    }

    #[test]
    fn slices_spans_for_wrapped_piece() {
        let spans = vec![
            SyntaxTokenSpan {
                start: 2,
                length: 5,
                kind: SyntaxHighlightKind::Keyword,
            },
            SyntaxTokenSpan {
                start: 9,
                length: 2,
                kind: SyntaxHighlightKind::Type,
            },
        ];

        let sliced = slice_wrapped_syntax_spans(&spans, 4, 4);

        assert_eq!(sliced.len(), 1);
        assert_eq!(sliced[0].start, 0);
        assert_eq!(sliced[0].length, 3);
        assert!(matches!(sliced[0].kind, SyntaxHighlightKind::Keyword));
    }

    #[test]
    fn decodes_rust_analyzer_flat_json_shape() {
        let legend = vec!["keyword".to_owned(), "variable".to_owned()];
        let value = json!({
            "resultId": "1",
            "data": [0, 0, 3, 0, 0, 0, 4, 3, 1, 0]
        });

        let decoded =
            decode_semantic_tokens_response(Some(value), &legend).expect("semantic decode");

        let line1 = decoded.get(&1).expect("line 1");
        assert_eq!(line1.len(), 2);
        assert_eq!(line1[0].start, 0);
        assert_eq!(line1[0].length, 3);
        assert!(matches!(line1[0].kind, SyntaxHighlightKind::Keyword));
        assert_eq!(line1[1].start, 4);
        assert_eq!(line1[1].length, 3);
        assert!(matches!(line1[1].kind, SyntaxHighlightKind::Variable));
    }
}
