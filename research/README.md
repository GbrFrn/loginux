# Research

Notes on Logitech devices, for anyone adding a device to Loginux or writing
their own tool.

- [hidpp20-lighting.md](hidpp20-lighting.md): the HID++ 2.0 lighting
  protocol as Loginux uses it, with the requests to avoid.
- [g316-x-98.md](g316-x-98.md): the G316 X 98: feature table, zones, light
  bar and the dot screen.
- [g903.md](g903.md): the G903 mouse: features, its two lighting clusters,
  and why its DPI indicator stays dark under host control.
- [g316-extra-controls.md](g316-extra-controls.md): G316 brightness, report
  rate, game mode and media keys: the evidence, the write boundary for each,
  and what Loginux does with them.

## Evidence labels

Every claim carries one of three labels:

- **live**: sent to or read from a device, or watched on it.
- **static**: read from firmware, vendor software or public source code.
- **inference**: reasoning that no observation backs yet.

Layout coordinates from vendor software are static. They say where the
vendor draws a key, not which LED a zone lights. A channel check in the
Loginux editor turns them into live evidence.

## Mapping a new device

1. `loginux probe --device /dev/hidrawN` reads the device's HID++ version,
   its lighting features and the zones it accepts, and prints a starting
   definition: keys for a device with `0x8081` per-key lighting, else one
   light per lighting cluster. It sends read requests only.
2. Run a channel check in the editor. Loginux lights one zone at a time, and
   you record what lit. The answers are saved as live evidence for that
   model.
3. Note each finding with its label in a device page here.

## Watching the vendor software

On Windows, USBPcap and Wireshark show what G HUB sends. HID++ requests start
with `10` or `11`; the filter `usb.capdata[0] == 0x11` keeps the long ones.

A capture records every report on the selected root hubs, keystrokes
included. Keep raw captures private and publish only the HID++ packets you
quote.

## Ground rules

- Write lighting to RAM only. Never send anything that saves to flash,
  updates firmware, resets the device or unlocks engineering features. A
  device setting can outlast a replug, as the G316's report rate has; write
  one only at the user's request (see
  [g316-extra-controls.md](g316-extra-controls.md)). The list of requests to
  avoid is in [hidpp20-lighting.md](hidpp20-lighting.md).
- Record a device's state before changing it, and restore it.
- Don't publish firmware images, decompiled code or vendor files. Describe
  behaviour in your own words and say which version you looked at.
