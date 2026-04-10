use std::env;

const DEFAULT_LARGE_FILE_THRESHOLD_BYTES: u64 = 10 * 1024 * 1024;
const DEFAULT_LARGE_FILE_READ_WINDOW_BYTES: usize = 64 * 1024;
const DEFAULT_SHELL_PROGRAM: &str = "/bin/sh";

pub struct Config {
    pub(crate) large_file_threshold_bytes: u64,
    pub(crate) large_file_read_window_bytes: usize,
    pub(crate) shell_program: String,
}

impl Config {
    pub fn new(
        large_file_threshold_bytes: u64,
        large_file_read_window_bytes: usize,
        shell_program: String,
    ) -> Self {
        Self {
            large_file_threshold_bytes,
            large_file_read_window_bytes,
            shell_program,
        }
    }

    pub fn from_env() -> Self {
        Self {
            large_file_threshold_bytes: env_u64("LARGE_FILE_THRESHOLD_BYTES")
                .unwrap_or(DEFAULT_LARGE_FILE_THRESHOLD_BYTES),
            large_file_read_window_bytes: env_usize("LARGE_FILE_READ_WINDOW_BYTES")
                .unwrap_or(DEFAULT_LARGE_FILE_READ_WINDOW_BYTES),
            shell_program: env::var("SHELL")
                .unwrap_or_else(|_| DEFAULT_SHELL_PROGRAM.to_owned()),
        }
    }

}

fn env_u64(key: &str) -> Option<u64> {
    env::var(key).ok()?.parse::<u64>().ok().filter(|&v| v > 0)
}

fn env_usize(key: &str) -> Option<usize> {
    env::var(key).ok()?.parse::<usize>().ok().filter(|&v| v > 0)
}
