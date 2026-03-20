use std::io;

pub enum AppError {
    Placeholder,
    Io(io::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

impl From<io::Error> for AppError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
