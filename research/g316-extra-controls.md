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
| Report rate `0x8061` | **Live:** index 6; functions 0 and 1 reported bitmap `0x0078`; function 2 returned rate index 3 at 1000 Hz and 6 at 8000 Hz; G HUB sent function 3 with 6, then 3. **Static:** the cached handler accepts indices 3–6 in its ordinary path. | Read supported/current rates; offer an explicit selection of 1000, 2000, 4000 or 8000 Hz after implementing reconnect handling. | The capture proves the 1000/8000 transitions, not a Linux write or how long the setting persists. |
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

Rate selection must be an explicit user action. Do not put it in a lighting
profile or send it automatically at startup, login, or after every replug.
The current rate may be a saved device setting; its persistence has not been
measured. Before a live Linux verification, record the current index and
agree on the intended value with the owner.

## Brightness and game mode

`0x8040/0` describes brightness, whereas `0x8040/1` reads the current value.
The captured brightness change in G HUB does not yet identify a request that
can safely be reproduced as a G316 brightness setter. Treat `0x8040` as
read-only until the setter, range rules and persistence are tied to a packet
and a controlled visible result. A brightness of zero can keep a valid
`0x8081/7` frame from appearing; the firmware check is described in
[hidpp20-lighting.md](hidpp20-lighting.md).

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
  feature by ID on every connection, reads it then and every ten seconds,
  and applies game-mode notifications as they arrive.
- `loginux rate HZ`, or a confirmed choice in the DEVICE tab, sends
  `0x8061/3` once. A lost reply is treated as unknown, not failure: Loginux
  waits up to 20 s for the keyboard to return, reads the rate back with
  `0x8061/2`, and repaints the chosen profile. Nothing sends a rate on its own.
- Brightness and game mode have no writes.
- As of 2026-10-22 Loginux has read these settings on Linux but has not sent
  a report-rate change.
