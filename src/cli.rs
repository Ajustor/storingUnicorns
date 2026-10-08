//! Command-line dispatch: GUI by default, `tui` for the terminal UI.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TuiOptions {
    pub debug: bool,
    pub no_animations: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Gui,
    Tui(TuiOptions),
    Update,
    Version,
    Help,
    /// An argument we don't understand.
    Invalid(String),
}

pub const HELP: &str = "\
storingUnicorns — database client (GUI, TUI)

USAGE:
    storingUnicorns                 Open the graphical interface
    storingUnicorns tui [OPTIONS]   Open the terminal interface
    storingUnicorns update          Download and install the latest version
    storingUnicorns --version       Print the version
    storingUnicorns --help          Print this help

TUI OPTIONS:
    -d, --debug             Show generated SQL in the editor instead of running it
    -na, --no-animations    Disable animations
";

impl Mode {
    /// Parse arguments (without the program name).
    pub fn parse(args: impl IntoIterator<Item = String>) -> Mode {
        let mut tui = None::<TuiOptions>;
        let mut update = false;
        let mut opts = TuiOptions::default();
        for arg in args {
            match arg.as_str() {
                "-v" | "--version" => return Mode::Version,
                "-h" | "--help" => return Mode::Help,
                "tui" => tui = Some(TuiOptions::default()),
                "update" => update = true,
                "-d" | "--debug" => opts.debug = true,
                "-na" | "--no-animations" => opts.no_animations = true,
                _ => return Mode::Invalid(arg),
            }
        }
        if update {
            return Mode::Update;
        }
        // Legacy: `--debug` / `--no-animations` alone used to start the TUI.
        if tui.is_some() || opts != TuiOptions::default() {
            return Mode::Tui(opts);
        }
        Mode::Gui
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Mode {
        Mode::parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn no_args_is_gui() {
        assert_eq!(parse(&[]), Mode::Gui);
    }

    #[test]
    fn tui_subcommand_with_flags() {
        assert_eq!(parse(&["tui"]), Mode::Tui(TuiOptions::default()));
        assert_eq!(
            parse(&["tui", "--debug", "-na"]),
            Mode::Tui(TuiOptions {
                debug: true,
                no_animations: true
            })
        );
    }

    #[test]
    fn legacy_flags_alone_launch_tui() {
        assert_eq!(
            parse(&["--debug"]),
            Mode::Tui(TuiOptions {
                debug: true,
                no_animations: false
            })
        );
        assert_eq!(
            parse(&["--no-animations"]),
            Mode::Tui(TuiOptions {
                debug: false,
                no_animations: true
            })
        );
    }

    #[test]
    fn update_version_help() {
        assert_eq!(parse(&["update"]), Mode::Update);
        assert_eq!(parse(&["--version"]), Mode::Version);
        assert_eq!(parse(&["-v"]), Mode::Version);
        assert_eq!(parse(&["tui", "-v"]), Mode::Version);
        assert_eq!(parse(&["--help"]), Mode::Help);
        assert_eq!(parse(&["-h"]), Mode::Help);
    }

    #[test]
    fn unknown_argument_is_an_error() {
        assert_eq!(parse(&["frobnicate"]), Mode::Invalid("frobnicate".into()));
    }
}
