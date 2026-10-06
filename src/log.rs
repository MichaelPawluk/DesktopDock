//! A small rotating log file: `logs\dock.log`, rolled over to `dock.old.log` at 256 KB, so
//! the two never take more than 512 KB. Release builds have no console, so this is the only
//! record of launch failures, config warnings, repairs and crashes.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{CloseHandle, HMODULE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_APPEND_DATA, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS, WriteFile,
};
use windows::Win32::System::Diagnostics::Debug::{EXCEPTION_POINTERS, RtlCaptureStackBackTrace, SetUnhandledExceptionFilter};
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::PCWSTR;

const MAX_BYTES: u64 = 256 * 1024;

/// The log file, once `init` has run.
static LOG: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The same path as UTF-16, made at `init` for the crash filter (which mustn't allocate).
static CRASH_PATH: OnceLock<Vec<u16>> = OnceLock::new();

/// Starts logging into `dir` (created if needed). Safe to call more than once.
pub fn init(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("dock.log");
    let _ = CRASH_PATH.set(path.as_os_str().encode_wide().chain(std::iter::once(0)).collect());
    if let Ok(mut log) = LOG.lock() {
        *log = Some(path);
    }
}

/// Logs a crash that isn't a panic (an access violation, say, inside Windows code the dock
/// calls): the exception code and where, as module+offset, so it can be matched to the code
/// with the build's .pdb. Then Windows carries on as usual (its error report; the watchdog
/// restarts the dock). Panics are logged by the panic hook instead.
pub fn install_crash_filter() {
    unsafe {
        SetUnhandledExceptionFilter(Some(on_crash));
    }
}

/// Written without allocating anything: the heap may be what broke.
unsafe extern "system" fn on_crash(info: *const EXCEPTION_POINTERS) -> i32 {
    use std::fmt::Write as _;
    let mut line = StackText { bytes: [0; 1024], len: 0 };
    unsafe {
        let Some(record) = info.as_ref().and_then(|info| info.ExceptionRecord.as_ref()) else { return 0 };
        let t = GetLocalTime();
        let _ = write!(
            line,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} ERROR crashed: exception {:#010x} at ",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds, record.ExceptionCode.0 as u32
        );
        let address = record.ExceptionAddress as usize;
        let mut module = HMODULE::default();
        let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        if GetModuleHandleExW(flags, PCWSTR(address as *const u16), &mut module).is_ok() {
            let mut name = [0u16; 260];
            let n = (GetModuleFileNameW(Some(module), &mut name) as usize).min(name.len());
            let file = name[..n].rsplit(|&c| c == u16::from(b'\\')).next().unwrap_or(&[]);
            for c in char::decode_utf16(file.iter().copied()) {
                let _ = line.write_char(c.unwrap_or('?'));
            }
            let _ = write!(line, "+{:#x}", address.wrapping_sub(module.0 as usize));
        } else {
            let _ = write!(line, "{address:#x}");
        }
        // The calls that led here within the dock's own code, as offsets for the build's .pdb.
        if let Ok(own) = GetModuleHandleW(None) {
            let _ = line.write_str("; dock calls:");
            let mut frames = [std::ptr::null_mut(); 48];
            let count = RtlCaptureStackBackTrace(0, &mut frames, None) as usize;
            let mut shown = 0;
            for &frame in &frames[..count.min(frames.len())] {
                let mut module = HMODULE::default();
                if shown < 16 && GetModuleHandleExW(flags, PCWSTR(frame as *const u16), &mut module).is_ok() && module == own {
                    let _ = write!(line, " +{:#x}", (frame as usize).wrapping_sub(own.0 as usize));
                    shown += 1;
                }
            }
        }
        let _ = line.write_str("\r\n");
        if let Some(path) = CRASH_PATH.get() {
            let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
            if let Ok(file) = CreateFileW(PCWSTR(path.as_ptr()), FILE_APPEND_DATA.0, share, None, OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, None) {
                let _ = WriteFile(file, Some(&line.bytes[..line.len]), None, None);
                let _ = CloseHandle(file);
            }
        }
    }
    0 // EXCEPTION_CONTINUE_SEARCH
}

/// A line built on the stack; anything past its end is dropped.
struct StackText {
    bytes: [u8; 1024],
    len: usize,
}

impl std::fmt::Write for StackText {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let take = s.len().min(self.bytes.len() - self.len);
        self.bytes[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// Logs a panic (with location) before the process aborts.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        write("ERROR", &format!("crashed: {info}"));
    }));
}

pub fn write(level: &str, message: &str) {
    let line = format!("{} {level:<5} {message}\r\n", timestamp());
    if cfg!(debug_assertions) {
        eprint!("{line}");
    }
    let Ok(guard) = LOG.lock() else { return };
    if let Some(path) = guard.as_ref() {
        append(path, &line);
    }
}

/// Opens the file, adds one line and closes it again. The dock, its watchdog and the settings
/// window all write here, and keeping it open would leave a process writing into the old file
/// after another one rolled it over (the watchdog runs for days and logs rarely).
fn append(path: &Path, line: &str) {
    let size = std::fs::metadata(path).map_or(0, |meta| meta.len());
    if size + line.len() as u64 > MAX_BYTES {
        rotate(path);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// One rename replaces the old file. If it can't happen just now (something is reading the
/// log without letting it be renamed), nothing is lost: this file carries on, and the next line
/// tries again. (Deleting the old file first would lose it whenever the rename then failed.)
fn rotate(path: &Path) {
    let _ = std::fs::rename(path, path.with_file_name("dock.old.log"));
}

fn timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log::write("INFO", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::log::write("WARN", &format!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::log::write("ERROR", &format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dock-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_log_rotates_and_never_grows_past_two_files() {
        let dir = temp_dir("rotate");
        let path = dir.join("dock.log");
        let line = format!("{}\r\n", "x".repeat(1000));
        for _ in 0..(3 * MAX_BYTES as usize / line.len()) {
            append(&path, &line);
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "dock.log and dock.old.log, never a third");
        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_BYTES);
        assert!(std::fs::metadata(dir.join("dock.old.log")).unwrap().len() <= MAX_BYTES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_writers_both_write_to_the_current_file_after_a_rollover() {
        let dir = temp_dir("writers");
        let path = dir.join("dock.log");
        std::fs::write(&path, "x".repeat(MAX_BYTES as usize - 5)).unwrap();
        append(&path, "first writer rolls it over\r\n");
        append(&path, "second writer\r\n");
        let now = std::fs::read_to_string(&path).unwrap();
        assert_eq!(now, "first writer rolls it over\r\nsecond writer\r\n", "both lines in the new file, none in the old");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reader_holding_the_log_loses_nothing() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = temp_dir("held");
        let path = dir.join("dock.log");
        std::fs::write(dir.join("dock.old.log"), "older\r\n").unwrap();
        std::fs::write(&path, "x".repeat(MAX_BYTES as usize - 5)).unwrap();
        // Like PowerShell's Get-Content or many viewers: reading, sharing read and write only.
        let reader = OpenOptions::new().read(true).share_mode(0x1 | 0x2).open(&path).unwrap();
        append(&path, "still logging\r\n");
        assert_eq!(std::fs::read_to_string(dir.join("dock.old.log")).unwrap(), "older\r\n", "the old file is kept");
        assert!(std::fs::read_to_string(&path).unwrap().ends_with("still logging\r\n"), "the line isn't lost");
        drop(reader);
        append(&path, "after\r\n");
        assert!(std::fs::read_to_string(dir.join("dock.old.log")).unwrap().contains("still logging"), "rolled over once the reader let go");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "after\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
