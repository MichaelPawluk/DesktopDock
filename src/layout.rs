//! Dock geometry. Pure arithmetic with no Windows calls, so it can be unit-tested.
//! Sizes are physical pixels; window coordinates start at the dock window's top-left corner.

use crate::config::{DockSettings, Edge};
use crate::motion::ease_out;

/// A horizontal slot on the dock: an icon (icon width + gap) or a separator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub item: usize,
    pub left: f32,
    pub width: f32,
    pub separator: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Geometry {
    pub scale: f32,
    pub icon: f32,
    pub hover_icon: f32,
    pub gap: f32,
    pub pad_top: f32,
    /// Window x where the first slot starts.
    pub origin: f32,
    pub panel_left: f32,
    pub panel_w: f32,
    /// How far the panel reaches from the screen edge (its bottom).
    pub panel_h: f32,
    /// Where a floating panel starts below the screen edge; 0 for one hanging from it.
    pub panel_top: f32,
    pub radius: f32,
    pub shadow: f32,
    pub label_h: f32,
    pub bounce: f32,
    /// The "Removed X · Undo" strip under the panel: its top and height.
    pub strip_top: f32,
    pub strip_h: f32,
    /// The window's size along the edge and across it (its width and height on the top edge).
    pub win_w: i32,
    pub win_h: i32,
    /// The edge the dock is on (everything above is as if on the top edge: see `Frame`).
    pub edge: Edge,
    /// On a side, how far a name beside an icon may reach (cut short beyond); 0 on top or bottom.
    pub label_w: f32,
    /// On a side, how wide the strip beside the panel may be; 0 on top or bottom.
    pub strip_w: f32,
}

impl Geometry {
    /// From the dock's own directions to the window's (see `Frame`).
    pub fn frame(&self) -> Frame {
        Frame { edge: self.edge, across: self.win_h as f32 }
    }

    /// The window's width and height on the screen.
    pub fn window_size(&self) -> (i32, i32) {
        self.frame().window_size(self.win_w, self.win_h)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub geo: Geometry,
    pub slots: Vec<Slot>,
}

/// Lays the dock out for a monitor at `dpi`. `separators[i]` says whether item `i` is a separator.
/// If the panel would be wider than `max_panel_width` (the screen, less a small margin), icons,
/// gaps and padding shrink together, as little as possible, until it fits (like RocketDock).
pub fn compute(settings: &DockSettings, separators: &[bool], dpi: u32, max_panel_width: Option<f32>) -> Layout {
    let scale = dpi.max(48) as f32 / 96.0;
    let px = |v: f32| (v * scale).round();

    let icons = separators.iter().filter(|separator| !**separator).count() as f32;
    let separator_count = separators.len() as f32 - icons;
    let natural = |icon: f32, gap: f32, pad: f32, sep: f32| {
        (2.0 * pad - gap + icons * (icon + gap) + separator_count * sep).max(2.0 * pad)
    };

    let mut icon = px(settings.icon_size).max(8.0);
    let mut gap = px(settings.spacing);
    let mut pad_x = px(12.0);
    let mut pad_top = px(8.0);
    let mut pad_bottom = px(10.0);
    let mut sep_w = px(16.0);
    if let Some(max) = max_panel_width.filter(|max| *max > 0.0) {
        let full = natural(icon, gap, pad_x, sep_w);
        if full > max {
            let f = max / full;
            let shrink = |v: f32| (v * f).floor();
            icon = shrink(icon).max(8.0);
            gap = shrink(gap);
            pad_x = shrink(pad_x);
            pad_top = shrink(pad_top);
            pad_bottom = shrink(pad_bottom);
            sep_w = shrink(sep_w).max(4.0);
            // Whole pixels: make icons as big as still fits. (With no icons, only separators,
            // the width doesn't depend on the icon size: no loop, or it would never end.)
            while icons > 0.0 && natural(icon + 1.0, gap, pad_x, sep_w) <= max && icon < 1024.0 {
                icon += 1.0;
            }
            while natural(icon, gap, pad_x, sep_w) > max && icon > 8.0 {
                icon -= 1.0;
            }
        }
    }
    // A floating panel (the capsule) starts a little below the screen edge: everything on it moves
    // down by that much, and the window reaches that much further.
    let skin = crate::skin::skin(settings.style, settings.look, false);
    let panel_top = px(skin.float);
    pad_top += panel_top;
    let hover_icon = px(settings.zoom_size).max(icon);
    let shadow = px(13.0);
    let label_h = if settings.show_labels { px(24.0 * settings.label_size / 12.5).round() } else { 0.0 };
    let bounce = px(12.0);

    let mut slots = Vec::with_capacity(separators.len());
    let mut x = 0.0;
    for (item, &separator) in separators.iter().enumerate() {
        let width = if separator { sep_w } else { icon + gap };
        slots.push(Slot { item, left: x, width, separator });
        x += width;
    }
    let panel_w = (2.0 * pad_x - gap + x).max(2.0 * pad_x);
    let panel_h = pad_top + icon + pad_bottom;
    let side = settings.edge.is_side();
    // On top or bottom, names go under the icons (room at the ends for the outer ones' names);
    // on a side, beside them, cut short beyond `label_w`, and the strip goes beside the panel.
    let margin_x = shadow.max(px(70.0));
    let (label_w, strip_w) = if side { (px(160.0), px(300.0)) } else { (0.0, 0.0) };
    // The strip goes past every icon's name when those always show.
    let names = if settings.show_labels && settings.always_show_labels { if side { px(8.0) + label_w } else { label_h } } else { 0.0 };
    let (strip_top, strip_h) = (panel_h + px(6.0) + names, px(28.0));
    let below = if side {
        ((hover_icon - icon - pad_bottom).max(0.0) + bounce + px(8.0) + label_w + shadow).max(strip_top + strip_w + shadow - panel_h)
    } else {
        ((hover_icon - icon - pad_bottom).max(0.0) + bounce + label_h + shadow).max(strip_top + strip_h + shadow - panel_h)
    };

    let geo = Geometry {
        scale,
        icon,
        hover_icon,
        gap,
        pad_top,
        origin: margin_x + pad_x - gap / 2.0,
        panel_left: margin_x,
        panel_w,
        panel_h,
        panel_top,
        // Round ends: half the panel's own height.
        radius: skin.radius.map_or(((panel_h - panel_top) / 2.0).floor(), px),
        shadow,
        label_h,
        bounce,
        strip_top,
        strip_h,
        win_w: (panel_w + 2.0 * margin_x) as i32,
        win_h: (panel_h + below).ceil() as i32,
        edge: settings.edge,
        label_w,
        strip_w,
    };
    Layout { geo, slots }
}

impl Geometry {
    /// How far below the screen edge the icons reach, counting a fully grown hovered icon.
    pub fn reach(&self) -> f32 {
        self.panel_h.max(self.pad_top + self.hover_icon)
    }

    /// How far everything moves up to slide completely out of view.
    pub fn travel(&self) -> f32 {
        if self.edge.is_side() {
            return self.win_h as f32; // names and the strip beside the panel: the whole depth
        }
        (self.panel_h + self.shadow + self.label_h + self.bounce + self.hover_icon).max(self.strip_top + self.strip_h + self.shadow)
    }

    /// Vertical offset for a reveal amount (1 = shown, 0 = hidden), in whole pixels.
    pub fn slide_offset(&self, reveal: f32) -> f32 {
        (-(1.0 - ease_out(reveal)) * self.travel()).round()
    }
}

impl Slot {
    pub fn center(&self, g: &Geometry) -> f32 {
        g.origin + self.left + self.width / 2.0
    }

    fn contains_x(&self, g: &Geometry, x: f32) -> bool {
        x >= g.origin + self.left && x < g.origin + self.left + self.width
    }
}

impl Layout {
    /// Where the window starts along the edge, to centre the dock between `from` and `to` (a
    /// screen's left and right, or its top and bottom on a side).
    pub fn window_x(&self, from: i32, to: i32) -> i32 {
        from + ((to - from) - self.geo.win_w) / 2
    }

    /// The icon under a point in window coordinates. Separators and empty space give None.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        if !(0.0..=self.geo.reach()).contains(&y) {
            return None;
        }
        self.slots
            .iter()
            .filter(|slot| !slot.separator)
            .find(|slot| slot.contains_x(&self.geo, x))
            .map(|slot| slot.item)
    }

    /// The separator under a point in window coordinates: separators can be picked up, moved and
    /// removed like icons, though they never zoom or open anything.
    pub fn separator_at(&self, x: f32, y: f32) -> Option<usize> {
        if !(0.0..=self.geo.panel_h).contains(&y) {
            return None;
        }
        self.slots.iter().filter(|slot| slot.separator).find(|slot| slot.contains_x(&self.geo, x)).map(|slot| slot.item)
    }

    pub fn slot_of(&self, item: usize) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.item == item)
    }

    /// Dragging `dragged` along the dock with the pointer at window x: the gap it would drop
    /// into, counted among the other items (as if it were already taken out). The others close
    /// up behind it, so an item lands after every one whose centre its left edge has passed.
    pub fn drag_gap(&self, dragged: usize, x: f32) -> usize {
        let width = self.slot_of(dragged).map_or(0.0, |slot| slot.width);
        let left_edge = x - width / 2.0;
        self.slots
            .iter()
            .filter(|slot| slot.item != dragged)
            .filter(|slot| {
                let closed_left = slot.left - if slot.item > dragged { width } else { 0.0 };
                self.geo.origin + closed_left + slot.width / 2.0 < left_edge
            })
            .count()
    }

    /// Dragging `dragged` at window x: between items (as `drag_gap`), or onto an item that takes
    /// it (a group, per `takes`), whose middle half catches it. Measured where the items sit with
    /// the dragged one taken out, so an item it's over doesn't slide away from under the pointer;
    /// just beside such an item it goes before or after it.
    pub fn drag_zone(&self, dragged: usize, x: f32, takes: impl Fn(usize) -> bool) -> DragZone {
        let width = self.slot_of(dragged).map_or(0.0, |slot| slot.width);
        for slot in self.slots.iter().filter(|slot| slot.item != dragged && !slot.separator && takes(slot.item)) {
            let closed_left = slot.left - if slot.item > dragged { width } else { 0.0 };
            let centre = self.geo.origin + closed_left + slot.width / 2.0;
            let off = (x - centre).abs();
            if off <= slot.width / 4.0 {
                return DragZone::Onto(slot.item);
            }
            if off < slot.width / 2.0 {
                let before = self.slots.iter().filter(|other| other.item != dragged && other.item < slot.item).count();
                return DragZone::Gap(if x < centre { before } else { before + 1 });
            }
        }
        DragZone::Gap(self.drag_gap(dragged, x))
    }

    /// How far `item` slides sideways while `dragged` hovers over `gap`: the others close up
    /// behind it and open a space where it would land. Off the dock (`None`), its own place
    /// stays open.
    pub fn drag_shift(&self, dragged: usize, gap: Option<usize>, item: usize) -> f32 {
        if item == dragged {
            return 0.0;
        }
        let width = self.slot_of(dragged).map_or(0.0, |slot| slot.width);
        let gap = gap.unwrap_or(dragged);
        let (index_without, mut shift) = if item > dragged { (item - 1, -width) } else { (item, 0.0) };
        if index_without >= gap {
            shift += width;
        }
        shift
    }

    /// Something dragged in is over the middle of an icon (its centre half), so it would go onto
    /// that item (open with it, or into the Recycle Bin) rather than between two.
    pub fn onto_icon(&self, x: f32, y: f32) -> Option<usize> {
        let g = &self.geo;
        if y < g.pad_top || y > g.pad_top + g.icon {
            return None;
        }
        self.slots
            .iter()
            .filter(|slot| !slot.separator)
            .find(|slot| (x - slot.center(g)).abs() <= g.icon * 0.25)
            .map(|slot| slot.item)
    }
    /// Something new dragged over the dock at window x (files from Explorer, say): the gap it
    /// would go into.
    pub fn insert_gap(&self, x: f32) -> usize {
        self.slots.iter().filter(|slot| slot.center(&self.geo) < x).count()
    }

    /// How far `item` slides to open room for something new at `gap`: half a slot each way, so
    /// the dock stays centred.
    pub fn insert_shift(&self, gap: usize, item: usize) -> f32 {
        let half = (self.geo.icon + self.geo.gap) / 2.0;
        if item >= gap { half } else { -half }
    }
    /// The pointer (window coordinates) is far enough from the dock that letting go removes the
    /// dragged icon: a bit more than half an icon below the panel (dipping slightly below while
    /// arranging is fine), or an icon's width past either end.
    pub fn drag_is_off(&self, x: f32, y: f32) -> bool {
        let g = &self.geo;
        y > g.panel_h + g.icon * 0.6 || x < g.panel_left - g.icon || x > g.panel_left + g.panel_w + g.icon
    }

    /// The panel's start and end along the edge, from the window's start.
    fn panel_span(&self) -> (i32, i32) {
        let start = self.geo.panel_left as i32;
        (start, start + self.geo.panel_w as i32)
    }

    // The screen's edge and the dock's own edge differ when a taskbar sits on that edge: the dock
    // sits beside it, and the pointer still finds it by pushing against the screen's real edge,
    // across the taskbar. Without one they're the same.

    /// Pointer pressed against the screen edge within the dock's span: starts the reveal. Only
    /// on the dock's own screen: a pointer on a screen beyond that edge doesn't count.
    pub fn at_reveal_edge(&self, place: &Place, x: i32, y: i32) -> bool {
        let (start, end) = self.panel_span();
        let (along, away, _) = place.locate(x, y);
        (0..=1).contains(&away) && along >= start && along < end
    }

    /// Pointer close to where the dock appears, so it's worth checking more often (on the
    /// dock's screen only).
    pub fn near_edge(&self, place: &Place, x: i32, y: i32) -> bool {
        let (start, end) = self.panel_span();
        let (along, away, from_dock) = place.locate(x, y);
        away >= 0 && from_dock <= self.geo.panel_h as i32 && along >= start - 64 && along < end + 64
    }

    /// Pointer over the shown dock (or over a taskbar beside it, on the way there), with a few
    /// pixels' grace.
    pub fn pointer_over(&self, place: &Place, x: i32, y: i32) -> bool {
        let (start, end) = self.panel_span();
        let (along, away, from_dock) = place.locate(x, y);
        along >= start - 4 && along < end + 4 && away >= 0 && from_dock <= self.geo.reach() as i32 + 4
    }
}

/// The dock's own directions, and the window's. Everything is laid out as if on the top edge:
/// `u` along the edge (left to right; top to bottom on a side), `v` away from it. `Frame` turns
/// that into the window's x and y for the edge the dock is on. Rectangles are mapped, never the
/// drawing itself, so icons and text are always upright.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub edge: Edge,
    /// The window's size across the edge (its height on top or bottom, its width on a side).
    pub across: f32,
}

impl Frame {
    /// A rectangle in the dock's directions (`u0..u1` along, `v0..v1` away), in the window:
    /// (left, top, right, bottom).
    pub fn rect(&self, u0: f32, v0: f32, u1: f32, v1: f32) -> (f32, f32, f32, f32) {
        match self.edge {
            Edge::Top => (u0, v0, u1, v1),
            Edge::Bottom => (u0, self.across - v1, u1, self.across - v0),
            Edge::Left => (v0, u0, v1, u1),
            Edge::Right => (self.across - v1, u0, self.across - v0, u1),
        }
    }

    /// A point in the window (x, y) in the dock's directions (u, v).
    pub fn from_window(&self, x: f32, y: f32) -> (f32, f32) {
        match self.edge {
            Edge::Top => (x, y),
            Edge::Bottom => (x, self.across - y),
            Edge::Left => (y, x),
            Edge::Right => (y, self.across - x),
        }
    }

    /// A shift away from the edge (`v`), as a shift in the window (x, y).
    pub fn shift(&self, v: f32) -> (f32, f32) {
        match self.edge {
            Edge::Top => (0.0, v),
            Edge::Bottom => (0.0, -v),
            Edge::Left => (v, 0.0),
            Edge::Right => (-v, 0.0),
        }
    }

    /// The window's width and height for a dock `along` long and `across` deep.
    pub fn window_size(&self, along: i32, across: i32) -> (i32, i32) {
        if self.edge.is_side() { (across, along) } else { (along, across) }
    }
}

/// Where the dock is on the screen, for the pointer: screen points in the dock's directions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub edge: Edge,
    /// Where the dock's window starts along the edge (screen x on top or bottom, y on a side).
    pub start: i32,
    /// The screen's own edge: its first pixel row or column on that side (the last, on the
    /// bottom or right), where the pointer stops.
    pub screen_edge: i32,
    /// Where the dock itself starts: inside a taskbar on that edge, the work area's edge; else the
    /// screen's.
    pub dock_edge: i32,
}

impl Place {
    /// A screen point as (along the dock, from its window's start; away from the screen's edge;
    /// away from the dock's own edge). Negative away: past the edge, on another screen.
    pub fn locate(&self, x: i32, y: i32) -> (i32, i32, i32) {
        match self.edge {
            Edge::Top => (x - self.start, y - self.screen_edge, y - self.dock_edge),
            Edge::Bottom => (x - self.start, self.screen_edge - y, self.dock_edge - y),
            Edge::Left => (y - self.start, x - self.screen_edge, x - self.dock_edge),
            Edge::Right => (y - self.start, self.screen_edge - x, self.dock_edge - x),
        }
    }

    /// The screen point `along` the dock and `away` from the screen's edge.
    pub fn point(&self, along: i32, away: i32) -> (i32, i32) {
        match self.edge {
            Edge::Top => (self.start + along, self.screen_edge + away),
            Edge::Bottom => (self.start + along, self.screen_edge - away),
            Edge::Left => (self.screen_edge + away, self.start + along),
            Edge::Right => (self.screen_edge - away, self.start + along),
        }
    }
}

/// Where a dragged icon would go: into a gap (counted among the other items), or onto an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragZone {
    Gap(usize),
    Onto(usize),
}

/// Where an icon is drawn: it grows in place when hovered (its centre never moves, so
/// neighbours stay put) and is pushed `bounce` pixels away from the screen edge.
/// Settled icons snap to whole pixels so they stay sharp.
pub fn icon_rect(g: &Geometry, slot: &Slot, hover_t: f32, bounce: f32) -> (f32, f32, f32) {
    let size = g.icon + (g.hover_icon - g.icon) * ease_out(hover_t);
    let x = slot.center(g) - size / 2.0;
    let y = g.pad_top + bounce;
    let settled = (hover_t == 0.0 || hover_t == 1.0) && bounce == 0.0;
    if settled { (x.round(), y.round(), size) } else { (x, y, size) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DockSettings {
        DockSettings::default() // icon 53, zoom 64, spacing 10, labels on
    }

    /// Window x of an item's centre, as drawn now.
    fn centre(layout: &Layout, item: usize) -> f32 {
        layout.slot_of(item).unwrap().center(&layout.geo)
    }

    #[test]
    fn a_group_catches_an_icon_over_its_middle() {
        // Items 0..5; 3 is a group. Dragging item 0 (so the others sit one place to the left).
        let layout = compute(&settings(), &[false; 6], 96, None);
        let group = |i: usize| i == 3;
        let w = layout.slots[0].width;
        let closed = |item: usize| centre(&layout, item) - w; // where it sits with item 0 taken out
        assert_eq!(layout.drag_zone(0, closed(3), group), DragZone::Onto(3), "its middle: into the group");
        assert_eq!(layout.drag_zone(0, closed(3) - w * 0.2, group), DragZone::Onto(3));
        assert_eq!(layout.drag_zone(0, closed(3) - w * 0.4, group), DragZone::Gap(2), "its left edge: just before it");
        assert_eq!(layout.drag_zone(0, closed(3) + w * 0.4, group), DragZone::Gap(3), "its right edge: just after it");
        assert_eq!(layout.drag_zone(0, closed(1), group), DragZone::Gap(layout.drag_gap(0, closed(1))), "elsewhere as before");
        assert_eq!(layout.drag_zone(3, centre(&layout, 3), group), DragZone::Gap(3), "a group isn't caught by itself");
        let nothing = |_: usize| false;
        assert_eq!(layout.drag_zone(0, closed(3), nothing), DragZone::Gap(layout.drag_gap(0, closed(3))), "nothing takes it: plain arranging");
    }

    #[test]
    fn dragging_over_its_own_place_drops_nowhere_new() {
        let layout = compute(&settings(), &[false; 4], 96, None);
        assert_eq!(layout.drag_gap(1, centre(&layout, 1)), 1, "its own gap: a no-op");
        assert_eq!(layout.drag_gap(1, centre(&layout, 0) - 1.0), 0, "before the first");
        assert_eq!(layout.drag_gap(1, centre(&layout, 2) + 1.0), 2, "after the next one");
        assert_eq!(layout.drag_gap(1, centre(&layout, 3) + 50.0), 3, "after the last");
    }

    #[test]
    fn the_others_make_room_where_it_will_land() {
        let layout = compute(&settings(), &[false; 4], 96, None);
        let w = layout.slot_of(1).unwrap().width;
        // Item 1 dragged over the gap after item 2: item 2 closes up, item 3 stays.
        assert_eq!(
            (0..4).map(|i| layout.drag_shift(1, Some(2), i)).collect::<Vec<_>>(),
            [0.0, 0.0, -w, 0.0]
        );
        // Dragged to the front: everything before its old place moves right.
        assert_eq!((0..4).map(|i| layout.drag_shift(2, Some(0), i)).collect::<Vec<_>>(), [w, w, 0.0, 0.0]);
        // Over its own place, or off the dock: nothing moves.
        for gap in [Some(1), None] {
            assert!((0..4).all(|i| layout.drag_shift(1, gap, i) == 0.0));
        }
    }

    #[test]
    fn letting_go_well_away_removes() {
        let layout = compute(&settings(), &[false; 4], 96, None);
        let g = layout.geo;
        let middle = g.panel_left + g.panel_w / 2.0;
        assert!(!layout.drag_is_off(middle, g.panel_h + g.icon * 0.4), "dipping below the dock: still arranging");
        assert!(layout.drag_is_off(middle, g.panel_h + g.icon * 0.6 + 1.0), "over half an icon below: remove");
        assert!(!layout.drag_is_off(g.panel_left - 2.0, 10.0), "just past the end: still arranging");
        assert!(layout.drag_is_off(g.panel_left - g.icon - 1.0, 10.0));
    }
    #[test]
    fn something_new_goes_where_the_pointer_is_and_the_dock_opens_evenly() {
        let layout = compute(&settings(), &[false; 4], 96, None);
        assert_eq!(layout.insert_gap(centre(&layout, 0) - 1.0), 0);
        assert_eq!(layout.insert_gap(centre(&layout, 1) + 1.0), 2);
        assert_eq!(layout.insert_gap(centre(&layout, 3) + 40.0), 4);
        let half = (layout.geo.icon + layout.geo.gap) / 2.0;
        assert_eq!((0..4).map(|i| layout.insert_shift(2, i)).collect::<Vec<_>>(), [-half, -half, half, half]);
    }
    #[test]
    fn onto_an_icon_only_over_its_middle() {
        let layout = compute(&settings(), &[false, true, false], 96, None);
        let g = layout.geo;
        let middle_y = g.pad_top + g.icon / 2.0;
        assert_eq!(layout.onto_icon(centre(&layout, 0), middle_y), Some(0));
        assert_eq!(layout.onto_icon(centre(&layout, 2) + g.icon * 0.2, middle_y), Some(2));
        assert_eq!(layout.onto_icon(centre(&layout, 0) + g.icon * 0.4, middle_y), None, "its edge: between");
        assert_eq!(layout.onto_icon(centre(&layout, 1), middle_y), None, "never a separator");
        assert_eq!(layout.onto_icon(centre(&layout, 0), g.pad_top + g.icon + 2.0), None, "below the icons");
    }
    /// A small dock: 3 icons | 5 icons | 2 icons.
    fn small_dock() -> Vec<bool> {
        let mut items = vec![false; 3];
        items.push(true);
        items.extend([false; 5]);
        items.push(true);
        items.extend([false; 2]);
        items
    }

    /// On the top edge: the window starting at `start`, the screen's top at `screen_edge`, the dock's at `dock_edge`.
    fn top(start: i32, screen_edge: i32, dock_edge: i32) -> Place {
        Place { edge: Edge::Top, start, screen_edge, dock_edge }
    }

    #[test]
    fn a_pointer_on_a_screen_above_never_reveals_the_dock() {
        let layout = compute(&settings(), &small_dock(), 96, None);
        let win_x = 100;
        let middle = win_x + (layout.geo.panel_left + layout.geo.panel_w / 2.0) as i32;
        // The dock's screen starts at y = 0 with another screen above it (negative y).
        let place = top(win_x, 0, 0);
        assert!(layout.at_reveal_edge(&place, middle, 0), "pressed against the edge");
        assert!(!layout.at_reveal_edge(&place, middle, -1), "just over on the screen above");
        assert!(!layout.at_reveal_edge(&place, middle, -500), "anywhere on the screen above");
        assert!(!layout.near_edge(&place, middle, -20), "not even worth checking quickly");
        assert!(!layout.pointer_over(&place, middle, -5), "and doesn't keep the dock out");
        // A dock on a lower screen (top at y = 1440) behaves the same.
        assert!(layout.at_reveal_edge(&top(win_x, 1440, 1440), middle, 1440));
        assert!(!layout.at_reveal_edge(&top(win_x, 1440, 1440), middle, 1439));
    }

    #[test]
    fn under_a_taskbar_at_the_top_the_screen_edge_still_brings_the_dock_out() {
        let layout = compute(&settings(), &small_dock(), 96, None);
        let win_x = 100;
        let middle = win_x + (layout.geo.panel_left + layout.geo.panel_w / 2.0) as i32;
        // A 48 px taskbar at the top: the dock hangs from y = 48, the screen's edge is y = 0.
        let dock_top = 48;
        let place = top(win_x, 0, dock_top);
        assert!(layout.at_reveal_edge(&place, middle, 0), "pushing against the screen's top, over the taskbar");
        assert!(!layout.at_reveal_edge(&place, middle, dock_top), "the line under the taskbar isn't the edge");
        assert!(layout.near_edge(&place, middle, 20));
        assert!(layout.pointer_over(&place, middle, 20), "on the way down across the taskbar it stays out");
        assert!(layout.pointer_over(&place, middle, dock_top + layout.geo.reach() as i32));
        assert!(!layout.pointer_over(&place, middle, dock_top + layout.geo.reach() as i32 + 50));
    }

    #[test]
    fn every_edge_finds_the_pointer_at_its_own_edge_and_the_taskbar_on_it_is_crossed() {
        let layout = compute(&settings(), &small_dock(), 96, None);
        let along = (layout.geo.panel_left + layout.geo.panel_w / 2.0) as i32;
        // A 1920x1080 screen at (0, 0); a 48 px taskbar on the same edge as the dock.
        let places = [
            Place { edge: Edge::Bottom, start: 200, screen_edge: 1079, dock_edge: 1079 - 48 },
            Place { edge: Edge::Left, start: 100, screen_edge: 0, dock_edge: 48 },
            Place { edge: Edge::Right, start: 100, screen_edge: 1919, dock_edge: 1919 - 48 },
        ];
        for place in places {
            let (x, y) = place.point(along, 0);
            assert!(layout.at_reveal_edge(&place, x, y), "{:?}: the screen's own edge", place.edge);
            assert_eq!(place.locate(x, y), (along, 0, -48), "{:?}", place.edge);
            let (x, y) = place.point(along, 48);
            assert!(!layout.at_reveal_edge(&place, x, y), "{:?}: the line beside the taskbar isn't the edge", place.edge);
            assert!(layout.pointer_over(&place, x, y), "{:?}: crossing the taskbar, it stays out", place.edge);
            let (x, y) = place.point(along, -1);
            assert!(!layout.at_reveal_edge(&place, x, y) && !layout.near_edge(&place, x, y), "{:?}: past the edge, another screen", place.edge);
            let (x, y) = place.point(-200, 0);
            assert!(!layout.at_reveal_edge(&place, x, y), "{:?}: beyond the dock's span", place.edge);
        }
    }

    #[test]
    fn the_frame_turns_the_dock_to_its_edge_and_back() {
        // A spot 10..20 along and 3..7 away from the edge, in a window 100 deep.
        let at = |edge| Frame { edge, across: 100.0 }.rect(10.0, 3.0, 20.0, 7.0);
        assert_eq!(at(Edge::Top), (10.0, 3.0, 20.0, 7.0));
        assert_eq!(at(Edge::Bottom), (10.0, 93.0, 20.0, 97.0));
        assert_eq!(at(Edge::Left), (3.0, 10.0, 7.0, 20.0));
        assert_eq!(at(Edge::Right), (93.0, 10.0, 97.0, 20.0));
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            let frame = Frame { edge, across: 100.0 };
            let (left, top, _, _) = frame.rect(15.0, 4.0, 15.0, 4.0);
            assert_eq!(frame.from_window(left, top), (15.0, 4.0), "{edge:?}: there and back");
            let (dx, dy) = frame.shift(5.0);
            assert_eq!(frame.from_window(left + dx, top + dy), (15.0, 9.0), "{edge:?}: a shift moves away from the edge");
        }
        assert_eq!(Frame { edge: Edge::Left, across: 100.0 }.window_size(400, 100), (100, 400), "a side dock is tall");
        assert_eq!(Frame { edge: Edge::Bottom, across: 100.0 }.window_size(400, 100), (400, 100));
    }

    #[test]
    fn a_dock_of_only_separators_on_a_narrow_screen_still_lays_out() {
        // 20 separators need 334 px; shrunk, they fit in 300 (and the shrinking has to stop).
        let layout = compute(&settings(), &[true; 20], 96, Some(300.0));
        assert_eq!(layout.slots.len(), 20);
        let empty = compute(&settings(), &[], 96, Some(300.0));
        assert!(empty.slots.is_empty());
    }

    #[test]
    fn sizes_scale_with_dpi() {
        let at_100 = compute(&settings(), &small_dock(), 96, None).geo;
        assert_eq!(at_100.icon, 53.0);
        assert_eq!(at_100.hover_icon, 64.0);
        let at_150 = compute(&settings(), &small_dock(), 144, None).geo;
        assert_eq!(at_150.icon, 80.0); // 53 x 1.5 = 79.5, rounded
        assert_eq!(at_150.hover_icon, 96.0);
        assert!(at_150.win_w > at_100.win_w);
    }

    #[test]
    fn resting_icons_sit_on_whole_pixels_at_common_scalings() {
        for dpi in [96, 120, 144, 168, 192] {
            let layout = compute(&settings(), &small_dock(), dpi, None);
            for slot in layout.slots.iter().filter(|s| !s.separator) {
                for hover in [0.0, 1.0] {
                    let (x, y, _) = icon_rect(&layout.geo, slot, hover, 0.0);
                    assert_eq!(x, x.round(), "dpi {dpi}");
                    assert_eq!(y, y.round(), "dpi {dpi}");
                }
            }
        }
    }

    #[test]
    fn panel_fits_the_slots_exactly() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let g = layout.geo;
        let first = layout.slots.first().unwrap();
        let last = layout.slots.last().unwrap();
        let pad = g.origin + g.gap / 2.0 - g.panel_left; // panel edge to first icon
        assert_eq!(pad, 18.0); // 12 x 1.5
        let content_right = g.origin + last.left + last.width - g.gap / 2.0;
        assert_eq!(g.panel_left + g.panel_w - content_right, pad); // same padding both ends
        assert_eq!(first.left, 0.0);
    }

    #[test]
    fn separators_are_narrow_and_never_hit() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let sep = layout.slots[3];
        assert!(sep.separator);
        assert!(sep.width < layout.slots[0].width);
        let y = layout.geo.pad_top + 10.0;
        assert_eq!(layout.hit_test(sep.center(&layout.geo), y), None);
        // ... but they can be picked up (across their whole slot, the panel's height).
        assert_eq!(layout.separator_at(sep.center(&layout.geo), y), Some(sep.item));
        assert_eq!(layout.separator_at(layout.geo.origin + sep.left + 0.5, y), Some(sep.item));
        assert_eq!(layout.separator_at(layout.slots[2].center(&layout.geo), y), None, "an icon isn't one");
        assert_eq!(layout.separator_at(sep.center(&layout.geo), layout.geo.panel_h + 5.0), None, "below the panel");
    }

    #[test]
    fn a_dragged_separator_lands_where_it_is_let_go() {
        // Items 0-2, separator 3, items 4-8, separator 9, items 10-11; separator 3 dragged right.
        let layout = compute(&settings(), &small_dock(), 144, None);
        let between = |a: usize, b: usize| (centre(&layout, a) + centre(&layout, b)) / 2.0;
        assert_eq!(layout.drag_gap(3, between(5, 6)), 5, "after items 0, 1, 2, 4 and 5 (five others before it)");
        assert_eq!(layout.drag_gap(3, between(7, 8)), 7);
        assert_eq!(layout.drag_gap(3, centre(&layout, 3)), 3, "where it was");
        assert_eq!(layout.drag_gap(3, between(0, 1)), 1);
    }

    #[test]
    fn hit_test_finds_each_icon_at_its_centre() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let y = layout.geo.pad_top + layout.geo.icon / 2.0;
        for slot in layout.slots.iter().filter(|s| !s.separator) {
            assert_eq!(layout.hit_test(slot.center(&layout.geo), y), Some(slot.item));
        }
    }

    #[test]
    fn gaps_between_icons_still_count_as_the_nearest_icon() {
        // No dead zones while sweeping along the dock.
        let layout = compute(&settings(), &small_dock(), 144, None);
        let (a, b) = (layout.slots[0], layout.slots[1]);
        let boundary = layout.geo.origin + b.left;
        let y = layout.geo.pad_top + 5.0;
        assert_eq!(layout.hit_test(boundary - 0.5, y), Some(a.item));
        assert_eq!(layout.hit_test(boundary, y), Some(b.item));
    }

    #[test]
    fn nothing_is_hit_outside_the_dock() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let g = layout.geo;
        let mid = layout.slots[0].center(&g);
        assert_eq!(layout.hit_test(mid, -1.0), None);
        assert_eq!(layout.hit_test(mid, g.reach() + 1.0), None);
        assert_eq!(layout.hit_test(g.panel_left - 20.0, g.pad_top + 5.0), None);
        assert_eq!(layout.hit_test(g.panel_left + g.panel_w + 20.0, g.pad_top + 5.0), None);
    }

    #[test]
    fn hovering_grows_in_place_and_neighbours_never_move() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let g = layout.geo;
        let slot = &layout.slots[4];
        let (rx, _, rsize) = icon_rect(&g, slot, 0.0, 0.0);
        let (hx, _, hsize) = icon_rect(&g, slot, 1.0, 0.0);
        assert_eq!(hsize, g.hover_icon);
        assert!((rx + rsize / 2.0 - (hx + hsize / 2.0)).abs() <= 0.5, "same centre");
        // Neighbours are computed from their own slot only, so hovering can't shift them.
        let neighbour = &layout.slots[5];
        assert_eq!(icon_rect(&g, neighbour, 0.0, 0.0), icon_rect(&g, neighbour, 0.0, 0.0));
    }

    #[test]
    fn bounce_moves_icons_away_from_the_edge() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let (_, y0, _) = icon_rect(&layout.geo, &layout.slots[0], 0.0, 0.0);
        let (_, y1, _) = icon_rect(&layout.geo, &layout.slots[0], 0.0, 6.0);
        assert_eq!(y1 - y0, 6.0);
    }

    #[test]
    fn window_is_centred_on_the_monitor() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let x = layout.window_x(0, 3840);
        assert_eq!(x * 2 + layout.geo.win_w, 3840 - (3840 - layout.geo.win_w) % 2);
        // Also on a monitor that doesn't start at 0 (e.g. one to the left).
        assert_eq!(layout.window_x(-2560, 0), x - 2560 - (3840 - 2560) / 2);
    }

    #[test]
    fn reveal_edge_is_the_top_pixel_row_within_the_dock() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let win_x = layout.window_x(0, 3840);
        let mid = win_x + layout.geo.panel_left as i32 + 100;
        let place = top(win_x, 0, 0);
        assert!(layout.at_reveal_edge(&place, mid, 0));
        assert!(!layout.at_reveal_edge(&place, mid, 5), "must touch the edge");
        assert!(!layout.at_reveal_edge(&place, 10, 0), "far left of the dock");
        assert!(!layout.at_reveal_edge(&place, 3830, 0), "far right of the dock");
    }

    #[test]
    fn pointer_over_covers_the_panel_and_a_grown_icon() {
        let layout = compute(&settings(), &small_dock(), 144, None);
        let win_x = layout.window_x(0, 3840);
        let mid = win_x + layout.geo.panel_left as i32 + 100;
        let place = top(win_x, 0, 0);
        assert!(layout.pointer_over(&place, mid, 0));
        assert!(layout.pointer_over(&place, mid, layout.geo.reach() as i32));
        assert!(!layout.pointer_over(&place, mid, layout.geo.reach() as i32 + 50));
    }

    #[test]
    fn slide_goes_from_hidden_to_shown() {
        let g = compute(&settings(), &small_dock(), 144, None).geo;
        assert_eq!(g.slide_offset(1.0), 0.0);
        assert_eq!(g.slide_offset(0.0), -g.travel().round());
        assert!(g.travel() > g.reach() + g.label_h, "fully hidden includes label and grown icon");
        let mut last = g.slide_offset(0.0);
        for i in 1..=20 {
            let off = g.slide_offset(i as f32 / 20.0);
            assert!(off >= last);
            last = off;
        }
    }

    #[test]
    fn empty_dock_doesnt_break() {
        let layout = compute(&settings(), &[], 144, None);
        assert!(layout.slots.is_empty());
        assert!(layout.geo.panel_w > 0.0);
        assert_eq!(layout.hit_test(100.0, 20.0), None);
    }

    /// A big dock: 43 items and 1 separator.
    fn full_dock() -> Vec<bool> {
        let mut items = vec![false; 42];
        items.push(true);
        items.push(false);
        items
    }

    #[test]
    fn a_dock_that_fits_is_never_shrunk() {
        let free = compute(&settings(), &small_dock(), 144, None).geo;
        let limited = compute(&settings(), &small_dock(), 144, Some(3816.0)).geo;
        assert_eq!(free, limited);
    }

    #[test]
    fn a_dock_wider_than_the_screen_shrinks_just_enough() {
        for (dpi, screen) in [(96, 2560.0), (144, 3840.0), (192, 3840.0)] {
            let max = screen - 2.0 * (8.0 * dpi as f32 / 96.0).round();
            let full = compute(&settings(), &full_dock(), dpi, None).geo;
            let fitted = compute(&settings(), &full_dock(), dpi, Some(max)).geo;
            assert!(full.panel_w > max, "dpi {dpi}: the test needs a dock that doesn't fit");
            assert!(fitted.panel_w <= max, "dpi {dpi}: {} > {max}", fitted.panel_w);
            assert!(fitted.icon < full.icon);
            // As big as possible: growing every icon by one more pixel wouldn't fit.
            assert!(max - fitted.panel_w < icons_in(&full_dock()), "dpi {dpi}: {} px left over", max - fitted.panel_w);
        }
    }

    fn icons_in(items: &[bool]) -> f32 {
        items.iter().filter(|s| !**s).count() as f32
    }

    #[test]
    fn shrunk_icons_still_sit_on_whole_pixels_and_zoom_stays() {
        let layout = compute(&settings(), &full_dock(), 144, Some(3816.0));
        assert_eq!(layout.geo.hover_icon, 96.0, "zoom size isn't shrunk");
        for slot in layout.slots.iter().filter(|s| !s.separator) {
            let (x, y, _) = icon_rect(&layout.geo, slot, 0.0, 0.0);
            assert_eq!((x, y), (x.round(), y.round()));
        }
    }

    #[test]
    fn a_capsule_floats_below_the_edge_with_round_ends_and_the_rest_keep_their_place() {
        use crate::config::DockStyle;
        let classic = compute(&settings(), &small_dock(), 96, None).geo;
        let with = |style: DockStyle, dpi: u32| {
            let mut s = settings();
            s.style = style;
            compute(&s, &small_dock(), dpi, None).geo
        };
        let capsule = with(DockStyle::Capsule, 96);
        assert_eq!((classic.panel_top, classic.radius), (0.0, 7.0), "classic as it always was");
        assert_eq!(capsule.panel_top, 8.0);
        assert_eq!(capsule.pad_top, classic.pad_top + 8.0, "the icons move down with it");
        assert_eq!(capsule.panel_h, classic.panel_h + 8.0);
        assert_eq!(capsule.radius, ((capsule.panel_h - capsule.panel_top) / 2.0).floor(), "round ends");
        assert_eq!(capsule.win_h, classic.win_h + 8, "the window reaches that much further");
        assert_eq!(capsule.travel(), classic.travel() + 8.0, "and it slides that much further to hide");
        assert_eq!(with(DockStyle::Capsule, 144).panel_top, 12.0, "scaled like everything else");
        for style in [DockStyle::Glass, DockStyle::Frosted] {
            let g = with(style, 96);
            assert_eq!((g.panel_top, g.pad_top, g.panel_h, g.win_h), (0.0, classic.pad_top, classic.panel_h, classic.win_h), "{style:?} hangs from the edge");
        }
        assert_eq!((with(DockStyle::Glass, 96).radius, with(DockStyle::Frosted, 96).radius, with(DockStyle::Glass, 144).radius), (18.0, 12.0, 27.0));
    }

    #[test]
    fn the_strip_goes_past_the_names_when_they_always_show() {
        use crate::config::Edge;
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            let on_hover = {
                let mut s = settings();
                s.edge = edge;
                compute(&s, &small_dock(), 96, None).geo
            };
            let always = {
                let mut s = settings();
                s.edge = edge;
                s.always_show_labels = true;
                compute(&s, &small_dock(), 96, None).geo
            };
            assert_eq!(on_hover.strip_top, on_hover.panel_h + 6.0, "{edge:?}: just past the panel, as always");
            // The names start just past the panel (2 px on top or bottom, 8 beside it on a side).
            let names_end = if edge.is_side() { always.panel_h + 8.0 + always.label_w } else { always.panel_h + 2.0 + always.label_h };
            assert!(always.strip_top >= names_end, "{edge:?}: the strip at {} starts past the names ({names_end})", always.strip_top);
            let strip_end = always.strip_top + if edge.is_side() { always.strip_w } else { always.strip_h };
            assert!(strip_end <= always.win_h as f32, "{edge:?}: and the window has room for it");
        }
    }

    #[test]
    fn labels_off_makes_the_window_shorter() {
        let mut no_labels = settings();
        no_labels.show_labels = false;
        let with = compute(&settings(), &small_dock(), 144, None).geo;
        let without = compute(&no_labels, &small_dock(), 144, None).geo;
        assert!(without.win_h < with.win_h);
    }
}
