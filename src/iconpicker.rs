//! The icon picker (settings process only): a grid of icons to choose from, like RocketDock's
//! Icon Settings had. Shows your icons folder (`Pictures\Icons`, its subfolders too), the icons
//! inside the item's program, or Windows' own icons (imageres.dll, shell32.dll), with a search
//! box. Clicking one shows it on the dock straight away (it's saved, so the dock reloads it);
//! "Use this icon" keeps it, Cancel or closing puts the old one back (the caller does that, with
//! what `choose` returns). Browse… opens Windows' own choosers for anything else.
//!
//! Thumbnails load on a background thread, in this process: nothing reaches the dock but the
//! chosen icon's name in dock.toml.

use crate::icons::{self, IconLoader, IconSource, Pixels};
use crate::pickers;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    HIMAGELIST, ILC_COLOR32, ImageList_Add, ImageList_Create, ImageList_Destroy, ImageList_Remove, LIST_VIEW_ITEM_STATE_FLAGS, LVIF_IMAGE,
    LVIF_STATE, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_DELETEALLITEMS, LVM_DELETEITEM, LVM_ENSUREVISIBLE, LVM_INSERTITEMW, LVM_SETIMAGELIST,
    LVM_SETITEMSTATE, LVM_SETITEMW, LVN_ITEMCHANGED, LVSIL_NORMAL, NMHDR, NMLISTVIEW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, DialogBoxParamW, EndDialog, GetDlgItem, GetDlgItemTextW, PostMessageW, SendMessageW, SetDlgItemTextW,
    KillTimer, SetTimer, SetWindowTextW, WM_APP, WM_TIMER, WM_COMMAND, WM_INITDIALOG, WM_NOTIFY,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

const IDD_PICKER: usize = 108;
const IDC_SHELF: i32 = 1801;
const IDC_SEARCH: i32 = 1802;
const IDC_ICONS: i32 = 1803;
const IDC_BROWSE: i32 = 1804;
/// Back to the program's own icon (Windows' icon for whatever the item opens).
const IDC_OWN: i32 = 1805;
const IDC_INFO: i32 = 1806;
const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
/// Thumbnails arrived from the loading thread.
const WM_APP_THUMBS: u32 = WM_APP + 3;
const EN_CHANGE: u32 = 0x0300;
const CBN_SELCHANGE: u32 = 1;
const BN_CLICKED: u32 = 0;
const NM_DBLCLK: u32 = 0xFFFF_FFFD; // (UINT)-3
const IMAGE_EXTENSIONS: &[&str] = &["png", "ico", "jpg", "jpeg", "bmp", "gif", "tif", "tiff"];
/// A folder of icons this big is plenty; past it, the rest aren't listed.
const MOST: usize = 3000;
/// A pick shows on the dock once the selection has rested this long (ms), so arrowing through
/// the grid doesn't save dock.toml at every step.
const SETTLE_MS: u32 = 150;
const TIMER_SETTLE: usize = 1;

/// What the picker needs to know.
pub struct Request {
    pub owner: HWND,
    /// The window's title ("Choose an icon for Photoshop").
    pub title: String,
    /// The item's program (an .exe or .dll), for its own icons.
    pub program: Option<String>,
    /// The icon as it is now (resolved): picked out in the grid if it's there.
    pub current: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shelf {
    Yours,
    Program,
    Windows,
}

#[derive(Debug, Clone)]
struct Choice {
    label: String,
    /// What goes into dock.toml: an image's path, or "file,index".
    icon: String,
    sources: Vec<IconSource>,
}

struct Picker {
    dialog: HWND,
    title: String,
    list: HWND,
    images: HIMAGELIST,
    px: u32,
    shelves: Vec<Shelf>,
    folder: Option<PathBuf>,
    program: Option<String>,
    current: String,
    choices: Vec<Choice>,
    /// The choices the search leaves, in list order.
    shown: Vec<usize>,
    image_of: HashMap<usize, i32>,
    /// Choices whose icon came out empty (Windows has a few blank ones): not shown.
    blank: std::collections::HashSet<usize>,
    arrived: Arc<Mutex<Vec<(u64, usize, Option<Pixels>)>>>,
    generation: Arc<AtomicU64>,
    /// Shows a click on the dock (the caller saves it).
    apply: Box<dyn Fn(&str)>,
    /// Something was applied (so Cancel has something to put back).
    applied: bool,
    /// The row picked, waiting for the selection to settle.
    pending: Option<usize>,
    /// Filling the list: its selection changes aren't clicks.
    filling: bool,
}

thread_local! {
    static PICKER: RefCell<Option<Picker>> = const { RefCell::new(None) };
}

/// "1 icon", "24 icons".
fn icons_count(n: usize) -> String {
    if n == 1 { "1 icon".to_string() } else { format!("{n} icons") }
}

fn with_picker<R>(f: impl FnOnce(&mut Picker) -> R) -> Option<R> {
    PICKER.with(|cell| cell.try_borrow_mut().ok().and_then(|mut p| p.as_mut().map(f)))
}

/// Shows the picker (modal to `request.owner`). `apply` is called with each icon clicked (to
/// show it on the dock) and with the final choice. Returns true if one was kept; false if
/// cancelled after something was applied, so the caller puts the old icon back.
pub fn choose(request: Request, apply: impl Fn(&str) + 'static) -> bool {
    let shelves = {
        let mut shelves = vec![Shelf::Yours];
        if request.program.is_some() {
            shelves.push(Shelf::Program);
        }
        shelves.push(Shelf::Windows);
        shelves
    };
    let picker = Picker {
        dialog: HWND::default(),
        title: request.title,
        list: HWND::default(),
        images: HIMAGELIST::default(),
        px: 48,
        shelves,
        folder: pickers::own_icons_folder(),
        program: request.program,
        current: request.current,
        choices: Vec::new(),
        shown: Vec::new(),
        image_of: HashMap::new(),
        blank: Default::default(),
        arrived: Arc::new(Mutex::new(Vec::new())),
        generation: Arc::new(AtomicU64::new(0)),
        apply: Box::new(apply),
        applied: false,
        pending: None,
        filling: false,
    };
    PICKER.with(|cell| *cell.borrow_mut() = Some(picker));
    let result = unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return false };
        DialogBoxParamW(Some(instance.into()), PCWSTR(IDD_PICKER as *const u16), Some(request.owner), Some(picker_proc), LPARAM(0))
    };
    let picker = PICKER.with(|cell| cell.borrow_mut().take());
    if let Some(picker) = picker {
        picker.generation.fetch_add(1, Ordering::SeqCst); // the loading thread stops
        unsafe {
            let _ = ImageList_Destroy(Some(picker.images));
        }
        return result == 1 || !picker.applied;
    }
    result == 1
}

unsafe extern "system" fn picker_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    if let Some(result) = crate::chrome::themed_message(hwnd, msg, wparam, lparam) {
        return result;
    }
    match msg {
        WM_INITDIALOG => {
            with_picker(|p| p.start(hwnd));
            if let Ok(list) = unsafe { GetDlgItem(Some(hwnd), IDC_ICONS) } {
                unsafe {
                    let _ = SetFocus(Some(list));
                }
            }
            0 // the focus is set
        }
        WM_COMMAND => {
            let (id, code) = ((wparam.0 & 0xFFFF) as i32, ((wparam.0 >> 16) & 0xFFFF) as u32);
            match (id, code) {
                (IDOK, _) => {
                    with_picker(|p| p.settle());
                    end(hwnd, 1);
                }
                (IDCANCEL, _) => end(hwnd, 0),
                (IDC_SHELF, CBN_SELCHANGE) => {
                    with_picker(|p| p.shelf_chosen());
                }
                (IDC_SEARCH, EN_CHANGE) => {
                    with_picker(|p| p.refill());
                }
                (IDC_OWN, BN_CLICKED) => {
                    with_picker(|p| {
                        p.pending = None;
                        (p.apply)("");
                    });
                    end(hwnd, 1);
                }
                (IDC_BROWSE, BN_CLICKED) => {
                    let start = with_picker(|p| p.current.clone()).unwrap_or_default();
                    // Windows' own choosers (an image, or the icons inside any program).
                    if let Some(icon) = pickers::choose_image(Some(hwnd), &start) {
                        with_picker(|p| (p.apply)(&icon));
                        end(hwnd, 1);
                    }
                }
                _ => {}
            }
            1
        }
        WM_NOTIFY => {
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            if header.idFrom == IDC_ICONS as usize {
                if header.code == LVN_ITEMCHANGED {
                    let change = unsafe { &*(lparam.0 as *const NMLISTVIEW) };
                    let selected_now = change.uNewState & LVIS_SELECTED.0 != 0 && change.uOldState & LVIS_SELECTED.0 == 0;
                    if selected_now && change.iItem >= 0 {
                        with_picker(|p| p.clicked(change.iItem as usize));
                    }
                } else if header.code == NM_DBLCLK {
                    with_picker(|p| p.settle());
                    end(hwnd, 1); // a double-click keeps it
                }
            }
            0
        }
        WM_APP_THUMBS => {
            with_picker(|p| p.receive());
            1
        }
        WM_TIMER if wparam.0 == TIMER_SETTLE => {
            with_picker(|p| p.settle());
            1
        }
        _ => 0,
    }
}

fn end(hwnd: HWND, result: isize) {
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_SETTLE);
        let _ = EndDialog(hwnd, result);
    }
}

/// Every image in the folder (and its subfolders), by name.
fn images_in(folder: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(folder) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_lowercase());
    for entry in entries {
        if found.len() >= MOST {
            return;
        }
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => images_in(&path, found),
            _ if path.extension().is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|known| e.eq_ignore_ascii_case(known))) => found.push(path),
            _ => {}
        }
    }
}

/// The icons inside a program or DLL.
fn icons_inside(file: &str, prefix: &str) -> Vec<Choice> {
    (0..icons::icon_count(file))
        .map(|k| Choice { label: format!("{prefix}{}", k + 1), icon: format!("{file},{k}"), sources: vec![IconSource::Resource(file.to_string(), k as i32)] })
        .collect()
}

impl Picker {
    fn start(&mut self, dialog: HWND) {
        self.dialog = dialog;
        crate::chrome::set_app_icon(dialog);
        crate::chrome::apply_theme(dialog);
        unsafe {
            let _ = SetWindowTextW(dialog, &HSTRING::from(self.title.as_str()));
            self.px = 48 * GetDpiForWindow(dialog).max(96) / 96;
            self.list = GetDlgItem(Some(dialog), IDC_ICONS).unwrap_or_default();
            self.images = ImageList_Create(self.px as i32, self.px as i32, ILC_COLOR32, 256, 256);
            SendMessageW(self.list, LVM_SETIMAGELIST, Some(WPARAM(LVSIL_NORMAL as usize)), Some(LPARAM(self.images.0)));
            if let Ok(combo) = GetDlgItem(Some(dialog), IDC_SHELF) {
                for shelf in &self.shelves {
                    let name = match shelf {
                        Shelf::Yours => "Your icons".to_string(),
                        Shelf::Program => "The program's own icons".into(),
                        Shelf::Windows => "Windows icons".into(),
                    };
                    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                    SendMessageW(combo, CB_ADDSTRING, None, Some(LPARAM(wide.as_ptr() as isize)));
                }
            }
        }
        // Start where the current icon is: inside the program, or (usually) your icons.
        let lower = self.current.to_ascii_lowercase();
        let in_program = self.program.as_ref().is_some_and(|program| lower.starts_with(&program.to_ascii_lowercase()));
        let in_windows = lower.contains("\\imageres.dll") || lower.contains("\\shell32.dll");
        // No icons folder of your own: the program's icons, or Windows'.
        let first = match () {
            _ if in_program => Shelf::Program,
            _ if in_windows => Shelf::Windows,
            _ if self.folder.is_some() => Shelf::Yours,
            _ if self.program.is_some() => Shelf::Program,
            _ => Shelf::Windows,
        };
        let at = self.shelves.iter().position(|&shelf| shelf == first).unwrap_or(0);
        unsafe {
            if let Ok(combo) = GetDlgItem(Some(dialog), IDC_SHELF) {
                SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(at)), None);
            }
        }
        self.load(self.shelves[at]);
    }

    fn shelf_chosen(&mut self) {
        let at = unsafe { GetDlgItem(Some(self.dialog), IDC_SHELF).map(|combo| SendMessageW(combo, CB_GETCURSEL, None, None).0).unwrap_or(0) };
        if let Some(&shelf) = usize::try_from(at).ok().and_then(|at| self.shelves.get(at)) {
            self.load(shelf);
        }
    }

    /// Lists a shelf's icons and starts their thumbnails loading.
    fn load(&mut self, shelf: Shelf) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.choices = match shelf {
            Shelf::Yours => {
                let mut found = Vec::new();
                if let Some(folder) = &self.folder {
                    images_in(folder, &mut found);
                }
                found
                    .into_iter()
                    .map(|path| {
                        let label = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        let icon = path.display().to_string();
                        Choice { label, sources: vec![IconSource::Image(icon.clone())], icon }
                    })
                    .collect()
            }
            Shelf::Program => self.program.as_deref().map(|program| icons_inside(program, "")).unwrap_or_default(),
            Shelf::Windows => {
                let system = std::env::var("WINDIR").map(|w| PathBuf::from(w).join("System32")).unwrap_or_else(|_| PathBuf::from("C:\\Windows\\System32"));
                // Today's set, then the older one (named so a search can tell them apart).
                let mut all = icons_inside(&system.join("imageres.dll").display().to_string(), "Windows ");
                all.extend(icons_inside(&system.join("shell32.dll").display().to_string(), "Windows classic "));
                all
            }
        };
        self.image_of.clear();
        self.blank.clear();
        unsafe {
            let _ = ImageList_Remove(self.images, -1); // all of them
        }
        let info = match (shelf, self.choices.len(), &self.folder) {
            (Shelf::Yours, 0, Some(_)) => r"No images in Pictures\Icons yet: put .png or .ico files there, or click Browse.".into(),
            (Shelf::Yours, _, None) => r"Put .png or .ico files in Pictures\Icons to see them here, or click Browse.".into(),
            (Shelf::Yours, n, Some(_)) => format!(r"{} in Pictures\Icons", icons_count(n)),
            (_, n, _) => icons_count(n),
        };
        set_info(self.dialog, &info);
        self.refill();
        // Thumbnails, in the background (this process only).
        let wanted: Vec<(usize, Vec<IconSource>)> = self.choices.iter().enumerate().map(|(k, c)| (k, c.sources.clone())).collect();
        let (px, arrived, current, dialog) = (self.px, self.arrived.clone(), self.generation.clone(), self.dialog.0 as isize);
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            }
            let Ok(loader) = IconLoader::new(None) else { return };
            for (n, (k, sources)) in wanted.into_iter().enumerate() {
                if current.load(Ordering::SeqCst) != generation {
                    return; // another shelf, or the picker closed
                }
                let pixels = loader.load(&sources, px);
                if let Ok(mut list) = arrived.lock() {
                    list.push((generation, k, pixels));
                }
                if n % 12 == 11 {
                    unsafe {
                        let _ = PostMessageW(Some(HWND(dialog as *mut c_void)), WM_APP_THUMBS, WPARAM(0), LPARAM(0));
                    }
                }
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(dialog as *mut c_void)), WM_APP_THUMBS, WPARAM(0), LPARAM(0));
            }
        });
    }

    /// The list, as the search leaves it; the current icon picked out if it's there.
    fn refill(&mut self) {
        self.pending = None; // its row is about to change
        let search = text_of(self.dialog, IDC_SEARCH).trim().to_lowercase();
        self.shown = (0..self.choices.len())
            .filter(|&k| !self.blank.contains(&k) && (search.is_empty() || self.choices[k].label.to_lowercase().contains(&search)))
            .collect();
        self.filling = true;
        unsafe {
            SendMessageW(self.list, LVM_DELETEALLITEMS, None, None);
            for (row, &k) in self.shown.iter().enumerate() {
                let mut text: Vec<u16> = self.choices[k].label.encode_utf16().chain(std::iter::once(0)).collect();
                let image = self.image_of.get(&k).copied().unwrap_or(-1);
                let item = LVITEMW { mask: LVIF_TEXT | LVIF_IMAGE, iItem: row as i32, pszText: PWSTR(text.as_mut_ptr()), iImage: image, ..Default::default() };
                SendMessageW(self.list, LVM_INSERTITEMW, None, Some(LPARAM(&item as *const _ as isize)));
            }
            let current = self.current.to_ascii_lowercase();
            if let Some(row) = self.shown.iter().position(|&k| self.choices[k].icon.to_ascii_lowercase() == current) {
                let both = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
                let state = LVITEMW { mask: LVIF_STATE, state: both, stateMask: both, ..Default::default() };
                SendMessageW(self.list, LVM_SETITEMSTATE, Some(WPARAM(row)), Some(LPARAM(&state as *const _ as isize)));
                SendMessageW(self.list, LVM_ENSUREVISIBLE, Some(WPARAM(row)), Some(LPARAM(0)));
            }
        }
        self.filling = false;
    }

    /// One was clicked (or arrowed to): it shows on the dock once the selection settles.
    fn clicked(&mut self, row: usize) {
        if self.filling {
            return;
        }
        self.pending = Some(row);
        unsafe {
            SetTimer(Some(self.dialog), TIMER_SETTLE, SETTLE_MS, None);
        }
    }

    /// The pick waiting to settle: it shows on the dock now.
    fn settle(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.dialog), TIMER_SETTLE);
        }
        let Some(row) = self.pending.take() else { return };
        let Some(choice) = self.shown.get(row).and_then(|&k| self.choices.get(k)) else { return };
        let icon = choice.icon.clone();
        let label = choice.label.clone();
        (self.apply)(&icon);
        self.applied = true;
        self.current = icon;
        set_info(self.dialog, &format!("{label}: showing on the dock. Click Use this icon to keep it, or Cancel to go back."));
    }

    fn receive(&mut self) {
        let generation = self.generation.load(Ordering::SeqCst);
        let arrived: Vec<_> = self.arrived.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default();
        for (made_for, k, pixels) in arrived {
            if made_for != generation {
                continue;
            }
            if pixels.as_ref().is_some_and(|p| p.data.chunks_exact(4).all(|px| px[3] == 0)) {
                self.blank.insert(k);
                if let Some(row) = self.shown.iter().position(|&shown| shown == k) {
                    self.shown.remove(row);
                    self.filling = true;
                    unsafe {
                        SendMessageW(self.list, LVM_DELETEITEM, Some(WPARAM(row)), None);
                    }
                    self.filling = false;
                }
                continue;
            }
            let Some(bitmap) = pixels.as_ref().and_then(bitmap_of) else { continue };
            let image = unsafe { ImageList_Add(self.images, bitmap, None) };
            unsafe {
                let _ = DeleteObject(bitmap.into());
            }
            if image < 0 {
                continue;
            }
            self.image_of.insert(k, image);
            if let Some(row) = self.shown.iter().position(|&shown| shown == k) {
                let item = LVITEMW { mask: LVIF_IMAGE, iItem: row as i32, iImage: image, ..Default::default() };
                unsafe {
                    SendMessageW(self.list, LVM_SETITEMW, None, Some(LPARAM(&item as *const _ as isize)));
                }
            }
        }
    }
}

fn text_of(hwnd: HWND, id: i32) -> String {
    let mut buffer = vec![0u16; 512];
    let len = unsafe { GetDlgItemTextW(hwnd, id, &mut buffer) } as usize;
    String::from_utf16_lossy(&buffer[..len.min(buffer.len())])
}

fn set_info(dialog: HWND, text: &str) {
    unsafe {
        let _ = SetDlgItemTextW(dialog, IDC_INFO, &HSTRING::from(text));
    }
}

/// A 32-bit premultiplied bitmap of an icon's pixels.
fn bitmap_of(pixels: &Pixels) -> Option<HBITMAP> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: pixels.width as i32,
            biHeight: -(pixels.height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    unsafe {
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if bits.is_null() || pixels.data.len() != (pixels.width * pixels.height * 4) as usize {
            let _ = DeleteObject(bitmap.into());
            return None;
        }
        std::ptr::copy_nonoverlapping(pixels.data.as_ptr(), bits as *mut u8, pixels.data.len());
        Some(bitmap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn your_icons_folder_lists_images_in_subfolders_too_by_name() {
        let dir = std::env::temp_dir().join(format!("dock-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Games")).unwrap();
        for name in ["b.png", "A.ico", "notes.txt", "Games\\steam.png"] {
            std::fs::write(dir.join(name), "").unwrap();
        }
        let mut found = Vec::new();
        images_in(&dir, &mut found);
        let names: Vec<String> = found.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["A.ico", "b.png", "steam.png"], "images only, by name, subfolders in their place");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
