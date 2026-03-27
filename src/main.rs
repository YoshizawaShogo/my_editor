use std::{path::PathBuf, process::ExitCode};

use clap::Parser;

mod app;
mod color;
mod config;
mod document;
mod error;
mod mode;
mod open_candidate;
mod picker_match;

#[derive(Parser)]
struct Args {
    path: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();

    match app::App::open(args.path.as_deref()) {
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
