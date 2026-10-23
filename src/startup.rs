//! Start with Windows: a sign-in task of your own in Task Scheduler, which Windows runs as you
//! sign in. Windows' startup list (the Run key) works too, but it starts its programs one at a
//! time, so on a PC with many of them the dock could take minutes to appear; the task runs at
//! once. It needs no administrator rights, and its name carries your user's id, so each person
//! on the PC has their own. Earlier versions used the Run key: an entry of theirs moves to a task.

use crate::{log_info, log_warn, system};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::TaskScheduler::{ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN, TaskScheduler};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, PWSTR};

/// True if the dock starts with Windows: its sign-in task is there, switched on and starting this
/// installed copy, or an entry of an earlier version in Windows' startup list is.
pub fn starts_with_windows() -> bool {
    let Ok(exe) = system::startup_exe() else { return false };
    let task = folder().and_then(|folder| unsafe {
        let task = folder.GetTask(&BSTR::from(task_name()?))?;
        Ok((task.Enabled()?.as_bool(), task.Xml()?.to_string()))
    });
    match task {
        Ok((enabled, xml)) => enabled && starts_program(&xml, &exe.to_string_lossy()),
        Err(_) => system::run_entry_is_on(),
    }
}

/// Switches Start with Windows on (the sign-in task made or brought up to date) or off (taken
/// away). Either way an entry of an earlier version in Windows' startup list goes.
pub fn set_starts_with_windows(on: bool) -> Result<(), String> {
    let exe = if on { Some(system::startup_exe()?) } else { None };
    let name = task_name().map_err(|_| "Your Windows user couldn't be found.".to_string())?;
    let folder = folder().map_err(|e| format!("Task Scheduler couldn't be reached ({}).", e.message()))?;
    unsafe {
        match &exe {
            Some(exe) => {
                let xml = task_xml(&exe.to_string_lossy(), &user_sid().map_err(|_| "Your Windows user couldn't be found.".to_string())?);
                let none = VARIANT::default();
                folder
                    .RegisterTask(&BSTR::from(&name), &BSTR::from(xml), TASK_CREATE_OR_UPDATE.0, &none, &none, TASK_LOGON_INTERACTIVE_TOKEN, &none)
                    .map_err(|e| format!("Windows didn't allow the sign-in task ({}).", e.message()))?;
            }
            None => {
                if let Err(e) = folder.DeleteTask(&BSTR::from(&name), 0) {
                    // Not there is fine: it's off either way.
                    if e.code().0 as u32 != 0x8007_0002 {
                        return Err(format!("Windows didn't allow the change ({}).", e.message()));
                    }
                }
            }
        }
    }
    system::remove_run_entry();
    Ok(())
}

/// An installed dock from before 1.0.1 started with Windows through Windows' startup list: as it
/// starts, it moves to a sign-in task (on stays on, off stays off). Nothing to do otherwise.
pub fn move_from_run_entry() {
    if !system::run_entry_exists() || system::startup_exe().is_err() {
        return;
    }
    let on = system::run_entry_is_on();
    match set_starts_with_windows(on) {
        Ok(()) => log_info!("Start with Windows now uses a sign-in task ({})", if on { "on" } else { "off, as it was" }),
        Err(e) => log_warn!("Start with Windows stays in Windows' startup list: {e}"),
    }
}

/// Task Scheduler's top folder, where a user without administrator rights can keep a task.
fn folder() -> windows::core::Result<ITaskFolder> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED); // (already done, on most threads)
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let none = VARIANT::default();
        service.Connect(&none, &none, &none, &none)?;
        service.GetFolder(&BSTR::from("\\"))
    }
}

/// "Desktop Dock (S-1-5-21-…)": one for each person on the PC.
fn task_name() -> windows::core::Result<String> {
    Ok(format!("Desktop Dock ({})", user_sid()?))
}

/// This Windows user's id ("S-1-5-21-…").
fn user_sid() -> windows::core::Result<String> {
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut buffer = vec![0u8; 256];
        let mut size = 0u32;
        let read = GetTokenInformation(token, TokenUser, Some(buffer.as_mut_ptr() as *mut _), buffer.len() as u32, &mut size);
        let _ = windows::Win32::Foundation::CloseHandle(token);
        read?;
        let user = &*(buffer.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text)?;
        let sid = text.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(text.0 as *mut _)));
        Ok(sid)
    }
}

/// The task: at this user's sign-in, start the dock with `--startup`, as this user, without
/// administrator rights. Not stopped after 3 days (Task Scheduler's own default), nor on
/// battery, and at normal priority (a task's default is below normal).
pub fn task_xml(exe: &str, sid: &str) -> String {
    let (exe, sid) = (escape(exe), escape(sid));
    let flag = system::STARTUP_FLAG;
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>Desktop Dock</Author>
    <Description>Starts Desktop Dock when you sign in (its Start with Windows setting).</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{sid}</UserId>
    </LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>{flag}</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

/// The task starts this program (as Task Scheduler gives its XML back).
fn starts_program(xml: &str, exe: &str) -> bool {
    xml.to_lowercase().contains(&format!("<command>{}</command>", escape(exe).to_lowercase()))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_starts_the_installed_dock_at_sign_in_for_good() {
        let xml = task_xml(r"C:\Users\alex\AppData\Local\Programs\Desktop Dock\desktop-dock.exe", "S-1-5-21-1-2-3-1001");
        assert!(xml.contains("<LogonTrigger>") && xml.contains("<UserId>S-1-5-21-1-2-3-1001</UserId>"), "at this user's sign-in");
        assert!(xml.contains("<Arguments>--startup</Arguments>"), "says it was started at sign-in");
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"), "no administrator rights");
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"), "not stopped after 3 days");
        assert!(xml.contains("<StopIfGoingOnBatteries>false") && xml.contains("<DisallowStartIfOnBatteries>false"), "on battery too");
        assert!(xml.contains("<Priority>5</Priority>"), "normal priority");
        assert!(starts_program(&xml, r"c:\users\alex\appdata\local\programs\desktop dock\DESKTOP-DOCK.exe"), "the same program, whatever the case");
        assert!(!starts_program(&xml, r"C:\Other\desktop-dock.exe"));
    }

    #[test]
    fn odd_characters_in_the_folder_are_written_safely() {
        let xml = task_xml(r"C:\Tom & Jerry's <apps>\desktop-dock.exe", "S-1-5-21-9");
        assert!(xml.contains(r"<Command>C:\Tom &amp; Jerry&apos;s &lt;apps&gt;\desktop-dock.exe</Command>"));
        assert!(starts_program(&xml, r"C:\Tom & Jerry's <apps>\desktop-dock.exe"));
    }
}
