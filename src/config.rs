//! Dock configuration: a plain, human-readable TOML file you can also edit by hand.
//! Format version 1 is described in docs/config-format.md. Older files (no `version` line)
//! still load, and `migrate` upgrades them in place, keeping comments.

use serde::Deserialize;
use toml_edit::{DocumentMut, Item as TomlItem, Table, value};

pub const CURRENT_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct DockSettings {
    /// Icon size in logical pixels (RocketDock's "IconMin").
    pub icon_size: f32,
    /// How big a hovered icon gets, in logical pixels (RocketDock's "IconMax"). Never below icon_size.
    pub zoom_size: f32,
    /// How long the zoom in/out takes.
    pub zoom_ms: u64,
    /// Gap between icons, in logical pixels.
    pub spacing: f32,
    /// How long the pointer must rest at the screen edge before the dock appears.
    pub popup_delay_ms: u64,
    /// How long after the pointer leaves before the dock hides.
    pub hide_delay_ms: u64,
    /// How long the slide down onto the screen takes. 0 = appear instantly.
    pub slide_in_ms: u64,
    /// How long the slide back up takes. 0 = vanish instantly.
    pub slide_out_ms: u64,
    pub show_labels: bool,
    /// Label text size, in logical pixels.
    pub label_size: f32,
    /// How solid the dock's background (panel, edge and shadow) is, in percent. 0 = icons only.
    pub background_opacity: u32,
    /// How solid icons are, in percent; the one under the pointer becomes fully solid.
    pub icon_opacity: u32,
    pub launch_effect: LaunchEffect,
    /// Icons can't be dragged around or off the dock (right-click → Lock icons).
    pub locked: bool,
    /// The dock's panel: light (CrystalXP-style, the default) or dark.
    pub look: DockLook,
    /// A soft glow behind the icon under the pointer: none (the default), Windows' accent
    /// colour, or each icon's own main colour.
    pub hover_glow: HoverGlow,
    /// Which screen the dock is on: "main" (Windows' main screen, the default) or a screen's id
    /// (Settings > Screen sets it). A screen that isn't plugged in gives the main one.
    pub monitor: String,
    /// A keyboard shortcut that brings the dock out, like "Ctrl+Shift+D"; empty for none (the
    /// default). See `hotkey::parse`.
    pub hotkey: String,
}

impl Default for DockSettings {
    fn default() -> Self {
        Self {
            icon_size: 53.0,
            zoom_size: 64.0,
            zoom_ms: 120,
            spacing: 10.0,
            popup_delay_ms: 350,
            hide_delay_ms: 250,
            slide_in_ms: 250, // RocketDock's "AutoHideTicks"
            slide_out_ms: 250,
            show_labels: true,
            label_size: 12.5,
            background_opacity: 100,
            icon_opacity: 100,
            launch_effect: LaunchEffect::Bounce,
            locked: false,
            look: DockLook::Light,
            hover_glow: HoverGlow::None,
            monitor: "main".into(),
            hotkey: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    #[default]
    App,
    Separator,
    Group,
    RecycleBin,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunState {
    #[default]
    Normal,
    Minimized,
    Maximized,
}

/// The dock's look: a light panel (the default) or a dark one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockLook {
    #[default]
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaunchEffect {
    #[default]
    Bounce,
    None,
    /// The clicked icon glows up for a moment (in the hover glow's colour).
    Glow,
}

/// The glow behind the icon under the pointer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HoverGlow {
    #[default]
    None,
    /// Windows' accent colour (Settings > Personalisation > Colours).
    Accent,
    /// Each icon's own main colour.
    Icon,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct ItemConfig {
    pub kind: Kind,
    pub name: String,
    /// What to open: an app, file, folder, URL, or a shell location such as `::{CLSID}`.
    pub target: String,
    /// Store/packaged app identity.
    pub app_id: String,
    pub args: String,
    pub start_in: String,
    /// Image file, or an icon inside a file: `file,-resource id` / `file,index`.
    pub icon: String,
    pub run: RunState,
    pub admin: bool,
    /// Children of a group.
    pub items: Vec<ItemConfig>,
    /// Version 0 spelling of `kind = "separator"`; read, never written.
    separator: bool,
}

/// The Recycle Bin's shell location.
pub const RECYCLE_BIN: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

impl ItemConfig {
    /// A plain item that opens `target`.
    pub fn new(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self { name: name.into(), target: target.into(), ..Default::default() }
    }

    pub fn is_separator(&self) -> bool {
        self.kind == Kind::Separator
    }

    /// What clicking opens. The Recycle Bin needs no target in the file.
    pub fn launch_target(&self) -> &str {
        if self.kind == Kind::RecycleBin && self.target.trim().is_empty() { RECYCLE_BIN } else { &self.target }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    #[allow(dead_code)] // kept for diagnostics and future migrations
    pub version: i64,
    pub dock: DockSettings,
    pub items: Vec<ItemConfig>,
}

/// The result of reading a file: the settings to use plus anything worth telling the user.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    pub config: Config,
    pub warnings: Vec<String>,
    /// The file is an older format that `migrate` can upgrade.
    pub needs_migration: bool,
    /// The file comes from a newer dock; use it but never save over it.
    pub read_only: bool,
}

/// The file exactly as written, before defaults and checks.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawConfig {
    version: Option<i64>,
    dock: RawDock,
    #[serde(rename = "item")]
    items: Vec<ItemConfig>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawDock {
    icon_size: Option<f32>,
    zoom_size: Option<f32>,
    zoom_ms: Option<u64>,
    spacing: Option<f32>,
    popup_delay_ms: Option<u64>,
    hide_delay_ms: Option<u64>,
    slide_in_ms: Option<u64>,
    slide_out_ms: Option<u64>,
    show_labels: Option<bool>,
    label_size: Option<f32>,
    background_opacity: Option<u32>,
    icon_opacity: Option<u32>,
    launch_effect: Option<LaunchEffect>,
    locked: Option<bool>,
    look: Option<DockLook>,
    hover_glow: Option<HoverGlow>,
    monitor: Option<String>,
    hotkey: Option<String>,
    /// Version 0: zoom as a multiple of icon_size.
    hover_scale: Option<f32>,
}

const ROOT_KEYS: &[&str] = &["version", "dock", "item"];
pub(crate) const DOCK_KEYS: &[&str] = &[
    "icon_size", "zoom_size", "zoom_ms", "spacing", "popup_delay_ms", "hide_delay_ms",
    "slide_in_ms", "slide_out_ms", "show_labels", "label_size", "background_opacity", "icon_opacity", "launch_effect",
    "locked", "look", "hover_glow", "monitor", "hotkey", "hover_scale",
];
pub(crate) const ITEM_KEYS: &[&str] = &[
    "kind", "name", "target", "app_id", "args", "start_in", "icon", "run", "admin", "items", "separator",
];

/// What's said about a file that isn't text the dock can read.
pub const NOT_TEXT: &str =
    "It isn't saved as UTF-8 text, the kind the dock reads. If you edited it, save it again with the encoding set to UTF-8.";

/// Reads a dock file as text: UTF-8, with or without a byte-order mark, or UTF-16 with one
/// (Notepad's "Unicode", and what Windows PowerShell's `>` writes). Anything else is
/// `InvalidData`, said in plain words (`NOT_TEXT`).
pub fn read_text(path: &std::path::Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    decode(bytes).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, NOT_TEXT))
}

fn decode(bytes: Vec<u8>) -> Option<String> {
    fn utf16(rest: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
        let (pairs, odd) = rest.as_chunks::<2>();
        if !odd.is_empty() {
            return None;
        }
        String::from_utf16(&pairs.iter().map(|&pair| unit(pair)).collect::<Vec<u16>>()).ok()
    }
    match bytes.as_slice() {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        _ => String::from_utf8(bytes).ok(),
    }
}

pub fn load(path: &std::path::Path) -> Result<Parsed, String> {
    let text = read_text(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{} has an error:\n\n{e}", path.display()))
}

/// Parses config text. Missing values get defaults; out-of-range values are clamped with a
/// warning; unknown keys are warned about and otherwise ignored.
pub fn parse(text: &str) -> Result<Parsed, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut warnings = unknown_keys(text);
    let version = raw.version.unwrap_or(0);

    let d = DockSettings::default();
    let r = &raw.dock;
    // A value that isn't a real number (`nan`, `inf`, `1e999`) gets the default; one out of
    // range is brought into it.
    let mut clamp = |key: &str, value: f32, default: f32, lo: f32, hi: f32| -> f32 {
        if !value.is_finite() {
            warnings.push(format!("[dock] {key} = {value} isn't a usable number; using {default}"));
            return default;
        }
        let clamped = value.clamp(lo, hi);
        if clamped != value {
            warnings.push(format!("[dock] {key} = {value} is out of range ({lo}–{hi}); using {clamped}"));
        }
        clamped
    };
    let icon_size = clamp("icon_size", r.icon_size.unwrap_or(d.icon_size), d.icon_size, 16.0, 256.0);
    let usual_zoom = (icon_size * 1.2).round();
    let zoom = r.zoom_size.or(r.hover_scale.map(|scale| (icon_size * scale).round())).unwrap_or(usual_zoom);
    let zoom_size = clamp("zoom_size", zoom, usual_zoom, icon_size, 512.0);
    let spacing = clamp("spacing", r.spacing.unwrap_or(d.spacing), d.spacing, 0.0, 64.0);
    let label_size = clamp("label_size", r.label_size.unwrap_or(d.label_size), d.label_size, 8.0, 24.0);
    let background_opacity = clamp("background_opacity", r.background_opacity.unwrap_or(d.background_opacity) as f32, 100.0, 0.0, 100.0) as u32;
    let icon_opacity = clamp("icon_opacity", r.icon_opacity.unwrap_or(d.icon_opacity) as f32, 100.0, 10.0, 100.0) as u32;
    let mut clamp_ms = |key: &str, value: Option<u64>, default: u64, hi: u64| -> u64 {
        let value = value.unwrap_or(default);
        if value > hi {
            warnings.push(format!("[dock] {key} = {value} is out of range (0–{hi}); using {hi}"));
        }
        value.min(hi)
    };
    let dock = DockSettings {
        icon_size,
        zoom_size,
        zoom_ms: clamp_ms("zoom_ms", r.zoom_ms, d.zoom_ms, 2000),
        spacing,
        popup_delay_ms: clamp_ms("popup_delay_ms", r.popup_delay_ms, d.popup_delay_ms, 5000),
        hide_delay_ms: clamp_ms("hide_delay_ms", r.hide_delay_ms, d.hide_delay_ms, 5000),
        slide_in_ms: clamp_ms("slide_in_ms", r.slide_in_ms, d.slide_in_ms, 2000),
        slide_out_ms: clamp_ms("slide_out_ms", r.slide_out_ms, d.slide_out_ms, 2000),
        show_labels: r.show_labels.unwrap_or(d.show_labels),
        label_size,
        background_opacity,
        icon_opacity,
        launch_effect: r.launch_effect.unwrap_or(d.launch_effect),
        locked: r.locked.unwrap_or(d.locked),
        look: r.look.unwrap_or(d.look),
        hover_glow: r.hover_glow.unwrap_or(d.hover_glow),
        monitor: r.monitor.clone().filter(|m| !m.trim().is_empty()).unwrap_or(d.monitor),
        hotkey: match r.hotkey.as_deref().map(crate::hotkey::parse) {
            Some(Ok(Some(hotkey))) => crate::hotkey::format(hotkey),
            Some(Err(e)) => {
                warnings.push(format!("[dock] hotkey: {e}; no shortcut for now"));
                String::new()
            }
            _ => String::new(),
        },
    };

    let mut items = raw.items;
    normalize_items(&mut items);
    let needs_migration = version < CURRENT_VERSION;
    let read_only = version > CURRENT_VERSION;
    if read_only {
        warnings.push(format!(
            "this file is version {version}, newer than this dock understands ({CURRENT_VERSION}); it won't be saved over"
        ));
    }
    Ok(Parsed { config: Config { version, dock, items }, warnings, needs_migration, read_only })
}

fn normalize_items(items: &mut [ItemConfig]) {
    for item in items {
        if item.separator {
            item.kind = Kind::Separator;
            item.separator = false;
        }
        normalize_items(&mut item.items);
    }
}

/// Warnings for keys the dock doesn't know (typos, settings from a newer version), with line numbers.
fn unknown_keys(text: &str) -> Vec<String> {
    let Ok(doc) = toml_edit::Document::parse(text) else { return Vec::new() };
    let line = |span: Option<std::ops::Range<usize>>| span.map(|s| text[..s.start].matches('\n').count() + 1);
    let mut warnings = Vec::new();
    let mut check = |table: &Table, allowed: &[&str], place: &str| {
        for (key, _) in table.iter() {
            if !allowed.contains(&key) {
                let at = table.key(key).and_then(|k| line(k.span())).map(|n| format!("line {n}: ")).unwrap_or_default();
                warnings.push(format!("{at}unknown setting '{key}' in {place} (ignored)"));
            }
        }
    };
    check(doc.as_table(), ROOT_KEYS, "the file");
    if let Some(dock) = doc.get("dock").and_then(TomlItem::as_table) {
        check(dock, DOCK_KEYS, "[dock]");
    }
    fn items<'a>(item: Option<&'a TomlItem>) -> Vec<&'a Table> {
        item.and_then(TomlItem::as_array_of_tables).map(|a| a.iter().collect()).unwrap_or_default()
    }
    for table in items(doc.get("item")) {
        check(table, ITEM_KEYS, "an [[item]]");
        for child in items(table.get("items")) {
            check(child, ITEM_KEYS, "a group's item");
        }
    }
    warnings
}

/// Upgrades older files to the current format, keeping comments and layout:
/// adds `version`, turns `hover_scale` into `zoom_size`, `separator = true` into `kind = "separator"`.
pub fn migrate(text: &str) -> Result<String, String> {
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    if doc.get("version").is_none() {
        doc.insert("version", value(CURRENT_VERSION));
    }
    if let Some(dock) = doc.get_mut("dock").and_then(TomlItem::as_table_mut) {
        if let Some(scale) = dock.remove("hover_scale").and_then(|v| v.as_float().or(v.as_integer().map(|i| i as f64))) {
            if !dock.contains_key("zoom_size") {
                let icon = dock
                    .get("icon_size")
                    .and_then(|v| v.as_float().or(v.as_integer().map(|i| i as f64)))
                    .unwrap_or(53.0);
                let mut zoom = value((icon * scale).round() as i64);
                if let Some(v) = zoom.as_value_mut() {
                    v.decor_mut().set_suffix("  # how big a hovered icon gets (was hover_scale)");
                }
                dock.insert("zoom_size", zoom);
            }
        }
    }
    fn fix_items(item: Option<&mut TomlItem>) {
        let Some(array) = item.and_then(TomlItem::as_array_of_tables_mut) else { return };
        for table in array.iter_mut() {
            if let Some(separator) = table.remove("separator") {
                if separator.as_bool() == Some(true) && !table.contains_key("kind") {
                    table.insert("kind", value("separator"));
                }
            }
            fix_items(table.get_mut("items"));
        }
    }
    fix_items(doc.get_mut("item"));
    Ok(doc.to_string())
}

/// Decides when a saved dock.toml should be reloaded. A new file date must be seen on two
/// checks in a row, so a file that's still being written is never read half-finished.
#[derive(Debug, Default)]
pub struct ChangeWatch<T> {
    loaded: Option<T>,
    pending: Option<T>,
}

impl<T: PartialEq + Copy> ChangeWatch<T> {
    pub fn new(loaded: Option<T>) -> Self {
        Self { loaded, pending: None }
    }

    /// Records the version just loaded (or just tried, so a broken file is reported once).
    pub fn loaded(&mut self, stamp: Option<T>) {
        self.loaded = stamp;
        self.pending = None;
    }

    /// A new version has been seen once and is waiting for its second look.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Feed the current file date; returns true when it's time to reload.
    pub fn observe(&mut self, stamp: T) -> bool {
        if Some(stamp) == self.loaded {
            self.pending = None;
            false
        } else if self.pending != Some(stamp) {
            self.pending = Some(stamp);
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V0: &str = r#"# Desktop Dock test config
[dock]
icon_size = 53        # same as RocketDock
hover_scale = 1.2     # old name
spacing = 10

[[item]]
name = "Chrome"
target = 'C:\chrome.exe'

[[item]]
separator = true

[[item]]
name = "Discord"   # chat
target = 'C:\Discord\Update.exe'
"#;

    fn ok(text: &str) -> Parsed {
        parse(text).unwrap()
    }

    #[test]
    fn empty_file_gives_rocketdock_like_defaults() {
        let parsed = ok("");
        let dock = &parsed.config.dock;
        assert_eq!(dock.icon_size, 53.0);
        assert_eq!(dock.zoom_size, 64.0);
        assert_eq!(dock.popup_delay_ms, 350);
        assert_eq!(dock.hide_delay_ms, 250);
        assert_eq!(dock.slide_in_ms, 250);
        assert_eq!(dock.launch_effect, LaunchEffect::Bounce);
        assert!(parsed.config.items.is_empty());
        assert!(parsed.needs_migration, "no version line = version 0");
    }

    #[test]
    fn the_example_file_shows_every_setting_and_reads_cleanly() {
        let example = include_str!("../dock.example.toml");
        let parsed = ok(example);
        assert!(parsed.warnings.is_empty() && !parsed.needs_migration, "{:?}", parsed.warnings);
        for key in DOCK_KEYS.iter().filter(|&&key| key != "hover_scale") {
            assert!(example.contains(&format!("
{key} =")), "the example shows [dock] {key}");
        }
        for key in ITEM_KEYS.iter().filter(|&&key| !["app_id", "items", "separator"].contains(&key)) {
            assert!(example.contains(&format!("
{key} =")), "the example shows the item key {key}");
        }
    }

    #[test]
    fn hover_glow_and_the_glow_launch_effect() {
        assert_eq!(ok("").config.dock.hover_glow, HoverGlow::None, "off unless chosen");
        let dock = ok("version = 1\n[dock]\nhover_glow = \"icon\"\nlaunch_effect = \"glow\"").config.dock;
        assert_eq!(dock.hover_glow, HoverGlow::Icon);
        assert_eq!(dock.launch_effect, LaunchEffect::Glow);
        assert_eq!(ok("version = 1\n[dock]\nhover_glow = \"accent\"").config.dock.hover_glow, HoverGlow::Accent);
        assert!(ok("version = 1\n[dock]\nhover_glow = \"accent\"").warnings.is_empty(), "a known setting");
    }

    #[test]
    fn missing_settings_keep_their_defaults() {
        let dock = ok("version = 1\n[dock]\nicon_size = 64").config.dock;
        assert_eq!(dock.icon_size, 64.0);
        assert_eq!(dock.zoom_size, 77.0, "default zoom follows icon size (x1.2)");
        assert_eq!(dock.spacing, 10.0);
    }

    #[test]
    fn version_0_hover_scale_becomes_zoom_size() {
        assert_eq!(ok("[dock]\nicon_size = 53\nhover_scale = 2.2").config.dock.zoom_size, 117.0);
    }

    #[test]
    fn zoom_size_never_below_icon_size() {
        let parsed = ok("version = 1\n[dock]\nicon_size = 64\nzoom_size = 40");
        assert_eq!(parsed.config.dock.zoom_size, 64.0);
        assert!(parsed.warnings.iter().any(|w| w.contains("zoom_size")));
    }

    #[test]
    fn items_kinds_and_options_are_read() {
        let text = r#"
            version = 1
            [[item]]
            name = "Chrome"
            target = 'C:\Program Files\Google\Chrome\Application\chrome.exe'
            run = "maximized"
            admin = true

            [[item]]
            kind = "separator"

            [[item]]
            name = "Bin"
            kind = "recycle-bin"

            [[item]]
            name = "Adobe"
            kind = "group"
              [[item.items]]
              name = "Photoshop"
              target = 'C:\ps.exe'
        "#;
        let items = ok(text).config.items;
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].run, RunState::Maximized);
        assert!(items[0].admin);
        assert!(items[1].is_separator());
        assert_eq!(items[2].kind, Kind::RecycleBin);
        assert_eq!(items[3].kind, Kind::Group);
        assert_eq!(items[3].items[0].name, "Photoshop");
    }

    #[test]
    fn version_0_separator_is_still_understood() {
        assert!(ok("[[item]]\nseparator = true").config.items[0].is_separator());
    }

    #[test]
    fn out_of_range_values_are_clamped_with_warnings() {
        let text = "version = 1\n[dock]\nicon_size = 5000\nslide_in_ms = 99999\npopup_delay_ms = 99999";
        let parsed = ok(text);
        assert_eq!(parsed.config.dock.icon_size, 256.0);
        assert_eq!(parsed.config.dock.slide_in_ms, 2000);
        assert_eq!(parsed.config.dock.popup_delay_ms, 5000);
        assert_eq!(parsed.warnings.len(), 3);
    }

    #[test]
    fn the_shortcut_is_off_unless_set_and_a_bad_one_is_reported() {
        assert_eq!(ok("version = 1\n[dock]\n").config.dock.hotkey, "");
        assert_eq!(ok("version = 1\n[dock]\nhotkey = \"shift+ctrl+d\"\n").config.dock.hotkey, "Ctrl+Shift+D");
        let bad = ok("version = 1\n[dock]\nhotkey = \"D\"\n");
        assert_eq!(bad.config.dock.hotkey, "");
        assert!(bad.warnings.iter().any(|w| w.contains("hotkey") && w.contains("Ctrl or Alt")), "{:?}", bad.warnings);
    }

    #[test]
    fn the_screen_is_the_main_one_unless_one_is_chosen() {
        assert_eq!(ok("version = 1\n[dock]\n").config.dock.monitor, "main");
        assert_eq!(ok("version = 1\n[dock]\nmonitor = \"\"\n").config.dock.monitor, "main");
        let chosen = ok(r"version = 1
[dock]
monitor = '\\?\DISPLAY#DEL40A3#2'
");
        assert_eq!(chosen.config.dock.monitor, r"\\?\DISPLAY#DEL40A3#2");
        assert!(chosen.warnings.is_empty(), "{:?}", chosen.warnings);
    }

    #[test]
    fn numbers_that_arent_numbers_get_the_default() {
        let d = DockSettings::default();
        for bad in ["nan", "+nan", "-nan", "inf", "+inf", "-inf", "1e39", "-1e39"] {
            let text = format!("version = 1
[dock]
icon_size = {bad}
zoom_size = {bad}
spacing = {bad}
label_size = {bad}
");
            let parsed = ok(&text);
            let dock = &parsed.config.dock;
            assert_eq!((dock.icon_size, dock.zoom_size, dock.spacing, dock.label_size), (d.icon_size, (d.icon_size * 1.2).round(), d.spacing, d.label_size), "{bad}");
            assert_eq!(parsed.warnings.len(), 4, "{bad}: {:?}", parsed.warnings);
            let old = ok(&format!("[dock]
icon_size = 40
hover_scale = {bad}
"));
            assert_eq!(old.config.dock.zoom_size, 48.0, "{bad}: version 0 hover_scale");
        }
        // Too big even for the file reader: an error that says where, like any typo.
        assert!(parse("version = 1\n[dock]\nicon_size = 1e999\n").is_err_and(|e| e.contains("line 3")));
    }

    #[test]
    fn look_settings_default_to_todays_look_and_stay_in_range() {
        let plain = ok("version = 1
[dock]
icon_size = 53");
        assert_eq!((plain.config.dock.label_size, plain.config.dock.background_opacity, plain.config.dock.icon_opacity), (12.5, 100, 100));
        let set = ok("version = 1
[dock]
label_size = 16
background_opacity = 40
icon_opacity = 80");
        assert_eq!((set.config.dock.label_size, set.config.dock.background_opacity, set.config.dock.icon_opacity), (16.0, 40, 80));
        assert!(set.warnings.is_empty(), "{:?}", set.warnings);
        let wild = ok("version = 1
[dock]
label_size = 2
background_opacity = 250
icon_opacity = 0");
        assert_eq!((wild.config.dock.label_size, wild.config.dock.background_opacity, wild.config.dock.icon_opacity), (8.0, 100, 10));
        assert_eq!(wild.warnings.len(), 3, "an icon can't vanish entirely: {:?}", wild.warnings);
    }

    #[test]
    fn unknown_keys_are_warned_about_with_line_numbers() {
        let text = "version = 1\n[dock]\nicon_size = 53\nicon_sise = 60\n\n[[item]]\nname = \"x\"\ntraget = 'C:\\x.exe'\n";
        let warnings = ok(text).warnings;
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].starts_with("line 4:") && warnings[0].contains("icon_sise"));
        assert!(warnings[1].starts_with("line 8:") && warnings[1].contains("traget"));
    }

    #[test]
    fn typos_in_values_are_errors_that_say_where() {
        let error = parse("version = 1\n[dock]\nicon_size = 53\nthis line is not valid\n").unwrap_err();
        assert!(error.contains('4'), "mentions line 4: {error}");
        assert!(parse("version = 1\n[[item]]\nkind = \"grop\"").is_err());
        assert!(parse("[dock]\nicon_size = \"big\"").is_err());
    }

    #[test]
    fn newer_files_are_read_only() {
        let parsed = ok("version = 99");
        assert!(parsed.read_only);
        assert!(!parsed.needs_migration);
    }

    #[test]
    fn migration_upgrades_and_keeps_comments() {
        let migrated = migrate(V0).unwrap();
        let parsed = ok(&migrated);
        assert!(!parsed.needs_migration);
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert_eq!(parsed.config.dock.zoom_size, 64.0);
        assert!(parsed.config.items[1].is_separator());
        assert!(!migrated.contains("hover_scale ="), "old key removed");
        assert!(!migrated.contains("separator = true"));
        for comment in ["# Desktop Dock test config", "# same as RocketDock", "# chat"] {
            assert!(migrated.contains(comment), "kept {comment}");
        }
        assert_eq!(parsed.config.items, ok(V0).config.items, "same items before and after");
    }

    #[test]
    fn migration_is_idempotent() {
        let once = migrate(V0).unwrap();
        assert_eq!(migrate(&once).unwrap(), once);
    }

    #[test]
    fn unchanged_file_never_reloads() {
        let mut watch = ChangeWatch::new(Some(1));
        assert!(!watch.observe(1));
        assert!(!watch.observe(1));
    }

    #[test]
    fn saved_file_reloads_on_the_second_matching_check() {
        let mut watch = ChangeWatch::new(Some(1));
        assert!(!watch.observe(2), "first sighting: maybe still being written");
        assert!(watch.observe(2), "unchanged a second later: reload");
        watch.loaded(Some(2));
        assert!(!watch.observe(2), "after reloading, quiet again");
    }

    #[test]
    fn file_still_changing_waits_until_it_settles() {
        let mut watch = ChangeWatch::new(Some(1));
        assert!(!watch.observe(2));
        assert!(!watch.observe(3), "changed again: wait");
        assert!(watch.observe(3));
    }

    #[test]
    fn broken_file_is_reported_once() {
        let mut watch = ChangeWatch::new(Some(1));
        watch.observe(2);
        assert!(watch.observe(2));
        watch.loaded(Some(2)); // recorded even though it failed to parse
        assert!(!watch.observe(2));
        assert!(!watch.observe(2));
    }

    #[test]
    fn files_saved_as_utf16_read_like_utf8() {
        let text = "[dock]\r\nicon_size = 48\r\n\r\n[[item]]\r\nname = 'Tést'\r\n";
        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain(text.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain(text.encode_utf16().flat_map(u16::to_be_bytes)).collect();
        assert_eq!(decode(le).as_deref(), Some(text));
        assert_eq!(decode(be).as_deref(), Some(text));
        assert_eq!(decode(text.as_bytes().to_vec()).as_deref(), Some(text));
        // Cut off mid-character, or not text at all (ANSI "é"): said in plain words.
        assert_eq!(decode(vec![0xFF, 0xFE, b'a']), None);
        assert_eq!(decode(b"name = 'T\xe9st'".to_vec()), None);
        let dir = std::env::temp_dir().join(format!("dd-read-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dock.toml");
        std::fs::write(&path, [0x93, 0x94]).unwrap();
        let error = read_text(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), NOT_TEXT);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
