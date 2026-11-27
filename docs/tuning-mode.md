# Tuning mode (praying hands, `debug.alvr.tune`)

## What it is

A menu floating in front of you in the headset, opened with praying hands. It changes the `debug.alvr.*`
settings that matter on the rig live, with a pinch and a drag, without adb. Four pages: hands against room,
arms in motion, depth edge, green screen. Each value you set is a menu override: it wins over `setprop`, it is
saved per place (per QR code) when the menu closes, and it comes back next time. A strip at the bottom shows
live numbers (how much skin and room the key keeps, how much of the zone is green). The Green screen page also
drives the green cap on the rig PC's monitor (`rigkit\green_patch.py`) over UDP. While the area editor is open,
the headset asks the rig PC to show the QR code. Nothing is drawn, read or sent while the menu is closed,
except the gesture being watched and one property poll about twice a second.

Code: `alvr/graphics/src/tuning.rs` (pure logic), `text_panel.rs` (drawing), `alvr/client_openxr/src/tuning_mode.rs`
(the mode), `rig_link.rs` (UDP), the override table in `passthrough.rs`.

## Opening and closing

The menu opens only while a stream with Passthrough = Depth runs, the headset has a head pose, and the area
editor is not editing. Otherwise the log says `tune: can't open the menu (<why>): <reason>`.

**Praying hands.** All of these at once, for 1.0 s (`PRAY_HOLD_S`):

| Condition | Value |
|---|---|
| Both hands tracked | yes |
| Palms face each other | palm normals' dot product below -0.7 |
| Palm centres close | less than 6 cm apart |
| Fingers point up | more than 45 degrees above horizontal (palm joint direction, else palm to index tip) |
| Distance from the eyes | each palm 20 to 60 cm |

Short drops of tracking are tolerated. After it fired, let the hands go apart before it can fire again. The
same gesture closes the menu.

| Property | Does |
|---|---|
| `setprop debug.alvr.tune 1` | opens the menu (a change to 1) |
| `setprop debug.alvr.tune 0` | closes it (a change to 0) |
| `setprop debug.alvr.tune_gesture 0` | the praying hands do nothing (unset or 1: they work). Logged |

The value of `tune` at the start of the app is not a request: set 0, then 1. Properties are polled twice a
second.

Other ways it closes (the `tune: off (<why>)` line names it):

| Why | Log text |
|---|---|
| Done row | `Done` |
| Praying hands again | `gesture` |
| `tune 0` | `tune 0` |
| 120 s without a pinch or a drag | `nothing done for 120 s` |
| The stream with Passthrough = Depth stops | `the stream with Passthrough = Depth stopped` |
| The area editor starts editing (other ways than the menu) | `the area editor started` |
| Edit boxes row (closes, then starts the area editor) | `Edit boxes` |
| Session stopping / exiting | `session stopping` / `session exiting`; also `the session ended` when the loop ends |

Closing saves the profile and puts the green key back. While the menu is open the hands are **not sent to the
game** (as while the area editor edits), and the editor's own pinches are off.

## Using the menu

- **Where.** A panel 0.40 x 0.30 m, placed once when it opens: 0.55 m ahead of your eyes along the way your
  head faces, 0.15 m above them, turned to face you. It stays in the room (world-locked): walk around it, it
  does not follow your head. It is drawn as the last layer, over everything.
- **Pointing.** The ray of each hand is its cursor (a dot on the panel). The row under it lights up.
- **Pinch.** Thumb and index together, with the same thresholds as the area editor (`area_pinch_on` /
  `area_pinch_off`). A pinch that is already closed when the menu opens does nothing until you open it.
- **Tabs** (top strip): pinch a tab and let go to change page. A pinch that was closed on the old page does
  nothing on the new one.
- **Value rows** (a label, a number, a bar): pinch on the row and drag left or right along the panel. The
  **full width of the panel is the full range** of the row, snapped to its step and clamped. You must move
  more than 2 % of the width (8 mm) first (dead zone): a pinch without a move changes nothing. The value
  follows the ray along the panel's plane, also past its edges. **On release** the value is the one from 0.12 s
  before the fingers opened (opening the fingers moves the ray; that move is thrown away). A hand lost while
  dragging keeps the last value.
- **On / off rows** (`Auto key`, `Move / size patch`): pinch and let go to switch. They show `on` or `off`.
- **Buttons.** `Reset page` removes the menu values of the properties on this page. `Reset all` removes all of
  them. After a reset the setprop value, or the code default, is in effect again. `Done` closes the menu.
  `Edit boxes` (Depth edge page) closes it and opens the area editor.
- A small mark at the left of a row means its value comes from the menu.
- A bar shows where the value is in its range.

## Pages and rows

The default is what the client uses when the property is unset (and the menu has no value). Ranges are the
menu's: setprop can still set other values, and the menu shows them clamped to the bar.

### Hands vs room

| Menu label | Property | Range | Step | Default | What it does |
|---|---|---|---|---|---|
| Auto key | `key_auto` | off / on | | off | The colour learner chooses the skin bias itself |
| Room strictness (auto on) | `key_auto_weight` | 0-1000 % | 10 | 300 | How much a leak of room costs against skin kept. Higher: less room leaks in, less hand kept |
| Skin preference (auto off) | `key_bias` | 0-1000 % | 10 | 150 | Preference for skin where room and skin look alike |
| Unknown colours kept | `key_unknown` | 0-100 % | 5 | 10 | How much of the colours never seen the key keeps |
| Hand rim max | `key_rim_max` | 0-30 mm | 1 | 8 | Limit of the rim of room kept around the hands |

"Room strictness" and "Skin preference" are one row: it shows the first while `key_auto` is on (in effect, so
the switch row above changes it at once) and the second while it is off. The two properties are separate; each
keeps its own menu value. If you start a drag and `key_auto` flips meanwhile, the drag keeps the row it began on.

### Arms in motion

| Menu label | Property | Range | Step | Default | What it does |
|---|---|---|---|---|---|
| Arm border to key | `cloud_trail` | 0-48 texels | 1 | 0 | How far the depth cloud's arm border is handed to the colour key |
| Fast-arm reach | `cloud_cap` | 4-48 texels | 1 | 14 | How far ahead the band reaches for a fast arm |
| Band gain | `cloud_gain` | 0-400 % | 10 | 150 | Band width per speed |
| Hand ghost fix | `cloud_own` | 0-40 mm | 1 | 0 (off) | Removes the late copy of a fast hand |

These act only while the depth cloud is on (`debug.alvr.cloud` 1, see `docs/depth-cloud.md`).

### Depth edge

| Menu label | Property | Range | Step | Default | What it does |
|---|---|---|---|---|---|
| Depth edge position | `edge_cut` | 17-84 % | 1 | 20 | Where the mask edge falls on an object |
| Memory margin | `mem_margin_mm` | 20-150 mm | 5 | 60 | Room-memory margin around the hand capsules |
| Edit boxes | (button) | | | | Closes the menu, opens the area editor |

`mem_margin_mm` acts only while the room memory is on (`mem_roll 1`).

### Green screen

| Menu label | Property | Range | Step | Default | What it does |
|---|---|---|---|---|---|
| Green strength | `green_ratio` | 0-300 | 5 | 135 | How much greener than red and blue counts as green (percent) |
| Dark green floor | `green_floor` | 0-255 | 1 | 70 | The darkest green still keyed |
| Edge softness | `green_soft` | 1-255 | 1 | 24 | Soft edge of the key |
| Cap cut line | (cap, not a property) | -1 to 1 | 0.05 | 0 | Where the disc on the rig monitor is cut. Sent to the PC, saved in the profile |
| Move / size patch | (switch) | off / on | | off | Moves and sizes the cap by hand, see below |

Every page also ends with Reset page, Reset all, Done.

Notes that apply to the rows:

- The `key_*` rows act through the colour learner, so they matter only if `debug.alvr.auto_key` was 1 before the
  app started (the learner does not start later). `key_rim_max` is read by the hand zones, not the learner.
- Only the menu's own properties are in the table. Start-up settings (`auto_key`, `mem_size`, ...) are not in
  the menu.
- Values a profile or setprop holds outside a range are snapped to the step; a profile clamps to the range.

## Live stats footer

One line at the bottom of the panel, read twice a second:

`skin 92 %   room 31 %   scene 18 %   bias 1.50   green 24 %`

| Number | Meaning | Source |
|---|---|---|
| skin | Share of the skin colours the key keeps | colour learner, at its `auto key:` line |
| room | Share of the colours around the hands the key keeps (the leak) | same |
| scene | Share of the scene colours the key keeps | same |
| bias | The skin bias `key_auto` chose | same, only while `key_auto` is on |
| green | Share of the camera pixels in the green zone that are green | green key, at its `green:` line |

A number shows `-` when it is not known or older than 5 s: the learner is not running (no `auto_key` at start),
`key_auto` is off (bias), the green zone is off or sees nothing (green). On the Green page, with a wired stream
and no `patch_host`, the footer shows `wired stream: setprop debug.alvr.patch_host <rig PC LAN IP>` instead.

## Overrides and profiles

**Order of values.** Menu value, else setprop, else the code default. The menu keeps its values in an
in-process table that `debug_property` and `debug_property_string` read first.

**Last writer wins.** If you `setprop` one of the menu's properties to a new value (also unset it) while the
client runs, the menu's value of that property is dropped and the new value applies. The log says
`tune: setprop <prop>=<v> replaces menu value <z>` (`<v>` is `(unset)` if cleared). The next profile saved no
longer has it.

**Profile files.** `tune-<payload>-<hash>.txt`: the payload of the code the play area is placed from, characters
other than letters, digits, `-` and `_` replaced by `_` (at most 40), then an 8-digit hash of the whole payload
(so payloads that look alike get their own files). With no payload known: `tune-default.txt`. They are
written to both folders of the area files:

- `/sdcard/Android/data/alvr.client.dev/files/areas/`
- `/data/local/tmp/alvr-areas/`

Format: two `#` comment lines, then `property=value` (without `debug.alvr.`, sorted) and, if a cap is known,
`patch_cap=cx,cy,r,cut`. Unknown keys are ignored.

| When | What |
|---|---|
| App start (first frame loop) | the profile of the payload known then, else `tune-default.txt`; if two folders have it, the newer file |
| The payload changes to another one (menu closed) | that payload's profile is read; a profile that does not exist keeps the current menu values, they are saved under the new name on close |
| The payload is lost | the profile in use stays; `tune-default.txt` is read only at the start |
| Menu closes (any reason) | saved from a thread, both folders |

Reading a profile replaces the menu values of every menu property: ones missing from the file are cleared.
`tools/install_apk.sh` keeps `tune-*.txt` across a reinstall (it copies them to `/data/local/tmp/alvr-areas`
with the area files and makes them world-readable).

Look at or delete one:

```
adb shell ls /sdcard/Android/data/alvr.client.dev/files/areas/ /data/local/tmp/alvr-areas/
adb shell cat /data/local/tmp/alvr-areas/tune-default.txt
adb shell rm /sdcard/Android/data/alvr.client.dev/files/areas/tune-default.txt /data/local/tmp/alvr-areas/tune-default.txt
```

Delete both copies, or the other one is read next time. The app may not be able to overwrite a file that adb
created in `/data/local/tmp`; if a save fails, the log says so.

**Log lines** (logcat tag `ALVR_CAM`):

| Line | When |
|---|---|
| `tune: on (<gesture or tune 1>), profile <name>, <n> values set by the menu` | the menu opens |
| `tune: off (<why>), profile <name>, <n> values set by the menu[, cap cx,cy,r,cut]` | it closes |
| `tune: saved <path>` / `tune: could not save <path>: <e>` / `tune: not saved, no saver thread: <e>` / `tune: not saved, the saver thread stopped` | each folder written |
| `tune: <prop> <old> -> <new>` | a value changed; a switch or reset at once (`(reset)`), a drag when it ends. `old` / `new` are the values in effect (`(empty)` for an empty override, `unset`) |
| `tune: patch_cap <old> -> <new> (seq <n>)` | a cap drag or the cut row ended |
| `tune: green_boxes <old> -> (empty) (green page: the green key is off)` | green page entered |
| `tune: green_boxes ... (green page left: the green key is back)` / `(menu closed: the green key is back)` / `(left from the last session: the green key is back)` | key restored |
| `tune: page <title>` | a tab was picked |
| `tune: moving / sizing the patch by hand: on` / `: off` | switch row |
| `tune: ResetPage` / `tune: ResetAll` | after the resets |
| `tune: setprop <prop>=<v> replaces menu value <z>` | last writer wins |
| `tune: the praying hands open and close the menu (tune_gesture 1)` / `tune: the praying hands do nothing (tune_gesture 0)` | the property changed |
| `tune: can't open the menu (<why>): ...` | refused |
| `tune: profile <path> <n> values[, patch_cap ...]: <p=v ...>` | profile read |
| `tune: no profile <name> in <dir1> or <dir2>[, the menu's values stay ...]` | none found |
| `tune: can't read <path>: <e>` | read failed |
| `tune: green_patch.py replied: shape <s>, cap <cx,cy,r,cut>, visible <b>` | first reply after a `get` |
| `tune: a reply of green_patch.py does not read (<e>): <text> (logged once)` | bad reply |
| `tune: panel swapchain 1024x768, format 0x<f>, <n> images` | panel made (first open) |
| `tune: no panel: <e>`, `tune: panel upload failed: <e> (logged once)`, `tune: the panel thread stopped, ...` | panel problems |
| `rig: green_patch.py messages go to <ip:port>` | destination known or changed |
| `rig: no address for green_patch.py yet (no streamer connected, debug.alvr.patch_host unset)` | no destination |
| `rig: streamer is wired (loopback); set debug.alvr.patch_host to the rig PC's LAN IP` | wired, no `patch_host` |
| `rig: debug.alvr.patch_host "<x>" is no IP address (or ip:port)` | bad `patch_host` |
| `rig: sending to <addr> failed: <e> (not logged again until a send works)`, `rig: no UDP socket: <e>` | UDP problems |
| `area: qr show <payload>` / `area: qr hide` / `area: qr hide (session stopping)` / `(session exiting)` | QR requests |
| `area editor: ... (tuning menu, Edit boxes)` | the area editor started from the menu (its usual start line, with this reason) |

## Green screen page and the rig PC

On the page the green key is **suspended** (`green_boxes` is overridden with nothing) so you see the real green
cap on the rig monitor. The play area's outline is drawn (as in the area editor). Leaving the page, closing the
menu, stopping the session or starting a new session puts the override back as it was. On entry the headset
sends `{"get":1}` and starts from the reply.

**The cap** is the part of a disc above a cut line, in fractions of the rig monitor:

| Value | Meaning | Range | Default |
|---|---|---|---|
| `cx` | centre, from the left, of the monitor's width | 0-1 | 0.5 |
| `cy` | centre, from the **top**, of the monitor's height | 0-1 | 0.6 |
| `r` | radius, of the monitor's **height** (the disc stays round) | 0.02-1 | 0.2 |
| `cut` | cut line, in radii below the centre: -1 nothing, 0 the upper half disc, 1 the full disc | -1 to 1 | 0 |

**Move / size patch** (switch on, then pinch off the panel):

| Gesture | Effect |
|---|---|
| One hand pinched, moved | moves the cap: 1 cm of hand = 1 % of the monitor (right = right along the panel, up = up) |
| Both hands pinched, moved apart or together | radius grows or shrinks by 1 % of the monitor height per cm of spread |
| Cap cut line row | drag as any value |

A hand that is lost while holding keeps the cap where it is. Switching the row off, or leaving the page, ends
the grab.

**UDP.** Port `9945` on the rig PC (`green_patch.py --udp-port`, 0 = off). One JSON object per datagram.

Headset to PC:

| Message | When |
|---|---|
| `{"get":1}` | page entered |
| `{"seq":n,"shape":"cap","cap":[cx,cy,r,cut],"visible":true}` | while the cap changes, at most every 0.1 s; the last one at release |
| `{"qr":"<payload>","show":1,"lease":6}` | area editor open, see below |
| `{"qr":"<payload>","show":0}` | area editor closed |

PC to headset (reply to `get`): the whole state, with `monitor`, `rect`, `color`, `visible`, `shape`, `cap`, the
echoed `seq`, `qr_shown`. The headset reads the cap and uses it as the start of the menu.

`seq` is absolute and starts from the Unix time in milliseconds when the menu object is made (it is kept
increasing for the whole run), so a new run is newer than the last. `green_patch.py` ignores, per sender IP, a
`seq` not newer than the last applied one, and treats 0 or a drop of more than 1000 as a restart of the sender.
A lost datagram does no harm: every state is absolute.

**Where it goes.**

1. `setprop debug.alvr.patch_host <ip>` or `<ip>:<port>`: that address (port 9945 if none). Polled about once a
   second.
2. Else the streamer's address of the last connection, port 9945.
3. **A wired stream** (USB or adb forwarding) has the streamer at a loopback address and `adb reverse` does not
   carry UDP. Then nothing is sent until `patch_host` is the rig PC's **LAN IP**. The headset's Wi-Fi must be
   on the same network; the stream itself may stay wired. This also applies to the QR requests.

The destination is logged when it changes (`rig: green_patch.py messages go to ...`).

**On the rig PC.** Run `rigkit\8_allow_green_udp.bat` once as administrator (it asks): it adds the inbound
firewall rule `Rig kit green patch UDP 9945` for all networks and warns about a rule that blocks `python.exe`.
Then `7_green_patch.bat`. `green_patch.py` hotkeys (work with the game in focus):

| Keys | Effect |
|---|---|
| Ctrl+Alt+arrows | move (cap: the centre) by 1 % |
| Ctrl+Alt+Shift+arrows | cap: Up / Down radius +/- 1 %, Right / Left cut +/- 0.05. Rectangle: size by 1 % |
| Ctrl+Alt+C | switch rectangle and cap |
| Ctrl+Alt+PageUp / PageDown | green value +10 / -10 (40-255) |
| Ctrl+Alt+G | hide / show |
| Ctrl+Alt+R | show / hide the QR code (no lease) |
| Ctrl+Alt+Q | quit |

The patch keeps its state in `logs\green_patch.json` and logs changes in `logs\green_patch-<date>.log`. The
cap set from the headset is saved there, so it survives a restart of the tool.

## QR code while the area editor is open

Whenever the area editor edits (two-hand pinch gesture, `area_edit 1`, or Edit boxes), the headset asks the rig
PC to show the QR code of the payload it looks for (`debug.alvr.marker_id`, else the code seen, else `SCR1`):

- Each time the request changes: 3 messages 0.1 s apart, `{"qr":"<p>","show":1,"lease":6}`. Then repeated
  (again 3 in a burst) every 2 s. The log gets `area: qr show <payload>` once.
- The **lease** is 6 s: if no show arrives for 6 s the PC hides the code itself (`qr: hide <payload> (lease
  expired)` in its log). So a headset that crashes or loses the network does not leave it on the monitor.
- When editing ends: 3 messages `{"qr":"<p>","show":0}`, log `area: qr hide`. When the session stops or exits
  while the QR is up: the hide is sent at once (`area: qr hide (session stopping)`).
- Needs `rigkit\qr\<payload>.txt` (rows of 0/1 without the quiet zone, made on the laptop by
  `tools\make_qr_matrix.py`). Present: `RIG1`, `SCR1`, `TEST`. A payload without a file is logged on the PC and
  not shown, and it hides a code shown before.
- The PC draws the code itself, in its own window above the green patch (not `show_qr.py`), with a white margin
  of 4 modules. Size: `qr.px` pixels per module in the state file (default 12).
- **Size must match the headset.** Measure the black edge of the code on the monitor in mm and give it to the
  headset with `marker_sizes` (for example `SCR1:150`), or the distance estimate is wrong. At the rig on
  2026-11-27, with no marker size set, the headset measured **69.9 mm**; set `px` or `marker_sizes` so that the
  two agree.

## First headset run (checklist)

Logcat: `adb logcat -s ALVR_CAM` (the tag of `stream_log`). Install with `tools/install_apk.sh` (keeps the
profiles).

1. `setprop debug.alvr.tune 0` then `1` (or hold the praying hands 1 s): `tune: on (...)`. The panel appears
   about 0.55 m ahead, a little above.
2. First open: `tune: panel swapchain 1024x768, format 0x...`. Any `tune: panel upload failed` or `no panel` is a
   problem. Format should be an 8-bit one (`0x8058`, or `0x8C43` as the fallback).
3. **Is the text upright and not mirrored?** The panel is drawn with row 0 at the top and flipped before the
   upload because a GL texture upload takes row 0 at the bottom (`flip_rows`). That is an assumption: if the
   panel looks upside down, the flip is wrong for this runtime.
4. Hover and pinch work: cursors sit where the ray points; a pinch-drag changes a bar; `tune: <prop> <old> ->
   <new>` appears when the drag ends.
5. Change `Room strictness` and watch the `auto key:` numbers move (needs `auto_key 1` at start). Reset page
   gives back the setprop value: `(reset)` lines.
6. `perf:` line (every 2 s while streaming): frame rate holds with the menu open (the panel redraws at most
   20 Hz while values change, 10 Hz while only the cursors move).
7. Close and look at the profile (`adb shell cat`). Restart the client: `tune: profile <path> <n> values`.
8. Green page: laptop or rig PC running `green_patch.py`, firewall rule in place, headset and PC on the same
   network. Look for `rig: green_patch.py messages go to <ip>:9945`, then `tune: green_patch.py replied: ...`.
   Drag the cap: the patch follows; `tune: patch_cap ... (seq ...)` at release. Leave the page: the key is
   back (`tune: green_boxes ... (green page left ...)`).
9. Area editor via the gesture and via Edit boxes: `area: qr show <payload>`, the code on the monitor within
   about 0.5 s, `marker:` locks, `area: qr hide` when closed.
10. Wired stream: set `patch_host`; without it the footer on the Green page and the `rig:` line say why.

**Known unverified.** Not yet run on a headset: the panel's orientation (item 3), that the quad layer is not
left out by the compositor, the cost of the panel, the Quest's pinch thresholds when hands are close to the
panel, hand tracking of praying hands with the palms touching (the gap and angle tolerances are guesses; adb
`tune 1` is the fallback), the green share and key stats on the footer, and the UDP path to the rig PC. The
LUT alpha of the green key does not show in adb screenshots; the panel should.
