//! Crash recovery: `desktop-dock.exe` starts as a tiny watchdog that runs the real dock as a
//! child process (`--child`) and waits. If the dock crashes, it's restarted about 0.1 s after the
//! crashed process ends (Windows Error Reporting first holds it for ~2 s to write its report).
//! A normal exit (menu → Exit) or being ended in Task Manager is respected.
//!
//! Limits: more than 3 crashes in 10 minutes → stop and say so. Two crashes in a row within
//! 15 s of starting → next start in safe mode (cached icons only, no repairs, no shell checks).
//!
//! A dock that stops responding is treated as a crash: every 15 s the watchdog asks the dock's
//! window for a sign of life (WM_NULL, which is answered even while a menu, a message box or a
//! drag is open). No answer three times in a row (within a minute), and the dock is ended and
//! restarted. Not while a debugger holds it, and not straight after the PC wakes up.

use crate::{log_error, log_info, log_warn, system};
use std::collections::VecDeque;
use std::os::windows::io::AsRawHandle;
use std::process::{Child, Command};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WAIT_TIMEOUT, WPARAM};
use windows::Win32::System::Diagnostics::Debug::CheckRemoteDebuggerPresent;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_NULL};
use windows::core::BOOL;

const WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_CRASHES: usize = 3;
const QUICK: Duration = Duration::from_secs(15);

/// How often the watchdog asks the dock for a sign of life.
const PROBE_EVERY: Duration = Duration::from_secs(15);
/// How long one ask waits for the answer, in ms.
const PROBE_WAIT_MS: u32 = 5000;
/// Unanswered asks in a row before the dock counts as not responding.
const HUNG_AFTER: u32 = 3;
/// A dock with no window this long after starting is stuck too.
const NO_WINDOW_LIMIT: Duration = Duration::from_secs(90);
/// A hang found this soon after starting counts as a quick crash (finding it takes a minute).
const EARLY_HANG: Duration = Duration::from_secs(3 * 60);
/// The exit code given to a dock ended for not responding: in the crash range, so the crash
/// rules apply ("DD" for Desktop Dock).
pub const HUNG_CODE: u32 = 0xC0DD_0001;

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// The dock exited on purpose (or was ended by the user): stop too.
    Stop,
    Restart { safe_mode: bool },
    /// Crashing repeatedly: stop restarting.
    GiveUp,
}

/// Windows exit codes 0xC000_0000 and up are crashes (access violation, Rust's abort, stack
/// overflow…). 0 is "Exit" from the menu; 1 is Task Manager's End task or a reported error.
pub fn is_crash(code: u32) -> bool {
    code >= 0xC000_0000
}

#[derive(Debug, Default)]
pub struct Policy {
    crashes: VecDeque<Instant>,
    quick_crashes: u32,
}

impl Policy {
    pub fn on_exit(&mut self, code: u32, ran_for: Duration, now: Instant) -> Decision {
        if !is_crash(code) {
            return Decision::Stop;
        }
        self.crashes.push_back(now);
        while self.crashes.front().is_some_and(|&t| now.duration_since(t) > WINDOW) {
            self.crashes.pop_front();
        }
        if self.crashes.len() > MAX_CRASHES {
            return Decision::GiveUp;
        }
        let quick = ran_for < QUICK || (code == HUNG_CODE && ran_for < EARLY_HANG);
        self.quick_crashes = if quick { self.quick_crashes + 1 } else { 0 };
        Decision::Restart { safe_mode: self.quick_crashes >= 2 }
    }
}

/// Counts unanswered asks in a row.
#[derive(Debug, Default)]
pub struct Liveness {
    misses: u32,
}

impl Liveness {
    /// Records one ask; true once the dock counts as not responding.
    pub fn record(&mut self, answered: bool) -> bool {
        self.misses = if answered { 0 } else { self.misses + 1 };
        self.misses >= HUNG_AFTER
    }

    /// Starts counting again (after the PC slept, or while a debugger holds the dock).
    pub fn reset(&mut self) {
        self.misses = 0;
    }
}

/// Runs the dock as a child and restarts it after crashes. `args` are passed through.
pub fn run(args: &[String]) -> ! {
    let Ok(exe) = std::env::current_exe() else { std::process::exit(1) };
    let mut policy = Policy::default();
    let mut safe_mode = false;
    loop {
        let started = Instant::now();
        let mut command = Command::new(&exe);
        command.args(args).arg("--child");
        if safe_mode {
            command.arg("--safe-mode");
        }
        let code = match command.spawn() {
            Ok(mut child) => watch(&mut child, started),
            Err(e) => {
                log_error!("watchdog couldn't start the dock: {e}");
                std::process::exit(1);
            }
        };
        match policy.on_exit(code, started.elapsed(), Instant::now()) {
            Decision::Stop => {
                if code != 0 {
                    log_info!("dock ended (exit code {code}); not restarting");
                }
                std::process::exit(code as i32);
            }
            Decision::GiveUp => {
                log_error!("dock crashed {} times in 10 minutes; stopped restarting it", MAX_CRASHES + 1);
                system::notice_blocking(
                    "Desktop Dock crashed several times in a few minutes, so it won't restart by itself.\n\n\
                     The details are in the dock folder's logs\\dock.log. Start it again from the Start \
                     menu when you're ready."
                        .into(),
                );
                std::process::exit(1);
            }
            Decision::Restart { safe_mode: safe } => {
                if code == HUNG_CODE {
                    log_warn!("dock stopped responding after {} s; restarting", started.elapsed().as_secs());
                } else {
                    log_warn!("dock crashed (code {code:#010x}) after {} s; restarting", started.elapsed().as_secs());
                }
                if safe && !safe_mode {
                    log_warn!("two quick crashes in a row: restarting in safe mode");
                    system::notice(
                        "Desktop Dock crashed twice while starting, so it's running in safe mode: icons come \
                         only from its cache, and repairs and Recycle Bin checks are off.\n\nTo leave safe \
                         mode, right-click it > Exit Desktop Dock, then start it again from the Start menu."
                            .into(),
                    );
                }
                safe_mode = safe_mode || safe;
                // A short pause so a crash at startup can't spin; the crash limit does the rest.
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Waits for the dock to end, asking it for a sign of life every 15 s. Returns its exit code; a
/// dock that stopped responding is ended with `HUNG_CODE`.
fn watch(child: &mut Child, started: Instant) -> u32 {
    let process = HANDLE(child.as_raw_handle());
    let mut liveness = Liveness::default();
    let mut window_seen = false;
    let mut last_tick = unsafe { GetTickCount64() };
    loop {
        if unsafe { WaitForSingleObject(process, PROBE_EVERY.as_millis() as u32) } != WAIT_TIMEOUT {
            break; // it ended (or the wait failed: then just wait for it below)
        }
        // The tick count runs on while the PC sleeps; the wait doesn't. A jump means it slept,
        // and the dock gets time to catch up before it's judged.
        let tick = unsafe { GetTickCount64() };
        let slept = tick.saturating_sub(last_tick) > (PROBE_EVERY + Duration::from_secs(10)).as_millis() as u64;
        last_tick = tick;
        if slept || debugger_attached(process) {
            liveness.reset();
            continue;
        }
        let answered = match dock_window(child.id()) {
            Some(window) => {
                window_seen = true;
                responds(window)
            }
            // Gone after being there: it's closing, and the exit shows how.
            None if window_seen => true,
            None => started.elapsed() < NO_WINDOW_LIMIT,
        };
        if liveness.record(answered) {
            log_error!("the dock stopped responding (no answer three times in a row); ending it to start it again");
            unsafe {
                let _ = TerminateProcess(process, HUNG_CODE);
            }
            break;
        }
        if !answered {
            log_warn!("the dock didn't answer (watching)");
        }
    }
    match child.wait() {
        Ok(status) => status.code().unwrap_or(-1) as u32,
        Err(_) => u32::MAX,
    }
}

/// True if the window answers WM_NULL within 5 s.
fn responds(window: HWND) -> bool {
    let mut result = 0usize;
    unsafe { SendMessageTimeoutW(window, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, PROBE_WAIT_MS, Some(&mut result)) }.0 != 0
}

/// The dock's main window (class `DesktopDock`) in process `pid`, shown or hidden.
fn dock_window(pid: u32) -> Option<HWND> {
    struct Search {
        pid: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(window: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
        if owner == search.pid {
            let mut class = [0u16; 32];
            let n = unsafe { GetClassNameW(window, &mut class) } as usize;
            if class[..n] == *"DesktopDock".encode_utf16().collect::<Vec<u16>>() {
                search.found = Some(window);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut search = Search { pid, found: None };
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize));
    }
    search.found
}

fn debugger_attached(process: HANDLE) -> bool {
    let mut present = BOOL(0);
    unsafe { CheckRemoteDebuggerPresent(process, &mut present) }.is_ok() && present.as_bool()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABORT: u32 = 0xC000_0409; // Rust's abort / fast-fail
    const ACCESS_VIOLATION: u32 = 0xC000_0005;
    const LONG: Duration = Duration::from_secs(3600);

    #[test]
    fn exit_and_end_task_are_respected() {
        let mut policy = Policy::default();
        assert_eq!(policy.on_exit(0, LONG, Instant::now()), Decision::Stop, "menu → Exit");
        assert_eq!(policy.on_exit(1, LONG, Instant::now()), Decision::Stop, "Task Manager → End task");
    }

    #[test]
    fn a_crash_is_restarted() {
        let mut policy = Policy::default();
        assert_eq!(policy.on_exit(ABORT, LONG, Instant::now()), Decision::Restart { safe_mode: false });
        assert_eq!(policy.on_exit(ACCESS_VIOLATION, LONG, Instant::now()), Decision::Restart { safe_mode: false });
    }

    #[test]
    fn more_than_three_crashes_in_ten_minutes_gives_up() {
        let mut policy = Policy::default();
        let t = Instant::now();
        for i in 0..3 {
            assert!(matches!(policy.on_exit(ABORT, LONG, t + Duration::from_secs(i)), Decision::Restart { .. }));
        }
        assert_eq!(policy.on_exit(ABORT, LONG, t + Duration::from_secs(4)), Decision::GiveUp);
    }

    #[test]
    fn crashes_spread_out_over_time_keep_being_restarted() {
        let mut policy = Policy::default();
        let t = Instant::now();
        for hour in 0..10 {
            let at = t + Duration::from_secs(hour * 3600);
            assert!(matches!(policy.on_exit(ABORT, LONG, at), Decision::Restart { .. }), "hour {hour}");
        }
    }

    #[test]
    fn two_quick_crashes_switch_to_safe_mode() {
        let mut policy = Policy::default();
        let t = Instant::now();
        let quick = Duration::from_secs(2);
        assert_eq!(policy.on_exit(ABORT, quick, t), Decision::Restart { safe_mode: false });
        assert_eq!(policy.on_exit(ABORT, quick, t + Duration::from_secs(3)), Decision::Restart { safe_mode: true });
    }

    #[test]
    fn a_dock_not_responding_counts_after_three_unanswered_asks_in_a_row() {
        let mut liveness = Liveness::default();
        assert!(!liveness.record(false) && !liveness.record(false));
        assert!(!liveness.record(true), "an answer starts the count again");
        assert!(!liveness.record(false) && !liveness.record(false));
        assert!(liveness.record(false), "three in a row");
        liveness.reset();
        assert!(!liveness.record(false), "after the PC wakes up it starts again");
    }

    #[test]
    fn a_hang_is_restarted_like_a_crash_and_an_early_one_counts_as_quick() {
        assert!(is_crash(HUNG_CODE));
        let mut policy = Policy::default();
        let t = Instant::now();
        let early = Duration::from_secs(70); // hung at once; found after about a minute
        assert_eq!(policy.on_exit(HUNG_CODE, early, t), Decision::Restart { safe_mode: false });
        assert_eq!(policy.on_exit(HUNG_CODE, early, t + Duration::from_secs(80)), Decision::Restart { safe_mode: true });
        let mut policy = Policy::default();
        policy.on_exit(HUNG_CODE, early, t);
        assert_eq!(policy.on_exit(HUNG_CODE, LONG, t + Duration::from_secs(3700)), Decision::Restart { safe_mode: false }, "a hang after an hour isn't quick");
    }

    #[test]
    fn a_slow_crash_resets_the_quick_count() {
        let mut policy = Policy::default();
        let t = Instant::now();
        policy.on_exit(ABORT, Duration::from_secs(2), t);
        assert_eq!(policy.on_exit(ABORT, LONG, t + Duration::from_secs(3)), Decision::Restart { safe_mode: false });
    }
}
