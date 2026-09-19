//! Wayland clipboard: deliberately not implemented, and this says so.
//!
//! Setting a selection through the core protocol needs an input serial from a
//! surface that has focus, and a terminal application has no surface of its own —
//! the terminal emulator does. `wlr-data-control`/`ext-data-control` lifts that
//! restriction, but GNOME offers neither. Wayland sessions run XWayland (WSLg
//! included), so copies go out over X11 instead.
//!
//! What is left is worth reporting rather than leaving the user to guess, so this
//! detects the compositor without talking to it.

use std::path::PathBuf;

/// The compositor's socket, when one is reachable. `WAYLAND_DISPLAY` is either an
//  absolute path or a name under `XDG_RUNTIME_DIR`.
pub fn socket() -> Option<PathBuf> {
    let display = std::env::var_os("WAYLAND_DISPLAY")?;
    if display.is_empty() {
        return None;
    }
    let path = PathBuf::from(&display);
    let path = if path.is_absolute() {
        path
    } else {
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(path)
    };
    path.exists().then_some(path)
}

/// Why a copy never goes out over Wayland: the editor does not implement it. The
/// compositor is named when there is one, so the reader can tell "nothing to talk
/// to" apart from "there is a compositor, we just don't use it".
pub fn unavailability() -> String {
    match socket() {
        Some(path) => format!("not implemented (compositor at {})", path.display()),
        None => "not implemented (no compositor here anyway)".to_owned(),
    }
}
