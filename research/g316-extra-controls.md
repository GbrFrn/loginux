# G316 controls beyond per-key RGB

These findings apply to the external G316 X 98 (`046d:c36b`, HID++ USB
interface 2) running application firmware `U186.01.B0007`. **Live** means
sent to or read from the keyboard, or watched on it, on Linux or in the
owner's Windows G HUB capture; **static** means the cached firmware, vendor
software or public source code such as Solaar. A Windows write is evidence
that the keyboard accepts the command, not proof that Loginux has
exercised it.

Loginux implements `0x8071` and `0x8081` lighting, a `0x4540` layout read,
and the brightness, report-rate and game-mode rows below as separate,
optional HID++ features; it sends nothing for the knob and does not use
`0x8081/5`. Resolve their indices through IRoot on every connection; the
indices in the capture are only identifiers for that firmware session.

| Control | Evidence | Safe next use | Still unknown |
| --- | --- | --- | --- |
| Lighting brightness `0x8040` | **Live:** feature index 15; function 0 returned `00 64 05 03` (maximum 100, then `05 03`, which Solaar reads as 5 steps and capability flags `03`, static); function 1 returned `00 64` (current value 100). | Read and show the current value and reported maximum. | The G316 setter's request and persistence have not been established. Do not send a brightness write based on the getter layout. |
| Report rate `0x8061` | **Live:** index 6; functions 0 and 1 reported bitmap `0x0078`; function 2 returned index 3 at 1000 Hz. Function 3 is refused with error 2 while `0x8101` profile management (index 16) is in the mode the keyboard starts in, and accepted after `0x8101/6 [05]`; Loginux set 2000, 4000 and 8000 Hz this way and read each back (2026-10-23). **Live, Windows:** on every attach G HUB reads 1000 Hz, sends `0x8101/6 [05]`, then writes its rate. **Static:** Solaar sends `0x8101/6 [05]` when it claims a device and `[03]` when it releases it. | Change the rate on an explicit request, and send that rate again when the keyboard connects at another one, as G HUB does: `[05]` and the rate write as in the capture, then `[03]` as Solaar does on release. | What `0x8101/6` controls beyond the rate; Solaar's comment calls `[05]` disabling firmware power management. Why the keyboard re-enumerates on its own (once at 2000 Hz and eight times at 8000 Hz on 2026-10-23, and nine more times between 23:06 that day and 03:24 the next with 8000 Hz kept; once its HID++ interface did not bind, usbhid error -71, until a replug). Why it kept 8000 Hz through every reconnect after 10:02 that day. |
| Game mode `0x4523` | **Live:** index 13; function 0 returned `00` while off; pressing the dedicated key produced `01` and `00` notifications. | Show current on/off state; update it on notifications or by re-reading after reconnect. | No G316 host toggle or per-key disable sequence has been validated. |
| Knob and mute press | **Live:** interface 1 sends standard HID Consumer Control usages `0x00e9` (volume up) and `0x00ea` (volume down); pressing the knob mutes. | Document that Linux's HID input and sound stack already receive these events. | Knob mode changes and host remapping are separate work; Loginux has no reason to open the key-report interface for lighting. |
| Consecutive-zone RGB `0x8081/5` | **Live, Windows:** G HUB sent up to three `(first zone, last zone, R, G, B)` records per long report and followed them with `0x8081/7` FrameEnd. The Esc-red capture included range `0x26–0x26`. | Optional transport optimization for runs of equal colours, after preserving `0x8081/1` as the proven Linux path. | Loginux has not used function 5 on Linux; it gives no new visible capability. |

## Report-rate details

The `0x0078` mask sets bits 3–6, which correspond to rate indices 3, 4, 5
and 6: 1000, 2000, 4000 and 8000 Hz. The G HUB capture sent a one-byte rate
index to `0x8061/3` (live, Windows); a `0x8061/2 [00]` query returns the
current index (live). The firmware's `hidpp_8061_f3` accepts 3–6 on its
normal branch (static). A rate change made the USB device disconnect and
re-enumerate in the Windows capture (live, Windows). The engine must treat
that disconnect as expected for an explicit change, then rediscover
interface 2 and reapply any active RAM-only lighting profile. A failed
reconnect is an error to show, not a reason to loop rate writes.

The keyboard refuses a rate write while `0x8101` profile management is in
the mode it starts in (live, 2026-10-23). In that mode `0x8061/3` with index 4,
5 or 6 returned error 2 (invalid argument) at once, on the same connection, in
short and long reports alike. The Windows capture shows what G HUB does each
time the keyboard attaches: it reads `0x8101/6` (`[0f]` and `[00]`, reply
`03 00 01`), sends `0x8101/6 [05]` (reply `00 00 01`), and only then writes the
rate. Solaar sends `[05]` when it claims a device for software control and
`[03]` when it releases it; on a device with `0x8100` it switches to host mode
and back to onboard mode at those points instead (static). Loginux sends
`[05]` before the rate write and `[03]` right after it. With that, the
keyboard accepted 2000, 4000 and 8000 Hz, and each read back after `[03]`.
Loginux does not read `0x8101/6` back, so the claim that `[03]` restores the
starting state rests on Solaar's pairing and on G HUB reading `03` at attach.

Whether the keyboard keeps its rate across a reconnect is not settled (live,
2026-10-23). An 8000 Hz rate held through a lighting release and an engine
restart and was still set twelve minutes after the write. About two and a
half minutes after a change to 2000 Hz the keyboard re-enumerated, cause not
established, and came back at 1000 Hz. After a replug at 10:02, with 8000 Hz
kept, it came back at another rate; the engine sent 8000 Hz again as it
connected and read it back 5 s later, with no further re-enumeration. Through
a replug at 11:26 and eight re-enumerations of its own between 11:01 and
12:37 it stayed at 8000 Hz, and the engine sent nothing. In the Windows
capture each attach reported 1000 Hz, and G HUB wrote its chosen rate again;
Loginux does the same with the rate the user chose, as described below.
Loginux sends nothing meant to save a rate on the keyboard.

The keyboard went on re-enumerating with 8000 Hz kept (live, 2026-10-23 and
2026-10-24). Between 23:06 and 03:24 the kernel logged nine disconnects, each
followed by a new device 0.29 to 0.37 s later; the keyboard was behind a USB
2.0 hub (`05e3:0608`). The first eight came while the service ran Loginux
0.1.6, which has no Claude Code display: each time the engine logged
`No such device (os error 19)`, reconnected, took the lighting again and
logged no rate restore. The ninth, at 03:24:07, came while Loginux 0.1.7 drew
Claude Code's compacting pattern on the light bar during a demo. The engine
logged `Protocol error (os error 71)`, the HID++ interface bound again, and
the engine reconnected and took the lighting again. Cause not established.

Rate selection is an explicit user action, and a rate is not part of a lighting
profile. The first plan ruled out any automatic rate write at startup, login or
replug. Once a reconnect brought the keyboard back at another rate, the owner
chose G HUB's behaviour instead (2026-10-23): the engine keeps the rate the
user chose and sends it again when the keyboard connects at another rate. It
skips a rate the keyboard has refused; `loginux rate HZ` still sends one when
asked. When the keyboard drops off within a minute of such a send, the write
may have made it leave; after three of those in ten minutes the engine stops
sending until the user chooses a rate, so it cannot loop. Ordinary replugs do
not count. Before a live Linux verification, record the current index and
agree on the intended value with the owner.

## Brightness and game mode

`0x8040/0` describes brightness, whereas `0x8040/1` reads the current value.
The captured brightness change in G HUB does not yet identify a request that
can safely be reproduced as a G316 brightness setter. Treat `0x8040` as
read-only until the setter, range rules and persistence are tied to a packet
and a controlled visible result. A brightness of zero can keep a valid
`0x8081/7` frame from appearing (static); the firmware check is described in
[hidpp20-lighting.md](hidpp20-lighting.md).

Solaar treats `0x8040` event 0 as a new brightness (two bytes, big-endian)
and event 1 as the lighting switched on or off; after "on" it reads the
brightness again, and after "off" it shows 0 (static). Loginux reads the
brightness again on either. Whether the G316 sends them when its knob
changes the brightness has not been observed; the two-second read covers a
keyboard that does not.

`0x4523/0` reports game-mode state. The keyboard's game-mode key changes that
state without Loginux. Read it for status; do not infer a host toggle from
the notification format. Loginux keeps the notifications that arrive between
replies (the 32 newest), applies game-mode ones as they come, and reads the
state again every two seconds.

## Evidence and boundaries

The Windows capture stays private because it contains keystrokes; the
captured command and response shapes are summarized here. The firmware
conclusions come from a private analysis of the cached image, which is not
published. Device information and software-derived conclusions remain
distinct from a watched Linux result.

## In Loginux

- `loginux status` and the editor's SETTINGS page show brightness, game
  mode and the report rate when the keyboard reports them. Loginux resolves
  each feature by ID on every connection, reads it then and every two
  seconds, applies game-mode notifications as they arrive, and reads the
  brightness again at once on any `0x8040` notification.
- `loginux rate HZ`, or a confirmed choice under SETTINGS, sends
  `0x8061/3` once, between `0x8101/6 [05]` and `[03]` on a keyboard that has
  `0x8101`. A lost reply is treated as unknown, not failure: Loginux
  waits up to 20 s for the keyboard to return, reads the rate back with
  `0x8061/2`, and repaints the chosen profile. A rate that reads back is kept
  in `~/.config/loginux/rates/` and sent again, the same way, whenever the
  keyboard connects at another rate.
- Brightness and game mode have no writes.
- On 2026-10-23 Loginux changed the rate on Linux to 2000, 4000 and 8000 Hz
  and read each one back. After the 10:02 replug it sent the kept 8000 Hz
  again and read it back; the 11:26 replug came back at 8000 Hz with nothing
  sent. Brightness and game mode have only been read.
