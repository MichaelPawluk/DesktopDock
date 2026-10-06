//! The settings windows' finish (settings process only): the section list drawn like Windows 11
//! Settings (a symbol and a name per row, the chosen one on a soft highlight with an accent
//! bar), the page titles and headings, and the app's icon on every window. Plain GDI on the
//! standard dialogs; the symbols come from the font Windows 11 draws its own with.

use crate::system;
use std::cell::RefCell;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, COLOR_BTNTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, CreateFontIndirectW, CreateSolidBrush, DT_CENTER, DT_END_ELLIPSIS,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawFocusRect, DrawTextW, FW_SEMIBOLD, FillRect, GetDC, GetStockObject, GetSysColor,
    GetSysColorBrush, GetTextFaceW, HDC, HFONT, HGDIOBJ, LOGFONTW, NULL_PEN, ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{CAccPropServices, IAccPropServices, PROPID_ACC_NAME};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows::Win32::UI::WindowsAndMessaging::{
    GW_CHILD, GW_HWNDNEXT, GetDlgCtrlID, GetWindow, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON,
    OBJID_CLIENT, SM_CXSMICON, SendMessageW, WM_GETFONT, WM_SETFONT, WM_SETICON,
};
use core::ffi::c_void;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::core::{BOOL, HSTRING, PCWSTR, w};

/// The app's icon in the exe's resources (assets\desktop-dock.rc).
const APP_ICON: u16 = 1;

struct Fonts {
    dpi: u32,
    title: HFONT,
    heading: HFONT,
    symbols: HFONT,
}

thread_local! {
    /// One set per scaling the windows have been at (a window moved to a screen with different
    /// scaling gets its own; the old set stays, as controls elsewhere may still use it).
    static FONTS: RefCell<Vec<Fonts>> = const { RefCell::new(Vec::new()) };
}

fn font(dpi: u32, points: f32, weight: i32, face: &str) -> HFONT {
    let mut log = LOGFONTW { lfHeight: -((points * dpi as f32 / 72.0).round() as i32), lfWeight: weight, lfQuality: windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY, ..Default::default() };
    for (slot, c) in log.lfFaceName.iter_mut().zip(face.encode_utf16()) {
        *slot = c;
    }
    unsafe { CreateFontIndirectW(&log) }
}

/// The font's real face (Windows quietly substitutes one it doesn't have).
fn face_of(font: HFONT) -> String {
    unsafe {
        let dc = GetDC(None);
        let old = SelectObject(dc, font.into());
        let mut name = [0u16; 64];
        let len = GetTextFaceW(dc, Some(&mut name)).max(1) as usize - 1;
        SelectObject(dc, old);
        ReleaseDC(None, dc);
        String::from_utf16_lossy(&name[..len.min(name.len())])
    }
}

/// The fonts for a window's DPI, made once for each DPI.
fn with_fonts<R>(hwnd: HWND, f: impl FnOnce(&Fonts) -> R) -> R {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    FONTS.with(|cell| {
        let mut sets = cell.borrow_mut();
        if !sets.iter().any(|fonts| fonts.dpi == dpi) {
            // Windows 11's symbol font; Windows 10 has the same symbols in its older one.
            let mut symbols = font(dpi, 11.5, 400, "Segoe Fluent Icons");
            if !face_of(symbols).eq_ignore_ascii_case("Segoe Fluent Icons") {
                unsafe {
                    let _ = DeleteObject(symbols.into());
                }
                symbols = font(dpi, 11.5, 400, "Segoe MDL2 Assets");
            }
            sets.push(Fonts { dpi, title: font(dpi, 15.0, FW_SEMIBOLD.0 as i32, "Segoe UI"), heading: font(dpi, 10.0, FW_SEMIBOLD.0 as i32, "Segoe UI"), symbols });
        }
        f(sets.iter().find(|fonts| fonts.dpi == dpi).expect("made above"))
    })
}

/// Whether High Contrast is on (rows are then drawn in its own highlight colours).
fn high_contrast() -> bool {
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut contrast = HIGHCONTRASTW { cbSize: size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    let asked = unsafe {
        SystemParametersInfoW(SPI_GETHIGHCONTRAST, contrast.cbSize, Some(&mut contrast as *mut _ as *mut core::ffi::c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    asked.is_ok() && contrast.dwFlags.0 & HCF_HIGHCONTRASTON.0 != 0
}

/// Moves a window that sticks out of its screen's work area back inside it (with larger text
/// sizes a window can be taller than the screen: then its title bar stays reachable).
pub fn keep_on_screen(hwnd: HWND) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};
    unsafe {
        let mut window = RECT::default();
        let mut screen = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetWindowRect(hwnd, &mut window).is_err() || !GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut screen).as_bool() {
            return;
        }
        let work = screen.rcWork;
        let fit = |start: i32, end: i32, low: i32, high: i32| (start - (end - high).max(0)).max(low);
        let (x, y) = (fit(window.left, window.right, work.left, work.right), fit(window.top, window.bottom, work.top, work.bottom));
        if (x, y) != (window.left, window.top) {
            let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// The app's icon at `size` pixels (from the exe), for a window or the About page.
pub fn app_icon(size: i32) -> Option<HICON> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let icon = LoadImageW(Some(instance.into()), PCWSTR(APP_ICON as usize as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR).ok()?;
        Some(HICON(icon.0))
    }
}

/// The app's icon on a window's title bar, the taskbar and Alt-Tab.
pub fn set_app_icon(hwnd: HWND) {
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96);
        for (kind, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
            if let Some(icon) = app_icon(GetSystemMetricsForDpi(metric, dpi)) {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize)));
            }
        }
    }
}

/// A page title: larger and semibold.
pub fn style_title(control: HWND) {
    let title = with_fonts(control, |fonts| fonts.title);
    unsafe {
        SendMessageW(control, WM_SETFONT, Some(WPARAM(title.0 as usize)), Some(LPARAM(1)));
    }
}

/// The headings on a page (every control numbered `id`): semibold.
pub fn style_headings(page: HWND, id: i32) {
    let heading = with_fonts(page, |fonts| fonts.heading);
    unsafe {
        let mut child = GetWindow(page, GW_CHILD).ok();
        while let Some(control) = child {
            if GetDlgCtrlID(control) == id {
                SendMessageW(control, WM_SETFONT, Some(WPARAM(heading.0 as usize)), Some(LPARAM(1)));
            }
            child = GetWindow(control, GW_HWNDNEXT).ok();
        }
    }
}

/// Moves `follower` to just after the end of `text`'s words (on the same line), as if it were
/// part of the sentence.
pub fn place_after_text(text: HWND, follower: HWND) {
    use windows::Win32::Graphics::Gdi::{GetTextExtentPoint32W, MapWindowPoints};
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::UI::WindowsAndMessaging::{GetParent, GetWindowRect, GetWindowTextW, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};
    unsafe {
        let mut words = [0u16; 512];
        let len = GetWindowTextW(text, &mut words) as usize;
        let dc = GetDC(Some(text));
        let font = HFONT(SendMessageW(text, WM_GETFONT, None, None).0 as *mut core::ffi::c_void);
        let old = SelectObject(dc, font.into());
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &words[..len], &mut size);
        SelectObject(dc, old);
        ReleaseDC(Some(text), dc);
        let mut rect = RECT::default();
        let _ = GetWindowRect(text, &mut rect);
        let mut corner = [windows::Win32::Foundation::POINT { x: rect.left, y: rect.top }];
        MapWindowPoints(None, GetParent(text).ok(), &mut corner);
        let gap = 4 * GetDpiForWindow(text).max(96) as i32 / 96;
        let _ = SetWindowPos(follower, None, corner[0].x + size.cx + gap, corner[0].y, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
    }
}

/// A row's height in the section list, in pixels.
pub fn row_height(hwnd: HWND) -> u32 {
    34 * unsafe { GetDpiForWindow(hwnd) }.max(96) / 96
}

/// A colour a little darker than `colour` (the highlight under the chosen row).
fn darker(colour: u32, by: f32) -> COLORREF {
    let channel = |shift: u32| (((colour >> shift) & 0xFF) as f32 * by).round() as u32;
    COLORREF(channel(0) | (channel(8) << 8) | (channel(16) << 16))
}

/// Draws one row of the section list: its symbol and name, and if it's the chosen one, a soft
/// highlight with Windows' accent colour as a short bar at its left (under High Contrast, its
/// own highlight colours). The row with the keyboard focus gets the usual dotted outline.
pub fn draw_row(item: &DRAWITEMSTRUCT, name: &[u16], symbol: u16) {
    let (symbols, hwnd) = (with_fonts(item.hwndItem, |fonts| fonts.symbols), item.hwndItem);
    let chosen = item.itemState.0 & ODS_SELECTED.0 != 0;
    let contrast = high_contrast();
    unsafe {
        let dc: HDC = item.hDC;
        let px = |v: i32| v * GetDpiForWindow(hwnd).max(96) as i32 / 96;
        let row = item.rcItem;
        let dark = dark();
        FillRect(dc, &row, window_brush());
        let inner = RECT { left: row.left + px(2), top: row.top + px(2), right: row.right - px(2), bottom: row.bottom - px(2) };
        if chosen && contrast {
            FillRect(dc, &inner, GetSysColorBrush(COLOR_HIGHLIGHT));
        } else if chosen {
            let pen = SelectObject(dc, GetStockObject(NULL_PEN));
            let fill = CreateSolidBrush(if dark { DARK_CHOSEN } else { darker(GetSysColor(COLOR_BTNFACE), 0.92) });
            let old = SelectObject(dc, fill.into());
            let _ = RoundRect(dc, inner.left, inner.top, inner.right + 1, inner.bottom + 1, px(8), px(8));
            let [r, g, b] = system::accent_colour().unwrap_or([0.0, 0.47, 0.84]);
            let accent = CreateSolidBrush(COLORREF((r * 255.0) as u32 | (((g * 255.0) as u32) << 8) | (((b * 255.0) as u32) << 16)));
            SelectObject(dc, accent.into());
            let (bar_h, bar_w) = (px(16), px(3));
            let top = (inner.top + inner.bottom - bar_h) / 2;
            let _ = RoundRect(dc, inner.left, top, inner.left + bar_w + 1, top + bar_h + 1, bar_w, bar_w);
            SelectObject(dc, old);
            SelectObject(dc, pen);
            let _ = DeleteObject(fill.into());
            let _ = DeleteObject(accent.into());
        }
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, if dark { DARK_TEXT } else { COLORREF(GetSysColor(if chosen && contrast { COLOR_HIGHLIGHTTEXT } else { COLOR_BTNTEXT })) });
        let old_font: HGDIOBJ = SelectObject(dc, symbols.into());
        let mut glyph = [symbol];
        let mut place = RECT { left: inner.left + px(10), right: inner.left + px(32), ..inner };
        DrawTextW(dc, &mut glyph, &mut place, DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_NOPREFIX);
        let text_font = HFONT(SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut core::ffi::c_void);
        SelectObject(dc, text_font.into());
        let mut text = name.to_vec();
        let mut place = RECT { left: inner.left + px(40), ..inner };
        DrawTextW(dc, &mut text, &mut place, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS);
        SelectObject(dc, old_font);
        // The outline shows once the keyboard is used (Windows hides it for the mouse alone).
        if item.itemState.0 & ODS_FOCUS.0 != 0 && item.itemState.0 & ODS_NOFOCUSRECT.0 == 0 {
            let _ = DrawFocusRect(dc, &inner);
        }
    }
}

// ---- Dark mode ----------------------------------------------------------------------------
//
// The windows follow Windows' own choice for apps (Settings > Personalisation > Colours), with
// documented calls only: a dark title bar from the window manager, Windows' dark visual styles
// for buttons, boxes and lists, and the dialogs' own colour messages for the rest. Tick boxes
// and slider tracks are drawn here, as Windows can't recolour their text and track. High
// Contrast keeps its own colours. Menus stay light: only an undocumented call makes them dark.

/// The colours of a dark window (Windows 11's own, near enough).
const DARK_WINDOW: COLORREF = COLORREF(0x00202020);
const DARK_CONTROL: COLORREF = COLORREF(0x002B2B2B);
const DARK_LIST: COLORREF = COLORREF(0x00191919);
const DARK_TEXT: COLORREF = COLORREF(0x00FFFFFF);
const DARK_MUTED: COLORREF = COLORREF(0x009D9D9D);
const DARK_CHOSEN: COLORREF = COLORREF(0x00383838);
const DARK_TRACK: COLORREF = COLORREF(0x00808080);
const DARK_DISABLED_BUTTON: COLORREF = COLORREF(0x00292929);
const DARK_EDGE: COLORREF = COLORREF(0x00454545);

thread_local! {
    static DARK_BRUSHES: RefCell<Option<[HBRUSH; 3]>> = const { RefCell::new(None) };
}

/// Window, control and list brushes for dark mode, made once.
fn dark_brushes() -> [HBRUSH; 3] {
    DARK_BRUSHES.with(|cell| *cell.borrow_mut().get_or_insert_with(|| unsafe { [CreateSolidBrush(DARK_WINDOW), CreateSolidBrush(DARK_CONTROL), CreateSolidBrush(DARK_LIST)] }))
}

thread_local! {
    /// Whether the windows are dark, worked out once and again when Windows' choice changes.
    static DARK: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Whether the windows are dark now: Windows' apps are set to dark, and High Contrast is off.
pub fn dark() -> bool {
    DARK.with(|dark| {
        dark.get().unwrap_or_else(|| {
            let now = look_override().unwrap_or_else(windows_is_dark);
            dark.set(Some(now));
            now
        })
    })
}

/// Development builds only: DESKTOP_DOCK_LOOK=light or dark, so tests can see either look
/// without changing Windows.
fn look_override() -> Option<bool> {
    let look = std::env::var("DESKTOP_DOCK_LOOK").ok()?;
    crate::home::is_dev_build().then(|| look.eq_ignore_ascii_case("dark"))
}

fn windows_is_dark() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut light = 1u32;
    let mut size = 4u32;
    let found = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut light as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    found.is_ok() && light == 0 && !high_contrast()
}

/// The window's background (the section list sits on it).
pub fn window_brush() -> HBRUSH {
    if dark() { dark_brushes()[0] } else { unsafe { GetSysColorBrush(COLOR_BTNFACE) } }
}

/// The name a screen reader says for `control`, where it can't take one from the label before
/// it: a list with no label of its own, a list whose label is a whole paragraph, two "Browse..."
/// buttons. (The settings process has COM set up.)
pub fn name_for_screen_readers(control: HWND, name: &str) {
    unsafe {
        let Ok(services) = CoCreateInstance::<_, IAccPropServices>(&CAccPropServices, None, CLSCTX_INPROC_SERVER) else { return };
        let _ = services.SetHwndPropStr(control, OBJID_CLIENT.0 as u32, 0, PROPID_ACC_NAME, &HSTRING::from(name));
    }
}

/// Gives a window and everything in it the current look, light or dark: when it opens, and
/// again when Windows' choice changes.
pub fn apply_theme(window: HWND) {
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    use windows::Win32::Graphics::Gdi::{RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow};
    use windows::Win32::UI::WindowsAndMessaging::EnumChildWindows;
    unsafe extern "system" fn each(control: HWND, dark: LPARAM) -> BOOL {
        unsafe { theme_control(control, dark.0 != 0) };
        true.into()
    }
    DARK.with(|dark| dark.set(None)); // worked out afresh: this may be because it changed
    let dark = dark();
    unsafe {
        let on = BOOL::from(dark);
        let _ = DwmSetWindowAttribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const BOOL as *const c_void, size_of::<BOOL>() as u32);
        let _ = EnumChildWindows(Some(window), Some(each), LPARAM(dark as isize));
        let _ = RedrawWindow(Some(window), None, None, RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN);
    }
}

/// One control's visual style for the look: Windows' dark styles, or its usual ones.
unsafe fn theme_control(control: HWND, dark: bool) {
    use windows::Win32::Graphics::Gdi::{COLOR_WINDOW, COLOR_WINDOWTEXT};
    use windows::Win32::UI::Controls::{LVM_GETHEADER, LVM_SETBKCOLOR, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, SetWindowTheme};
    let style = |hwnd: HWND, dark_style: PCWSTR| unsafe {
        let _ = if dark { SetWindowTheme(hwnd, dark_style, PCWSTR::null()) } else { SetWindowTheme(hwnd, PCWSTR::null(), PCWSTR::null()) };
    };
    match class_of(control).as_str() {
        "Button" | "ScrollBar" => style(control, w!("DarkMode_Explorer")),
        "Edit" | "ComboBox" => style(control, w!("DarkMode_CFD")),
        "SysListView32" => unsafe {
            style(control, w!("DarkMode_Explorer"));
            let _ = windows::Win32::UI::Shell::SetWindowSubclass(control, Some(list_headings), 2, 0);
            // Light: Windows' own list colours (what a list has before anything is set).
            let (back, text) = if dark { (DARK_LIST, DARK_TEXT) } else { (COLORREF(GetSysColor(COLOR_WINDOW)), COLORREF(GetSysColor(COLOR_WINDOWTEXT))) };
            let (back, text) = (back.0 as isize, text.0 as isize);
            SendMessageW(control, LVM_SETBKCOLOR, None, Some(LPARAM(back)));
            SendMessageW(control, LVM_SETTEXTBKCOLOR, None, Some(LPARAM(back)));
            SendMessageW(control, LVM_SETTEXTCOLOR, None, Some(LPARAM(text)));
            let header = HWND(SendMessageW(control, LVM_GETHEADER, None, None).0 as *mut c_void);
            if !header.is_invalid() {
                style(header, w!("DarkMode_ItemsView"));
            }
        },
        "msctls_hotkey32" => unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};
            let _ = windows::Win32::UI::Shell::SetWindowSubclass(control, Some(shortcut_box), 3, 0);
            let _ = SetWindowPos(control, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        },
        _ => {}
    }
}

fn class_of(control: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(control, &mut name) }.max(0) as usize;
    String::from_utf16_lossy(&name[..len])
}

/// Answers a dialog's colour messages in dark mode (None in light mode: Windows' own colours).
pub fn colours(msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<isize> {
    use windows::Win32::Graphics::Gdi::SetBkColor;
    use windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    use windows::Win32::UI::WindowsAndMessaging::{WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC};
    if !matches!(msg, WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX) || !dark() {
        return None;
    }
    let [window, control_brush, _] = dark_brushes();
    let dc = HDC(wparam.0 as *mut c_void);
    let control = HWND(lparam.0 as *mut c_void);
    let enabled = msg == WM_CTLCOLORDLG || unsafe { IsWindowEnabled(control) }.as_bool();
    unsafe {
        SetTextColor(dc, if enabled { DARK_TEXT } else { DARK_MUTED });
        if matches!(msg, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX) {
            SetBkColor(dc, DARK_CONTROL);
            Some(control_brush.0 as isize)
        } else {
            SetBkColor(dc, DARK_WINDOW);
            Some(window.0 as isize)
        }
    }
}

/// Draws tick boxes, round choice buttons and slider tracks in dark mode, where Windows would
/// draw dark text or a light track. For a dialog's WM_NOTIFY: true if it answered (the answer
/// is in the dialog's result), false to leave it.
pub fn custom_draw(dialog: HWND, lparam: LPARAM) -> bool {
    use windows::Win32::UI::Controls::{CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_NOTIFYITEMDRAW, CDRF_SKIPDEFAULT, NM_CUSTOMDRAW, NMCUSTOMDRAW, TBCD_CHANNEL};
    use windows::Win32::UI::WindowsAndMessaging::{DWLP_MSGRESULT, SetWindowLongPtrW, WINDOW_LONG_PTR_INDEX};
    let draw = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
    if draw.hdr.code != NM_CUSTOMDRAW || !dark() {
        return false;
    }
    let answer = |result: u32| unsafe {
        SetWindowLongPtrW(dialog, WINDOW_LONG_PTR_INDEX(DWLP_MSGRESULT as i32), result as isize);
        true
    };
    match class_of(draw.hdr.hwndFrom).as_str() {
        // The track: a thin line in a grey that shows on dark (the thumb is Windows' own).
        "msctls_trackbar32" => match draw.dwDrawStage {
            CDDS_PREPAINT => answer(CDRF_NOTIFYITEMDRAW),
            CDDS_ITEMPREPAINT if draw.dwItemSpec == TBCD_CHANNEL as usize => unsafe {
                let px = (GetDpiForWindow(draw.hdr.hwndFrom).max(96) / 96) as i32;
                let middle = (draw.rc.top + draw.rc.bottom) / 2;
                let line = RECT { left: draw.rc.left, top: middle - px, right: draw.rc.right, bottom: middle + px };
                let brush = CreateSolidBrush(DARK_TRACK);
                FillRect(draw.hdc, &line, brush);
                let _ = DeleteObject(brush.into());
                answer(CDRF_SKIPDEFAULT)
            },
            _ => false,
        },
        "Button" if draw.dwDrawStage == CDDS_PREPAINT && (draw_tick_box(draw) || draw_disabled_button(draw)) => answer(CDRF_SKIPDEFAULT),
        _ => false,
    }
}

/// A tick box or round choice button, drawn whole in dark colours. False if it's another kind
/// of button (push buttons take Windows' dark style as they are).
fn draw_tick_box(draw: &windows::Win32::UI::Controls::NMCUSTOMDRAW) -> bool {
    use windows::Win32::Graphics::Gdi::{DT_CALCRECT, DT_HIDEPREFIX};
    use windows::Win32::UI::Controls::{
        BP_CHECKBOX, BP_RADIOBUTTON, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDIS_SELECTED, CloseThemeData, DrawThemeBackground, GetThemePartSize, OpenThemeData, TS_DRAW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        BM_GETCHECK, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_CHECKBOX, BS_RADIOBUTTON, BS_TYPEMASK, GWL_STYLE, GetWindowLongW, GetWindowTextW, UISF_HIDEACCEL,
        UISF_HIDEFOCUS, WM_QUERYUISTATE,
    };
    let control = draw.hdr.hwndFrom;
    let kind = unsafe { GetWindowLongW(control, GWL_STYLE) } & BS_TYPEMASK;
    let radio = kind == BS_RADIOBUTTON || kind == BS_AUTORADIOBUTTON;
    if !(radio || kind == BS_CHECKBOX || kind == BS_AUTOCHECKBOX) {
        return false;
    }
    unsafe {
        let dc = draw.hdc;
        FillRect(dc, &draw.rc, dark_brushes()[0]);
        let ticked = SendMessageW(control, BM_GETCHECK, None, None).0 == 1;
        let state = draw.uItemState.0;
        let disabled = state & CDIS_DISABLED.0 != 0;
        // Windows' state numbers: normal, hot, pressed, disabled; then the same, ticked.
        let step = if disabled { 3 } else if state & CDIS_SELECTED.0 != 0 { 2 } else if state & CDIS_HOT.0 != 0 { 1 } else { 0 };
        let (part, state_id) = (if radio { BP_RADIOBUTTON.0 } else { BP_CHECKBOX.0 }, 1 + step + if ticked { 4 } else { 0 });
        let theme = OpenThemeData(Some(control), w!("Button"));
        let size = GetThemePartSize(theme, Some(dc), part, state_id, None, TS_DRAW).unwrap_or(windows::Win32::Foundation::SIZE { cx: 13, cy: 13 });
        let middle = (draw.rc.top + draw.rc.bottom) / 2;
        let tick = RECT { left: draw.rc.left, top: middle - size.cy / 2, right: draw.rc.left + size.cx, bottom: middle - size.cy / 2 + size.cy };
        let _ = DrawThemeBackground(theme, dc, part, state_id, &tick, None);
        let _ = CloseThemeData(theme);
        let mut words = [0u16; 256];
        let len = GetWindowTextW(control, &mut words).max(0) as usize;
        let mut text = words[..len].to_vec();
        let cues = SendMessageW(control, WM_QUERYUISTATE, None, None).0 as u32;
        let hide = if cues & UISF_HIDEACCEL != 0 { DT_HIDEPREFIX } else { Default::default() };
        let font = HFONT(SendMessageW(control, WM_GETFONT, None, None).0 as *mut c_void);
        let old = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, if disabled { DARK_MUTED } else { DARK_TEXT });
        let gap = 4 * GetDpiForWindow(control).max(96) as i32 / 96;
        let mut place = RECT { left: tick.right + gap, ..draw.rc };
        DrawTextW(dc, &mut text, &mut place, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | hide);
        if state & CDIS_FOCUS.0 != 0 && cues & UISF_HIDEFOCUS == 0 {
            let mut around = RECT { left: tick.right + gap, ..draw.rc };
            DrawTextW(dc, &mut text, &mut around, DT_SINGLELINE | DT_CALCRECT | hide);
            let height = around.bottom - around.top;
            let focus = RECT { left: around.left - 1, top: middle - height / 2 - 1, right: around.right + 1, bottom: middle - height / 2 + height + 1 };
            let _ = DrawFocusRect(dc, &focus);
        }
        SelectObject(dc, old);
    }
    true
}

/// A push button that can't be used now, drawn flat with grey words. (Windows' dark style can
/// keep drawing one that was usable a moment ago as if it still were.) False if it's usable.
fn draw_disabled_button(draw: &windows::Win32::UI::Controls::NMCUSTOMDRAW) -> bool {
    use windows::Win32::UI::Controls::CDIS_DISABLED;
    use windows::Win32::UI::WindowsAndMessaging::{BS_DEFPUSHBUTTON, BS_PUSHBUTTON, BS_TYPEMASK, GWL_STYLE, GetWindowLongW, GetWindowTextW};
    let control = draw.hdr.hwndFrom;
    let kind = unsafe { GetWindowLongW(control, GWL_STYLE) } & BS_TYPEMASK;
    if draw.uItemState.0 & CDIS_DISABLED.0 == 0 || !(kind == BS_PUSHBUTTON || kind == BS_DEFPUSHBUTTON) {
        return false;
    }
    unsafe {
        let dc = draw.hdc;
        let px = |v: i32| v * GetDpiForWindow(control).max(96) as i32 / 96;
        FillRect(dc, &draw.rc, dark_brushes()[0]);
        let pen = SelectObject(dc, GetStockObject(NULL_PEN));
        let fill = CreateSolidBrush(DARK_DISABLED_BUTTON);
        let old = SelectObject(dc, fill.into());
        let _ = RoundRect(dc, draw.rc.left + px(1), draw.rc.top + px(1), draw.rc.right - px(1) + 1, draw.rc.bottom - px(1) + 1, px(4), px(4));
        SelectObject(dc, old);
        SelectObject(dc, pen);
        let _ = DeleteObject(fill.into());
        let mut words = [0u16; 128];
        let len = GetWindowTextW(control, &mut words).max(0) as usize;
        let font = HFONT(SendMessageW(control, WM_GETFONT, None, None).0 as *mut c_void);
        let old_font = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, DARK_MUTED);
        let mut place = draw.rc;
        DrawTextW(dc, &mut words[..len], &mut place, DT_SINGLELINE | DT_VCENTER | DT_CENTER | windows::Win32::Graphics::Gdi::DT_HIDEPREFIX);
        SelectObject(dc, old_font);
    }
    true
}

/// For a window's procedure: Windows changed between light and dark, or High Contrast went on
/// or off. True if the window should take its look again.
pub fn look_changed(msg: u32, lparam: LPARAM) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{WM_SETTINGCHANGE, WM_SYSCOLORCHANGE, WM_THEMECHANGED};
    match msg {
        WM_SYSCOLORCHANGE | WM_THEMECHANGED => true,
        WM_SETTINGCHANGE if lparam.0 != 0 => unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.is_ok_and(|area| area == "ImmersiveColorSet"),
        _ => false,
    }
}

/// Everything a dialog needs to follow the look, for the top of its procedure: its colours,
/// the tick boxes and sliders it draws, and taking the look again when Windows' changes.
pub fn themed_message(dialog: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::WM_NOTIFY;
    if let Some(brush) = colours(msg, wparam, lparam) {
        return Some(brush);
    }
    if msg == WM_NOTIFY && custom_draw(dialog, lparam) {
        return Some(1);
    }
    if look_changed(msg, lparam) {
        apply_theme(dialog);
    }
    None
}

/// A list's column headings: Windows' dark style keeps their words dark, and tells only the
/// list itself, so the list is asked to pass them on in a light colour.
unsafe extern "system" fn list_headings(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::UI::Controls::{CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDRF_DODEFAULT, CDRF_NOTIFYITEMDRAW, NM_CUSTOMDRAW, NMCUSTOMDRAW};
    use windows::Win32::UI::Shell::DefSubclassProc;
    use windows::Win32::UI::WindowsAndMessaging::WM_NOTIFY;
    if msg == WM_NOTIFY && dark() {
        let draw = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
        if draw.hdr.code == NM_CUSTOMDRAW && class_of(draw.hdr.hwndFrom) == "SysHeader32" {
            match draw.dwDrawStage {
                CDDS_PREPAINT => return LRESULT(CDRF_NOTIFYITEMDRAW as isize),
                CDDS_ITEMPREPAINT => unsafe {
                    SetTextColor(draw.hdc, DARK_TEXT);
                    return LRESULT(CDRF_DODEFAULT as isize);
                },
                _ => {}
            }
        }
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

/// The shortcut box (Windows' hotkey control) can't be recoloured, so in dark mode it's painted
/// here, with the words Windows would write in it ("Ctrl + Shift + D"); it still takes the keys
/// itself. In light mode it's left entirely to Windows.
unsafe extern "system" fn shortcut_box(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::Graphics::Gdi::{BeginPaint, DT_CALCRECT, EndPaint, FrameRect, GetWindowDC, PAINTSTRUCT};
    use windows::Win32::UI::Controls::HKM_GETHOTKEY;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, IsWindowEnabled};
    use windows::Win32::UI::Shell::DefSubclassProc;
    use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetWindowRect, SetCaretPos, WM_ERASEBKGND, WM_NCPAINT, WM_PAINT};
    if !dark() {
        return unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) };
    }
    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_NCPAINT => unsafe {
            // Its sunken edge, as a thin dark frame.
            let dc = GetWindowDC(Some(hwnd));
            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            let outer = RECT { left: 0, top: 0, right: rect.right - rect.left, bottom: rect.bottom - rect.top };
            let edge = CreateSolidBrush(DARK_EDGE);
            FrameRect(dc, &outer, edge);
            let _ = DeleteObject(edge.into());
            let inner = RECT { left: 1, top: 1, right: outer.right - 1, bottom: outer.bottom - 1 };
            FrameRect(dc, &inner, dark_brushes()[1]);
            ReleaseDC(Some(hwnd), dc);
            LRESULT(0)
        },
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rect = RECT::default();
            let _ = GetClientRect(hwnd, &mut rect);
            FillRect(dc, &rect, dark_brushes()[1]);
            let value = SendMessageW(hwnd, HKM_GETHOTKEY, None, None).0 as u32;
            let mut text: Vec<u16> = shortcut_words(value).encode_utf16().collect();
            let font = HFONT(SendMessageW(hwnd, WM_GETFONT, None, None).0 as *mut c_void);
            let old = SelectObject(dc, font.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, if IsWindowEnabled(hwnd).as_bool() { DARK_TEXT } else { DARK_MUTED });
            let pad = 2 * GetDpiForWindow(hwnd).max(96) as i32 / 96;
            let mut place = RECT { left: rect.left + pad, ..rect };
            DrawTextW(dc, &mut text, &mut place, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
            // The caret goes after the words, where Windows would put it.
            if GetFocus() == hwnd {
                let mut size = RECT { left: rect.left + pad, ..rect };
                DrawTextW(dc, &mut text, &mut size, DT_SINGLELINE | DT_NOPREFIX | DT_CALCRECT);
                let height = size.bottom - size.top;
                let _ = SetCaretPos(if value == 0 { rect.left + pad } else { size.right }, (rect.top + rect.bottom - height) / 2);
            }
            SelectObject(dc, old);
            let _ = EndPaint(hwnd, &paint);
            LRESULT(0)
        },
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// What the shortcut box holds, as Windows writes it: "Ctrl + Shift + D", "Ctrl + " while it's
/// being pressed, "None" when empty. Key names are Windows' own, in your language.
fn shortcut_words(value: u32) -> String {
    use crate::hotkey::{HOTKEYF_ALT, HOTKEYF_CONTROL, HOTKEYF_SHIFT};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyNameTextW, MAPVK_VK_TO_VSC, MapVirtualKeyW};
    let (key, flags) = (value & 0xFF, (value >> 8) & 0xFF);
    if key == 0 && flags == 0 {
        return "None".into();
    }
    let mut words = String::new();
    for (flag, name) in [(HOTKEYF_CONTROL, "Ctrl"), (HOTKEYF_SHIFT, "Shift"), (HOTKEYF_ALT, "Alt")] {
        if flags & flag != 0 {
            words.push_str(name);
            words.push_str(" + ");
        }
    }
    if key != 0 {
        // Keys that share a scan code with the number pad (arrows, Home, Insert...) are told
        // apart by the "extended" bit.
        let extended = matches!(key, 0x21..=0x28 | 0x2D | 0x2E | 0x6F | 0x90);
        let scan = unsafe { MapVirtualKeyW(key, MAPVK_VK_TO_VSC) } as i32;
        let mut name = [0u16; 64];
        let len = unsafe { GetKeyNameTextW((scan << 16) | if extended { 1 << 24 } else { 0 }, &mut name) }.max(0) as usize;
        words.push_str(&if len > 0 { String::from_utf16_lossy(&name[..len]) } else { format!("Key {key}") });
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::{HOTKEYF_CONTROL, HOTKEYF_SHIFT};

    #[test]
    fn the_shortcut_box_reads_as_windows_writes_it() {
        assert_eq!(shortcut_words(0), "None");
        assert_eq!(shortcut_words(HOTKEYF_CONTROL << 8), "Ctrl + ");
        let full = shortcut_words(((HOTKEYF_CONTROL | HOTKEYF_SHIFT) << 8) | 0x44);
        assert!(full.starts_with("Ctrl + Shift + ") && full.len() > "Ctrl + Shift + ".len(), "{full}");
        assert!(!full.contains("0x"), "a key's own name, not its number: {full}");
    }
}
