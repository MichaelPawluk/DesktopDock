//! Recently removed: the last 20 items taken off the dock, newest first, kept in `removed.toml`
//! in the dock folder. A removal is never final: right-click → Put back (and the settings window
//! later). The file is safe to read, edit or delete.

use crate::edit;
use crate::home::Home;
use crate::store;
use std::path::PathBuf;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};
use windows::Win32::System::SystemInformation::GetLocalTime;

const KEEP: usize = 20;
const HEADER: &str = "# Items removed from the dock, newest first (the last 20).\n\
                      # To return one: right-click the dock > Put back, or Dock settings > Recently removed. Safe to delete.\n";
/// The header before 1.0, taken off when the file is read (so it isn't kept under the new one).
const OLD_HEADER: &str = "# Items removed from the dock, newest first (the last 20).\n\
                          # Right-click the dock → Put back to return one. Safe to delete.\n";
/// Bookkeeping added to each removed item.
const REMOVED_AT: &str = "removed_at";
/// Its position when removed, counted from 1.
const WAS_AT: &str = "was_at";
/// The group it was in, if any.
const WAS_IN: &str = "was_in";

#[derive(Debug, Clone)]
pub struct Entry {
    /// When it was removed, to the millisecond; also its id.
    pub removed_at: String,
    pub label: String,
    /// Where it was: position on the dock or in its group (counted from 0), and the group.
    pub was_at: usize,
    pub was_in: Option<String>,
    /// The item exactly as it was in dock.toml, ready to go back in.
    pub table: Table,
}

#[derive(Debug, Clone)]
pub struct Removed {
    path: PathBuf,
}

impl Removed {
    pub fn new(home: &Home) -> Self {
        Self { path: home.dir.join("removed.toml") }
    }

    /// Newest first. A missing or unreadable file is an empty list.
    pub fn list(&self) -> Vec<Entry> {
        let doc = self.load();
        let Some(list) = doc.get("removed").and_then(Item::as_array_of_tables) else { return Vec::new() };
        list.iter().filter_map(entry_from).collect()
    }

    /// Records an item just taken out of dock.toml. Returns its id.
    pub fn add(&self, table: &Table, was_at: usize, was_in: Option<&str>) -> Result<String, String> {
        let _lock = self.lock()?;
        self.add_locked(table, was_at, was_in)
    }

    fn add_locked(&self, table: &Table, was_at: usize, was_in: Option<&str>) -> Result<String, String> {
        let mut doc = self.load();
        // The time is the id; two removals within the same millisecond get a suffix.
        let taken: Vec<String> = self.list().into_iter().map(|e| e.removed_at).collect();
        let base = now();
        let mut removed_at = base.clone();
        let mut n = 1;
        while taken.contains(&removed_at) {
            n += 1;
            removed_at = format!("{base}-{n}");
        }
        let mut entry = Table::new();
        entry[REMOVED_AT] = toml_edit::value(removed_at.as_str());
        entry[WAS_AT] = toml_edit::value(was_at as i64 + 1);
        if let Some(group) = was_in {
            entry[WAS_IN] = edit::literal(group);
        }
        for (key, item) in table.iter() {
            entry.insert(key, item.clone());
        }
        entry.decor_mut().set_prefix("\n");
        if doc.get("removed").is_none() {
            doc.insert("removed", Item::ArrayOfTables(ArrayOfTables::new()));
        }
        let list = doc.get_mut("removed").and_then(Item::as_array_of_tables_mut).ok_or("removed.toml is damaged")?;
        list.insert(0, entry);
        while list.len() > KEEP {
            list.remove(list.len() - 1);
        }
        self.save(doc)?;
        Ok(removed_at)
    }

    /// Takes the entry `id` out of the list (to put it back, or after an undo).
    pub fn take(&self, id: &str) -> Option<Entry> {
        let _lock = self.lock().ok()?;
        let mut doc = self.load();
        let list = doc.get_mut("removed").and_then(Item::as_array_of_tables_mut)?;
        let index = list.iter().position(|t| t.get(REMOVED_AT).and_then(Item::as_str) == Some(id))?;
        let entry = entry_from(list.get(index)?)?;
        list.remove(index);
        self.save(doc).ok()?;
        Some(entry)
    }

    /// Puts an entry back at the top of the list (undoing a Put back).
    pub fn restore(&self, entry: &Entry) -> Result<(), String> {
        let _lock = self.lock()?;
        let id = self.add_locked(&entry.table, entry.was_at, entry.was_in.as_deref())?;
        // Keep its original time, so it reads as it did.
        let mut doc = self.load();
        if let Some(first) = doc.get_mut("removed").and_then(Item::as_array_of_tables_mut).and_then(|l| l.get_mut(0)) {
            if first.get(REMOVED_AT).and_then(Item::as_str) == Some(id.as_str()) {
                first[REMOVED_AT] = toml_edit::value(entry.removed_at.as_str());
            }
        }
        self.save(doc)
    }

    /// Forgets every entry (Settings > Recently removed > Clear the list).
    pub fn clear(&self) -> Result<(), String> {
        let _lock = self.lock()?;
        self.save(DocumentMut::new())
    }

    /// The dock folder's save lock: the dock and Dock settings both change this list.
    fn lock(&self) -> Result<store::SaveLock, String> {
        store::lock_folder(self.path.parent().unwrap_or(std::path::Path::new(".")))
    }

    fn load(&self) -> DocumentMut {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let body = text.strip_prefix(HEADER).or_else(|| text.strip_prefix(OLD_HEADER)).unwrap_or(&text);
        body.parse().unwrap_or_default()
    }

    fn save(&self, mut doc: DocumentMut) -> Result<(), String> {
        edit::renumber(&mut doc);
        let body = doc.to_string();
        store::atomic_write(&self.path, &format!("{HEADER}{body}")).map_err(|e| format!("Couldn't save Recently removed: {e}."))
    }
}

fn entry_from(table: &Table) -> Option<Entry> {
    let removed_at = table.get(REMOVED_AT)?.as_str()?.to_string();
    let was_at = table.get(WAS_AT).and_then(Item::as_integer).unwrap_or(1).max(1) as usize - 1;
    let was_in = table.get(WAS_IN).and_then(Item::as_str).map(str::to_string);
    let mut clean = table.clone();
    for key in [REMOVED_AT, WAS_AT, WAS_IN] {
        clean.remove(key);
    }
    clean.decor_mut().set_prefix("\n");
    Some(Entry { removed_at, label: edit::label(&clean), was_at, was_in, table: clean })
}

fn now() -> String {
    let t = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{Spot, insert, remove};

    const FILE: &str = r#"version = 1

[[item]]
name = 'Photoshop'
target = 'C:\Adobe\Photoshop.exe' # the 2026 one

[[item]]
kind = "group"
name = 'Games'

[[item.items]]
name = 'Steam'
target = 'C:\steam.exe'
"#;

    fn temp_home(name: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("dock-removed-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Home::new(dir)
    }

    #[test]
    fn removed_items_come_back_exactly_as_they_were() {
        let home = temp_home("roundtrip");
        let removed = Removed::new(&home);
        let mut doc: DocumentMut = FILE.parse().unwrap();
        let table = remove(&mut doc, Spot::dock(0), "Photoshop").unwrap();
        let id = removed.add(&table, 0, None).unwrap();

        let text = std::fs::read_to_string(home.dir.join("removed.toml")).unwrap();
        assert!(text.starts_with(HEADER) && text.contains("was_at = 1") && text.contains("# the 2026 one"), "{text}");
        let list = removed.list();
        assert_eq!((list.len(), list[0].label.as_str(), list[0].was_at), (1, "Photoshop", 0));

        let entry = removed.take(&id).unwrap();
        assert!(removed.list().is_empty());
        insert(&mut doc, Spot::dock(entry.was_at), entry.table).unwrap();
        assert_eq!(doc.to_string(), FILE);
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_whole_group_and_where_it_was() {
        let home = temp_home("group");
        let removed = Removed::new(&home);
        let mut doc: DocumentMut = FILE.parse().unwrap();
        let steam = remove(&mut doc, Spot::in_group(1, 0), "Steam").unwrap();
        removed.add(&steam, 0, Some("Games")).unwrap();
        let games = remove(&mut doc, Spot::dock(1), "Games").unwrap();
        removed.add(&games, 1, None).unwrap();
        let list = removed.list();
        assert_eq!(list.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["Games", "Steam"], "newest first");
        assert_eq!(list[1].was_in.as_deref(), Some("Games"));
        let text = std::fs::read_to_string(home.dir.join("removed.toml")).unwrap();
        assert!(text.parse::<DocumentMut>().is_ok(), "{text}");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn keeps_the_newest_twenty() {
        let home = temp_home("keep");
        let removed = Removed::new(&home);
        for i in 0..25 {
            let mut table = Table::new();
            table["name"] = edit::literal(&format!("App {i}"));
            removed.add(&table, i, None).unwrap();
        }
        let list = removed.list();
        assert_eq!(list.len(), KEEP);
        assert_eq!(list[0].label, "App 24");
        assert_eq!(list[KEEP - 1].label, "App 5");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_missing_or_damaged_file_is_just_an_empty_list() {
        let home = temp_home("damaged");
        let removed = Removed::new(&home);
        assert!(removed.list().is_empty());
        std::fs::write(home.dir.join("removed.toml"), "this is [not toml").unwrap();
        assert!(removed.list().is_empty());
        assert!(removed.take("anything").is_none());
        let _ = std::fs::remove_dir_all(&home.dir);
    }
}
