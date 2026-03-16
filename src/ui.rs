use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, List, ListItem, Paragraph};

use crate::app::{App, NotificationLevel, PromptKind, mode_name, prompt_title};

pub fn render(app: &App, frame: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(if app.prompt.is_some() { 8 } else { 0 }),
        ])
        .split(frame.area());

    let cursor = render_editor(app, frame, chunks[0]);
    render_status(app, frame, chunks[1]);
    if let Some(prompt) = &app.prompt {
        render_prompt(app, prompt.kind, frame, chunks[2]);
        frame.set_cursor_position(prompt_cursor_position(app, chunks[2]));
    } else if let Some(position) = cursor {
        frame.set_cursor_position(position);
    }
    if app.notification.is_some() {
        render_notification(app, frame, chunks[0]);
    }
}

fn render_editor(app: &App, frame: &mut Frame, area: Rect) -> Option<(u16, u16)> {
    let constraints = match app.editor.panes.len() {
        0 | 1 => vec![Constraint::Percentage(100)],
        _ => vec![Constraint::Percentage(50), Constraint::Percentage(50)],
    };
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);
    let mut cursor = None;

    for (index, pane) in app.editor.panes.iter().enumerate() {
        let focused = index == app.editor.focused_pane;
        let pane_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(panes[index]);
        let gutter_width = line_number_width(pane.buffer.lines.len());
        let title = Line::from(vec![
            Span::styled(
                format!(" {} ", index + 1),
                Style::default()
                    .bg(if focused {
                        app.theme.accent
                    } else {
                        app.theme.accent_soft
                    })
                    .fg(app.theme.selection_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                format!(
                    "{}{}",
                    pane.buffer.display_name(),
                    if pane.buffer.dirty { " +" } else { "" }
                ),
                Style::default().fg(if focused {
                    app.theme.text
                } else {
                    app.theme.text_muted
                }),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(title).style(Style::default().bg(app.theme.background)),
            pane_chunks[0],
        );

        let height = pane_chunks[1].height as usize;
        let lines = pane
            .buffer
            .lines
            .iter()
            .skip(pane.scroll_row)
            .take(height)
            .enumerate()
            .map(|(offset, line)| {
                let actual_row = pane.scroll_row + offset;
                let number_style = if focused && actual_row == pane.cursor_row {
                    Style::default()
                        .bg(app.theme.accent_soft)
                        .fg(app.theme.text_muted)
                } else {
                    Style::default()
                        .bg(app.theme.background)
                        .fg(app.theme.text_muted)
                };
                let style = if focused && actual_row == pane.cursor_row {
                    Style::default()
                        .bg(app.theme.accent_soft)
                        .fg(app.theme.text)
                } else {
                    Style::default().bg(app.theme.background).fg(app.theme.text)
                };
                Line::from(vec![
                    Span::styled(
                        format!(
                            "{:>width$} ",
                            actual_row + 1,
                            width = gutter_width as usize - 1
                        ),
                        number_style,
                    ),
                    Span::styled(line.clone(), style),
                ])
            })
            .collect::<Vec<_>>();

        let paragraph = Paragraph::new(lines).style(Style::default().bg(app.theme.background));
        frame.render_widget(paragraph, pane_chunks[1]);

        if focused {
            cursor = Some(editor_cursor_position(pane, pane_chunks[1], gutter_width));
        }
    }

    cursor
}

fn render_status(app: &App, frame: &mut Frame, area: Rect) {
    let pane = app.focused_pane();
    let file_name = pane.buffer.display_name();
    let status = Line::from(vec![
        Span::styled(
            format!(" {} ", mode_name(app.mode)),
            Style::default()
                .bg(app.theme.accent)
                .fg(app.theme.selection_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " pane:{} file:{} dirty:{} cursor:{}:{}  Enter insert  ijkl/Arrows move  Ctrl-F find  Ctrl-P files  Ctrl-B buffers  Ctrl-L symbols  Ctrl-K diagnostics  Ctrl-Q quit",
            app.editor.focused_pane + 1,
            file_name,
            if pane.buffer.dirty { "yes" } else { "no" },
            pane.cursor_row + 1,
            pane.cursor_col + 1
        )),
    ]);
    frame.render_widget(
        Paragraph::new(status).style(
            Style::default()
                .bg(app.theme.background)
                .fg(app.theme.text_muted),
        ),
        area,
    );
}

fn render_prompt(app: &App, kind: PromptKind, frame: &mut Frame, area: Rect) {
    let prompt = app.prompt.as_ref().unwrap();
    let overlay = centered(area, area.width.saturating_sub(4), area.height);
    frame.render_widget(Clear, overlay);
    frame.render_widget(
        Paragraph::new("").style(Style::default().bg(app.theme.background)),
        overlay,
    );

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(1)])
        .split(overlay);

    let header = Line::from(vec![
        Span::styled(
            prompt_title(kind),
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(prompt.query.clone(), Style::default().fg(app.theme.text)),
    ]);
    frame.render_widget(
        Paragraph::new(header).style(Style::default().bg(app.theme.background)),
        layout[0],
    );

    let items = if prompt.items.is_empty() {
        vec![ListItem::new("(no matches)")]
    } else {
        prompt
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let style = if index == prompt.selected {
                    Style::default()
                        .fg(app.theme.selection_fg)
                        .bg(app.theme.selection_bg)
                } else {
                    Style::default().fg(app.theme.text)
                };
                ListItem::new(Line::from(Span::styled(item.label.clone(), style)))
            })
            .collect()
    };
    let list = List::new(items).style(Style::default().bg(app.theme.background));
    frame.render_widget(list, layout[1]);
}

fn line_number_width(total_lines: usize) -> u16 {
    total_lines.max(1).to_string().len() as u16 + 1
}

fn editor_cursor_position(
    pane: &crate::app::PaneState,
    area: Rect,
    gutter_width: u16,
) -> (u16, u16) {
    let visible_row = pane.cursor_row.saturating_sub(pane.scroll_row) as u16;
    let x = area.x + gutter_width + pane.cursor_col as u16;
    let y = area.y + visible_row;
    (
        x.min(area.x + area.width.saturating_sub(1)),
        y.min(area.y + area.height.saturating_sub(1)),
    )
}

fn prompt_cursor_position(app: &App, area: Rect) -> (u16, u16) {
    let prompt = app.prompt.as_ref().unwrap();
    let overlay = centered(area, area.width.saturating_sub(4), area.height);
    let query_offset =
        prompt_title(prompt.kind).chars().count() as u16 + 1 + prompt.query.chars().count() as u16;
    let x = overlay.x + query_offset.min(overlay.width.saturating_sub(1));
    (x, overlay.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_numbers_scale_with_buffer_size() {
        assert_eq!(line_number_width(1), 2);
        assert_eq!(line_number_width(9), 2);
        assert_eq!(line_number_width(10), 3);
        assert_eq!(line_number_width(120), 4);
    }

    #[test]
    fn cursor_position_accounts_for_line_number_gutter() {
        let pane = crate::app::PaneState {
            buffer: crate::app::Buffer {
                path: None,
                lines: vec!["alpha".to_string(); 12],
                dirty: false,
            },
            cursor_row: 3,
            cursor_col: 2,
            scroll_row: 1,
            viewport_height: 8,
        };

        let position = editor_cursor_position(&pane, Rect::new(5, 2, 40, 8), line_number_width(12));

        assert_eq!(position, (10, 4));
    }
}

fn render_notification(app: &App, frame: &mut Frame, area: Rect) {
    let notification = app.notification.as_ref().unwrap();
    let (label, color) = match notification.level {
        NotificationLevel::Info => ("INFO", app.theme.accent),
        NotificationLevel::Warning => ("WARN", Color::Rgb(242, 196, 92)),
        NotificationLevel::Error => ("ERR", Color::Rgb(236, 98, 95)),
    };
    let width = (label.chars().count() as u16 + notification.message.chars().count() as u16 + 1)
        .min(area.width)
        .max(12);
    let height = 1;
    let x = area.x + area.width.saturating_sub(width);
    let y = area.y + area.height.saturating_sub(height);
    let popup = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup);
    let widget = Paragraph::new(Line::from(vec![
        Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            notification.message.clone(),
            Style::default()
                .fg(app.theme.text_muted)
                .add_modifier(Modifier::DIM),
        ),
    ]))
    .alignment(Alignment::Right)
    .style(Style::default().bg(app.theme.background));
    frame.render_widget(widget, popup);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(width)) / 2),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(horizontal[1])[1]
}
