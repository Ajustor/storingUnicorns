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

fn manifest_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_global(Some(MANIFEST_TIMEOUT))
        .build()
        .new_agent()
}

fn download_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
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
/// so the installer must handle the upgrade.
fn install_kind() -> InstallKind {
    if cfg!(windows) {
        return kind_for_location(
            std::env::current_exe().ok(),
            std::env::var_os("ProgramFiles"),
        );
    }
    InstallKind::ReplaceBinary
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
        ExitAction::Relaunch => std::env::current_exe().and_then(|exe| {
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
}
