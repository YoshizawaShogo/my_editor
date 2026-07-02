use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    editor::{ActiveBuffer, Editor},
    position::{char_idx_to_display_pos, display_col_after},
};

const TAB_SIZE: usize = 4;
const BG: Color = Color::Rgb(0x16, 0x18, 0x21);
const FG: Color = Color::Rgb(0xc6, 0xc8, 0xd1);
const MUTED: Color = Color::Rgb(0x6b, 0x70, 0x89);
const SELECTION: Color = Color::Rgb(0x27, 0x2c, 0x42);
const STATUS_BG: Color = Color::Rgb(0x0f, 0x11, 0x17);
const ADDED_BG: Color = Color::Rgb(0x24, 0x30, 0x25);
const REMOVED_BG: Color = Color::Rgb(0x38, 0x22, 0x28);
const CHANGED_BG: Color = Color::Rgb(0x38, 0x30, 0x22);
const POPUP_BG: Color = Color::Rgb(0x1e, 0x21, 0x32);

pub fn draw(frame: &mut Frame<'_>, editor: &Editor) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());

    frame.render_widget(Block::default().style(Style::default().bg(BG)), areas[0]);
    if editor.shell_visible() {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(areas[0]);
        if let Some(buffer) = editor.active_buffer() {
            draw_buffer(frame, panes[0], &buffer, !editor.shell_focused());
            draw_status(frame, areas[1], editor, &buffer);
        }
        if let Some(screen) = editor.terminal_screen() {
            draw_terminal(frame, panes[1], screen, editor.shell_focused());
        }
    } else if editor.show_start_page() {
        draw_start_page(frame, areas[0]);
        draw_status_text(frame, areas[1], editor.status().unwrap_or("Ready"));
    } else if let Some((left, right, true)) = editor.split_buffers() {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(areas[0]);
        draw_diff(
            frame,
            panes[0],
            panes[1],
            &left,
            &right,
            editor.focused_side(),
        );
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
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(areas[0]);
        let focused = editor.focused_side();
        draw_buffer(frame, panes[0], &left, focused == crate::editor::Side::Left);
        draw_buffer(
            frame,
            panes[1],
            &right,
            focused == crate::editor::Side::Right,
        );
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
    if let Some(search) = editor.search_view() {
        draw_search(frame, &search);
    }
    if let Some(completion) = editor.completion_view() {
        draw_completion(frame, editor, &completion);
    }
    if let Some(rename) = editor.rename_view() {
        draw_rename(frame, rename);
    }
    if let Some(confirm) = editor.confirm_view() {
        draw_confirm(frame, confirm);
    }
    if let Some(hover) = editor.hover_view() {
        draw_hover(frame, editor, hover);
    }
}

fn draw_terminal(frame: &mut Frame<'_>, area: Rect, screen: &vt100::Screen, focused: bool) {
    let (screen_rows, screen_cols) = screen.size();
    let rows = area.height.min(screen_rows);
    let cols = area.width.min(screen_cols);
    let lines = terminal_lines(screen, rows, cols);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(FG).bg(Color::Rgb(0x12, 0x14, 0x1c))),
        area,
    );
    if focused {
        let (row, col) = screen.cursor_position();
        if row < rows && col < cols {
            frame.set_cursor_position((area.x + col, area.y + row));
        }
    }
}

fn terminal_lines(screen: &vt100::Screen, rows: u16, cols: u16) -> Vec<Line<'static>> {
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
                let style = terminal_style(cell);
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
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(viewport);
        if focused == crate::editor::Side::Right {
            panes[0]
        } else {
            panes[1]
        }
    } else {
        viewport
    };
    let width = target.width.saturating_sub(2).clamp(1, 64);
    let height = (line_count + 2).min(target.height.saturating_sub(1).max(1));
    Rect {
        x: if split {
            (target.x + 1).min(target.right().saturating_sub(width))
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

fn completion_anchor_position(
    editor: &Editor,
    viewport: Rect,
    anchor: crate::position::CharIdx,
) -> Option<(u16, u16)> {
    let buffer = editor.active_buffer()?;
    let mut pane = viewport;
    if editor.shell_visible() || editor.split_buffers().is_some() {
        pane.width /= 2;
        if editor.focused_side() == crate::editor::Side::Right {
            pane.x += pane.width;
        }
    }
    let gutter_width = (buffer.text.len_lines().max(1).to_string().len().max(2) + 3)
        .min(usize::from(pane.width.saturating_sub(1)));
    let text_width = usize::from(pane.width.saturating_sub(gutter_width as u16)).max(1);
    let cursor = char_idx_to_display_pos(buffer.text, anchor, TAB_SIZE);
    if cursor.line < buffer.view.scroll.top_line {
        return None;
    }
    let visual_row = (buffer.view.scroll.top_line..cursor.line)
        .map(|line| wrapped_line_rows(buffer.text, line, text_width))
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

fn draw_search(frame: &mut Frame<'_>, search: &crate::editor::SearchView) {
    let viewport = overlay_area(frame);
    let width = viewport.width.saturating_sub(4).clamp(1, 76);
    let available_height = viewport.height.saturating_sub(1).max(1);
    let filter_rows = u16::from(search.scope == crate::editor::SearchScope::Directory) * 2;
    let input_rows = 1 + u16::from(search.replacement.is_some()) + filter_rows;
    let height = (search.items.len() as u16 + input_rows + 2)
        .min(available_height)
        .max(1);
    let area = Rect {
        x: viewport.x + (viewport.width.saturating_sub(width)) / 2,
        y: viewport.y + 1,
        width,
        height,
    };
    let inner = draw_popup_frame(frame, area);
    let scope = match search.scope {
        crate::editor::SearchScope::CurrentBuffer => "buffer",
        crate::editor::SearchScope::AllBuffers => "buffers",
        crate::editor::SearchScope::Directory => "directory",
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("Search [{scope}]  "), Style::default().fg(MUTED)),
        Span::styled(&search.query, Style::default().fg(FG)),
        Span::styled(
            format!(
                "  [{}]aA [{}]word [{}].*",
                if search.options.case_sensitive {
                    "x"
                } else {
                    " "
                },
                if search.options.whole_word { "x" } else { " " },
                if search.options.regex { "x" } else { " " },
            ),
            Style::default().fg(MUTED),
        ),
    ])];
    if let Some(replacement) = &search.replacement {
        lines.push(Line::from(vec![
            Span::styled("Replace  ", Style::default().fg(MUTED)),
            Span::styled(
                replacement,
                Style::default().fg(if search.editing_replace {
                    Color::Yellow
                } else {
                    FG
                }),
            ),
        ]));
    }
    if search.scope == crate::editor::SearchScope::Directory {
        lines.push(Line::from(vec![
            Span::styled("Include  ", Style::default().fg(MUTED)),
            Span::styled(
                &search.include,
                Style::default().fg(
                    if search.editing_filter == Some(crate::editor::SearchFilterField::Include) {
                        Color::Yellow
                    } else {
                        FG
                    },
                ),
            ),
            Span::styled(
                format!(
                    "  ignore:{} hidden:{}",
                    if search.filters.respect_ignore_files {
                        "on"
                    } else {
                        "off"
                    },
                    if search.filters.include_hidden {
                        "on"
                    } else {
                        "off"
                    },
                ),
                Style::default().fg(MUTED),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Exclude  ", Style::default().fg(MUTED)),
            Span::styled(
                &search.exclude,
                Style::default().fg(
                    if search.editing_filter == Some(crate::editor::SearchFilterField::Exclude) {
                        Color::Yellow
                    } else {
                        FG
                    },
                ),
            ),
        ]));
    }
    for (index, item) in search.items.iter().enumerate() {
        lines.push(Line::styled(
            item,
            if index == search.current {
                Style::default().fg(FG).bg(SELECTION)
            } else {
                Style::default().fg(FG).bg(POPUP_BG)
            },
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(POPUP_BG)),
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
    let mut parts = Vec::new();
    if errors > 0 {
        parts.push((format!("E{errors}"), Color::Rgb(0xe2, 0x78, 0x78)));
    }
    if warnings > 0 {
        parts.push((format!("W{warnings}"), Color::Rgb(0xe2, 0xa4, 0x78)));
    }
    if modified > 0 {
        parts.push((format!("M{modified}"), Color::Rgb(0x84, 0xa0, 0xc6)));
    }
    if added > 0 {
        parts.push((format!("A{added}"), Color::Rgb(0xb4, 0xbe, 0x82)));
    }
    let width = (1 + parts
        .iter()
        .map(|(text, _)| 1 + text.chars().count())
        .sum::<usize>())
    .min(usize::from(area.width)) as u16;
    let badge = Rect {
        x: area.right().saturating_sub(width),
        y: if above { area.y } else { area.bottom() - 1 },
        width,
        height: 1,
    };
    let mut spans = vec![Span::styled(
        arrow.to_string(),
        Style::default().fg(Color::Yellow).bg(POPUP_BG),
    )];
    for (text, color) in parts {
        spans.push(Span::styled(" ", Style::default().bg(POPUP_BG)));
        spans.push(Span::styled(text, Style::default().fg(color).bg(POPUP_BG)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), badge);
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
        lines.push(picker_item_line(item, index == picker.selected));
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

fn picker_item_line(item: &crate::editor::PickerViewItem, selected: bool) -> Line<'static> {
    let background = if selected { SELECTION } else { POPUP_BG };
    let spans = item
        .label
        .chars()
        .enumerate()
        .map(|(index, character)| {
            let matched = item.matched.contains(&index);
            let style = Style::default()
                .fg(if matched { Color::Yellow } else { FG })
                .bg(background)
                .add_modifier(if matched {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                });
            Span::styled(character.to_string(), style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn draw_diff(
    frame: &mut Frame<'_>,
    left_area: Rect,
    right_area: Rect,
    left: &ActiveBuffer<'_>,
    right: &ActiveBuffer<'_>,
    focused: crate::editor::Side,
) {
    let left_lines = rope_lines(left.text);
    let right_lines = rope_lines(right.text);
    let rows = aligned_diff(&left_lines, &right_lines);
    let height = usize::from(left_area.height.min(right_area.height));
    let focused_line = match focused {
        crate::editor::Side::Left => {
            char_idx_to_display_pos(left.text, left.view.selections.primary().head, TAB_SIZE).line
        }
        crate::editor::Side::Right => {
            char_idx_to_display_pos(right.text, right.view.selections.primary().head, TAB_SIZE).line
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
    let start = cursor_row
        .map(|row| row.saturating_sub(height.saturating_sub(1) / 2))
        .unwrap_or(0)
        .min(rows.len().saturating_sub(height));
    let mut rendered_left = Vec::new();
    let mut rendered_right = Vec::new();
    for row in rows.iter().skip(start).take(height) {
        let (background, marker) = match row.kind {
            DiffKind::Equal => (BG, " "),
            DiffKind::Added => (ADDED_BG, "+"),
            DiffKind::Removed => (REMOVED_BG, "-"),
            DiffKind::Changed => (CHANGED_BG, "~"),
        };
        rendered_left.push(diff_line(row.left.clone(), marker, background));
        rendered_right.push(diff_line(row.right.clone(), marker, background));
    }
    frame.render_widget(
        Paragraph::new(rendered_left).style(Style::default().fg(FG).bg(BG)),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(rendered_right).style(Style::default().fg(FG).bg(BG)),
        right_area,
    );
    if let Some(row) = cursor_row.filter(|row| *row >= start && *row < start + height) {
        let (area, buffer) = match focused {
            crate::editor::Side::Left => (left_area, left),
            crate::editor::Side::Right => (right_area, right),
        };
        let position =
            char_idx_to_display_pos(buffer.text, buffer.view.selections.primary().head, TAB_SIZE);
        let column = 6usize.saturating_add(position.col);
        if column < usize::from(area.width) {
            frame.set_cursor_position((area.x + column as u16, area.y + (row - start) as u16));
        }
    }
}

fn diff_line(line: Option<(usize, String)>, marker: &str, background: Color) -> Line<'static> {
    match line {
        Some((number, text)) => Line::styled(
            format!("{marker}{:>4} {text}", number + 1),
            Style::default().fg(FG).bg(background),
        ),
        None => Line::styled(
            format!("{marker}     "),
            Style::default().fg(MUTED).bg(background),
        ),
    }
}

fn rope_lines(text: &ropey::Rope) -> Vec<String> {
    text.lines()
        .map(|line| line.to_string().trim_end_matches(['\r', '\n']).to_owned())
        .collect()
}

#[derive(Clone, Copy)]
enum DiffKind {
    Equal,
    Added,
    Removed,
    Changed,
}

struct DiffRow {
    left: Option<(usize, String)>,
    right: Option<(usize, String)>,
    kind: DiffKind,
}

fn aligned_diff(left: &[String], right: &[String]) -> Vec<DiffRow> {
    if left.len().saturating_mul(right.len()) > 1_000_000 {
        return index_diff(left, right);
    }
    let width = right.len() + 1;
    let mut lcs = vec![0usize; (left.len() + 1) * width];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            lcs[i * width + j] = if left[i] == right[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let mut rows = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < left.len() || j < right.len() {
        if i < left.len() && j < right.len() && left[i] == right[j] {
            rows.push(DiffRow {
                left: Some((i, left[i].clone())),
                right: Some((j, right[j].clone())),
                kind: DiffKind::Equal,
            });
            i += 1;
            j += 1;
            continue;
        }
        let mut removed = Vec::new();
        let mut added = Vec::new();
        while i < left.len() || j < right.len() {
            if i < left.len() && j < right.len() && left[i] == right[j] {
                break;
            }
            if j == right.len()
                || (i < left.len() && lcs[(i + 1) * width + j] >= lcs[i * width + j + 1])
            {
                removed.push((i, left[i].clone()));
                i += 1;
            } else {
                added.push((j, right[j].clone()));
                j += 1;
            }
        }
        let count = removed.len().max(added.len());
        for index in 0..count {
            let left_row = removed.get(index).cloned();
            let right_row = added.get(index).cloned();
            rows.push(DiffRow {
                kind: match (left_row.is_some(), right_row.is_some()) {
                    (true, true) => DiffKind::Changed,
                    (true, false) => DiffKind::Removed,
                    (false, true) => DiffKind::Added,
                    (false, false) => unreachable!(),
                },
                left: left_row,
                right: right_row,
            });
        }
    }
    rows
}

fn index_diff(left: &[String], right: &[String]) -> Vec<DiffRow> {
    (0..left.len().max(right.len()))
        .map(|index| {
            let left_row = left.get(index).cloned().map(|line| (index, line));
            let right_row = right.get(index).cloned().map(|line| (index, line));
            let kind = match (&left_row, &right_row) {
                (Some((_, left)), Some((_, right))) if left == right => DiffKind::Equal,
                (Some(_), Some(_)) => DiffKind::Changed,
                (Some(_), None) => DiffKind::Removed,
                (None, Some(_)) => DiffKind::Added,
                (None, None) => unreachable!(),
            };
            DiffRow {
                left: left_row,
                right: right_row,
                kind,
            }
        })
        .collect()
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
    let mut gutter_lines = Vec::with_capacity(end.saturating_sub(start));
    let mut text_lines = Vec::with_capacity(end.saturating_sub(start));

    for line_index in start..end {
        let line_diagnostic = buffer
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.line as usize == line_index)
            .min_by_key(|diagnostic| match diagnostic.severity {
                crate::lsp::DiagnosticSeverity::Error => 0,
                crate::lsp::DiagnosticSeverity::Warning => 1,
                crate::lsp::DiagnosticSeverity::Information => 2,
                crate::lsp::DiagnosticSeverity::Hint => 3,
            });
        let severity = line_diagnostic.map(|diagnostic| diagnostic.severity);
        let (marker, marker_color) = match severity {
            Some(crate::lsp::DiagnosticSeverity::Error) => ("×", Color::Rgb(0xe2, 0x78, 0x78)),
            Some(crate::lsp::DiagnosticSeverity::Warning) => ("▲", Color::Rgb(0xe2, 0xa4, 0x78)),
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
                Style::default().fg(MUTED).bg(BG),
            ),
        ];
        let mut spans = Vec::new();
        let line = buffer.text.line(line_index);
        let line_start = buffer.text.line_to_char(line_index);
        let mut display_col = 0;
        for (char_col, character) in line.chars().enumerate() {
            if matches!(character, '\r' | '\n') {
                break;
            }
            let next_col = display_col_after(display_col, character, TAB_SIZE);
            let rendered = if character == '\t' {
                " ".repeat(next_col - display_col)
            } else {
                character.to_string()
            };
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
                    .map(|span| span.token_type);
                Style::default()
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
                    .bg(BG)
            };
            spans.push(Span::styled(rendered, style));
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
                let color = match diagnostic.severity {
                    crate::lsp::DiagnosticSeverity::Error => Color::Rgb(0xe2, 0x78, 0x78),
                    crate::lsp::DiagnosticSeverity::Warning => Color::Rgb(0xe2, 0xa4, 0x78),
                    crate::lsp::DiagnosticSeverity::Information => Color::Rgb(0x89, 0xb8, 0xc2),
                    crate::lsp::DiagnosticSeverity::Hint => MUTED,
                };
                spans.push(Span::styled(
                    text,
                    Style::default()
                        .fg(color)
                        .bg(BG)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
        }
        gutter_lines.push(Line::from(gutter_spans));
        gutter_lines.extend(
            std::iter::repeat_with(Line::default)
                .take(display_col.max(1).div_ceil(text_width).saturating_sub(1)),
        );
        text_lines.push(Line::from(spans));
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
            .wrap(Wrap { trim: false })
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
        let caret = char_idx_to_display_pos(buffer.text, selection.head, TAB_SIZE);
        if caret.line < start {
            continue;
        }
        let rows_before = (start..caret.line)
            .map(|line| wrapped_line_rows(buffer.text, line, text_width))
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
            cell.set_symbol("▏").set_fg(FG);
        }
    }

    let above_errors = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            (diagnostic.line as usize) < start
                && diagnostic.severity == crate::lsp::DiagnosticSeverity::Error
        })
        .count();
    let above_warnings = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            (diagnostic.line as usize) < start
                && diagnostic.severity == crate::lsp::DiagnosticSeverity::Warning
        })
        .count();
    let below_errors = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.line as usize >= end
                && diagnostic.severity == crate::lsp::DiagnosticSeverity::Error
        })
        .count();
    let below_warnings = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.line as usize >= end
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

    let cursor =
        char_idx_to_display_pos(buffer.text, buffer.view.selections.primary().head, TAB_SIZE);
    let rows_before = (start..cursor.line)
        .map(|line| wrapped_line_rows(buffer.text, line, text_width))
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
    const PREFIX: &str = "  › ";
    if available <= PREFIX.len() {
        return String::new();
    }
    let mut text = PREFIX.to_owned();
    text.extend(message.chars().take(available - PREFIX.len()));
    text
}

fn wrapped_line_rows(text: &ropey::Rope, line: usize, width: usize) -> usize {
    let display_width = text
        .line(line.min(text.len_lines().saturating_sub(1)))
        .chars()
        .take_while(|character| !matches!(character, '\r' | '\n'))
        .fold(0, |column, character| {
            display_col_after(column, character, TAB_SIZE)
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

fn semantic_color(token_type: u32) -> Color {
    match token_type % 6 {
        0 => Color::Rgb(0x84, 0xa0, 0xc6),
        1 => Color::Rgb(0x89, 0xb8, 0xc2),
        2 => Color::Rgb(0xa0, 0x93, 0xc7),
        3 => Color::Rgb(0xb4, 0xbe, 0x82),
        4 => Color::Rgb(0xe2, 0xa4, 0x78),
        _ => MUTED,
    }
}

fn highlight_color(kind: Option<&str>) -> Color {
    match kind.unwrap_or_default() {
        kind if kind.contains("comment") => MUTED,
        kind if kind.contains("string") => Color::Rgb(0xb4, 0xbe, 0x82),
        kind if kind.contains("number") || kind.contains("constant") => {
            Color::Rgb(0xe2, 0xa4, 0x78)
        }
        kind if kind.contains("keyword") || kind.contains("function") => {
            Color::Rgb(0x84, 0xa0, 0xc6)
        }
        kind if kind.contains("type") || kind.contains("property") => Color::Rgb(0x89, 0xb8, 0xc2),
        kind if kind.contains("text.title") => Color::Rgb(0x84, 0xa0, 0xc6),
        kind if kind.contains("text.literal") => Color::Rgb(0xb4, 0xbe, 0x82),
        kind if kind.contains("text.uri") || kind.contains("text.reference") => {
            Color::Rgb(0x89, 0xb8, 0xc2)
        }
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
    fn diff_aligns_insertions_and_changed_lines() {
        let left = vec!["same".to_owned(), "old".to_owned(), "tail".to_owned()];
        let right = vec![
            "same".to_owned(),
            "new".to_owned(),
            "added".to_owned(),
            "tail".to_owned(),
        ];

        let rows = aligned_diff(&left, &right);

        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0].kind, DiffKind::Equal));
        assert!(matches!(rows[1].kind, DiffKind::Changed));
        assert!(matches!(rows[2].kind, DiffKind::Added));
        assert!(rows[2].left.is_none());
        assert_eq!(rows[2].right.as_ref().unwrap().1, "added");
    }

    #[test]
    fn terminal_cells_preserve_ansi_colors_and_attributes() {
        let mut parser = vt100::Parser::new(2, 10, 0);
        parser.process(b"\x1b[31;44;1mR\x1b[0mN");

        let lines = terminal_lines(parser.screen(), 1, 10);
        let colored = &lines[0].spans[0];

        assert_eq!(colored.content.as_ref(), "R");
        assert_eq!(colored.style.fg, Some(Color::Indexed(1)));
        assert_eq!(colored.style.bg, Some(Color::Indexed(4)));
        assert!(colored.style.add_modifier.contains(Modifier::BOLD));
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
    fn git_and_diagnostic_markers_use_separate_gutter_columns() {
        let backend = TestBackend::new(20, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let text = ropey::Rope::from_str("value");
        let view = crate::view::View::new(crate::document::DocumentId(0));
        let diagnostics = vec![crate::lsp::Diagnostic {
            line: 0,
            character: 0,
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

        let line = picker_item_line(&item, false);

        assert_eq!(line.spans[0].style.fg, Some(Color::Yellow));
        assert_eq!(line.spans[1].style.fg, Some(FG));
        assert_eq!(line.spans[2].style.fg, Some(Color::Yellow));
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn long_logical_lines_count_as_multiple_soft_wrapped_rows() {
        let text = ropey::Rope::from_str("abcdefghij");

        assert_eq!(wrapped_line_rows(&text, 0, 5), 2);
        assert_eq!(wrapped_line_rows(&text, 0, 20), 1);
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
    fn secondary_cursors_are_vertical_bars() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut editor = Editor::default();
        editor.update(crate::editor::AppEvent::Resize { cols: 20, rows: 5 });
        editor.update(crate::editor::AppEvent::TextPaste("a\nb".to_owned()));
        editor.update(
            crate::editor::Command::AddCursor {
                direction: crate::editor::VerticalDirection::Up,
            }
            .into(),
        );

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        assert_eq!(terminal.backend().buffer()[(6, 1)].symbol(), "▏");
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
    fn split_hover_is_rendered_in_the_opposite_editor_pane() {
        let viewport = Rect::new(0, 0, 100, 20);

        let from_right = hover_popup_area(viewport, 5, true, crate::editor::Side::Right);
        assert!(from_right.right() <= 50);

        let from_left = hover_popup_area(viewport, 5, true, crate::editor::Side::Left);
        assert!(from_left.x >= 50);
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
                    severity: crate::lsp::DiagnosticSeverity::Error,
                    message: "bad value".to_owned(),
                }],
            },
        ));
        editor.update(crate::editor::AppEvent::Resize { cols: 40, rows: 5 });

        terminal.draw(|frame| draw(frame, &editor)).unwrap();

        let cell = &terminal.backend().buffer()[(14, 0)];
        assert_eq!(cell.symbol(), "b");
        assert_eq!(cell.fg, Color::Rgb(0xe2, 0x78, 0x78));
        assert!(cell.modifier.contains(Modifier::ITALIC));

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
        assert_eq!(
            lines[3].spans[0].style.fg,
            Some(Color::Rgb(0x84, 0xa0, 0xc6))
        );
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
