use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, MouseButton};

const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Default)]
pub struct KeyChordState {
    prefix: Option<KeyEvent>,
    last_click: Option<Click>,
}

impl KeyChordState {
    pub fn clear(&mut self) {
        self.prefix = None;
    }

    pub fn register_click(
        &mut self,
        at: Instant,
        column: u16,
        row: u16,
        button: MouseButton,
    ) -> u8 {
        let clicks = self
            .last_click
            .filter(|last| {
                at.saturating_duration_since(last.at) <= MULTI_CLICK_INTERVAL
                    && last.column == column
                    && last.row == row
                    && last.button == button
            })
            .map_or(1, |last| (last.clicks % 3) + 1);
        self.last_click = Some(Click {
            at,
            column,
            row,
            button,
            clicks,
        });
        clicks
    }
}

#[derive(Clone, Copy, Debug)]
struct Click {
    at: Instant,
    column: u16,
    row: u16,
    button: MouseButton,
    clicks: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_count_requires_matching_position_and_interval() {
        let start = Instant::now();
        let mut state = KeyChordState::default();

        assert_eq!(state.register_click(start, 3, 4, MouseButton::Left), 1);
        assert_eq!(
            state.register_click(start + Duration::from_millis(100), 3, 4, MouseButton::Left),
            2
        );
        assert_eq!(
            state.register_click(start + Duration::from_millis(700), 3, 4, MouseButton::Left),
            1
        );
    }
}
