use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
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
            draw_buffer(frame, panes[0], &buffer);
            draw_status(frame, areas[1], editor, &buffer);
        }
        frame.render_widget(
            Paragraph::new(editor.terminal_contents().unwrap_or_default())
                .style(Style::default().fg(FG).bg(Color::Rgb(0x12, 0x14, 0x1c))),
            panes[1],
        );
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
        draw_status(frame, areas[1], editor, &left);
    } else if let Some((left, right, false)) = editor.split_buffers() {
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(areas[0]);
        draw_buffer(frame, panes[0], &left);
        draw_buffer(frame, panes[1], &right);
        draw_status(frame, areas[1], editor, &left);
    } else if let Some(buffer) = editor.active_buffer() {
        draw_buffer(frame, areas[0], &buffer);
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
        draw_completion(frame, &completion);
    }
    if let Some(rename) = editor.rename_view() {
        draw_rename(frame, rename);
    }
    if let Some(confirm) = editor.confirm_view() {
        draw_confirm(frame, confirm);
    }
    if let Some(hover) = editor.hover_view() {
        draw_hover(frame, hover);
    }
    draw_notifications(frame, editor);
    if editor.hint_guide_visible() {
        draw_hint_guide(frame);
    }
}

fn draw_notifications(frame: &mut Frame<'_>, editor: &Editor) {
    let notifications = editor.notification_views();
    if notifications.is_empty() {
        return;
    }
    let width = frame.area().width.saturating_sub(4).clamp(1, 60);
    let height = notifications.len() as u16;
    let area = Rect {
        x: frame.area().right().saturating_sub(width + 1),
        y: frame.area().bottom().saturating_sub(height + 1),
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let lines = notifications
        .into_iter()
        .map(|notification| {
            let color = match notification.level {
                crate::editor::ToastLevel::Info => Color::Cyan,
                crate::editor::ToastLevel::Success => Color::Green,
                crate::editor::ToastLevel::Warn => Color::Yellow,
                crate::editor::ToastLevel::Error => Color::Red,
            };
            Line::styled(notification.text, Style::default().fg(color).bg(POPUP_BG))
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(POPUP_BG)),
        area,
    );
}

fn draw_confirm(frame: &mut Frame<'_>, message: &str) {
    let width = frame.area().width.saturating_sub(4).clamp(1, 76);
    let area = Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + frame.area().height.saturating_sub(3) / 2,
        width,
        height: 3.min(frame.area().height),
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(message).style(Style::default().fg(Color::Yellow).bg(POPUP_BG)),
        area,
    );
}

fn draw_hover(frame: &mut Frame<'_>, text: &str) {
    let width = frame.area().width.saturating_sub(4).clamp(1, 64);
    let line_count = text.lines().count().max(1) as u16;
    let height = line_count.min(frame.area().height.saturating_sub(2).max(1));
    let area = Rect {
        x: frame.area().right().saturating_sub(width + 2),
        y: frame.area().y + 1,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(FG).bg(POPUP_BG)),
        area,
    );
}

fn draw_rename(frame: &mut Frame<'_>, value: &str) {
    let width = frame.area().width.saturating_sub(4).clamp(1, 56);
    let area = Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + 2,
        width,
        height: 1,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(format!("Rename  {value}")).style(Style::default().fg(FG).bg(POPUP_BG)),
        area,
    );
}

fn draw_hint_guide(frame: &mut Frame<'_>) {
    let entries = [
        "Ctrl+T  Files",
        "Ctrl+P  Commands",
        "Ctrl+G  Buffers",
        "F6      Diff",
        "Ctrl+F  Search",
        "Ctrl+S  Save",
        "Ctrl+\\ Split",
        "Ctrl+@  Shell",
        "F4      Quit",
        "Alt+H   Hide guide",
    ];
    let width = 24.min(frame.area().width);
    let height = entries.len().min(usize::from(frame.area().height)) as u16;
    let area = Rect {
        x: frame.area().right().saturating_sub(width),
        y: frame.area().bottom().saturating_sub(height),
        width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(entries.join("\n")).style(Style::default().fg(FG).bg(POPUP_BG)),
        area,
    );
}

fn draw_completion(frame: &mut Frame<'_>, completion: &crate::editor::CompletionView) {
    let width = frame.area().width.saturating_sub(4).clamp(1, 48);
    let height = (completion.items.len() as u16).clamp(1, frame.area().height.max(1));
    let area = Rect {
        x: frame.area().x + 2,
        y: frame.area().y + 1,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    let lines = completion
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            Line::styled(
                item,
                if index == completion.selected {
                    Style::default().fg(FG).bg(SELECTION)
                } else {
                    Style::default().fg(FG).bg(POPUP_BG)
                },
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(POPUP_BG)),
        area,
    );
}

fn draw_search(frame: &mut Frame<'_>, search: &crate::editor::SearchView) {
    let width = frame.area().width.saturating_sub(4).clamp(1, 76);
    let available_height = frame.area().height.saturating_sub(2).max(1);
    let filter_rows = u16::from(search.scope == crate::editor::SearchScope::Directory) * 2;
    let height = (search.items.len() as u16 + 3 + filter_rows).clamp(2, available_height.max(2));
    let area = Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + 1,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(POPUP_BG)), area);
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
        area,
    );
}

fn draw_start_page(frame: &mut Frame<'_>, area: Rect) {
    let lines = vec![
        Line::styled("my_editor", Style::default().fg(FG)),
        Line::from(""),
        Line::styled("Ctrl+T  ファイルを開く", Style::default().fg(MUTED)),
        Line::styled("Ctrl+P  コマンドを検索", Style::default().fg(MUTED)),
        Line::styled("Alt+H   ヒントガイド", Style::default().fg(MUTED)),
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

fn draw_edge_badge(frame: &mut Frame<'_>, area: Rect, above: bool, diagnostics: usize, git: usize) {
    if diagnostics == 0 && git == 0 || area.width == 0 || area.height == 0 {
        return;
    }
    let arrow = if above { '↑' } else { '↓' };
    let text = format!("{arrow} D:{diagnostics} G:{git}");
    let width = (text.chars().count() as u16).min(area.width);
    let badge = Rect {
        x: area.right().saturating_sub(width),
        y: if above { area.y } else { area.bottom() - 1 },
        width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::Yellow).bg(POPUP_BG)),
        badge,
    );
}

fn draw_picker(frame: &mut Frame<'_>, picker: &crate::editor::PickerView) {
    let width = frame.area().width.saturating_sub(4).clamp(1, 70);
    let available_height = frame.area().height.saturating_sub(2).max(1);
    let height = (picker.items.len() as u16 + 2).clamp(1, available_height);
    let area = Rect {
        x: frame.area().x + (frame.area().width.saturating_sub(width)) / 2,
        y: frame.area().y + 1,
        width,
        height,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(Style::default().bg(POPUP_BG)), area);
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{}  ", picker.title), Style::default().fg(MUTED)),
        Span::styled(&picker.query, Style::default().fg(FG)),
    ])];
    let visible = usize::from(height.saturating_sub(1));
    let start = picker.selected.saturating_sub(visible.saturating_sub(1));
    for (index, item) in picker.items.iter().enumerate().skip(start).take(visible) {
        let style = if index == picker.selected {
            Style::default().fg(FG).bg(SELECTION)
        } else {
            Style::default().fg(FG).bg(POPUP_BG)
        };
        lines.push(Line::styled(item, style));
    }
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(POPUP_BG)),
        area,
    );
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

fn draw_buffer(frame: &mut Frame<'_>, area: Rect, buffer: &ActiveBuffer<'_>) {
    let digits = buffer.text.len_lines().max(1).to_string().len().max(2);
    let gutter_width = digits + 3;
    let visible_width = usize::from(area.width).saturating_sub(gutter_width);
    let start = buffer.view.scroll.top_line;
    let end = (start + usize::from(area.height)).min(buffer.text.len_lines());
    let selections: Vec<_> = buffer
        .view
        .selections
        .iter()
        .map(|selection| selection.range())
        .collect();
    let secondary_carets: Vec<_> = buffer
        .view
        .selections
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != buffer.view.selections.primary_index())
        .map(|(_, selection)| selection.head.0)
        .collect();
    let mut lines = Vec::with_capacity(end.saturating_sub(start));

    for line_index in start..end {
        let severity = buffer
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.line as usize == line_index)
            .map(|diagnostic| diagnostic.severity)
            .min_by_key(|severity| match severity {
                crate::lsp::DiagnosticSeverity::Error => 0,
                crate::lsp::DiagnosticSeverity::Warning => 1,
                crate::lsp::DiagnosticSeverity::Information => 2,
                crate::lsp::DiagnosticSeverity::Hint => 3,
            });
        let (marker, marker_color) = match severity {
            Some(crate::lsp::DiagnosticSeverity::Error) => ("●", Color::Rgb(0xe2, 0x78, 0x78)),
            Some(crate::lsp::DiagnosticSeverity::Warning) => ("▲", Color::Rgb(0xe2, 0xa4, 0x78)),
            _ => (" ", MUTED),
        };
        let (git_marker, git_color) = buffer
            .git_lines
            .iter()
            .find(|git| git.line == line_index)
            .map_or((" ", MUTED), |git| match git.kind {
                crate::editor::GitLineKind::Added => ("+", Color::Rgb(0xb4, 0xbe, 0x82)),
                crate::editor::GitLineKind::Modified => ("~", Color::Rgb(0x84, 0xa0, 0xc6)),
                crate::editor::GitLineKind::Deleted => ("-", Color::Rgb(0xe2, 0x78, 0x78)),
            });
        let mut spans = vec![
            Span::styled(git_marker, Style::default().fg(git_color).bg(BG)),
            Span::styled(marker, Style::default().fg(marker_color).bg(BG)),
            Span::styled(
                format!("{:>digits$} ", line_index + 1),
                Style::default().fg(MUTED).bg(BG),
            ),
        ];
        let line = buffer.text.line(line_index);
        let line_start = buffer.text.line_to_char(line_index);
        let mut display_col = 0;
        for (char_col, character) in line.chars().enumerate() {
            if matches!(character, '\r' | '\n') {
                break;
            }
            let next_col = display_col_after(display_col, character, TAB_SIZE);
            let left = buffer.view.scroll.left_col;
            if next_col > left && display_col < left + visible_width {
                let rendered = if character == '\t' {
                    " ".repeat(next_col - display_col)
                } else {
                    character.to_string()
                };
                let index = line_start + char_col;
                let byte = buffer.text.char_to_byte(index);
                let style = if secondary_carets.contains(&index) {
                    Style::default().fg(BG).bg(FG)
                } else if selections
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
                                        .find(|span| {
                                            span.start_byte <= byte && byte < span.end_byte
                                        })
                                        .map(|span| span.kind.as_str()),
                                )
                            },
                            semantic_color,
                        ))
                        .bg(BG)
                };
                spans.push(Span::styled(rendered, style));
            }
            display_col = next_col;
            if display_col >= left + visible_width {
                break;
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines).style(Style::default().bg(BG)), area);

    let above_diagnostics = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| (diagnostic.line as usize) < start)
        .count();
    let below_diagnostics = buffer
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.line as usize >= end)
        .count();
    let above_git = buffer
        .git_lines
        .iter()
        .filter(|git| git.line < start)
        .count();
    let below_git = buffer
        .git_lines
        .iter()
        .filter(|git| git.line >= end)
        .count();
    draw_edge_badge(frame, area, true, above_diagnostics, above_git);
    draw_edge_badge(frame, area, false, below_diagnostics, below_git);

    let cursor =
        char_idx_to_display_pos(buffer.text, buffer.view.selections.primary().head, TAB_SIZE);
    let cursor_row = cursor.line.saturating_sub(buffer.view.scroll.top_line);
    let cursor_col = cursor.col.saturating_sub(buffer.view.scroll.left_col) + gutter_width;
    if cursor_row < usize::from(area.height) && cursor_col < usize::from(area.width) {
        frame.set_cursor_position((area.x + cursor_col as u16, area.y + cursor_row as u16));
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
        _ => FG,
    }
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, editor: &Editor, buffer: &ActiveBuffer<'_>) {
    if let Some(status) = editor.status() {
        draw_status_text(frame, area, status);
        return;
    }
    let position =
        char_idx_to_display_pos(buffer.text, buffer.view.selections.primary().head, TAB_SIZE);
    let marker = if buffer.modified { "● " } else { "" };
    let external = if buffer.external_changed { "⚠ " } else { "" };
    let cursors = buffer.view.selections.len();
    let cursor_status = if cursors > 1 {
        format!("  {cursors} cursors")
    } else {
        String::new()
    };
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
    let percent = ((position.line + 1) * 100 / buffer.text.len_lines().max(1)).min(100);
    let diagnostic = buffer
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.line as usize == position.line)
        .map_or("", |diagnostic| diagnostic.message.as_str());
    draw_status_text(
        frame,
        area,
        &format!(
            "{external}{marker}{}  E:{errors} W:{warnings} G:{}  Ln {}/{} ({percent}%) Col {}{cursor_status}  {diagnostic}",
            buffer.name,
            buffer.git_lines.len(),
            position.line + 1,
            buffer.text.len_lines(),
            position.col + 1
        ),
    );
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
}
