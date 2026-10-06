//! Windows items: places and tools Windows itself provides (This PC, Downloads, Control Panel,
//! Settings…), offered by Add ▸ Windows item on the dock's menu and in Items > Add…. Each is an
//! ordinary item, the same as dragging it in from Explorer or Start gives, with Windows' own
//! icon. Your folders are kept by their shell names (`shell:Personal` for Documents), so they
//! follow the folder when it moves (to OneDrive, say), and names are Windows' own, in your
//! language.

use crate::config::{ItemConfig, Kind, RECYCLE_BIN};
use crate::edit;
use crate::home::Home;
use std::ffi::c_void;
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Registry::{HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures, FOLDERID_Videos, KF_FLAG_DEFAULT,
    SHGetKnownFolderPath, SHLoadIndirectString,
};
use windows::core::{GUID, HSTRING};

enum Where {
    /// A shell name (`::{CLSID}`, `shell:…`), as Explorer reports it for a dragged item.
    Shell(&'static str),
    /// One of your folders: Windows' id for it, and the shell name that finds it wherever it is.
    Folder(GUID, &'static str),
}

struct Place {
    /// How the first dock picks it (names differ by language).
    id: &'static str,
    /// Its name if Windows doesn't give one.
    english: &'static str,
    at: Where,
}

/// What the menus offer, in order. `None`: a dividing line.
const PLACES: [Option<Place>; 14] = [
    Some(Place { id: "this-pc", english: "This PC", at: Where::Shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}") }),
    Some(Place { id: "file-explorer", english: "File Explorer", at: Where::Shell(r"shell:AppsFolder\Microsoft.Windows.Explorer") }),
    None,
    Some(Place { id: "desktop", english: "Desktop", at: Where::Folder(FOLDERID_Desktop, "shell:Desktop") }),
    Some(Place { id: "documents", english: "Documents", at: Where::Folder(FOLDERID_Documents, "shell:Personal") }),
    Some(Place { id: "downloads", english: "Downloads", at: Where::Folder(FOLDERID_Downloads, "shell:Downloads") }),
    Some(Place { id: "pictures", english: "Pictures", at: Where::Folder(FOLDERID_Pictures, "shell:My Pictures") }),
    Some(Place { id: "music", english: "Music", at: Where::Folder(FOLDERID_Music, "shell:My Music") }),
    Some(Place { id: "videos", english: "Videos", at: Where::Folder(FOLDERID_Videos, "shell:My Video") }),
    None,
    Some(Place { id: "network", english: "Network", at: Where::Shell("::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}") }),
    Some(Place { id: "control-panel", english: "Control Panel", at: Where::Shell("::{26EE0668-A00A-44D7-9371-BEB064C98683}") }),
    Some(Place {
        id: "settings",
        english: "Settings",
        at: Where::Shell(r"shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel"),
    }),
    Some(Place { id: SHOW_DESKTOP, english: "Show desktop", at: Where::Shell(SHOW_DESKTOP_TARGET) }),
];
const SHOW_DESKTOP: &str = "show-desktop";
/// Show desktop's shell item. It's an action, not a place: Windows has nothing to "open" for it
/// (see launch.rs), so it runs the way Windows' own Show desktop shortcut does.
pub const SHOW_DESKTOP_TARGET: &str = "shell:::{3080F90D-D7AD-11D9-BD98-0000947B0257}";
/// Its own icon is a tiny old one; this is Windows' desktop picture.
const SHOW_DESKTOP_ICON: &str = r"System32\imageres.dll,-110";

/// Every Windows item's id ("this-pc", "show-desktop"…), for the feature inventory (W.<id>).
#[cfg_attr(not(all(test, dev_lab)), allow(dead_code))]
pub(crate) fn ids() -> Vec<&'static str> {
    PLACES.iter().flatten().map(|place| place.id).collect()
}

/// The Windows items, as dock items (None: a dividing line). A folder that can't be found is
/// left out.
pub fn all() -> Vec<Option<ItemConfig>> {
    PLACES.iter().filter_map(|place| place.as_ref().map(item).unwrap_or(Some(None))).collect()
}

/// One place as a dock item: Some(Some(item)), or None if it isn't on this PC.
fn item(place: &Place) -> Option<Option<ItemConfig>> {
    let name = local_name(place).unwrap_or_else(|| place.english.to_string());
    let mut item = match &place.at {
        Where::Shell(_) if place.id == "network" && crate::breaks::broken("places.network-typo") => {
            ItemConfig::new(name, "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3D}")
        }
        Where::Shell(target) => ItemConfig::new(name, *target),
        Where::Folder(id, shell_name) => {
            known_folder(id)?;
            ItemConfig::new(name, *shell_name)
        }
    };
    if place.id == SHOW_DESKTOP {
        let windows = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        item.icon = format!(r"{windows}\{SHOW_DESKTOP_ICON}");
    }
    Some(Some(item))
}

fn by_id(id: &str) -> Option<ItemConfig> {
    PLACES.iter().flatten().find(|place| place.id == id).and_then(item).flatten()
}

/// A new dock's dock.toml: This PC, your Documents, Downloads, Pictures, Music and Videos (wherever
/// they are on this PC) and the Recycle Bin, named in your language. Everything else is yours
/// to add.
pub fn first_dock() -> String {
    let mut items = Vec::new();
    let separator = || {
        let mut separator = ItemConfig::default();
        separator.kind = Kind::Separator;
        separator
    };
    items.extend(by_id("this-pc"));
    items.push(separator());
    items.extend(["documents", "downloads", "pictures", "music", "videos"].into_iter().filter_map(by_id));
    items.push(separator());
    let mut bin = ItemConfig::new(recycle_bin_name(), "");
    bin.kind = Kind::RecycleBin;
    items.push(bin);
    let mut list = toml_edit::ArrayOfTables::new();
    for item in &items {
        list.push(edit::item_table(item));
    }
    let mut doc: toml_edit::DocumentMut = "version = 1\n\n[dock]\n".parse().expect("a fixed text");
    doc["item"] = toml_edit::Item::ArrayOfTables(list);
    edit::renumber(&mut doc);
    format!(
        "# Your dock. Change it on the dock itself (drag things on, off and along it; right-click for\n\
         # more) or in Dock settings; it's saved here as you go. You can edit this file by hand too:\n\
         # the dock picks up changes when you save. Settings you change are added under [dock].\n\n{doc}"
    )
}

/// The Recycle Bin's name in your language.
pub fn recycle_bin_name() -> String {
    clsid_name(RECYCLE_BIN.trim_start_matches("::")).unwrap_or_else(|| "Recycle Bin".into())
}

/// Where a shell name for one of your folders (`shell:Personal`) is on disk now, for Open file
/// location. None for anything else.
pub fn filesystem_path(target: &str) -> Option<String> {
    let target = target.trim();
    PLACES.iter().flatten().find_map(|place| match &place.at {
        Where::Folder(id, shell_name) if shell_name.eq_ignore_ascii_case(target) => known_folder(id),
        _ => None,
    })
}

/// A target that isn't a file, folder or web address, in words, for showing in place of its
/// shell name: "This PC (a Windows item)", "Calculator (an app)". None for anything that's
/// clearer shown as it is.
pub fn described(target: &str) -> Option<String> {
    let target = target.trim();
    let lower = target.to_ascii_lowercase();
    if !(target.starts_with("::") || lower.starts_with("shell:")) || filesystem_path(target).is_some() {
        return None;
    }
    let ours = PLACES.iter().flatten().find(|place| matches!(place.at, Where::Shell(at) if at.eq_ignore_ascii_case(target)));
    let name = shell_display_name(target).or_else(|| ours.map(|place| local_name(place).unwrap_or_else(|| place.english.to_string())));
    let app = lower.starts_with(r"shell:appsfolder\") && ours.is_none();
    Some(match (name, app) {
        (Some(name), true) => format!("{name} (an app)"),
        (Some(name), false) => format!("{name} (a Windows item)"),
        (None, true) => "An app".into(),
        (None, false) => "A Windows item".into(),
    })
}

/// What Windows calls a shell item, in your language ("This PC", "Calculator").
fn shell_display_name(target: &str) -> Option<String> {
    use windows::Win32::UI::Shell::{IShellItem, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY};
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(target), None).ok()?;
        let text = item.GetDisplayName(SIGDN_NORMALDISPLAY).ok()?;
        let name = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const c_void));
        name.filter(|name| !name.trim().is_empty())
    }
}

/// The file or folder an item opens, on disk: one of your folders by its shell name, or the
/// target as written (variables and relative paths resolved).
pub fn on_disk(home: &Home, target: &str) -> String {
    filesystem_path(target).unwrap_or_else(|| home.resolve(target))
}

/// The shell name for one of your folders when it's given by its path (dragged in from
/// Explorer, say), so it's the same item the menus give and follows the folder when it moves.
pub fn shell_name_for_folder(path: &str) -> Option<&'static str> {
    let path = path.trim().trim_end_matches('\\');
    PLACES.iter().flatten().find_map(|place| match &place.at {
        Where::Folder(id, shell_name) => known_folder(id).filter(|known| known.trim_end_matches('\\').eq_ignore_ascii_case(path)).map(|_| *shell_name),
        _ => None,
    })
}

fn known_folder(id: &GUID) -> Option<String> {
    unsafe {
        let path = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const c_void));
        // A folder kept on a server (Documents at work, say) isn't asked for: see `on_network`.
        text.filter(|t| on_network(t) || std::path::Path::new(t).is_dir())
    }
}

/// Whether `path` is on another computer: a server path, or a drive letter mapped to one.
/// Asking about such a path can keep the asker waiting many seconds when the server is
/// switched off, so the dock never does on its own thread. (Windows answers what kind a
/// drive letter is without asking the server.)
pub fn on_network(path: &str) -> bool {
    const DRIVE_REMOTE: u32 = 4;
    let path = path.trim().trim_start_matches('"');
    if let Some(rest) = path.strip_prefix(r"\\?\") {
        return rest.get(..4).is_some_and(|unc| unc.eq_ignore_ascii_case(r"UNC\"));
    }
    if path.starts_with(r"\\") || path.starts_with("//") {
        return true;
    }
    match path.as_bytes() {
        [letter, b':', ..] if letter.is_ascii_alphabetic() => unsafe {
            GetDriveTypeW(&HSTRING::from(format!("{}:\\", *letter as char))) == DRIVE_REMOTE
        },
        _ => false,
    }
}

/// Windows' own name for a place, in your language: the "@file,-id" reference Windows keeps
/// for it in the registry, loaded as text. Cheap, and none of the shell's machinery comes into
/// the dock. None if there's no such name (then the English one is used).
fn local_name(place: &Place) -> Option<String> {
    match &place.at {
        Where::Shell(target) => clsid_name(target.trim_start_matches("shell:").trim_start_matches("::")),
        Where::Folder(id, _) => indirect_string(
            HKEY_LOCAL_MACHINE,
            &format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\FolderDescriptions\{}", braced(id)),
            "LocalizedName",
        ),
    }
}

/// The name of a shell object by its class id ("{20D04FE0-…}").
fn clsid_name(clsid: &str) -> Option<String> {
    (clsid.starts_with('{') && clsid.ends_with('}')).then_some(())?;
    indirect_string(HKEY_CLASSES_ROOT, &format!(r"CLSID\{clsid}"), "LocalizedString")
}

fn braced(id: &GUID) -> String {
    let d = id.data4;
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        id.data1, id.data2, id.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
    )
}

/// Reads a registry text holding an "@file,-id" reference and loads the text it points to.
fn indirect_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let mut buffer = vec![0u16; 512];
    let mut bytes = (buffer.len() * 2) as u32;
    unsafe {
        let found = RegGetValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut c_void),
            Some(&mut bytes),
        );
        if found != windows::Win32::Foundation::ERROR_SUCCESS {
            return None;
        }
        let reference = String::from_utf16_lossy(&buffer[..(bytes as usize / 2).saturating_sub(1)]);
        let mut text = [0u16; 260];
        SHLoadIndirectString(&HSTRING::from(reference.as_str()), &mut text, None).ok()?;
        let name = String::from_utf16_lossy(&text[..text.iter().position(|&c| c == 0).unwrap_or(text.len())]);
        Some(name.trim().to_string()).filter(|name| !name.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::Home;
    use crate::icons::{self, IconLoader};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, IDataObject};
    use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, ILFree, SHCreateItemFromParsingName, SHParseDisplayName};
    use windows::core::HSTRING;

    #[test]
    fn a_new_dock_starts_with_this_pc_your_folders_and_the_bin() {
        let text = first_dock();
        let parsed = crate::config::parse(&text).unwrap();
        assert!(parsed.warnings.is_empty() && !parsed.needs_migration, "{:?}", parsed.warnings);
        let names: Vec<String> = parsed.config.items.iter().map(edit::label_of).collect();
        assert_eq!(names.first().map(String::as_str), Some("This PC"));
        assert_eq!(names.last().map(String::as_str), Some("Recycle Bin"));
        assert!(names.iter().any(|n| n == "Documents") && names.iter().any(|n| n == "Downloads"));
        assert_eq!(parsed.config.items.iter().filter(|item| item.is_separator()).count(), 2);
    }

    /// Each one parses to a shell item and has a proper icon. Opening them is left to the
    /// end-to-end tests: this test never opens anything.
    #[test]
    fn every_windows_item_parses_to_an_item_id_and_has_a_proper_icon() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let loader = IconLoader::new(None).unwrap();
        let items: Vec<ItemConfig> = all().into_iter().flatten().collect();
        assert!(items.len() >= 10, "the shell places, and your folders where they are");
        for item in items {
            // Launching opens a shell name by its item ID (launch.rs), so it must parse.
            if item.target.starts_with("::") || item.target.starts_with("shell:") {
                unsafe {
                    let mut pidl = std::ptr::null_mut();
                    SHParseDisplayName(&HSTRING::from(item.target.as_str()), None, &mut pidl, 0, None).unwrap();
                    ILFree(Some(pidl));
                }
            }
            let pixels = loader.load(&icons::sources_for(&item, false), 48).unwrap_or_else(|| panic!("{}: no icon", item.name));
            let seen = pixels.data.chunks_exact(4).filter(|px| px[3] > 0).count();
            assert!(seen > 48 * 48 / 4, "{}: a proper icon, not a speck ({seen} pixels)", item.name);
        }
    }

    #[test]
    fn your_folders_are_kept_by_shell_names_that_follow_them() {
        let text = first_dock();
        let parsed = crate::config::parse(&text).unwrap();
        let folders: Vec<&ItemConfig> = parsed.config.items.iter().filter(|item| item.target.starts_with("shell:")).collect();
        assert!(folders.len() >= 4, "Documents, Downloads, Pictures, Music, Videos: {:?}", parsed.config.items);
        let documents = filesystem_path("shell:Personal").expect("Documents is on this PC");
        assert!(std::path::Path::new(&documents).is_dir());
        assert_eq!(filesystem_path("SHELL:personal").as_deref(), Some(documents.as_str()), "any case");
        assert_eq!(shell_name_for_folder(&documents), Some("shell:Personal"), "dragged in by its path: the same item");
        assert_eq!(shell_name_for_folder(&format!(r"{documents}\")), Some("shell:Personal"), "with a trailing backslash");
        assert_eq!(filesystem_path("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"), None, "This PC isn't a folder on disk");
        let home = Home::new(std::env::temp_dir());
        assert_eq!(on_disk(&home, "shell:Personal"), documents, "Open file location finds it");
    }

    #[test]
    fn names_come_from_windows_in_your_language() {
        for place in PLACES.iter().flatten().filter(|place| !matches!(place.at, Where::Shell(target) if target.contains("AppsFolder"))) {
            let name = local_name(place).unwrap_or_else(|| panic!("{}: Windows has a name for it", place.english));
            assert!(!name.is_empty() && !name.starts_with('@'), "{}: {name}", place.english);
        }
        assert!(!recycle_bin_name().is_empty());
    }

    /// Dragging the same thing in from Explorer or Start gives the same item, so either way it
    /// opens and looks the same.
    #[test]
    fn dragged_in_from_explorer_they_are_the_same_items() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let home = Home::new(std::env::temp_dir());
        for item in all().into_iter().flatten().filter(|item| !item.target.contains("3080F90D")) { // Show desktop isn't something to drag
            let dragged = unsafe {
                let shell: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(item.target.as_str()), None).unwrap();
                // The same data Explorer offers when you drag the item.
                let data: IDataObject = shell.BindToHandler(None, &BHID_DataObject).unwrap();
                crate::drop::items_from(&data, &home)
            };
            assert_eq!(dragged.len(), 1, "{}", item.name);
            assert!(dragged[0].target.eq_ignore_ascii_case(&item.target), "{}: dragged in as {}", item.name, dragged[0].target);
        }
    }
}
