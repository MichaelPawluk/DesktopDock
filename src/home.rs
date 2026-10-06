//! The dock folder ("home"): wherever the `dock.toml` in use lives. Backups, the icon cache,
//! private shortcut copies and logs sit next to it. One rule covers the installed dock,
//! a portable copy, and development builds (which use `<project>\dev-home`).

use crate::target::{self, Class};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Home {
    pub dir: PathBuf,
}

impl Home {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn config(&self) -> PathBuf {
        self.dir.join("dock.toml")
    }

    pub fn backups(&self) -> PathBuf {
        self.dir.join("backups")
    }

    pub fn icon_cache(&self) -> PathBuf {
        self.dir.join("cache").join("icons")
    }

    pub fn logs(&self) -> PathBuf {
        self.dir.join("logs")
    }

    /// Expands `%VARS%` and resolves paths relative to the dock folder (see `target::Class`).
    /// Shell locations and links (`https:`, `ms-settings:`, `mailto:`…) are returned as they
    /// are, and so is a bare name the dock folder doesn't have, for Windows to find.
    pub fn resolve(&self, path: &str) -> String {
        let expanded = expand_vars(path.trim(), |name| std::env::var(name).ok());
        match target::classify(&expanded) {
            Class::Relative => self.dir.join(&expanded).to_string_lossy().into_owned(),
            Class::Bare if self.dir.join(&expanded).exists() => self.dir.join(&expanded).to_string_lossy().into_owned(),
            // A deliberate fault for mutation testing (breaks.rs): links and bare names as files.
            Class::Uri | Class::Bare if crate::breaks::broken("target.old-resolve") => self.dir.join(&expanded).to_string_lossy().into_owned(),
            _ => expanded,
        }
    }

    /// The file or folder a target names, resolved: None for shell locations, links, and names
    /// Windows looks up itself. These are what the self-healing pass and "not found" check.
    pub fn resolve_file(&self, path: &str) -> Option<String> {
        let resolved = self.resolve(path);
        (target::classify(&resolved) == Class::Absolute).then_some(resolved)
    }

    /// Where a target's icon comes from: as `resolve`, except that a bare name the dock folder
    /// doesn't have is the program Windows would start for it (App Paths, then the search path).
    pub fn resolve_for_icon(&self, target: &str) -> String {
        let resolved = self.resolve(target);
        if target::classify(&resolved) == Class::Bare {
            if let Some(found) = crate::system::find_program(&resolved) {
                return found;
            }
        }
        resolved
    }
}

/// Finds the dock folder. Order:
/// 1. `--home <dir>` (or the folder of `--config <file>`);
/// 2. the executable's folder, if it holds a `dock.toml` (installed or portable);
/// 3. for builds inside a Cargo project (`target\debug`, `target\release`): `<project>\dev-home`;
/// 4. otherwise the executable's folder (first run creates `dock.toml` there).
pub fn locate(explicit: Option<PathBuf>) -> Home {
    if let Some(path) = explicit {
        let dir = if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("toml")) {
            path.parent().map(Path::to_path_buf).unwrap_or(path)
        } else {
            path
        };
        return Home::new(dir);
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    if exe_dir.join("dock.toml").is_file() {
        return Home::new(exe_dir);
    }
    if let Some(project) = cargo_project_of(&exe_dir) {
        return Home::new(project.join("dev-home"));
    }
    Home::new(exe_dir)
}

/// True for a development build: an exe in `<project>\target\<profile>` beside a Cargo.toml.
/// Test-only messages and command-line flags work only there, so other programs can't drive or
/// crash an installed dock, and a stray `--stress-save` can't overwrite someone's dock.
pub fn is_dev_build() -> bool {
    static DEV: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DEV.get_or_init(|| std::env::current_exe().ok().and_then(|exe| exe.parent().and_then(cargo_project_of)).is_some())
}

/// If `dir` is `<project>\target\<profile>`, returns `<project>`.
fn cargo_project_of(dir: &Path) -> Option<PathBuf> {
    let target = dir.parent()?;
    if !target.file_name()?.eq_ignore_ascii_case("target") {
        return None;
    }
    let project = target.parent()?;
    project.join("Cargo.toml").is_file().then(|| project.to_path_buf())
}

/// Replaces `%NAME%` with the variable's value; unknown names are left as they are.
pub fn expand_vars(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 && !after[..end].contains(['\\', '/', ' ']) => {
                let name = &after[..end];
                match lookup(name) {
                    Some(value) => out.push_str(&value),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> Option<String> {
        match name {
            "USERPROFILE" => Some(r"C:\Users\alex".into()),
            _ => None,
        }
    }

    #[test]
    fn expands_known_variables() {
        assert_eq!(expand_vars(r"%USERPROFILE%\Pictures", vars), r"C:\Users\alex\Pictures");
    }

    #[test]
    fn leaves_unknown_variables_and_lone_percent_signs() {
        assert_eq!(expand_vars("%NOPE%\\x", vars), "%NOPE%\\x");
        assert_eq!(expand_vars("100% done", vars), "100% done");
        assert_eq!(expand_vars("%", vars), "%");
    }

    #[test]
    fn relative_paths_resolve_against_the_dock_folder() {
        let home = Home::new(PathBuf::from(r"C:\Dock"));
        assert_eq!(home.resolve(r"shortcuts\Word.lnk"), r"C:\Dock\shortcuts\Word.lnk");
        assert_eq!(home.resolve(r"C:\Apps\x.exe"), r"C:\Apps\x.exe");
    }

    #[test]
    fn shell_locations_and_urls_are_untouched() {
        let home = Home::new(PathBuf::from(r"C:\Dock"));
        for s in [
            "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
            "shell:RecycleBinFolder",
            "https://127.0.0.1:8384",
            "ms-settings:display",
            "mailto:alex@example.com",
            "steam://rungameid/1",
        ] {
            assert_eq!(home.resolve(s), s);
            assert_eq!(home.resolve_file(s), None, "{s} isn't a file");
        }
    }

    #[test]
    fn a_bare_name_is_the_dock_folders_file_if_it_has_one_otherwise_windows_finds_it() {
        let dir = std::env::temp_dir().join(format!("dock-home-bare-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let home = Home::new(dir.clone());
        // Not in the dock folder: left for Windows (its PATH, App Paths), and not a file to check.
        assert_eq!(home.resolve("notepad.exe"), "notepad.exe");
        assert_eq!(home.resolve_file("notepad.exe"), None);
        // In the dock folder (a portable dock's own tool): that file.
        std::fs::write(dir.join("tool.exe"), b"").unwrap();
        let beside = dir.join("tool.exe").to_string_lossy().into_owned();
        assert_eq!(home.resolve("tool.exe"), beside);
        assert_eq!(home.resolve_file("tool.exe").as_deref(), Some(beside.as_str()));
        // Paths are files to check, wherever they are.
        assert_eq!(home.resolve_file(r"C:\Apps\x.exe").as_deref(), Some(r"C:\Apps\x.exe"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_bare_names_icon_comes_from_the_program_windows_would_start() {
        let home = Home::new(std::env::temp_dir().join(format!("dock-home-icon-{}", std::process::id())));
        for name in ["notepad.exe", "cmd.exe"] {
            let found = home.resolve_for_icon(name);
            assert!(found.to_lowercase().ends_with(&format!(r"\{name}")) && Path::new(&found).is_file(), "{name}: {found}");
        }
        // Windows wouldn't find it either: left as it is (a placeholder icon, as before).
        assert_eq!(home.resolve_for_icon("no-such-program-anywhere.exe"), "no-such-program-anywhere.exe");
        // Links and paths are as resolve() has them.
        assert_eq!(home.resolve_for_icon("ms-settings:display"), "ms-settings:display");
        assert_eq!(home.resolve_for_icon(r"C:\Apps\x.exe"), r"C:\Apps\x.exe");
    }

    #[test]
    fn cargo_builds_use_dev_home() {
        let project = std::env::temp_dir().join(format!("dock-home-test-{}", std::process::id()));
        let release = project.join("target").join("release");
        std::fs::create_dir_all(&release).unwrap();
        std::fs::write(project.join("Cargo.toml"), "").unwrap();
        assert_eq!(cargo_project_of(&release), Some(project.clone()));
        assert_eq!(cargo_project_of(&project), None);
        let _ = std::fs::remove_dir_all(&project);
    }
}
