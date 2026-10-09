//! The packaged apps (macOS `.app`, Linux AppImage) put the `storingUnicorns` command on
//! the `PATH` themselves, as `scripts/install.sh` does for other installs: a symlink in
//! `/usr/local/bin` when writable (macOS), else in `~/.local/bin`, which is then added to
//! the shell's startup file if the `PATH` lacks it. Refreshed at every launch, so the link
//! follows the app when it moves. A `storingUnicorns` that isn't a link to the app (such
//! as the binary installed by `scripts/install.sh`) is never replaced.

// Windows has its own installers: nothing to link there.
#![cfg_attr(not(unix), allow(dead_code))]

use std::path::{Path, PathBuf};

/// The command created on the `PATH`.
const LINK_NAME: &str = "storingUnicorns";
/// A symlink whose target's file name starts with this is ours to update (the AppImage,
/// the binary in the bundle).
const OUR_PREFIX: &str = "storingUnicorns";
/// Comment written above the line added to a shell startup file.
const PROFILE_MARKER: &str = "# Added by storingUnicorns: the `storingUnicorns` command";

/// Install or refresh the link on a background thread (no effect outside a packaged app).
pub fn install_in_background() {
    #[cfg(unix)]
    std::thread::spawn(|| {
        if let Err(e) = install() {
            tracing::warn!("could not put `{LINK_NAME}` on the PATH: {e}");
        }
    });
}

#[cfg(unix)]
fn install() -> Result<(), String> {
    let Some(target) = packaged_executable() else {
        return Ok(());
    };
    let home = dirs::home_dir().ok_or("no home directory")?;
    let local_bin = home.join(".local").join("bin");
    let mut dirs = Vec::new();
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/usr/local/bin"));
    }
    dirs.push(local_bin.clone());

    for dir in dirs {
        if dir != local_bin && !dir.is_dir() {
            continue;
        }
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        match ensure_link(&dir.join(LINK_NAME), &target) {
            Ok(_) => {}
            // /usr/local/bin belongs to root on a fresh Mac: fall back to ~/.local/bin.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && dir != local_bin => {
                continue;
            }
            Err(e) => return Err(format!("linking {}: {e}", dir.display())),
        }
        if !on_path(&dir, &std::env::var("PATH").unwrap_or_default()) {
            let shell = std::env::var("SHELL").unwrap_or_default();
            if let Some((profile, line)) = profile_line(&shell, &home, &dir) {
                add_to_profile(&profile, &line)
                    .map_err(|e| format!("updating {}: {e}", profile.display()))?;
            }
        }
        return Ok(());
    }
    Ok(())
}

/// What `storingUnicorns` should point to: the `.AppImage` file, or the executable inside the `.app`.
#[cfg(unix)]
fn packaged_executable() -> Option<PathBuf> {
    if let Some(appimage) = crate::updater::running_appimage() {
        return Some(appimage);
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    (cfg!(target_os = "macos") && exe.to_string_lossy().contains(".app/Contents/MacOS/"))
        .then_some(exe)
}

/// Make `link` a symlink to `target`. Returns whether it was (re)created; an existing file
/// or a symlink to something else than this app is left alone.
#[cfg(unix)]
fn ensure_link(link: &Path, target: &Path) -> std::io::Result<bool> {
    match std::fs::symlink_metadata(link) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
        Ok(meta) if !meta.file_type().is_symlink() => return Ok(false),
        Ok(_) => {
            let current = std::fs::read_link(link)?;
            if current == target {
                return Ok(false);
            }
            let resolved = link.parent().unwrap_or(Path::new("/")).join(&current);
            let ours = !resolved.exists()
                || current
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(OUR_PREFIX));
            if !ours {
                return Ok(false);
            }
            std::fs::remove_file(link)?;
        }
    }
    std::os::unix::fs::symlink(target, link)?;
    Ok(true)
}

/// Whether `dir` is one of the entries of `path`.
fn on_path(dir: &Path, path: &str) -> bool {
    std::env::split_paths(path).any(|p| p == dir)
}

/// The startup file of `shell` and the line that adds `dir` to its `PATH`, for the shells
/// we know (zsh, bash, fish).
fn profile_line(shell: &str, home: &Path, dir: &Path) -> Option<(PathBuf, String)> {
    let name = Path::new(shell).file_name()?.to_str()?;
    let dir = dir.display();
    let export = format!("export PATH=\"{dir}:$PATH\"");
    match name {
        "zsh" => Some((home.join(".zshrc"), export)),
        // macOS terminals start login shells, which read .bash_profile, not .bashrc.
        "bash" if cfg!(target_os = "macos") => Some((home.join(".bash_profile"), export)),
        "bash" => Some((home.join(".bashrc"), export)),
        "fish" => Some((
            home.join(".config/fish/conf.d/storingUnicorns.fish"),
            format!("fish_add_path --global \"{dir}\""),
        )),
        _ => None,
    }
}

/// Append `line` (under our marker) to `profile`, unless it is already there.
fn add_to_profile(profile: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let current = match std::fs::read_to_string(profile) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    if current.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    if let Some(parent) = profile.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sep = if current.is_empty() || current.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(profile)?;
    write!(file, "{sep}\n{PROFILE_MARKER}\n{line}\n")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn link_is_created_then_left_as_is() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("storingUnicorns-linux-x64.AppImage");
        std::fs::write(&target, "").unwrap();
        let link = dir.path().join("storingUnicorns-link");
        assert!(ensure_link(&link, &target).unwrap());
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert!(!ensure_link(&link, &target).unwrap());
    }

    #[test]
    fn our_old_or_broken_link_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("old").join("storingUnicorns");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "").unwrap();
        let target = dir.path().join("new.AppImage");
        let link = dir.path().join("storingUnicorns-link");
        std::os::unix::fs::symlink(&old, &link).unwrap();
        assert!(ensure_link(&link, &target).unwrap());
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        // Broken link (the AppImage was moved): replaced too.
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(dir.path().join("gone"), &link).unwrap();
        assert!(ensure_link(&link, &target).unwrap());
    }

    #[test]
    fn foreign_cu_is_never_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("storingUnicorns");
        let other = dir.path().join("other-tool");
        std::fs::write(&other, "").unwrap();
        let link = dir.path().join("storingUnicorns-link");
        std::os::unix::fs::symlink(&other, &link).unwrap();
        assert!(!ensure_link(&link, &target).unwrap());
        assert_eq!(std::fs::read_link(&link).unwrap(), other);
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, "#!/bin/sh").unwrap();
        assert!(!ensure_link(&link, &target).unwrap());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "#!/bin/sh");
    }

    #[test]
    fn path_entries_are_compared_whole() {
        let dir = Path::new("/home/u/.local/bin");
        assert!(on_path(dir, "/usr/bin:/home/u/.local/bin"));
        assert!(!on_path(dir, "/usr/bin:/home/u/.local/bin2"));
        assert!(!on_path(dir, ""));
    }

    #[test]
    fn profile_per_shell() {
        let home = Path::new("/home/u");
        let dir = Path::new("/home/u/.local/bin");
        let (zsh, line) = profile_line("/bin/zsh", home, dir).unwrap();
        assert_eq!(zsh, home.join(".zshrc"));
        assert_eq!(line, "export PATH=\"/home/u/.local/bin:$PATH\"");
        let (fish, line) = profile_line("/usr/bin/fish", home, dir).unwrap();
        assert!(fish.ends_with("conf.d/storingUnicorns.fish"));
        assert_eq!(line, "fish_add_path --global \"/home/u/.local/bin\"");
        assert!(profile_line("/bin/bash", home, dir).is_some());
        assert!(profile_line("/usr/bin/nu", home, dir).is_none());
        assert!(profile_line("", home, dir).is_none());
    }

    #[test]
    fn profile_line_is_added_once() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join(".zshrc");
        std::fs::write(&profile, "alias ll='ls -l'").unwrap();
        add_to_profile(&profile, "export PATH=\"/x:$PATH\"").unwrap();
        add_to_profile(&profile, "export PATH=\"/x:$PATH\"").unwrap();
        let text = std::fs::read_to_string(&profile).unwrap();
        assert_eq!(
            text,
            format!("alias ll='ls -l'\n\n{PROFILE_MARKER}\nexport PATH=\"/x:$PATH\"\n")
        );
        // Created along with its directory (fish's conf.d).
        let fish = dir.path().join("conf.d").join("su.fish");
        add_to_profile(&fish, "fish_add_path --global \"/x\"").unwrap();
        assert!(std::fs::read_to_string(&fish)
            .unwrap()
            .ends_with("\"/x\"\n"));
    }
}
