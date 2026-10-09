//! Self-update from the release manifest published on GitHub Pages.
//!
//! The release workflow deploys `latest.json` plus the binaries to GitHub Pages.
//! `fetch_latest()` reads the manifest and compares its version with
//! `CARGO_PKG_VERSION`; `download_and_apply()` downloads the asset for this
//! platform, verifies its SHA-256 against the manifest, then either replaces the
//! running executable (portable installs, the macOS `.app` bundle), replaces the
//! `.AppImage` file the app runs from (Linux AppImage), or stages the `.msi` to
//! run with `msiexec` once the app has exited (Windows installs under Program
//! Files).
//!
//! Ported from codingUnicorns (`src/updater/mod.rs`), without the egui dependency.

mod background;
pub use background::{UpdateEvent, UpdateState, Updater};

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Written by the `pages` job of `.github/workflows/release.yml`.
const MANIFEST_URL: &str = "https://ajustor.github.io/storingUnicorns/latest.json";
const USER_AGENT: &str = concat!("storingUnicorns/", env!("CARGO_PKG_VERSION"));
/// Hard cap on downloaded asset size, as a guard against a runaway response.
const MAX_ASSET_BYTES: u64 = 256 * 1024 * 1024;
/// Name prefix of staging files and directories.
const STAGING_PREFIX: &str = "storingUnicorns-update";

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

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const APPIMAGE_ASSET: Option<&str> = Some("storingUnicorns-linux-x64.AppImage");
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
const APPIMAGE_ASSET: Option<&str> = None;

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
    // Used by the GUI (plan 3b).
    #[allow(dead_code)]
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
    /// Overwrite the `.AppImage` file the app was started from (Linux).
    AppImage,
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

/// Connection establishment (incl. TLS handshake).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Whole manifest request.
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Whole asset download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Waiting for response headers on the asset request. ureq 3 has no
/// per-read idle timeout (`timeout_recv_body` is a total budget), so a
/// stalled body is bounded by `DOWNLOAD_TIMEOUT` only.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// Trust the system's certificate store (Keychain, Windows store, system CA
/// bundle) rather than the roots bundled in the binary, so a corporate proxy
/// or an antivirus inspecting HTTPS doesn't fail with "UnknownIssuer".
fn tls_config() -> ureq::tls::TlsConfig {
    ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build()
}

fn manifest_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(tls_config())
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(MANIFEST_TIMEOUT))
        .build()
        .new_agent()
}

fn download_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(tls_config())
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .new_agent()
}

fn http_get(agent: &ureq::Agent, url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    agent
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| format!("request to {url} failed: {e}"))
}

/// Whether the user chose to skip `info`'s version. `skipped` may carry a
/// leading `v`; an unparsable value skips nothing.
pub fn is_skipped(info: &ReleaseInfo, skipped: Option<&str>) -> bool {
    skipped
        .and_then(|v| semver::Version::parse(v.trim_start_matches('v')).ok())
        .is_some_and(|v| v == info.version)
}

/// Minimum time between automatic (post-exit) update checks.
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Whether an automatic check is due, given the unix time of the last
/// completed one.
pub fn should_check(last: Option<u64>, now: u64) -> bool {
    match last {
        None => true,
        // A timestamp in the future (clock changed) must not block checks.
        Some(last) => last > now || now - last > CHECK_INTERVAL_SECS,
    }
}

/// `last_update_check` next to the config file; holds unix seconds.
fn last_check_path() -> Option<PathBuf> {
    crate::engine::config::AppConfig::config_path()
        .ok()
        .map(|p| p.with_file_name("last_update_check"))
}

/// Unix time of the last completed automatic check, if recorded.
pub fn last_check() -> Option<u64> {
    std::fs::read_to_string(last_check_path()?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Remember that an automatic check completed at `now` (best effort).
pub fn record_check(now: u64) {
    let Some(path) = last_check_path() else {
        return;
    };
    if let Err(e) = std::fs::write(&path, now.to_string()) {
        tracing::debug!("writing {}: {e}", path.display());
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Blocking: fetch the manifest and return the update for this build, if any.
pub fn fetch_latest() -> Result<Option<ReleaseInfo>, String> {
    let mut resp = http_get(&manifest_agent(), MANIFEST_URL)?;
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

#[cfg(test)]
pub(crate) fn release_from_manifest_for_tests(text: &str) -> Result<Option<ReleaseInfo>, String> {
    release_from_manifest(text, InstallKind::Msi)
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
        InstallKind::AppImage => APPIMAGE_ASSET.ok_or("no AppImage for this platform")?,
    };
    let asset = manifest
        .assets
        .iter()
        .find(|a| a.name == wanted)
        .cloned()
        .ok_or_else(|| format!("release v{version} has no asset {wanted}"))?;
    Ok(Some(ReleaseInfo {
        version,
        notes: manifest.notes,
        page_url: manifest.page_url,
        asset,
        kind,
    }))
}

/// MSI installs live under Program Files, which a normal user can't write to,
/// so the installer must handle the upgrade. An AppImage is replaced as a whole.
fn install_kind() -> InstallKind {
    if cfg!(windows) {
        return kind_for_location(
            std::env::current_exe().ok(),
            std::env::var_os("ProgramFiles"),
        );
    }
    if running_appimage().is_some() {
        return InstallKind::AppImage;
    }
    InstallKind::ReplaceBinary
}

/// The `.AppImage` file this process runs from, if any.
pub(crate) fn running_appimage() -> Option<PathBuf> {
    appimage_of(
        std::env::current_exe().ok()?,
        std::env::var_os("APPIMAGE")?,
        std::env::var_os("APPDIR")?,
    )
}

/// The AppImage runtime sets `APPIMAGE` (the file) and `APPDIR` (where the
/// image is mounted), and child processes inherit both: only trust them when
/// `exe` really lives in `APPDIR`.
fn appimage_of(
    exe: PathBuf,
    appimage: std::ffi::OsString,
    appdir: std::ffi::OsString,
) -> Option<PathBuf> {
    let inside = !appdir.is_empty() && exe.starts_with(&appdir);
    (inside && !appimage.is_empty()).then(|| PathBuf::from(appimage))
}

/// What to start to relaunch the app: the AppImage file when running from one
/// (the executable itself lives in the image's mount, gone once we exit).
fn relaunch_exe() -> std::io::Result<PathBuf> {
    match running_appimage() {
        Some(appimage) => Ok(appimage),
        None => std::env::current_exe(),
    }
}

fn kind_for_location(
    exe: Option<PathBuf>,
    program_files: Option<std::ffi::OsString>,
) -> InstallKind {
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
    let mut resp = http_get(&download_agent(), &info.asset.url)?;
    let bytes = resp
        .body_mut()
        .with_config()
        .limit(MAX_ASSET_BYTES)
        .read_to_vec()
        .map_err(|e| format!("downloading {}: {e}", info.asset.name))?;
    stage_and_apply(info, &bytes, None)
}

/// Verify `bytes`, then stage and install them. Staging happens in freshly
/// created, uniquely named files/directories (never a predictable shared
/// path). `dir` overrides where they are created (tests); `None` uses the
/// executable's directory (binary) or the system temp dir (MSI).
fn stage_and_apply(
    info: &ReleaseInfo,
    bytes: &[u8],
    dir: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    verify_digest(bytes, &info.asset.sha256)?;
    match info.kind {
        InstallKind::Msi => stage_msi(&info.asset.name, bytes, dir).map(Some),
        InstallKind::ReplaceBinary => replace_binary(bytes, dir).map(|_| None),
        InstallKind::AppImage => {
            let appimage = running_appimage().ok_or("not running from an AppImage")?;
            replace_file(bytes, &appimage).map(|_| None)
        }
    }
}

/// Staging directories older than this are considered abandoned.
const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Best effort: remove abandoned staging directories (e.g. MSIs kept for
/// msiexec) from the system temp dir. Errors, such as directories owned by
/// another user, are ignored.
pub fn cleanup_stale_staging() {
    cleanup_stale_in(&std::env::temp_dir(), std::time::SystemTime::now());
}

fn cleanup_stale_in(root: &Path, now: std::time::SystemTime) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX)
        {
            continue;
        }
        // `DirEntry::metadata` doesn't follow symlinks: only real directories.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let stale = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age > STALE_STAGING_AGE);
        if meta.is_dir() && stale {
            let path = entry.path();
            match std::fs::remove_dir_all(&path) {
                Ok(()) => tracing::debug!("removed stale {}", path.display()),
                Err(e) => tracing::debug!("removing stale {}: {e}", path.display()),
            }
        }
    }
}

/// Create a private (mode 0700 on unix) staging directory.
fn private_staging_dir(parent: Option<&Path>) -> Result<tempfile::TempDir, String> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(STAGING_PREFIX);
    match parent {
        Some(parent) => builder.tempdir_in(parent),
        None => builder.tempdir(),
    }
    .map_err(|e| format!("creating staging directory: {e}"))
}

/// Write the MSI into a new private directory that outlives this process, so
/// `msiexec` can read it after exit. Returns the MSI path.
fn stage_msi(name: &str, bytes: &[u8], parent: Option<&Path>) -> Result<PathBuf, String> {
    let dir = private_staging_dir(parent)?;
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
    let _kept = dir.keep();
    Ok(path)
}

/// Write the new binary to a unique temp file (next to the running executable
/// when possible, so the swap stays on one filesystem) and swap it in.
fn replace_binary(bytes: &[u8], dir: Option<&Path>) -> Result<(), String> {
    use std::io::Write;

    let near_exe = match dir {
        Some(dir) => Some(dir.to_path_buf()),
        None => std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf)),
    };
    let new_file = |dir: &Path| {
        tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempfile_in(dir)
    };
    // Keeps the fallback directory alive (and removes it on drop).
    let mut fallback_dir = None;
    let mut file = match near_exe.as_deref().map(new_file) {
        Some(Ok(file)) => file,
        _ => {
            let tmp = private_staging_dir(None)?;
            let file = new_file(tmp.path()).map_err(|e| format!("creating staging file: {e}"))?;
            fallback_dir = Some(tmp);
            file
        }
    };
    file.write_all(bytes)
        .and_then(|_| file.flush())
        .map_err(|e| format!("writing {}: {e}", file.path().display()))?;
    // Close the handle; the path is still deleted on drop.
    let path = file.into_temp_path();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", path.display()))?;
    }
    let res = self_replace::self_replace(&path).map_err(|e| format!("replacing executable: {e}"));
    drop(path);
    drop(fallback_dir);
    res
}

/// Swap `target` for an executable file holding `bytes`. The new file is
/// written next to `target` and renamed over it, so the swap is atomic and
/// the running AppImage (mounted from the old file, still open) keeps working
/// until the app exits.
fn replace_file(bytes: &[u8], target: &Path) -> Result<(), String> {
    use std::io::Write;

    let dir = target
        .parent()
        .ok_or_else(|| format!("bad AppImage path {}", target.display()))?;
    let mut file = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempfile_in(dir)
        .map_err(|e| format!("creating a file in {}: {e}", dir.display()))?;
    file.write_all(bytes)
        .and_then(|_| file.flush())
        .map_err(|e| format!("writing {}: {e}", file.path().display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", file.path().display()))?;
    }
    file.persist(target)
        .map(|_| ())
        .map_err(|e| format!("replacing {}: {e}", target.display()))
}

fn verify_digest(bytes: &[u8], expected: &str) -> Result<(), String> {
    if expected.is_empty() {
        return Err("release asset has no sha256; refusing to install".into());
    }
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch (expected {expected}, got {actual})"
        ))
    }
}

/// Apply `action` as the app shuts down. `args` are passed to the relaunched app.
/// Fails if the installer / relaunched process could not be started.
pub fn run_exit_action(action: &ExitAction, args: &[&str]) -> std::io::Result<()> {
    match action {
        ExitAction::Relaunch => relaunch_exe().and_then(|exe| {
            std::process::Command::new(exe)
                .args(args)
                .spawn()
                .map(|_| ())
        }),
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
        assert!(
            select_update(manifest("1.0.0", all), &v("1.0.0"), InstallKind::Msi)
                .unwrap()
                .is_none()
        );
        assert!(
            select_update(manifest("0.9.0", all), &v("1.0.0"), InstallKind::Msi)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn newer_release_picks_msi_asset() {
        let info = select_update(
            manifest("v1.1.0", &[MSI_ASSET]),
            &v("1.0.0"),
            InstallKind::Msi,
        )
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
        assert!(select_update(
            manifest("1.0.0-rc.1", &[MSI_ASSET]),
            &v("1.0.0"),
            InstallKind::Msi
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn binary_asset_for_this_platform() {
        if let Some(name) = BINARY_ASSET {
            let info = select_update(
                manifest("9.0.0", &[name]),
                &v("1.0.0"),
                InstallKind::ReplaceBinary,
            )
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
        assert!(verify_digest(b"abd", sha_abc)
            .unwrap_err()
            .contains("checksum mismatch"));
        assert!(verify_digest(b"abc", "").is_err());
    }

    // Windows paths: `\` is not a separator elsewhere, and only Windows calls it.
    #[cfg(windows)]
    #[test]
    fn kind_for_location_detects_program_files() {
        let pf = Some(std::ffi::OsString::from(r"C:\Program Files"));
        assert_eq!(
            kind_for_location(
                Some(r"C:\Program Files\storingUnicorns\storingUnicorns.exe".into()),
                pf.clone()
            ),
            InstallKind::Msi
        );
        assert_eq!(
            kind_for_location(
                Some(
                    r"C:\Users\me\AppData\Local\Programs\storingUnicorns\storingUnicorns.exe"
                        .into()
                ),
                pf
            ),
            InstallKind::ReplaceBinary
        );
        assert_eq!(kind_for_location(None, None), InstallKind::ReplaceBinary);
    }

    #[test]
    fn appimage_is_trusted_only_from_inside_its_mount() {
        let appimage = || std::ffi::OsString::from("/home/u/Apps/su.AppImage");
        let appdir = || std::ffi::OsString::from("/tmp/.mount_suAbc");
        let inside = PathBuf::from("/tmp/.mount_suAbc/usr/bin/storingUnicorns");
        assert_eq!(
            appimage_of(inside.clone(), appimage(), appdir()),
            Some(PathBuf::from("/home/u/Apps/su.AppImage"))
        );
        // Variables inherited by another copy started from a child shell.
        let elsewhere = PathBuf::from("/home/u/.local/bin/storingUnicorns");
        assert_eq!(appimage_of(elsewhere, appimage(), appdir()), None);
        // A component-wise prefix, not a string one.
        let sibling = PathBuf::from("/tmp/.mount_suAbcd/usr/bin/storingUnicorns");
        assert_eq!(appimage_of(sibling, appimage(), appdir()), None);
        assert_eq!(appimage_of(inside.clone(), appimage(), "".into()), None);
        assert_eq!(appimage_of(inside, "".into(), appdir()), None);
    }

    #[test]
    fn select_update_appimage_asset_for_this_platform() {
        let m = manifest("9.0.0", &[MSI_ASSET, "storingUnicorns-linux-x64.AppImage"]);
        let res = select_update(m, &v("1.0.0"), InstallKind::AppImage);
        match APPIMAGE_ASSET {
            Some(name) => {
                let info = res.unwrap().unwrap();
                assert_eq!(info.asset.name, name);
                assert_eq!(info.kind, InstallKind::AppImage);
            }
            None => assert!(res.is_err()),
        }
    }

    #[test]
    fn replace_file_swaps_in_an_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("su.AppImage");
        std::fs::write(&target, b"old").unwrap();
        replace_file(b"new", &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        // No temporary file left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn replace_file_reports_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("gone").join("su.AppImage");
        let err = replace_file(b"new", &target).unwrap_err();
        assert!(err.starts_with("creating"), "{err}");
    }

    #[test]
    fn stage_refuses_checksum_mismatch_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let mut info = select_update(
            manifest("9.0.0", &[MSI_ASSET]),
            &v("1.0.0"),
            InstallKind::Msi,
        )
        .unwrap()
        .unwrap();
        info.asset.sha256 = "00".into();
        assert!(stage_and_apply(&info, b"payload", Some(dir.path())).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn stage_msi_writes_verified_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut info = select_update(
            manifest("9.0.0", &[MSI_ASSET]),
            &v("1.0.0"),
            InstallKind::Msi,
        )
        .unwrap()
        .unwrap();
        info.asset.sha256 =
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into();
        let staged = stage_and_apply(&info, b"abc", Some(dir.path()))
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read(staged).unwrap(), b"abc");
    }

    #[test]
    fn stage_msi_uses_a_fresh_private_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut info = select_update(
            manifest("9.0.0", &[MSI_ASSET]),
            &v("1.0.0"),
            InstallKind::Msi,
        )
        .unwrap()
        .unwrap();
        info.asset.sha256 =
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into();
        let first = stage_and_apply(&info, b"abc", Some(dir.path()))
            .unwrap()
            .unwrap();
        let second = stage_and_apply(&info, b"abc", Some(dir.path()))
            .unwrap()
            .unwrap();
        let parent = first.parent().unwrap();
        assert_eq!(parent.parent().unwrap(), dir.path());
        assert_ne!(parent.file_name().unwrap(), STAGING_PREFIX);
        assert!(parent
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX));
        assert_eq!(first.file_name().unwrap(), MSI_ASSET);
        assert_ne!(first.parent(), second.parent());
    }

    #[test]
    fn encode_powershell_command_is_base64_utf16le() {
        // "A" in UTF-16LE is 0x41 0x00 → "QQA="
        assert_eq!(encode_powershell_command("A"), "QQA=");
    }

    #[test]
    fn is_skipped_ignores_v_prefix() {
        let info = select_update(
            manifest("1.2.0", &[MSI_ASSET]),
            &v("1.0.0"),
            InstallKind::Msi,
        )
        .unwrap()
        .unwrap();
        assert!(is_skipped(&info, Some("1.2.0")));
        assert!(is_skipped(&info, Some("v1.2.0")));
        assert!(!is_skipped(&info, Some("1.1.0")));
        assert!(!is_skipped(&info, Some("garbage")));
        assert!(!is_skipped(&info, None));
    }

    #[test]
    fn should_check_at_most_daily() {
        let day = 24 * 60 * 60;
        let now = 1_000_000;
        assert!(should_check(None, now));
        assert!(!should_check(Some(now), now));
        assert!(!should_check(Some(now - day), now));
        assert!(should_check(Some(now - day - 1), now));
        assert!(should_check(Some(now + 10), now));
    }

    #[test]
    fn encode_powershell_command_padding() {
        // One padding char for 4 bytes, none for 6 (3 UTF-16 units).
        assert_eq!(encode_powershell_command("AB"), "QQBCAA==");
        assert_eq!(encode_powershell_command("ABC"), "QQBCAEMA");
    }

    #[test]
    fn msi_relaunch_script_quotes_paths() {
        let script = msi_relaunch_script(
            Path::new(r"C:\Users\O'Brien\My Files\setup.msi"),
            Path::new(r"C:\Program Files\it's here\app.exe"),
        );
        assert!(
            script.contains(r#"'/i','"C:\Users\O''Brien\My Files\setup.msi"','/passive'"#),
            "{script}"
        );
        assert!(
            script.contains(r"Start-Process -FilePath 'C:\Program Files\it''s here\app.exe'"),
            "{script}"
        );
    }

    #[test]
    fn cleanup_removes_only_old_staging_dirs() {
        let root = tempfile::tempdir().unwrap();
        let staged = root.path().join(format!("{STAGING_PREFIX}abc123"));
        let other = root.path().join("unrelated");
        let file = root.path().join(format!("{STAGING_PREFIX}.txt"));
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join(MSI_ASSET), b"x").unwrap();
        std::fs::create_dir(&other).unwrap();
        std::fs::write(&file, b"x").unwrap();

        // Fresh: kept.
        cleanup_stale_in(root.path(), std::time::SystemTime::now());
        assert!(staged.exists());

        let later = std::time::SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60);
        cleanup_stale_in(root.path(), later);
        assert!(!staged.exists());
        assert!(other.exists());
        assert!(file.exists());
    }
}
