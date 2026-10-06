//! Installing and removing Desktop Dock: one exe that installs itself.
//!
//! Run from anywhere but its install folder, with no arguments, it shows a small setup window:
//! where it goes (`%LOCALAPPDATA%\Programs\Desktop Dock`, or a folder of your own you choose;
//! an update goes where it's installed, as its Apps & features entry records), Start with
//! Windows, and Install (or Update). That copies it there, adds a Start menu shortcut and an Apps
//! & features entry, and starts it, with a progress bar through the steps and then a page
//! saying where the dock is (the dock comes out to show itself on its very first start). The
//! Apps & features entry runs `desktop-dock.exe --uninstall`, which takes it all away again,
//! keeping your dock (dock.toml, its backups, Recently removed) unless you ask otherwise.
//! `--install` / `--uninstall` with `--quiet` do the same with no window (for scripts) and
//! report through their exit code and `setup.log`.
//!
//! Nothing here needs administrator rights: everything is in your own user folders and
//! registry. Your dock.toml is never touched by an install.

use crate::{chrome, pickers, places, store, system};
use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{ERROR_SUCCESS, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VS_FIXEDFILEINFO, VerQueryValueW};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
    RegSetValueExW,
};
use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CheckDlgButton, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx,
    IsDlgButtonChecked, PBM_SETPOS, PBM_SETRANGE32,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::Shell::{FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{
    DialogBoxParamW, EndDialog, EnumWindows, GetClientRect, GetDlgItem, GetPropW, GetWindowRect, IDCANCEL, IDOK, KillTimer,
    MB_ICONINFORMATION, MB_OK, MapDialogRect, PostMessageW, SW_HIDE, SW_SHOW, STM_SETICON, SWP_NOMOVE, SWP_NOZORDER,
    SendMessageW, SetDlgItemTextW, SetTimer, SetWindowPos, ShowWindow, WM_APP, WM_CLOSE, WM_COMMAND, WM_INITDIALOG, WM_TIMER,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, w};

const IDD_SETUP: usize = 110;
const IDD_UNINSTALL: usize = 111;
const IDC_ICON: i32 = 2001;
const IDC_HEADING: i32 = 2002;
const IDC_TEXT: i32 = 2003;
/// Setup: Start with Windows. Uninstall: also remove my dock.
const IDC_CHOICE: i32 = 2004;
const IDC_STATUS: i32 = 2005;
/// Setup: where it goes, Change..., and a word about that place.
const IDC_WHERE: i32 = 2007;
const IDC_CHANGE: i32 = 2008;
const IDC_WHERE_NOTE: i32 = 2009;
const IDC_WHERE_LABEL: i32 = 2006;
const IDC_PROGRESS: i32 = 2010;
/// From the install, running on its own thread: a step begins (wparam: its place in `Step::ALL`).
const WM_APP_STEP: u32 = WM_APP + 1;
/// The install is over (its result in `FINISHED`).
const WM_APP_FINISHED: u32 = WM_APP + 2;
/// The progress bar moving toward where the install is.
const TIMER_BAR: usize = 1;
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\DesktopDock";
/// Tests only: install under this folder instead (and a test Apps & features entry), so a test
/// can install, update and uninstall without touching the real install.
const TEST_ROOT: &str = "DESKTOP_DOCK_TEST_ROOT";

/// What this run of the program is for, decided before anything else happens (no log, no
/// window), so running a download never leaves files beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The setup window: a copy run from outside its install folder.
    Setup,
    Install(Options),
    Uninstall { quiet: bool, remove_dock: bool },
    /// Everything else: the dock itself (installed, portable or a development build), the
    /// helpers and windows (their own flags).
    Run,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub quiet: bool,
    /// Start the installed dock afterwards.
    pub start: bool,
    /// Start with Windows: on or off; None leaves it as it is.
    pub startup: Option<bool>,
}

/// Decides the role from the arguments and where this copy is. Pure, so it's tested.
pub fn role(args: &[String], exe: &Path, install_dir: Option<&Path>, dock_toml_beside: bool, dev_build: bool) -> Role {
    let has = |flag: &str| args.iter().any(|arg| arg.eq_ignore_ascii_case(flag));
    if has("--install") {
        let startup = if has("--no-startup") { None } else { Some(true) };
        return Role::Install(Options { quiet: has("--quiet"), start: !has("--no-start"), startup });
    }
    if has("--uninstall") {
        return Role::Uninstall { quiet: has("--quiet"), remove_dock: has("--remove-dock") };
    }
    if !args.is_empty() || dev_build || dock_toml_beside {
        return Role::Run;
    }
    let installed_here = install_dir.is_some_and(|dir| exe.parent().is_some_and(|here| same_path(here, dir)));
    if installed_here { Role::Run } else { Role::Setup }
}

/// Two paths name the same file or folder (case, separators and a trailing slash don't matter;
/// so does an 8.3 short name or `\\?\`, when the path exists).
pub fn same_path(a: &Path, b: &Path) -> bool {
    let plain = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let text = p.to_string_lossy().replace('/', "\\");
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text).trim_end_matches('\\').to_lowercase();
        text
    };
    plain(a) == plain(b)
}

fn test_root() -> Option<PathBuf> {
    std::env::var_os(TEST_ROOT).filter(|root| !root.is_empty()).map(PathBuf::from)
}

/// Where Desktop Dock is installed: where its Installed apps entry says (you may have chosen the
/// folder); or else where an uninstall kept your dock, so installing again brings it back; or
/// else where it goes unless you choose another.
pub fn install_dir() -> Option<PathBuf> {
    choose_install_dir(recorded_install_dir(), kept_dock_dir(), default_install_dir())
}

fn choose_install_dir(recorded: Option<PathBuf>, kept: Option<PathBuf>, default: Option<PathBuf>) -> Option<PathBuf> {
    recorded.or(kept).or(default)
}

/// Where an uninstall left your dock (the Installed apps entry, which said so, goes with it):
/// its own key, outside that entry, removed with "Also remove my dock" or the next install.
const KEPT_KEY: &str = r"Software\Desktop Dock";
const KEPT_VALUE: PCWSTR = w!("KeptDock");

fn kept_key() -> String {
    if test_root().is_some() { format!("{KEPT_KEY}.Test") } else { KEPT_KEY.to_string() }
}

/// The folder an uninstall kept your dock in, if it's still there.
fn kept_dock_dir() -> Option<PathBuf> {
    let text = system::read_user_text(&kept_key(), "KeptDock")?;
    Some(PathBuf::from(text.trim())).filter(|dir| !dir.as_os_str().is_empty() && dir.is_dir())
}

/// Remembers (Some) or forgets (None) where an uninstall kept your dock.
fn remember_kept_dock(dir: Option<&Path>) {
    unsafe {
        let path = HSTRING::from(kept_key());
        match dir {
            Some(dir) => {
                let mut key = HKEY::default();
                if RegCreateKeyExW(HKEY_CURRENT_USER, &path, None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None) == ERROR_SUCCESS {
                    let wide: Vec<u16> = dir.display().to_string().encode_utf16().chain(std::iter::once(0)).collect();
                    let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
                    let _ = RegSetValueExW(key, KEPT_VALUE, None, REG_SZ, Some(bytes));
                    let _ = RegCloseKey(key);
                }
            }
            None => {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &path);
            }
        }
    }
}

/// `%LOCALAPPDATA%\Programs\Desktop Dock`: in your own user folder, no administrator rights needed.
pub fn default_install_dir() -> Option<PathBuf> {
    let base = match test_root() {
        Some(root) => root,
        None => PathBuf::from(std::env::var_os("LOCALAPPDATA")?),
    };
    Some(base.join("Programs").join("Desktop Dock"))
}

/// The installed folder, as the Installed apps entry records it (if the program is there).
fn recorded_install_dir() -> Option<PathBuf> {
    let text = system::read_user_text(&uninstall_key(), "InstallLocation")?;
    Some(PathBuf::from(text.trim())).filter(|dir| !dir.as_os_str().is_empty() && dir.join("desktop-dock.exe").is_file())
}

/// The install folder for a folder you picked: a "Desktop Dock" folder in it (unless it is one
/// already). What to say instead when it can't go there: on another computer, or somewhere
/// only administrators may write (Program Files, say), which Desktop Dock never asks for.
fn install_dir_in(picked: &Path) -> Result<PathBuf, String> {
    if places::on_network(&picked.to_string_lossy()) {
        return Err("That folder is on another computer. Choose one on this PC, so your dock works without the network.".into());
    }
    let dir = if picked.file_name().is_some_and(|name| name.eq_ignore_ascii_case("Desktop Dock")) {
        picked.to_path_buf()
    } else {
        picked.join("Desktop Dock")
    };
    // Tried for real (made and taken away again), as the install will.
    let made = !dir.is_dir();
    let tried = std::fs::create_dir_all(&dir).and_then(|()| {
        let probe = dir.join(format!(".desktop-dock-setup-{}.tmp", std::process::id()));
        std::fs::write(&probe, b"").and_then(|()| std::fs::remove_file(&probe))
    });
    if made {
        let _ = std::fs::remove_dir(&dir); // only if still empty
    }
    match tried {
        Ok(()) => Ok(dir),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(
            "Only administrators can install there, and Desktop Dock installs without asking for that. Choose a folder of your own, like the one it suggests."
                .into(),
        ),
        Err(e) => Err(format!("That folder can't be used: {}", system::plain(&e.to_string()))),
    }
}

fn shortcut_path() -> Option<PathBuf> {
    let folder = match test_root() {
        Some(root) => root.join("Start Menu"),
        None => known_folder(&FOLDERID_Programs)?,
    };
    Some(folder.join("Desktop Dock.lnk"))
}

fn uninstall_key() -> String {
    if test_root().is_some() { format!("{UNINSTALL_KEY}.Test") } else { UNINSTALL_KEY.to_string() }
}

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const c_void));
        text.map(PathBuf::from)
    }
}

/// An exe's version (major, minor, patch), from its version resource.
pub fn version_of(exe: &Path) -> Option<(u16, u16, u16)> {
    unsafe {
        let name = HSTRING::from(exe);
        let size = GetFileVersionInfoSizeW(&name, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&name, None, size, data.as_mut_ptr() as *mut c_void).ok()?;
        let mut info: *mut c_void = std::ptr::null_mut();
        let mut len = 0u32;
        if !VerQueryValueW(data.as_ptr() as *const c_void, w!("\\"), &mut info, &mut len).as_bool() || info.is_null() {
            return None;
        }
        let fixed = &*(info as *const VS_FIXEDFILEINFO);
        Some(((fixed.dwFileVersionMS >> 16) as u16, (fixed.dwFileVersionMS & 0xFFFF) as u16, (fixed.dwFileVersionLS >> 16) as u16))
    }
}

/// This copy's version, as Cargo.toml says.
fn my_version() -> (u16, u16, u16) {
    let number = |s: &str| s.parse().unwrap_or(0);
    (number(env!("CARGO_PKG_VERSION_MAJOR")), number(env!("CARGO_PKG_VERSION_MINOR")), number(env!("CARGO_PKG_VERSION_PATCH")))
}

fn shown((major, minor, patch): (u16, u16, u16)) -> String {
    format!("{major}.{minor}.{patch}")
}

/// A line in `setup.log` in the install folder `dir` (the dock's own log isn't open during setup).
fn log(dir: &Path, line: &str) {
    let _ = std::fs::create_dir_all(dir);
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let stamp = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond);
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("setup.log")) {
        let _ = writeln!(file, "{stamp} {line}");
    }
}

/// Waits until no Desktop Dock holds its single-instance lock (the watchdog has gone too).
fn wait_until_free(limit: Duration) -> bool {
    let started = Instant::now();
    loop {
        let held = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, w!("Local\\DesktopDock.SingleInstance")) };
        match held {
            Ok(handle) => unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            },
            Err(_) => return true,
        }
        if started.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Closes any open Dock settings or Properties windows (they're marked with a window property).
fn close_settings_windows() {
    unsafe extern "system" fn each(hwnd: HWND, _: LPARAM) -> BOOL {
        unsafe {
            if !GetPropW(hwnd, w!("DesktopDock.Window")).is_invalid() {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(0));
    }
}

/// Today's date and time, for file names.
fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// The steps of an install, as the setup window shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Only when a dock is running (an update).
    Close,
    Copy,
    StartMenu,
    InstalledApps,
    Startup,
    Start,
}

impl Step {
    const ALL: [Step; 6] = [Step::Close, Step::Copy, Step::StartMenu, Step::InstalledApps, Step::Startup, Step::Start];

    /// What the setup window says during it.
    fn words(self) -> &'static str {
        match self {
            Step::Close => "Closing the dock that's running...",
            Step::Copy => "Copying Desktop Dock...",
            Step::StartMenu => "Adding it to the Start menu...",
            Step::InstalledApps => "Adding it to Installed apps...",
            Step::Startup => "Setting Start with Windows...",
            Step::Start => "Starting your dock...",
        }
    }

    /// How far along the progress bar is once it's under way, in percent.
    fn percent(self) -> u32 {
        match self {
            Step::Close => 5,
            Step::Copy => 25,
            Step::StartMenu => 45,
            Step::InstalledApps => 65,
            Step::Startup => 80,
            Step::Start => 92,
        }
    }
}

/// Installs (or updates) this copy in `dir`, saying each step to `progress` as it begins. Your
/// dock.toml is never touched.
pub fn install(options: Options, dir: &Path, progress: &dyn Fn(Step)) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {}", dir.display(), system::plain(&e.to_string())))?;
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = dir.join("desktop-dock.exe");
    log(dir, &format!("installing {} from {}", shown(my_version()), me.display()));
    if !same_path(&me, &exe) {
        // Only one dock runs at a time: the one running now steps aside (in a test, nothing is
        // touched outside the test folder).
        if test_root().is_none() {
            close_settings_windows();
            if system::dock_is_running() {
                progress(Step::Close);
            }
            if !system::quit_running_dock() {
                return Err("The running dock didn't close. Right-click it > Exit Desktop Dock, then try again.".into());
            }
            if !wait_until_free(Duration::from_secs(10)) {
                log(dir, "the old dock was still running after 10 s; carrying on");
            }
        }
        progress(Step::Copy);
        // The old exe is renamed aside (allowed while it runs) and removed next time; the new one
        // is written as fresh bytes, so it doesn't inherit a download's "from the internet" mark.
        if exe.exists() {
            let aside = dir.join(format!("desktop-dock.old-{}.exe", stamp()));
            std::fs::rename(&exe, &aside).map_err(|e| format!("Couldn't move the old version aside: {}", system::plain(&e.to_string())))?;
        }
        let bytes = std::fs::read(&me).map_err(|e| format!("Couldn't read {}: {}", me.display(), system::plain(&e.to_string())))?;
        store::atomic_write_bytes(&exe, &bytes).map_err(|e| format!("Couldn't write {}: {}", exe.display(), system::plain(&e.to_string())))?;
        log(dir, &format!("copied to {}", exe.display()));
    }
    progress(Step::StartMenu);
    match create_shortcut(&exe, dir) {
        Ok(path) => log(dir, &format!("Start menu shortcut: {}", path.display())),
        Err(e) => log(dir, &format!("no Start menu shortcut: {e}")),
    }
    progress(Step::InstalledApps);
    write_uninstall_entry(&exe, dir, bytes_of(&exe))?;
    log(dir, "Apps & features entry written");
    // That entry says where it is now; a dock an uninstall kept is either here again or not wanted.
    remember_kept_dock(None);
    if let Some(on) = options.startup.filter(|_| test_root().is_none()) {
        progress(Step::Startup);
        match system::set_starts_with_windows(on) {
            Ok(()) => log(dir, &format!("start with Windows: {}", if on { "on" } else { "off" })),
            Err(e) => log(dir, &format!("start with Windows unchanged: {e}")),
        }
    }
    if options.start {
        progress(Step::Start);
        std::process::Command::new(&exe).current_dir(dir).spawn().map_err(|e| format!("Installed, but it didn't start: {}", system::plain(&e.to_string())))?;
        log(dir, "started");
    }
    Ok(())
}

fn bytes_of(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// The Start menu shortcut (Programs folder).
fn create_shortcut(exe: &Path, dir: &Path) -> Result<PathBuf, String> {
    let path = shortcut_path().ok_or("no Start menu folder")?;
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.message())?;
        link.SetPath(&HSTRING::from(exe)).map_err(|e| e.message())?;
        link.SetWorkingDirectory(&HSTRING::from(dir)).map_err(|e| e.message())?;
        let _ = link.SetDescription(w!("A lightweight, productivity-focused dock for your desktop"));
        let _ = link.SetIconLocation(&HSTRING::from(exe), 0);
        let file: IPersistFile = link.cast().map_err(|e| e.message())?;
        file.Save(&HSTRING::from(path.as_path()), true).map_err(|e| e.message())?;
    }
    Ok(path)
}

/// Settings > Apps > Installed apps (Apps & features): name, version, size, and how to remove it.
fn write_uninstall_entry(exe: &Path, dir: &Path, size: u64) -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let path = HSTRING::from(uninstall_key());
        if RegCreateKeyExW(HKEY_CURRENT_USER, &path, None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None) != ERROR_SUCCESS {
            return Err("Couldn't add Desktop Dock to Installed apps.".into());
        }
        let text = |name: PCWSTR, value: &str| {
            let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
            let _ = RegSetValueExW(key, name, None, REG_SZ, Some(bytes));
        };
        let number = |name: PCWSTR, value: u32| {
            let _ = RegSetValueExW(key, name, None, REG_DWORD, Some(&value.to_le_bytes()));
        };
        let quoted = format!("\"{}\"", exe.display());
        let t = windows::Win32::System::SystemInformation::GetLocalTime();
        text(w!("DisplayName"), "Desktop Dock");
        text(w!("DisplayIcon"), &format!("{},0", exe.display()));
        text(w!("DisplayVersion"), &shown(my_version()));
        text(w!("Publisher"), "Desktop Dock");
        text(w!("InstallDate"), &format!("{:04}{:02}{:02}", t.wYear, t.wMonth, t.wDay));
        text(w!("InstallLocation"), &dir.display().to_string());
        text(w!("UninstallString"), &format!("{quoted} --uninstall"));
        text(w!("QuietUninstallString"), &format!("{quoted} --uninstall --quiet"));
        number(w!("NoModify"), 1);
        number(w!("NoRepair"), 1);
        number(w!("EstimatedSize"), (size / 1024).max(1) as u32);
        let _ = RegCloseKey(key);
    }
    Ok(())
}

/// Removes Desktop Dock: its shortcut, its Installed apps entry, Start with Windows, the program,
/// its icon cache and logs; with `remove_dock`, your dock too. Returns what's left, if anything.
pub fn uninstall(remove_dock: bool) -> Result<Option<PathBuf>, String> {
    let dir = install_dir().ok_or("Windows didn't say where your programs go (LOCALAPPDATA).")?;
    log(&dir, "removing Desktop Dock");
    if test_root().is_none() {
        close_settings_windows();
        if !system::quit_running_dock() {
            return Err("The running dock didn't close. Right-click it > Exit Desktop Dock, then try again.".into());
        }
        let _ = wait_until_free(Duration::from_secs(10));
        let _ = system::set_starts_with_windows(false);
    }
    if let Some(shortcut) = shortcut_path() {
        let _ = std::fs::remove_file(shortcut);
    }
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(uninstall_key()));
    }
    // A running exe can't be deleted, but it can be moved on the same drive: this copy (run
    // from Installed apps) moves itself to the temporary folder and is gone at the next cleanup.
    let _ = std::env::set_current_dir(std::env::temp_dir());
    let exe = dir.join("desktop-dock.exe");
    if let Ok(me) = std::env::current_exe() {
        if same_path(&me, &exe) {
            let away = std::env::temp_dir().join(format!("desktop-dock-removed-{}.exe", stamp()));
            let _ = std::fs::rename(&me, &away);
        }
    }
    let _ = std::fs::remove_file(&exe);
    remove_old_copies(&dir);
    for folder in ["cache", "logs"] {
        let _ = std::fs::remove_dir_all(dir.join(folder));
    }
    let _ = std::fs::remove_file(dir.join("setup.log"));
    if remove_dock {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let left = dir.exists().then_some(dir);
    remember_kept_dock(left.as_deref());
    Ok(left)
}

/// Old versions renamed aside by an update (removed at the next start, once they've exited).
pub fn remove_old_copies(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        if name.starts_with("desktop-dock.old-") && name.ends_with(".exe") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// ---- The windows ------------------------------------------------------------------------------

/// What the setup window says and offers, for the version already installed (if any).
fn setup_texts(installed: Option<(u16, u16, u16)>, mine: (u16, u16, u16)) -> (String, &'static str) {
    match installed {
        None => ("Installs Desktop Dock for you on this PC, with a Start menu shortcut. It doesn't need administrator rights.".into(), "Install"),
        Some(old) if old < mine => (format!("Updates Desktop Dock from {} to {}. Your dock stays as it is.", shown(old), shown(mine)), "Update"),
        Some(old) if old == mine => (format!("Desktop Dock {} is already installed. Install it again? Your dock stays as it is.", shown(old)), "Reinstall"),
        Some(old) => (
            format!("A newer Desktop Dock ({}) is installed. Install this older one ({}) instead? Your dock stays as it is.", shown(old), shown(mine)),
            "Install anyway",
        ),
    }
}

thread_local! {
    static CHOICE: RefCell<bool> = const { RefCell::new(false) };
    static SETUP: RefCell<SetupState> = RefCell::new(SetupState::default());
}

/// The install's result, from its own thread to the window.
static FINISHED: std::sync::Mutex<Option<Result<(), String>>> = std::sync::Mutex::new(None);

/// The setup window's state.
#[derive(Default)]
struct SetupState {
    /// Where it goes: the suggested folder, the one you chose, or where it's installed.
    dir: Option<PathBuf>,
    /// Already installed there (an update or reinstall goes where it is).
    installed: bool,
    /// It's a newer version than the installed one.
    updating: bool,
    startup: bool,
    /// The install is under way.
    busy: bool,
    /// The progress bar, in percent: where the install is, and where the bar is drawn (it moves
    /// there steadily, so each step can be seen).
    target: u32,
    shown: u32,
    /// The steps the install has begun, and the one the window says (the bar's, not always the
    /// install's latest: the words follow the bar).
    steps: Vec<Step>,
    said: Option<Step>,
    /// The install went well, and since when the bar has been full.
    succeeded: bool,
    full_since: Option<Instant>,
    /// The Done page is showing.
    done: bool,
}

fn enable(dialog: HWND, id: i32, on: bool) {
    unsafe {
        if let Ok(control) = GetDlgItem(Some(dialog), id) {
            let _ = EnableWindow(control, on);
        }
    }
}

fn show(dialog: HWND, id: i32, on: bool) {
    unsafe {
        if let Ok(control) = GetDlgItem(Some(dialog), id) {
            let _ = ShowWindow(control, if on { SW_SHOW } else { SW_HIDE });
        }
    }
}

/// The folder it's going to, and a word about it.
fn show_where(dialog: HWND, dir: &Path, installed: bool) {
    set_text(dialog, IDC_WHERE, &dir.display().to_string());
    let note = if installed {
        "It's installed here, so it updates in place."
    } else if dir.join("dock.toml").is_file() {
        "Your dock already in this folder will be used."
    } else {
        ""
    };
    set_text(dialog, IDC_WHERE_NOTE, note);
}

/// Install pressed: the choices are set, and the install runs on its own thread while the bar
/// shows how far it is.
fn start_install(dialog: HWND) {
    let startup = unsafe { IsDlgButtonChecked(dialog, IDC_CHOICE) } == BST_CHECKED.0;
    let Some(dir) = SETUP.with(|s| {
        let mut s = s.borrow_mut();
        (s.busy, s.target, s.shown, s.startup) = (true, 0, 0, startup);
        (s.steps, s.said) = (Vec::new(), None);
        if crate::breaks::broken("setup.ignores-change") { default_install_dir() } else { s.dir.clone() }
    }) else {
        return;
    };
    for id in [IDOK.0, IDCANCEL.0, IDC_CHANGE, IDC_CHOICE] {
        enable(dialog, id, false);
    }
    set_text(dialog, IDC_WHERE_NOTE, "");
    set_text(dialog, IDC_STATUS, "Getting ready...");
    unsafe {
        if let Ok(bar) = GetDlgItem(Some(dialog), IDC_PROGRESS) {
            SendMessageW(bar, PBM_SETRANGE32, Some(WPARAM(0)), Some(LPARAM(100)));
            SendMessageW(bar, PBM_SETPOS, Some(WPARAM(0)), None);
        }
        let _ = SetTimer(Some(dialog), TIMER_BAR, 30, None);
    }
    show(dialog, IDC_PROGRESS, true);
    let window = dialog.0 as isize;
    std::thread::spawn(move || {
        let post = |message: u32, value: usize| unsafe {
            let _ = PostMessageW(Some(HWND(window as *mut c_void)), message, WPARAM(value), LPARAM(0));
        };
        let result = install(Options { quiet: false, start: true, startup: Some(startup) }, &dir, &|step| {
            post(WM_APP_STEP, Step::ALL.iter().position(|s| *s == step).unwrap_or(0));
        });
        if let Err(e) = &result {
            log(&dir, &format!("failed: {e}"));
        }
        if let Ok(mut slot) = FINISHED.lock() {
            *slot = Some(result);
        }
        post(WM_APP_FINISHED, 0);
    });
}

/// The install went wrong: say why, and let you try again (or cancel).
fn install_failed(dialog: HWND, why: &str) {
    unsafe {
        let _ = KillTimer(Some(dialog), TIMER_BAR);
    }
    let installed = SETUP.with(|s| {
        let mut s = s.borrow_mut();
        s.busy = false;
        s.installed
    });
    show(dialog, IDC_PROGRESS, false);
    set_text(dialog, IDC_STATUS, why);
    for id in [IDOK.0, IDCANCEL.0, IDC_CHOICE] {
        enable(dialog, id, true);
    }
    enable(dialog, IDC_CHANGE, !installed);
}

/// What the Done page says: where the dock is, and how to bring it out.
fn done_texts(updating: bool, startup: bool) -> (&'static str, String) {
    let heading = if updating { "Desktop Dock is updated" } else { "Desktop Dock is installed" };
    let text = format!(
        "Your dock is at the top of your screen: rest the pointer at the top edge to bring it out. It's in the Start menu too{}",
        if startup { ", and it starts with Windows." } else { "." }
    );
    (heading, text)
}

fn show_done(dialog: HWND) {
    let (updating, startup) = SETUP.with(|s| {
        let mut s = s.borrow_mut();
        (s.done, s.busy) = (true, false);
        (s.updating, s.startup)
    });
    for id in [IDC_WHERE_LABEL, IDC_WHERE, IDC_CHANGE, IDC_WHERE_NOTE, IDC_CHOICE, IDC_PROGRESS, IDCANCEL.0] {
        show(dialog, id, false);
    }
    let (heading, text) = done_texts(updating, startup);
    set_text(dialog, IDC_HEADING, heading);
    set_text(dialog, IDC_TEXT, &text);
    set_text(dialog, IDC_STATUS, "");
    set_text(dialog, IDOK.0, "Done");
    enable(dialog, IDOK.0, true);
    // The window shrinks to what's left (the heading and two lines), Done at the right below.
    unsafe {
        let mut button = RECT { left: 202, top: 70, right: 272, bottom: 84 };
        let mut client = RECT { left: 0, top: 0, right: 280, bottom: 94 };
        let _ = MapDialogRect(dialog, &mut button);
        let _ = MapDialogRect(dialog, &mut client);
        if let Ok(done) = GetDlgItem(Some(dialog), IDOK.0) {
            let _ = SetWindowPos(done, None, button.left, button.top, button.right - button.left, button.bottom - button.top, SWP_NOZORDER);
        }
        let (mut window, mut now) = (RECT::default(), RECT::default());
        if GetWindowRect(dialog, &mut window).is_ok() && GetClientRect(dialog, &mut now).is_ok() {
            let height = (window.bottom - window.top) - (now.bottom - client.bottom);
            let _ = SetWindowPos(dialog, None, 0, 0, window.right - window.left, height, SWP_NOZORDER | SWP_NOMOVE);
        }
    }
    unsafe {
        if let Ok(done) = GetDlgItem(Some(dialog), IDOK.0) {
            let _ = SetFocus(Some(done));
        }
    }
}

/// The bar moves toward where the install is, at most 3% a tick (about a second for all of it),
/// and once it's full and the install went well, the Done page follows a moment later.
fn step_bar(dialog: HWND) {
    let (shown, say, finished) = SETUP.with(|s| {
        let mut s = s.borrow_mut();
        s.shown = (s.shown + 3).min(s.target);
        if s.succeeded && s.shown == 100 {
            s.full_since.get_or_insert_with(Instant::now);
        }
        // The step the bar has come to (each one's words show as the bar reaches it).
        let at = s.shown;
        let step = s.steps.iter().rev().find(|step| step.percent() <= at + 12).or(s.steps.first()).copied();
        let say = step.filter(|step| s.said != Some(*step));
        if say.is_some() {
            s.said = say;
        }
        (s.shown, say, s.full_since.is_some_and(|since| since.elapsed() >= Duration::from_millis(400)))
    });
    if let Some(step) = say {
        set_text(dialog, IDC_STATUS, step.words());
    }
    unsafe {
        if let Ok(bar) = GetDlgItem(Some(dialog), IDC_PROGRESS) {
            SendMessageW(bar, PBM_SETPOS, Some(WPARAM(shown as usize)), None);
        }
    }
    if finished {
        unsafe {
            let _ = KillTimer(Some(dialog), TIMER_BAR);
        }
        show_done(dialog);
    }
}

fn set_text(dialog: HWND, id: i32, text: &str) {
    unsafe {
        let _ = SetDlgItemTextW(dialog, id, &HSTRING::from(text));
    }
}

fn show_icon(dialog: HWND) {
    unsafe {
        if let Ok(control) = GetDlgItem(Some(dialog), IDC_ICON) {
            let mut rect = windows::Win32::Foundation::RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(control, &mut rect);
            if let Some(icon) = chrome::app_icon(rect.right - rect.left) {
                SendMessageW(control, STM_SETICON, Some(WPARAM(icon.0 as usize)), None);
            }
        }
    }
    chrome::set_app_icon(dialog);
    unsafe {
        if let Ok(heading) = GetDlgItem(Some(dialog), IDC_HEADING) {
            chrome::style_title(heading);
        }
    }
    chrome::apply_theme(dialog);
}

/// The setup window (a copy run from outside its install folder). The exit code: 0 installed,
/// 1 failed, 2 cancelled.
pub fn run_setup_window() -> i32 {
    unsafe extern "system" fn proc(dialog: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
        if let Some(result) = chrome::themed_message(dialog, msg, wparam, lparam) {
            return result;
        }
        match msg {
            WM_INITDIALOG => {
                show_icon(dialog);
                // Installed already (where its Installed apps entry says, or the usual place): an
                // update goes there. Otherwise the usual place, unless you choose another.
                let dir = install_dir();
                let installed = dir.as_ref().map(|dir| dir.join("desktop-dock.exe")).filter(|exe| exe.is_file());
                let (text, button) = setup_texts(installed.as_deref().and_then(version_of), my_version());
                set_text(dialog, IDC_TEXT, &text);
                set_text(dialog, IDOK.0, button);
                let startup = installed.is_none() || system::starts_with_windows();
                unsafe {
                    let _ = CheckDlgButton(dialog, IDC_CHOICE, if startup { BST_CHECKED } else { BST_UNCHECKED });
                }
                if let Some(dir) = &dir {
                    show_where(dialog, dir, installed.is_some());
                }
                enable(dialog, IDC_CHANGE, installed.is_none());
                SETUP.with(|s| {
                    *s.borrow_mut() = SetupState { dir, installed: installed.is_some(), updating: button == "Update", startup, ..SetupState::default() }
                });
                // Enter installs (the first button would otherwise be Change...).
                unsafe {
                    if let Ok(install) = GetDlgItem(Some(dialog), IDOK.0) {
                        let _ = SetFocus(Some(install));
                    }
                }
                0
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as i32;
                let (busy, done) = SETUP.with(|s| (s.borrow().busy, s.borrow().done));
                if busy {
                    return 1; // nothing until the install is over (Esc included)
                }
                if id == IDCANCEL.0 {
                    unsafe {
                        let _ = EndDialog(dialog, if done { 0 } else { 2 });
                    }
                } else if id == IDOK.0 && done {
                    unsafe {
                        let _ = EndDialog(dialog, 0);
                    }
                } else if id == IDOK.0 {
                    start_install(dialog);
                } else if id == IDC_CHANGE {
                    let current = SETUP.with(|s| s.borrow().dir.clone());
                    let start = current.as_deref().and_then(Path::parent).map(Path::to_path_buf);
                    if let Some(picked) = pickers::choose_install_folder(Some(dialog), start.as_deref()) {
                        match install_dir_in(Path::new(&picked)) {
                            Ok(dir) => {
                                show_where(dialog, &dir, false);
                                SETUP.with(|s| s.borrow_mut().dir = Some(dir));
                            }
                            Err(why) => set_text(dialog, IDC_WHERE_NOTE, &why),
                        }
                    }
                    unsafe {
                        if let Ok(install) = GetDlgItem(Some(dialog), IDOK.0) {
                            let _ = SetFocus(Some(install)); // Enter installs
                        }
                    }
                }
                1
            }
            WM_APP_STEP => {
                let step = Step::ALL.get(wparam.0).copied().unwrap_or(Step::Copy);
                SETUP.with(|s| {
                    let mut s = s.borrow_mut();
                    s.target = step.percent();
                    s.steps.push(step);
                });
                1
            }
            WM_APP_FINISHED => {
                match FINISHED.lock().ok().and_then(|mut slot| slot.take()) {
                    Some(Ok(())) => SETUP.with(|s| {
                        let mut s = s.borrow_mut();
                        (s.succeeded, s.target) = (true, 100);
                    }),
                    Some(Err(why)) => install_failed(dialog, &why),
                    None => install_failed(dialog, "The install stopped before it finished."),
                }
                1
            }
            WM_TIMER if wparam.0 == TIMER_BAR => {
                step_bar(dialog);
                1
            }
            _ => 0,
        }
    }
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let controls = INITCOMMONCONTROLSEX { dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES };
        let _ = InitCommonControlsEx(&controls);
        let Ok(instance) = GetModuleHandleW(None) else { return 1 };
        DialogBoxParamW(Some(instance.into()), PCWSTR(IDD_SETUP as *const u16), None, Some(proc), LPARAM(0)) as i32
    }
}

/// `--install [--quiet]`: installs with no window (or with a message if it fails and isn't quiet),
/// where it's installed already or else in the usual place.
pub fn run_install(options: Options) -> i32 {
    let Some(dir) = install_dir() else { return 1 };
    match install(options, &dir, &|_| {}) {
        Ok(()) => 0,
        Err(e) => {
            log(&dir, &format!("failed: {e}"));
            if !options.quiet {
                system::message_box(&e, "Desktop Dock setup", MB_ICONINFORMATION | MB_OK);
            }
            1
        }
    }
}

/// `--uninstall`: asks first (unless quiet), then removes Desktop Dock.
pub fn run_uninstall(quiet: bool, remove_dock: bool) -> i32 {
    unsafe extern "system" fn proc(dialog: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
        if let Some(result) = chrome::themed_message(dialog, msg, wparam, lparam) {
            return result;
        }
        match msg {
            WM_INITDIALOG => {
                show_icon(dialog);
                1
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as i32;
                if id == IDOK.0 || id == IDCANCEL.0 {
                    let remove = unsafe { IsDlgButtonChecked(dialog, IDC_CHOICE) } == BST_CHECKED.0;
                    CHOICE.with(|choice| *choice.borrow_mut() = remove);
                    unsafe {
                        let _ = EndDialog(dialog, if id == IDOK.0 { 0 } else { 2 });
                    }
                }
                1
            }
            _ => 0,
        }
    }
    let mut remove_dock = remove_dock;
    if !quiet {
        let answer = unsafe {
            let Ok(instance) = GetModuleHandleW(None) else { return 1 };
            DialogBoxParamW(Some(instance.into()), PCWSTR(IDD_UNINSTALL as *const u16), None, Some(proc), LPARAM(0))
        };
        if answer != 0 {
            return 2;
        }
        remove_dock = CHOICE.with(|choice| *choice.borrow());
    }
    match uninstall(remove_dock) {
        Ok(left) => {
            if !quiet {
                let message = match left {
                    Some(dir) => format!("Desktop Dock has been removed. Your dock is kept in {}, so installing again brings it back.", dir.display()),
                    None => "Desktop Dock has been removed.".to_string(),
                };
                system::message_box(&message, "Desktop Dock", MB_ICONINFORMATION | MB_OK);
            }
            0
        }
        Err(e) => {
            if !quiet {
                system::message_box(&e, "Desktop Dock", MB_ICONINFORMATION | MB_OK);
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn what_a_run_is_for() {
        let install = Path::new(r"C:\Users\alex\AppData\Local\Programs\Desktop Dock");
        let installed = install.join("desktop-dock.exe");
        let download = Path::new(r"C:\Users\alex\Downloads\desktop-dock.exe");
        // Double-clicked in Downloads: the setup window. Installed, portable or a dev build: the dock.
        assert_eq!(role(&[], download, Some(install), false, false), Role::Setup);
        assert_eq!(role(&[], &installed, Some(install), false, false), Role::Run);
        assert_eq!(role(&[], download, Some(install), true, false), Role::Run, "a dock.toml beside it: portable");
        assert_eq!(role(&[], download, Some(install), false, true), Role::Run, "a development build");
        // Explicit flags win wherever it is (a script may install a development build).
        assert_eq!(
            role(&args(&["--install", "--quiet", "--no-start"]), download, Some(install), false, true),
            Role::Install(Options { quiet: true, start: false, startup: Some(true) })
        );
        assert_eq!(
            role(&args(&["--install", "--no-startup"]), download, Some(install), false, false),
            Role::Install(Options { quiet: false, start: true, startup: None })
        );
        assert_eq!(role(&args(&["--uninstall"]), &installed, Some(install), false, false), Role::Uninstall { quiet: false, remove_dock: false });
        assert_eq!(
            role(&args(&["--uninstall", "--quiet", "--remove-dock"]), &installed, Some(install), false, false),
            Role::Uninstall { quiet: true, remove_dock: true }
        );
        // The dock's own flags (the watchdog's child, --quit, --settings...) are the dock's.
        assert_eq!(role(&args(&["--child"]), download, Some(install), false, false), Role::Run);
    }

    #[test]
    fn installing_again_offers_the_folder_an_uninstall_kept_your_dock_in() {
        let chosen = || Some(PathBuf::from(r"C:\Users\alex\Documents\Desktop Dock"));
        let usual = || Some(PathBuf::from(r"C:\Users\alex\AppData\Local\Programs\Desktop Dock"));
        // Installed: where it is. Uninstalled, dock kept: that folder, not the usual place.
        assert_eq!(choose_install_dir(chosen(), None, usual()), chosen());
        assert_eq!(choose_install_dir(None, chosen(), usual()), chosen());
        assert_eq!(choose_install_dir(None, None, usual()), usual());
    }

    #[test]
    fn paths_compare_without_case_or_trailing_slashes() {
        assert!(same_path(Path::new(r"C:\Users\Alex\AppData"), Path::new(r"c:\users\alex\appdata\")));
        assert!(same_path(Path::new("C:/Users/alex"), Path::new(r"C:\Users\alex")));
        assert!(!same_path(Path::new(r"C:\Users\alex\Downloads"), Path::new(r"C:\Users\alex\AppData")));
    }

    #[test]
    fn the_setup_window_says_install_update_or_reinstall() {
        let mine = (1, 0, 2);
        assert_eq!(setup_texts(None, mine).1, "Install");
        assert_eq!(setup_texts(Some((1, 0, 1)), mine).1, "Update");
        assert!(setup_texts(Some((1, 0, 1)), mine).0.contains("from 1.0.1 to 1.0.2"));
        assert_eq!(setup_texts(Some(mine), mine).1, "Reinstall");
        assert_eq!(setup_texts(Some((2, 0, 0)), mine).1, "Install anyway");
    }

    #[test]
    fn a_chosen_folder_gets_its_own_desktop_dock_folder_and_nothing_is_left_behind() {
        let picked = std::env::temp_dir().join(format!("dd-setup-pick-{}", std::process::id()));
        std::fs::create_dir_all(&picked).unwrap();
        let dir = install_dir_in(&picked).unwrap();
        assert_eq!(dir, picked.join("Desktop Dock"));
        assert!(!dir.exists(), "only tried, not made: Cancel leaves nothing");
        // A "Desktop Dock" folder chosen itself is used as it is (and kept, it was there).
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(install_dir_in(&dir).unwrap(), dir);
        assert!(dir.is_dir());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "the try leaves no file");
        let _ = std::fs::remove_dir_all(&picked);
        // Another computer: said plainly.
        assert!(install_dir_in(Path::new(r"\\nas\apps")).unwrap_err().contains("another computer"));
    }

    #[test]
    fn the_done_page_says_where_the_dock_is() {
        let (heading, text) = done_texts(false, true);
        assert_eq!(heading, "Desktop Dock is installed");
        assert!(text.contains("top of your screen") && text.ends_with("starts with Windows."));
        assert_eq!(done_texts(true, false).0, "Desktop Dock is updated");
        assert!(done_texts(true, false).1.ends_with("Start menu too."));
    }

    #[test]
    fn this_copy_knows_its_version() {
        assert_eq!(shown(my_version()), env!("CARGO_PKG_VERSION"));
        // The manifest's version is written by hand: keep it equal to Cargo.toml's.
        let manifest = include_str!("../assets/desktop-dock.manifest");
        assert!(manifest.contains(&format!("version=\"{}.0\"", env!("CARGO_PKG_VERSION"))), "assets/desktop-dock.manifest's version");
    }
}
