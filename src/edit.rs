//! Changes to the dock's items: add, remove and move, on the dock and in and out of groups.
//! The same operations serve dragging on the dock, its menus and the settings window.
//!
//! dock.toml has no item ids, so items are addressed by position, and every change first checks
//! that the item there is the one meant (the file may have been edited by hand meanwhile).
//! Changes go through toml_edit, so your comments and layout survive.

use crate::config::{ItemConfig, Kind, RunState};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

/// Where an item sits: on the dock (`child: None`), or inside the group at `index`.
/// For an insert or a move it names a gap: the item ends up in front of what's there now
/// (one past the end appends).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    pub index: usize,
    pub child: Option<usize>,
}

impl Spot {
    pub fn dock(index: usize) -> Self {
        Self { index, child: None }
    }

    pub fn in_group(group: usize, child: usize) -> Self {
        Self { index: group, child: Some(child) }
    }
}

/// What an item is called in messages and checks: its name, or what kind of thing it is.
pub fn label(table: &Table) -> String {
    match table.get("name").and_then(Item::as_str).filter(|name| !name.trim().is_empty()) {
        Some(name) => name.to_string(),
        None => match table.get("kind").and_then(Item::as_str) {
            Some("separator") => "separator".into(),
            Some("recycle-bin") => "Recycle Bin".into(),
            Some("group") => "group".into(),
            _ => "item".into(),
        },
    }
}

/// The same label, for an item as the dock loaded it.
pub fn label_of(item: &ItemConfig) -> String {
    if !item.name.trim().is_empty() {
        return item.name.clone();
    }
    match item.kind {
        Kind::Separator => "separator".into(),
        Kind::RecycleBin => "Recycle Bin".into(),
        Kind::Group => "group".into(),
        Kind::App => "item".into(),
    }
}

/// Takes the item at `spot` out of the file, after checking it's the one called `expect`.
pub fn remove(doc: &mut DocumentMut, spot: Spot, expect: &str) -> Result<Table, String> {
    let list = list_mut(doc, spot, false)?;
    let at = spot.child.unwrap_or(spot.index);
    let found = list.get(at).map(label).ok_or_else(|| "That item isn't there any more: your dock changed in the meantime, so nothing was saved. Try again.".to_string())?;
    if found != expect {
        return Err(format!("Your dock changed in the meantime ({expect} has moved; {found} is there now), so nothing was saved. Try again."));
    }
    let table = list.remove(at);
    renumber(doc);
    Ok(table)
}

/// Puts `table` into the gap at `spot`.
pub fn insert(doc: &mut DocumentMut, spot: Spot, table: Table) -> Result<(), String> {
    if spot.child.is_some() && is_group(&table) {
        return Err("A group can't go inside another group.".into());
    }
    let list = list_mut(doc, spot, true)?;
    let at = spot.child.unwrap_or(spot.index);
    if at > list.len() {
        return Err("Your dock changed in the meantime, so nothing was saved. Try again.".into());
    }
    list.insert(at, table);
    renumber(doc);
    Ok(())
}

/// Moves the item at `from` (checked against `expect`) into the gap `to`, where `to` is counted
/// before the item is taken out, the way a drag sees it: dropping it on either side of itself
/// changes nothing. All or nothing: if any step fails, the file is left exactly as it was.
pub fn move_item(doc: &mut DocumentMut, from: Spot, to: Spot, expect: &str) -> Result<(), String> {
    if to.child.is_some() && from.child.is_none() && from.index == to.index {
        return Err("An item can't go inside itself.".into());
    }
    let mut trial = doc.clone();
    move_steps(&mut trial, from, to, expect)?;
    *doc = trial;
    Ok(())
}

fn move_steps(doc: &mut DocumentMut, from: Spot, mut to: Spot, expect: &str) -> Result<(), String> {
    let table = remove(doc, from, expect)?;
    // Taking it out shifts everything after it in the same list down by one.
    match (from.child, to.child) {
        (None, None) if to.index > from.index => to.index -= 1,
        (None, Some(_)) if to.index > from.index => to.index -= 1, // the group itself shifted
        (Some(a), Some(b)) if from.index == to.index && b > a => to.child = Some(b - 1),
        _ => {}
    }
    insert(doc, to, table)
}

/// Sets one text field of an item (its icon, say), after checking it's the one meant. An empty
/// value removes the key (for the icon: back to the program's own).
pub fn set_text(doc: &mut DocumentMut, spot: Spot, expect: &str, key: &str, value: &str) -> Result<(), String> {
    let list = list_mut(doc, spot, false)?;
    let at = spot.child.unwrap_or(spot.index);
    let table = list.get_mut(at).ok_or_else(|| "That item isn't there any more: your dock changed in the meantime, so nothing was saved. Try again.".to_string())?;
    let found = label(table);
    if found != expect {
        return Err(format!("Your dock changed in the meantime ({expect} has moved; {found} is there now), so nothing was saved. Try again."));
    }
    if value.is_empty() {
        table.remove(key);
    } else {
        table[key] = literal(value);
    }
    Ok(())
}
/// Sets a true/false field of an item (`admin`), after checking it's the one meant. Off removes
/// the key, since off is the default.
pub fn set_flag(doc: &mut DocumentMut, spot: Spot, expect: &str, key: &str, on: bool) -> Result<(), String> {
    let list = list_mut(doc, spot, false)?;
    let at = spot.child.unwrap_or(spot.index);
    let table = list.get_mut(at).ok_or_else(|| "That item isn't there any more: your dock changed in the meantime, so nothing was saved. Try again.".to_string())?;
    let found = label(table);
    if found != expect {
        return Err(format!("Your dock changed in the meantime ({expect} has moved; {found} is there now), so nothing was saved. Try again."));
    }
    if on {
        table[key] = toml_edit::value(true);
    } else {
        table.remove(key);
    }
    Ok(())
}

/// Makes a new group in the dock item's place, holding that item (right-click → New group).
/// Groups don't go inside groups. All or nothing.
pub fn make_group(doc: &mut DocumentMut, index: usize, expect: &str, name: &str) -> Result<(), String> {
    let mut trial = doc.clone();
    let mut table = remove(&mut trial, Spot::dock(index), expect)?;
    if is_group(&table) {
        return Err(format!("{expect} is a group already."));
    }
    table.decor_mut().set_prefix("
");
    let mut group = Table::new();
    group["kind"] = toml_edit::value("group");
    group["name"] = literal(name);
    let mut items = ArrayOfTables::new();
    items.push(table);
    group.insert("items", Item::ArrayOfTables(items));
    group.decor_mut().set_prefix("
");
    insert(&mut trial, Spot::dock(index), group)?;
    *doc = trial;
    Ok(())
}

/// Ungroup: the group's items take its place on the dock, in their order. All or nothing.
pub fn ungroup(doc: &mut DocumentMut, index: usize, expect: &str) -> Result<usize, String> {
    let mut trial = doc.clone();
    let group = remove(&mut trial, Spot::dock(index), expect)?;
    if !is_group(&group) {
        return Err(format!("{expect} isn't a group."));
    }
    let children: Vec<Table> = group.get("items").and_then(Item::as_array_of_tables).map(|list| list.iter().cloned().collect()).unwrap_or_default();
    let count = children.len();
    for (k, child) in children.into_iter().enumerate() {
        insert(&mut trial, Spot::dock(index + k), child)?;
    }
    *doc = trial;
    Ok(count)
}

/// Sets one of the dock's settings (`[dock] key = value`), keeping any comment after it.
pub fn set_dock(doc: &mut DocumentMut, key: &str, value: impl Into<toml_edit::Value>) -> Result<(), String> {
    let dock = doc.entry("dock").or_insert(toml_edit::table()).as_table_mut().ok_or("[dock] in dock.toml isn't a section")?;
    let mut value = value.into();
    match dock.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) => {
            *value.decor_mut() = existing.decor().clone();
            *existing = value;
        }
        None => {
            value.decor_mut().clear();
            dock.insert(key, Item::Value(value));
        }
    }
    Ok(())
}

/// Points an item at a new program (a newer version, say): its target, arguments and start-in
/// folder change; its place, name, icon and settings stay.
pub fn replace_program(doc: &mut DocumentMut, spot: Spot, expect: &str, new: &ItemConfig) -> Result<(), String> {
    let list = list_mut(doc, spot, false)?;
    let at = spot.child.unwrap_or(spot.index);
    let table = list.get_mut(at).ok_or_else(|| "That item isn't there any more: your dock changed in the meantime, so nothing was saved. Try again.".to_string())?;
    let found = label(table);
    if found != expect {
        return Err(format!("Your dock changed in the meantime ({expect} has moved; {found} is there now), so nothing was saved. Try again."));
    }
    for (key, text) in [("target", &new.target), ("args", &new.args), ("start_in", &new.start_in)] {
        if text.is_empty() {
            table.remove(key);
        } else {
            table[key] = literal(text);
        }
    }
    Ok(())
}
/// Where a removed item goes back: into its group if that group is still there (by name), else
/// onto the dock; at its old position, or the end if things have shrunk since.
pub fn put_back_spot(doc: &DocumentMut, was_at: usize, was_in: Option<&str>) -> Spot {
    let items = doc.get("item").and_then(Item::as_array_of_tables);
    let dock_len = items.map_or(0, ArrayOfTables::len);
    let group = was_in.and_then(|name| {
        items?.iter().enumerate().find_map(|(i, t)| {
            (is_group(t) && t.get("name").and_then(Item::as_str) == Some(name)).then(|| {
                let len = t.get("items").and_then(Item::as_array_of_tables).map_or(0, ArrayOfTables::len);
                (i, len)
            })
        })
    });
    match group {
        Some((index, len)) => Spot::in_group(index, was_at.min(len)),
        None => Spot::dock(was_at.min(dock_len)),
    }
}
/// A new `[[item]]` table for `item`, written the way the rest of the file is.
pub fn item_table(item: &ItemConfig) -> Table {
    let mut table = Table::new();
    match item.kind {
        Kind::App => {}
        Kind::Separator => table["kind"] = toml_edit::value("separator"),
        Kind::Group => table["kind"] = toml_edit::value("group"),
        Kind::RecycleBin => table["kind"] = toml_edit::value("recycle-bin"),
    }
    for (key, text) in [
        ("name", &item.name),
        ("target", &item.target),
        ("app_id", &item.app_id),
        ("args", &item.args),
        ("start_in", &item.start_in),
        ("icon", &item.icon),
    ] {
        if !text.is_empty() {
            table[key] = literal(text);
        }
    }
    match item.run {
        RunState::Normal => {}
        RunState::Minimized => table["run"] = toml_edit::value("minimized"),
        RunState::Maximized => table["run"] = toml_edit::value("maximized"),
    }
    if item.admin {
        table["admin"] = toml_edit::value(true);
    }
    table.decor_mut().set_prefix("\n"); // a blank line before each [[item]], like the rest
    table
}

/// A TOML string written single-quoted when possible (backslashes need no escaping), matching
/// the file's style.
/// A copy of dock.toml that works on another PC or account: paths in your own folders are
/// written with the variable for that folder (`%USERPROFILE%\Documents\…`), so they point at
/// the other person's. `folders` are (variable, its value here), e.g. ("LOCALAPPDATA",
/// "C:\Users\alex\AppData\Local"); the longest that fits wins, and only whole folder names
/// match. Shell names, web addresses and paths already written with a variable stay as they are.
pub fn portable(text: &str, folders: &[(&str, String)]) -> Result<String, String> {
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let mut by_length: Vec<&(&str, String)> = folders.iter().filter(|(_, value)| !value.trim().is_empty()).collect();
    by_length.sort_by_key(|(_, value)| std::cmp::Reverse(value.trim_end_matches('\\').len()));
    let rewrite = |value: &str| -> Option<String> {
        by_length.iter().find_map(|(name, folder)| {
            let folder = folder.trim_end_matches('\\');
            let head = value.get(..folder.len())?;
            let rest = &value[folder.len()..];
            (head.eq_ignore_ascii_case(folder) && (rest.is_empty() || rest.starts_with('\\'))).then(|| format!("%{name}%{rest}"))
        })
    };
    fn each(list: Option<&mut Item>, rewrite: &dyn Fn(&str) -> Option<String>) {
        let Some(list) = list.and_then(Item::as_array_of_tables_mut) else { return };
        for table in list.iter_mut() {
            for key in ["target", "start_in", "icon"] {
                let Some(new) = table.get(key).and_then(Item::as_str).and_then(rewrite) else { continue };
                let decor = table.get(key).and_then(Item::as_value).map(|v| v.decor().clone());
                let mut item = literal(&new);
                if let (Some(decor), Some(value)) = (decor, item.as_value_mut()) {
                    *value.decor_mut() = decor;
                }
                table.insert(key, item);
            }
            each(table.get_mut("items"), rewrite);
        }
    }
    each(doc.get_mut("item"), &rewrite);
    Ok(doc.to_string())
}

pub fn literal(s: &str) -> Item {
    if !s.contains('\'') && !s.contains('\n') {
        if let Ok(value) = format!("'{s}'").parse::<toml_edit::Value>() {
            return Item::Value(value);
        }
    }
    toml_edit::value(s)
}

fn is_group(table: &Table) -> bool {
    table.get("kind").and_then(Item::as_str) == Some("group")
}

/// The list a spot lives in: the dock's items, or a group's. `create` makes a missing list.
fn list_mut(doc: &mut DocumentMut, spot: Spot, create: bool) -> Result<&mut ArrayOfTables, String> {
    if create && doc.get("item").is_none() {
        doc.insert("item", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let items = doc.get_mut("item").and_then(Item::as_array_of_tables_mut).ok_or("dock.toml has no items")?;
    if spot.child.is_none() {
        return Ok(items);
    }
    let group = items.get_mut(spot.index).ok_or_else(|| "That group isn't there any more: your dock changed in the meantime. Try again.".to_string())?;
    if !is_group(group) {
        return Err(format!("{} isn't a group.", label(group)));
    }
    if create && group.get("items").is_none() {
        group.insert("items", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    group.get_mut("items").and_then(Item::as_array_of_tables_mut).ok_or_else(|| "that group is empty".to_string())
}

/// toml_edit writes tables out in the order they were read, so after a change number them
/// again in the order they now sit.
pub fn renumber(doc: &mut DocumentMut) {
    let mut next = 1;
    renumber_table(doc.as_table_mut(), &mut next);
}

fn renumber_table(table: &mut Table, next: &mut isize) {
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(t) => {
                if !t.is_dotted() {
                    t.set_position(Some(*next));
                    *next += 1;
                }
                renumber_table(t, next);
            }
            Item::ArrayOfTables(list) => {
                for t in list.iter_mut() {
                    t.set_position(Some(*next));
                    *next += 1;
                    renumber_table(t, next);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_copy_writes_your_folders_so_they_work_on_another_pc() {
        let folders = [
            ("USERPROFILE", r"C:\Users\alex".to_string()),
            ("LOCALAPPDATA", r"C:\Users\alex\AppData\Local".to_string()),
            ("APPDATA", r"C:\Users\alex\AppData\Roaming".to_string()),
            ("ProgramFiles", r"C:\Program Files".to_string()),
        ];
        let text = r#"version = 1

[[item]]
name = "Tool"
target = 'C:\Users\alex\AppData\Local\Programs\Tool\tool.exe'  # mine
start_in = 'c:\users\ALEX\Documents'
icon = 'C:\Program Files\App\app.exe,0'

[[item]]
name = "Someone else"
target = 'C:\Users\alexander\x.exe'

[[item]]
name = "Documents"
target = 'shell:Personal'

[[item]]
kind = "group"
name = "Games"

[[item.items]]
name = "Game"
target = 'C:\Users\alex\AppData\Roaming\Game\game.exe'
"#;
        let copy = portable(text, &folders).unwrap();
        assert!(copy.contains(r"target = '%LOCALAPPDATA%\Programs\Tool\tool.exe'  # mine"), "the longest fitting folder, comment kept: {copy}");
        assert!(copy.contains(r"start_in = '%USERPROFILE%\Documents'"), "any case: {copy}");
        assert!(copy.contains(r"icon = '%ProgramFiles%\App\app.exe,0'"), "{copy}");
        assert!(copy.contains(r"target = 'C:\Users\alexander\x.exe'"), "only whole folder names: {copy}");
        assert!(copy.contains("target = 'shell:Personal'"), "{copy}");
        assert!(copy.contains(r"target = '%APPDATA%\Game\game.exe'"), "inside groups too: {copy}");
        let parsed = crate::config::parse(&copy).unwrap();
        assert_eq!(parsed.config.items.len(), 4);
    }

    const FILE: &str = r#"# My dock
version = 1

[dock]
icon_size = 53 # keep me

[[item]]
name = 'Photoshop'
target = 'C:\Adobe\Photoshop.exe' # the 2026 one

[[item]]
kind = "separator"

[[item]]
name = 'Chrome'
target = 'C:\chrome.exe'

[[item]]
kind = "group"
name = 'Games'

[[item.items]]
name = 'Steam'
target = 'C:\steam.exe'

[[item.items]]
name = 'Epic'
target = 'C:\epic.exe'

[[item]]
name = 'Explorer'
target = 'C:\Windows\explorer.exe'
"#;

    fn doc() -> DocumentMut {
        FILE.parse().unwrap()
    }

    /// The dock as the file now reads: top-level labels, with a group's children in brackets.
    fn order(doc: &DocumentMut) -> String {
        let text = doc.to_string();
        let config = crate::config::parse(&text).expect("still a valid dock.toml").config;
        config
            .items
            .iter()
            .map(|item| {
                if item.kind == Kind::Group {
                    let children: Vec<String> = item.items.iter().map(label_of).collect();
                    format!("{}[{}]", label_of(item), children.join(","))
                } else {
                    label_of(item)
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn remove_takes_the_named_item_and_keeps_comments() {
        let mut d = doc();
        let table = remove(&mut d, Spot::dock(2), "Chrome").unwrap();
        assert_eq!(label(&table), "Chrome");
        assert_eq!(order(&d), "Photoshop separator Games[Steam,Epic] Explorer");
        let text = d.to_string();
        assert!(text.contains("# My dock") && text.contains("# keep me") && text.contains("# the 2026 one"));
    }

    #[test]
    fn remove_refuses_when_the_file_changed_meanwhile() {
        let mut d = doc();
        let err = remove(&mut d, Spot::dock(2), "Photoshop").unwrap_err();
        assert!(err.contains("changed in the meantime"), "{err}");
        assert_eq!(order(&d), "Photoshop separator Chrome Games[Steam,Epic] Explorer", "nothing touched");
        assert!(remove(&mut d, Spot::dock(9), "Chrome").is_err());
    }

    #[test]
    fn moving_along_the_dock_both_ways() {
        let mut d = doc();
        move_item(&mut d, Spot::dock(0), Spot::dock(3), "Photoshop").unwrap(); // gap before Games
        assert_eq!(order(&d), "separator Chrome Photoshop Games[Steam,Epic] Explorer");
        move_item(&mut d, Spot::dock(4), Spot::dock(0), "Explorer").unwrap();
        assert_eq!(order(&d), "Explorer separator Chrome Photoshop Games[Steam,Epic]");
        move_item(&mut d, Spot::dock(1), Spot::dock(5), "separator").unwrap(); // to the very end
        assert_eq!(order(&d), "Explorer Chrome Photoshop Games[Steam,Epic] separator");
    }

    #[test]
    fn dropping_an_item_either_side_of_itself_changes_nothing() {
        for gap in [2, 3] {
            let mut d = doc();
            move_item(&mut d, Spot::dock(2), Spot::dock(gap), "Chrome").unwrap();
            assert_eq!(order(&d), "Photoshop separator Chrome Games[Steam,Epic] Explorer");
        }
    }

    #[test]
    fn into_and_out_of_a_group() {
        let mut d = doc();
        move_item(&mut d, Spot::dock(0), Spot::in_group(3, 1), "Photoshop").unwrap();
        assert_eq!(order(&d), "separator Chrome Games[Steam,Photoshop,Epic] Explorer");
        move_item(&mut d, Spot::dock(3), Spot::in_group(2, 3), "Explorer").unwrap(); // after the group: no shift
        assert_eq!(order(&d), "separator Chrome Games[Steam,Photoshop,Epic,Explorer]");
        move_item(&mut d, Spot::in_group(2, 0), Spot::dock(0), "Steam").unwrap();
        assert_eq!(order(&d), "Steam separator Chrome Games[Photoshop,Epic,Explorer]");
        move_item(&mut d, Spot::in_group(3, 0), Spot::in_group(3, 3), "Photoshop").unwrap(); // within the group
        assert_eq!(order(&d), "Steam separator Chrome Games[Epic,Explorer,Photoshop]");
    }

    #[test]
    fn groups_stay_one_level_deep() {
        let mut d = doc();
        let mut other = Table::new();
        other["kind"] = toml_edit::value("group");
        other["name"] = literal("Other");
        insert(&mut d, Spot::dock(0), other).unwrap();
        let err = move_item(&mut d, Spot::dock(0), Spot::in_group(4, 0), "Other").unwrap_err();
        assert!(err.contains("inside another group"), "{err}");
        let err = move_item(&mut d, Spot::dock(1), Spot::in_group(3, 0), "Photoshop").unwrap_err();
        assert!(err.contains("isn't a group"), "{err}");
        assert_eq!(order(&d), "Other[] Photoshop separator Chrome Games[Steam,Epic] Explorer", "nothing moved");
    }

    #[test]
    fn a_new_item_is_written_like_the_rest() {
        let mut d = doc();
        let mut item = ItemConfig::default();
        item.name = "VLC".into();
        item.target = r"C:\Program Files\VideoLAN\VLC\vlc.exe".into();
        item.run = RunState::Maximized;
        insert(&mut d, Spot::dock(1), item_table(&item)).unwrap();
        assert_eq!(order(&d), "Photoshop VLC separator Chrome Games[Steam,Epic] Explorer");
        let text = d.to_string();
        assert!(
            text.contains("\n\n[[item]]\nname = 'VLC'\ntarget = 'C:\\Program Files\\VideoLAN\\VLC\\vlc.exe'\nrun = \"maximized\"\n"),
            "{text}"
        );
    }

    #[test]
    fn a_new_group_member_gets_a_list_when_the_group_is_empty() {
        let mut d = doc();
        let mut group = Table::new();
        group["kind"] = toml_edit::value("group");
        group["name"] = literal("Empty");
        insert(&mut d, Spot::dock(5), group).unwrap();
        let mut item = ItemConfig::default();
        item.name = "Notepad".into();
        insert(&mut d, Spot::in_group(5, 0), item_table(&item)).unwrap();
        assert_eq!(order(&d), "Photoshop separator Chrome Games[Steam,Epic] Explorer Empty[Notepad]");
    }

    #[test]
    fn put_back_goes_to_its_group_or_its_old_place() {
        let d = doc();
        assert_eq!(put_back_spot(&d, 1, Some("Games")), Spot::in_group(3, 1));
        assert_eq!(put_back_spot(&d, 9, Some("Games")), Spot::in_group(3, 2), "clamped to the group's end");
        assert_eq!(put_back_spot(&d, 1, Some("Gone")), Spot::dock(1), "group removed since: the dock");
        assert_eq!(put_back_spot(&d, 40, None), Spot::dock(5), "dock shrank: the end");
    }
    #[test]
    fn replacing_a_program_keeps_place_name_and_comments() {
        let mut d = doc();
        let mut newer = ItemConfig::new("Adobe Photoshop 2027", r"C:\Adobe\2027\Photoshop.exe");
        newer.args = "--fast".into();
        replace_program(&mut d, Spot::dock(0), "Photoshop", &newer).unwrap();
        assert_eq!(order(&d), "Photoshop separator Chrome Games[Steam,Epic] Explorer", "same place, same name");
        let text = d.to_string();
        assert!(text.contains(r"target = 'C:\Adobe\2027\Photoshop.exe'") && text.contains("args = '--fast'"), "{text}");
        assert!(text.contains("# My dock") && text.contains("# keep me"));
        assert!(replace_program(&mut d, Spot::dock(2), "Photoshop", &newer).is_err(), "checks it's the one meant");
    }
    #[test]
    fn setting_and_clearing_an_icon() {
        let mut d = doc();
        set_text(&mut d, Spot::dock(2), "Chrome", "icon", r"C:\Users\alex\Pictures\Icons\chrome.png").unwrap();
        assert!(d.to_string().contains("target = 'C:\\chrome.exe'\nicon = 'C:\\Users\\alex\\Pictures\\Icons\\chrome.png'\n"), "{d}");
        set_text(&mut d, Spot::dock(2), "Chrome", "icon", "").unwrap();
        assert_eq!(d.to_string(), FILE, "back to the program's own icon: the file as it was");
        assert!(set_text(&mut d, Spot::dock(0), "Chrome", "icon", "x.png").is_err(), "checks it's the one meant");
    }
    #[test]
    fn a_field_added_and_taken_away_again_across_saves_leaves_the_file_as_it_was() {
        // The icon picker shows a pick by saving it, and Cancel takes it away again (as does a
        // Properties window's Revert): each a save, the file read back in between. Nothing may
        // be left behind.
        let file = "version = 1\n\n[dock]\npopup_delay_ms = 150\n\n[[item]]\nname = 'Alpha'\ntarget = 'C:\\WINDOWS\\explorer.exe'\n\n[[item]]\nname = 'Beta'\ntarget = 'C:\\b.exe'\n\n[[item]]\nkind = 'recycle-bin'\n";
        for key in ["icon", "args", "start_in"] {
            let mut d: DocumentMut = file.parse().unwrap();
            set_text(&mut d, Spot::dock(0), "Alpha", key, r"C:\WINDOWS\System32\imageres.dll,3").unwrap();
            let mut d: DocumentMut = d.to_string().parse().unwrap(); // saved, and read back
            set_text(&mut d, Spot::dock(0), "Alpha", key, "").unwrap();
            assert_eq!(d.to_string(), file, "{key}: the file as it was");
        }
    }
    #[test]
    fn a_new_group_holds_the_item_in_its_place() {
        let mut d = doc();
        make_group(&mut d, 2, "Chrome", "Browsers").unwrap();
        assert_eq!(order(&d), "Photoshop separator Browsers[Chrome] Games[Steam,Epic] Explorer");
        assert!(make_group(&mut d, 3, "Games", "More").is_err(), "no groups in groups");
        assert!(make_group(&mut d, 0, "Chrome", "X").is_err(), "checks it's the one meant");
        assert_eq!(order(&d), "Photoshop separator Browsers[Chrome] Games[Steam,Epic] Explorer", "a refused change changes nothing");
        // Then more go in, and it can be undone by ungrouping.
        move_item(&mut d, Spot::dock(4), Spot::in_group(2, 1), "Explorer").unwrap();
        assert_eq!(order(&d), "Photoshop separator Browsers[Chrome,Explorer] Games[Steam,Epic]");
    }
    #[test]
    fn ungroup_puts_the_items_back_in_its_place() {
        let mut d = doc();
        assert_eq!(ungroup(&mut d, 3, "Games").unwrap(), 2);
        assert_eq!(order(&d), "Photoshop separator Chrome Steam Epic Explorer");
        assert!(ungroup(&mut d, 2, "Chrome").is_err(), "not a group");
        assert_eq!(order(&d), "Photoshop separator Chrome Steam Epic Explorer");
        assert!(d.to_string().contains("# keep me"), "comments kept");
    }
    #[test]
    fn dock_settings_change_in_place_keeping_comments() {
        let mut d = doc();
        set_dock(&mut d, "icon_size", 64).unwrap();
        assert!(d.to_string().contains("icon_size = 64 # keep me
"), "{d}");
        set_dock(&mut d, "label_size", 13.5).unwrap();
        set_dock(&mut d, "locked", true).unwrap();
        let config = crate::config::parse(&d.to_string()).unwrap().config;
        assert_eq!((config.dock.icon_size, config.dock.label_size, config.dock.locked), (64.0, 13.5, true));
        assert_eq!(order(&d), "Photoshop separator Chrome Games[Steam,Epic] Explorer", "items untouched");
        let mut bare: DocumentMut = "version = 1
".parse().unwrap();
        set_dock(&mut bare, "spacing", 12).unwrap();
        assert_eq!(crate::config::parse(&bare.to_string()).unwrap().config.dock.spacing, 12.0, "{bare}");
    }
    #[test]
    fn run_as_administrator_on_and_off() {
        let mut d = doc();
        set_flag(&mut d, Spot::in_group(3, 1), "Epic", "admin", true).unwrap();
        assert!(d.to_string().contains("target = 'C:\\epic.exe'\nadmin = true\n"), "{d}");
        assert!(crate::config::parse(&d.to_string()).unwrap().config.items[3].items[1].admin);
        set_flag(&mut d, Spot::in_group(3, 1), "Epic", "admin", false).unwrap();
        assert_eq!(d.to_string(), FILE, "off is the default: the key goes, the file reads as before");
        assert!(set_flag(&mut d, Spot::dock(0), "Epic", "admin", true).is_err(), "checks it's the one meant");
    }
    #[test]
    fn renaming_and_a_cleared_name() {
        let mut d = doc();
        set_text(&mut d, Spot::dock(2), "Chrome", "name", "Browser").unwrap();
        assert_eq!(order(&d), "Photoshop separator Browser Games[Steam,Epic] Explorer");
        set_text(&mut d, Spot::dock(2), "Browser", "args", "--new-window").unwrap();
        assert_eq!(crate::config::parse(&d.to_string()).unwrap().config.items[2].args, "--new-window");
        set_text(&mut d, Spot::dock(2), "Browser", "name", "").unwrap();
        assert_eq!(order(&d), "Photoshop separator item Games[Steam,Epic] Explorer", "no name: shown by kind");
    }
    #[test]
    fn a_removed_item_put_back_reads_exactly_as_before() {
        let mut d = doc();
        let table = remove(&mut d, Spot::dock(0), "Photoshop").unwrap();
        insert(&mut d, Spot::dock(0), table).unwrap();
        assert_eq!(d.to_string(), FILE);
    }
}
