mod cli;
mod console;
mod engine;
mod gui;
mod login_path;
mod tui;
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
        Mode::Gui => {
            if console::prepare_gui() == console::GuiLaunch::Exit {
                return Ok(());
            }
            // Panics in a detached GUI have no console: keep a trace next to the config.
            tracing_to_file();
            // Before the GUI starts any thread: it sets PATH.
            login_path::import();
            gui::run()
        }
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

/// Send `tracing` output to `debug.log` in the config directory
/// (`~/.config/storing-unicorns/` or the platform equivalent, or
/// `$STORINGUNICORNS_CONFIG_DIR`).
fn tracing_to_file() {
    let log_path = engine::config::app_dir().expect("Could not create config directory");
    let file = match std::fs::File::create(log_path.join("debug.log")) {
        Ok(file) => file,
        Err(error) => panic!("Error: {:?}", error),
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::sync::Arc::new(file))
        .init();
}

fn run_tui(opts: cli::TuiOptions) -> anyhow::Result<()> {
    tracing_to_file();
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(tui::run(opts));
    // Don't wait for a post-exit update check that outlived its timeout.
    runtime.shutdown_background();
    result
}

/// `storingUnicorns update`: check, download, install, report on stdout.
fn run_update() -> anyhow::Result<()> {
    updater::cleanup_stale_staging();
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
