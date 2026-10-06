//! Deliberate faults for mutation testing: each one breaks a single behaviour on purpose, so the
//! end-to-end tests can show they notice when it stops working. They exist only in builds with
//! the `test-break` feature, never in a release: there `broken` is always false and every fault
//! compiles away (tools\package.ps1 checks the release exe has no trace of them).
//!
//! A test names the faults it wants, one per line, in `break.txt` at the root of the development
//! layout (or `C:\dd\break.txt` on the test machine: a file, because a dock started through
//! Explorer never sees a test's environment), then restarts the dock. Read once, as it starts.

/// Every fault, and what it does.
#[cfg(test)]
pub const MUTANTS: &[(&str, &str)] = &[
    ("launch.show-desktop", "Show desktop launched as an ordinary item (Windows: no app for it)"),
    ("target.old-resolve", "links and bare names taken as files in the dock folder"),
    ("launch.ignore-run", "run minimised or maximised ignored"),
    ("launch.ignore-args", "an item's arguments left off"),
    ("launch.ignore-start-in", "an item's start-in folder left off"),
    ("launch.ignore-admin", "run as administrator ignored"),
    ("heal.no-moved", "a program an update moved isn't looked for"),
    ("group.wrong-child", "a click in a group's pop-up opens the item after the one clicked"),
    ("menu.remove-neighbour", "Remove takes the item after the one right-clicked"),
    ("menu.lock-keeps-remove", "a locked dock still offers Remove"),
    ("undo.put-back-at-end", "Put back puts it at the end, not where it was"),
    ("undo.no-op", "Undo changes nothing (but says it did)"),
    ("undo.no-hit-area", "a click on the strip's Undo does nothing"),
    ("drop.open-with-no-file", "a file dropped onto a program opens the program without it"),
    ("drop.replace-keeps-target", "Replace keeps the old program"),
    ("drop.url-as-file", "a dropped link becomes the .url file, not its address"),
    ("drop.no-kept-copy", "a shortcut to a place isn't kept as a copy"),
    ("settings.wrong-key", "the Spacing slider saves to label_size"),
    ("look.dark-ignored", "the dark look drawn light"),
    ("labels.always", "names shown with show_labels off"),
    ("glow.accent-for-icon", "the icon's-colour glow drawn in the accent colour"),
    ("missing.not-dimmed", "a missing program's icon not dimmed"),
    ("bin.icon-stale", "the Recycle Bin icon never changes between full and empty"),
    ("bin.empty-does-nothing", "Empty Recycle Bin empties nothing"),
    ("location.no-select", "Open file location opens the folder without selecting the file"),
    ("startup.not-written", "Start with Windows on, but nothing written for Windows to start"),
    ("hotkey.not-registered", "the keyboard shortcut never registered"),
    ("places.network-typo", "the Network Windows item added with a mistyped CLSID"),
    ("setup.ignores-change", "setup installs to the usual folder whatever Change... chose"),
];

/// Whether the break `id` is on (`MUTANTS` says what each does).
#[cfg(feature = "test-break")]
pub fn broken(id: &str) -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<Vec<String>> = OnceLock::new();
    ON.get_or_init(|| {
        // <root>\target\release\desktop-dock.exe; a copy run from elsewhere (setup's download)
        // reads the test machine's fixed folder.
        let beside = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.parent()?.parent()?.join("break.txt")));
        let text = [beside, Some(r"C:\dd\break.txt".into())]
            .into_iter()
            .flatten()
            .find_map(|file: std::path::PathBuf| std::fs::read_to_string(file).ok())
            .unwrap_or_default();
        let on: Vec<String> = text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(String::from).collect();
        if !on.is_empty() {
            // INFO, not WARN: a step must fail on what the break does, not on this line.
            crate::log_info!("broken on purpose, for a test: {}", on.join(", "));
        }
        on
    })
    .iter()
    .any(|on| on == id)
}

#[cfg(not(feature = "test-break"))]
#[inline(always)]
pub fn broken(_id: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::MUTANTS;

    /// Each fault in the list is switched somewhere in the code, and each switch is in the list.
    #[test]
    fn every_break_is_listed_and_used() {
        let sources = [
            include_str!("dock.rs"),
            include_str!("launch.rs"),
            include_str!("home.rs"),
            include_str!("heal.rs"),
            include_str!("drop.rs"),
            include_str!("settings.rs"),
            include_str!("system.rs"),
            include_str!("places.rs"),
            include_str!("setup.rs"),
        ]
        .join("\n");
        let used: Vec<&str> = sources.split("broken(\"").skip(1).filter_map(|rest| rest.split('"').next()).collect();
        let listed: Vec<&str> = MUTANTS.iter().map(|(id, _)| *id).collect();
        for id in &listed {
            assert!(used.contains(id), "{id} is listed but never switched");
        }
        for id in &used {
            assert!(listed.contains(id), "{id} is switched but not listed in MUTANTS");
        }
    }

    /// The maintainer's test notes say which end-to-end checks must catch each fault.
    #[cfg(dev_lab)]
    #[test]
    fn every_break_has_its_rows() {
        let table = include_str!("../testing/mutants.md");
        for (id, _) in MUTANTS {
            assert!(table.contains(&format!("| {id} |")), "testing/mutants.md has no row for {id}");
        }
    }
}
