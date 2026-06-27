mod app;
mod color;
mod config;
mod document;
mod error;
mod keymap_hints;
mod language;
mod mode;
mod open_candidate;
mod picker_match;
mod search_options;

use std::path::PathBuf;

use crate::error::Result;

fn main() -> Result<()> {
    let mut silent = false;
    let mut path: Option<PathBuf> = None;

    for arg in std::env::args().skip(1) {
        if arg == "--silent" {
            silent = true;
        } else {
            path = Some(PathBuf::from(arg));
        }
    }

    let mut app = match path {
        Some(path) => app::App::open_path(&path)?,
        None => app::App::new()?,
    };
    app.silent = silent;

    app.run()
}
