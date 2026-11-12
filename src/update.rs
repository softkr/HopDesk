//! Update check and in-place install from the GitHub releases of this repository.
//!
//! Downloads use the system `curl` (bundled with macOS, Linux distributions and Windows 10+) over
//! HTTPS only, and every package is verified against the release's `SHA256SUMS.txt` before it
//! replaces the installed app.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, bail, ensure};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/softkr/HopDesk/releases/latest";
const CHECKSUMS_ASSET: &str = "SHA256SUMS.txt";
/// Generic client name: no user or machine details leave the computer.
const USER_AGENT: &str = "HopDesk-updater";
const CHECK_TIMEOUT_SECS: &str = "15";
const DOWNLOAD_TIMEOUT_SECS: &str = "600";

/// A newer release with a package for this platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub page_url: String,
    package_name: String,
    package_url: String,
    checksums_url: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

/// Returns the latest release when it is newer than this build and has a package for this platform.
pub fn check() -> anyhow::Result<Option<Release>> {
    let body = curl_text(LATEST_RELEASE_URL, CHECK_TIMEOUT_SECS).context("query the latest release")?;
    let release: ApiRelease = serde_json::from_str(&body).context("parse the latest release")?;
    Ok(newer_release(release, env!("CARGO_PKG_VERSION")))
}

fn newer_release(release: ApiRelease, current: &str) -> Option<Release> {
    let version = release.tag_name.trim_start_matches('v').to_owned();
    if parse_version(&version)? <= parse_version(current)? {
        return None;
    }
    let package_name = format!("HopDesk-{version}-{}", package_suffix()?);
    let url_of = |name: &str| {
        release
            .assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    Some(Release {
        package_url: url_of(&package_name)?,
        checksums_url: url_of(CHECKSUMS_ASSET)?,
        package_name,
        version,
        page_url: release.html_url,
    })
}

/// `major.minor.patch`; anything else is not compared.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    let parsed = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(parsed)
}

/// Release package for this build, as named by the release workflow.
fn package_suffix() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-apple-silicon.dmg")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("macos-intel.dmg")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64.tar.gz")
    } else {
        None
    }
}

/// Downloads, verifies and installs `release` over the running app, then starts the new version.
/// The caller should exit right after.
pub fn install(release: &Release) -> anyhow::Result<()> {
    let work = std::env::temp_dir().join(format!("hopdesk-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).with_context(|| format!("create {}", work.display()))?;
    let result = std::env::current_exe()
        .context("locate the running app")
        .and_then(|current| download_and_install(release, &current, &work));
    let _ = fs::remove_dir_all(&work);
    platform::relaunch(&result?)
}

/// Downloads and verifies the package, then installs it over `current`; returns what to launch.
fn download_and_install(release: &Release, current: &Path, work: &Path) -> anyhow::Result<PathBuf> {
    let package = work.join(&release.package_name);
    curl_file(&release.package_url, &package).context("download the update")?;
    let sums = curl_text(&release.checksums_url, CHECK_TIMEOUT_SECS).context("download the checksums")?;
    verify_checksum(&package, &release.package_name, &sums)?;
    platform::install(&package, current, work)
}

#[cfg(test)]
impl Release {
    pub fn for_test(version: &str) -> Self {
        Self {
            version: version.to_owned(),
            page_url: String::new(),
            package_name: String::new(),
            package_url: String::new(),
            checksums_url: String::new(),
        }
    }
}

fn verify_checksum(file: &Path, name: &str, sums: &str) -> anyhow::Result<()> {
    let expected = expected_checksum(sums, name).with_context(|| format!("no checksum listed for {name}"))?;
    let bytes = fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let actual = sha256_hex(&bytes);
    ensure!(actual.eq_ignore_ascii_case(expected), "checksum mismatch for {name}: the download is corrupt or was altered");
    Ok(())
}

/// Hash listed for `name` in `sha256sum` output (`<hash>  <name>`, optionally `*<name>`).
fn expected_checksum<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then_some(hash)
    })
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn curl() -> Command {
    let mut command = Command::new("curl");
    command.args(["--fail", "--silent", "--show-error", "--location", "--proto", "=https", "--proto-redir", "=https"]);
    command.args(["--user-agent", USER_AGENT]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// CREATE_NO_WINDOW: no console flashes up from the GUI.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn curl_text(url: &str, timeout_secs: &str) -> anyhow::Result<String> {
    let output = curl().args(["--max-time", timeout_secs, url]).output().context("run curl")?;
    if !output.status.success() {
        bail!("curl failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    String::from_utf8(output.stdout).context("response is not UTF-8")
}

fn curl_file(url: &str, dest: &Path) -> anyhow::Result<()> {
    run(curl().args(["--max-time", DOWNLOAD_TIMEOUT_SECS, "--output"]).arg(dest).arg(url))
}

fn run(command: &mut Command) -> anyhow::Result<()> {
    let output = command.output().with_context(|| format!("run {:?}", command.get_program()))?;
    if !output.status.success() {
        bail!(
            "{:?} failed: {}",
            command.get_program(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Removes what a previous update left behind (Windows and Linux keep the replaced binary).
pub fn clean_up_previous() {
    if let Ok(current) = std::env::current_exe() {
        let _ = fs::remove_file(old_binary_path(&current));
    }
}

fn old_binary_path(current: &Path) -> PathBuf {
    let mut name = current.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    current.with_file_name(name)
}

/// Swaps the running binary for `new`: a running executable can be renamed but not overwritten.
#[cfg(not(target_os = "macos"))]
fn replace_binary(current: &Path, new: &Path) -> anyhow::Result<()> {
    let old = old_binary_path(current);
    let _ = fs::remove_file(&old);
    fs::rename(current, &old).with_context(|| format!("move {} aside (is the folder writable?)", current.display()))?;
    if let Err(error) = fs::copy(new, current) {
        let _ = fs::rename(&old, current);
        return Err(error).with_context(|| format!("write {}", current.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(current, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    /// Replaces the `.app` bundle the running binary belongs to; returns the bundle path.
    pub fn install(dmg: &Path, current: &Path, work: &Path) -> anyhow::Result<PathBuf> {
        let app = current
            .ancestors()
            .nth(3)
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .context("HopDesk is not running from an app bundle; install the update manually")?
            .to_owned();
        let parent = app.parent().context("app bundle has no parent folder")?;

        let mount = work.join("mount");
        run(Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
            .arg(&mount)
            .arg(dmg))
        .context("open the disk image")?;
        // Stage next to the installed app so the final swap is a rename on the same volume.
        let staged = parent.join(".HopDesk-update.app");
        let _ = fs::remove_dir_all(&staged);
        let copied = run(Command::new("ditto").arg(mount.join("HopDesk.app")).arg(&staged));
        let _ = run(Command::new("hdiutil").args(["detach", "-quiet"]).arg(&mount));
        copied.with_context(|| format!("copy the new app into {} (is the folder writable?)", parent.display()))?;

        let old = parent.join(".HopDesk-old.app");
        let _ = fs::remove_dir_all(&old);
        fs::rename(&app, &old).with_context(|| format!("move {} aside", app.display()))?;
        if let Err(error) = fs::rename(&staged, &app) {
            let _ = fs::rename(&old, &app);
            return Err(error).context("put the new app in place");
        }
        let _ = fs::remove_dir_all(&old);
        Ok(app)
    }

    pub fn relaunch(app: &Path) -> anyhow::Result<()> {
        run(Command::new("open").arg("-n").arg(app))
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub fn install(zip: &Path, current: &Path, work: &Path) -> anyhow::Result<PathBuf> {
        let unpacked = work.join("unpacked");
        fs::create_dir_all(&unpacked)?;
        // bsdtar ships with Windows 10 and reads zip archives.
        run(Command::new("tar").arg("-xf").arg(zip).arg("-C").arg(&unpacked)).context("unpack the update")?;
        replace_binary(current, &unpacked.join("HopDesk.exe"))?;
        Ok(current.to_owned())
    }

    pub fn relaunch(exe: &Path) -> anyhow::Result<()> {
        Command::new(exe).spawn().context("start the new version")?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    pub fn install(archive: &Path, current: &Path, work: &Path) -> anyhow::Result<PathBuf> {
        run(Command::new("tar").arg("-xzf").arg(archive).arg("-C").arg(work)).context("unpack the update")?;
        let folder = archive
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".tar.gz"))
            .context("unexpected package name")?;
        replace_binary(current, &work.join(folder).join("hopdesk"))?;
        Ok(current.to_owned())
    }

    pub fn relaunch(exe: &Path) -> anyhow::Result<()> {
        Command::new(exe).spawn().context("start the new version")?;
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
mod platform {
    use super::*;

    pub fn install(_package: &Path, _current: &Path, _work: &Path) -> anyhow::Result<PathBuf> {
        bail!("updates are not supported on this platform")
    }

    pub fn relaunch(_app: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_release(tag: &str, assets: &[&str]) -> ApiRelease {
        ApiRelease {
            tag_name: tag.to_owned(),
            html_url: format!("https://github.com/softkr/HopDesk/releases/tag/{tag}"),
            assets: assets
                .iter()
                .map(|name| ApiAsset {
                    name: (*name).to_owned(),
                    browser_download_url: format!("https://example.invalid/{name}"),
                })
                .collect(),
        }
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(parse_version("0.1.10") > parse_version("0.1.9"));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3-beta"), None);
    }

    #[test]
    fn only_a_newer_release_with_this_platforms_package_is_offered() {
        let suffix = package_suffix().expect("tests run on a supported platform");
        let package = format!("HopDesk-0.2.0-{suffix}");
        let offered = newer_release(api_release("v0.2.0", &[&package, CHECKSUMS_ASSET]), "0.1.3")
            .expect("newer release is offered");
        assert_eq!(offered.version, "0.2.0");
        assert_eq!(offered.package_name, package);

        assert_eq!(newer_release(api_release("v0.1.3", &[&package, CHECKSUMS_ASSET]), "0.1.3"), None);
        assert_eq!(newer_release(api_release("v0.2.0", &[CHECKSUMS_ASSET]), "0.1.3"), None);
        assert_eq!(newer_release(api_release("v0.2.0", &[&package]), "0.1.3"), None);
    }

    #[test]
    fn checksum_is_read_from_sha256sum_output() {
        let sums = "aaa  HopDesk-0.2.0-windows-x64.zip\nbbb *HopDesk-0.2.0-macos-intel.dmg\n";
        assert_eq!(expected_checksum(sums, "HopDesk-0.2.0-windows-x64.zip"), Some("aaa"));
        assert_eq!(expected_checksum(sums, "HopDesk-0.2.0-macos-intel.dmg"), Some("bbb"));
        assert_eq!(expected_checksum(sums, "HopDesk-0.2.0-linux-x86_64.tar.gz"), None);
    }

    /// End to end against the published latest release: replaces a copy of the locally built app.
    /// Run with `cargo test -- --ignored installs_the_latest_release` after `scripts/bundle-macos.sh`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "downloads the latest release; needs target/release/HopDesk.app"]
    fn installs_the_latest_release() {
        let root = std::env::temp_dir().join(format!("hopdesk-update-e2e-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("apps")).expect("create temp dir");
        let app = root.join("apps/HopDesk.app");
        let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/HopDesk.app");
        run(Command::new("ditto").arg(&built).arg(&app)).expect("copy the built app");

        let release = check().expect("check").expect("the published release is newer than 0.1.0");
        let work = root.join("work");
        fs::create_dir_all(&work).expect("create work dir");
        let installed = download_and_install(&release, &app.join("Contents/MacOS/hopdesk"), &work).expect("install");

        assert_eq!(installed, app);
        let plist = fs::read_to_string(app.join("Contents/Info.plist")).expect("read Info.plist");
        assert!(plist.contains(&format!("<string>{}</string>", release.version)), "{plist}");
        let leftovers: Vec<_> = fs::read_dir(root.join("apps")).expect("list").flatten().map(|e| e.file_name()).collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("HopDesk.app")]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tampered_package_is_rejected() {
        let dir = std::env::temp_dir().join(format!("hopdesk-update-test-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let file = dir.join("pkg");
        fs::write(&file, b"package").expect("write package");
        let good = format!("{}  pkg\n", sha256_hex(b"package"));
        assert!(verify_checksum(&file, "pkg", &good).is_ok());
        let bad = format!("{}  pkg\n", sha256_hex(b"other"));
        assert!(verify_checksum(&file, "pkg", &bad).is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
