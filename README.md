# Pollen

A lightweight native dock for Windows that replaces the taskbar, written in Rust with
Direct2D. No WebView, no GPU device, no background polling: about 16 MB in Task Manager.

> Working name. Early stage: dock only, primary monitor only.

## What works

- Replaces the Windows taskbar
- Looks like Nectar's dock: Nectar's "Midnight" color, attached to the bottom edge,
  rounded on top with small inward flares, macOS-style magnification
- Animations run in step with the display's refresh rate (144 fps on a 144 Hz screen)
- Nectar's motion: hiding shrinks the bar into a circle that sinks below the edge;
  showing raises the circle, flattens it against the edge and spreads it into the bar
- Three modes (right-click the dock, or the notch's Settings tab):
  - **Sabit (Fixed):** always visible, reserves its strip. While a window is
    maximized or snapped down to it, the bar widens to the full screen width, so the
    strip never shows bare wallpaper beside the bar
  - **Akıllı (Smart):** reserves nothing, hides while a window covers it
  - **Göz at (Peek):** hidden until the mouse reaches the bottom edge
- The Windows taskbar pins, in the same order, including Store apps pinned from Start
- Running apps that are not pinned, after a separator
- Windows' own minimize/restore animation flies into and out of the app's dock icon
- An app's icon rises out of a small bulge in the bar when it opens and melts back in
  when it closes
- Tray: a "hidden icons" button opens Windows' own notification-area flyout right
  above it, so tray apps (Steam, Discord, ...) and their menus work as usual
- Start button and a clock (click: notifications and calendar)
- Click to open, click a running app to focus it, again to minimize, or to cycle its windows
- Right-click menu: new window, close windows, mode, quit
- Window previews (Nectar's): hovering a running app shows a live thumbnail of
  each of its windows, with its title; click one to switch to it, close it with
  its × (or a middle click). Minimized windows show the app's icon
- Hides while a fullscreen window is the top window on its monitor, even when the
  focus is on another monitor
- Per-monitor DPI

### Notch

- An island attached to the top of the primary monitor (Nectar's look and opening)
- Closed: time and date; while something plays, its art and level bars driven by
  the output's real peak meter. Hovered: opens with icon tabs across the top, on
  Music if something plays, else on the Control Center
- Music: what Windows' media sessions report (Spotify, browsers, players): art,
  title, artist, player, a progress bar to seek, previous / play-pause / next, and
  a picker when several players have sessions, and a slider for the player's own
  volume in the Windows mixer (for a browser that is the whole browser, not one tab). Event driven; position-only
  updates (sent about every second by live streams) get a light read
- Control Center: network and sound-output tiles (Wi-Fi / Bluetooth tiles when the
  computer has them), shortcut buttons, and sliders for the volume and for each
  monitor's brightness separately over DDC/CI (Nectar changed all monitors at once)
- Status: the weather (city, condition, today's high and low), CPU, memory, free
  space on C: and network speed. When nothing plays, the closed island shows the
  weather on the left, the time in the middle and the date on the right
- Calendar: a plain month view (Monday first), today highlighted, previous / next
  month. No accounts or calendar sync
- Settings: the dock's mode, size and magnification, the background opacity of
  the dock and the notch, and quitting Pollen. Changes apply at once and are saved
  in `%APPDATA%\Pollen\settings.txt`

The weather comes from [Open-Meteo](https://open-meteo.com/) for a rough location
looked up from the computer's public IP address (ipwho.is, falling back to
geojs.io), refreshed every 30 minutes. Choosing a city by hand is still to come.

## Getting the taskbar back

Quitting Pollen (right-click › Pollen'dan çık) restores the taskbar. If Pollen was
killed or crashed, starting it again restores it first, or run:

```powershell
pollen.exe --restore-taskbar
```

## Not yet

- Monitors other than the primary
- Drag to reorder, choosing the weather's city

## Build

```powershell
cargo build --release
.\target\release\pollen.exe
```

Debug helpers (use a debug build, which has a console):

- `pollen.exe --diag` prints the pins and windows the dock sees.
- `pollen.exe --snapshot <folder>` renders the dock's and the notch's animations to
  PNG files without showing anything on screen.
- `pollen.exe --controls` prints the sound output, network, monitors and their
  brightness, the media sessions, system use and the weather as the notch sees them.

## Credits and license

Pollen is inspired by [Nectar](https://github.com/kua8/Nectar) by kua8, which grew out
of [Bloom](https://github.com/SehajveerSingh2005/bloom) by Sehajveer Singh. Parts of
Pollen's Win32 code (reading taskbar pins, AppUserModelIDs, file descriptions, hiding
and restoring the taskbar) are ported from Nectar, so Pollen is licensed under the same
[GPL-3.0](LICENSE).
