# Desktop Dock

A lightweight dock for Windows 11. It hides at the top edge of your screen and slides down when you rest the pointer there: your programs, folders and files, one click away. Inspired by the original RocketDock and its creators at Punk Labs ♥

![The dock, out at the top of the screen](docs/images/dock.png)

**[Download the latest release](https://github.com/MichaelPawluk/desktop-dock/releases/latest)** · Windows 11, 64-bit · free and open source (MIT) · use at your own risk

## What it does

- **Out of the way until you want it:** rest the pointer at the top edge and it slides down; icons zoom as you point at them and show their names. It stays hidden over fullscreen games and videos.
- **Drag things on:** files, folders, programs and links from Explorer, the Start menu or a browser. Drop files on a program to open them with it, or on the Recycle Bin to recycle them.
- **Groups:** keep related things together in one icon that opens a pop-up.
- **Windows' own places:** This PC, Downloads, Control Panel, Settings, Show desktop and more.
- **Hard to lose things:** Undo after every change, a list of what you removed recently, and automatic versions of your dock to go back to.
- **Fixes itself:** when an update moves a program (Photoshop 2026 becoming 2027, say), the dock finds it again.
- **Coming from RocketDock:** import your old dock, icons and all.
- **Light or dark:** follows Windows, with an optional glow in your accent colour.
- **Light on your PC:** about 9 MB of memory, no network access, no telemetry.

<p align="center">
  <img src="docs/images/group.png" alt="A group's pop-up" width="49%">
  <img src="docs/images/menu.png" alt="The right-click menu" width="49%">
</p>
<p align="center">
  <img src="docs/images/settings-light.png" alt="Dock settings, light" width="49%">
  <img src="docs/images/settings-dark.png" alt="Dock settings, dark" width="49%">
</p>

## Install

**Needs** Windows 11, 64-bit (x64). Nothing else to install, and no administrator rights.

1. Download `DesktopDock-<version>.zip` from [Releases](https://github.com/MichaelPawluk/desktop-dock/releases/latest) and unzip it.
2. Open `desktop-dock.exe` and click **Install**.

The program isn't signed (that costs money every year), so Windows warns first: "Unknown Publisher" (click **Run**), or "Windows protected your PC" (click **More info**, then **Run anyway**).

It installs just for you, in `%LOCALAPPDATA%\Programs\Desktop Dock` (or a folder you pick with Change…), with a Start menu entry and an entry in Settings > Apps > Installed apps. It starts with Windows unless you untick that.

![Setup](docs/images/setup.png)

- **Update:** open a newer `desktop-dock.exe` and click Update. Your dock stays exactly as it is.
- **Getting back in:** if you've exited the dock, start Desktop Dock from the Start menu. Starting it while it's running opens Dock settings.
- **Uninstall:** Settings > Apps > Installed apps > Desktop Dock > Uninstall. Your dock is kept for next time, unless you tick *Also remove my dock*.

## Using it

- **Open something:** click its icon.
- **Add things:** drag them onto the dock, or right-click > Add (File, Folder, Separator, Recycle Bin, or a Windows item).
- **Arrange:** drag icons along the dock. Drag one off the dock to remove it; Undo appears for a few seconds, and right-click > Put back brings back anything removed recently.
- **Groups:** right-click an icon > Move to group > New group, or drag an icon onto a group. Click a group to open it; drag items in, out and around inside it.
- **Lock icons:** right-click > Lock icons stops accidental dragging. Hold Ctrl to move one anyway.
- **From the keyboard:** set a shortcut in Dock settings > Screen (say Ctrl+Shift+D) to bring the dock out; press it again, or Esc, to send it back up.
- **Change an icon:** right-click > Change icon... (your own images, the program's icons, or Windows' icons).
- **Everything else:** right-click > Dock settings...

Dock settings shows each change on the dock as you make it, and **Revert all changes** puts your dock back as it was when the window opened. Its pages: Look, Behaviour, Screen, Items, Recently removed, Versions, Files and About.

## Coming from RocketDock

Dock settings > Files > **Import from RocketDock**: from RocketDock on this PC, or from a `.reg` export of its settings (`reg export HKCU\Software\RocketDock rocketdock.reg`). Your icons, their order and the dock's sizes and timings come across; your current dock is kept in Versions first.

## Where things live

Everything is in the folder it's installed in:

| | |
|---|---|
| `desktop-dock.exe` | the program |
| `dock.toml` | your dock: its items and settings, in plain text ([format](docs/config-format.md)); hand edits are picked up when you save |
| `backups\` | automatic versions and snapshots |
| `removed.toml` | Recently removed |
| `shortcuts\` | copies of shortcuts you dropped on the dock, so they keep working if the original goes |
| `cache\icons\` | icons, made once at the right size (safe to clear) |
| `logs\` | what the dock did (the last 512 KB) |

## Known issues

- Pressing Esc at the very moment you let go of a dragged icon, while another program's message is in front and takes the Esc, can move the icon instead of cancelling. Undo puts it back.
- If `dock.toml` is changed by hand during a drag, the drop lands on the changed file instead of cancelling the drag. Your edit is kept, and Undo is offered.
- Windows on ARM isn't tested.

## Building from source

You need Rust (stable) and Visual Studio's C++ build tools, for the Windows SDK's resource compiler.

```
cargo build --release      # target\release\desktop-dock.exe
cargo test
.\tools\package.ps1        # the release: dist\DesktopDock-<version>.zip
```

`tools\package.ps1` builds reproducibly: the same source, built with the same Rust and Visual Studio versions (listed in each release's notes), gives a byte-identical `desktop-dock.exe`, so you can check a release against its source with the SHA-256 published beside it.

## Licence

MIT: see [LICENSE](LICENSE). Desktop Dock is provided as is, without warranty of any kind; you use it at your own risk. It is not affiliated with RocketDock or Punk Labs. The open-source libraries it is built with, and their licences, are listed in `THIRD-PARTY-NOTICES.txt` in each release.
