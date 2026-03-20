use std::{io, path::PathBuf, process::ExitCode};

use clap::Parser;
use crossterm::terminal;

mod app;
mod config;
mod document;
mod error;
mod mode;
mod open_candidate;
mod picker_match;

#[derive(Parser)]
struct Args {
    path: PathBuf,
}

fn main() -> ExitCode {
    let args = Args::parse();

    match app::App::open(&args.path) {
        Ok(app) => {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            let (page_width, page_height) = terminal::size().unwrap_or((80, 24));

            match app.render_to(
                &mut stdout,
                page_height as usize,
                page_width as usize,
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error:?}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            eprintln!("{error:?}");
            ExitCode::FAILURE
        }
    }
}
