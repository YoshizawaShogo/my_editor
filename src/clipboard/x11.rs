//! Owning the X11 `CLIPBOARD` selection from inside the editor.
//!
//! X11 has no "push": the client that copied keeps the data and hands it over
//! when something else pastes. A long-running editor is a natural owner, which
//! is why this needs no external tool — `xclip`/`xsel` exist mostly to leave a
//! background process behind for exactly this reason.
//!
//! So a copy is two halves. Claiming the selection is one round trip from the
//! caller's thread ([`X11Clipboard::set`]); answering the conversion requests
//! that arrive later belongs to a thread of its own ([`Shared::serve`]).

use std::sync::{Arc, Mutex};

use x11rb::connection::{Connection, RequestConnection as _};
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

/// The atoms an owner of the clipboard has to name, interned once at startup.
struct Atoms {
    clipboard: Atom,
    targets: Atom,
    utf8_string: Atom,
    text_plain_utf8: Atom,
}

impl Atoms {
    fn intern(connection: &RustConnection) -> Result<Self, String> {
        let atom = |name: &str| -> Result<Atom, String> {
            Ok(connection
                .intern_atom(false, name.as_bytes())
                .map_err(|error| error.to_string())?
                .reply()
                .map_err(|error| error.to_string())?
                .atom)
        };
        Ok(Self {
            clipboard: atom("CLIPBOARD")?,
            targets: atom("TARGETS")?,
            utf8_string: atom("UTF8_STRING")?,
            text_plain_utf8: atom("text/plain;charset=utf-8")?,
        })
    }
}

/// What both halves of the copy need: the connection, the window the selection
/// is owned by, and the text being offered.
struct Shared {
    connection: RustConnection,
    window: Window,
    atoms: Atoms,
    /// The text pasting clients are handed. Empty once another client takes the
    /// clipboard away from us, so a stale copy is never served.
    text: Mutex<Vec<u8>>,
}

pub struct X11Clipboard {
    shared: Arc<Shared>,
}

impl X11Clipboard {
    /// Connect to the X server named by `DISPLAY` and start serving pastes.
    ///
    /// `None` when there is no server to talk to — no `DISPLAY`, or the
    /// connection or handshake fails — which is the editor's cue that copies
    /// stay internal. The serving thread holds its own handle, so the selection
    /// keeps being served for as long as the process lives.
    pub fn spawn() -> Option<Self> {
        let (connection, screen_index) = x11rb::connect(None).ok()?;
        let root = connection.setup().roots.get(screen_index)?.root;
        let window = connection.generate_id().ok()?;
        // Never mapped: it exists only to hold the selection and to receive the
        // requests that come with it.
        connection
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                window,
                root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_OUTPUT,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .ok()?;
        let atoms = Atoms::intern(&connection).ok()?;
        connection.flush().ok()?;
        let shared = Arc::new(Shared {
            connection,
            window,
            atoms,
            text: Mutex::new(Vec::new()),
        });
        let server = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("x11-clipboard".to_owned())
            .spawn(move || server.serve())
            .ok()?;
        Some(Self { shared })
    }

    /// Whether a copy could be handed to an X server here: the display it would
    /// talk to, or why it could not. Connects and hangs up again, claiming
    /// nothing, so `--status` can answer this without owning the clipboard.
    pub fn probe() -> Result<String, String> {
        let display = std::env::var("DISPLAY").unwrap_or_default();
        if display.is_empty() {
            return Err("DISPLAY unset".to_owned());
        }
        match x11rb::connect(None) {
            Ok(_) => Ok(format!("DISPLAY={display}")),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Claim the clipboard and offer `text` from now on.
    ///
    /// The claim is confirmed against the server rather than assumed: a refused
    /// one would otherwise look like a copy that worked until the user pasted.
    pub fn set(&self, text: &str) -> Result<(), String> {
        let shared = &self.shared;
        // A property is handed over in one request, so text that does not fit in
        // one is refused here (INCR transfers are not implemented) — at the copy,
        // where it can still be reported, rather than at the paste.
        let limit = shared.connection.maximum_request_bytes();
        if text.len() + CHANGE_PROPERTY_OVERHEAD > limit {
            return Err(format!(
                "The clipboard takes at most {}KiB",
                (limit - CHANGE_PROPERTY_OVERHEAD) / 1024
            ));
        }
        *shared.text.lock().map_err(|error| error.to_string())? = text.as_bytes().to_vec();
        shared
            .connection
            .set_selection_owner(shared.window, shared.atoms.clipboard, x11rb::CURRENT_TIME)
            .map_err(|error| error.to_string())?;
        shared
            .connection
            .flush()
            .map_err(|error| error.to_string())?;
        let owner = shared
            .connection
            .get_selection_owner(shared.atoms.clipboard)
            .map_err(|error| error.to_string())?
            .reply()
            .map_err(|error| error.to_string())?
            .owner;
        if owner != shared.window {
            return Err("Could not take ownership of the clipboard".to_owned());
        }
        Ok(())
    }
}

/// A ChangeProperty request's fixed part, which shares the request's size budget
/// with the text itself.
const CHANGE_PROPERTY_OVERHEAD: usize = 24;

impl Shared {
    /// Answer conversion requests until the connection goes away. Nothing is
    /// left to serve after that, so the thread ends with it.
    fn serve(&self) {
        while let Ok(event) = self.connection.wait_for_event() {
            match event {
                Event::SelectionRequest(request) => {
                    // A failed reply is the requestor's problem (a window that
                    // vanished mid-paste is routine), and there is nobody left to
                    // report it to; keep serving the next one.
                    let _ = self.answer(&request);
                }
                // Another client owns the clipboard now, so what we hold is no
                // longer what the user last copied.
                Event::SelectionClear(_) => {
                    if let Ok(mut text) = self.text.lock() {
                        text.clear();
                    }
                }
                _ => {}
            }
        }
    }

    fn answer(&self, request: &SelectionRequestEvent) -> Result<(), String> {
        let text = self.text.lock().map_err(|error| error.to_string())?.clone();
        // Obsolete clients ask for the data on the target atom itself.
        let property = if request.property == x11rb::NONE {
            request.target
        } else {
            request.property
        };
        let string: Atom = AtomEnum::STRING.into();
        let supplied = if request.target == self.atoms.targets {
            let targets = [
                self.atoms.targets,
                self.atoms.utf8_string,
                self.atoms.text_plain_utf8,
                string,
            ];
            self.connection
                .change_property32(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )
                .map_err(|error| error.to_string())?;
            true
        } else if request.target == self.atoms.utf8_string
            || request.target == self.atoms.text_plain_utf8
            || request.target == string
        {
            self.connection
                .change_property8(
                    PropMode::REPLACE,
                    request.requestor,
                    property,
                    request.target,
                    &text,
                )
                .map_err(|error| error.to_string())?;
            true
        } else {
            false
        };
        // A property of None is how a target we cannot produce is refused.
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: request.time,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if supplied { property } else { x11rb::NONE },
        };
        self.connection
            .send_event(false, request.requestor, EventMask::NO_EVENT, notify)
            .map_err(|error| error.to_string())?;
        self.connection.flush().map_err(|error| error.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hands text to the real X server and pastes it back with `xsel`, which is
    /// the only way to prove the selection hand-off works — the protocol only
    /// happens between two clients. Ignored by default: it replaces whatever the
    /// machine running the tests has on its clipboard. Run with
    /// `cargo test --ignored x11_clipboard`.
    #[test]
    #[ignore = "touches the machine's real clipboard and needs an X server plus xsel"]
    fn text_set_on_the_clipboard_can_be_pasted_by_another_client() {
        let Some(clipboard) = X11Clipboard::spawn() else {
            panic!("no X server to talk to (DISPLAY unset or connection refused)");
        };
        let text = "コピー test 漢字\nsecond line";
        clipboard.set(text).expect("claiming the clipboard");

        let pasted = std::process::Command::new("xsel")
            .args(["--clipboard", "--output"])
            .output()
            .expect("running xsel");
        assert_eq!(String::from_utf8_lossy(&pasted.stdout), text);
    }
}
