//! What kind of thing an item's target is, decided in one place: a shell location, a web
//! address or other `scheme:` link, a path (absolute, or relative to the dock folder), or a bare
//! name. Opening, the self-healing pass, imports and drops all ask the same question here, so
//! one answer holds everywhere: `ms-settings:display`, `mailto:…` and a bare `notepad.exe` are
//! never taken for files inside the dock folder (they wouldn't open, and would show as missing).

/// The kinds of target. A target is classified after its `%VARIABLES%` are expanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Nothing there.
    Empty,
    /// A shell location: `::{CLSID}` or `shell:…` (This PC, a Start menu app…).
    Shell,
    /// A link with a scheme of two or more characters: `https://…`, `ms-settings:display`,
    /// `mailto:…`, `steam://…`. Windows opens it with whatever handles the scheme.
    Uri,
    /// A full path: `C:\…`, `C:x` (relative to that drive), `\\server\share\…`, `\…`.
    Absolute,
    /// A path inside the dock folder: `shortcuts\Word.lnk`, `.\tool.exe`, `..\x.exe`.
    Relative,
    /// A name and nothing else (`notepad.exe`, `calc`): the file of that name in the dock folder
    /// if there is one, otherwise left for Windows to find (its PATH and App Paths).
    Bare,
}

impl Class {
    /// Every kind, so the tests (and the feature inventory) can check each has a case.
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: [Class; 6] = [Class::Empty, Class::Shell, Class::Uri, Class::Absolute, Class::Relative, Class::Bare];
}

/// What kind of target `target` is (already expanded).
pub fn classify(target: &str) -> Class {
    let t = target.trim().trim_matches('"');
    if t.is_empty() {
        return Class::Empty;
    }
    if t.starts_with("::") || t.get(..6).is_some_and(|head| head.eq_ignore_ascii_case("shell:")) {
        return Class::Shell;
    }
    if is_uri(t) {
        return Class::Uri;
    }
    let bytes = t.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if drive || t.starts_with(['\\', '/']) {
        return Class::Absolute;
    }
    if t.contains(['\\', '/']) || t.starts_with('.') {
        return Class::Relative;
    }
    Class::Bare
}

/// `scheme:…`, the scheme two or more letters, digits, `+`, `.` or `-`, starting with a letter
/// (a single letter is a drive: `C:`).
fn is_uri(t: &str) -> bool {
    let Some(colon) = t.find(':') else { return false };
    let scheme = &t[..colon];
    scheme.len() >= 2
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_target_is_told_apart() {
        let cases: &[(&str, Class)] = &[
            ("", Class::Empty),
            ("   ", Class::Empty),
            ("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", Class::Shell),
            ("shell:Personal", Class::Shell),
            ("SHELL:AppsFolder\\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", Class::Shell),
            ("shell:::{3080F90D-D7AD-11D9-BD98-0000947B0257}", Class::Shell),
            ("https://example.com/a?b=c", Class::Uri),
            ("ms-settings:display", Class::Uri),
            ("mailto:alex@example.com", Class::Uri),
            ("steam://rungameid/1", Class::Uri),
            ("ddtest:hello", Class::Uri),
            ("file:///C:/x.txt", Class::Uri),
            (r"C:\Windows\notepad.exe", Class::Absolute),
            ("C:/Windows/notepad.exe", Class::Absolute),
            ("C:x.exe", Class::Absolute),
            (r"\\server\share\tool.exe", Class::Absolute),
            (r"\Tools\x.exe", Class::Absolute),
            ("\"C:\\Program Files\\App\\app.exe\"", Class::Absolute),
            (r"shortcuts\Word.lnk", Class::Relative),
            (r".\tool.exe", Class::Relative),
            (r"..\x.exe", Class::Relative),
            ("sub/x.exe", Class::Relative),
            ("notepad.exe", Class::Bare),
            ("calc", Class::Bare),
            ("Gone App.exe", Class::Bare),
            ("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", Class::Bare),
        ];
        for (target, class) in cases {
            assert_eq!(classify(target), *class, "{target:?}");
        }
        // Every kind is covered above.
        for class in Class::ALL {
            assert!(cases.iter().any(|(_, c)| *c == class), "{class:?} has no case");
        }
    }
}
