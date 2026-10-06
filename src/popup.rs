//! A group's pop-up: the window that opens below a group's icon, showing its items in a grid
//! (phone-folder style: every name shown, the group's name on top). A one-row "strip" was built
//! too, to compare by feel; the grid was kept.
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
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_ROUNDED_RECT, ID2D1Bitmap, ID2D1DCRenderTarget,
    ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{DWRITE_MEASURING_MODE_NATURAL, IDWriteTextFormat};

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
    /// The middle of the group's icon, where the pop-up hangs from.
    pub anchor_x: f32,
    /// Where the pop-up's panel starts (just below how far the dock's icons reach).
    pub top: f32,
    /// The screen's left and right edges, and how far down the pop-up may reach (above a
    /// taskbar at the bottom).
    pub screen_left: f32,
    pub screen_right: f32,
    pub screen_bottom: f32,
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
    // Rows that fit between the pop-up's top and the screen's bottom, and columns across it.
    let room_down = p.screen_bottom - screen_margin - (p.top - shadow) - 2.0 * shadow - pad - title_h;
    let fit_rows = ((room_down / cell_h).floor() as usize).max(1);
    let fit_columns = (((p.screen_right - p.screen_left - 2.0 * screen_margin - 2.0 * shadow - 2.0 * pad) / cell_w).floor() as usize).max(1);
    let columns = square.max(count.div_ceil(fit_rows)).min(fit_columns.max(square)).min(count);
    let rows = count.div_ceil(columns);
    let panel_w = 2.0 * pad + columns as f32 * cell_w;
    let panel_h = pad * 0.5 + title_h + rows as f32 * cell_h + pad * 0.5;
    let cells = (0..p.count)
        .map(|k| {
            let (row, column) = (k / columns, k % columns);
            let left = shadow + pad + column as f32 * cell_w;
            let top = shadow + pad * 0.5 + title_h + row as f32 * cell_h;
            let icon_left = (left + (cell_w - p.icon) / 2.0).round();
            let icon = Rect::new(icon_left, top + margin, icon_left + p.icon, top + margin + p.icon);
            let label = Rect::new(left + 4.0 * s, icon.bottom + label_gap, left + cell_w - 4.0 * s, icon.bottom + label_gap + 2.0 * p.line_h);
            Cell { hit: Rect::new(left, top, left + cell_w, top + cell_h), icon, label }
        })
        .collect();
    let title = Rect::new(shadow + pad, shadow + pad * 0.5, shadow + panel_w - pad, shadow + pad * 0.5 + title_h);
    let width = (panel_w + 2.0 * shadow).ceil();
    let height = (panel_h + 2.0 * shadow).ceil();
    // Centred under the group's icon, but always fully on the screen.
    let lowest = p.screen_left + screen_margin;
    let highest = (p.screen_right - screen_margin - width).max(lowest);
    let x = (p.anchor_x - width / 2.0).clamp(lowest, highest).round() as i32;
    let y = (p.top - shadow).round() as i32;
    let panel = Rect::new(shadow, shadow, shadow + panel_w, shadow + panel_h);
    Layout { x, y, width: width as i32, height: height as i32, panel, title, cells, radius, shadow, margin }
}

impl Layout {
    /// Which item is at (x, y) in the window, if any.
    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.cells.iter().position(|cell| cell.hit.contains(x, y))
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
        Params { count, icon: 80.0, scale: 1.5, line_h: 27.0, anchor_x: 1900.0, top: 200.0, screen_left: 0.0, screen_right: 3840.0, screen_bottom: 2160.0 }
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
