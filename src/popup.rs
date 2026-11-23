//! A group's pop-up: the window that opens below a group's icon, showing its items in a grid
//! (phone-folder style: every name shown, the group's name on top).
//!
//! This file holds what can be tested without a window: the layout (pure geometry, kept on the
//! screen), the rules for when it closes, and its drawing. The window itself belongs to the dock
//! (dock.rs), which shares its icons and its renderer with it.
//!
//! It never takes the keyboard focus (like the dock), so it can't rely on Windows telling it
//! when you click elsewhere: the dock looks, using `Closer`.

use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D1_COLOR_F};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ROUNDED_RECT, ID2D1Bitmap,
    ID2D1DCRenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows_numerics::Matrix3x2;
use windows::Win32::Graphics::DirectWrite::{DWRITE_MEASURING_MODE_NATURAL, IDWriteTextFormat};

use crate::config::Edge;

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Rect {
    fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self { left, top, right, bottom }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.bottom - self.top
    }

    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.left, top: self.top, right: self.right, bottom: self.bottom }
    }
}

/// What the layout needs to know. All in physical pixels; positions are on the screen.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    pub count: usize,
    /// The dock's icon size.
    pub icon: f32,
    /// Display scale (DPI / 96).
    pub scale: f32,
    /// The height of one line of names (the font's own line height).
    pub line_h: f32,
    /// The middle of the group's icon along the dock's edge (screen x on top or bottom, y on a
    /// side), where the pop-up hangs from.
    pub anchor_x: f32,
    /// Where the pop-up's panel starts, just past how far the dock's icons reach from its edge:
    /// a screen y below the dock on top, above it on the bottom; a screen x on a side.
    pub top: f32,
    /// The screen's work area (beside its taskbar): the pop-up stays inside it.
    pub screen_left: f32,
    pub screen_right: f32,
    pub screen_top: f32,
    pub screen_bottom: f32,
    /// The dock's edge: the pop-up opens away from it.
    pub edge: Edge,
    /// Showing what's inside a folder (None: a group).
    pub folder: Option<FolderLook>,
}

/// How a folder's pop-up differs from a group's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FolderLook {
    /// Inside a subfolder: a back button before the title.
    pub back: bool,
    /// At most this many across (about square below that, as a group is), and past this many
    /// rows it scrolls (a group's grid grows wider instead).
    pub max_columns: usize,
    pub max_rows: usize,
    /// The panel's size before (going into a subfolder or back): it never gets smaller than
    /// that while it's open, so it doesn't shrink away from under the pointer. (0, 0) at first.
    pub at_least: (f32, f32),
    /// No items, only a message (an empty folder, one that can't be read): room for two lines
    /// of it under the title.
    pub note: bool,
}

/// The buttons a folder's pop-up has besides its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// Back to the folder this one is in.
    Back,
    /// Open the folder in File Explorer.
    Explore,
}

/// A folder's scroll bar, when it scrolls: a thin rounded thumb in a lane of its own down the
/// right of the panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    /// Where the pointer counts as on it: the whole lane, beside the items.
    pub lane: Rect,
    /// The thumb's middle, across.
    pub x: f32,
    /// How far the thumb runs, top to bottom.
    pub top: f32,
    pub bottom: f32,
    /// The thumb's least length, so it's always easy to take hold of.
    pub least: f32,
}

/// Where on the scroll bar the pointer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarPart {
    Thumb,
    /// The lane above or below the thumb: a click there goes a page that way.
    Above,
    Below,
}

/// One item's place in the pop-up (window coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    /// Where the pointer counts as being on it.
    pub hit: Rect,
    /// Its icon (square).
    pub icon: Rect,
    /// Its name: room for two lines.
    pub label: Rect,
}

/// Where everything goes. The window is the panel plus room for its shadow all round.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// The window's top-left on the screen, and its size.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// The panel, in window coordinates.
    pub panel: Rect,
    /// The group's name.
    pub title: Rect,
    pub cells: Vec<Cell>,
    pub radius: f32,
    pub shadow: f32,
    /// Space around a highlighted item's icon and name.
    pub margin: f32,
    /// Where the items show (window coordinates): the cells beyond it are scrolled out of sight.
    pub view: Rect,
    /// How far the items are scrolled up, in pixels (0 for a group), and how far they can be.
    pub scroll: f32,
    pub max_scroll: f32,
    /// One row of items.
    pub row_h: f32,
    /// A folder's buttons, where they are.
    pub buttons: Vec<(Button, Rect)>,
    /// A folder's scroll bar, if it scrolls.
    pub bar: Option<Bar>,
}

/// The grid is about square, and no wider than this many columns, unless it wouldn't fit on
/// the screen that way: then it grows wider rather than off the bottom.
const MAX_COLUMNS: usize = 6;

pub fn layout(p: &Params) -> Layout {
    let s = p.scale;
    let shadow = (13.0 * s).round();
    let pad = (12.0 * s).round();
    let radius = (10.0 * s).round();
    let margin = (6.0 * s).round(); // round a highlight's icon and name
    let screen_margin = (8.0 * s).round();
    let count = p.count.max(1);
    // Wide enough for most names on two lines ("Adobe Dreamweaver / 2021"); a longer one shows
    // in full under the pointer.
    let cell_w = (p.icon * 2.2).max(p.icon + 64.0 * s).round();
    let label_gap = (4.0 * s).round();
    let cell_h = (margin + p.icon + label_gap + 2.0 * p.line_h + margin).round();
    let title_h = (p.line_h + 8.0 * s).round();
    let square = ((count as f32).sqrt().ceil() as usize).clamp(1, MAX_COLUMNS).min(count);
    // Rows that fit between where the pop-up starts and the far side of the screen, and columns
    // across it (on a side, it opens across the screen and runs along the edge).
    let room_away = match p.edge {
        Edge::Top | Edge::Left => (if p.edge == Edge::Top { p.screen_bottom } else { p.screen_right }) - screen_margin - (p.top - shadow) - 2.0 * shadow,
        Edge::Bottom | Edge::Right => (p.top + shadow) - (if p.edge == Edge::Bottom { p.screen_top } else { p.screen_left }) - screen_margin - 2.0 * shadow,
    };
    let (room_down, room_across) = if p.edge.is_side() {
        (p.screen_bottom - p.screen_top - 2.0 * screen_margin - 2.0 * shadow - pad - title_h, room_away - 2.0 * pad)
    } else {
        (room_away - pad - title_h, p.screen_right - p.screen_left - 2.0 * screen_margin - 2.0 * shadow - 2.0 * pad)
    };
    let fit_rows = ((room_down / cell_h).floor() as usize).max(1);
    let fit_columns = ((room_across / cell_w).floor() as usize).max(1);
    // A group grows wider rather than off the screen; a folder (which can hold hundreds) stays
    // about square and scrolls instead, with a scroll bar in a lane of its own on the right.
    let lane_w = (8.0 * s).round();
    let (columns, rows, shown_rows, lane) = match p.folder {
        None => {
            let columns = square.max(count.div_ceil(fit_rows)).min(fit_columns.max(square)).min(count);
            let rows = count.div_ceil(columns);
            (columns, rows, rows, 0.0)
        }
        Some(look) => {
            let fit_columns = (((room_across - lane_w) / cell_w).floor() as usize).max(1);
            // As wide as it was, if it was wider: its items fill those columns. A message alone
            // gets two columns' width.
            let kept = ((look.at_least.0 - 2.0 * pad) / cell_w).round().max(0.0) as usize;
            // About square, but wide enough to fit the rows chosen when it can (a grid stopping
            // at what fits, 6 x 3 say, is 6 across, not a square 5 that would need a 4th row).
            let most = look.max_columns.max(1);
            let square = ((count as f32).sqrt().ceil() as usize).max(count.div_ceil(look.max_rows.max(1))).clamp(1, most).min(count);
            let columns = (if look.note { 2 } else { square }).min(fit_columns).max(kept.min(fit_columns)).max(1);
            let rows = if look.note { 0 } else { count.div_ceil(columns) };
            let shown = rows.min(look.max_rows.max(1)).min(fit_rows);
            (columns, rows, shown, if shown < rows { lane_w } else { 0.0 })
        }
    };
    let items_h = match p.folder {
        Some(look) if look.note => (2.0 * p.line_h + 2.0 * margin).round(),
        _ => shown_rows as f32 * cell_h,
    };
    let panel_w = 2.0 * pad + columns as f32 * cell_w + lane;
    let panel_h = pad * 0.5 + title_h + items_h + pad * 0.5;
    // As tall as it was, if it was taller (the room left over is below its items).
    let (panel_w, panel_h) = match p.folder {
        Some(look) => (panel_w.max(look.at_least.0), panel_h.max(look.at_least.1)),
        None => (panel_w, panel_h),
    };
    let items_top = shadow + pad * 0.5 + title_h;
    let cells = (0..p.count)
        .map(|k| {
            let (row, column) = (k / columns, k % columns);
            let left = shadow + pad + column as f32 * cell_w;
            let top = items_top + row as f32 * cell_h;
            let icon_left = (left + (cell_w - p.icon) / 2.0).round();
            let icon = Rect::new(icon_left, top + margin, icon_left + p.icon, top + margin + p.icon);
            let label = Rect::new(left + 4.0 * s, icon.bottom + label_gap, left + cell_w - 4.0 * s, icon.bottom + label_gap + 2.0 * p.line_h);
            Cell { hit: Rect::new(left, top, left + cell_w, top + cell_h), icon, label }
        })
        .collect();
    let mut title = Rect::new(shadow + pad, shadow + pad * 0.5, shadow + panel_w - pad, shadow + pad * 0.5 + title_h);
    let view = Rect::new(shadow + 1.0, items_top, shadow + panel_w - 1.0, items_top + items_h);
    let mut buttons = Vec::new();
    if let Some(look) = p.folder {
        // A button a little bigger than a line of text, at each end of the title.
        let b = (title_h - 4.0 * s).round();
        let (top, inset) = (title.top + ((title_h - b) / 2.0).round(), 2.0 * s);
        if look.back {
            buttons.push((Button::Back, Rect::new(title.left - inset, top, title.left - inset + b, top + b)));
        }
        buttons.push((Button::Explore, Rect::new(title.right + inset - b, top, title.right + inset, top + b)));
        title = Rect::new(title.left + b + 2.0 * s, title.top, title.right - b - 2.0 * s, title.bottom);
    }
    let max_scroll = (rows - shown_rows) as f32 * cell_h;
    // The scroll bar: in the gap between the last column and the panel's edge, as tall as the
    // items' view less a little at each end.
    let bar = (lane > 0.0).then(|| {
        let right = shadow + panel_w;
        let gutter = pad + lane;
        let inset = (4.0 * s).round();
        Bar {
            lane: Rect::new(right - gutter, view.top, right, view.bottom),
            x: (right - gutter / 2.0).round(),
            top: view.top + inset,
            bottom: view.bottom - inset,
            least: (28.0 * s).round(),
        }
    });
    let width = (panel_w + 2.0 * shadow).ceil();
    let height = (panel_h + 2.0 * shadow).ceil();
    // Centred on the group's icon along the edge, but always fully on the screen; away from the
    // edge, just past the dock.
    let centred = |size: f32, from: f32, to: f32| {
        let lowest = from + screen_margin;
        let highest = (to - screen_margin - size).max(lowest);
        (p.anchor_x - size / 2.0).clamp(lowest, highest).round() as i32
    };
    let (x, y) = match p.edge {
        Edge::Top => (centred(width, p.screen_left, p.screen_right), (p.top - shadow).round() as i32),
        Edge::Bottom => (centred(width, p.screen_left, p.screen_right), (p.top + shadow - height).round() as i32),
        Edge::Left => ((p.top - shadow).round() as i32, centred(height, p.screen_top, p.screen_bottom)),
        Edge::Right => ((p.top + shadow - width).round() as i32, centred(height, p.screen_top, p.screen_bottom)),
    };
    let panel = Rect::new(shadow, shadow, shadow + panel_w, shadow + panel_h);
    Layout { x, y, width: width as i32, height: height as i32, panel, title, cells, radius, shadow, margin, view, scroll: 0.0, max_scroll, row_h: cell_h, buttons, bar }
}

impl Layout {
    /// Which item is at (x, y) in the window, if any (one scrolled out of sight isn't).
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !self.view.contains(x, y) {
            return None;
        }
        self.cells.iter().position(|cell| cell.hit.contains(x, y + self.scroll))
    }

    /// Which of a folder's buttons is at (x, y), if any.
    pub fn button_at(&self, x: f32, y: f32) -> Option<Button> {
        self.buttons.iter().find(|(_, rect)| rect.contains(x, y)).map(|(button, _)| *button)
    }

    /// The scroll bar's thumb, top to bottom (window coordinates): as long as the share of the
    /// items in sight, never shorter than `least`, and as far down as they're scrolled.
    pub fn thumb(&self) -> Option<(f32, f32)> {
        let bar = self.bar?;
        let run = bar.bottom - bar.top;
        let seen = self.view.bottom - self.view.top;
        let length = (run * seen / (seen + self.max_scroll)).max(bar.least).min(run);
        let along = if self.max_scroll > 0.0 { (self.scroll / self.max_scroll).clamp(0.0, 1.0) } else { 0.0 };
        let top = bar.top + (run - length) * along;
        Some((top, top + length))
    }

    /// Where on the scroll bar (x, y) is, if on it.
    pub fn bar_at(&self, x: f32, y: f32) -> Option<BarPart> {
        let (bar, (top, bottom)) = (self.bar?, self.thumb()?);
        if !bar.lane.contains(x, y) {
            return None;
        }
        Some(if y < top { BarPart::Above } else if y >= bottom { BarPart::Below } else { BarPart::Thumb })
    }

    /// How far it's scrolled with the thumb's top at `top` (a drag), within the ends.
    pub fn scroll_for_thumb(&self, top: f32) -> f32 {
        let (Some(bar), Some((now, end))) = (self.bar, self.thumb()) else { return 0.0 };
        let room = (bar.bottom - bar.top) - (end - now);
        if room <= 0.0 {
            return 0.0;
        }
        ((top - bar.top) / room * self.max_scroll).clamp(0.0, self.max_scroll)
    }

    /// How many whole rows show at once.
    pub fn shown_rows(&self) -> usize {
        ((self.view.bottom - self.view.top) / self.row_h).round() as usize
    }

    /// A scroll position `rows` rows on from `from` (back if negative), kept to whole rows
    /// and within the ends.
    pub fn scrolled(&self, from: f32, rows: i32) -> f32 {
        let row = (from / self.row_h).round() + rows as f32;
        (row * self.row_h).clamp(0.0, self.max_scroll)
    }

    /// The lines drawn for a button: back, or out to File Explorer (a box with an arrow leaving
    /// it). Window coordinates.
    pub fn glyph(&self, button: Button, scale: f32) -> Vec<Vec<(f32, f32)>> {
        let Some((_, r)) = self.buttons.iter().find(|(b, _)| *b == button) else { return Vec::new() };
        let (cx, cy) = ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0);
        let u = 4.0 * scale;
        match button {
            Button::Back => vec![vec![(cx + u * 0.5, cy - u), (cx - u * 0.5, cy), (cx + u * 0.5, cy + u)]],
            Button::Explore => vec![
                vec![(cx - u * 0.2, cy - u * 1.1), (cx - u * 1.1, cy - u * 1.1), (cx - u * 1.1, cy + u * 1.1), (cx + u * 1.1, cy + u * 1.1), (cx + u * 1.1, cy + u * 0.2)],
                vec![(cx + u * 0.3, cy - u * 1.4), (cx + u * 1.4, cy - u * 1.4), (cx + u * 1.4, cy - u * 0.3)],
                vec![(cx + u * 1.4, cy - u * 1.4), (cx - u * 0.1, cy + u * 0.1)],
            ],
        }
    }

    /// Where something dragged over the pop-up at (x, y) (window coordinates) would go: before
    /// the nearest item when on its left half, after it on its right.
    pub fn gap_at(&self, x: f32, y: f32) -> usize {
        let distance = |cell: &Cell| {
            let (cx, cy) = ((cell.hit.left + cell.hit.right) / 2.0, (cell.hit.top + cell.hit.bottom) / 2.0);
            (x - cx).powi(2) + (y - cy).powi(2)
        };
        let nearest = self.cells.iter().enumerate().min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)));
        match nearest {
            Some((k, cell)) if x < (cell.icon.left + cell.icon.right) / 2.0 => k,
            Some((k, _)) => k + 1,
            None => 0,
        }
    }

    /// Whether a screen point is on the pop-up's panel (a little slack round it).
    pub fn covers(&self, x: i32, y: i32) -> bool {
        let slack = 4.0;
        let (x, y) = ((x - self.x) as f32, (y - self.y) as f32);
        x >= self.panel.left - slack && x < self.panel.right + slack && y >= self.panel.top - slack && y < self.panel.bottom + slack
    }

    /// A highlighted item's background: round its icon and its name as tall as the name is
    /// (`name_h`, measured), never shorter than the icon needs.
    pub fn highlight(&self, cell: &Cell, name_h: f32) -> Rect {
        let bottom = (cell.label.top + name_h + self.margin).max(cell.icon.bottom + self.margin);
        Rect::new(cell.hit.left, cell.hit.top, cell.hit.right, bottom.round())
    }
}

// ---- When it closes ----------------------------------------------------------------------------

/// Why the pop-up closed (for the log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Esc,
    ClickedOutside,
    PointerLeft,
}

/// What the dock sees on a look (every ~25 ms while a group is open).
#[derive(Debug, Clone, Copy, Default)]
pub struct Seen {
    /// The pointer is on the pop-up, on the group's icon, or in between.
    pub inside: bool,
    /// A mouse button is down somewhere else.
    pub button_outside: bool,
    pub escape: bool,
}

/// The rules for closing a pop-up that never has the keyboard focus. Launching an item,
/// clicking the group's icon again and the dock hiding close it directly (dock.rs); this
/// covers the rest: Esc, a click anywhere else, or the pointer staying away for a moment.
#[derive(Debug)]
pub struct Closer {
    away_since: Option<Instant>,
    grace: Duration,
}

impl Closer {
    pub fn new(grace: Duration) -> Self {
        Self { away_since: None, grace }
    }

    pub fn update(&mut self, seen: Seen, now: Instant) -> Option<Why> {
        if seen.escape {
            return Some(Why::Esc);
        }
        if seen.button_outside {
            return Some(Why::ClickedOutside);
        }
        if seen.inside {
            self.away_since = None;
            return None;
        }
        let since = *self.away_since.get_or_insert(now);
        (now.duration_since(since) >= self.grace).then_some(Why::PointerLeft)
    }
}

/// A closing pop-up first stays a moment (after a drop, so you see where the icon landed), then
/// fades out. How see-through it is at `elapsed` since it started closing: 1 = solid, 0 = gone.
pub fn fade(elapsed: Duration, hold: Duration, out: Duration) -> f32 {
    if elapsed <= hold {
        return 1.0;
    }
    if out.is_zero() {
        return 0.0;
    }
    (1.0 - (elapsed - hold).as_secs_f32() / out.as_secs_f32()).clamp(0.0, 1.0)
}

// ---- Drawing -----------------------------------------------------------------------------------

/// One item as the pop-up shows it.
pub struct Shown<'a> {
    pub icon: Option<&'a ID2D1Bitmap>,
    pub label: &'a [u16],
    /// Its program is missing (dimmed, like on the dock).
    pub dim: bool,
    /// It's being dragged elsewhere: only a faint trace stays.
    pub ghost: bool,
}

/// The pop-up's fonts: names (wrapping onto two lines, cut short with "…"), names wrapping in
/// full (the pointed-at one, if it's longer), and the group's name.
pub struct Fonts<'a> {
    pub name: &'a IDWriteTextFormat,
    pub wrapped: &'a IDWriteTextFormat,
    pub title: &'a IDWriteTextFormat,
}

/// What's going on in the pop-up.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    pub hovered: Option<usize>,
    /// A folder's button under the pointer.
    pub button: Option<Button>,
    /// The pointer is on the scroll bar (it widens a little and its lane shows), or is dragging
    /// its thumb.
    pub bar_hot: bool,
    pub bar_dragged: bool,
    /// Where a dragged icon would land (a gap, as `gap_at`).
    pub marker: Option<usize>,
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn rounded(r: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius }
}

/// Draws the pop-up into the render target (already begun, cleared, window-sized). `measure`
/// gives the height a name takes in the `wrapped` font at a width.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    rt: &ID2D1DCRenderTarget,
    brush: &ID2D1SolidColorBrush,
    fonts: &Fonts,
    layout: &Layout,
    title: &[u16],
    items: &[Shown],
    state: State,
    scale: f32,
    measure: &dyn Fn(&[u16], f32) -> f32,
    note: Option<&[u16]>,
) {
    let no_stroke = None::<&ID2D1StrokeStyle>;
    let panel = layout.panel;
    unsafe {
        // Soft shadow all round, then a dark panel (names stay readable on any wallpaper) with a
        // light edge, like the dock's.
        let rings = layout.shadow as i32;
        for d in 1..=rings {
            let t = (d as f32 - 0.5) / layout.shadow;
            brush.SetColor(&rgba(0.0, 0.0, 0.0, 0.30 * (1.0 - t) * (1.0 - t)));
            let grow = d as f32 - 0.5;
            let ring = Rect::new(panel.left - grow, panel.top - grow, panel.right + grow, panel.bottom + grow);
            rt.DrawRoundedRectangle(&rounded(ring, layout.radius + grow), brush, 1.0, no_stroke);
        }
        // Nearly solid: what's behind mustn't show through the names.
        brush.SetColor(&rgba(0.10, 0.11, 0.13, 0.97));
        rt.FillRoundedRectangle(&rounded(panel, layout.radius), brush);
        brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.55));
        let edge = Rect::new(panel.left + 0.5, panel.top + 0.5, panel.right - 0.5, panel.bottom - 0.5);
        rt.DrawRoundedRectangle(&rounded(edge, layout.radius - 0.5), brush, 1.0, no_stroke);

        brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.95));
        rt.DrawText(title, fonts.title, &layout.title.d2d(), brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        // A folder's buttons: the one under the pointer lights (its lines are drawn by the dock).
        if let Some((_, rect)) = layout.buttons.iter().find(|(b, _)| Some(*b) == state.button) {
            brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.14));
            rt.FillRoundedRectangle(&rounded(*rect, 5.0 * scale), brush);
        }
        // Why there's nothing to show (an empty folder, one that can't be reached), in the middle
        // of its room, wrapping if it's long.
        if let Some(note) = note {
            let view = layout.view;
            let area = Rect::new(view.left + layout.margin, view.top, view.right - layout.margin, view.bottom);
            let h = measure(note, area.width()).min(area.height());
            let top = (view.top + (area.height() - h) / 2.0).round();
            brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.65));
            let at = Rect::new(area.left, top, area.right, view.bottom);
            rt.DrawText(note, fonts.wrapped, &at.d2d(), brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
        // The items, scrolled, showing only within their view.
        let scrolls = layout.max_scroll > 0.0;
        if scrolls {
            rt.PushAxisAlignedClip(&layout.view.d2d(), D2D1_ANTIALIAS_MODE_ALIASED);
            rt.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: -layout.scroll.round() });
        }
        for (k, (cell, item)) in layout.cells.iter().zip(items).enumerate() {
            let on = state.hovered == Some(k) && !item.ghost;
            // The pointed-at item's highlight hugs its name: a short name a short highlight, a
            // name longer than two lines shown in full, the highlight growing down for it.
            let name_h = if on { measure(item.label, cell.label.width()) } else { 0.0 };
            let two_lines = cell.label.bottom - cell.label.top;
            if on {
                brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.16));
                rt.FillRoundedRectangle(&rounded(layout.highlight(cell, name_h), 8.0 * scale), brush);
            }
            let opacity = if item.ghost { 0.2 } else if item.dim { 0.4 } else { 1.0 };
            match item.icon {
                Some(bitmap) => rt.DrawBitmap(bitmap, Some(&cell.icon.d2d()), opacity, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None),
                None => {
                    brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.25));
                    rt.FillRoundedRectangle(&rounded(cell.icon, cell.icon.width() * 0.2), brush);
                }
            }
            if item.ghost {
                continue;
            }
            brush.SetColor(&rgba(1.0, 1.0, 1.0, if item.dim { 0.6 } else if on { 1.0 } else { 0.92 }));
            let (font, area) = if on && name_h > two_lines + 1.0 {
                (fonts.wrapped, Rect::new(cell.label.left, cell.label.top, cell.label.right, cell.label.top + name_h))
            } else {
                (fonts.name, cell.label)
            };
            rt.DrawText(item.label, font, &area.d2d(), brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
        if scrolls {
            rt.SetTransform(&Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 });
            rt.PopAxisAlignedClip();
        }
        // The scroll bar: a thin rounded thumb, faint until the pointer is on it; then a little
        // wider and brighter, with its lane showing faintly behind.
        if let (Some(bar), Some((top, bottom))) = (layout.bar, layout.thumb()) {
            let hot = state.bar_hot || state.bar_dragged;
            let w = ((if hot { 6.0 } else { 4.0 }) * scale).round();
            if hot {
                let lane_w = w + (2.0 * scale).round();
                let lane = Rect::new(bar.x - lane_w / 2.0, bar.top - scale, bar.x + lane_w / 2.0, bar.bottom + scale);
                brush.SetColor(&rgba(1.0, 1.0, 1.0, 0.07));
                rt.FillRoundedRectangle(&rounded(lane, lane_w / 2.0), brush);
            }
            let alpha = if state.bar_dragged { 0.62 } else if hot { 0.48 } else { 0.30 };
            brush.SetColor(&rgba(1.0, 1.0, 1.0, alpha));
            let thumb = Rect::new(bar.x - w / 2.0, top, bar.x + w / 2.0, bottom);
            rt.FillRoundedRectangle(&rounded(thumb, w / 2.0), brush);
        }
        // Where a dragged icon would land: a bright bar between two items.
        if let Some(gap) = state.marker {
            let at = if gap < layout.cells.len() { layout.cells.get(gap).map(|c| (c.hit.left, c)) } else { layout.cells.last().map(|c| (c.hit.right, c)) };
            if let Some((x, cell)) = at {
                let w = (3.0 * scale).round();
                let bar = Rect::new(x - w / 2.0, cell.icon.top - 4.0 * scale, x + w / 2.0, cell.icon.bottom + 4.0 * scale);
                brush.SetColor(&rgba(0.55, 0.78, 1.0, 1.0));
                rt.FillRoundedRectangle(&rounded(bar, w / 2.0), brush);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(count: usize) -> Params {
        Params { count, icon: 80.0, scale: 1.5, line_h: 27.0, anchor_x: 1900.0, top: 200.0, screen_left: 0.0, screen_right: 3840.0, screen_top: 0.0, screen_bottom: 2160.0, edge: Edge::Top, folder: None }
    }

    #[test]
    fn a_big_group_grows_wider_instead_of_off_the_bottom_of_the_screen() {
        // A 1080-line screen at 150%: about four rows fit under the dock.
        let short = |count| Params { screen_bottom: 1080.0, screen_right: 1920.0, anchor_x: 960.0, ..params(count) };
        for count in [13, 20, 30] {
            let l = layout(&short(count));
            assert!(l.y + l.height <= 1080, "{count} items: bottom at {} on a 1080-line screen", l.y + l.height);
            assert!(l.x >= 0 && l.x + l.width <= 1920, "{count} items: still on the screen");
            assert_eq!(l.cells.len(), count);
        }
        // Too many to fit at all: as wide as the screen allows, then it does run off.
        let huge = layout(&short(200));
        assert!(huge.width <= 1920 && huge.cells.len() == 200);
        // On a tall screen nothing changes: about square.
        let tall = layout(&params(13));
        let tops: std::collections::BTreeSet<i32> = tall.cells.iter().map(|c| c.icon.top as i32).collect();
        assert_eq!(tops.len(), 4, "13 items: 4 x 4 as before");
    }

    #[test]
    fn a_folder_is_at_most_as_wide_as_chosen_and_scrolls_past_its_rows() {
        let sized = |count, back, max_columns, max_rows| {
            layout(&Params { folder: Some(FolderLook { back, max_columns, max_rows, at_least: (0.0, 0.0), note: false }), ..params(count) })
        };
        let folder = |count, back| sized(count, back, 4, 5);
        let grid = |l: &Layout| {
            let lefts: std::collections::BTreeSet<i32> = l.cells.iter().map(|c| c.icon.left as i32).collect();
            let tops: std::collections::BTreeSet<i32> = l.cells.iter().map(|c| c.icon.top as i32).collect();
            (lefts.len(), tops.len())
        };
        let big = folder(140, true);
        assert_eq!((big.cells.len(), grid(&big), big.shown_rows()), (140, (4, 35), 5), "4 across, 35 rows, 5 of them showing");
        assert!((big.max_scroll - 30.0 * big.row_h).abs() < 0.5);
        assert!(big.y + big.height <= 2160, "on the screen");
        // Fewer items, about square below that; another size when chosen.
        assert_eq!(grid(&folder(3, false)), (2, 2));
        assert_eq!(grid(&folder(9, false)), (3, 3));
        let wide = sized(140, false, 6, 3);
        assert_eq!((grid(&wide).0, wide.shown_rows()), (6, 3), "6 across, 3 rows in sight");
        // What fits a grid fills it without scrolling: 18 at 6 x 3 (a square 5 would need a
        // 4th row), 20 at 4 x 5; fewer rows than chosen stay about square.
        for (count, columns, rows, want) in [(18, 6, 3, (6, 3)), (20, 4, 5, (4, 5)), (7, 4, 2, (4, 2)), (7, 4, 5, (3, 3))] {
            let l = sized(count, false, columns, rows);
            assert_eq!((grid(&l), l.max_scroll), (want, 0.0), "{count} items at {columns} x {rows}");
        }
        let kinds: Vec<Button> = big.buttons.iter().map(|(b, _)| *b).collect();
        assert_eq!(kinds, [Button::Back, Button::Explore]);
        // Its scroll bar: right of the last column, the thumb at the top and as long as the share
        // in sight (5 rows of 35).
        let bar = big.bar.expect("a scroll bar");
        let last = big.cells[3].hit;
        assert!(bar.lane.left >= last.right && bar.lane.right <= big.panel.right, "in a lane of its own");
        let (top, bottom) = big.thumb().unwrap();
        assert_eq!(top, bar.top);
        assert!(((bottom - top) - ((bar.bottom - bar.top) * 5.0 / 35.0).max(bar.least)).abs() < 1.0);
        assert_eq!(big.bar_at(bar.x, (top + bottom) / 2.0), Some(BarPart::Thumb));
        assert_eq!(big.bar_at(bar.x, bottom + 5.0), Some(BarPart::Below));
        assert_eq!(big.bar_at(last.right - 5.0, top + 5.0), None, "the items aren't the bar");
        // Dragged to the bottom: scrolled to the end; halfway: about halfway.
        assert_eq!(big.scroll_for_thumb(bar.bottom), big.max_scroll);
        let mut end = big.clone();
        end.scroll = end.max_scroll;
        assert_eq!(end.thumb().unwrap().1, bar.bottom, "at the end, the thumb is at the bottom");
        assert_eq!(end.bar_at(bar.x, top + 1.0), Some(BarPart::Above));
        let run = (bar.bottom - bar.top) - (bottom - top);
        assert!((big.scroll_for_thumb(bar.top + run / 2.0) - big.max_scroll / 2.0).abs() < 1.0);
        // The pointer on the first row's icon: that item, then a row on, the one below it.
        let first = big.cells[0].icon;
        let (x, y) = ((first.left + first.right) / 2.0, (first.top + first.bottom) / 2.0);
        assert_eq!(big.hit(x, y), Some(0));
        let mut scrolled = big.clone();
        scrolled.scroll = scrolled.scrolled(0.0, 1);
        assert_eq!(scrolled.hit(x, y), Some(4));
        let hidden = big.cells[30].icon;
        assert_eq!(big.hit((hidden.left + hidden.right) / 2.0, (hidden.top + hidden.bottom) / 2.0), None, "a row out of sight isn't hit");
        assert_eq!(big.scrolled(0.0, -3), 0.0);
        assert_eq!(big.scrolled(0.0, 100), big.max_scroll, "never past the end");
        assert_eq!(big.scrolled(big.row_h * 0.4, 1), big.row_h, "whole rows");
        // A small folder neither scrolls nor has a scroll bar; the back button is only in a
        // subfolder.
        let small = folder(14, false);
        assert_eq!((small.max_scroll, small.bar), (0.0, None));
        assert_eq!(small.buttons.iter().map(|(b, _)| *b).collect::<Vec<_>>(), [Button::Explore]);
        assert!(small.title.left > small.buttons[0].1.right || small.title.right < small.buttons[0].1.left, "the title leaves the button room");
        // A group of 140 is as before: wider, never scrolled, no buttons.
        let group = layout(&params(140));
        assert_eq!((group.max_scroll, group.buttons.len()), (0.0, 0));
        assert!(group.width > big.width);
    }

    #[test]
    fn going_back_in_a_folder_never_shrinks_it_from_under_the_pointer() {
        let folder = |count, back, at_least| layout(&Params { folder: Some(FolderLook { back, max_columns: 4, max_rows: 5, at_least, note: false }), ..params(count) });
        // Into a 140-item subfolder of a 14-item folder: it grows (taller, and its scroll bar's
        // lane wider).
        let project = folder(14, false, (0.0, 0.0));
        let invoices = folder(140, true, (project.panel.width(), project.panel.height()));
        assert!(invoices.width > project.width && invoices.height > project.height);
        // Back out of it: the pointer, still on the back button, is still on the pop-up.
        let back = invoices.buttons.iter().find(|(b, _)| *b == Button::Back).unwrap().1;
        let (x, y) = (invoices.x as f32 + back.left + 1.0, invoices.y as f32 + back.top + 1.0);
        let again = folder(14, false, (invoices.panel.width(), invoices.panel.height()));
        assert_eq!((again.x, again.y, again.width, again.height), (invoices.x, invoices.y, invoices.width, invoices.height));
        let (wx, wy) = (x - again.x as f32, y - again.y as f32);
        assert!(again.panel.contains(wx, wy), "the pointer stays on it");
        // Its 14 are 4 across as before, and nothing scrolls.
        let tops: std::collections::BTreeSet<i32> = again.cells.iter().map(|c| c.icon.top as i32).collect();
        assert_eq!((tops.len(), again.max_scroll), (4, 0.0));
        // A folder opened afresh from the dock is its own size again.
        assert_eq!(folder(14, false, (0.0, 0.0)), project);
    }

    #[test]
    fn a_message_alone_gets_room_for_two_lines_not_for_tiles() {
        let look = |note, at_least| Some(FolderLook { back: false, max_columns: 4, max_rows: 5, at_least, note });
        let note = layout(&Params { folder: look(true, (0.0, 0.0)), ..params(0) });
        let tiles = layout(&Params { folder: look(false, (0.0, 0.0)), ..params(3) });
        assert!(note.cells.is_empty() && note.bar.is_none() && note.max_scroll == 0.0);
        assert_eq!(note.view.bottom - note.view.top, (2.0 * 27.0 + 2.0 * note.margin).round(), "two lines and a little room");
        assert!(note.height < tiles.height - 100, "much shorter than room for tiles ({} vs {})", note.height, tiles.height);
        assert_eq!(note.width, tiles.width, "two columns wide, room for a short sentence on a line");
        assert!(note.panel.bottom - note.view.bottom < note.view.top - note.panel.top, "no empty room below it");
        // Into an empty subfolder from a big folder: it keeps the size it had.
        let big = layout(&Params { folder: look(false, (0.0, 0.0)), ..params(140) });
        let empty = layout(&Params { folder: look(true, (big.panel.width(), big.panel.height())), ..params(0) });
        assert_eq!((empty.width, empty.height), (big.width, big.height));
    }

    #[test]
    fn a_grid_is_about_square_and_names_every_item() {
        for (count, columns, rows) in [(1, 1, 1), (2, 2, 1), (4, 2, 2), (5, 3, 2), (13, 4, 4), (40, 6, 7)] {
            let l = layout(&params(count));
            let lefts: std::collections::BTreeSet<i32> = l.cells.iter().map(|c| c.icon.left as i32).collect();
            let tops: std::collections::BTreeSet<i32> = l.cells.iter().map(|c| c.icon.top as i32).collect();
            assert_eq!((lefts.len(), tops.len()), (columns, rows), "{count} items");
            for cell in &l.cells {
                assert!(cell.label.top >= cell.icon.bottom, "names under icons");
                assert!(l.panel.contains(cell.icon.left, cell.icon.top) && cell.label.bottom <= l.panel.bottom);
                assert!(cell.label.bottom <= cell.hit.bottom, "two lines of name fit in the cell");
            }
        }
    }

    #[test]
    fn a_highlight_hugs_its_name() {
        let l = layout(&params(4));
        let cell = l.cells[0];
        let one_line = l.highlight(&cell, 27.0);
        let two_lines = l.highlight(&cell, 54.0);
        let five_lines = l.highlight(&cell, 135.0);
        assert!(one_line.bottom < two_lines.bottom && two_lines.bottom <= cell.hit.bottom + 0.5, "short names, short highlights");
        assert!(five_lines.bottom > cell.hit.bottom, "a very long name is shown in full");
        assert!(l.highlight(&cell, 0.0).bottom > cell.icon.bottom + l.margin, "never shorter than the icon needs");
    }

    #[test]
    fn it_hangs_under_the_group_but_stays_on_the_screen() {
        let l = layout(&params(9));
        let middle = l.x as f32 + l.width as f32 / 2.0;
        assert!((middle - 1900.0).abs() <= 1.0, "centred under the group");
        assert_eq!(l.y as f32 + l.panel.top, 200.0, "the panel starts where asked");
        for anchor in [0.0, 30.0, 3830.0, 3840.0] {
            let mut p = params(9);
            p.anchor_x = anchor;
            let l = layout(&p);
            assert!(l.x >= 0 && l.x + l.width <= 3840, "anchor {anchor}: {} + {}", l.x, l.width);
        }
    }

    #[test]
    fn on_every_edge_it_opens_away_from_the_dock_and_stays_inside_the_work_area() {
        // A 1920 x 1080 screen at 150%, the dock's icons reaching 150 pixels from its edge.
        let screen = |edge, anchor, top, count| Params {
            count,
            anchor_x: anchor,
            top,
            edge,
            screen_left: 0.0,
            screen_right: 1920.0,
            screen_top: 0.0,
            screen_bottom: 1080.0,
            ..params(count)
        };
        for count in [1, 5, 13, 30] {
            let bottom = layout(&screen(Edge::Bottom, 960.0, 1080.0 - 150.0, count));
            assert_eq!(bottom.y as f32 + bottom.panel.bottom, 930.0, "{count} items: on the bottom, the panel ends where asked");
            assert!(bottom.y >= 0 && bottom.x >= 0 && bottom.x + bottom.width <= 1920, "{count} items: on the screen");
            let left = layout(&screen(Edge::Left, 540.0, 150.0, count));
            assert_eq!(left.x as f32 + left.panel.left, 150.0, "{count} items: on the left, the panel starts where asked");
            assert!(left.y >= 0 && left.y + left.height <= 1080 && left.x + left.width <= 1920, "{count} items: on the screen");
            let right = layout(&screen(Edge::Right, 540.0, 1920.0 - 150.0, count));
            assert_eq!(right.x as f32 + right.panel.right, 1770.0, "{count} items: on the right, the panel ends where asked");
            assert!(right.y >= 0 && right.y + right.height <= 1080 && right.x >= 0, "{count} items: on the screen");
        }
        // On a side it's centred on the group's icon up and down, but kept on the screen.
        let middle = layout(&screen(Edge::Left, 540.0, 150.0, 4));
        assert!(((middle.y as f32 + middle.height as f32 / 2.0) - 540.0).abs() <= 1.0);
        for anchor in [0.0, 1080.0] {
            let l = layout(&screen(Edge::Right, anchor, 1770.0, 9));
            assert!(l.y >= 0 && l.y + l.height <= 1080, "anchor {anchor}: {} + {}", l.y, l.height);
        }
    }

    #[test]
    fn every_cell_is_hit_at_its_icon_and_nowhere_else_overlaps() {
        let l = layout(&params(11));
        for (k, cell) in l.cells.iter().enumerate() {
            let (x, y) = ((cell.icon.left + cell.icon.right) / 2.0, (cell.icon.top + cell.icon.bottom) / 2.0);
            assert_eq!(l.hit(x, y), Some(k), "item {k}");
        }
        assert_eq!(l.hit(1.0, 1.0), None, "the shadow isn't an item");
    }

    #[test]
    fn a_dragged_icon_goes_beside_the_nearest_item() {
        let l = layout(&params(7));
        let middle = |cell: Cell| ((cell.icon.left + cell.icon.right) / 2.0, (cell.icon.top + cell.icon.bottom) / 2.0);
        let (x, y) = middle(l.cells[2]);
        assert_eq!(l.gap_at(x - 5.0, y), 2, "left half: before it");
        assert_eq!(l.gap_at(x + 5.0, y), 3, "right half: after it");
        let (x, y) = middle(l.cells[6]);
        assert_eq!(l.gap_at(x + 30.0, y), 7, "past the last: at the end");
        let (x, y) = middle(l.cells[0]);
        assert_eq!(l.gap_at(x - 30.0, y), 0, "before the first");
    }

    #[test]
    fn covers_is_the_panel_on_the_screen() {
        let l = layout(&params(4));
        let (x, y) = (l.x + l.panel.left as i32 + 10, l.y + l.panel.top as i32 + 10);
        assert!(l.covers(x, y));
        assert!(!l.covers(l.x - 50, y));
    }

    #[test]
    fn a_closing_pop_up_stays_a_moment_then_fades() {
        let ms = Duration::from_millis;
        assert_eq!(fade(ms(0), ms(500), ms(200)), 1.0);
        assert_eq!(fade(ms(500), ms(500), ms(200)), 1.0, "held");
        assert!((fade(ms(600), ms(500), ms(200)) - 0.5).abs() < 0.01, "halfway out");
        assert_eq!(fade(ms(700), ms(500), ms(200)), 0.0, "gone");
        assert_eq!(fade(ms(1), ms(0), ms(0)), 0.0, "no motion: gone at once");
    }

    #[test]
    fn closing_rules() {
        let start = Instant::now();
        let ms = |n| start + Duration::from_millis(n);
        let inside = Seen { inside: true, ..Default::default() };
        let away = Seen::default();

        let mut closer = Closer::new(Duration::from_millis(400));
        assert_eq!(closer.update(inside, ms(0)), None);
        assert_eq!(closer.update(Seen { escape: true, inside: true, ..Default::default() }, ms(10)), Some(Why::Esc));

        let mut closer = Closer::new(Duration::from_millis(400));
        assert_eq!(closer.update(Seen { button_outside: true, ..Default::default() }, ms(0)), Some(Why::ClickedOutside), "at once");

        let mut closer = Closer::new(Duration::from_millis(400));
        assert_eq!(closer.update(away, ms(0)), None, "the pointer left: a moment's grace");
        assert_eq!(closer.update(away, ms(300)), None);
        assert_eq!(closer.update(inside, ms(350)), None, "came back in time");
        assert_eq!(closer.update(away, ms(700)), None, "the grace starts over");
        assert_eq!(closer.update(away, ms(1099)), None);
        assert_eq!(closer.update(away, ms(1100)), Some(Why::PointerLeft));
    }
}
