//! Launching items with ShellExecuteEx, the same documented call Explorer uses.
//!
//! The call itself runs in a short-lived helper: `desktop-dock.exe --launch …`, the same program
//! with no window, gone a moment later. Opening anything loads Windows' shell machinery (and any
//! shell add-ons) into whichever process asks, for good: about 3 MB in the dock, measured. In
//! the helper it goes when the helper exits, and an add-on that misbehaves can't take the dock
//! down. The dock waits for the helper on a background thread, so a slow launch never stalls
//! its animation; if the helper can't be started, the dock does it itself (fail soft).

use crate::config::{ItemConfig, Kind, RunState};
use crate::heal;
use crate::home::Home;
use crate::places;
use crate::target::{self, Class};
use crate::{log_info, log_warn};
use std::ffi::c_void;
use std::path::Path;
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, ERROR_CANCELLED, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::UI::Shell::{
    ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_NOTRUNCATE, ASSOCSTR_COMMAND, ASSOCSTR_EXECUTABLE, AssocQueryStringW, ILFree, SEE_MASK_FLAG_NO_UI, SEE_MASK_IDLIST, SEE_MASK_NOASYNC,
    SHELLEXECUTEINFOW, SHEmptyRecycleBinW, SHParseDisplayName, ShellExecuteExW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, PostMessageW, SHOW_WINDOW_CMD, SW_SHOWMAXIMIZED, SW_SHOWMINNOACTIVE,
    SW_SHOWNORMAL,
};
use windows::core::{Error, HRESULT, HSTRING, PCWSTR, PWSTR, Result, w};

/// How a launch reports back to the dock's window.
#[derive(Clone, Copy)]
pub struct Notify {
    hwnd: isize,
    /// Posted when the program has moved (an update): time for a repair pass.
    moved: u32,
    /// Posted with `item` and `generation` when the launch failed, for a visual cue.
    failed: u32,
    item: usize,
    generation: u64,
}

impl Notify {
    pub fn new(hwnd: HWND, moved: u32, failed: u32, item: usize, generation: u64) -> Self {
        Self { hwnd: hwnd.0 as isize, moved, failed, item, generation }
    }

    fn post(&self, message: u32, wparam: usize, lparam: isize) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut c_void)), message, WPARAM(wparam), LPARAM(lparam));
        }
    }
}

/// Launches an item. If its program has moved (an update), launches the new location and tells
/// the dock so it repairs dock.toml; if the launch fails, tells the dock that too.
pub fn launch(item: &ItemConfig, home: &Home, notify: Option<Notify>) {
    // The dock just received the user's click, so it may hand foreground rights on. This lets
    // apps started through a helper (Discord's Update.exe, for example) come to the front.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    if item.kind == Kind::Group {
        return; // a group opens its pop-up on the dock; there's nothing to launch
    }
    let target = home.resolve(item.launch_target());
    if target::classify(&target) == Class::Empty {
        // Windows would open its own folder for an empty name (System32): nothing at all instead,
        // and the icon shakes, as for anything that doesn't open.
        log_warn!("{} has nothing to open (no target)", item.name);
        if let Some(notify) = notify {
            notify.post(notify.failed, notify.item, notify.generation as isize);
        }
        return;
    }
    let start_in = home.resolve(&item.start_in);
    let args = item.args.trim().to_string();
    let name = item.name.clone();
    let (run, admin) = (item.run, item.admin);
    let item = item.clone();
    std::thread::spawn(move || {
        let target = match heal::moved_target(&item, &target) {
            Some(moved) => {
                log_info!("{name}: {target} has moved to {moved}; launching that");
                if let Some(notify) = notify {
                    notify.post(notify.moved, 0, 0);
                }
                moved
            }
            None => target,
        };
        let dir = start_dir(&target, &start_in);
        match run_job(&Job::Item { target: target.clone(), args, dir, run, admin }) {
            Ok(()) => log_info!("launched {name}"),
            Err(e) if declined(e.code(), admin, &target) => log_info!("{name}: cancelled"),
            Err(e) => {
                log_warn!("couldn't launch {name} ({target}): {}", e.message());
                if let Some(notify) = notify {
                    notify.post(notify.failed, notify.item, notify.generation as isize);
                }
            }
        }
    });
}

/// "No" at Windows' administrator prompt: not a failure. Windows answers "cancelled" for that,
/// but also after showing its own error box (a missing file, nothing to open it with), which is.
fn declined(code: HRESULT, admin: bool, target: &str) -> bool {
    code == ERROR_CANCELLED.to_hresult() && admin && !is_missing_path(target)
}

/// True when `target` is a file or folder path (not a shell name or web address) that isn't there.
fn is_missing_path(target: &str) -> bool {
    target::classify(target) == Class::Absolute && !Path::new(target).exists()
}

/// Opens a folder in Explorer (or any file with its default program).
pub fn open(path: &Path) {
    let path = path.to_string_lossy().into_owned();
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    std::thread::spawn(move || {
        if let Err(e) = run_job(&Job::Open(path.clone())) {
            log_warn!("couldn't open {path}: {}", e.message());
        }
    });
}

/// Opens a text file for editing, without Windows' "no app associated" errors: the file's own
/// edit/open program if it has one, otherwise whatever you use for .txt files, then Notepad.
pub fn edit(path: &Path) {
    let path = path.to_string_lossy().into_owned();
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
    std::thread::spawn(move || {
        if run_job(&Job::Edit(path.clone())).is_err() {
            log_warn!("couldn't open {path} for editing");
        }
    });
}

/// Empties the Recycle Bin, with Windows' usual confirmation. Waits: call it from a background
/// thread.
pub fn empty_recycle_bin() -> Result<()> {
    run_job(&Job::EmptyBin)
}

/// One thing for the helper to do.
#[derive(Debug, Clone, PartialEq)]
enum Job {
    /// An item: what to open, its arguments and start-in folder, how its window opens, and
    /// whether as administrator.
    Item { target: String, args: String, dir: String, run: RunState, admin: bool },
    Open(String),
    Edit(String),
    EmptyBin,
}

impl Job {
    /// The helper's command line (after `--launch`).
    fn to_args(&self) -> Vec<String> {
        match self {
            Job::Item { target, args, dir, run, admin } => {
                let run = match run {
                    RunState::Normal => "normal",
                    RunState::Minimized => "minimized",
                    RunState::Maximized => "maximized",
                };
                vec!["item".into(), run.into(), if *admin { "admin" } else { "-" }.into(), target.clone(), args.clone(), dir.clone()]
            }
            Job::Open(path) => vec!["open".into(), path.clone()],
            Job::Edit(path) => vec!["edit".into(), path.clone()],
            Job::EmptyBin => vec!["empty-bin".into()],
        }
    }

    fn from_args(args: &[String]) -> Option<Job> {
        match args {
            [what, run, admin, target, args, dir] if what == "item" => {
                let run = match run.as_str() {
                    "minimized" => RunState::Minimized,
                    "maximized" => RunState::Maximized,
                    _ => RunState::Normal,
                };
                Some(Job::Item { target: target.clone(), args: args.clone(), dir: dir.clone(), run, admin: admin == "admin" })
            }
            [what, path] if what == "open" => Some(Job::Open(path.clone())),
            [what, path] if what == "edit" => Some(Job::Edit(path.clone())),
            [what] if what == "empty-bin" => Some(Job::EmptyBin),
            _ => None,
        }
    }

    /// Does it, in this process (COM must be set up). The helper exits straight after, so the
    /// shell is asked to finish before returning (SEE_MASK_NOASYNC).
    fn run_here(&self) -> Result<()> {
        unsafe {
            match self {
                // Show desktop is an action: asked to "open" its shell item, Windows says no app is
                // associated with it. Windows' own Show desktop shortcut runs it through Explorer.
                Job::Item { target, .. } if target.eq_ignore_ascii_case(places::SHOW_DESKTOP_TARGET) && !crate::breaks::broken("launch.show-desktop") => {
                    shell_execute_with("explorer.exe", target, "", None, SW_SHOWNORMAL, SEE_MASK_NOASYNC)
                }
                Job::Item { target, args, dir, run, admin } => {
                    use crate::breaks::broken;
                    let show = match run {
                        _ if broken("launch.ignore-run") => SW_SHOWNORMAL,
                        RunState::Normal => SW_SHOWNORMAL,
                        RunState::Minimized => SW_SHOWMINNOACTIVE,
                        RunState::Maximized => SW_SHOWMAXIMIZED,
                    };
                    // "runas" asks Windows to run it as administrator (with its usual permission prompt).
                    let verb = (*admin && !broken("launch.ignore-admin")).then_some(w!("runas"));
                    let args = if broken("launch.ignore-args") { "" } else { args.as_str() };
                    let dir = if broken("launch.ignore-start-in") { "" } else { dir.as_str() };
                    shell_execute_with(target, args, dir, verb, show, SEE_MASK_NOASYNC)
                }
                Job::Open(path) => shell_execute_with(path, "", "", None, SW_SHOWNORMAL, SEE_MASK_NOASYNC),
                Job::Edit(path) => {
                    let quoted = format!("\"{path}\"");
                    let quiet = |target: &str, args: &str, verb: Option<PCWSTR>| {
                        shell_execute_with(target, args, "", verb, SW_SHOWNORMAL, SEE_MASK_FLAG_NO_UI | SEE_MASK_NOASYNC).is_ok()
                    };
                    // Its own app only if Windows has one for it (or it would ask which).
                    let opened = (has_app(path, "edit") && quiet(path, "", Some(w!("edit"))))
                        || (has_app(path, "open") && quiet(path, "", None))
                        || text_editor().is_some_and(|editor| quiet(&editor, &quoted, None))
                        || quiet("notepad.exe", &quoted, None);
                    if opened { Ok(()) } else { Err(Error::from_hresult(E_FAIL)) }
                }
                Job::EmptyBin if crate::breaks::broken("bin.empty-does-nothing") => Ok(()),
                Job::EmptyBin => SHEmptyRecycleBinW(None, PCWSTR::null(), 0),
            }
        }
    }
}

/// Runs a job in the helper and waits for it; its result as the helper reports it (an HRESULT
/// as its exit code). If the helper can't be started, does the job here instead.
fn run_job(job: &Job) -> Result<()> {
    let status = std::env::current_exe().and_then(|exe| std::process::Command::new(exe).arg("--launch").args(job.to_args()).status());
    match status {
        Ok(status) => match status.code() {
            Some(0) => Ok(()),
            Some(code) => Err(Error::from_hresult(HRESULT(code))),
            None => Err(Error::from_hresult(E_FAIL)),
        },
        Err(e) => {
            log_warn!("the launch helper didn't start ({e}); opening it from the dock instead");
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            }
            job.run_here()
        }
    }
}

/// The helper's whole job (`desktop-dock.exe --launch <job…>`): does it and exits with the result,
/// 0 or the HRESULT of what went wrong.
pub fn run_helper(args: &[String]) -> i32 {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    match Job::from_args(args).map(|job| job.run_here()) {
        Some(Ok(())) => 0,
        Some(Err(e)) => e.code().0,
        None => E_INVALIDARG.0,
    }
}

/// The program Windows uses to open .txt files (your preferred text editor).
/// Whether Windows has an app that does `verb` ("open", "edit") for this kind of file. Without
/// one, Windows asks which app to use instead.
fn has_app(path: &str, verb: &str) -> bool {
    let Some(extension) = Path::new(path).extension() else { return false };
    let (extension, verb) = (HSTRING::from(format!(".{}", extension.to_string_lossy())), HSTRING::from(verb));
    let mut len = 0u32;
    unsafe { AssocQueryStringW(ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE, ASSOCSTR_COMMAND, &extension, &verb, None, &mut len) }.is_ok()
}

fn text_editor() -> Option<String> {
    let mut buffer = vec![0u16; 1024];
    let mut len = buffer.len() as u32;
    unsafe {
        AssocQueryStringW(ASSOCF_NOTRUNCATE, ASSOCSTR_EXECUTABLE, w!(".txt"), w!("open"), Some(PWSTR(buffer.as_mut_ptr())), &mut len)
            .ok()
            .ok()?;
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    let exe = String::from_utf16_lossy(&buffer[..end]);
    Path::new(&exe).is_file().then_some(exe)
}

unsafe fn shell_execute_with(
    target: &str,
    args: &str,
    dir: &str,
    verb: Option<PCWSTR>,
    show: SHOW_WINDOW_CMD,
    mask: u32,
) -> Result<()> {
    let file = HSTRING::from(target);
    let params = HSTRING::from(args);
    let dir = HSTRING::from(dir);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: mask,
        nShow: show.0,
        lpVerb: verb.unwrap_or(PCWSTR::null()),
        ..Default::default()
    };
    if !params.is_empty() {
        info.lpParameters = PCWSTR(params.as_ptr());
    }
    if !dir.is_empty() {
        info.lpDirectory = PCWSTR(dir.as_ptr());
    }

    if target::classify(target) == Class::Shell {
        // Shell locations (This PC, Recycle Bin, ...) are opened by their item ID.
        unsafe {
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(&file, None, &mut pidl, 0, None)?;
            info.fMask |= SEE_MASK_IDLIST;
            info.lpIDList = pidl as *mut c_void;
            let result = ShellExecuteExW(&mut info);
            ILFree(Some(pidl));
            result
        }
    } else {
        info.lpFile = PCWSTR(file.as_ptr());
        unsafe { ShellExecuteExW(&mut info) }
    }
}

/// The configured start-in folder if it exists, otherwise the target's own folder for files.
/// (A start-in folder can vanish when an app updates, e.g. Discord's versioned `app-1.0.x`.)
fn start_dir(target: &str, start_in: &str) -> String {
    if !start_in.is_empty() {
        if Path::new(start_in).is_dir() {
            return start_in.to_string();
        }
        log_info!("start-in folder {start_in} is missing; using the app's own folder");
    }
    let target = Path::new(target);
    if target.is_file() {
        if let Some(parent) = target.parent() {
            return parent.to_string_lossy().into_owned();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_survive_the_trip_to_the_helper() {
        let jobs = [
            Job::Item {
                target: r"C:\Program Files\App\app.exe".into(),
                args: r#"--open "C:\My Files\a b.txt" """#.into(),
                dir: String::new(),
                run: RunState::Maximized,
                admin: true,
            },
            Job::Item { target: r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into(), args: String::new(), dir: r"C:\".into(), run: RunState::Normal, admin: false },
            Job::Open(r"C:\Users\me\Desktop Dock".into()),
            Job::Edit(r"C:\dock.toml".into()),
            Job::EmptyBin,
        ];
        for job in jobs {
            assert_eq!(Job::from_args(&job.to_args()), Some(job.clone()), "{job:?}");
        }
        assert_eq!(Job::from_args(&["item".into(), "normal".into()]), None, "half a job is none");
    }

    #[test]
    fn a_kind_of_file_windows_has_no_app_for_is_told_apart() {
        // Without an app, "open" shows Windows' "Select an app" chooser (for a .toml file on a
        // fresh Windows, say), so the dock goes to a text editor instead.
        let dir = std::env::temp_dir().join(format!("dock-launch-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (unknown, text) = (dir.join("dock.ddnoapp"), dir.join("note.txt"));
        std::fs::write(&unknown, "x").unwrap();
        std::fs::write(&text, "x").unwrap();
        assert!(!has_app(&unknown.to_string_lossy(), "open"));
        assert!(!has_app(&unknown.to_string_lossy(), "edit"));
        assert!(has_app(&text.to_string_lossy(), "open"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cancelled_is_a_failure_unless_it_was_no_at_the_administrator_prompt() {
        let cancelled = ERROR_CANCELLED.to_hresult();
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        assert!(declined(cancelled, true, &exe), "No at the administrator prompt");
        assert!(!declined(cancelled, true, r"C:\not-here\nothing.exe"), "Windows said it can't find it");
        // Without "run as administrator" there's no prompt: Windows showed its own error box
        // (Show desktop's "no app associated", a link nothing opens).
        assert!(!declined(cancelled, false, places::SHOW_DESKTOP_TARGET));
        assert!(!declined(cancelled, false, "ddtest:nothing-opens-this"));
        assert!(!declined(cancelled, false, &exe));
        assert!(!declined(E_FAIL, true, &exe), "any other error is a failure");
    }

    #[test]
    fn a_missing_start_in_folder_falls_back_to_the_apps_own_folder() {
        let exe = std::env::current_exe().unwrap();
        let own = exe.parent().unwrap().to_string_lossy().into_owned();
        let target = exe.to_string_lossy().into_owned();
        assert_eq!(start_dir(&target, r"C:\Users\nobody\AppData\Local\Discord\app-1.0.9158"), own);
        assert_eq!(start_dir(&target, ""), own);
        let temp = std::env::temp_dir().to_string_lossy().into_owned();
        assert_eq!(start_dir(&target, &temp), temp, "an existing start-in folder is used as is");
    }

    #[test]
    fn only_paths_that_are_not_there_count_as_missing() {
        let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
        assert!(!is_missing_path(&exe));
        assert!(is_missing_path(r"Q:\Tools\gone\start.bat"));
        assert!(is_missing_path(r"\\localhost\no-such-share\gone.exe"));
        assert!(!is_missing_path("shell:RecycleBinFolder"), "shell names aren't paths");
        assert!(!is_missing_path("::{645FF040-5081-101B-9F08-00AA002F954E}"));
        assert!(!is_missing_path("https://example.com"));
    }
}
