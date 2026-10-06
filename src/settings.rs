//! The settings window and an item's properties window, each a separate process that lives only
//! while its window is open (docs/settings-design.md):
//! - `desktop-dock.exe --settings [--item N [--child K]]`: the whole dock, in sections: Look,
//!   Behaviour, Screen, Items (every item, groups' items under them, the selected one's
//!   properties beside them), Recently removed, Versions, Files and About;
//! - `desktop-dock.exe --properties N [--child K]`: one item's properties in a small window of its
//!   own (right-click an icon > Properties...); with `--icon`, just its icon picker (iconpicker.rs;
//!   right-click an icon > Change icon...).
//! Everything such a window loads (file dialogs, icon extraction, Explorer add-ons) stays in its
//! process, never in the dock.
//!
//! Both edit dock.toml through the same safe saving as the dock (atomic, verified, backed up,
//! comments kept, only the changed key written) and then tell the dock to reload at once. Every
//! change applies straight away; Revert puts back what the window found when it opened. Problems
//! show in the status line, never as pop-ups.
//!
//! Built from dialog templates (assets\desktop-dock.rc), so Windows scales them for the display.

use crate::config::{self, DockLook, DockSettings, HoverGlow, ItemConfig, Kind, LaunchEffect, RunState};
use crate::edit::{self, Spot};
use crate::home::Home;
use crate::icons::{self, IconLoader, IconSource, Pixels};
use crate::removed::{Entry, Removed};
use crate::hotkey;
use crate::store::{Backup, BackupKind, Store};
use crate::{chrome, drop, iconpicker, import, launch, pickers, places, system};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, AlphaBlend, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, FillRect, HBITMAP, HDC, InvalidateRect, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, CheckDlgButton, DRAWITEMSTRUCT, HIMAGELIST, HKCOMB_NONE, HKCOMB_S, HKM_GETHOTKEY, HKM_SETHOTKEY, HKM_SETRULES,
    ICC_BAR_CLASSES, ICC_HOTKEY_CLASS, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES,
    ILC_COLOR32, INITCOMMONCONTROLSEX, ImageList_Add, ImageList_Create, InitCommonControlsEx, IsDlgButtonChecked, LIST_VIEW_ITEM_FLAGS,
    LIST_VIEW_ITEM_STATE_FLAGS, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_IMAGE, LVIF_STATE, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW,
    LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE, LVM_GETITEMCOUNT, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETCOLUMNWIDTH,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETIMAGELIST, LVM_SETITEMSTATE, LVM_SETITEMW, LVN_ITEMCHANGED, LVN_KEYDOWN, LVS_EX_FULLROWSELECT,
    LVSIL_SMALL, MEASUREITEMSTRUCT, NMHDR, NMLISTVIEW, NMLVKEYDOWN,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey, VK_DELETE};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL, CreateDialogParamW, CreatePopupMenu, DestroyMenu, DestroyWindow,
    DispatchMessageW, FindWindowExW, FindWindowW, GetClientRect, GetDlgCtrlID, GetDlgItem, GetDlgItemTextW, GetMessageW, GetPropW,
    GetWindowRect, HMENU, IDCANCEL, IDOK, IsDialogMessageW, IsIconic, IsWindowVisible, KillTimer, LB_ADDSTRING, LB_GETCURSEL, LB_SETCURSEL, LB_SETITEMHEIGHT, MF_POPUP, MF_SEPARATOR,
    MF_STRING, MSG, MapDialogRect, PostMessageW, PostQuitMessage, RemovePropW, SW_HIDE, SW_RESTORE, SW_SHOW, SWP_NOZORDER, SendMessageW,
    SetDlgItemTextW, SetForegroundWindow, SetPropW, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_TOPALIGN, TrackPopupMenu, TranslateMessage, WM_APP, WM_CLOSE, WM_COMMAND, WM_CTLCOLORLISTBOX, WM_DESTROY, WM_DRAWITEM, WM_HSCROLL,
    WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_INITDIALOG, WM_MEASUREITEM, WM_NOTIFY, WM_SYSCOLORCHANGE, WM_TIMER, STM_SETICON,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

// Dialogs and controls: these numbers must match assets\desktop-dock.rc.
const IDD_SETTINGS: usize = 100;
const IDD_ITEMS: usize = 101;
const IDD_PROPERTIES: usize = 102;
const IDD_LOOK: usize = 103;
const IDD_BEHAVIOUR: usize = 104;
const IDD_REMOVED: usize = 105;
const IDD_VERSIONS: usize = 106;
const IDD_FILES: usize = 107;
const IDD_ABOUT: usize = 109;
const IDD_SCREEN: usize = 112;
const IDC_SECTIONS: i32 = 1001;
/// The chosen section's title, above its page.
const IDC_TITLE: i32 = 1004;
/// Headings on the pages (several controls share the number; they're drawn semibold).
const IDC_HEADING: i32 = 1098;
const IDC_ABOUT_ICON: i32 = 1901;
const IDC_ABOUT_NAME: i32 = 1902;
const IDC_VERSION: i32 = 1903;
const IDC_MEMORY: i32 = 1904;
const IDC_OPEN_LOG: i32 = 1906;
const IDC_INSPIRED: i32 = 1907;
/// A red heart after the "inspired by" line.
const IDC_HEART: i32 = 1908;
const IDC_SAVE_COPY: i32 = 1707;
const IDC_LOAD_COPY: i32 = 1708;
const IDC_IMPORT_ROCKETDOCK: i32 = 1709;
const IDC_STATUS: i32 = 1002;
const IDC_REVERT: i32 = 1003;
const IDC_LIST: i32 = 1101;
const IDC_ADD: i32 = 1102;
const IDC_REMOVE: i32 = 1103;
const IDC_UP: i32 = 1104;
const IDC_DOWN: i32 = 1105;
const IDC_GROUP: i32 = 1106;
const IDC_NAME: i32 = 1110;
const IDC_TARGET: i32 = 1111;
const IDC_TARGET_BROWSE: i32 = 1112;
const IDC_ARGS: i32 = 1113;
const IDC_START_IN: i32 = 1114;
const IDC_START_BROWSE: i32 = 1115;
const IDC_RUN: i32 = 1116;
const IDC_ADMIN: i32 = 1117;
const IDC_ICON: i32 = 1118;
const IDC_ICON_CHANGE: i32 = 1119;
const IDC_ICON_RESET: i32 = 1120;
const IDC_NOTE: i32 = 1121;
const IDC_OPEN_LOCATION: i32 = 1122;
const IDC_SHOW_LABELS: i32 = 1307;
const IDC_LABEL_SIZE: i32 = 1308;
const IDC_LAUNCH_EFFECT: i32 = 1314;
const IDC_LOOK: i32 = 1315;
const IDC_HOVER_GLOW: i32 = 1316;
const IDC_LOCKED: i32 = 1411;
const IDC_STARTUP: i32 = 1412;
const IDC_REMOVED_LIST: i32 = 1501;
const IDC_PUT_BACK: i32 = 1502;
const IDC_FORGET: i32 = 1503;
const IDC_CLEAR_REMOVED: i32 = 1504;
const IDC_VERSIONS_LIST: i32 = 1601;
const IDC_RESTORE: i32 = 1602;
const IDC_SNAPSHOT: i32 = 1603;
const IDC_OPEN_BACKUPS: i32 = 1604;
const IDC_FOLDER: i32 = 1701;
const IDC_SIZES: i32 = 1702;
const IDC_OPEN_FOLDER: i32 = 1703;
const IDC_EDIT_CONFIG: i32 = 1704;
const IDC_RELOAD: i32 = 1705;
const IDC_CLEAR_CACHE: i32 = 1706;
/// From a second `--settings --item N` / `--properties N`: show that item in the open window
/// (wparam: the item; lparam: its place in its group + 1, or 0).
const WM_APP_SELECT: u32 = WM_APP + 1;
/// Icons finished loading in the background.
const WM_APP_ICONS: u32 = WM_APP + 2;
/// From `--properties N --icon`: open the icon picker (wparam 1: and close the window after).
const WM_APP_PICK_ICON: u32 = WM_APP + 4;
/// The window moved to a screen with different scaling: its fonts follow (posted to itself).
const WM_APP_RESTYLE: u32 = WM_APP + 5;
/// The dock reloads dock.toml at once when it gets this (dock.rs, WM_APP_RELOAD_NOW).
const DOCK_RELOAD_NOW: u32 = WM_APP + 9;
/// The dock comes out for a moment, to show a change (dock.rs, WM_APP_PEEK).
const DOCK_PEEK: u32 = WM_APP + 10;
/// The dock shows a moving slider's value without saving it (dock.rs, WM_APP_PREVIEW).
const DOCK_PREVIEW: u32 = WM_APP + 11;
/// The sliders the dock can show while they move, in the order dock.rs' `preview` knows them.
const PREVIEWED: [&str; 6] = ["icon_size", "zoom_size", "spacing", "label_size", "background_opacity", "icon_opacity"];
const TIMER_FILE: usize = 1;
/// A slider is saved once it has been still this long (or let go of).
const TIMER_SLIDERS: usize = 2;
const SLIDER_SETTLE_MS: u32 = 200;
/// Marks our windows (with which kind), so a second start finds the open one.
const MARK: PCWSTR = w!("DesktopDock.Window");
// Notifications from text boxes, the Run list and buttons (winuser.h).
const EN_KILLFOCUS: u32 = 0x0200;
const CBN_SELCHANGE: u32 = 1;
const BN_CLICKED: u32 = 0;
const LBN_SELCHANGE: u32 = 1;
// Sliders and lists (commctrl.h).
const TBM_GETPOS: u32 = 0x0400;
const TBM_SETPOS: u32 = 0x0405;
const TBM_SETRANGEMIN: u32 = 0x0407;
const TBM_SETRANGEMAX: u32 = 0x0408;
const TBM_SETPAGESIZE: u32 = 0x0415;
const TBM_SETLINESIZE: u32 = 0x0417;
const TB_THUMBTRACK: u32 = 5;
const TB_ENDTRACK: u32 = 8;
const LVM_GETNEXTITEM: u32 = 0x100C;
const LVM_SETITEMTEXTW: u32 = 0x1074;
const LVNI_SELECTED: isize = 2;
const LVIF_INDENT: LIST_VIEW_ITEM_FLAGS = LIST_VIEW_ITEM_FLAGS(0x10);
// The Add, Change… and Group… menus.
const ADD_FILE: usize = 1;
const ADD_FOLDER: usize = 2;
const ADD_SEPARATOR: usize = 3;
const ADD_BIN: usize = 4;
/// Add… > Windows item ▸: one entry per places::all() entry, from here.
const ADD_WINDOWS: usize = 100;
const GROUP_NEW: usize = 1;
const GROUP_TAKE_OUT: usize = 2;
const GROUP_UNGROUP: usize = 3;
/// Group… > Move to (group): one entry per group, from here.
const GROUP_MOVE: usize = 100;
/// What Group… > New group calls it (rename it in its properties).
const NEW_GROUP_NAME: &str = "New group";

/// The settings window's sections, in the order listed; each is a page beside the list. The
/// number is its symbol in Windows' symbol font (Segoe Fluent Icons).
const SECTIONS: [(PCWSTR, usize, u16); 8] = [
    (w!("Look"), IDD_LOOK, 0xE790),              // a palette
    (w!("Behaviour"), IDD_BEHAVIOUR, 0xE713),    // a gear
    (w!("Screen"), IDD_SCREEN, 0xE7F4),          // a monitor
    (w!("Items"), IDD_ITEMS, 0xE71D),            // a grid of apps
    (w!("Recently removed"), IDD_REMOVED, 0xE74D), // a bin
    (w!("Versions"), IDD_VERSIONS, 0xE81C),      // a clock going back
    (w!("Files"), IDD_FILES, 0xE8B7),            // a folder
    (w!("About"), IDD_ABOUT, 0xE946),            // an i
];
const SECTION_SCREEN: usize = 2;
const SECTION_ITEMS: usize = 3;
const SECTION_REMOVED: usize = 4;
const SECTION_VERSIONS: usize = 5;
const SECTION_FILES: usize = 6;
const SECTION_ABOUT: usize = 7;
// The Screen page.
const IDC_SCREEN_LIST: i32 = 2101;
const IDC_SCREEN_NOTE: i32 = 2102;
const IDC_SHORTCUT: i32 = 2103;
const IDC_SHORTCUT_CLEAR: i32 = 2104;
const IDC_SHORTCUT_NOTE: i32 = 2105;
/// The shortcut box (a hotkey control) changed.
const EN_CHANGE: u32 = 0x0300;

/// A slider for one of the dock's settings. Its value shows in the control after it (id + 1).
struct Slider {
    id: i32,
    key: &'static str,
    what: &'static str,
    /// Range and step, in the setting's own units.
    min: f32,
    max: f32,
    step: f32,
    unit: &'static str,
    /// Changing it makes new icons: saved when you let go, not while you drag.
    heavy: bool,
}

const SLIDERS: [Slider; 11] = [
    Slider { id: 1301, key: "icon_size", what: "Icon size", min: 16.0, max: 256.0, step: 1.0, unit: "px", heavy: true },
    Slider { id: 1303, key: "zoom_size", what: "Zoom size", min: 16.0, max: 256.0, step: 1.0, unit: "px", heavy: true },
    Slider { id: 1305, key: "spacing", what: "Spacing", min: 0.0, max: 64.0, step: 1.0, unit: "px", heavy: false },
    Slider { id: IDC_LABEL_SIZE, key: "label_size", what: "Name size", min: 8.0, max: 24.0, step: 0.5, unit: "px", heavy: false },
    Slider { id: 1310, key: "background_opacity", what: "Background", min: 0.0, max: 100.0, step: 1.0, unit: "%", heavy: false },
    Slider { id: 1312, key: "icon_opacity", what: "Icons", min: 10.0, max: 100.0, step: 1.0, unit: "%", heavy: false },
    Slider { id: 1401, key: "popup_delay_ms", what: "Show after", min: 0.0, max: 2000.0, step: 50.0, unit: "ms", heavy: false },
    Slider { id: 1403, key: "hide_delay_ms", what: "Hide after", min: 0.0, max: 2000.0, step: 50.0, unit: "ms", heavy: false },
    Slider { id: 1405, key: "slide_in_ms", what: "Slide in", min: 0.0, max: 1000.0, step: 25.0, unit: "ms", heavy: false },
    Slider { id: 1407, key: "slide_out_ms", what: "Slide out", min: 0.0, max: 1000.0, step: 25.0, unit: "ms", heavy: false },
    Slider { id: 1409, key: "zoom_ms", what: "Zoom time", min: 0.0, max: 1000.0, step: 10.0, unit: "ms", heavy: false },
];

/// A dock setting's current value, in the slider's units.
fn dock_value(dock: &DockSettings, key: &str) -> f32 {
    match key {
        "icon_size" => dock.icon_size,
        "zoom_size" => dock.zoom_size,
        "spacing" => dock.spacing,
        "label_size" => dock.label_size,
        "background_opacity" => dock.background_opacity as f32,
        "icon_opacity" => dock.icon_opacity as f32,
        "popup_delay_ms" => dock.popup_delay_ms as f32,
        "hide_delay_ms" => dock.hide_delay_ms as f32,
        "slide_in_ms" => dock.slide_in_ms as f32,
        "slide_out_ms" => dock.slide_out_ms as f32,
        "zoom_ms" => dock.zoom_ms as f32,
        _ => 0.0,
    }
}

/// How a value is written: whole numbers as integers, like the rest of the file.
fn toml_number(value: f32) -> toml_edit::Value {
    if value.fract() == 0.0 { toml_edit::Value::from(value as i64) } else { toml_edit::Value::from(value as f64) }
}

fn shown(slider: &Slider, value: f32) -> String {
    if value.fract() == 0.0 { format!("{value:.0} {}", slider.unit) } else { format!("{value:.1} {}", slider.unit) }
}

/// The text boxes: control, dock.toml key, and what the status line calls it.
const FIELDS: [(i32, &str, &str); 4] =
    [(IDC_NAME, "name", "Name"), (IDC_TARGET, "target", "Target"), (IDC_ARGS, "args", "Arguments"), (IDC_START_IN, "start_in", "Start in")];

/// Which window to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// The settings window, at this item in Items if given.
    Settings(Option<Spot>),
    /// One item's properties, in a small window of its own.
    Properties(Spot),
    /// Just the icon picker for one item (its properties window is its owner, and closes with it).
    Icon(Spot),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Settings,
    Properties,
}

impl Mode {
    fn mark(self) -> usize {
        match self {
            Mode::Settings => 1,
            Mode::Properties => 2,
        }
    }

    /// Held by the process showing this kind of window, from before the window exists.
    fn lock(self) -> &'static str {
        match self {
            Mode::Settings => r"Local\DesktopDock.SettingsWindow",
            Mode::Properties => r"Local\DesktopDock.PropertiesWindow",
        }
    }
}

thread_local! {
    static SETTINGS: RefCell<Option<Settings>> = const { RefCell::new(None) };
}

struct Settings {
    mode: Mode,
    home: Home,
    store: Store,
    removed: Removed,
    /// dock.toml when the window opened: the settings window's Revert puts this back.
    opening: String,
    /// dock.toml as last read.
    now: String,
    /// The properties window's item as it was when shown: its Revert puts this back.
    opening_item: Option<ItemConfig>,
    /// The dock's settings, as last read.
    dock: DockSettings,
    /// The settings window's section pages, and which one shows.
    pages: Vec<HWND>,
    section: usize,
    /// A slider has moved and isn't saved yet.
    sliders_pending: bool,
    /// What the dock was last asked to show while a slider moved (so it isn't asked twice).
    previewed: Option<(usize, f32)>,
    items: Vec<ItemConfig>,
    /// The Items list's rows: every item, each group's items right after it.
    rows: Vec<Spot>,
    selected: Option<Spot>,
    /// The selected item's label, to keep showing that item if the dock is rearranged meanwhile.
    following: Option<String>,
    /// What the Recently removed and Versions lists show.
    removed_list: Vec<Entry>,
    versions: Vec<Backup>,
    /// dock.toml's date as last read here, to notice changes made elsewhere.
    stamp: Option<SystemTime>,
    window: HWND,
    /// Where the property controls are: the Items page, or the properties window itself.
    page: HWND,
    /// The settings window's list of items, and its icons.
    list: Option<(HWND, HIMAGELIST)>,
    /// Icon sizes: in the list, and in the Icon box.
    list_px: u32,
    preview_px: u32,
    /// Icons by where they come from: (list size, preview size); None while loading or if
    /// there's none.
    icons: HashMap<Vec<IconSource>, (Option<Pixels>, Option<Pixels>)>,
    /// Where each loaded icon sits in the list's image list.
    image_index: HashMap<Vec<IconSource>, i32>,
    arrived: Arc<Mutex<Vec<(Vec<IconSource>, Option<Pixels>, Option<Pixels>)>>>,
    /// The Screen page's list: the `monitor` value each entry stands for.
    screen_choices: Vec<String>,
}

/// Something that opens a dialog or menu of its own: done outside the borrow of the window's
/// state, then the answer comes back in.
enum Ask {
    Target(Spot, String, String),
    StartIn(Spot, String, String),
    /// The icon picker: the item, its label, its icon as stored (to put back on Cancel) and
    /// resolved, and its program if that holds icons.
    Icon(Spot, String, String, String, Option<String>),
    /// Add… at this gap (in a group if it has a child place); whether the dock has a Recycle Bin.
    Add(Spot, POINT, bool),
    /// Group…: the menu's entries (id, text).
    Group(POINT, Vec<(usize, String)>),
    /// Files > Save a copy… / Load a copy… / Import from RocketDock… (its menu here).
    SaveCopy,
    LoadCopy,
    ImportRocketDock(POINT),
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn text_of(hwnd: HWND, id: i32) -> String {
    let mut buffer = vec![0u16; 4096];
    let len = unsafe { GetDlgItemTextW(hwnd, id, &mut buffer) } as usize;
    String::from_utf16_lossy(&buffer[..len.min(buffer.len())])
}

/// What a kept copy of your dock is, in words (Versions).
fn kept_copy_kind(label: &str) -> &'static str {
    match label {
        "before-restore" => "Before restoring a version",
        "before-load" => "Before loading a copy",
        "before-import" => "Before RocketDock import",
        "before-revert" => "Before revert",
        "before-install" => "Before install",
        "broken" => "File with an error (kept)",
        upgrade if upgrade.starts_with("before-v") && upgrade.ends_with("-migration") => "Before upgrade",
        _ => "Snapshot",
    }
}

/// How an item's field is shown: one of your folders kept by its shell name (`shell:Personal`)
/// shows where it is on disk. Saving what's shown changes nothing.
fn shown_value(key: &str, value: &str) -> String {
    match key {
        "target" => places::filesystem_path(value).or_else(|| places::described(value)).unwrap_or_else(|| value.to_string()),
        _ => value.to_string(),
    }
}

/// Where an item's own icon comes from, in words: an image by its file name, one of Windows'
/// own icons, or the program it's taken from ("from Google Chrome").
fn icon_in_words(home: &Home, icon: &str) -> String {
    let icon = icon.trim();
    // "file,number": one of the icons inside a program or library.
    let file = match icon.rsplit_once(',') {
        Some((file, number)) if number.trim().parse::<i32>().is_ok() => file.trim(),
        _ => icon,
    };
    let path = home.resolve(file);
    let lower = path.to_ascii_lowercase();
    let file_name = Path::new(&path).file_name().map_or_else(|| file.to_string(), |name| name.to_string_lossy().into_owned());
    let windows_folder = std::env::var("SystemRoot").map_or_else(|_| r"c:\windows".to_string(), |root| root.to_ascii_lowercase());
    let in_windows = lower.starts_with(&windows_folder) || file.to_ascii_lowercase().starts_with(r"system32\");
    let program = [".exe", ".dll", ".icl", ".cpl", ".mun"].iter().any(|kind| lower.ends_with(kind));
    match (program, in_windows) {
        (true, true) => "one of Windows' own".into(),
        (true, false) => format!("from {}", system::program_name(&path).unwrap_or(file_name)),
        (false, _) => file_name,
    }
}

/// "1 item", "3 items".
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn set_text(hwnd: HWND, id: i32, text: &str) {
    unsafe {
        let _ = SetDlgItemTextW(hwnd, id, &HSTRING::from(text));
    }
}

fn enable(hwnd: HWND, id: i32, on: bool) {
    unsafe {
        if let Ok(control) = GetDlgItem(Some(hwnd), id) {
            let _ = EnableWindow(control, on);
        }
    }
}

/// Below a button, for its menu.
fn below(page: HWND, id: i32) -> POINT {
    let mut rect = RECT::default();
    unsafe {
        if let Ok(button) = GetDlgItem(Some(page), id) {
            let _ = GetWindowRect(button, &mut rect);
        }
    }
    POINT { x: rect.left, y: rect.bottom }
}

/// A small menu under a button; returns the chosen entry (0 if none). An entry with id 0 is a
/// dividing line.
fn pop_menu(owner: HWND, at: POINT, entries: &[(usize, String)]) -> usize {
    pop_menu_with(owner, at, entries, None)
}

/// The same, with a submenu at the end: its name and entries.
fn pop_menu_with(owner: HWND, at: POINT, entries: &[(usize, String)], sub: Option<(&str, &[(usize, String)])>) -> usize {
    unsafe fn fill(menu: HMENU, entries: &[(usize, String)]) {
        for (id, text) in entries {
            unsafe {
                if *id == 0 {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                } else {
                    let _ = AppendMenuW(menu, MF_STRING, *id, &HSTRING::from(text.replace('&', "&&")));
                }
            }
        }
    }
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        fill(menu, entries);
        if let Some((name, sub_entries)) = sub {
            if let Ok(submenu) = CreatePopupMenu() {
                fill(submenu, sub_entries);
                let _ = AppendMenuW(menu, MF_POPUP, submenu.0 as usize, &HSTRING::from(name));
            }
        }
        let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, at.x, at.y, None, owner, None);
        let _ = DestroyMenu(menu); // and the submenu with it
        choice.0 as usize
    }
}

fn tell_dock(message: u32, wparam: usize, lparam: isize) {
    unsafe {
        if let Ok(dock) = FindWindowW(w!("DesktopDock"), w!("Desktop Dock")) {
            let _ = PostMessageW(Some(dock), message, WPARAM(wparam), LPARAM(lparam));
        }
    }
}

/// Tells the running dock to reload dock.toml now (rather than when it notices the save), and
/// to come out to show it if it's something you can see.
fn nudge_dock(show: bool) {
    tell_dock(DOCK_RELOAD_NOW, 0, 0);
    if show {
        tell_dock(DOCK_PEEK, 0, 0);
    }
}

/// Our window of this kind, if one is open.
fn find_open(mode: Mode) -> Option<HWND> {
    let mut after = None;
    unsafe {
        while let Ok(found) = FindWindowExW(None, after, w!("#32770"), PCWSTR::null()) {
            if GetPropW(found, MARK).0 as usize == mode.mark() {
                return Some(found);
            }
            after = Some(found);
        }
    }
    None
}

/// Whether two versions of an item differ in anything a properties window sets.
fn same_properties(a: &ItemConfig, b: &ItemConfig) -> bool {
    (&a.name, &a.target, &a.args, &a.start_in, &a.icon, a.run, a.admin) == (&b.name, &b.target, &b.args, &b.start_in, &b.icon, b.run, b.admin)
}

fn run_word(run: RunState) -> &'static str {
    match run {
        RunState::Normal => "",
        RunState::Minimized => "minimized",
        RunState::Maximized => "maximized",
    }
}

/// How much is in a folder (and everything in it), in bytes.
fn folder_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => folder_size(&entry.path()),
            _ => entry.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// How many items a dock.toml holds (groups' items counted too).
fn item_count(text: &str) -> Option<usize> {
    let parsed = config::parse(text).ok()?;
    Some(parsed.config.items.iter().map(|item| 1 + item.items.len()).sum())
}

/// A list's selected row, if any.
fn selected_row(list: HWND) -> Option<usize> {
    let row = unsafe { SendMessageW(list, LVM_GETNEXTITEM, Some(WPARAM(usize::MAX)), Some(LPARAM(LVNI_SELECTED))).0 };
    usize::try_from(row).ok()
}

/// Sets up a report list's columns: (title, share of the width).
fn columns(list: HWND, titles: &[(PCWSTR, f32)]) {
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(list, &mut client);
        let full_row = LVS_EX_FULLROWSELECT as usize;
        SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, Some(WPARAM(full_row)), Some(LPARAM(full_row as isize)));
        // Room left for the scroll bar, so the columns never need a sideways one.
        let width = ((client.right - client.left) as f32 * 0.93).max(100.0);
        for (k, (title, share)) in titles.iter().enumerate() {
            let column = LVCOLUMNW { mask: LVCF_TEXT | LVCF_WIDTH, cx: (width * share) as i32, pszText: PWSTR(title.as_ptr() as *mut u16), ..Default::default() };
            SendMessageW(list, LVM_INSERTCOLUMNW, Some(WPARAM(k)), Some(LPARAM(&column as *const _ as isize)));
        }
    }
}

/// Fills a report list, one row of texts per entry.
fn fill_rows(list: HWND, rows: &[Vec<String>]) {
    unsafe {
        SendMessageW(list, LVM_DELETEALLITEMS, None, None);
        for (r, row) in rows.iter().enumerate() {
            for (c, text) in row.iter().enumerate() {
                let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                let item = LVITEMW { mask: LVIF_TEXT, iItem: r as i32, iSubItem: c as i32, pszText: PWSTR(wide.as_mut_ptr()), ..Default::default() };
                let message = if c == 0 { LVM_INSERTITEMW } else { LVM_SETITEMTEXTW };
                SendMessageW(list, message, Some(WPARAM(r)), Some(LPARAM(&item as *const _ as isize)));
            }
        }
    }
}

/// Opens the window (or brings the open one of its kind forward, showing the item asked for).
pub fn run(home: Home, open: Open) -> i32 {
    let (mode, item) = match open {
        Open::Settings(item) => (Mode::Settings, item),
        Open::Properties(spot) | Open::Icon(spot) => (Mode::Properties, Some(spot)),
    };
    let pick_icon = matches!(open, Open::Icon(_));
    unsafe {
        // One window of each kind. Making one takes a moment, when it can't be found yet, so
        // the lock decides (without it, many quick starts could open several). A copy without it
        // waits, up to 5 s, for the other's window to bring forward, or for the other to end
        // (its window was just closed) to open its own.
        let lock = system::ProcessLock::new(mode.lock());
        let mut first = lock.as_ref().is_none_or(|lock| lock.take(Duration::ZERO));
        let mut existing = find_open(mode);
        for _ in 0..50 {
            if first || existing.is_some() {
                break;
            }
            first = lock.as_ref().is_some_and(|lock| lock.take(Duration::from_millis(100)));
            existing = find_open(mode);
        }
        if let Some(existing) = existing {
            if IsIconic(existing).as_bool() {
                let _ = ShowWindow(existing, SW_RESTORE);
            } else if !IsWindowVisible(existing).as_bool() {
                let _ = ShowWindow(existing, SW_SHOW); // started hidden (see below)
            }
            let _ = SetForegroundWindow(existing);
            if let Some(spot) = item {
                let child = spot.child.map_or(0, |c| c as isize + 1);
                let _ = PostMessageW(Some(existing), WM_APP_SELECT, WPARAM(spot.index), LPARAM(child));
            }
            if pick_icon {
                let _ = PostMessageW(Some(existing), WM_APP_PICK_ICON, WPARAM(0), LPARAM(0));
            }
            return 0;
        }
        if !first {
            return 0; // the other copy is stuck: don't add a second window
        }
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES | ICC_BAR_CLASSES | ICC_HOTKEY_CLASS,
        };
        let _ = InitCommonControlsEx(&controls);
        let Ok(instance) = GetModuleHandleW(None) else { return 1 };
        let template = |id: usize| PCWSTR(id as *const u16);
        let mut pages = Vec::new();
        let (window, page) = match mode {
            Mode::Settings => {
                let Ok(window) = CreateDialogParamW(Some(instance.into()), template(IDD_SETTINGS), None, Some(window_proc), LPARAM(0)) else {
                    return 1;
                };
                // Each section's page fills the page area of the window (dialog units to pixels).
                let mut area = RECT { left: 94, top: 26, right: 464, bottom: 272 };
                let _ = MapDialogRect(window, &mut area);
                let sections = GetDlgItem(Some(window), IDC_SECTIONS).ok();
                for (name, id, _) in SECTIONS {
                    let Ok(page) = CreateDialogParamW(Some(instance.into()), template(id), Some(window), Some(page_proc), LPARAM(0)) else {
                        return 1;
                    };
                    let _ = SetWindowPos(page, None, area.left, area.top, area.right - area.left, area.bottom - area.top, SWP_NOZORDER);
                    if let Some(sections) = sections {
                        SendMessageW(sections, LB_ADDSTRING, None, Some(LPARAM(name.as_ptr() as isize)));
                    }
                    pages.push(page);
                }
                for slider in &SLIDERS {
                    let Some(control) = pages.iter().find_map(|&page| GetDlgItem(Some(page), slider.id).ok()) else { continue };
                    let steps = ((slider.max - slider.min) / slider.step).round() as isize;
                    SendMessageW(control, TBM_SETRANGEMIN, Some(WPARAM(0)), Some(LPARAM(0)));
                    SendMessageW(control, TBM_SETRANGEMAX, Some(WPARAM(1)), Some(LPARAM(steps)));
                    SendMessageW(control, TBM_SETLINESIZE, None, Some(LPARAM(1)));
                    SendMessageW(control, TBM_SETPAGESIZE, None, Some(LPARAM((steps / 10).max(1))));
                }
                let combo = |id: i32, choices: &[PCWSTR]| {
                    if let Some(combo) = pages.iter().find_map(|&page| GetDlgItem(Some(page), id).ok()) {
                        for choice in choices {
                            SendMessageW(combo, CB_ADDSTRING, None, Some(LPARAM(choice.as_ptr() as isize)));
                        }
                    }
                };
                combo(IDC_LAUNCH_EFFECT, &[w!("None"), w!("Bounce"), w!("Glow pulse")]);
                combo(IDC_LOOK, &[w!("Light"), w!("Dark")]);
                combo(IDC_HOVER_GLOW, &[w!("None"), w!("Windows accent colour"), w!("Each icon's own colour")]);
                if let Ok(shortcut) = GetDlgItem(Some(pages[SECTION_SCREEN]), IDC_SHORTCUT) {
                    let instead = (hotkey::HOTKEYF_CONTROL | hotkey::HOTKEYF_SHIFT) as isize;
                    SendMessageW(shortcut, HKM_SETRULES, Some(WPARAM((HKCOMB_NONE | HKCOMB_S) as usize)), Some(LPARAM(instead)));
                }
                if let Ok(list) = GetDlgItem(Some(pages[SECTION_REMOVED]), IDC_REMOVED_LIST) {
                    columns(list, &[(w!("Name"), 0.45), (w!("Removed"), 0.30), (w!("Was in"), 0.25)]);
                }
                if let Ok(list) = GetDlgItem(Some(pages[SECTION_VERSIONS]), IDC_VERSIONS_LIST) {
                    columns(list, &[(w!("Saved"), 0.36), (w!("Kind"), 0.44), (w!("Items"), 0.20)]);
                }
                // The finish: the title's and headings' fonts, the About page's icon.
                style(window, &pages);
                if let Ok(icon) = GetDlgItem(Some(pages[SECTION_ABOUT]), IDC_ABOUT_ICON) {
                    let mut rect = RECT::default();
                    let _ = GetClientRect(icon, &mut rect);
                    if let Some(image) = chrome::app_icon(rect.right - rect.left) {
                        SendMessageW(icon, STM_SETICON, Some(WPARAM(image.0 as usize)), None);
                    }
                }
                (window, pages[SECTION_ITEMS])
            }
            Mode::Properties => {
                let Ok(window) = CreateDialogParamW(Some(instance.into()), template(IDD_PROPERTIES), None, Some(properties_proc), LPARAM(0))
                else {
                    return 1;
                };
                (window, window)
            }
        };
        let _ = SetPropW(window, MARK, Some(HANDLE(mode.mark() as *mut c_void)));
        chrome::set_app_icon(window);
        let list_px = 20 * GetDpiForWindow(window).max(96) / 96;
        let preview_px = {
            let mut rect = RECT::default();
            if let Ok(preview) = GetDlgItem(Some(page), IDC_ICON) {
                let _ = GetWindowRect(preview, &mut rect);
            }
            ((rect.bottom - rect.top).clamp(16, 256)) as u32
        };
        let list = match GetDlgItem(Some(page), IDC_LIST) {
            Ok(list) if mode == Mode::Settings => {
                let images = ImageList_Create(list_px as i32, list_px as i32, ILC_COLOR32, 64, 16);
                let full_row = LVS_EX_FULLROWSELECT as usize;
                SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, Some(WPARAM(full_row)), Some(LPARAM(full_row as isize)));
                SendMessageW(list, LVM_SETIMAGELIST, Some(WPARAM(LVSIL_SMALL as usize)), Some(LPARAM(images.0)));
                let column = LVCOLUMNW { mask: LVCF_WIDTH, cx: 100, ..Default::default() };
                SendMessageW(list, LVM_INSERTCOLUMNW, Some(WPARAM(0)), Some(LPARAM(&column as *const _ as isize)));
                Some((list, images))
            }
            _ => None,
        };
        if let Ok(run) = GetDlgItem(Some(page), IDC_RUN) {
            for choice in [w!("Normal window"), w!("Minimised"), w!("Maximised")] {
                SendMessageW(run, CB_ADDSTRING, None, Some(LPARAM(choice.as_ptr() as isize)));
            }
        }

        // Names for screen readers where the label before a control doesn't give a good one.
        if let Ok(sections) = GetDlgItem(Some(window), IDC_SECTIONS) {
            chrome::name_for_screen_readers(sections, "Settings pages");
        }
        for &page in &pages {
            for (id, name) in [
                (IDC_LIST, "Your dock's items"),
                (IDC_TARGET_BROWSE, "Browse for the target"),
                (IDC_START_BROWSE, "Browse for the start-in folder"),
                (IDC_REMOVED_LIST, "Recently removed items"),
                (IDC_VERSIONS_LIST, "Saved versions of your dock"),
            ] {
                if let Ok(control) = GetDlgItem(Some(page), id) {
                    chrome::name_for_screen_readers(control, name);
                }
            }
        }
        let store = Store::new(&home);
        let opening = config::read_text(store.config_path()).unwrap_or_default();
        let removed = Removed::new(&home);
        let settings = Settings {
            mode,
            home,
            store,
            removed,
            now: opening.clone(),
            opening,
            opening_item: None,
            dock: DockSettings::default(),
            pages,
            section: 0,
            sliders_pending: false,
            previewed: None,
            screen_choices: Vec::new(),
            items: Vec::new(),
            rows: Vec::new(),
            selected: None,
            following: None,
            removed_list: Vec::new(),
            versions: Vec::new(),
            stamp: None,
            window,
            page,
            list,
            list_px,
            preview_px,
            icons: HashMap::new(),
            image_index: HashMap::new(),
            arrived: Arc::new(Mutex::new(Vec::new())),
        };
        SETTINGS.with(|cell| *cell.borrow_mut() = Some(settings));
        with_settings(|s| {
            s.reload_model();
            s.select(item.unwrap_or(Spot::dock(0)));
            // The settings window opens at Look, or at Items when asked for an item.
            s.show_section(if item.is_some() { SECTION_ITEMS } else { 0 });
        });
        SetTimer(Some(window), TIMER_FILE, 1000, None);
        chrome::apply_theme(window);
        chrome::keep_on_screen(window);
        // Windows applies the way this process was started to its first ShowWindow: a start
        // asked to be hidden (a script, a launcher) would leave a window nobody can see, which
        // every later "open settings" would only bring forward. Asked for, it shows.
        let _ = ShowWindow(window, SW_SHOW);
        if !IsWindowVisible(window).as_bool() {
            let _ = ShowWindow(window, SW_SHOW);
        }
        let _ = SetForegroundWindow(window);
        if pick_icon {
            let _ = PostMessageW(Some(window), WM_APP_PICK_ICON, WPARAM(1), LPARAM(0));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if !IsDialogMessageW(window, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        SETTINGS.with(|cell| cell.borrow_mut().take());
    }
    0
}

/// The settings window's finish: the title's and headings' fonts and the section list's rows,
/// for the window's scaling (again when it moves to a screen with different scaling).
fn style(window: HWND, pages: &[HWND]) {
    unsafe {
        if let Ok(title) = GetDlgItem(Some(window), IDC_TITLE) {
            chrome::style_title(title);
        }
        for &page in pages {
            chrome::style_headings(page, IDC_HEADING);
        }
        if let Some(&about) = pages.get(SECTION_ABOUT) {
            chrome::style_headings(about, IDC_ABOUT_NAME);
        }
        if let Ok(sections) = GetDlgItem(Some(window), IDC_SECTIONS) {
            SendMessageW(sections, LB_SETITEMHEIGHT, Some(WPARAM(0)), Some(LPARAM(chrome::row_height(window) as isize)));
            let _ = InvalidateRect(Some(sections), None, true);
        }
    }
}

/// Runs `f` on the window's state. Messages sent while it's already in use (the list reporting
/// a selection made by code, say) are skipped: they're not the user's doing.
fn with_settings<R>(f: impl FnOnce(&mut Settings) -> R) -> Option<R> {
    SETTINGS.with(|cell| cell.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

/// The window itself: status line, Revert and Close (and in the settings window, sections).
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    // Light or dark, as Windows is set (the section list has its own background, below).
    if msg != WM_CTLCOLORLISTBOX {
        if let Some(result) = chrome::themed_message(hwnd, msg, wparam, lparam) {
            return result;
        }
    }
    match msg {
        WM_INITDIALOG => 1,
        // The section list, drawn here (chrome.rs). Measured as the list is made.
        WM_MEASUREITEM if wparam.0 == IDC_SECTIONS as usize => {
            let measure = unsafe { &mut *(lparam.0 as *mut MEASUREITEMSTRUCT) };
            measure.itemHeight = chrome::row_height(hwnd);
            1
        }
        WM_DRAWITEM if wparam.0 == IDC_SECTIONS as usize => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            if let Some((name, _, symbol)) = SECTIONS.get(item.itemID as usize) {
                chrome::draw_row(item, unsafe { name.as_wide() }, *symbol);
            }
            1
        }
        // The list sits on the window's own background, not a white box.
        WM_CTLCOLORLISTBOX => chrome::window_brush().0 as isize,
        // Moved to a screen with different scaling: Windows rescales the window and its controls
        // (its own handling, so not handled here), and the fonts set here follow just after.
        WM_DPICHANGED => {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_APP_RESTYLE, WPARAM(0), LPARAM(0));
            }
            0
        }
        WM_APP_RESTYLE => {
            with_settings(|s| style(s.window, &s.pages));
            1
        }
        // High Contrast turned on or off: the section list redraws in the new colours.
        WM_SYSCOLORCHANGE => {
            if let Ok(sections) = unsafe { GetDlgItem(Some(hwnd), IDC_SECTIONS) } {
                unsafe {
                    let _ = InvalidateRect(Some(sections), None, true);
                }
            }
            0
        }
        WM_COMMAND => {
            match (wparam.0 & 0xFFFF) as i32 {
                // Enter, in a text box: save it now.
                id if id == IDOK.0 => {
                    with_settings(|s| s.save_focused());
                }
                // Esc: first puts back a text box you're typing in; otherwise (and Close) closes.
                id if id == IDCANCEL.0 => {
                    if with_settings(|s| s.discard_typing()) != Some(true) {
                        close(hwnd);
                    }
                }
                IDC_REVERT => {
                    with_settings(|s| s.revert());
                }
                IDC_SECTIONS if ((wparam.0 >> 16) & 0xFFFF) as u32 == LBN_SELCHANGE => {
                    with_settings(|s| s.section_chosen());
                }
                _ => {}
            }
            1
        }
        WM_TIMER if wparam.0 == TIMER_FILE => {
            with_settings(|s| s.check_file());
            1
        }
        WM_TIMER if wparam.0 == TIMER_SLIDERS => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_SLIDERS);
            }
            with_settings(|s| s.save_sliders());
            1
        }
        WM_APP_SELECT => {
            let spot = Spot { index: wparam.0, child: usize::try_from(lparam.0 - 1).ok() };
            with_settings(|s| {
                s.save_focused();
                s.show_section(SECTION_ITEMS);
                s.select(spot);
            });
            1
        }
        WM_APP_PICK_ICON => {
            if let Some(ask) = with_settings(|s| s.command(IDC_ICON_CHANGE, BN_CLICKED)).flatten() {
                answer(ask);
            }
            if wparam.0 == 1 {
                close(hwnd); // it was opened just for the picker
            }
            1
        }
        WM_CLOSE => {
            close(hwnd);
            1
        }
        WM_DESTROY => unsafe {
            let _ = KillTimer(Some(hwnd), TIMER_FILE);
            let _ = KillTimer(Some(hwnd), TIMER_SLIDERS);
            let _ = RemovePropW(hwnd, MARK);
            PostQuitMessage(0);
            1
        },
        _ => 0,
    }
}

/// Closing keeps whatever was typed last (as leaving the box would), and a slider just moved.
fn close(hwnd: HWND) {
    with_settings(|s| {
        s.save_focused();
        s.save_sliders();
    });
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// The pages' controls: the property boxes, the lists, sliders, buttons.
unsafe extern "system" fn page_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    if msg == WM_CTLCOLORSTATIC && unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) } == IDC_HEART {
        unsafe {
            let dc = HDC(wparam.0 as *mut c_void);
            SetTextColor(dc, COLORREF(0x004E34E8)); // a warm red (BGR)
            SetBkMode(dc, TRANSPARENT);
        }
        return chrome::window_brush().0 as isize;
    }
    if let Some(result) = chrome::themed_message(hwnd, msg, wparam, lparam) {
        return result;
    }
    match msg {
        WM_INITDIALOG => 1,
        WM_NOTIFY => {
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            if header.idFrom != IDC_LIST as usize {
                return 0;
            }
            if header.code == LVN_ITEMCHANGED {
                let change = unsafe { &*(lparam.0 as *const NMLISTVIEW) };
                let selected_now = change.uNewState & LVIS_SELECTED.0 != 0 && change.uOldState & LVIS_SELECTED.0 == 0;
                if selected_now && change.iItem >= 0 {
                    with_settings(|s| {
                        if let Some(&spot) = s.rows.get(change.iItem as usize) {
                            s.select(spot);
                        }
                    });
                }
            } else if header.code == LVN_KEYDOWN {
                let key = unsafe { &*(lparam.0 as *const NMLVKEYDOWN) };
                if key.wVKey == VK_DELETE.0 {
                    with_settings(|s| s.remove_selected());
                }
            }
            0
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let code = ((wparam.0 >> 16) & 0xFFFF) as u32;
            if let Some(ask) = with_settings(|s| s.command(id, code)).flatten() {
                answer(ask);
            }
            1
        }
        WM_DRAWITEM if wparam.0 == IDC_ICON as usize => {
            let item = unsafe { &*(lparam.0 as *const DRAWITEMSTRUCT) };
            with_settings(|s| s.draw_preview(item));
            1
        }
        WM_HSCROLL => {
            let id = unsafe { GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) };
            with_settings(|s| s.slider_moved(id, (wparam.0 & 0xFFFF) as u32));
            1
        }
        WM_APP_ICONS => {
            with_settings(|s| s.receive_icons());
            1
        }
        _ => 0,
    }
}

/// The properties window is both: the window and its property controls in one dialog.
unsafe extern "system" fn properties_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    let id = (wparam.0 & 0xFFFF) as i32;
    let for_window = match msg {
        WM_COMMAND => id == IDOK.0 || id == IDCANCEL.0 || id == IDC_REVERT,
        WM_TIMER | WM_APP_SELECT | WM_APP_PICK_ICON | WM_CLOSE | WM_DESTROY => true,
        _ => false,
    };
    unsafe { if for_window { window_proc(hwnd, msg, wparam, lparam) } else { page_proc(hwnd, msg, wparam, lparam) } }
}

/// Dialogs and menus run here, outside the borrow (they run their own message loops); their
/// answers go back in. The window owns each dialog, so it waits while one is open.
fn answer(ask: Ask) {
    let Some((window, page, home)) = with_settings(|s| (s.window, s.page, s.home.clone())) else { return };
    let item = |text: &str| text.to_string();
    match ask {
        Ask::Target(spot, expect, start) => {
            if let Some(path) = pickers::choose_target(Some(window), &start) {
                with_settings(|s| s.set_field(spot, &expect, "target", &path, "Target"));
            }
        }
        Ask::StartIn(spot, expect, start) => {
            if let Some(path) = pickers::choose_folder(Some(window), &start) {
                with_settings(|s| s.set_field(spot, &expect, "start_in", &path, "Start in"));
            }
        }
        Ask::Icon(spot, expect, stored, current, program) => {
            // Each icon clicked is saved, so the dock shows it; Cancel puts the stored one back.
            let title = format!("Choose an icon for {expect}");
            let request = iconpicker::Request { owner: window, title, program, current };
            let label = expect.clone();
            let kept = iconpicker::choose(request, move |icon| {
                with_settings(|s| s.set_field(spot, &label, "icon", icon, "Icon"));
            });
            if !kept {
                with_settings(|s| {
                    s.set_field(spot, &expect, "icon", &stored, "Icon");
                    s.status("Icon unchanged.");
                });
            }
        }
        Ask::Add(gap, at, has_bin) => {
            // Into a group: files and folders (a group holds things to open).
            let mut entries = vec![(ADD_FILE, item("File...")), (ADD_FOLDER, item("Folder..."))];
            if gap.child.is_none() {
                entries.push((ADD_SEPARATOR, item("Separator")));
                if !has_bin {
                    entries.push((ADD_BIN, item("Recycle Bin")));
                }
            }
            // Windows items (This PC, Downloads, Settings…): into a group too.
            let windows = places::all();
            let windows_entries: Vec<(usize, String)> =
                windows.iter().enumerate().map(|(k, place)| place.as_ref().map_or((0, String::new()), |place| (ADD_WINDOWS + k, place.name.clone()))).collect();
            let new: Vec<ItemConfig> = match pop_menu_with(page, at, &entries, Some(("Windows item", &windows_entries))) {
                choice if (ADD_WINDOWS..ADD_WINDOWS + windows.len()).contains(&choice) => windows[choice - ADD_WINDOWS].iter().cloned().collect(),
                ADD_FILE => pickers::choose_files(Some(window)).iter().map(|path| drop::item_for_path(path, &home)).collect(),
                ADD_FOLDER => pickers::choose_folder(Some(window), "").iter().map(|path| drop::item_for_path(path, &home)).collect(),
                ADD_SEPARATOR => {
                    let mut separator = ItemConfig::default();
                    separator.kind = Kind::Separator;
                    vec![separator]
                }
                ADD_BIN => {
                    let mut bin = ItemConfig::new("Recycle Bin", "");
                    bin.kind = Kind::RecycleBin;
                    vec![bin]
                }
                _ => Vec::new(),
            };
            if !new.is_empty() {
                with_settings(|s| s.add(gap, &new));
            }
        }
        Ask::Group(at, entries) => {
            let choice = pop_menu(page, at, &entries);
            if choice != 0 {
                with_settings(|s| s.group_action(choice));
            }
        }
        Ask::SaveCopy => {
            if let Some(path) = pickers::choose_save(Some(window), "Save a copy of your dock", "My dock.toml", "Dock files", "*.toml") {
                with_settings(|s| s.save_copy(&path));
            }
        }
        Ask::LoadCopy => {
            if let Some(path) = pickers::choose_file(Some(window), "Load a copy of a dock", "Dock files", "*.toml") {
                with_settings(|s| s.load_copy(&path));
            }
        }
        Ask::ImportRocketDock(at) => {
            // From RocketDock's settings on this PC (if it's here), or from a .reg export of them.
            let here = import::RocketDock::from_registry().ok().filter(|rocketdock| !rocketdock.icons.is_empty());
            let mut entries = Vec::new();
            if here.is_some() {
                entries.push((1, item("From RocketDock on this PC")));
            }
            entries.push((2, item("From a RocketDock settings file (.reg)...")));
            let source = match pop_menu(page, at, &entries) {
                1 => here.map(Ok),
                2 => pickers::choose_file(Some(window), "RocketDock's settings", "Registry files", "*.reg")
                    .map(|path| import::RocketDock::from_reg_file(Path::new(&path))),
                _ => None,
            };
            if let Some(source) = source {
                with_settings(|s| s.import_rocketdock(source));
            }
        }
    }
}

impl Settings {
    fn status(&self, text: &str) {
        set_text(self.window, IDC_STATUS, &system::plain(text));
    }

    /// The item at a place on the dock or in a group.
    fn at(&self, spot: Spot) -> Option<&ItemConfig> {
        let top = self.items.get(spot.index)?;
        match spot.child {
            None => Some(top),
            Some(child) => top.items.get(child),
        }
    }

    fn label(&self, spot: Spot) -> Option<String> {
        self.at(spot).map(edit::label_of)
    }

    /// How many items share a list with `spot`: the dock's, or its group's.
    fn siblings(&self, spot: Spot) -> usize {
        match spot.child {
            None => self.items.len(),
            Some(_) => self.items.get(spot.index).map_or(0, |group| group.items.len()),
        }
    }

    /// Re-reads dock.toml (after a change here, or one made elsewhere) and updates the window.
    fn reload_model(&mut self) {
        let path = self.store.config_path().to_path_buf();
        self.stamp = modified(&path);
        let text = config::read_text(&path).unwrap_or_default();
        match config::parse(&text) {
            Ok(parsed) => {
                self.items = parsed.config.items;
                self.dock = parsed.config.dock;
            }
            Err(e) => {
                self.status(&format!("Your dock's file can't be read right now: {e}"));
                return;
            }
        }
        self.now = text;
        self.rows = self
            .items
            .iter()
            .enumerate()
            .flat_map(|(index, item)| std::iter::once(Spot::dock(index)).chain((0..item.items.len()).map(move |child| Spot::in_group(index, child))))
            .collect();
        self.follow();
        self.fill_list();
        self.load_icons();
        self.fill_properties();
        self.fill_sections();
        self.fill_page(self.section);
    }

    // ---- Sections ---------------------------------------------------------------------------

    fn show_section(&mut self, section: usize) {
        if section >= self.pages.len() {
            return;
        }
        self.section = section;
        for (k, &page) in self.pages.iter().enumerate() {
            unsafe {
                let _ = ShowWindow(page, if k == section { SW_SHOW } else { SW_HIDE });
            }
        }
        unsafe {
            if let Ok(sections) = GetDlgItem(Some(self.window), IDC_SECTIONS) {
                SendMessageW(sections, LB_SETCURSEL, Some(WPARAM(section)), None);
            }
            let _ = SetDlgItemTextW(self.window, IDC_TITLE, SECTIONS[section].0);
        }
        self.fill_page(section);
    }

    /// The pages that show lists of things outside dock.toml are filled as they're shown.
    fn fill_page(&mut self, section: usize) {
        match section {
            SECTION_REMOVED if self.mode == Mode::Settings => self.fill_removed(),
            SECTION_VERSIONS if self.mode == Mode::Settings => self.fill_versions(),
            SECTION_FILES if self.mode == Mode::Settings => self.fill_files(),
            SECTION_ABOUT if self.mode == Mode::Settings => self.fill_about(),
            SECTION_SCREEN if self.mode == Mode::Settings => self.fill_screen(),
            _ => {}
        }
    }

    /// Screen: which screen the dock is on (the main one, or a chosen one), what to know about
    /// reaching its top edge, and the keyboard shortcut.
    fn fill_screen(&mut self) {
        let Some(&page) = self.pages.get(SECTION_SCREEN) else { return };
        let screens = system::screens();
        let chosen = self.dock.monitor.clone();
        let mut choices: Vec<(String, String)> = vec![("main".into(), "The main screen".into())];
        for screen in &screens {
            let (width, height) = (screen.monitor.right - screen.monitor.left, screen.monitor.bottom - screen.monitor.top);
            let main = if screen.main { " (the main screen)" } else { "" };
            choices.push((screen.id.clone(), format!("Screen {}: {width} × {height}{main}", screen.number)));
        }
        let mut selected = if chosen.eq_ignore_ascii_case("main") { Some(0) } else { choices.iter().position(|(id, _)| id.eq_ignore_ascii_case(&chosen)) };
        if selected.is_none() {
            choices.push((chosen.clone(), "A screen that isn't plugged in (the main screen meanwhile)".into()));
            selected = Some(choices.len() - 1);
        }
        unsafe {
            if let Ok(combo) = GetDlgItem(Some(page), IDC_SCREEN_LIST) {
                SendMessageW(combo, CB_RESETCONTENT, None, None);
                for (_, label) in &choices {
                    let label = HSTRING::from(label.as_str());
                    SendMessageW(combo, CB_ADDSTRING, None, Some(LPARAM(label.as_ptr() as isize)));
                }
                SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(selected.unwrap_or(0))), None);
            }
            if let Ok(shortcut) = GetDlgItem(Some(page), IDC_SHORTCUT) {
                let value = hotkey::parse(&self.dock.hotkey).ok().flatten().map_or(0, hotkey::to_control);
                SendMessageW(shortcut, HKM_SETHOTKEY, Some(WPARAM(value as usize)), None);
            }
        }
        self.screen_choices = choices.into_iter().map(|(id, _)| id).collect();
        set_text(page, IDC_SCREEN_NOTE, system::choose_screen(&screens, &chosen).and_then(system::top_edge_note).unwrap_or(""));
        set_text(page, IDC_SHORTCUT_NOTE, "");
    }

    /// The shortcut box changed. Once it holds a whole shortcut that no other program uses,
    /// it's saved (and the dock takes it straight away).
    fn shortcut_changed(&mut self) {
        let (Some(&page), Some(control)) = (self.pages.get(SECTION_SCREEN), self.control(IDC_SHORTCUT)) else { return };
        let value = unsafe { SendMessageW(control, HKM_GETHOTKEY, None, None).0 } as u32;
        let Some(key) = hotkey::from_control(value) else { return }; // still being pressed (Ctrl, Shift…)
        let text = hotkey::format(key);
        if let Err(e) = hotkey::parse(&text) {
            set_text(page, IDC_SHORTCUT_NOTE, &format!("{e}."));
            return;
        }
        if text == self.dock.hotkey {
            set_text(page, IDC_SHORTCUT_NOTE, "");
            return;
        }
        // Taken by another program? Try it for a moment. (The dock holds the current one, so
        // only a new one is tried.)
        const TRY: i32 = 0xBFFF;
        let free = unsafe { RegisterHotKey(None, TRY, HOT_KEY_MODIFIERS(key.modifiers | hotkey::MOD_NOREPEAT), key.key).is_ok() };
        if !free {
            set_text(page, IDC_SHORTCUT_NOTE, &format!("Another program already uses {text}. Choose another."));
            return;
        }
        unsafe {
            let _ = UnregisterHotKey(None, TRY);
        }
        set_text(page, IDC_SHORTCUT_NOTE, "");
        self.change(&format!("Shortcut {text}"), None, false, |doc| edit::set_dock(doc, "hotkey", text.as_str()));
    }

    /// About: the version, and what the dock is using right now.
    fn fill_about(&mut self) {
        let page = self.pages[SECTION_ABOUT];
        set_text(page, IDC_VERSION, &format!("Version {}", env!("CARGO_PKG_VERSION")));
        let memory = match system::dock_memory() {
            Some((dock, watchdog)) => {
                let watchdog = watchdog.map_or(String::new(), |w| {
                    format!(", plus {w:.1} MB for its watchdog (which starts it again if it ever stops): {:.1} MB in all", dock + w)
                });
                format!("The dock is using {dock:.1} MB of memory{watchdog}. This window runs separately and uses no memory once it's closed.")
            }
            None => "The dock isn't running.".to_string(),
        };
        set_text(page, IDC_MEMORY, &memory);
        set_text(page, IDC_HEART, "\u{2665}");
        unsafe {
            if let (Ok(text), Ok(heart)) = (GetDlgItem(Some(page), IDC_INSPIRED), GetDlgItem(Some(page), IDC_HEART)) {
                chrome::place_after_text(text, heart);
            }
        }
    }

    /// A section picked in the list on the left.
    fn section_chosen(&mut self) {
        let Ok(sections) = (unsafe { GetDlgItem(Some(self.window), IDC_SECTIONS) }) else { return };
        let chosen = unsafe { SendMessageW(sections, LB_GETCURSEL, None, None).0 };
        if let Ok(section) = usize::try_from(chosen) {
            self.show_section(section);
        }
    }

    /// A control on any of the section pages.
    fn control(&self, id: i32) -> Option<HWND> {
        self.pages.iter().find_map(|&page| unsafe { GetDlgItem(Some(page), id).ok() })
    }

    fn checked(&self, id: i32) -> bool {
        self.pages.iter().any(|&page| unsafe { GetDlgItem(Some(page), id).is_ok() && IsDlgButtonChecked(page, id) == BST_CHECKED.0 })
    }

    fn set_check(&self, id: i32, on: bool) {
        for &page in &self.pages {
            unsafe {
                if GetDlgItem(Some(page), id).is_ok() {
                    let _ = CheckDlgButton(page, id, if on { BST_CHECKED } else { BST_UNCHECKED });
                }
            }
        }
    }

    // ---- Look and Behaviour -----------------------------------------------------------------

    /// Where a slider is now, in the setting's units (snapped to its step).
    fn slider_value(&self, slider: &Slider) -> Option<f32> {
        let control = self.control(slider.id)?;
        let position = unsafe { SendMessageW(control, TBM_GETPOS, None, None).0 } as f32;
        Some((slider.min + position * slider.step).clamp(slider.min, slider.max))
    }

    fn show_value(&self, slider: &Slider, value: f32) {
        if let Some(label) = self.control(slider.id + 1) {
            unsafe {
                let _ = SetWindowTextW(label, &HSTRING::from(shown(slider, value)));
            }
        }
    }

    /// The Look and Behaviour pages, from the file (a slider you've just moved stays where it is
    /// until it's saved).
    fn fill_sections(&mut self) {
        if self.pages.is_empty() {
            return;
        }
        if !self.sliders_pending {
            for slider in &SLIDERS {
                let value = dock_value(&self.dock, slider.key);
                if let Some(control) = self.control(slider.id) {
                    let position = ((value.clamp(slider.min, slider.max) - slider.min) / slider.step).round() as isize;
                    unsafe {
                        SendMessageW(control, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(position)));
                    }
                }
                self.show_value(slider, value);
            }
        }
        self.set_check(IDC_SHOW_LABELS, self.dock.show_labels);
        self.set_check(IDC_LOCKED, self.dock.locked);
        self.set_check(IDC_STARTUP, system::starts_with_windows());
        let pick = |id: i32, choice: usize| {
            if let Some(combo) = self.control(id) {
                unsafe {
                    SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(choice)), None);
                }
            }
        };
        pick(
            IDC_LAUNCH_EFFECT,
            match self.dock.launch_effect {
                LaunchEffect::None => 0,
                LaunchEffect::Bounce => 1,
                LaunchEffect::Glow => 2,
            },
        );
        pick(IDC_LOOK, if self.dock.look == DockLook::Dark { 1 } else { 0 });
        pick(
            IDC_HOVER_GLOW,
            match self.dock.hover_glow {
                HoverGlow::None => 0,
                HoverGlow::Accent => 1,
                HoverGlow::Icon => 2,
            },
        );
        if let Some(label_size) = self.control(IDC_LABEL_SIZE) {
            unsafe {
                let _ = EnableWindow(label_size, self.dock.show_labels);
            }
        }
    }

    /// A slider moved: its value shows at once; it's saved once it settles. Sizes (which make
    /// new icons) wait until you let go.
    fn slider_moved(&mut self, id: i32, code: u32) {
        let Some(slider) = SLIDERS.iter().find(|slider| slider.id == id) else { return };
        if let Some(value) = self.slider_value(slider) {
            self.show_value(slider, value);
            // The dock shows it straight away (saving waits until the slider settles).
            if let Some(which) = PREVIEWED.iter().position(|key| *key == slider.key) {
                if self.previewed != Some((which, value)) {
                    self.previewed = Some((which, value));
                    tell_dock(DOCK_PREVIEW, which, (value * 10.0).round() as isize);
                }
            }
        }
        self.sliders_pending = true;
        if code == TB_ENDTRACK {
            // Let go (or a key released): save now.
            unsafe {
                let _ = KillTimer(Some(self.window), TIMER_SLIDERS);
            }
            self.save_sliders();
        } else if !(slider.heavy && code == TB_THUMBTRACK) {
            // Still moving: save once it settles. A size waits until you let go.
            unsafe {
                SetTimer(Some(self.window), TIMER_SLIDERS, SLIDER_SETTLE_MS, None);
            }
        }
    }

    /// Saves every slider that differs from the file, in one change. The zoom size never goes
    /// below the icon size: moving the icon size past it takes it along.
    fn save_sliders(&mut self) {
        if !self.sliders_pending {
            return;
        }
        self.sliders_pending = false;
        let mut values: Vec<(&Slider, f32)> = SLIDERS.iter().filter_map(|slider| self.slider_value(slider).map(|v| (slider, v))).collect();
        let icon = values.iter().find(|(slider, _)| slider.key == "icon_size").map(|&(_, v)| v);
        if let (Some(icon), Some((_, zoom))) = (icon, values.iter_mut().find(|(slider, _)| slider.key == "zoom_size")) {
            *zoom = zoom.max(icon);
        }
        let changed: Vec<(&Slider, f32)> = values.into_iter().filter(|(slider, v)| (v - dock_value(&self.dock, slider.key)).abs() > 0.001).collect();
        if changed.is_empty() {
            self.fill_sections();
            return;
        }
        let what = changed.iter().map(|(slider, _)| slider.what).collect::<Vec<_>>().join(", ");
        let show = changed.iter().any(|(slider, _)| PREVIEWED.contains(&slider.key));
        self.change(&what, None, show, |doc| {
            for (slider, value) in &changed {
                let key = if slider.key == "spacing" && crate::breaks::broken("settings.wrong-key") { "label_size" } else { slider.key };
                edit::set_dock(doc, key, toml_number(*value))?;
            }
            Ok(())
        });
    }

    // ---- Items ------------------------------------------------------------------------------

    /// Keeps showing the same item if it moved (the dock rearranged, or it went into a group).
    /// The properties window says so if it's gone; the settings window stays at the same row.
    fn follow(&mut self) {
        let Some(label) = self.following.clone() else { return };
        if self.selected.and_then(|spot| self.label(spot)).as_deref() == Some(label.as_str()) {
            return;
        }
        let was_row = self.selected.and_then(|spot| self.rows.iter().position(|&row| row == spot)).unwrap_or(0);
        let nearest = (0..self.rows.len())
            .filter(|&row| self.label(self.rows[row]).as_deref() == Some(label.as_str()))
            .min_by_key(|&row| row.abs_diff(was_row));
        match (nearest, self.mode) {
            (Some(row), _) => self.selected = Some(self.rows[row]),
            (None, Mode::Properties) => {
                self.selected = None;
                self.status(&format!("{label} isn't on the dock any more."));
            }
            (None, Mode::Settings) => self.selected = self.rows.get(was_row.min(self.rows.len().saturating_sub(1))).copied(),
        }
    }

    /// The settings window's list: every item in dock order, a group's items indented under it.
    /// Rows are updated in place when the count is unchanged, so the scroll position stays put.
    fn fill_list(&mut self) {
        let Some((list, _)) = self.list else { return };
        unsafe {
            let count = SendMessageW(list, LVM_GETITEMCOUNT, None, None).0 as usize;
            let rebuild = count != self.rows.len();
            if rebuild {
                SendMessageW(list, LVM_DELETEALLITEMS, None, None);
            }
            for (row, &spot) in self.rows.iter().enumerate() {
                let Some(item) = self.at(spot) else { continue };
                let label = match item.kind {
                    Kind::Separator => "(separator)".to_string(),
                    _ => edit::label_of(item),
                };
                let mut text: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
                let image = self.image_index.get(&self.sources(item)).copied().unwrap_or(-1);
                let entry = LVITEMW {
                    mask: LVIF_TEXT | LVIF_IMAGE | LVIF_INDENT,
                    iItem: row as i32,
                    pszText: PWSTR(text.as_mut_ptr()),
                    iImage: image,
                    iIndent: if spot.child.is_some() { 1 } else { 0 },
                    ..Default::default()
                };
                let message = if rebuild { LVM_INSERTITEMW } else { LVM_SETITEMW };
                SendMessageW(list, message, None, Some(LPARAM(&entry as *const _ as isize)));
            }
            SendMessageW(list, LVM_SETCOLUMNWIDTH, Some(WPARAM(0)), Some(LPARAM(-2))); // LVSCW_AUTOSIZE_USEHEADER
        }
        if let Some(spot) = self.selected {
            self.mark_selected(spot);
        }
    }

    fn mark_selected(&self, spot: Spot) {
        let (Some((list, _)), Some(row)) = (self.list, self.rows.iter().position(|&row| row == spot)) else { return };
        let both = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0);
        let state = LVITEMW { mask: LVIF_STATE, state: both, stateMask: both, ..Default::default() };
        unsafe {
            SendMessageW(list, LVM_SETITEMSTATE, Some(WPARAM(row)), Some(LPARAM(&state as *const _ as isize)));
            SendMessageW(list, LVM_ENSUREVISIBLE, Some(WPARAM(row)), Some(LPARAM(0)));
        }
    }

    /// Shows an item: selected in the list, or (properties window) the one the window is about,
    /// from now on with its own Revert.
    fn select(&mut self, spot: Spot) {
        let spot = if self.at(spot).is_some() {
            spot
        } else {
            // Not there (any more): the nearest place that is.
            match self.rows.iter().rev().find(|row| (row.index, row.child) <= (spot.index, spot.child)) {
                Some(&row) => row,
                None => match self.rows.first() {
                    Some(&row) => row,
                    None => {
                        self.selected = None;
                        self.fill_properties();
                        return;
                    }
                },
            }
        };
        self.selected = Some(spot);
        self.following = self.label(spot);
        if self.mode == Mode::Properties {
            self.opening_item = self.at(spot).cloned();
            self.load_icons();
        }
        self.mark_selected(spot);
        self.fill_properties();
    }

    /// The property controls, for the selected item. A separator has nothing to set; the
    /// Recycle Bin and groups have a name and an icon.
    fn fill_properties(&mut self) {
        let page = self.page;
        let item = self.selected.and_then(|spot| self.at(spot)).cloned();
        let kind = item.as_ref().map(|item| item.kind);
        let opens_something = kind == Some(Kind::App);
        let named = matches!(kind, Some(Kind::App | Kind::RecycleBin | Kind::Group));
        if self.mode == Mode::Properties {
            let title = match &item {
                Some(item) => format!("{} properties", edit::label_of(item)),
                None => "Properties".into(),
            };
            unsafe {
                let _ = SetWindowTextW(self.window, &HSTRING::from(title));
            }
        }
        let item = item.unwrap_or_default();
        for (id, key, value) in [(IDC_NAME, "name", &item.name), (IDC_TARGET, "target", &item.target), (IDC_ARGS, "args", &item.args), (IDC_START_IN, "start_in", &item.start_in)] {
            set_text(page, id, &shown_value(key, value));
        }
        // A Windows item (This PC, Settings, the Recycle Bin) or an app from the Start menu: what
        // it is, in words, and nothing to set about how it opens.
        let windows_item = kind.is_some() && places::described(item.launch_target()).is_some();
        if kind == Some(Kind::RecycleBin) {
            set_text(page, IDC_TARGET, &shown_value("target", item.launch_target()));
        }
        unsafe {
            if let Ok(run) = GetDlgItem(Some(page), IDC_RUN) {
                let choice = match item.run {
                    RunState::Normal => 0,
                    RunState::Minimized => 1,
                    RunState::Maximized => 2,
                };
                SendMessageW(run, CB_SETCURSEL, Some(WPARAM(choice)), None);
            }
            let _ = CheckDlgButton(page, IDC_ADMIN, if item.admin { BST_CHECKED } else { BST_UNCHECKED });
        }
        for id in [IDC_TARGET, IDC_TARGET_BROWSE, IDC_ARGS, IDC_START_IN, IDC_START_BROWSE, IDC_RUN, IDC_ADMIN] {
            enable(page, id, opens_something && !windows_item);
        }
        let there = opens_something && Path::new(&places::on_disk(&self.home, item.launch_target())).exists();
        enable(page, IDC_OPEN_LOCATION, there);
        enable(page, IDC_NAME, named);
        enable(page, IDC_ICON_CHANGE, named);
        enable(page, IDC_ICON_RESET, named && !item.icon.trim().is_empty());
        let spot = self.selected;
        let place = spot.map(|spot| spot.child.unwrap_or(spot.index));
        let siblings = spot.map_or(0, |spot| self.siblings(spot));
        enable(page, IDC_REMOVE, spot.is_some());
        enable(page, IDC_UP, place.is_some_and(|p| p > 0));
        enable(page, IDC_DOWN, place.is_some_and(|p| p + 1 < siblings));
        enable(page, IDC_GROUP, kind.is_some_and(|kind| kind != Kind::Separator));
        let group_name = spot.and_then(|spot| spot.child.and(self.items.get(spot.index))).map(edit::label_of);
        let note = match (kind, group_name) {
            (None, _) => String::new(),
            (Some(Kind::Separator), _) => "A separator: nothing to set. Move it with Up and Down.".into(),
            (Some(Kind::Group), _) => format!(
                "A group with {}. Its icon shows the first four, unless you give it one of its own.",
                plural(item.items.len(), "item", "items")
            ),
            (Some(_), Some(group)) if !item.icon.trim().is_empty() => format!("In {group}. Icon: {}.", icon_in_words(&self.home, &item.icon)),
            (Some(_), Some(group)) => format!("In {group}. Icon: default."),
            (Some(_), None) if !item.icon.trim().is_empty() => format!("Icon: {}.", icon_in_words(&self.home, &item.icon)),
            (Some(Kind::RecycleBin), None) => "Icon: Windows' own, empty or full.".into(),
            (Some(_), None) => "Icon: default.".into(),
        };
        set_text(page, IDC_NOTE, &note);
        let changed = match self.mode {
            Mode::Settings => self.now != self.opening,
            Mode::Properties => self.selected.is_some() && self.opening_item.as_ref().is_some_and(|was| !same_properties(was, &item)),
        };
        enable(self.window, IDC_REVERT, changed);
        unsafe {
            if let Ok(preview) = GetDlgItem(Some(page), IDC_ICON) {
                let _ = InvalidateRect(Some(preview), None, true);
            }
        }
    }

    /// What the text box `id` holds in the file now, if it's one of the text boxes.
    fn saved_value(&self, id: i32) -> Option<(&'static str, &'static str, String)> {
        let (_, key, what) = FIELDS.iter().find(|(field, _, _)| *field == id)?;
        let item = self.at(self.selected?)?;
        let value = match *key {
            "name" => &item.name,
            "target" => &item.target,
            "args" => &item.args,
            _ => &item.start_in,
        };
        Some((key, what, value.clone()))
    }

    fn focused_field(&self) -> Option<i32> {
        let id = unsafe { GetDlgCtrlID(GetFocus()) };
        FIELDS.iter().any(|(field, _, _)| *field == id).then_some(id)
    }

    /// Saves the text box you're in (Enter, or closing the window).
    fn save_focused(&mut self) {
        if let Some(id) = self.focused_field() {
            self.save_field(id);
        }
    }

    fn save_field(&mut self, id: i32) {
        let Some(spot) = self.selected else { return };
        let Some(expect) = self.label(spot) else { return };
        if let Some((key, what, saved)) = self.saved_value(id) {
            let typed = text_of(self.page, id);
            if typed.trim() == shown_value(key, &saved).trim() {
                return; // as shown: nothing typed (a folder shown by its path keeps its shell name)
            }
            self.set_field(spot, &expect, key, typed.trim(), what);
        }
    }

    /// Esc while typing: puts the box back as saved. True if there was typing to discard.
    fn discard_typing(&mut self) -> bool {
        let Some(id) = self.focused_field() else { return false };
        let Some((key, _, saved)) = self.saved_value(id) else { return false };
        let shown = shown_value(key, &saved);
        if text_of(self.page, id).trim() == shown.trim() {
            return false;
        }
        set_text(self.page, id, &shown);
        true
    }

    fn command(&mut self, id: i32, code: u32) -> Option<Ask> {
        if (id, code) == (IDC_SHORTCUT, EN_CHANGE) {
            self.shortcut_changed();
            return None;
        }
        if code == BN_CLICKED || code == CBN_SELCHANGE {
            if let Some(ask) = self.page_command(id, code) {
                return ask;
            }
        }
        if (id, code) == (IDC_ADD, BN_CLICKED) {
            // Just after the selected item, in its list (its group's, if it's in one).
            let gap = match self.selected {
                Some(Spot { index, child: Some(child) }) => Spot::in_group(index, child + 1),
                Some(Spot { index, child: None }) => Spot::dock(index + 1),
                None => Spot::dock(self.items.len()),
            };
            let has_bin = self.items.iter().any(|item| item.kind == Kind::RecycleBin);
            return Some(Ask::Add(gap, below(self.page, IDC_ADD), has_bin));
        }
        let spot = self.selected?;
        let item = self.at(spot)?.clone();
        let expect = edit::label_of(&item);
        let place = spot.child.unwrap_or(spot.index);
        let moved = |to: usize| Spot { index: if spot.child.is_some() { spot.index } else { to }, child: spot.child.map(|_| to) };
        match (id, code) {
            // A text box is saved when you leave it (or press Enter, or close the window).
            (_, EN_KILLFOCUS) => self.save_field(id),
            (IDC_RUN, CBN_SELCHANGE) => {
                let choice = unsafe { GetDlgItem(Some(self.page), IDC_RUN).map(|run| SendMessageW(run, CB_GETCURSEL, None, None).0).unwrap_or(0) };
                let run = match choice {
                    1 => RunState::Minimized,
                    2 => RunState::Maximized,
                    _ => RunState::Normal,
                };
                self.set_field(spot, &expect, "run", run_word(run), "Run");
            }
            (IDC_ADMIN, BN_CLICKED) => {
                let on = unsafe { IsDlgButtonChecked(self.page, IDC_ADMIN) } == BST_CHECKED.0;
                self.change("Run as administrator", None, false, |doc| edit::set_flag(doc, spot, &expect, "admin", on));
            }
            (IDC_TARGET_BROWSE, BN_CLICKED) => return Some(Ask::Target(spot, expect, self.home.resolve(&item.target))),
            (IDC_START_BROWSE, BN_CLICKED) => {
                let start = self.home.resolve(if item.start_in.trim().is_empty() { &item.target } else { &item.start_in });
                let folder = if Path::new(&start).is_dir() { start } else { Path::new(&start).parent().map(|p| p.display().to_string()).unwrap_or_default() };
                return Some(Ask::StartIn(spot, expect, folder));
            }
            (IDC_ICON_CHANGE, BN_CLICKED) => {
                if item.kind == Kind::Separator {
                    return None; // nothing to show (asked for straight, with --icon)
                }
                // The picker picks out the current icon (its file resolved, as the picker lists them).
                let current = match icons::parse_icon_ref(&item.icon) {
                    _ if item.icon.trim().is_empty() => String::new(),
                    (file, Some(n)) => format!("{},{n}", self.home.resolve(&file)),
                    (file, None) => self.home.resolve(&file),
                };
                let program = Some(self.home.resolve(item.launch_target())).filter(|path| [".exe", ".dll"].iter().any(|e| path.to_ascii_lowercase().ends_with(e)));
                return Some(Ask::Icon(spot, expect, item.icon.clone(), current, program));
            }
            (IDC_ICON_RESET, BN_CLICKED) => self.set_field(spot, &expect, "icon", "", "Icon"),
            (IDC_OPEN_LOCATION, BN_CLICKED) => {
                let path = places::on_disk(&self.home, item.launch_target());
                if let Err(e) = system::open_location(&path) {
                    self.status(&format!("Can't open its file location. {e}"));
                }
            }
            (IDC_REMOVE, BN_CLICKED) => self.remove_selected(),
            (IDC_UP, BN_CLICKED) if place > 0 => self.move_to(spot, moved(place - 1), &expect),
            (IDC_DOWN, BN_CLICKED) if place + 1 < self.siblings(spot) => self.move_to(spot, moved(place + 2), &expect),
            (IDC_GROUP, BN_CLICKED) => return Some(Ask::Group(below(self.page, IDC_GROUP), self.group_menu(spot, &item))),
            _ => {}
        }
        None
    }

    /// The buttons and lists on the pages other than Items. None: not one of theirs.
    fn page_command(&mut self, id: i32, code: u32) -> Option<Option<Ask>> {
        match (id, code) {
            (IDC_SHOW_LABELS, BN_CLICKED) => {
                let on = self.checked(IDC_SHOW_LABELS);
                self.change("Show names", None, true, |doc| edit::set_dock(doc, "show_labels", on));
            }
            (IDC_LAUNCH_EFFECT, CBN_SELCHANGE) => {
                let choice = self.control(IDC_LAUNCH_EFFECT).map_or(0, |combo| unsafe { SendMessageW(combo, CB_GETCURSEL, None, None).0 });
                let word = match choice {
                    0 => "none",
                    2 => "glow",
                    _ => "bounce",
                };
                self.change("Launch effect", None, false, |doc| edit::set_dock(doc, "launch_effect", word));
            }
            (IDC_SCREEN_LIST, CBN_SELCHANGE) => {
                let choice = self.control(IDC_SCREEN_LIST).map_or(0, |combo| unsafe { SendMessageW(combo, CB_GETCURSEL, None, None).0 } as usize);
                if let Some(id) = self.screen_choices.get(choice).cloned().filter(|id| !id.eq_ignore_ascii_case(&self.dock.monitor)) {
                    self.change("Screen", None, true, |doc| edit::set_dock(doc, "monitor", id.as_str()));
                    self.fill_screen();
                }
            }
            (IDC_SHORTCUT_CLEAR, BN_CLICKED) => {
                if let Some(shortcut) = self.control(IDC_SHORTCUT) {
                    unsafe {
                        SendMessageW(shortcut, HKM_SETHOTKEY, Some(WPARAM(0)), None);
                    }
                }
                if !self.dock.hotkey.is_empty() {
                    self.change("No shortcut", None, false, |doc| edit::set_dock(doc, "hotkey", ""));
                }
                self.fill_screen();
            }
            (IDC_HOVER_GLOW, CBN_SELCHANGE) => {
                let choice = self.control(IDC_HOVER_GLOW).map_or(0, |combo| unsafe { SendMessageW(combo, CB_GETCURSEL, None, None).0 });
                let word = match choice {
                    1 => "accent",
                    2 => "icon",
                    _ => "none",
                };
                self.change("Hover glow", None, true, |doc| edit::set_dock(doc, "hover_glow", word));
            }
            (IDC_LOOK, CBN_SELCHANGE) => {
                let dark = self.control(IDC_LOOK).is_some_and(|combo| unsafe { SendMessageW(combo, CB_GETCURSEL, None, None).0 } == 1);
                self.change("Dock look", None, true, |doc| edit::set_dock(doc, "look", if dark { "dark" } else { "light" }));
            }
            (IDC_LOCKED, BN_CLICKED) => {
                let on = self.checked(IDC_LOCKED);
                self.change("Lock icons", None, false, |doc| edit::set_dock(doc, "locked", on));
            }
            (IDC_STARTUP, BN_CLICKED) => {
                // Not in dock.toml: Windows' list of programs to start (so Revert doesn't undo it).
                let on = self.checked(IDC_STARTUP);
                match system::set_starts_with_windows(on) {
                    Ok(()) => self.status(if on { "Start with Windows: on." } else { "Start with Windows: off." }),
                    Err(e) => self.status(&format!("Start with Windows wasn't changed. {e}")),
                }
                self.fill_sections();
            }
            (IDC_PUT_BACK, BN_CLICKED) => self.put_back(),
            (IDC_FORGET, BN_CLICKED) => self.forget_removed(),
            (IDC_CLEAR_REMOVED, BN_CLICKED) => self.clear_removed(),
            (IDC_RESTORE, BN_CLICKED) => self.restore_version(),
            (IDC_SNAPSHOT, BN_CLICKED) => self.save_snapshot(),
            (IDC_OPEN_BACKUPS, BN_CLICKED) => launch::open(self.store.backups_dir()),
            (IDC_OPEN_FOLDER, BN_CLICKED) => launch::open(&self.home.dir),
            (IDC_EDIT_CONFIG, BN_CLICKED) => launch::edit(self.store.config_path()),
            (IDC_RELOAD, BN_CLICKED) => {
                nudge_dock(true);
                self.status("The dock read your dock's file again.");
            }
            (IDC_CLEAR_CACHE, BN_CLICKED) => self.clear_icon_cache(),
            (IDC_OPEN_LOG, BN_CLICKED) => launch::edit(&self.home.dir.join("logs").join("dock.log")),
            (IDC_SAVE_COPY, BN_CLICKED) => return Some(Some(Ask::SaveCopy)),
            (IDC_LOAD_COPY, BN_CLICKED) => return Some(Some(Ask::LoadCopy)),
            (IDC_IMPORT_ROCKETDOCK, BN_CLICKED) => {
                let page = self.pages.get(SECTION_FILES).copied().unwrap_or(self.page);
                return Some(Some(Ask::ImportRocketDock(below(page, IDC_IMPORT_ROCKETDOCK))));
            }
            _ => return None,
        }
        Some(None)
    }

    /// Saves one change to dock.toml, tells the dock, and says how it went. `renamed`: the
    /// item's label after this change, if it changes it (so the window keeps following it).
    /// `show`: the dock comes out to show it (not for things you can't see, like timings).
    /// `what` is a setting ("Icon size": "Icon size: saved.") or, ending in a full stop, what
    /// was done ("Moved Chrome.", shown as it is).
    fn change(&mut self, what: &str, renamed: Option<String>, show: bool, edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>) -> bool {
        let result = self.store.change(edit);
        if result.is_ok() && renamed.is_some() {
            self.following = renamed;
        }
        self.reload_model();
        match result {
            Ok(_) => {
                nudge_dock(show);
                self.status(&if what.ends_with('.') { what.to_string() } else { format!("{what}: saved.") });
                true
            }
            Err(e) => {
                self.status(&if what.ends_with('.') { e } else { format!("{what} wasn't saved. {e}") });
                false
            }
        }
    }

    fn set_field(&mut self, spot: Spot, expect: &str, key: &str, value: &str, what: &str) {
        let Some(item) = self.at(spot) else { return };
        let current = match key {
            "name" => item.name.as_str(),
            "target" => &item.target,
            "args" => &item.args,
            "start_in" => &item.start_in,
            "icon" => &item.icon,
            "run" => run_word(item.run),
            _ => "",
        };
        if current == value {
            return; // nothing changed
        }
        let renamed = (key == "name").then(|| {
            let mut renamed = item.clone();
            renamed.name = value.to_string();
            edit::label_of(&renamed)
        });
        let show = key != "run"; // a name, target or icon: worth a look
        self.change(what, renamed, show, |doc| edit::set_text(doc, spot, expect, key, value));
    }

    fn remove_selected(&mut self) {
        let Some(spot) = self.selected else { return };
        let Some(expect) = self.label(spot) else { return };
        let group = spot.child.and(self.items.get(spot.index)).map(edit::label_of);
        let result = self.store.change(|doc| edit::remove(doc, spot, &expect));
        self.reload_model();
        match result {
            Ok((table, _)) => {
                let _ = self.removed.add(&table, spot.child.unwrap_or(spot.index), group.as_deref());
                nudge_dock(true);
                self.select(spot);
                self.status(&format!("Removed {expect}. To get it back: Revert, or Recently removed > Put back."));
            }
            Err(e) => self.status(&format!("{expect} wasn't removed: {e}")),
        }
    }

    /// Moves the item at `from` to the gap `to` in the same list (in front of what's there now).
    fn move_to(&mut self, from: Spot, to: Spot, expect: &str) {
        if self.change(&format!("Moved {expect}."), None, true, |doc| edit::move_item(doc, from, to, expect)) {
            let (old, new) = (from.child.unwrap_or(from.index), to.child.unwrap_or(to.index));
            let landed = if new > old { new - 1 } else { new };
            self.select(Spot { index: if from.child.is_some() { from.index } else { landed }, child: from.child.map(|_| landed) });
        }
    }

    fn add(&mut self, gap: Spot, new: &[ItemConfig]) {
        let what = match new {
            [one] => format!("Added {}", edit::label_of(one)),
            _ => format!("Added {} items", new.len()),
        };
        let added = self.change(&what, None, true, |doc| {
            for (k, item) in new.iter().enumerate() {
                let spot = match gap.child {
                    Some(child) => Spot::in_group(gap.index, child + k),
                    None => Spot::dock(gap.index + k),
                };
                edit::insert(doc, spot, edit::item_table(item))?;
            }
            Ok(())
        });
        if added {
            self.select(gap);
        }
    }

    /// Group…: what can be done with the selected item and groups.
    fn group_menu(&self, spot: Spot, item: &ItemConfig) -> Vec<(usize, String)> {
        let label = edit::label_of(item);
        let mut entries = Vec::new();
        let groups: Vec<(usize, String)> = self.items.iter().enumerate().filter(|(_, it)| it.kind == Kind::Group).map(|(i, it)| (i, edit::label_of(it))).collect();
        match (item.kind, spot.child) {
            (Kind::Group, None) => entries.push((GROUP_UNGROUP, format!("Ungroup {label}"))),
            (_, Some(_)) => {
                let group = self.items.get(spot.index).map(edit::label_of).unwrap_or_default();
                entries.push((GROUP_TAKE_OUT, format!("Take {label} out of {group}")));
            }
            (_, None) => entries.push((GROUP_NEW, format!("New group with {label}"))),
        }
        if item.kind != Kind::Group {
            let others: Vec<&(usize, String)> = groups.iter().filter(|(g, _)| spot.child.is_none() || *g != spot.index).collect();
            if !others.is_empty() {
                entries.push((0, String::new()));
            }
            for (g, name) in others {
                entries.push((GROUP_MOVE + g, format!("Move to {name}")));
            }
        }
        entries
    }

    fn group_action(&mut self, choice: usize) {
        let Some(spot) = self.selected else { return };
        let Some(expect) = self.label(spot) else { return };
        match choice {
            GROUP_NEW => {
                if self.change(&format!("New group with {expect}."), Some(NEW_GROUP_NAME.into()), true, |doc| edit::make_group(doc, spot.index, &expect, NEW_GROUP_NAME)) {
                    self.select(Spot::dock(spot.index));
                    self.status(&format!("New group with {expect}. Type its name in Name."));
                }
            }
            GROUP_UNGROUP => {
                self.change(&format!("Ungrouped {expect}."), None, true, |doc| edit::ungroup(doc, spot.index, &expect).map(|_| ()));
            }
            GROUP_TAKE_OUT => {
                self.change(&format!("Took {expect} out of the group."), None, true, |doc| edit::move_item(doc, spot, Spot::dock(spot.index + 1), &expect));
            }
            _ if choice >= GROUP_MOVE => {
                let group = choice - GROUP_MOVE;
                let name = self.items.get(group).map(edit::label_of).unwrap_or_default();
                let end = self.items.get(group).map_or(0, |g| g.items.len());
                self.change(&format!("Moved {expect} to {name}."), None, true, |doc| edit::move_item(doc, spot, Spot::in_group(group, end), &expect));
            }
            _ => {}
        }
    }

    // ---- Recently removed -------------------------------------------------------------------

    fn removed_list_control(&self) -> Option<HWND> {
        self.pages.get(SECTION_REMOVED).and_then(|&page| unsafe { GetDlgItem(Some(page), IDC_REMOVED_LIST).ok() })
    }

    fn fill_removed(&mut self) {
        self.removed_list = self.removed.list();
        let Some(list) = self.removed_list_control() else { return };
        let rows: Vec<Vec<String>> = self
            .removed_list
            .iter()
            .map(|entry| vec![entry.label.clone(), entry.removed_at.get(..16).unwrap_or(&entry.removed_at).to_string(), entry.was_in.clone().unwrap_or_else(|| "Dock".into())])
            .collect();
        fill_rows(list, &rows);
        let page = self.pages[SECTION_REMOVED];
        let any = !self.removed_list.is_empty();
        for id in [IDC_PUT_BACK, IDC_FORGET, IDC_CLEAR_REMOVED] {
            enable(page, id, any);
        }
    }

    /// The one picked in the list, or the newest if none is.
    fn chosen_removed(&mut self) -> Option<Entry> {
        let row = self.removed_list_control().and_then(selected_row).unwrap_or(0);
        let entry = self.removed_list.get(row).cloned();
        if entry.is_none() {
            self.status("Nothing to put back.");
        }
        entry
    }

    /// Back where it was (in its group, if that's still there), like the dock's Put back.
    fn put_back(&mut self) {
        let Some(chosen) = self.chosen_removed() else { return };
        let Some(entry) = self.removed.take(&chosen.removed_at) else { return };
        let table = entry.table.clone();
        let result = self.store.change(|doc| {
            let spot = edit::put_back_spot(doc, entry.was_at, entry.was_in.as_deref());
            edit::insert(doc, spot, table)
        });
        match result {
            Ok(_) => {
                nudge_dock(true);
                self.status(&format!("Put back {}.", entry.label));
            }
            Err(e) => {
                let _ = self.removed.restore(&entry); // leave it in the list
                self.status(&format!("{} wasn't put back: {e}", entry.label));
            }
        }
        self.reload_model();
        self.fill_removed();
    }

    fn forget_removed(&mut self) {
        let row = self.removed_list_control().and_then(selected_row);
        let Some(chosen) = row.and_then(|row| self.removed_list.get(row)).cloned() else {
            self.status("Pick one in the list first.");
            return;
        };
        if self.removed.take(&chosen.removed_at).is_some() {
            self.status(&format!("Forgot {}.", chosen.label));
        }
        self.fill_removed();
    }

    fn clear_removed(&mut self) {
        match self.removed.clear() {
            Ok(()) => self.status("Recently removed is empty."),
            Err(e) => self.status(&format!("Couldn't clear the list: {e}")),
        }
        self.fill_removed();
    }

    // ---- Versions ---------------------------------------------------------------------------

    fn fill_versions(&mut self) {
        self.versions = self.store.backups();
        let Some(list) = self.pages.get(SECTION_VERSIONS).and_then(|&page| unsafe { GetDlgItem(Some(page), IDC_VERSIONS_LIST).ok() }) else { return };
        let rows: Vec<Vec<String>> = self
            .versions
            .iter()
            .map(|backup| {
                let kind = match backup.kind {
                    BackupKind::Recent => "Before a change".to_string(),
                    BackupKind::Daily => "Daily".to_string(),
                    BackupKind::Pinned if backup.label.is_empty() => "Snapshot".to_string(),
                    BackupKind::Pinned => kept_copy_kind(&backup.label).to_string(),
                };
                let count = config::read_text(&backup.path).ok().and_then(|text| item_count(&text)).map_or("?".to_string(), |n| n.to_string());
                vec![if backup.when.is_empty() { "-".into() } else { backup.when.clone() }, kind, count]
            })
            .collect();
        fill_rows(list, &rows);
    }

    /// Puts a backup back as dock.toml, after keeping the current one as a snapshot.
    fn restore_version(&mut self) {
        let list = self.pages.get(SECTION_VERSIONS).and_then(|&page| unsafe { GetDlgItem(Some(page), IDC_VERSIONS_LIST).ok() });
        let Some(backup) = list.and_then(selected_row).and_then(|row| self.versions.get(row)).cloned() else {
            self.status("Pick a version in the list first.");
            return;
        };
        let text = match config::read_text(&backup.path) {
            Ok(text) if config::parse(&text).is_ok() => text,
            Ok(_) => {
                self.status("That version can't be read as a dock; nothing changed.");
                return;
            }
            Err(e) => {
                self.status(&format!("Couldn't read it: {e}"));
                return;
            }
        };
        let kept = self.store.pin("before-restore");
        let result = self.store.replace(&text);
        self.reload_model();
        match result {
            Ok(()) => {
                nudge_dock(true);
                let kept = if kept.is_some() { " Your dock as it was is kept in Versions." } else { "" };
                self.status(&format!("Restored the version from {}.{kept}", backup.when));
            }
            Err(e) => self.status(&format!("Couldn't restore it: {e}")),
        }
        self.fill_versions();
    }

    // ---- Moving your dock (Files) -------------------------------------------------------------

    /// Your dock, written so it works on another PC or account too (your folders as
    /// %USERPROFILE% and the like).
    fn save_copy(&mut self, path: &str) {
        let folders: Vec<(&str, String)> = ["USERPROFILE", "LOCALAPPDATA", "APPDATA", "ProgramFiles", "ProgramFiles(x86)"]
            .into_iter()
            .filter_map(|name| std::env::var(name).ok().map(|value| (name, value)))
            .collect();
        let copy = config::read_text(self.store.config_path())
            .map_err(|e| e.to_string())
            .and_then(|text| edit::portable(&text, &folders))
            .and_then(|text| std::fs::write(path, text).map_err(|e| e.to_string()));
        match copy {
            Ok(()) => self.status(&format!("Saved a copy of your dock: {path}")),
            Err(e) => self.status(&format!("Couldn't save the copy: {e}")),
        }
    }

    fn load_copy(&mut self, path: &str) {
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let text = match config::read_text(Path::new(path)) {
            Ok(text) => text,
            Err(e) => return self.status(&format!("Couldn't read {name}: {e}")),
        };
        match config::parse(&text) {
            Err(e) => self.status(&format!("{name} isn't a Desktop Dock file: {e}")),
            Ok(parsed) if parsed.read_only => self.status(&format!("{name} is from a newer Desktop Dock; nothing changed.")),
            Ok(_) => self.replace_dock(&text, "before-load", &format!("Loaded {name}")),
        }
    }

    fn import_rocketdock(&mut self, source: Result<import::RocketDock, String>) {
        let rocketdock = match source {
            Ok(rocketdock) => rocketdock,
            Err(e) => return self.status(&e),
        };
        let imported = import::convert(&rocketdock, |path| Path::new(path).exists(), icons::icon_count);
        // A short summary here; each note in full in the log.
        let mut done = format!("Imported {} from RocketDock", plural(imported.items, "item", "items"));
        for note in &imported.report {
            crate::log_info!("RocketDock import: {note}");
        }
        if !imported.report.is_empty() {
            done.push_str(&format!(" ({}, in the log: Files > Open dock folder > logs)", plural(imported.report.len(), "note", "notes")));
        }
        self.replace_dock(&imported.text, "before-import", &done);
    }

    /// Replaces your whole dock (a loaded copy, an import), keeping it as it was in Versions.
    fn replace_dock(&mut self, text: &str, pin: &str, done: &str) {
        let kept = self.store.pin(pin);
        let result = self.store.replace(text);
        self.reload_model();
        self.fill_sections();
        match result {
            Ok(()) => {
                nudge_dock(true);
                let kept = if kept.is_some() { " Your dock as it was is kept in Versions." } else { "" };
                self.status(&format!("{done}.{kept}"));
            }
            Err(e) => self.status(&format!("Nothing changed. {e}")),
        }
    }

    fn save_snapshot(&mut self) {
        match self.store.pin("snapshot") {
            Some(_) => self.status("Saved a snapshot of your dock: it's kept in Versions."),
            None => self.status("Couldn't save a snapshot."),
        }
        self.fill_versions();
    }

    // ---- Files ------------------------------------------------------------------------------

    fn fill_files(&mut self) {
        let Some(&page) = self.pages.get(SECTION_FILES) else { return };
        set_text(page, IDC_FOLDER, &self.home.dir.display().to_string());
        let (all, cache, backups, logs) =
            (folder_size(&self.home.dir), folder_size(&self.home.icon_cache()), folder_size(&self.home.backups()), folder_size(&self.home.logs()));
        set_text(
            page,
            IDC_SIZES,
            &format!("Uses {}: icons {}, backups {}, logs {}, and dock.toml itself.", megabytes(all), megabytes(cache), megabytes(backups), megabytes(logs)),
        );
    }

    /// Deletes the made icons; the dock makes them again as it needs them.
    fn clear_icon_cache(&mut self) {
        let cache = self.home.icon_cache();
        let mut gone = 0;
        for entry in std::fs::read_dir(&cache).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("bgra")) && std::fs::remove_file(&path).is_ok() {
                gone += 1;
            }
        }
        nudge_dock(false);
        self.status(&format!("Cleared {}; the dock remakes them as it needs them.", plural(gone, "icon", "icons")));
        self.fill_files();
    }

    // ---- Revert -----------------------------------------------------------------------------

    fn revert(&mut self) {
        match self.mode {
            Mode::Settings => self.revert_file(),
            Mode::Properties => self.revert_item(),
        }
    }

    /// The settings window: dock.toml back exactly as when the window opened.
    fn revert_file(&mut self) {
        let now = config::read_text(self.store.config_path()).unwrap_or_default();
        if now == self.opening {
            self.status("Nothing to revert: your dock is as it was when this window opened.");
            return;
        }
        // What's being undone stays in Versions, in case the revert wasn't wanted after all.
        self.store.pin("before-revert");
        let result = self.store.replace(&self.opening);
        self.reload_model();
        match result {
            Ok(()) => {
                nudge_dock(true);
                self.status("Reverted: your dock is back as it was when this window opened.");
            }
            Err(e) => self.status(&format!("Couldn't revert: {e}")),
        }
    }

    /// The properties window: this item's properties back as they were when it was shown.
    /// Only what differs is written, and nothing else on the dock changes.
    fn revert_item(&mut self) {
        let (Some(spot), Some(was)) = (self.selected, self.opening_item.clone()) else { return };
        let Some(now) = self.at(spot).cloned() else { return };
        if same_properties(&was, &now) {
            self.status("Nothing to revert.");
            return;
        }
        let expect = edit::label_of(&now);
        let renamed = (was.name != now.name).then(|| edit::label_of(&was));
        self.change("Reverted.", renamed, true, |doc| {
            for (key, before, after) in [
                ("target", was.target.as_str(), now.target.as_str()),
                ("args", &was.args, &now.args),
                ("start_in", &was.start_in, &now.start_in),
                ("icon", &was.icon, &now.icon),
                ("run", run_word(was.run), run_word(now.run)),
            ] {
                if before != after {
                    edit::set_text(doc, spot, &expect, key, before)?;
                }
            }
            if was.admin != now.admin {
                edit::set_flag(doc, spot, &expect, "admin", was.admin)?;
            }
            if was.name != now.name {
                edit::set_text(doc, spot, &expect, "name", &was.name)?; // last: it changes the label
            }
            Ok(())
        });
    }

    /// Once a second: has dock.toml changed elsewhere (on the dock, or by hand)? Not while
    /// you're typing in a box; that's saved first, and then the window catches up.
    fn check_file(&mut self) {
        if modified(self.store.config_path()) == self.stamp {
            return;
        }
        let typing = self.focused_field().and_then(|id| self.saved_value(id).map(|(_, _, saved)| (id, saved)));
        if typing.is_some_and(|(id, saved)| text_of(self.page, id).trim() != saved.trim()) {
            return;
        }
        self.reload_model();
    }

    // ---- Icons ------------------------------------------------------------------------------

    fn sources(&self, item: &ItemConfig) -> Vec<IconSource> {
        if item.kind == Kind::Separator {
            return Vec::new();
        }
        // A group without an icon of its own: its first item's (the dock draws its tile).
        if item.kind == Kind::Group && item.icon.trim().is_empty() {
            return item.items.iter().map(|child| self.sources(child)).find(|sources| !sources.is_empty()).unwrap_or_default();
        }
        let mut resolved = item.clone();
        resolved.icon = self.home.resolve(&item.icon);
        resolved.target = self.home.resolve_for_icon(item.launch_target());
        icons::sources_for(&resolved, false)
    }

    /// Loads icons not loaded yet (every item's for the list; just the item's for a properties
    /// window), on a background thread, in this process only: nothing goes into the dock's cache.
    fn load_icons(&mut self) {
        let shown: Vec<&ItemConfig> = match self.mode {
            Mode::Settings => self.rows.iter().filter_map(|&spot| self.at(spot)).collect(),
            Mode::Properties => self.selected.and_then(|spot| self.at(spot)).into_iter().collect(),
        };
        let wanted: Vec<Vec<IconSource>> = shown.into_iter().map(|item| self.sources(item)).filter(|s| !s.is_empty() && !self.icons.contains_key(s)).collect();
        if wanted.is_empty() {
            return;
        }
        for sources in &wanted {
            self.icons.insert(sources.clone(), (None, None)); // asked for
        }
        let (list_px, preview_px, arrived, page) = (self.list_px, self.preview_px, self.arrived.clone(), self.page.0 as isize);
        let with_list = self.list.is_some();
        std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            }
            let Ok(loader) = IconLoader::new(None) else { return };
            for sources in wanted {
                let small = if with_list { loader.load(&sources, list_px) } else { None };
                let large = loader.load(&sources, preview_px);
                if let Ok(mut list) = arrived.lock() {
                    list.push((sources, small, large));
                }
                unsafe {
                    let _ = PostMessageW(Some(HWND(page as *mut c_void)), WM_APP_ICONS, WPARAM(0), LPARAM(0));
                }
            }
        });
    }

    fn receive_icons(&mut self) {
        let arrived: Vec<_> = self.arrived.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default();
        if arrived.is_empty() {
            return;
        }
        for (sources, small, large) in arrived {
            if let (Some((_, images)), Some(bitmap)) = (self.list, small.as_ref().and_then(bitmap_of)) {
                let at = unsafe { ImageList_Add(images, bitmap, None) };
                unsafe {
                    let _ = DeleteObject(bitmap.into());
                }
                if at >= 0 {
                    self.image_index.insert(sources.clone(), at);
                }
            }
            self.icons.insert(sources, (small, large));
        }
        self.fill_list();
        unsafe {
            if let Ok(preview) = GetDlgItem(Some(self.page), IDC_ICON) {
                let _ = InvalidateRect(Some(preview), None, true);
            }
        }
    }

    /// The Icon box: the selected item's icon.
    fn draw_preview(&self, item: &DRAWITEMSTRUCT) {
        unsafe {
            let _ = FillRect(item.hDC, &item.rcItem, chrome::window_brush());
        }
        let Some(selected) = self.selected.and_then(|spot| self.at(spot)) else { return };
        let Some(pixels) = self.icons.get(&self.sources(selected)).and_then(|(_, large)| large.as_ref()) else { return };
        let Some(bitmap) = bitmap_of(pixels) else { return };
        unsafe {
            let source = CreateCompatibleDC(Some(item.hDC));
            let old = SelectObject(source, bitmap.into());
            let width = item.rcItem.right - item.rcItem.left;
            let height = item.rcItem.bottom - item.rcItem.top;
            let side = (pixels.width as i32).min(width).min(height);
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: AC_SRC_ALPHA as u8 };
            let (x, y) = (item.rcItem.left + (width - side) / 2, item.rcItem.top + (height - side) / 2);
            let _ = AlphaBlend(item.hDC, x, y, side, side, source, 0, 0, pixels.width as i32, pixels.height as i32, blend);
            SelectObject(source, old);
            let _ = DeleteDC(source);
            let _ = DeleteObject(bitmap.into());
        }
    }
}

/// A 32-bit premultiplied bitmap of an icon's pixels (for the list and the Icon box).
fn bitmap_of(pixels: &Pixels) -> Option<HBITMAP> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: pixels.width as i32,
            biHeight: -(pixels.height as i32), // top-down
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

    /// Every dialog in the resource file: its number, size, and each control's (line, x, y, w, h).
    fn dialogs() -> Vec<(usize, i32, i32, Vec<(String, i32, i32, i32, i32)>)> {
        let rc = include_str!("../assets/desktop-dock.rc");
        let mut found = Vec::new();
        let mut lines = rc.lines();
        while let Some(line) = lines.next() {
            let Some((id, size)) = line.split_once(" DIALOGEX ") else { continue };
            let Ok(id) = id.trim().parse::<usize>() else { continue };
            let numbers: Vec<i32> = size.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            let (width, height) = (numbers[2], numbers[3]);
            let mut controls = Vec::new();
            for line in lines.by_ref().skip_while(|l| l.trim() != "BEGIN").skip(1) {
                if line.trim() == "END" {
                    break;
                }
                // Quoted text aside, the last four whole numbers are x, y, width and height.
                let unquoted: String = line.split('"').step_by(2).collect::<Vec<_>>().join("");
                let numbers: Vec<i32> = unquoted.split(',').filter_map(|n| n.trim().parse().ok()).collect();
                if numbers.len() >= 4 {
                    let [x, y, w, h] = numbers[numbers.len() - 4..] else { unreachable!() };
                    // A drop-down list's height is its open list; closed, it's one line.
                    let h = if line.trim_start().starts_with("COMBOBOX") { 14 } else { h };
                    controls.push((line.trim().to_string(), x, y, w, h));
                }
            }
            found.push((id, width, height, controls));
        }
        found
    }

    /// The settings window fits a 1080-line laptop screen at 150% (a 1008-pixel work area, 3.125
    /// pixels a unit at 150%), and nothing on any page or dialog sits outside it.
    #[test]
    fn every_window_fits_and_nothing_sits_outside_its_page() {
        let all = dialogs();
        assert!(all.len() >= 10, "found {} dialogs", all.len());
        let main = all.iter().find(|(id, ..)| *id == IDD_SETTINGS).unwrap();
        assert!(main.2 <= 300, "the settings window is {} units tall", main.2);
        // The pages fill the page area exactly (settings.rs places them there).
        for (id, _) in SECTIONS.iter().map(|(_, id, _)| (*id, ())) {
            let page = all.iter().find(|(d, ..)| *d == id).unwrap_or_else(|| panic!("page {id} missing"));
            assert_eq!((page.1, page.2), (464 - 94, 272 - 26), "page {id} isn't the page area's size");
        }
        for (id, width, height, controls) in &all {
            for (line, x, y, w, h) in controls {
                assert!(*x >= 0 && *y >= 0 && x + w <= *width && y + h <= *height, "dialog {id} ({width} x {height}): outside it: {line}");
            }
        }
    }

    /// One-line labels, tick boxes and buttons are wide enough for their words (roughly: Segoe
    /// UI averages under 4 units a letter; a tick box takes 12 more for its box).
    #[test]
    fn one_line_texts_fit_their_controls() {
        for (id, _, _, controls) in dialogs() {
            for (line, _, _, w, h) in controls {
                let Some(text) = line.split('"').nth(1).filter(|text| !text.is_empty()) else { continue };
                let kind = line.split_whitespace().next().unwrap_or_default();
                let box_width = match kind {
                    "AUTOCHECKBOX" | "AUTORADIOBUTTON" => 12,
                    "PUSHBUTTON" | "DEFPUSHBUTTON" => 8,
                    "LTEXT" | "RTEXT" | "CTEXT" if h <= 11 => 0,
                    _ => continue,
                };
                let needs = (text.chars().count() as f32 * 3.6) as i32 + box_width;
                assert!(needs <= w, "dialog {id}: '{text}' needs about {needs} units, has {w}");
            }
        }
    }

    #[test]
    fn revert_compares_only_what_a_properties_window_sets() {
        let a = ItemConfig::new("Photoshop", r"C:\ps.exe");
        let mut b = a.clone();
        assert!(same_properties(&a, &b));
        b.admin = true;
        assert!(!same_properties(&a, &b));
        let mut c = a.clone();
        c.items.push(ItemConfig::default()); // a group's children aren't a property here
        assert!(same_properties(&a, &c));
    }

    #[test]
    fn versions_count_groups_items_too() {
        let text = "version = 1\n[[item]]\nname = 'A'\n[[item]]\nkind = 'group'\nname = 'G'\n[[item.items]]\nname = 'B'\n[[item.items]]\nname = 'C'\n";
        assert_eq!(item_count(text), Some(4));
        assert_eq!(item_count("not toml ["), None);
    }
}
