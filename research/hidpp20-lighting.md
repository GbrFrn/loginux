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

Logitech's specification of this feature, "x8071 - RGB Effects" version 4,
is a public PDF; OpenLogi's notes on `0x8071` link it. Field meanings marked
static below come from it.

- Function 0 `getInfo`:
  - `[ff ff 00]`: byte 2 is the number of lighting clusters, bytes 3 and 4
    the non-volatile capabilities (see function 3), bytes 5 and 6 extended
    capabilities. (live; meanings static)
  - `[cluster ff 00]`: bytes 2 and 3 are the location (`0x0001` primary,
    `0x0002` logo, and so on) and byte 4 the number of effects. (live)
    Byte 5 lists the cluster's event-display modes (`01` always on, `02`
    always off, `04` on for a while after an event) and is `00` for a plain
    lighting cluster. Byte 6 says whether effects can be saved to EEPROM, and
    byte 7 whether function 2 may set which of the cluster's LEDs are on.
    (static)
  - `[cluster i 00]`: bytes 2 and 3 are effect `i`'s ID; `0x0001` is Static. (live)
- Function 5, software control:
  - `[00]` reads `00, control, events`.
  - `[01 control events]` sets them.
  - Control bit 0 hands every cluster to the host and bit 1 hands over power
    management. (live, and static from Solaar and the specification)
  - The events byte only turns on notifications: `01` effect sync, `02` user
    activity, `04` no-activity timeout. It does not change what the LEDs
    show. (static)
  - Releasing bit 0 makes the firmware reload its onboard effects: the
    lighting came back on the G316, and the G903 reads back `00 00`. (live)
- Function 1 `setEffect`: `[cluster, effect index, 10 parameter bytes, persistence]`.
  - For Static the parameters are `R G B` followed by seven zeros.
  - It fails with error 5 unless control bit 0 is set. (live)
  - Persistence bit 0 applies the effect in RAM and bit 1 saves it to
    EEPROM; bits 2 and 3 pick the power mode it is for (0 full power, 1 power
    save). Loginux always sends `01`, so nothing is saved. (static, from the
    G316 firmware and the specification)
- Function 3 `manageNvConfig` reads or writes one non-volatile setting.
  `[00 capability]` reads it and returns the capability, its state and its
  parameters; `[01 ...]` writes EEPROM, and Loginux refuses to send that
  form. Capabilities: `0x0001` boot-up effect, `0x0002` demo, `0x0004` user
  demo, `0x0008` events display (such as DPI status), `0x0010` active
  dimming, `0x0020` ramp down to off, `0x0040` shutdown effect. (static;
  the get form is live on the G903)

### Indicator clusters

A cluster with a non-zero byte 5 is an event display, such as the three DPI
LEDs of a G903: several LEDs share one colour, and the firmware decides which
of them are on. Claiming software control stops the firmware's handling of
every cluster. When byte 7 is `00`, the host also has no way to switch the
LEDs on, and on the G903 they stay dark whatever effect the host sets. (live
on the G903; the cause is inference, see [g903.md](g903.md))

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

These write non-volatile memory, which Loginux never does:

- `0x8071` function 3 in its set form (`01 ...`): non-volatile lighting
  settings
- `0x8071` function 4 in its set form: the factory colour calibration of
  each cluster's LEDs
- `0x8100` memory writes: onboard profiles, the way libratbag changes
  lighting on `0x8071` mice

Changing the report rate through `0x8061` makes the USB device re-enumerate,
so its hidraw node disappears and comes back. (live on the G316 under
Windows)
