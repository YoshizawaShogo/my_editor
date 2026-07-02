use my_editor::{Result, error::install_panic_hook, runtime::Runtime, terminal::TerminalSession};
use std::path::PathBuf;

fn main() {
    install_panic_hook();

    if let Err(error) = run() {
        eprintln!("my_editor: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let terminal = TerminalSession::enter()?;
    let cwd = std::env::current_dir()?;
    let paths = std::env::args()
        .skip(1)
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .collect();
    let mut runtime = Runtime::new(terminal, paths);
    tokio.block_on(runtime.run())
}
