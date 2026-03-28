use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::{
    color::AppColors,
    document::{DocumentRenderLine, SyntaxHighlightKind, SyntaxTokenSpan},
    error::Result,
    mode::Mode,
};

use super::{
    App, FindKind, FocusedPane, LayoutMode, PendingNormalAction, PendingOperator, PickerScope,
    ReplaceField, ReplayableAction,
};

impl App {
    pub(super) fn render_frame(
        &self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<()> {
        terminal.draw(|frame| {
            let area = frame.area();
            let layout = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(area);
            let command_hint = self.command_hint_text();
            let pending_width = (command_hint.chars().count() as u16 + 2).min(layout[1].width);
            let footer_layout =
                Layout::horizontal([Constraint::Min(0), Constraint::Length(pending_width)])
                    .split(layout[1]);
            let footer =
                Paragraph::new(self.footer_line()).style(Style::default().bg(AppColors::PANEL));

            let background = Block::default().style(Style::default().bg(AppColors::BACKGROUND));
            frame.render_widget(background, area);
            self.render_content(frame, layout[0]);
            frame.render_widget(footer, footer_layout[0]);

            let pending = Paragraph::new(format!(" {command_hint} ")).style(
                Style::default()
                    .fg(AppColors::ACCENT)
                    .bg(AppColors::PANEL),
            );
            frame.render_widget(pending, footer_layout[1]);

            if self.go_input.active {
                let popup = centered_rect(24, 3, area);
                let input = Paragraph::new(format!("Go: {}", self.go_input.value))
                    .block(
                        Block::default()
                            .title(" Go ")
                            .borders(Borders::ALL)
                            .style(Style::default().bg(AppColors::PANEL).fg(AppColors::ACCENT)),
                    )
                    .style(Style::default().bg(AppColors::PANEL).fg(AppColors::FOREGROUND));
                frame.render_widget(Clear, popup);
                frame.render_widget(input, popup);
            }

            if self.rename_input.active {
                let popup = centered_rect(36, 3, area);
                let input = Paragraph::new(self.rename_input.value.clone())
                    .block(
                        Block::default()
                            .title(" Rename ")
                            .borders(Borders::ALL)
                            .style(Style::default().bg(AppColors::PANEL).fg(AppColors::ACCENT)),
                    )
                    .style(Style::default().bg(AppColors::PANEL).fg(AppColors::FOREGROUND));
                frame.render_widget(Clear, popup);
                frame.render_widget(input, popup);
            }

            if self.hover_popup.active {
                self.render_hover_popup(frame, area);
            }

            if self.search_input.active {
                let popup = centered_rect(36, 3, area);
                let input = Paragraph::new(self.search_input.value.clone())
                    .block(
                        Block::default()
                            .title(format!(" Search [{}]: ", self.search_input.scope.label()))
                            .borders(Borders::ALL)
                            .style(Style::default().bg(AppColors::PANEL).fg(AppColors::ACCENT)),
                    )
                    .style(Style::default().bg(AppColors::PANEL).fg(AppColors::FOREGROUND));
                frame.render_widget(Clear, popup);
                frame.render_widget(input, popup);
            }

            if self.replace_input.active {
                self.render_replace_popup(frame, area);
            }

            if self.diagnostic_popup.active {
                self.render_diagnostic_popup(frame, area);
            }

            if self.picker.active {
                self.render_picker(frame, area);
            }

            if self.toast.message.is_some() {
                self.render_toast(frame, layout[0]);
            }

            let cursor_position = if self.go_input.active {
                self.go_input_cursor_position(area)
            } else if self.rename_input.active {
                self.rename_input_cursor_position(area)
            } else if self.hover_popup.active {
                self.hover_popup_cursor_position(area)
            } else if self.selection_input.active {
                self.cursor_position(layout[0])
            } else if self.diagnostic_popup.active {
                self.diagnostic_popup_cursor_position(area)
            } else if self.search_input.active {
                self.search_input_cursor_position(area)
            } else if self.replace_input.active {
                self.replace_input_cursor_position(area)
            } else if self.picker.active {
                self.picker_cursor_position(area)
            } else {
                self.cursor_position(layout[0])
            };
            frame.set_cursor_position(cursor_position);
        })?;
        Ok(())
    }

    fn footer_color(&self) -> ratatui::style::Color {
        match self.effective_mode() {
            Mode::Normal => AppColors::NORMAL_MODE,
            Mode::Insert => AppColors::INSERT_MODE,
            Mode::Shell => AppColors::SHELL_MODE,
        }
    }

    fn effective_mode(&self) -> Mode {
        if self.focused_pane == FocusedPane::Right
            && matches!(self.layout_mode, LayoutMode::TerminalSplit | LayoutMode::Single)
        {
            Mode::Shell
        } else {
            self.mode
        }
    }

    fn footer_line(&self) -> Line<'static> {
        let mode = self.effective_mode().label();
        let file_name = self.active_pane_label();
        let status = self.active_pane_status();
        let mode_bg = self.footer_color();
        let footer_bg = AppColors::PANEL;

        Line::from(vec![
            powerline_segment(mode.to_owned(), AppColors::BACKGROUND, mode_bg),
            powerline_separator_left(mode_bg, footer_bg),
            powerline_segment(file_name, AppColors::ACCENT, footer_bg),
            powerline_separator_right(mode_bg),
            powerline_segment(status.to_owned(), AppColors::MUTED, footer_bg),
            powerline_separator_right(mode_bg),
        ])
    }

    fn active_pane_label(&self) -> String {
        if self.focused_pane == FocusedPane::Right
            && matches!(self.layout_mode, LayoutMode::TerminalSplit | LayoutMode::Single)
        {
            return format!("terminal {}", self.shell.program);
        }

        self.active_document_name()
            .unwrap_or_else(|| "nothing".to_owned())
    }

    fn active_pane_status(&self) -> String {
        if self.focused_pane == FocusedPane::Right
            && matches!(self.layout_mode, LayoutMode::TerminalSplit | LayoutMode::Single)
        {
            return "TERMINAL".to_owned();
        }

        let base = self
            .active_document()
            .and_then(|document| {
                document
                    .render_first_page(self.viewport_row, 2, 80)
                    .ok()
                    .map(|render| render.status)
            })
            .unwrap_or_else(|| "NO BUFFER".to_owned());

        match &self.last_save_feedback {
            Some(feedback) => format!("{base} | {feedback}"),
            None => base,
        }
    }

    fn active_document_index(&self) -> Option<usize> {
        if !self.workspace.has_documents() {
            return None;
        }

        match self.layout_mode {
            LayoutMode::TerminalSplit if self.focused_pane == FocusedPane::Right => None,
            LayoutMode::Single if self.focused_pane == FocusedPane::Right => None,
            _ => Some(self.workspace.current_index),
        }
    }

    fn active_document(&self) -> Option<&crate::document::Document> {
        self.active_document_index()
            .and_then(|index| self.workspace.documents.get(index).map(|entry| &entry.document))
    }

    fn active_document_name(&self) -> Option<String> {
        self.active_document_index()
            .and_then(|index| self.workspace.documents.get(index))
            .map(|entry| super::display_name(&entry.path))
    }

    fn render_content(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        match self.layout_mode {
            LayoutMode::Single => {
                if self.focused_pane == FocusedPane::Right {
                    self.render_terminal_pane(frame, area, true);
                } else if !self.workspace.has_documents() {
                    self.render_title_screen(frame, area);
                } else {
                    self.render_document_pane(
                        frame,
                        area,
                        self.workspace.current_index,
                        self.viewport_row,
                        true,
                    );
                }
            }
            LayoutMode::Dual => {
                if !self.workspace.has_documents() {
                    self.render_title_screen(frame, area);
                    return;
                }
                let panes = Layout::horizontal([
                    Constraint::Fill(1),
                    Constraint::Length(1),
                    Constraint::Fill(1),
                ])
                .split(area);
                self.render_split_divider(frame, panes[1]);
                let left_index = self.workspace.documents.get(0).map(|_| 0);
                let right_index = self.workspace.documents.get(1).map(|_| 1);
                if let Some(index) = left_index {
                    self.render_document_pane(
                        frame,
                        panes[0],
                        index,
                        if self.focused_pane == FocusedPane::Left {
                            self.viewport_row
                        } else {
                            0
                        },
                        self.focused_pane == FocusedPane::Left,
                    );
                } else {
                    self.render_placeholder_pane(
                        frame,
                        panes[0],
                        "nothing",
                        self.focused_pane == FocusedPane::Left,
                    );
                }
                if let Some(index) = right_index {
                    self.render_document_pane(
                        frame,
                        panes[2],
                        index,
                        if self.focused_pane == FocusedPane::Right {
                            self.viewport_row
                        } else {
                            0
                        },
                        self.focused_pane == FocusedPane::Right,
                    );
                } else {
                    self.render_placeholder_pane(
                        frame,
                        panes[2],
                        "nothing",
                        self.focused_pane == FocusedPane::Right,
                    );
                }
            }
            LayoutMode::TerminalSplit => {
                let panes = Layout::horizontal([
                    Constraint::Fill(1),
                    Constraint::Length(1),
                    Constraint::Fill(1),
                ])
                .split(area);
                self.render_split_divider(frame, panes[1]);
                if self.workspace.has_documents() {
                    self.render_document_pane(
                        frame,
                        panes[0],
                        self.workspace.current_index,
                        self.viewport_row,
                        self.focused_pane == FocusedPane::Left,
                    );
                } else {
                    self.render_title_screen(frame, panes[0]);
                }
                self.render_terminal_pane(frame, panes[2], self.focused_pane == FocusedPane::Right);
            }
        }
    }

    fn render_document_pane(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        document_index: usize,
        viewport_row: usize,
        focused: bool,
    ) {
        let Some(entry) = self.workspace.documents.get(document_index) else {
            self.render_placeholder_pane(frame, area, "nothing", focused);
            return;
        };
        let indent_width = entry.document.indent_width();
        let render = entry
            .document
            .render_first_page(viewport_row, area.height as usize, area.width as usize)
            .expect("document render should succeed during draw");
        let search_query = if self.search_input.active && document_index == self.workspace.current_index {
            Some(self.search_input.value.as_str())
        } else {
            None
        };
        let pane_background = if focused {
            AppColors::EDITOR_PANE_FOCUSED
        } else {
            AppColors::EDITOR_PANE
        };
        let current_row_in_view = if focused && document_index == self.workspace.current_index {
            self.cursor.row.checked_sub(viewport_row)
        } else {
            None
        };
        let selection_range = if focused && document_index == self.workspace.current_index {
            self.current_selection_range_in_view(viewport_row)
        } else {
            None
        };
        let content = Paragraph::new(format_render_lines(
            &render.lines,
            indent_width,
            search_query,
            current_row_in_view,
            selection_range,
        ))
        .style(
            Style::default()
                .fg(AppColors::FOREGROUND)
                .bg(pane_background),
        );
        frame.render_widget(content, area);
    }

    fn render_placeholder_pane(
        &self,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        label: &str,
        focused: bool,
    ) {
        let widget = Paragraph::new(label.to_owned()).style(
            Style::default()
                .fg(if focused { AppColors::ACCENT } else { AppColors::MUTED })
                .bg(if focused {
                    AppColors::EDITOR_PANE_FOCUSED
                } else {
                    AppColors::EDITOR_PANE
                }),
        );
        frame.render_widget(widget, area);
    }

    fn render_terminal_pane(&self, frame: &mut ratatui::Frame<'_>, area: Rect, focused: bool) {
        let lines = self.terminal_screen_lines(area);
        let widget = Paragraph::new(lines)
        .style(
            Style::default()
                .fg(if focused {
                    AppColors::SHELL_MODE
                } else {
                    AppColors::FOREGROUND
                })
                .bg(AppColors::BACKGROUND),
        );
        frame.render_widget(widget, area);
    }

    fn render_split_divider(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let divider = Paragraph::new(vec![Line::from(" "); area.height as usize]).style(
            Style::default().bg(AppColors::SPLIT_DIVIDER),
        );
        frame.render_widget(divider, area);
    }

    fn render_title_screen(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(AppColors::PANEL_ALT))
            .style(Style::default().bg(AppColors::BACKGROUND));
        frame.render_widget(block, area);

        let art = vec![Line::from(Span::styled(
            "Press Ctrl-P to open a file.",
            Style::default().fg(AppColors::ACCENT),
        ))];

        let inner = Rect::new(
            area.x.saturating_add(2),
            area.y.saturating_add(1),
            area.width.saturating_sub(4),
            area.height.saturating_sub(2),
        );
        let widget = Paragraph::new(art)
            .style(Style::default().fg(AppColors::FOREGROUND).bg(AppColors::BACKGROUND));
        frame.render_widget(widget, inner);
    }

    fn render_picker(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let popup = centered_rect(72, 12, area);
        let matches = self.ranked_picker_matches();
        let scope = match self.picker.scope {
            PickerScope::All => "all",
            PickerScope::Buffers => "buffers",
        };

        let mut lines = vec![Line::from(self.picker.query.clone())];
        for (index, matched) in matches.into_iter().take(8).enumerate() {
            let prefix = if index == 0 { "> " } else { "  " };
            let (kind, display_name) = match matched.candidate {
                crate::open_candidate::OpenCandidate::OpenBuffer(candidate) => {
                    ("[buf] ", candidate.display_name)
                }
                crate::open_candidate::OpenCandidate::ProjectFile(candidate) => {
                    ("[file] ", candidate.display_name)
                }
            };
            let mut spans = vec![Span::styled(
                prefix,
                if index == 0 {
                    Style::default().fg(AppColors::ACCENT)
                } else {
                    Style::default().fg(AppColors::MUTED)
                },
            )];
            spans.push(Span::styled(kind, Style::default().fg(AppColors::MUTED)));
            spans.extend(highlight_fuzzy_match(&display_name, &matched.indices));
            lines.push(Line::from(spans));
        }

        let widget = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(format!(" Open [{scope}] "))
                    .borders(Borders::ALL)
                    .style(Style::default().bg(AppColors::PANEL).fg(AppColors::ACCENT)),
            )
            .style(Style::default().bg(AppColors::PANEL).fg(AppColors::FOREGROUND));
        frame.render_widget(Clear, popup);
        frame.render_widget(widget, popup);
    }

    fn render_replace_popup(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let popup = centered_rect(64, 9, area);
        let inner = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(popup);
        let find_style = if self.replace_input.field == ReplaceField::Find {
            Style::default().fg(AppColors::ACCENT)
        } else {
            Style::default().fg(AppColors::FOREGROUND)
        };
        let replace_style = if self.replace_input.field == ReplaceField::Replace {
            Style::default().fg(AppColors::ACCENT)
        } else {
            Style::default().fg(AppColors::FOREGROUND)
        };
        let frame_widget = Block::default()
            .title(format!(" Replace [{}] ", self.replace_input.scope.label()))
            .borders(Borders::ALL)
            .style(Style::default().bg(AppColors::PANEL).fg(AppColors::ACCENT));
        frame.render_widget(Clear, popup);
        frame.render_widget(frame_widget, popup);

        let from = Paragraph::new(self.replace_input.find.clone())
            .block(
                Block::default()
                    .title(" From ")
                    .borders(Borders::ALL)
                    .style(Style::default().bg(AppColors::PANEL).fg(if self.replace_input.field == ReplaceField::Find {
                        AppColors::ACCENT
                    } else {
                        AppColors::MUTED
                    })),
            )
            .style(find_style.bg(AppColors::PANEL))
            .wrap(Wrap { trim: false });
        let to = Paragraph::new(self.replace_input.replace.clone())
            .block(
                Block::default()
                    .title(" To ")
                    .borders(Borders::ALL)
                    .style(Style::default().bg(AppColors::PANEL).fg(if self.replace_input.field == ReplaceField::Replace {
                        AppColors::ACCENT
                    } else {
                        AppColors::MUTED
                    })),
            )
            .style(replace_style.bg(AppColors::PANEL))
            .wrap(Wrap { trim: false });
        frame.render_widget(from, inner[0]);
        frame.render_widget(to, inner[1]);
    }

    fn render_diagnostic_popup(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let height = (self.diagnostic_popup.lines.len() as u16 + 2).clamp(3, 10);
        let popup = centered_rect(72, height, area);
        let mut lines = self
            .diagnostic_popup
            .lines
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(Line::from(""));
        }

        let widget = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Diagnostics ")
                    .borders(Borders::ALL)
                    .style(Style::default().bg(AppColors::PANEL).fg(AppColors::ACCENT)),
            )
            .style(Style::default().bg(AppColors::PANEL).fg(AppColors::FOREGROUND));
        frame.render_widget(Clear, popup);
        frame.render_widget(widget, popup);
    }

    fn render_hover_popup(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let height = (self.hover_popup.lines.len() as u16 + 2).clamp(3, 12);
        let popup = centered_rect(72, height, area);
        let mut lines = self
            .hover_popup
            .lines
            .iter()
            .map(|line| Line::from(line.clone()))
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(Line::from(""));
        }

        let widget = Paragraph::new(lines)
            .block(
                Block::default()
                    .title(" Hover ")
                    .borders(Borders::ALL)
                    .style(Style::default().bg(AppColors::PANEL).fg(AppColors::ACCENT)),
            )
            .style(Style::default().bg(AppColors::PANEL).fg(AppColors::FOREGROUND));
        frame.render_widget(Clear, popup);
        frame.render_widget(widget, popup);
    }

    fn render_toast(&self, frame: &mut ratatui::Frame<'_>, area: Rect) {
        let Some(message) = &self.toast.message else {
            return;
        };

        let width = (message.chars().count() as u16 + 4).clamp(12, area.width.max(12));
        let x = area
            .x
            .saturating_add(area.width.saturating_sub(width));
        let y = area
            .y
            .saturating_add(area.height.saturating_sub(1));
        let popup = Rect::new(x, y, width.min(area.width), 1);
        let widget = Paragraph::new(format!(" {message} ")).style(
            Style::default()
                .fg(AppColors::ACCENT)
                .bg(AppColors::PANEL_ALT),
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(widget, popup);
    }

    fn pending_input_text(&self) -> Option<String> {
        match self.pending_normal_action {
            Some(PendingNormalAction::GoPrefix) => Some("g".to_owned()),
            Some(PendingNormalAction::DiagnosticPrefix) => Some("e".to_owned()),
            Some(PendingNormalAction::Find(FindKind::Forward)) => Some("f".to_owned()),
            Some(PendingNormalAction::Find(FindKind::Backward)) => Some("F".to_owned()),
            Some(PendingNormalAction::Find(FindKind::TillForward)) => Some("t".to_owned()),
            Some(PendingNormalAction::Find(FindKind::TillBackward)) => Some("T".to_owned()),
            Some(PendingNormalAction::Operator(PendingOperator::Change)) => Some("c".to_owned()),
            Some(PendingNormalAction::Operator(PendingOperator::Delete)) => Some("d".to_owned()),
            Some(PendingNormalAction::Operator(PendingOperator::Yank)) => Some("y".to_owned()),
            Some(PendingNormalAction::OperatorFind(PendingOperator::Change, FindKind::Forward)) => {
                Some("cf".to_owned())
            }
            Some(PendingNormalAction::OperatorFind(PendingOperator::Change, FindKind::Backward)) => {
                Some("cF".to_owned())
            }
            Some(PendingNormalAction::OperatorFind(
                PendingOperator::Change,
                FindKind::TillForward,
            )) => Some("ct".to_owned()),
            Some(PendingNormalAction::OperatorFind(
                PendingOperator::Change,
                FindKind::TillBackward,
            )) => Some("cT".to_owned()),
            Some(PendingNormalAction::OperatorFind(PendingOperator::Delete, FindKind::Forward)) => {
                Some("df".to_owned())
            }
            Some(PendingNormalAction::OperatorFind(PendingOperator::Delete, FindKind::Backward)) => {
                Some("dF".to_owned())
            }
            Some(PendingNormalAction::OperatorFind(
                PendingOperator::Delete,
                FindKind::TillForward,
            )) => Some("dt".to_owned()),
            Some(PendingNormalAction::OperatorFind(
                PendingOperator::Delete,
                FindKind::TillBackward,
            )) => Some("dT".to_owned()),
            Some(PendingNormalAction::OperatorFind(PendingOperator::Yank, FindKind::Forward)) => {
                Some("yf".to_owned())
            }
            Some(PendingNormalAction::OperatorFind(PendingOperator::Yank, FindKind::Backward)) => {
                Some("yF".to_owned())
            }
            Some(PendingNormalAction::OperatorFind(PendingOperator::Yank, FindKind::TillForward)) => {
                Some("yt".to_owned())
            }
            Some(PendingNormalAction::OperatorFind(PendingOperator::Yank, FindKind::TillBackward)) => {
                Some("yT".to_owned())
            }
            None => None,
        }
    }

    fn replay_command_text(&self) -> Option<String> {
        match self.last_replayable_action {
            Some(ReplayableAction::GitHunk { forward: true }) => Some("gg".to_owned()),
            Some(ReplayableAction::GitHunk { forward: false }) => Some("gG".to_owned()),
            Some(ReplayableAction::Find(FindKind::Forward, target)) => Some(format!("f{target}")),
            Some(ReplayableAction::Find(FindKind::Backward, target)) => Some(format!("F{target}")),
            Some(ReplayableAction::Find(FindKind::TillForward, target)) => Some(format!("t{target}")),
            Some(ReplayableAction::Find(FindKind::TillBackward, target)) => Some(format!("T{target}")),
            Some(ReplayableAction::Diagnostic {
                error_only: false,
                forward: true,
            }) => Some("gw".to_owned()),
            Some(ReplayableAction::Diagnostic {
                error_only: false,
                forward: false,
            }) => Some("gW".to_owned()),
            Some(ReplayableAction::Diagnostic {
                error_only: true,
                forward: true,
            }) => Some("ge".to_owned()),
            Some(ReplayableAction::Diagnostic {
                error_only: true,
                forward: false,
            }) => Some("gE".to_owned()),
            Some(ReplayableAction::Search { forward: true }) => Some("gf".to_owned()),
            Some(ReplayableAction::Search { forward: false }) => Some("gF".to_owned()),
            None => None,
        }
    }

    fn command_hint_text(&self) -> String {
        if self.go_input.active {
            return "<buffer|replay>".to_owned();
        }

        let buffer_label = match self.pending_input_text() {
            Some(pending) => format!("buffer {pending}"),
            None => "buffer".to_owned(),
        };
        let replay_label = match self.replay_command_text() {
            Some(replay) => format!("replay {replay}"),
            None => "replay".to_owned(),
        };

        format!("<{buffer_label}|{replay_label}>")
    }

    fn cursor_position(&self, area: Rect) -> Position {
        if !self.workspace.has_documents()
            && !(self.focused_pane == FocusedPane::Right
                && matches!(self.layout_mode, LayoutMode::TerminalSplit | LayoutMode::Single))
        {
            return Position::new(area.x.saturating_add(1), area.y.saturating_add(1));
        }

        let pane_area = match self.layout_mode {
            LayoutMode::Single => area,
            LayoutMode::Dual | LayoutMode::TerminalSplit => {
                let panes = Layout::horizontal([
                    Constraint::Fill(1),
                    Constraint::Length(1),
                    Constraint::Fill(1),
                ])
                .split(area);
                match self.focused_pane {
                    FocusedPane::Left => panes[0],
                    FocusedPane::Right => panes[2],
                }
            }
        };

        if self.focused_pane == FocusedPane::Right
            && matches!(self.layout_mode, LayoutMode::TerminalSplit | LayoutMode::Single)
        {
            let Some(parser) = &self.shell.parser else {
                return Position::new(pane_area.x, pane_area.y);
            };
            let (row, col) = parser.screen().cursor_position();
            return Position::new(
                pane_area.x.saturating_add(col.min(pane_area.width.saturating_sub(1))),
                pane_area.y.saturating_add(row.min(pane_area.height.saturating_sub(1))),
            );
        }

        let line_width = self
            .active_document()
            .and_then(|document| {
                document
                    .display_line_width(self.cursor.row, self.current_page_width())
                    .ok()
            })
            .unwrap_or(0);
        let column = self.cursor.column.min(line_width);
        let relative_row = self.cursor.row.saturating_sub(self.viewport_row);
        Position::new(
            pane_area.x.saturating_add(11).saturating_add(column as u16),
            pane_area.y.saturating_add(relative_row as u16),
        )
    }

    fn go_input_cursor_position(&self, area: Rect) -> Position {
        let popup = centered_rect(24, 3, area);
        Position::new(
            popup.x
                .saturating_add(5 + self.go_input.value.chars().count() as u16),
            popup.y.saturating_add(1),
        )
    }

    fn search_input_cursor_position(&self, area: Rect) -> Position {
        let popup = centered_rect(36, 3, area);
        Position::new(
            popup.x
                .saturating_add(1 + self.search_input.value.chars().count() as u16),
            popup.y.saturating_add(1),
        )
    }

    fn replace_input_cursor_position(&self, area: Rect) -> Position {
        let popup = centered_rect(64, 9, area);
        let inner = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(popup);
        let content_width = inner[0].width.saturating_sub(2).max(1);
        match self.replace_input.field {
            ReplaceField::Find => wrapped_text_cursor_position(inner[0], &self.replace_input.find, content_width),
            ReplaceField::Replace => {
                wrapped_text_cursor_position(inner[1], &self.replace_input.replace, content_width)
            }
        }
    }

    fn current_selection_range_in_view(&self, viewport_row: usize) -> Option<(usize, usize, usize, usize)> {
        let range = self.selection_input.current_range()?;
        let start_row = range.start_row.checked_sub(viewport_row)?;
        let end_row = range.end_row.checked_sub(viewport_row)?;
        Some((start_row, range.start_column, end_row, range.end_column))
    }

    fn rename_input_cursor_position(&self, area: Rect) -> Position {
        let popup = centered_rect(36, 3, area);
        Position::new(
            popup.x
                .saturating_add(1 + self.rename_input.value.chars().count() as u16),
            popup.y.saturating_add(1),
        )
    }

    fn picker_cursor_position(&self, area: Rect) -> Position {
        let popup = centered_rect(72, 12, area);
        Position::new(
            popup.x
                .saturating_add(1 + self.picker.query.chars().count() as u16),
            popup.y.saturating_add(1),
        )
    }

    fn diagnostic_popup_cursor_position(&self, area: Rect) -> Position {
        let popup = centered_rect(72, 3, area);
        Position::new(popup.x.saturating_add(1), popup.y.saturating_add(1))
    }

    fn hover_popup_cursor_position(&self, area: Rect) -> Position {
        let popup = centered_rect(72, 3, area);
        Position::new(popup.x.saturating_add(1), popup.y.saturating_add(1))
    }

    fn terminal_screen_lines(&self, area: Rect) -> Vec<Line<'static>> {
        let Some(parser) = &self.shell.parser else {
            return vec![Line::from("")];
        };

        let screen = parser.screen();
        let rows = area.height.max(1);
        let cols = area.width.max(1);
        let mut lines = Vec::with_capacity(rows as usize);

        for row in 0..rows {
            let mut spans = Vec::new();
            let mut current_text = String::new();
            let mut current_style = None::<Style>;

            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }

                let mut style = vt100_cell_style(cell);
                if style.fg.is_none() {
                    style = style.fg(AppColors::FOREGROUND);
                }
                if style.bg.is_none() {
                    style = style.bg(AppColors::BACKGROUND);
                }
                let text = if cell.has_contents() {
                    cell.contents()
                } else {
                    " ".to_owned()
                };

                if current_style == Some(style) {
                    current_text.push_str(&text);
                } else {
                    if !current_text.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut current_text), current_style.unwrap_or_default()));
                    }
                    current_text = text;
                    current_style = Some(style);
                }
            }

            if !current_text.is_empty() {
                spans.push(Span::styled(current_text, current_style.unwrap_or_default()));
            }
            if spans.is_empty() {
                spans.push(Span::raw(" "));
            }
            lines.push(Line::from(spans));
        }

        lines
    }
}

fn vt100_cell_style(cell: &vt100::Cell) -> Style {
    let mut fg = vt100_to_ratatui_color(cell.fgcolor());
    let mut bg = vt100_to_ratatui_color(cell.bgcolor());
    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }

    let mut style = Style::default();
    if let Some(fg) = fg {
        style = style.fg(fg);
    }
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

fn vt100_to_ratatui_color(color: vt100::Color) -> Option<ratatui::style::Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Idx(index) => Some(ratatui::style::Color::Indexed(index)),
        vt100::Color::Rgb(r, g, b) => Some(ratatui::style::Color::Rgb(r, g, b)),
    }
}

fn format_render_lines(
    lines: &[DocumentRenderLine],
    indent_width: usize,
    search_query: Option<&str>,
    current_row_in_view: Option<usize>,
    selection_range: Option<(usize, usize, usize, usize)>,
) -> Vec<Line<'static>> {
    let mut formatted_lines = Vec::with_capacity(lines.len());
    let mut previous_guide_width = 0usize;

    for (index, line) in lines.iter().enumerate() {
        let current_guide_width = if line.text.is_empty() {
            previous_guide_width
        } else {
            line.text.chars().take_while(|ch| *ch == ' ').count()
        };

        formatted_lines.push(format_render_line(
            line,
            indent_width,
            current_guide_width,
            search_query,
            current_row_in_view == Some(index),
            selection_range.map(|(start_row, start_col, end_row, end_col)| {
                (index, start_row, start_col, end_row, end_col)
            }),
            &line.syntax_spans,
        ));
        previous_guide_width = current_guide_width;
    }

    formatted_lines
}

fn format_render_line(
    line: &DocumentRenderLine,
    indent_width: usize,
    empty_line_guide_width: usize,
    search_query: Option<&str>,
    current_row: bool,
    selection_context: Option<(usize, usize, usize, usize, usize)>,
    syntax_spans: &[SyntaxTokenSpan],
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            format!("{:>1}", line.diagnostic_marker),
            Style::default().fg(diagnostic_color(&line.diagnostic_marker)),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:>6}", line.line_number),
            Style::default().fg(if current_row {
                AppColors::CURRENT_LINE_NUMBER
            } else {
                AppColors::MUTED
            }),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:>1}", line.gutter_marker),
            Style::default().fg(git_gutter_color(&line.gutter_marker)),
        ),
        Span::raw(" "),
    ];
    spans.extend(render_text_with_indent_guides(
        &line.text,
        indent_width,
        empty_line_guide_width,
        search_query,
        selection_context,
        syntax_spans,
    ));
    Line::from(spans)
}

fn render_text_with_indent_guides(
    text: &str,
    indent_width: usize,
    empty_line_guide_width: usize,
    search_query: Option<&str>,
    selection_context: Option<(usize, usize, usize, usize, usize)>,
    syntax_spans: &[SyntaxTokenSpan],
) -> Vec<Span<'static>> {
    let leading_spaces = text.chars().take_while(|ch| *ch == ' ').count();
    let guide_width = indent_width.max(1);

    if leading_spaces == 0 && empty_line_guide_width == 0 {
        return render_search_highlighted_text(text, search_query, selection_context, syntax_spans);
    }

    let mut spans = Vec::new();
    let visual_indent_width = if leading_spaces == 0 {
        empty_line_guide_width
    } else {
        leading_spaces
    };
    let guide_count = visual_indent_width / guide_width;
    let trailing_spaces = visual_indent_width % guide_width;

    for _ in 0..guide_count {
        spans.push(Span::styled(
            format!("\u{2502}{}", " ".repeat(guide_width.saturating_sub(1))),
            Style::default().fg(AppColors::INDENT_GUIDE),
        ));
    }

    if trailing_spaces > 0 {
        spans.push(Span::raw(" ".repeat(trailing_spaces)));
    }

    if !text.is_empty() {
        spans.extend(render_search_highlighted_text(
            &text.chars().skip(leading_spaces).collect::<String>(),
            search_query,
            selection_context.map(|(index, start_row, start_col, end_row, end_col)| {
                let selection = adjusted_selection_for_trimmed_prefix(
                    index,
                    start_row,
                    start_col,
                    end_row,
                    end_col,
                    leading_spaces,
                );
                selection
            }),
            syntax_spans,
        ));
    }

    spans
}

fn render_search_highlighted_text(
    text: &str,
    search_query: Option<&str>,
    selection_context: Option<(usize, usize, usize, usize, usize)>,
    syntax_spans: &[SyntaxTokenSpan],
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut search_matches = Vec::<(usize, usize)>::new();
    if let Some(query) = search_query.filter(|query| !query.is_empty()) {
        let mut offset = 0usize;
        let mut remaining = text;
        while let Some(index) = remaining.find(query) {
            let start = offset + index;
            let end = start + query.len();
            search_matches.push((start, end));
            offset = end;
            remaining = &text[offset..];
        }
    }

    let selection_bounds = selection_context.and_then(selection_bounds_for_row);
    let chars: Vec<char> = text.chars().collect();
    let mut current = String::new();
    let mut current_style = None::<Style>;

    for (index, ch) in chars.iter().enumerate() {
        let in_search = search_matches
            .iter()
            .any(|(start, end)| index >= *start && index < *end);
        let in_selection = selection_bounds
            .is_some_and(|(start, end)| index >= start && index < end);
        let syntax_kind = syntax_spans.iter().find_map(|span| {
            let end = span.start.saturating_add(span.length);
            (index >= span.start && index < end).then_some(span.kind)
        });

        let mut style = Style::default().fg(syntax_color(syntax_kind));
        if in_selection {
            style = style.bg(AppColors::SELECTION_HIGHLIGHT);
        }
        if in_search {
            style = style.fg(AppColors::BACKGROUND).bg(AppColors::SEARCH_HIGHLIGHT);
        }

        match current_style {
            Some(existing) if existing == style => current.push(*ch),
            Some(existing) => {
                spans.push(Span::styled(std::mem::take(&mut current), existing));
                current.push(*ch);
                current_style = Some(style);
            }
            None => {
                current.push(*ch);
                current_style = Some(style);
            }
        }
    }

    if !current.is_empty() {
        spans.push(Span::styled(
            current,
            current_style.unwrap_or_else(|| Style::default().fg(AppColors::FOREGROUND)),
        ));
    }

    if spans.is_empty() {
        spans.push(Span::styled(text.to_owned(), Style::default().fg(AppColors::FOREGROUND)));
    }

    spans
}

fn adjusted_selection_for_trimmed_prefix(
    index: usize,
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
    leading_spaces: usize,
) -> (usize, usize, usize, usize, usize) {
    let start_col = if index == start_row {
        start_col.saturating_sub(leading_spaces)
    } else {
        start_col
    };
    let end_col = if index == end_row {
        end_col.saturating_sub(leading_spaces)
    } else {
        end_col
    };
    (index, start_row, start_col, end_row, end_col)
}

fn selection_bounds_for_row(
    selection_context: (usize, usize, usize, usize, usize),
) -> Option<(usize, usize)> {
    let (index, start_row, start_col, end_row, end_col) = selection_context;
    if index < start_row || index > end_row {
        return None;
    }
    if start_row == end_row {
        return Some((start_col, end_col));
    }
    if index == start_row {
        return Some((start_col, usize::MAX));
    }
    if index == end_row {
        return Some((0, end_col));
    }
    Some((0, usize::MAX))
}

fn wrapped_text_cursor_position(area: Rect, text: &str, content_width: u16) -> Position {
    let width = content_width.max(1) as usize;
    let chars = text.chars().count();
    let wrapped_row = chars / width;
    let wrapped_col = chars % width;
    Position::new(
        area.x.saturating_add(1 + wrapped_col as u16),
        area.y.saturating_add(1 + wrapped_row as u16).min(area.y.saturating_add(area.height.saturating_sub(2))),
    )
}

fn highlight_fuzzy_match(text: &str, indices: &[usize]) -> Vec<Span<'static>> {
    if indices.is_empty() {
        return vec![Span::styled(
            text.to_owned(),
            Style::default().fg(AppColors::FOREGROUND),
        )];
    }

    let highlighted: std::collections::HashSet<usize> = indices.iter().copied().collect();
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut current_highlight = None;

    for (index, ch) in text.chars().enumerate() {
        let is_highlighted = highlighted.contains(&index);
        match current_highlight {
            Some(active) if active == is_highlighted => current.push(ch),
            Some(active) => {
                spans.push(Span::styled(
                    current.clone(),
                    if active {
                        Style::default().fg(AppColors::SEARCH_HIGHLIGHT)
                    } else {
                        Style::default().fg(AppColors::FOREGROUND)
                    },
                ));
                current.clear();
                current.push(ch);
                current_highlight = Some(is_highlighted);
            }
            None => {
                current.push(ch);
                current_highlight = Some(is_highlighted);
            }
        }
    }

    if let Some(active) = current_highlight {
        spans.push(Span::styled(
            current,
            if active {
                Style::default().fg(AppColors::SEARCH_HIGHLIGHT)
            } else {
                Style::default().fg(AppColors::FOREGROUND)
            },
        ));
    }

    spans
}

fn powerline_segment(
    text: String,
    foreground: ratatui::style::Color,
    background: ratatui::style::Color,
) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::default().fg(foreground).bg(background),
    )
}

fn git_gutter_color(marker: &str) -> ratatui::style::Color {
    match marker {
        "A" => AppColors::GIT_ADDED,
        "M" => AppColors::GIT_MODIFIED,
        "D" => AppColors::GIT_REMOVED,
        _ => AppColors::MUTED,
    }
}

fn diagnostic_color(marker: &str) -> ratatui::style::Color {
    match marker {
        "W" => AppColors::DIAGNOSTIC_WARNING,
        "E" => AppColors::DIAGNOSTIC_ERROR,
        _ => AppColors::MUTED,
    }
}

fn syntax_color(kind: Option<SyntaxHighlightKind>) -> ratatui::style::Color {
    match kind {
        Some(SyntaxHighlightKind::Keyword) => AppColors::SYNTAX_KEYWORD,
        Some(SyntaxHighlightKind::String) => AppColors::SYNTAX_STRING,
        Some(SyntaxHighlightKind::Comment) => AppColors::SYNTAX_COMMENT,
        Some(SyntaxHighlightKind::Type) => AppColors::SYNTAX_TYPE,
        Some(SyntaxHighlightKind::Function) => AppColors::SYNTAX_FUNCTION,
        Some(SyntaxHighlightKind::Variable) => AppColors::SYNTAX_VARIABLE,
        Some(SyntaxHighlightKind::Parameter) => AppColors::SYNTAX_PARAMETER,
        Some(SyntaxHighlightKind::Number) => AppColors::SYNTAX_NUMBER,
        Some(SyntaxHighlightKind::Operator) => AppColors::SYNTAX_OPERATOR,
        Some(SyntaxHighlightKind::Macro) => AppColors::SYNTAX_MACRO,
        Some(SyntaxHighlightKind::Namespace) => AppColors::SYNTAX_NAMESPACE,
        Some(SyntaxHighlightKind::Property) => AppColors::SYNTAX_PROPERTY,
        None => AppColors::FOREGROUND,
    }
}

fn powerline_separator_left(
    left_background: ratatui::style::Color,
    right_background: ratatui::style::Color,
) -> Span<'static> {
    Span::styled(
        "\u{e0b0}",
        Style::default().fg(left_background).bg(right_background),
    )
}

fn powerline_separator_right(foreground: ratatui::style::Color) -> Span<'static> {
    Span::styled(
        format!(" {} ", '\u{e0b1}'),
        Style::default().fg(foreground).bg(AppColors::PANEL),
    )
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let y = area.y.saturating_add(area.height.saturating_sub(height) / 2);
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
