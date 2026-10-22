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
lit nothing. Not yet observed: re-enumeration after a report-rate change, and
whether the keyboard keeps a rate across a replug.
[`research/`](research/README.md) records the evidence and its labels.

## Supported keyboards

| Keyboard | Definition | Notes |
| --- | --- | --- |
| Logitech G316 X 98 (ANSI) | `keyboards/g316-x-98.toml` | 95 keys, 30-segment light bar, 8 zones with no known key |

Other Logitech keyboards that report HID++ features `0x8071` and `0x8081` can
be added with a definition file and no code; `CONTRIBUTING.md` walks through
it, starting from `loginux probe`. Protocol and device notes are in
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

```sh
cargo build --release          # binary at target/release/loginux
cargo install --path .         # or install it as ~/.cargo/bin/loginux
```

The keyboard's HID++ interface needs a udev rule so your user can open it.
The rule tags interface 2 only; the interfaces that carry keystrokes stay
root-only.

```sh
sudo cp udev/70-loginux-hidpp.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=hidraw --property-match=ID_MODEL_ID=c36b
```

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

## Run at login

```sh
mkdir -p ~/.config/systemd/user
cp systemd/loginux.service ~/.config/systemd/user/
systemctl --user daemon-reload
loginux use NAME                                  # the profile to keep
systemctl --user enable --now loginux.service     # what RUN AT LOGIN does
```

Enabling creates the link
`~/.config/systemd/user/default.target.wants/loginux.service`, so `loginux run`
starts at every login. The unit expects the binary at `~/.cargo/bin/loginux`;
edit `ExecStart` if it lives elsewhere.

- Stop now, handing the lighting back: `systemctl --user stop loginux`
- Stop at login: `systemctl --user disable --now loginux`
- Logs: `journalctl --user -u loginux`

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
- `~/.config/loginux/keyboards/*.toml`: your own keyboard definitions; one with
  a built-in's ID replaces it
- `$XDG_RUNTIME_DIR/loginux/`: lock, socket and claim record (cleared at logout)

## Credits

Bitmap lettering is drawn from Silkscreen (with a redrawn bold W) and body
text uses Archivo, both under the SIL Open Font License 1.1 (`fonts/`). The
G316 key positions were measured from G HUB's layout data and snapped to
quarter-key units; the G HUB files themselves are not part of this program.
