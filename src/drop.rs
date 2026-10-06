//! Dropping onto the dock from Explorer, the desktop, the Start menu or a browser. This is
//! Windows' own drag and drop (OLE), which only reports real drags of files, apps or links, so
//! the dock reacts to those and never to window or text drags.
//!
//! What's dropped becomes ordinary dock items. Shortcuts are read, not linked: their target,
//! arguments, start-in folder, icon and window state are copied into dock.toml, so deleting the
//! original changes nothing. The dock never moves or deletes the files themselves.

use crate::config::{ItemConfig, RunState};
use crate::home::Home;
use crate::target::{self, Class};
use crate::log_warn;
use std::cell::Cell;
use std::ffi::c_void;
use std::path::Path;
use windows::Win32::Foundation::{HGLOBAL, POINT, POINTL, S_OK};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, DVASPECT_CONTENT, FORMATETC, IDataObject, IPersistFile, STGM_READ,
    TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    DragQueryFileW, HDROP, IShellItem, IShellItemArray, IShellLinkDataList, IShellLinkW, SHCreateShellItemArrayFromDataObject, SIGDN,
    SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH, SIGDN_NORMALDISPLAY, SLDF_HAS_DARWINID, SLGP_RAWPATH, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, SW_SHOWMINNOACTIVE};
use windows::core::{HSTRING, Interface, Ref, implement, w};

/// What the drop target tells the dock.
pub enum Event<'a> {
    Enter(&'a IDataObject, POINT),
    Over(POINT),
    Leave,
    Drop(&'a IDataObject, POINT),
}

/// Handles an event; true means "this can go on the dock here". The flag says the event came
/// from the drop strip (the dock was hidden) rather than the dock itself.
pub type Handler = fn(Event, bool) -> bool;

/// Windows' drop target for one window: the dock, or the drop strip that reveals it.
///
/// Kept deliberately minimal, so a drag passing over the dock can't get stuck: each call does only
/// arithmetic and returns at once; reading what was dropped happens in Drop, saving it after Drop
/// has returned. No drag-picture helper (`IDropTargetHelper`): it's cosmetic and the one part
/// that talks back to the source's drag image.
#[implement(IDropTarget)]
pub struct DropTarget {
    from_strip: bool,
    handler: Handler,
    /// What the source allows (copy, move, link), from DragEnter.
    allowed: Cell<DROPEFFECT>,
}

impl DropTarget {
    pub fn create(from_strip: bool, handler: Handler) -> IDropTarget {
        DropTarget { from_strip, handler, allowed: Cell::new(DROPEFFECT_NONE) }.into()
    }
}

/// Link, not copy or move: the source keeps its files untouched.
fn effect(allowed: DROPEFFECT, accept: bool) -> DROPEFFECT {
    if !accept {
        DROPEFFECT_NONE
    } else if allowed.0 & DROPEFFECT_LINK.0 != 0 {
        DROPEFFECT_LINK
    } else if allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else {
        DROPEFFECT_NONE
    }
}

/// Where the pointer is, in the dock's own (real pixel) terms. Windows hands over the position
/// in the drag source's terms: a program that isn't aware of display scaling would report it
/// scaled (at 150%, two-thirds of the real position), so ask Windows directly instead.
fn point(pt: &POINTL) -> POINT {
    crate::system::cursor_pos().unwrap_or(POINT { x: pt.x, y: pt.y })
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect_out: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let data = data.ok()?;
        let allowed = unsafe { *effect_out };
        self.allowed.set(allowed);
        let accept = accepts(data) && (self.handler)(Event::Enter(data, point(pt)), self.from_strip);
        let chosen = effect(allowed, accept);
        unsafe {
            *effect_out = chosen;
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect_out: *mut DROPEFFECT) -> windows::core::Result<()> {
        let accept = (self.handler)(Event::Over(point(pt)), self.from_strip);
        let chosen = effect(self.allowed.get(), accept);
        unsafe {
            *effect_out = chosen;
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        (self.handler)(Event::Leave, self.from_strip);
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect_out: *mut DROPEFFECT) -> windows::core::Result<()> {
        let data = data.ok()?;
        let accept = (self.handler)(Event::Drop(data, point(pt)), self.from_strip);
        let chosen = effect(self.allowed.get(), accept);
        unsafe {
            *effect_out = chosen;
        }
        Ok(())
    }
}

// ---- What was dropped -------------------------------------------------------------------------

fn format(cf: u16) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

fn url_format() -> u16 {
    unsafe { RegisterClipboardFormatW(w!("UniformResourceLocatorW")) as u16 }
}

/// Files, shell items (Start menu apps, This PC…) or a web link: things the dock can hold.
pub fn accepts(data: &IDataObject) -> bool {
    let shell_ids = unsafe { RegisterClipboardFormatW(w!("Shell IDList Array")) as u16 };
    [CF_HDROP.0, shell_ids, url_format()]
        .into_iter()
        .any(|cf| unsafe { data.QueryGetData(&format(cf)) } == S_OK)
}

/// What a drop brings: the dock items it would add, and the dropped files' own paths. Opening
/// with an app or recycling uses the paths: a shortcut dropped on the Recycle Bin is recycled
/// itself, never the program it points to.
pub struct Dropped {
    pub items: Vec<ItemConfig>,
    pub paths: Vec<String>,
}

pub fn read(data: &IDataObject, home: &Home) -> Dropped {
    Dropped { items: items_from(data, home), paths: file_paths(data) }
}

/// The dropped files and folders, as they are (shortcuts not followed).
fn file_paths(data: &IDataObject) -> Vec<String> {
    if let Ok(array) = unsafe { SHCreateShellItemArrayFromDataObject::<_, IShellItemArray>(data) } {
        let count = unsafe { array.GetCount() }.unwrap_or(0);
        let paths: Vec<String> = (0..count)
            .filter_map(|i| unsafe { array.GetItemAt(i) }.ok())
            .filter_map(|item| display_name(&item, SIGDN_FILESYSPATH))
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    paths_from(data)
}

/// A program, as opposed to a document, folder or link: dropped on an app's icon, it offers to
/// replace that app.
pub fn is_program(target: &str) -> bool {
    let lower = target.trim().to_ascii_lowercase();
    lower.starts_with(r"shell:appsfolder\") || [".exe", ".com", ".bat", ".cmd", ".lnk"].iter().any(|e| lower.ends_with(e))
}

/// An item that files can be dropped onto to open them with it: an ordinary program (Store apps
/// open files differently; not yet).
pub fn opens_files(target: &str) -> bool {
    let lower = target.trim().to_ascii_lowercase();
    [".exe", ".com", ".bat", ".cmd", ".lnk"].iter().any(|e| lower.ends_with(e))
}
/// The dock items a drop brings, in order.
pub fn items_from(data: &IDataObject, home: &Home) -> Vec<ItemConfig> {
    if let Ok(array) = unsafe { SHCreateShellItemArrayFromDataObject::<_, IShellItemArray>(data) } {
        let count = unsafe { array.GetCount() }.unwrap_or(0);
        let items: Vec<ItemConfig> = (0..count)
            .filter_map(|i| unsafe { array.GetItemAt(i) }.ok())
            .filter_map(|item| from_shell_item(&item, home))
            .collect();
        if !items.is_empty() {
            return items;
        }
    }
    // Many programs (download lists, editors, zip tools) offer only the plain file list.
    let paths = paths_from(data);
    if !paths.is_empty() {
        return paths.iter().map(|path| from_path(Path::new(path), "", home)).collect();
    }
    url_from(data).into_iter().collect()
}

/// The dropped files from the plain file list (CF_HDROP) that every file drag offers.
fn paths_from(data: &IDataObject) -> Vec<String> {
    unsafe {
        let Ok(mut medium) = data.GetData(&format(CF_HDROP.0)) else { return Vec::new() };
        let drop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(drop, u32::MAX, None).min(1000);
        let mut paths = Vec::new();
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buffer = vec![0u16; len + 1];
            if DragQueryFileW(drop, i, Some(&mut buffer)) > 0 {
                paths.push(wide(&buffer));
            }
        }
        ReleaseStgMedium(&mut medium);
        paths
    }
}

fn display_name(item: &IShellItem, kind: SIGDN) -> Option<String> {
    unsafe {
        let text = item.GetDisplayName(kind).ok()?;
        let result = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const c_void));
        result.filter(|s| !s.is_empty())
    }
}

fn from_shell_item(item: &IShellItem, home: &Home) -> Option<ItemConfig> {
    let shown = display_name(item, SIGDN_NORMALDISPLAY).unwrap_or_default();
    match display_name(item, SIGDN_FILESYSPATH) {
        Some(path) => Some(from_path(Path::new(&path), &shown, home)),
        // Not a file: a Start menu app, This PC, Control Panel… stored by its shell name.
        None => {
            let target = display_name(item, SIGDN_DESKTOPABSOLUTEPARSING)?;
            Some(ItemConfig::new(clean_name(&shown), shell_target(&target)))
        }
    }
}

/// A shell name as the dock can open it later. Start menu apps come as bare app IDs
/// (`Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`), meaningful only inside the Apps folder.
pub fn shell_target(parsing: &str) -> String {
    let absolute = matches!(target::classify(parsing), Class::Shell | Class::Uri | Class::Absolute);
    if absolute { parsing.to_string() } else { format!(r"shell:AppsFolder\{parsing}") }
}

/// A dock item for a file or folder chosen in a dialog: shortcuts are read like dropped ones.
pub fn item_for_path(path: &str, home: &Home) -> ItemConfig {
    from_path(Path::new(path), "", home)
}

fn from_path(path: &Path, shown: &str, home: &Home) -> ItemConfig {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = clean_name(if shown.is_empty() { &stem } else { shown });
    let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    match extension.as_deref() {
        Some("lnk") => from_shortcut(path, name, home),
        Some("url") => from_internet_shortcut(path, name),
        // One of your folders (Documents, say) becomes its shell name, as the menus give it: it
        // then follows the folder if it moves.
        _ => {
            let shown = path.display().to_string();
            let target = crate::places::shell_name_for_folder(&shown).map_or(shown, str::to_string);
            ItemConfig::new(name, target)
        }
    }
}

/// What Explorer shows, without the extensions it sometimes shows.
pub fn clean_name(shown: &str) -> String {
    let shown = shown.trim();
    for extension in [".lnk", ".url", ".exe"] {
        let cut = shown.len().saturating_sub(extension.len());
        if shown.len() > extension.len() && shown.is_char_boundary(cut) && shown[cut..].eq_ignore_ascii_case(extension) {
            return shown[..cut].trim_end().to_string();
        }
    }
    shown.to_string()
}

struct Shortcut {
    target: String,
    args: String,
    start_in: String,
    icon: String,
    run: RunState,
    /// Installer-managed ("advertised"): its path isn't a real program path.
    advertised: bool,
}

fn wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

fn read_shortcut(path: &Path) -> Option<Shortcut> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        link.cast::<IPersistFile>().ok()?.Load(&HSTRING::from(path), STGM_READ).ok()?;
        let advertised = link
            .cast::<IShellLinkDataList>()
            .ok()
            .and_then(|data| data.GetFlags().ok())
            .is_some_and(|flags| flags & SLDF_HAS_DARWINID.0 as u32 != 0);
        let mut buffer = vec![0u16; 2048];
        // The raw path keeps %VARIABLES% (the dock expands them when launching).
        let target = link.GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32).map(|_| wide(&buffer)).unwrap_or_default();
        let args = link.GetArguments(&mut buffer).map(|_| wide(&buffer)).unwrap_or_default();
        let start_in = link.GetWorkingDirectory(&mut buffer).map(|_| wide(&buffer)).unwrap_or_default();
        let mut index = 0;
        let icon_file = link.GetIconLocation(&mut buffer, &mut index).map(|_| wide(&buffer)).unwrap_or_default();
        // The program's own first icon is what the dock shows anyway.
        let icon = if icon_file.is_empty() || (index == 0 && icon_file.eq_ignore_ascii_case(&target)) {
            String::new()
        } else {
            format!("{icon_file},{index}")
        };
        let run = match link.GetShowCmd() {
            Ok(cmd) if cmd == SW_SHOWMAXIMIZED => RunState::Maximized,
            Ok(cmd) if cmd == SW_SHOWMINNOACTIVE || cmd == SW_SHOWMINIMIZED => RunState::Minimized,
            _ => RunState::Normal,
        };
        Some(Shortcut { target, args, start_in, icon, run, advertised })
    }
}

fn from_shortcut(path: &Path, name: String, home: &Home) -> ItemConfig {
    match read_shortcut(path) {
        Some(link) if !link.target.is_empty() && !link.advertised => {
            let mut item = ItemConfig::new(name, link.target);
            item.args = link.args;
            item.start_in = link.start_in;
            item.icon = link.icon;
            item.run = link.run;
            item
        }
        // Installer-managed shortcuts, and ones pointing at shell places, only work as
        // themselves: keep a private copy in the dock folder, so deleting the original is fine.
        _ => {
            let kept = if crate::breaks::broken("drop.no-kept-copy") { None } else { keep_copy(path, home) };
            let target = kept.unwrap_or_else(|| path.display().to_string());
            ItemConfig::new(name, target)
        }
    }
}

/// Copies a shortcut into the dock folder's `shortcuts\`, as `shortcuts\Name.lnk` (relative,
/// so it moves with the dock folder).
fn keep_copy(path: &Path, home: &Home) -> Option<String> {
    let folder = home.dir.join("shortcuts");
    std::fs::create_dir_all(&folder).ok()?;
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let extension = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "lnk".into());
    let original = std::fs::read(path).ok()?;
    for n in 1..100 {
        let file = if n == 1 { format!("{stem}.{extension}") } else { format!("{stem} ({n}).{extension}") };
        let copy = folder.join(&file);
        match std::fs::read(&copy) {
            Ok(existing) if existing == original => return Some(format!(r"shortcuts\{file}")), // the same one again
            Ok(_) => continue,
            Err(_) => {
                return match std::fs::write(&copy, &original) {
                    Ok(()) => Some(format!(r"shortcuts\{file}")),
                    Err(e) => {
                        log_warn!("couldn't keep a copy of {}: {e}", path.display());
                        None
                    }
                };
            }
        }
    }
    None
}

/// A `.url` file (an internet shortcut): its address, and its icon if it names one.
fn from_internet_shortcut(path: &Path, name: String) -> ItemConfig {
    let text = std::fs::read(path).map(|bytes| crate::system::text_from_bytes(&bytes)).unwrap_or_default();
    let (url, icon) = parse_internet_shortcut(&text);
    let url = if crate::breaks::broken("drop.url-as-file") { String::new() } else { url };
    let mut item = ItemConfig::new(name, if url.is_empty() { path.display().to_string() } else { url });
    item.icon = icon;
    item
}

/// The URL and icon (`file,index`) from a `.url` file's `[InternetShortcut]` section.
pub fn parse_internet_shortcut(text: &str) -> (String, String) {
    let mut in_section = false;
    let (mut url, mut icon_file, mut icon_index) = (String::new(), String::new(), String::from("0"));
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line.eq_ignore_ascii_case("[InternetShortcut]");
            continue;
        }
        let Some((key, value)) = line.split_once('=').filter(|_| in_section) else { continue };
        match key.trim().to_ascii_lowercase().as_str() {
            "url" => url = value.trim().to_string(),
            "iconfile" => icon_file = value.trim().to_string(),
            "iconindex" => icon_index = value.trim().to_string(),
            _ => {}
        }
    }
    let icon = if icon_file.is_empty() { String::new() } else { format!("{icon_file},{icon_index}") };
    (url, icon)
}

/// A link dragged from a browser.
fn url_from(data: &IDataObject) -> Option<ItemConfig> {
    unsafe {
        let mut medium = data.GetData(&format(url_format())).ok()?;
        let global: HGLOBAL = medium.u.hGlobal;
        let text = GlobalLock(global) as *const u16;
        let url = if text.is_null() {
            None
        } else {
            // Never read past the end of the block, even if the text isn't terminated.
            let limit = (GlobalSize(global) / 2).min(4096);
            let len = (0..limit).take_while(|&i| *text.add(i) != 0).count();
            Some(String::from_utf16_lossy(std::slice::from_raw_parts(text, len)))
        };
        let _ = GlobalUnlock(global);
        ReleaseStgMedium(&mut medium);
        let url = url.filter(|u| !u.trim().is_empty())?;
        Some(ItemConfig::new(name_for_url(&url), url))
    }
}

/// A short name for a web link: its site, without "www.".
pub fn name_for_url(url: &str) -> String {
    let Some((_, rest)) = url.split_once("://") else { return url.to_string() };
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.split(':').next().unwrap_or(host);
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() { url.to_string() } else { host.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;

    /// A drop that offers only the plain file list (as many programs do) still gives an item.
    #[test]
    fn a_plain_file_list_drop_becomes_items() {
        use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, STGMEDIUM, STGMEDIUM_0};
        use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc};
        use windows::Win32::UI::Shell::{DROPFILES, SHCreateDataObject};
        let dir = std::env::temp_dir().join(format!("dock-drop-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("Notes.txt");
        std::fs::write(&file, "x").unwrap();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            // The classic CF_HDROP block: a DROPFILES header, then the path, double-terminated.
            let path: Vec<u16> = file.as_os_str().encode_wide().chain([0, 0]).collect();
            let header = size_of::<DROPFILES>();
            let global = GlobalAlloc(GMEM_MOVEABLE, header + path.len() * 2).unwrap();
            let base = GlobalLock(global) as *mut u8;
            *(base as *mut DROPFILES) = DROPFILES { pFiles: header as u32, fWide: true.into(), ..Default::default() };
            std::ptr::copy_nonoverlapping(path.as_ptr() as *const u8, base.add(header), path.len() * 2);
            let _ = GlobalUnlock(global);
            let data: IDataObject = SHCreateDataObject(None, None, None).unwrap();
            let medium = STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: global },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            };
            data.SetData(&format(CF_HDROP.0), &medium, true).unwrap();
            assert!(accepts(&data));
            let home = Home::new(dir.clone());
            let items = items_from(&data, &home);
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].target, file.display().to_string());
            assert!(items[0].name == "Notes" || items[0].name == "Notes.txt", "{}", items[0].name);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn programs_and_what_takes_files() {
        assert!(is_program(r"C:\Program Files\Adobe\Photoshop.exe"));
        assert!(is_program(r"shortcuts\Photoshop.lnk"));
        assert!(is_program(r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"));
        assert!(!is_program(r"C:\Users\alex\Pictures\photo.jpg"));
        assert!(!is_program(r"C:\Users\alex\Documents"));
        assert!(!is_program("https://example.com/setup.exe/"), "a web page");
        assert!(opens_files(r"C:\Tools\vlc.EXE"));
        assert!(!opens_files(r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"), "Store apps: not yet");
        assert!(!opens_files(r"C:\Users\alex\Documents"));
    }
    #[test]
    fn start_menu_apps_open_through_the_apps_folder() {
        assert_eq!(shell_target("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App"), r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App");
        assert_eq!(
            shell_target(r"{6D809377-6AF0-444B-8957-A3773F02200E}\Notepad++\notepad++.exe"),
            r"shell:AppsFolder\{6D809377-6AF0-444B-8957-A3773F02200E}\Notepad++\notepad++.exe"
        );
        assert_eq!(shell_target("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"), "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "This PC stays");
        assert_eq!(shell_target(r"C:\Tools\x.exe"), r"C:\Tools\x.exe");
    }

    #[test]
    fn names_lose_the_extensions_explorer_sometimes_shows() {
        assert_eq!(clean_name("VLC media player.lnk"), "VLC media player");
        assert_eq!(clean_name("chrome.EXE"), "chrome");
        assert_eq!(clean_name("Plex.url"), "Plex");
        assert_eq!(clean_name("Notes.txt"), "Notes.txt", "documents keep theirs");
        assert_eq!(clean_name(".exe"), ".exe");
    }

    #[test]
    fn internet_shortcuts_give_their_address_and_icon() {
        let text = "[{000214A0-0000-0000-C000-000000000046}]\r\nProp3=19,11\r\n[InternetShortcut]\r\nIDList=\r\nURL=https://app.plex.tv/desktop\r\nIconFile=C:\\Icons\\plex.ico\r\nIconIndex=0\r\n";
        assert_eq!(parse_internet_shortcut(text), ("https://app.plex.tv/desktop".to_string(), r"C:\Icons\plex.ico,0".to_string()));
        assert_eq!(parse_internet_shortcut("[InternetShortcut]\nURL=https://example.com\n"), ("https://example.com".to_string(), String::new()));
    }

    #[test]
    fn web_links_are_named_after_their_site() {
        assert_eq!(name_for_url("https://www.youtube.com/watch?v=1"), "youtube.com");
        assert_eq!(name_for_url("http://localhost:8080/admin"), "localhost");
        assert_eq!(name_for_url("https://user@git.example.org/repo"), "git.example.org");
        assert_eq!(name_for_url("mailto:me"), "mailto:me");
    }

    #[test]
    fn link_beats_copy_and_nothing_is_ever_moved() {
        assert_eq!(effect(DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_LINK.0 | 2), true), DROPEFFECT_LINK);
        assert_eq!(effect(DROPEFFECT(DROPEFFECT_COPY.0 | 2), true), DROPEFFECT_COPY);
        assert_eq!(effect(DROPEFFECT(2), true), DROPEFFECT_NONE, "move only: refuse");
        assert_eq!(effect(DROPEFFECT(DROPEFFECT_LINK.0), false), DROPEFFECT_NONE);
    }
}
