use std::io;

use crate::open_candidate;

#[derive(Debug)]
pub enum AppError {
    Io(io::Error),
    OpenCandidate(open_candidate::Error),
    CommandFailed(String),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<open_candidate::Error> for AppError {
    fn from(error: open_candidate::Error) -> Self {
        Self::OpenCandidate(error)
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::OpenCandidate(e) => write!(f, "Open candidate error: {e:?}"),
            Self::CommandFailed(msg) => write!(f, "Command failed: {msg}"),
        }
    }
}
