# Loginux

Per-key RGB for Logitech HID++ keyboards on Linux, starting with the G316 X 98
(USB `046d:c36b`) and its 30-segment light bar. You paint keys in an editor
window or in profile files, and Loginux keeps the chosen profile on the keyboard
through replugs and logins. One command hands the lighting back to the
keyboard's own effects.

Loginux writes colours to the keyboard's RAM only. It never sets the flash-save
bit, never calls the DFU, reset or engineering features, and unplugging the
keyboard always restores its onboard lighting.

## Status

Watched on a G316 X 98 on 2026-10-22: a single-key paint and its release, the
light-bar order (a red-to-blue gradient ran left to right), the service
repainting the keyboard by itself after an unplug and replug, live colour
edits from the editor without visible flicker or lag, and the onboard lighting
returning when the service stopped. A channel check of all 133 zones found
every key and bar segment where the definition puts it; the 8 unknown zones
lit nothing. On CachyOS with Plasma 6.8 the package installed cleanly, the
service's tray icon named the keyboard and its profile, and a left click on it
opened the editor. Picked from the tray, the Wave and Breathing presets played
on the keyboard. Not yet observed: re-enumeration after a report-rate change,
and whether the keyboard keeps a rate across a replug.
[`research/`](research/README.md) records the evidence and its labels.

## Supported devices

| Device | Definition | Notes |
| --- | --- | --- |
| Logitech G316 X 98 (ANSI) | `devices/g316-x-98.toml` | 95 keys, 30-segment light bar, 8 zones with no known key |
| Logitech G903 HERO, on its cable | `devices/g903-hero.toml` | logo only; the DPI indicator stays dark while Loginux holds the lighting ([why](research/g903.md)) |

Other Logitech keyboards that report HID++ features `0x8071` and `0x8081` can
be added with a definition file and no code, and so can devices with `0x8071`
alone, such as mice, at one colour per lighting cluster. `CONTRIBUTING.md`
walks through it, starting from `loginux probe`. Protocol and device notes are in
[`research/`](research/README.md).

## Device settings

The editor's DEVICE tab and `loginux status` show settings the keyboard
reports, when it reports them. Loginux reads them on every connection and
every ten seconds after.

| Setting | What Loginux does | Proof |
| --- | --- | --- |
| Lighting brightness (`0x8040`) | Shows the current value and the maximum. Read only. | Read on Linux |
| Game mode (`0x4523`) | Shows on or off, and follows the keyboard's game-mode key. Read only. | Read on Linux |
| Report rate (`0x8061`) | Shows the current rate and the rates the keyboard supports. Changes it only when you ask: the DEVICE tab after a confirmation, or `loginux rate HZ`. | The change was seen in G HUB on Windows; Loginux has not changed it on Linux yet |
| Volume knob | Nothing: the knob sends ordinary media keys, which Linux handles. | Seen on Windows |

A report-rate change makes the keyboard leave the USB bus and come back.
Loginux sends the new rate once and never retries it, waits up to 20 s for
the keyboard, reads the rate back, and repaints the chosen profile. If the
keyboard does not return or reports another rate, the editor and the command
line say so. Whether the keyboard keeps the rate after a replug is not known
yet. Brightness and game-mode changes are not offered: their commands on the
G316 are not verified. [G316 extra controls](research/g316-extra-controls.md)
has the evidence.

## Install

### Arch Linux and CachyOS

`packaging/aur/PKGBUILD` builds the package `loginux-git` from the latest
commit. It installs `loginux`, the udev rule, the user service, a menu entry
and the icons; pacman reloads udev, so the devices need no replug. Once the
package is on the AUR, `paru -S loginux-git` (or `yay`) installs it; from this
repository, `cd packaging/aur && makepkg -si` does. Then, as your user:

```sh
systemctl --user enable --now loginux.service    # or RUN AT LOGIN in the editor
```

### From source

```sh
cargo build --release          # binary at target/release/loginux
cargo install --path .         # or install it as ~/.cargo/bin/loginux
```

The device's HID++ interface needs a udev rule so your user can open it. The
rule tags the HID++ interface only; the interfaces that carry keystrokes and
mouse input stay root-only.

```sh
sudo cp udev/70-loginux.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=hidraw
```

For a menu entry, copy `packaging/loginux.desktop` to
`~/.local/share/applications/` and `icons/loginux.svg` to
`~/.local/share/icons/hicolor/scalable/apps/`. The entry runs `loginux ui`,
so `~/.cargo/bin` has to be on the desktop session's `PATH`.

## The editor

Run `loginux` (or `loginux ui`). The window shows the board with every key and bar
segment in its colour. Click a key, shift-click to add more, or drag across
the board, then set the colour with the picker, the hex field or the RGB
values. The keyboard follows while you paint. SAVE writes the profile; USE
THIS PROFILE makes it the one Loginux keeps applied. RELEASE LIGHTING hands the
keyboard back to its onboard effects.

The lettering is drawn on whole device pixels, so it stays sharp at any
display scale; `LOGINUX_UI_ZOOM=1.25 loginux` enlarges the window's contents beyond
the display's own scale.

The window never opens the keyboard itself. If no Loginux engine is running, it
starts one inside the window, and that engine hands the lighting back when the
window closes. To keep a profile after the window closes and at every login,
turn on RUN AT LOGIN (it needs the service installed, see below).

CHANNEL CHECK lights one zone at a time, white, with everything else dark. You
click where the light appeared, or answer that nothing lit or that it lit
somewhere not on the map. Answers go to
`~/.config/loginux/checks/KEYBOARD.toml` and show on the board as marks: green
where the LED matched the map, amber where it did not. Until a zone is
checked, its position is layout data and not proof.

## Command line

| Command | What it does |
| --- | --- |
| `loginux zones` | Lists key names, `bar1`–`bar30` and the unknown zones with their IDs |
| `loginux status` | Shows the keyboard, who holds the lighting and the chosen profile |
| `loginux use NAME` | Chooses the profile Loginux keeps applied (`--none` to stop) |
| `loginux apply NAME [--hold S \| --keep]` | Paints a profile for S seconds (default 10), then hands back |
| `loginux paint esc=ff0000 bar3=00ff00 [--base RRGGBB]` | Paints single zones; takes the same `--hold`/`--keep` |
| `loginux release` | Hands the lighting back to the keyboard now |
| `loginux rate [HZ]` | Shows the report rate, or changes it and reads it back (the keyboard reconnects) |
| `loginux run [--refresh S]` | The engine: keeps the profile applied; the service runs this |
| `loginux probe --device /dev/hidrawN` | Read-only: prints a starting definition for a new keyboard |

`--device /dev/hidrawN` picks the node by hand; Loginux checks that it really is
the HID++ interface of a known keyboard before sending anything. `apply` and `paint`
refuse to run while an engine owns the keyboard, and they check every name and
colour before the first write.

## Profiles

Profiles live in `~/.config/loginux/profiles/NAME.toml`:

```toml
[keys]
base = "#3a2a6e"      # every key not listed
esc = "#5b6cff"
w = "#38d07a"
leftshift = "#38d07a"

[bar]
base = "#000000"      # every light-bar segment not listed
bar1 = "#5b6cff"

[zones]               # raw zone IDs, for research
154 = "#ffffff"
```

Key names follow Linux `KEY_*` names (`esc`, `f1`, `grave`, `leftbrace`,
`kp5`, `rightmeta`); `loginux zones` prints the connected keyboard's names.
Ambiguous names such as `shift` are rejected with a suggestion. Each light
strip gets a table named after it (`[bar]` on the G316). Zones with no known
key (0, 47, 99–103 and 154 on the G316) stay dark unless `[zones]` names
them. Hand edits to the chosen profile apply within a second while the engine
runs.

### Effects

A profile with an `[effect]` table has the device run one of its own effects
in place of the colours:

```toml
[effect]
kind = "wave"         # breathe, cycle, wave or ripple
period = 3000         # milliseconds per cycle (breathe, cycle, wave)
direction = "right"   # wave: right, left, up, down, centre-out, centre-in
color = "#ff2e3f"     # breathe and ripple
```

Four presets are built in: Breathing, Colour Cycle, Wave and Ripple. They
show in the editor (marked BUILT IN), in the tray's Profile menu and to
`loginux use`. In the editor, LIGHTING switches the open profile between its
colours and an effect; a changed preset is saved under a new name. A cluster
without the chosen effect shows the effect's colour instead, and a device
with none of it refuses the profile: the G903 has no wave or ripple.

## Run at login

The package installs the user service. From source, copy it first:

```sh
mkdir -p ~/.config/systemd/user
cp systemd/loginux.service ~/.config/systemd/user/
systemctl --user daemon-reload
```

Then:

```sh
loginux use NAME                                  # the profile to keep
systemctl --user enable --now loginux.service     # what RUN AT LOGIN does
```

Enabling makes `loginux run` start at every login. The copied unit expects the
binary at `~/.cargo/bin/loginux`; edit `ExecStart` if it lives elsewhere.

- Stop now, handing the lighting back: `systemctl --user stop loginux`
- Stop at login: `systemctl --user disable --now loginux`
- Logs: `journalctl --user -u loginux`

## Tray icon

While `loginux run` runs, Loginux shows a tray icon. It speaks the
StatusNotifierItem protocol: KDE Plasma shows it, and GNOME does with the
AppIndicator extension. The menu names the device and the profile in use,
switches profiles (Onboard Lighting hands the lighting back), opens the editor
(so does a left click), and Quit stops the service until the next login,
handing the lighting back.

## Getting the onboard lighting back

`loginux release`, RELEASE LIGHTING in the editor, or stopping the service all
hand the lighting back. If Loginux was killed without a chance to do that, the
next `loginux release` restores the saved values from
`$XDG_RUNTIME_DIR/loginux/claim`. Unplugging and replugging the keyboard always
resets it.

When another program holds the keyboard's lighting, Loginux does not overwrite
it: the editor shows the conflict and offers TAKE OVER, and the command line
stops with a message.

## How it talks to the keyboard

The keyboard is found through sysfs by USB ID and interface number, and the
check is repeated on the opened file. HID++ feature indices come from the
keyboard (IRoot) on every connect. Loginux sends only these requests:

- `0x0000` IRoot: getFeature, getProtocolVersion
- `0x8071` RGB effects: getInfo, setEffect with persistence `0x01` (RAM), get
  and set software control (mode bit 0 only; the firmware keeps power
  management)
- `0x8081` per-key lighting: getInfo, setIndividualRgbZones, frameEnd
- `0x4540` keyboard layout: read

A paint takes control, sets the Static effect on each lighting cluster, sends
the frame and commits it. After that only changed frames are sent, and the
claim is checked every two seconds; if the keyboard reset itself, Loginux paints
again. A single engine owns the keyboard at a time (a lock in
`$XDG_RUNTIME_DIR/loginux`), and the editor and the command line talk to it over a
socket there.

## Files

- `~/.config/loginux/profiles/*.toml`: profiles
- `~/.config/loginux/config.toml`: the chosen profile and the keyboard last seen
- `~/.config/loginux/checks/KEYBOARD.toml`: channel-check answers per keyboard model
- `~/.config/loginux/devices/*.toml`: your own device definitions; one with
  a built-in's ID replaces it
- `$XDG_RUNTIME_DIR/loginux/`: lock, socket and claim record (cleared at logout)

## Licence

Loginux is licensed under either of the Apache License, Version 2.0
(`LICENSE-APACHE`) or the MIT licence (`LICENSE-MIT`), at your option. The
fonts in `fonts/`, and `src/pixelfont.rs`, which is generated from Silkscreen,
stay under the SIL Open Font License 1.1.

Unless you say otherwise, a contribution you submit for inclusion in Loginux is
licensed as above, without any additional terms.

## Credits

Bitmap lettering is drawn from Silkscreen (with a redrawn bold W) and body
text uses Archivo, both under the SIL Open Font License 1.1 (`fonts/`). The
G316 key positions were measured from G HUB's layout data and snapped to
quarter-key units; the G HUB files themselves are not part of this program.
