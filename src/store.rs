//! Safe saving of dock.toml (docs/config-format.md, "Saving"):
//! - every write is verified (it must parse) before it can replace the file;
//! - the replace is atomic (temp file → flush to disk → MoveFileEx with write-through), so an
//!   interruption leaves the old file or the new one, never a broken one;
//! - backups: 20 recent (at most one a minute), 7 daily, pinned copies before risky changes,
//!   and `last-good.toml`, the last version that loaded cleanly;
//! - edits apply to the file as it is on disk at that moment, so hand edits are never lost;
//! - one save at a time per dock folder, across threads and processes (the dock, Dock settings).

use crate::config::{self, Parsed};
use crate::home::Home;
use crate::{log_error, log_info, log_warn};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};
use toml_edit::DocumentMut;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::HSTRING;

const RECENT_KEEP: usize = 20;
const DAILY_KEEP: usize = 7;
const RECENT_EVERY: Duration = Duration::from_secs(60);

/// One writer at a time, across threads.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// How long a save waits for another window's save to finish.
const SAVE_WAIT: Duration = Duration::from_secs(4);

/// Temp files a minute old were left by a save that was cut off.
const LEFTOVER_AGE: Duration = Duration::from_secs(60);

/// Makes every temp file name unique, within this process too.
static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Saving in a dock folder: this process's writers one at a time (`WRITE_LOCK`), then one
/// process at a time (the dock, Dock settings, a second copy) through a named mutex for that
/// folder. Always taken in that order. Not `Send`: the thread that took it releases it.
pub(crate) struct SaveLock {
    // Released first, then the in-process lock (fields drop in this order).
    _named: NamedLock,
    _local: std::sync::MutexGuard<'static, ()>,
}

/// Waits (at most 4 s) until no other thread or process is saving in `dir`.
pub(crate) fn lock_folder(dir: &Path) -> Result<SaveLock, String> {
    let local = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let named = NamedLock::take(dir, SAVE_WAIT)?;
    Ok(SaveLock { _named: named, _local: local })
}

/// The named mutex part of `SaveLock`. None if Windows couldn't make it: then saving still
/// works, kept in order within this process only.
struct NamedLock(Option<HANDLE>);

impl NamedLock {
    fn take(dir: &Path, wait: Duration) -> Result<Self, String> {
        let name = HSTRING::from(mutex_name(dir));
        let Ok(handle) = (unsafe { CreateMutexW(None, false, &name) }) else {
            log_warn!("couldn't make the save lock for {}; saving without it", dir.display());
            return Ok(Self(None));
        };
        let waited = unsafe { WaitForSingleObject(handle, wait.as_millis().min(u32::MAX as u128) as u32) };
        if waited == WAIT_OBJECT_0 {
            return Ok(Self(Some(handle)));
        }
        if waited == WAIT_ABANDONED {
            // Its save either finished or never replaced the file (the replace is atomic).
            log_warn!("a program ended while saving in {}; carrying on", dir.display());
            return Ok(Self(Some(handle)));
        }
        unsafe {
            let _ = CloseHandle(handle);
        }
        Err("Your dock is busy saving in another window. Try again in a moment.".into())
    }
}

impl Drop for NamedLock {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            unsafe {
                let _ = ReleaseMutex(handle);
                let _ = CloseHandle(handle);
            }
        }
    }
}

/// `Local\DesktopDock.Config.<hash>`: one name per dock folder (case and slashes ignored), so
/// the dev dock, the installed one and the tests never wait for each other. FNV-1a, so every
/// version of the program agrees on it.
fn mutex_name(dir: &Path) -> String {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let key = absolute.components().collect::<PathBuf>().to_string_lossy().to_lowercase();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.trim_end_matches('\\').bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("Local\\DesktopDock.Config.{hash:016x}")
}

/// A saved edit: the file just before and just after it, so it can be undone.
#[derive(Debug, Clone)]
pub struct Change {
    pub before: String,
    pub after: String,
}

#[derive(Clone, Debug)]
pub struct Store {
    config: PathBuf,
    backups: PathBuf,
}

/// How the dock is running, decided when the file is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// The file is from a newer dock: used, never saved over.
    ReadOnly,
    /// The file couldn't be read: running from last-good (or empty) until it's fixed.
    SafeStart,
}

pub struct Opened {
    pub parsed: Parsed,
    pub mode: Mode,
    /// Something to tell the user (shown without blocking the dock).
    pub notice: Option<String>,
    /// The file couldn't be read at all (not an error in it): the dock reads it again once it can.
    pub unreadable: bool,
}

impl Store {
    pub fn new(home: &Home) -> Self {
        Self { config: home.config(), backups: home.backups() }
    }

    pub fn config_path(&self) -> &Path {
        &self.config
    }

    /// The dock folder.
    fn folder(&self) -> &Path {
        self.config.parent().unwrap_or(Path::new("."))
    }

    /// Applies `edit` to the file as it is on disk now, then saves it safely.
    #[allow(dead_code)] // kept for edits that don't need Undo
    pub fn edit(&self, edit: impl FnOnce(&mut DocumentMut) -> Result<(), String>) -> Result<(), String> {
        self.change(edit).map(|_| ())
    }

    /// Like `edit`, and also returns what the edit returned and the change itself, for undo.
    pub fn change<R>(&self, edit: impl FnOnce(&mut DocumentMut) -> Result<R, String>) -> Result<(R, Change), String> {
        let _lock = lock_folder(self.folder())?;
        let before = config::read_text(&self.config).map_err(|e| e.to_string())?;
        let mut doc: DocumentMut = before.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
        let result = edit(&mut doc)?;
        let after = same_line_ends(&before, doc.to_string());
        // Nothing changed: no save, no backup, and the file's date stays as it was.
        if after != before {
            self.write_locked(&after)?;
        }
        Ok((result, Change { before, after }))
    }

    /// Undoes `change`, but only if the file still reads exactly as that change left it, so a
    /// hand edit made since is never thrown away.
    pub fn revert(&self, change: &Change) -> Result<(), String> {
        let _lock = lock_folder(self.folder())?;
        let now = config::read_text(&self.config).map_err(|e| e.to_string())?;
        if now != change.after {
            return Err("Your dock has changed since, so undoing now would lose those changes.".into());
        }
        self.write_locked(&change.before)
    }

    /// Replaces the whole file (migration, import, restore), safely.
    pub fn replace(&self, text: &str) -> Result<(), String> {
        let _lock = lock_folder(self.folder())?;
        self.write_locked(text)
    }

    fn write_locked(&self, text: &str) -> Result<(), String> {
        let parsed = config::parse(text).map_err(|e| {
            log_warn!("not saved: the new text doesn't read back ({e})");
            "That change would have left your dock unreadable, so it wasn't saved.".to_string()
        })?;
        if parsed.read_only {
            return Err("Your dock was saved by a newer Desktop Dock, so this one doesn't change it.".into());
        }
        if let Err(e) = self.backup_current() {
            log_warn!("backup before saving failed: {e}");
        }
        atomic_write(&self.config, text).map_err(|e| format!("Couldn't save your dock: {e}."))
    }

    /// Every backup there is, newest first: recent (one a minute, the last 20), daily (the last 7)
    /// and pinned (snapshots, and copies made before imports, migrations and restores).
    pub fn backups(&self) -> Vec<Backup> {
        let mut found = Vec::new();
        for (folder, kind) in [("recent", BackupKind::Recent), ("daily", BackupKind::Daily), ("pinned", BackupKind::Pinned)] {
            let Ok(entries) = std::fs::read_dir(self.backups.join(folder)) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("toml")) {
                    continue;
                }
                let modified = entry.metadata().and_then(|m| m.modified()).ok();
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let (label, when) = split_backup_name(&stem);
                found.push(Backup { path, kind, label, when, modified });
            }
        }
        found.sort_by(|a, b| b.modified.cmp(&a.modified));
        found
    }

    pub fn backups_dir(&self) -> &Path {
        &self.backups
    }

    /// Copies the current file into `backups\pinned\` (kept until you delete it).
    pub fn pin(&self, label: &str) -> Option<PathBuf> {
        let text = std::fs::read(&self.config).ok()?;
        let path = self.backups.join("pinned").join(format!("{label}-{}.toml", timestamp().1));
        match atomic_write_bytes(&path, &text) {
            Ok(()) => Some(path),
            Err(e) => {
                log_warn!("couldn't pin a backup ({label}): {e}");
                None
            }
        }
    }

    /// Records `text` as the last version that loaded cleanly (only when it changed).
    pub fn remember_good(&self, text: &str) {
        if text.trim().is_empty() {
            return; // never "good": see EMPTY_FILE
        }
        let path = self.backups.join("last-good.toml");
        if config::read_text(&path).is_ok_and(|old| old == text) {
            return;
        }
        if let Err(e) = atomic_write(&path, text) {
            log_warn!("couldn't update last-good.toml: {e}");
        }
    }

    pub fn last_good(&self) -> Option<String> {
        config::read_text(&self.backups.join("last-good.toml")).ok()
    }

    /// Deletes temp files left by saves that were cut off (the program ended mid-save). Only
    /// ones a minute old, so a save going on elsewhere is never disturbed.
    pub fn clear_leftover_temp_files(&self) {
        let folders = [self.folder().to_path_buf(), self.backups.clone(), self.backups.join("recent"), self.backups.join("daily"), self.backups.join("pinned")];
        for folder in folders {
            for entry in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                if !name.ends_with(".tmp") || !(name.contains(".toml.") || name.contains(".exe.")) {
                    continue;
                }
                let old = entry.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().map_or(true, |age| age >= LEFTOVER_AGE));
                if old && std::fs::remove_file(entry.path()).is_ok() {
                    log_info!("removed a leftover temp file, {}", entry.path().display());
                }
            }
        }
    }

    /// Puts dock.toml back if it's gone (deleted, or lost by a sync app): from last-good, or else
    /// the newest backup that reads cleanly. Returns where it came from; None if it isn't gone
    /// or there's nothing to put back. (To start over on purpose, delete the whole dock folder.)
    pub fn restore_missing(&self) -> Option<PathBuf> {
        if !matches!(self.config.try_exists(), Ok(false)) {
            return None;
        }
        let last_good = self.backups.join("last-good.toml");
        let (from, text) = std::iter::once(last_good).chain(self.backups().into_iter().map(|b| b.path)).find_map(|path| {
            let text = config::read_text(&path).ok()?;
            config::parse(&text).ok().filter(|parsed| !parsed.read_only)?;
            Some((path, text))
        })?;
        let _lock = lock_folder(self.folder()).ok()?;
        if !matches!(self.config.try_exists(), Ok(false)) {
            return None; // it came back meanwhile
        }
        match atomic_write(&self.config, &text) {
            Ok(()) => {
                log_warn!("dock.toml was missing; put back {}", from.display());
                Some(from)
            }
            Err(e) => {
                log_warn!("dock.toml is missing and couldn't be put back: {e}");
                None
            }
        }
    }

    fn backup_current(&self) -> std::io::Result<()> {
        let Ok(current) = std::fs::read(&self.config) else { return Ok(()) };
        let (date, time) = timestamp();
        let recent = self.backups.join("recent");
        let newest = newest_modified(&recent);
        // A backup dated in the future (the clock was wrong, then fixed) counts as old.
        if newest.is_none_or(|t| t.elapsed().map_or(true, |age| age >= RECENT_EVERY)) {
            atomic_write_bytes(&recent.join(format!("{time}.toml")), &current)?;
            prune(&recent, RECENT_KEEP);
        }
        let daily = self.backups.join("daily");
        let today = daily.join(format!("{date}.toml"));
        if !today.exists() {
            atomic_write_bytes(&today, &current)?;
            prune(&daily, DAILY_KEEP);
        }
        Ok(())
    }
}

/// An edit's result with the file's own line ends: one written with Windows' (`\r\n`), as Notepad
/// writes it, stays so (the TOML editor writes new lines with `\n` alone).
fn same_line_ends(before: &str, after: String) -> String {
    if before.contains("\r\n") { after.replace("\r\n", "\n").replace('\n', "\r\n") } else { after }
}

/// What's said about an empty dock.toml.
pub const EMPTY_FILE: &str = "The file is empty. The dock never saves it that way, so something else emptied it.";

/// Opens dock.toml at startup: migrates older formats (after a pinned backup), records
/// last-good, and falls back to last-good if the file can't be read.
pub fn open(store: &Store) -> Opened {
    store.clear_leftover_temp_files();
    let path = store.config_path().to_path_buf();
    let text = read_patiently(&path);
    // Held or locked, not damaged: read again as soon as it can be. Text the dock can't read is
    // damage, said once, like an error in the file.
    let unreadable = text.as_ref().is_err_and(|e| e.kind() != std::io::ErrorKind::InvalidData);
    let result = text.as_deref().map_err(|e| e.to_string()).and_then(|t| {
        // The dock never saves an empty file: one is damage (a sync app or an editor that
        // crashed), not a dock with no items.
        if t.trim().is_empty() {
            return Err(EMPTY_FILE.to_string());
        }
        config::parse(t).map(|p| (t.to_string(), p))
    });
    match result {
        Ok((text, parsed)) => {
            for warning in &parsed.warnings {
                log_warn!("dock.toml: {warning}");
            }
            if parsed.read_only {
                let notice = format!(
                    "{} was saved by a newer version of Desktop Dock.\n\nIt's being used as it is, and won't be saved over.",
                    path.display()
                );
                return Opened { parsed, mode: Mode::ReadOnly, notice: Some(notice), unreadable };
            }
            let (text, parsed) = if parsed.needs_migration { migrate(store, text, parsed) } else { (text, parsed) };
            store.remember_good(&text);
            Opened { parsed, mode: Mode::Normal, notice: None, unreadable }
        }
        Err(error) => {
            log_error!("dock.toml can't be read: {error}");
            let what = if unreadable {
                format!("Your dock (dock.toml) couldn't be opened:\n\n{error}\n\n")
            } else {
                format!("dock.toml has an error:\n\n{error}\n\n")
            };
            let then = if unreadable {
                "The dock keeps trying, and switches to it as soon as it can be read."
            } else {
                "Fix the file and save it (the dock picks it up), or right-click the dock > Restore last working version."
            };
            match store.last_good().and_then(|good| config::parse(&good).ok()) {
                Some(parsed) => Opened {
                    parsed,
                    mode: Mode::SafeStart,
                    notice: Some(format!("{what}Running your last working setup meanwhile. {then}")),
                    unreadable,
                },
                None => Opened {
                    parsed: Parsed::default(),
                    mode: Mode::SafeStart,
                    notice: Some(format!("{what}There's no earlier working version, so the dock is empty meanwhile. {then}")),
                    unreadable,
                },
            }
        }
    }
}

/// Reads dock.toml, waiting a little if something holds it (antivirus, a sync app): 5 tries in
/// about 2 s. A missing file, or one that isn't text, isn't waited for.
fn read_patiently(path: &Path) -> std::io::Result<String> {
    let mut tries = 1;
    loop {
        match config::read_text(path) {
            Err(e) if tries < 5 && !matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidData) => {
                log_warn!("couldn't read {} just now ({e}); trying again", path.display());
                tries += 1;
                std::thread::sleep(Duration::from_millis(400));
            }
            result => return result,
        }
    }
}

fn migrate(store: &Store, text: String, parsed: Parsed) -> (String, Parsed) {
    // Once: if the upgrade can't be saved (a read-only folder), every start would pin again.
    let label = format!("before-v{}-migration", config::CURRENT_VERSION);
    let pinned = if store.backups().iter().any(|b| b.kind == BackupKind::Pinned && b.label == label) { None } else { store.pin(&label) };
    let upgraded = config::migrate(&text).and_then(|new| store.replace(&new).map(|()| new));
    match upgraded.and_then(|new| config::parse(&new).map(|p| (new, p))) {
        Ok((new, new_parsed)) => {
            log_info!(
                "upgraded dock.toml to format version {} (backup: {})",
                config::CURRENT_VERSION,
                pinned.map(|p| p.display().to_string()).unwrap_or_else(|| "none".into())
            );
            (new, new_parsed)
        }
        Err(e) => {
            log_warn!("couldn't upgrade dock.toml ({e}); using it as it is");
            (text, parsed)
        }
    }
}

/// Writes `text` to `path` atomically: temp file in the same folder (named for this process and
/// this write, so two writers never share one), flushed to disk, then swapped in with
/// write-through. Retries briefly if something (antivirus, an editor) holds it.
pub fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    atomic_write_bytes(path, text.as_bytes())
}

pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(format!(".{}-{}.tmp", std::process::id(), TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)));
    let tmp = PathBuf::from(tmp_name);
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?; // FlushFileBuffers
    }
    let (from, to) = (HSTRING::from(tmp.as_os_str()), HSTRING::from(path.as_os_str()));
    let mut last_error = None;
    for attempt in 0..10 {
        match unsafe { MoveFileExW(&from, &to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_error = Some(e);
                std::thread::sleep(Duration::from_millis(20 * (attempt + 1)));
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    // Windows' words end with a full stop; whoever says this ends the sentence.
    let why = last_error.map(|e| e.message().trim_end_matches('.').to_string()).unwrap_or_default();
    Err(std::io::Error::other(why))
}

/// Deletes the oldest `.toml` files in `dir` beyond `keep` (names sort by time).
fn prune(dir: &Path, keep: usize) {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep);
    for old in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
}

fn newest_modified(dir: &Path) -> Option<SystemTime> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .max()
}

/// ("2026-10-01", "2026-10-01_131254") in local time; names sort chronologically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupKind {
    Recent,
    Daily,
    Pinned,
}

/// One saved copy of dock.toml.
#[derive(Debug, Clone)]
pub struct Backup {
    pub path: PathBuf,
    pub kind: BackupKind,
    /// A pinned copy's name ("snapshot", "before-import", ...); empty for the others.
    pub label: String,
    /// When it was saved, as its name says ("2026-10-02 11:12:07").
    pub when: String,
    pub modified: Option<std::time::SystemTime>,
}

/// "before-install-2026-10-02_11-12-07" → ("before-install", "2026-10-02 11:12:07");
/// "2026-10-02_002908" → ("", "2026-10-02 00:29:08"); "2026-10-01" → ("", "2026-10-01").
fn split_backup_name(stem: &str) -> (String, String) {
    let bytes = stem.as_bytes();
    let is_date = |i: usize| {
        i + 10 <= bytes.len()
            && bytes[i..i + 10].iter().enumerate().all(|(k, b)| if k == 4 || k == 7 { *b == b'-' } else { b.is_ascii_digit() })
    };
    let Some(at) = (0..bytes.len()).find(|&i| is_date(i)) else { return (stem.to_string(), String::new()) };
    let label = stem[..at].trim_end_matches(['-', '_']).to_string();
    let date = &stem[at..at + 10];
    let digits: String = stem[at + 10..].chars().filter(char::is_ascii_digit).collect();
    let when = match digits.len() {
        6 => format!("{date} {}:{}:{}", &digits[..2], &digits[2..4], &digits[4..]),
        4 => format!("{date} {}:{}", &digits[..2], &digits[2..]),
        _ => date.to_string(),
    };
    (label, when)
}

fn timestamp() -> (String, String) {
    let t = unsafe { GetLocalTime() };
    let date = format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay);
    let time = format!("{date}_{:02}{:02}{:02}", t.wHour, t.wMinute, t.wSecond);
    (date, time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_read_as_when_and_what() {
        assert_eq!(split_backup_name("before-install-2026-10-02_11-12-07"), ("before-install".into(), "2026-10-02 11:12:07".into()));
        assert_eq!(split_backup_name("2026-10-02_002908"), (String::new(), "2026-10-02 00:29:08".into()));
        assert_eq!(split_backup_name("2026-10-01"), (String::new(), "2026-10-01".into()));
        assert_eq!(split_backup_name("before-big-change"), ("before-big-change".into(), String::new()));
        assert_eq!(split_backup_name("snapshot-2026-10-02_143000"), ("snapshot".into(), "2026-10-02 14:30:00".into()));
    }

    const GOOD: &str = "version = 1\n[dock]\nicon_size = 53 # mine\n";

    fn temp_home(name: &str) -> Home {
        let dir = std::env::temp_dir().join(format!("dock-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Home::new(dir)
    }

    fn files_in(dir: &Path) -> usize {
        std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
    }

    /// How many edits each child process makes in the two-process test.
    const ROUNDS: u32 = 25;

    /// Not a test on its own: run in child copies of the test program by the tests below.
    #[test]
    #[ignore]
    fn child_saver() {
        let Ok(dir) = std::env::var("DOCK_TEST_SAVER_DIR") else { return };
        let home = Home::new(PathBuf::from(dir));
        if std::env::var("DOCK_TEST_SAVER_ABANDON").is_ok() {
            // Ends while holding the lock, as a crash or Task Manager would.
            std::mem::forget(lock_folder(&home.dir).unwrap());
            std::process::exit(0);
        }
        let store = Store::new(&home);
        for _ in 0..ROUNDS {
            store
                .edit(|doc| {
                    let n: u32 = doc["item"][0]["name"].as_str().unwrap_or("0").parse().unwrap_or(0);
                    doc["item"][0]["name"] = toml_edit::value((n + 1).to_string());
                    Ok(())
                })
                .unwrap();
        }
    }

    fn run_child(dir: &Path, abandon: bool) -> std::process::Child {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["store::tests::child_saver", "--exact", "--ignored", "--test-threads=1"]).env("DOCK_TEST_SAVER_DIR", dir);
        if abandon {
            command.env("DOCK_TEST_SAVER_ABANDON", "1");
        }
        command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap()
    }

    #[test]
    fn two_processes_saving_at_once_lose_nothing() {
        let home = temp_home("two-processes");
        std::fs::write(home.config(), "version = 1\n\n[[item]]\nname = \"0\"\ntarget = 'x'\n").unwrap();
        let children: Vec<_> = (0..3).map(|_| run_child(&home.dir, false)).collect();
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        let parsed = config::parse(&std::fs::read_to_string(home.config()).unwrap()).unwrap();
        assert_eq!(parsed.config.items[0].name, (3 * ROUNDS).to_string(), "every edit from every process kept");
        assert!(!std::fs::read_dir(&home.dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tmp")), "no temp files left");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_lock_left_by_a_program_that_ended_is_taken_over() {
        let home = temp_home("abandoned");
        assert!(run_child(&home.dir, true).wait().unwrap().success());
        let started = std::time::Instant::now();
        drop(lock_folder(&home.dir).unwrap());
        assert!(started.elapsed() < Duration::from_secs(1), "no wait for a program that's gone");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_save_waits_for_another_window_then_says_its_busy() {
        let home = temp_home("busy");
        // Another "process" holds the folder's lock (a thread taking only the named mutex).
        let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        let dir = home.dir.clone();
        let holder = std::thread::spawn(move || {
            let lock = NamedLock::take(&dir, SAVE_WAIT).unwrap();
            held.0.send(()).unwrap();
            let _ = release.1.recv();
            drop(lock);
        });
        held.1.recv().unwrap();
        let error = NamedLock::take(&home.dir, Duration::from_millis(100)).err().unwrap();
        assert!(error.contains("busy saving"), "{error}");
        assert!(NamedLock::take(&temp_home("busy-other").dir, Duration::from_millis(100)).is_ok(), "another dock folder isn't held up");
        release.0.send(()).unwrap();
        holder.join().unwrap();
        assert!(NamedLock::take(&home.dir, Duration::from_millis(100)).is_ok(), "free again once released");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn the_lock_name_ignores_case_and_slashes() {
        assert_eq!(mutex_name(Path::new(r"C:\Users\alex\Dock")), mutex_name(Path::new("c:/users/ALEX/dock/")));
        assert_ne!(mutex_name(Path::new(r"C:\Users\alex\Dock")), mutex_name(Path::new(r"C:\Users\alex\Dock2")));
        assert!(mutex_name(Path::new(r"C:\x")).starts_with(r"Local\DesktopDock.Config."));
    }

    #[test]
    fn leftover_temp_files_are_cleared_once_theyre_old() {
        let home = temp_home("leftovers");
        let store = Store::new(&home);
        let old = home.dir.join("dock.toml.4242-7.tmp");
        let fresh = home.dir.join("dock.toml.4242-8.tmp");
        let backup = home.backups().join("recent").join("12-00-00.toml.4242-9.tmp");
        let other = home.dir.join("notes.tmp");
        std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
        for path in [&old, &fresh, &backup, &other] {
            std::fs::write(path, "x").unwrap();
        }
        let long_ago = SystemTime::now() - Duration::from_secs(600);
        for path in [&old, &backup, &other] {
            std::fs::File::options().write(true).open(path).unwrap().set_modified(long_ago).unwrap();
        }
        store.clear_leftover_temp_files();
        assert!(!old.exists() && !backup.exists(), "old leftovers removed");
        assert!(fresh.exists(), "a save going on now is left alone");
        assert!(other.exists(), "files that aren't ours are left alone");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_missing_dock_comes_back_from_last_good_or_the_newest_backup() {
        let home = temp_home("missing");
        let store = Store::new(&home);
        assert!(store.restore_missing().is_none(), "nothing to put back: a first run");
        std::fs::write(home.config(), "version = 1\n[dock]\nicon_size = 40\n").unwrap();
        store.replace("version = 1\n[dock]\nicon_size = 41\n").unwrap(); // backs up 40
        assert!(store.restore_missing().is_none(), "it isn't gone");
        std::fs::remove_file(home.config()).unwrap();
        assert!(!store.restore_missing().unwrap().ends_with("last-good.toml"), "no last-good yet: the newest backup");
        assert!(std::fs::read_to_string(home.config()).unwrap().contains("icon_size = 40"));
        store.remember_good("version = 1\n[dock]\nicon_size = 42\n");
        std::fs::remove_file(home.config()).unwrap();
        assert!(store.restore_missing().unwrap().ends_with("last-good.toml"));
        assert!(std::fs::read_to_string(home.config()).unwrap().contains("icon_size = 42"));
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_file_held_for_a_moment_is_read_once_its_free() {
        use std::os::windows::fs::OpenOptionsExt;
        let home = temp_home("held");
        std::fs::write(home.config(), "version = 1\n[dock]\nicon_size = 44\n").unwrap();
        // Opened with no sharing, as some antivirus and sync apps do, for about a second.
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(home.config()).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(900));
            drop(held);
        });
        let opened = open(&Store::new(&home));
        release.join().unwrap();
        assert_eq!(opened.mode, Mode::Normal, "{:?}", opened.notice);
        assert_eq!(opened.parsed.config.dock.icon_size, 44.0);
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp_file() {
        let home = temp_home("atomic");
        let path = home.config();
        atomic_write(&path, "a = 1\n").unwrap();
        atomic_write(&path, "a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
        assert_eq!(files_in(&home.dir), 1, "no dock.toml.tmp left behind");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_save_that_fails_says_why_in_words_that_fit_a_sentence() {
        // Callers end the sentence: "Couldn't save your dock: Access is denied." (not "denied..").
        let home = temp_home("readonly");
        let path = home.config();
        atomic_write(&path, "a = 1\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions.clone()).unwrap();
        let why = atomic_write(&path, "a = 2\n").unwrap_err().to_string();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
        let _ = std::fs::remove_dir_all(&home.dir);
        assert!(!why.is_empty() && !why.ends_with('.'), "{why:?}");
    }

    #[test]
    fn a_file_that_doesnt_read_back_is_never_saved() {
        let home = temp_home("verify");
        let store = Store::new(&home);
        store.replace(GOOD).unwrap();
        assert!(store.replace("this is not toml").is_err());
        assert!(store.replace("version = 99").is_err(), "never writes a 'newer' file");
        assert_eq!(std::fs::read_to_string(home.config()).unwrap(), GOOD);
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn edits_apply_to_the_file_on_disk_and_keep_comments() {
        let home = temp_home("edit");
        let store = Store::new(&home);
        store.replace(GOOD).unwrap();
        // A hand edit the dock hasn't seen yet...
        std::fs::write(home.config(), GOOD.replace("53", "60")).unwrap();
        // ...is kept when the dock edits something else.
        store
            .edit(|doc| {
                doc["dock"]["spacing"] = toml_edit::value(12);
                Ok(())
            })
            .unwrap();
        let text = std::fs::read_to_string(home.config()).unwrap();
        assert!(text.contains("icon_size = 60 # mine"), "{text}");
        assert!(text.contains("spacing = 12"));
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_change_can_be_undone_unless_the_file_moved_on() {
        let home = temp_home("revert");
        let store = Store::new(&home);
        store.replace(GOOD).unwrap();
        let ((), change) = store
            .change(|doc| {
                doc["dock"]["spacing"] = toml_edit::value(12);
                Ok(())
            })
            .unwrap();
        assert_eq!(change.before, GOOD);
        store.revert(&change).unwrap();
        assert_eq!(std::fs::read_to_string(home.config()).unwrap(), GOOD, "undone");

        let ((), change) = store
            .change(|doc| {
                doc["dock"]["spacing"] = toml_edit::value(14);
                Ok(())
            })
            .unwrap();
        let hand_edited = change.after.replace("53", "60");
        std::fs::write(home.config(), &hand_edited).unwrap();
        assert!(store.revert(&change).is_err(), "a hand edit since is never thrown away");
        assert_eq!(std::fs::read_to_string(home.config()).unwrap(), hand_edited);
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_file_with_windows_line_ends_keeps_them_through_an_edit_and_its_undo() {
        // As Notepad writes it.
        let home = temp_home("crlf");
        let mine = "version = 1\r\n\r\n[dock]\r\nicon_size = 53 # mine\r\n\r\n[[item]]\r\nname = 'Notepad'\r\ntarget = 'notepad.exe'\r\n";
        std::fs::write(home.config(), mine).unwrap();
        let store = Store::new(&home);
        let ((), change) = store.change(|doc| crate::edit::set_dock(doc, "spacing", 12_i64)).unwrap();
        let edited = std::fs::read_to_string(home.config()).unwrap();
        assert!(edited.contains("spacing = 12\r\n"), "the new line ends as the file's do: {edited:?}");
        assert!(!edited.replace("\r\n", "").contains('\n'), "no bare line ends: {edited:?}");
        store.revert(&change).unwrap();
        assert_eq!(std::fs::read_to_string(home.config()).unwrap(), mine, "undone, byte for byte");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn backups_one_recent_per_minute_and_one_daily() {
        let home = temp_home("backups");
        let store = Store::new(&home);
        store.replace(GOOD).unwrap(); // nothing to back up yet
        store.replace(&GOOD.replace("53", "54")).unwrap();
        store.replace(&GOOD.replace("53", "55")).unwrap();
        assert_eq!(files_in(&home.backups().join("recent")), 1, "at most one a minute");
        assert_eq!(files_in(&home.backups().join("daily")), 1);
        // A minute later (the newest backup made a minute ago), the next change is kept too.
        let recent = home.backups().join("recent");
        for entry in std::fs::read_dir(&recent).unwrap().flatten() {
            let file = std::fs::File::options().write(true).open(entry.path()).unwrap();
            file.set_modified(SystemTime::now() - RECENT_EVERY - Duration::from_secs(1)).unwrap();
        }
        std::thread::sleep(Duration::from_millis(1100)); // its name has the second in it
        store.replace(&GOOD.replace("53", "56")).unwrap();
        assert_eq!(files_in(&recent), 2, "one more a minute later");
        assert_eq!(files_in(&home.backups().join("daily")), 1, "still one a day");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn prune_keeps_the_newest() {
        let home = temp_home("prune");
        let dir = home.dir.join("recent");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..25 {
            std::fs::write(dir.join(format!("2026-10-01_1200{i:02}.toml")), "").unwrap();
        }
        prune(&dir, RECENT_KEEP);
        assert_eq!(files_in(&dir), RECENT_KEEP);
        assert!(!dir.join("2026-10-01_120000.toml").exists(), "oldest removed");
        assert!(dir.join("2026-10-01_120024.toml").exists(), "newest kept");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn an_empty_file_is_damage_not_an_empty_dock() {
        let home = temp_home("empty");
        let store = Store::new(&home);
        store.remember_good(GOOD);
        std::fs::write(home.config(), "  \r\n").unwrap();
        let opened = open(&store);
        assert_eq!(opened.mode, Mode::SafeStart, "the last good dock runs meanwhile");
        assert!(opened.notice.is_some_and(|n| n.contains("empty")));
        store.remember_good("");
        assert_eq!(store.last_good().as_deref(), Some(GOOD), "an empty file never becomes the last good one");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn last_good_only_rewritten_when_it_changes() {
        let home = temp_home("lastgood");
        let store = Store::new(&home);
        store.remember_good(GOOD);
        let path = home.backups().join("last-good.toml");
        let first = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        store.remember_good(GOOD);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), first);
        assert_eq!(store.last_good().as_deref(), Some(GOOD));
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn opening_an_old_file_migrates_it_with_a_pinned_backup() {
        let home = temp_home("migrate");
        std::fs::write(home.config(), "[dock]\nhover_scale = 1.2 # old\n\n[[item]]\nseparator = true\n").unwrap();
        let opened = open(&Store::new(&home));
        assert_eq!(opened.mode, Mode::Normal);
        let text = std::fs::read_to_string(home.config()).unwrap();
        assert!(text.contains("version = 1") && text.contains("zoom_size") && text.contains("kind = \"separator\""), "{text}");
        assert_eq!(files_in(&home.backups().join("pinned")), 1);
        assert!(home.backups().join("last-good.toml").exists());
        // An old file again (say the upgrade couldn't be saved): no second pre-upgrade copy.
        std::fs::write(home.config(), "[dock]\nhover_scale = 1.3\n").unwrap();
        open(&Store::new(&home));
        assert_eq!(files_in(&home.backups().join("pinned")), 1, "the pre-upgrade copy is made once");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_broken_file_runs_from_last_good_and_is_left_alone() {
        let home = temp_home("safestart");
        let store = Store::new(&home);
        std::fs::write(home.config(), GOOD).unwrap();
        assert_eq!(open(&store).mode, Mode::Normal); // records last-good
        std::fs::write(home.config(), "version = 1\n[dock\nbroken").unwrap();
        let opened = open(&store);
        assert_eq!(opened.mode, Mode::SafeStart);
        assert!(opened.notice.is_some());
        assert_eq!(opened.parsed.config.dock.icon_size, 53.0, "from last-good");
        assert!(std::fs::read_to_string(home.config()).unwrap().contains("broken"), "broken file untouched");
        let _ = std::fs::remove_dir_all(&home.dir);
    }

    #[test]
    fn a_broken_file_with_no_last_good_gives_an_empty_dock() {
        let home = temp_home("nothing");
        std::fs::write(home.config(), "[dock\n").unwrap();
        let opened = open(&Store::new(&home));
        assert_eq!(opened.mode, Mode::SafeStart);
        assert!(opened.parsed.config.items.is_empty());
        let _ = std::fs::remove_dir_all(&home.dir);
    }
}
