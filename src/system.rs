//! Small questions the dock asks Windows: where the pointer is, which monitor to use,
//! and whether a fullscreen app (game, video, presentation) is in front.

use crate::{log_info, log_warn};
use std::ffi::c_void;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError, HANDLE, HWND, LPARAM, POINT, RECT, WAIT_ABANDONED,
    WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::Storage::FileSystem::{FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FindFirstChangeNotificationW};
use windows::Win32::System::Power::{POWERBROADCAST_SETTING, RegisterPowerSettingNotification};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::System::Threading::OpenProcessToken;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, WaitForSingleObject};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW, SHFileOperationW, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DEVICE_NOTIFY_WINDOW_HANDLE, FindWindowW, GUI_INMOVESIZE, GUITHREADINFO, GetClassNameW, GetCursorPos, GetForegroundWindow,
    GetGUIThreadInfo, GetSystemMetrics, GetWindowRect, IsZoomed, SM_SWAPBUTTON,
    MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MESSAGEBOX_RESULT, MESSAGEBOX_STYLE, MessageBoxW, PostMessageW, RegisterWindowMessageW,
    SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW, WM_CLOSE, WTS_CONSOLE_CONNECT,
    WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGON, WTS_SESSION_UNLOCK,
};
use windows::core::{HSTRING, PCWSTR, w};

/// A message as people read it: Windows' error numbers ("(os error 32)", "(0x80070005)") are
/// left out, as its own words already say what happened.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let inner = &rest[open + 1..];
        let code = inner.find(')').map(|close| &inner[..close]).filter(|code| {
            let number = code.strip_prefix("os error ").map(|n| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty());
            let hex = code.strip_prefix("0x").map(|n| n.len() == 8 && n.chars().all(|c| c.is_ascii_hexdigit()));
            number.or(hex).unwrap_or(false)
        });
        match code {
            Some(code) => {
                out.push_str(rest[..open].trim_end_matches(' '));
                rest = &inner[code.len() + 1..];
                if out.ends_with('.') {
                    rest = rest.strip_prefix('.').unwrap_or(rest); // "denied. (0x80070005)." reads "denied."
                }
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = inner;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Every message box goes through here, so the log says what it said (and a person's log tells
/// whoever helps them what they saw).
pub fn message_box(text: &str, title: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    log_info!("message box: {}", text.split_whitespace().collect::<Vec<_>>().join(" "));
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), style) }
}

/// Shows a message without blocking the dock: the box runs on its own thread.
pub fn notice(message: String) {
    std::thread::spawn(move || {
        message_box(&plain(&message), "Desktop Dock", MB_ICONINFORMATION | MB_OK | MB_SETFOREGROUND | MB_TOPMOST);
    });
}

/// Something went wrong: the same as `notice`, with Windows' warning icon.
pub fn warning(message: String) {
    std::thread::spawn(move || {
        message_box(&plain(&message), "Desktop Dock", MB_ICONWARNING | MB_OK | MB_SETFOREGROUND | MB_TOPMOST);
    });
}

/// Shows a message and waits for OK (for when the process is about to exit).
pub fn notice_blocking(message: String) {
    message_box(&plain(&message), "Desktop Dock", MB_ICONINFORMATION | MB_OK | MB_SETFOREGROUND | MB_TOPMOST);
}

/// A dock is running (its window is there).
pub fn dock_is_running() -> bool {
    unsafe { FindWindowW(w!("DesktopDock"), w!("Desktop Dock")).is_ok() }
}

/// Asks a running dock to exit, exactly like choosing Exit, and waits up to 5 s for its window
/// to close. True if no dock is left running. Any dock window still there is asked again, so a
/// second dock (one an older version could leave running) is closed too.
pub fn quit_running_dock() -> bool {
    unsafe {
        for _ in 0..50 {
            let Ok(hwnd) = FindWindowW(w!("DesktopDock"), w!("Desktop Dock")) else { return true };
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        FindWindowW(w!("DesktopDock"), w!("Desktop Dock")).is_err()
    }
}

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("Desktop Dock");
/// Where Windows keeps Startup apps' on/off switches (Settings > Apps > Startup, Task Manager).
const APPROVED_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run");
/// What Windows writes there for "on" (an odd first byte is "off").
const APPROVED_ON: [u8; 12] = [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

/// The installed copy, wherever setup put it (the usual place, or a folder you chose): the one
/// that starts with Windows.
pub fn installed_exe() -> Option<std::path::PathBuf> {
    let exe = crate::setup::install_dir()?.join("desktop-dock.exe");
    exe.is_file().then_some(exe)
}

/// What Windows runs at sign-in tells the dock so: if it's already running then (started by
/// hand a moment before), that copy just leaves, rather than opening Dock settings as starting
/// it again otherwise does.
pub const STARTUP_FLAG: &str = "--startup";

/// The command Windows runs at sign-in: always the installed copy, whichever copy asks, so a
/// development build never takes over your sign-in. A portable copy (not in a build folder)
/// registers itself if nothing is installed.
fn startup_command() -> Result<String, String> {
    startup_exe().map(|exe| format!("\"{}\" {STARTUP_FLAG}", exe.display()))
}

fn startup_exe() -> Result<std::path::PathBuf, String> {
    let exe = match installed_exe() {
        Some(installed) => installed,
        None => {
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            if crate::home::is_dev_build() {
                return Err("Start with Windows only works for the installed copy.".into());
            }
            me
        }
    };
    Ok(exe)
}

fn read_value(path: PCWSTR, name: PCWSTR, kind: windows::Win32::System::Registry::REG_ROUTINE_FLAGS) -> Option<Vec<u8>> {
    let mut buffer = vec![0u8; 2048];
    let mut bytes = buffer.len() as u32;
    let found = unsafe { RegGetValueW(HKEY_CURRENT_USER, path, name, kind, None, Some(buffer.as_mut_ptr() as *mut _), Some(&mut bytes)) };
    (found == ERROR_SUCCESS).then(|| {
        buffer.truncate(bytes as usize);
        buffer
    })
}

/// The program Windows starts for a bare name ("notepad.exe"): its App Paths entry (yours, then
/// the PC's), else the first in a PATH folder on this PC. None if neither has it. (Folders on
/// other computers are skipped: one that's switched off would hold the dock up.)
pub fn find_program(name: &str) -> Option<String> {
    let key = HSTRING::from(format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{name}"));
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let mut buffer = vec![0u16; 1024];
        let mut bytes = (buffer.len() * 2) as u32;
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ; // expanded as it's read
        let found = unsafe { RegGetValueW(root, &key, PCWSTR::null(), flags, None, Some(buffer.as_mut_ptr() as *mut _), Some(&mut bytes)) };
        if found == ERROR_SUCCESS {
            let path = String::from_utf16_lossy(&buffer[..bytes as usize / 2]).trim_end_matches('\0').trim().trim_matches('"').to_string();
            if std::path::Path::new(&path).is_file() {
                return Some(path);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty() && !crate::places::on_network(&dir.to_string_lossy()))
        .map(|dir| dir.join(name))
        .find(|file| file.is_file())
        .map(|file| file.to_string_lossy().into_owned())
}

/// A text value under HKEY_CURRENT_USER, if it's there.
pub fn read_user_text(path: &str, name: &str) -> Option<String> {
    let (path, name) = (HSTRING::from(path), HSTRING::from(name));
    let bytes = read_value(PCWSTR(path.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ)?;
    let wide: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).take_while(|&c| c != 0).collect();
    Some(String::from_utf16_lossy(&wide))
}

/// True if the installed dock starts with Windows: its Run entry is there, and Windows' Startup
/// apps switch for it isn't off.
pub fn starts_with_windows() -> bool {
    let Some(bytes) = read_value(RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ) else { return false };
    let wide: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).take_while(|&c| c != 0).collect();
    let value = String::from_utf16_lossy(&wide);
    // Ours as written now, or as earlier versions wrote it (the program alone).
    let ours = startup_exe().is_ok_and(|exe| {
        let value = value.trim();
        value.eq_ignore_ascii_case(&format!("\"{}\" {STARTUP_FLAG}", exe.display())) || value.eq_ignore_ascii_case(&format!("\"{}\"", exe.display()))
    });
    let switched_off = read_value(APPROVED_KEY, RUN_VALUE, RRF_RT_REG_BINARY).is_some_and(|flag| flag.first().is_some_and(|b| b % 2 == 1));
    ours && !switched_off
}

/// Adds or removes the per-user Run entry, with Windows' Startup apps switch set to match:
/// Windows 11 can skip an entry that has no switch record at all. No admin
/// needed; you can also switch it off in Settings > Apps > Startup.
pub fn set_starts_with_windows(on: bool) -> Result<(), String> {
    if on && crate::breaks::broken("startup.not-written") {
        return Ok(());
    }
    let command = if on { Some(startup_command()?) } else { None };
    unsafe {
        let open = |path: PCWSTR| -> Result<HKEY, String> {
            let mut key = HKEY::default();
            let status = RegCreateKeyExW(HKEY_CURRENT_USER, path, None, None, REG_OPTION_NON_VOLATILE, KEY_SET_VALUE, None, &mut key, None);
            if status == ERROR_SUCCESS { Ok(key) } else { Err("Windows' list of programs to start couldn't be opened.".into()) }
        };
        let run = open(RUN_KEY)?;
        let approved = open(APPROVED_KEY);
        let status = match &command {
            Some(command) => {
                let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
                let status = RegSetValueExW(run, RUN_VALUE, None, REG_SZ, Some(bytes));
                if let Ok(approved) = approved {
                    let _ = RegSetValueExW(approved, RUN_VALUE, None, REG_BINARY, Some(&APPROVED_ON));
                }
                status
            }
            None => {
                let status = RegDeleteValueW(run, RUN_VALUE);
                if let Ok(approved) = approved {
                    let _ = RegDeleteValueW(approved, RUN_VALUE);
                }
                if status == ERROR_FILE_NOT_FOUND { ERROR_SUCCESS } else { status }
            }
        };
        let _ = RegCloseKey(run);
        if let Ok(approved) = approved {
            let _ = RegCloseKey(approved);
        }
        if status == ERROR_SUCCESS { Ok(()) } else { Err(format!("Windows didn't allow the change (error {}).", status.0)) }
    }
}

/// Windows' accent colour, as window frames use it (Settings > Personalisation > Colours), as
/// 0..1 RGB. Read when the dock starts and whenever Windows says it changed.
pub fn accent_colour() -> Option<[f32; 3]> {
    let mut colour = 0u32;
    let mut opaque = windows::core::BOOL(0);
    unsafe { windows::Win32::Graphics::Dwm::DwmGetColorizationColor(&mut colour, &mut opaque).ok()? };
    let channel = |shift: u32| ((colour >> shift) & 0xFF) as f32 / 255.0;
    Some([channel(16), channel(8), channel(0)])
}

/// A Ctrl key is held down (it overrides Lock icons for a drag).
pub fn control_down() -> bool {
    unsafe { GetAsyncKeyState(VK_CONTROL.0 as i32) < 0 }
}

/// Shows a file or folder in Explorer, selected. Explorer does it in its own process, so nothing
/// of Explorer's loads into the dock.
pub fn open_location(path: &str) -> Result<(), String> {
    let path = path.trim().trim_matches('"');
    if !std::path::Path::new(path).exists() {
        return Err(format!("{path} isn't there any more."));
    }
    let windows = std::env::var_os("WINDIR").map(std::path::PathBuf::from).unwrap_or_else(|| "C:\\Windows".into());
    // Explorer wants the quotes around the path only (/select,"C:\Program Files\…"); quoting
    // the whole argument, as Command would for a path with spaces, isn't what it expects.
    use std::os::windows::process::CommandExt;
    let select = if crate::breaks::broken("location.no-select") { "" } else { "/select," };
    std::process::Command::new(windows.join("explorer.exe"))
        .raw_arg(format!("{select}\"{path}\""))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Couldn't open File Explorer: {e}."))
}

/// Whether anyone can see the dock. While the session is locked or switched away from, or the
/// display is off, the dock stops polling entirely.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Presence {
    locked: bool,
    disconnected: bool,
    display_off: bool,
}

impl Presence {
    pub fn away(&self) -> bool {
        self.locked || self.disconnected || self.display_off
    }

    /// A WM_WTSSESSION_CHANGE code.
    pub fn session_event(&mut self, code: u32) {
        match code {
            WTS_SESSION_LOCK => self.locked = true,
            WTS_SESSION_UNLOCK | WTS_SESSION_LOGON => self.locked = false,
            WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => self.disconnected = true,
            WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => self.disconnected = false,
            _ => {}
        }
    }

    /// The display's state from GUID_CONSOLE_DISPLAY_STATE: 0 off, 1 on, 2 dimmed (still visible).
    pub fn display_state(&mut self, state: u32) {
        self.display_off = state == 0;
    }

    /// Corrects the state from a direct look, in case a message was missed (the dock would
    /// otherwise wait for ever): the session's lock and connection as Windows reports them now
    /// (None if it can't say), and whether there's been keyboard or mouse input since the dock
    /// decided nobody was there (then the display is on).
    pub fn correct(&mut self, session: Option<(bool, bool)>, input_since: bool) {
        if let Some((locked, disconnected)) = session {
            self.locked = locked;
            self.disconnected = disconnected;
        }
        if input_since && !self.locked && !self.disconnected {
            self.display_off = false;
        }
    }
}

/// This session as Windows sees it now: (locked, disconnected). None if it can't say.
pub fn session_state() -> Option<(bool, bool)> {
    use windows::Win32::System::RemoteDesktop::{
        WTS_CURRENT_SESSION, WTS_SESSIONSTATE_LOCK, WTS_SESSIONSTATE_UNLOCK, WTSActive, WTSFreeMemory, WTSINFOEXW, WTSQuerySessionInformationW, WTSSessionInfoEx,
    };
    unsafe {
        let mut buffer = windows::core::PWSTR::null();
        let mut bytes = 0u32;
        WTSQuerySessionInformationW(None, WTS_CURRENT_SESSION, WTSSessionInfoEx, &mut buffer, &mut bytes).ok()?;
        let result = (!buffer.is_null() && bytes as usize >= size_of::<WTSINFOEXW>()).then(|| {
            let info = &*(buffer.0 as *const WTSINFOEXW);
            let level = info.Data.WTSInfoExLevel1;
            let flags = level.SessionFlags as u32;
            (flags == WTS_SESSIONSTATE_LOCK || flags == WTS_SESSIONSTATE_UNLOCK).then_some((flags == WTS_SESSIONSTATE_LOCK, level.SessionState != WTSActive))
        });
        if !buffer.is_null() {
            WTSFreeMemory(buffer.0 as *mut c_void);
        }
        result.flatten()
    }
}

/// When the last keyboard or mouse input happened (a tick count), to tell whether someone has
/// been there since.
pub fn last_input_tick() -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe {
        let _ = GetLastInputInfo(&mut info);
    }
    info.dwTime
}

/// Asks Windows to tell the dock's window when the session locks or unlocks and when the
/// display turns off or on (it sends the display's current state straight away). Fail soft:
/// without these messages the dock just keeps polling as before.
pub fn watch_presence(hwnd: HWND) {
    unsafe {
        if let Err(e) = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
            log_warn!("no lock/unlock messages: {}", e.message());
        }
        if let Err(e) = RegisterPowerSettingNotification(HANDLE(hwnd.0), &GUID_CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE) {
            log_warn!("no display on/off messages: {}", e.message());
        }
    }
}

pub fn unwatch_presence(hwnd: HWND) {
    unsafe {
        let _ = WTSUnRegisterSessionNotification(hwnd);
    }
}

/// The display state carried by a WM_POWERBROADCAST / PBT_POWERSETTINGCHANGE message.
///
/// # Safety
/// `lparam` must be that message's lParam.
pub unsafe fn display_state(lparam: LPARAM) -> Option<u32> {
    let setting = unsafe { (lparam.0 as *const POWERBROADCAST_SETTING).as_ref()? };
    if setting.PowerSetting != GUID_CONSOLE_DISPLAY_STATE || setting.DataLength < 4 {
        return None;
    }
    Some(unsafe { std::ptr::read_unaligned(setting.Data.as_ptr() as *const u32) })
}

/// Windows' "Animation effects" setting (Settings → Accessibility → Visual effects).
pub fn animations_enabled() -> bool {
    let mut on: i32 = 1;
    let read = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut i32 as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    read.is_err() || on != 0
}

/// The message Explorer broadcasts when the taskbar is (re)created, e.g. after Explorer restarts.
pub fn taskbar_created_message() -> u32 {
    unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
}

/// A handle Windows signals when a file directly inside `dir` is written, created, renamed or
/// deleted. Re-arm it with FindNextChangeNotification after each signal.
pub fn watch_folder(dir: &std::path::Path) -> Option<HANDLE> {
    unsafe {
        FindFirstChangeNotificationW(&HSTRING::from(dir), false, FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME)
            .inspect_err(|e| log_warn!("can't watch {} for changes ({}); checking every second instead", dir.display(), e.message()))
            .ok()
    }
}

/// True when the dock runs as administrator: then everything it opens does too, and Windows
/// blocks drag and drop from ordinary windows onto it.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut size = 0u32;
        let read = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut TOKEN_ELEVATION as *mut c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        );
        let _ = CloseHandle(token);
        read.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// This process's private memory in MB (what Task Manager calls "Memory").
pub fn private_mb() -> f64 {
    let mut counters = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    let ok = unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS, counters.cb)
    }
    .is_ok();
    if ok { counters.PrivateUsage as f64 / (1024.0 * 1024.0) } else { 0.0 }
}

/// For tests: this process's heaps (how many; MB handed out and MB committed) and its threads.
/// Allocated rising means something is kept; committed rising with allocated flat means the
/// heap is holding freed space (fragmentation), not a leak.
pub fn heap_and_threads() -> String {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next};
    use windows::Win32::System::Memory::{GetProcessHeaps, HEAP_SUMMARY, HeapSummary};
    let mb = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    let mut heaps = [HANDLE::default(); 64];
    let count = (unsafe { GetProcessHeaps(&mut heaps) } as usize).min(heaps.len());
    let (mut allocated, mut committed, mut process_allocated, mut process_committed) = (0, 0, 0, 0);
    let process_heap = unsafe { windows::Win32::System::Memory::GetProcessHeap() }.ok();
    for &heap in &heaps[..count] {
        let mut summary = HEAP_SUMMARY { cb: size_of::<HEAP_SUMMARY>() as u32, ..Default::default() };
        if unsafe { HeapSummary(heap, 0, &mut summary) }.as_bool() {
            allocated += summary.cbAllocated;
            committed += summary.cbCommitted;
            if Some(heap) == process_heap {
                (process_allocated, process_committed) = (summary.cbAllocated, summary.cbCommitted);
            }
        }
    }
    let me = std::process::id();
    let mut threads = 0;
    unsafe {
        if let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) {
            let mut entry = THREADENTRY32 { dwSize: size_of::<THREADENTRY32>() as u32, ..Default::default() };
            let mut more = Thread32First(snapshot, &mut entry).is_ok();
            while more {
                threads += usize::from(entry.th32OwnerProcessID == me);
                more = Thread32Next(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
        }
    }
    format!(
        "heaps={count} heap_alloc_mb={:.2} heap_commit_mb={:.2} main_heap_alloc_mb={:.2} main_heap_commit_mb={:.2} threads={threads}",
        mb(allocated), mb(committed), mb(process_allocated), mb(process_committed)
    )
}

/// What this process is using now, for the hourly log line: private memory, handles, GDI and
/// USER objects.
pub fn health() -> String {
    use windows::Win32::System::Threading::{GR_GDIOBJECTS, GR_USEROBJECTS, GetGuiResources, GetProcessHandleCount};
    let mut handles = 0u32;
    unsafe {
        let process = GetCurrentProcess();
        let _ = GetProcessHandleCount(process, &mut handles);
        let gdi = GetGuiResources(process, GR_GDIOBJECTS);
        let user = GetGuiResources(process, GR_USEROBJECTS);
        format!("{:.1} MB private, {handles} handles, {gdi} GDI and {user} USER objects", private_mb())
    }
}

/// Asks Windows to hand back what the process's heaps hold but no longer use: their caches
/// and free space (`HeapOptimizeResources`, Windows 8.1 and later; about 50 microseconds).
/// Drawing frees and reuses memory in many sizes, and without this the heaps kept growing
/// with use: about +2 MB for every few minutes of pointing along the dock, measured live, and
/// flat with it (`--hover-churn` shows it).
pub fn give_back_memory() {
    use windows::Win32::System::Memory::{HeapOptimizeResources, HeapSetInformation};
    #[repr(C)]
    struct OptimizeResources {
        version: u32,
        flags: u32,
    }
    let info = OptimizeResources { version: 1, flags: 0 }; // HEAP_OPTIMIZE_RESOURCES_CURRENT_VERSION
    unsafe {
        let _ = HeapSetInformation(None, HeapOptimizeResources, Some(&info as *const OptimizeResources as *const c_void), size_of::<OptimizeResources>());
    }
}

/// What the running dock is using now, in MB of private memory: the dock, and its watchdog if
/// there is one (for the settings window's About page). None if no dock is running.
pub fn dock_memory() -> Option<(f64, Option<f64>)> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    let private_of = |pid: u32| -> Option<f64> {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid).ok()?;
            let mut counters = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
            let ok = GetProcessMemoryInfo(process, &mut counters as *mut _ as *mut PROCESS_MEMORY_COUNTERS, counters.cb).is_ok();
            let _ = CloseHandle(process);
            ok.then_some(counters.PrivateUsage as f64 / (1024.0 * 1024.0))
        }
    };
    unsafe {
        let dock = FindWindowW(w!("DesktopDock"), w!("Desktop Dock")).ok()?;
        let mut pid = 0u32;
        GetWindowThreadProcessId(dock, Some(&mut pid));
        let mine = private_of(pid)?;
        // Its watchdog: the desktop-dock.exe that started it.
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok();
        let mut processes = Vec::new();
        if let Some(snapshot) = snapshot {
            let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
            let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
            while more {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                processes.push((entry.th32ProcessID, entry.th32ParentProcessID, String::from_utf16_lossy(&entry.szExeFile[..len])));
                more = Process32NextW(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
        }
        let parent = processes.iter().find(|(id, _, _)| *id == pid).map(|(_, parent, _)| *parent);
        let watchdog = processes
            .iter()
            .find(|(id, _, exe)| Some(*id) == parent && exe.eq_ignore_ascii_case("desktop-dock.exe"))
            .and_then(|(id, _, _)| private_of(*id));
        Some((mine, watchdog))
    }
}

const SINGLE_INSTANCE: PCWSTR = w!("Local\\DesktopDock.SingleInstance");

/// The dock under its watchdog holds the single-instance lock too, so it stays taken while
/// either runs: a dock whose watchdog was ended (Task Manager, say) still counts as running, and
/// starting it again opens its settings rather than a second dock.
pub fn hold_single_instance() {
    unsafe {
        // The handle is intentionally kept open for the life of the process.
        let _ = CreateMutexW(None, false, SINGLE_INSTANCE);
    }
}

/// Returns false if another copy of the dock is already running.
pub fn claim_single_instance() -> bool {
    unsafe {
        match CreateMutexW(None, true, SINGLE_INSTANCE) {
            // The handle is intentionally kept open for the life of the process.
            Ok(_) => GetLastError() != ERROR_ALREADY_EXISTS,
            Err(_) => true,
        }
    }
}

/// A session-wide lock that one process at a time holds, until it ends. One that ended without
/// letting go counts as let go.
pub struct ProcessLock(HANDLE);

impl ProcessLock {
    pub fn new(name: &str) -> Option<ProcessLock> {
        // The handle is intentionally kept open for the life of the process.
        unsafe { CreateMutexW(None, false, &HSTRING::from(name)).ok().map(ProcessLock) }
    }

    /// Takes it for the rest of this process, waiting up to `wait` for another to let go; false
    /// if another still has it.
    pub fn take(&self, wait: std::time::Duration) -> bool {
        let ms = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
        unsafe { matches!(WaitForSingleObject(self.0, ms), WAIT_OBJECT_0 | WAIT_ABANDONED) }
    }
}

pub fn cursor_pos() -> Option<POINT> {
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt).ok().map(|_| pt) }
}

/// A window is being dragged by its title bar or resized, for example toward Snap at the top
/// edge. Windows reports this reliably, while file and text drags don't set it.
pub fn window_being_moved() -> bool {
    unsafe {
        let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        GetGUIThreadInfo(0, &mut info).is_ok() && (info.flags.0 & GUI_INMOVESIZE.0) != 0
    }
}
/// Sends files to the Recycle Bin the way Explorer's Delete does: restorable, and if a file
/// can't go there (some network drives) Windows warns before deleting it. Runs in the helper
/// process (`--recycle`), so this machinery stays out of the dock. 0 means done.
pub fn recycle(paths: &[String]) -> i32 {
    // A list of paths, each ending in a null, and one more at the very end.
    let mut list: Vec<u16> = Vec::new();
    for path in paths.iter().filter(|p| !p.trim().is_empty()) {
        list.extend(path.encode_utf16());
        list.push(0);
    }
    if list.is_empty() {
        return 0;
    }
    list.push(0);
    let mut operation = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(list.as_ptr()),
        fFlags: (FOF_ALLOWUNDO.0 | FOF_WANTNUKEWARNING.0) as u16,
        ..Default::default()
    };
    unsafe { SHFileOperationW(&mut operation) }
}


/// Esc is down, or was tapped since the last look (a quick tap can fall between two of the
/// dock's looks). Call `forget_escape` first when a look starts mattering, so an Esc pressed long
/// ago elsewhere doesn't count.
pub fn escape_pressed() -> bool {
    unsafe { (GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8001) != 0 }
}

pub fn forget_escape() {
    unsafe {
        GetAsyncKeyState(VK_ESCAPE.0 as i32);
    }
}

/// The primary button (left, or right if they're swapped in Windows' mouse settings) is down.
pub fn primary_button_down() -> bool {
    let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
    let key = if swapped { VK_RBUTTON } else { VK_LBUTTON };
    unsafe { (GetAsyncKeyState(key.0 as i32) as u16 & 0x8000) != 0 }
}

pub fn mouse_buttons_down() -> bool {
    [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON]
        .iter()
        .any(|key| unsafe { (GetAsyncKeyState(key.0 as i32) as u16 & 0x8000) != 0 })
}

/// The primary monitor's full rectangle (not the work area: the dock overlays) and its DPI.
pub fn primary_monitor() -> (RECT, u32) {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_err() {
            dpi_x = 96;
        }
        (info.rcMonitor, dpi_x)
    }
}

/// A screen as the dock sees it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Screen {
    /// Stays the same across restarts and when screens are plugged in again: the monitor's
    /// device path, or Windows' display name if there's none.
    pub id: String,
    /// Windows' number for it, as in Settings > System > Display ("1", "2", …).
    pub number: String,
    /// The whole screen, and the part not taken by a taskbar or other app bars.
    pub monitor: RECT,
    pub work: RECT,
    pub dpi: u32,
    pub main: bool,
}

/// Every screen, the main one first.
pub fn screens() -> Vec<Screen> {
    use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, HDC, HMONITOR, MONITORINFOEXW};
    use windows::Win32::UI::WindowsAndMessaging::{EDD_GET_DEVICE_INTERFACE_NAME, MONITORINFOF_PRIMARY};
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, found: LPARAM) -> windows::core::BOOL {
        unsafe { (*(found.0 as *mut Vec<HMONITOR>)).push(monitor) };
        windows::core::BOOL(1)
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut handles as *mut Vec<HMONITOR> as isize));
    }
    let text = |wide: &[u16]| String::from_utf16_lossy(&wide[..wide.iter().position(|&c| c == 0).unwrap_or(wide.len())]);
    let mut found: Vec<Screen> = handles
        .into_iter()
        .filter_map(|handle| unsafe {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            if !GetMonitorInfoW(handle, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
                return None;
            }
            let device = text(&info.szDevice);
            let mut display = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
            let path = EnumDisplayDevicesW(&HSTRING::from(device.as_str()), 0, &mut display, EDD_GET_DEVICE_INTERFACE_NAME)
                .as_bool()
                .then(|| text(&display.DeviceID))
                .filter(|path| !path.is_empty());
            let (mut dpi, mut dpi_y) = (96u32, 96u32);
            if GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_err() {
                dpi = 96;
            }
            Some(Screen {
                id: path.unwrap_or_else(|| device.clone()),
                number: device.trim_start_matches(r"\\.\DISPLAY").to_string(),
                monitor: info.monitorInfo.rcMonitor,
                work: info.monitorInfo.rcWork,
                dpi,
                main: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            })
        })
        .collect();
    found.sort_by_key(|screen| !screen.main);
    found
}

/// Which screen the dock goes on for the `monitor` setting: "main" (or nothing) is Windows'
/// main screen; anything else is a screen's id. A screen that isn't there gives the main one;
/// the setting is kept, so the dock goes back when that screen is plugged in again.
pub fn choose_screen<'a>(screens: &'a [Screen], wanted: &str) -> Option<&'a Screen> {
    let wanted = wanted.trim();
    let main = screens.iter().find(|screen| screen.main).or(screens.first());
    if wanted.is_empty() || wanted.eq_ignore_ascii_case("main") {
        return main;
    }
    screens.iter().find(|screen| screen.id.eq_ignore_ascii_case(wanted)).or(main)
}

/// The dock's screen now: the chosen one if it's there, otherwise the main one, otherwise
/// (Windows can't say) the screen at 0,0.
pub fn dock_screen(wanted: &str) -> Screen {
    if let Some(screen) = choose_screen(&screens(), wanted) {
        return screen.clone();
    }
    let (monitor, dpi) = primary_monitor();
    Screen { id: String::new(), number: "1".into(), monitor, work: monitor, dpi, main: true }
}

/// A program's name as it gives it ("Google Chrome" for chrome.exe), from its version resource.
pub fn program_name(file: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
    unsafe {
        let name = HSTRING::from(file);
        let size = GetFileVersionInfoSizeW(&name, None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(&name, None, size, data.as_mut_ptr() as *mut core::ffi::c_void).ok()?;
        let query = |path: &str| -> Option<(*const u16, usize)> {
            let mut at: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut len = 0u32;
            let found = VerQueryValueW(data.as_ptr() as *const core::ffi::c_void, &HSTRING::from(path), &mut at, &mut len).as_bool();
            (found && !at.is_null() && len > 0).then_some((at as *const u16, len as usize))
        };
        // The file's own language and code page first, then US English as most programs have.
        let mut tables = Vec::new();
        if let Some((at, bytes)) = query(r"\VarFileInfo\Translation") {
            // Language and code page pairs, 4 bytes each (its length here is in bytes).
            let pairs = std::slice::from_raw_parts(at, bytes.min(64) / 4 * 2);
            tables.extend(pairs.chunks_exact(2).map(|pair| format!("{:04x}{:04x}", pair[0], pair[1])));
        }
        tables.extend(["040904b0".to_string(), "040904e4".to_string()]);
        tables.iter().find_map(|table| {
            let (at, len) = query(&format!(r"\StringFileInfo\{table}\FileDescription"))?;
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(at, len));
            let text = text.trim_end_matches('\0').trim().to_string();
            (!text.is_empty()).then_some(text)
        })
    }
}

/// A text file's contents, whatever it was saved as: UTF-16 or UTF-8 (with or without a
/// byte-order mark), or else this PC's own code page (older programs save `.url` files and
/// REGEDIT4 exports that way).
pub fn text_from_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let wide: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&wide);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    use windows::Win32::Globalization::{CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar};
    unsafe {
        let needed = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
        if needed <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut wide));
        String::from_utf16_lossy(&wide[..written.max(0) as usize])
    }
}

/// The user's locale name ("en-US", "de-DE", …), for DirectWrite; "en-us" if Windows can't say.
pub fn locale_name() -> HSTRING {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut name = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    let len = unsafe { GetUserDefaultLocaleName(&mut name) };
    if len > 1 { HSTRING::from_wide(&name[..len as usize - 1]) } else { HSTRING::from("en-us") }
}

/// What to know about reaching the dock at the top of this screen (Settings > Screen).
pub fn top_edge_note(screen: &Screen) -> Option<&'static str> {
    if monitor_above(screen.monitor) {
        return Some(
            "Another screen sits right above this one, so the pointer slides past the top edge instead of stopping there: \
             the dock is hard to bring out with the mouse. Choose another screen, or set a keyboard shortcut below.",
        );
    }
    if auto_hide_bar_on_top(screen.monitor) {
        return Some("The taskbar also hides along the top of this screen, so the two share the edge. A keyboard shortcut (below) brings out just the dock.");
    }
    if screen.work.top > screen.monitor.top {
        return Some("The taskbar is at the top of this screen: push the pointer against the top edge as usual, and the dock comes out just below the taskbar.");
    }
    None
}

/// A taskbar (or other bar) that hides itself along this screen's top edge.
fn auto_hide_bar_on_top(monitor: RECT) -> bool {
    use windows::Win32::UI::Shell::{ABE_TOP, ABM_GETAUTOHIDEBAREX, APPBARDATA, SHAppBarMessage};
    let mut data = APPBARDATA { cbSize: size_of::<APPBARDATA>() as u32, uEdge: ABE_TOP, rc: monitor, ..Default::default() };
    unsafe { SHAppBarMessage(ABM_GETAUTOHIDEBAREX, &mut data) != 0 }
}

/// True if another monitor touches this one's top edge (anywhere along it).
pub fn monitor_above(monitor: RECT) -> bool {
    let width = monitor.right - monitor.left;
    (1..8).any(|i| unsafe {
        let x = monitor.left + width * i / 8;
        !MonitorFromPoint(POINT { x, y: monitor.top - 1 }, MONITOR_DEFAULTTONULL).is_invalid()
    })
}

/// True when a fullscreen game, video or presentation is in front on this monitor.
/// Windows' own signal catches exclusive fullscreen; the rectangle check catches
/// borderless-window games and fullscreen video, which the signal can miss.
pub fn fullscreen_active(monitor: RECT) -> bool {
    unsafe {
        if let Ok(state) = SHQueryUserNotificationState() {
            if state == QUNS_BUSY || state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
                return true;
            }
        }
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return false;
        }
        let mut class = [0u16; 64];
        let len = GetClassNameW(window, &mut class).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(window, &mut rect).is_err() {
            return false;
        }
        let covers = rect.left <= monitor.left
            && rect.top <= monitor.top
            && rect.right >= monitor.right
            && rect.bottom >= monitor.bottom;
        // A maximized window can also cover the monitor when the taskbar auto-hides;
        // that's not fullscreen.
        covers && !IsZoomed(window).as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_leave_out_windows_error_numbers() {
        assert_eq!(
            plain("Your dock's file can't be opened right now: The process cannot access the file because it is being used by another process. (os error 32)"),
            "Your dock's file can't be opened right now: The process cannot access the file because it is being used by another process."
        );
        assert_eq!(plain("Couldn't open it: Access is denied. (0x80070005)."), "Couldn't open it: Access is denied.");
        assert_eq!(plain("Icon size (in pixels) stays (os error x) as is"), "Icon size (in pixels) stays (os error x) as is");
        assert_eq!(plain("unclosed (os error 5"), "unclosed (os error 5");
    }

    fn screen(id: &str, main: bool, left: i32) -> Screen {
        let rect = RECT { left, top: 0, right: left + 1920, bottom: 1080 };
        Screen { id: id.into(), number: id.into(), monitor: rect, work: rect, dpi: 96, main }
    }

    #[test]
    fn text_files_read_right_however_they_were_saved() {
        let text = "Caf\u{e9} [InternetShortcut]";
        assert_eq!(text_from_bytes(text.as_bytes()), text, "UTF-8");
        let with_bom: Vec<u8> = [0xEF, 0xBB, 0xBF].into_iter().chain(text.bytes()).collect();
        assert_eq!(text_from_bytes(&with_bom), text, "UTF-8 with a byte-order mark");
        let utf16: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(text_from_bytes(&utf16), text, "UTF-16");
        // "Café" in Western European ANSI (0xE9 for é) isn't valid UTF-8: the code page decodes
        // it. On a PC set to Western European that's "Café"; elsewhere it's still readable text.
        let ansi = b"Caf\xe9 [InternetShortcut]";
        let decoded = text_from_bytes(ansi);
        assert!(decoded.starts_with("Caf") && decoded.ends_with("[InternetShortcut]") && !decoded.contains('\u{fffd}'), "{decoded}");
    }

    #[test]
    fn the_dock_goes_on_the_chosen_screen_or_else_the_main_one() {
        let screens = [screen(r"\\?\DISPLAY#GSM5B09#1", true, 0), screen(r"\\?\DISPLAY#DEL40A3#2", false, 1920)];
        assert_eq!(choose_screen(&screens, "main").unwrap().left_id(), "GSM", "main");
        assert_eq!(choose_screen(&screens, "").unwrap().left_id(), "GSM", "nothing set: main");
        assert_eq!(choose_screen(&screens, r"\\?\display#del40a3#2").unwrap().left_id(), "DEL", "by id, any case");
        assert_eq!(choose_screen(&screens, r"\\?\DISPLAY#ACR0000#9").unwrap().left_id(), "GSM", "unplugged: main");
        assert!(choose_screen(&[], "main").is_none());
        let no_main = [screen("a", false, 0)];
        assert_eq!(choose_screen(&no_main, "main").unwrap().id, "a", "Windows names no main screen: the first");
    }

    impl Screen {
        fn left_id(&self) -> &str {
            &self.id[self.id.find('#').map_or(0, |i| i + 1)..][..3]
        }
    }

    #[test]
    fn this_pc_has_a_main_screen_with_a_work_area_inside_it() {
        let screens = screens();
        assert!(!screens.is_empty());
        let main = &screens[0];
        assert!(main.main, "the main screen comes first: {screens:?}");
        assert!(main.work.left >= main.monitor.left && main.work.top >= main.monitor.top);
        assert!(main.work.right <= main.monitor.right && main.work.bottom <= main.monitor.bottom);
        assert!(main.dpi >= 96);
    }

    #[test]
    fn presence_is_corrected_from_a_direct_look() {
        let mut presence = Presence::default();
        presence.session_event(WTS_SESSION_LOCK);
        presence.display_state(0);
        assert!(presence.away());
        presence.correct(Some((true, false)), true);
        assert!(presence.away(), "still locked: input on the lock screen doesn't count");
        presence.correct(Some((false, false)), false);
        assert!(presence.away(), "unlocked, but the display is off and nobody has touched anything");
        presence.correct(Some((false, false)), true);
        assert!(!presence.away(), "unlocked and someone moved the mouse: back");
        presence.display_state(0);
        presence.correct(None, true);
        assert!(!presence.away(), "Windows can't say about the session: input alone brings it back");
    }

    #[test]
    fn locking_pauses_and_unlocking_resumes() {
        let mut presence = Presence::default();
        assert!(!presence.away());
        presence.session_event(WTS_SESSION_LOCK);
        assert!(presence.away());
        presence.session_event(WTS_SESSION_UNLOCK);
        assert!(!presence.away());
    }

    #[test]
    fn display_off_pauses_but_dimmed_does_not() {
        let mut presence = Presence::default();
        presence.display_state(2);
        assert!(!presence.away(), "dimmed is still visible");
        presence.display_state(0);
        assert!(presence.away());
        presence.display_state(1);
        assert!(!presence.away());
    }

    #[test]
    fn every_reason_must_clear_before_resuming() {
        let mut presence = Presence::default();
        presence.session_event(WTS_SESSION_LOCK);
        presence.display_state(0);
        presence.display_state(1);
        assert!(presence.away(), "display back on, but still locked");
        presence.session_event(WTS_SESSION_UNLOCK);
        assert!(!presence.away());
    }

    #[test]
    fn switching_users_or_remote_desktop_away_and_back() {
        let mut presence = Presence::default();
        presence.session_event(WTS_CONSOLE_DISCONNECT);
        assert!(presence.away());
        presence.session_event(WTS_REMOTE_CONNECT);
        assert!(!presence.away(), "now on a remote screen");
        presence.session_event(WTS_REMOTE_DISCONNECT);
        presence.session_event(WTS_CONSOLE_CONNECT);
        assert!(!presence.away(), "back at the computer");
    }
}
