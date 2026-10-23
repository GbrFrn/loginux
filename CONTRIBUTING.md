# Contributing

## Adding a keyboard

Loginux drives Logitech keyboards that speak HID++ 2.0 and report two features:
`0x8071` (RGB effects) and `0x8081` (per-key lighting). Such a keyboard needs
no code, only a definition file; so does a device with `0x8071` alone, such as
a mouse (see [Cluster devices](#cluster-devices)). Other devices need a
protocol backend; see the last section.

1. **Give your user access to the keyboard's HID++ interface.** Find its
   hidraw node with `udevadm info /dev/hidrawN`: look for your keyboard's
   `ID_VENDOR_ID` and `ID_MODEL_ID`, and note `ID_USB_INTERFACE_NUM`. Add a
   line for it to `udev/70-loginux.rules` and install the rule as
   [docs/usage.md](docs/usage.md#install-from-source) describes. Tag only the HID++ interface; the interfaces that carry
   keystrokes must stay root-only.

2. **Probe it.** This sends only read requests and prints a starting
   definition:

   ```sh
   loginux probe --device /dev/hidrawN > ~/.config/loginux/devices/my-keyboard.toml
   ```

   The file name, without `.toml`, becomes the definition's ID. The key names
   are guesses: on the keyboards seen so far, a `0x8081` zone is the HID
   keyboard usage minus 3, and the modifiers are 104 to 111. The positions are
   a placeholder grid.

3. **Check every zone on the keyboard.** Run `loginux`, open SETTINGS, then OPEN
   CHANNEL CHECK, and
   check all keys and the unknown zones. Loginux lights one zone at a time;
   click the key that lit, or answer NOTHING LIT or NOT ON THE MAP. Then fix
   the file: rename keys that lit somewhere else, move zones that lit a
   strip or a logo, and leave zones that lit nothing in `unknown`.

4. **Lay it out.** Set `at` and `size` so the board looks like the keyboard,
   using key units (1 = one key pitch, quarter steps are usual). Restart
   `loginux` to see each change.

5. **Send it.** Copy the file into `devices/`, add a line for it to
   `BUILT_IN` in `src/layout.rs`, add your udev line, and open a pull request.
   Say what you observed on the keyboard and what you took from elsewhere,
   such as vendor software.

## Definition format

```toml
name = "Logitech G316 X 98"     # shown in the editor and in logs
protocol = "hidpp20"            # the only protocol so far
vendor = 0x046d                 # USB vendor ID
product = 0xc36b                # USB product ID
interface = 2                   # USB interface that carries HID++
device_index = 0xff             # optional; 0xff = a keyboard on its own cable
board = [19.5, 6.625]           # board width and height in key units

# Zones the keyboard reports that light nothing known. They are lit only when a
# profile names them under [zones].
unknown = [0, 47, 99]

# One entry per key. `name` is the profile name (lowercase letters, digits and
# _; use Linux KEY_* names where they exist). `label` defaults to the name in
# capitals and is drawn on the key; the bitmap font covers ASCII and the four
# arrows. `short` is drawn instead when the label does not fit the key at the
# current display scale; without one the key shows no legend. Never make a
# short legend that reads as another key. `size` defaults to [1, 1].
keys = [
  { name = "esc", zone = 38, at = [0, 0] },
  { name = "f10", zone = 64, short = "10", at = [10.75, 0] },
  { name = "backspace", zone = 39, label = "BKSP", at = [13, 1.375], size = [2, 1] },
]

# Optional light strips. The zones are split into equal segments across `rect`
# (x, y, width, height), in the order listed, and are named bar1, bar2, ...
# A profile sets them in a [bar] table with its own base colour.
[[strip]]
name = "bar"
title = "Light bar"
rect = [0, 1.12, 19.5, 0.14]
zones = [160, 161, 162]
```

Every zone ID may appear only once, and 255 is not allowed (it means "skip"
on the wire). A definition with an unknown field is rejected, so a typo does
not get silently ignored.

### Cluster devices

A device without `0x8081` sets `lighting = "clusters"` and lists one light per
`0x8071` cluster in place of keys. `zone` is the cluster index from the probe's
`0x8071 cluster N` lines, and Loginux paints each light with that cluster's
Static effect. Such a definition has no `unknown` and no `[[strip]]`. The probe
prints no starting definition for it, so write the file from those lines.

```toml
lighting = "clusters"
lights = [
  { name = "logo", zone = 1, at = [1.25, 3], size = [1.5, 1.5] },
]
```

When the sixth byte of a cluster's probe line is not `00`, the cluster is a
status display, like the DPI LEDs of a G903. Its firmware decides which LEDs
are on, and it can stay dark while Loginux holds the lighting. Say so in the
file's comments; [research/g903.md](research/g903.md) has the evidence.

## Adding a protocol

Everything protocol-specific sits in three places:

- `src/hidpp.rs`: finding and opening the device, and the HID++ request and
  reply matching.
- `src/device.rs`: the typed feature calls and `ALLOWED`, the list of every
  request Loginux may send.
- `take`, `paint` and `give_back` in `src/engine.rs`: claiming the lighting
  from the firmware, painting a frame, and handing the lighting back.

A second protocol needs those three operations plus "which zones does the
device accept". When it arrives, put a trait over them; with one backend there
was nothing to abstract yet. Keep the rules the HID++ backend follows:

- Write colours to RAM only. Never send anything that saves to flash, updates
  firmware, resets the device or unlocks engineering features.
- Record what the device reported before taking control, and restore it on
  every exit path.
- Never take the lighting from another program without an explicit request.

Optional controls such as brightness, report rate and game mode are separate
from the `0x8071`/`0x8081` lighting requirement. Discover each feature by ID
on every connection and do not assume that another keyboard implements a
G316-specific payload. Show an optional feature only when the connected
device reports it. A setting that re-enumerates the device must be a deliberate
action with a reconnect and readback path; it must not be replayed automatically
like RAM-only lighting. Record getter, setter, persistence and restoration
evidence separately in `research/` before exposing a write.

## Tests

`cargo test` runs the unit tests in each source file and the tests of the
binary in `tests/cli.rs`. Besides Rust they need `dbus-daemon` and
`dbus-send` (the `dbus` package), and they leave the machine's own state
alone:

- Devices are simulated. `src/fake.rs` answers HID++ requests the way the
  G316 X 98 and the G903 answered in their probes, and records every request.
- The config folder, the runtime folder and sysfs are scratch copies in a
  private folder under `$TMPDIR` (`src/testenv.rs`), removed when the tests
  end, so tests never see your profiles, your running service or your
  devices. The tray test starts its own session bus, and `systemctl` and the
  editor are stand-in scripts in `tests/stand-ins`.
- The editor runs without a window, and tests click its controls by their
  accessibility labels.

A simulated device repeats what the real one did once. It cannot show how the
device handles a request nobody has seen it answer, so passing tests do not
prove that a change works on the hardware. After changing what Loginux sends
to a device, try it on the device and say what you watched in the pull
request.

### Coverage

CI measures line coverage on every push and updates the badge in the README.
To measure it yourself, run `rustup component add llvm-tools` once, then
`tools/coverage.sh`. It counts the code in `src/` and leaves out the test
code. Lines marked with lcov's `LCOV_EXCL` markers are left out as well, and
the report says how many. Two places carry them: `ui::run`, which opens the
real window, and one error arm each after `poll` and `flock`, for errors that
a valid file descriptor never returns. Add a marker only for the same kind of
reason, and write the reason next to it.

## Evidence

Say what you observed on a keyboard and what you read in software, firmware
or documentation, and keep the two apart. Layout positions from vendor
software are layout data, not proof of which LED lights. Device findings go
in `research/`, with the labels described in
[research/README.md](research/README.md).
