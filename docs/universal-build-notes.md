# Universal mask: build notes

One section per unit of the build order in `design/universal/final.md` (section 5), in the order they were
built. Each says what changed, the properties it adds (all default off: a build with them installed behaves
like the one before unless switched on), and what is measured, assumed or left out.

## U0: clock pair in bursts (2026-11-24)

**What.** Every burst index now carries the headset's clocks read back to back at the start and at the end
of the recording, so logs of other machines (the laptop's logcat in wall time, the rig PC's input log) can
be put on the time of the burst records. Index `version` 5, new top-level field (the same format, one
field more: `design/universal/final.md` asked for the pair in the version 4 index; the version goes up as
it did for every field added before, so a reader can tell bursts with the pair from those without):

```
"clocks": {"start": {"realtime_ns": ..., "monotonic_ns": ..., "xr_ns": ..., "read_ns": ...},
           "end":   {...}}
```

- Read in this order: `CLOCK_MONOTONIC`, `CLOCK_REALTIME`, the runtime's XrTime now, `CLOCK_MONOTONIC`
  again. `read_ns` is the time between the two monotonic reads: every value of the triple lies within it
  of the others (a few microseconds expected [assumed, the first burst shows it]).
- `start.xr_ns` is the burst's `start_ns`, `end.xr_ns` its `end_ns`. Every `*_ns` of the records is XrTime.
- `end` without XrTime (`xr_ns` null): the stream stopped during the burst (no session to ask). A burst
  ended early by a new one gets the clocks the new one started with.
- The client's XrTime "now" is the runtime's conversion of a `CLOCK_MONOTONIC` read
  (`xrConvertTimespecTimeToTimeKHR`, openxr crate). That XrTime equals `CLOCK_MONOTONIC` on Quest is
  assumed in `tools/log_inputs.py`; `xr_ns - monotonic_ns` of the pair now shows it.
- `CLOCK_REALTIME` is the headset's wall clock as its NTP sets it. The rig PC is a separate machine on
  another network: its own wall clock error against the headset's is not covered by the pair (the adb clock
  probe of `log_inputs.py`, or motion in view, still measures that).

The same triple is on the log lines (tag ALVR_CAM), as `key=value` at the end of the line:
`burst: <n> started, ...; clocks realtime_ns=<n> monotonic_ns=<n> xr_ns=<n> read_ns=<n>` and
`burst: <n> ended after ...; clocks ...` (`null`: not read).

**Reader.** `tools/read_burst.py` (and its copy `docs/tools/read_burst.py`): `burst["clocks"]` (`start` /
`end`, None for older bursts), `xr_to_realtime(burst, t_ns)` (XrTime to headset Unix ns, int64, linear
between the two pairs, constant outside them), and a `clocks:` line in the summary (UTC start / end,
realtime - XrTime, XrTime - monotonic, read time, the offset's change over the burst, a warning when it is
over 1 ms: an NTP step). `realtime_ns` (about 1.8e18) is above 2^53: Python's `json` reads it exactly, a
float-based JSON reader (JavaScript, jq) rounds it by about 256 ns. Bursts of version 4 and older still
read; their summary says to use the epoch time of the "started" logcat line instead.

**Properties.** None: recording only. Cost: two triples per burst, nothing per frame.

**Tests.** `depth_burst::tests::index_is_valid_json_in_time_order` (the `clocks` object and version 5),
`clocks_text_is_one_key_value_per_clock`; `burst::tests::read_clocks_passes_xr_and_reads_wall_clock` (on
the PC build only the wall clock and the given XrTime are read: `CLOCK_MONOTONIC` is read on Android only).

**Not tested.** The Android clock read (`clock_gettime`) is compile-checked only; `xr_to_realtime` was
checked by hand on a synthetic version 5 burst. The first headset burst shows the real `read_ns` and
`xr_ns - monotonic_ns`.

**Outside the repo.** The header of `tools/log_inputs.py` (ALIGNMENT, method 2) now points to the index's
`clocks` and `xr_to_realtime`, with the logcat line as the fallback for older bursts.

## U2: lever window (2026-11-24)

**What.** `design/universal/final.md` section 4.3, without PC data. A play-area box can be named a lever
(shifter, handbrake). While a tracked hand is near it, every view ray that crosses the box shows the room,
whatever the depth says: a held lever is removed from the depth with the hand (of the held 19.3 cm bottle
only the bottom 3.6-5.5 cm stayed [measured, KB]; hands are removed whatever `hand_removal` says
[measured, `logs/analysis-20261125-e1`]). Client: `alvr/client_openxr/src/lever.rs` (gate, late close,
fade, learned volume, `lever:` line; pure, tested on the PC), called from `StreamContext::lever_windows`.
GPU: `LeverWindow` in `depth_mask.rs`, `lever_coverage` in `depth_mask.wgsl` (a slab test per open window
at the end of the mask pass; only adds coverage).

- **Gate.** Open while a valid joint of either hand, located at the display time, is within
  `lever_gate_mm` of the box (its signed distance with domes; inside is 0). The key locates its hands
  earlier (display time minus `key_delay`), so the gate opens a little before the key sees the hand: the
  safe side. An untracked hand is no hand: the runtime then leaves the lever in the depth [assumed: removal
  follows tracking].
- **Late close.** Once no hand is near, the window stays open until the depth frame shown was captured at
  least `lever_close_ms` after the first frame without a hand (both XrTime). The depth then shows the lever
  where it came to rest, also after a spring return [assumed: 300 ms covers it; L2 bursts measure it]. If no
  such depth frame comes (no depth, a stalled provider) it closes 1 s after that anyway. Expected close in
  display time: the depth's age when shown (124-170 ms in the figures of `design/universal/design.md` and
  `design/72fps/recommendation.md`) + 300 ms after the hand left.
- **Fade.** After the close the coverage falls from the top of the mask edge's cut to 0 over 150 ms
  [assumed, set by eye], so the whole fade shows. The top follows `debug.alvr.edge_cut`
  (`alvr_graphics::mask_edge_top`: 0.35 for today's edge, `clamp(cut - 0.15, 0.02, 0.69) + 0.3` with a cut,
  0.65 at `edge_cut 50`); a fixed 0.35 would fall below a higher cut and the window would pop off. A hand
  back during the fade opens it fully at once.
- **Shrink** (`lever_shrink 1`). While the gate is open from a hand, the palm joint's positions inside the
  box are kept as a bounding box in the frame of the box (six numbers). The window is that box plus 2 cm in
  x and z, clamped to the box, and the whole height of the box. Down to its floor for the lever's rest
  extent (the shaft from the base to the knob the palm holds); up to its top because the palm joint is the
  centre of the palm: a fist round a tall sequential stick or a handbrake grip leaves the top of the lever
  some cm above it, and the runtime removes what sticks out above a held object too [assumed from the
  bottle, not measured on a lever]. So the shrink is in plan only. Nothing learned yet: the whole box. Learning runs with `lever_shrink 0` too, so the `lever:` line
  shows what the window would be before it is switched on.
- **Kept per session, not with the layout.** The layout file belongs to the area editor, which rewrites it
  from `PlayArea::to_text` (unknown lines are dropped on its next save) and must not change; a second
  writer would race its save thread. A change of the box (an edit, another layout) or of `lever_forget`
  forgets the volume. The six numbers are on the `lever:` line (`min` / `max`, meters, frame of the box).
- **Windows.** At most 4 levers. The window is the box without its domes (an oriented box), placed as the
  mask places the box. No soft edge: the mask is drawn at 1/5 of the view and filtered, which softens it by
  about one mask texel. If the eye is inside a lever box every ray crosses it (a box drawn around the head
  would show the whole room while a hand is near).
- **A change of the properties** shuts every window at once (no fade) and restarts the counts of the
  `lever:` line (`LeverWindows::shut_all`): a window open when switched off would otherwise show again,
  with no hand, when switched on. The learned volumes stay.
- **Debug tint.** Where a window shows the room, the part credited only to the remembered playzone (mask
  alpha, blue in the `playzone_view` tint) shrinks by what the window shows.
- **Depth dumps** (`debug.alvr.key_dump`) get `"lever_windows"` (per window: stage-space `center`, `half`
  extents, `yaw_deg`, `open`; null for a free slot) only when a window is drawn, so other dumps are as
  before and an offline re-simulation can account for the window.
- **Not built.** The channel binding (a PC input bound to the lever keeps the gate open for 0.5 s; bound
  after three changes with a hand inside). Hook: `LeverWindows::channel_changed(index, time_ns)`, already
  part of the gate and tested; U6 calls it. A change stamped more than 50 ms after the display time
  (`CHANNEL_AHEAD_NS`, a wrong PC-to-XrTime mapping) does not hold the gate. Lever state is not in bursts
  (the `lever:` line is the record; a burst field would be a format change, left for U6 with the angle).

**Properties** (all read twice a second; `lever_boxes` unset = off, the default):

| Property | Default | Meaning |
|---|---|---|
| `debug.alvr.lever_boxes` | empty (off) | Box indices that are levers, `#<index>` of the editor's log, from 0, separated by commas (`1` or `1,2`), at most 4. Others are listed as ignored on the settings line |
| `debug.alvr.lever_gate_mm` | 130 (0..500) | A hand joint this close to the box opens the window |
| `debug.alvr.lever_close_ms` | 300 (0..3000) | Open until the depth shown was captured this long after the hand left |
| `debug.alvr.lever_shrink` | 0 | 1: the window is the learned palm path + 2 cm in x and z, the box's whole height; 0: the whole box |
| `debug.alvr.lever_forget` | 0 | Another value forgets the learned paths |

**Log** (tag ALVR_CAM). On each change of the properties: `lever: boxes #1, gate 130 mm, close 300 ms
after the depth shows the hand gone, window: the whole box` (or `lever: off ...`). Every 2 s while on, per
lever: `#1: open 35 % of 144 frames, 2 opens, 2 closes 470 ms after the hand left, nearest hand 0 mm, palm
path 12 x 3 x 10 cm, window 16 x 30 x 14 of 20 x 30 x 20 cm (used), min ... max ... m`, or `no such box`,
`nothing learned: the whole box ...`, `the box changed: learned volume forgotten`, `(... by the 1 s limit:
no newer depth)`. Without a placed play area: `lever: no play area placed (the marker's boxes), no window`.

**Cost.** Off: one uniform compare per mask texel (`area.w` = 0) and 320 bytes more of uniforms per frame
(zeros); no hand lookup, no line. On: two hand-joint lookups per frame, and per mask texel and open window
two matrix-vector products and a slab test (about 60 ALU operations). At 1/5 of the view (about 0.3 M mask
texels for both eyes) and 4 open windows that is under 0.1 G operations per frame, well under 1 % of the
Quest 3 GPU [estimate, not measured]. No texture reads. Not done: the eye in each window's frame is the
same for every texel of an eye and could be computed on the CPU (about a quarter fewer operations); it
would add per-eye fields to the uniform block and split the Rust copy from the shader, for a cost that only
exists while a window is open.

**Tests.** `lever::tests` (9): unset is off, the box list, gate and late close with the fade and a hand back
during it, the 1 s limit, the palm path window (whole height) and its placement in a turned box,
forgetting (box change, `lever_forget`), only named boxes / no area, the fade start following the edge cut,
`shut_all`, the channel hook (and a change stamped seconds ahead). `depth_mask::tests`:
`lever_window_covers_the_rays_through_its_box` (against brute force), `lever_windows_in_the_uniforms`
(off is zeros, packing, clamping), `layout_size` (4496 bytes), `mask_edge_top` in the edge cut test.
`depth_dump::tests::json_is_valid`: no `lever_windows` without a window, its fields with one. GPU (`tests.rs`):
`lever_window_shows_the_room_through_its_box` (pixel for pixel against the Rust copy, off / closed / behind
the eye give today's pixels, the fade shows partly), `depth_uniforms_layout_matches_the_shader` (the
windows last, after the play area).

**Not tested.** On the headset: the gate distance, the close time and the fade are first guesses. Whether
the hand is tracked at the shifter while the driver looks ahead (L1). Whether the learned path with the 2 cm
margin and the whole box height covers the knob, the shaft and the boot of a real H shifter and a
handbrake (L1-L4 and the user's eyes). `StreamContext::lever_windows` (the joint flags, the area
transform, the depth capture time) needs OpenXR and has no test: on the headset the `lever:` line's
`nearest hand` should read 0 mm with a hand inside the box.

**On the headset** (after a build with U2 is installed; marker placed, a box drawn around the lever with the
editor): `setprop debug.alvr.lever_boxes <index>`; hold the bottle (desk) or the shifter: the box shows the
room, the bottle / lever whole; let go: the window stays about half a second, then fades. Then
`lever_shrink 1` after a few gear changes: the window narrows to the knob's travel (seen from above), full
height. Box indices follow the editor's order: removing a box shifts them, and `lever_boxes` then names
another box (check the `#<index>` in the editor's log after an edit).
