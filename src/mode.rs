#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    #[allow(dead_code)]
    Shell,
}

impl Mode {
    #[allow(dead_code)]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Shell => "SHELL",
        }
    }
}
