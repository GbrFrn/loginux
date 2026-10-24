# Using Loginux

The README has the short version; this page has the rest.

- [Install from source](#install-from-source)
- [The editor](#the-editor)
- [The tray icon](#the-tray-icon)
- [Notifications](#notifications)
- [Claude Code status](#claude-code-status)
- [Command line](#command-line)
- [Profiles](#profiles) and [effects](#effects)
- [Device settings](#device-settings)
- [Run at login](#run-at-login)
- [Getting the onboard lighting back](#getting-the-onboard-lighting-back)
- [How it talks to the device](#how-it-talks-to-the-device)
- [Files](#files)
- [What has been tested](#what-has-been-tested)
- [Credits](#credits)

## Install from source

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

Run `loginux` (or `loginux ui`). The window shows the board with every key
and bar segment in its colour. Click a key, shift-click to add more, or drag
across the board, then set the colour with the picker, the hex field or the
RGB values. The keyboard follows while you paint. SAVE writes the profile;
USE THIS PROFILE makes it the one Loginux keeps applied. RELEASE LIGHTING
hands the keyboard back to its onboard effects and unsets the chosen profile.
The first time it starts, the editor creates an empty profile, My lighting,
and opens it, so there is a board to paint right away.

The lettering is drawn on whole device pixels, so it stays sharp at any
display scale; `LOGINUX_UI_ZOOM=1.25 loginux` enlarges the window's contents
beyond the display's own scale.

The window never opens the keyboard itself. If no Loginux engine is running, it
starts one inside the window, and that engine hands the lighting back when the
window closes. To keep a profile after the window closes and at every login,
turn on RUN AT LOGIN (it needs the service installed, see below).

The channel check, under SETTINGS (OPEN CHANNEL CHECK), lights one zone at a
time, white, with everything else dark. You click where the light appeared, or
answer that nothing lit or that it lit somewhere not on the map. Answers go to
`~/.config/loginux/checks/KEYBOARD.toml` and show on the check's board as
marks: green where the LED matched the map, amber where it did not. Until a
zone is checked, its position is layout data and not proof.

## The tray icon

While `loginux run` runs, Loginux shows a tray icon. It speaks the
StatusNotifierItem protocol: KDE Plasma shows it, and GNOME does with the
AppIndicator extension. The menu names the device and the profile in use,
switches profiles (Onboard Lighting hands the lighting back), opens the editor
(so does a left click), and Quit stops the service until the next login,
handing the lighting back.

## Notifications

The engine tells the desktop when something changes that you may not be
watching:

- a profile chosen from the tray, the editor or the command line, or the
  lighting handed back to the keyboard;
- a report-rate change you asked for, read back or refused;
- a kept report rate that could not be set again after a reconnect (one that
  could is not announced, since that happens at every login and replug);
- the lighting taken by another program.

The profile in use when the engine starts is not announced, and a new
notification replaces the last one. They are sent through
`org.freedesktop.Notifications` with `busctl`, which comes with systemd. To
turn them off, find Loginux in the desktop's notification settings (on KDE
Plasma: System Settings, Notifications, Configure for Applications).

## Claude Code status

Loginux can turn the light bar into a status line for Claude Code. It is
optional and off until you turn on SHOW ON THE LIGHT BAR under SETTINGS, or
run `loginux claude --enable`. That installs Loginux's plugin in Claude Code
(`claude plugin install loginux-status --marketplace GbrFrn/loginux`), whose
hooks report what Claude Code does, and puts Loginux in front of your status
line, which is the only place Claude Code reports context and usage limits.
Your status line still runs and shows as before. Turning the switch off, or
`loginux claude --disable`, turns the plugin off, leaving it and its
marketplace installed, and puts your status line command back as it was.
Both are written to survive Loginux being uninstalled first: the hooks then
do nothing, and your status line runs on its own.

Each running Claude Code gets its own share of the light bar, from the moment
it starts until its process ends. One instance has all 30 segments, two have
15 each, four have 7, 8, 7 and 8, and thirty have one each; an instance beyond
that waits for a segment. A share shows its instance's state as pixel art, 12.5
frames a second:

| Claude Code | Its share of the light bar |
| --- | --- |
| Open, before the first prompt | dim orange, lit from its middle, with a heartbeat every 3 seconds |
| Thinking | an orange pulse spreads from the middle and back |
| Running a tool | orange pairs run out from the middle |
| Compacting the conversation | violet, squeezing in from both ends |
| Waiting for you: a permission, a question, a plan or an MCP server's form | amber lights run in from both ends to a blinking middle |
| Finished, waiting for your next prompt | fills green from both ends and stays lit, a white glint crossing it every 6 seconds |
| Stopped on an API error, or on a usage limit | a broken red line that jumps a step every 3 seconds |
| Closed | flashes white, then shrinks to its middle |

A subagent counts as part of its instance, and the share shows whichever of
them most wants you: waiting, then an error, finished, a tool, compacting and
thinking. Your next prompt, or the end of the turn, settles what the subagents
were doing; one still running comes back with its next event.

The context and the usage limits show on the bar too:

| Situation | On the light bar | On the keys |
| --- | --- | --- |
| The context fills up | from 70 %, violet walls close in on the instance's share, a third of it at most at 95 % | the C key and the number row, filled to the tenths used, at 90 % |
| The 5-hour limit | the bar's left end lamp: amber from 80 %, red and blinking from 95 %, red once reached | the H key and the number row at 80, 90 and 95 % |
| The weekly limit, or a Claude apps gateway's spend limit | the right end lamp, the same way | the W or S key and the number row |
| A turn stopped on a usage limit | both lamps red until Claude works again | a cross |

The keys keep the chosen profile. An effect pauses on a still frame of itself,
a rainbow across the board for Wave and Colour Cycle, and runs again once no
Claude Code is open. For about a second and a half the keys go dark and show
a sprite, one key to a pixel:

- a new instance: Claude Code's spinner turns once in the middle of the keys;
- a turn that waits for you, or finishes, at least 30 seconds after you last
  typed a prompt or answered: an exclamation mark, or a check mark;
- an API error: a cross that lands with a glitch;
- the context or a limit crossing a mark: the gauges above.

No sprite starts within ten seconds of another. With no profile chosen,
Loginux leaves the lighting to the keyboard and shows nothing, and an edit in
the editor and the channel check show over the display.

What Claude Code reports, and what Loginux makes of it:

| Claude Code event | Shown as |
| --- | --- |
| `SessionStart` | a new instance, idle (not after `/compact`) |
| `UserPromptSubmit`, `PostToolUse`, `PostToolUseFailure`, `PostCompact` (automatic) | thinking |
| `PreToolUse` | running a tool, or waiting for you for `AskUserQuestion` and `ExitPlanMode` |
| `PermissionRequest`; `Notification` for a permission prompt or an MCP form | waiting for you |
| `Stop` | finished |
| `Notification` `idle_prompt`; `PostCompact` after `/compact` | finished, with no sprite; an error, or an instance with no turn yet, stays as it is |
| `StopFailure` | stopped on an error; on a usage limit if its error is `rate_limit` |
| `Notification` `quota_auto_resume_fired` | thinking: Claude Code resumed after a limit reset |
| `PreCompact` | compacting |
| `SubagentStop` | the subagent is done |
| `SessionEnd` | the instance closes (not for `/clear` and `/resume`, which go on in the same instance) |
| The status line's `context_window` and `rate_limits` | the context walls, the end lamps and the gauges |

Runs without a person at the terminal, `claude -p` and the Agent SDK, are left
out. An interrupt (Esc) sends no event, so a share keeps its state until the
next one, such as Claude Code's notice a minute later that it is waiting for
input; thinking
or a tool with no event for 15 minutes shows as idle. Claude Code tells the
hook which process it runs in through `CLAUDE_PID`; without it, an instance
is its session and drops off after 30 minutes without an event.

A hook writes nothing to standard output, which Claude Code would read, and
always exits successfully. The engine answers it before drawing, and a hook
gives up on the engine after a second; without an engine running, there is
nothing to show. Counted from the frames the display draws, a moving share
sends at most 6 HID++ reports a frame, about 25 a second while Claude thinks,
nothing between the glints of a finished one, and a sprite at most a full
frame of 33. Its cost to the service has not been measured on the keyboard yet.

LEDs do not show a colour the way a screen does. To change the colours, write
`~/.config/loginux/claude.toml`; the display reads it at the next event, and a
value it cannot read keeps its default:

```toml
working = "#ff5a14"      # open, thinking and tools
compacting = "#963cff"   # compacting and the context walls
asking = "#ffb400"       # waiting for you, and limits from 80 %
done = "#28ff5a"
error = "#ff0000"        # errors, and limits from 95 %
```

`loginux claude EVENT` sets the display by hand, for an instance named
`manual` unless `--session` names another. EVENT is `start`, `prompt`,
`working`, `tool`, `compact`, `asking`, `done`, `waiting`, `error`, `limit`
or `end`, which closes the instance. `loginux status` lists each instance's state, in
their order on the bar.

## Command line

| Command | What it does |
| --- | --- |
| `loginux zones` | Lists key names, `bar1`–`bar30` and the unknown zones with their IDs |
| `loginux status` | Shows the keyboard, who holds the lighting and the chosen profile |
| `loginux use NAME` | Chooses the profile Loginux keeps applied (`--none` to stop) |
| `loginux apply NAME [--hold S \| --keep]` | Paints a profile for S seconds (default 10), then hands back |
| `loginux paint esc=ff0000 bar3=00ff00 [--base RRGGBB]` | Paints single zones; takes the same `--hold`/`--keep` |
| `loginux effect CLUSTER=EFFECT[,param=value]... [--hold S \| --keep]` | Runs one of the device's effects (static, breathe, cycle, wave, ripple) on a cluster, by number or light name |
| `loginux release` | Hands the lighting back to the keyboard now; through a running engine it also unsets the chosen profile, like `use --none` |
| `loginux rate [HZ]` | Shows the report rate, or changes it, reads it back and keeps it (the keyboard may reconnect) |
| `loginux run [--refresh S]` | The engine: keeps the profile applied; the service runs this |
| `loginux probe --device /dev/hidrawN` | Read-only: prints a starting definition for a new device |
| `loginux claude --enable` or `--disable` | Connects Claude Code to the light bar, or disconnects it: [Claude Code status](#claude-code-status) |
| `loginux claude --hook`, `--statusline` or `EVENT` | What Claude Code's hooks and status line run, or a state set by hand |

`--device /dev/hidrawN` picks the node by hand; Loginux checks that it really
is the HID++ interface of a known device before sending anything, except
`probe`, which sends only getters and takes any USB hidraw node. `apply`,
`paint` and `effect` refuse to run while an engine owns the keyboard, and they
check every name and colour before the first write.

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
strip gets a table named after it (`[bar]` on the G316), and lights that are
not keys share a `[lights]` table: a G903 profile sets `logo` there and has no
`[keys]`. Zones with no known key (0, 47, 99–103 and 154 on the G316) stay
dark unless `[zones]` names them. Hand edits to the chosen profile apply
within a second while the engine runs.

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

Ripple takes `period` as well, on its own scale: Solaar allows 2 to 200, and
the preset uses 20.

Four presets are built in: Breathing, Colour Cycle, Wave and Ripple. They
show in the editor (marked BUILT IN), in the tray's Profile menu and to
`loginux use`. In the editor, LIGHTING switches the open profile between its
colours and an effect; a changed preset is saved under a new name. A cluster
without the chosen effect shows the effect's colour instead, and a device
with none of it refuses the profile: the G903 has no wave or ripple.

## Device settings

The editor's SETTINGS page and `loginux status` show settings the keyboard
reports, when it reports them. Loginux reads them on every connection and
every two seconds after, and at once when the keyboard says its brightness
changed, for example from its knob.

| Setting | What Loginux does | Proof |
| --- | --- | --- |
| Lighting brightness (`0x8040`) | Shows the current value and the maximum. Read only. | Read on Linux |
| Game mode (`0x4523`) | Shows on or off, and follows the keyboard's game-mode key. Read only. | Read on Linux |
| Report rate (`0x8061`) | Shows the current rate and the rates the keyboard supports. Changes it when you choose a rate, and sets that rate again after a reconnect. | Changed on Linux to 2000, 4000 and 8000 Hz and read back |
| Volume knob | Nothing: the knob sends ordinary media keys, which Linux handles. | Seen on Windows |

A report-rate change can make the keyboard leave the USB bus and come back: the
G316 X 98 did under Windows, and did not for 4000 and 8000 Hz under Linux.
Loginux sends the new rate once, waits up to 20 s for the keyboard if it
leaves, reads the rate back, and repaints the chosen profile; a change that
fails is not tried again. If the keyboard does not return or reports another
rate, the editor and the command line say so. On a keyboard with profile
management (`0x8101`), Loginux switches it to host mode for the write and back
to onboard mode right after, the switch Solaar makes when it claims and
releases a device; G HUB makes the same host switch before its rate writes. The
keyboard can come back at its own rate after a reconnect (1000 Hz on the
G316 X 98), though it has kept 8000 Hz through most reconnects so far. So,
like G HUB, Loginux keeps the rate you chose and sends it again when the
keyboard connects at another rate, never a rate the keyboard refused. If the
keyboard drops off within a minute of three such sends in ten minutes, Loginux
stops sending until you choose a rate, so a keyboard that leaves after every
write cannot loop; ordinary replugs do not count. Choosing the keyboard's own
rate stops the writes. A rate the keyboard still refuses is marked and not
offered again (kept in `~/.config/loginux/rates/`; delete that file to try it
once more). Brightness and game-mode changes are not offered: their commands
on the G316 are not verified.
[G316 extra controls](../research/g316-extra-controls.md) has the evidence.

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

The service drives one device: the one `keyboard` names in
`~/.config/loginux/config.toml`, which Loginux writes when it first connects
to a device. It waits for that device while it is unplugged and never takes
another in its place. To drive another device, set `keyboard` to its
definition, such as `g903-hero`, and restart the service. A device the service
does not drive keeps its onboard lighting. `loginux effect ... --device
/dev/hidrawN` can light it for a while by hand, but only with the service
stopped: one lock covers every device.

- Stop now, handing the lighting back: `systemctl --user stop loginux`
- Stop now and at every login: `systemctl --user disable --now loginux`
- Logs: `journalctl --user -u loginux`

## Getting the onboard lighting back

`loginux release`, RELEASE LIGHTING in the editor, or stopping the service all
hand the lighting back. The first two also unset the chosen profile; a stopped
service applies it again when it starts. If Loginux was killed without a
chance to hand the lighting back, the next `loginux release` restores the
saved values from `$XDG_RUNTIME_DIR/loginux/claim-ID`, one file per device
definition (such as `claim-g316-x-98`). Unplugging and replugging the keyboard
always resets it.

When another program holds the keyboard's lighting, Loginux does not overwrite
it: the editor shows the conflict and offers TAKE OVER, the tray menu offers
Take Over Lighting, and the command line stops with a message.

## How it talks to the device

The keyboard is found through sysfs by USB ID and interface number, and the
check is repeated on the opened file. HID++ feature indices come from the
keyboard (IRoot) on every connect. Loginux sends only these requests:

- `0x0000` IRoot: getFeature, getProtocolVersion
- `0x8071` RGB effects: getInfo, setEffect with persistence `0x01` (RAM) for
  one cluster or for all of them (`0xFF`), get and set software control (mode
  bit 0 only; the firmware keeps power management)
- `0x8081` per-key lighting: getInfo, setIndividualRgbZones, frameEnd
- `0x4540` keyboard layout, `0x8040` brightness, `0x4523` game mode: read
- `0x8061` report rate: read the supported rates and the current one; set the
  rate you choose, and set it again when the keyboard reconnects at another
  rate
- `0x8101` profile management: function 6 only, host mode (`05`) before a rate
  change and back (`03`) right after; never the profile memory
- `loginux probe` adds getters only: `0x0001` feature set, `0x8070` colour LED
  effects, `0x8100` onboard profiles, and the get form of `0x8071` function 3

A paint takes control, sets the Static effect on each lighting cluster, then
sends and commits the frame twice, since the first frame after a claim can be
dropped; a device without `0x8081` gets each cluster's Static colour instead.
An effect is sent once. When it covers every cluster and the device lists it
among the effects it can play on all clusters at once, it goes to cluster
`0xFF`, so keys and light bar run on one clock; otherwise each cluster gets
its own. After that a change stages only the zones that changed, at most about
60 frames a second while you drag a colour, and the claim is checked every two
seconds; if the keyboard reset itself, Loginux paints again. The Claude Code
status display is drawn the same way, with no other requests. A single engine
owns the keyboard at a time (a lock in `$XDG_RUNTIME_DIR/loginux`), and the
editor and the command line talk to it over a socket there.

## Files

- `~/.config/loginux/profiles/*.toml`: profiles
- `~/.config/loginux/config.toml`: the chosen profile, the keyboard last
  seen, and whether Claude Code's status shows
- `~/.config/loginux/checks/KEYBOARD.toml`: channel-check answers per keyboard
  model
- `~/.config/loginux/rates/KEYBOARD.toml`: the report rate you chose for this
  keyboard (`keep`) and the rates it refused; delete it to forget both
- `~/.config/loginux/devices/*.toml`: your own device definitions; one with
  a built-in's ID replaces it
- `~/.config/loginux/claude.toml`: colours of the Claude Code status display
- `$XDG_RUNTIME_DIR/loginux/`: lock, socket and a claim record per device
  (cleared at logout)

## What has been tested

Watched on a G316 X 98 on 2026-10-22: a single-key paint and its release, the
light-bar order (a red-to-blue gradient ran left to right), the service
repainting the keyboard by itself after an unplug and replug, live colour
edits from the editor without visible flicker or lag, and the onboard lighting
returning when the service stopped. A channel check of all 133 zones found
every key and bar segment where the definition puts it; the 8 unknown zones
lit nothing. On CachyOS with Plasma 6.8 the package installed cleanly, the
service's tray icon named the keyboard and its profile, and a left click on it
opened the editor. Picked from the tray, the Wave and Breathing presets played
on the keyboard. On 2026-10-23 the report rate changed to 2000, 4000 and
8000 Hz and read back each time; 8000 Hz then held for twelve minutes, through
a lighting release and an engine restart. The keyboard re-enumerated once,
about two and a half minutes after a change to 2000 Hz, and came back at
1000 Hz. After a replug with 8000 Hz kept, the keyboard came back at another
rate; Loginux sent 8000 Hz again as it connected and read it back 5 s later.
On 2026-10-24 the Claude Code status display ran on a G316 X 98 with events
sent by hand: each state, three instances sharing the light bar, the spinner,
question mark, check and cross sprites, and instances closing. Some sprites
and patterns were retouched after that, and the context walls, the limit lamps
and the gauges were added; those have run only against the simulated keyboard
of the tests and in pictures drawn from its frames.
[`research/`](../research/README.md) records the evidence and its labels.

## Credits

Bitmap lettering is drawn from Silkscreen (with a redrawn bold W) and body
text uses Archivo, both under the SIL Open Font License 1.1 (`fonts/`). The
G316 key positions were measured from G HUB's layout data and snapped to
quarter-key units; the G HUB files themselves are not part of this program.
