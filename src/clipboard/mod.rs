mod wayland;
mod x11;

pub use x11::X11Clipboard;

/// Where one clipboard backend stands, for `my_editor --status` to report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendStatus {
    pub name: &'static str,
    /// `Ok` carries what a copy would be handed to; `Err` why it would not —
    /// including "the editor does not implement this", which is Wayland's answer.
    pub state: Result<String, String>,
}

/// Every clipboard backend and its standing here, in the order the editor picks
/// them. Probes the display server, so this belongs to a command being run, not
/// to rendering.
pub fn backend_status() -> Vec<BackendStatus> {
    vec![
        BackendStatus {
            name: "x11",
            state: X11Clipboard::probe(),
        },
        BackendStatus {
            name: "wayland",
            state: Err(wayland::unavailability()),
        },
    ]
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Register {
    fragments: Vec<String>,
    linewise: bool,
}

impl Register {
    pub fn store(&mut self, fragments: Vec<String>) {
        self.fragments = fragments;
        self.linewise = false;
    }

    pub fn store_linewise(&mut self, fragments: Vec<String>) {
        self.fragments = fragments;
        self.linewise = true;
    }

    pub fn fragments(&self) -> &[String] {
        &self.fragments
    }

    /// The register as one string for the OS clipboard. A linewise register
    /// keeps its trailing newline, so pasting it elsewhere lands whole lines.
    pub fn text(&self) -> String {
        let text = self.fragments.join("\n");
        if self.linewise {
            format!("{text}\n")
        } else {
            text
        }
    }

    pub fn is_linewise(&self) -> bool {
        self.linewise
    }

    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linewise_register_keeps_one_trailing_newline() {
        let mut register = Register::default();
        register.store_linewise(vec!["one".to_owned(), "two".to_owned()]);

        assert!(register.is_linewise());
        assert_eq!(register.text(), "one\ntwo\n");
    }
}
