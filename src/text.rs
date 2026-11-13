//! The words the dock shows, in the language in use.
//!
//! English is written in the code itself (and in the dialogs in `desktop-dock.rc`): it's what
//! each translation is looked up by, and what any text a language lacks falls back to. Every
//! other language is a file in `lang\` compiled into the program, of lines like
//! `"Lock icons" = "Symbole sperren"`. A text with a count gives one form per plural category of
//! the language (`"{n} item" = ["{n} Element", "{n} Elemente"]`).
//!
//! `tr!("…")` is a text; `tr!("Moved {name}", name = label)` fills in its blanks; `trn!("{n} item",
//! "{n} items", count)` picks the form for the count. `translate_dialog` puts a dialog's own texts
//! into the language as it opens.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A text in the language in use (English if it has none). With `name = value` pairs, the
/// text's `{name}` blanks are filled in (and it's a `String`).
macro_rules! tr {
    ($english:literal) => {
        $crate::text::t($english)
    };
    ($english:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::text::tf($english, &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+])
    };
}

/// A text that depends on a count: the English for one and for more, the count (shown as
/// `{n}`), and any other blanks. Each language has its own forms and rules for which one a count
/// takes.
macro_rules! trn {
    ($one:literal, $other:literal, $n:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $crate::text::tn($one, $other, ($n) as u64, &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),*])
    };
}

/// `tr!` as Windows wants it (an `HSTRING`).
macro_rules! wtr {
    ($($args:tt)*) => {
        ::windows::core::HSTRING::from(tr!($($args)*))
    };
}

/// A text written the same in English as another but not in every language (setup's "Remove"
/// is uninstalling, the Items page's is taking an item off): looked up as `place|English`
/// (`"uninstall|Remove"` in a language's file), the English when a language has none.
macro_rules! trc {
    ($place:literal, $english:literal) => {
        $crate::text::tc(concat!($place, "|", $english), $english)
    };
}

/// An English text kept in a table and shown later, with `text::t` where it's shown: noted here
/// so it's translated, and left as it is.
macro_rules! n {
    ($english:literal) => {
        $english
    };
}

/// A language the dock speaks.
pub struct Language {
    /// As written in dock.toml (`language = "de"`).
    pub code: &'static str,
    /// Its name in itself, as the Language list shows it.
    pub name: &'static str,
    /// Its translations (empty for English).
    source: &'static str,
    /// How many plural forms its texts with a count have, and which one a count takes.
    pub forms: usize,
    plural: fn(u64) -> usize,
}

/// English first: the language of the code, and the fallback.
pub static LANGUAGES: [Language; 8] = [
    Language { code: "en", name: "English", source: "", forms: 2, plural: one_other },
    Language { code: "de", name: "Deutsch", source: include_str!("../lang/de.toml"), forms: 2, plural: one_other },
    Language { code: "fr", name: "Français", source: include_str!("../lang/fr.toml"), forms: 2, plural: zero_one_other },
    Language { code: "es", name: "Español", source: include_str!("../lang/es.toml"), forms: 2, plural: one_other },
    Language { code: "pt-BR", name: "Português (Brasil)", source: include_str!("../lang/pt-BR.toml"), forms: 2, plural: zero_one_other },
    Language { code: "ru", name: "Русский", source: include_str!("../lang/ru.toml"), forms: 3, plural: russian },
    Language { code: "zh-CN", name: "中文(简体)", source: include_str!("../lang/zh-CN.toml"), forms: 1, plural: no_plural },
    Language { code: "ja", name: "日本語", source: include_str!("../lang/ja.toml"), forms: 1, plural: no_plural },
];

// The plural rules (Unicode CLDR's, for whole numbers).
fn one_other(n: u64) -> usize {
    if n == 1 { 0 } else { 1 }
}
fn zero_one_other(n: u64) -> usize {
    if n <= 1 { 0 } else { 1 }
}
fn russian(n: u64) -> usize {
    let (last, last_two) = (n % 10, n % 100);
    if last == 1 && last_two != 11 {
        0
    } else if (2..=4).contains(&last) && !(12..=14).contains(&last_two) {
        1
    } else {
        2
    }
}
fn no_plural(_: u64) -> usize {
    0
}

enum Entry {
    One(String),
    Forms(Vec<String>),
}

static TABLES: [OnceLock<HashMap<String, Entry>>; 8] = [const { OnceLock::new() }; 8];
static CURRENT: AtomicUsize = AtomicUsize::new(0);

fn table(index: usize) -> &'static HashMap<String, Entry> {
    TABLES[index].get_or_init(|| parse(LANGUAGES[index].source).unwrap_or_default())
}

/// A language file's `[text]` table. (A file that doesn't read leaves its language in English;
/// the tests make sure none ships like that.)
fn parse(source: &str) -> Result<HashMap<String, Entry>, String> {
    if source.is_empty() {
        return Ok(HashMap::new());
    }
    let doc: toml::Table = source.parse().map_err(|e: toml::de::Error| e.to_string())?;
    let Some(text) = doc.get("text").and_then(toml::Value::as_table) else { return Err("no [text] table".into()) };
    let mut entries = HashMap::new();
    for (english, value) in text {
        let entry = match value {
            toml::Value::String(s) => Entry::One(s.clone()),
            toml::Value::Array(forms) => Entry::Forms(forms.iter().map(|f| f.as_str().map(str::to_string).ok_or_else(|| format!("'{english}': a form that isn't text"))).collect::<Result<_, _>>()?),
            _ => return Err(format!("'{english}': neither a text nor a list of forms")),
        };
        entries.insert(english.clone(), entry);
    }
    Ok(entries)
}

/// The language in use.
pub fn current() -> &'static Language {
    &LANGUAGES[CURRENT.load(Ordering::Relaxed)]
}

/// Uses the language the setting names (`language` in dock.toml): a code, or "auto" (or
/// anything else) for Windows' own display language.
pub fn use_setting(setting: &str) {
    CURRENT.store(chosen(setting), Ordering::Relaxed);
}

/// The language a setting gives: the one it names, or Windows' choice.
pub fn chosen(setting: &str) -> usize {
    LANGUAGES.iter().position(|language| language.code.eq_ignore_ascii_case(setting.trim())).unwrap_or_else(windows_choice)
}

/// The first of Windows' display languages (as set in Settings > Time & language) the dock has,
/// or English.
pub fn windows_choice() -> usize {
    preferred_languages().iter().find_map(|tag| matching(tag)).unwrap_or(0)
}

/// The dock's language for a Windows language tag ("de-AT", "pt-PT", "zh-Hans-CN"), if any.
/// Chinese written in Traditional characters (Taiwan, Hong Kong, Macao) has none yet.
fn matching(tag: &str) -> Option<usize> {
    let tag = tag.trim().to_ascii_lowercase();
    if let Some(exact) = LANGUAGES.iter().position(|language| language.code.eq_ignore_ascii_case(&tag)) {
        return Some(exact);
    }
    let base = tag.split('-').next().unwrap_or_default();
    if base == "zh" {
        let traditional = tag.contains("hant") || ["-tw", "-hk", "-mo"].iter().any(|region| tag.ends_with(region));
        return if traditional { None } else { LANGUAGES.iter().position(|language| language.code == "zh-CN") };
    }
    LANGUAGES.iter().position(|language| language.code.split('-').next().is_some_and(|code| code.eq_ignore_ascii_case(base)))
}

/// Windows' display languages for this user, most wanted first ("de-DE", "en-US").
fn preferred_languages() -> Vec<String> {
    use windows::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    use windows::core::PWSTR;
    unsafe {
        let (mut count, mut len) = (0u32, 0u32);
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut len).is_err() || len == 0 {
            return Vec::new();
        }
        let mut buffer = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, Some(PWSTR(buffer.as_mut_ptr())), &mut len).is_err() {
            return Vec::new();
        }
        String::from_utf16_lossy(&buffer).split('\0').filter(|tag| !tag.is_empty()).map(str::to_string).collect()
    }
}

/// `tr!` without blanks.
pub fn t(english: &'static str) -> &'static str {
    let index = CURRENT.load(Ordering::Relaxed);
    if index == 0 {
        return english;
    }
    match table(index).get(english) {
        Some(Entry::One(text)) => text.as_str(),
        _ => english,
    }
}

/// `trc!`: the text for `key` (`place|English`), or the English.
pub fn tc(key: &'static str, english: &'static str) -> &'static str {
    let index = CURRENT.load(Ordering::Relaxed);
    match table(index).get(key) {
        Some(Entry::One(text)) if index != 0 => text.as_str(),
        _ => english,
    }
}

/// `tr!` with blanks.
pub fn tf(english: &'static str, args: &[(&str, &dyn Display)]) -> String {
    fill(t(english), args)
}

/// `trn!`.
pub fn tn(one: &'static str, other: &'static str, n: u64, args: &[(&str, &dyn Display)]) -> String {
    let index = CURRENT.load(Ordering::Relaxed);
    let english = if n == 1 { one } else { other };
    let template = if index == 0 {
        english
    } else {
        match table(index).get(one) {
            // (A list that isn't the language's forms is a file to fix: English meanwhile.)
            Some(Entry::Forms(forms)) if forms.len() == LANGUAGES[index].forms => forms[(LANGUAGES[index].plural)(n)].as_str(),
            Some(Entry::One(text)) => text.as_str(),
            Some(Entry::Forms(_)) | None => english,
        }
    };
    let n = n.to_string();
    let mut all: Vec<(&str, &dyn Display)> = vec![("n", &n)];
    all.extend_from_slice(args);
    fill(template, &all)
}

/// A text with each `{name}` replaced by its value, in one pass: a value that itself holds a
/// blank's name (an item called "{group}") is left as it is.
fn fill(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}').and_then(|close| args.iter().find(|(name, _)| *name == &after[..close]).map(|(_, value)| (close, value))) {
            Some((close, value)) => {
                out.push_str(&value.to_string());
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A moment the dock wrote down ("2026-11-11 01:21", local time, as its stores keep it), as
/// Windows writes a short date and time in your region ("11/11/2026 1:21 AM", "11.11.2026 01:21").
/// Anything else comes back as it is.
pub fn when(stamp: &str) -> String {
    use windows::Win32::Foundation::SYSTEMTIME;
    use windows::Win32::Globalization::{DATE_SHORTDATE, GetDateFormatEx, GetTimeFormatEx, TIME_NOSECONDS};
    use windows::core::PCWSTR;
    let number = |range: std::ops::Range<usize>| stamp.get(range).and_then(|s| s.parse::<u16>().ok());
    let parsed = (|| {
        let bytes = stamp.as_bytes();
        if bytes.len() < 16 || bytes[4] != b'-' || bytes[7] != b'-' || !matches!(bytes[10], b' ' | b'T') || bytes[13] != b':' {
            return None;
        }
        Some(SYSTEMTIME { wYear: number(0..4)?, wMonth: number(5..7)?, wDay: number(8..10)?, wHour: number(11..13)?, wMinute: number(14..16)?, ..Default::default() })
    })();
    let Some(time) = parsed else { return stamp.to_string() };
    let user = PCWSTR::null(); // the user's own region
    let mut date = [0u16; 64];
    let mut clock = [0u16; 64];
    unsafe {
        let d = GetDateFormatEx(user, DATE_SHORTDATE, Some(&time), PCWSTR::null(), Some(&mut date), PCWSTR::null());
        let c = GetTimeFormatEx(user, TIME_NOSECONDS, Some(&time), PCWSTR::null(), Some(&mut clock));
        if d <= 1 || c <= 1 {
            return stamp.to_string();
        }
        format!("{} {}", String::from_utf16_lossy(&date[..d as usize - 1]), String::from_utf16_lossy(&clock[..c as usize - 1]))
    }
}

/// A number with `decimals` places, written with your region's decimal sign ("14.5", "14,5").
pub fn number(value: f64, decimals: usize) -> String {
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SDECIMAL};
    use windows::core::PCWSTR;
    let text = format!("{value:.decimals$}");
    let mut sign = [0u16; 8];
    let len = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_SDECIMAL, Some(&mut sign)) };
    if len <= 1 {
        return text;
    }
    text.replace('.', &String::from_utf16_lossy(&sign[..len as usize - 1]))
}

/// What's known of a dialog control, by window: its English text as the .rc wrote it (empty: the
/// code fills it, so it's left alone), the text last put in it, and its control number. So a
/// dialog can be put into another language while it's open (the Language setting changed); a
/// window showing something else, or numbered differently, is a new one with a closed one's handle.
struct Known {
    english: String,
    last: String,
    id: i32,
}
static ENGLISH: std::sync::Mutex<Option<HashMap<isize, Known>>> = std::sync::Mutex::new(None);

/// Puts a dialog's own texts (its title, and the text of each label, button and box, all written
/// in English in the .rc) into the language in use: as it opens, and again (from the English it
/// had) when the language changes. What the person typed or chose is left alone.
pub fn translate_dialog(dialog: windows::Win32::Foundation::HWND) {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};
    use windows::core::BOOL;
    translate_window(dialog);
    unsafe extern "system" fn each(child: HWND, _: LPARAM) -> BOOL {
        let mut class = [0u16; 32];
        let len = unsafe { GetClassNameW(child, &mut class) } as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if class.eq_ignore_ascii_case("Static") || class.eq_ignore_ascii_case("Button") {
            translate_window(child);
        }
        BOOL(1)
    }
    unsafe {
        let _ = EnumChildWindows(Some(dialog), Some(each), LPARAM(0));
    }
}

/// A window's text in the language in use: from its English (remembered the first time), if
/// that has a translation, or the English itself.
fn translate_window(window: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{GetDlgCtrlID, GetWindowTextLengthW, GetWindowTextW, SetWindowTextW};
    use windows::core::HSTRING;
    let mut known = ENGLISH.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let known = known.get_or_insert_with(HashMap::new);
    let index = CURRENT.load(Ordering::Relaxed);
    let (now, id) = unsafe {
        let len = GetWindowTextLengthW(window);
        let mut buffer = vec![0u16; len.max(0) as usize + 1];
        let got = GetWindowTextW(window, &mut buffer) as usize;
        (String::from_utf16_lossy(&buffer[..got]), GetDlgCtrlID(window))
    };
    let key = window.0 as isize;
    let english = match known.get(&key) {
        Some(seen) if seen.id == id && (seen.last == now || seen.english.is_empty()) => seen.english.clone(),
        _ => {
            // First seen as it opens: what the .rc wrote (or nothing, for the code to fill).
            known.insert(key, Known { english: now.clone(), last: now.clone(), id });
            if index == 0 {
                return; // (English already)
            }
            now
        }
    };
    if english.is_empty() {
        return; // the code's own text: it puts it in the language itself
    }
    let text = match table(index).get(&english) {
        Some(Entry::One(text)) if index != 0 => text.clone(),
        _ => english.clone(),
    };
    known.insert(key, Known { english, last: text.clone(), id });
    unsafe {
        let _ = SetWindowTextW(window, &HSTRING::from(text.as_str()));
    }
}

/// Every English text the program shows, from its source and its dialogs (for the tests and for
/// translators: `--english-texts` is not offered; the tests print what's missing).
#[cfg(test)]
pub(crate) mod english {
    use std::collections::BTreeMap;

    /// A text and whether it takes a count (its English for more, if so).
    pub type Texts = BTreeMap<String, Option<String>>;

    /// Reads a Rust string literal starting at `s` (just past its opening quote): its value and
    /// the rest after the closing quote.
    fn literal(s: &str) -> Option<(String, &str)> {
        let mut out = String::new();
        let mut chars = s.char_indices();
        while let Some((i, c)) = chars.next() {
            match c {
                '"' => return Some((out, &s[i + 1..])),
                '\\' => match chars.next()?.1 {
                    // A line carried on: the line break and the next line's indent aren't in it.
                    '\r' | '\n' => {
                        while let Some(&(_, c)) = chars.clone().peekable().peek() {
                            if !c.is_whitespace() {
                                break;
                            }
                            chars.next();
                        }
                    }
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    '\'' => out.push('\''),
                    'u' => {
                        let rest = &s[i + 2..];
                        let close = rest.find('}')?;
                        let code = u32::from_str_radix(&rest[1..close], 16).ok()?;
                        out.push(char::from_u32(code)?);
                        for _ in 0..close + 1 {
                            chars.next();
                        }
                    }
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    }

    /// The texts in `tr!`, `wtr!`, `trn!` and `n!` calls in one source file.
    pub fn in_source(source: &str, texts: &mut Texts) {
        for (macro_name, counted) in [("tr!(", false), ("wtr!(", false), ("trn!(", true), ("n!(", false)] {
            let mut rest = source;
            while let Some(at) = rest.find(macro_name) {
                // (Not part of a longer name: `wtr!(` also ends in `tr!(`.)
                let before = rest[..at].chars().next_back();
                rest = &rest[at + macro_name.len()..];
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let Some(start) = rest.trim_start().strip_prefix('"') else { continue };
                let Some((one, after)) = literal(start) else { continue };
                let other = if counted {
                    after.trim_start().strip_prefix(',').and_then(|a| a.trim_start().strip_prefix('"')).and_then(literal).map(|(other, _)| other)
                } else {
                    None
                };
                texts.insert(one, other);
            }
        }
        // `trc!("place", "English")`: the text is `place|English`.
        let mut rest = source;
        while let Some(at) = rest.find("trc!(") {
            let before = rest[..at].chars().next_back();
            rest = &rest[at + "trc!(".len()..];
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let Some((place, after)) = rest.trim_start().strip_prefix('"').and_then(literal) else { continue };
            let Some((english, _)) = after.trim_start().strip_prefix(',').and_then(|a| a.trim_start().strip_prefix('"')).and_then(literal) else { continue };
            texts.insert(format!("{place}|{english}"), None);
        }
    }

    /// The texts of the dialogs: titles, labels, buttons, boxes.
    pub fn in_dialogs(rc: &str, texts: &mut Texts) {
        for line in rc.lines() {
            let line = line.trim();
            let kind = line.split_whitespace().next().unwrap_or_default();
            if !matches!(kind, "CAPTION" | "LTEXT" | "RTEXT" | "CTEXT" | "PUSHBUTTON" | "DEFPUSHBUTTON" | "AUTOCHECKBOX" | "AUTORADIOBUTTON" | "GROUPBOX" | "CONTROL") {
                continue;
            }
            let Some(start) = line.find('"') else { continue };
            // The .rc writes a quote inside a text as two.
            let mut text = String::new();
            let mut chars = line[start + 1..].chars().peekable();
            while let Some(c) = chars.next() {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        text.push('"');
                        continue;
                    }
                    break;
                }
                text.push(c);
            }
            if !text.trim().is_empty() {
                texts.insert(text, None);
            }
        }
    }

    /// Everything: every source file of the program and its dialogs.
    pub fn all() -> Texts {
        let mut texts = Texts::new();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for entry in std::fs::read_dir(&dir).expect("the source folder") {
            let path = entry.expect("a source file").path();
            if path.extension().is_some_and(|e| e == "rs") && path.file_name().is_some_and(|n| n != "text.rs") {
                in_source(&std::fs::read_to_string(&path).expect("readable source"), &mut texts);
            }
        }
        in_dialogs(include_str!("../assets/desktop-dock.rc"), &mut texts);
        texts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Languages whose translations are still being written: their files must already be sound
    /// (they read, their blanks match the English, their counts have every form), but they may
    /// lack texts. Empty before a release.
    const UNFINISHED: &[&str] = &[];

    fn blanks(text: &str) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            let Some(close) = rest[open..].find('}') else { break };
            found.insert(rest[open + 1..open + close].to_string());
            rest = &rest[open + close + 1..];
        }
        found
    }

    #[test]
    fn plural_rules_pick_the_right_form() {
        let forms = |code: &str, ns: &[u64]| {
            let language = LANGUAGES.iter().find(|l| l.code == code).unwrap();
            ns.iter().map(|&n| (language.plural)(n)).collect::<Vec<_>>()
        };
        assert_eq!(forms("en", &[0, 1, 2, 5]), [1, 0, 1, 1]);
        assert_eq!(forms("de", &[0, 1, 2]), [1, 0, 1]);
        assert_eq!(forms("fr", &[0, 1, 2]), [0, 0, 1], "French: 0 and 1 are both singular");
        assert_eq!(forms("pt-BR", &[0, 1, 2]), [0, 0, 1]);
        // Russian: 1, 21, 101 one; 2-4, 22-24 few; 0, 5-20, 25, 111-114 many.
        assert_eq!(forms("ru", &[1, 21, 101, 2, 3, 4, 22, 0, 5, 11, 12, 14, 20, 25, 111, 114]), [0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2]);
        assert_eq!(forms("ja", &[0, 1, 2]), [0, 0, 0]);
        for language in &LANGUAGES {
            for n in 0..200 {
                assert!((language.plural)(n) < language.forms, "{}: {n} takes a form it doesn't have", language.code);
            }
        }
    }

    #[test]
    fn windows_languages_find_the_dock_s() {
        let code = |tag: &str| matching(tag).map(|i| LANGUAGES[i].code);
        assert_eq!(code("de-DE"), Some("de"));
        assert_eq!(code("de-AT"), Some("de"));
        assert_eq!(code("fr-CA"), Some("fr"));
        assert_eq!(code("es-MX"), Some("es"));
        assert_eq!(code("pt-BR"), Some("pt-BR"));
        assert_eq!(code("pt-PT"), Some("pt-BR"), "Portuguese from Portugal: the Brazilian translation, nearer than English");
        assert_eq!(code("ru-RU"), Some("ru"));
        assert_eq!(code("zh-CN"), Some("zh-CN"));
        assert_eq!(code("zh-Hans-SG"), Some("zh-CN"));
        assert_eq!(code("zh-TW"), None, "Traditional characters: not the Simplified translation");
        assert_eq!(code("zh-Hant-HK"), None);
        assert_eq!(code("ja-JP"), Some("ja"));
        assert_eq!(code("en-GB"), Some("en"));
        assert_eq!(code("nl-NL"), None);
        // The setting: a code (any case), or anything else for Windows' choice.
        assert_eq!(LANGUAGES[chosen("DE")].code, "de");
        assert_eq!(chosen("auto"), windows_choice());
        assert_eq!(chosen("xx"), windows_choice());
    }

    #[test]
    fn blanks_are_filled_and_counts_pick_their_form() {
        use super::fill;
        assert_eq!(fill("Moved {name} to {group}", &[("name", &"Notepad"), ("group", &"Tools")]), "Moved Notepad to Tools");
        assert_eq!(fill("No blanks", &[("name", &"x")]), "No blanks");
        assert_eq!(fill("Moved {name} to {group}", &[("name", &"{group}"), ("group", &"Tools")]), "Moved {group} to Tools", "a value isn't filled in again");
        assert_eq!(fill("{unknown} stays", &[("name", &"x")]), "{unknown} stays");
        // English, in use while the tests run unless one sets another.
        assert_eq!(tn("{n} item", "{n} items", 1, &[]), "1 item");
        assert_eq!(tn("{n} item", "{n} items", 3, &[]), "3 items");
        assert_eq!(tn("{n} item in {group}", "{n} items in {group}", 2, &[("group", &"Tools")]), "2 items in Tools");
    }

    #[test]
    fn moments_and_numbers_are_written_the_region_s_way() {
        // (In whatever region this runs: the parts are there, the year in full.)
        let shown = when("2026-11-11 01:21");
        assert!(shown.contains("2026") && shown.contains("11") && shown != "2026-11-11 01:21", "{shown}");
        assert_eq!(when("not a moment"), "not a moment");
        assert_eq!(when("2026-11-11"), "2026-11-11", "a date alone stays as it is");
        let n = number(14.5, 1);
        assert!(n == "14.5" || n == "14,5" || n.len() == 4, "{n}");
        assert_eq!(number(3.0, 0), "3");
    }

    #[test]
    fn the_english_texts_are_found_in_source_and_dialogs() {
        let mut texts = english::Texts::new();
        english::in_source(r#"tr!("Lock icons"); wtr!("Properties..."); trn!("{n} item", "{n} items", k); tr!("Moved {name}", name = x); let ptr!("no") = 1; const A: &str = n!("Icon size"); an!("not this");"#, &mut texts);
        assert_eq!(texts.get("Lock icons"), Some(&None));
        assert!(texts.contains_key("Icon size") && !texts.contains_key("not this"));
        english::in_source(r#"trc!("uninstall", "Remove")"#, &mut texts);
        assert!(texts.contains_key("uninstall|Remove") && !texts.contains_key("uninstall"));
        assert_eq!(tc("uninstall|Remove", "Remove"), "Remove", "English: the text itself");
        assert_eq!(texts.get("Properties..."), Some(&None));
        assert_eq!(texts.get("{n} item"), Some(&Some("{n} items".to_string())));
        assert!(texts.contains_key("Moved {name}") && !texts.contains_key("no"));
        english::in_source(r#"tr!("Say \"hi\"\u{2026}")"#, &mut texts);
        assert!(texts.contains_key("Say \"hi\"…"));
        english::in_source("tr!(\"A long text, \\\n        carried on.\")", &mut texts);
        assert!(texts.contains_key("A long text, carried on."), "{:?}", texts.keys().collect::<Vec<_>>());
        english::in_dialogs("CAPTION \"Properties\"\n  LTEXT \"Name:\", -1, 0, 0, 10, 10\n  CONTROL \"\", 1, \"Static\", 0, 0, 0, 1, 1\n  PUSHBUTTON \"Say \"\"hi\"\"\", 2, 0, 0, 1, 1", &mut texts);
        assert!(texts.contains_key("Properties") && texts.contains_key("Name:") && texts.contains_key("Say \"hi\"") && !texts.contains_key(""));
    }

    #[test]
    fn every_language_file_is_sound_and_complete() {
        let english = english::all();
        assert!(english.len() > 50, "the program's texts were found ({})", english.len());
        let mut problems = Vec::new();
        for (index, language) in LANGUAGES.iter().enumerate().skip(1) {
            let entries = match parse(language.source) {
                Ok(entries) => entries,
                Err(e) => {
                    problems.push(format!("{}: doesn't read: {e}", language.code));
                    continue;
                }
            };
            for (key, entry) in &entries {
                let Some(other) = english.get(key) else {
                    problems.push(format!("{}: '{key}' isn't a text the program shows (any more)", language.code));
                    continue;
                };
                let wanted = blanks(key).union(&other.as_deref().map(blanks).unwrap_or_default()).cloned().collect::<BTreeSet<_>>();
                match (entry, other) {
                    (Entry::One(text), None) => {
                        if blanks(text) != wanted {
                            problems.push(format!("{}: '{key}': its blanks {:?} aren't the English {:?}", language.code, blanks(text), wanted));
                        }
                        if text.trim().is_empty() {
                            problems.push(format!("{}: '{key}' is empty", language.code));
                        }
                    }
                    (Entry::Forms(forms), Some(_)) => {
                        if forms.len() != language.forms {
                            problems.push(format!("{}: '{key}' has {} forms, the language {}", language.code, forms.len(), language.forms));
                        }
                        for form in forms {
                            // A form may leave out the count ("an item"), not other blanks.
                            let mut want = wanted.clone();
                            if !blanks(form).contains("n") {
                                want.remove("n");
                            }
                            if blanks(form) != want {
                                problems.push(format!("{}: '{key}' form '{form}': blanks {:?}, not {:?}", language.code, blanks(form), want));
                            }
                        }
                    }
                    (Entry::One(_), Some(_)) => problems.push(format!("{}: '{key}' takes a count: give its forms as a list", language.code)),
                    (Entry::Forms(_), None) => problems.push(format!("{}: '{key}' doesn't take a count: give one text", language.code)),
                }
            }
            if !UNFINISHED.contains(&language.code) {
                let missing: Vec<&String> = english.keys().filter(|key| !entries.contains_key(key.as_str())).collect();
                if !missing.is_empty() {
                    problems.push(format!("{}: {} texts missing, such as '{}'", language.code, missing.len(), missing[0]));
                }
            }
            let _ = index;
        }
        assert!(problems.is_empty(), "language files:\n  {}", problems.join("\n  "));
    }

    /// For translators: every English text with where it's shown, written to
    /// `target\english-texts.toml` (`cargo test --release -- --ignored write_the_english_texts`).
    #[test]
    #[ignore]
    fn write_the_english_texts() {
        use std::collections::BTreeMap;
        let mut places: BTreeMap<String, (Option<String>, Vec<String>)> = BTreeMap::new();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files: Vec<_> = std::fs::read_dir(dir.join("src")).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "rs") && p.file_name().is_some_and(|n| n != "text.rs")).collect();
        files.sort();
        for path in files {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            for (number, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
                let mut found = english::Texts::new();
                english::in_source(line, &mut found);
                for (text, other) in found {
                    let entry = places.entry(text).or_insert((other.clone(), Vec::new()));
                    entry.1.push(format!("{name}:{}", number + 1));
                }
            }
        }
        let mut dialog = String::new();
        for line in include_str!("../assets/desktop-dock.rc").lines() {
            if let Some((id, _)) = line.trim().split_once(" DIALOGEX") {
                dialog = id.trim().to_string();
            }
            let mut found = english::Texts::new();
            english::in_dialogs(line, &mut found);
            for (text, _) in found {
                let kind = line.split_whitespace().next().unwrap_or_default().to_string();
                places.entry(text).or_insert((None, Vec::new())).1.push(format!("dialog {dialog} {kind}"));
            }
        }
        // Texts found only across lines (a call spread over several) still count.
        for (text, other) in english::all() {
            places.entry(text).or_insert((other, vec!["(a call over several lines)".into()]));
        }
        let quote = |s: &str| toml::Value::String(s.to_string()).to_string();
        let mut out = String::from("# Every English text Desktop Dock shows, with where (file:line, or the dialog), for translators.\n# A text with a count lists its English for more after it.\n\n");
        for (text, (other, wheres)) in &places {
            out.push_str(&format!("# {}\n", wheres.join(", ")));
            match other {
                Some(other) => out.push_str(&format!("{} = [{}]\n", quote(text), quote(other))),
                None => out.push_str(&format!("{} = \"\"\n", quote(text))),
            }
        }
        let path = dir.join("target").join("english-texts.toml");
        std::fs::write(&path, out).unwrap();
        println!("{} texts in {}", places.len(), path.display());
    }

    /// A dialog control with a text, from the .rc: its dialog, kind, text and size in dialog units.
    struct RcText {
        dialog: String,
        kind: String,
        text: String,
        width: i32,
        height: i32,
        cut_on_purpose: bool,
    }

    fn rc_texts() -> Vec<RcText> {
        let mut found = Vec::new();
        let mut dialog = String::new();
        for line in include_str!("../assets/desktop-dock.rc").lines() {
            let line = line.trim();
            if let Some((id, _)) = line.split_once(" DIALOGEX") {
                dialog = id.trim().to_string();
                continue;
            }
            let kind = line.split_whitespace().next().unwrap_or_default();
            if !matches!(kind, "LTEXT" | "RTEXT" | "CTEXT" | "PUSHBUTTON" | "DEFPUSHBUTTON" | "AUTOCHECKBOX" | "AUTORADIOBUTTON" | "GROUPBOX") {
                continue;
            }
            let (Some(open), Some(close)) = (line.find('"'), line.rfind('"')) else { continue };
            let text = line[open + 1..close].replace("\"\"", "\"");
            if text.trim().is_empty() {
                continue;
            }
            // `, id, x, y, width, height[, style]`
            let fields: Vec<&str> = line[close + 1..].split(',').map(str::trim).collect();
            let (Some(width), Some(height)) = (fields.get(4).and_then(|f| f.parse().ok()), fields.get(5).and_then(|f| f.parse().ok())) else { continue };
            let cut_on_purpose = line.contains("ELLIPSIS");
            found.push(RcText { dialog: dialog.clone(), kind: kind.to_string(), text, width, height, cut_on_purpose });
        }
        found
    }

    #[test]
    fn every_dialog_text_fits_its_control_in_every_language() {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleDC, CreateFontW, DT_CALCRECT, DT_SINGLELINE, DT_WORDBREAK, DeleteDC, DeleteObject, DrawTextW, GetTextMetricsW, HGDIOBJ, SelectObject, TEXTMETRICW,
        };
        use windows::core::w;
        let texts = rc_texts();
        assert!(texts.len() > 50, "the dialogs' texts were found ({})", texts.len());
        let mut problems = Vec::new();
        unsafe {
            // The dialogs' font (FONT 9, "Segoe UI") at 100%: 9 points is 12 pixels.
            let dc = CreateCompatibleDC(None);
            let font = CreateFontW(-12, 0, 0, 0, 400, 0, 0, 0, Default::default(), Default::default(), Default::default(), Default::default(), 0, w!("Segoe UI"));
            let old = SelectObject(dc, HGDIOBJ(font.0));
            let size = |text: &str, wrap_width: Option<i32>| {
                let mut wide: Vec<u16> = text.encode_utf16().collect();
                let mut r = RECT { left: 0, top: 0, right: wrap_width.unwrap_or(10_000), bottom: 0 };
                let flags = DT_CALCRECT | if wrap_width.is_some() { DT_WORDBREAK } else { DT_SINGLELINE };
                DrawTextW(dc, &mut wide, &mut r, flags);
                (r.right - r.left, r.bottom - r.top)
            };
            // Dialog units: a quarter of the font's average letter across, an eighth of its height down.
            let (letters, _) = size("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz", None);
            let base_x = (letters / 26 + 1) / 2;
            let mut metrics = TEXTMETRICW::default();
            let _ = GetTextMetricsW(dc, &mut metrics);
            let base_y = metrics.tmHeight;
            for (index, language) in LANGUAGES.iter().enumerate() {
                for rc in &texts {
                    let shown = match table(index).get(&rc.text) {
                        Some(Entry::One(text)) if index != 0 => text.as_str(),
                        _ => rc.text.as_str(),
                    };
                    let (width, height) = (rc.width * base_x / 4, rc.height * base_y / 8);
                    // Room the control itself takes: a button's edges, a tick box and its gap.
                    let taken = match rc.kind.as_str() {
                        "PUSHBUTTON" | "DEFPUSHBUTTON" => 10,
                        "AUTOCHECKBOX" | "AUTORADIOBUTTON" => 18,
                        "GROUPBOX" => 16,
                        _ => 0,
                    };
                    let lines = (height / metrics.tmHeight.max(1)).max(1);
                    let fits = if rc.kind == "LTEXT" && lines > 1 {
                        size(shown, Some(width)).1 <= height
                    } else {
                        rc.cut_on_purpose || size(shown, None).0 + taken <= width
                    };
                    if !fits {
                        problems.push(format!("{} dialog {} {} '{}' ({} DLU wide): '{}'", language.code, rc.dialog, rc.kind, rc.text, rc.width, shown));
                    }
                }
            }
            SelectObject(dc, old);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = DeleteDC(dc);
        }
        assert!(problems.is_empty(), "texts too long for their controls:\n  {}", problems.join("\n  "));
    }

    #[test]
    fn english_texts_have_no_stray_braces() {
        // A blank is `{name}`; anything else in braces would never be filled in.
        for (one, other) in english::all() {
            for text in std::iter::once(&one).chain(other.iter()) {
                let opens = text.matches('{').count();
                assert_eq!(opens, text.matches('}').count(), "'{text}'");
                assert!(blanks(text).iter().all(|b| !b.is_empty() && b.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')), "'{text}'");
            }
        }
    }
}
