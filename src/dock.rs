//! The dock window: show/hide behaviour, hover and click handling, and drawing.
//! The arithmetic lives in `layout` and `motion`, where it's unit-tested.

use crate::config::{self, ChangeWatch, Config, DockLook, DockSettings, HoverGlow, ItemConfig, LaunchEffect};
use crate::config::Kind;
use crate::heal;
use crate::hotkey;
use crate::home::Home;
use crate::icons::{self, IconLoader, IconSource, IconWorker, Job, Part};
use crate::layout::{self, DragZone, Layout};
use crate::motion::{self, ClickGuard, Dwell};
use crate::render::Renderer;
use crate::drop;
use crate::pickers::{self, Pick};
use crate::popup;
use crate::edit::{self, Spot};
use crate::removed::{self, Removed};
use crate::store::{self, Mode, Store};
use crate::{launch, places, system};
use crate::{log_info, log_warn};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use windows::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};
use windows::Win32::Foundation::{COLORREF, E_FAIL, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::Storage::FileSystem::{FindCloseChangeNotification, FindNextChangeNotification};
use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, HBRUSH};
use windows::Win32::System::Ole::{OleInitialize, RegisterDragDrop, RevokeDragDrop};
use windows::Win32::System::Threading::INFINITE;
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F, D2D1_GRADIENT_STOP};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_EXTEND_MODE_CLAMP,
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_ELLIPSE, D2D1_GAMMA_2_2, D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES,
    D2D1_OPACITY_MASK_CONTENT_GRAPHICS, D2D1_ROUNDED_RECT, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1LinearGradientBrush, ID2D1SolidColorBrush,
    ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, HOT_KEY_MODIFIERS, RegisterHotKey, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
    ASFW_ANY, AllowSetForegroundWindow, GetMessageW, GetWindowRect, HWND_TOPMOST, IDC_ARROW, KillTimer, LWA_ALPHA, LoadCursorW, MA_NOACTIVATE,
    IDYES, MB_ICONQUESTION, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MF_CHECKED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PBT_APMRESUMEAUTOMATIC,
    PBT_APMRESUMESUSPEND, PBT_POWERSETTINGCHANGE, PM_REMOVE, PeekMessageW, PostMessageW,
    PostQuitMessage, QS_ALLINPUT, RegisterClassExW, SM_CXDRAG, SM_CYDRAG, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    SWP_NOSIZE, SWP_SHOWWINDOW, SetCoalescableTimer, SetCursorPos, SetLayeredWindowAttributes, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow,
    TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, TrackPopupMenuEx,
    TranslateMessage, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_CAPTURECHANGED, WM_HOTKEY, WM_MOUSEACTIVATE, WM_MOUSEFIRST, WM_MOUSELAST, WM_MOUSEMOVE, WM_NULL, WM_POWERBROADCAST, WM_QUIT, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_TIMER,
    MF_GRAYED, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Error, HSTRING, PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

/// From winuser.h; the windows crate files it under Common Controls.
const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_POLL: usize = 1;
/// One-shot: the recheck after display, DPI, Explorer, sleep or unlock changes (see `recheck`).
const TIMER_RECHECK: usize = 2;
/// One-shot: dock.toml's second look, to be sure it has finished being saved.
const TIMER_CONFIG: usize = 3;
/// One-shot: the undo strip's time is up.
const TIMER_STRIP: usize = 4;
/// One-shot: read dock.toml again after something held it (antivirus, a sync app).
const TIMER_READ_RETRY: usize = 5;
/// Every hour: a line in the log with what the dock is using (memory, handles, GDI and USER
/// objects), so a slow leak in real use shows up in the log.
const TIMER_HEALTH: usize = 7;
const HEALTH_EVERY_MS: u32 = 60 * 60 * 1000;
/// One-shot: the very first start's welcome, once the icons are in (see `welcome`).
const TIMER_WELCOME: usize = 8;
const WELCOME_AFTER_MS: u32 = 800;
/// How long the welcome keeps the dock out, unless the pointer goes to it.
const WELCOME_FOR: Duration = Duration::from_secs(8);
/// While nobody's there (polling paused): a look once a minute in case "they're back" was missed.
const TIMER_PRESENCE: usize = 6;
const PRESENCE_CHECK_MS: u32 = 60_000;
/// Missing items and placeholder icons get a second look at most this often when the dock comes
/// out (see `recheck_missing`).
const MISSING_RECHECK_EVERY: Duration = Duration::from_secs(5 * 60);
/// Placeholder icons are tried again this many times after starting or waking, so one that
/// never loads isn't tried for ever.
const PLACEHOLDER_RETRIES: u32 = 3;
/// dock.toml held by another program (antivirus, a sync app): tried again quickly this many
/// times, then every few seconds for as long as it takes. Only a file that stays unreadable
/// this long is worth a word; one that frees up sooner is just read.
const READ_RETRIES: u32 = 5;
const READ_RETRY_MS: u32 = 400;
const READ_RETRY_SLOW_MS: u32 = 3000;
const UNREADABLE_TELL_AFTER: Duration = Duration::from_secs(15);
/// How long the "Removed X · Undo" strip stays (the dock stays out meanwhile). Undo lasts as
/// long as the strip; after that, Put back is the way to return a removed item.
const STRIP_FOR: Duration = Duration::from_secs(6);
/// Shown when a drag meets a locked dock.
const LOCKED_NOTICE: &str = "Icons are locked (right-click > Lock icons to unlock)";
/// Shown when one of the dock's own icons is dragged while locked.
const LOCKED_ICON_NOTICE: &str = "Icons are locked (hold Ctrl to move one, or right-click > Lock icons to unlock)";
/// After a drop into a group's pop-up it stays this long, so you see where the icon landed...
const SETTLE_HOLD: Duration = Duration::from_millis(600);
/// ...and a closing pop-up fades out over this long (at once with Windows' animations off).
const POPUP_FADE: Duration = Duration::from_millis(180);
/// How long the pointer may be away from an open group (and the dock) before it closes.
const GROUP_CLOSE_GRACE: Duration = Duration::from_millis(500);
/// A whole group dragged off the dock goes only after it's been held there this long.
const GROUP_HOLD: Duration = Duration::from_secs(1);
/// The small icons on a group's tile, as a share of the tile's size when pointed at.
const PREVIEW_FRACTION: f32 = 0.36;
/// Held over a group while dragging an icon, it springs open to place the icon exactly.
const SPRING_OPEN: Duration = Duration::from_millis(600);
/// What right-click → New group calls it (rename it in Properties).
const NEW_GROUP_NAME: &str = "New group";
/// Changes like these often come in bursts; one recheck runs once they've settled.
const RECHECK_DELAY_MS: u32 = 3000;
/// dock.toml is reloaded once its date stays the same this long (an editor may still be writing).
const CONFIG_SETTLE_MS: u32 = 500;
const MENU_EXIT: usize = 2;
const MENU_RESTORE: usize = 5;
const MENU_EMPTY_BIN: usize = 6;
const MENU_REMOVE: usize = 9;
const MENU_LOCK: usize = 10;
const MENU_ICON_CHOOSE: usize = 11;
const MENU_ADD_FILES: usize = 14;
const MENU_ADD_FOLDER: usize = 15;
const MENU_ADD_SEPARATOR: usize = 16;
const MENU_ADD_BIN: usize = 17;
const MENU_SETTINGS: usize = 18;
const MENU_PROPERTIES: usize = 19;
const MENU_NEW_GROUP: usize = 20;
const MENU_UNGROUP: usize = 21;
const MENU_OPEN_LOCATION: usize = 22;
/// Move to group ▸: one entry per group on the dock.
const MENU_TO_GROUP: usize = 200;
/// Add ▸ Windows item ▸ (one entry per places::all() entry, from here).
const MENU_ADD_WINDOWS: usize = 400;
/// Put back: one id per Recently removed entry shown, from here up.
const MENU_PUT_BACK: usize = 100;
const PUT_BACK_SHOWN: usize = 10;
/// Posted by the icon worker when icons are ready.
const WM_APP_ICONS: u32 = WM_APP + 1;
/// Posted when the Recycle Bin check finishes.
const WM_APP_BIN: u32 = WM_APP + 2;
// Test-only messages (WM_APP+91…99) are answered only by development builds (home::is_dev_build).
/// Test only: write what the dock shows now to %TEMP%\desktop-dock-test-state.json before
/// answering, so a test can check what a person would see (see `test_state`).
const WM_APP_TEST_STATE: u32 = WM_APP + 91;
/// Test only: log where every icon is on screen, so a test can drag to it.
const WM_APP_TEST_WHERE: u32 = WM_APP + 98;
/// Test only: crash on purpose, to prove the watchdog restarts the dock
/// (wparam 1: as a Windows exception rather than a Rust abort, to check it's logged).
const WM_APP_TEST_CRASH: u32 = WM_APP + 99;
/// Test only: stop responding for good, to prove the watchdog notices.
const WM_APP_TEST_HANG: u32 = WM_APP + 94;
/// Test only: log what the dock is holding, so a test can see what grows.
const WM_APP_TEST_STATS: u32 = WM_APP + 93;
/// Test only: run a message loop of its own for wparam seconds, as an
/// open menu or a Yes/No question does, to prove the watchdog doesn't take that for a hang.
const WM_APP_TEST_MODAL: u32 = WM_APP + 92;
/// Test only: open and close the first group wparam times, in a row.
const WM_APP_TEST_GROUP: u32 = WM_APP + 97;
/// Test only: point at item wparam (nothing, if it's past the end), as
/// the mouse would: it zooms, shows its name and glows.
const WM_APP_TEST_HOVER: u32 = WM_APP + 95;
/// Test only: click item wparam (a program or file), as the mouse would.
const WM_APP_TEST_LAUNCH: u32 = WM_APP + 96;
/// Posted after emptying the Recycle Bin, to check it again straight away.
const WM_APP_RECHECK_BIN: u32 = WM_APP + 3;
/// Asks for a self-healing pass (posted when a launch finds its program has moved).
const WM_APP_HEAL: u32 = WM_APP + 4;
/// Posted when a self-healing pass finishes.
const WM_APP_HEALED: u32 = WM_APP + 5;
/// Posted when a launch fails (wparam = item, lparam = generation): the item shakes.
const WM_APP_LAUNCH_FAILED: u32 = WM_APP + 6;
/// Posted by a drop, to add what was dropped once Windows' drop call has returned.
const WM_APP_DROPPED: u32 = WM_APP + 7;
/// Posted when a chooser (in its helper process) has been answered or closed.
const WM_APP_PICKED: u32 = WM_APP + 8;
/// The settings window saved dock.toml: reload now (sent by settings.rs; keep the number).
const WM_APP_RELOAD_NOW: u32 = WM_APP + 9;
/// The settings window changed something you can see: come out for a moment to show it.
const WM_APP_PEEK: u32 = WM_APP + 10;
/// The settings window: a Look slider is moving. Show its value now, without saving it
/// (wparam: which, in settings.rs' PREVIEWED order; lparam: the value × 10).
const WM_APP_PREVIEW: u32 = WM_APP + 11;
/// Posted when the second look at missing items finishes (wparam 1: one of them is back).
const WM_APP_MISSING_CHECKED: u32 = WM_APP + 12;
/// How long the dock stays out after the settings window's last change.
const PEEK_FOR: Duration = Duration::from_secs(3);
/// How long the dock stays out after the keyboard shortcut, unless the pointer goes to it.
const SUMMON_FOR: Duration = Duration::from_secs(5);
/// A release counts as missed once the button has been up this long without the dock hearing
/// of it. Windows shows the button up a moment before the release reaches the dock, so a look
/// in between mustn't take a release that's on its way for a missed one (a drag would then be
/// cancelled instead of dropped).
const MISSED_RELEASE_AFTER: Duration = Duration::from_millis(150);
/// The keyboard shortcut's id with RegisterHotKey.
const HOTKEY_ID: i32 = 1;
const SHAKE: Duration = Duration::from_millis(450);
/// Zoom-size icons are made when an icon is first hovered; only this many are kept.
const ZOOM_KEEP: usize = 6;
/// The Recycle Bin is checked when the dock appears, at most this often (it may wake drives).
const BIN_CHECK_EVERY: Duration = Duration::from_secs(10);
const BOUNCE: Duration = Duration::from_millis(700);
/// The glow's colour when Windows doesn't say what its accent colour is: a soft blue.
const DEFAULT_GLOW: [f32; 3] = [0.35, 0.65, 1.0];
/// The Glow pulse launch effect: up quickly, then fading.
const PULSE: Duration = Duration::from_millis(700);
/// Windows changed its accent colour (sent to top-level windows).
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const LINGER_AT_START: Duration = Duration::from_millis(1500);
/// How often to check whether dock.toml was saved, when Windows can't tell the dock (one
/// file-date lookup a second). When it can, a slow check stays on as a safety net.
const CONFIG_CHECK: Duration = Duration::from_secs(1);
const CONFIG_CHECK_WATCHED: Duration = Duration::from_secs(10);

thread_local! {
    static DOCK: RefCell<Option<Dock>> = const { RefCell::new(None) };
}

/// Things that must happen outside the dock's borrow, because they can run a nested
/// message loop (menus, message boxes) or should not block it (launching).
enum Action {
    /// The item, and where it sits (index and layout generation) for a failure cue.
    Launch(ItemConfig, usize, u64),
    /// Where to open it, and the item under the pointer (its menu can add item actions).
    Menu(POINT, Option<usize>),
    Error(String),
    /// dock.toml couldn't be read or has an error: the dock carries on as it was.
    FileError(String),
    /// A program dropped onto another: ask before replacing (index, its label, the new one).
    ConfirmReplace(usize, String, ItemConfig),
    /// Right-click on an item in an open group's pop-up: where, the group, the item.
    ChildMenu(POINT, usize, usize),
    /// Test only: a message loop of its own for this many seconds (WM_APP_TEST_MODAL).
    TestModal(u64),
}

struct Item {
    cfg: ItemConfig,
    label: Vec<u16>,
    /// Where the icon comes from (paths resolved), best first.
    sources: Vec<IconSource>,
    base: Option<ID2D1Bitmap>,
    /// Zoom-size icon, made on first hover (see ZOOM_KEEP).
    zoom: Option<ID2D1Bitmap>,
    zoom_requested: bool,
    hover_t: f32,
    bounce_start: Option<Instant>,
    /// The Glow pulse launch effect.
    pulse_start: Option<Instant>,
    /// The launch failed: a short shake.
    shake_start: Option<Instant>,
    /// Sideways offset while it makes room for a dragged icon, in pixels.
    shift: f32,
    /// A group's items (none for anything else).
    children: Vec<Child>,
}

/// One of a group's items.
struct Child {
    cfg: ItemConfig,
    label: Vec<u16>,
    sources: Vec<IconSource>,
    /// At the dock's icon size, for the pop-up.
    icon: Option<ID2D1Bitmap>,
    /// Small, for the group's tile (the first four).
    preview: Option<ID2D1Bitmap>,
}

impl Item {
    fn new(cfg: ItemConfig) -> Self {
        let label = cfg.name.encode_utf16().collect();
        let children = cfg
            .items
            .iter()
            .map(|child| Child {
                cfg: child.clone(),
                label: edit::label_of(child).encode_utf16().collect(),
                sources: Vec::new(),
                icon: None,
                preview: None,
            })
            .collect();
        Self {
            cfg,
            label,
            sources: Vec::new(),
            base: None,
            zoom: None,
            zoom_requested: false,
            hover_t: 0.0,
            bounce_start: None,
            pulse_start: None,
            shake_start: None,
            shift: 0.0,
            children,
        }
    }

    /// A group without an icon of its own shows its first four items on a tile.
    fn shows_preview(&self) -> bool {
        self.cfg.kind == Kind::Group && self.cfg.icon.trim().is_empty()
    }

    fn bounce(&self, amplitude: f32) -> f32 {
        self.bounce_start.map_or(0.0, |start| {
            motion::bounce_offset(start.elapsed().as_secs_f32() / BOUNCE.as_secs_f32(), amplitude)
        })
    }

    fn shake(&self, amplitude: f32) -> f32 {
        self.shake_start.map_or(0.0, |start| {
            motion::shake_offset(start.elapsed().as_secs_f32() / SHAKE.as_secs_f32(), amplitude)
        })
    }

    /// How strong the Glow pulse is now (0 to 1): up quickly, then fading.
    fn pulse(&self) -> f32 {
        self.pulse_start.map_or(0.0, |start| {
            let t = start.elapsed().as_secs_f32() / PULSE.as_secs_f32();
            match t {
                _ if t >= 1.0 => 0.0,
                _ if t < 0.15 => t / 0.15,
                _ => (1.0 - (t - 0.15) / 0.85).powi(2),
            }
        })
    }

    fn effect_running(&self) -> bool {
        self.bounce_start.is_some() || self.pulse_start.is_some() || self.shake_start.is_some()
    }
}

pub struct Dock {
    hwnd: Option<HWND>,
    home: Home,
    /// None in snapshot mode (no reloading or saving).
    store: Option<Store>,
    mode: Mode,
    settings: DockSettings,
    items: Vec<Item>,
    layout: Layout,
    renderer: Renderer,
    brush: ID2D1SolidColorBrush,
    fill: Option<ID2D1LinearGradientBrush>,
    /// Windows' accent colour, brightened to glow (see `HoverGlow::Accent`).
    accent: [f32; 3],
    /// The glow's brushes, one per colour (made as needed; dropped when the layout changes).
    glow_mask: Option<ID2D1Bitmap>,
    /// Each icon's main colour, by where it comes from (None: hardly any colour). Kept across
    /// reloads like the icons themselves.
    tints: HashMap<Vec<IconSource>, Option<[f32; 3]>>,
    text_format: Option<IDWriteTextFormat>,
    /// Where the dock lives: the work area's left, top and right (beside and below any taskbar
    /// or app bar), and the screen's bottom.
    monitor: RECT,
    /// The screen it's on (see `system::dock_screen`); its whole area is for fullscreen checks.
    screen: system::Screen,
    win_x: i32,
    /// 0 = hidden above the screen edge, 1 = fully shown.
    reveal: f32,
    reveal_target: f32,
    window_visible: bool,
    hovered: Option<usize>,
    pressed: Option<usize>,
    clicks: ClickGuard,
    tracking_mouse: bool,
    /// Pointer resting at the screen edge (popup delay).
    edge: Dwell,
    /// Pointer away from the shown dock (hide delay).
    leave: Dwell,
    linger_until: Option<Instant>,
    /// Since when the button has been up while a press or a drag is still on (its release not
    /// heard yet): see `MISSED_RELEASE_AFTER`.
    button_up_since: Option<Instant>,
    /// Showing settings from the settings window that aren't saved (yet): see `preview`.
    previewing: bool,
    poll_ms: u32,
    menu_open: bool,
    last_frame: Option<Instant>,
    watch: ChangeWatch<SystemTime>,
    last_config_check: Option<Instant>,
    /// None in snapshot mode, where icons load directly.
    icons: Option<IconWorker>,
    /// Bumped on every relayout so late icon results from an older layout are ignored.
    generation: u64,
    zoom_lru: VecDeque<usize>,
    bin_full: bool,
    bin_checked: Option<Instant>,
    bin_result: Arc<Mutex<Option<bool>>>,
    /// A Recycle Bin check is under way; another is wanted once it's done (after emptying it).
    bin_checking: bool,
    bin_check_again: bool,
    /// Process start, until every icon has arrived (for the startup-time log line).
    started: Option<Instant>,
    pending_base: usize,
    /// Items whose program is missing and couldn't be repaired (drawn dimmed).
    missing: HashSet<usize>,
    healing: bool,
    /// Started by the watchdog after quick crashes: cached icons only, no repairs, no shell checks.
    safe_mode: bool,
    /// Slide by moving the drawn window (cheap) unless another monitor sits above this one,
    /// where a moving window would show; then the drawing slides inside a fixed window.
    slide_by_moving: bool,
    heal_result: Arc<Mutex<Option<heal::Report>>>,
    /// Programs already mentioned in a repair notice this session.
    repairs_told: HashSet<String>,
    /// The monitor DPI the layout was made for (with `monitor`, to notice changes).
    dpi: u32,
    /// Locked, switched away from or display off: no polling at all.
    presence: system::Presence,
    /// Windows' "Animation effects" is off: no sliding, zooming, bouncing or shaking.
    reduced_motion: bool,
    /// Signalled when a file in the dock folder changes (None: check dock.toml every second).
    folder_watch: Option<HANDLE>,
    /// The last keyboard or mouse input when the dock decided nobody was there.
    away_input_tick: u32,
    /// The keyboard shortcut registered now (see `apply_hotkey`).
    hotkey: Option<hotkey::Hotkey>,
    /// A fingerprint of the dock.toml text last read (see WM_APP_RELOAD_NOW).
    loaded_text: Option<u64>,
    /// Reads of dock.toml tried again in a row, since when it couldn't be read, and whether
    /// that's been said (see `reload`).
    read_retries: u32,
    unreadable_since: Option<Instant>,
    unreadable_told: bool,
    /// What to say if dock.toml still can't be read a while after starting (the dock runs on
    /// its last working setup meanwhile).
    start_notice: Option<String>,
    /// When dock.toml was first seen missing (it's put back once it stays gone).
    missing_since: Option<Instant>,
    /// When missing items and placeholder icons last had a second look, and whether one is
    /// under way (see `recheck_missing`).
    missing_checked: Option<Instant>,
    missing_checking: bool,
    placeholder_retries: u32,
    /// Something missing is back: refresh the dock when it next hides.
    refresh_on_hide: bool,
    /// Windows' "TaskbarCreated" message: Explorer has (re)started.
    taskbar_created: u32,
    /// Something is being dragged in from outside (files, apps, links); where it would land.
    incoming: Option<Incoming>,
    /// An invisible strip along the top edge that tells the hidden dock about drags of files,
    /// apps or links (Windows' drag and drop), shown only while a button is held near the edge.
    drop_zone: Option<HWND>,
    drop_zone_shown: bool,
    /// When the zone went out, and whether Windows has been nudged to notice it (see on_poll).
    zone_since: Option<Instant>,
    zone_nudged: bool,
    /// Dropped items waiting to be added, just after Windows' drop call has returned.
    pending_drop: Option<PendingDrop>,
    /// A chooser is open (one at a time), and where its answer lands.
    picking: bool,
    picked: Arc<Mutex<Option<(Purpose, Vec<String>)>>>,
    /// The latest change, undoable while its strip shows.
    undo: Option<UndoStep>,
    /// Recently removed items (None in snapshot and bench modes).
    removed: Option<Removed>,
    /// "Removed X · Undo" under the panel after a change, until it times out.
    strip: Option<Strip>,
    /// The left button went down on this item, here (screen); a drag starts once it moves.
    press: Option<(usize, POINT)>,
    /// The same while icons are locked (no mouse held): moving far enough shows the lock notice.
    locked_press: Option<POINT>,
    drag: Option<Drag>,
    /// Carries the dragged icon under the pointer, also outside the dock.
    drag_window: Option<HWND>,
    /// The open group, if any, and the window its pop-up shows in (see popup.rs).
    group: Option<OpenGroup>,
    /// The left button went down on an item in a group's pop-up (group, item, where on the
    /// screen); a drag starts once it moves. The pop-up holds the mouse meanwhile.
    child_press: Option<(usize, usize, POINT)>,
    popup_wrap_format: Option<IDWriteTextFormat>,
    popup_window: Option<HWND>,
    /// A pop-up's names (cut short with "…") and its title.
    popup_format: Option<IDWriteTextFormat>,
    popup_title_format: Option<IDWriteTextFormat>,
    /// Items inside groups whose program is missing: (group, item).
    missing_children: HashSet<(usize, usize)>,
    /// Off while a test opens and closes a group thousands of times (the log would fill up).
    log_groups: bool,
    /// A closed group's pop-up still on screen, fading out: since when, how long it holds first,
    /// and where it is.
    popup_fading: Option<(Instant, Duration, i32, i32)>,
}

/// An open group: which, where its pop-up is, and what the pointer is doing on it.
struct OpenGroup {
    index: usize,
    layout: popup::Layout,
    hovered: Option<usize>,
    pressed: Option<usize>,
    closer: popup::Closer,
    tracking: bool,
}

/// Things dragged in from outside, over the dock: the gap they'd go into (None: not over it).
struct Incoming {
    gap: Option<usize>,
    /// Over the middle of this item: the drop would go onto it (see `takes_drops`).
    onto: Option<usize>,
    /// What would happen, shown under that item: "Open with Photoshop".
    label: Vec<u16>,
}

/// What a chooser's answer is for.
enum Purpose {
    /// New items, into this gap.
    Add(usize),
}

/// A drop waiting to be handled, just after Windows' drop call has returned.
enum PendingDrop {
    /// Add these in the gap.
    Add(usize, Vec<ItemConfig>),
    /// Dropped onto the item at this index (with this label, to check it's still the one).
    Onto(usize, String, drop::Dropped),
}

/// One change that can be undone.
struct UndoStep {
    /// What it did, for the menu: "remove Photoshop".
    what: String,
    change: store::Change,
    /// A removal also went into Recently removed; undoing takes it back out of there.
    removed_id: Option<String>,
    /// A Put back took it out of Recently removed; undoing returns it there.
    put_back: Option<removed::Entry>,
}

/// An icon being dragged: along the dock to move it, or well away to remove it.
struct Drag {
    /// The dock item dragged, or (with `child`) the group it's taken from.
    index: usize,
    /// An item dragged out of a group's pop-up: which one.
    child: Option<usize>,
    label: String,
    /// Where it was grabbed, from the icon's top-left, so that point stays under the pointer.
    grab: (f32, f32),
    /// Its drawn size when picked up.
    size: f32,
    /// The gap it would drop into, counted among the other items; None: off the dock (remove).
    gap: Option<usize>,
    /// Which look the drag image has now (true: the remove look); None before it's drawn.
    image_off: Option<bool>,
    /// Since when it's been off the dock (a whole group needs holding there a moment).
    off_since: Option<Instant>,
    /// Onto a group on the dock (it goes in at the end).
    into: Option<usize>,
    /// Over an open group's pop-up: that group, and the place among its items.
    in_popup: Option<(usize, usize)>,
    /// Over this group since then (it springs open after a moment).
    over: Option<(usize, Instant)>,
    /// This drag sprang a group open (it closes again if the drag moves on along the dock).
    sprung: bool,
}

/// The undo strip: its text, where it sits in the window, and its parts.
struct Strip {
    text: Vec<u16>,
    rect: D2D_RECT_F,
    text_rect: D2D_RECT_F,
    /// Where "Undo" sits; None for a plain notice ("Icons are locked").
    undo_rect: Option<D2D_RECT_F>,
}

/// The glow mask's width and height in pixels: smooth when stretched to the largest glow.
const GLOW_MASK_SIDE: u32 = 128;

/// The glow's shape as a white image whose alpha is the falloff (premultiplied), `side` pixels
/// square: full to 45% of the radius, then (1 - u)^2.5 to nothing at the edge.
fn glow_shape(side: u32) -> icons::Pixels {
    let falloff = |t: f32| (1.0 - ((t - 0.45) / 0.55).clamp(0.0, 1.0)).powf(2.5);
    let centre = side as f32 / 2.0;
    let mut data = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let (dx, dy) = (x as f32 + 0.5 - centre, y as f32 + 0.5 - centre);
            let alpha = (falloff((dx * dx + dy * dy).sqrt() / centre) * 255.0).round() as u8;
            data.extend_from_slice(&[alpha; 4]);
        }
    }
    icons::Pixels { width: side, height: side, data }
}

/// A text as a JSON string (for the test state).
fn json_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_rect(left: f32, top: f32, right: f32, bottom: f32) -> String {
    format!(r#"{{"left":{},"top":{},"right":{},"bottom":{}}}"#, left.round(), top.round(), right.round(), bottom.round())
}

fn json_index(index: Option<usize>) -> String {
    index.map_or("null".to_string(), |i| i.to_string())
}

/// A quick fingerprint of a text, to tell whether dock.toml changed since it was read.
fn fingerprint(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

impl Dock {
    fn new(hwnd: Option<HWND>, config: Config, home: Home, store: Option<Store>, mode: Mode) -> Result<Self> {
        let renderer = Renderer::new()?;
        let brush = unsafe { renderer.rt.CreateSolidColorBrush(&rgba(1.0, 1.0, 1.0, 1.0), None)? };
        let watch = ChangeWatch::new(store.as_ref().and_then(|s| modified(s.config_path())));
        Ok(Self {
            hwnd,
            home,
            store,
            mode,
            settings: config.dock,
            items: config.items.into_iter().map(Item::new).collect(),
            layout: Layout::default(),
            renderer,
            brush,
            fill: None,
            accent: system::accent_colour().map_or(DEFAULT_GLOW, icons::glow_bright),
            glow_mask: None,
            tints: HashMap::new(),
            text_format: None,
            monitor: RECT::default(),
            screen: system::Screen::default(),
            win_x: 0,
            reveal: 0.0,
            reveal_target: 0.0,
            window_visible: false,
            hovered: None,
            pressed: None,
            clicks: ClickGuard::default(),
            tracking_mouse: false,
            edge: Dwell::default(),
            leave: Dwell::default(),
            linger_until: None,
            button_up_since: None,
            previewing: false,
            poll_ms: 0,
            menu_open: false,
            last_frame: None,
            watch,
            last_config_check: None,
            icons: None,
            generation: 0,
            zoom_lru: VecDeque::new(),
            bin_full: false,
            bin_checked: None,
            bin_result: Arc::new(Mutex::new(None)),
            bin_checking: false,
            bin_check_again: false,
            started: None,
            pending_base: 0,
            missing: HashSet::new(),
            healing: false,
            safe_mode: false,
            slide_by_moving: true,
            heal_result: Arc::new(Mutex::new(None)),
            repairs_told: HashSet::new(),
            dpi: 0,
            presence: system::Presence::default(),
            reduced_motion: false,
            folder_watch: None,
            away_input_tick: 0,
            hotkey: None,
            loaded_text: None,
            read_retries: 0,
            unreadable_since: None,
            unreadable_told: false,
            start_notice: None,
            missing_since: None,
            missing_checked: None,
            missing_checking: false,
            placeholder_retries: PLACEHOLDER_RETRIES,
            refresh_on_hide: false,
            taskbar_created: 0,
            incoming: None,
            drop_zone: None,
            drop_zone_shown: false,
            zone_since: None,
            zone_nudged: false,
            pending_drop: None,
            picking: false,
            picked: Arc::new(Mutex::new(None)),
            undo: None,
            removed: None,
            strip: None,
            press: None,
            locked_press: None,
            drag: None,
            drag_window: None,
            group: None,
            child_press: None,
            popup_wrap_format: None,
            popup_window: None,
            popup_format: None,
            popup_title_format: None,
            missing_children: HashSet::new(),
            log_groups: true,
            popup_fading: None,
        })
    }

    /// Recomputes every size for the current monitor and DPI, and reloads icons at the
    /// exact pixel sizes they will be drawn.
    fn relayout(&mut self) {
        let previous = self.take_icons();
        self.relayout_keeping(previous);
    }

    /// Takes the current icons out of the items, keyed by (sources, size), for reuse. The size is
    /// the image's own: during a preview, or until a new size arrives, it can differ from the
    /// layout's.
    fn take_icons(&mut self) -> HashMap<(Vec<IconSource>, u32), ID2D1Bitmap> {
        let mut taken = HashMap::new();
        let mut keep = |sources: &Vec<IconSource>, bitmap: Option<ID2D1Bitmap>| {
            if let Some(bitmap) = bitmap {
                let size = unsafe { bitmap.GetPixelSize() }.width;
                taken.insert((sources.clone(), size), bitmap);
            }
        };
        for item in &mut self.items {
            keep(&item.sources, item.base.take());
            for child in &mut item.children {
                keep(&child.sources, child.icon.take());
                keep(&child.sources, child.preview.take());
            }
        }
        taken
    }

    fn relayout_keeping(&mut self, previous: HashMap<(Vec<IconSource>, u32), ID2D1Bitmap>) {
        self.apply_layout();
        self.load_icons(previous);
        self.settle_if_hidden();
    }

    /// Sizes, positions, label text and background for the current settings, monitor and DPI.
    /// Icons stay as they are (drawn scaled if their size changed).
    fn apply_layout(&mut self) {
        self.strip = None; // its place depends on the layout
        let screen = system::dock_screen(&self.settings.monitor);
        if screen.id != self.screen.id {
            let chosen = &self.settings.monitor;
            let fallback = if !chosen.eq_ignore_ascii_case("main") && !screen.id.eq_ignore_ascii_case(chosen) { " (the chosen screen isn't there)" } else { "" };
            log_info!(
                "on screen {} ({}x{} at {} dpi){fallback}",
                screen.number,
                screen.monitor.right - screen.monitor.left,
                screen.monitor.bottom - screen.monitor.top,
                screen.dpi
            );
        }
        let monitor = RECT { left: screen.work.left, top: screen.work.top, right: screen.work.right, bottom: screen.monitor.bottom };
        let dpi = screen.dpi;
        self.monitor = monitor;
        self.dpi = dpi;
        // Sliding by moving the window passes above the dock's top edge: never over a taskbar at
        // the top or onto a screen above. Then it slides by drawing instead.
        self.slide_by_moving = !system::monitor_above(screen.monitor) && screen.work.top == screen.monitor.top;
        self.screen = screen;
        let separators: Vec<bool> = self.items.iter().map(|item| item.cfg.is_separator()).collect();
        // Shrink to fit the screen, keeping a small margin at each end.
        let margin = (8.0 * dpi as f32 / 96.0).round();
        let max_width = (monitor.right - monitor.left) as f32 - 2.0 * margin;
        self.layout = layout::compute(&self.settings, &separators, dpi, Some(max_width));
        self.win_x = self.layout.window_x(monitor.left, monitor.right);
        let g = self.layout.geo;

        unsafe {
            // The user's language, for DirectWrite's font fallback and line breaking.
            let locale = system::locale_name();
            self.text_format = self
                .renderer
                .dwrite
                .CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_SEMI_BOLD,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    self.settings.label_size * g.scale,
                    &locale,
                )
                .ok();
            if let Some(format) = &self.text_format {
                let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
            }
            // A group's pop-up: names cut short with "…" when they're long, and a title.
            let dwrite = self.renderer.dwrite.clone();
            let make = |weight: DWRITE_FONT_WEIGHT, size: f32, wrap: bool, trim: bool| -> Option<IDWriteTextFormat> {
                let format = dwrite.CreateTextFormat(w!("Segoe UI"), None, weight, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, size, &locale).ok()?;
                let _ = format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
                let _ = format.SetWordWrapping(if wrap { DWRITE_WORD_WRAPPING_WRAP } else { DWRITE_WORD_WRAPPING_NO_WRAP });
                if trim {
                    if let Ok(sign) = dwrite.CreateEllipsisTrimmingSign(&format) {
                        let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                        let _ = format.SetTrimming(&trimming, &sign);
                    }
                }
                Some(format)
            };
            let size = self.settings.label_size * g.scale;
            self.popup_format = make(DWRITE_FONT_WEIGHT_NORMAL, size, true, true); // two lines, then "…"
            self.popup_wrap_format = make(DWRITE_FONT_WEIGHT_NORMAL, size, true, false);
            self.popup_title_format = make(DWRITE_FONT_WEIGHT_SEMI_BOLD, size + 1.5 * g.scale, false, true);

            // CrystalXP's fill: white, brightening from 8% at the screen edge to 49% at the bottom
            // (times the background opacity setting).
            let solid = self.settings.background_opacity as f32 / 100.0;
            let look = if crate::breaks::broken("look.dark-ignored") { DockLook::Light } else { self.settings.look };
            let stops = match look {
                DockLook::Light => [
                    D2D1_GRADIENT_STOP { position: 0.0, color: rgba(1.0, 1.0, 1.0, 0.08 * solid) },
                    D2D1_GRADIENT_STOP { position: 1.0, color: rgba(1.0, 1.0, 1.0, 0.49 * solid) },
                ],
                // Dark: smoked glass, deepening toward the bottom edge.
                DockLook::Dark => [
                    D2D1_GRADIENT_STOP { position: 0.0, color: rgba(0.06, 0.06, 0.08, 0.55 * solid) },
                    D2D1_GRADIENT_STOP { position: 1.0, color: rgba(0.06, 0.06, 0.08, 0.80 * solid) },
                ],
            };
            let rt = &self.renderer.rt;
            self.fill = rt
                .CreateGradientStopCollection(&stops, D2D1_GAMMA_2_2, D2D1_EXTEND_MODE_CLAMP)
                .and_then(|collection| {
                    rt.CreateLinearGradientBrush(
                        &D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                            startPoint: Vector2 { X: 0.0, Y: 0.0 },
                            endPoint: Vector2 { X: 0.0, Y: g.panel_h },
                        },
                        None,
                        &collection,
                    )
                })
                .ok();
        }
    }

    /// Replaced icons are only really freed when a frame is drawn (see `Renderer::settle`), so
    /// while the dock is hidden this lets Direct2D tidy up. Without it, a hidden dock that
    /// reloads leaks every replaced icon (about 1 MB per reload).
    fn settle_if_hidden(&mut self) {
        if !self.window_visible {
            self.tidy_hidden();
        }
    }

    /// Starts loading every item's icon at the current size, on the worker thread. Items whose
    /// icon didn't change (same sources and size) keep their image, so reloads don't flicker.
    /// Fail soft: an icon that can't be loaded shows a placeholder, never an error.
    fn load_icons(&mut self, mut previous: HashMap<(Vec<IconSource>, u32), ID2D1Bitmap>) {
        self.generation += 1;
        self.zoom_lru.clear();
        self.pending_base = 0;
        let g = self.layout.geo;
        let (base_px, zoom_px) = (g.icon as u32, g.hover_icon as u32);
        let preview_px = (g.hover_icon.max(g.icon) * PREVIEW_FRACTION).round().max(8.0) as u32;
        let direct = if self.icons.is_none() { IconLoader::new(None).ok() } else { None };
        let mut keep = Vec::new();
        for (index, item) in self.items.iter_mut().enumerate() {
            item.zoom = None;
            item.zoom_requested = false;
            if item.cfg.is_separator() {
                continue;
            }
            // A group's items: each at the dock's size for its pop-up, and the first four small
            // for its tile (unless it has an icon of its own).
            let preview = item.shows_preview();
            let mut previewed = 0;
            for (k, child) in item.children.iter_mut().enumerate() {
                if child.cfg.is_separator() {
                    continue;
                }
                child.sources = icons::sources_for(&resolved(&self.home, &child.cfg), false);
                if child.sources.is_empty() {
                    continue;
                }
                let mut wanted = vec![(Part::Child(k), base_px)];
                if preview && previewed < 4 {
                    wanted.push((Part::Preview(k), preview_px));
                    previewed += 1;
                }
                for (part, size) in wanted {
                    keep.push((child.sources.clone(), size));
                    let slot = if part == Part::Child(k) { &mut child.icon } else { &mut child.preview };
                    *slot = previous.remove(&(child.sources.clone(), size));
                    if slot.is_some() {
                        continue;
                    }
                    match (&self.icons, &direct) {
                        (Some(worker), _) => {
                            worker.request(Job { generation: self.generation, item: index, size, part, sources: child.sources.clone(), also: None })
                        }
                        (None, Some(loader)) => {
                            let pixels = loader.load(&child.sources, size);
                            if part == Part::Child(k) {
                                self.tints.insert(child.sources.clone(), pixels.as_ref().and_then(icons::main_colour));
                            }
                            *slot = pixels.and_then(|p| self.renderer.bitmap(&p).ok());
                        }
                        (None, None) => {}
                    }
                }
            }
            let resolved = resolved(&self.home, &item.cfg);
            item.sources = icons::sources_for(&resolved, self.bin_full);
            if item.sources.is_empty() {
                continue; // nothing to make an icon from (a group showing its tile, say)
            }
            keep.push((item.sources.clone(), base_px));
            keep.push((item.sources.clone(), zoom_px));
            if item.cfg.kind == Kind::RecycleBin {
                let other = icons::sources_for(&resolved, !self.bin_full);
                keep.push((other.clone(), base_px));
                keep.push((other, zoom_px));
            }
            item.base = previous.remove(&(item.sources.clone(), base_px));
            if item.base.is_some() {
                continue;
            }
            // Not at this size yet: until it arrives, the old size stands in (scaled), so a size
            // change never flashes empty placeholders.
            let stand_in = previous.keys().find(|(sources, _)| *sources == item.sources).cloned();
            item.base = stand_in.and_then(|key| previous.remove(&key));
            match (&self.icons, &direct) {
                (Some(worker), _) => {
                    self.pending_base += 1;
                    worker.request(Job {
                        generation: self.generation,
                        item: index,
                        size: base_px,
                        part: Part::Base,
                        sources: item.sources.clone(),
                        also: (zoom_px > base_px).then_some(zoom_px), // the hover size, made in the same go
                    })
                }
                (None, Some(loader)) => {
                    let pixels = loader.load(&item.sources, base_px);
                    self.tints.insert(item.sources.clone(), pixels.as_ref().and_then(icons::main_colour));
                    item.base = pixels.and_then(|p| self.renderer.bitmap(&p).ok());
                }
                (None, None) => {}
            }
        }
        let wanted: HashSet<&Vec<IconSource>> = keep.iter().map(|(sources, _)| sources).collect();
        self.tints.retain(|sources, _| wanted.contains(sources));
        if let Some(worker) = &self.icons {
            worker.prune(keep);
        }
    }

    /// Takes finished icons from the worker and puts them on the dock.
    fn receive_icons(&mut self) {
        let Some(worker) = &self.icons else { return };
        let done: Vec<icons::Done> = worker.done.lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default();
        let mut changed = false;
        let mut popup_changed = false;
        for result in done {
            if result.generation != self.generation || result.item >= self.items.len() {
                continue; // from an older layout
            }
            // An icon's main colour, for the glow (from the dock-size icons, not the zoom ones).
            if result.part != Part::Zoom {
                let item = &self.items[result.item];
                let sources = match result.part {
                    Part::Child(k) | Part::Preview(k) => item.children.get(k).map(|child| &child.sources),
                    _ => Some(&item.sources),
                };
                if let Some(sources) = sources.filter(|sources| !self.tints.contains_key(*sources)) {
                    let tint = result.pixels.as_ref().and_then(icons::main_colour);
                    self.tints.insert(sources.clone(), tint);
                }
            }
            let bitmap = result.pixels.and_then(|p| self.renderer.bitmap(&p).ok());
            let item = &mut self.items[result.item];
            if result.part == Part::Zoom {
                item.zoom = bitmap;
                self.zoom_lru.retain(|&i| i != result.item);
                self.zoom_lru.push_back(result.item);
                while self.zoom_lru.len() > ZOOM_KEEP {
                    if let Some(oldest) = self.zoom_lru.pop_front() {
                        self.items[oldest].zoom = None;
                        self.items[oldest].zoom_requested = false;
                    }
                }
            } else {
                match result.part {
                    Part::Child(k) => {
                        if let Some(child) = item.children.get_mut(k) {
                            child.icon = bitmap;
                        }
                        popup_changed |= self.group.as_ref().is_some_and(|open| open.index == result.item);
                    }
                    Part::Preview(k) => {
                        if let Some(child) = item.children.get_mut(k) {
                            child.preview = bitmap;
                        }
                    }
                    _ => {
                        item.base = bitmap;
                        self.pending_base = self.pending_base.saturating_sub(1);
                    }
                }
            }
            changed = true;
        }
        if popup_changed {
            self.render_popup();
        }
        if self.pending_base == 0 {
            if let Some(started) = self.started.take() {
                log_info!("all icons loaded {} ms after start", started.elapsed().as_millis());
            }
        }
        if changed {
            if self.window_visible { self.render() } else { self.settle_if_hidden() }
        }
    }

    /// Asks for the zoom-size icon the first time an item is hovered.
    fn request_zoom(&mut self, index: usize) {
        let zoom_px = self.layout.geo.hover_icon as u32;
        let Some(item) = self.items.get_mut(index) else { return };
        if item.zoom.is_some() || item.zoom_requested || zoom_px <= self.layout.geo.icon as u32 || item.sources.is_empty() {
            return;
        }
        if let Some(worker) = &self.icons {
            item.zoom_requested = true;
            worker.request(Job { generation: self.generation, item: index, size: zoom_px, part: Part::Zoom, sources: item.sources.clone(), also: None });
        }
    }

    /// Checks whether the Recycle Bin has anything in it, off the UI thread and at most every
    /// few seconds, so drives that are asleep aren't woken more than needed.
    fn check_recycle_bin(&mut self) {
        let has_bin = self.items.iter().any(|item| item.cfg.kind == Kind::RecycleBin);
        // One check at a time: a sleeping drive can make one take longer than BIN_CHECK_EVERY.
        if !has_bin || self.safe_mode || self.bin_checking || self.bin_checked.is_some_and(|at| at.elapsed() < BIN_CHECK_EVERY) {
            return;
        }
        self.bin_checked = Some(Instant::now());
        self.bin_checking = true;
        let Some(hwnd) = self.hwnd else { return };
        let result = self.bin_result.clone();
        let hwnd_value = hwnd.0 as isize;
        std::thread::spawn(move || {
            let started = Instant::now();
            let mut info = SHQUERYRBINFO { cbSize: size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
            let full = unsafe { SHQueryRecycleBinW(PCWSTR::null(), &mut info) }.is_ok() && info.i64NumItems > 0;
            let took = started.elapsed();
            if took > Duration::from_millis(500) {
                log_info!("Recycle Bin check took {} ms (a drive may have been asleep)", took.as_millis());
            }
            if let Ok(mut slot) = result.lock() {
                *slot = Some(full);
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd_value as *mut std::ffi::c_void)), WM_APP_BIN, WPARAM(0), LPARAM(0));
            }
        });
    }

    /// Checks items for programs that moved (e.g. an app update) on a worker thread, repairs
    /// dock.toml where the fix is unambiguous, and reports the rest. Never in safe start or
    /// read-only mode, where the file mustn't be saved over.
    fn start_heal(&mut self) {
        let (Some(store), Some(hwnd)) = (self.store.clone(), self.hwnd) else { return };
        if self.mode != Mode::Normal || self.healing || self.safe_mode {
            return;
        }
        self.healing = true;
        let home = self.home.clone();
        let result = self.heal_result.clone();
        let hwnd_value = hwnd.0 as isize;
        std::thread::spawn(move || {
            let report = heal::heal(&store, &home, &heal::RealFs);
            if let Ok(mut slot) = result.lock() {
                *slot = Some(report);
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd_value as *mut std::ffi::c_void)), WM_APP_HEALED, WPARAM(0), LPARAM(0));
            }
        });
    }

    /// A healing pass finished: dim what's missing, and say what was repaired. (A repair
    /// changes dock.toml, which the dock then reloads by itself.)
    fn receive_heal(&mut self) {
        self.healing = false;
        let Some(report) = self.heal_result.lock().ok().and_then(|mut slot| slot.take()) else { return };
        self.missing = report.missing.iter().filter(|spot| spot.child.is_none()).map(|spot| spot.index).filter(|&i| i < self.items.len()).collect();
        self.missing_children = report.missing.iter().filter_map(|spot| spot.child.map(|child| (spot.index, child))).collect();
        for &index in &self.missing {
            let item = &mut self.items[index];
            item.label = format!("{} (not found)", item.cfg.name).encode_utf16().collect();
        }
        // Each program is mentioned once per session, however often it's repaired.
        let news: Vec<&heal::Fix> = report.fixed.iter().filter(|fix| self.repairs_told.insert(fix.name.to_lowercase())).collect();
        if !news.is_empty() {
            let mut text = String::from("Some programs moved when they were updated, so the dock now opens them from their new folders:\n\n");
            for fix in news {
                text.push_str(&format!("• {}\n", fix.line));
            }
            text.push_str("\nYour dock as it was is kept in Dock settings > Versions.");
            system::notice(text);
        }
        if self.window_visible {
            self.render();
        }
    }

    /// At logon a network drive or a synced folder may not be there yet, so items show "(not
    /// found)" and some icons a placeholder. They get a second look, read-only: when the dock
    /// comes out (at most every 5 minutes; `force` after waking or a display change). If one is
    /// back, the dock refreshes (and saves any repair) when it next hides, so nothing you're
    /// doing is interrupted.
    fn recheck_missing(&mut self, force: bool) {
        let Some(hwnd) = self.hwnd else { return };
        if self.safe_mode || self.mode != Mode::Normal || self.missing_checking || self.refresh_on_hide || self.pending_base > 0 {
            return;
        }
        if !force && self.missing_checked.is_some_and(|at| at.elapsed() < MISSING_RECHECK_EVERY) {
            return;
        }
        let placeholders = self.placeholder_retries > 0
            && self.items.iter().any(|item| item.base.is_none() && !item.sources.is_empty() && item.cfg.kind != Kind::Group && !item.cfg.is_separator());
        let targets: Vec<String> = self
            .missing
            .iter()
            .filter_map(|&index| self.items.get(index).map(|item| &item.cfg))
            .chain(self.missing_children.iter().filter_map(|&(index, child)| self.items.get(index)?.children.get(child).map(|child| &child.cfg)))
            .map(|cfg| self.home.resolve(&cfg.target))
            .collect();
        if targets.is_empty() && !placeholders {
            return;
        }
        self.missing_checked = Some(Instant::now());
        if placeholders {
            self.placeholder_retries -= 1;
            log_info!("some icons are placeholders; trying them again when the dock hides");
            self.refresh_on_hide = true;
            return;
        }
        self.missing_checking = true;
        let hwnd_value = hwnd.0 as isize;
        std::thread::spawn(move || {
            let back = targets.iter().any(|target| Path::new(target).exists() || heal::find_moved(target, &heal::RealFs).is_some());
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd_value as *mut std::ffi::c_void)), WM_APP_MISSING_CHECKED, WPARAM(back as usize), LPARAM(0));
            }
        });
    }

    /// The second look at missing items finished.
    fn receive_missing_check(&mut self, back: bool) {
        self.missing_checking = false;
        if back {
            log_info!("a missing item is back; refreshing the dock when it hides");
            self.refresh_on_hide = true;
            if !self.window_visible {
                self.refresh_hidden();
            }
        }
    }

    /// Reloads the dock while it's out of sight (icons that were placeholders are tried again,
    /// and missing items are checked and repaired).
    fn refresh_hidden(&mut self) {
        if !std::mem::take(&mut self.refresh_on_hide) {
            return;
        }
        log_info!("refreshing out of sight (placeholder icons or a missing item)");
        if let Err(e) = self.reload_keeping_undo() {
            log_warn!("couldn't refresh the dock: {e}");
        }
    }

    /// For tests: what the dock holds now (see WM_APP_TEST_STATS).
    fn test_stats(&self) -> String {
        let count = |f: &dyn Fn(&Item) -> usize| -> usize { self.items.iter().map(f).sum() };
        format!(
            "private_mb={:.2} items={} base={} zoom={} zoom_lru={} child_icons={} child_previews={} glow_mask={} tints={} labels_kb={} missing={} {} {}",
            system::private_mb(),
            self.items.len(),
            count(&|item| usize::from(item.base.is_some())),
            count(&|item| usize::from(item.zoom.is_some())),
            self.zoom_lru.len(),
            count(&|item| item.children.iter().filter(|child| child.icon.is_some()).count()),
            count(&|item| item.children.iter().filter(|child| child.preview.is_some()).count()),
            usize::from(self.glow_mask.is_some()),
            self.tints.len(),
            count(&|item| item.label.capacity() * 2 + item.children.iter().map(|child| child.label.capacity() * 2).sum::<usize>()) / 1024,
            self.missing.len(),
            self.renderer.describe(),
            system::heap_and_threads(),
        )
    }

    /// For tests: what the dock shows now, as JSON (see WM_APP_TEST_STATE). Positions are real
    /// screen pixels; an icon's rect is where it is when the dock is out.
    fn test_state(&self) -> String {
        let g = self.layout.geo;
        let s = &self.settings;
        let (x, top) = (self.win_x as f32, self.monitor.top as f32);
        let mut window = RECT::default();
        if let Some(hwnd) = self.hwnd {
            let _ = unsafe { GetWindowRect(hwnd, &mut window) };
        }
        let items: Vec<String> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let cfg = &item.cfg;
                // Its icon, and its whole slot (the gaps either side count as it too).
                let place = self.layout.slots.iter().find(|slot| slot.item == index).map_or(r#""rect":null,"slot":null"#.to_string(), |slot| {
                    let (left, centre) = (x + g.origin + slot.left, x + slot.center(&g));
                    let half = if slot.separator { slot.width / 2.0 } else { g.icon / 2.0 };
                    let (icon_top, icon_bottom) = (top + g.pad_top, top + g.pad_top + g.icon);
                    format!(
                        r#""rect":{},"slot":{}"#,
                        json_rect(centre - half, icon_top, centre + half, icon_bottom),
                        json_rect(left, icon_top, left + slot.width, icon_bottom)
                    )
                });
                let children: Vec<String> = item
                    .children
                    .iter()
                    .enumerate()
                    .map(|(child, inner)| {
                        format!(
                            r#"{{"label":{},"target":{},"resolved":{},"missing":{}}}"#,
                            json_text(&edit::label_of(&inner.cfg)),
                            json_text(&inner.cfg.target),
                            json_text(&self.home.resolve(&inner.cfg.target)),
                            self.missing_children.contains(&(index, child)),
                        )
                    })
                    .collect();
                format!(
                    r#"{{"index":{index},"label":{},"shown":{},"name":{},"kind":"{:?}","target":{},"resolved":{},"args":{},"start_in":{},"run":"{:?}","admin":{},"icon":{},"missing":{},"takes_drops":{},{place},"children":[{}]}}"#,
                    json_text(&edit::label_of(cfg)),
                    json_text(&String::from_utf16_lossy(&item.label)), // the name as drawn under it
                    json_text(&cfg.name),
                    cfg.kind,
                    json_text(&cfg.target),
                    json_text(&self.home.resolve(&cfg.target)),
                    json_text(&cfg.args),
                    json_text(&cfg.start_in),
                    cfg.run,
                    cfg.admin,
                    json_text(&cfg.icon),
                    self.missing.contains(&index),
                    self.takes_drops(index),
                    children.join(","),
                )
            })
            .collect();
        let group = self.group.as_ref().map_or("null".to_string(), |open| {
            let l = &open.layout;
            let (gx, gy) = (l.x as f32, l.y as f32);
            let cells: Vec<String> = open
                .layout
                .cells
                .iter()
                .zip(&self.items[open.index].children)
                .map(|(cell, child)| {
                    let r = cell.icon;
                    format!(r#"{{"label":{},"rect":{}}}"#, json_text(&edit::label_of(&child.cfg)), json_rect(gx + r.left, gy + r.top, gx + r.right, gy + r.bottom))
                })
                .collect();
            format!(
                r#"{{"index":{},"rect":{},"hovered":{},"cells":[{}]}}"#,
                open.index,
                json_rect(gx, gy, gx + l.width as f32, gy + l.height as f32),
                json_index(open.hovered),
                cells.join(","),
            )
        });
        // A drag under way: where letting go would put it (a gap among the other items, into a
        // group, or off the dock).
        let drag = self.drag.as_ref().map_or("null".to_string(), |drag| {
            format!(
                r#"{{"index":{},"child":{},"gap":{},"into":{},"off":{}}}"#,
                drag.index,
                json_index(drag.child),
                json_index(drag.gap),
                json_index(drag.into),
                drag.gap.is_none() && drag.into.is_none() && drag.in_popup.is_none()
            )
        });
        let strip = self.strip.as_ref().map_or("null".to_string(), |strip| {
            let on_screen = |r: &D2D_RECT_F| json_rect(x + r.left, top + r.top, x + r.right, top + r.bottom);
            format!(
                r#"{{"text":{},"rect":{},"undo":{}}}"#,
                json_text(&String::from_utf16_lossy(&strip.text)),
                on_screen(&strip.rect),
                strip.undo_rect.as_ref().map_or("null".to_string(), on_screen),
            )
        });
        format!(
            concat!(
                r#"{{"window":{{"rect":{},"visible":{},"reveal":{:.3},"reveal_target":{:.3}}},"#,
                r#""mode":"{:?}","safe_mode":{},"locked":{},"bin_full":{},"menu_open":{},"dragging":{},"picking":{},"hovered":{},"#,
                r#""hotkey":{},"hotkey_registered":{},"screen":{{"id":{},"number":{},"main":{},"dpi":{}}},"#,
                r#""settings":{{"icon_size":{},"zoom_size":{},"zoom_ms":{},"spacing":{},"popup_delay_ms":{},"hide_delay_ms":{},"slide_in_ms":{},"slide_out_ms":{},"show_labels":{},"label_size":{},"background_opacity":{},"icon_opacity":{},"launch_effect":"{:?}","look":"{:?}","hover_glow":"{:?}","monitor":{}}},"#,
                r#""pixels":{{"scale":{},"icon":{},"zoom":{},"gap":{},"panel":{}}},"#,
                r#""undo":{},"strip":{},"drag":{},"group":{},"items":[{}]}}"#,
            ),
            json_rect(window.left as f32, window.top as f32, window.right as f32, window.bottom as f32),
            self.window_visible,
            self.reveal,
            self.reveal_target,
            self.mode,
            self.safe_mode,
            s.locked,
            self.bin_full,
            self.menu_open,
            self.drag.is_some(),
            self.picking,
            json_index(self.hovered),
            json_text(&s.hotkey),
            self.hotkey.is_some(),
            json_text(&self.screen.id),
            json_text(&self.screen.number),
            self.screen.main,
            self.dpi,
            s.icon_size,
            s.zoom_size,
            s.zoom_ms,
            s.spacing,
            s.popup_delay_ms,
            s.hide_delay_ms,
            s.slide_in_ms,
            s.slide_out_ms,
            s.show_labels,
            s.label_size,
            s.background_opacity,
            s.icon_opacity,
            s.launch_effect,
            s.look,
            s.hover_glow,
            json_text(&s.monitor),
            g.scale,
            g.icon,
            g.hover_icon,
            g.gap,
            json_rect(x + g.panel_left, top, x + g.panel_left + g.panel_w, top + g.panel_h),
            self.undo.as_ref().map_or("null".to_string(), |step| json_text(&step.what)),
            strip,
            drag,
            group,
            items.join(","),
        )
    }

    /// The Recycle Bin check finished: switch its icon between empty and full.
    fn receive_bin_state(&mut self) {
        self.bin_checking = false;
        if std::mem::take(&mut self.bin_check_again) {
            self.bin_checked = None;
            self.check_recycle_bin();
        }
        let Some(full) = self.bin_result.lock().ok().and_then(|mut slot| slot.take()) else { return };
        if full == self.bin_full {
            return;
        }
        self.bin_full = full;
        log_info!("Recycle Bin is {}", if full { "full" } else { "empty" });
        let base_px = self.layout.geo.icon as u32;
        for (index, item) in self.items.iter_mut().enumerate() {
            if item.cfg.kind != Kind::RecycleBin || crate::breaks::broken("bin.icon-stale") {
                continue;
            }
            item.sources = icons::sources_for(&resolved(&self.home, &item.cfg), full);
            item.zoom = None;
            item.zoom_requested = false;
            if let Some(worker) = &self.icons {
                let zoom_px = self.layout.geo.hover_icon as u32;
                worker.request(Job { generation: self.generation, item: index, size: base_px, part: Part::Base, sources: item.sources.clone(), also: Some(zoom_px) });
            }
        }
    }

    /// Re-reads dock.toml. On an error the current setup stays as it is and the message is
    /// returned for the caller to show (outside the dock's borrow).
    fn reload(&mut self) -> std::result::Result<(), String> {
        self.cancel_drag(); // positions are about to change
        self.close_group("dock.toml changed");
        let Some(store) = self.store.clone() else { return Ok(()) };
        let path = store.config_path().to_path_buf();
        let stamp = modified(&path);
        let text = match config::read_text(&path) {
            Ok(text) => text,
            // Held by another program (antivirus, a sync app): the current setup stays, and the
            // file is tried again, quietly, for as long as it takes. This version isn't recorded
            // as loaded, so it's read as soon as it can be. Only if it stays unreadable for a
            // while is that said, once.
            Err(e) if e.kind() != std::io::ErrorKind::InvalidData => {
                let since = *self.unreadable_since.get_or_insert_with(Instant::now);
                if self.read_retries == 0 {
                    log_info!("couldn't read dock.toml just now ({e}); trying again");
                }
                self.read_retries = self.read_retries.saturating_add(1);
                self.start_timer(TIMER_READ_RETRY, if self.read_retries < READ_RETRIES { READ_RETRY_MS } else { READ_RETRY_SLOW_MS });
                if since.elapsed() < UNREADABLE_TELL_AFTER || self.unreadable_told {
                    return Ok(());
                }
                self.unreadable_told = true;
                log_warn!("dock.toml still can't be read ({e})");
                return Err(self.start_notice.take().unwrap_or_else(|| {
                    format!(
                        "Your dock's file can't be opened right now: {e}\n\nAnother program may be using it. The dock \
                         keeps its current setup and switches to your changes as soon as the file can be read."
                    )
                }));
            }
            Err(e) => {
                self.read_retries = 0;
                self.watch.loaded(stamp); // reported once, not every second
                return Err(format!(
                    "Your dock's file couldn't be read. {e}\n\nThe dock keeps its current setup until the file is fixed."
                ));
            }
        };
        if self.unreadable_since.take().is_some() {
            log_info!("dock.toml can be read again");
        }
        self.read_retries = 0;
        self.unreadable_told = false;
        self.start_notice = None;
        // Remember this version even if it fails, so a broken file is reported once, not every second.
        self.watch.loaded(stamp);
        self.loaded_text = Some(fingerprint(&text));
        if text.trim().is_empty() {
            log_warn!("dock.toml is empty; keeping the current setup");
            return Err(format!(
                "{} is empty, so the dock keeps your items as they are. It reads the file again once it has items in it (Dock settings > Versions has your recent versions).",
                path.display()
            ));
        }
        let parsed = config::parse(&text)
            .map_err(|e| format!("{} has an error:\n\n{e}\n\nThe dock keeps its current setup until the file is fixed.", path.display()))
            .inspect_err(|e| log_warn!("reload failed: {e}"))?;
        for warning in &parsed.warnings {
            log_warn!("dock.toml: {warning}");
        }
        if self.mode == Mode::SafeStart {
            log_info!("dock.toml is readable again; leaving safe start");
        }
        self.mode = if parsed.read_only { Mode::ReadOnly } else { Mode::Normal };
        if self.mode == Mode::Normal {
            store.remember_good(&text);
        }
        log_info!("reloaded {}", path.display());
        self.previewing = false;
        let config = parsed.config;
        let previous = self.take_icons();
        self.settings = config.dock;
        self.apply_hotkey();
        self.items = config.items.into_iter().map(Item::new).collect();
        self.hovered = None;
        self.pressed = None;
        self.clicks.reset();
        self.missing.clear();
        self.relayout_keeping(previous);
        if self.window_visible {
            self.render(); // when hidden, the next reveal draws it
        }
        self.start_heal();
        Ok(())
    }

    /// "Restore last working version": keeps the broken file as a pinned backup, then puts
    /// last-good back and reloads it.
    fn restore_last_good(&mut self) -> std::result::Result<(), String> {
        let store = self.store.clone().ok_or("There's nothing to restore.")?;
        let good = store.last_good().ok_or("There's no earlier working version to restore.")?;
        if let Some(kept) = store.pin("broken") {
            log_info!("kept the broken dock.toml as {}", kept.display());
        }
        store.replace(&good)?;
        log_info!("restored the last working version of dock.toml");
        self.reload()
    }

    // ---- Editing ------------------------------------------------------------------------------

    /// Edits are saved to dock.toml straight away, so they need a file that's safe to write.
    fn editable(&self) -> std::result::Result<(Store, Removed), String> {
        match self.mode {
            Mode::Normal => {}
            Mode::ReadOnly => return Err("Your dock was saved by a newer Desktop Dock, so this one can't change it.".into()),
            Mode::SafeStart => return Err("Your dock's file can't be read right now, so it can't be changed. Fix the file, or right-click the dock > Restore last working version.".into()),
        }
        match (&self.store, &self.removed) {
            (Some(store), Some(removed)) => Ok((store.clone(), removed.clone())),
            _ => Err("This copy of the dock can't change your dock.".into()),
        }
    }

    /// Takes an item off the dock. Nothing is lost: Undo on the strip that appears puts it straight
    /// back, and afterwards it's in Recently removed (right-click → Put back).
    fn remove_item(&mut self, index: usize, expect: &str) -> std::result::Result<(), String> {
        let (store, removed) = self.editable()?;
        let neighbour = crate::breaks::broken("menu.remove-neighbour") && index + 1 < self.items.len();
        let index = if neighbour { index + 1 } else { index };
        let label = edit::label_of(&self.items.get(index).ok_or("That item isn't there any more.")?.cfg);
        if label != expect && !neighbour {
            return Err(format!("The dock changed while the menu was open ({expect} has moved), so nothing was done. Try again."));
        }
        let (table, change) = store.change(|doc| edit::remove(doc, Spot::dock(index), &label))?;
        let removed_id = removed.add(&table, index, None).inspect_err(|e| log_warn!("Recently removed: {e}")).ok();
        log_info!("removed {label}");
        self.show_change(UndoStep { what: format!("remove {label}"), change, removed_id, put_back: None }, format!("Removed {label}"))
    }

    /// Puts a Recently removed item back where it was.
    fn put_back(&mut self, id: &str) -> std::result::Result<(), String> {
        let (store, removed) = self.editable()?;
        let entry = removed.take(id).ok_or("It's no longer in Recently removed.")?;
        let result = store.change(|doc| {
            let spot = if crate::breaks::broken("undo.put-back-at-end") {
                edit::put_back_spot(doc, usize::MAX, None)
            } else {
                edit::put_back_spot(doc, entry.was_at, entry.was_in.as_deref())
            };
            edit::insert(doc, spot, entry.table.clone())
        });
        let ((), change) = result.inspect_err(|_| {
            let _ = removed.restore(&entry); // leave it in the list
        })?;
        log_info!("put back {}", entry.label);
        let label = entry.label.clone();
        self.show_change(UndoStep { what: format!("put back {label}"), change, removed_id: None, put_back: Some(entry) }, format!("Put {label} back"))
    }

    /// Undoes the latest change (the strip's Undo), unless dock.toml was changed by hand since:
    /// then nothing is touched.
    fn undo(&mut self) -> std::result::Result<(), String> {
        let (store, removed) = self.editable()?;
        let step = self.undo.take().ok_or("There's nothing to undo.")?;
        if !crate::breaks::broken("undo.no-op") {
            store.revert(&step.change)?;
        }
        if let Some(id) = &step.removed_id {
            removed.take(id);
        }
        if let Some(entry) = &step.put_back {
            let _ = removed.restore(entry).inspect_err(|e| log_warn!("Recently removed: {e}"));
        }
        log_info!("undid: {}", step.what);
        self.strip = None;
        self.reload_keeping_undo()
    }

    /// Moves an item along the dock into `gap` (counted among the other items).
    fn move_to(&mut self, from: usize, gap: usize, expect: &str) -> std::result::Result<(), String> {
        let (store, _) = self.editable()?;
        let label = edit::label_of(&self.items.get(from).ok_or("That item isn't there any more.")?.cfg);
        if label != expect {
            return Err(format!("The dock changed during the drag ({expect} has moved), so nothing was done. Try again."));
        }
        let to = if gap >= from { gap + 1 } else { gap }; // edit::move_item counts the item itself
        let ((), change) = store.change(|doc| edit::move_item(doc, Spot::dock(from), Spot::dock(to), &label))?;
        log_info!("moved {label}");
        self.show_change(UndoStep { what: format!("move {label}"), change, removed_id: None, put_back: None }, format!("Moved {label}"))
    }

    /// Lock icons: no dragging them around or off the dock, and no Remove in the menu.
    fn set_locked(&mut self, locked: bool) -> std::result::Result<(), String> {
        let (store, _) = self.editable()?;
        store.edit(|doc| edit::set_dock(doc, "locked", locked))?;
        log_info!("icons {}", if locked { "locked" } else { "unlocked" });
        self.reload_keeping_undo()
    }

    // ---- Dragging icons -------------------------------------------------------------------------

    /// The button went down on an icon and the pointer has since moved past Windows' own drag
    /// distance, so a click never turns into a move.
    fn drag_should_start(&self) -> bool {
        self.press.is_some_and(|(_, start)| self.moved_past_drag_distance(start))
    }

    fn moved_past_drag_distance(&self, start: POINT) -> bool {
        let Some(pt) = system::cursor_pos() else { return false };
        let (dx, dy) = unsafe { (GetSystemMetricsForDpi(SM_CXDRAG, self.dpi), GetSystemMetricsForDpi(SM_CYDRAG, self.dpi)) };
        (pt.x - start.x).abs() > dx || (pt.y - start.y).abs() > dy
    }

    fn start_drag(&mut self) {
        let Some((index, start)) = self.press.take() else { return };
        self.close_group_now("a drag started");
        let g = self.layout.geo;
        let (Some(slot), Some(item)) = (self.layout.slot_of(index), self.items.get(index)) else {
            self.end_press(); // the item went away (a reload): let go of the mouse
            return;
        };
        let (x, y, size) = layout::icon_rect(&g, slot, item.hover_t, 0.0);
        let left = self.win_x as f32 + x;
        let top = self.window_y() as f32 + self.content_offset() + y;
        self.drag = Some(Drag {
            index,
            label: edit::label_of(&item.cfg),
            grab: (start.x as f32 - left, start.y as f32 - top),
            size,
            gap: Some(index),
            image_off: None,
            off_since: None,
            child: None,
            into: None,
            in_popup: None,
            over: None,
            sprung: false,
        });
        self.hovered = None;
        self.pressed = None;
        system::forget_escape(); // only an Esc from now on cancels it
        self.render(); // its place shows empty
        self.update_drag();
    }

    /// Follows the pointer: the others make room where it would land, and the icon fades with a
    /// remove mark once it's well away from the dock.
    fn update_drag(&mut self) {
        if system::escape_pressed() {
            self.cancel_drag();
            return;
        }
        let Some(pt) = system::cursor_pos() else { return };
        let x = (pt.x - self.win_x) as f32;
        let y = (pt.y - self.window_y()) as f32 - self.content_offset();
        let Some((index, child)) = self.drag.as_ref().map(|drag| (drag.index, drag.child)) else { return };
        let now = Instant::now();
        // A whole group never goes inside a group, nor does a separator (a group holds things to open).
        let stays_out_of_groups = child.is_none() && self.items.get(index).is_some_and(|item| matches!(item.cfg.kind, Kind::Group | Kind::Separator));
        let is_group = |i: usize| self.items.get(i).is_some_and(|item| item.cfg.kind == Kind::Group);
        // Over an open group's pop-up: a place among its items.
        let in_popup = match &self.group {
            Some(open) if !stays_out_of_groups && open.layout.covers(pt.x, pt.y) => {
                Some((open.index, open.layout.gap_at((pt.x - open.layout.x) as f32, (pt.y - open.layout.y) as f32)))
            }
            _ => None,
        };
        let off = in_popup.is_none() && self.layout.drag_is_off(x, y);
        // On the dock: between items, or onto a group (its middle).
        let (gap, into) = if in_popup.is_some() || off {
            (None, None)
        } else {
            let zone = match child {
                None => self.layout.drag_zone(index, x, |i| !stays_out_of_groups && is_group(i)),
                Some(_) => match self.layout.onto_icon(x, y).filter(|&i| is_group(i)) {
                    Some(group) => DragZone::Onto(group),
                    None => DragZone::Gap(self.layout.insert_gap(x)),
                },
            };
            match zone {
                DragZone::Onto(group) => (None, Some(group)),
                DragZone::Gap(gap) => (Some(gap), None),
            }
        };
        // Held over a group a moment, it springs open to place the icon exactly; it closes again
        // if the drag moves on along the dock (not on the way down into it).
        let previous = self.drag.as_ref().map(|drag| (drag.into, drag.in_popup, drag.over, drag.sprung));
        let Some((was_into, was_in_popup, was_over, sprung)) = previous else { return };
        let over = match (into, was_over) {
            (Some(group), Some((was, since))) if was == group => Some((group, since)),
            (Some(group), _) => Some((group, now)),
            (None, _) => None,
        };
        let spring = over
            .filter(|&(group, since)| now - since >= SPRING_OPEN && !self.group.as_ref().is_some_and(|open| open.index == group))
            .filter(|&(group, _)| self.items.get(group).is_some_and(|item| !item.children.is_empty()))
            .map(|(group, _)| group);
        // (Not while the pointer is anywhere above the pop-up: on the way down into it, a slanting
        // path leaves the group's narrow middle while still at the dock's height.)
        let slack = self.layout.geo.icon;
        let above_popup = self.group.as_ref().is_some_and(|open| {
            pt.x as f32 >= open.layout.x as f32 - slack && (pt.x as f32) < (open.layout.x + open.layout.width) as f32 + slack
        });
        let leave_sprung = sprung && in_popup.is_none() && into.is_none() && y < self.layout.geo.panel_h && !above_popup;
        if let Some(drag) = &mut self.drag {
            drag.off_since = if off { drag.off_since.or(Some(now)) } else { None };
            drag.gap = gap;
            drag.into = into;
            drag.in_popup = in_popup;
            drag.over = over;
        }
        if leave_sprung {
            self.close_group("the drag moved on");
            if let Some(drag) = &mut self.drag {
                drag.sprung = false;
            }
        }
        if let Some(group) = spring {
            self.open_group(group);
            if let Some(drag) = &mut self.drag {
                drag.sprung = self.group.is_some();
            }
            if let Some(window) = self.drag_window {
                unsafe {
                    // The dragged icon stays on top of the pop-up that just opened.
                    let _ = SetWindowPos(window, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
        }
        if in_popup != was_in_popup {
            self.render_popup();
        }
        if into != was_into && self.window_visible {
            self.render();
        }
        // A whole group goes only after it's been held off the dock for a moment.
        let whole_group = child.is_none() && self.items.get(index).is_some_and(|item| item.cfg.kind == Kind::Group);
        let held = self.drag.as_ref().and_then(|drag| drag.off_since).is_some_and(|since| now - since >= GROUP_HOLD);
        let removable = off && (!whole_group || held);
        let redraw = self.drag.as_ref().is_some_and(|drag| drag.image_off != Some(removable));
        if redraw {
            self.draw_drag_image(removable);
        }
        if let (Some(drag), Some(window)) = (&self.drag, self.drag_window) {
            let pad = drag_image_pad(drag.size);
            let (left, top) = (pt.x - (drag.grab.0 + pad) as i32, pt.y - (drag.grab.1 + pad) as i32);
            let _ = self.renderer.present_drag_image(window, left, top);
            if redraw && drag.image_off.is_some() {
                // On top of everything, the dock included: the dock puts itself on top each time
                // it appears, and a window that's merely shown would sit underneath it.
                unsafe {
                    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
                    let _ = SetWindowPos(window, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
                }
            }
        }
    }

    /// The icon under the pointer: as it looks on the dock, or faded with a remove mark.
    fn draw_drag_image(&mut self, off: bool) {
        let Some(drag) = &self.drag else { return };
        let g = self.layout.geo;
        let (size, index) = (drag.size, drag.index);
        let pad = drag_image_pad(size);
        let side = (size + 2.0 * pad).ceil() as i32;
        if self.renderer.begin_drag_image(side).is_err() {
            return;
        }
        let rt = self.renderer.rt.clone();
        let brush = &self.brush;
        let item = &self.items[index];
        let child = drag.child.and_then(|k| item.children.get(k));
        let bitmap = match child {
            Some(child) => child.icon.as_ref(),
            None if size > g.icon + 0.5 => item.zoom.as_ref().or(item.base.as_ref()),
            None => item.base.as_ref(),
        };
        let dest = rect(pad, pad, pad + size, pad + size);
        unsafe {
            match bitmap {
                _ if child.is_none() && item.cfg.is_separator() => {
                    // A separator: a short upright bar, easy to see over anything.
                    let (cx, half_w, half_h) = (pad + size / 2.0, (1.5 * g.scale).max(1.5), size * 0.42);
                    let bar = rounded(cx - half_w, pad + size / 2.0 - half_h, cx + half_w, pad + size / 2.0 + half_h, half_w);
                    brush.SetColor(&rgba(0.0, 0.0, 0.0, if off { 0.25 } else { 0.45 }));
                    rt.FillRoundedRectangle(&rounded(bar.rect.left - 1.0, bar.rect.top - 1.0, bar.rect.right + 1.0, bar.rect.bottom + 1.0, half_w + 1.0), brush);
                    brush.SetColor(&rgba(1.0, 1.0, 1.0, if off { 0.45 } else { 0.95 }));
                    rt.FillRoundedRectangle(&bar, brush);
                }
                _ if child.is_none() && item.shows_preview() => draw_group_tile(&rt, brush, item, dest, if off { 0.45 } else { 0.92 }),
                Some(bitmap) => {
                    let opacity = if off { 0.45 } else { 0.92 };
                    rt.DrawBitmap(bitmap, Some(&dest), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None);
                }
                None => {
                    brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.35));
                    rt.FillRoundedRectangle(&rounded(pad, pad, pad + size, pad + size, size * 0.2), brush);
                }
            }
            if off {
                // A red badge with a white cross at the top right: let go to remove.
                let (cx, cy, r) = (pad + size - pad * 0.2, pad + pad * 0.2, pad * 0.95);
                brush.SetColor(&rgba(0.84, 0.22, 0.2, 1.0));
                rt.FillEllipse(&D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: r, radiusY: r }, brush);
                brush.SetColor(&rgba(1.0, 1.0, 1.0, 1.0));
                let arm = r * 0.42;
                let width = (r * 0.26).max(1.5);
                rt.DrawLine(Vector2 { X: cx - arm, Y: cy - arm }, Vector2 { X: cx + arm, Y: cy + arm }, brush, width, None);
                rt.DrawLine(Vector2 { X: cx - arm, Y: cy + arm }, Vector2 { X: cx + arm, Y: cy - arm }, brush, width, None);
            }
        }
        if self.renderer.end().is_ok() {
            if let Some(drag) = &mut self.drag {
                drag.image_off = Some(off);
            }
        }
    }

    /// Let go: off the dock removes it, anywhere along the dock moves it there.
    fn finish_drag(&mut self) -> Option<Action> {
        // Esc and the release in the same moment, between two of the dock's looks: the Esc
        // wins, as it would have a moment earlier.
        if self.drag.is_some() && system::escape_pressed() {
            self.cancel_drag();
            return None;
        }
        let drag = self.drag.take()?;
        self.end_drag_visuals();
        let result = match (drag.child, drag.in_popup, drag.into) {
            // Into or within a group, at the place shown in its pop-up (which then stays a moment,
            // showing it there, before it fades); or onto a group: at its end.
            (child, Some((group, place)), _) => {
                let done = self.drop_in_group(drag.index, child, group, Some(place), &drag.label);
                if done.is_ok() {
                    self.open_group(group);
                    self.close_group_after("dropped into it", SETTLE_HOLD);
                }
                done
            }
            (child, None, Some(group)) => self.drop_in_group(drag.index, child, group, None, &drag.label),
            // Out of a group: onto the dock, or (well away) removed.
            (Some(child), None, None) => match drag.gap {
                Some(gap) => self.take_out_to(drag.index, child, gap, &drag.label),
                None if drag.image_off == Some(true) => {
                    let group_label = self.items.get(drag.index).map(|item| edit::label_of(&item.cfg)).unwrap_or_default();
                    self.remove_child(drag.index, child, &drag.label, &group_label)
                }
                None => Ok(()),
            },
            (None, None, None) => self.finish_dock_drag(&drag),
        };
        self.close_group("the drag ended");
        result.err().map(Action::Error)
    }

    /// A dock icon let go along the dock (moved) or well away (removed).
    fn finish_dock_drag(&mut self, drag: &Drag) -> std::result::Result<(), String> {
        match drag.gap {
            None if drag.image_off != Some(true) => {
                // A group let go before it was held off long enough: nothing happens.
                log_info!("{} let go off the dock too soon to remove it", drag.label);
                self.show_notice(&format!("To remove {}, hold it off the dock for a moment", drag.label));
                Ok(())
            }
            None => self.remove_item(drag.index, &drag.label),
            Some(gap) if gap == drag.index => {
                log_info!("dropped {} back in its own place", drag.label);
                Ok(())
            }
            Some(gap) => self.move_to(drag.index, gap, &drag.label),
        }
    }

    /// Let go into a group: the dock icon or group item `child` of `from` goes into `group`, at
    /// `place` (counted the way its pop-up showed it) or at its end.
    fn drop_in_group(&mut self, from: usize, child: Option<usize>, group: usize, place: Option<usize>, expect: &str) -> std::result::Result<(), String> {
        let group_label = self.items.get(group).map(|item| edit::label_of(&item.cfg)).ok_or("That group isn't there any more.")?;
        let end = self.items.get(group).map_or(0, |item| item.children.len());
        let source = match child {
            Some(k) => Spot::in_group(from, k),
            None => Spot::dock(from),
        };
        let place = place.unwrap_or(end);
        if child.is_some() && from == group {
            let k = child.unwrap_or(0);
            if place == k || place == k + 1 {
                log_info!("dropped {expect} back in its own place");
                return Ok(());
            }
            return self.move_spot(source, Spot::in_group(group, place), expect, format!("Moved {expect}"));
        }
        self.move_spot(source, Spot::in_group(group, place), expect, format!("Moved {expect} to {group_label}"))
    }

    /// Out of a group onto the dock, at `gap` (counted among everything on the dock).
    fn take_out_to(&mut self, group: usize, child: usize, gap: usize, expect: &str) -> std::result::Result<(), String> {
        let group_label = self.items.get(group).map(|item| edit::label_of(&item.cfg)).unwrap_or_default();
        self.move_spot(Spot::in_group(group, child), Spot::dock(gap), expect, format!("Took {expect} out of {group_label}"))
    }

    /// One move, saved, with the Undo strip saying `done`.
    fn move_spot(&mut self, from: Spot, to: Spot, expect: &str, done: String) -> std::result::Result<(), String> {
        let (store, _) = self.editable()?;
        let ((), change) = store.change(|doc| edit::move_item(doc, from, to, expect))?;
        log_info!("{}", done.to_lowercase());
        let step = UndoStep { what: done.to_lowercase(), change, removed_id: None, put_back: None };
        self.show_change(step, done)
    }

    /// Esc, a right-click, or Windows taking the mouse away: everything goes back.
    fn cancel_drag(&mut self) {
        if let Some(drag) = self.drag.take() {
            // (a sprung or source pop-up closes below, fading)
            log_info!("drag cancelled");
            self.end_drag_visuals();
            if drag.sprung || drag.child.is_some() {
                self.close_group("the drag was cancelled");
            }
        }
    }

    /// The button went down on an item in a group's pop-up and the pointer has moved: it's
    /// picked up (the pop-up stays, with a trace where it was).
    fn start_child_drag(&mut self) {
        let Some((group, child, start)) = self.child_press.take() else { return };
        let Some(open) = &self.group else { return };
        let (Some(cell), Some(item)) = (open.layout.cells.get(child), self.items.get(group).and_then(|item| item.children.get(child))) else {
            return;
        };
        if open.index != group || item.cfg.is_separator() {
            return;
        }
        let left = open.layout.x as f32 + cell.icon.left;
        let top = open.layout.y as f32 + cell.icon.top;
        self.drag = Some(Drag {
            index: group,
            child: Some(child),
            label: edit::label_of(&item.cfg),
            grab: (start.x as f32 - left, start.y as f32 - top),
            size: cell.icon.width(),
            gap: None,
            image_off: None,
            off_since: None,
            into: None,
            in_popup: Some((group, child)),
            over: None,
            sprung: false,
        });
        if let Some(open) = &mut self.group {
            open.hovered = None;
            open.pressed = None;
        }
        log_info!("dragging {} out of a group", edit::label_of(&item.cfg));
        system::forget_escape(); // only an Esc from now on cancels it
        self.render_popup();
        self.update_drag();
    }

    fn end_drag_visuals(&mut self) {
        self.press = None;
        self.child_press = None;
        unsafe {
            let _ = ReleaseCapture();
            if let Some(window) = self.drag_window {
                let _ = ShowWindow(window, SW_HIDE);
            }
        }
        self.renderer.release_drag_image();
        if self.window_visible {
            self.render(); // its place fills again; the others glide back
        }
    }

    // ---- Dropping things in from outside ------------------------------------------------------

    /// Files, apps or links dragged in arrive. From the drop zone, the dock comes out for them.
    /// Lock icons turns this off too: a locked dock doesn't change.
    fn drop_enter(&mut self, pt: POINT, from_zone: bool) -> bool {
        if self.settings.locked {
            // Never silent: the dock comes out and says why it won't take the drop.
            log_info!("icons are locked: the drag is turned away (and told why)");
            self.show();
            self.show_notice(LOCKED_NOTICE);
            return false;
        }
        if self.editable().is_err() {
            return false;
        }
        if from_zone {
            // The zone stays until the drag is over (the dock comes out on top of it): taking it away
            // could leave nothing under the pointer for a moment, and a drop there would land on
            // whatever is behind (on the desktop, Explorer would move the file).
            self.show();
        }
        self.incoming = Some(Incoming { gap: None, onto: None, label: Vec::new() });
        self.drop_over(pt)
    }

    /// Follows them over the dock: over the middle of a program or the Recycle Bin they'd go
    /// onto it (it grows and says what would happen); anywhere else a gap opens where they'd go.
    fn drop_over(&mut self, pt: POINT) -> bool {
        let Some(previous) = self.incoming.as_ref().map(|incoming| incoming.onto) else { return false };
        // Measured against where the dock is when fully out, so this works while it slides in.
        let x = (pt.x - self.win_x) as f32;
        let y = (pt.y - self.monitor.top) as f32;
        let onto = self.layout.onto_icon(x, y).filter(|&index| self.takes_drops(index));
        let gap = if onto.is_some() || self.layout.drag_is_off(x, y) { None } else { Some(self.layout.insert_gap(x)) };
        let label = match onto {
            Some(index) if self.items[index].cfg.kind == Kind::RecycleBin => "Move to Recycle Bin".to_string(),
            Some(index) if self.items[index].cfg.kind == Kind::Group => format!("Add to {}", edit::label_of(&self.items[index].cfg)),
            Some(index) => format!("Open with {}", edit::label_of(&self.items[index].cfg)),
            None => String::new(),
        };
        if onto != previous {
            if let Some(index) = onto {
                self.request_zoom(index);
            }
        }
        let was_over = self.incoming.as_ref().is_some_and(|incoming| incoming.gap.is_some() || incoming.onto.is_some());
        if onto != previous || (gap.is_some() || onto.is_some()) != was_over {
            // A line each time a drag changes between onto an icon, between icons and off the dock.
            let place = match (onto, gap) {
                (Some(index), _) => format!("onto {}", edit::label_of(&self.items[index].cfg)),
                (None, Some(_)) => "between icons".to_string(),
                (None, None) => format!("off the dock (at {x:.0},{y:.0} in the dock)"),
            };
            log_info!("drag over: {place}");
        }
        self.incoming = Some(Incoming { gap, onto, label: label.encode_utf16().collect() });
        gap.is_some() || onto.is_some()
    }

    /// Items that take things dropped onto them: ordinary programs (to open files) and the
    /// Recycle Bin. Folders, documents, links and Store apps only take drops beside them.
    fn takes_drops(&self, index: usize) -> bool {
        let Some(item) = self.items.get(index) else { return false };
        match item.cfg.kind {
            Kind::RecycleBin | Kind::Group => true,
            Kind::App => drop::opens_files(&self.home.resolve(item.cfg.launch_target())),
            _ => false,
        }
    }

    fn drop_leave(&mut self) {
        self.incoming = None; // the gap closes; the dock hides as usual once the pointer is away
    }

    /// Let go over the dock. The work (saving, opening, recycling, asking) happens just after
    /// Windows' drop call returns, so Explorer never waits on the dock.
    fn drop_items(&mut self, dropped: drop::Dropped) -> std::result::Result<bool, String> {
        let Some(incoming) = self.incoming.take() else { return Ok(false) };
        self.pending_drop = match (incoming.onto, incoming.gap) {
            (Some(index), _) => {
                let label = self.items.get(index).map(|item| edit::label_of(&item.cfg)).unwrap_or_default();
                Some(PendingDrop::Onto(index, label, dropped))
            }
            (None, Some(gap)) if !dropped.items.is_empty() => Some(PendingDrop::Add(gap, dropped.items)),
            (None, Some(_)) => return Err("The dock can't add what was dropped.".into()),
            (None, None) => return Ok(false),
        };
        if let Some(hwnd) = self.hwnd {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_APP_DROPPED, WPARAM(0), LPARAM(0));
            }
        }
        Ok(true)
    }

    /// Something dropped onto an item: the Recycle Bin recycles it; a program opens the files,
    /// or, for a single program dropped on it, offers to be replaced by it (asks first).
    fn drop_onto(&mut self, index: usize, expect: &str, dropped: drop::Dropped) -> Option<Action> {
        let item = self.items.get(index).filter(|item| edit::label_of(&item.cfg) == expect);
        let Some(item) = item else {
            return Some(Action::Error(format!("The dock changed during the drop ({expect} has moved), so nothing was done. Try again.")));
        };
        if item.cfg.kind == Kind::RecycleBin {
            self.recycle(dropped.paths);
            return None;
        }
        if item.cfg.kind == Kind::Group {
            return self.add_into_group(index, expect, &dropped.items).err().map(Action::Error);
        }
        if let [program] = dropped.items.as_slice() {
            if drop::is_program(&program.target) {
                return Some(Action::ConfirmReplace(index, expect.to_string(), program.clone()));
            }
        }
        if dropped.paths.is_empty() {
            return Some(Action::Error(format!("Only files and folders can be opened with {expect}.")));
        }
        let quoted: Vec<String> = dropped.paths.iter().map(|path| format!("\"{path}\"")).collect();
        let mut with_files = item.cfg.clone();
        if !crate::breaks::broken("drop.open-with-no-file") {
            with_files.args = format!("{} {}", with_files.args, quoted.join(" ")).trim().to_string();
        }
        log_info!("opening {} file(s) with {expect}", dropped.paths.len());
        self.launch_effect(index);
        Some(Action::Launch(with_files, index, self.generation))
    }

    /// Files dropped on the Recycle Bin go there the way Explorer's Delete does (they can be
    /// restored; Windows asks first if you've set it to), on a worker thread.
    fn recycle(&self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        let Some(hwnd) = self.hwnd else { return };
        let hwnd_value = hwnd.0 as isize;
        log_info!("recycling {} dropped item(s)", paths.len());
        // Done by the helper process (`--recycle`), like icons: Windows' file-operation machinery
        // stays out of the dock. It may ask questions, so there's no time limit; the dock doesn't wait.
        std::thread::spawn(move || {
            // Its own list for each drop, so two quick drops never share one.
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let list = std::env::temp_dir().join(format!("desktop-dock-recycle-{}-{n}.txt", std::process::id()));
            let ran = std::fs::write(&list, paths.join("\n"))
                .map_err(|e| e.to_string())
                .and_then(|()| std::env::current_exe().map_err(|e| e.to_string()))
                .and_then(|exe| std::process::Command::new(exe).arg("--recycle").arg(&list).status().map_err(|e| e.to_string()));
            match ran {
                Ok(status) if status.success() => {}
                Ok(status) => log_warn!("recycling ended with code {:?}", status.code()),
                Err(e) => log_warn!("couldn't recycle: {e}"),
            }
            let _ = std::fs::remove_file(&list);
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd_value as *mut std::ffi::c_void)), WM_APP_RECHECK_BIN, WPARAM(0), LPARAM(0));
            }
        });
    }

    // ---- Choosers (Icon, Add) ------------------------------------------------------------------

    /// Opens one of Windows' choosers in a helper process (see pickers.rs) and carries on; the
    /// answer arrives as WM_APP_PICKED. One at a time.
    fn start_pick(&mut self, pick: Pick, start: String, purpose: Purpose) -> std::result::Result<(), String> {
        self.editable()?;
        if self.picking {
            return Err("A file window is already open.".into());
        }
        let Some(hwnd) = self.hwnd else { return Ok(()) };
        self.picking = true;
        let slot = self.picked.clone();
        let hwnd_value = hwnd.0 as isize;
        unsafe {
            // The dock just had your click; let the chooser come to the front.
            let _ = AllowSetForegroundWindow(ASFW_ANY);
        }
        std::thread::spawn(move || {
            let chosen = pickers::ask(pick, &start);
            if let Ok(mut slot) = slot.lock() {
                *slot = Some((purpose, chosen));
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd_value as *mut std::ffi::c_void)), WM_APP_PICKED, WPARAM(0), LPARAM(0));
            }
        });
        Ok(())
    }

    /// A chooser was answered (or closed): make the change, show the dock with its Undo strip.
    fn apply_pick(&mut self) -> std::result::Result<(), String> {
        self.picking = false;
        let Some((purpose, chosen)) = self.picked.lock().ok().and_then(|mut slot| slot.take()) else { return Ok(()) };
        if chosen.is_empty() {
            return Ok(()); // cancelled
        }
        let result = match purpose {
            Purpose::Add(gap) => {
                let items: Vec<ItemConfig> = chosen.iter().map(|path| drop::item_for_path(path, &self.home)).collect();
                self.add_items(gap.min(self.items.len()), &items)
            }
        };
        if result.is_ok() {
            self.show(); // so you see the change, and Undo
        }
        result
    }

    /// After you've said yes: points the item at the dropped program. Its place, name, icon and
    /// settings stay; Undo is on the strip.
    fn replace_program(&mut self, index: usize, expect: &str, new: &ItemConfig) -> std::result::Result<(), String> {
        let (store, _) = self.editable()?;
        let kept = (crate::breaks::broken("drop.replace-keeps-target")).then(|| self.items.get(index).map(|item| item.cfg.clone())).flatten();
        let new = kept.as_ref().unwrap_or(new);
        let ((), change) = store.change(|doc| edit::replace_program(doc, Spot::dock(index), expect, new))?;
        log_info!("replaced {expect}'s program with {} ({})", edit::label_of(new), new.target);
        self.show_change(UndoStep { what: format!("replace {expect}"), change, removed_id: None, put_back: None }, format!("Replaced {expect}"))
    }
    /// Things dropped onto a group go in at its end.
    fn add_into_group(&mut self, group: usize, expect: &str, items: &[ItemConfig]) -> std::result::Result<(), String> {
        if items.is_empty() {
            return Err(format!("None of that can go into {expect}."));
        }
        let (store, _) = self.editable()?;
        let end = self.items.get(group).map_or(0, |item| item.children.len());
        let ((), change) = store.change(|doc| {
            for (k, item) in items.iter().enumerate() {
                edit::insert(doc, Spot::in_group(group, end + k), edit::item_table(item))?;
            }
            Ok(())
        })?;
        let label = match items {
            [one] => edit::label_of(one),
            many => format!("{} items", many.len()),
        };
        log_info!("added {label} to {expect}");
        self.show_change(UndoStep { what: format!("add {label} to {expect}"), change, removed_id: None, put_back: None }, format!("Added {label} to {expect}"))
    }

    fn add_items(&mut self, gap: usize, items: &[ItemConfig]) -> std::result::Result<(), String> {
        let (store, _) = self.editable()?;
        let ((), change) = store.change(|doc| {
            for (k, item) in items.iter().enumerate() {
                edit::insert(doc, Spot::dock(gap + k), edit::item_table(item))?;
            }
            Ok(())
        })?;
        let label = match items {
            [one] => edit::label_of(one),
            many => format!("{} items", many.len()),
        };
        for item in items {
            log_info!("added {} ({})", edit::label_of(item), item.target);
        }
        self.show_change(UndoStep { what: format!("add {label}"), change, removed_id: None, put_back: None }, format!("Added {label}"))
    }

    /// Shows or hides the drop zone along the top edge, over the dock's width.
    fn set_drop_zone(&mut self, show: bool) {
        if show == self.drop_zone_shown {
            return;
        }
        let Some(zone) = self.drop_zone else { return };
        self.drop_zone_shown = show;
        self.zone_since = show.then(Instant::now);
        self.zone_nudged = false;
        unsafe {
            if show {
                let g = self.layout.geo;
                let left = self.win_x + g.panel_left as i32;
                let height = (3.0 * g.scale).round() as i32;
                let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW;
                let _ = SetWindowPos(zone, Some(HWND_TOPMOST), left, self.monitor.top, g.panel_w as i32, height, flags);
            } else {
                let _ = ShowWindow(zone, SW_HIDE);
            }
        }
    }

    /// The button came up without a drag: let go of the mouse.
    fn end_press(&mut self) {
        self.press = None;
        self.sync_capture();
    }

    /// The one rule for the mouse: the dock holds it only while a press or a drag of one of its
    /// icons is in progress. Checked after every mouse message and on every poll, so the dock can
    /// never keep the mouse by mistake (every click elsewhere would go to the dock).
    fn sync_capture(&self) {
        if self.press.is_some() || self.child_press.is_some() || self.drag.is_some() {
            return;
        }
        unsafe {
            let held = GetCapture();
            if !held.is_invalid() && (Some(held) == self.hwnd || Some(held) == self.popup_window) {
                let _ = ReleaseCapture();
            }
        }
    }

    /// Where `item` should sit sideways right now (making room for a dragged icon).
    fn shift_target(&self, item: usize) -> f32 {
        if let Some(drag) = &self.drag {
            return match drag.child {
                // Out of a group: room opens where it would go, like something dragged in.
                Some(_) => drag.gap.map_or(0.0, |gap| self.layout.insert_shift(gap, item)),
                // Into a group: its own place stays open; nothing else moves.
                None if drag.into.is_some() || drag.in_popup.is_some() => self.layout.drag_shift(drag.index, None, item),
                None => self.layout.drag_shift(drag.index, drag.gap, item),
            };
        }
        match self.incoming.as_ref().and_then(|incoming| incoming.gap) {
            Some(gap) => self.layout.insert_shift(gap, item),
            None => 0.0,
        }
    }

    /// After a change: show it straight away (no waiting for the file watch), and put up the strip
    /// with its Undo.
    fn show_change(&mut self, step: UndoStep, strip: String) -> std::result::Result<(), String> {
        self.reload_keeping_undo()?;
        self.undo = Some(step);
        self.show_strip(&strip);
        Ok(())
    }

    fn reload_keeping_undo(&mut self) -> std::result::Result<(), String> {
        let undo = self.undo.take();
        let result = self.reload();
        self.undo = undo;
        result
    }

    /// "Removed X · Undo" under the panel for a few seconds; the dock stays out meanwhile.
    fn show_strip(&mut self, message: &str) {
        self.put_strip(message, true);
    }

    /// A plain notice in the same place, without Undo ("Icons are locked").
    fn show_notice(&mut self, message: &str) {
        self.put_strip(message, false);
    }

    /// The very first start (a new dock was just made, by setup or a new portable copy): the
    /// dock comes out for a few seconds with a line saying how to bring it out, so someone new
    /// sees where it lives. Not over a fullscreen app, nor with nobody there.
    fn welcome(&mut self) {
        self.peek();
        if self.reveal_target < 0.5 {
            return;
        }
        self.show_notice("Rest the pointer at the top of the screen to bring your dock out.");
        self.linger_until = Some(Instant::now() + WELCOME_FOR);
        self.start_timer(TIMER_STRIP, WELCOME_FOR.as_millis() as u32);
        log_info!("first start: the dock came out to show where it lives");
    }

    fn put_strip(&mut self, message: &str, undoable: bool) {
        log_info!("strip: {message}{}", if undoable { " · Undo" } else { "" });
        let g = self.layout.geo;
        let text: Vec<u16> = message.encode_utf16().collect();
        let undo: Vec<u16> = "Undo".encode_utf16().collect();
        let text_w = self.text_width(&text);
        let undo_w = if undoable { self.text_width(&undo) } else { 0.0 };
        let (pad, gap) = (12.0 * g.scale, if undoable { 14.0 * g.scale } else { 0.0 });
        let width = (pad + text_w + gap + undo_w + pad).round();
        let left = (g.panel_left + (g.panel_w - width) / 2.0).round();
        let (top, bottom) = (g.strip_top, g.strip_top + g.strip_h);
        let text_left = left + pad;
        let undo_left = text_left + text_w + gap;
        self.strip = Some(Strip {
            text,
            rect: rect(left, top, left + width, bottom),
            text_rect: rect(text_left, top, text_left + text_w, bottom),
            undo_rect: undoable.then(|| rect(undo_left, top, undo_left + undo_w, bottom)),
        });
        let now = Instant::now();
        self.linger_until = Some(self.linger_until.map_or(now + STRIP_FOR, |until| until.max(now + STRIP_FOR)));
        self.start_timer(TIMER_STRIP, STRIP_FOR.as_millis() as u32);
        if self.window_visible {
            self.render();
        }
    }

    /// How wide a line of label text is, in pixels.
    fn text_width(&self, text: &[u16]) -> f32 {
        match &self.text_format {
            Some(format) => self.text_width_in(text, format),
            None => 0.0,
        }
    }

    /// How tall text is when it wraps at `width`, in pixels.
    fn text_height_in(&self, text: &[u16], format: &IDWriteTextFormat, width: f32) -> f32 {
        unsafe {
            let Ok(layout) = self.renderer.dwrite.CreateTextLayout(text, format, width.max(1.0), 4000.0) else { return 0.0 };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_err() {
                return 0.0;
            }
            metrics.height.ceil()
        }
    }

    fn text_width_in(&self, text: &[u16], format: &IDWriteTextFormat) -> f32 {
        unsafe {
            let Ok(layout) = self.renderer.dwrite.CreateTextLayout(text, format, 4000.0, 100.0) else { return 0.0 };
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut metrics).is_err() {
                return 0.0;
            }
            metrics.widthIncludingTrailingWhitespace.ceil()
        }
    }

    /// The regular check, from the poll timer: every second, or every 10 s as a safety net when
    /// Windows tells the dock about changes in its folder.
    fn check_config(&mut self, now: Instant) -> Option<String> {
        let every = if self.folder_watch.is_some() { CONFIG_CHECK_WATCHED } else { CONFIG_CHECK };
        if self.last_config_check.is_some_and(|last| now - last < every) {
            return None;
        }
        self.last_config_check = Some(now);
        self.check_config_now()
    }

    /// Reloads dock.toml once a new save has stayed unchanged for a moment (an editor may still
    /// be writing it at first sight). Returns an error message for the caller to show.
    fn check_config_now(&mut self) -> Option<String> {
        let store = self.store.clone()?;
        let Some(stamp) = modified(store.config_path()) else {
            self.config_missing(&store);
            return None;
        };
        self.missing_since = None;
        if self.watch.observe(stamp) {
            // Changed outside the dock (a hand edit): the last change can no longer be undone.
            self.undo = None;
            self.strip = None;
            return self.reload().err();
        }
        if self.watch.is_pending() {
            self.start_timer(TIMER_CONFIG, CONFIG_SETTLE_MS); // take the second look soon
        }
        None
    }

    /// dock.toml is gone (deleted, or lost by a sync app). Once it has stayed gone for a moment
    /// (an editor may save by deleting and renaming), the last working version is put back; the
    /// next check then reads it.
    fn config_missing(&mut self, store: &Store) {
        if !matches!(store.config_path().try_exists(), Ok(false)) || self.mode == Mode::ReadOnly {
            return;
        }
        let now = Instant::now();
        match self.missing_since {
            Some(since) if now - since >= Duration::from_millis(CONFIG_SETTLE_MS.into()) => {
                self.missing_since = None;
                if store.restore_missing().is_some() {
                    self.start_timer(TIMER_CONFIG, CONFIG_SETTLE_MS);
                }
            }
            Some(_) => self.start_timer(TIMER_CONFIG, CONFIG_SETTLE_MS),
            None => {
                self.missing_since = Some(now);
                self.start_timer(TIMER_CONFIG, CONFIG_SETTLE_MS);
            }
        }
    }

    fn start_timer(&self, id: usize, ms: u32) {
        if let Some(hwnd) = self.hwnd {
            unsafe {
                SetTimer(Some(hwnd), id, ms, None); // the same id restarts it: a natural debounce
            }
        }
    }

    fn stop_timer(&self, id: usize) {
        if let Some(hwnd) = self.hwnd {
            unsafe {
                let _ = KillTimer(Some(hwnd), id);
            }
        }
    }

    // ---- Keeping up with Windows ------------------------------------------------------------

    /// Brings the dock up to date with the screen and Windows' settings: runs after display,
    /// DPI or settings changes, an Explorer restart, waking or unlocking, and whenever the dock
    /// appears. Safe to run any number of times; does nothing when nothing changed.
    fn recheck(&mut self) {
        self.reduced_motion = !system::animations_enabled();
        let screen = system::dock_screen(&self.settings.monitor);
        if screen.id != self.screen.id || screen.monitor != self.screen.monitor || screen.work != self.screen.work || screen.dpi != self.dpi {
            log_info!(
                "screen changed to {}x{} at {} dpi; laying the dock out again",
                screen.monitor.right - screen.monitor.left,
                screen.monitor.bottom - screen.monitor.top,
                screen.dpi
            );
            self.relayout();
            if self.window_visible {
                self.render();
            }
        }
        // After Explorer restarts or the display changes, make sure the dock is still on top.
        if let (true, Some(hwnd)) = (self.window_visible, self.hwnd) {
            unsafe {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    fn schedule_recheck(&self) {
        self.start_timer(TIMER_RECHECK, RECHECK_DELAY_MS);
    }

    /// Stops all polling while nobody can see the dock (locked, switched away, display off),
    /// and picks up again when they're back.
    fn apply_presence(&mut self) {
        if self.presence.away() {
            if self.poll_ms != 0 {
                log_info!("away (locked, switched user or display off): polling paused");
                self.stop_timer(TIMER_POLL);
                self.poll_ms = 0;
                self.away_input_tick = system::last_input_tick();
                self.start_timer(TIMER_PRESENCE, PRESENCE_CHECK_MS);
            }
            self.hide_now();
        } else if self.poll_ms == 0 {
            log_info!("back: polling resumed");
            self.stop_timer(TIMER_PRESENCE);
            self.set_poll(100);
            self.schedule_recheck();
        }
    }

    /// Looks directly at whether someone's there, in case a lock, unlock or display message
    /// was missed (after waking, and once a minute while away).
    fn check_presence(&mut self) {
        if !self.presence.away() {
            return;
        }
        self.presence.correct(system::session_state(), system::last_input_tick() != self.away_input_tick);
        if !self.presence.away() {
            log_info!("someone is back, though Windows didn't say so");
        }
        self.apply_presence();
    }

    // ---- Showing and hiding -------------------------------------------------------------

    fn show(&mut self) {
        self.edge.reset();
        self.leave.reset();
        if !self.window_visible {
            self.recheck(); // in case the screen changed while the dock was hidden
            self.render(); // draw the hidden position first so nothing stale flashes
            if let Some(hwnd) = self.hwnd {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
            self.window_visible = true;
        }
        self.reveal_target = 1.0;
        self.check_recycle_bin();
        self.recheck_missing(false);
    }

    fn hide(&mut self) {
        self.close_group("the dock hid");
        self.reveal_target = 0.0;
        self.hovered = None;
        self.pressed = None;
        self.leave.reset();
        self.linger_until = None;
        if self.previewing {
            // A preview that was never saved (the slider went back, or the window closed):
            // back to what dock.toml says.
            if let Err(e) = self.reload() {
                log_warn!("couldn't go back from a preview: {e}");
            }
        }
    }

    /// Comes out and stays out for a moment, to show a change made in the settings window.
    /// Not over a fullscreen app or game, and not while nobody's there.
    /// Registers the keyboard shortcut from the settings (and lets go of an old one). If another
    /// program already has it, the log says so and the dock carries on without one.
    fn apply_hotkey(&mut self) {
        let Some(hwnd) = self.hwnd else { return };
        let wanted = hotkey::parse(&self.settings.hotkey).ok().flatten();
        if wanted == self.hotkey {
            return;
        }
        unsafe {
            if self.hotkey.take().is_some() {
                let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
            }
            if let Some(key) = wanted.filter(|_| !crate::breaks::broken("hotkey.not-registered")) {
                let modifiers = HOT_KEY_MODIFIERS(key.modifiers | hotkey::MOD_NOREPEAT);
                match RegisterHotKey(Some(hwnd), HOTKEY_ID, modifiers, key.key) {
                    Ok(()) => {
                        log_info!("shortcut {} brings the dock out", hotkey::format(key));
                        self.hotkey = Some(key);
                    }
                    Err(_) => log_warn!("another program already uses {}; no shortcut", hotkey::format(key)),
                }
            }
        }
    }

    /// The keyboard shortcut: out if the dock is hidden (for a few seconds, or until the pointer
    /// has been over it and left; Esc sends it back), back up if it's out. Not over a
    /// fullscreen app or game.
    fn summon(&mut self) {
        self.check_presence(); // the key press itself means someone's there
        if self.presence.away() || system::fullscreen_active(self.screen.monitor) {
            return;
        }
        if self.reveal_target >= 0.5 {
            self.hide();
            return;
        }
        // Windows keeps "Esc was pressed" until someone asks, and the dock doesn't while it's up
        // out of sight: an Esc pressed in another program before would send it straight back.
        system::forget_escape();
        self.linger_until = Some(Instant::now() + SUMMON_FOR);
        self.show();
        self.set_poll(16); // Esc sends it back: see the end of `tick`
    }

    fn peek(&mut self) {
        if self.presence.away() || system::fullscreen_active(self.screen.monitor) {
            return;
        }
        let now = Instant::now();
        if self.linger_until.is_none() {
            system::forget_escape(); // only an Esc from now on sends it back (see `summon`)
        }
        self.linger_until = Some(self.linger_until.map_or(now + PEEK_FOR, |until| until.max(now + PEEK_FOR)));
        if self.reveal_target < 0.5 {
            self.show();
        }
    }

    /// A Look slider moving in the settings window: shown now, saved by the window when it
    /// settles or is let go of (then the dock reloads as usual). Sizes are shown by scaling the
    /// current icons; new ones are made only once the size is saved.
    fn preview(&mut self, which: usize, value: f32) {
        let s = &mut self.settings;
        match which {
            0 => {
                s.icon_size = value.clamp(16.0, 256.0);
                s.zoom_size = s.zoom_size.max(s.icon_size);
            }
            1 => s.zoom_size = value.clamp(s.icon_size, 512.0),
            2 => s.spacing = value.clamp(0.0, 64.0),
            3 => s.label_size = value.clamp(8.0, 24.0),
            4 => s.background_opacity = value.clamp(0.0, 100.0) as u32,
            5 => s.icon_opacity = value.clamp(10.0, 100.0) as u32,
            _ => return,
        }
        self.previewing = true;
        self.cancel_drag(); // positions are about to change
        self.apply_layout();
        self.peek();
        if self.window_visible {
            self.render();
        }
    }

    // ---- Groups ----------------------------------------------------------------------------

    /// Opens a group's pop-up below its icon (closing any other).
    fn open_group(&mut self, index: usize) {
        self.close_group_now("another group opened");
        let Some(item) = self.items.get(index) else { return };
        if item.children.is_empty() {
            let label = edit::label_of(&item.cfg);
            self.show_notice(&format!("{label} is empty: right-click an icon > Move to group"));
            return;
        }
        let Some(slot) = self.layout.slot_of(index) else { return };
        let g = self.layout.geo;
        let params = popup::Params {
            count: item.children.len(),
            icon: g.icon,
            scale: g.scale,
            line_h: (self.settings.label_size * 1.4 * g.scale).ceil(), // Segoe UI's own line height
            anchor_x: self.win_x as f32 + slot.center(&g) + item.shift,
            top: self.monitor.top as f32 + g.reach() + (6.0 * g.scale).round(),
            screen_left: self.monitor.left as f32,
            screen_right: self.monitor.right as f32,
            screen_bottom: self.screen.work.bottom as f32,
        };
        if self.log_groups {
            log_info!("opened group {} ({} items)", edit::label_of(&item.cfg), item.children.len());
        }
        let layout = popup::layout(&params);
        let closer = popup::Closer::new(GROUP_CLOSE_GRACE);
        self.group = Some(OpenGroup { index, layout, hovered: None, pressed: None, closer, tracking: false });
        if self.drag.is_none() {
            system::forget_escape(); // only an Esc from now on closes it
        }
        if self.render_popup() {
            if let Some(window) = self.popup_window {
                unsafe {
                    let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(window, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
        }
        self.set_poll(25);
        if self.window_visible {
            self.render(); // its name under it gives way to the pop-up
        }
    }

    /// Closes the open group's pop-up, fading it out.
    fn close_group(&mut self, why: &str) {
        self.close_group_after(why, Duration::ZERO);
    }

    /// Closes it after it has stayed `hold` (it takes no clicks meanwhile), then fades it out.
    /// With Windows' animations off, or while a test runs thousands of opens, it goes at once.
    fn close_group_after(&mut self, why: &str, hold: Duration) {
        let Some((x, y)) = self.group.as_ref().map(|open| (open.layout.x, open.layout.y)) else { return };
        if self.reduced_motion || !self.log_groups || self.popup_window.is_none() {
            self.close_group_now(why);
            return;
        }
        self.end_group(why);
        self.popup_fading = Some((Instant::now(), hold, x, y));
        self.last_frame = None; // the frame loop picks it up
    }

    /// Closes it at once (another group opening, a drag starting).
    fn close_group_now(&mut self, why: &str) {
        self.end_group(why);
        self.end_fade();
    }

    fn end_fade(&mut self) {
        self.popup_fading = None;
        if let Some(window) = self.popup_window {
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
            }
        }
        self.renderer.release_popup();
    }

    /// One step of a closing pop-up's fade; true while it's still showing.
    fn fade_popup(&mut self) -> bool {
        let Some((since, hold, x, y)) = self.popup_fading else { return false };
        let alpha = popup::fade(since.elapsed(), hold, POPUP_FADE);
        if alpha <= 0.0 {
            self.end_fade();
            return false;
        }
        if let Some(window) = self.popup_window {
            let _ = self.renderer.present_popup_faded(window, x, y, alpha);
        }
        true
    }

    fn end_group(&mut self, why: &str) {
        let Some(open) = self.group.take() else { return };
        self.child_press = None;
        if self.log_groups {
            let label = self.items.get(open.index).map(|item| edit::label_of(&item.cfg)).unwrap_or_default();
            log_info!("closed group {label} ({why})");
        }
        if self.window_visible {
            self.render();
        }
    }

    /// Draws the open group's pop-up; true if there's a picture to show.
    fn draw_popup(&mut self) -> bool {
        let Some(open) = &self.group else { return false };
        let (Some(name_font), Some(wrapped), Some(title)) = (&self.popup_format, &self.popup_wrap_format, &self.popup_title_format) else {
            return false;
        };
        let Some(item) = self.items.get(open.index) else { return false };
        if self.renderer.begin_popup(open.layout.width, open.layout.height).is_err() {
            return false;
        }
        // An item being dragged out of this group leaves a trace; where a dragged icon would
        // land shows as a bar.
        let dragged = self.drag.as_ref().filter(|drag| drag.index == open.index).and_then(|drag| drag.child);
        let marker = self.drag.as_ref().and_then(|drag| drag.in_popup).filter(|&(group, _)| group == open.index).map(|(_, gap)| gap);
        let shown: Vec<popup::Shown> = item
            .children
            .iter()
            .enumerate()
            .map(|(k, child)| popup::Shown {
                icon: child.icon.as_ref(),
                label: &child.label,
                dim: self.missing_children.contains(&(open.index, k)),
                ghost: dragged == Some(k),
            })
            .collect();
        let name: Vec<u16> = edit::label_of(&item.cfg).encode_utf16().collect();
        let fonts = popup::Fonts { name: name_font, wrapped, title };
        let state = popup::State { hovered: open.hovered, marker };
        let measure = |text: &[u16], width: f32| self.text_height_in(text, wrapped, width);
        popup::draw(&self.renderer.rt, &self.brush, &fonts, &open.layout, &name, &shown, state, self.layout.geo.scale, &measure);
        self.renderer.end().is_ok()
    }

    fn render_popup(&mut self) -> bool {
        if !self.draw_popup() {
            return false;
        }
        let (Some(window), Some(open)) = (self.popup_window, &self.group) else { return false };
        self.renderer.present_popup(window, open.layout.x, open.layout.y).is_ok()
    }

    /// The mouse on an open group's pop-up: hover, click to launch, right-click for its menu.
    fn handle_popup(&mut self, msg: u32, lparam: LPARAM) -> (Option<LRESULT>, Option<Action>) {
        let done = Some(LRESULT(0));
        if msg == WM_MOUSEACTIVATE {
            return (Some(LRESULT(MA_NOACTIVATE as isize)), None); // never takes the focus
        }
        // Dragging an item out of the pop-up (the pop-up holds the mouse meanwhile).
        match msg {
            WM_MOUSEMOVE if self.drag.is_some() => {
                self.update_drag();
                return (done, None);
            }
            WM_MOUSEMOVE if self.child_press.is_some_and(|(_, _, start)| self.moved_past_drag_distance(start)) => {
                self.start_child_drag();
                return (done, None);
            }
            WM_LBUTTONUP if self.drag.is_some() => return (done, self.finish_drag()),
            WM_RBUTTONUP if self.drag.is_some() => {
                self.cancel_drag();
                return (done, None);
            }
            WM_CAPTURECHANGED => {
                if lparam.0 != self.popup_window.map_or(0, |window| window.0 as isize) {
                    self.child_press = None;
                    self.cancel_drag();
                }
                return (done, None);
            }
            WM_LBUTTONUP => self.child_press = None, // a click, not a drag
            _ => {}
        }
        let (x, y) = point_from_lparam(lparam);
        let Some(open) = self.group.as_mut() else { return (None, None) };
        let at = open.layout.hit(x as f32, y as f32);
        match msg {
            WM_MOUSEMOVE => {
                if !open.tracking {
                    open.tracking = true;
                    if let Some(window) = self.popup_window {
                        let mut track = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: window, dwHoverTime: 0 };
                        unsafe {
                            let _ = TrackMouseEvent(&mut track);
                        }
                    }
                }
                if open.hovered != at {
                    open.hovered = at;
                    self.render_popup();
                }
                (done, None)
            }
            WM_MOUSELEAVE => {
                open.tracking = false;
                if open.hovered.take().is_some() {
                    self.render_popup();
                }
                (done, None)
            }
            WM_LBUTTONDOWN => {
                log_info!("pop-up: press on {at:?}");
                open.pressed = at;
                // Hold the mouse while the button is down, so a drag out of the pop-up keeps
                // getting moves (not when locked: then a press can only be a click).
                let group = open.index;
                let locked = self.settings.locked && !system::control_down(); // Ctrl overrides Lock
                if let (Some(k), false, Some(pt), Some(window)) = (at, locked, system::cursor_pos(), self.popup_window) {
                    self.child_press = Some((group, k, pt));
                    unsafe {
                        SetCapture(window);
                    }
                }
                (done, None)
            }
            WM_LBUTTONUP => {
                let (index, pressed) = (open.index, open.pressed.take());
                let Some(k) = pressed.filter(|&k| Some(k) == at) else { return (done, None) };
                let k = if crate::breaks::broken("group.wrong-child") { (k + 1) % self.items.get(index).map_or(1, |item| item.children.len().max(1)) } else { k };
                let Some(child) = self.items.get(index).and_then(|item| item.children.get(k)).map(|child| child.cfg.clone()) else {
                    return (done, None);
                };
                if child.is_separator() {
                    return (done, None);
                }
                self.close_group("launched an item");
                self.launch_effect(index); // on the group's icon
                (done, Some(Action::Launch(child, index, self.generation)))
            }
            WM_RBUTTONUP => {
                let index = open.index;
                match (at, system::cursor_pos()) {
                    (Some(k), Some(pt)) => (done, Some(Action::ChildMenu(pt, index, k))),
                    _ => (done, None),
                }
            }
            _ => (None, None),
        }
    }

    /// Right-click → New group: the item becomes a group holding it, in its place.
    fn new_group(&mut self, index: usize, expect: &str) -> std::result::Result<(), String> {
        let (store, _) = self.editable()?;
        let ((), change) = store.change(|doc| edit::make_group(doc, index, expect, NEW_GROUP_NAME))?;
        log_info!("made a new group with {expect}");
        let step = UndoStep { what: format!("new group with {expect}"), change, removed_id: None, put_back: None };
        self.show_change(step, format!("New group with {expect} (right-click > Properties to name it)"))
    }

    /// Right-click → Ungroup: its items go back on the dock, in its place.
    fn ungroup(&mut self, index: usize, expect: &str) -> std::result::Result<(), String> {
        let (store, _) = self.editable()?;
        let (count, change) = store.change(|doc| edit::ungroup(doc, index, expect))?;
        log_info!("ungrouped {expect} ({count} items)");
        let step = UndoStep { what: format!("ungroup {expect}"), change, removed_id: None, put_back: None };
        self.show_change(step, format!("Ungrouped {expect}"))
    }

    /// Right-click → Move to group ▸: into that group, at its end.
    fn move_into_group(&mut self, index: usize, expect: &str, group: usize, group_label: &str) -> std::result::Result<(), String> {
        let end = self.items.get(group).map_or(0, |item| item.children.len());
        self.move_spot(Spot::dock(index), Spot::in_group(group, end), expect, format!("Moved {expect} to {group_label}"))
    }

    /// From a group's pop-up: out onto the dock, just after the group.
    fn take_out(&mut self, group: usize, child: usize, expect: &str, group_label: &str) -> std::result::Result<(), String> {
        self.move_spot(Spot::in_group(group, child), Spot::dock(group + 1), expect, format!("Took {expect} out of {group_label}"))
    }

    /// From a group's pop-up: removed (into Recently removed, remembering its group).
    fn remove_child(&mut self, group: usize, child: usize, expect: &str, group_label: &str) -> std::result::Result<(), String> {
        let (store, removed) = self.editable()?;
        let (table, change) = store.change(|doc| edit::remove(doc, Spot::in_group(group, child), expect))?;
        let removed_id = removed.add(&table, child, Some(group_label)).inspect_err(|e| log_warn!("Recently removed: {e}")).ok();
        log_info!("removed {expect} from {group_label}");
        let step = UndoStep { what: format!("remove {expect} from {group_label}"), change, removed_id, put_back: None };
        self.show_change(step, format!("Removed {expect} from {group_label}"))
    }

    /// Hides at once, with no slide.
    fn hide_now(&mut self) {
        self.cancel_drag();
        self.hide();
        self.reveal = 0.0;
        if self.window_visible {
            self.finish_hiding();
        }
        self.last_frame = None;
    }

    /// The dock is out of sight: hide the window, stop every animation (nothing would show, and
    /// drawing would rebuild the buffer) and free the drawing buffer.
    fn finish_hiding(&mut self) {
        // Never keep the mouse while out of sight.
        self.end_press();
        self.cancel_drag();
        self.incoming = None;
        if let Some(hwnd) = self.hwnd {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
        self.window_visible = false;
        self.tracking_mouse = false;
        for item in &mut self.items {
            item.hover_t = 0.0;
            item.bounce_start = None;
            item.pulse_start = None;
            item.shake_start = None;
        }
        self.refresh_hidden();
        self.tidy_hidden();
    }

    /// Once out of sight: the drawing buffer goes (~3 MB back while hidden; rebuilt in ~2 ms on
    /// the next reveal), Direct2D lets go of what it freed, and the heaps hand back their free
    /// space (see `system::give_back_memory`).
    fn tidy_hidden(&mut self) {
        self.renderer.settle();
        system::give_back_memory();
    }

    /// Runs on a timer: slowly (10x a second) while the pointer is far from the edge, quickly
    /// while it's near, so the dock reacts on time without waking the CPU constantly.
    fn on_poll(&mut self) -> Option<Action> {
        let now = Instant::now();
        if let Some(message) = self.check_config(now) {
            return Some(Action::FileError(message));
        }
        // Windows can't say where the pointer is while the lock screen or a UAC prompt is up:
        // then it counts as far away, so the dock still goes back up.
        let pt = system::cursor_pos().unwrap_or(POINT { x: -1_000_000, y: -1_000_000 });
        let top = self.monitor.top;
        let edge = self.screen.monitor.top; // the screen's own top edge (above a top taskbar)
        // Safety net: the dock thinks a press or a drag is going on, but the button is up (its
        // release never reached the dock). Let go of the mouse, so nothing else is blocked.
        if (self.press.is_some() || self.child_press.is_some() || self.drag.is_some()) && !system::primary_button_down() {
            if now.duration_since(*self.button_up_since.get_or_insert(now)) >= MISSED_RELEASE_AFTER {
                log_warn!("missed a button release; letting go of the mouse");
                self.button_up_since = None;
                self.child_press = None;
                self.end_press();
                self.cancel_drag();
            }
        } else {
            self.button_up_since = None;
        }
        self.sync_capture();

        if self.reveal_target < 0.5 {
            // A held mouse button means a drag: a window toward Snap (never reveals the dock), or
            // files, apps or links. For those, an invisible drop zone goes out along the edge, and
            // Windows' drag and drop tells the dock when they arrive. Text drags never do.
            let at_edge = self.layout.at_reveal_edge(self.win_x, edge, pt.x, pt.y);
            let near = self.layout.near_edge(self.win_x, edge, top, pt.x, pt.y);
            // Buttons are only asked about near the edge, or while the zone is out (it's not free).
            let buttons = (near || self.drop_zone_shown) && system::mouse_buttons_down();
            let held = near && buttons;
            // The zone goes out once a button is held near the edge, and then stays put until every
            // button is up: no windows coming and going under a drag in progress.
            let allowed = !system::window_being_moved(); // locked too: the dock then says so
            self.set_drop_zone(allowed && (held || (self.drop_zone_shown && buttons)));
            // Windows only looks for a drop target when the pointer moves. A drag that came to rest
            // on the edge before the zone was out would never be noticed, so nudge the pointer one
            // pixel and straight back (invisible), once.
            let waited = self.zone_since.is_some_and(|since| now - since >= Duration::from_millis(100));
            if self.drop_zone_shown && at_edge && held && waited && !self.zone_nudged && self.incoming.is_none() {
                self.zone_nudged = true;
                unsafe {
                    let _ = SetCursorPos(pt.x, pt.y + 1);
                    let _ = SetCursorPos(pt.x, pt.y);
                }
            }
            let at_edge = at_edge && !held;
            let delay = Duration::from_millis(self.settings.popup_delay_ms);
            if self.edge.update(at_edge, now, delay) && !system::fullscreen_active(self.screen.monitor) {
                self.show();
            }
            // Quick checks near the edge, and while any button is held (a drag may be on its way up).
            let quick = near || self.drop_zone_shown || system::mouse_buttons_down();
            self.set_poll(if quick { 16 } else { 100 });
        } else {
            // The dock (on top of the zone) takes drops now; the zone goes once the drag is over.
            if self.drop_zone_shown && !system::mouse_buttons_down() {
                self.set_drop_zone(false);
            }
            let busy = self.menu_open || self.drag.is_some() || self.press.is_some() || self.child_press.is_some() || self.incoming.is_some() || self.group.is_some();
            if !busy && system::fullscreen_active(self.screen.monitor) {
                self.hide();
                return None;
            }
            let over = self.layout.pointer_over(self.win_x, edge, top, pt.x, pt.y);
            // An open group never has the keyboard focus, so the dock looks: Esc, a click
            // anywhere else, or the pointer away from the dock and the pop-up for a moment.
            if self.group.is_some() && !self.menu_open && self.drag.is_none() && self.child_press.is_none() {
                let on_popup = self.group.as_ref().is_some_and(|open| open.layout.covers(pt.x, pt.y));
                let inside = over || on_popup;
                let seen = popup::Seen { inside, button_outside: !inside && system::mouse_buttons_down(), escape: system::escape_pressed() };
                if let Some(why) = self.group.as_mut().and_then(|open| open.closer.update(seen, now)) {
                    self.close_group(&format!("{why:?}"));
                }
            }
            // During a drag, things that happen with the pointer still: a group held off the dock
            // gets its remove mark, a group held over springs open.
            if self.drag.is_some() {
                self.update_drag();
            }
            if over {
                self.linger_until = None; // from now on it goes when the pointer does
            }
            let lingering = self.linger_until.is_some_and(|until| now < until);
            if lingering && !busy && system::escape_pressed() {
                self.linger_until = None;
                self.hide();
                return None;
            }
            let away = !(over || lingering || busy);
            if self.leave.update(away, now, Duration::from_millis(self.settings.hide_delay_ms)) {
                self.hide();
            }
            // During a drag, and while out for the shortcut, Esc counts: looked for often enough
            // to see a quick tap while it's down (Windows' "was pressed" note isn't reliable).
            self.set_poll(if self.drag.is_some() || lingering { 16 } else if self.group.is_some() { 25 } else { 50 });
        }
        None
    }

    fn set_poll(&mut self, ms: u32) {
        if self.poll_ms == ms || self.presence.away() {
            return; // while away, polling stays off until `apply_presence` turns it back on
        }
        self.poll_ms = ms;
        if let Some(hwnd) = self.hwnd {
            let tolerance = if ms >= 100 { 50 } else { 0 };
            unsafe {
                SetCoalescableTimer(Some(hwnd), TIMER_POLL, ms, None, tolerance);
            }
        }
    }

    // ---- Input ---------------------------------------------------------------------------

    fn on_mouse_move(&mut self, x: f32, y: f32) {
        if !self.tracking_mouse {
            if let Some(hwnd) = self.hwnd {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut track);
                }
            }
            self.tracking_mouse = true;
        }
        if self.reveal_target == 0.0 && self.reveal > 0.0 {
            self.show(); // pointer came back while the dock was sliding away
        }
        self.leave.reset();
        let hovered = self.layout.hit_test(x, y);
        if hovered != self.hovered {
            if let Some(index) = hovered {
                self.request_zoom(index);
            }
        }
        self.hovered = hovered;
    }

    /// The launch effect on a clicked icon: a bounce or a glow pulse (none with Windows' "Animation
    /// effects" off).
    fn launch_effect(&mut self, index: usize) {
        let Some(item) = self.items.get_mut(index).filter(|_| !self.reduced_motion) else { return };
        match self.settings.launch_effect {
            LaunchEffect::Bounce => item.bounce_start = Some(Instant::now()),
            LaunchEffect::Glow => item.pulse_start = Some(Instant::now()),
            LaunchEffect::None => {}
        }
    }

    /// The separator under the pointer (a mouse message's position), if any.
    fn separator_at(&self, lparam: LPARAM) -> Option<usize> {
        let (x, y) = point_from_lparam(lparam);
        self.layout.separator_at(x as f32, y as f32)
    }

    /// A click on the undo strip (anywhere on it: it's small, and it only does one thing).
    fn strip_hit(&self, lparam: LPARAM) -> bool {
        let Some(strip) = self.strip.as_ref().filter(|strip| strip.undo_rect.is_some() && !crate::breaks::broken("undo.no-hit-area")) else { return false };
        let (x, y) = point_from_lparam(lparam);
        let (x, y) = (x as f32, y as f32 - self.content_offset());
        x >= strip.rect.left && x < strip.rect.right && y >= strip.rect.top && y < strip.rect.bottom
    }
    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> (Option<LRESULT>, Option<Action>) {
        let done = Some(LRESULT(0));
        if msg == self.taskbar_created && msg != 0 {
            log_info!("Explorer restarted");
            self.schedule_recheck();
            return (done, None);
        }
        match msg {
            WM_MOUSEMOVE => {
                if self.drag.is_some() {
                    self.update_drag();
                } else if self.drag_should_start() {
                    self.start_drag();
                } else if self.locked_press.is_some_and(|start| self.moved_past_drag_distance(start)) {
                    self.locked_press = None; // an icon can't be dragged while locked: say so
                    self.show_notice(LOCKED_ICON_NOTICE);
                } else {
                    let (x, y) = point_from_lparam(lparam);
                    self.on_mouse_move(x as f32, y as f32);
                }
                (done, None)
            }
            WM_MOUSELEAVE => {
                self.tracking_mouse = false;
                self.hovered = None;
                (done, None)
            }
            WM_LBUTTONDOWN => {
                if self.group.is_some() || self.hovered.is_none() {
                    let (x, y) = point_from_lparam(lparam);
                    log_info!("dock: press at {x},{y} (window) on {:?}; group open: {}", self.hovered, self.group.is_some());
                }
                self.pressed = self.hovered;
                // An icon, or a separator (picked up and moved like one; never clicked).
                let grabbed = self.hovered.or_else(|| self.separator_at(lparam));
                // Hold on to the mouse while the button is down, so a drag keeps getting moves
                // outside the dock too. Not when locked: then a press can only be a click.
                if self.settings.locked && !system::control_down() {
                    self.locked_press = grabbed.and(system::cursor_pos());
                } else if let (Some(index), Some(pt), Some(hwnd)) = (grabbed, system::cursor_pos(), self.hwnd) {
                    self.press = Some((index, pt));
                    unsafe {
                        SetCapture(hwnd);
                    }
                }
                (done, None)
            }
            WM_LBUTTONUP if self.drag.is_some() => (done, self.finish_drag()),
            WM_LBUTTONUP if self.strip_hit(lparam) => {
                self.end_press();
                self.pressed = None;
                (done, self.undo().err().map(Action::Error))
            }
            WM_LBUTTONUP => {
                self.locked_press = None;
                self.end_press();
                match (self.pressed.take(), self.hovered) {
                    (Some(pressed), Some(hovered)) if pressed == hovered => {
                        let now = Instant::now();
                        if !self.clicks.allow(pressed, now) {
                            log_info!("click on {} ignored: the second half of a double-click", edit::label_of(&self.items[pressed].cfg));
                            return (done, None);
                        }
                        // A group opens (or, already open, closes); anything else closes it.
                        if self.items[pressed].cfg.kind == Kind::Group {
                            if self.group.as_ref().is_some_and(|open| open.index == pressed) {
                                self.close_group("its icon clicked again");
                            } else {
                                self.open_group(pressed);
                            }
                            return (done, None);
                        }
                        self.close_group("another item clicked");
                        self.launch_effect(pressed);
                        (done, Some(Action::Launch(self.items[pressed].cfg.clone(), pressed, self.generation)))
                    }
                    (pressed, hovered) => {
                        // Not a click on one item (pressed on one, let go on another or on none).
                        if pressed.is_some() || hovered.is_some() {
                            log_info!("no click: pressed on {pressed:?}, let go on {hovered:?}");
                        }
                        (done, None)
                    }
                }
            }
            WM_RBUTTONUP if self.drag.is_some() => {
                self.cancel_drag();
                (done, None)
            }
            WM_RBUTTONUP => {
                self.close_group("right-click on the dock");
                let item = self.hovered.or_else(|| self.separator_at(lparam));
                (done, system::cursor_pos().map(|pt| Action::Menu(pt, item)))
            }
            WM_CAPTURECHANGED => {
                // Windows gave the mouse to another window (not us letting go): cancel the drag.
                if lparam.0 != self.hwnd.map_or(0, |hwnd| hwnd.0 as isize) {
                    self.press = None;
                    self.cancel_drag();
                }
                (done, None)
            }
            WM_MOUSEACTIVATE => (Some(LRESULT(MA_NOACTIVATE as isize)), None),
            WM_TIMER if wparam.0 == TIMER_POLL => (done, self.on_poll()),
            WM_TIMER if wparam.0 == TIMER_RECHECK => {
                self.stop_timer(TIMER_RECHECK);
                self.recheck();
                self.recheck_missing(true);
                (done, None)
            }
            WM_APP_MISSING_CHECKED => {
                self.receive_missing_check(wparam.0 != 0);
                (done, None)
            }
            WM_TIMER if wparam.0 == TIMER_WELCOME => {
                self.stop_timer(TIMER_WELCOME);
                self.welcome();
                (done, None)
            }
            WM_TIMER if wparam.0 == TIMER_STRIP => {
                self.stop_timer(TIMER_STRIP);
                self.undo = None; // from now on, Put back
                if self.strip.take().is_some() {
                    log_info!("strip gone");
                    if self.window_visible {
                        self.render();
                    }
                }
                (done, None)
            }
            WM_TIMER if wparam.0 == TIMER_CONFIG => {
                self.stop_timer(TIMER_CONFIG);
                (done, self.check_config_now().map(Action::FileError))
            }
            WM_TIMER if wparam.0 == TIMER_HEALTH => {
                log_info!("health: {}", system::health());
                (done, None)
            }
            WM_TIMER if wparam.0 == TIMER_PRESENCE => {
                self.check_presence(); // the timer repeats while away
                (done, None)
            }
            WM_TIMER if wparam.0 == TIMER_READ_RETRY => {
                self.stop_timer(TIMER_READ_RETRY);
                (done, self.reload_keeping_undo().err().map(Action::FileError))
            }
            WM_APP_PICKED => (done, self.apply_pick().err().map(Action::Error)),
            WM_APP_PEEK => {
                self.peek();
                (done, None)
            }
            WM_APP_PREVIEW => {
                self.preview(wparam.0, lparam.0 as f32 / 10.0);
                (done, None)
            }
            WM_APP_RELOAD_NOW => {
                // Its saves are complete when it says so (atomic), so no need to wait for the
                // file to settle. Like any change made elsewhere, it ends the last Undo. The
                // text is compared, not the file's date: on some drives (FAT32) two saves in
                // the same 2 seconds have the same date.
                let text = self.store.as_ref().and_then(|store| config::read_text(store.config_path()).ok());
                if text.is_some() && text.as_deref().map(fingerprint) == self.loaded_text {
                    (done, None)
                } else {
                    self.undo = None;
                    self.strip = None;
                    (done, self.reload().err().map(Action::FileError))
                }
            }
            WM_APP_DROPPED => match self.pending_drop.take() {
                Some(PendingDrop::Add(gap, items)) => (done, self.add_items(gap, &items).err().map(Action::Error)),
                Some(PendingDrop::Onto(index, label, dropped)) => (done, self.drop_onto(index, &label, dropped)),
                None => (done, None),
            },
            WM_APP_LAUNCH_FAILED => {
                let (index, generation) = (wparam.0, lparam.0 as u64);
                let showing = self.window_visible && self.reveal_target == 1.0;
                if showing && generation == self.generation && index < self.items.len() && !self.reduced_motion {
                    self.items[index].bounce_start = None;
                    self.items[index].pulse_start = None;
                    self.items[index].shake_start = Some(Instant::now());
                    log_info!("shook {} (launch failed)", edit::label_of(&self.items[index].cfg));
                }
                (done, None)
            }
            WM_APP_ICONS => {
                self.receive_icons();
                (done, None)
            }
            WM_APP_BIN => {
                self.receive_bin_state();
                (done, None)
            }
            WM_APP_TEST_WHERE if crate::home::is_dev_build() => {
                let g = self.layout.geo;
                for slot in &self.layout.slots {
                    let item = &self.items[slot.item];
                    let x = self.win_x + slot.center(&g).round() as i32;
                    let y = self.monitor.top + (g.pad_top + g.icon / 2.0).round() as i32;
                    if slot.separator {
                        log_info!("TEST where: separator {} at {x},{y} kind Separator takes drops false", slot.item);
                        continue;
                    }
                    log_info!("TEST where: {} at {x},{y} kind {:?} takes drops {}", edit::label_of(&item.cfg), item.cfg.kind, self.takes_drops(slot.item));
                }
                if let Some(open) = &self.group {
                    for (cell, child) in open.layout.cells.iter().zip(&self.items[open.index].children) {
                        let x = open.layout.x + ((cell.icon.left + cell.icon.right) / 2.0) as i32;
                        let y = open.layout.y + ((cell.icon.top + cell.icon.bottom) / 2.0) as i32;
                        log_info!("TEST where: in group {} at {x},{y}", edit::label_of(&child.cfg));
                    }
                }
                (done, None)
            }
            WM_APP_TEST_GROUP if crate::home::is_dev_build() => {
                if let Some(index) = self.items.iter().position(|item| item.cfg.kind == Kind::Group) {
                    self.log_groups = false;
                    for _ in 0..wparam.0.min(10_000) {
                        self.open_group(index);
                        self.close_group("test");
                    }
                    self.log_groups = true;
                }
                log_info!("TEST group cycles done");
                (done, None)
            }
            WM_APP_TEST_HOVER if crate::home::is_dev_build() => {
                let index = self.items.get(wparam.0).filter(|item| !item.cfg.is_separator()).map(|_| wparam.0);
                if index != self.hovered {
                    if let Some(index) = index {
                        self.request_zoom(index);
                    }
                }
                self.hovered = index;
                (done, None)
            }
            WM_APP_TEST_LAUNCH if crate::home::is_dev_build() => match self.items.get(wparam.0).filter(|item| item.cfg.kind == Kind::App) {
                Some(item) => {
                    let cfg = item.cfg.clone();
                    self.launch_effect(wparam.0);
                    (done, Some(Action::Launch(cfg, wparam.0, self.generation)))
                }
                None => (done, None),
            },
            WM_APP_TEST_CRASH if crate::home::is_dev_build() => {
                log_warn!("crashing on purpose (test)");
                if wparam.0 == 1 {
                    unsafe { windows::Win32::System::Diagnostics::Debug::RaiseException(0xE0D0_C0DE, 0, None) };
                }
                std::process::abort();
            }
            WM_APP_TEST_STATS if crate::home::is_dev_build() => {
                log_info!("TEST stats {}", self.test_stats());
                (done, None)
            }
            WM_APP_TEST_STATE if crate::home::is_dev_build() => {
                // Outside the dock folder: a write there is a change the dock waits to settle,
                // and a test asking often would hold up the very reload it waits for. Whole or
                // not at all: a test may be reading the last one.
                let path = std::env::temp_dir().join("desktop-dock-test-state.json");
                let temp = path.with_extension("json.tmp");
                if let Err(e) = std::fs::write(&temp, self.test_state()).and_then(|()| std::fs::rename(&temp, &path)) {
                    log_warn!("couldn't write the test state: {e}");
                }
                (done, None)
            }
            WM_APP_TEST_MODAL if crate::home::is_dev_build() => (done, Some(Action::TestModal(wparam.0 as u64))),
            WM_APP_TEST_HANG if crate::home::is_dev_build() => {
                log_warn!("not responding on purpose (test)");
                loop {
                    std::thread::sleep(Duration::from_secs(3600));
                }
            }
            WM_APP_HEAL => {
                self.start_heal();
                (done, None)
            }
            WM_APP_HEALED => {
                self.receive_heal();
                (done, None)
            }
            WM_APP_RECHECK_BIN => {
                self.bin_checked = None;
                self.bin_check_again = self.bin_checking; // the one under way may have started before
                self.check_recycle_bin();
                (done, None)
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                self.schedule_recheck();
                (done, None)
            }
            WM_DWMCOLORIZATIONCOLORCHANGED => {
                self.accent = system::accent_colour().map_or(DEFAULT_GLOW, icons::glow_bright);
                if self.window_visible {
                    self.render();
                }
                (done, None)
            }
            WM_POWERBROADCAST => {
                match wparam.0 as u32 {
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                        self.placeholder_retries = PLACEHOLDER_RETRIES;
                        self.check_presence();
                        self.schedule_recheck();
                    }
                    PBT_POWERSETTINGCHANGE => {
                        if let Some(state) = unsafe { system::display_state(lparam) } {
                            self.presence.display_state(state);
                            self.apply_presence();
                        }
                    }
                    _ => {}
                }
                (Some(LRESULT(1)), None)
            }
            WM_WTSSESSION_CHANGE => {
                self.presence.session_event(wparam.0 as u32);
                self.apply_presence();
                (done, None)
            }
            WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
                self.summon();
                (done, None)
            }
            WM_DESTROY => {
                if let Some(hwnd) = self.hwnd.filter(|_| self.hotkey.take().is_some()) {
                    unsafe {
                        let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                    }
                }
                unsafe { PostQuitMessage(0) };
                (done, None)
            }
            _ => (None, None),
        }
    }

    // ---- Animation and drawing -----------------------------------------------------------

    fn hover_goal(&self, index: usize) -> f32 {
        let onto = self.incoming.as_ref().is_some_and(|incoming| incoming.onto == Some(index));
        if (self.hovered == Some(index) || onto) && self.reveal_target == 1.0 { 1.0 } else { 0.0 }
    }

    fn needs_frames(&self) -> bool {
        self.popup_fading.is_some()
            || self.reveal != self.reveal_target
            || self
                .items
                .iter()
                .enumerate()
                .any(|(i, item)| item.hover_t != self.hover_goal(i) || item.effect_running() || item.shift != self.shift_target(i))
    }

    fn frame(&mut self) {
        let now = Instant::now();
        let dt = self.last_frame.map_or(1.0 / 120.0, |last| (now - last).as_secs_f32()).min(0.05);
        self.last_frame = Some(now);

        // With Windows' "Animation effects" off, everything jumps straight to where it's going.
        let s = &self.settings;
        let (slide_in, slide_out, zoom_ms) =
            if self.reduced_motion { (0, 0, 0) } else { (s.slide_in_ms, s.slide_out_ms, s.zoom_ms) };
        self.reveal = motion::step_toward(self.reveal, self.reveal_target, dt, slide_in, slide_out);
        let goals: Vec<f32> = (0..self.items.len()).map(|i| self.hover_goal(i)).collect();
        let hover_was_moving = self.items.iter().zip(&goals).any(|(item, goal)| item.hover_t != *goal);
        let effects_were_running = self.items.iter().any(Item::effect_running);
        // Icons making room for a dragged one glide sideways.
        let targets: Vec<f32> = (0..self.items.len()).map(|i| self.shift_target(i)).collect();
        let shifts_were_moving = self.items.iter().zip(&targets).any(|(item, target)| item.shift != *target);
        let glide = if self.reduced_motion { 1.0 } else { (dt * 18.0).min(1.0) };
        for (item, target) in self.items.iter_mut().zip(&targets) {
            item.shift += (target - item.shift) * glide;
            if (target - item.shift).abs() < 0.5 {
                item.shift = *target;
            }
        }
        for (item, goal) in self.items.iter_mut().zip(goals) {
            item.hover_t = motion::step_toward(item.hover_t, goal, dt, zoom_ms, zoom_ms);
            if item.bounce_start.is_some_and(|start| now - start >= BOUNCE) {
                item.bounce_start = None;
            }
            if item.pulse_start.is_some_and(|start| now - start >= PULSE) {
                item.pulse_start = None;
            }
            if item.shake_start.is_some_and(|start| now - start >= SHAKE) {
                item.shake_start = None;
            }
        }

        self.fade_popup();
        let content_changing = !self.slide_by_moving
            || effects_were_running
            || shifts_were_moving
            || (0..self.items.len()).any(|i| self.items[i].hover_t != self.hover_goal(i))
            || hover_was_moving;
        if content_changing {
            self.render();
        } else {
            self.move_window(); // only the slide is moving: no redraw needed
        }

        if self.reveal == 0.0 && self.reveal_target == 0.0 && self.window_visible {
            self.finish_hiding();
            self.set_poll(100);
        }
        if !self.needs_frames() {
            self.last_frame = None;
        }
    }

    fn render(&mut self) {
        // Fail soft: a failed frame is skipped; the next one tries again.
        if self.draw().is_ok() {
            if let Some(hwnd) = self.hwnd {
                let _ = self.renderer.present(hwnd, self.win_x, self.window_y());
            }
        }
    }

    /// Where the window's top edge goes. When sliding by moving the window, it rises past the
    /// screen edge as `reveal` goes to 0; otherwise it stays put and the drawing slides instead.
    fn window_y(&self) -> i32 {
        let offset = if self.slide_by_moving { self.layout.geo.slide_offset(self.reveal) as i32 } else { 0 };
        self.monitor.top + offset
    }

    /// How far the drawing itself is shifted up, when sliding by drawing (see `slide_by_moving`).
    fn content_offset(&self) -> f32 {
        if self.slide_by_moving { 0.0 } else { self.layout.geo.slide_offset(self.reveal) }
    }

    /// Slides the already-drawn dock by moving its window: no redraw at all.
    fn move_window(&self) {
        if let Some(hwnd) = self.hwnd {
            unsafe {
                let _ = SetWindowPos(hwnd, None, self.win_x, self.window_y(), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
    }

    /// The glow's colour for an item: Windows' accent, or the icon's own main colour (a group's
    /// tile: its first item's), the accent standing in for icons with hardly any colour.
    fn glow_colour(&self, item: &Item) -> [f32; 3] {
        if self.settings.hover_glow == HoverGlow::Accent || crate::breaks::broken("glow.accent-for-icon") {
            return self.accent;
        }
        let sources = if item.shows_preview() {
            item.children.iter().find(|child| !child.cfg.is_separator()).map_or(&item.sources, |child| &child.sources)
        } else {
            &item.sources
        };
        self.tints.get(sources).copied().flatten().unwrap_or(self.accent)
    }

    /// How strongly an item glows now: its hover (if the hover glow is on) or a launch pulse.
    fn glow_strength(&self, item: &Item) -> f32 {
        let hover = if self.settings.hover_glow == HoverGlow::None { 0.0 } else { motion::ease_out(item.hover_t) };
        hover.max(item.pulse())
    }

    /// The glow's shape, made once and drawn in each icon's own colour (`FillOpacityMask`):
    /// full in the middle, already easing off under the icon (from 45% of the radius; its edge
    /// is at about 60%) and fading gradually to nothing, (1 - u)^2.5. No rim. One small bitmap
    /// instead of a gradient brush per colour, which Direct2D's software renderer kept adding to
    /// as the glow grew and shrank (`--hover-churn` shows it).
    fn glow_mask(&mut self) -> Option<ID2D1Bitmap> {
        if let Some(mask) = &self.glow_mask {
            return Some(mask.clone());
        }
        let mask = self.renderer.bitmap(&glow_shape(GLOW_MASK_SIDE)).ok()?;
        self.glow_mask = Some(mask.clone());
        Some(mask)
    }

    fn draw(&mut self) -> Result<()> {
        let g = self.layout.geo;
        // The glows this frame (behind the icons under the pointer, or pulsing after a click).
        let mut glows = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            let strength = self.glow_strength(item);
            if strength > 0.0 && !item.cfg.is_separator() && self.drag.as_ref().is_none_or(|drag| drag.child.is_some() || drag.index != index) {
                glows.push((index, strength, self.glow_colour(item)));
            }
        }
        let glow_mask = if glows.is_empty() { None } else { self.glow_mask() };
        self.renderer.begin(g.win_w, g.win_h)?;
        let rt = self.renderer.rt.clone();
        let brush = &self.brush;
        let no_stroke = None::<&ID2D1StrokeStyle>;
        unsafe {
            // Slide by drawing (only when another monitor sits above, see `slide_by_moving`):
            // everything moves up past the screen edge as `reveal` goes to 0.
            let content_offset = self.content_offset();
            rt.SetTransform(&translate(0.0, content_offset));

            let (left, right, bottom) = (g.panel_left, g.panel_left + g.panel_w, g.panel_h);
            // The top edge sits above the screen, so only the bottom corners show rounded.
            let top = -g.radius - g.shadow - 2.0;

            // Soft shadow: 1 px rings fading out, matching CrystalXP's ~38% falloff. The shadow,
            // fill and edge all follow the background opacity setting.
            let solid = self.settings.background_opacity as f32 / 100.0;
            let rings = if solid > 0.0 { g.shadow as i32 } else { 0 };
            for d in 1..=rings {
                let t = (d as f32 - 0.5) / g.shadow;
                brush.SetColor(&rgba(0.0, 0.0, 0.0, 0.38 * (1.0 - t) * (1.0 - t) * solid));
                let grow = d as f32 - 0.5;
                rt.DrawRoundedRectangle(
                    &rounded(left - grow, top, right + grow, bottom + grow, g.radius + grow),
                    brush,
                    1.0,
                    no_stroke,
                );
            }
            if let Some(fill) = &self.fill {
                rt.FillRoundedRectangle(&rounded(left, top, right, bottom, g.radius), fill);
            }
            let (edge, divider) = match self.settings.look {
                DockLook::Light => (0.72, 0.51),
                DockLook::Dark => (0.28, 0.30),
            };
            brush.SetColor(&rgba(1.0, 1.0, 1.0, edge * solid));
            rt.DrawRoundedRectangle(
                &rounded(left + 0.5, top, right - 0.5, bottom - 0.5, g.radius - 0.5),
                brush,
                1.0,
                no_stroke,
            );

            let inset = (4.0 * g.scale).round();
            let icon_opacity = self.settings.icon_opacity as f32 / 100.0;
            let mut icons = Vec::with_capacity(self.layout.slots.len());
            let dragged = self.drag.as_ref().filter(|drag| drag.child.is_none()).map(|drag| drag.index);
            let into = self.drag.as_ref().and_then(|drag| drag.into);
            for slot in &self.layout.slots {
                if Some(slot.item) == dragged {
                    continue; // it's under the pointer, in its own window
                }
                if slot.separator {
                    let x = (slot.center(&g) + self.items[slot.item].shift).floor();
                    brush.SetColor(&rgba(1.0, 1.0, 1.0, divider));
                    rt.FillRectangle(&rect(x, inset, x + 1.0, g.panel_h - inset), brush);
                } else {
                    icons.push(slot);
                }
            }
            // Glows go under every icon, so a glow never tints the icon beside it.
            let peak = match self.settings.look {
                DockLook::Light => 0.6,
                DockLook::Dark => 0.7,
            };
            if let Some(mask) = &glow_mask {
                // Drawing through a mask needs aliased mode (the mask's own edges are soft).
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_ALIASED);
                for &(index, strength, [r, gr, b]) in &glows {
                    let Some(slot) = self.layout.slot_of(index) else { continue };
                    let item = &self.items[index];
                    let (x, y, size) = layout::icon_rect(&g, slot, item.hover_t, item.bounce(g.bounce));
                    let (cx, cy) = (x + item.shake(6.0 * g.scale) + item.shift + size / 2.0, y + size / 2.0);
                    // Out to about a third of the icon past its edge; a pulse reaches a little further.
                    let radius = size * (0.85 + 0.12 * item.pulse());
                    brush.SetColor(&rgba(r, gr, b, peak * strength));
                    let area = D2D_RECT_F { left: cx - radius, top: cy - radius, right: cx + radius, bottom: cy + radius };
                    rt.FillOpacityMask(mask, brush, D2D1_OPACITY_MASK_CONTENT_GRAPHICS, Some(&area), None);
                }
                rt.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            }
            // Enlarged icons are drawn last, so they sit on top of their neighbours instead of
            // under the next icon along (the most enlarged on top of all).
            icons.sort_by(|a, b| self.items[a.item].hover_t.total_cmp(&self.items[b.item].hover_t));
            for slot in icons {
                let item = &self.items[slot.item];
                let (x, y, size) = layout::icon_rect(&g, slot, item.hover_t, item.bounce(g.bounce));
                let x = x + item.shake(6.0 * g.scale) + item.shift;
                let dest = rect(x, y, x + size, y + size);
                if into == Some(slot.item) {
                    // The group a dragged icon would go into lights up.
                    let m = 5.0 * g.scale;
                    brush.SetColor(&rgba(0.55, 0.78, 1.0, 0.55));
                    rt.FillRoundedRectangle(&rounded(x - m, y - m, x + size + m, y + size + m, size * 0.25), brush);
                }
                if item.shows_preview() {
                    let opacity = icon_opacity + (1.0 - icon_opacity) * item.hover_t;
                    draw_group_tile(&rt, brush, item, dest, opacity);
                    continue;
                }
                let bitmap = if size > g.icon + 0.5 { item.zoom.as_ref().or(item.base.as_ref()) } else { item.base.as_ref() };
                match bitmap {
                    Some(bitmap) => {
                        // The icon opacity setting, rising to solid as the icon grows under the
                        // pointer. Dimmed further: the program is missing and couldn't be repaired.
                        let opacity = icon_opacity + (1.0 - icon_opacity) * item.hover_t;
                        let dim = self.missing.contains(&slot.item) && !crate::breaks::broken("missing.not-dimmed");
                        let opacity = if dim { 0.4 * opacity } else { opacity };
                        rt.DrawBitmap(bitmap, Some(&dest), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None)
                    }
                    None => {
                        brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.35));
                        rt.FillRoundedRectangle(&rounded(x, y, x + size, y + size, size * 0.2), brush);
                    }
                }
            }

            // Under the hovered icon its name; under an icon something is dragged onto, what would
            // happen (shown even with labels off: it's the only sign of what a drop will do).
            let into_text = into.and_then(|g| self.items.get(g)).map(|item| format!("Move into {}", edit::label_of(&item.cfg)).encode_utf16().collect::<Vec<u16>>());
            let onto = self
                .incoming
                .as_ref()
                .and_then(|incoming| incoming.onto.map(|index| (index, &incoming.label)))
                .or(into.zip(into_text.as_ref()));
            let shown = match onto {
                Some((index, text)) => Some((index, text)),
                None => self
                    .hovered
                    .filter(|_| self.settings.show_labels || crate::breaks::broken("labels.always"))
                    .filter(|&index| !self.group.as_ref().is_some_and(|open| open.index == index)) // its pop-up names it
                    .and_then(|index| self.items.get(index).map(|item| (index, &item.label))),
            };
            if let (Some((index, text)), Some(format)) = (shown, &self.text_format) {
                if let Some(slot) = self.layout.slot_of(index) {
                    let item = &self.items[index];
                    let opacity = item.hover_t;
                    let (_, y, size) = layout::icon_rect(&g, slot, item.hover_t, item.bounce(g.bounce));
                    let cx = slot.center(&g);
                    let text_top = (y + size + 3.0 * g.scale).round();
                    let area = rect(cx - 200.0, text_top, cx + 200.0, text_top + g.label_h);
                    // A thin dark outline keeps white text readable on any wallpaper.
                    brush.SetColor(&rgba(0.0, 0.0, 0.0, 0.55 * opacity));
                    for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                        let shifted = rect(area.left + dx, area.top + dy, area.right + dx, area.bottom + dy);
                        rt.DrawText(text, format, &shifted, brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                    }
                    brush.SetColor(&rgba(1.0, 1.0, 1.0, opacity));
                    rt.DrawText(text, format, &area, brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
            if let (Some(strip), Some(format)) = (&self.strip, &self.text_format) {
                let radius = 6.0 * g.scale;
                brush.SetColor(&rgba(0.08, 0.08, 0.08, 0.84));
                rt.FillRoundedRectangle(&D2D1_ROUNDED_RECT { rect: strip.rect, radiusX: radius, radiusY: radius }, brush);
                let middle = |r: D2D_RECT_F| {
                    let top = ((r.top + r.bottom - g.label_h) / 2.0).round();
                    rect(r.left, top, r.right, top + g.label_h)
                };
                brush.SetColor(&rgba(1.0, 1.0, 1.0, 1.0));
                rt.DrawText(&strip.text, format, &middle(strip.text_rect), brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                if let Some(undo_rect) = strip.undo_rect {
                    brush.SetColor(&rgba(0.52, 0.72, 0.92, 1.0));
                    let undo: Vec<u16> = "Undo".encode_utf16().collect();
                    rt.DrawText(&undo, format, &middle(undo_rect), brush, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
                }
            }
            rt.SetTransform(&translate(0.0, 0.0));
        }
        self.renderer.end()
    }
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

/// A group without an icon of its own: a rounded tile with its first four items, small.
fn draw_group_tile(rt: &ID2D1DCRenderTarget, brush: &ID2D1SolidColorBrush, item: &Item, area: D2D_RECT_F, opacity: f32) {
    let size = area.right - area.left;
    let radius = size * 0.22;
    let margin = size * 0.13;
    let gap = size * 0.07;
    let cell = (size - 2.0 * margin - gap) / 2.0;
    unsafe {
        brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.30 * opacity));
        rt.FillRoundedRectangle(&rounded(area.left, area.top, area.right, area.bottom, radius), brush);
        brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.70 * opacity));
        rt.DrawRoundedRectangle(&rounded(area.left + 0.5, area.top + 0.5, area.right - 0.5, area.bottom - 0.5, radius), brush, 1.0, None);
        for (k, child) in item.children.iter().filter(|child| !child.cfg.is_separator()).take(4).enumerate() {
            let left = area.left + margin + (k % 2) as f32 * (cell + gap);
            let top = area.top + margin + (k / 2) as f32 * (cell + gap);
            let dest = rect(left, top, left + cell, top + cell);
            match child.preview.as_ref().or(child.icon.as_ref()) {
                Some(bitmap) => rt.DrawBitmap(bitmap, Some(&dest), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                None => {
                    brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.35 * opacity));
                    rt.FillRoundedRectangle(&rounded(left, top, left + cell, top + cell, cell * 0.2), brush);
                }
            }
        }
    }
}

/// Room around the drag image for the remove badge, which sits over the icon's corner.
fn drag_image_pad(size: f32) -> f32 {
    (size * 0.16).ceil()
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F { left, top, right, bottom }
}

fn rounded(left: f32, top: f32, right: f32, bottom: f32, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: rect(left, top, right, bottom), radiusX: radius, radiusY: radius }
}

fn translate(x: f32, y: f32) -> Matrix3x2 {
    Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: x, M32: y }
}

fn point_from_lparam(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as u32;
    ((v & 0xFFFF) as i16 as i32, (v >> 16) as i16 as i32)
}

/// A copy of the item with its paths resolved against the dock folder, for its icon (a bare name
/// is the program Windows would start).
fn resolved(home: &Home, cfg: &ItemConfig) -> ItemConfig {
    let mut item = cfg.clone();
    item.icon = home.resolve(&cfg.icon);
    item.target = home.resolve_for_icon(cfg.launch_target());
    item
}

fn with_dock<R>(f: impl FnOnce(&mut Dock) -> R) -> Option<R> {
    DOCK.with(|cell| cell.try_borrow_mut().ok().and_then(|mut dock| dock.as_mut().map(f)))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // While a menu is open, the dock is already borrowed; nested messages get default handling.
    let (result, action) = with_dock(|dock| dock.handle(msg, wparam, lparam)).unwrap_or((None, None));
    if (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&msg) || msg == WM_CAPTURECHANGED {
        with_dock(|dock| dock.sync_capture());
    }
    run_action(hwnd, action);
    result.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

/// An open group's pop-up: its mouse messages go to the dock, which draws it.
unsafe extern "system" fn popup_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_MOUSEACTIVATE {
        return LRESULT(MA_NOACTIVATE as isize); // never takes the focus, even between groups
    }
    let (result, action) = with_dock(|dock| dock.handle_popup(msg, lparam)).unwrap_or((None, None));
    if (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&msg) || msg == WM_CAPTURECHANGED {
        with_dock(|dock| dock.sync_capture());
    }
    if let Some(dock) = with_dock(|dock| dock.hwnd).flatten() {
        run_action(dock, action);
    }
    result.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

/// What a click asked for, done outside the dock's borrow. `hwnd`: the dock's window.
fn run_action(hwnd: HWND, action: Option<Action>) {
    match action {
        Some(Action::Launch(item, index, generation)) => {
            if let Some(home) = with_dock(|dock| dock.home.clone()) {
                let notify = launch::Notify::new(hwnd, WM_APP_HEAL, WM_APP_LAUNCH_FAILED, index, generation);
                launch::launch(&item, &home, Some(notify));
            }
        }
        Some(Action::Menu(pt, item)) => show_menu(hwnd, pt, item),
        Some(Action::ChildMenu(pt, group, child)) => show_child_menu(hwnd, pt, group, child),
        Some(Action::Error(message)) => show_error(&message),
        Some(Action::FileError(message)) => system::warning(message),
        Some(Action::ConfirmReplace(index, label, new)) => confirm_replace(index, &label, &new),
        Some(Action::TestModal(seconds)) => {
            log_info!("TEST a message loop of its own for {seconds} s, as an open menu has");
            let until = Instant::now() + Duration::from_secs(seconds);
            let mut msg = MSG::default();
            unsafe {
                while Instant::now() < until {
                    let _ = MsgWaitForMultipleObjectsEx(None, 100, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        if msg.message == WM_QUIT {
                            PostQuitMessage(msg.wParam.0 as i32);
                            return;
                        }
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
            log_info!("TEST message loop done");
        }
        None => {}
    }
}

/// Right-click on an item in a group's pop-up: take it out onto the dock, remove it, or open its
/// properties.
fn show_child_menu(hwnd: HWND, pt: POINT, group: usize, child: usize) {
    let facts = with_dock(|dock| {
        let item = dock.items.get(group)?;
        let inner = item.children.get(child)?;
        dock.menu_open = true;
        Some((edit::label_of(&inner.cfg), edit::label_of(&item.cfg), dock.settings.locked, dock.mode))
    })
    .flatten();
    let Some((label, group_label, locked, mode)) = facts else { return };
    let choice = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            with_dock(|dock| dock.menu_open = false);
            return;
        };
        // Nothing to offer that could only fail: say why instead.
        let why = match mode {
            Mode::SafeStart => Some(w!("dock.toml has an error (right-click the dock to restore it)")),
            Mode::ReadOnly => Some(w!("dock.toml is from a newer Desktop Dock: it's used as it is")),
            Mode::Normal if locked => Some(w!("Icons are locked (right-click the dock to unlock)")),
            Mode::Normal => None,
        };
        match why {
            Some(why) => {
                let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, why);
            }
            None => {
                let _ = AppendMenuW(menu, MF_STRING, CHILD_TAKE_OUT, &HSTRING::from(format!("Take {} out of {}", label, group_label).replace('&', "&&")));
                let _ = AppendMenuW(menu, MF_STRING, CHILD_REMOVE, &HSTRING::from(format!("Remove {label}").replace('&', "&&")));
            }
        }
        if mode == Mode::Normal {
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, CHILD_PROPERTIES, w!("Properties..."));
        }
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD.0 | TPM_RIGHTBUTTON.0 | TPM_TOPALIGN.0 | TPM_LEFTALIGN.0;
        let choice = TrackPopupMenuEx(menu, flags, pt.x, pt.y, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        choice.0 as usize
    };
    with_dock(|dock| dock.menu_open = false);
    if let Some(name) = child_menu_name(choice) {
        log_info!("menu: {name} ({label} in {group_label})");
    }
    let result = match choice {
        CHILD_TAKE_OUT => with_dock(|dock| dock.take_out(group, child, &label, &group_label)),
        CHILD_REMOVE => with_dock(|dock| dock.remove_child(group, child, &label, &group_label)),
        CHILD_PROPERTIES => {
            if let Some(home) = with_dock(|dock| dock.home.clone()) {
                open_window(&home, &["--properties".into(), group.to_string(), "--child".into(), child.to_string()]);
            }
            None
        }
        _ => None,
    };
    if let Some(Err(message)) = result {
        show_error(&message);
    }
}

/// The right-click menu. Put back comes first (harmless); item actions appear only
/// for the item that was right-clicked, never first under the pointer: in particular "Empty
/// Recycle Bin" only on the Recycle Bin.
fn show_menu(hwnd: HWND, pt: POINT, item: Option<usize>) {
    struct Facts {
        safe_start: bool,
        /// Changes can be saved (not in safe start, nor with a newer version's dock.toml): the
        /// menu offers no edit that could only fail.
        editable: bool,
        on_bin: bool,
        /// The right-clicked item: its index and label.
        target: Option<(usize, String)>,
        /// Recently removed, newest first: id and menu text.
        removed: Vec<(String, String)>,
        locked: bool,
        /// The right-clicked item's kind.
        kind: Option<Kind>,
        has_bin: bool,
        count: usize,
        /// The groups on the dock: index and name.
        groups: Vec<(usize, String)>,
        /// The right-clicked item's file or folder, if it's one that's there (Open file location).
        location: Option<String>,
    }
    let facts = with_dock(|dock| {
        dock.menu_open = true;
        let picked = item.and_then(|i| dock.items.get(i).map(|it| (i, it)));
        let today = removed_date_today();
        Facts {
            safe_start: dock.mode == Mode::SafeStart,
            editable: dock.mode == Mode::Normal,
            locked: dock.settings.locked,
            kind: picked.map(|(_, it)| it.cfg.kind),
            has_bin: dock.items.iter().any(|it| it.cfg.kind == Kind::RecycleBin),
            groups: dock.items.iter().enumerate().filter(|(_, it)| it.cfg.kind == Kind::Group).map(|(i, it)| (i, edit::label_of(&it.cfg))).collect(),
            // On a server, it isn't asked (one switched off kept the dock waiting 15 s): there
            // unless the dock's own look, in the background, found it missing.
            location: picked
                .filter(|(_, it)| it.cfg.kind == Kind::App)
                .map(|(i, it)| (i, places::on_disk(&dock.home, it.cfg.launch_target())))
                .filter(|(i, path)| if places::on_network(path) { !dock.missing.contains(i) } else { std::path::Path::new(path).exists() })
                .map(|(_, path)| path),
            count: dock.items.len(),
            on_bin: picked.is_some_and(|(_, it)| it.cfg.kind == Kind::RecycleBin),
            target: picked.map(|(i, it)| (i, edit::label_of(&it.cfg))),
            removed: dock
                .removed
                .as_ref()
                .map(|r| r.list())
                .unwrap_or_default()
                .into_iter()
                .take(PUT_BACK_SHOWN)
                .map(|entry| {
                    let when = match entry.removed_at.split_once(' ') {
                        Some((date, time)) if date == today => time.get(..5).unwrap_or(time).to_string(),
                        Some((date, _)) => date.to_string(),
                        None => entry.removed_at.clone(),
                    };
                    (entry.removed_at, format!("{}\t{when}", entry.label))
                })
                .collect(),
        }
    });
    let Some(facts) = facts else { return };
    let places = places::all();
    let choice = unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            with_dock(|dock| dock.menu_open = false); // or the dock would never hide again
            return;
        };
        // `open`: the current section has entries, so the next section starts with a line.
        let mut open = false;
        let item = |flags, id: usize, text: &str, open: &mut bool| {
            let _ = AppendMenuW(menu, flags, id, &HSTRING::from(text.replace('&', "&&"))); // & would underline
            *open = true;
        };
        let separate = |open: &mut bool| {
            if *open {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                *open = false;
            }
        };
        if facts.safe_start {
            item(MF_STRING, MENU_RESTORE, "Restore last working version", &mut open);
        }
        // The right-clicked item. (Changing the dock: not while locked, a locked dock doesn't change;
        // not at all when changes can't be saved.)
        let changeable = facts.editable && !facts.locked;
        separate(&mut open);
        if facts.on_bin {
            item(MF_STRING, MENU_EMPTY_BIN, "Empty Recycle Bin", &mut open);
        }
        if facts.location.is_some() {
            item(MF_STRING, MENU_OPEN_LOCATION, "Open file location", &mut open);
        }
        if changeable && facts.kind.is_some_and(|kind| kind != Kind::Separator) {
            item(MF_STRING, MENU_ICON_CHOOSE, "Change icon...", &mut open);
        }
        // Groups: put an item in one (or make a new one with it), or ungroup a group.
        if changeable && matches!(facts.kind, Some(Kind::App | Kind::RecycleBin)) {
            if let Ok(sub) = CreatePopupMenu() {
                for (k, (_, name)) in facts.groups.iter().enumerate() {
                    let _ = AppendMenuW(sub, MF_STRING, MENU_TO_GROUP + k, &HSTRING::from(name.replace('&', "&&")));
                }
                if !facts.groups.is_empty() {
                    let _ = AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null());
                }
                let _ = AppendMenuW(sub, MF_STRING, MENU_NEW_GROUP, w!("New group"));
                item(MF_POPUP, sub.0 as usize, "Move to group", &mut open);
            }
        }
        if changeable && facts.kind == Some(Kind::Group) {
            item(MF_STRING, MENU_UNGROUP, "Ungroup", &mut open);
        }
        if facts.editable && facts.kind.is_some_and(|kind| kind != Kind::Separator) {
            item(MF_STRING, MENU_PROPERTIES, "Properties...", &mut open);
        }
        if let (Some((_, label)), true) = (&facts.target, facts.editable && (!facts.locked || crate::breaks::broken("menu.lock-keeps-remove"))) {
            item(MF_STRING, MENU_REMOVE, &format!("Remove {label}"), &mut open);
        }
        // Bringing things onto the dock: new ones, or ones taken off (Put back: also while locked,
        // as it only undoes a removal).
        separate(&mut open);
        if changeable {
            if let Ok(sub) = CreatePopupMenu() {
                let _ = AppendMenuW(sub, MF_STRING, MENU_ADD_FILES, w!("File..."));
                let _ = AppendMenuW(sub, MF_STRING, MENU_ADD_FOLDER, w!("Folder..."));
                let _ = AppendMenuW(sub, MF_STRING, MENU_ADD_SEPARATOR, w!("Separator"));
                if !facts.has_bin {
                    let _ = AppendMenuW(sub, MF_STRING, MENU_ADD_BIN, w!("Recycle Bin"));
                }
                if let Ok(windows) = CreatePopupMenu() {
                    for (k, place) in places.iter().enumerate() {
                        match place {
                            Some(place) => {
                                let _ = AppendMenuW(windows, MF_STRING, MENU_ADD_WINDOWS + k, &HSTRING::from(place.name.as_str()));
                            }
                            None => {
                                let _ = AppendMenuW(windows, MF_SEPARATOR, 0, PCWSTR::null());
                            }
                        }
                    }
                    let _ = AppendMenuW(sub, MF_POPUP, windows.0 as usize, w!("Windows item"));
                }
                item(MF_POPUP, sub.0 as usize, "Add", &mut open);
            }
        }
        if facts.editable && !facts.removed.is_empty() {
            if let Ok(sub) = CreatePopupMenu() {
                for (k, (_, text)) in facts.removed.iter().enumerate() {
                    let _ = AppendMenuW(sub, MF_STRING, MENU_PUT_BACK + k, &HSTRING::from(text.replace('&', "&&")));
                }
                item(MF_POPUP, sub.0 as usize, "Put back", &mut open);
            }
        }
        // The dock itself. Everything else (its file, folder, Start with Windows) is in its settings.
        separate(&mut open);
        if facts.editable {
            let lock_flags = if facts.locked { MF_STRING | MF_CHECKED } else { MF_STRING };
            item(lock_flags, MENU_LOCK, "Lock icons", &mut open);
        }
        item(MF_STRING, MENU_SETTINGS, "Dock settings...", &mut open);
        separate(&mut open);
        item(MF_STRING, MENU_EXIT, "Exit Desktop Dock", &mut open);
        // Required for the menu to close when clicking elsewhere (documented TrackPopupMenu quirk).
        let _ = SetForegroundWindow(hwnd);
        let flags = TPM_RETURNCMD.0 | TPM_RIGHTBUTTON.0 | TPM_TOPALIGN.0 | TPM_LEFTALIGN.0;
        let choice = TrackPopupMenuEx(menu, flags, pt.x, pt.y, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu); // and its submenus with it
        choice.0 as usize
    };
    with_dock(|dock| dock.menu_open = false);
    if let Some(name) = menu_name(choice) {
        log_info!("menu: {name}"); // a breadcrumb for anything that happens next
    }
    let result = match choice {
        MENU_REMOVE => facts.target.as_ref().and_then(|(index, label)| with_dock(|dock| dock.remove_item(*index, label))),
        MENU_LOCK => with_dock(|dock| dock.set_locked(!facts.locked)),
        MENU_NEW_GROUP => facts.target.as_ref().and_then(|(index, label)| with_dock(|dock| dock.new_group(*index, label))),
        MENU_UNGROUP => facts.target.as_ref().and_then(|(index, label)| with_dock(|dock| dock.ungroup(*index, label))),
        _ if choice >= MENU_TO_GROUP && choice < MENU_TO_GROUP + facts.groups.len() => {
            let (group, group_label) = facts.groups[choice - MENU_TO_GROUP].clone();
            facts.target.as_ref().and_then(|(index, label)| with_dock(|dock| dock.move_into_group(*index, label, group, &group_label)))
        }
        _ if (MENU_ADD_WINDOWS..MENU_ADD_WINDOWS + places.len()).contains(&choice) => {
            // In after the right-clicked item, or at the end.
            let gap = facts.target.as_ref().map_or(facts.count, |(index, _)| index + 1);
            let place = places[choice - MENU_ADD_WINDOWS].clone();
            place.and_then(|place| with_dock(|dock| dock.add_items(gap, &[place])))
        }
        MENU_ADD_FILES | MENU_ADD_FOLDER | MENU_ADD_SEPARATOR | MENU_ADD_BIN => {
            // In after the right-clicked item, or at the end.
            let gap = facts.target.as_ref().map_or(facts.count, |(index, _)| index + 1);
            with_dock(|dock| match choice {
                MENU_ADD_FILES => dock.start_pick(Pick::Files, String::new(), Purpose::Add(gap)),
                MENU_ADD_FOLDER => dock.start_pick(Pick::Folder, String::new(), Purpose::Add(gap)),
                MENU_ADD_SEPARATOR => {
                    let mut separator = ItemConfig::default();
                    separator.kind = Kind::Separator;
                    dock.add_items(gap, &[separator])
                }
                _ => {
                    let mut bin = ItemConfig::new("Recycle Bin", "");
                    bin.kind = Kind::RecycleBin;
                    dock.add_items(gap, &[bin])
                }
            })
        }
        _ if (MENU_PUT_BACK..MENU_PUT_BACK + PUT_BACK_SHOWN).contains(&choice) => {
            let id = facts.removed.get(choice - MENU_PUT_BACK).map(|(id, _)| id.clone());
            id.and_then(|id| with_dock(|dock| dock.put_back(&id)))
        }
        _ => None,
    };
    if let Some(Err(message)) = result {
        show_error(&message);
    }
    match choice {
        MENU_RESTORE => {
            if let Some(Err(message)) = with_dock(|dock| dock.restore_last_good()) {
                show_error(&message);
            }
        }
        MENU_EMPTY_BIN => {
            // Windows shows its usual confirmation. Afterwards, re-check so the icon updates.
            let hwnd_value = hwnd.0 as isize;
            std::thread::spawn(move || unsafe {
                match launch::empty_recycle_bin() {
                    Ok(()) => log_info!("Recycle Bin emptied"),
                    // No to Windows' question, or nothing in it.
                    Err(e) => log_info!("Recycle Bin not emptied ({})", e.message()),
                }
                let _ = PostMessageW(Some(HWND(hwnd_value as *mut std::ffi::c_void)), WM_APP_RECHECK_BIN, WPARAM(0), LPARAM(0));
            });
        }
        MENU_OPEN_LOCATION => {
            if let Some(path) = facts.location.as_deref() {
                match system::open_location(path) {
                    Ok(()) => log_info!("opened the file location of {path}"),
                    Err(message) => show_error(&message),
                }
            }
        }
        MENU_SETTINGS | MENU_PROPERTIES | MENU_ICON_CHOOSE => {
            // The icon picker is in the settings process (its own, like the windows).
            let open = match (choice, &facts.target) {
                (MENU_PROPERTIES, Some((index, _))) => vec!["--properties".to_string(), index.to_string()],
                (MENU_ICON_CHOOSE, Some((index, _))) => vec!["--properties".to_string(), index.to_string(), "--icon".to_string()],
                _ => vec!["--settings".to_string()],
            };
            if let Some(home) = with_dock(|dock| dock.home.clone()) {
                open_window(&home, &open);
            }
        }
        MENU_EXIT => unsafe { PostQuitMessage(0) },
        _ => {}
    }
}

/// Today's date as Recently removed writes it, to show today's removals by time only.
fn removed_date_today() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}
/// The group pop-up's menu (`show_child_menu`).
const CHILD_TAKE_OUT: usize = 1;
const CHILD_REMOVE: usize = 2;
const CHILD_PROPERTIES: usize = 3;

/// What the group pop-up's menu entry is called, for the log (and the feature inventory).
pub(crate) fn child_menu_name(choice: usize) -> Option<&'static str> {
    Some(match choice {
        CHILD_TAKE_OUT => "Take out",
        CHILD_REMOVE => "Remove",
        CHILD_PROPERTIES => "Properties",
        _ => return None,
    })
}

/// What the dock's menu entry is called, for the log (and the feature inventory).
pub(crate) fn menu_name(choice: usize) -> Option<&'static str> {
    Some(match choice {
        MENU_EXIT => "Exit",
        MENU_RESTORE => "Restore last working version",
        MENU_EMPTY_BIN => "Empty Recycle Bin",
        MENU_REMOVE => "Remove",
        MENU_LOCK => "Lock icons",
        MENU_ICON_CHOOSE => "Change icon",
        MENU_ADD_FILES => "Add file",
        MENU_ADD_FOLDER => "Add folder",
        MENU_ADD_SEPARATOR => "Add separator",
        MENU_ADD_BIN => "Add Recycle Bin",
        _ if (MENU_ADD_WINDOWS..MENU_ADD_WINDOWS + 100).contains(&choice) => "Add Windows item",
        MENU_SETTINGS => "Dock settings",
        MENU_PROPERTIES => "Properties",
        MENU_NEW_GROUP => "New group",
        MENU_UNGROUP => "Ungroup",
        MENU_OPEN_LOCATION => "Open file location",
        _ if choice >= MENU_TO_GROUP => "Move to group",
        _ if (MENU_PUT_BACK..MENU_PUT_BACK + PUT_BACK_SHOWN).contains(&choice) => "Put back",
        _ => return None,
    })
}

/// Opens the settings window or an item's properties window (each its own process, see
/// settings.rs). If one is already open, that one comes forward instead.
fn open_window(home: &Home, which: &[String]) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut command = std::process::Command::new(exe);
    command.args(which).arg("--home").arg(&home.dir);
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY); // so the window can come to the front
    }
    if let Err(e) = command.spawn() {
        log_warn!("couldn't open {}: {e}", which.join(" "));
        show_error(&format!("Couldn't open Dock settings: {e}"));
    }
}

/// The one question the dock asks about a drop: replace a program with the one dropped on it
/// (an update that left a new desktop shortcut, say). Asked after the drop has finished, so
/// Explorer isn't waiting.
fn confirm_replace(index: usize, label: &str, new: &ItemConfig) {
    let text = format!(
        "Replace {label} with {}?\n\n{}\n\nIt keeps its place, name and icon. You can undo this on the dock for a few seconds.",
        edit::label_of(new),
        new.target
    );
    let answer = system::message_box(&text, "Desktop Dock", MB_YESNO | MB_ICONQUESTION | MB_SETFOREGROUND | MB_TOPMOST);
    if answer != IDYES {
        log_info!("kept {label} (replace declined)");
        return;
    }
    if let Some(Err(message)) = with_dock(|dock| dock.replace_program(index, label, new)) {
        show_error(&message);
    }
}
/// Reports a problem without changing anything (the dock keeps its current setup) and without
/// blocking the dock.
fn show_error(message: &str) {
    system::warning(message.to_string());
}

/// Windows' drag and drop, for the dock and its drop zone. The shell work of reading what was
/// dropped happens outside the dock's borrow.
fn on_drop(event: drop::Event, from_zone: bool) -> bool {
    match event {
        drop::Event::Enter(_, pt) => {
            let accepted = with_dock(|dock| dock.drop_enter(pt, from_zone)).unwrap_or(false);
            log_info!("a drag arrived over the {} ({})", if from_zone { "drop zone" } else { "dock" }, if accepted { "welcome" } else { "not here" });
            accepted
        }
        drop::Event::Over(pt) => with_dock(|dock| dock.drop_over(pt)).unwrap_or(false),
        drop::Event::Leave => {
            with_dock(|dock| dock.drop_leave());
            log_info!("the drag left the {}", if from_zone { "drop zone" } else { "dock" });
            false
        }
        drop::Event::Drop(data, _) => {
            let Some(home) = with_dock(|dock| dock.home.clone()) else { return false };
            let dropped = drop::read(data, &home);
            log_info!("dropped on the {}: {} item(s), {} file(s)", if from_zone { "drop zone" } else { "dock" }, dropped.items.len(), dropped.paths.len());
            match with_dock(|dock| dock.drop_items(dropped)) {
                Some(Ok(added)) => added,
                Some(Err(message)) => {
                    show_error(&message);
                    false
                }
                None => false,
            }
        }
    }
}

/// The drop zone: a thin strip along the top edge, all but invisible (alpha 1 of 255, which
/// still counts for drag and drop), shown only while a button is held near the edge.
fn create_drop_zone() -> Result<HWND> {
    unsafe extern "system" fn plain(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("DesktopDockDropZone");
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(plain),
            hInstance: instance.into(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(Error::from_thread());
        }
        let zone = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name,
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        SetLayeredWindowAttributes(zone, COLORREF(0), 1, LWA_ALPHA)?;
        Ok(zone)
    }
}
/// The small window that carries a dragged icon under the pointer: layered, click-through (so
/// the dock still gets the mouse), never activated. Made hidden at start and reused.
/// The window an open group's pop-up shows in: layered, never activated, owned by the dock.
fn create_popup_window(owner: HWND) -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("DesktopDockGroup");
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(popup_wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(Error::from_thread());
        }
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name,
            w!("Desktop Dock group"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            Some(owner),
            None,
            Some(instance.into()),
            None,
        )
    }
}

fn create_drag_window() -> Result<HWND> {
    unsafe extern "system" fn plain(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("DesktopDockDragImage");
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(plain),
            hInstance: instance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(Error::from_thread());
        }
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name,
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}
/// The dock's window: layered (per-pixel transparency), always on top, never takes focus,
/// no taskbar button. Created hidden.
fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("DesktopDock");
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: class_name,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(Error::from_thread());
        }
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name,
            w!("Desktop Dock"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

pub fn run(opened: store::Opened, store: Store, home: Home, started: Instant, safe_mode: bool, first_run: bool) -> Result<()> {
    unsafe {
        let hwnd = create_window()?;

        // A file that couldn't be read as the dock started is usually held for a moment by
        // another program: its notice waits, and is never shown if the file frees up soon.
        let mut held_notice = None;
        match opened.notice {
            Some(notice) if opened.unreadable => held_notice = Some(notice),
            Some(notice) => system::notice(notice),
            None => {}
        }
        if system::is_elevated() {
            log_warn!("running as administrator");
            system::notice(
                "Desktop Dock is running as administrator, so everything it opens runs as administrator \
                 too, and Windows won't let you drag files onto it.\n\nStart it the normal way (not \
                 \"Run as administrator\") to avoid this."
                    .into(),
            );
        }
        let mut dock = Dock::new(Some(hwnd), opened.parsed.config, home, Some(store), opened.mode)?;
        if opened.unreadable {
            dock.watch = ChangeWatch::new(None); // read it as soon as it can be
            dock.unreadable_since = Some(started);
            dock.start_notice = held_notice;
            dock.start_timer(TIMER_READ_RETRY, READ_RETRY_MS);
        }
        dock.safe_mode = safe_mode;
        if first_run {
            dock.start_timer(TIMER_WELCOME, WELCOME_AFTER_MS);
        }
        dock.apply_hotkey();
        dock.removed = Some(Removed::new(&dock.home));
        dock.drag_window = create_drag_window().inspect_err(|e| log_warn!("no drag window: {}", e.message())).ok();
        dock.popup_window = create_popup_window(hwnd).inspect_err(|e| log_warn!("no group window: {}", e.message())).ok();
        // Dropping files, apps and links onto the dock (and onto the drop zone, while it's hidden).
        if let Err(e) = OleInitialize(None) {
            log_warn!("drag and drop unavailable: {}", e.message());
        } else {
            if let Err(e) = RegisterDragDrop(hwnd, &drop::DropTarget::create(false, on_drop)) {
                log_warn!("the dock can't take drops: {}", e.message());
            }
            dock.drop_zone = create_drop_zone().inspect_err(|e| log_warn!("no drop zone: {}", e.message())).ok();
            if let Some(zone) = dock.drop_zone {
                if let Err(e) = RegisterDragDrop(zone, &drop::DropTarget::create(true, on_drop)) {
                    log_warn!("the drop zone can't take drops: {}", e.message());
                }
            }
        }
        dock.reduced_motion = !system::animations_enabled();
        dock.taskbar_created = system::taskbar_created_message();
        dock.folder_watch = system::watch_folder(&dock.home.dir);
        dock.start_timer(TIMER_HEALTH, HEALTH_EVERY_MS);
        dock.icons = Some(IconWorker::start(&dock.home, hwnd, WM_APP_ICONS, safe_mode));
        dock.relayout();
        dock.check_recycle_bin();
        dock.start_heal();
        // Start shown for a moment so it's obvious the dock is running, then tuck away.
        dock.reveal = 1.0;
        dock.reveal_target = 1.0;
        dock.linger_until = Some(Instant::now() + LINGER_AT_START);
        dock.render();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        log_info!(
            "ready: {} items shown {} ms after start (icons arrive as they load)",
            dock.items.len(),
            started.elapsed().as_millis()
        );
        dock.started = Some(started);
        dock.window_visible = true;
        dock.set_poll(50);
        let folder_watch = dock.folder_watch;
        DOCK.with(|cell| *cell.borrow_mut() = Some(dock));
        // Lock/unlock and display on/off messages (pause polling while nobody can see the dock).
        system::watch_presence(hwnd);
        // Started while locked (the watchdog restarted it, say): no lock message will come.
        with_dock(|dock| {
            dock.presence.correct(system::session_state(), false);
            dock.apply_presence();
        });

        let drop_zone = with_dock(|dock| dock.drop_zone).flatten();
        message_loop(folder_watch);
        let _ = RevokeDragDrop(hwnd);
        if let Some(zone) = drop_zone {
            let _ = RevokeDragDrop(zone);
        }
        system::unwatch_presence(hwnd);
        if let Some(handle) = folder_watch {
            let _ = FindCloseChangeNotification(handle);
        }
        DOCK.with(|cell| cell.borrow_mut().take());
    }
    Ok(())
}

/// Sleeps when nothing moves (zero CPU) until a message arrives or a file in the dock folder
/// changes. While animating, renders one frame per screen refresh: DwmFlush waits for the
/// compositor, so animation runs at the monitor's rate.
fn message_loop(mut folder_watch: Option<HANDLE>) {
    let mut msg = MSG::default();
    loop {
        let animating = with_dock(|dock| dock.needs_frames()).unwrap_or(false);
        unsafe {
            if animating {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let started = Instant::now();
                with_dock(|dock| dock.frame());
                if DwmFlush().is_err() || started.elapsed() < Duration::from_millis(2) {
                    std::thread::sleep(Duration::from_millis(4)); // never spin
                }
            } else if let Some(folder) = folder_watch {
                let woke = MsgWaitForMultipleObjectsEx(Some(&[folder]), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                if woke == WAIT_OBJECT_0 {
                    // Something in the dock folder changed. Re-arm the signal, then look at
                    // dock.toml once things are quiet (one save can signal several times).
                    if FindNextChangeNotification(folder).is_err() {
                        log_warn!("stopped watching the dock folder; checking dock.toml every second instead");
                        folder_watch = None;
                        with_dock(|dock| dock.folder_watch = None);
                    }
                    with_dock(|dock| dock.start_timer(TIMER_CONFIG, CONFIG_SETTLE_MS));
                    continue;
                }
                if woke != WAIT_OBJECT_0_PLUS_1 {
                    log_warn!("waiting failed; checking dock.toml every second instead");
                    folder_watch = None; // never spin on a broken wait
                    with_dock(|dock| dock.folder_watch = None);
                }
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        return;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            } else {
                if GetMessageW(&mut msg, None, 0, 0).0 <= 0 {
                    return;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// MsgWaitForMultipleObjectsEx's answer when messages (not the one handle) woke it.
const WAIT_OBJECT_0_PLUS_1: windows::Win32::Foundation::WAIT_EVENT = windows::Win32::Foundation::WAIT_EVENT(WAIT_OBJECT_0.0 + 1);

/// Measures frame cost (`--bench`) without anything appearing on screen: renders a hover sweep
/// across every icon and a full slide into a window that's never shown, and times each frame.
pub fn bench(config: Config, home: Home) -> Result<String> {
    let hwnd = create_window()?;
    let mut dock = Dock::new(Some(hwnd), config, home, None, Mode::Normal)?;
    dock.relayout(); // no icon worker here, so icons load directly
    let zoom_px = dock.layout.geo.hover_icon as u32;
    if let Ok(loader) = IconLoader::new(Some(dock.home.icon_cache())) {
        for item in dock.items.iter_mut().filter(|item| !item.cfg.is_separator()) {
            item.zoom = loader.load(&item.sources, zoom_px).and_then(|p| dock.renderer.bitmap(&p).ok());
        }
    }
    let icons: Vec<usize> = (0..dock.items.len()).filter(|&i| !dock.items[i].cfg.is_separator()).collect();
    let frame = |dock: &mut Dock| -> (f64, f64) {
        let t0 = Instant::now();
        let _ = dock.draw();
        let t1 = Instant::now();
        let _ = dock.renderer.present(hwnd, dock.win_x, dock.monitor.top - 2000); // off screen, never shown
        (t1.duration_since(t0).as_secs_f64() * 1000.0, t1.elapsed().as_secs_f64() * 1000.0)
    };
    let summarize = |name: &str, samples: &[(f64, f64)]| {
        let n = samples.len() as f64;
        let draw = samples.iter().map(|s| s.0).sum::<f64>() / n;
        let present = samples.iter().map(|s| s.1).sum::<f64>() / n;
        let worst = samples.iter().map(|s| s.0 + s.1).fold(0.0, f64::max);
        let total = draw + present;
        format!(
            "{name}: {:.2} ms/frame (draw {draw:.2} + show {present:.2}), worst {worst:.2} ms; at 120 Hz ≈ {:.0}% of one core while it animates",
            total,
            total * 120.0 / 10.0
        )
    };

    dock.reveal = 1.0;
    dock.reveal_target = 1.0;
    let mut hover = Vec::new();
    for f in 0..360 {
        let index = icons[f % icons.len()];
        for item in &mut dock.items {
            item.hover_t = 0.0;
        }
        dock.hovered = Some(index);
        dock.items[index].hover_t = 0.6;
        hover.push(frame(&mut dock));
    }
    dock.hovered = None;
    for item in &mut dock.items {
        item.hover_t = 0.0;
    }
    let mut slide = Vec::new();
    dock.slide_by_moving = false;
    for f in 0..=120 {
        dock.reveal = f as f32 / 120.0;
        slide.push(frame(&mut dock));
    }
    let mut moves = Vec::new();
    dock.slide_by_moving = true;
    for f in 0..=120 {
        dock.reveal = f as f32 / 120.0;
        let t0 = Instant::now();
        dock.move_window();
        moves.push((t0.elapsed().as_secs_f64() * 1000.0, 0.0));
    }
    let g = dock.layout.geo;
    let result = format!(
        "bench: {} items, window {}x{} px, icon {} px, zoom {} px\n  {}\n  {}\n  {}",
        dock.items.len(),
        g.win_w,
        g.win_h,
        g.icon,
        g.hover_icon,
        summarize("hover sweep", &hover),
        summarize("slide, redrawing", &slide),
        summarize("slide, moving the window", &moves)
    );
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(result)
}

/// Memory check (`--churn N`): loads every icon cold, round after round, and reports how private
/// memory moves for each kind of icon source, then for the drawing bitmaps made from them.
pub fn churn(config: Config, home: Home, rounds: u32) -> Result<String> {
    let mut dock = Dock::new(None, config, home, None, Mode::Normal)?;
    dock.relayout();
    let size = dock.layout.geo.icon as u32;
    let loader = IconLoader::new(None)?; // no cache: every load is cold
    let kind = |s: &IconSource| match s {
        IconSource::Image(_) => "image file",
        IconSource::Resource(..) => "icon in exe/dll",
        IconSource::Shell(_) => "shell",
        IconSource::RecycleBin { .. } => "recycle bin",
    };
    let mut report = format!("churn: {} items, icon {size} px, {rounds} rounds each\n", dock.items.len());
    for name in ["image file", "icon in exe/dll", "shell", "recycle bin"] {
        let sources: Vec<Vec<IconSource>> = dock
            .items
            .iter()
            .filter(|item| item.sources.first().is_some_and(|s| kind(s) == name))
            .map(|item| item.sources.clone())
            .collect();
        if sources.is_empty() {
            continue;
        }
        for s in &sources {
            let _ = loader.load(s, size); // warm-up round
        }
        let before = system::private_mb();
        for round in 0..rounds {
            for s in &sources {
                let _ = loader.load(s, size + round % 2); // alternate sizes, as resizing the dock does
            }
        }
        let after = system::private_mb();
        report.push_str(&format!(
            "  {name}: {} items, {:+.2} MB over {} loads ({:+.1} KB per load)\n",
            sources.len(),
            after - before,
            sources.len() as u32 * rounds,
            (after - before) * 1024.0 / (sources.len() as u32 * rounds) as f64
        ));
    }
    let pixels: Vec<icons::Pixels> = dock.items.iter().filter_map(|item| loader.load(&item.sources, size)).collect();
    let mut phase = |name: &str, dock: &mut Dock, after_round: &dyn Fn(&mut Dock)| {
        let before = system::private_mb();
        for _ in 0..rounds {
            let bitmaps: Vec<ID2D1Bitmap> = pixels.iter().filter_map(|p| dock.renderer.bitmap(p).ok()).collect();
            drop(bitmaps);
            after_round(dock);
        }
        let after = system::private_mb();
        report.push_str(&format!("  bitmaps, {name}: {:+.2} MB over {} bitmaps\n", after - before, pixels.len() as u32 * rounds));
    };
    phase("hidden, nothing else", &mut dock, &|_| {});
    phase("hidden, settle() each round", &mut dock, &|dock| dock.renderer.settle());
    phase("shown, a full frame each round", &mut dock, &|dock| {
        let _ = dock.draw();
    });
    let before = system::private_mb();
    dock.renderer.release();
    let freed = system::private_mb() - before;
    report.push_str(&format!("  (the full-size drawing buffer was {:.2} MB)\n", -freed));
    Ok(report)
}

/// Memory check for pointing along the dock (`--hover-churn N`, development builds): N sweeps
/// across every icon, drawn as the dock draws them (each zoom icon made on its first hover from
/// the icon cache, the last ZOOM_KEEP kept; names; glows), each sweep ending as the dock does
/// when it hides. No window, so it runs anywhere. Reports memory and the heaps before and after.
pub fn hover_churn(config: Config, home: Home, rounds: u32) -> Result<String> {
    let mut dock = Dock::new(None, config, home.clone(), None, Mode::Normal)?;
    dock.settings.hover_glow = HoverGlow::Icon;
    dock.relayout();
    let cache = home.icon_cache();
    let (size, zoom_px) = (dock.layout.geo.icon as u32, dock.layout.geo.hover_icon as u32);
    for index in 0..dock.items.len() {
        let pixels = icons::cached(&cache, &dock.items[index].sources, size);
        let tint = pixels.as_ref().and_then(icons::main_colour);
        let sources = dock.items[index].sources.clone();
        dock.tints.insert(sources, tint);
        dock.items[index].base = pixels.and_then(|p| dock.renderer.bitmap(&p).ok());
    }
    let steps = 6;
    let (start_mb, start_heap) = (system::private_mb(), system::heap_and_threads());
    let mut frames = 0;
    for _ in 0..rounds {
        for index in 0..dock.items.len() {
            if dock.items[index].zoom.is_none() {
                if let Some(pixels) = icons::cached(&cache, &dock.items[index].sources, zoom_px) {
                    dock.items[index].zoom = dock.renderer.bitmap(&pixels).ok();
                    dock.zoom_lru.retain(|&i| i != index);
                    dock.zoom_lru.push_back(index);
                    while dock.zoom_lru.len() > ZOOM_KEEP {
                        if let Some(oldest) = dock.zoom_lru.pop_front() {
                            dock.items[oldest].zoom = None;
                        }
                    }
                }
            }
            dock.hovered = Some(index);
            for step in 1..=steps {
                for (k, item) in dock.items.iter_mut().enumerate() {
                    item.hover_t = if k == index { step as f32 / steps as f32 } else { (item.hover_t - 1.0 / steps as f32).max(0.0) };
                }
                let _ = dock.draw();
                frames += 1;
            }
        }
        dock.hovered = None;
        for item in &mut dock.items {
            item.hover_t = 0.0;
        }
        dock.tidy_hidden();
    }
    Ok(format!(
        "hover churn: {} items, {rounds} sweeps, {frames} frames\n  private {start_mb:.2} -> {:.2} MB\n  start: {start_heap}\n  end:   {}",
        dock.items.len(),
        system::private_mb(),
        system::heap_and_threads()
    ))
}

/// Renders the dock to image files without showing a window (used to check the look).
pub fn snapshot(config: Config, home: Home, dir: &Path) -> Result<()> {
    let io = |e: std::io::Error| Error::new(E_FAIL, e.to_string());
    // For the group pictures: your first group if you have one, otherwise the first nine programs
    // in a group at the front; and a group with a long name in it (shown in full when pointed at).
    let grouped = {
        let mut grouped = config.clone();
        if !grouped.items.iter().any(|item| item.kind == Kind::Group) {
            let apps: Vec<ItemConfig> = grouped.items.iter().filter(|item| item.kind == Kind::App).take(9).cloned().collect();
            let mut group = ItemConfig::new("Test group", "");
            group.kind = Kind::Group;
            group.items = apps;
            grouped.items.insert(0, group);
        }
        grouped
    };
    let long_named = {
        let mut long_named = config.clone();
        let mut apps: Vec<ItemConfig> = long_named.items.iter().filter(|item| item.kind == Kind::App).take(6).cloned().collect();
        if let Some(app) = apps.get_mut(2) {
            app.name = "A program whose name is far too long for two lines".into();
        }
        let mut group = ItemConfig::new("Long names", "");
        group.kind = Kind::Group;
        group.items = apps;
        long_named.items.insert(0, group);
        long_named
    };
    let group_home = home.clone();
    std::fs::create_dir_all(dir).map_err(io)?;
    let mut dock = Dock::new(None, config, home, None, Mode::Normal)?;
    dock.relayout();
    dock.slide_by_moving = false; // so the 'sliding' picture shows the slide
    dock.reveal = 1.0;
    dock.reveal_target = 1.0;
    dock.draw()?;
    dock.renderer.save_bmp(&dir.join("1-shown.bmp")).map_err(io)?;

    let pick = dock
        .items
        .iter()
        .position(|item| item.cfg.name.eq_ignore_ascii_case("chrome"))
        .or_else(|| dock.items.iter().position(|item| !item.cfg.is_separator()));
    if let Some(index) = pick {
        if let Ok(loader) = IconLoader::new(None) {
            let zoom_px = dock.layout.geo.hover_icon as u32;
            let pixels = loader.load(&dock.items[index].sources, zoom_px);
            dock.items[index].zoom = pixels.and_then(|p| dock.renderer.bitmap(&p).ok());
        }
        dock.hovered = Some(index);
        dock.items[index].hover_t = 1.0;
        dock.draw()?;
        dock.renderer.save_bmp(&dir.join("2-hover.bmp")).map_err(io)?;
        // The hover glow: Windows' accent colour, then the icon's own.
        for (glow, name) in [(HoverGlow::Accent, "2b-glow-accent.bmp"), (HoverGlow::Icon, "2c-glow-icon.bmp")] {
            dock.settings.hover_glow = glow;
            dock.draw()?;
            dock.renderer.save_bmp(&dir.join(name)).map_err(io)?;
        }
        dock.settings.hover_glow = HoverGlow::None;
        dock.hovered = None;
        dock.items[index].hover_t = 0.0;
        // The Glow pulse launch effect at its brightest (with the hover glow off).
        dock.items[index].pulse_start = Instant::now().checked_sub(PULSE.mul_f32(0.15));
        dock.draw()?;
        dock.renderer.save_bmp(&dir.join("2d-glow-pulse.bmp")).map_err(io)?;
        dock.items[index].pulse_start = None;
    }
    dock.reveal = 0.5;
    dock.draw()?;
    dock.renderer.save_bmp(&dir.join("3-sliding.bmp")).map_err(io)?;
    dock.reveal = 1.0;
    dock.show_strip("Removed Photoshop");
    dock.draw()?;
    dock.renderer.save_bmp(&dir.join("4-undo-strip.bmp")).map_err(io)?;
    dock.strip = None;
    if let Some(index) = pick {
        // An icon picked up and held three places along: the others make room.
        dock.drag = Some(Drag { index, label: String::new(), grab: (0.0, 0.0), size: dock.layout.geo.icon, gap: Some(index + 3), image_off: None, off_since: None, child: None, into: None, in_popup: None, over: None, sprung: false });
        for i in 0..dock.items.len() {
            dock.items[i].shift = dock.shift_target(i);
        }
        dock.draw()?;
        dock.renderer.save_bmp(&dir.join("5-making-room.bmp")).map_err(io)?;
        // The picture that follows the pointer, as it looks over the dock and well away from it.
        dock.draw_drag_image(false);
        dock.renderer.save_drag_bmp(&dir.join("6-drag-image.bmp")).map_err(io)?;
        dock.draw_drag_image(true);
        dock.renderer.save_drag_bmp(&dir.join("7-drag-remove.bmp")).map_err(io)?;
    }
    if let Some(separator) = dock.items.iter().position(|item| item.cfg.is_separator()) {
        // A separator picked up: the bar under the pointer, its place closed up behind it.
        dock.drag = Some(Drag { index: separator, label: String::new(), grab: (0.0, 0.0), size: dock.layout.geo.icon, gap: None, image_off: None, off_since: None, child: None, into: None, in_popup: None, over: None, sprung: false });
        for i in 0..dock.items.len() {
            dock.items[i].shift = dock.shift_target(i);
        }
        dock.draw_drag_image(false);
        dock.renderer.save_drag_bmp(&dir.join("7b-separator-drag.bmp")).map_err(io)?;
        dock.drag = None;
        for item in &mut dock.items {
            item.shift = 0.0;
        }
    }

    let mut dock = Dock::new(None, grouped, group_home.clone(), None, Mode::Normal)?;
    dock.relayout();
    dock.reveal = 1.0;
    dock.reveal_target = 1.0;
    dock.draw()?;
    dock.renderer.save_bmp(&dir.join("8-group-icon.bmp")).map_err(io)?;
    let first_group = dock.items.iter().position(|item| item.cfg.kind == Kind::Group).unwrap_or(0);
    dock.open_group(first_group);
    if let Some(open) = &mut dock.group {
        open.hovered = Some(2);
    }
    if dock.draw_popup() {
        dock.renderer.save_popup_bmp(&dir.join("9-group.bmp")).map_err(io)?;
    }
    let mut dock = Dock::new(None, long_named, group_home, None, Mode::Normal)?;
    dock.relayout();
    dock.open_group(0);
    if let Some(open) = &mut dock.group {
        open.hovered = Some(2);
    }
    if dock.draw_popup() {
        dock.renderer.save_popup_bmp(&dir.join("10-group-long-name.bmp")).map_err(io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The glow's shape: full under the middle of the icon, fading gradually, nothing at the
    /// edge and no rim. It is drawn as one mask; this keeps that mask's shape.
    #[test]
    fn the_glow_is_full_in_the_middle_and_fades_to_nothing() {
        let side = 128;
        let shape = glow_shape(side);
        assert_eq!(shape.data.len(), (side * side * 4) as usize);
        let alpha = |x: u32, y: u32| shape.data[((y * side + x) * 4 + 3) as usize];
        let middle = side / 2;
        assert_eq!(alpha(middle, middle), 255, "full in the middle");
        assert_eq!(alpha(middle + 25, middle), 255, "still full at ~40% of the radius");
        assert!(alpha(middle + 50, middle) < 30, "nearly gone at ~80%");
        assert_eq!(alpha(side - 1, middle), 0, "nothing at the edge");
        assert_eq!(alpha(0, 0), 0, "nothing in the corners");
        let along: Vec<u8> = (middle..side).map(|x| alpha(x, middle)).collect();
        assert!(along.windows(2).all(|pair| pair[1] <= pair[0]), "only ever fades outward (no rim): {along:?}");
        assert!(shape.data.chunks_exact(4).all(|p| p[0] == p[3] && p[1] == p[3] && p[2] == p[3]), "white, premultiplied");
    }
}
