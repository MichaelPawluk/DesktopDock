//! Checking GitHub for a newer Desktop Dock, and fetching it, only when asked: Check now and
//! Update now in Dock settings > About, and once a day when "Check for a new version once a
//! day" is ticked. Only GitHub's release pages and files are asked for, with a fixed user agent
//! and no cookies: nothing about the PC or the dock goes with them.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR, w};

/// Where Desktop Dock's releases are.
pub const RELEASES: &str = "https://github.com/MichaelPawluk/DesktopDock/releases";

/// For tests on one PC: a releases page served on this computer (`http://127.0.0.1:<port>`)
/// instead of GitHub's. Nothing else is accepted, so it can't send the dock anywhere else.
const TEST_FEED: &str = "DESKTOP_DOCK_TEST_FEED";

pub type Version = (u16, u16, u16);

pub fn shown((major, minor, patch): Version) -> String {
    format!("{major}.{minor}.{patch}")
}

/// The releases page asked: GitHub's, or a test's own on this computer.
fn feed() -> String {
    std::env::var(TEST_FEED).ok().filter(|feed| local_feed(feed)).map_or_else(|| RELEASES.to_string(), |feed| feed.trim_end_matches('/').to_string())
}

fn local_feed(url: &str) -> bool {
    ["http://127.0.0.1:", "http://localhost:", "http://127.0.0.1/", "http://localhost/"].iter().any(|start| url.to_ascii_lowercase().starts_with(start))
}

/// A release's version from its tag ("v1.2.0" or "1.2.0"); None for anything else, a test
/// release's "v1.2.0-beta" included.
pub fn version_from_tag(tag: &str) -> Option<Version> {
    let mut parts = tag.trim().strip_prefix(['v', 'V']).unwrap_or(tag.trim()).split('.');
    let mut next = || parts.next().filter(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))?.parse::<u16>().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// The tag GitHub's "latest release" link leads to, from where it sends you
/// (".../releases/tag/v1.2.0").
pub fn tag_from_location(location: &str) -> Option<&str> {
    let after = &location[location.find("/releases/tag/")? + "/releases/tag/".len()..];
    let tag = after.split(['?', '#', '/']).next()?;
    (!tag.is_empty()).then_some(tag)
}

/// The SHA-256 a ".sha256" file gives: its first word, 64 hexadecimal digits.
pub fn sha256_from_file(text: &str) -> Option<[u8; 32]> {
    let word = text.trim_start_matches('\u{feff}').split_whitespace().next()?;
    if word.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(word.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

/// SHA-256, from Windows' own cryptography.
pub fn sha256(data: &[u8]) -> Option<[u8; 32]> {
    use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    (unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) }.0 >= 0).then_some(out)
}

/// What Windows said back.
struct Reply {
    status: u32,
    location: Option<String>,
    body: Vec<u8>,
}

// In English here; said with `text::t`, in the language in use.
const UNREACHABLE: &str = n!("Couldn't reach GitHub just now. Check your connection, or try again later.");
const CUT_OFF: &str = n!("The connection to GitHub was cut off. Try again later.");
const BUSY: &str = n!("GitHub didn't answer as it should just now. Try again later.");

/// One of the texts above, as an error.
fn said(english: &'static str) -> String {
    crate::text::t(english).to_string()
}

/// Asks for one address: GitHub (https) or a test's own page on this computer (http). Redirects
/// are followed only when `follow`, and from a test's page only to another of its pages; at most
/// `limit` bytes are read.
fn get(url: &str, follow: bool, limit: usize) -> Result<Reply, String> {
    let mut reply = get_once(url, follow, limit)?;
    // From a test's page, Windows doesn't follow redirects (it would follow them anywhere): they're
    // followed here, a few at most, only to another page on this computer.
    for _ in 0..3 {
        match reply.location.clone().filter(|_| follow && reply.status / 100 == 3 && local_feed(url)) {
            Some(next) if local_feed(&next) => reply = get_once(&next, false, limit)?,
            _ => break,
        }
    }
    Ok(reply)
}

fn get_once(url: &str, follow: bool, limit: usize) -> Result<Reply, String> {
    let (secure, rest) = match url.split_once("://") {
        Some(("https", rest)) => (true, rest),
        Some(("http", rest)) if local_feed(url) => (false, rest),
        _ => return Err(tr!("Not an address the dock asks: {address}", address = url)),
    };
    let (host_port, path) = rest.find('/').map_or((rest, "/"), |at| (&rest[..at], &rest[at..]));
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().map_err(|_| tr!("Not an address the dock asks: {address}", address = url))?),
        None => (host_port, if secure { INTERNET_DEFAULT_HTTPS_PORT as u16 } else { INTERNET_DEFAULT_HTTP_PORT as u16 }),
    };
    unsafe {
        let session = WinHttpOpen(w!("DesktopDock-update-check"), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return Err(said(UNREACHABLE));
        }
        let protocols = (WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3).to_le_bytes();
        let _ = WinHttpSetOption(Some(session as *const c_void), WINHTTP_OPTION_SECURE_PROTOCOLS, Some(&protocols));
        let _ = WinHttpSetTimeouts(session, 10_000, 10_000, 15_000, 30_000);
        let connect = WinHttpConnect(session, &HSTRING::from(host), port.into(), 0);
        let request = if connect.is_null() {
            std::ptr::null_mut()
        } else {
            WinHttpOpenRequest(connect, w!("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) })
        };
        let close = || {
            for handle in [request, connect, session] {
                if !handle.is_null() {
                    let _ = WinHttpCloseHandle(handle);
                }
            }
        };
        if request.is_null() {
            close();
            return Err(said(UNREACHABLE));
        }
        let _ = WinHttpSetOption(Some(request as *const c_void), WINHTTP_OPTION_DISABLE_FEATURE, Some(&WINHTTP_DISABLE_COOKIES.to_le_bytes()));
        if !follow || !secure {
            let _ = WinHttpSetOption(Some(request as *const c_void), WINHTTP_OPTION_REDIRECT_POLICY, Some(&WINHTTP_OPTION_REDIRECT_POLICY_NEVER.to_le_bytes()));
        }
        if WinHttpSendRequest(request, None, None, 0, 0, 0).is_err() || WinHttpReceiveResponse(request, std::ptr::null_mut()).is_err() {
            close();
            return Err(said(UNREACHABLE));
        }
        let mut status = 0u32;
        let mut len = 4u32;
        let _ = WinHttpQueryHeaders(request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut status as *mut u32 as *mut c_void), &mut len, std::ptr::null_mut());
        let location = {
            let mut len = 0u32;
            let _ = WinHttpQueryHeaders(request, WINHTTP_QUERY_LOCATION, PCWSTR::null(), None, &mut len, std::ptr::null_mut());
            let mut text = vec![0u16; (len / 2 + 1) as usize];
            (len > 0 && WinHttpQueryHeaders(request, WINHTTP_QUERY_LOCATION, PCWSTR::null(), Some(text.as_mut_ptr() as *mut c_void), &mut len, std::ptr::null_mut()).is_ok())
                .then(|| String::from_utf16_lossy(&text[..(len / 2) as usize]))
        };
        // How long the file says it is, when it says.
        let expected = {
            let mut length = 0u32;
            let mut len = 4u32;
            WinHttpQueryHeaders(request, WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut length as *mut u32 as *mut c_void), &mut len, std::ptr::null_mut())
                .is_ok()
                .then_some(length as usize)
        };
        // Only a found file is read: anything else (GitHub's "not found" page, a redirect) is
        // answered by its status alone.
        let mut body = Vec::new();
        let (mut too_big, mut cut) = (false, false);
        while status / 100 == 2 {
            let mut ready = 0u32;
            if WinHttpQueryDataAvailable(request, &mut ready).is_err() {
                cut = true;
                break;
            }
            if ready == 0 {
                break;
            }
            if body.len() + ready as usize > limit {
                too_big = true;
                break;
            }
            let mut chunk = vec![0u8; ready as usize];
            let mut read = 0u32;
            if WinHttpReadData(request, chunk.as_mut_ptr() as *mut c_void, ready, &mut read).is_err() || read == 0 {
                cut = true;
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
        close();
        if too_big {
            return Err(tr!("GitHub sent more than a release's file should be, so it wasn't used.").into());
        }
        if cut || expected.is_some_and(|length| status / 100 == 2 && body.len() != length) {
            return Err(said(CUT_OFF));
        }
        Ok(Reply { status, location, body })
    }
}

/// Asks GitHub which release is the newest: its version.
pub fn latest() -> Result<Version, String> {
    let mut reply = get(&format!("{}/latest", feed()), false, 256 * 1024)?;
    // A repository that has been renamed or moved: GitHub first sends you to its new address.
    if let Some(moved) = reply.location.clone().filter(|location| reply.status / 100 == 3 && moved_latest(location)) {
        reply = get(&moved, false, 256 * 1024)?;
    }
    let tag = reply.location.as_deref().filter(|_| reply.status / 100 == 3).and_then(tag_from_location);
    tag.and_then(version_from_tag).ok_or_else(|| tr!("GitHub didn't say which version is the newest. Try again later.").to_string())
}

/// Where GitHub sends "latest release" for a repository that has a new name or owner: the same
/// page at the new address (on GitHub, or a test's own page on this computer).
fn moved_latest(location: &str) -> bool {
    location.trim_end_matches('/').ends_with("/releases/latest") && (location.starts_with("https://github.com/") || local_feed(location))
}

/// Downloads a release's desktop-dock.exe into `dir`, and keeps it only if it's what that release
/// says it is: its SHA-256 matches the one published beside it, and it's that version.
pub fn download(version: Version, dir: &Path) -> Result<PathBuf, String> {
    let base = format!("{}/download/v{}", feed(), shown(version));
    // A release without the files Update now needs: it's downloaded by hand instead.
    let by_hand = tr!("Version {version} can't be updated to from here: download it from GitHub instead (What's new).", version = shown(version));
    let sum = get(&format!("{base}/desktop-dock.exe.sha256"), true, 4096)?;
    match sum.status {
        200 => {}
        404 => return Err(by_hand),
        _ => return Err(said(BUSY)),
    }
    let want = sha256_from_file(&String::from_utf8_lossy(&sum.body)).ok_or(tr!("The release's checksum couldn't be read, so nothing was downloaded."))?;
    let exe = get(&format!("{base}/desktop-dock.exe"), true, 64 * 1024 * 1024)?;
    match exe.status {
        200 if !exe.body.is_empty() => {}
        404 => return Err(by_hand),
        _ => return Err(said(BUSY)),
    }
    if sha256(&exe.body) != Some(want) {
        return Err(tr!("The download didn't match its checksum, so it wasn't used.").into());
    }
    std::fs::create_dir_all(dir).map_err(|e| tr!("Couldn't save the download: {why}", why = crate::system::plain(&e.to_string())))?;
    let path = dir.join(format!("desktop-dock-{}.exe", shown(version)));
    crate::store::atomic_write_bytes(&path, &exe.body).map_err(|e| tr!("Couldn't save the download: {why}", why = crate::system::plain(&e.to_string())))?;
    if crate::setup::version_of(&path) != Some(version) {
        let _ = std::fs::remove_file(&path);
        return Err(tr!("The download isn't the version it should be, so it wasn't used.").into());
    }
    Ok(path)
}

/// What the last check found, kept in the dock folder (`updates\status.toml`) for Dock
/// settings and the dock.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct Status {
    /// When it was checked, as the PC's clock said ("2026-11-10 17:02").
    pub checked: String,
    /// The newest version GitHub has, if the check got that far.
    pub latest: String,
    /// What went wrong, in plain words, if it did.
    pub error: String,
    /// The newest version the dock has said is out (it says so once).
    pub told: String,
}

impl Status {
    pub fn latest_version(&self) -> Option<Version> {
        version_from_tag(&self.latest)
    }

    pub fn path(home: &Path) -> PathBuf {
        home.join("updates").join("status.toml")
    }

    pub fn load(home: &Path) -> Status {
        std::fs::read_to_string(Status::path(home)).ok().and_then(|text| toml::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self, home: &Path) {
        let quote = |text: &str| toml_edit::Value::from(text).to_string();
        let text = format!(
            "# What Desktop Dock's last check for a new version found (Dock settings > About).\nchecked = {}\nlatest = {}\nerror = {}\ntold = {}\n",
            quote(&self.checked),
            quote(&self.latest),
            quote(&self.error),
            quote(&self.told)
        );
        let path = Status::path(home);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = crate::store::atomic_write_bytes(&path, text.as_bytes());
    }
}

/// Checks now and keeps what it found (beside what the dock already said). The dock runs this in
/// a helper (`--check-updates`) so its own memory never holds Windows' web code.
pub fn check_and_keep(home: &Path) -> Status {
    let mut status = Status::load(home);
    status.checked = crate::log::now_text();
    match latest() {
        Ok(version) => {
            status.latest = shown(version);
            status.error.clear();
        }
        Err(e) => status.error = e,
    }
    status.save(home);
    status
}

/// Downloads a new version that was found, and starts installing it: it closes the dock and
/// Dock settings, replaces the program, keeps your dock and Start with Windows as they are, and
/// starts the new one. Only for an installed copy.
pub fn install(version: Version, home: &Path) -> Result<(), String> {
    if !is_newer(version, this_version()) {
        return Err(tr!("Desktop Dock {version} isn't newer than this one ({this}), so nothing was installed.", version = shown(version), this = shown(this_version())));
    }
    let exe = download(version, &home.join("updates"))?;
    std::process::Command::new(&exe)
        .args(["--install", "--quiet", "--no-startup"])
        .current_dir(exe.parent().unwrap_or(home))
        .spawn()
        .map_err(|e| tr!("The new version didn't start: {why}", why = crate::system::plain(&e.to_string())))?;
    Ok(())
}

/// Newer than this copy.
pub fn is_newer(version: Version, than: Version) -> bool {
    version > than
}

/// This copy's version, as Cargo.toml says.
pub fn this_version() -> Version {
    let number = |s: &str| s.parse().unwrap_or(0);
    (number(env!("CARGO_PKG_VERSION_MAJOR")), number(env!("CARGO_PKG_VERSION_MINOR")), number(env!("CARGO_PKG_VERSION_PATCH")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_tag_gives_its_version_and_nothing_else_does() {
        assert_eq!(version_from_tag("v1.2.0"), Some((1, 2, 0)));
        assert_eq!(version_from_tag("1.10.3"), Some((1, 10, 3)));
        assert_eq!(version_from_tag(" V2.0.11 "), Some((2, 0, 11)));
        for not in ["", "v1.2", "v1.2.0-beta", "v1.2.0.4", "vx.y.z", "latest", "v1..0", "v70000.0.0"] {
            assert_eq!(version_from_tag(not), None, "{not}");
        }
    }

    #[test]
    fn where_the_latest_link_leads_says_the_tag() {
        assert_eq!(tag_from_location("https://github.com/MichaelPawluk/DesktopDock/releases/tag/v1.2.0"), Some("v1.2.0"));
        assert_eq!(tag_from_location("/MichaelPawluk/DesktopDock/releases/tag/v1.2.0?x=1"), Some("v1.2.0"));
        assert_eq!(tag_from_location("http://127.0.0.1:8765/releases/tag/v99.0.0"), Some("v99.0.0"));
        assert_eq!(tag_from_location("https://github.com/MichaelPawluk/DesktopDock/releases"), None, "no release yet: the releases page");
        assert_eq!(tag_from_location("https://github.com/login"), None);
    }

    #[test]
    fn a_checksum_file_gives_its_sha256() {
        let hex = "c49c51a899a3f2570c61f073c139b3880d03f53eebc6bd09dfd7ee870ff5bcad";
        let sum = sha256_from_file(&format!("{}  desktop-dock.exe\r\n", hex.to_uppercase())).unwrap();
        assert_eq!((sum[0], sum[31]), (0xc4, 0xad));
        assert_eq!(sha256_from_file(&format!("\u{feff}{hex}")), Some(sum), "with a byte-order mark");
        assert_eq!(sha256_from_file("not a checksum"), None);
        assert_eq!(sha256_from_file(&hex[..60]), None);
        assert_eq!(sha256_from_file(&format!("{}zz", &hex[..62])), None);
    }

    #[test]
    fn sha256_is_sha256() {
        // The standard test vector for "abc".
        let abc = sha256(b"abc").unwrap();
        assert_eq!(abc.iter().map(|b| format!("{b:02x}")).collect::<String>(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn only_github_or_a_page_on_this_computer_is_asked() {
        for local in ["http://127.0.0.1:8765", "http://localhost:9000/releases", "HTTP://127.0.0.1/"] {
            assert!(local_feed(local), "{local}");
        }
        for not in ["http://192.168.1.5:8765", "https://127.0.0.1:8765", "http://127.0.0.1.example.com/", "http://localhost.example.com:80", "file:///C:/x"] {
            assert!(!local_feed(not), "{not}");
        }
        assert!(get("http://example.com/x", true, 10).is_err_and(|e| e.starts_with("Not an address")), "plain http elsewhere is refused before anything is sent");
    }

    #[test]
    fn what_was_found_is_kept_and_read_back() {
        let home = std::env::temp_dir().join(format!("dd-update-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(Status::load(&home), Status::default(), "never checked");
        let status = Status { checked: "2026-11-10 17:02".into(), latest: "1.2.0".into(), error: String::new(), told: "1.1.9".into() };
        status.save(&home);
        assert_eq!(Status::load(&home), status);
        assert_eq!(Status::load(&home).latest_version(), Some((1, 2, 0)));
        let odd = Status { error: "Couldn't \"reach\" GitHub\nat all".into(), ..Default::default() };
        odd.save(&home);
        assert_eq!(Status::load(&home), odd, "quotes and line breaks survive");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn newer_means_newer() {
        assert!(is_newer((1, 2, 0), (1, 1, 9)) && is_newer((2, 0, 0), (1, 99, 99)) && is_newer((1, 1, 10), (1, 1, 9)));
        assert!(!is_newer((1, 1, 0), (1, 1, 0)) && !is_newer((1, 0, 9), (1, 1, 0)));
    }

    /// A web server on this computer for one test: answers each request with what `answer` gives
    /// for its path (the whole raw reply), for `count` requests. Its address.
    fn serve(count: usize, answer: impl Fn(&str, u16) -> Vec<u8> + Send + 'static) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(count).flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                let _ = reader.read_line(&mut first);
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let path = first.split(' ').nth(1).unwrap_or("/").to_string();
                let mut stream = stream;
                let _ = stream.write_all(&answer(&path, port));
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn a_download_cut_off_part_way_says_so_rather_than_blaming_the_checksum() {
        let url = serve(1, |_, _| b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nonly part".to_vec());
        assert_eq!(get(&format!("{url}/file"), true, 4096).err().as_deref(), Some(CUT_OFF));
    }

    #[test]
    fn githubs_not_found_page_is_answered_by_its_status_not_read() {
        let page = format!("<html>{}</html>", "<p>Not found.</p>".repeat(3000));
        let url = serve(1, move |_, _| format!("HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).into_bytes());
        let reply = get(&format!("{url}/missing.sha256"), true, 4096).expect("an answer, not 'too big'");
        assert_eq!((reply.status, reply.body.len()), (404, 0));
    }

    #[test]
    fn a_test_page_is_followed_only_to_another_page_on_this_computer() {
        let url = serve(3, |path, port| match path {
            "/elsewhere" => b"HTTP/1.1 302 Found\r\nLocation: http://example.com/evil.exe\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            "/here" => format!("HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{port}/files/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes(),
            _ => b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc".to_vec(),
        });
        let away = get(&format!("{url}/elsewhere"), true, 4096).unwrap();
        assert_eq!(away.status, 302, "not followed off this computer");
        let here = get(&format!("{url}/here"), true, 4096).unwrap();
        assert_eq!((here.status, here.body.as_slice()), (200, b"abc".as_slice()));
    }

    #[test]
    fn a_moved_repository_is_followed_to_its_latest_release_only_on_github() {
        assert!(moved_latest("https://github.com/someone/NewName/releases/latest"));
        assert!(moved_latest("http://127.0.0.1:8765/moved/releases/latest"));
        assert!(!moved_latest("https://github.com/someone/NewName/releases/tag/v1.2.0"), "that's the answer itself");
        assert!(!moved_latest("https://example.com/someone/releases/latest"));
        assert!(!moved_latest("https://github.com.example.com/x/releases/latest"));
    }

    #[test]
    fn update_now_never_installs_the_same_or_an_older_version() {
        let dir = std::env::temp_dir().join(format!("dock-update-older-{}", std::process::id()));
        for version in [this_version(), (0, 9, 0)] {
            let said = install(version, &dir).unwrap_err();
            assert!(said.contains("isn't newer"), "{said}");
        }
        assert!(!dir.join("updates").exists(), "nothing downloaded");
    }

    /// GitHub itself, the way an update reaches it: the newest release's version, and a published
    /// release's file (sent from GitHub's file servers, a redirect away) matching the SHA-256
    /// published beside it, as Update now checks the new version. It goes online, so it runs only
    /// when asked: cargo test -- --ignored.
    #[test]
    #[ignore]
    fn github_itself_answers_the_way_an_update_expects() {
        let newest = latest().expect("GitHub's newest release");
        assert!(newest >= (1, 0, 1), "{newest:?}");
        let base = format!("{RELEASES}/download/v1.0.1/DesktopDock-1.0.1.zip");
        let sum = get(&format!("{base}.sha256"), true, 4096).expect("the checksum file");
        assert_eq!(sum.status, 200);
        let want = sha256_from_file(&String::from_utf8_lossy(&sum.body)).expect("a SHA-256 in it");
        let file = get(&base, true, 64 * 1024 * 1024).expect("the file");
        assert_eq!(file.status, 200);
        assert_eq!(sha256(&file.body), Some(want), "the file matches its published checksum ({} bytes)", file.body.len());
        // A release without the file Update now needs (as before 1.1): GitHub says it isn't there.
        let missing = get(&format!("{RELEASES}/download/v1.0.1/desktop-dock.exe.sha256"), true, 4096).expect("an answer");
        assert_eq!(missing.status, 404);
    }
}
