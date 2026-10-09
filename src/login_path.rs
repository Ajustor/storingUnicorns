//! An app started from the Finder or the Dock gets launchd's minimal `PATH`
//! (`/usr/bin:/bin:/usr/sbin:/sbin`): no Homebrew, so the Azure CLI (`az`,
//! Azure AD sign-in) would not be found. Like VS Code, take the `PATH` of the
//! user's login shell instead.

// Only macOS imports the PATH; the helpers stay compiled (and tested) everywhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::time::Duration;

/// launchd's default `PATH` for GUI apps.
const LAUNCHD_PATH: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
/// Delimits the `PATH` in the shell's output, which startup files may clutter.
const MARKER: &str = "__STORINGUNICORNS_PATH__";
/// A slow or stuck shell startup file must not hold the window back for long.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Replace launchd's `PATH` with the login shell's. Started from a terminal, the `PATH`
/// is already the user's and is kept. Call before any thread is started.
#[cfg(target_os = "macos")]
pub fn import() {
    let current = std::env::var("PATH").unwrap_or_default();
    if !is_launchd_path(&current) {
        return;
    }
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/zsh".into());
    match login_shell_path(&shell) {
        Some(path) => std::env::set_var("PATH", path),
        None => tracing::warn!("could not read the PATH of {shell}; keeping {current}"),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn import() {}

/// Whether `path` only holds launchd's default directories (or is empty).
fn is_launchd_path(path: &str) -> bool {
    path.split(':')
        .filter(|dir| !dir.is_empty())
        .all(|dir| LAUNCHD_PATH.contains(&dir))
}

/// Run `shell` as an interactive login shell (what a new Terminal window does) and read
/// the `PATH` it ends up with.
#[cfg(target_os = "macos")]
fn login_shell_path(shell: &str) -> Option<String> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    let mut child = Command::new(shell)
        .args(["-l", "-i", "-c"])
        .arg(format!("printf '{MARKER}%s{MARKER}' \"$PATH\""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = String::new();
        let _ = stdout.read_to_string(&mut out);
        out
    });
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    parse_output(&reader.join().ok()?)
}

/// The `PATH` between the markers of the shell's output.
fn parse_output(out: &str) -> Option<String> {
    let start = out.find(MARKER)? + MARKER.len();
    let len = out[start..].find(MARKER)?;
    let path = out[start..start + len].trim();
    (!path.is_empty()).then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_path_is_recognised() {
        assert!(is_launchd_path("/usr/bin:/bin:/usr/sbin:/sbin"));
        assert!(is_launchd_path("/usr/bin:/bin"));
        assert!(is_launchd_path(""));
        assert!(!is_launchd_path(
            "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        ));
    }

    #[test]
    fn path_is_read_between_the_markers() {
        let out = format!("motd\nWelcome!\n{MARKER}/opt/homebrew/bin:/usr/bin{MARKER}");
        assert_eq!(
            parse_output(&out).as_deref(),
            Some("/opt/homebrew/bin:/usr/bin")
        );
        assert_eq!(parse_output("no markers"), None);
        assert_eq!(parse_output(&format!("{MARKER}{MARKER}")), None);
        assert_eq!(parse_output(&format!("{MARKER}/usr/bin")), None);
    }
}
