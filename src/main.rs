// Release builds are a normal Windows app with no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod breaks;
mod chrome;
mod config;
mod dock;
mod drop;
mod edit;
mod heal;
mod home;
mod hotkey;
mod iconpicker;
mod icons;
mod import;
#[cfg(all(test, dev_lab))]
mod inventory_tests;
mod launch;
mod layout;
mod log;
mod motion;
mod pickers;
mod places;
mod popup;
mod removed;
mod render;
mod settings;
mod setup;
mod store;
mod system;
mod target;
mod watchdog;

use std::path::PathBuf;
use std::time::Instant;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_ICONINFORMATION, MB_OK};


fn main() {
    let started = Instant::now();
    // The icon helper (see icons::make_icons): makes icons into the cache, then exits. Before
    // anything else: no log, no lock, no window.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // The launch helper (see launch.rs): opens one thing with Windows' shell, then exits.
    if raw.first().is_some_and(|flag| flag == "--launch") {
        std::process::exit(launch::run_helper(&raw[1..]));
    }
    // The chooser helper (see pickers.rs): shows one of Windows' choosers, writes the choice.
    if raw.len() >= 3 && raw[0] == "--pick" {
        let start = raw.get(3).map(String::as_str);
        std::process::exit(pickers::run_helper(&raw[1], std::path::Path::new(&raw[2]), start));
    }
    // The recycle helper: sends the listed files to the Recycle Bin (see Dock::recycle).
    if let [flag, list] = raw.as_slice() {
        if flag == "--recycle" {
            let paths: Vec<String> = std::fs::read_to_string(list).unwrap_or_default().lines().map(str::to_string).collect();
            std::process::exit(system::recycle(&paths));
        }
    }
    // The settings window and an item's properties window (see settings.rs): each its own
    // process, open only while the window is. No log (the dock writes that one), no lock, no
    // watchdog. `--settings [--item N [--child K]]`, `--properties N [--child K] [--icon]` (K: an
    // item inside the group N; --icon: straight to its icon picker), either with `--home <dir>`.
    if raw.first().is_some_and(|flag| flag == "--settings" || flag == "--properties") {
        let mut explicit_home = None;
        let mut item = None;
        let mut child = None;
        let mut icon = false;
        let mut rest = raw.iter().peekable();
        while let Some(arg) = rest.next() {
            match arg.as_str() {
                "--home" => explicit_home = rest.next().map(PathBuf::from),
                "--item" | "--properties" => item = rest.next_if(|n| n.parse::<usize>().is_ok()).and_then(|n| n.parse().ok()),
                "--child" => child = rest.next().and_then(|n| n.parse::<usize>().ok()),
                "--icon" => icon = true,
                _ => {}
            }
        }
        let spot = item.map(|index| edit::Spot { index, child });
        let open = match (raw[0].as_str(), spot) {
            ("--properties", Some(spot)) if icon => settings::Open::Icon(spot),
            ("--properties", Some(spot)) => settings::Open::Properties(spot),
            _ => settings::Open::Settings(spot),
        };
        std::process::exit(settings::run(home::locate(explicit_home), open));
    }
    if let [flag, list, cache] = raw.as_slice() {
        if flag == "--make-icons" {
            icons::make_icons(std::path::Path::new(list), std::path::Path::new(cache));
            return;
        }
    }
    // What this run is for, before any log or window (see setup.rs): a copy double-clicked in
    // Downloads shows the setup window and leaves nothing beside it; --install and --uninstall.
    let exe = std::env::current_exe().unwrap_or_default();
    let beside = exe.parent().is_some_and(|dir| dir.join("dock.toml").is_file());
    match setup::role(&raw, &exe, setup::install_dir().as_deref(), beside, home::is_dev_build()) {
        setup::Role::Setup => std::process::exit(setup::run_setup_window()),
        setup::Role::Install(options) => std::process::exit(setup::run_install(options)),
        setup::Role::Uninstall { quiet, remove_dock } => std::process::exit(setup::run_uninstall(quiet, remove_dock)),
        setup::Role::Run => {}
    }
    let mut explicit_home: Option<PathBuf> = None;
    let mut snapshot_dir: Option<PathBuf> = None;
    let mut stress_save = false;
    let mut import: Option<Option<PathBuf>> = None;
    let mut quiet = false;
    let mut bench = false;
    let mut churn: Option<u32> = None;
    let mut hover_churn: Option<u32> = None;
    let mut child = false;
    let mut safe_mode = false;
    let mut no_watchdog = false;
    let mut quit = false;
    let all_args: Vec<String> = std::env::args().skip(1).collect();
    let mut args = all_args.iter().cloned().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--child" => child = true,         // the real dock, started by the watchdog
            "--safe-mode" => safe_mode = true, // set by the watchdog after quick crashes
            "--no-watchdog" => no_watchdog = true,
            "--quit" => quit = true, // asks a running dock to exit (for scripts)
            // Started by Windows at sign-in (system::STARTUP_FLAG): runs as usual; if the dock is
            // already running, this copy leaves without opening Dock settings (below).
            "--startup" => {}
            "--home" | "--config" => explicit_home = args.next().map(PathBuf::from),
            "--snapshot" => snapshot_dir = args.next().map(PathBuf::from),
            "--stress-save" => stress_save = true, // test only: saves over and over
            // --import-rocketdock [file.reg]: from the registry, or from a `reg export` file.
            "--import-rocketdock" => {
                let file = args.next_if(|next| !next.starts_with("--")).map(PathBuf::from);
                import = Some(file);
            }
            "--quiet" => quiet = true,
            "--bench" => bench = true, // frame cost, nothing shown
            "--churn" => churn = Some(args.next().and_then(|n| n.parse().ok()).unwrap_or(20)), // memory check: icons
            "--hover-churn" => hover_churn = Some(args.next().and_then(|n| n.parse().ok()).unwrap_or(30)), // memory check: pointing
            _ => {}
        }
    }

    if quit {
        std::process::exit(if system::quit_running_dock() { 0 } else { 1 });
    }
    let home = home::locate(explicit_home);
    // Test-only flags work only in development builds: on an installed copy, --stress-save would
    // overwrite the real dock.toml forever.
    if !home::is_dev_build() {
        (snapshot_dir, bench, churn, hover_churn, stress_save) = (None, false, None, None, false);
    }
    let tool_mode = snapshot_dir.is_some() || bench || churn.is_some() || hover_churn.is_some() || stress_save || import.is_some();
    if snapshot_dir.is_none() {
        log::init(&home.logs());
        log::install_panic_hook();
        log::install_crash_filter();
    }

    // Normally this process is just the watchdog: it holds the single-instance lock and runs the
    // real dock as a child, restarting it after a crash.
    if !child && !no_watchdog && !tool_mode {
        if !system::claim_single_instance() {
            // Already running: starting it again (from the Start menu, say) opens its settings.
            if raw.is_empty() {
                std::process::exit(settings::run(home, settings::Open::Settings(None)));
            }
            return;
        }
        if let Some(dir) = exe.parent() {
            setup::remove_old_copies(dir); // versions an update renamed aside
        }
        log_info!("watchdog starting; dock folder {}", home.dir.display());
        watchdog::run(&all_args);
    }
    log_info!("starting{}; dock folder {}", if safe_mode { " in safe mode" } else { "" }, home.dir.display());

    unsafe {
        // Per-monitor DPI awareness: Windows never stretches (blurs) the dock.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }

    let path = home.config();
    let mut first_run = false;
    // No dock.toml: put the last working version back if there is one (it was deleted, or a sync
    // app lost it); a dock folder without backups is a first run.
    if matches!(path.try_exists(), Ok(false)) && store::Store::new(&home).restore_missing().is_none() {
        match store::atomic_write(&path, &places::first_dock()) {
            Ok(()) => {
                log_info!("first run: created {}", path.display());
                first_run = true;
            }
            Err(e) => fatal(&format!("Couldn't create {}: {e}", path.display())),
        }
    }
    if bench {
        let config = config::load(&path).unwrap_or_else(|message| fatal(&message)).config;
        match dock::bench(config, home) {
            Ok(result) => log_info!("{result}"),
            Err(e) => log_error!("bench failed: {e}"),
        }
        return;
    }
    if let Some(rounds) = hover_churn {
        let config = config::load(&path).unwrap_or_else(|message| fatal(&message)).config;
        match dock::hover_churn(config, home, rounds) {
            Ok(result) => log_info!("{result}"),
            Err(e) => log_error!("hover churn failed: {e}"),
        }
        return;
    }
    if let Some(rounds) = churn {
        let config = config::load(&path).unwrap_or_else(|message| fatal(&message)).config;
        match dock::churn(config, home, rounds) {
            Ok(result) => log_info!("{result}"),
            Err(e) => log_error!("churn failed: {e}"),
        }
        return;
    }
    if let Some(dir) = snapshot_dir {
        let config = config::load(&path).unwrap_or_else(|message| fatal(&message)).config;
        if let Err(e) = dock::snapshot(config, home, &dir) {
            fatal(&format!("Snapshot failed: {e}"));
        }
        return;
    }
    let store = store::Store::new(&home);
    if stress_save {
        stress_saves(&store);
    }
    if let Some(file) = import {
        import_rocketdock(&store, file.as_deref(), quiet);
        return;
    }
    // Under the watchdog, it already holds the lock; the dock holds it too, so it stays taken if
    // the watchdog is ended on its own.
    if child {
        system::hold_single_instance();
    } else if !system::claim_single_instance() {
        log_info!("another copy is already running; exiting");
        return;
    }
    let opened = store::open(&store);
    if let Err(e) = dock::run(opened, store, home, started, safe_mode, first_run) {
        log_error!("stopped: {e}");
        fatal(&format!("Desktop Dock stopped: {e}"));
    }
    log_info!("exited normally");
}

/// Replaces dock.toml with your RocketDock setup (after a pinned backup of the current file).
/// A running dock picks the new file up by itself.
fn import_rocketdock(store: &store::Store, file: Option<&std::path::Path>, quiet: bool) {
    let source = match file {
        Some(path) => import::RocketDock::from_reg_file(path),
        None => import::RocketDock::from_registry(),
    };
    let rocketdock = source.unwrap_or_else(|e| fatal(&e));
    let imported = import::convert(&rocketdock, |p| std::path::Path::new(p).exists(), icons::icon_count);
    if let Some(pinned) = store.pin("before-import") {
        log_info!("kept the previous dock.toml as {}", pinned.display());
    }
    if let Err(e) = store.replace(&imported.text) {
        fatal(&format!("Import failed: {e}"));
    }
    log_info!("imported {} RocketDock entries", imported.items);
    for line in &imported.report {
        log_info!("import: {line}");
    }
    if !quiet {
        let mut summary = format!("Imported {} RocketDock entries into {}.", imported.items, store.config_path().display());
        if !imported.report.is_empty() {
            summary.push_str("\n\nWorth knowing:\n");
            for line in &imported.report {
                summary.push_str(&format!("• {line}\n"));
            }
        }
        system::message_box(&summary, "Desktop Dock", MB_ICONINFORMATION | MB_OK);
    }
}

/// Test only: saves two different versions back and forth forever, so a script can kill the
/// process at random moments and check dock.toml is never left broken.
fn stress_saves(store: &store::Store) -> ! {
    let a = "version = 1\n[dock]\nicon_size = 53\n# version A\n";
    let b = "version = 1\n[dock]\nicon_size = 54\n# version B, a bit longer so sizes differ\n";
    loop {
        let _ = store.replace(a);
        let _ = store.replace(b);
    }
}

fn fatal(message: &str) -> ! {
    system::message_box(message, "Desktop Dock", MB_ICONERROR | MB_OK);
    std::process::exit(1)
}
