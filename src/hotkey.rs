//! The keyboard shortcut that brings the dock out (`[dock] hotkey`, off unless set), for a
//! screen whose top edge is hard to reach, or for keyboard users. Written as
//! "Ctrl+Shift+D": Ctrl, Alt and Shift, then one key. The Windows key isn't offered (Windows
//! keeps most of its shortcuts, and the settings window's shortcut box can't record it).

/// Modifier flags as `RegisterHotKey` takes them (MOD_*).
pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
/// Holding the keys down doesn't repeat it.
pub const MOD_NOREPEAT: u32 = 0x4000;

/// Modifier flags as the settings window's shortcut box (a Windows hotkey control) gives them
/// (HOTKEYF_*): Shift and Alt are the other way round from MOD_*.
pub const HOTKEYF_SHIFT: u32 = 0x01;
pub const HOTKEYF_CONTROL: u32 = 0x02;
pub const HOTKEYF_ALT: u32 = 0x04;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    /// MOD_ALT, MOD_CONTROL and MOD_SHIFT.
    pub modifiers: u32,
    /// The virtual-key code.
    pub key: u32,
}

/// The keys a shortcut can end in, by name (letters and digits are their own names).
const NAMED: &[(&str, u32)] = &[
    ("Space", 0x20), ("PageUp", 0x21), ("PageDown", 0x22), ("End", 0x23), ("Home", 0x24),
    ("Left", 0x25), ("Up", 0x26), ("Right", 0x27), ("Down", 0x28), ("Insert", 0x2D), ("Delete", 0x2E),
    ("Num0", 0x60), ("Num1", 0x61), ("Num2", 0x62), ("Num3", 0x63), ("Num4", 0x64),
    ("Num5", 0x65), ("Num6", 0x66), ("Num7", 0x67), ("Num8", 0x68), ("Num9", 0x69),
    ("Pause", 0x13), ("ScrollLock", 0x91),
    ("`", 0xC0), ("-", 0xBD), ("=", 0xBB), ("[", 0xDB), ("]", 0xDD), ("\\", 0xDC), (";", 0xBA), ("'", 0xDE), (",", 0xBC), (".", 0xBE), ("/", 0xBF),
];

fn key_from_name(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    if upper.len() == 1 {
        let c = upper.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(u32::from(c));
        }
    }
    if let Some(n) = upper.strip_prefix('F').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=24).contains(n)) {
        return Some(0x6F + n); // VK_F1 = 0x70
    }
    NAMED.iter().find(|(known, _)| known.eq_ignore_ascii_case(name)).map(|&(_, key)| key)
}

fn key_name(key: u32) -> Option<String> {
    match key {
        0x30..=0x39 | 0x41..=0x5A => Some(char::from(key as u8).to_string()),
        0x70..=0x87 => Some(format!("F{}", key - 0x6F)),
        _ => NAMED.iter().find(|&&(_, known)| known == key).map(|(name, _)| name.to_string()),
    }
}

/// Reads "Ctrl+Shift+D" (any case, spaces allowed). Empty means no shortcut: Ok(None). A
/// shortcut needs Ctrl or Alt (on their own, Shift and plain keys are for typing), except F-keys.
pub fn parse(text: &str) -> Result<Option<Hotkey>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let mut modifiers = 0;
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= MOD_CONTROL,
            "alt" => modifiers |= MOD_ALT,
            "shift" => modifiers |= MOD_SHIFT,
            "" => return Err(format!("\"{text}\" isn't a shortcut (written like \"Ctrl+Shift+D\")")),
            _ if key.is_some() => return Err(format!("\"{text}\" has more than one key")),
            _ => key = Some(key_from_name(part).ok_or_else(|| format!("\"{part}\" isn't a key a shortcut can use"))?),
        }
    }
    let key = key.ok_or_else(|| format!("\"{text}\" has no key, only Ctrl, Alt or Shift"))?;
    let function_key = (0x70..=0x87).contains(&key);
    if modifiers & (MOD_CONTROL | MOD_ALT) == 0 && !function_key {
        return Err(format!("\"{text}\" needs Ctrl or Alt, so it doesn't get in the way of typing"));
    }
    Ok(Some(Hotkey { modifiers, key }))
}

/// "Ctrl+Shift+D", the way `parse` reads it.
pub fn format(hotkey: Hotkey) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (flag, name) in [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift")] {
        if hotkey.modifiers & flag != 0 {
            parts.push(name.into());
        }
    }
    parts.push(key_name(hotkey.key).unwrap_or_else(|| format!("0x{:02X}", hotkey.key)));
    parts.join("+")
}

/// From the shortcut box's value (HKM_GETHOTKEY: key in the low byte, HOTKEYF_* in the next).
pub fn from_control(value: u32) -> Option<Hotkey> {
    let key = value & 0xFF;
    let flags = (value >> 8) & 0xFF;
    if key == 0 {
        return None;
    }
    let mut modifiers = 0;
    if flags & HOTKEYF_CONTROL != 0 {
        modifiers |= MOD_CONTROL;
    }
    if flags & HOTKEYF_ALT != 0 {
        modifiers |= MOD_ALT;
    }
    if flags & HOTKEYF_SHIFT != 0 {
        modifiers |= MOD_SHIFT;
    }
    Some(Hotkey { modifiers, key })
}

/// For the shortcut box (HKM_SETHOTKEY): the reverse of `from_control`.
pub fn to_control(hotkey: Hotkey) -> u32 {
    let mut flags = 0;
    if hotkey.modifiers & MOD_CONTROL != 0 {
        flags |= HOTKEYF_CONTROL;
    }
    if hotkey.modifiers & MOD_ALT != 0 {
        flags |= HOTKEYF_ALT;
    }
    if hotkey.modifiers & MOD_SHIFT != 0 {
        flags |= HOTKEYF_SHIFT;
    }
    (flags << 8) | (hotkey.key & 0xFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_are_read_in_any_case_and_written_back_neatly() {
        let ctrl_shift_d = Hotkey { modifiers: MOD_CONTROL | MOD_SHIFT, key: 0x44 };
        assert_eq!(parse("Ctrl+Shift+D"), Ok(Some(ctrl_shift_d)));
        assert_eq!(parse(" shift + ctrl + d "), Ok(Some(ctrl_shift_d)));
        assert_eq!(format(ctrl_shift_d), "Ctrl+Shift+D");
        assert_eq!(parse("Alt+F12").map(|h| h.map(format)), Ok(Some("Alt+F12".into())));
        assert_eq!(parse("F9").map(|h| h.map(format)), Ok(Some("F9".into())), "a function key on its own is fine");
        assert_eq!(parse("Ctrl+Alt+Space").map(|h| h.map(format)), Ok(Some("Ctrl+Alt+Space".into())));
        assert_eq!(parse("ctrl+`").map(|h| h.map(format)), Ok(Some("Ctrl+`".into())));
        assert_eq!(parse(""), Ok(None), "empty: no shortcut");
    }

    #[test]
    fn shortcuts_that_would_get_in_the_way_are_refused_with_a_reason() {
        assert!(parse("D").unwrap_err().contains("Ctrl or Alt"));
        assert!(parse("Shift+D").unwrap_err().contains("Ctrl or Alt"));
        assert!(parse("Ctrl+Shift").unwrap_err().contains("no key"));
        assert!(parse("Ctrl+D+E").unwrap_err().contains("more than one key"));
        assert!(parse("Ctrl+Banana").unwrap_err().contains("Banana"));
        assert!(parse("Ctrl++").is_err());
        assert!(parse("Win+D").is_err(), "the Windows key isn't offered");
    }

    #[test]
    fn the_shortcut_box_and_windows_swap_shift_and_alt() {
        // The box says Shift = 1, Alt = 4; RegisterHotKey says Alt = 1, Shift = 4.
        let from_box = from_control((HOTKEYF_SHIFT << 8) | 0x44).unwrap();
        assert_eq!(from_box.modifiers, MOD_SHIFT);
        let from_box = from_control((HOTKEYF_ALT << 8) | 0x44).unwrap();
        assert_eq!(from_box.modifiers, MOD_ALT);
        for text in ["Ctrl+Shift+D", "Alt+F12", "Ctrl+Alt+Shift+1", "F9"] {
            let hotkey = parse(text).unwrap().unwrap();
            assert_eq!(from_control(to_control(hotkey)), Some(hotkey), "{text} survives the trip");
        }
        assert_eq!(from_control(0), None, "an empty box: no shortcut");
    }
}
