use std::{env, sync::OnceLock};

const LARGE_FILE_READ_WINDOW_BYTES: usize = 64 * 1024;
static LARGE_FILE_READ_WINDOW_BYTES_CACHE: OnceLock<usize> = OnceLock::new();

pub fn large_file_read_window_bytes() -> usize {
    *LARGE_FILE_READ_WINDOW_BYTES_CACHE.get_or_init(|| {
        env::var("LARGE_FILE_READ_WINDOW_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(LARGE_FILE_READ_WINDOW_BYTES)
    })
}
