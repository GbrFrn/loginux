# Product

<!-- impeccable:product-schema 1 -->

## Platform

desktop (Linux, native window)

## Stack

Rust. One binary, `loginux`: a command line, a background engine run by a
systemd user service, and a native egui window for editing (chosen by GbrFrn,
2026-10-22, over a local web page or a webview). The engine is the only process
that opens the keyboard; the window and the command line talk to it over a
local socket.

## Users

Linux owners of the Logitech G316 X 98 (USB `046d:c36b`) first, and of other
Logitech HID++ keyboards as definitions are contributed. The first user is
GbrFrn on a KDE Wayland desktop. The project will be open source (GbrFrn,
2026-10-22), so nothing may assume this one machine, and adding a keyboard
should take a definition file rather than code. Their jobs:

- set a lighting profile once and have it stay applied through replug, USB
  re-enumeration and login;
- calibrate and research: light single zones to confirm the light-bar order
  and what the unknown zones do.

## Product Purpose

Per-key and light-bar RGB for this keyboard on Linux, where Logitech G HUB
does not run. Colours live in local profile files. A user service keeps the
chosen profile applied, and one action hands the lighting back to the
keyboard's onboard effects. Success: the user picks colours, sees them on the
keyboard, and stops thinking about it.

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
- Only HID++ features 0x8071 (RGB effects), 0x8081 (per-key lighting) and
  read-only queries are used, resolved by feature ID. The flash-save bit is
  never set. DFU, reset and engineering features are never called.
- Taking control from another program that already holds the lighting needs
  an explicit user action.
- Keyboards are data: `keyboards/*.toml` (built in) and
  `~/.config/loginux/keyboards/*.toml` describe USB identity, zones, key
  positions and light strips. Only the HID++ 2.0 protocol (0x8071 + 0x8081)
  is implemented; `CONTRIBUTING.md` covers adding keyboards and protocols.
- The G316 definition covers the ANSI 98-key layout (G HUB `g316x_ww_98`).
  Other layouts are undecided.
- Observed on the keyboard (2026-10-22): paint and release, light-bar order,
  reconnect after replug, live edits without flicker, a channel check of all
  133 zones. Not yet observed: a report-rate change on Linux.
- Out of scope for now: the dot screen. It is a separate phase.

## G316 controls

Optional device status covers brightness (`0x8040`), report rate (`0x8061`)
and game mode (`0x4523`). Each is discovered per connection, so other
keyboard definitions promise none of them. Report-rate selection is a
deliberate user action: the keyboard re-enumerates, and the engine reads the
rate back and reapplies its RAM lighting profile. Brightness and game-mode
writes wait for a verified G316 request and restoration behaviour. The knob
already reports standard Consumer Control events to Linux. The evidence and
remaining gaps are in
[research/g316-extra-controls.md](research/g316-extra-controls.md).

## Brand Commitments

- Dark interface. GbrFrn asked for darker colours or a dark mode when choosing the
  bitmap-specimen direction (2026-10-22).

## Evidence on Hand

- [`research/`](research/README.md): protocol, zone map and proof labels.
- Live visual proof (Python tool, 2026-10-22): Esc turned red, then the onboard
  lighting returned on release.
- The private research workspace contains G HUB layout metadata used to
  derive the G316 definition. Its coordinates are not proof of physical LED
  order; vendor files are not shipped in this repository.
- No photos, screenshots or brand assets of our own exist. Logitech's product
  images and logos are not ours to ship.

## Product Principles

1. Never leave the keyboard worse than you found it: RAM-only lighting writes,
   a clean release, and unplug as the lighting fallback. Any device-setting
   write needs an explicit user action and a documented recovery path.
2. Say what is proven. Observed, from metadata, and unknown stay distinct
   everywhere.
3. Set once, then stay out of the way.
4. Research is first-class but fenced: raw zones only by explicit number.
5. One owner of the device at a time.
