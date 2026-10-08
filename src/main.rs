mod cli;
mod engine;
mod tui;

use cli::Mode;

// `main` stays synchronous: the GUI (plan 3) owns its own tokio runtime, and a
// runtime must not be created or dropped inside another one.
fn main() -> anyhow::Result<()> {
    match Mode::parse(std::env::args().skip(1)) {
        // The GUI arrives in plan 3; until then the TUI is the only interface.
        Mode::Gui => run_tui(Default::default()),
        Mode::Tui(opts) => run_tui(opts),
        Mode::Update => {
            println!("Self-update is not available yet.");
            Ok(())
        }
        Mode::Version => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Mode::Help => {
            print!("{}", cli::HELP);
            Ok(())
        }
        Mode::Invalid(arg) => {
            eprintln!("Unknown argument: {arg}\n\n{}", cli::HELP);
            std::process::exit(2);
        }
    }
}

fn run_tui(opts: cli::TuiOptions) -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(tui::run(opts))
}
