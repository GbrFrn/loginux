# Logitech HID++ 2.0 lighting

How Loginux drives Logitech lighting, written for anyone working on another
Logitech device. Labels: **live** means sent to or read from a device;
**static** means read from firmware, vendor software or public source code;
**inference** means reasoning that no observation backs yet. Unless a line
names another device, the live observations come from a G316 X 98
([g316-x-98.md](g316-x-98.md)).

## Transport

- HID++ runs over the device's vendor HID interface. On Linux that interface
  is one hidraw node; a G316 X 98 on its own cable exposes it as USB
  interface 2. (live)
- Requests are long reports of 20 bytes:
  `11 <device> <feature index> <function << 4 | software id> <16 parameter bytes>`.
  Short reports (`10`, 7 bytes) exist too, and replies to them can arrive as
  long reports. (live)
- `<device>` is `ff` for a device on its own cable. A device paired to a
  receiver answers under its slot number, 1 to 6. The Linux DJ driver gives
  each paired device its own hidraw node; the node's `HID_PHYS` ends in `:N`,
  the slot. (static, kernel sources; the slot suffix is live on a G903)
- The reply repeats the feature index, the function and the software ID.
  Notifications carry software ID 0, and the kernel's hid-logitech-hidpp
  driver uses 1, so a user program should pick 2 to 15 and match all three.
  (static)
- Errors come back as `11 <device> ff <feature index> <function|swid> <code>`.
  Code 8 (busy) means "try again"; Loginux retries for up to a second. (live)

## Finding features

Feature indices differ between devices and firmware versions, so resolve them
on every connection:

- `0x0000` IRoot, function 0 `getFeature(feature ID, big-endian)` returns
  the index, or 0 when the device lacks the feature. (live)
- `0x0000` function 1 `getProtocolVersion(0, 0, ping)` returns
  `major, minor, ping`. (live)

## 0x8071 RGB effects

- Function 0 `getInfo`:
  - `[ff ff 00]`: byte 2 is the number of lighting clusters.
  - `[cluster ff 00]`: byte 4 is that cluster's number of effects.
  - `[cluster i 00]`: bytes 2 and 3 are effect `i`'s ID; `0x0001` is Static. (live)
- Function 5, software control:
  - `[00]` reads `?, mode, flags`.
  - `[01 mode flags]` sets them.
  - Mode bit 0 hands the effects to the host and bit 1 hands over power
    management. (live, and static from Solaar)
  - Releasing bit 0 makes the firmware reload its onboard effects: the
    lighting came back on the G316. (live)
- Function 1 `setEffect`: `[cluster, effect index, 10 parameter bytes, persistence]`.
  - For Static the parameters are `R G B` followed by seven zeros.
  - It fails with error 5 unless mode bit 0 is set. (live)
  - In the G316 firmware, persistence bit 0 applies the effect, bit 1 saves it
    to flash, and bits 2 and 3 select a power state. Loginux always sends
    `01`, so nothing is saved. (static)

## 0x8081 per-key lighting (v2)

- Function 0 `getInfo([00 page])` returns `?, page, 14-byte bitmap` for pages
  0, 1 and 2. Bit `b` of byte `i` on page `p` means zone `p * 112 + i * 8 + b`
  exists. (live)
- Function 1 `setIndividualRgbZones` takes four `zone R G B` records. A zone
  of `ff` skips the record. The reply repeats the zone IDs. This only stages
  colours. (live)
- Function 5 sets one colour on a range of zones, three ranges per report:
  `start end R G B`, then one unused byte. The reply repeats the start
  zones. G HUB uses it. (live, from a Windows capture of G HUB)
- Function 7 `frameEnd([00 00 00 00 00])` draws the staged frame. It returns
  busy until the previous frame has been taken. In the G316 firmware it draws
  only while the brightness is above zero. (live; the brightness detail is
  static)
- On the keyboards seen so far, a key's zone is its HID keyboard usage minus
  3, and the modifiers are 104 to 111 (left Ctrl, Shift, Alt, Meta, then the
  right ones). Esc is zone 38. Extra lights use higher zones. (live on the
  G316; static from Solaar's key table for other Logitech keyboards)

## A sequence that hands the lighting back

1. Read software control and remember it.
2. Write `[01 01 00]`: take the effects, leave power management to the
   firmware. Wait about 50 ms.
3. Set Static, persistence `01`, on every cluster.
4. Stage the frame and commit it with frameEnd. Send the first frame twice,
   because the first frame after a claim can be dropped; Solaar does the
   same.
5. From then on, send only changed frames.
6. To stop, write back the remembered mode with bit 0 cleared. The firmware
   reloads its onboard effects, and unplugging the device resets everything
   anyway.

Every write in this sequence goes to RAM. (live on the G316)

## Requests to avoid

These features reset, reflash or flood the device. Never call them while
experimenting (from the G316 sessions; the IDs are standard HID++ features):

- `0x00c3` and `0x00d0`: firmware update
- `0x1801`: manufacturing mode
- `0x1802`: device reset
- `0x1805`: out-of-box state, which is a factory reset
- `0x18b0`: monitor mode, which floods reports
- `0x1e02` and `0x1602`: managing deactivatable features and password
  authentication; engineering features sit behind them

Changing the report rate through `0x8061` makes the USB device re-enumerate,
so its hidraw node disappears and comes back. (live on the G316 under
Windows)
