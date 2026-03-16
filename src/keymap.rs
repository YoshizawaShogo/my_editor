use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, Direction, Mode};

pub fn map_key(mode: Mode, key: KeyEvent) -> Option<Action> {
    match mode {
        Mode::Normal => map_normal(key),
        Mode::Insert => map_insert(key),
        Mode::BufferSearch
        | Mode::FilePicker
        | Mode::BufferList
        | Mode::SymbolSearch
        | Mode::Diagnostics => map_prompt(key),
    }
}

fn map_normal(key: KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), KeyModifiers::CONTROL) => Some(Action::Quit),
        (KeyCode::Enter, KeyModifiers::NONE) => Some(Action::EnterInsert),
        (KeyCode::Char('j'), KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Left)),
        (KeyCode::Char('l'), KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Right)),
        (KeyCode::Char('i'), KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Up)),
        (KeyCode::Char('k'), KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Down)),
        (KeyCode::Left, KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Left)),
        (KeyCode::Right, KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Right)),
        (KeyCode::Up, KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Up)),
        (KeyCode::Down, KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Down)),
        (KeyCode::Left, KeyModifiers::ALT) => Some(Action::MovePaneFocus(Direction::Left)),
        (KeyCode::Right, KeyModifiers::ALT) => Some(Action::MovePaneFocus(Direction::Right)),
        (KeyCode::Char('f'), KeyModifiers::CONTROL) => Some(Action::OpenBufferSearch),
        (KeyCode::Char('p'), KeyModifiers::CONTROL) => Some(Action::OpenFilePicker),
        (KeyCode::Char('b'), KeyModifiers::CONTROL) => Some(Action::OpenBufferList),
        (KeyCode::Char('l'), KeyModifiers::CONTROL) => Some(Action::OpenSymbolSearch),
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => Some(Action::OpenDiagnostics),
        _ => None,
    }
}

fn map_insert(key: KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => Some(Action::EnterNormal),
        (KeyCode::Enter, KeyModifiers::NONE) => Some(Action::InsertNewline),
        (KeyCode::Backspace, KeyModifiers::NONE) => Some(Action::DeleteBackward),
        (KeyCode::Left, KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Left)),
        (KeyCode::Right, KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Right)),
        (KeyCode::Up, KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Up)),
        (KeyCode::Down, KeyModifiers::NONE) => Some(Action::MoveCursor(Direction::Down)),
        (KeyCode::Char('f'), KeyModifiers::CONTROL) => Some(Action::OpenBufferSearch),
        (KeyCode::Char('p'), KeyModifiers::CONTROL) => Some(Action::OpenFilePicker),
        (KeyCode::Char('b'), KeyModifiers::CONTROL) => Some(Action::OpenBufferList),
        (KeyCode::Char('l'), KeyModifiers::CONTROL) => Some(Action::OpenSymbolSearch),
        (KeyCode::Char('k'), KeyModifiers::CONTROL) => Some(Action::OpenDiagnostics),
        (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            Some(Action::InsertChar(ch))
        }
        (KeyCode::Tab, KeyModifiers::NONE) => Some(Action::InsertChar('\t')),
        _ => None,
    }
}

fn map_prompt(key: KeyEvent) -> Option<Action> {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => Some(Action::CancelPrompt),
        (KeyCode::Enter, _) => Some(Action::ConfirmPrompt),
        (KeyCode::Backspace, _) => Some(Action::PromptBackspace),
        (KeyCode::Up, _) => Some(Action::PromptMoveSelection(Direction::Up)),
        (KeyCode::Down, _) => Some(Action::PromptMoveSelection(Direction::Down)),
        (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            Some(Action::PromptInput(ch))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_char(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    #[test]
    fn normal_mode_uses_ijkl_for_movement() {
        assert_eq!(
            map_key(Mode::Normal, plain_char('j')),
            Some(Action::MoveCursor(Direction::Left))
        );
        assert_eq!(
            map_key(Mode::Normal, plain_char('l')),
            Some(Action::MoveCursor(Direction::Right))
        );
        assert_eq!(
            map_key(Mode::Normal, plain_char('i')),
            Some(Action::MoveCursor(Direction::Up))
        );
        assert_eq!(
            map_key(Mode::Normal, plain_char('k')),
            Some(Action::MoveCursor(Direction::Down))
        );
    }

    #[test]
    fn enter_starts_insert_mode_from_normal() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(map_key(Mode::Normal, key), Some(Action::EnterInsert));
    }

    #[test]
    fn insert_mode_keeps_i_as_text_input() {
        assert_eq!(
            map_key(Mode::Insert, plain_char('i')),
            Some(Action::InsertChar('i'))
        );
    }

    #[test]
    fn control_bindings_open_specialized_modes() {
        assert_eq!(
            map_key(
                Mode::Normal,
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)
            ),
            Some(Action::OpenBufferList)
        );
        assert_eq!(
            map_key(
                Mode::Normal,
                KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)
            ),
            Some(Action::OpenSymbolSearch)
        );
        assert_eq!(
            map_key(
                Mode::Normal,
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL)
            ),
            Some(Action::OpenDiagnostics)
        );
    }
}
