//! Self-healing: when an update moves a program (e.g. "Adobe Photoshop 2026" replaced by
//! "Adobe Photoshop 2027"), find it again. Only unambiguous fixes are made; everything else is
//! left alone and reported (it fixes what it safely can, keeps a backup and tells you).

use crate::config::{self, ItemConfig, Kind};
use crate::edit::Spot;
use crate::home::Home;
use crate::store::Store;
use crate::target::{self, Class};
use crate::{log_info, log_warn};
use std::path::{Path, PathBuf};

/// What the healer needs from the disk (a fake in tests).
pub trait Fs {
    fn exists(&self, path: &Path) -> bool;
    fn subdirs(&self, dir: &Path) -> Vec<String>;
}

pub struct RealFs;

impl Fs for RealFs {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn subdirs(&self, dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }
}

/// A folder name split into its text and its numbers: "Adobe Photoshop 2026" →
/// (["adobe photoshop ", ""], [2026]). Two names are "the same program" when the text matches.
fn shape(name: &str) -> (Vec<String>, Vec<u64>) {
    let mut texts = vec![String::new()];
    let mut numbers = Vec::new();
    let mut digits = String::new();
    for c in name.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            if !digits.is_empty() {
                numbers.push(digits.parse().unwrap_or(0));
                digits.clear();
                texts.push(String::new());
            }
            texts.last_mut().unwrap().extend(c.to_lowercase());
        }
    }
    if !digits.is_empty() {
        numbers.push(digits.parse().unwrap_or(0));
        texts.push(String::new());
    }
    (texts, numbers)
}

/// For a missing target, finds where the program went: a sibling folder whose name differs
/// only in its numbers and that contains the same file. Picks the highest version. Returns None
/// if nothing matches, or if more than one folder in the path could be the one that changed.
pub fn find_moved(target: &str, fs: &impl Fs) -> Option<(String, String)> {
    let path = Path::new(target);
    let parts: Vec<&std::ffi::OsStr> = path.iter().collect();
    if parts.len() < 3 {
        return None;
    }
    let mut fixes = Vec::new();
    // Every folder in the path except the drive and the file itself.
    for k in 1..parts.len() - 1 {
        let name = parts[k].to_string_lossy();
        if !name.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let parent: PathBuf = parts[..k].iter().collect();
        let rest: PathBuf = parts[k + 1..].iter().collect();
        let (texts, _) = shape(&name);
        let best = fs
            .subdirs(&parent)
            .into_iter()
            .filter(|candidate| *candidate != name && shape(candidate).0 == texts)
            .filter(|candidate| fs.exists(&parent.join(candidate).join(&rest)))
            .max_by_key(|candidate| shape(candidate).1);
        if let Some(best) = best {
            let old_folder = parent.join(name.as_ref()).to_string_lossy().into_owned();
            let new_folder = parent.join(&best).to_string_lossy().into_owned();
            fixes.push((old_folder, new_folder));
        }
    }
    (fixes.len() == 1).then(|| fixes.remove(0))
}


/// One program found again.
#[derive(Debug)]
pub struct Fix {
    /// The item's name, e.g. "Photoshop".
    pub name: String,
    /// "Photoshop: …\Adobe Photoshop 2026 → …\Adobe Photoshop 2027"
    pub line: String,
}

/// The result of a healing pass, by item position.
#[derive(Debug, Default)]
pub struct Report {
    /// Only items whose entry in dock.toml really changed.
    pub fixed: Vec<Fix>,
    /// Items whose target is missing and couldn't be fixed (shown dimmed), on the dock or in a
    /// group.
    pub missing: Vec<Spot>,
}

/// Checks every item, fixes moved programs in dock.toml (safely, with a backup), and reports
/// what's still missing. Runs on a worker thread: it touches the disk, which can be slow.
///
/// Stored paths are compared as they resolve (`%VARS%`, relative to the dock folder, either
/// slash), and a fix keeps the form it was written in where it can. A fix counts only if the
/// file really changed, so the same repair is never made or reported twice.
pub fn heal(store: &Store, home: &Home, fs: &impl Fs) -> Report {
    let mut report = Report::default();
    let Ok(text) = config::read_text(store.config_path()) else { return report };
    let Ok(parsed) = config::parse(&text) else { return report };
    let mut planned: Vec<Planned> = Vec::new();
    // Every program, on the dock and inside groups.
    let everything = parsed.config.items.iter().enumerate().flat_map(|(index, item)| {
        std::iter::once((Spot::dock(index), item))
            .chain(item.items.iter().enumerate().map(move |(child, inner)| (Spot::in_group(index, child), inner)))
    });
    for (spot, item) in everything {
        // Only files and folders: not shell locations, links, or names Windows looks up.
        let Some(target) = (item.kind == Kind::App).then(|| home.resolve_file(&item.target)).flatten() else {
            continue;
        };
        let target = normal(&target);
        if fs.exists(Path::new(&target)) {
            continue;
        }
        match find_moved(&target, fs) {
            Some((old_folder, new_folder)) => planned.push(Planned { spot, name: item.name.clone(), target: item.target.clone(), old_folder, new_folder }),
            None => {
                log_warn!("{}: {target} is missing and couldn't be found elsewhere", item.name);
                report.missing.push(spot);
            }
        }
    }
    if planned.is_empty() {
        return report;
    }
    let result = store.change(|doc| apply(doc, &planned, home)).map(|(fixed, _)| fixed);
    match result {
        Ok(fixed) => {
            for fix in &fixed {
                log_info!("repaired {}", fix.line);
            }
            report.fixed = fixed;
        }
        Err(e) => log_warn!("couldn't save repairs: {e}"),
    }
    report
}

/// Applies the planned fixes to dock.toml and returns those that changed something.
fn apply(doc: &mut toml_edit::DocumentMut, planned: &[Planned], home: &Home) -> Result<Vec<Fix>, String> {
    let Some(items) = doc.get_mut("item").and_then(|i| i.as_array_of_tables_mut()) else {
        return Err("no items".into());
    };
    let mut fixed = Vec::new();
    for plan in planned {
        let Some(top) = items.get_mut(plan.spot.index) else { continue };
        let table = match plan.spot.child {
            None => top,
            Some(child) => match top.get_mut("items").and_then(|i| i.as_array_of_tables_mut()).and_then(|list| list.get_mut(child)) {
                Some(table) => table,
                None => continue,
            },
        };
        // Something changed the dock since the check (Dock settings moved or edited this
        // item): leave it for the next pass.
        if table.get("target").and_then(|v| v.as_str()) != Some(plan.target.as_str()) {
            continue;
        }
        let mut changed = false;
        for key in ["target", "start_in", "icon"] {
            let Some(value) = table.get(key).and_then(|v| v.as_str()) else { continue };
            let Some(updated) = moved_value(value, &plan.old_folder, &plan.new_folder, home) else { continue };
            if updated == value {
                continue;
            }
            let decor = table.get(key).and_then(|v| v.as_value()).map(|v| v.decor().clone());
            let mut new_value = literal(&updated);
            if let (Some(decor), Some(v)) = (decor, new_value.as_value_mut()) {
                *v.decor_mut() = decor;
            }
            table.insert(key, new_value);
            changed = true;
        }
        if changed {
            fixed.push(Fix { name: plan.name.clone(), line: format!("{}: {} → {}", plan.name, plan.old_folder, plan.new_folder) });
        }
    }
    Ok(fixed)
}

/// A repair found by the check, applied in the edit.
struct Planned {
    spot: Spot,
    name: String,
    /// The target as written in dock.toml when it was checked.
    target: String,
    old_folder: String,
    new_folder: String,
}

/// A path with one kind of slash and no `.` parts, for comparing.
fn normal(path: &str) -> String {
    Path::new(path).components().collect::<PathBuf>().to_string_lossy().into_owned()
}

/// `value` (as stored) with `old_folder` replaced by `new_folder`, if it lies inside it. A
/// leading `%VAR%` is kept, and a path relative to the dock folder stays relative, when they
/// still fit.
fn moved_value(value: &str, old_folder: &str, new_folder: &str, home: &Home) -> Option<String> {
    let resolved = normal(&home.resolve(value));
    let rest = inside(&resolved, old_folder)?;
    let updated = format!("{new_folder}{rest}");
    let raw = value.trim();
    if let Some((name, _)) = raw.strip_prefix('%').and_then(|after| after.split_once('%')) {
        if let Some(base) = std::env::var(name).ok().map(|v| normal(&v)) {
            if let Some(tail) = inside(&updated, &base) {
                return Some(format!("%{name}%{tail}"));
            }
        }
        return Some(updated);
    }
    if !Path::new(raw).is_absolute() {
        if let Some(tail) = inside(&updated, &normal(&home.dir.to_string_lossy())) {
            return Some(tail.trim_start_matches('\\').to_string());
        }
    }
    Some(updated)
}

/// The part of `path` after `folder` (empty, or starting with `\`), if `path` is that folder or
/// lies inside it. Ignores case; "Tool 2026" isn't inside "Tool 20".
fn inside<'a>(path: &'a str, folder: &str) -> Option<&'a str> {
    let rest = strip_prefix_ignore_case(path, folder.trim_end_matches('\\'))?;
    (rest.is_empty() || rest.starts_with('\\')).then_some(rest)
}

fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &value[prefix.len()..])
}

/// A TOML value written as a single-quoted string when possible (matching the file's style).
fn literal(s: &str) -> toml_edit::Item {
    if !s.contains('\'') {
        if let Ok(value) = format!("'{s}'").parse::<toml_edit::Value>() {
            return toml_edit::Item::Value(value);
        }
    }
    toml_edit::value(s)
}

/// Used when a launch finds its target missing: the moved program, if it can be found.
pub fn moved_target(item: &ItemConfig, target: &str) -> Option<String> {
    if item.kind != Kind::App || target::classify(target) != Class::Absolute || Path::new(target).exists() || crate::breaks::broken("heal.no-moved") {
        return None;
    }
    let target = normal(target);
    let (old_folder, new_folder) = find_moved(&target, &RealFs)?;
    inside(&target, &old_folder).map(|rest| format!("{new_folder}{rest}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A pretend disk: a set of existing paths (folders are implied by their files).
    struct FakeFs(HashSet<String>);

    impl FakeFs {
        fn new(files: &[&str]) -> Self {
            Self(files.iter().map(|f| f.to_ascii_lowercase()).collect())
        }
    }

    impl Fs for FakeFs {
        fn exists(&self, path: &Path) -> bool {
            let p = path.to_string_lossy().to_ascii_lowercase();
            self.0.iter().any(|f| *f == p || f.starts_with(&format!("{p}\\")))
        }

        fn subdirs(&self, dir: &Path) -> Vec<String> {
            let d = format!("{}\\", dir.to_string_lossy().to_ascii_lowercase().trim_end_matches('\\'));
            let mut names: Vec<String> = self
                .0
                .iter()
                .filter_map(|f| f.strip_prefix(&d))
                .filter_map(|rest| rest.split_once('\\').map(|(first, _)| first.to_string()))
                .collect();
            names.sort();
            names.dedup();
            names
        }
    }

    const PS_2026: &str = r"C:\Program Files\Adobe\Adobe Photoshop 2026\Photoshop.exe";

    #[test]
    fn adobe_2026_becomes_2027() {
        let fs = FakeFs::new(&[r"C:\Program Files\Adobe\Adobe Photoshop 2027\Photoshop.exe"]);
        let (old, new) = find_moved(PS_2026, &fs).unwrap();
        assert!(old.to_ascii_lowercase().ends_with("adobe photoshop 2026"));
        assert!(new.to_ascii_lowercase().ends_with("adobe photoshop 2027"));
    }

    #[test]
    fn the_highest_version_wins() {
        let fs = FakeFs::new(&[
            r"C:\Program Files\Adobe\Adobe Photoshop 2027\Photoshop.exe",
            r"C:\Program Files\Adobe\Adobe Photoshop 2028\Photoshop.exe",
        ]);
        assert!(find_moved(PS_2026, &fs).unwrap().1.ends_with("2028"));
    }

    #[test]
    fn a_folder_without_the_program_doesnt_count() {
        let fs = FakeFs::new(&[r"C:\Program Files\Adobe\Adobe Photoshop 2027\readme.txt"]);
        assert!(find_moved(PS_2026, &fs).is_none());
    }

    #[test]
    fn a_differently_named_folder_doesnt_count() {
        let fs = FakeFs::new(&[r"C:\Program Files\Adobe\Adobe Photoshop 2027 (Beta)\Photoshop.exe"]);
        assert!(find_moved(PS_2026, &fs).is_none());
    }

    #[test]
    fn deeper_paths_and_other_apps_work() {
        let fs = FakeFs::new(&[r"C:\Program Files\Image-Line\FL Studio 2026\FL64.exe"]);
        let fixed = find_moved(r"C:\Program Files\Image-Line\FL Studio 2025\FL64.exe", &fs).unwrap();
        assert!(fixed.1.ends_with("FL Studio 2026") || fixed.1.ends_with("fl studio 2026"));
        let fs = FakeFs::new(&[r"C:\Program Files\Adobe\Adobe Illustrator 2027\Support Files\Contents\Windows\Illustrator.exe"]);
        assert!(find_moved(r"C:\Program Files\Adobe\Adobe Illustrator 2026\Support Files\Contents\Windows\Illustrator.exe", &fs).is_some());
    }

    #[test]
    fn two_possible_changes_is_ambiguous_and_left_alone() {
        let fs = FakeFs::new(&[r"C:\Apps 2\Tool 3\x.exe", r"C:\Apps 1\Tool 4\x.exe"]);
        assert!(find_moved(r"C:\Apps 1\Tool 3\x.exe", &fs).is_none());
    }

    #[test]
    fn shapes_compare_text_and_numbers() {
        assert_eq!(shape("Adobe Photoshop 2026").0, shape("adobe photoshop 2027").0);
        assert_ne!(shape("Adobe Photoshop 2026").0, shape("Adobe Photoshop 2026 (Beta)").0);
        assert_eq!(shape("app-1.0.9158").1, vec![1, 0, 9158]);
        assert!(shape("app-1.0.9200").1 > shape("app-1.0.9158").1);
    }

    /// End to end on a real (temporary) disk: the file is fixed, comments kept, backup made.
    #[test]
    fn heal_fixes_dock_toml_with_a_backup() {
        let dir = std::env::temp_dir().join(format!("dock-heal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let apps = dir.join("Apps");
        std::fs::create_dir_all(apps.join("Tool 2027")).unwrap();
        std::fs::write(apps.join("Tool 2027").join("tool.exe"), "").unwrap();
        let old = apps.join("Tool 2026").join("tool.exe").to_string_lossy().into_owned();
        let gone = apps.join("Gone").join("gone.exe").to_string_lossy().into_owned();
        let home = Home::new(dir.clone());
        let text = format!(
            "version = 1\n\n[[item]]\nname = \"Tool\"\ntarget = '{old}'  # my tool\n\n[[item]]\nname = \"Gone\"\ntarget = '{gone}'\n"
        );
        std::fs::write(home.config(), &text).unwrap();
        let store = Store::new(&home);

        let report = heal(&store, &home, &RealFs);
        assert_eq!(report.fixed.len(), 1, "{report:?}");
        assert_eq!(report.missing, vec![Spot::dock(1)], "the other item stays broken and is reported");
        let saved = std::fs::read_to_string(home.config()).unwrap();
        assert!(saved.contains("Tool 2027"), "{saved}");
        assert!(saved.contains("# my tool"), "comment kept: {saved}");
        assert!(saved.contains(&format!("target = '{}'", apps.join("Tool 2027").join("tool.exe").display())), "{saved}");
        assert!(home.backups().join("recent").exists(), "backup made before saving");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A temporary dock folder with `Apps\Tool 2027\tool.exe` in it, and a dock.toml.
    fn dock_with_moved_tool(test: &str, items: &str) -> (PathBuf, Home) {
        let dir = std::env::temp_dir().join(format!("dock-heal-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Apps").join("Tool 2027")).unwrap();
        std::fs::write(dir.join("Apps").join("Tool 2027").join("tool.exe"), "").unwrap();
        let home = Home::new(dir.clone());
        std::fs::write(home.config(), format!("version = 1\n\n{items}")).unwrap();
        (dir, home)
    }

    /// The second pass finds nothing to do and leaves the file (and its date) alone.
    fn second_pass_is_quiet(home: &Home) {
        let written = std::fs::metadata(home.config()).unwrap().modified().unwrap();
        let text = std::fs::read_to_string(home.config()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let again = heal(&Store::new(home), home, &RealFs);
        assert!(again.fixed.is_empty() && again.missing.is_empty(), "{again:?}");
        assert_eq!(std::fs::read_to_string(home.config()).unwrap(), text);
        assert_eq!(std::fs::metadata(home.config()).unwrap().modified().unwrap(), written, "not saved again");
    }

    #[test]
    fn links_and_names_windows_looks_up_are_never_missing() {
        // Neither a file in the dock folder nor anywhere to check: Windows opens them.
        let items = "[[item]]\nname = 'Display'\ntarget = 'ms-settings:display'\n\n\
                     [[item]]\nname = 'Mail'\ntarget = 'mailto:alex@example.com'\n\n\
                     [[item]]\nname = 'Notepad'\ntarget = 'notepad.exe'\n\n\
                     [[item]]\nname = 'Gone'\ntarget = 'C:\\Nowhere 2026\\gone.exe'\n";
        let (dir, home) = dock_with_moved_tool("links", items);
        let report = heal(&Store::new(&home), &home, &RealFs);
        assert_eq!(report.missing, vec![Spot::dock(3)], "only the real missing file: {report:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_relative_target_is_fixed_once_and_stays_relative() {
        let (dir, home) = dock_with_moved_tool("relative", "[[item]]\nname = \"Tool\"\ntarget = 'Apps/Tool 2026/tool.exe'\nicon = '.\\Apps\\Tool 2026\\tool.exe'\n");
        let report = heal(&Store::new(&home), &home, &RealFs);
        assert_eq!(report.fixed.len(), 1, "{report:?}");
        assert_eq!(report.fixed[0].name, "Tool");
        let saved = std::fs::read_to_string(home.config()).unwrap();
        assert!(saved.contains(r"target = 'Apps\Tool 2027\tool.exe'"), "{saved}");
        assert!(saved.contains(r"icon = 'Apps\Tool 2027\tool.exe'"), "{saved}");
        second_pass_is_quiet(&home);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_target_with_a_variable_is_fixed_once_and_keeps_it() {
        let Ok(temp) = std::env::var("TEMP") else { return };
        let dir = PathBuf::from(&temp).join(format!("dock-heal-vars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Tool 2027")).unwrap();
        std::fs::write(dir.join("Tool 2027").join("tool.exe"), "").unwrap();
        let home = Home::new(dir.clone());
        let folder = dir.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::write(home.config(), format!("version = 1\n\n[[item]]\nname = \"Tool\"\ntarget = '%TEMP%\\{folder}\\Tool 2026\\tool.exe'\nstart_in = '%temp%/{folder}/Tool 2026'\n")).unwrap();
        let report = heal(&Store::new(&home), &home, &RealFs);
        assert_eq!(report.fixed.len(), 1, "{report:?}");
        let saved = std::fs::read_to_string(home.config()).unwrap();
        assert!(saved.contains(&format!(r"target = '%TEMP%\{folder}\Tool 2027\tool.exe'")), "{saved}");
        assert!(saved.contains(&format!(r"start_in = '%temp%\{folder}\Tool 2027'")), "{saved}");
        second_pass_is_quiet(&home);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fixed_dock_isnt_saved_or_reported_again() {
        let (dir, home) = dock_with_moved_tool("twice", "");
        let old = dir.join("Apps").join("Tool 2026").join("tool.exe");
        std::fs::write(home.config(), format!("version = 1\n\n[[item]]\nname = \"Tool\"\ntarget = '{}'\n", old.display())).unwrap();
        assert_eq!(heal(&Store::new(&home), &home, &RealFs).fixed.len(), 1);
        second_pass_is_quiet(&home);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_item_changed_since_the_check_is_left_alone() {
        let (dir, home) = dock_with_moved_tool("changed", "[[item]]\nname = \"Tool\"\ntarget = 'Apps\\Tool 2026\\tool.exe'\n");
        // The check plans a fix for item 0; before it's saved, item 0 becomes something else.
        let plan = Planned {
            spot: Spot::dock(0),
            name: "Tool".into(),
            target: "Apps\\Tool 2026\\other.exe".into(),
            old_folder: dir.join("Apps").join("Tool 2026").to_string_lossy().into_owned(),
            new_folder: dir.join("Apps").join("Tool 2027").to_string_lossy().into_owned(),
        };
        let text = std::fs::read_to_string(home.config()).unwrap();
        let mut doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert!(apply(&mut doc, &[plan], &home).unwrap().is_empty());
        assert_eq!(doc.to_string(), text, "nothing written over the newer item");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inside_respects_folder_boundaries() {
        assert_eq!(inside(r"C:\Apps\Tool 2026\x.exe", r"C:\Apps\Tool 2026"), Some(r"\x.exe"));
        assert_eq!(inside(r"C:\Apps\Tool 2026", r"c:\apps\tool 2026\"), Some(""));
        assert_eq!(inside(r"C:\Apps\Tool 20261\x.exe", r"C:\Apps\Tool 2026"), None);
        assert_eq!(normal("C:/Apps/./Tool 2026/x.exe"), r"C:\Apps\Tool 2026\x.exe");
    }

    #[test]
    fn heal_reaches_inside_groups() {
        let dir = std::env::temp_dir().join(format!("dock-heal-groups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let apps = dir.join("Adobe");
        std::fs::create_dir_all(apps.join("Photoshop 2027")).unwrap();
        std::fs::write(apps.join("Photoshop 2027").join("Photoshop.exe"), "").unwrap();
        let old = apps.join("Photoshop 2026").join("Photoshop.exe").to_string_lossy().into_owned();
        let gone = apps.join("Gone").join("gone.exe").to_string_lossy().into_owned();
        let home = Home::new(dir.clone());
        let text = format!(
            "version = 1\n\n[[item]]\nkind = \"group\"\nname = \"Adobe\"\n\n[[item.items]]\nname = \"Gone\"\ntarget = '{gone}'\n\n[[item.items]]\nname = \"Photoshop\"\ntarget = '{old}'\n"
        );
        std::fs::write(home.config(), &text).unwrap();
        let report = heal(&Store::new(&home), &home, &RealFs);
        assert_eq!(report.fixed.len(), 1, "{report:?}");
        assert_eq!(report.missing, vec![Spot::in_group(0, 0)]);
        let saved = std::fs::read_to_string(home.config()).unwrap();
        assert!(saved.contains("Photoshop 2027") && !saved.contains("Photoshop 2026"), "{saved}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
