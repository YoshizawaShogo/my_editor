use std::io;

#[derive(Debug)]
pub enum AppError {
    Placeholder,
    CommandFailed(String),
    Io(io::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
