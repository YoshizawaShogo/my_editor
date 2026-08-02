use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::{
    diff::{DiffKind, Segment, aligned, rope_lines, word_segments},
    editor::{ActiveBuffer, Editor},
    position::{char_idx_to_display_pos, display_col_after},
    view::is_word,
};

const BG: Color = Color::Rgb(0x16, 0x18, 0x21);
const FG: Color = Color::Rgb(0xc6, 0xc8, 0xd1);
const MUTED: Color = Color::Rgb(0x6b, 0x70, 0x89);
/// The caret's own line number, drawn white against the muted rest of the
/// gutter so the cursor row is findable at a glance — including while focus is
/// in the find pane or the shell, where the terminal cursor sits elsewhere.
const CURRENT_LINE_NUMBER: Color = Color::Rgb(0xff, 0xff, 0xff);

// Syntax palette, shared by LSP semantic tokens ([`semantic_color`]) and the
// tree-sitter fallback ([`highlight_color`]). Both map the same token category
// to the same colour so a token never flickers when the LSP result arrives to
// replace (or is replaced by) the tree-sitter guess. Keywords, operators and
// numbers are deliberately three distinct hues — folding them together (e.g.
// `fn`, `%` and `2` all orange) makes a line hard to read at a glance.
const CODE_KEYWORD: Color = Color::Rgb(0xa0, 0x93, 0xc7); // purple: fn, if, let, return, mut
const CODE_OPERATOR: Color = Color::Rgb(0xe2, 0xa4, 0x78); // orange: %, ==, &, +
const CODE_FUNCTION: Color = Color::Rgb(0x84, 0xa0, 0xc6); // blue: solve, println, new
const CODE_TYPE: Color = Color::Rgb(0x89, 0xb8, 0xc2); // cyan: String, usize, bool
const CODE_STRING: Color = Color::Rgb(0xb4, 0xbe, 0x82); // green: "text", 'c'
const CODE_NUMBER: Color = Color::Rgb(0xd7, 0xb8, 0x7a); // amber: 0, 1, 2, true
const CODE_BUILTIN: Color = Color::Rgb(0xc6, 0x8b, 0xa5); // dusty rose: macros, lifetimes
const SELECTION: Color = Color::Rgb(0x27, 0x2c, 0x42);
/// A more prominent selection tint for list rows (pickers), where the subtle
/// editor selection colour is hard to spot.
const SELECTION_STRONG: Color = Color::Rgb(0x3c, 0x4a, 0x78);
const STATUS_BG: Color = Color::Rgb(0x0f, 0x11, 0x17);
const ADDED_BG: Color = Color::Rgb(0x24, 0x30, 0x25);
const REMOVED_BG: Color = Color::Rgb(0x38, 0x22, 0x28);
const CHANGED_BG: Color = Color::Rgb(0x38, 0x30, 0x22);
/// The part of a changed line that actually differs, tinted above the row's own
/// background so the eye lands on the edit rather than the whole line.
const WORD_ADDED_BG: Color = Color::Rgb(0x3f, 0x5c, 0x3f);
const WORD_REMOVED_BG: Color = Color::Rgb(0x63, 0x36, 0x40);
const POPUP_BG: Color = Color::Rgb(0x1e, 0x21, 0x32);
const OCCURRENCE_BG: Color = Color::Rgb(0x3d, 0x44, 0x60);
const MATCHING_BRACKET_BG: Color = Color::Rgb(0x4a, 0x50, 0x68);

pub fn draw(frame: &mut Frame<'_>, editor: &Editor) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());

    frame.render_widget(Block::default().style(Style::default().bg(BG)), areas[0]);
    if editor.search_pane_visible() {
        let (left, divider, right) = split_panes(areas[0]);
        if let Some(buffer) = editor.active_buffer() {
            draw_buffer(frame, left, &buffer, false);
            draw_status(frame, areas[1], editor, &buffer);
        } else if let Some(buffer) = editor.active_large_buffer() {
            draw_large_buffer(frame, left, &buffer);
            draw_status_text(
                frame,
                areas[1],
                editor.status().unwrap_or("READ ONLY · large file"),
            );
        } else {
            draw_status_text(frame, areas[1], editor.status().unwrap_or("Ready"));
        }
        draw_split_divider(frame, divider);
        if let Some(search) = editor.search_view() {
            draw_search_pane(frame, right, &search);
        }
    } else if editor.shell_visible() {
        let (left, divider, right) = split_panes(areas[0]);
        if let Some(buffer) = editor.active_buffer() {
            draw_buffer(frame, left, &buffer, !editor.shell_focused());
            draw_status(frame, areas[1], editor, &buffer);
        }
        if let Some(screen) = editor.terminal_screen() {
            draw_terminal(
                frame,
                right,
                screen,
                editor.shell_focused(),
                editor.terminal_selection_view(),
            );
        }
        draw_split_divider(frame, divider);
    } else if editor.show_start_page() {
        draw_start_page(frame, areas[0]);
        draw_status_text(frame, areas[1], editor.status().unwrap_or("Ready"));
    } else if let Some((left, right, true)) = editor.split_buffers() {
        let (left_area, divider, right_area) = split_panes(areas[0]);
        draw_diff(
            frame,
            left_area,
            right_area,
            &left,
            &right,
            DiffViewport {
                focused: editor.focused_side(),
                top_row: editor.diff_top_row(),
                hunks: editor.diff_hunk_position(),
            },
        );
        draw_split_divider(frame, divider);
        draw_status(
            frame,
            areas[1],
            editor,
            if editor.focused_side() == crate::editor::Side::Right {
                &right
            } else {
                &left
            },
        );
    } else if let Some((left, right, false)) = editor.split_buffers() {
        let (left_area, divider, right_area) = split_panes(areas[0]);
        let focused = editor.focused_side();
        draw_buffer(
            frame,
            left_area,
            &left,
            focused == crate::editor::Side::Left,
        );
        draw_buffer(
            frame,
            right_area,
            &right,
            focused == crate::editor::Side::Right,
        );
        draw_split_divider(frame, divider);
        draw_status(
            frame,
            areas[1],
            editor,
            if focused == crate::editor::Side::Right {
                &right
            } else {
                &left
            },
        );
    } else if let Some(buffer) = editor.active_buffer() {
        draw_buffer(frame, areas[0], &buffer, true);
        draw_status(frame, areas[1], editor, &buffer);
    } else if let Some(buffer) = editor.active_large_buffer() {
        draw_large_buffer(frame, areas[0], &buffer);
        draw_status_text(
            frame,
            areas[1],
            editor.status().unwrap_or("READ ONLY · large file"),
        );
    } else {
        frame.render_widget(
            Paragraph::new("my_editor").style(Style::default().fg(FG)),
            areas[0],
        );
        draw_status_text(frame, areas[1], editor.status().unwrap_or("No buffer"));
    }
    if let Some(picker) = editor.picker_view() {
        draw_picker(frame, &picker);
    }
    if let Some(completion) = editor.completion_view() {
        draw_completion(frame, editor, &completion);
    }
    if let Some(confirm) = editor.confirm_view() {
        draw_confirm(frame, confirm);
    }
    if let Some(hover) = editor.hover_view() {
        draw_hover(frame, editor, hover);
    }
    if let Some(help) = editor.signature_help_view() {
        draw_signature_help(frame, editor, &help);
    }
    // The rename prompt is modal, so it draws last — on top of any hover or
    // diagnostic popup that would otherwise sit over it.
    if let Some(rename) = editor.rename_view() {
        draw_rename(frame, rename);
    }
    if let Some(goto) = editor.goto_view() {
        draw_goto_line(frame, goto);
    }
}

fn split_panes(area: Rect) -> (Rect, Rect, Rect) {
    let left_width = area.width / 2;
    let divider_width = u16::from(area.width > 0);
    let right_width = area
        .width
        .saturating_sub(left_width.saturating_add(divider_width));
    let left = Rect::new(area.x, area.y, left_width, area.height);
    let divider = Rect::new(area.x + left_width, area.y, divider_width, area.height);
    let right = Rect::new(divider.x + divider_width, area.y, right_width, area.height);
    (left, divider, right)
}

fn draw_split_divider(frame: &mut Frame<'_>, area: Rect) {
    if area.width == 0 {
        return;
    }
    frame.render_widget(
        Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(MUTED).bg(BG)),
        area,
    );
}

fn draw_terminal(
    frame: &mut Frame<'_>,
    area: Rect,
    screen: &vt100::Screen,
    focused: bool,
    selection: Option<crate::editor::TerminalSelectionView>,
) {
    let (screen_rows, screen_cols) = screen.size();
    let rows = area.height.min(screen_rows);
    let cols = area.width.min(screen_cols);
    let lines = terminal_lines(screen, rows, cols, selection);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(FG).bg(Color::Rgb(0x12, 0x14, 0x1c))),
        area,
    );
    if focused && screen.scrollback() == 0 {
        let (row, col) = screen.cursor_position();
        if row < rows && col < cols {
            frame.set_cursor_position((area.x + col, area.y + row));
        }
    }
}

fn terminal_lines(
    screen: &vt100::Screen,
    rows: u16,
    cols: u16,
    selection: Option<crate::editor::TerminalSelectionView>,
) -> Vec<Line<'static>> {
    (0..rows)
        .map(|row| {
            let mut spans = Vec::new();
            let mut run = String::new();
            let mut run_style = None;
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut style = terminal_style(cell);
                if selection.is_some_and(|selection| {
                    (row, col) >= selection.start && (row, col) <= selection.end
                }) {
                    style = style.bg(SELECTION).add_modifier(Modifier::REVERSED);
                }
                if run_style.is_some_and(|current| current != style) {
                    spans.push(Span::styled(std::mem::take(&mut run), run_style.unwrap()));
                }
                run_style = Some(style);
                if cell.has_contents() {
                    run.push_str(&cell.contents());
                } else {
                    run.push(' ');
                }
            }
            if let Some(style) = run_style {
                spans.push(Span::styled(run, style));
            }
            Line::from(spans)
        })
        .collect()
}

fn terminal_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(terminal_color(cell.fgcolor(), FG))
        .bg(terminal_color(cell.bgcolor(), Color::Rgb(0x12, 0x14, 0x1c)));
    let mut modifiers = Modifier::empty();
    if cell.bold() {
        modifiers |= Modifier::BOLD;
    }
    if cell.italic() {
        modifiers |= Modifier::ITALIC;
    }
    if cell.underline() {
        modifiers |= Modifier::UNDERLINED;
    }
    if cell.inverse() {
        modifiers |= Modifier::REVERSED;
    }
    style = style.add_modifier(modifiers);
    style
}

fn terminal_color(color: vt100::Color, default: Color) -> Color {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(index) => Color::Indexed(index),
        vt100::Color::Rgb(red, green, blue) => Color::Rgb(red, green, blue),
    }
}

fn draw_popup_frame(frame: &mut Frame<'_>, area: Rect) -> Rect {
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(MUTED))
        .style(Style::default().bg(POPUP_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn overlay_area(frame: &Frame<'_>) -> Rect {
    let mut area = frame.area();
    area.height = area.height.saturating_sub(1);
    area
}

fn draw_confirm(frame: &mut Frame<'_>, message: &str) {
    let viewport = overlay_area(frame);
    let width = viewport.width.saturating_sub(4).clamp(1, 76);
    let area = Rect {
        x: viewport.x + (viewport.width.saturating_sub(width)) / 2,
        y: viewport.y + viewport.height.saturating_sub(3) / 2,
        width,
        height: 4.min(viewport.height),
    };
    let inner = draw_popup_frame(frame, area);
    frame.render_widget(
        Paragraph::new(message).style(Style::default().fg(Color::Yellow).bg(POPUP_BG)),
        inner,
    );
}

fn draw_hover(frame: &mut Frame<'_>, editor: &Editor, text: &str) {
    let viewport = overlay_area(frame);
    let line_count = text.lines().count().max(1) as u16;
    let area = hover_popup_area(
        viewport,
        line_count,
        editor.split_buffers().is_some(),
        editor.focused_side(),
    );
    let inner = draw_popup_frame(frame, area);
    frame.render_widget(
        Paragraph::new(highlighted_hover_lines(text)).style(Style::default().bg(POPUP_BG)),
        inner,
    );
}

fn hover_popup_area(
    viewport: Rect,
    line_count: u16,
    split: bool,
    focused: crate::editor::Side,
) -> Rect {
    let target = if split {
        let (left, _, right) = split_panes(viewport);
        if focused == crate::editor::Side::Right {
            left
        } else {
            right
        }
    } else {
        viewport
    };
    let width = target.width.saturating_sub(2).clamp(1, 64);
    let height = (line_count + 2).min(target.height.saturating_sub(1).max(1));
    Rect {
        x: if split {
            if focused == crate::editor::Side::Right {
                target.right().saturating_sub(width + 1).max(target.x)
            } else {
                (target.x + 1).min(target.right().saturating_sub(width))
            }
        } else {
            target.right().saturating_sub(width + 2)
        },
        y: target.y + 1,
        width,
        height,
    }
}

fn highlighted_hover_lines(text: &str) -> Vec<Line<'static>> {
    let syntax = hover_syntax_spans(text);
    let mut byte_base = 0;
    text.split('\n')
        .map(|line| {
            let diagnostic = line.starts_with("診断:");
            let mut spans = Vec::new();
            let mut run = String::new();
            let mut run_style = None;
            for (byte, character) in line.char_indices() {
                let kind = syntax
                    .iter()
                    .rev()
                    .find(|span| {
                        span.start_byte <= byte_base + byte && byte_base + byte < span.end_byte
                    })
                    .map(|span| span.kind.as_str());
                let style = if diagnostic {
                    Style::default()
                        .fg(Color::Rgb(0xe2, 0x78, 0x78))
                        .bg(POPUP_BG)
                } else {
                    Style::default().fg(highlight_color(kind)).bg(POPUP_BG)
                };
                if run_style.is_some_and(|current| current != style) {
                    spans.push(Span::styled(std::mem::take(&mut run), run_style.unwrap()));
                }
                run_style = Some(style);
                run.push(character);
            }
            if let Some(style) = run_style {
                spans.push(Span::styled(run, style));
            }
            byte_base += line.len() + 1;
            Line::from(spans)
        })
        .collect()
}

fn hover_syntax_spans(text: &str) -> Vec<crate::highlight::HighlightSpan> {
    let mut spans = crate::highlight::highlight("markdown", text);
    let mut offset = 0;
    let mut fence: Option<(String, usize, char)> = None;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        let marker = trimmed
            .strip_prefix("```")
            .map(|language| ('`', language))
            .or_else(|| trimmed.strip_prefix("~~~").map(|language| ('~', language)));
        match (&fence, marker) {
            (None, Some((delimiter, language))) => {
                fence = Some((
                    hover_fence_language(language),
                    offset + line.len(),
                    delimiter,
                ));
            }
            (Some((language, start, delimiter)), Some((closing, _))) if *delimiter == closing => {
                for mut span in crate::highlight::highlight(language, &text[*start..offset]) {
                    span.start_byte += start;
                    span.end_byte += start;
                    spans.push(span);
                }
                fence = None;
            }
            _ => {}
        }
        if fence.is_none() && looks_like_rust_hover_line(trimmed) {
            for mut span in crate::highlight::highlight("rust", trimmed) {
                let indentation = line.len() - line.trim_start().len();
                span.start_byte += offset + indentation;
                span.end_byte += offset + indentation;
                spans.push(span);
            }
        }
        offset += line.len();
    }
    if let Some((language, start, _)) = fence {
        for mut span in crate::highlight::highlight(&language, &text[start..]) {
            span.start_byte += start;
            span.end_byte += start;
            spans.push(span);
        }
    }
    spans
}

fn hover_fence_language(marker: &str) -> String {
    let language = marker
        .trim()
        .trim_matches(|character| matches!(character, '{' | '}' | '.'))
        .split([',', ' ', '\t'])
        .next()
        .unwrap_or_default();
    match language {
        "" | "rs" => "rust".to_owned(),
        language => language.to_owned(),
    }
}

fn looks_like_rust_hover_line(line: &str) -> bool {
    let starts_like_code = [
        "pub ", "fn ", "let ", "use ", "impl ", "trait ", "struct ", "enum ", "type ", "const ",
        "static ", "match ", "if ", "for ", "while ", "loop ", "return ", "Ok(", "Err(", "Some(",
        "None",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix));
    starts_like_code
        || line.starts_with("std::")
        || line.starts_with("crate::")
        || line.ends_with(";")
        || line == "}"
}

fn draw_rename(frame: &mut Frame<'_>, value: &str) {
    let viewport = overlay_area(frame);
    let width = viewport.width.saturating_sub(4).clamp(1, 56);
    let area = Rect {
        x: viewport.x + (viewport.width.saturating_sub(width)) / 2,
        y: viewport.y + 2,
        width,
        height: 3.min(viewport.height.saturating_sub(2)),
    };
    let inner = draw_popup_frame(frame, area);
    frame.render_widget(
        Paragraph::new(format!("Rename  {value}")).style(Style::default().fg(FG).bg(POPUP_BG)),
        inner,
    );
}

fn draw_goto_line(frame: &mut Frame<'_>, value: &str) {
    let viewport = overlay_area(frame);
    let width = viewport.width.saturating_sub(4).clamp(1, 56);
    let area = Rect {
        x: viewport.x + (viewport.width.saturating_sub(width)) / 2,
        y: viewport.y + 2,
        width,
        height: 3.min(viewport.height.saturating_sub(2)),
    };
    let inner = draw_popup_frame(frame, area);
    frame.render_widget(
        Paragraph::new(format!("Go to line  {value}")).style(Style::default().fg(FG).bg(POPUP_BG)),
        inner,
    );
}

fn draw_completion(
    frame: &mut Frame<'_>,
    editor: &Editor,
    completion: &crate::editor::CompletionView,
) {
    let viewport = overlay_area(frame);
    let Some(cursor) = completion_anchor_position(editor, viewport, completion.anchor) else {
        return;
    };
    let Some(area) = completion_popup_area(viewport, cursor, completion.items.len()) else {
        return;
    };
    let inner = draw_popup_frame(frame, area);
    let lines = completion
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            Line::styled(
                item,
                if index == completion.selected {
                    Style::default()
                        .fg(Color::Rgb(0x16, 0x18, 0x21))
                        .bg(Color::Rgb(0x84, 0xa0, 0xc6))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(FG).bg(POPUP_BG)
                },
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(POPUP_BG)),
        inner,
    );
}

fn draw_signature_help(
    frame: &mut Frame<'_>,
    editor: &Editor,
    help: &crate::editor::SignatureHelpView<'_>,
) {
    let viewport = overlay_area(frame);
    let Some(cursor) = completion_anchor_position(editor, viewport, help.anchor) else {
        return;
    };
    let Some(area) = completion_popup_area(viewport, cursor, 1) else {
        return;
    };
    let inner = draw_popup_frame(frame, area);
    // The label is one line; fold any stray newline so byte offsets (which index
    // the original string) stay valid — '\n' and ' ' are both one byte.
    let label = help.label.replace('\n', " ");
    let dim = Style::default().fg(MUTED).bg(POPUP_BG);
    let spans = match help.active_parameter {
        Some((start, end)) if start <= end && end <= label.len() => vec![
            Span::styled(label[..start].to_string(), dim),
            Span::styled(
                label[start..end].to_string(),
                Style::default()
                    .fg(FG)
                    .bg(POPUP_BG)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(label[end..].to_string(), dim),
        ],
        _ => vec![Span::styled(label, Style::default().fg(FG).bg(POPUP_BG))],
    };
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(POPUP_BG)),
        inner,
    );
}

fn completion_anchor_position(
    editor: &Editor,
    viewport: Rect,
    anchor: crate::position::CharIdx,
) -> Option<(u16, u16)> {
    let buffer = editor.active_buffer()?;
    let mut pane = viewport;
    if editor.is_split() {
        let (left, _, right) = split_panes(viewport);
        pane = if editor.focused_side() == crate::editor::Side::Right {
            right
        } else {
            left
        };
    }
    let gutter_width = (buffer.text.len_lines().max(1).to_string().len().max(2) + 3)
        .min(usize::from(pane.width.saturating_sub(1)));
    let text_width = usize::from(pane.width.saturating_sub(gutter_width as u16)).max(1);
    let cursor = char_idx_to_display_pos(buffer.text, anchor, buffer.tab_size);
    if cursor.line < buffer.view.scroll.top_line {
        return None;
    }
    let visual_row = (buffer.view.scroll.top_line..cursor.line)
        .map(|line| wrapped_line_rows(buffer.text, line, text_width, buffer.tab_size))
        .sum::<usize>()
        + cursor.col / text_width;
    if visual_row < buffer.view.scroll.wrapped_row_offset {
        return None;
    }
    let row = visual_row - buffer.view.scroll.wrapped_row_offset;
    if row >= usize::from(pane.height) {
        return None;
    }
    Some((
        pane.x + gutter_width as u16 + (cursor.col % text_width) as u16,
        pane.y + row as u16,
    ))
}

fn completion_popup_area(viewport: Rect, cursor: (u16, u16), item_count: usize) -> Option<Rect> {
    let width = viewport.width.saturating_sub(2).clamp(1, 48);
    let desired_height = (item_count as u16 + 2).max(1);
    let space_above = cursor.1.saturating_sub(viewport.y);
    let space_below = viewport.bottom().saturating_sub(cursor.1.saturating_add(1));
    let below = space_below >= space_above;
    let available = if below { space_below } else { space_above };
    if available == 0 {
        return None;
    }
    let height = desired_height.min(available);
    let x = cursor
        .0
        .min(viewport.right().saturating_sub(width))
        .max(viewport.x);
    let y = if below {
        cursor.1 + 1
    } else {
        cursor.1 - height
    };
    Some(Rect::new(x, y, width, height))
}

fn draw_search_pane(frame: &mut Frame<'_>, area: Rect, search: &crate::editor::SearchView) {
    use crate::editor::{
        SEARCH_SCOPE_LABELS, SEARCH_TOGGLE_LABELS, SearchFilterField, SearchScope,
        search_pane_layout,
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Block::default().style(Style::default().bg(POPUP_BG)), area);

    let directory = search.scope == SearchScope::Directory;
    let replace_enabled = search.replacement.is_some();
    let layout = search_pane_layout(directory, replace_enabled);
    let active_include = search.editing_filter == Some(SearchFilterField::Include);
    let active_exclude = search.editing_filter == Some(SearchFilterField::Exclude);
    let active_replace = search.editing_filter.is_none() && search.editing_replace;
    let active_query = !active_include && !active_exclude && !active_replace;
    let row_at = |offset: u16| area.y + offset;
    let visible = |offset: u16| row_at(offset) < area.bottom();
    fn line_at(frame: &mut Frame<'_>, area: Rect, offset: u16, line: Line<'static>) {
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(area.x + 1, area.y + offset, area.width.saturating_sub(1), 1),
        );
    }

    // Scope tabs — the active one is highlighted prominently.
    if visible(layout.scope_row) {
        let scopes = [
            SearchScope::CurrentBuffer,
            SearchScope::AllBuffers,
            SearchScope::Directory,
        ];
        let spans = SEARCH_SCOPE_LABELS
            .iter()
            .zip(scopes)
            .map(|(label, scope)| {
                Span::styled(
                    (*label).to_owned(),
                    if search.scope == scope {
                        Style::default()
                            .fg(BG)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(MUTED)
                    },
                )
            })
            .collect::<Vec<_>>();
        line_at(frame, area, layout.scope_row, Line::from(spans));
    }

    // Option toggles — labels are separated by two blanks; the gaps stay uncolored.
    if visible(layout.toggle_row) {
        let states = [
            search.options.case_sensitive,
            search.options.whole_word,
            search.options.regex,
        ];
        let mut spans = Vec::new();
        for (index, (label, on)) in SEARCH_TOGGLE_LABELS.iter().zip(states).enumerate() {
            if index > 0 {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::styled(
                (*label).to_owned(),
                if on {
                    Style::default().fg(BG).bg(Color::Yellow)
                } else {
                    Style::default().fg(MUTED)
                },
            ));
        }
        line_at(frame, area, layout.toggle_row, Line::from(spans));
    }

    draw_search_field(
        frame,
        area,
        layout.find_top,
        "Find",
        &search.query,
        active_query,
    );

    // Replace checkbox toggles the replacement field; the run button sits to its
    // right, and only while replace is enabled.
    if visible(layout.replace_checkbox_row) {
        let mark = if replace_enabled { "[x]" } else { "[ ]" };
        let mut spans = vec![Span::styled(
            format!("{mark} Replace"),
            Style::default().fg(if replace_enabled { FG } else { MUTED }),
        )];
        if replace_enabled {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                crate::editor::SEARCH_RUN_BUTTON,
                Style::default()
                    .fg(BG)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        line_at(frame, area, layout.replace_checkbox_row, Line::from(spans));
    }
    if let Some(top) = layout.replace_top {
        draw_search_field(
            frame,
            area,
            top,
            "Replace",
            search.replacement.as_deref().unwrap_or(""),
            active_replace,
        );
    }

    if let Some(top) = layout.include_top {
        draw_search_field(
            frame,
            area,
            top,
            "include (-name)",
            &search.include,
            active_include,
        );
    }
    if let Some(top) = layout.exclude_top {
        draw_search_field(
            frame,
            area,
            top,
            "exclude (-name)",
            &search.exclude,
            active_exclude,
        );
    }

    // Result list — scrollable, and each row opens its file on click.
    if visible(layout.results_top) {
        let results_area = Rect::new(
            area.x,
            row_at(layout.results_top),
            area.width,
            area.bottom() - row_at(layout.results_top),
        );
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(Style::default().fg(MUTED))
            .title(Span::styled(
                format!(" {} 件 ", search.total),
                Style::default().fg(MUTED),
            ));
        let inner = block.inner(results_area);
        frame.render_widget(block, results_area);
        let lines = search
            .items
            .iter()
            .skip(search.results_scroll)
            .take(usize::from(inner.height))
            .map(|item| Line::styled(item.clone(), Style::default().fg(FG)))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(lines).style(Style::default().bg(POPUP_BG)),
            inner,
        );
    }

    // Caret in the active input field.
    let field_top = if active_include {
        layout.include_top
    } else if active_exclude {
        layout.exclude_top
    } else if active_replace {
        layout.replace_top
    } else {
        Some(layout.find_top)
    };
    if let Some(top) = field_top {
        let cursor_x = area.x + 1 + search.field_cursor as u16;
        let cursor_y = row_at(top + 1);
        if cursor_x < area.right() && cursor_y < area.bottom() {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

fn draw_search_field(
    frame: &mut Frame<'_>,
    pane: Rect,
    top: u16,
    title: &str,
    value: &str,
    active: bool,
) {
    let y = pane.y + top;
    if y >= pane.bottom() {
        return;
    }
    let area = Rect::new(pane.x, y, pane.width, 3.min(pane.bottom() - y));
    let color = if active { Color::Yellow } else { MUTED };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(color),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(value.to_owned()).style(Style::default().fg(FG).bg(POPUP_BG)),
        inner,
    );
}

fn draw_start_page(frame: &mut Frame<'_>, area: Rect) {
    let lines = vec![
        Line::styled("my_editor", Style::default().fg(FG)),
        Line::from(""),
        Line::styled("Ctrl+T  ファイルを開く", Style::default().fg(MUTED)),
        Line::styled("Ctrl+P  コマンドを検索", Style::default().fg(MUTED)),
        Line::styled("F4      終了", Style::default().fg(MUTED)),
    ];
    let y = area.y + area.height.saturating_sub(lines.len() as u16) / 3;
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(ratatui::layout::Alignment::Center)
            .style(Style::default().bg(BG)),
        Rect { y, ..area },
    );
}

fn draw_large_buffer(frame: &mut Frame<'_>, area: Rect, buffer: &crate::editor::LargeBuffer<'_>) {
    let start = buffer.view.scroll.top_line;
    buffer
        .file
        .ensure_line(start + usize::from(area.height) + 1);
    let mut lines = Vec::with_capacity(usize::from(area.height));
    for line_index in start..start + usize::from(area.height) {
        let Some(text) = buffer.file.line(line_index) else {
            break;
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:>6} ", line_index + 1),
                Style::default().fg(MUTED).bg(BG),
            ),
            Span::styled(
                text,
                Style::default().fg(FG).bg(
                    if line_index == buffer.view.selections.primary().head.0 {
                        SELECTION
                    } else {
                        BG
                    },
                ),
            ),
        ]));
    }
    frame.render_widget(Paragraph::new(lines).style(Style::default().bg(BG)), area);
    let cursor_line = buffer.view.selections.primary().head.0;
    if cursor_line >= start && cursor_line < start + usize::from(area.height) {
        frame.set_cursor_position((
            area.x + 7.min(area.width.saturating_sub(1)),
            area.y + (cursor_line - start) as u16,
        ));
    }
}

/// Which count a click on an edge badge landed on. `Any` is the arrow itself,
/// which navigates to the nearest change of any kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeBadgeCategory {
    Error,
    Warning,
    Modified,
    Added,
    Any,
}

/// A click on an edge badge: whether it was the top (`above`) or bottom badge,
/// and which count segment the click landed on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EdgeBadgeHit {
    pub above: bool,
    pub category: EdgeBadgeCategory,
}

/// The count segments an edge badge draws, in render order. Shared by the
/// renderer and the hit-test so their geometry can never drift apart.
fn edge_badge_parts(
    errors: usize,
    warnings: usize,
    modified: usize,
    added: usize,
) -> Vec<(String, Color, EdgeBadgeCategory)> {
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push((
            format!("E{errors}"),
            Color::Rgb(0xe2, 0x78, 0x78),
            EdgeBadgeCategory::Error,
        ));
    }
    if warnings > 0 {
        parts.push((
            format!("W{warnings}"),
            Color::Rgb(0xe2, 0xa4, 0x78),
            EdgeBadgeCategory::Warning,
        ));
    }
    if modified > 0 {
        parts.push((
            format!("M{modified}"),
            Color::Rgb(0x84, 0xa0, 0xc6),
            EdgeBadgeCategory::Modified,
        ));
    }
    if added > 0 {
        parts.push((
            format!("A{added}"),
            Color::Rgb(0xb4, 0xbe, 0x82),
            EdgeBadgeCategory::Added,
        ));
    }
    parts
}

/// The badge's rectangle: right-aligned, one row tall, pinned to the top of the
/// pane when `above` and the bottom otherwise.
fn edge_badge_rect(area: Rect, above: bool, parts_width: usize) -> Rect {
    let width = (parts_width).min(usize::from(area.width)) as u16;
    Rect {
        x: area.right().saturating_sub(width),
        y: if above { area.y } else { area.bottom() - 1 },
        width,
        height: 1,
    }
}

fn draw_edge_badge(
    frame: &mut Frame<'_>,
    area: Rect,
    above: bool,
    errors: usize,
    warnings: usize,
    modified: usize,
    added: usize,
) {
    if errors == 0 && warnings == 0 && modified == 0 && added == 0
        || area.width == 0
        || area.height == 0
    {
        return;
    }
    let arrow = if above { '↑' } else { '↓' };
    let parts = edge_badge_parts(errors, warnings, modified, added);
    let parts_width = 1 + parts
        .iter()
        .map(|(text, _, _)| 1 + text.chars().count())
        .sum::<usize>();
    let badge = edge_badge_rect(area, above, parts_width);
    let mut spans = vec![Span::styled(
        arrow.to_string(),
        Style::default().fg(Color::Yellow).bg(POPUP_BG),
    )];
    for (text, color, _) in parts {
        spans.push(Span::styled(" ", Style::default().bg(POPUP_BG)));
        spans.push(Span::styled(text, Style::default().fg(color).bg(POPUP_BG)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), badge);
}

/// Which count segment of an edge badge the click at `(column, row)` landed on,
/// or `None` for anywhere else. Takes the pane geometry as plain columns so the
/// editor can hit-test without depending on ratatui's `Rect`, mirroring the
/// geometry `draw_edge_badge` uses exactly.
#[allow(clippy::too_many_arguments)]
pub fn edge_badge_hit(
    pane_x: u16,
    pane_width: u16,
    pane_height: u16,
    above: bool,
    errors: usize,
    warnings: usize,
    modified: usize,
    added: usize,
    column: u16,
    row: u16,
) -> Option<EdgeBadgeHit> {
    if errors == 0 && warnings == 0 && modified == 0 && added == 0
        || pane_width == 0
        || pane_height == 0
    {
        return None;
    }
    let parts = edge_badge_parts(errors, warnings, modified, added);
    let parts_width = 1 + parts
        .iter()
        .map(|(text, _, _)| 1 + text.chars().count())
        .sum::<usize>();
    let badge = edge_badge_rect(
        Rect::new(pane_x, 0, pane_width, pane_height),
        above,
        parts_width,
    );
    if row != badge.y || column < badge.x || column >= badge.x + badge.width {
        return None;
    }
    if column == badge.x {
        return Some(EdgeBadgeHit {
            above,
            category: EdgeBadgeCategory::Any,
        });
    }
    let mut cursor = badge.x + 1;
    for (text, _, category) in &parts {
        let segment = 1 + text.chars().count() as u16;
        if column >= cursor && column < cursor + segment {
            return Some(EdgeBadgeHit {
                above,
                category: *category,
            });
        }
        cursor += segment;
    }
    None
}

fn draw_picker(frame: &mut Frame<'_>, picker: &crate::editor::PickerView) {
    let viewport = overlay_area(frame);
    let width = viewport.width.saturating_sub(4).clamp(1, 70);
    let available_height = viewport.height.saturating_sub(1).max(1);
    let ellipsis_rows = u16::from(picker.has_before) + u16::from(picker.has_after);
    let height = (picker.items.len() as u16 + 3 + ellipsis_rows)
        .min(available_height)
        .max(1);
    let area = Rect {
        x: viewport.x + (viewport.width.saturating_sub(width)) / 2,
        y: viewport.y + 1,
        width,
        height,
    };
    let inner = draw_popup_frame(frame, area);
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{}  ", picker.title), Style::default().fg(MUTED)),
        Span::styled(&picker.query, Style::default().fg(FG)),
    ])];
    if picker.has_before {
        lines.push(Line::styled("…", Style::default().fg(MUTED).bg(POPUP_BG)));
    }
    for (index, item) in picker.items.iter().enumerate() {
        lines.push(picker_item_line(
            item,
            index == picker.selected,
            inner.width,
        ));
    }
    if picker.has_after {
        lines.push(Line::styled(
            format!("…  {} candidates", picker.total),
            Style::default().fg(MUTED).bg(POPUP_BG),
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(POPUP_BG)),
        inner,
    );
}

fn picker_item_line(
    item: &crate::editor::PickerViewItem,
    selected: bool,
    width: u16,
) -> Line<'static> {
    let background = if selected { SELECTION_STRONG } else { POPUP_BG };
    let mut spans = item
        .label
        .chars()
        .enumerate()
        .map(|(index, character)| {
            let matched = item.matched.contains(&index);
            let style = Style::default()
                .fg(if matched {
                    Color::Yellow
                } else if selected {
                    Color::White
                } else {
                    FG
                })
                .bg(background)
                .add_modifier(if matched || selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            Span::styled(character.to_string(), style)
        })
        .collect::<Vec<_>>();
    // Extend the highlight across the whole row so the selection is easy to spot.
    if selected {
        let used = item.label.chars().count() as u16;
        if used < width {
            spans.push(Span::styled(
                " ".repeat(usize::from(width - used)),
                Style::default().bg(background),
            ));
        }
    }
    Line::from(spans)
}

/// Where the diff view is looking: which side has the caret, the first visible
/// aligned row, and `(current, total)` for the hunk navigator.
struct DiffViewport {
    focused: crate::editor::Side,
    top_row: usize,
    hunks: Option<(usize, usize)>,
}

fn draw_diff(
    frame: &mut Frame<'_>,
    left_area: Rect,
    right_area: Rect,
    left: &ActiveBuffer<'_>,
    right: &ActiveBuffer<'_>,
    viewport: DiffViewport,
) {
    let DiffViewport {
        focused,
        top_row,
        hunks,
    } = viewport;
    let left_lines = rope_lines(left.text);
    let right_lines = rope_lines(right.text);
    let rows = aligned(&left_lines, &right_lines);
    let height = usize::from(left_area.height.min(right_area.height));
    let focused_line = match focused {
        crate::editor::Side::Left => {
            char_idx_to_display_pos(
                left.text,
                left.view.selections.primary().head,
                left.tab_size,
            )
            .line
        }
        crate::editor::Side::Right => {
            char_idx_to_display_pos(
                right.text,
                right.view.selections.primary().head,
                right.tab_size,
            )
            .line
        }
    };
    let cursor_row = rows.iter().position(|row| match focused {
        crate::editor::Side::Left => row
            .left
            .as_ref()
            .is_some_and(|(line, _)| *line == focused_line),
        crate::editor::Side::Right => row
            .right
            .as_ref()
            .is_some_and(|(line, _)| *line == focused_line),
    });
    // The view scrolls by aligned row. `top_row` is an upper-bounded guess made
    // without the alignment (see `Editor::scroll_diff`), so the last screenful
    // is clamped here, where the real row count is known.
    let start = top_row.min(rows.len().saturating_sub(height));
    let mut rendered_left = Vec::new();
    let mut rendered_right = Vec::new();
    for row in rows.iter().skip(start).take(height) {
        let (background, marker) = match row.kind {
            DiffKind::Equal => (BG, " "),
            DiffKind::Added => (ADDED_BG, "+"),
            DiffKind::Removed => (REMOVED_BG, "-"),
            DiffKind::Changed => (CHANGED_BG, "~"),
        };
        // Only a changed row has both sides to compare within; added and
        // removed rows have no counterpart, so the whole line is the change.
        let (left_words, right_words) = match (row.kind, &row.left, &row.right) {
            (DiffKind::Changed, Some((_, before)), Some((_, after))) => {
                let (before, after) = word_segments(before, after);
                (Some(before), Some(after))
            }
            _ => (None, None),
        };
        rendered_left.push(diff_line(
            row.left.clone(),
            marker,
            background,
            left_words,
            WORD_REMOVED_BG,
        ));
        rendered_right.push(diff_line(
            row.right.clone(),
            marker,
            background,
            right_words,
            WORD_ADDED_BG,
        ));
    }
    frame.render_widget(
        Paragraph::new(rendered_left).style(Style::default().fg(FG).bg(BG)),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(rendered_right).style(Style::default().fg(FG).bg(BG)),
        right_area,
    );
    if let Some((current, total)) = hunks {
        draw_diff_navigator(frame, right_area, current, total);
    }
    if let Some(row) = cursor_row.filter(|row| *row >= start && *row < start + height) {
        let (area, buffer) = match focused {
            crate::editor::Side::Left => (left_area, left),
            crate::editor::Side::Right => (right_area, right),
        };
        let position = char_idx_to_display_pos(
            buffer.text,
            buffer.view.selections.primary().head,
            buffer.tab_size,
        );
        let column = usize::from(crate::diff::GUTTER_WIDTH).saturating_add(position.col);
        if column < usize::from(area.width) {
            frame.set_cursor_position((area.x + column as u16, area.y + (row - start) as u16));
        }
    }
}

/// `▲ ▼ 3/12` pinned to the top-right of the diff pane. The arrows are click
/// targets; [`diff_navigator_hit`] maps a click back to a direction using the
/// same geometry.
fn draw_diff_navigator(frame: &mut Frame<'_>, pane: Rect, current: usize, total: usize) {
    let width = diff_navigator_label_width(current, total);
    if pane.width <= width || pane.height == 0 {
        return;
    }
    let area = Rect::new(pane.right() - width, pane.y, width, 1);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                DIFF_NAV_PREV,
                Style::default().fg(if total == 0 { MUTED } else { FG }),
            ),
            Span::styled(
                DIFF_NAV_NEXT,
                Style::default().fg(if total == 0 { MUTED } else { FG }),
            ),
            Span::styled(format!(" {current}/{total} "), Style::default().fg(MUTED)),
        ]))
        .style(Style::default().bg(POPUP_BG)),
        area,
    );
}

const DIFF_NAV_PREV: &str = " ▲ ";
const DIFF_NAV_NEXT: &str = " ▼ ";

fn diff_navigator_label(current: usize, total: usize) -> String {
    format!("{DIFF_NAV_PREV}{DIFF_NAV_NEXT} {current}/{total} ")
}

pub fn diff_navigator_label_width(current: usize, total: usize) -> u16 {
    diff_navigator_label(current, total).chars().count() as u16
}

/// Which arrow a click landed on: `Some(true)` for next, `Some(false)` for
/// previous, `None` for anywhere else. Takes the pane geometry as plain columns
/// so the editor can hit-test without depending on ratatui's `Rect`.
pub fn diff_navigator_hit(
    pane_x: u16,
    pane_width: u16,
    current: usize,
    total: usize,
    column: u16,
    row: u16,
) -> Option<bool> {
    let width = diff_navigator_label_width(current, total);
    if pane_width <= width || row != 0 {
        return None;
    }
    let x = pane_x + pane_width - width;
    let prev_width = DIFF_NAV_PREV.chars().count() as u16;
    let next_width = DIFF_NAV_NEXT.chars().count() as u16;
    if column >= x && column < x + prev_width {
        return Some(false);
    }
    if column >= x + prev_width && column < x + prev_width + next_width {
        return Some(true);
    }
    None
}

/// One rendered diff row. `words` splits the text so the differing runs can be
/// tinted with `word_background`; without it the line is drawn in one piece.
fn diff_line(
    line: Option<(usize, String)>,
    marker: &str,
    background: Color,
    words: Option<Vec<Segment>>,
    word_background: Color,
) -> Line<'static> {
    let Some((number, text)) = line else {
        return Line::styled(
            format!(
                "{marker}{:width$}",
                "",
                width = crate::diff::LINE_NUMBER_WIDTH + 1
            ),
            Style::default().fg(MUTED).bg(background),
        );
    };
    let gutter = Span::styled(
        format!(
            "{marker}{:>width$} ",
            number + 1,
            width = crate::diff::LINE_NUMBER_WIDTH
        ),
        Style::default().fg(FG).bg(background),
    );
    let Some(words) = words else {
        return Line::from(vec![
            gutter,
            Span::styled(text, Style::default().fg(FG).bg(background)),
        ]);
    };
    let mut spans = vec![gutter];
    spans.extend(words.into_iter().map(|segment| {
        Span::styled(
            segment.text,
            Style::default().fg(FG).bg(if segment.changed {
                word_background
            } else {
                background
            }),
        )
    }));
    Line::from(spans)
}

fn draw_buffer(frame: &mut Frame<'_>, area: Rect, buffer: &ActiveBuffer<'_>, focused: bool) {
    let digits = buffer.text.len_lines().max(1).to_string().len().max(2);
    let gutter_width = (digits + 3).min(usize::from(area.width.saturating_sub(1)));
    let gutter_area = Rect::new(area.x, area.y, gutter_width as u16, area.height);
    let text_area = Rect::new(
        area.x + gutter_width as u16,
        area.y,
        area.width.saturating_sub(gutter_width as u16),
        area.height,
    );
    let text_width = usize::from(text_area.width).max(1);
    let start = buffer.view.scroll.top_line;
    let end = (start + usize::from(area.height) + buffer.view.scroll.wrapped_row_offset)
        .min(buffer.text.len_lines());
    let selections: Vec<_> = buffer
        .view
        .selections
        .iter()
        .map(|selection| selection.range())
        .collect();
    let occurrence_ranges = visible_occurrence_ranges(
        buffer.text,
        buffer.view.selections.primary().head,
        start,
        end,
    );
    let matching_brackets =
        matching_bracket_indices(buffer.text, buffer.view.selections.primary().head.0);
    let caret_line = buffer.text.char_to_line(
        buffer
            .view
            .selections
            .primary()
            .head
            .0
            .min(buffer.text.len_chars()),
    );
    // Diagnostics already carry char-index ranges kept in sync with edits, so no
    // per-frame position conversion is needed.
    let diagnostic_ranges = buffer
        .diagnostics
        .iter()
        .map(|diagnostic| {
            (
                diagnostic.range.start.0..diagnostic.range.end.0,
                diagnostic.severity,
            )
        })
        .collect::<Vec<_>>();
    let mut gutter_lines = Vec::with_capacity(end.saturating_sub(start));
    let mut text_lines = Vec::with_capacity(end.saturating_sub(start));

    for line_index in start..end {
        let line_diagnostic = buffer
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic_line(buffer.text, diagnostic) == line_index)
            .min_by_key(|diagnostic| match diagnostic.severity {
                crate::lsp::DiagnosticSeverity::Error => 0,
                crate::lsp::DiagnosticSeverity::Warning => 1,
                crate::lsp::DiagnosticSeverity::Information => 2,
                crate::lsp::DiagnosticSeverity::Hint => 3,
            });
        let severity = line_diagnostic.map(|diagnostic| diagnostic.severity);
        let (marker, marker_color) = match severity {
            Some(crate::lsp::DiagnosticSeverity::Error) => ("×", Color::Rgb(0xe2, 0x78, 0x78)),
            Some(crate::lsp::DiagnosticSeverity::Warning) => ("▵", Color::Rgb(0xe2, 0xa4, 0x78)),
            Some(crate::lsp::DiagnosticSeverity::Information) => {
                ("i", Color::Rgb(0x89, 0xb8, 0xc2))
            }
            Some(crate::lsp::DiagnosticSeverity::Hint) => ("·", Color::Rgb(0x84, 0xa0, 0xc6)),
            _ => (" ", MUTED),
        };
        let (git_marker, git_color) = buffer
            .git_lines
            .iter()
            .find(|git| git.line == line_index)
            .map_or((" ", MUTED), |git| git_gutter_style(git.kind));
        let gutter_spans = vec![
            Span::styled(git_marker, Style::default().fg(git_color).bg(BG)),
            Span::styled(marker, Style::default().fg(marker_color).bg(BG)),
            Span::styled(
                format!("{:>digits$} ", line_index + 1),
                Style::default()
                    .fg(if line_index == caret_line {
                        CURRENT_LINE_NUMBER
                    } else {
                        MUTED
                    })
                    .bg(BG),
            ),
        ];
        // Wrap the line into fixed-width visual rows ourselves so the rendered
        // layout matches the caret/gutter/scroll math, which all assume a hard
        // wrap at exactly `text_width` display columns. Relying on ratatui's
        // word wrapping instead would drift whenever a wrapped line contains
        // spaces (e.g. tab-expanded indentation).
        let mut rows: Vec<Vec<Span>> = vec![Vec::new()];
        let line = buffer.text.line(line_index);
        let line_start = buffer.text.line_to_char(line_index);
        let mut display_col = 0;
        for (char_col, character) in line.chars().enumerate() {
            if matches!(character, '\r' | '\n') {
                break;
            }
            let next_col = display_col_after(display_col, character, buffer.tab_size);
            let index = line_start + char_col;
            let byte = buffer.text.char_to_byte(index);
            let style = if selections
                .iter()
                .any(|selection| selection.contains(&index))
            {
                Style::default().fg(FG).bg(SELECTION)
            } else {
                let semantic = buffer
                    .semantic_spans
                    .iter()
                    .find(|span| span.start.0 <= index && index < span.end.0)
                    .map(|span| span.token_kind.as_str());
                let mut style = Style::default()
                    .fg(semantic.map_or_else(
                        || {
                            highlight_color(
                                buffer
                                    .syntax_spans
                                    .iter()
                                    .rev()
                                    .find(|span| span.start_byte <= byte && byte < span.end_byte)
                                    .map(|span| span.kind.as_str()),
                            )
                        },
                        semantic_color,
                    ))
                    .bg(
                        if occurrence_ranges
                            .iter()
                            .any(|occurrence| occurrence.contains(&index))
                        {
                            OCCURRENCE_BG
                        } else {
                            BG
                        },
                    );
                if matching_brackets.contains(&index) {
                    style = style.bg(MATCHING_BRACKET_BG).add_modifier(Modifier::BOLD);
                }
                if let Some((_, severity)) = diagnostic_ranges
                    .iter()
                    .find(|(range, _)| range.contains(&index))
                {
                    style = style
                        .underline_color(diagnostic_color(*severity))
                        .add_modifier(Modifier::UNDERLINED);
                }
                style
            };
            if character == '\t' {
                for column in display_col..next_col {
                    push_cell(&mut rows, column / text_width, " ".to_owned(), style);
                }
            } else {
                // Keep a wide glyph whole: if it would straddle a wrap boundary,
                // place it on the row where it finishes.
                let target_row = next_col.saturating_sub(1) / text_width;
                push_cell(&mut rows, target_row, character.to_string(), style);
            }
            display_col = next_col;
        }
        if let Some(diagnostic) = line_diagnostic {
            let used = display_col % text_width;
            let available = if display_col > 0 && used == 0 {
                0
            } else {
                text_width - used
            };
            let text = truncate_virtual_diagnostic(&diagnostic.message, available);
            if !text.is_empty() {
                let color = diagnostic_color(diagnostic.severity);
                if let Some(last) = rows.last_mut() {
                    last.push(Span::styled(
                        text,
                        Style::default()
                            .fg(color)
                            .bg(BG)
                            .add_modifier(Modifier::ITALIC),
                    ));
                }
            }
        }
        // Keep the visual row count aligned with the caret/gutter math.
        let row_count = display_col.max(1).div_ceil(text_width);
        while rows.len() < row_count {
            rows.push(Vec::new());
        }
        gutter_lines.push(Line::from(gutter_spans));
        gutter_lines
            .extend(std::iter::repeat_with(Line::default).take(rows.len().saturating_sub(1)));
        text_lines.extend(rows.into_iter().map(Line::from));
    }
    frame.render_widget(
        Paragraph::new(gutter_lines)
            .style(Style::default().bg(BG))
            .scroll((
                buffer.view.scroll.wrapped_row_offset.min(u16::MAX as usize) as u16,
                0,
            )),
        gutter_area,
    );
    frame.render_widget(
        Paragraph::new(text_lines)
            .style(Style::default().bg(BG))
            .scroll((
                buffer.view.scroll.wrapped_row_offset.min(u16::MAX as usize) as u16,
                0,
            )),
        text_area,
    );

    let scroll_offset = buffer.view.scroll.wrapped_row_offset;
    for (_, selection) in buffer
        .view
        .selections
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != buffer.view.selections.primary_index())
    {
        let caret = char_idx_to_display_pos(buffer.text, selection.head, buffer.tab_size);
        if caret.line < start {
            continue;
        }
        let rows_before = (start..caret.line)
            .map(|line| wrapped_line_rows(buffer.text, line, text_width, buffer.tab_size))
            .sum::<usize>();
        let absolute_row = rows_before + caret.col / text_width;
        if absolute_row < scroll_offset {
            continue;
        }
        let row = absolute_row - scroll_offset;
        let col = caret.col % text_width;
        if row < usize::from(text_area.height)
            && col < usize::from(text_area.width)
            && let Some(cell) = frame
                .buffer_mut()
                .cell_mut((text_area.x + col as u16, text_area.y + row as u16))
        {
            let symbol = format!("{}\u{20d2}", cell.symbol());
            cell.set_symbol(&symbol).set_fg(FG);
        }
    }

    let above_errors = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic_line(buffer.text, diagnostic) < start
                && diagnostic.severity == crate::lsp::DiagnosticSeverity::Error
        })
        .count();
    let above_warnings = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic_line(buffer.text, diagnostic) < start
                && diagnostic.severity == crate::lsp::DiagnosticSeverity::Warning
        })
        .count();
    let below_errors = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic_line(buffer.text, diagnostic) >= end
                && diagnostic.severity == crate::lsp::DiagnosticSeverity::Error
        })
        .count();
    let below_warnings = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic_line(buffer.text, diagnostic) >= end
                && diagnostic.severity == crate::lsp::DiagnosticSeverity::Warning
        })
        .count();
    let above_modified = buffer
        .git_lines
        .iter()
        .filter(|git| git.line < start && git.kind == crate::editor::GitLineKind::Modified)
        .count();
    let above_added = buffer
        .git_lines
        .iter()
        .filter(|git| git.line < start && git.kind == crate::editor::GitLineKind::Added)
        .count();
    let below_modified = buffer
        .git_lines
        .iter()
        .filter(|git| git.line >= end && git.kind == crate::editor::GitLineKind::Modified)
        .count();
    let below_added = buffer
        .git_lines
        .iter()
        .filter(|git| git.line >= end && git.kind == crate::editor::GitLineKind::Added)
        .count();
    draw_edge_badge(
        frame,
        area,
        true,
        above_errors,
        above_warnings,
        above_modified,
        above_added,
    );
    draw_edge_badge(
        frame,
        area,
        false,
        below_errors,
        below_warnings,
        below_modified,
        below_added,
    );

    let cursor = char_idx_to_display_pos(
        buffer.text,
        buffer.view.selections.primary().head,
        buffer.tab_size,
    );
    let rows_before = (start..cursor.line)
        .map(|line| wrapped_line_rows(buffer.text, line, text_width, buffer.tab_size))
        .sum::<usize>();
    let cursor_row = rows_before + cursor.col / text_width;
    let cursor_row = cursor_row.saturating_sub(buffer.view.scroll.wrapped_row_offset);
    let cursor_col = cursor.col % text_width;
    if focused
        && cursor_row < usize::from(text_area.height)
        && cursor_col < usize::from(text_area.width)
    {
        frame.set_cursor_position((
            text_area.x + cursor_col as u16,
            text_area.y + cursor_row as u16,
        ));
    }
}

fn truncate_virtual_diagnostic(message: &str, available: usize) -> String {
    const PREFIX: &str = "    ● ";
    const PREFIX_WIDTH: usize = 6;
    if available <= PREFIX_WIDTH {
        return String::new();
    }
    let mut text = PREFIX.to_owned();
    text.extend(message.chars().take(available - PREFIX_WIDTH));
    text
}

/// The line a diagnostic starts on, derived from its (edit-tracked) char range.
fn diagnostic_line(text: &ropey::Rope, diagnostic: &crate::document::ActiveDiagnostic) -> usize {
    text.char_to_line(diagnostic.range.start.0.min(text.len_chars()))
}

fn diagnostic_color(severity: crate::lsp::DiagnosticSeverity) -> Color {
    match severity {
        crate::lsp::DiagnosticSeverity::Error => Color::Rgb(0xe2, 0x78, 0x78),
        crate::lsp::DiagnosticSeverity::Warning => Color::Rgb(0xe2, 0xa4, 0x78),
        crate::lsp::DiagnosticSeverity::Information => Color::Rgb(0x89, 0xb8, 0xc2),
        crate::lsp::DiagnosticSeverity::Hint => Color::Rgb(0x84, 0xa0, 0xc6),
    }
}

fn visible_occurrence_ranges(
    text: &ropey::Rope,
    cursor: crate::position::CharIdx,
    first_line: usize,
    end_line: usize,
) -> Vec<std::ops::Range<usize>> {
    let len = text.len_chars();
    let cursor = cursor.0.min(len);
    let adjacent = if cursor < len && is_word(text.char(cursor)) {
        Some(cursor)
    } else if cursor > 0 && is_word(text.char(cursor - 1)) {
        Some(cursor - 1)
    } else {
        None
    };
    let Some(adjacent) = adjacent else {
        return Vec::new();
    };
    let mut word_start = adjacent;
    while word_start > 0 && is_word(text.char(word_start - 1)) {
        word_start -= 1;
    }
    let mut word_end = adjacent + 1;
    while word_end < len && is_word(text.char(word_end)) {
        word_end += 1;
    }
    let word = text.slice(word_start..word_end).to_string();
    let mut ranges = Vec::new();
    for line in first_line..end_line.min(text.len_lines()) {
        let mut index = text.line_to_char(line);
        let line_end = if line + 1 < text.len_lines() {
            text.line_to_char(line + 1)
        } else {
            len
        };
        while index < line_end {
            if !is_word(text.char(index)) {
                index += 1;
                continue;
            }
            let start = index;
            while index < line_end && is_word(text.char(index)) {
                index += 1;
            }
            if text.slice(start..index) == word.as_str() {
                ranges.push(start..index);
            }
        }
    }
    ranges
}

fn matching_bracket_indices(text: &ropey::Rope, cursor: usize) -> Vec<usize> {
    let len = text.len_chars();
    let candidate = [cursor.min(len), cursor.saturating_sub(1)]
        .into_iter()
        .find(|index| {
            *index < len && matches!(text.char(*index), '(' | ')' | '[' | ']' | '{' | '}')
        });
    let Some(index) = candidate else {
        return Vec::new();
    };
    let bracket = text.char(index);
    let (opening, closing, forward) = match bracket {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        _ => return Vec::new(),
    };
    let mut depth = 0usize;
    if forward {
        for other in index + 1..len {
            match text.char(other) {
                character if character == opening => depth += 1,
                character if character == closing && depth == 0 => return vec![index, other],
                character if character == closing => depth -= 1,
                _ => {}
            }
        }
    } else {
        for other in (0..index).rev() {
            match text.char(other) {
                character if character == closing => depth += 1,
                character if character == opening && depth == 0 => return vec![other, index],
                character if character == opening => depth -= 1,
                _ => {}
            }
        }
    }
    Vec::new()
}

fn push_cell(rows: &mut Vec<Vec<Span<'static>>>, row: usize, text: String, style: Style) {
    while rows.len() <= row {
        rows.push(Vec::new());
    }
    rows[row].push(Span::styled(text, style));
}

fn wrapped_line_rows(text: &ropey::Rope, line: usize, width: usize, tab_size: usize) -> usize {
    let display_width = text
        .line(line.min(text.len_lines().saturating_sub(1)))
        .chars()
        .take_while(|character| !matches!(character, '\r' | '\n'))
        .fold(0, |column, character| {
            display_col_after(column, character, tab_size)
        });
    display_width.max(1).div_ceil(width.max(1))
}

fn git_gutter_style(kind: crate::editor::GitLineKind) -> (&'static str, Color) {
    match kind {
        crate::editor::GitLineKind::Added => ("▌", Color::Rgb(0xb4, 0xbe, 0x82)),
        crate::editor::GitLineKind::Modified => ("▌", Color::Rgb(0x84, 0xa0, 0xc6)),
        crate::editor::GitLineKind::Deleted => (" ", MUTED),
    }
}

fn semantic_color(token_kind: &str) -> Color {
    match token_kind {
        // Every type-category token rust-analyzer may emit. `builtinType` in
        // particular is what it reports for `usize`, `u8`, `bool`, `str`, … —
        // omitting it left builtin types uncoloured (falling through to FG) once
        // the semantic result replaced the tree-sitter guess, so a signature's
        // `String` stayed cyan while its `usize`s reverted to plain foreground.
        "namespace" | "type" | "class" | "enum" | "interface" | "struct" | "typeParameter"
        | "property" | "enumMember" | "builtinType" | "typeAlias" | "union" | "selfTypeKeyword" => {
            CODE_TYPE
        }
        "function" | "method" => CODE_FUNCTION,
        // Macros and lifetimes get their own accent so `println!`/`vec!` and
        // `'a` don't read as ordinary calls or punctuation.
        "macro" | "lifetime" | "label" => CODE_BUILTIN,
        "keyword" | "modifier" | "selfKeyword" => CODE_KEYWORD,
        // rust-analyzer emits the plain `operator`, but also finer arithmetic/
        // bitwise/comparison/logical variants when negotiated; colour them all
        // as operators so `%`, `==`, `&` stand apart from keywords and numbers.
        "operator" | "arithmetic" | "bitwise" | "comparison" | "logical" => CODE_OPERATOR,
        "string" | "regexp" | "character" | "escapeSequence" => CODE_STRING,
        "number" | "boolean" => CODE_NUMBER,
        "comment" => MUTED,
        "parameter" | "variable" | "const" | "static" | "constParameter" => FG,
        _ => FG,
    }
}

fn highlight_color(kind: Option<&str>) -> Color {
    match kind.unwrap_or_default() {
        kind if kind.contains("comment") => MUTED,
        kind if kind.contains("string") || kind.contains("character") => CODE_STRING,
        kind if kind.contains("boolean") => CODE_NUMBER,
        kind if kind.contains("number") || kind.contains("constant") => CODE_NUMBER,
        // Keep these aligned with `semantic_color`: a token the tree-sitter pass
        // colours must match the colour the LSP semantic pass gives it, or it
        // visibly flips (e.g. a keyword changing hue) when the debounced
        // semantic-tokens result lands a beat after each keystroke.
        kind if kind.contains("keyword") => CODE_KEYWORD,
        kind if kind.contains("operator") => CODE_OPERATOR,
        kind if kind.contains("macro") || kind.contains("lifetime") => CODE_BUILTIN,
        kind if kind.contains("function") => CODE_FUNCTION,
        kind if kind.contains("type") || kind.contains("property") => CODE_TYPE,
        kind if kind.contains("text.title") => CODE_FUNCTION,
        kind if kind.contains("text.literal") => CODE_STRING,
        kind if kind.contains("text.uri") || kind.contains("text.reference") => CODE_TYPE,
        kind if kind.contains("text.emphasis") || kind.contains("text.strong") => {
            Color::Rgb(0xa0, 0x93, 0xc7)
        }
        kind if kind.contains("punctuation") => MUTED,
        _ => FG,
    }
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, editor: &Editor, buffer: &ActiveBuffer<'_>) {
    let marker = if buffer.modified { "● " } else { "" };
    let external = if buffer.external_changed { "⚠ " } else { "" };
    let errors = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == crate::lsp::DiagnosticSeverity::Error)
        .count();
    let warnings = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == crate::lsp::DiagnosticSeverity::Warning)
        .count();
    let mut segments = vec![(
        format!("{external}{marker}{}", buffer.name),
        Color::Rgb(0x35, 0x4f, 0x7a),
    )];
    segments.push((buffer.language_status.clone(), Color::Rgb(0x35, 0x65, 0x70)));
    if let Some(git) = git_status_label(buffer.git_branch, buffer.git_status) {
        segments.push((git, Color::Rgb(0x3f, 0x64, 0x53)));
    }
    segments.push((
        format!("E:{errors} W:{warnings}"),
        if errors > 0 {
            Color::Rgb(0x8f, 0x3f, 0x4d)
        } else if warnings > 0 {
            Color::Rgb(0x8a, 0x63, 0x35)
        } else {
            Color::Rgb(0x3b, 0x4a, 0x62)
        },
    ));
    if let Some(status) = editor.status() {
        segments.push((status.to_owned(), Color::Rgb(0x4b, 0x4f, 0x61)));
    }
    frame.render_widget(
        Paragraph::new(powerline_line(segments)).style(Style::default().bg(STATUS_BG)),
        area,
    );
}

fn git_status_label(branch: Option<&str>, status: Option<&str>) -> Option<String> {
    let branch = branch?;
    Some(status.map_or_else(
        || format!("<git> clean @{branch}"),
        |status| format!("<git> {status} @{branch}"),
    ))
}

fn powerline_line(segments: Vec<(String, Color)>) -> Line<'static> {
    let mut spans = Vec::with_capacity(segments.len() * 2);
    for (index, (text, background)) in segments.iter().enumerate() {
        let next_background = segments
            .get(index + 1)
            .map_or(STATUS_BG, |(_, color)| *color);
        spans.push(Span::styled(
            format!(" {text} "),
            Style::default().fg(Color::White).bg(*background),
        ));
        spans.push(Span::styled(
            "",
            Style::default().fg(*background).bg(next_background),
        ));
    }
    Line::from(spans)
}

fn draw_status_text(frame: &mut Frame<'_>, area: Rect, status: &str) {
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(FG).bg(STATUS_BG)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn terminal_cells_preserve_ansi_colors_and_attributes() {
        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process(b"\x1b[31;44;1mR\x1b[0mN");

        let lines = terminal_lines(parser.screen(), 1, 10, None);
        let colored = &lines[0].spans[0];

        assert_eq!(colored.content.as_ref(), "R");
        assert_eq!(colored.style.fg, Some(Color::Indexed(1)));
        assert_eq!(colored.style.bg, Some(Color::Indexed(4)));
        assert!(colored.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn terminal_selection_is_rendered_without_changing_its_text() {
        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process(b"hello");
        let lines = terminal_lines(
            parser.screen(),
            1,
            10,
            Some(crate::editor::TerminalSelectionView {
                start: (0, 1),
                end: (0, 3),
            }),
        );

        assert_eq!(
            lines[0]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "hello     "
        );
        assert!(lines[0].spans.iter().any(|span| {
            span.style.add_modifier.contains(Modifier::REVERSED) && span.content.contains("ell")
        }));
    }

    #[test]
    fn popup_frame_draws_visible_line_borders() {
        let backend = TestBackend::new(20, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_popup_frame(
                    frame,
                    Rect {
                        x: 2,
                        y: 1,
                        width: 10,
                        height: 4,
                    },
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(2, 1)].symbol(), "┌");
        assert_eq!(buffer[(11, 1)].symbol(), "┐");
        assert_eq!(buffer[(2, 4)].symbol(), "└");
        assert_eq!(buffer[(11, 4)].symbol(), "┘");
    }

    #[test]
    fn git_gutter_uses_colored_bars_and_hides_deletions() {
        let (added, added_color) = git_gutter_style(crate::editor::GitLineKind::Added);
        let (modified, modified_color) = git_gutter_style(crate::editor::GitLineKind::Modified);
        let (deleted, _) = git_gutter_style(crate::editor::GitLineKind::Deleted);

        assert_eq!(added, "▌");
        assert_eq!(added_color, Color::Rgb(0xb4, 0xbe, 0x82));
        assert_eq!(modified, "▌");
        assert_eq!(modified_color, Color::Rgb(0x84, 0xa0, 0xc6));
        assert_eq!(deleted, " ");
    }

    #[test]
    fn semantic_color_fallback_stays_readable() {
        assert_eq!(semantic_color("unresolvedReference"), FG);
        assert_eq!(semantic_color("unknown"), FG);
    }

    #[test]
    fn builtin_types_are_coloured_like_other_types() {
        // rust-analyzer reports `usize`/`u8`/`bool`/`str` as `builtinType`; it must
        // read as a type, not fall through to plain foreground.
        assert_eq!(semantic_color("builtinType"), CODE_TYPE);
        assert_eq!(semantic_color("struct"), CODE_TYPE);
        assert_eq!(semantic_color("builtinType"), semantic_color("struct"));
    }

    #[test]
    fn keywords_operators_and_numbers_are_distinct_hues() {
        // `fn`, `%` and `2` sharing one colour was the readability complaint; the
        // three categories must stay visually separable in both highlight passes.
        let keyword = semantic_color("keyword");
        let operator = semantic_color("operator");
        let number = semantic_color("number");
        assert_ne!(keyword, operator);
        assert_ne!(operator, number);
        assert_ne!(keyword, number);
        // The tree-sitter fallback must agree so nothing flickers between passes.
        assert_eq!(highlight_color(Some("keyword")), keyword);
        assert_eq!(highlight_color(Some("operator")), operator);
        assert_eq!(highlight_color(Some("number")), number);
    }

    #[test]
    fn git_and_diagnostic_markers_use_separate_gutter_columns() {
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let text = ropey::Rope::from_str("value");
        let view = crate::view::View::new(crate::document::DocumentId(0));
        let diagnostics = vec![crate::document::ActiveDiagnostic {
            range: crate::position::CharIdx(0)..crate::position::CharIdx(1),
            severity: crate::lsp::DiagnosticSeverity::Error,
            message: "error".to_owned(),
        }];
        let git_lines = vec![crate::editor::GitLine {
            line: 0,
            kind: crate::editor::GitLineKind::Modified,
        }];
        let buffer = crate::editor::ActiveBuffer {
            name: "file.rs".to_owned(),
            text: &text,
            view: &view,
            modified: false,
            external_changed: false,
            language: Some("rust"),
            tab_size: 4,
            language_status: "<lsp> rust: ready".to_owned(),
            diagnostics: &diagnostics,
            git_lines: &git_lines,
            git_branch: Some("main"),
            git_status: Some("M"),
            semantic_spans: &[],
            syntax_spans: &[],
        };

        terminal
            .draw(|frame| draw_buffer(frame, Rect::new(0, 0, 20, 3), &buffer, true))
            .unwrap();

        let rendered = terminal.backend().buffer();
        assert_eq!(rendered[(0, 0)].symbol(), "▌");
        assert_eq!(rendered[(1, 0)].symbol(), "×");
    }

    #[test]
    fn cursor_word_occurrences_are_subtly_highlighted() {
        let backend = TestBackend::new(30, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::TextPaste("foo bar foo".to_owned()));

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let rendered = terminal.backend().buffer();
        assert_eq!(rendered[(5, 0)].bg, OCCURRENCE_BG);
        assert_eq!(rendered[(13, 0)].bg, OCCURRENCE_BG);
        assert_eq!(rendered[(9, 0)].bg, BG);
    }

    #[test]
    fn matching_brackets_next_to_the_caret_are_highlighted() {
        let backend = TestBackend::new(20, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::TextPaste("(x)".to_owned()));

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let rendered = terminal.backend().buffer();
        assert_eq!(rendered[(5, 0)].bg, MATCHING_BRACKET_BG);
        assert_eq!(rendered[(7, 0)].bg, MATCHING_BRACKET_BG);
        assert!(rendered[(5, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn edge_badge_separates_error_warning_modified_and_added_counts() {
        let backend = TestBackend::new(30, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                draw_edge_badge(frame, Rect::new(0, 0, 30, 3), false, 2, 3, 4, 5);
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let row = (0..30)
            .map(|column| buffer[(column, 2)].symbol())
            .collect::<String>();
        assert!(row.ends_with("↓ E2 W3 M4 A5"));
        assert_eq!(buffer[(19, 2)].fg, Color::Rgb(0xe2, 0x78, 0x78));
        assert_eq!(buffer[(22, 2)].fg, Color::Rgb(0xe2, 0xa4, 0x78));
        assert_eq!(buffer[(25, 2)].fg, Color::Rgb(0x84, 0xa0, 0xc6));
        assert_eq!(buffer[(28, 2)].fg, Color::Rgb(0xb4, 0xbe, 0x82));
    }

    #[test]
    fn edge_badge_hit_maps_columns_to_the_segment_drawn_there() {
        // Same layout as the draw test above: within a width-30 pane the bottom
        // badge renders "↓ E2 W3 M4 A5" right-aligned, so its arrow sits at
        // column 17 and each count follows.
        let hit = |column| edge_badge_hit(0, 30, 3, false, 2, 3, 4, 5, column, 2);

        assert_eq!(
            hit(17),
            Some(EdgeBadgeHit {
                above: false,
                category: EdgeBadgeCategory::Any,
            })
        );
        assert_eq!(hit(19).unwrap().category, EdgeBadgeCategory::Error);
        assert_eq!(hit(22).unwrap().category, EdgeBadgeCategory::Warning);
        assert_eq!(hit(25).unwrap().category, EdgeBadgeCategory::Modified);
        assert_eq!(hit(28).unwrap().category, EdgeBadgeCategory::Added);
        // Left of the badge, and on the top row where only an above-badge lives.
        assert_eq!(hit(16), None);
        assert_eq!(edge_badge_hit(0, 30, 3, false, 2, 3, 4, 5, 19, 0), None);
    }

    #[test]
    fn edge_badge_hit_skips_absent_counts() {
        // With only warnings and modified the badge is "↓ W3 M4" right-aligned in
        // the width-30 pane, so its arrow is at column 23 and the segments follow
        // with no gap left for the absent error count.
        let hit = |column| edge_badge_hit(0, 30, 3, false, 0, 3, 4, 0, column, 2);
        assert_eq!(hit(23).unwrap().category, EdgeBadgeCategory::Any);
        assert_eq!(hit(25).unwrap().category, EdgeBadgeCategory::Warning);
        assert_eq!(hit(28).unwrap().category, EdgeBadgeCategory::Modified);
    }

    #[test]
    fn statusline_uses_powerline_separators_and_distinct_backgrounds() {
        let first = Color::Rgb(1, 2, 3);
        let second = Color::Rgb(4, 5, 6);
        let line = powerline_line(vec![
            ("file.rs".to_owned(), first),
            ("<git> M @main".to_owned(), second),
        ]);

        assert_eq!(line.spans[1].content.as_ref(), "");
        assert_eq!(line.spans[1].style.fg, Some(first));
        assert_eq!(line.spans[1].style.bg, Some(second));
        assert_eq!(line.spans[3].style.bg, Some(STATUS_BG));
    }

    #[test]
    fn git_status_label_shows_branch_and_porcelain_state() {
        assert_eq!(
            git_status_label(Some("main"), Some("M")),
            Some("<git> M @main".to_owned())
        );
        assert_eq!(
            git_status_label(Some("main"), None),
            Some("<git> clean @main".to_owned())
        );
        assert_eq!(git_status_label(None, Some("M")), None);
    }

    #[test]
    fn picker_item_highlights_fuzzy_matched_characters() {
        let item = crate::editor::PickerViewItem {
            label: "diff".to_owned(),
            matched: vec![0, 2],
        };

        let line = picker_item_line(&item, false, 20);

        assert_eq!(line.spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(line.spans[1].style.fg, Some(FG));
        assert_eq!(line.spans[2].style.fg, Some(Color::Yellow));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn long_logical_lines_count_as_multiple_soft_wrapped_rows() {
        let text = ropey::Rope::from_str("abcdefghij");

        assert_eq!(wrapped_line_rows(&text, 0, 5, 4), 2);
        assert_eq!(wrapped_line_rows(&text, 0, 20, 4), 1);
    }

    #[test]
    fn soft_wrap_continuations_stay_to_the_right_of_the_gutter() {
        let backend = TestBackend::new(10, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::Resize { cols: 10, rows: 5 });
        editor.update(crate::editor::AppEvent::TextPaste("abcdefghij".to_owned()));

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!((0..5).all(|column| buffer[(column, 1)].symbol() == " "));
        assert_eq!(buffer[(5, 1)].symbol(), "f");
    }

    #[test]
    fn tab_expanded_lines_wrap_at_a_fixed_width_so_the_caret_stays_aligned() {
        let backend = TestBackend::new(10, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::Resize { cols: 10, rows: 5 });
        // A leading tab expands to spaces (word-break opportunities); the display
        // string "    abcdef" is 10 columns wide and must wrap at exactly text_width
        // (5) rather than at the whitespace, or the caret math drifts from the text.
        editor.update(crate::editor::AppEvent::TextPaste("\tabcdef".to_owned()));

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let buffer = terminal.backend().buffer();
        // gutter is 5 wide, so text begins at column 5: row 0 shows "    a".
        assert!((5..9).all(|column| buffer[(column, 0)].symbol() == " "));
        assert_eq!(buffer[(9, 0)].symbol(), "a");
        // The continuation row resumes with the very next character, "bcdef".
        assert_eq!(buffer[(5, 1)].symbol(), "b");
        assert_eq!(buffer[(6, 1)].symbol(), "c");
    }

    #[test]
    fn find_and_replace_opens_as_a_right_pane_with_scope_tabs_and_results() {
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::Resize { cols: 40, rows: 12 });
        editor.update(crate::editor::AppEvent::TextPaste("foo bar foo".to_owned()));
        editor.update(crate::editor::Command::OpenReplace.into());
        for character in "foo".chars() {
            editor.update(crate::editor::AppEvent::TextInput(character));
        }

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let buffer = terminal.backend().buffer();
        let screen: String = (0..12)
            .flat_map(|row| (0..40).map(move |column| (column, row)))
            .map(|(column, row)| buffer[(column, row)].symbol().to_owned())
            .collect();
        assert!(screen.contains("Find"), "find box missing: {screen:?}");
        assert!(
            screen.contains("Replace"),
            "replace box missing: {screen:?}"
        );
        assert!(screen.contains("file"), "scope tabs missing: {screen:?}");
        // "foo" occurs twice in the current buffer.
        assert!(screen.contains("2 件"), "result count missing: {screen:?}");
    }

    #[test]
    fn secondary_cursors_are_vertical_bars() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::Resize { cols: 20, rows: 5 });
        editor.update(crate::editor::AppEvent::TextPaste("ab\ncd".to_owned()));
        editor.update(
            crate::editor::Command::Move {
                direction: crate::editor::Direction::Left,
                unit: crate::editor::Unit::Character,
                extend: false,
            }
            .into(),
        );
        editor.update(
            crate::editor::Command::AddCursor {
                direction: crate::editor::VerticalDirection::Up,
            }
            .into(),
        );

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!(buffer[(6, 1)].symbol().contains('d'));
        assert!(buffer[(6, 1)].symbol().contains('\u{20d2}'));
        assert_eq!(buffer[(7, 1)].symbol(), " ");
    }

    #[test]
    fn completion_popup_never_covers_the_cursor_row() {
        let viewport = Rect::new(0, 0, 80, 20);
        let below = completion_popup_area(viewport, (20, 4), 5).unwrap();
        assert!(below.y > 4);

        let above = completion_popup_area(viewport, (20, 18), 12).unwrap();
        assert!(above.bottom() <= 18);
    }

    #[test]
    fn only_the_differing_run_of_a_changed_line_is_tinted() {
        let backend = TestBackend::new(80, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::Resize { cols: 80, rows: 4 });
        editor.open_paths([
            std::path::PathBuf::from("a.txt"),
            std::path::PathBuf::from("b.txt"),
        ]);
        editor.update(crate::editor::AppEvent::Io(
            crate::editor::IoEvent::FileLoaded {
                id: crate::document::DocumentId(1),
                result: Ok("let total = 1;".to_owned()),
            },
        ));
        editor.update(crate::editor::AppEvent::Io(
            crate::editor::IoEvent::FileLoaded {
                id: crate::document::DocumentId(2),
                result: Ok("let total = 2;".to_owned()),
            },
        ));
        editor.update(crate::editor::Command::OpenDiffPicker.into());
        editor.update(crate::editor::Command::PickerConfirm.into());

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        // Both panes draw "~   1 let total = N;". Only the digit that differs
        // may carry the strong tint; everything else keeps the row background.
        let buffer = terminal.backend().buffer();
        let tinted = |columns: std::ops::Range<u16>, background: Color| -> Vec<String> {
            columns
                .filter(|column| buffer[(*column, 0)].bg == background)
                .map(|column| buffer[(column, 0)].symbol().to_owned())
                .collect()
        };

        assert_eq!(tinted(0..39, WORD_REMOVED_BG), vec!["2"]);
        assert_eq!(tinted(41..68, WORD_ADDED_BG), vec!["1"]);
        // The unchanged remainder is still marked as a changed row.
        assert!(
            (0..39).any(|column| buffer[(column, 0)].symbol() == "l"
                && buffer[(column, 0)].bg == CHANGED_BG)
        );
    }

    #[test]
    fn split_layout_reserves_a_visible_divider_column() {
        let backend = TestBackend::new(21, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::Resize { cols: 21, rows: 5 });
        editor.update(crate::editor::AppEvent::TextInput('x'));
        editor.update(crate::editor::Command::ToggleSplit.into());

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!((0..4).all(|row| buffer[(10, row)].symbol() == "│"));
    }

    #[test]
    fn split_hover_is_rendered_in_the_opposite_editor_pane() {
        let viewport = Rect::new(0, 0, 180, 20);

        let from_right = hover_popup_area(viewport, 5, true, crate::editor::Side::Right);
        assert!(from_right.right() <= 90);
        assert!(from_right.x > 1);

        let from_left = hover_popup_area(viewport, 5, true, crate::editor::Side::Left);
        assert!(from_left.x > 90);
    }

    #[test]
    fn diagnostics_render_as_colored_non_editable_virtual_text() {
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        let path = std::path::PathBuf::from("/tmp/virtual.rs");
        editor.open_paths([path.clone()]);
        editor.update(crate::editor::AppEvent::Io(
            crate::editor::IoEvent::FileLoaded {
                id: crate::document::DocumentId(1),
                result: Ok("let x".to_owned()),
            },
        ));
        editor.update(crate::editor::AppEvent::Lsp(
            crate::lsp::LspEvent::Diagnostics {
                uri: format!("file://{}", path.display()),
                diagnostics: vec![crate::lsp::Diagnostic {
                    line: 0,
                    character: 0,
                    end_line: 0,
                    end_character: 3,
                    severity: crate::lsp::DiagnosticSeverity::Error,
                    message: "bad value".to_owned(),
                }],
            },
        ));
        editor.update(crate::editor::AppEvent::Resize { cols: 40, rows: 5 });

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let cell = &terminal.backend().buffer()[(16, 0)];
        assert_eq!(cell.symbol(), "b");
        assert_eq!(cell.fg, Color::Rgb(0xe2, 0x78, 0x78));
        assert!(cell.modifier.contains(Modifier::ITALIC));
        let diagnosed_code = &terminal.backend().buffer()[(5, 0)];
        assert_eq!(diagnosed_code.underline_color, Color::Rgb(0xe2, 0x78, 0x78));
        assert!(diagnosed_code.modifier.contains(Modifier::UNDERLINED));

        editor.update(crate::editor::AppEvent::Mouse(crate::editor::MouseInput {
            event: crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 20,
                row: 0,
                modifiers: crossterm::event::KeyModifiers::NONE,
            },
            clicks: 1,
        }));
        assert_eq!(
            editor
                .active_buffer()
                .unwrap()
                .view
                .selections
                .primary()
                .head,
            crate::position::CharIdx(5)
        );
    }

    #[test]
    fn hover_markdown_uses_syntax_colors() {
        let lines = highlighted_hover_lines("# Heading\n\n```rust\nfn main() {}\n```");

        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| { span.style.fg.is_some_and(|color| color != FG) })
        );
        assert_eq!(lines[3].spans[0].content.as_ref(), "fn");
        // `fn` is a keyword, coloured to match the editor's semantic keyword hue.
        assert_eq!(lines[3].spans[0].style.fg, Some(CODE_KEYWORD));
    }

    #[test]
    fn hover_plain_rust_signatures_and_examples_use_syntax_colors() {
        let lines = highlighted_hover_lines(
            "std::env\n\npub fn current_dir() -> std::io::Result<PathBuf>\n\nReturns a path.\n\nfn main() {\n    let path = std::env::current_dir();\n}",
        );

        assert!(lines[2].spans.iter().any(|span| span.style.fg != Some(FG)));
        assert!(lines[6].spans.iter().any(|span| span.style.fg != Some(FG)));
        assert!(lines[7].spans.iter().any(|span| span.style.fg != Some(FG)));
    }

    #[test]
    fn overlays_do_not_cover_the_status_bar() {
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::Command::OpenCommandPalette.into());

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let buffer = terminal.backend().buffer();
        assert!((0..40).all(|column| {
            !matches!(
                buffer[(column, 7)].symbol(),
                "┌" | "┐" | "└" | "┘" | "│" | "─"
            )
        }));
    }
}
