use std::{env, sync::OnceLock};

const LARGE_FILE_THRESHOLD_BYTES: u64 = 10 * 1024 * 1024;
const LARGE_FILE_READ_WINDOW_BYTES: usize = 64 * 1024;
const DEFAULT_SHELL_PROGRAM: &str = "/bin/sh";
static LARGE_FILE_THRESHOLD_BYTES_CACHE: OnceLock<u64> = OnceLock::new();
static LARGE_FILE_READ_WINDOW_BYTES_CACHE: OnceLock<usize> = OnceLock::new();
static SHELL_PROGRAM_CACHE: OnceLock<String> = OnceLock::new();

pub fn large_file_threshold_bytes() -> u64 {
    *LARGE_FILE_THRESHOLD_BYTES_CACHE.get_or_init(|| {
        env::var("LARGE_FILE_THRESHOLD_BYTES")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(LARGE_FILE_THRESHOLD_BYTES)
    })
}

pub fn large_file_read_window_bytes() -> usize {
    *LARGE_FILE_READ_WINDOW_BYTES_CACHE.get_or_init(|| {
        env::var("LARGE_FILE_READ_WINDOW_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(LARGE_FILE_READ_WINDOW_BYTES)
    })
}

pub fn shell_program() -> &'static str {
    SHELL_PROGRAM_CACHE
        .get_or_init(|| env::var("SHELL").unwrap_or_else(|_| DEFAULT_SHELL_PROGRAM.to_owned()))
        .as_str()
}
