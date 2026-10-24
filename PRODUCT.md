# Product

<!-- impeccable:product-schema 1 -->

## Platform

desktop (Linux, native window)

## Stack

Rust. One binary, `loginux`: a command line, a background engine run by a
systemd user service, which also shows a tray icon and sends desktop
notifications, and a native egui window for editing (chosen by GbrFrn,
2026-10-22, over a local web page or a webview). The engine is the only part
that opens the device; the window, the tray and the command line talk to it
over a local socket. When no engine answers, the window starts the service if
it is enabled, or else runs an engine in its own process, which hands the
lighting back when the window closes.

## Users

Linux owners of the Logitech G316 X 98 (USB `046d:c36b`) first, and of other
Logitech HID++ keyboards and mice as definitions are contributed; the G903
HERO mouse (its logo light) is built in too. The first user is GbrFrn on a
KDE Wayland desktop. The project is open source (MIT OR Apache-2.0), so
nothing may assume this one machine, and adding a device should take a
definition file rather than code. Their jobs:

- set a lighting profile once and have it stay applied through replug, USB
  re-enumeration and login;
- calibrate and research: light single zones to confirm the light-bar order
  and what the unknown zones do.

## Product Purpose

Per-key and light-bar RGB for this keyboard on Linux, where Logitech G HUB
does not run, and the device's own effects (breathing, colour cycle, wave,
ripple). Colours live in local profile files, and the four effects also come
as built-in presets. A user service keeps the chosen profile applied, and one
action hands the lighting back to the keyboard's onboard effects. Success:
the user picks colours, sees them on the keyboard, and stops thinking about
it.

## Positioning

Built on this keyboard's own evidence: the live zone bitmap, its firmware and
its G HUB layout. It writes colours to RAM only, never to flash, and every
release restores the onboard lighting. It shows what is proven and what is
not, rather than presenting metadata as fact.

## Operating Context

- The keyboard is on USB. Its HID++ interface (interface 2) is reachable
  without root through a udev rule that tags only that interface `uaccess`.
- Profiles are TOML files in the user's config directory, editable by hand.
- Per-key colours are lost on replug, reset or release, so a host process must
  reapply them. Unplugging the keyboard is the last-resort recovery.

## Capabilities and Constraints

- 133 lighting zones: 95 keys, a 30-segment light bar between the function row
  and the number row, and 8 zones with unknown or no physical effect (0, 47,
  99–103, 154). Unknown zones are touched only by explicit numeric selection.
- Lighting uses HID++ features 0x8071 (RGB effects) and 0x8081 (per-key
  lighting), resolved by feature ID. The only other writes are the report
  rate (0x8061/3) and, around it, the 0x8101/6 switch to host mode and back,
  for a rate the user chose; every other request is a read. The flash-save
  bit is never set. DFU, reset and engineering features are never called.
- Taking control from another program that already holds the lighting needs
  an explicit user action.
- Devices are data: `devices/*.toml` (built in) and
  `~/.config/loginux/devices/*.toml` describe USB identity, zones, key
  positions and light strips, or the lighting clusters of a device without
  per-key lighting. Only HID++ 2.0 is implemented: 0x8081 per-key zones where
  the device has them, else 0x8071 clusters (the G903's logo);
  `CONTRIBUTING.md` covers adding devices and protocols.
- The G316 definition covers the ANSI 98-key layout (G HUB `g316x_ww_98`).
  Other layouts are undecided.
- Observed on the keyboard: paint and release, light-bar order, reconnect
  after replug, live edits without flicker, a channel check of all 133 zones
  (2026-10-22); report-rate changes to 2000, 4000 and 8000 Hz, each read
  back, and the kept rate sent again and read back after a replug
  (2026-10-23). Brightness and game mode have only been read.
- Optional, and off until the editor's switch (or `loginux claude --enable`)
  turns it on: the light bar as a status line for Claude Code, one share per
  running instance, with its context and the account's usage limits, drawn by
  the engine as per-key frames (no other requests). The keys keep the chosen
  profile, an effect paused on a still frame, and show a short pixel sprite
  only when an instance starts, when a long turn waits or ends, on an error,
  and when the context or a limit crosses a mark (GbrFrn, 2026-10-24: the
  light bar is the constant status indicator; animations on the keys rarer,
  like pixel art; instances split the bar). Turning it on installs Loginux's
  Claude Code plugin from this repository's marketplace and passes the user's
  status line command through Loginux, both written to keep working if
  Loginux is uninstalled; turning it off disables the plugin, leaving it
  installed, and puts the command back. Nothing shows without a chosen profile.
- Out of scope for now: the dot screen. It is a separate phase.

## G316 controls

Optional device status covers brightness (`0x8040`), report rate (`0x8061`)
and game mode (`0x4523`). Each is discovered per connection, so other
keyboard definitions promise none of them. Report-rate selection is a
deliberate user action: the engine switches `0x8101` profile management to
host mode, writes the rate, switches back to onboard mode and reads the rate
back; if the keyboard re-enumerates, the engine reapplies its RAM lighting
profile. The keyboard can come back at its own rate after a reconnect, so
the engine sends the chosen rate again, as G HUB does (GbrFrn, 2026-10-23),
never a rate the keyboard refused, and it stops after three quick drops in
ten minutes. Brightness and game-mode writes wait for a verified G316 request
and restoration behaviour. The knob already reports standard Consumer Control
events to Linux. The evidence and remaining gaps are in
[research/g316-extra-controls.md](research/g316-extra-controls.md).

## Brand Commitments

- Dark interface. GbrFrn asked for darker colours or a dark mode when
  choosing the bitmap-specimen direction (2026-10-22).

## Evidence on Hand

- [`research/`](research/README.md): protocol, zone map and proof labels.
- Live visual proof (Python tool, 2026-10-22): Esc turned red, then the onboard
  lighting returned on release.
- The private research workspace contains G HUB layout metadata used to
  derive the G316 definition. Its coordinates are not proof of physical LED
  order; vendor files are not shipped in this repository.
- Our own images are the Loginux mark (`icons/`, drawn from
  `icons/loginux.txt`) and the editor screenshots in `docs/screenshots/`.
  There are no photos. Logitech's product images and logos are not ours to
  ship.

## Product Principles

1. Never leave the keyboard worse than you found it: RAM-only lighting writes,
   a clean release, and unplug as the lighting fallback. Any device-setting
   write follows an explicit user choice and has a documented recovery path;
   a chosen report rate is sent again after a reconnect, as G HUB does.
2. Say what is proven. Observed, from metadata, and unknown stay distinct
   wherever proof is shown: in the channel check, the research notes and the
   docs. The editor shows colour; proof lives in the channel check (GbrFrn,
   2026-10-23).
3. Set once, then stay out of the way.
4. Research is first-class but fenced: raw zones only by explicit number.
5. One owner of the device at a time.
