//! RocketDock import: reads RocketDock's settings (from the registry, or a `.reg` export) and
//! writes an equivalent dock.toml, plus a report of anything that needs attention.
//!
//! RocketDock stores everything as text values: dock settings under
//! `HKCU\Software\RocketDock`, items under `...\Icons` as "N-Title", "N-Command", etc.

use crate::target::{self, Class};
use std::collections::HashMap;
use std::path::Path;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW};
use windows::core::{HSTRING, PWSTR};

const IMAGE_EXTENSIONS: &[&str] = &["png", "ico", "jpg", "jpeg", "bmp", "gif", "tif", "tiff"];

#[derive(Debug, Default)]
pub struct RocketDock {
    pub settings: HashMap<String, String>,
    pub icons: HashMap<String, String>,
}

pub struct Imported {
    pub text: String,
    pub items: usize,
    pub report: Vec<String>,
}

impl RocketDock {
    /// Reads a `reg export` file (UTF-16 with its byte-order mark, UTF-8, or an older REGEDIT4
    /// export in this PC's code page).
    pub fn from_reg_file(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
        Self::from_reg_text(&crate::system::text_from_bytes(&bytes))
    }

    /// Parses a `reg export` of HKCU\Software\RocketDock (UTF-16 or UTF-8 text).
    pub fn from_reg_text(text: &str) -> Result<Self, String> {
        enum Section {
            Root,
            Icons,
            Other,
        }
        let mut rd = RocketDock::default();
        let mut section = Section::Other;
        for line in text.lines() {
            let line = line.trim_start_matches('\u{feff}').trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let lower = name.to_ascii_lowercase();
                section = if lower.ends_with(r"\software\rocketdock") {
                    Section::Root
                } else if lower.ends_with(r"\software\rocketdock\icons") {
                    Section::Icons
                } else {
                    Section::Other
                };
                continue;
            }
            let Some((name, rest)) = read_quoted(line) else { continue };
            let Some(value) = rest.strip_prefix('=') else { continue };
            let value = if let Some((text, _)) = read_quoted(value) {
                text
            } else if let Some(hex) = value.strip_prefix("dword:") {
                u32::from_str_radix(hex.trim(), 16).map(|n| n.to_string()).unwrap_or_default()
            } else {
                continue;
            };
            match section {
                Section::Root => rd.settings.insert(name, value),
                Section::Icons => rd.icons.insert(name, value),
                Section::Other => None,
            };
        }
        if rd.icons.is_empty() {
            return Err("That file isn't a RocketDock settings export.".into());
        }
        Ok(rd)
    }

    /// Reads RocketDock's settings straight from the registry.
    pub fn from_registry() -> Result<Self, String> {
        let settings = read_key(r"Software\RocketDock")?;
        let icons = read_key(r"Software\RocketDock\Icons")?;
        Ok(RocketDock { settings, icons })
    }
}

/// Reads a `"..."` with .reg escapes (`\\`, `\"`); returns the text and what follows it.
fn read_quoted(s: &str) -> Option<(String, &str)> {
    let mut chars = s.strip_prefix('"')?.char_indices();
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?.1),
            '"' => return Some((out, &s[i + 2..])),
            c => out.push(c),
        }
    }
    None
}

fn read_key(path: &str) -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_READ, &mut key) != ERROR_SUCCESS {
            return Err("RocketDock's settings aren't on this PC (it may never have run here).".into());
        }
        let mut index = 0;
        loop {
            let mut name = vec![0u16; 512];
            let mut name_len = name.len() as u32;
            let mut data = vec![0u8; 8192];
            let mut data_len = data.len() as u32;
            let mut kind = 0u32;
            let status = RegEnumValueW(
                key,
                index,
                Some(PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            );
            if status != ERROR_SUCCESS {
                break;
            }
            let name = String::from_utf16_lossy(&name[..name_len as usize]);
            let value = if kind == 4 && data_len >= 4 {
                // REG_DWORD
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]).to_string()
            } else {
                let wide: Vec<u16> =
                    data[..data_len as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&wide).trim_end_matches('\0').to_string()
            };
            values.insert(name, value);
            index += 1;
        }
        let _ = RegCloseKey(key);
    }
    Ok(values)
}

/// Converts RocketDock's settings into dock.toml text. `exists` and `icon_count` look at the
/// disk (injected so tests can run without it).
pub fn convert(rd: &RocketDock, exists: impl Fn(&str) -> bool, icon_count: impl Fn(&str) -> u32) -> Imported {
    let setting = |name: &str| rd.settings.get(name).map(|v| v.trim().to_string()).unwrap_or_default();
    let number = |name: &str, default: i64| setting(name).parse::<i64>().unwrap_or(default);
    let mut report = Vec::new();
    let mut out = String::new();

    out.push_str("# Desktop Dock - imported from RocketDock.\n");
    out.push_str("# Edit freely: the dock reloads this file when you save it.\n");
    out.push_str("# Paths use single quotes so backslashes need no escaping.\n\n");
    out.push_str("version = 1\n\n[dock]\n");
    let slide = number("AutoHideTicks", 250);
    out.push_str(&format!("icon_size = {}       # RocketDock: IconMin\n", number("IconMin", 53)));
    out.push_str(&format!("zoom_size = {}      # RocketDock: IconMax\n", number("IconMax", 64)));
    out.push_str(&format!("zoom_ms = {}        # RocketDock: ZoomTicks\n", number("ZoomTicks", 120)));
    out.push_str("spacing = 10\n");
    out.push_str(&format!("popup_delay_ms = {} # RocketDock: PopupDelay\n", number("PopupDelay", 350)));
    out.push_str(&format!("hide_delay_ms = {}  # RocketDock: AutoHideDelay\n", number("AutoHideDelay", 250)));
    out.push_str(&format!("slide_in_ms = {slide}    # RocketDock: AutoHideTicks\n"));
    out.push_str(&format!("slide_out_ms = {slide}\n"));
    out.push_str(&format!("show_labels = {}\n", setting("HideLabels") != "1"));
    let effect = if setting("IconActivationFX") == "0" { "none" } else { "bounce" };
    out.push_str(&format!("launch_effect = \"{effect}\"\n"));

    let count: usize = rd.icons.get("count").and_then(|c| c.trim().parse().ok()).unwrap_or(0);
    let mut dropped_icons = 0;
    let mut versioned = Vec::new();
    for i in 0..count {
        let get = |key: &str| rd.icons.get(&format!("{i}-{key}")).map(|v| v.trim().to_string()).unwrap_or_default();
        out.push_str("\n[[item]]\n");
        if get("IsSeparator") == "1" {
            out.push_str("kind = \"separator\"\n");
            continue;
        }
        let name = get("Title");
        let command = strip_quotes(&get("Command"));
        if command.eq_ignore_ascii_case("[RecycleBin]") {
            out.push_str(&format!("name = {}\nkind = \"recycle-bin\"\n", toml_str(&name)));
            continue;
        }
        out.push_str(&format!("name = {}\ntarget = {}\n", toml_str(&name), toml_str(&command)));
        // Only paths can be looked for: not shell locations, links or names Windows looks up.
        let is_shell_or_url = !matches!(target::classify(&command), Class::Absolute | Class::Relative);
        if !is_shell_or_url && !exists(&command) {
            report.push(format!("{name}: {command} wasn't found"));
        }
        let args = get("Arguments");
        if !args.is_empty() {
            out.push_str(&format!("args = {}\n", toml_str(&args)));
        }
        let start_in = strip_quotes(&get("WorkingDirectory"));
        if !start_in.is_empty() && !is_shell_or_url && !same_path(&start_in, parent_of(&command)) {
            if exists(&start_in) {
                out.push_str(&format!("start_in = {}\n", toml_str(&start_in)));
            } else {
                report.push(format!("{name}: its start-in folder {start_in} isn't there any more, so it starts in the program's own folder"));
            }
        }
        match get("ShowCmd").as_str() {
            "1" => out.push_str("run = \"minimized\"\n"),
            "2" => out.push_str("run = \"maximized\"\n"),
            _ => {}
        }
        match map_icon(&get("FileName"), &command, &icon_count) {
            IconChoice::Keep(icon) => out.push_str(&format!("icon = {}\n", toml_str(&icon))),
            IconChoice::Dropped => dropped_icons += 1,
            IconChoice::Default => {}
        }
        if has_year_folder(&command) {
            versioned.push(name);
        }
    }
    if dropped_icons > 0 {
        report.push(format!(
            "{} couldn't be brought over; those items use the program's own icon",
            if dropped_icons == 1 { "1 icon".to_string() } else { format!("{dropped_icons} icons") }
        ));
    }
    if !versioned.is_empty() {
        report.push(format!(
            "{} a version number in {} folder ({}). If an update moves {}, the dock finds the new folder by itself",
            if versioned.len() == 1 { "1 item has".to_string() } else { format!("{} items have", versioned.len()) },
            if versioned.len() == 1 { "its" } else { "their" },
            versioned.join(", "),
            if versioned.len() == 1 { "it" } else { "them" }
        ));
    }
    Imported { text: out, items: count, report }
}

enum IconChoice {
    /// Use this icon (image path or `file,-id`).
    Keep(String),
    /// Same as the item's own icon; nothing to write.
    Default,
    /// RocketDock's reference wasn't a real icon number.
    Dropped,
}

/// Maps RocketDock's "FileName" (an image path, or `file?number`) to our `icon`.
fn map_icon(file_name: &str, target: &str, icon_count: &impl Fn(&str) -> u32) -> IconChoice {
    if file_name.is_empty() {
        return IconChoice::Default;
    }
    let (file, number) = match file_name.rsplit_once('?') {
        Some((file, n)) if n.trim().parse::<i64>().is_ok() => (file.to_string(), n.trim().parse::<i64>().ok()),
        _ => (file_name.to_string(), None),
    };
    let is_image = Path::new(&file)
        .extension()
        .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|known| ext.eq_ignore_ascii_case(known)));
    if is_image {
        return IconChoice::Keep(file);
    }
    let same_as_target = same_path(&file, target);
    match number {
        None if same_as_target => IconChoice::Default,
        None => IconChoice::Keep(file),
        // Negative numbers are resource ids (e.g. imageres.dll's folder icons): always valid.
        Some(n) if n < 0 => IconChoice::Keep(format!("{file},{n}")),
        Some(n) if (n as u64) < icon_count(&file) as u64 => {
            if same_as_target && n == 0 { IconChoice::Default } else { IconChoice::Keep(format!("{file},{n}")) }
        }
        Some(_) => IconChoice::Dropped,
    }
}

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s).trim().to_string()
}

fn parent_of(path: &str) -> &str {
    path.rfind('\\').map(|i| &path[..i]).unwrap_or("")
}

fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_end_matches('\\').to_ascii_lowercase();
    !a.trim().is_empty() && norm(a) == norm(b)
}

/// True if a folder in the path contains a year-like number (e.g. "Adobe Photoshop 2026").
fn has_year_folder(path: &str) -> bool {
    let folders: Vec<&str> = path.split('\\').collect();
    folders.iter().take(folders.len().saturating_sub(1)).any(|folder| {
        folder
            .split(|c: char| !c.is_ascii_digit())
            .any(|run| run.len() == 4 && (run.starts_with("19") || run.starts_with("20")))
    })
}

/// A TOML string: single-quoted (no escaping) when possible, otherwise a basic string.
fn toml_str(s: &str) -> String {
    if !s.contains('\'') && !s.chars().any(char::is_control) {
        format!("'{s}'")
    } else {
        toml_edit::Value::from(s).to_string().trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, Kind};

    fn backup() -> RocketDock {
        // A made-up export with the same cases a real one has (tests/fixtures).
        let bytes = include_bytes!("../tests/fixtures/rocketdock-sample.reg");
        let wide: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        RocketDock::from_reg_text(&String::from_utf16_lossy(&wide)).unwrap()
    }

    fn fake_exists(path: &str) -> bool {
        !path.contains("app-1.0.9158") // the chat app's old version folder is gone; everything else exists
    }

    fn fake_icon_count(file: &str) -> u32 {
        if file.ends_with(".bat") { 0 } else if file.contains("imageres") { 400 } else { 5 }
    }

    #[test]
    fn reg_text_parsing_handles_escapes_and_sections() {
        let text = "Windows Registry Editor Version 5.00\r\n\r\n[HKEY_CURRENT_USER\\Software\\RocketDock]\r\n\"IconMin\"=\"53\"\r\n\r\n[HKEY_CURRENT_USER\\Software\\RocketDock\\Icons]\r\n\"0-Command\"=\"\\\"C:\\\\a b\\\\x.bat\\\"\"\r\n\"count\"=dword:00000001\r\n\r\n[HKEY_CURRENT_USER\\Software\\RocketDock\\WindowFilters]\r\n\"count\"=\"0\"\r\n";
        let rd = RocketDock::from_reg_text(text).unwrap();
        assert_eq!(rd.settings["IconMin"], "53");
        assert_eq!(rd.icons["0-Command"], r#""C:\a b\x.bat""#);
        assert_eq!(rd.icons["count"], "1", "the WindowFilters count doesn't overwrite it");
    }

    #[test]
    fn icon_references_map_to_ours() {
        let count = fake_icon_count;
        let chrome = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
        assert!(matches!(map_icon(&format!("{chrome}?15014248"), chrome, &count), IconChoice::Dropped));
        assert!(matches!(map_icon(chrome, chrome, &count), IconChoice::Default));
        match map_icon(r"C:\Windows\System32\imageres.dll?-109", "::{20D04FE0}", &count) {
            IconChoice::Keep(icon) => assert_eq!(icon, r"C:\Windows\System32\imageres.dll,-109"),
            _ => panic!("resource ids are kept"),
        }
        match map_icon(r"C:\Users\alex\Pictures\Icons\x.png", chrome, &count) {
            IconChoice::Keep(icon) => assert_eq!(icon, r"C:\Users\alex\Pictures\Icons\x.png"),
            _ => panic!("images are kept"),
        }
        assert!(matches!(map_icon(r"C:\Scripts\go.bat?51", r"C:\Scripts\go.bat", &count), IconChoice::Dropped));
    }

    #[test]
    fn year_folders_are_spotted() {
        assert!(has_year_folder(r"C:\Program Files\Adobe\Adobe Photoshop 2026\Photoshop.exe"));
        assert!(!has_year_folder(r"C:\Program Files\Image-Line\FL Studio\FL64.exe"), "64 isn't a year");
        assert!(!has_year_folder(r"C:\Apps\Tool2026.exe"), "only folders count");
    }

    #[test]
    fn toml_strings_are_safe() {
        assert_eq!(toml_str(r"C:\x y\z.exe"), r"'C:\x y\z.exe'");
        assert_eq!(toml_str("it's"), "\"it's\"");
    }

    /// Golden test: a RocketDock export converts to a clean v1 dock.toml.
    #[test]
    fn a_rocketdock_export_imports_cleanly() {
        let imported = convert(&backup(), fake_exists, fake_icon_count);
        let parsed = config::parse(&imported.text).unwrap_or_else(|e| panic!("{e}\n{}", imported.text));
        assert!(parsed.warnings.is_empty(), "{:?}", parsed.warnings);
        assert!(!parsed.needs_migration);
        let dock = &parsed.config.dock;
        assert_eq!((dock.icon_size, dock.zoom_size, dock.zoom_ms), (53.0, 117.0, 200));
        assert_eq!((dock.popup_delay_ms, dock.hide_delay_ms, dock.slide_in_ms), (350, 250, 250));
        assert!(dock.show_labels);

        let items = &parsed.config.items;
        assert_eq!(imported.items, 9);
        assert_eq!(items.len(), 9, "8 items + 1 separator, in order");
        assert_eq!(items[0].name, "This PC");
        assert_eq!(items[0].target, "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}");
        assert_eq!(items[0].icon, r"C:\Windows\System32\imageres.dll,-109");
        assert_eq!(items[2].icon, r"C:\Users\alex\Pictures\Icons\mail.png");
        assert_eq!(items[3].name, "Browser");
        assert!(items[3].icon.is_empty(), "the browser's bogus ?15014248 dropped");
        assert_eq!(items[4].name, "Chat");
        assert_eq!(items[4].args, "--processStart Chat.exe");
        assert!(items[4].start_in.is_empty(), "missing versioned folder dropped");
        assert!(!items[6].target.contains('"'), "quotes stripped: {}", items[6].target);
        assert!(items[7].is_separator());
        assert_eq!(items[8].kind, Kind::RecycleBin);

        let report = imported.report.join("\n");
        assert!(report.contains("app-1.0.9158"), "{report}");
        assert!(report.contains("Example Editor 2026"), "{report}");
        assert!(report.contains("couldn't be brought over"), "{report}");
    }
}
