//! Windows' own choosers (an image file, a program's icons, files, a folder), shown by a helper
//! process: `desktop-dock.exe --pick <what> <result file> [start]`. File dialogs load Explorer's
//! add-ons and icon machinery into whatever process shows them, for good, and an add-on can hang
//! or crash; in a helper, none of that reaches the dock. The helper writes what was chosen to
//! the result file (one per line) and exits; the dock then makes the change itself, with its
//! usual Undo.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Pictures, FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
    FOS_PICKFOLDERS, FileOpenDialog, FileSaveDialog, IFileOpenDialog, IFileSaveDialog, IShellItem, KF_FLAG_DEFAULT, PickIconDlg,
    SHCreateItemFromParsingName, SHGetKnownFolderPath, SIGDN_FILESYSPATH,
};
use windows::Win32::Foundation::HWND;
use windows::core::{HSTRING, PCWSTR, w};

/// What to ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Files to add to the dock.
    Files,
    /// A folder to add to the dock.
    Folder,
}

impl Pick {
    fn word(self) -> &'static str {
        match self {
            Pick::Files => "files",
            Pick::Folder => "folder",
        }
    }

    fn from_word(word: &str) -> Option<Pick> {
        [Pick::Files, Pick::Folder].into_iter().find(|pick| pick.word() == word)
    }
}

// ---- The dock's side ---------------------------------------------------------------------------

/// Shows a chooser in a helper process and waits for it (call from a worker thread: you may take
/// your time choosing). Returns what was chosen; empty if cancelled.
pub fn ask(pick: Pick, start: &str) -> Vec<String> {
    let Ok(exe) = std::env::current_exe() else { return Vec::new() };
    let result = std::env::temp_dir().join(format!("desktop-dock-pick-{}-{}.txt", std::process::id(), unique()));
    let mut command = std::process::Command::new(exe);
    command.arg("--pick").arg(pick.word()).arg(&result);
    if !start.is_empty() {
        command.arg(start);
    }
    let chosen = match command.status() {
        Ok(status) if status.success() => std::fs::read_to_string(&result)
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let _ = std::fs::remove_file(&result);
    chosen
}

fn unique() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// ---- The helper's side -------------------------------------------------------------------------

/// `--pick <what> <result file> [start]`: shows the chooser, writes the choice, exit code 0 if
/// something was chosen.
pub fn run_helper(what: &str, result: &Path, start: Option<&str>) -> i32 {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    let Some(pick) = Pick::from_word(what) else { return 2 };
    let _ = start;
    let chosen: Vec<String> = match pick {
        Pick::Files => choose_files(None),
        Pick::Folder => choose_folder(None, "").into_iter().collect(),
    };
    if chosen.is_empty() || std::fs::write(result, chosen.join("\n")).is_err() {
        return 1;
    }
    0
}

/// Pictures\Icons, where your custom icons live (or Pictures if it's not there).
pub(crate) fn icons_folder() -> Option<PathBuf> {
    let pictures = known_folder(&FOLDERID_Pictures)?;
    let icons = pictures.join("Icons");
    Some(if icons.is_dir() { icons } else { pictures })
}

/// Pictures\Icons, if you have one: the icon picker's "Your icons".
pub(crate) fn own_icons_folder() -> Option<PathBuf> {
    known_folder(&FOLDERID_Pictures).map(|pictures| pictures.join("Icons")).filter(|icons| icons.is_dir())
}

fn known_folder(id: &windows::core::GUID) -> Option<PathBuf> {
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const c_void));
        text.map(PathBuf::from)
    }
}

/// A file of one kind to open (`kind`: what the filter says, `pattern`: like "*.toml").
pub fn choose_file(owner: Option<HWND>, title: &str, kind: &str, pattern: &str) -> Option<String> {
    let start = known_folder(&FOLDERID_Documents);
    let dialog = dialog(PCWSTR(HSTRING::from(title).as_ptr()), 0, start.as_deref())?;
    unsafe {
        let (kind, pattern) = (HSTRING::from(kind), HSTRING::from(pattern));
        let filters = [
            COMDLG_FILTERSPEC { pszName: PCWSTR(kind.as_ptr()), pszSpec: PCWSTR(pattern.as_ptr()) },
            COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") },
        ];
        let _ = dialog.SetFileTypes(&filters);
        dialog.Show(owner).ok()?;
        path_of(&dialog.GetResult().ok()?)
    }
}

/// Where to save a file, starting in Documents with `name` suggested.
pub fn choose_save(owner: Option<HWND>, title: &str, name: &str, kind: &str, pattern: &str) -> Option<String> {
    unsafe {
        let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let current = dialog.GetOptions().ok()?;
        dialog.SetOptions(current | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT).ok()?;
        let _ = dialog.SetTitle(&HSTRING::from(title));
        let _ = dialog.SetFileName(&HSTRING::from(name));
        let (kind, pattern) = (HSTRING::from(kind), HSTRING::from(pattern));
        let _ = dialog.SetFileTypes(&[COMDLG_FILTERSPEC { pszName: PCWSTR(kind.as_ptr()), pszSpec: PCWSTR(pattern.as_ptr()) }]);
        let _ = dialog.SetDefaultExtension(&HSTRING::from(pattern.to_string().trim_start_matches("*.")));
        if let Some(folder) = known_folder(&FOLDERID_Documents) {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(folder.as_path()), None) {
                let _ = dialog.SetFolder(&item);
            }
        }
        dialog.Show(owner).ok()?;
        path_of(&dialog.GetResult().ok()?)
    }
}

fn dialog(title: PCWSTR, options: u32, start_folder: Option<&Path>) -> Option<IFileOpenDialog> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let current = dialog.GetOptions().ok()?;
        dialog.SetOptions(current | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST | windows::Win32::UI::Shell::FILEOPENDIALOGOPTIONS(options)).ok()?;
        let _ = dialog.SetTitle(title);
        if let Some(folder) = start_folder.filter(|f| f.is_dir()) {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(folder), None) {
                let _ = dialog.SetFolder(&item);
            }
        }
        Some(dialog)
    }
}

fn path_of(item: &IShellItem) -> Option<String> {
    unsafe {
        let text = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const c_void));
        path
    }
}

// The choosers themselves. The settings window (its own process) calls them directly, as the
// dialogs' owner (the icon picker's Browse... too); the dock never does (it asks the helper above).

/// An image for an item's icon. Picking a program, DLL or icon file instead opens Windows' icon
/// chooser for it, to pick which of its icons. Returns an icon reference (`file` or `file,index`).
pub fn choose_image(owner: Option<HWND>, start: &str) -> Option<String> {
    let start_folder = Path::new(start).parent().filter(|p| p.is_dir()).map(Path::to_path_buf).or_else(icons_folder);
    let dialog = dialog(w!("Choose an icon"), 0, start_folder.as_deref())?;
    unsafe {
        let filters = [
            COMDLG_FILTERSPEC { pszName: w!("Images and icons"), pszSpec: w!("*.png;*.ico;*.jpg;*.jpeg;*.bmp;*.gif;*.tif;*.tiff;*.exe;*.dll") },
            COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") },
        ];
        let _ = dialog.SetFileTypes(&filters);
        dialog.Show(owner).ok()?;
        let path = path_of(&dialog.GetResult().ok()?)?;
        let lower = path.to_ascii_lowercase();
        if [".exe", ".dll", ".icl"].iter().any(|e| lower.ends_with(e)) {
            return choose_program_icon(owner, &path);
        }
        Some(path)
    }
}

/// Windows' own icon chooser for the icons inside `file` (it can browse to other files too).
pub fn choose_program_icon(owner: Option<HWND>, file: &str) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let wide: Vec<u16> = file.encode_utf16().collect();
    let len = wide.len().min(buffer.len() - 1);
    buffer[..len].copy_from_slice(&wide[..len]);
    let mut index = 0i32;
    let chosen = unsafe { PickIconDlg(owner, &mut buffer, Some(&mut index)) };
    if chosen == 0 {
        return None;
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    let path = String::from_utf16_lossy(&buffer[..end]);
    Some(format!("{path},{index}"))
}

/// Files to add to the dock (several at once is fine).
pub fn choose_files(owner: Option<HWND>) -> Vec<String> {
    // The real Desktop, wherever it is (OneDrive moves it, for one).
    let start = known_folder(&FOLDERID_Desktop);
    let Some(dialog) = dialog(w!("Add to the dock"), FOS_ALLOWMULTISELECT.0, start.as_deref()) else { return Vec::new() };
    unsafe {
        if dialog.Show(owner).is_err() {
            return Vec::new();
        }
        let Ok(items) = dialog.GetResults() else { return Vec::new() };
        let count = items.GetCount().unwrap_or(0);
        (0..count).filter_map(|i| items.GetItemAt(i).ok()).filter_map(|item| path_of(&item)).collect()
    }
}

/// A folder: to add to the dock, or (with `start`) an item's start-in folder.
pub fn choose_folder(owner: Option<HWND>, start: &str) -> Option<String> {
    let (title, start) = if start.is_empty() {
        (w!("Add a folder to the dock"), std::env::var("USERPROFILE").map(PathBuf::from).ok())
    } else {
        (w!("Choose a folder"), Some(PathBuf::from(start)))
    };
    let dialog = dialog(title, FOS_PICKFOLDERS.0, start.as_deref())?;
    unsafe {
        dialog.Show(owner).ok()?;
        path_of(&dialog.GetResult().ok()?)
    }
}

/// Where to install Desktop Dock (setup's Change...): opens where it's going now.
pub fn choose_install_folder(owner: Option<HWND>, start: Option<&Path>) -> Option<String> {
    let dialog = dialog(w!("Choose where to install Desktop Dock"), FOS_PICKFOLDERS.0, start)?;
    unsafe {
        dialog.Show(owner).ok()?;
        path_of(&dialog.GetResult().ok()?)
    }
}

/// Any file, for an item's target; opens in the current target's folder.
pub fn choose_target(owner: Option<HWND>, start: &str) -> Option<String> {
    let folder = Path::new(start).parent().filter(|p| p.is_dir()).map(Path::to_path_buf);
    let dialog = dialog(w!("Choose what this item opens"), 0, folder.as_deref())?;
    unsafe {
        let filters = [
            COMDLG_FILTERSPEC { pszName: w!("Programs"), pszSpec: w!("*.exe;*.lnk;*.bat;*.cmd;*.url") },
            COMDLG_FILTERSPEC { pszName: w!("All files"), pszSpec: w!("*.*") },
        ];
        let _ = dialog.SetFileTypes(&filters);
        let lower = start.to_ascii_lowercase();
        let _ = dialog.SetFileTypeIndex(if [".exe", ".lnk", ".bat", ".cmd", ".url"].iter().any(|e| lower.ends_with(e)) || start.is_empty() { 1 } else { 2 });
        dialog.Show(owner).ok()?;
        path_of(&dialog.GetResult().ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooser_names_survive_the_trip_to_the_helper() {
        for pick in [Pick::Files, Pick::Folder] {
            assert_eq!(Pick::from_word(pick.word()), Some(pick));
        }
        assert_eq!(Pick::from_word("anything"), None);
    }
}
