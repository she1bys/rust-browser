//! Updates: asks GitHub whether a newer release is out, downloads its
//! installer, checks it against the release's checksum list, and runs it.
//!
//! Nothing happens without the user: the browser only checks (once a day,
//! if "Check for updates automatically" is on, or when asked in Settings) and
//! shows an Update button. Clicking it downloads and verifies the installer,
//! then the browser closes, the installer updates it in a couple of seconds
//! and starts it again with the same tabs (`/RELAUNCH`, see the installer script).
//!
//! Network access uses Windows' own WinHTTP and hashing uses Windows' BCrypt,
//! so this adds no libraries to the browser.

use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    thread,
};

use serde::Deserialize;
use tao::event_loop::EventLoopProxy;

use crate::ipc::{UpdateEvent, UserEvent};

/// The newest stable release, as GitHub's API describes it.
const LATEST_RELEASE_API: &str = "https://api.github.com/repos/she1bys/rust-browser/releases/latest";
/// Every release, for people running a copy that can't update itself.
pub const RELEASES_PAGE: &str = "https://github.com/she1bys/rust-browser/releases/latest";
/// For testing updates against a local server: a `http://127.0.0.1:<port>/...`
/// URL that answers like [`LATEST_RELEASE_API`]. Only loopback addresses are
/// accepted (see [`Url::parse`]), so it can't point the updater anywhere else.
const FEED_OVERRIDE_ENV: &str = "RUST_BROWSER_UPDATE_FEED";
const CHECKSUMS_ASSET: &str = "SHA256SUMS.txt";
const MAX_RELEASE_JSON: u64 = 2 << 20;
const MAX_CHECKSUMS: u64 = 64 << 10;
const MAX_INSTALLER: u64 = 256 << 20;

/// A release version: `major.minor.patch`, compared numerically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Reads `1.2.3`, `v1.2.3`, `1.2` or `1.2.3-beta` (the suffix is ignored).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches(['v', 'V']);
        let core = text.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let mut next = |required: bool| match parts.next() {
            Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok(),
            None if !required => Some(0),
            _ => None,
        };
        let version = Version(next(true)?, next(false)?, next(false)?);
        parts.next().is_none().then_some(version)
    }

    /// This build's version, from `Cargo.toml`.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version(0, 0, 0))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A release that has an installer this browser can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The release's page on GitHub.
    pub page_url: String,
    installer_name: String,
    installer_url: String,
    /// From the release listing; 0 if unknown.
    installer_size: u64,
    checksums_url: Option<String>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// Reads GitHub's description of a release.
fn parse_release(json: &[u8]) -> Result<Release, String> {
    let api: ApiRelease =
        serde_json::from_slice(json).map_err(|_| "GitHub sent an answer the browser doesn't understand".to_string())?;
    if api.draft || api.prerelease {
        return Err("the newest release isn't a finished one".into());
    }
    let version = Version::parse(&api.tag_name)
        .ok_or_else(|| format!("the newest release has an unknown version ({})", api.tag_name))?;
    let versioned_name = format!("RustBrowser-Setup-{version}.exe");
    let installer = api
        .assets
        .iter()
        .find(|a| a.name == versioned_name)
        .or_else(|| api.assets.iter().find(|a| a.name == "RustBrowser-Setup.exe"))
        .ok_or("the newest release has no installer")?;
    let checksums_url = api.assets.iter().find(|a| a.name == CHECKSUMS_ASSET).map(|a| a.browser_download_url.clone());
    let page_url = if api.html_url.starts_with("https://") { api.html_url } else { RELEASES_PAGE.to_string() };
    Ok(Release {
        version,
        page_url,
        installer_name: installer.name.clone(),
        installer_url: installer.browser_download_url.clone(),
        installer_size: installer.size,
        checksums_url,
    })
}

/// Where release information comes from.
fn release_feed() -> String {
    env::var(FEED_OVERRIDE_ENV)
        .ok()
        .filter(|url| Url::parse(url).is_ok_and(|u| !u.secure))
        .unwrap_or_else(|| LATEST_RELEASE_API.to_string())
}

/// Asks GitHub for the newest release, on a background thread. Answers with
/// [`UpdateEvent::Checked`]: `Some(release)` only if it's newer than this build.
pub fn check(proxy: EventLoopProxy<UserEvent>, manual: bool) {
    thread::spawn(move || {
        let result = (|| {
            let mut json = Vec::new();
            http::get(&release_feed(), &mut json, MAX_RELEASE_JSON, |_, _| {})?;
            let release = parse_release(&json)?;
            Ok((release.version > Version::current()).then_some(release))
        })();
        let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Checked { result, manual }));
    });
}

/// Downloads and verifies the release's installer on a background thread.
/// Reports [`UpdateEvent::Progress`] (whole percents) and finally
/// [`UpdateEvent::Downloaded`] with the installer's path.
pub fn download(release: Release, proxy: EventLoopProxy<UserEvent>) {
    thread::spawn(move || {
        let progress_proxy = proxy.clone();
        let result = download_and_verify(&release, |percent| {
            let _ = progress_proxy.send_event(UserEvent::Update(UpdateEvent::Progress(percent)));
        });
        let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Downloaded(result)));
    });
}

/// `%TEMP%\RustBrowser-Update`: holds the downloaded installer.
fn download_dir() -> PathBuf {
    env::temp_dir().join("RustBrowser-Update")
}

/// Deletes an installer left over from the last update (in the background;
/// it may still be locked for a moment while that installer finishes).
pub fn clean_up_old_download() {
    thread::spawn(|| {
        let dir = download_dir();
        if dir.exists() {
            let _ = fs::remove_dir_all(dir);
        }
    });
}

fn download_and_verify(release: &Release, mut progress: impl FnMut(u8)) -> Result<PathBuf, String> {
    // The checksum list first: it's small, and without it nothing gets run.
    let checksums_url =
        release.checksums_url.as_deref().ok_or("the release has no checksum list to verify the installer with")?;
    let mut checksums = Vec::new();
    http::get(checksums_url, &mut checksums, MAX_CHECKSUMS, |_, _| {})?;
    let expected = expected_sha256(&String::from_utf8_lossy(&checksums), &release.installer_name)
        .ok_or("the installer isn't in the release's checksum list")?;

    let dir = download_dir();
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("couldn't create a folder for the download: {e}"))?;
    let path = dir.join(&release.installer_name);
    let result = (|| {
        let mut file = fs::File::create(&path).map_err(|e| format!("couldn't save the download: {e}"))?;
        let mut shown = None;
        http::get(&release.installer_url, &mut file, MAX_INSTALLER, |received, total| {
            if let Some(total) = total.filter(|&t| t > 0) {
                let percent = (received.saturating_mul(100) / total).min(100) as u8;
                if shown != Some(percent) {
                    shown = Some(percent);
                    progress(percent);
                }
            }
        })?;
        file.flush().map_err(|e| format!("couldn't save the download: {e}"))?;
        drop(file);

        let data = fs::read(&path).map_err(|e| format!("couldn't read the download: {e}"))?;
        if release.installer_size > 0 && data.len() as u64 != release.installer_size {
            return Err("the download is incomplete".to_string());
        }
        if !data.starts_with(b"MZ") {
            return Err("the download isn't a Windows program".to_string());
        }
        if to_hex(&http::sha256(&data)?) != expected {
            return Err("the downloaded installer doesn't match its checksum".to_string());
        }
        Ok(path.clone())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&path);
    }
    result
}

/// The SHA-256 for `file_name` in a `sha256sum`-style list (`<hash>  <name>`).
fn expected_sha256(list: &str, file_name: &str) -> Option<String> {
    list.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let hash = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        let valid = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        (valid && name == file_name).then(|| hash.to_ascii_lowercase())
    })
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// True if this copy was installed with the installer (so the installer can
/// update it), rather than a portable .exe or a build folder.
pub fn can_update_itself() -> bool {
    env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join("unins000.exe").is_file())).unwrap_or(false)
}

/// Starts the downloaded installer: it updates this copy in place (for this
/// user, or for all users if that's how it was installed), shows only a small
/// progress window, and starts the browser again when done. The caller exits
/// straight after, so the files aren't in use.
pub fn run_installer(installer: &Path) -> io::Result<()> {
    let scope = if installed_for_all_users() { "/ALLUSERS" } else { "/CURRENTUSER" };
    Command::new(installer)
        .args([
            "/SILENT",
            "/SUPPRESSMSGBOXES",
            "/NOCANCEL",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
            "/NORESTARTAPPLICATIONS",
            scope,
            "/RELAUNCH",
        ])
        .spawn()
        .map(|_| ())
}

/// Installed under Program Files (an "all users" install, which needs
/// administrator rights to update) rather than in the user's own folder.
fn installed_for_all_users() -> bool {
    let Ok(exe) = env::current_exe() else { return false };
    let exe = exe.to_string_lossy().to_lowercase();
    ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|name| env::var(name).ok())
        .any(|dir| exe.starts_with(&(dir.to_lowercase().trim_end_matches('\\').to_string() + "\\")))
}

/// A URL the updater is willing to fetch.
#[derive(Debug, PartialEq, Eq)]
struct Url {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

impl Url {
    /// Accepts `https://` URLs, and plain `http://` only for this PC itself
    /// (127.0.0.1 or localhost, used for testing).
    fn parse(url: &str) -> Result<Url, String> {
        let bad = || "the update address isn't valid".to_string();
        let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(bad());
        };
        let (authority, path) = match rest.find(['/', '?', '#']) {
            Some(i) if rest.as_bytes()[i] == b'/' => (&rest[..i], &rest[i..]),
            Some(_) => return Err(bad()),
            None => (rest, "/"),
        };
        if authority.contains('@') || authority.contains('[') {
            return Err(bad());
        }
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().map_err(|_| bad())?),
            None => (authority, if secure { 443 } else { 80 }),
        };
        let host_ok = !host.is_empty() && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        if !host_ok || port == 0 {
            return Err(bad());
        }
        if !secure && !(host == "127.0.0.1" || host.eq_ignore_ascii_case("localhost")) {
            return Err("updates are only downloaded over a secure connection".into());
        }
        Ok(Url { secure, host: host.to_string(), port, path: path.to_string() })
    }
}

/// Downloads with WinHTTP (Windows' built-in HTTP client: it uses the system's
/// proxy settings and certificate store) and hashes with BCrypt.
mod http {
    use std::{ffi::c_void, io::Write, ptr};

    use windows_sys::Win32::{
        Networking::WinHttp::{
            WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
            WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH,
            WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
        },
        Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE},
    };

    use super::Url;

    struct Handle(*mut c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: a handle WinHTTP gave us, closed exactly once.
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A readable message for the last WinHTTP error.
    fn last_error(doing: &str) -> String {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let reason = match code {
            12002 => "the connection timed out".to_string(),
            12007 => "you seem to be offline (the server's name couldn't be looked up)".to_string(),
            12029 | 12030 => "couldn't connect to the server".to_string(),
            12175 | 12045 | 12169 | 12038 | 12037 => "the server's security certificate isn't valid".to_string(),
            _ => format!("network error {code}"),
        };
        format!("{doing}: {reason}")
    }

    fn query_number(request: &Handle, what: u32) -> Option<u32> {
        let mut value: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: `value` is a writable u32 and `size` says so; no header name or index is used.
        let ok = unsafe {
            WinHttpQueryHeaders(
                request.0,
                what | WINHTTP_QUERY_FLAG_NUMBER,
                ptr::null(),
                (&mut value as *mut u32).cast(),
                &mut size,
                ptr::null_mut(),
            )
        };
        (ok != 0).then_some(value)
    }

    /// GETs `url` (following redirects) into `out`, refusing anything bigger
    /// than `limit` bytes. `progress(received, total)` runs after each chunk.
    pub fn get(
        url: &str,
        out: &mut dyn Write,
        limit: u64,
        mut progress: impl FnMut(u64, Option<u64>),
    ) -> Result<(), String> {
        let url = Url::parse(url)?;
        let agent = wide(concat!("RustBrowser-Updater/", env!("CARGO_PKG_VERSION")));
        let (host, path, verb) = (wide(&url.host), wide(&url.path), wide("GET"));
        // SAFETY: every pointer passed is either null where the API allows it
        // or a live NUL-terminated UTF-16 buffer / correctly sized out-param;
        // handles are closed by `Handle` in reverse order of creation.
        unsafe {
            let session =
                Handle(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, ptr::null(), ptr::null(), 0));
            if session.0.is_null() {
                return Err(last_error("couldn't start the download"));
            }
            WinHttpSetTimeouts(session.0, 15_000, 15_000, 30_000, 60_000);
            let connection = Handle(WinHttpConnect(session.0, host.as_ptr(), url.port, 0));
            if connection.0.is_null() {
                return Err(last_error("couldn't connect"));
            }
            let flags = if url.secure { WINHTTP_FLAG_SECURE } else { 0 };
            let request = Handle(WinHttpOpenRequest(
                connection.0,
                verb.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                flags,
            ));
            if request.0.is_null() {
                return Err(last_error("couldn't connect"));
            }
            if WinHttpSendRequest(request.0, ptr::null(), 0, ptr::null(), 0, 0, 0) == 0
                || WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0
            {
                return Err(last_error("couldn't reach the update server"));
            }
            match query_number(&request, WINHTTP_QUERY_STATUS_CODE) {
                Some(200) => {}
                // Also what GitHub answers while the project is unavailable to the public.
                Some(404) => return Err("the update page on GitHub isn't available right now. Try again later".into()),
                Some(403 | 429) => return Err("GitHub asked to wait a while before checking again".into()),
                Some(code) => return Err(format!("the update server answered with error {code}")),
                None => return Err(last_error("the update server's answer was unreadable")),
            }
            let total = query_number(&request, WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);
            if total.is_some_and(|t| t > limit) {
                return Err("the download is unexpectedly large".into());
            }

            let mut received: u64 = 0;
            let mut buffer = vec![0u8; 64 * 1024];
            loop {
                let mut available: u32 = 0;
                if WinHttpQueryDataAvailable(request.0, &mut available) == 0 {
                    return Err(last_error("the download stopped"));
                }
                if available == 0 {
                    break;
                }
                let chunk = (available as usize).min(buffer.len());
                let mut read: u32 = 0;
                if WinHttpReadData(request.0, buffer.as_mut_ptr().cast(), chunk as u32, &mut read) == 0 {
                    return Err(last_error("the download stopped"));
                }
                if read == 0 {
                    break;
                }
                received += u64::from(read);
                if received > limit {
                    return Err("the download is unexpectedly large".into());
                }
                out.write_all(&buffer[..read as usize]).map_err(|e| format!("couldn't save the download: {e}"))?;
                progress(received, total);
            }
            if total.is_some_and(|t| t != received) {
                return Err("the download was cut off".into());
            }
        }
        Ok(())
    }

    /// SHA-256 of `data`, computed by Windows.
    pub fn sha256(data: &[u8]) -> Result<[u8; 32], String> {
        let mut hash = [0u8; 32];
        let len = u32::try_from(data.len()).map_err(|_| "the download is too large to check".to_string())?;
        // SAFETY: input and output buffers are valid for the lengths given; no secret (HMAC key) is used.
        let status = unsafe {
            BCryptHash(
                BCRYPT_SHA256_ALG_HANDLE,
                ptr::null(),
                0,
                data.as_ptr(),
                len,
                hash.as_mut_ptr(),
                hash.len() as u32,
            )
        };
        if status < 0 {
            return Err(format!("couldn't check the download (error {status:#x})"));
        }
        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.1"), Some(Version(0, 1, 0)));
        assert_eq!(Version::parse("2"), Some(Version(2, 0, 0)));
        assert_eq!(Version::parse("1.2.3-beta.1"), Some(Version(1, 2, 3)));
        for bad in ["", "v", "1..2", "1.2.3.4", "one", "1.x", "-1.0.0", "1.+2"] {
            assert_eq!(Version::parse(bad), None, "{bad:?}");
        }
        assert!(Version(0, 10, 0) > Version(0, 9, 9), "numeric, not alphabetical");
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    fn release_json(tag: &str, assets: &[&str]) -> String {
        let assets: Vec<String> = assets
            .iter()
            .map(|name| {
                format!(
                    r#"{{"name":"{name}","browser_download_url":"https://github.com/she1bys/rust-browser/releases/download/{tag}/{name}","size":1234}}"#
                )
            })
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/she1bys/rust-browser/releases/tag/{tag}","draft":false,"prerelease":false,"assets":[{}]}}"#,
            assets.join(",")
        )
    }

    #[test]
    fn reads_a_github_release() {
        let json = release_json(
            "v0.3.0",
            &[
                "rust-browser-0.3.0-portable.exe",
                "RustBrowser-Setup-0.3.0.exe",
                "RustBrowser-Setup.exe",
                "SHA256SUMS.txt",
            ],
        );
        let release = parse_release(json.as_bytes()).unwrap();
        assert_eq!(release.version, Version(0, 3, 0));
        assert_eq!(release.installer_name, "RustBrowser-Setup-0.3.0.exe", "prefers the versioned installer");
        assert!(release.installer_url.ends_with("/v0.3.0/RustBrowser-Setup-0.3.0.exe"));
        assert_eq!(release.installer_size, 1234);
        assert!(release.checksums_url.unwrap().ends_with("/SHA256SUMS.txt"));
        assert!(release.page_url.ends_with("/tag/v0.3.0"));

        // Only the fixed-name installer: still fine.
        let release = parse_release(release_json("v0.3.0", &["RustBrowser-Setup.exe"]).as_bytes()).unwrap();
        assert_eq!(release.installer_name, "RustBrowser-Setup.exe");
        assert_eq!(release.checksums_url, None);
    }

    #[test]
    fn rejects_unusable_releases() {
        assert!(parse_release(release_json("v0.3.0", &["rust-browser-0.3.0-portable.exe"]).as_bytes()).is_err());
        assert!(parse_release(release_json("nightly", &["RustBrowser-Setup.exe"]).as_bytes()).is_err());
        let draft = release_json("v0.3.0", &["RustBrowser-Setup.exe"]).replace(r#""draft":false"#, r#""draft":true"#);
        assert!(parse_release(draft.as_bytes()).is_err());
        assert!(parse_release(b"<html>rate limited</html>").is_err());
    }

    #[test]
    fn finds_the_installer_checksum() {
        let list =
            "56826c8f4ed36ec9f8b3278703fff165cd51464b072794e9b54d97ce8444dc6a  rust-browser-0.1.0-portable.exe\r\n\
                    BB1307DB9D8211C70384A147A254873511D68037065FA28E933A68F418BA99E6 *RustBrowser-Setup-0.1.0.exe\r\n\
                    tooshort  RustBrowser-Setup.exe\r\n";
        assert_eq!(
            expected_sha256(list, "RustBrowser-Setup-0.1.0.exe").as_deref(),
            Some("bb1307db9d8211c70384a147a254873511d68037065fa28e933a68f418ba99e6")
        );
        assert_eq!(expected_sha256(list, "RustBrowser-Setup.exe"), None, "malformed hash is ignored");
        assert_eq!(expected_sha256(list, "other.exe"), None);
    }

    #[test]
    fn sha256_matches_known_values() {
        assert_eq!(
            to_hex(&http::sha256(b"").unwrap()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            to_hex(&http::sha256(b"abc").unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn only_secure_or_local_urls_are_fetched() {
        let u = Url::parse("https://api.github.com/repos/she1bys/rust-browser/releases/latest").unwrap();
        assert_eq!((u.secure, u.host.as_str(), u.port), (true, "api.github.com", 443));
        assert_eq!(u.path, "/repos/she1bys/rust-browser/releases/latest");
        assert_eq!(Url::parse("https://example.com").unwrap().path, "/");
        let local = Url::parse("http://127.0.0.1:8765/latest.json").unwrap();
        assert_eq!((local.secure, local.port), (false, 8765));
        for bad in [
            "http://example.com/setup.exe",
            "ftp://example.com/",
            "https://user@example.com/",
            "https://example.com:0/",
            "https://exa mple.com/",
            "https://example.com?x=1",
            "file:///C:/setup.exe",
            "",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn feed_override_must_be_local() {
        // Not set in tests: the real GitHub API.
        if env::var_os(FEED_OVERRIDE_ENV).is_none() {
            assert_eq!(release_feed(), LATEST_RELEASE_API);
        }
    }
}
