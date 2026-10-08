mod cli;
mod console;
mod engine;
mod tui;
// Parts of the updater (background state machine) are only used by the GUI (plan 3).
#[allow(dead_code, unused_imports)]
mod updater;

use cli::Mode;

// `main` stays synchronous: the GUI (plan 3) owns its own tokio runtime, and a
// runtime must not be created or dropped inside another one.
fn main() -> anyhow::Result<()> {
    // Lossy conversion: a non-UTF-8 argument becomes `Mode::Invalid`
    // instead of panicking in `std::env::args()`.
    let args = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned());
    match Mode::parse(args) {
        // The GUI arrives in plan 3; until then the TUI is the only interface.
        Mode::Gui => run_tui(Default::default()),
        Mode::Tui(opts) => run_tui(opts),
        Mode::Update => run_update(),
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
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(tui::run(opts));
    // Don't wait for a post-exit update check that outlived its timeout.
    runtime.shutdown_background();
    result
}

/// `storingUnicorns update`: check, download, install, report on stdout.
fn run_update() -> anyhow::Result<()> {
    println!("Current version: {}", updater::current_version());
    println!("Checking for updates…");
    let info = match updater::fetch_latest() {
        Ok(Some(info)) => info,
        Ok(None) => {
            println!("storingUnicorns is up to date.");
            return Ok(());
        }
        Err(e) => anyhow::bail!("update check failed: {e}"),
    };
    println!("Installing v{}…", info.version);
    match updater::download_and_apply(&info).map_err(|e| anyhow::anyhow!(e))? {
        None => println!(
            "Updated to v{}. Restart storingUnicorns to use it.",
            info.version
        ),
        Some(msi) => {
            let action = updater::ExitAction::RunMsi(msi.clone());
            if let Err(e) = updater::run_exit_action(&action, &[]) {
                eprintln!(
                    "Failed to launch the installer ({e}). Run it manually: {}",
                    msi.display()
                );
                std::process::exit(1);
            }
            println!("Installer launched.");
        }
    }
    Ok(())
}
