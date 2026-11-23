//! What's inside a folder, for a folder on the dock that shows it (`View::Grid`): read when it's
//! clicked, never watched, in Explorer's order. Nothing about the files is kept afterwards but
//! their icons in a cache of their own.

use crate::icons::IconSource;
use std::cmp::Ordering;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::StrCmpLogicalW;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::HSTRING;

/// One thing in the folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// As Explorer shows it: the file's name, a shortcut's without `.lnk` or `.url`.
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// Kept online only (OneDrive and the like): asking for its own icon would download it.
    pub cloud_only: bool,
}

/// Why a folder couldn't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// It isn't there (moved, deleted, a drive that's gone, a share its server doesn't have).
    Gone,
    /// It's on another computer that doesn't answer, or the network is down.
    Unreachable,
    /// Windows didn't allow reading it.
    Denied,
    /// Anything else (a server that answered with an error, say).
    Other,
}

/// Windows' errors that mean the computer a folder is on can't be reached (not that the folder
/// isn't there): the network path not found, the network name or connection gone, no network,
/// the host or network unreachable, a mapped drive not connected, timed out.
const UNREACHABLE: &[i32] = &[51, 53, 59, 64, 121, 1201, 1203, 1222, 1231, 1232, 1236, 2250];

/// What a failed read means for the person who clicked.
pub fn failure_of(e: &std::io::Error) -> Failure {
    use std::io::ErrorKind;
    if e.raw_os_error().is_some_and(|code| UNREACHABLE.contains(&code)) {
        return Failure::Unreachable;
    }
    match e.kind() {
        ErrorKind::NotFound => Failure::Gone,
        ErrorKind::PermissionDenied => Failure::Denied,
        ErrorKind::TimedOut | ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable | ErrorKind::NetworkDown => Failure::Unreachable,
        _ => Failure::Other,
    }
}

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;

/// The file type `IconSource::FileType` names a folder by (file types start with a dot).
pub const FOLDER_TYPE: &str = "folder";

/// File types whose files each have an icon of their own (a program's, a shortcut's); every
/// other file shows its type's icon, so a folder of 140 PDFs needs one icon, not 140.
const OWN_ICON: &[&str] = &["exe", "lnk", "url", "ico", "msc", "cpl", "appref-ms", "scr"];

/// Reads `dir`: everything but hidden and system files, folders first, then by name as
/// Explorer orders them ("file2" before "file10").
pub fn read(dir: &Path) -> Result<Vec<Entry>, Failure> {
    use std::os::windows::fs::MetadataExt;
    let list = std::fs::read_dir(dir).map_err(|e| failure_of(&e))?;
    let mut entries = Vec::new();
    for entry in list.flatten() {
        // On Windows this comes with the listing itself: no look at each file.
        let Ok(meta) = entry.metadata() else { continue };
        let attributes = meta.file_attributes();
        if attributes & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0 {
            continue;
        }
        // The attribute, not `is_dir`: a folder that's a junction (an old "My Music") counts.
        let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let cloud_only = attributes & (FILE_ATTRIBUTE_OFFLINE | FILE_ATTRIBUTE_RECALL_ON_OPEN | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS) != 0;
        entries.push(Entry { name: shown_name(&file_name, is_dir), path: entry.path(), is_dir, cloud_only });
    }
    entries.sort_by(order);
    Ok(entries)
}

/// Folders read in the background, waiting to be taken: each read's number and what it found.
pub type Answers = Arc<Mutex<Vec<(u64, Result<Vec<Entry>, Failure>)>>>;

/// Reads `dir` on a thread of its own (a server that's switched off can keep a reader waiting
/// for many seconds), then puts the answer in `done` and posts `message` to the dock.
pub fn read_in_background(id: u64, dir: PathBuf, done: Answers, hwnd: HWND, message: u32) {
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || {
        let result = read(&dir);
        if let Ok(mut list) = done.lock() {
            list.push((id, result));
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut c_void)), message, WPARAM(0), LPARAM(0));
        }
    });
}

/// Folders first, then by name.
pub fn order(a: &Entry, b: &Entry) -> Ordering {
    b.is_dir.cmp(&a.is_dir).then_with(|| natural(&a.name, &b.name))
}

/// Explorer's order for names (numbers by their value, letters regardless of case), by Windows'
/// own comparison, the one Explorer uses; names it finds equal keep a fixed order.
pub fn natural(a: &str, b: &str) -> Ordering {
    let by_windows = unsafe { StrCmpLogicalW(&HSTRING::from(a), &HSTRING::from(b)) };
    by_windows.cmp(&0).then_with(|| a.cmp(b))
}

/// A file's name as Explorer shows it: a shortcut without its `.lnk` or `.url`.
pub fn shown_name(file_name: &str, is_dir: bool) -> String {
    if !is_dir {
        let lower = file_name.to_ascii_lowercase();
        for ext in [".lnk", ".url"] {
            if lower.ends_with(ext) && file_name.len() > ext.len() {
                return file_name[..file_name.len() - ext.len()].to_string();
            }
        }
    }
    file_name.to_string()
}

/// Where an entry's icon comes from, best first. A program or shortcut has its own, unless it's
/// kept online only or on another computer (asking would download it, or wait on the server);
/// everything else shows its type's icon, which needs no look at the file at all.
pub fn icon_sources(entry: &Entry, on_network: bool) -> Vec<IconSource> {
    if entry.is_dir {
        return vec![IconSource::FileType(FOLDER_TYPE.into())];
    }
    let ext = entry.path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let mut sources = Vec::new();
    if !entry.cloud_only && !on_network && OWN_ICON.contains(&ext.as_str()) {
        let path = entry.path.to_string_lossy().into_owned();
        if ext == "exe" {
            sources.push(IconSource::Resource(path.clone(), 0));
        }
        sources.push(IconSource::Shell(path));
    }
    sources.push(IconSource::FileType(if ext.is_empty() { String::new() } else { format!(".{ext}") }));
    sources
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool) -> Entry {
        Entry { name: shown_name(name, is_dir), path: PathBuf::from(format!(r"C:\F\{name}")), is_dir, cloud_only: false }
    }

    #[test]
    fn names_go_in_explorers_order_with_folders_first() {
        let mut list: Vec<Entry> = ["file10.txt", "File2.txt", "b", "a.txt", "file1.txt", "Invoice 140.pdf", "Invoice 9.pdf"]
            .iter()
            .map(|n| entry(n, false))
            .chain(["zeta", "Alpha"].iter().map(|n| entry(n, true)))
            .collect();
        list.sort_by(order);
        let names: Vec<&str> = list.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "zeta", "a.txt", "b", "file1.txt", "File2.txt", "file10.txt", "Invoice 9.pdf", "Invoice 140.pdf"]);
    }

    #[test]
    fn numbers_compare_by_value_then_by_their_zeros() {
        assert_eq!(natural("a2", "a10"), Ordering::Less);
        assert_eq!(natural("a010", "a9"), Ordering::Greater);
        assert_eq!(natural("logo.png", "logo-dark.png"), Ordering::Less, "as Explorer: a hyphen counts for little");
        assert_eq!(natural("a", "a1"), Ordering::Less);
        assert_eq!(natural("ABC", "abc"), Ordering::Less, "a stable order for names that differ only in case");
    }

    #[test]
    fn shortcuts_show_without_their_extension() {
        assert_eq!(shown_name("Photo Editor.lnk", false), "Photo Editor");
        assert_eq!(shown_name("Site.URL", false), "Site");
        assert_eq!(shown_name("notes.txt", false), "notes.txt");
        assert_eq!(shown_name("Old.lnk", true), "Old.lnk", "a folder's name is its name");
        assert_eq!(shown_name(".lnk", false), ".lnk");
    }

    #[test]
    fn programs_and_shortcuts_have_their_own_icon_everything_else_its_types() {
        let types = |e: &Entry, network| icon_sources(e, network);
        assert_eq!(types(&entry("report.pdf", false), false), vec![IconSource::FileType(".pdf".into())]);
        assert_eq!(types(&entry("photo.PNG", false), false), vec![IconSource::FileType(".png".into())], "never a thumbnail");
        assert_eq!(types(&entry("assets", true), false), vec![IconSource::FileType(FOLDER_TYPE.into())]);
        assert_eq!(types(&entry("README", false), false), vec![IconSource::FileType(String::new())]);
        let link = entry("Editor.lnk", false);
        assert_eq!(types(&link, false), vec![IconSource::Shell(r"C:\F\Editor.lnk".into()), IconSource::FileType(".lnk".into())]);
        assert_eq!(types(&link, true), vec![IconSource::FileType(".lnk".into())], "not asked of a server");
        let cloud = Entry { cloud_only: true, ..entry("Tool.exe", false) };
        assert_eq!(types(&cloud, false), vec![IconSource::FileType(".exe".into())], "not downloaded for its icon");
        let program = entry("Tool.exe", false);
        assert_eq!(types(&program, false)[0], IconSource::Resource(r"C:\F\Tool.exe".into(), 0));
    }

    #[test]
    fn a_server_that_doesnt_answer_is_unreachable_not_gone() {
        let os = std::io::Error::from_raw_os_error;
        assert_eq!(failure_of(&os(53)), Failure::Unreachable, "the network path not found: the server didn't answer");
        assert_eq!(failure_of(&os(1232)), Failure::Unreachable, "the host unreachable");
        assert_eq!(failure_of(&os(2250)), Failure::Unreachable, "a mapped drive not connected");
        assert_eq!(failure_of(&os(67)), Failure::Gone, "the server answered: it has no such share");
        assert_eq!(failure_of(&os(2)), Failure::Gone);
        assert_eq!(failure_of(&os(3)), Failure::Gone);
        assert_eq!(failure_of(&os(5)), Failure::Denied);
        assert_eq!(failure_of(&os(1117)), Failure::Other, "a device error");
    }

    #[test]
    fn a_real_folder_is_read_without_hidden_files() {
        let dir = std::env::temp_dir().join(format!("desktop-dock-folder-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for name in ["b.txt", "a10.txt", "a2.txt", "hidden.txt"] {
            std::fs::write(dir.join(name), "x").unwrap();
        }
        let hidden = windows::core::HSTRING::from(dir.join("hidden.txt").as_os_str());
        unsafe {
            windows::Win32::Storage::FileSystem::SetFileAttributesW(&hidden, windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_HIDDEN).unwrap();
        }
        let names: Vec<String> = read(&dir).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["sub", "a2.txt", "a10.txt", "b.txt"]);
        assert_eq!(read(&dir.join("not-there")), Err(Failure::Gone));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
