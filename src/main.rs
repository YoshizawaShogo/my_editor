use std::{path::PathBuf, process::ExitCode};

use clap::Parser;

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
        Ok(mut app) => match app.run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error:?}");
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("{error:?}");
            ExitCode::FAILURE
        }
    }
}
