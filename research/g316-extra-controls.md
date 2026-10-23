# G316 controls beyond per-key RGB

These findings apply to the external G316 X 98 (`046d:c36b`, HID++ USB
interface 2) running application firmware `U186.01.B0007`. **Live** means a
Linux device query or the owner's Windows G HUB capture; **static** means the
cached firmware or vendor software. A Windows write is evidence that the
keyboard accepts the command, not proof that Loginux has exercised it.

Loginux implements `0x8071` and `0x8081` lighting, a `0x4540` layout read,
and the controls below as separate, optional HID++ features. Resolve
their indices through IRoot on every connection; the indices in the capture
are only identifiers for that firmware session.

| Control | Evidence | Safe next use | Still unknown |
| --- | --- | --- | --- |
| Lighting brightness `0x8040` | **Live:** feature index 15; function 0 returned `00 64 05 03` (maximum 100, capability bytes `05 03`); function 1 returned `00 64` (current value 100). | Read and show the current value and reported maximum. | The G316 setter's request and persistence have not been established. Do not send a brightness write based on the getter layout. |
| Report rate `0x8061` | **Live:** index 6; functions 0 and 1 reported bitmap `0x0078`; function 2 returned index 3 at 1000 Hz. Function 3 is refused with error 2 while `0x8101` profile management (index 16) is in the mode the keyboard starts in, and accepted after `0x8101/6 [05]`; Loginux set 2000, 4000 and 8000 Hz this way and read each back (2026-10-23). **Capture:** on every attach G HUB reads 1000 Hz, sends `0x8101/6 [05]`, then writes its rate. **Static:** Solaar sends `0x8101/6 [05]` when it claims a device and `[03]` when it releases it. | Change the rate on an explicit request, and send that rate again when the keyboard connects at another one, as G HUB does: `[05]`, the rate write, `[03]`. | What `0x8101/6` controls beyond the rate; Solaar's comment calls `[05]` disabling firmware power management. What made the keyboard re-enumerate once after a 2000 Hz change. |
| Game mode `0x4523` | **Live:** index 13; function 0 returned `00` while off; pressing the dedicated key produced `01` and `00` notifications. | Show current on/off state; update it on notifications or by re-reading after reconnect. | No G316 host toggle or per-key disable sequence has been validated. |
| Knob and mute press | **Live:** interface 1 sends standard HID Consumer Control usages `0x00e9` (volume up) and `0x00ea` (volume down); pressing the knob mutes. | Document that Linux's HID input and sound stack already receive these events. | Knob mode changes and host remapping are separate work; Loginux has no reason to open the key-report interface for lighting. |
| Consecutive-zone RGB `0x8081/5` | **Live, Windows:** G HUB sent up to three `(first zone, last zone, R, G, B)` records per long report and followed them with `0x8081/7` FrameEnd. The Esc-red capture included range `0x26–0x26`. | Optional transport optimization for runs of equal colors, after preserving `0x8081/1` as the proven Linux path. | Loginux has not used function 5 on Linux; it gives no new visible capability. |

## Report-rate details

The `0x0078` mask sets bits 3–6, which correspond to rate indices 3, 4, 5
and 6: 1000, 2000, 4000 and 8000 Hz. The G HUB capture sent a one-byte rate
index to `0x8061/3`; a `0x8061/2 [00]` query returns the current index. The
firmware's `hidpp_8061_f3` accepts 3–6 on its normal branch. A rate change
made the USB device disconnect and re-enumerate in the Windows capture. The
engine must treat that disconnect as expected for an explicit change, then
rediscover interface 2 and reapply any active RAM-only lighting profile. A
failed reconnect is an error to show, not a reason to loop rate writes.

The keyboard refuses a rate write while `0x8101` profile management is in
the mode it starts in (live, 2026-10-23). In that mode `0x8061/3` with index 4,
5 or 6 returned error 2 (invalid argument) at once, on the same connection, in
short and long reports alike. The Windows capture shows what G HUB does each
time the keyboard attaches: it reads `0x8101/6` (`[0f]` and `[00]`, reply
`03 00 01`), sends `0x8101/6 [05]` (reply `00 00 01`), and only then writes the
rate. Solaar sends `[05]` when it claims a device for software control and
`[03]` when it releases it; on a device with `0x8100` it switches to host mode
and back to onboard mode at those points instead. Loginux sends `[05]` before
the rate write and `[03]` right after it. With that, the keyboard accepted
2000, 4000 and 8000 Hz, and each read back after `[03]`. Loginux does not read
`0x8101/6` back, so the claim that `[03]` restores the starting state rests on
Solaar's pairing and on G HUB reading `03` at attach.

The rate is not saved on the keyboard. An 8000 Hz rate held through a lighting
release and an engine restart and was still set twelve minutes after the
write. About two and a half minutes after a change to 2000 Hz the keyboard
re-enumerated, cause not established, and came back at 1000 Hz. The Windows
capture agrees: each time the keyboard attached it reported 1000 Hz, and G HUB
wrote its chosen rate again. Loginux does the same with the rate the user
chose, as described below. A replug on Linux confirmed both halves (live,
2026-10-23): with 8000 Hz kept, the keyboard came back at another rate, and
the engine sent 8000 Hz again as it connected and read it back 5 s later,
with no further re-enumeration. Making a rate permanent on the keyboard would
mean writing the onboard profile, which Loginux does not do.

Rate selection is an explicit user action, and a rate is not part of a lighting
profile. The first plan ruled out any automatic rate write at startup, login or
replug. Once the keyboard turned out to forget its rate on every reconnect, the
owner chose G HUB's behaviour instead (2026-10-23): the engine keeps the rate
the user chose and sends it again when the keyboard connects at another rate,
and never sends a rate the keyboard refused. A send the keyboard drops off
within a minute of looks like the write making it leave; after three of those
in ten minutes the engine stops sending until the user chooses a rate, so it
cannot loop. Ordinary replugs do not count. Before a live Linux verification,
record the current index and agree on the intended value with the owner.

## Brightness and game mode

`0x8040/0` describes brightness, whereas `0x8040/1` reads the current value.
The captured brightness change in G HUB does not yet identify a request that
can safely be reproduced as a G316 brightness setter. Treat `0x8040` as
read-only until the setter, range rules and persistence are tied to a packet
and a controlled visible result. A brightness of zero can keep a valid
`0x8081/7` frame from appearing; the firmware check is described in
[hidpp20-lighting.md](hidpp20-lighting.md).

Solaar treats `0x8040` event 0 as a new brightness (two bytes, big-endian)
and event 1 as the lighting switched on or off, after which it reads the
brightness again (static). Loginux reads the brightness again on either.
Whether the G316 sends them when its knob changes the brightness has not been
observed; the two-second read covers a keyboard that does not.

`0x4523/0` reports game-mode state. The keyboard's game-mode key changes that
state without Loginux. Read it for status; do not infer a host toggle from
the notification format. The existing Loginux engine discards unsolicited
reports while matching HID++ replies, so notification display requires a
deliberate transport change or a bounded status poll.

## Evidence and boundaries

The Windows capture stays private because it contains keystrokes; the
captured command and response shapes are summarized here. The firmware
conclusions come from a private analysis of the cached image, which is not
published. Device information and software-derived conclusions remain
distinct from a watched Linux result.

## In Loginux

- `loginux status` and the editor's DEVICE tab show brightness, game mode and
  the report rate when the keyboard reports them. Loginux resolves each
  feature by ID on every connection, reads it then and every two seconds,
  applies game-mode notifications as they arrive, and reads the brightness
  again at once on any `0x8040` notification.
- `loginux rate HZ`, or a confirmed choice in the DEVICE tab, sends
  `0x8061/3` once, between `0x8101/6 [05]` and `[03]` on a keyboard that has
  `0x8101`. A lost reply is treated as unknown, not failure: Loginux
  waits up to 20 s for the keyboard to return, reads the rate back with
  `0x8061/2`, and repaints the chosen profile. A rate that reads back is kept
  in `~/.config/loginux/rates/` and sent again, the same way, whenever the
  keyboard connects at another rate.
- Brightness and game mode have no writes.
- On 2026-10-23 Loginux changed the rate on Linux to 2000, 4000 and 8000 Hz
  and read each one back, and after a replug it sent the kept 8000 Hz again
  and read it back. Brightness and game mode have only been read.
