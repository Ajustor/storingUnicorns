# CLI dispatch & self-updater — Implementation Plan (2/4)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add argument dispatch (`gui` default, `tui`, `update`, `--version`, `--help`), a frontend-agnostic self-updater reading `latest.json` from GitHub Pages, and correct console behaviour on Windows.

**Architecture:** `src/cli.rs` parses args into a `Mode` (pure, unit-tested). `src/updater/` is ported from codingUnicorns (`Ajustor/codingUnicorns@2a994a7:src/updater/mod.rs`) with the `egui::Context` dependency replaced by a repaint callback, plus blocking helpers for the CLI. `update-notifier` (crates.io) is removed. Until plan 3 lands, `Mode::Gui` falls back to the TUI.

**Tech Stack:** ureq 3, semver 1, sha2 0.10, self-replace 1, serde_json; Win32 `kernel32` console APIs.

Prerequisite: plan 1 merged on the branch. Conventions: `rtk` prefix, green `rtk cargo test` before each commit, commit trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

---

### Task 1: `cli.rs` — argument parsing

**Files:**
- Create: `src/cli.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write the failing tests** in `src/cli.rs`

```rust
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
            Mode::Tui(TuiOptions { debug: true, no_animations: true })
        );
    }

    #[test]
    fn legacy_flags_alone_launch_tui() {
        assert_eq!(parse(&["--debug"]), Mode::Tui(TuiOptions { debug: true, no_animations: false }));
        assert_eq!(parse(&["--no-animations"]), Mode::Tui(TuiOptions { debug: false, no_animations: true }));
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
```

- [ ] **Step 2: Run to verify failure**

Add `mod cli;` to `src/main.rs`, then run: `rtk cargo test cli`
Expected: FAIL — `Mode` not found.

- [ ] **Step 3: Implement** (top of `src/cli.rs`)

```rust
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
```

- [ ] **Step 4: Run tests**

Run: `rtk cargo test cli`
Expected: 5 PASS.

- [ ] **Step 5: Wire `main.rs` and `tui::run`**

`src/tui/mod.rs`: change the signature to `pub async fn run(opts: crate::cli::TuiOptions) -> Result<()>`, delete the in-function argument parsing (`args`, `debug_mode`, `no_animations`, `version` and the `--version` early return), and use `opts.debug` / `opts.no_animations` where `debug_mode` / `no_animations` were used.

`src/main.rs`:

```rust
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
            eprintln!("Unknown argument: {arg}

{}", cli::HELP);
            std::process::exit(2);
        }
    }
}

fn run_tui(opts: cli::TuiOptions) -> anyhow::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(tui::run(opts))
}
```

- [ ] **Step 6: Verify**

Run: `cargo run -- --help`, `cargo run -- -v`, `cargo run -- tui --no-animations` (quit with `q`), `cargo run -- nope; echo $?`
Expected: help text; version; TUI; error + exit code 2.

- [ ] **Step 7: Commit**

```bash
rtk git add src/cli.rs src/main.rs src/tui/mod.rs
rtk git commit -m "feat: command-line dispatch with tui/update subcommands"
```

---

### Task 2: Updater core (manifest, selection, digest)

**Files:**
- Create: `src/updater/mod.rs`
- Modify: `Cargo.toml`, `src/main.rs`

- [ ] **Step 1: Dependencies** — add to `[dependencies]` in `Cargo.toml`:

```toml
# Self-update from GitHub Pages
ureq = "3"
semver = "1"
sha2 = "0.10"
self-replace = "1"
```

- [ ] **Step 2: Write the failing tests** — create `src/updater/mod.rs` containing only:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, assets: &[&str]) -> Manifest {
        Manifest {
            version: version.into(),
            notes: "notes".into(),
            page_url: "https://ajustor.github.io/storingUnicorns/".into(),
            assets: assets
                .iter()
                .map(|n| ManifestAsset {
                    name: n.to_string(),
                    url: format!("https://example.test/{n}"),
                    sha256: "ab".into(),
                })
                .collect(),
        }
    }

    fn v(s: &str) -> semver::Version {
        semver::Version::parse(s).unwrap()
    }

    #[test]
    fn older_or_equal_release_is_not_an_update() {
        let all = &[MSI_ASSET, "storingUnicorns-linux-x64"];
        assert!(select_update(manifest("1.0.0", all), &v("1.0.0"), InstallKind::Msi).unwrap().is_none());
        assert!(select_update(manifest("0.9.0", all), &v("1.0.0"), InstallKind::Msi).unwrap().is_none());
    }

    #[test]
    fn newer_release_picks_msi_asset() {
        let info = select_update(manifest("v1.1.0", &[MSI_ASSET]), &v("1.0.0"), InstallKind::Msi)
            .unwrap()
            .unwrap();
        assert_eq!(info.version, v("1.1.0"));
        assert_eq!(info.asset.name, MSI_ASSET);
        assert_eq!(info.notes, "notes");
    }

    #[test]
    fn newer_release_without_asset_errors() {
        assert!(select_update(manifest("2.0.0", &[]), &v("1.0.0"), InstallKind::Msi).is_err());
    }

    #[test]
    fn prerelease_ordering() {
        assert!(select_update(manifest("1.0.0-rc.1", &[MSI_ASSET]), &v("1.0.0"), InstallKind::Msi)
            .unwrap()
            .is_none());
    }

    #[test]
    fn binary_asset_for_this_platform() {
        if let Some(name) = BINARY_ASSET {
            let info = select_update(manifest("9.0.0", &[name]), &v("1.0.0"), InstallKind::ReplaceBinary)
                .unwrap()
                .unwrap();
            assert_eq!(info.asset.name, name);
        }
    }

    #[test]
    fn parses_manifest_written_by_release_workflow() {
        let json = r#"{"version":"0.9.0","notes":"n","page_url":"https://p/","assets":[{"name":"a","url":"https://u/a","sha256":"00"}]}"#;
        let m: Manifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.assets[0].name, "a");
        assert!(release_from_manifest("{not json", InstallKind::Msi).is_err());
    }

    #[test]
    fn digest_verification() {
        let sha_abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(verify_digest(b"abc", sha_abc).is_ok());
        assert!(verify_digest(b"abc", &sha_abc.to_uppercase()).is_ok());
        assert!(verify_digest(b"abd", sha_abc).unwrap_err().contains("checksum mismatch"));
        assert!(verify_digest(b"abc", "").is_err());
    }

    #[test]
    fn kind_for_location_detects_program_files() {
        let pf = Some(std::ffi::OsString::from(r"C:\Program Files"));
        assert_eq!(
            kind_for_location(Some(r"C:\Program Files\storingUnicorns\storingUnicorns.exe".into()), pf.clone()),
            InstallKind::Msi
        );
        assert_eq!(
            kind_for_location(Some(r"C:\Users\me\AppData\Local\Programs\storingUnicorns\storingUnicorns.exe".into()), pf),
            InstallKind::ReplaceBinary
        );
        assert_eq!(kind_for_location(None, None), InstallKind::ReplaceBinary);
    }

    #[test]
    fn stage_refuses_checksum_mismatch_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let mut info = select_update(manifest("9.0.0", &[MSI_ASSET]), &v("1.0.0"), InstallKind::Msi)
            .unwrap()
            .unwrap();
        info.asset.sha256 = "00".into();
        assert!(stage_and_apply(&info, b"payload", dir.path()).is_err());
        assert!(!dir.path().join(MSI_ASSET).exists());
    }

    #[test]
    fn stage_msi_writes_verified_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut info = select_update(manifest("9.0.0", &[MSI_ASSET]), &v("1.0.0"), InstallKind::Msi)
            .unwrap()
            .unwrap();
        info.asset.sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into();
        let staged = stage_and_apply(&info, b"abc", dir.path()).unwrap().unwrap();
        assert_eq!(std::fs::read(staged).unwrap(), b"abc");
    }

    #[test]
    fn encode_powershell_command_is_base64_utf16le() {
        // "A" in UTF-16LE is 0x41 0x00 → "QQA="
        assert_eq!(encode_powershell_command("A"), "QQA=");
    }
}
```

Add `mod updater;` to `src/main.rs`.

- [ ] **Step 3: Run to verify failure**

Run: `rtk cargo test updater`
Expected: FAIL — items not found.

- [ ] **Step 4: Implement the core** — prepend to `src/updater/mod.rs`:

```rust
//! Self-update from the release manifest published on GitHub Pages.
//!
//! The release workflow deploys `latest.json` plus the binaries to GitHub Pages.
//! `fetch_latest()` reads the manifest and compares its version with
//! `CARGO_PKG_VERSION`; `download_and_apply()` downloads the asset for this
//! platform, verifies its SHA-256 against the manifest, then either replaces the
//! running executable (portable installs) or stages the `.msi` to run with
//! `msiexec` once the app has exited (Windows installs under Program Files).
//!
//! Ported from codingUnicorns (`src/updater/mod.rs`), without the egui dependency.

mod background;
pub use background::{UpdateEvent, UpdateState, Updater};

use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Written by the `pages` job of `.github/workflows/release.yml`.
const MANIFEST_URL: &str = "https://ajustor.github.io/storingUnicorns/latest.json";
const USER_AGENT: &str = concat!("storingUnicorns/", env!("CARGO_PKG_VERSION"));
/// Hard cap on downloaded asset size, as a guard against a runaway response.
const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const BINARY_ASSET: Option<&str> = Some("storingUnicorns-windows-x64.exe");
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const BINARY_ASSET: Option<&str> = Some("storingUnicorns-linux-x64");
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const BINARY_ASSET: Option<&str> = Some("storingUnicorns-macos-arm64");
#[cfg(not(any(
    all(target_os = "windows", target_arch = "x86_64"),
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "aarch64"),
)))]
const BINARY_ASSET: Option<&str> = None;

const MSI_ASSET: &str = "storingUnicorns-setup.msi";

/// `latest.json` on GitHub Pages.
#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    notes: String,
    /// Human-facing download page.
    page_url: String,
    #[serde(default)]
    assets: Vec<ManifestAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestAsset {
    name: String,
    url: String,
    /// Lowercase hex SHA-256 of the file.
    sha256: String,
}

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub version: semver::Version,
    pub notes: String,
    pub page_url: String,
    asset: ManifestAsset,
    kind: InstallKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallKind {
    /// Overwrite the running executable.
    ReplaceBinary,
    /// Run the MSI installer after exit (Windows, installed under Program Files).
    Msi,
}

/// What to do on exit to apply a ready update.
#[derive(Debug, Clone)]
pub enum ExitAction {
    Relaunch,
    /// Install the MSI; the app stays closed.
    RunMsi(PathBuf),
    /// Install the MSI, then start the upgraded app.
    RunMsiThenRelaunch(PathBuf),
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn http_get(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("request to {url} failed: {e}"))
}

/// Blocking: fetch the manifest and return the update for this build, if any.
pub fn fetch_latest() -> Result<Option<ReleaseInfo>, String> {
    let mut resp = http_get(MANIFEST_URL)?;
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("reading release manifest: {e}"))?;
    release_from_manifest(&text, install_kind())
}

fn release_from_manifest(text: &str, kind: InstallKind) -> Result<Option<ReleaseInfo>, String> {
    let manifest: Manifest =
        serde_json::from_str(text).map_err(|e| format!("parsing release manifest: {e}"))?;
    let current = semver::Version::parse(current_version())
        .map_err(|e| format!("bad current version: {e}"))?;
    select_update(manifest, &current, kind)
}

fn select_update(
    manifest: Manifest,
    current: &semver::Version,
    kind: InstallKind,
) -> Result<Option<ReleaseInfo>, String> {
    let version = semver::Version::parse(manifest.version.trim_start_matches('v'))
        .map_err(|e| format!("bad release version {:?}: {e}", manifest.version))?;
    if version <= *current {
        return Ok(None);
    }
    let wanted = match kind {
        InstallKind::Msi => MSI_ASSET,
        InstallKind::ReplaceBinary => BINARY_ASSET.ok_or("no prebuilt binary for this platform")?,
    };
    let asset = manifest
        .assets
        .iter()
        .find(|a| a.name == wanted)
        .cloned()
        .ok_or_else(|| format!("release v{version} has no asset {wanted}"))?;
    Ok(Some(ReleaseInfo { version, notes: manifest.notes, page_url: manifest.page_url, asset, kind }))
}

/// MSI installs live under Program Files, which a normal user can't write to,
/// so the installer must handle the upgrade.
fn install_kind() -> InstallKind {
    if cfg!(windows) {
        return kind_for_location(std::env::current_exe().ok(), std::env::var_os("ProgramFiles"));
    }
    InstallKind::ReplaceBinary
}

fn kind_for_location(exe: Option<PathBuf>, program_files: Option<std::ffi::OsString>) -> InstallKind {
    let in_program_files = exe
        .and_then(|exe| Some(exe.starts_with(program_files?)))
        .unwrap_or(false);
    if in_program_files {
        InstallKind::Msi
    } else {
        InstallKind::ReplaceBinary
    }
}

/// Blocking: download, verify and install. Returns the staged MSI for MSI
/// installs, `None` once the binary has been replaced.
pub fn download_and_apply(info: &ReleaseInfo) -> Result<Option<PathBuf>, String> {
    let mut resp = http_get(&info.asset.url)?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(MAX_ASSET_BYTES)
        .read_to_vec()
        .map_err(|e| format!("downloading {}: {e}", info.asset.name))?;
    let dir = std::env::temp_dir().join("storingUnicorns-update");
    stage_and_apply(info, &bytes, &dir)
}

fn stage_and_apply(info: &ReleaseInfo, bytes: &[u8], dir: &Path) -> Result<Option<PathBuf>, String> {
    verify_digest(bytes, &info.asset.sha256)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let path = dir.join(&info.asset.name);
    std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
    match info.kind {
        InstallKind::Msi => Ok(Some(path)),
        InstallKind::ReplaceBinary => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| format!("chmod {}: {e}", path.display()))?;
            }
            let res = self_replace::self_replace(&path).map_err(|e| format!("replacing executable: {e}"));
            let _ = std::fs::remove_file(&path);
            res.map(|_| None)
        }
    }
}

fn verify_digest(bytes: &[u8], expected: &str) -> Result<(), String> {
    if expected.is_empty() {
        return Err("release asset has no sha256; refusing to install".into());
    }
    let actual: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!("checksum mismatch (expected {expected}, got {actual})"))
    }
}

/// Apply `action` as the app shuts down. `args` are passed to the relaunched app.
pub fn run_exit_action(action: &ExitAction, args: &[&str]) {
    let result = match action {
        ExitAction::Relaunch => std::env::current_exe()
            .and_then(|exe| std::process::Command::new(exe).args(args).spawn().map(|_| ())),
        // `/passive` shows a progress bar and triggers UAC; MajorUpgrade in the
        // WiX manifest removes the previous version.
        ExitAction::RunMsi(msi) => std::process::Command::new("msiexec")
            .arg("/i")
            .arg(msi)
            .arg("/passive")
            .spawn()
            .map(|_| ()),
        ExitAction::RunMsiThenRelaunch(msi) => std::env::current_exe().and_then(|exe| {
            let script = msi_relaunch_script(msi, &exe);
            let mut cmd = std::process::Command::new("powershell.exe");
            cmd.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden"])
                .arg("-EncodedCommand")
                .arg(encode_powershell_command(&script));
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
            cmd.spawn().map(|_| ())
        }),
    };
    if let Err(e) = result {
        tracing::error!("failed to apply update on exit: {e}");
    }
}

/// PowerShell that installs `msi` and, on success, relaunches `exe`.
fn msi_relaunch_script(msi: &Path, exe: &Path) -> String {
    let literal = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "''"));
    let arg = |p: &Path| format!("'\"{}\"'", p.display().to_string().replace('\'', "''"));
    format!(
        "$p = Start-Process -FilePath 'msiexec.exe' -ArgumentList '/i',{msi},'/passive' -Wait -PassThru\n\
         # 3010: success, reboot required\n\
         if ($p.ExitCode -eq 0 -or $p.ExitCode -eq 3010) {{ Start-Process -FilePath {exe} }}\n",
        msi = arg(msi),
        exe = literal(exe),
    )
}

/// Encode `script` for `powershell -EncodedCommand` (base64 of UTF-16LE).
fn encode_powershell_command(script: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &b)| acc | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
```

Create a temporary `src/updater/background.rs` with only `pub enum UpdateEvent {} pub enum UpdateState {} pub struct Updater;` so the module compiles (Task 3 replaces it).

- [ ] **Step 5: Run tests**

Run: `rtk cargo test updater`
Expected: 10 PASS.

- [ ] **Step 6: Commit**

```bash
rtk git add Cargo.toml Cargo.lock src/updater src/main.rs
rtk git commit -m "feat: self-updater core reading latest.json from GitHub Pages"
```

---

### Task 3: Background updater state machine (for the GUI)

**Files:**
- Modify: `src/updater/background.rs`

- [ ] **Step 1: Write the failing tests** — replace the content of `src/updater/background.rs` with the tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn info(version: &str) -> ReleaseInfo {
        let json = format!(
            r#"{{"version":"{version}","notes":"","page_url":"p","assets":[{{"name":"storingUnicorns-setup.msi","url":"u","sha256":"00"}}]}}"#
        );
        super::super::release_from_manifest_for_tests(&json).unwrap().unwrap()
    }

    fn wait(u: &mut Updater) -> Option<UpdateEvent> {
        for _ in 0..200 {
            if let Some(ev) = u.poll() {
                return Some(ev);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn check_reports_available_and_skips_skipped_version() {
        let mut u = Updater::new(|| {});
        u.check_with(false, None, || Ok(Some(info("99.0.0"))));
        assert!(matches!(wait(&mut u), Some(UpdateEvent::Available(_))));
        assert!(matches!(u.state, UpdateState::Available(_)));

        let mut u = Updater::new(|| {});
        u.check_with(false, Some("99.0.0"), || Ok(Some(info("99.0.0"))));
        assert!(wait(&mut u).is_none());
        assert!(matches!(u.state, UpdateState::UpToDate));
    }

    #[test]
    fn up_to_date_and_errors_only_reported_when_manual() {
        let mut u = Updater::new(|| {});
        u.check_with(false, None, || Err("boom".into()));
        assert!(wait(&mut u).is_none());
        assert!(matches!(u.state, UpdateState::Failed(_)));

        let mut u = Updater::new(|| {});
        u.check_with(true, None, || Ok(None));
        assert!(matches!(wait(&mut u), Some(UpdateEvent::UpToDate)));
    }

    #[test]
    fn install_moves_to_ready_and_schedules_restart() {
        let mut u = Updater::new(|| {});
        u.state = UpdateState::Available(info("99.0.0"));
        u.install_with(|_| Ok(Some("x.msi".into())));
        assert!(matches!(u.state, UpdateState::Downloading(_)));
        assert!(matches!(wait(&mut u), Some(UpdateEvent::Ready)));
        u.schedule_restart();
        assert!(matches!(u.exit_action, Some(super::super::ExitAction::RunMsiThenRelaunch(_))));
    }

    #[test]
    fn install_error_is_reported() {
        let mut u = Updater::new(|| {});
        u.state = UpdateState::Available(info("99.0.0"));
        u.install_with(|_| Err("disk full".into()));
        assert!(matches!(wait(&mut u), Some(UpdateEvent::Error(e)) if e == "disk full"));
    }
}
```

In `src/updater/mod.rs`, add (after `release_from_manifest`):

```rust
#[cfg(test)]
pub(crate) fn release_from_manifest_for_tests(text: &str) -> Result<Option<ReleaseInfo>, String> {
    release_from_manifest(text, InstallKind::Msi)
}
```

- [ ] **Step 2: Run to verify failure**

Run: `rtk cargo test updater::background`
Expected: FAIL — `Updater::new`, `check_with`… not found.

- [ ] **Step 3: Implement** — prepend to `src/updater/background.rs`:

```rust
//! Non-blocking wrapper around the updater for interactive frontends.
//! Network and disk work runs on a background thread; results come back over a
//! channel drained by `poll()` (once per frame in the GUI).

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

use super::{download_and_apply, fetch_latest, ExitAction, ReleaseInfo};

#[derive(Debug, Clone)]
pub enum UpdateState {
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Downloading(ReleaseInfo),
    /// Installed (binary) or staged (MSI); a restart applies it.
    Ready(ReleaseInfo),
    Failed(String),
}

/// Surfaced by `poll()`, typically shown as a notification.
#[derive(Debug)]
pub enum UpdateEvent {
    UpToDate,
    Available(semver::Version),
    Ready,
    Error(String),
}

enum Msg {
    Checked(Result<Option<ReleaseInfo>, String>),
    Installed(Result<Option<PathBuf>, String>),
}

pub struct Updater {
    pub state: UpdateState,
    /// The "update available" banner was dismissed for this session.
    pub dismissed: bool,
    pub exit_action: Option<ExitAction>,
    manual: bool,
    staged_msi: Option<PathBuf>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

impl Updater {
    /// `repaint` is called from the worker thread after each result (the GUI
    /// passes `ctx.request_repaint()`).
    pub fn new(repaint: impl Fn() + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            state: UpdateState::Idle,
            dismissed: false,
            exit_action: None,
            manual: false,
            staged_msi: None,
            repaint: Arc::new(repaint),
            tx,
            rx,
        }
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.state, UpdateState::Checking | UpdateState::Downloading(_))
    }

    /// Manual checks report "up to date" and errors, and ignore `skipped_version`.
    pub fn check(&mut self, manual: bool, skipped_version: Option<&str>) {
        self.check_with(manual, skipped_version, fetch_latest);
    }

    fn check_with(
        &mut self,
        manual: bool,
        skipped_version: Option<&str>,
        fetch: fn() -> Result<Option<ReleaseInfo>, String>,
    ) {
        if self.is_busy() || matches!(self.state, UpdateState::Ready(_)) {
            return;
        }
        self.manual = manual;
        self.dismissed = false;
        self.state = UpdateState::Checking;
        let skipped = if manual { None } else { skipped_version.and_then(|v| semver::Version::parse(v).ok()) };
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let res = fetch().map(|r| r.filter(|info| Some(&info.version) != skipped.as_ref()));
            let _ = tx.send(Msg::Checked(res));
            repaint();
        });
    }

    pub fn install(&mut self) {
        self.install_with(download_and_apply);
    }

    fn install_with(&mut self, apply: fn(&ReleaseInfo) -> Result<Option<PathBuf>, String>) {
        let UpdateState::Available(info) = &self.state else {
            return;
        };
        let info = info.clone();
        self.state = UpdateState::Downloading(info.clone());
        let tx = self.tx.clone();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let res = apply(&info);
            let _ = tx.send(Msg::Installed(res));
            repaint();
        });
    }

    /// Apply the update when the app exits, then relaunch it.
    pub fn schedule_restart(&mut self) {
        if !matches!(self.state, UpdateState::Ready(_)) {
            return;
        }
        self.exit_action = Some(match &self.staged_msi {
            Some(msi) => ExitAction::RunMsiThenRelaunch(msi.clone()),
            None => ExitAction::Relaunch,
        });
    }

    /// On plain quit, a staged MSI still has to run (a replaced binary doesn't).
    pub fn schedule_on_quit(&mut self) {
        if self.exit_action.is_none() {
            if let (UpdateState::Ready(_), Some(msi)) = (&self.state, &self.staged_msi) {
                self.exit_action = Some(ExitAction::RunMsi(msi.clone()));
            }
        }
    }

    pub fn poll(&mut self) -> Option<UpdateEvent> {
        match self.rx.try_recv().ok()? {
            Msg::Checked(Ok(Some(info))) => {
                let v = info.version.clone();
                self.state = UpdateState::Available(info);
                Some(UpdateEvent::Available(v))
            }
            Msg::Checked(Ok(None)) => {
                self.state = UpdateState::UpToDate;
                self.manual.then_some(UpdateEvent::UpToDate)
            }
            Msg::Checked(Err(e)) => {
                tracing::warn!("update check failed: {e}");
                self.state = UpdateState::Failed(e.clone());
                self.manual.then_some(UpdateEvent::Error(e))
            }
            Msg::Installed(res) => {
                let UpdateState::Downloading(info) = std::mem::replace(&mut self.state, UpdateState::Idle) else {
                    return None;
                };
                match res {
                    Ok(staged) => {
                        self.staged_msi = staged;
                        self.state = UpdateState::Ready(info);
                        Some(UpdateEvent::Ready)
                    }
                    Err(e) => {
                        self.state = UpdateState::Failed(e.clone());
                        Some(UpdateEvent::Error(e))
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `rtk cargo test updater`
Expected: 14 PASS.

- [ ] **Step 5: Commit**

```bash
rtk git add src/updater
rtk git commit -m "feat: background updater state machine"
```

---

### Task 4: `update` command, TUI post-exit check, skipped version

**Files:**
- Modify: `src/main.rs`, `src/tui/mod.rs`, `src/engine/config/mod.rs`, `Cargo.toml`

- [ ] **Step 1: Add the config fields** — in `AppConfig` (`src/engine/config/mod.rs`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub connections: Vec<ConnectionConfig>,
    pub last_connection: Option<String>,
    /// Release the user chose not to be offered again.
    #[serde(default)]
    pub skipped_version: Option<String>,
    /// GUI theme: "system" (default), "dark" or "light".
    #[serde(default)]
    pub theme: Option<String>,
}
```

Add a test at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_config_without_new_fields_still_parses() {
        let c: AppConfig = toml::from_str("connections = []").unwrap();
        assert!(c.skipped_version.is_none() && c.theme.is_none());
    }
}
```

Run: `rtk cargo test config` → PASS.

- [ ] **Step 2: Implement `update` in `src/main.rs`**

Replace the `Mode::Update` arm with `Mode::Update => run_update(),` and add:

```rust
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
        None => println!("Updated to v{}. Restart storingUnicorns to use it.", info.version),
        Some(msi) => {
            println!("Launching the installer…");
            updater::run_exit_action(&updater::ExitAction::RunMsi(msi), &[]);
        }
    }
    Ok(())
}
```

`main` is synchronous, so `run_update` can do blocking HTTP directly.

- [ ] **Step 3: Replace `update-notifier` in the TUI**

In `src/tui/mod.rs`, remove `use update_notifier::check_version;` and replace the `check_version(…)` call at the end of `run()` with:

```rust
// Check for updates after the TUI exits so the message is visible.
let skipped = state.config.skipped_version.clone();
let check = tokio::task::spawn_blocking(crate::updater::fetch_latest);
if let Ok(Ok(Ok(Some(info)))) = tokio::time::timeout(Duration::from_secs(5), check).await {
    if skipped.as_deref() != Some(info.version.to_string().as_str()) {
        println!(
            "storingUnicorns v{} is available (current v{}). Run `storingUnicorns update` to install it.",
            info.version,
            crate::updater::current_version()
        );
    }
}
```

 Remove `update-notifier = "0.1.9"` from `Cargo.toml`.

- [ ] **Step 4: Build and test**

Run: `rtk cargo build && rtk cargo test`
Expected: builds without `update_notifier`; all tests PASS.

- [ ] **Step 5: Manual check**

Run: `cargo run -- update`
Expected (before the first Pages release exists): `update check failed: request to https://ajustor.github.io/storingUnicorns/latest.json failed: …404…` and a non-zero exit. This is correct until plan 4 publishes the manifest.

- [ ] **Step 6: Commit**

```bash
rtk git add -A src Cargo.toml Cargo.lock
rtk git commit -m "feat: update subcommand, replace update-notifier with Pages manifest"
```

---

### Task 5: Windows console behaviour

Design note (deviation from the spec, recorded in the spec in Step 5): a `windows`-subsystem binary that attaches to its parent console makes the shell return immediately and fight the TUI for keyboard input. We keep the **console** subsystem and instead, in GUI mode:
- launched from Explorer / Start menu (the console was created just for us) → `FreeConsole()` so the window disappears;
- launched from a terminal (the console is shared with the shell) → respawn ourselves detached and exit, so the shell prompt returns immediately like any GUI app.

**Files:**
- Create: `src/console.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Write `src/console.rs`**

```rust
//! Windows console handling for the GUI mode (no-op elsewhere).

/// Environment flag set on the detached GUI child, to avoid respawning forever.
const DETACHED_ENV: &str = "STORINGUNICORNS_DETACHED";

/// What `main` should do before starting the GUI.
#[derive(Debug, PartialEq, Eq)]
pub enum GuiLaunch {
    /// Start the GUI in this process.
    Continue,
    /// A detached copy was started; exit now.
    Exit,
}

#[cfg(windows)]
mod sys {
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
        pub fn FreeConsole() -> i32;
    }
}

/// Prepare the process for running the GUI.
#[cfg(windows)]
pub fn prepare_gui() -> GuiLaunch {
    use std::os::windows::process::CommandExt;

    if std::env::var_os(DETACHED_ENV).is_some() {
        unsafe { sys::FreeConsole() };
        return GuiLaunch::Continue;
    }
    let mut pids = [0u32; 4];
    let attached = unsafe { sys::GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    if attached <= 1 {
        // Our own console (double-click / shortcut): just hide it.
        unsafe { sys::FreeConsole() };
        return GuiLaunch::Continue;
    }
    // Shared with a shell: start a detached copy so the prompt returns.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    let spawned = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env(DETACHED_ENV, "1")
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()
    });
    match spawned {
        Ok(_) => GuiLaunch::Exit,
        // Could not detach: run attached rather than not at all.
        Err(_) => GuiLaunch::Continue,
    }
}

#[cfg(not(windows))]
pub fn prepare_gui() -> GuiLaunch {
    GuiLaunch::Continue
}
```

- [ ] **Step 2: Declare the module** — add `mod console;` to `src/main.rs` and put `#[allow(dead_code)]` on both `prepare_gui` functions. It is not called yet: while `Mode::Gui` still runs the TUI, freeing the console would break it. Plan 3, Task 1 wires it in front of `gui::run`.

- [ ] **Step 3: Build on Windows**

Run: `rtk cargo build`
Expected: compiles (the `#[link(name = "kernel32")]` block links against the system kernel32).

- [ ] **Step 4: Record the deviation in the spec**

In `docs/superpowers/specs/2026-10-08-gui-distribution-design.md`, replace the paragraph starting "Sous Windows, le binaire est compilé en sous-système `windows`" with:

```markdown
Sous Windows, le binaire reste en sous-système console (un binaire `windows` qui se
rattache à la console parente laisse le shell rendre la main et se disputer le
clavier avec le TUI). En mode GUI : lancé hors terminal (double-clic, menu
Démarrer), la console créée pour l'occasion est libérée (`FreeConsole`) ; lancé
depuis un terminal, le programme se relance détaché et rend la main au shell.
```

- [ ] **Step 5: Commit**

```bash
rtk git add src/console.rs src/main.rs docs/superpowers/specs/2026-10-08-gui-distribution-design.md
rtk git commit -m "feat: Windows console handling for GUI launches"
```
