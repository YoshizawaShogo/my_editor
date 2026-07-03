use std::time::Instant;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};

use crate::editor::{AppEvent, Command, Direction, Focus, MouseInput, Unit, VerticalDirection};

use super::KeyChordState;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RawInput {
    Key { event: KeyEvent, at: Instant },
    Paste(String),
    Mouse { event: MouseEvent, at: Instant },
    Resize { cols: u16, rows: u16 },
    ReadError(String),
}

impl TryFrom<Event> for RawInput {
    type Error = ();

    fn try_from(event: Event) -> Result<Self, ()> {
        match event {
            Event::Key(event) => Ok(Self::Key {
                event,
                at: Instant::now(),
            }),
            Event::Mouse(event) => Ok(Self::Mouse {
                event,
                at: Instant::now(),
            }),
            Event::Resize(cols, rows) => Ok(Self::Resize { cols, rows }),
            Event::Paste(text) => Ok(Self::Paste(text)),
            Event::FocusGained | Event::FocusLost => Err(()),
        }
    }
}

pub fn translate(raw: RawInput, _focus: &Focus, pending: &mut KeyChordState) -> Option<AppEvent> {
    pending.clear();
    match raw {
        RawInput::Key { event: key, .. }
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
        {
            None
        }
        RawInput::Key { event: key, .. } if key.code == KeyCode::F(4) => Some(Command::Quit.into()),
        RawInput::Key { event: key, .. } if key.code == KeyCode::F(6) => {
            Some(Command::OpenDiffPicker.into())
        }
        RawInput::Key { event: key, at } => translate_key(key, at, _focus),
        RawInput::Paste(text) => Some(AppEvent::TextPaste(text)),
        RawInput::Mouse { event, at } => {
            let clicks = match event.kind {
                MouseEventKind::Down(button) => {
                    pending.register_click(at, event.column, event.row, button)
                }
                _ => 0,
            };
            Some(AppEvent::Mouse(MouseInput { event, clicks }))
        }
        RawInput::Resize { cols, rows } => Some(AppEvent::Resize { cols, rows }),
        RawInput::ReadError(message) => Some(AppEvent::Error(message)),
    }
}

fn translate_key(key: KeyEvent, at: Instant, focus: &Focus) -> Option<AppEvent> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if ctrl && key.code == KeyCode::Char('p') {
        return Some(Command::OpenCommandPalette.into());
    }
    if ctrl && key.code == KeyCode::Char('g') {
        return Some(Command::OpenBufferPicker.into());
    }
    if ctrl && key.code == KeyCode::Char('t') {
        return Some(Command::OpenDirectoryPicker.into());
    }
    if ctrl && shift && key.code == KeyCode::Char('f') {
        return Some(Command::OpenSearchInDirectory.into());
    }
    if ctrl && key.code == KeyCode::Char('f') {
        return Some(Command::OpenSearch.into());
    }
    if ctrl && key.code == KeyCode::Char('h') {
        return Some(Command::OpenReplace.into());
    }
    if ctrl
        && matches!(
            key.code,
            KeyCode::Char(' ') | KeyCode::Char('@') | KeyCode::Null
        )
    {
        return Some(Command::ToggleCompletion.into());
    }
    if ctrl && key.code == KeyCode::Char('o') {
        return Some(Command::ToggleShell.into());
    }
    if ctrl && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('5')) {
        return Some(Command::ToggleSplit.into());
    }
    if matches!(focus, Focus::Overlay | Focus::Completion(_))
        && ctrl
        && key.code == KeyCode::Char('c')
    {
        return Some(Command::PickerCancel.into());
    }
    if matches!(focus, Focus::Shell) {
        if ctrl && key.code == KeyCode::Char('c') {
            return Some(Command::CopyShellSelection.into());
        }
        return shell_key(key).map(AppEvent::TerminalInput);
    }
    if matches!(focus, Focus::Completion(_)) {
        match key.code {
            KeyCode::Esc => return Some(Command::PickerCancel.into()),
            KeyCode::Enter => return Some(Command::PickerConfirm.into()),
            KeyCode::Up => return Some(Command::PickerUp.into()),
            KeyCode::Down => return Some(Command::PickerDown.into()),
            _ => {}
        }
    }
    if matches!(focus, Focus::Overlay) {
        return match key.code {
            KeyCode::Esc => Some(Command::PickerCancel.into()),
            KeyCode::Enter => Some(Command::PickerConfirm.into()),
            KeyCode::Up => Some(Command::PickerUp.into()),
            KeyCode::Down => Some(Command::PickerDown.into()),
            KeyCode::Backspace => Some(Command::PickerBackspace.into()),
            KeyCode::Tab => Some(Command::SearchToggleField.into()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(Command::SearchToggleCase.into())
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(Command::SearchToggleWholeWord.into())
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(Command::SearchToggleRegex.into())
            }
            KeyCode::Char('i') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(Command::SearchToggleIgnore.into())
            }
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::ALT) => {
                Some(Command::SearchToggleHidden.into())
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(AppEvent::TextInput(character))
            }
            _ => None,
        };
    }
    if !matches!(focus, Focus::Editor(_) | Focus::Completion(_)) {
        return None;
    }
    match key.code {
        KeyCode::F(2) => Some(Command::Rename.into()),
        KeyCode::Char('z') if ctrl => Some(Command::Undo.into()),
        KeyCode::Char('y') if ctrl => Some(Command::Redo.into()),
        KeyCode::Char('a') if ctrl => Some(Command::SelectAll.into()),
        KeyCode::Char('d') if ctrl => Some(Command::SelectNextOccurrence.into()),
        KeyCode::Char('c') if ctrl => Some(Command::Copy.into()),
        KeyCode::Char('x') if ctrl => Some(Command::Cut.into()),
        KeyCode::Char('v') if ctrl => Some(Command::Paste.into()),
        KeyCode::Char('s') if ctrl => Some(Command::Save.into()),
        KeyCode::Char('w') if ctrl => Some(Command::CloseBuffer.into()),
        KeyCode::Char('q') if ctrl => Some(Command::ToggleComment.into()),
        KeyCode::Char('/') if ctrl => Some(Command::ToggleComment.into()),
        KeyCode::Up if ctrl && key.modifiers.contains(KeyModifiers::ALT) => Some(
            Command::AddCursor {
                direction: VerticalDirection::Up,
            }
            .into(),
        ),
        KeyCode::Down if ctrl && key.modifiers.contains(KeyModifiers::ALT) => Some(
            Command::AddCursor {
                direction: VerticalDirection::Down,
            }
            .into(),
        ),
        KeyCode::Esc => Some(Command::CollapseSelections.into()),
        KeyCode::Enter => Some(Command::InsertNewline.into()),
        KeyCode::Tab if shift => Some(Command::Outdent.into()),
        KeyCode::Tab => Some(Command::Indent.into()),
        KeyCode::Backspace => Some(Command::DeleteBackward.into()),
        KeyCode::Delete => Some(Command::DeleteForward.into()),
        KeyCode::Left => Some(move_event(
            Direction::Left,
            if ctrl { Unit::Word } else { Unit::Character },
            shift,
        )),
        KeyCode::Right => Some(move_event(
            Direction::Right,
            if ctrl { Unit::Word } else { Unit::Character },
            shift,
        )),
        KeyCode::Up => Some(move_event(Direction::Up, Unit::Character, shift)),
        KeyCode::Down => Some(move_event(Direction::Down, Unit::Character, shift)),
        KeyCode::Home => Some(move_event(
            Direction::Left,
            if ctrl { Unit::Document } else { Unit::Line },
            shift,
        )),
        KeyCode::End => Some(move_event(
            Direction::Right,
            if ctrl { Unit::Document } else { Unit::Line },
            shift,
        )),
        KeyCode::Char(character)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(AppEvent::TextInputAt { character, at })
        }
        _ => None,
    }
}

fn shell_key(key: KeyEvent) -> Option<Vec<u8>> {
    match key.code {
        KeyCode::Char(character) if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let ascii = character.to_ascii_lowercase();
            ascii.is_ascii().then(|| vec![(ascii as u8) & 0x1f])
        }
        KeyCode::Char(character) => {
            let mut bytes = [0; 4];
            Some(character.encode_utf8(&mut bytes).as_bytes().to_vec())
        }
        KeyCode::Enter => Some(vec![b'\r']),
        KeyCode::Backspace => Some(vec![0x7f]),
        KeyCode::Tab => Some(vec![b'\t']),
        KeyCode::Esc => Some(vec![0x1b]),
        KeyCode::Up => Some(b"\x1b[A".to_vec()),
        KeyCode::Down => Some(b"\x1b[B".to_vec()),
        KeyCode::Right => Some(b"\x1b[C".to_vec()),
        KeyCode::Left => Some(b"\x1b[D".to_vec()),
        _ => None,
    }
}

fn move_event(direction: Direction, unit: Unit, extend: bool) -> AppEvent {
    Command::Move {
        direction,
        unit,
        extend,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyModifiers;

    use super::*;
    use crate::editor::Side;

    fn raw_key(event: KeyEvent) -> RawInput {
        RawInput::Key {
            event,
            at: Instant::now(),
        }
    }

    #[test]
    fn f4_translates_to_quit() {
        let raw = raw_key(KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        let mut pending = KeyChordState::default();

        let event = translate(raw, &Focus::Editor(Side::Left), &mut pending);

        assert_eq!(event, Some(AppEvent::Command(Command::Quit)));
    }

    #[test]
    fn key_release_does_not_trigger_a_command() {
        let mut key = KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;
        let mut pending = KeyChordState::default();

        assert_eq!(translate(raw_key(key), &Focus::Shell, &mut pending), None);
    }

    #[test]
    fn printable_key_becomes_text_input_in_editor_focus() {
        let raw = raw_key(KeyEvent::new(KeyCode::Char('日'), KeyModifiers::NONE));
        let mut pending = KeyChordState::default();

        assert!(matches!(
            translate(raw, &Focus::Editor(Side::Left), &mut pending),
            Some(AppEvent::TextInputAt {
                character: '日',
                ..
            })
        ));
    }

    #[test]
    fn ctrl_shift_arrow_extends_by_word() {
        let raw = raw_key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        let mut pending = KeyChordState::default();

        assert_eq!(
            translate(raw, &Focus::Editor(Side::Left), &mut pending),
            Some(AppEvent::Command(Command::Move {
                direction: Direction::Left,
                unit: Unit::Word,
                extend: true,
            }))
        );
    }

    #[test]
    fn f6_opens_the_diff_picker() {
        let raw = raw_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        let mut pending = KeyChordState::default();

        assert_eq!(
            translate(raw, &Focus::Editor(Side::Left), &mut pending),
            Some(AppEvent::Command(Command::OpenDiffPicker))
        );
    }

    #[test]
    fn ctrl_f_opens_search() {
        let raw = raw_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        let mut pending = KeyChordState::default();

        assert_eq!(
            translate(raw, &Focus::Editor(Side::Left), &mut pending),
            Some(AppEvent::Command(Command::OpenSearch))
        );
    }

    #[test]
    fn ctrl_p_opens_command_palette() {
        let raw = raw_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        let mut pending = KeyChordState::default();

        assert_eq!(
            translate(raw, &Focus::Editor(Side::Left), &mut pending),
            Some(AppEvent::Command(Command::OpenCommandPalette))
        );
    }

    #[test]
    fn replacement_shortcuts_map_to_completion_shell_and_split() {
        let mut pending = KeyChordState::default();
        assert_eq!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)),
                &Focus::Editor(Side::Left),
                &mut pending,
            ),
            Some(AppEvent::Command(Command::ToggleCompletion))
        );
        assert_eq!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
                &Focus::Editor(Side::Left),
                &mut pending,
            ),
            Some(AppEvent::Command(Command::ToggleShell))
        );
        assert_eq!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL)),
                &Focus::Editor(Side::Left),
                &mut pending,
            ),
            Some(AppEvent::Command(Command::ToggleSplit))
        );
        assert_eq!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::CONTROL)),
                &Focus::Editor(Side::Left),
                &mut pending,
            ),
            Some(AppEvent::Command(Command::ToggleSplit))
        );
    }

    #[test]
    fn ctrl_c_cancels_an_overlay_but_remains_shell_input() {
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let mut pending = KeyChordState::default();
        assert_eq!(
            translate(raw_key(key), &Focus::Overlay, &mut pending),
            Some(AppEvent::Command(Command::PickerCancel))
        );
        assert_eq!(
            translate(raw_key(key), &Focus::Shell, &mut pending),
            Some(AppEvent::TerminalInput(vec![3]))
        );
    }

    #[test]
    fn completion_only_captures_navigation_confirmation_and_cancel() {
        let focus = Focus::Completion(Side::Right);
        let mut pending = KeyChordState::default();

        assert!(matches!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
                &focus,
                &mut pending,
            ),
            Some(AppEvent::TextInputAt { character: 'x', .. })
        ));
        assert_eq!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
                &focus,
                &mut pending,
            ),
            Some(Command::DeleteBackward.into())
        );
        assert_eq!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
                &focus,
                &mut pending,
            ),
            Some(Command::PickerUp.into())
        );
        assert_eq!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                &focus,
                &mut pending,
            ),
            Some(Command::PickerConfirm.into())
        );
        assert_eq!(
            translate(
                raw_key(KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL)),
                &focus,
                &mut pending,
            ),
            Some(Command::ToggleCompletion.into())
        );
    }

    #[test]
    fn ctrl_shift_f_opens_directory_search() {
        let raw = raw_key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        let mut pending = KeyChordState::default();

        assert_eq!(
            translate(raw, &Focus::Editor(Side::Left), &mut pending),
            Some(AppEvent::Command(Command::OpenSearchInDirectory))
        );
    }
}
