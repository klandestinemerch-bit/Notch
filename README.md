<p align="center">
  <img src="assets/icon.svg" width="112" alt="FathNotch icon">
</p>

<h1 align="center">FathNotch</h1>

<p align="center">
  A light, fast <b>dynamic island</b> for Windows, written in Rust.<br>
  A small black pill at the top of your screen that shows what's happening, opens into a panel of tools,<br>
  and morphs into a capsule for alerts.
</p>

<p align="center">
  <a href="https://cfathurrohman.github.io/FathNotch/"><b>Website</b></a> ·
  <a href="../../releases/latest"><b>Download</b></a>
</p>

<p align="center">
  <img alt="Windows 10/11" src="https://img.shields.io/badge/Windows-10%20%7C%2011-0078D6">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-stable-orange">
  <img alt="License: MIT" src="https://img.shields.io/badge/License-MIT-green">
</p>

<p align="center">
  <img src="docs/images/todos.png" width="640" alt="FathNotch open on the To-Dos tab">
</p>

## Features

- **Closed pill** shows live info: clock, weather, next prayer, open to-dos, battery, and while music
  plays the song title or a synced karaoke-style lyric line (the pill widens to fit it).
- **Alerts** morph the pill into a capsule: Caps/Num Lock, keyboard layout, volume and brightness bars,
  network, Bluetooth, power, Windows notifications, reminders, prayer times and phone transfers.
- **Tools** in the open panel:

  | Tool | What it does |
  |---|---|
  | Home | Greeting, clock, weather, now playing, today's events and to-dos |
  | Control | Wi-Fi, Bluetooth, hotspot, silent; audio devices, per-app volume, brightness |
  | Shelf | Park files from anywhere; box-select, multi-select, drag them back out |
  | Phone | Send files and text both ways with your phone over Wi-Fi: scan a QR code, no app needed |
  | Clipboard | Clipboard history |
  | Music | Now playing with synced lyrics, and a player for music files on your PC (queue, shuffle, repeat) |
  | To-Dos | Lists, due dates with a clock-face time picker, priorities, optional TickTick sync |
  | Calendar | Month and day views, local events, .ics subscriptions, dated to-dos |
  | Timers | Pomodoro, countdown, stopwatch |
  | Prayer | Locally computed prayer times with alerts |
  | System | CPU, memory, disk, network, battery, apps with end task |
  | Weather, Notes, Notifications, Emoji, Shortcuts, Teleprompter | |

- **Themes**: 17 color schemes (dark and light), 16 accents, optional translucent panel.
- **Private**: everything stays on your PC. Data lives in `%LOCALAPPDATA%\FathNotch`, secrets are
  protected with Windows DPAPI, and the phone link only works on your local network.

## Screenshots

| | |
|---|---|
| <img src="docs/images/calendar.png" alt="Calendar"> | <img src="docs/images/timer.png" alt="Timers"> |
| <img src="docs/images/shelf.png" alt="Shelf"> | <img src="docs/images/emoji.png" alt="Emoji"> |
| <img src="docs/images/phone.png" alt="Phone transfer on the PC"> | <img src="docs/images/settings.png" alt="Settings"> |

### Phone transfer

Turn on **Phone**, scan the QR code with your phone's camera, and a page opens in its browser. Send photos,
files and text to the PC, or download what you dropped on the notch. The page uses the same colors as the
app and follows your accent live.

<p align="center">
  <img src="docs/images/phone-web.png" width="300" alt="The phone page">
</p>

## Install

1. Download `FathNotch.exe` from the [Releases](../../releases) page.
2. Run it. The notch appears at the top of your main display, and a cat icon appears in the tray.
3. Optional: Settings → General → *Open at login*.

No installer and no runtime needed. To uninstall, quit from the tray icon and delete the exe (and
`%LOCALAPPDATA%\FathNotch` if you also want to remove your data).

> Windows SmartScreen may warn about an unsigned app the first time. Choose *More info → Run anyway*,
> or build it yourself from source.

## Build from source

Requires [Rust](https://rustup.rs) (stable). The GNU toolchain works, so Visual Studio isn't required.

```powershell
git clone https://github.com/cfathurrohman/FathNotch.git
cd FathNotch
cargo build --release      # → target\release\fathnotch.exe
```

The only dependencies are the `windows` crate and `serde`/`serde_json`. There's no C code to compile.

## Why it's light

- One layered, top-most window. Drawing uses Direct2D's software renderer into a bitmap shown with
  `UpdateLayeredWindow`, so no GPU driver is loaded into the process. Transparent pixels pass clicks through.
- At rest the UI thread sleeps in `GetMessage`; the clock wakes it once a minute.
- While something animates, it draws one frame per display refresh (`DwmFlush`) and stops once all
  springs settle.

Measured at idle with all tools: about **16 MB** of memory (private working set), **0 %** CPU, and a
single **~4 MB** executable with the Inter font built in.

## Project layout

```
src/
  main.rs          window, message loop, tray, hotkeys
  notch.rs         the pill: shape, springs, closed/open/alert states
  gfx.rs           Direct2D + DirectWrite drawing, icons, the tray cat
  ui/              immediate-mode UI kit (Rect, Ui, widgets, icons)
  theme.rs         color schemes and accents
  views/           one file per tool
  services/        settings, phone server, media, network, weather, …
assets/            phone page (phone.html), Inter font, icon
docs/              SPEC.md (what the app does), UI-GUIDE.md (writing a tool)
tools/shot.ps1     renders the app off screen to a PNG (for screenshots)
```

For screenshots without touching your real data, point `FATHNOTCH_DATA_DIR` at a scratch folder;
`tools\shot.ps1 -Tool todos -Out todos.png -DataDir C:\temp\demo` does this for you.

## Contributing

Issues and pull requests are welcome. Please read [docs/SPEC.md](docs/SPEC.md) for how the app should
behave and [docs/UI-GUIDE.md](docs/UI-GUIDE.md) before adding or changing a tool.

## License

[MIT](LICENSE). The bundled Inter font is under the SIL Open Font License
([assets/fonts/Inter-LICENSE.txt](assets/fonts/Inter-LICENSE.txt)).
