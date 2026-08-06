# Audio Library

A lightweight personal audio collection for GNOME/Ubuntu. Store references to
your MP3 files, play them from the main window **or straight from a top-bar
system icon** (StatusNotifierItem / AppIndicator).

## Features

* Add audio: pick an MP3, give it a title, save it to a local SQLite library
  (`~/.local/share/audio-library/audio-library.db`).
* Play / pause / stop / seek with GStreamer, resume position, per-entry
  "last played" memory.
* **Top-bar indicator** (Ubuntu navbar): lists every saved title, plays a
  title on click, shows now-playing state, and offers play/pause/stop, "Open
  Audio Library" and "Quit".
* **Always-present icon**: the app closes to the tray (closing the window
  hides it, playback continues) and autostarts at login, so the indicator is
  in the navbar from login onward — it only disappears if you explicitly Quit
  or log out.

## Building

Requires the usual GTK4/libadwaita/GStreamer dev packages (already present on
this machine).

```bash
cargo build --release
```

## Installing (menu entry + tray autostart + icon)

```bash
./packaging/install.sh          # installs into ~/.local (or /usr/local as root)
```

This installs the release binary, an application-menu entry, the app icon and
an **autostart entry** (`~/.config/autostart/audio-library.desktop`) so the
tray icon appears automatically after login. Add `--no-autostart` to skip the
login autostart.

## Running

```bash
audio-library                     # open the main window (tray icon too)
env AUDIO_LIBRARY_BACKGROUND=1 audio-library   # tray-only, no window
```

The top-bar icon needs the AppIndicator/StatusNotifier host that stock Ubuntu
already provides (GNOME's "AppIndicator" extension). Without it the app still
runs normally, just without the icon.

## Configuration / data

* Library: `~/.local/share/audio-library/audio-library.db`
* Autostart: `~/.config/autostart/audio-library.desktop`

## Project layout

* `src/` – Rust sources (GTK window, playlist popover, dialogs, player,
  database, and the `tray.rs` status-notifier indicator).
* `resources/` – bundled stylesheet and the `icons/audio-library.png` icon.
* `packaging/` – `.desktop` files and the `install.sh` installer.
