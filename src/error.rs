use std::io;

use crate::open_candidate;

#[derive(Debug)]
pub enum AppError {
    Io(io::Error),
    OpenCandidate(open_candidate::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
