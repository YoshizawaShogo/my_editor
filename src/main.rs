use std::io;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use crossterm::cursor::{SetCursorStyle, Show};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use my_editor::app::App;
use my_editor::keymap::map_key;
use my_editor::theme::{ThemeOption, ThemePalette};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

#[derive(Debug, Parser)]
#[command(name = "my_editor")]
struct Cli {
    file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = ThemeOption::Classic)]
    theme: ThemeOption,
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        Show,
        SetCursorStyle::SteadyBar
    )?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = run(&mut terminal, cli);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, cli: Cli) -> io::Result<()> {
    let mut app = App::new(
        std::env::current_dir()?,
        cli.file,
        ThemePalette::resolve(cli.theme),
    );

    while !app.should_quit {
        app.tick();
        terminal.draw(|frame| {
            app.sync_viewports(frame.area());
            my_editor::ui::render(&app, frame);
        })?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if let Some(action) = map_key(app.mode, key) {
                    app.dispatch(action);
                }
            }
        }
    }

    Ok(())
}
