//! An app started from the Finder or the Dock gets launchd's minimal `PATH`
//! (`/usr/bin:/bin:/usr/sbin:/sbin`): no Homebrew, so the Azure CLI (`az`, Azure AD sign-in)
//! would not be found. Like VS Code, take the `PATH` of the user's login shell instead.
//!
//! The shell writes its `PATH` to a file rather than to a pipe: a startup file that starts
//! a background process (ssh-agent, gpg-agent, a plugin's update check...) hands it the
//! shell's stdout, and reading a pipe to its end then waited for that process, so the app
//! never opened its window.

// Only macOS imports the PATH; the helpers stay compiled (and tested) everywhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::time::Duration;

/// launchd's default `PATH` for GUI apps.
const LAUNCHD_PATH: [&str; 4] = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
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
    // An interactive login shell, as a new Terminal window starts it.
    let mut cmd = std::process::Command::new(&shell);
    cmd.args(["-l", "-i", "-c"]);
    match shell_path(cmd, TIMEOUT) {
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

/// Run `shell` (its last argument a `-c`) with a command that writes its `PATH` to a
/// file, and read it. Gives up after `timeout`, killing the shell.
#[cfg(unix)]
fn shell_path(mut shell: std::process::Command, timeout: Duration) -> Option<String> {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;

    // Unique per call: the tests run several at once.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!("storingUnicorns-{}-{n}.path", std::process::id());
    let out = std::env::temp_dir().join(name);
    let out_str = out.to_str().filter(|s| !s.contains('\''))?;
    let _ = std::fs::remove_file(&out);
    let mut child = shell
        .arg(format!("printf '%s' \"$PATH\" > '{out_str}'"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // Away from our process group: nothing it starts can signal the app.
        .process_group(0)
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&out);
                return None;
            }
        }
    }
    let path = std::fs::read_to_string(&out).ok();
    let _ = std::fs::remove_file(&out);
    path.map(|p| p.trim().to_string()).filter(|p| !p.is_empty())
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

    /// `sh -c '<startup>; eval "$0"' <our command>`: a stand-in for a login shell whose
    /// startup files run `startup`.
    #[cfg(unix)]
    fn fake_shell(startup: &str) -> std::process::Command {
        let mut cmd = std::process::Command::new("sh");
        cmd.env("PATH", "/opt/tools/bin:/usr/bin:/bin")
            .arg("-c")
            .arg(format!("{startup}\neval \"$0\""));
        cmd
    }

    #[cfg(unix)]
    #[test]
    fn path_is_read_from_the_shell() {
        let path = shell_path(fake_shell("echo 'Welcome!'"), TIMEOUT);
        assert_eq!(path.as_deref(), Some("/opt/tools/bin:/usr/bin:/bin"));
    }

    #[cfg(unix)]
    #[test]
    fn a_background_process_started_by_the_shell_is_not_waited_for() {
        let start = std::time::Instant::now();
        let path = shell_path(fake_shell("sleep 30 &"), TIMEOUT);
        assert_eq!(path.as_deref(), Some("/opt/tools/bin:/usr/bin:/bin"));
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_stuck_shell_is_given_up_on() {
        let start = std::time::Instant::now();
        let path = shell_path(fake_shell("sleep 30"), Duration::from_millis(300));
        assert_eq!(path, None);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
    }
}
