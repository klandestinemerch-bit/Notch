# FathNotch: functional spec

FathNotch is a Windows "dynamic island": a small black pill at the top of the screen that shows live
information while closed, grows into a panel of tools when the pointer rests on it, and briefly grows
into an alert when something happens. This document describes **what** the app does. The code in this
repository is written from this description.

## Principles

- **Light.** At rest the app waits for messages and uses no CPU. It only redraws while something is
  moving, and then in step with the display's refresh (vsync). Target: < 15 MB of memory, 0 % CPU at idle.
- **Smooth.** All size and shape changes go through damped springs. Content fades and slides, it never pops.
- **One shape.** The notch is a single outline whose parameters (width, height, ears, corner radii,
  drop below the top edge) are animated together, so any state can morph into any other.
- **Plain look.** Solid colors, no gradients. Default accent is white (Snow), with readable contrast on toggles.

## The shell

### Styles
- **Notch:** hangs from the top edge. Concave "ears" curve into the screen edge, and the bottom corners are rounded.
- **Capsule:** floats 8 DIP below the top edge, fully rounded.

### States
- **Closed (idle):** a fixed width so the pill doesn't change size as items rotate. A glyph sits on the
  left and a value on the right, in the same layout for every item. Items include the clock, date,
  weather, next prayer, open to-dos, and battery. When there are several, they rotate: the old item
  drifts up and fades out, then the new one rises in.
- **Closed (activity):** music shows the title, a karaoke-style lyric line in time with the song, and an
  equalizer. Timers and reminders show their countdown. The pill widens for these.
- **Open:** the panel springs open when the pointer rests on the pill, or from a hotkey or the tray. It
  has a tab bar with icons, and content scales and fades in. It closes shortly after the pointer leaves
  unless pinned.
- **Alert:** the pill always becomes a capsule, even in notch style. The ears melt away, the top
  corners round off, and the pill drops slightly and widens, using a slower, softer spring. Two kinds:
  - **Compact** (Caps Lock, Num Lock, keyboard layout, network, Bluetooth, power): a glyph and one line.
  - **Level** (volume, brightness): a glyph and a progress bar that glides to the new value. No percent text.
  - **Full** (Windows notifications, reminders, prayer time, downloads): icon, title, body (up to 2
    lines), dismiss button, and a "+N" count of queued alerts. A repeat of the same alert (for example,
    volume steps) updates in place.

### Behavior
- Dodge: when the pointer comes from the side, the closed pill slides away from it.
- Hides automatically for full-screen apps and for listed apps.
- Starts at login (optional), single instance, tray icon with menu.

## Tools (tabs)

| Tab | What it does |
|---|---|
| Home | Greeting, clock, weather pill, now-playing card, today's events and open to-dos, lyrics shortcut |
| Control | Wi-Fi, Bluetooth, Hotspot and Silent tiles (the round icon toggles, the card opens a list); output and input device pickers with mute; volume per app; brightness per display |
| Music | Now playing (art, controls, progress), synced lyrics, on/off toggle; music files from this PC: + or a drop adds songs or folders to a queue (play, remove, shuffle, repeat off/all/one, clear), played with Windows' MediaPlayer and kept in player.json |
| To-do | List, Important, Completed. Each to-do has a date (Today, Tomorrow, Next week, No date, next 14 days), a time picked on a clock face (hour, then minutes, AM/PM, Set time / All day), and a priority (None/Low/Medium/High). Two-way sync with TickTick (personal token) |
| Calendar | Month and day views. Local events and subscribed .ics feeds. To-dos with a date appear too. Same clock-face time picker |
| Prayer | Times computed locally (Kemenag: Fajr 20°, Isha 18°, 2-minute safety margin). A rolling timeline with a horizon line: day arcs above it, night below. Alert at each prayer time |
| Notes | Quick notes |
| Timer | Pomodoro, countdown, and stopwatch on a round progress arc |
| System | Performance (CPU, RAM, network) and an app list with end task |
| Notifications | History of Windows notifications |
| Weather | Current conditions and forecast for a chosen city |
| Settings | General, Appearance (themes including translucent, accent colors), Display (style, size, panel scale, border), Tabs, Alerts, Keyboard, Music, Calendar, Prayer, To-dos, Data, About |

## Data

- Stored as JSON under `%LOCALAPPDATA%\FathNotch`, written atomically.
- Secrets (TickTick token, calendar links) are protected with DPAPI.
- On first launch, data is imported from the earlier .NET version, so to-dos, notes, settings and the
  TickTick token carry over.
