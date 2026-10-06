# dock.toml format (version 1) and the dock folder

Every setting here can also be changed in Dock settings; the dock saves this file as you go and picks up hand edits when you save. [dock.example.toml](../dock.example.toml) shows every setting with its default.

The file is UTF-8 text. A copy saved as UTF-16 (Notepad's "Unicode", or Windows PowerShell's `>`) is read too, and the dock's next save makes it UTF-8 again; any other encoding (an older editor's "ANSI") isn't read, and the dock says so once.

## Example
```toml
version = 1

[dock]
icon_size = 53             # pixels at 100% scaling
zoom_size = 64             # how big a hovered icon gets
zoom_ms = 120              # zoom speed
spacing = 10
popup_delay_ms = 350
hide_delay_ms = 250
slide_in_ms = 250
slide_out_ms = 250
show_labels = true
label_size = 12.5          # name text size, logical pixels (8-24)
background_opacity = 100   # how solid the panel, its edge and shadow are, in % (0 = icons only)
icon_opacity = 100         # how solid icons are, in % (10-100); the one under the pointer is always solid
launch_effect = "bounce"   # or "none", or "glow" (the clicked icon glows up for a moment)
look = "light"             # the panel: "light" (CrystalXP-style) or "dark"
hover_glow = "none"        # a glow behind the icon you point at: "none", "accent" (Windows' accent colour) or "icon" (each icon's own colour)
monitor = "main"           # the screen the dock is on: "main" (Windows' main screen), or a screen's id from Settings > Screen; one that isn't plugged in gives the main screen
hotkey = ""                # a keyboard shortcut that brings the dock out (and sends it back), like "Ctrl+Shift+D": Ctrl and/or Alt, Shift if you like, and one key; "" for none (the default)
locked = false             # true: no dragging icons around, off, or onto the dock (right-click > Lock icons)

[[item]]
name = "Chrome"
target = 'C:\Program Files\Google\Chrome\Application\chrome.exe'

[[item]]
name = "Pictures"
target = '%USERPROFILE%\Pictures'
icon = 'C:\Windows\System32\imageres.dll,-113'   # icon inside a file: "file,-resource id"

[[item]]
name = "Music Studio 2026"
target = 'C:\Program Files\Example\Music Studio 2026\studio.exe'
start_in = '%USERPROFILE%\Documents\Music Studio'
run = "maximized"          # normal (default) | minimized | maximized

[[item]]
kind = "separator"

[[item]]
name = "Calculator"
app_id = 'Microsoft.WindowsCalculator_8wekyb3d8bbwe!App'   # a Store app

[[item]]
name = "Tools"
kind = "group"             # no icon: its tile shows the first 4 items
  [[item.items]]
  name = "Notepad"
  target = 'C:\Windows\System32\notepad.exe'
  [[item.items]]
  name = "Paint"
  target = 'C:\Windows\System32\mspaint.exe'

[[item]]
name = "Recycle Bin"
kind = "recycle-bin"       # uses Windows' own empty/full icons
```

## Item fields
| Field | Meaning | Default |
|---|---|---|
| `kind` | `app`, `separator`, `group` or `recycle-bin` | `app` |
| `name` | Label shown on hover | — |
| `target` | What to open: program, file, folder, URL or shell location (`::{CLSID}`, `shell:…`) | — |
| `app_id` | Store/packaged app identity (`PackageFamilyName!AppId`), which survives app updates | — |
| `args` | Command-line arguments | none |
| `start_in` | Working folder. If missing or gone, the target's own folder is used | target's folder |
| `icon` | Image file (PNG/ICO/JPG) or `file,-id` / `file,index` icon inside a file | target's own icon |
| `run` | Window state on launch | `normal` |
| `admin` | Run as administrator (Windows asks for permission) | `false` |
| `items` | Child items, for `kind = "group"` only (one level deep) | — |

**Paths:** `%VARS%` like `%USERPROFILE%` are expanded. Relative paths resolve against the dock folder (e.g. `shortcuts\Word.lnk`).

## Rules when reading
- **Unknown keys** (typos, future settings): logged as warnings with line numbers, kept in the file, otherwise ignored.
- **Out-of-range values:** clamped, with a warning.
- **A file from a newer version** (`version` higher than the dock knows): the dock runs from it read-only and never saves over it. This protects your settings if you roll back to an older dock.
- **A file that can't be read:** the dock runs from `backups\last-good.toml` and shows a notice. The broken file is never overwritten: fix it and save, or restore a version in Dock settings > Versions.

## Older files (v0 → v1)
A file with no `version` line is v0 (the earliest format). On first load:
1. A pinned backup is made: `backups\pinned\before-v1-migration-<time>.toml`.
2. `hover_scale` becomes `zoom_size = round(icon_size × hover_scale)`.
3. `separator = true` becomes `kind = "separator"`.
4. `version = 1` is added and the file is written once (comments kept). The change is logged.

Old names are always accepted when reading; only the newest format is written.

## Saving
- Edits change only what changed (via toml_edit), so your comments and layout stay as they were.
- **Each save:**
  1. write `dock.toml.tmp` in the same folder;
  2. flush it to disk;
  3. re-read and check it;
  4. replace `dock.toml` in one atomic step.

  Interrupting a save at any point leaves either the old file or the new one, never a broken one.
- The dock's own saves don't trigger its auto-reload. If you edited the file by hand since the dock loaded it, the dock reloads your version first, then applies its change on top.

## The dock folder
Installed: `%LOCALAPPDATA%\Programs\Desktop Dock\`. A copy with a `dock.toml` beside it runs from its own folder (portable). During development, the same layout lives in `<project>\dev-home\`, which git ignores. Rule: **the dock folder is wherever the `dock.toml` in use lives.**

```
Desktop Dock\
  desktop-dock.exe
  dock.toml
  backups\
    recent\        last 20 versions, at most one per minute
    daily\         one per day, last 7 days
    pinned\        snapshots, and copies made before imports, restores and upgrades (kept until you delete them)
    last-good.toml the last version that loaded cleanly
  cache\icons\     icons pre-scaled to the exact size drawn; safe to delete
  shortcuts\       private copies of special shortcuts
  removed.toml     Recently removed (the last 20)
  logs\            dock.log, dock.old.log (256 KB each)
```

Getting at it: Dock settings > Files shows its sizes and has Open dock folder, Edit dock.toml, Reload the dock and Clear icon cache.

Custom icons stay where you keep them (the icon picker looks in `Pictures\Icons`). If one goes missing, the dock falls back to its cached copy, then to the program's own icon.
