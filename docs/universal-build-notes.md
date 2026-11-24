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

## U3: static rig memory (2026-11-24)

**What.** Step 3 of `docs/mask-v2-design.md` (section 4.3, "rolling room memory"), with the corrections of
`docs/mask-v2-check.md` and the moving-cell flag of `design/universal/final.md` 2.1. It is aimed at the
user's "transparent ghost" where the bottle (shifter) was moved, and at stale legs and chair. Today's
playzone is one copy of the depth image. It is taken only when no hand was near the depth view for 1.5 s,
so while the user drives it stays frozen everywhere, and the old places of moved things are drawn from it
near the hands [measured, logs/analysis-20261124-bottle]. The memory is written from the live depth on
every new depth image, except near the hands. There it is kept ("frozen"), because the runtime removes the
hands and what they hold. The mask reads only frozen texels.
Code:
- `alvr/graphics/resources/room_memory.wgsl`: the update, the re-projection, the stats and the dump passes.
- `alvr/graphics/src/room_memory.rs`: camera, anchor rules, uniforms, stats, JSON. Pure and tested.
- `alvr/graphics/src/room_memory_gpu.rs`: pipelines, textures, ping-pong, the eye split, the stats readback.
- `alvr/client_openxr/src/room_memory.rs`: properties, the capsules at the capture time, the `memory:` line.

- **Camera.** It is fixed in stage space: 120 x 120 degrees, `mem_size` square (512: 0.0068 per texel in
  tangent units, the depth's 0.0069), one Rgba16Float texture per eye (two, ping-pong), each fed by that
  eye's depth image.
  - Position: the middle of the two depth cameras when it is anchored.
  - Aim: the head's yaw at that moment (the mean of the two depth views, no roll), pitched `mem_pitch_deg`
    (35) below the horizon. Looking straight down or up, the head's up vector gives the yaw. The design's
    "towards the centre of the play area" is not used: for this play area, which holds the seat, it turns
    90-180 degrees away from the hands [measured, check L4].
- **Anchored again** (at a new depth image):
  - No hand near the depth view for 1.5 s (today's trigger): cleanly (cleared, everything from the live
    depth) when the head is more than 3 cm from the anchor, or its yaw more than 40 degrees from the aim.
  - Otherwise (a hand near or seen within 1.5 s): when the head is more than 12 cm away or turned more
    than 40 degrees, by one re-projection pass that keeps every channel (frozen texels included).
  - A changed `mem_pitch_deg` counts as turned. The first depth image anchors it cleanly, also with hands.
    A change of the reference space (`update_reference_space`, a recenter) or `debug.alvr.playzone -1`
    forgets the camera: the next image anchors cleanly.
  - The 40-degree yaw rule with no hand near goes beyond the design: it is cheap and it keeps the aim on
    the head.
- **Channels.** r: distance from the camera plane (0: nothing). g: moving. b: frozen. a: seconds since the
  last live write (above about 2 minutes not exact: 16 bit float). The design's "g owner" is for carry
  (V2 steps 4-5), which is not in the U build order, so g holds final.md's moving flag:
  - set to 1 when the live depth changes by more than 3 cm with no hand within 20 cm (the hand's bounding
    sphere, a lower bound of the capsule distance);
  - falls to 0 over 5 s [both assumed].
  It is recorded and counted, and read only with `mem_moving 1` (frozen texels marked moving are then not
  read). U4 is meant to use it for the rig map's cells.
- **Update** (`update_main`, one fragment pass per eye per new depth image). Per texel:
  - Live surface: 2 fixed-point steps from the remembered distance (else 0.7 m), one depth read each. It
    counts only if the surface found lies within 1.5 texel widths of the texel's ray.
  - "Near": the segment from this eye's depth camera to the remembered point (else the live one) passes
    within `mem_margin_mm` of a hand capsule at the depth's capture time. Two sphere tests per texel; the
    56 capsule tests only where a hand's bounding sphere is within reach.
  - Rule, in this order:

    | Case | Written |
    |---|---|
    | Remembered and near | Kept, frozen |
    | No live value | Kept, not frozen |
    | Remembered, in the outer ring (`mem_outer_mm`), live 8 cm or more behind | Kept, frozen |
    | Else | The live value, not frozen, age 0 (also near a hand with nothing remembered: the fill is better than nothing) |

  - Deviation from section 4.3: there "no live value" comes before "near" (kept, not frozen). Here a
    remembered point near a hand stays frozen also without a live value. The runtime's fill near a hand may
    be no data, and the read counts only frozen texels: it would otherwise cut the held object where the
    fill is a hole.
- **Capsules at the depth's capture time.** They come from the arm's own sample history (`arm_history.rs`).
  While the memory is on, each sample also keeps all 26 joints (`push_joints`, `joints_at`). Those joints
  are located every frame anyway, so there is no extra lookup (the check counted "one more lookup per
  frame"; with the history there is none). The runtime's past-time lookups return its newest sample
  [measured, wave report], so they are never used.
  - Where the capture time is not in the history (a gap, a hand just found, no capture time reported): the
    newest sample's capsules, or after the hand was lost the last known ones (for up to 1 s), grown by 3
    cm. A failed lookup must never let the fill be written over the memory.
  - The capsules use the joints' own radii (not `key_scale`): the margin dominates.
- **Read** (`depth_mask.wgsl`, `playzone_coverage`). With the memory on, only frozen texels count (and with
  `mem_moving 1` not those marked moving). The others equal the live depth, so nothing is lost. The arm,
  written outside the margin, is never drawn twice when step 1 moves the live arm.
  - The zone near each hand (`focus_area.wgsl`, `hands_view_rect_reach`) reaches farther by the wrist's
    travel since the capture (`ArmMotion::displacement`, 0 without a motion), and by the margin or outer
    ring beyond 6 cm.
  - The memory is read only once it is anchored. While it is on, today's copy is neither taken nor read,
    and the client writes no `playzone:` lines.
  - Without the color key the memory is not kept at all: the mask reads the playzone only with the key.
- **Eyes on two frames.** The update of eye 0 runs on the frame a new depth image arrives, that of eye 1 on
  the next frame (same depth image, same uniforms). This spreads the spike of a depth frame (check, section
  4) over two frames. Two new images on consecutive frames: both eyes at once. A clean anchor clears both
  eyes, a re-projection does both eyes, on the frame of the new image (rare).
- **Debug view.** `mem_view 1` turns on the playzone's "Tint mask" view: what only the memory shows is blue,
  faintly blue near the hands. With `mem_view 2` that blue is paler the older the texels are (full down to
  30 % over 5 s). Frozen texels and their age are also in the stats and the dumps.

**Properties** (read twice a second; `mem_roll` unset or 0 = off, the default: today's playzone copy, no
texture, no pass):

| Property | Default | Meaning |
|---|---|---|
| `debug.alvr.mem_roll` | 0 (off) | 1: the rolling room memory instead of today's copy (needs the color key) |
| `debug.alvr.mem_margin_mm` | 60 (20..150) | A remembered point this close to a hand capsule (on the depth camera's line to it) is kept |
| `debug.alvr.mem_outer_mm` | 0 (off, ..250) | Beyond the margin up to this distance, kept only if the live depth is the far fill (8 cm or more behind). Not beyond the margin: off |
| `debug.alvr.mem_view` | 0 | 1: with "Tint mask", what only the memory shows is blue; 2: paler with age |
| `debug.alvr.mem_size` | 512 | 384: smaller textures (other values: 512). Read when the textures are made; a change makes them again (cleanly anchored) |
| `debug.alvr.mem_pitch_deg` | 35 (0..80) | The camera's aim below the horizon; a change anchors again |
| `debug.alvr.mem_moving` | 0 | 1: frozen texels marked moving are not read |

**Log** (tag ALVR_CAM). On each change of the properties (unset: never, as before): `memory: rolling,
margin 60 mm, no outer ring, 512 x 512 texels, aimed 35 deg down, no view, texels marked moving are read`,
or `memory: off ...` when switched off. Every 2 s while it is on: `memory: 512 x 512, anchors 1 clean / 0
with hands, updates 7.4 / 7.4 per s, left eye: frozen 1.20 %, no live value 41 %, empty 30 %, moving 0.5 %,
oldest frozen 3.2 s (stats 0.4 s old), capsules at the capture time: left history 98 % last known 2 % none
0 %, right ...`. "No live value" counts texels not written live at the last update and not frozen; that
includes those never seen, since about a third of the 120-degree camera is outside the depth view. The
shares come from a stats pass on the left eye every 2 s (16 x 16 cells, a 2 KB readback).

**Dumps and bursts.**
- A depth dump (`key_dump`, and the dump at the start of a burst) taken while the memory is anchored has
  `alvr_dump_<n>_memory_left.f32` / `_right.f32`: 4 little-endian f32 per texel (r, g, b, a above), rows
  in the order of the texture. The JSON gets `"memory": {"status": {camera (position, yaw_deg, pitch_deg,
  view: pose, tangents, world_to_view, view_to_world), size, anchors, updates, stats, margin_mm,
  outer_mm, skip_moving, hands}, "files": [...]}`. Without it the dump is as before (no `memory` field).
- Bursts: index version 6. Every display record has `"memory"`: null when off, else the same status as the
  renderer left it after that frame (camera, anchors, updates, the last stats with their age, where this
  frame's capsules came from).
- `tools/read_burst.py` (and the identical `docs/tools/read_burst.py`):
  - `display["mem_*"]` arrays;
  - a `room memory` summary line;
  - `load_memory_dump(path)` for the dump files.
  Older bursts still read (checked on `logs/bursts-20261125/alvr_burst_15`, version 3). Found on the way,
  not changed: `load_burst(..., images=False)` still opens the key table file of the index (the open
  question of final.md section 7: an index pulled alone does not load).

**Cost** [estimates, not measured]. Off: no texture, no pass, no pipeline. 16 bytes more of uniforms (zeros),
one add per focus-area texel, and in the mask's near-hand path a 4-channel load instead of the r one plus
two compares. The picture is the same.

On, per update of one eye:
- At 512: 262,144 texels, each one memory read and up to two depth reads (texels outside the depth view
  stop at the first projection), about 150-200 ALU operations, and two sphere tests.
- The capsule loop (56 segment tests, about 2,000 operations) runs only where a hand's bounding sphere is
  within reach of the texel's line: about 7 % of the texels per hand in view. That is about 0.13 G
  operations and 0.8 M texture reads per eye update.
- For comparison, the mask pass is 192 k texels per eye per frame with 9-11 depth reads each: the update
  is about half of one eye's mask pass in texture reads, once per depth image per eye, eye 0 and eye 1 on
  consecutive frames.
- At 384: 147 k texels (56 %).
- Stats: one pass of the same texel count every 2 s, a 2 KB readback.
- Memory: 4 textures x 2 MB at 512 (1.2 MB at 384).
- The pipelines are compiled the first time it is switched on (a one-time hitch on that frame).

The check's stop rule stands: more than 2 points of GPU load, then try `mem_size 384`. Measure on depth
frames, not as an average.

**Tests.**
- `room_memory::tests` (graphics, 7): the aim (yaw kept, pitch, no roll, fov), looking straight
  down / up, the anchor rules (3 / 12 cm, 40 degrees, pitch, yaw wrap, forget), the uniform packing, the
  read vector (zeros when off), the stats decode, the status JSON.
- `depth_dump::tests::memory_dump_is_decoded`; `json_is_valid`: no `memory` field when off.
- `depth_burst::tests::index_is_valid_json_in_time_order`: version 6, `memory` null and set.
- `depth_mask::tests::layout_size` (4512 bytes).
- GPU (`tests.rs`, run on the desktop adapter):
  - `memory_drops_a_moved_object_away_from_the_hand`: an object moved with the hand 15 cm away. After
    one update the old place is the game and the new place the room; frozen texels hold only the
    background behind the hand.
  - `memory_keeps_a_held_object_whole`: the bottle removed from the live depth within the margin stays
    whole and frozen over 50 updates, with its age. It is drawn, also 2 texels from its edges. The GPU
    stats match the texels.
  - `memory_never_draws_the_arm_twice`: the forearm below the wrist is never frozen. With the live arm
    moved 10 cm by an arm motion, its old place is the game. Read like today's copy, it would be drawn
    there.
  - `memory_off_reads_the_playzone_as_today`: with no memory input, today's copy (frozen 0 everywhere) is
    drawn as before; with the memory flag it is not. The uniforms differ only in the memory vector,
    which is zeros when off.
  - `memory_camera_maps_points_both_ways`: a point seen by a depth camera turned 30 degrees and moved
    10 cm lands on its texel and comes back within 2 cm. A re-projection into a camera moved 13 cm and
    turned 23 degrees keeps it, frozen, with its age.
  - `room_memory_shader_translates_to_gles` (all three entry points);
    `memory_uniforms_layout_matches_the_shader`; `depth_uniforms_layout_matches_the_shader` (the memory
    vector after the playzone's).
- Breaking the near test, or reading every texel, makes these tests fail (checked by mutating the shaders
  once).
- Client: `room_memory::tests` (4: properties and clamps, the capsules from the history / last known /
  none, the reach, the log line), `arm_history::tests::joints_follow_the_samples`.
- `mem_roll 0` is today's picture by construction: every new branch is gated on the memory vector (zeros
  when off), and all earlier GPU tests pass unchanged, including the pixel-exact ones.

**Not tested.**
- On the headset: all of it.
- The thresholds: margin 6 cm, outer ring 8 cm, moving 3 cm / 20 cm / 5 s, 1.5 texels, 3 / 12 cm, 40
  degrees, the 1 s of last known capsules.
- The stats readback and the per-frame flow of `RoomMemory::plan` / `encode` (depth keys, the eye split,
  the GL error path) need the GLES context: compile-checked only.
- `StreamContext::memory_input` and `report_memory` need OpenXR.
- The moving flag at silhouettes: a head move changes the depth at edges, so some edge texels will be
  marked moving. Only `mem_moving 1` reads that.

**Left out.** Carry (V2 steps 4-5, the owner channel). Merging the two eye textures into one. An age limit
(`mem_max_age_s`). The forearm-stub rule of the design's failure table (not writing texels inside a
forearm stub). The KNOWLEDGE_BASE rows the older brief asked for: that file belongs to the other session
tonight.

**On the headset** (a build with U3 installed, the color key on). The user's tests of design section 8,
step 3, plus the check's addition:
- `setprop debug.alvr.mem_roll 1`, `setprop debug.alvr.mem_view 1` and the "Tint mask" debug view; look
  for blue only next to the hands.
- Move the bottle 10 cm and let go: the ghost at the old place goes when the hand is 6 cm away. Grab it
  again: whole.
- Hold the bottle still for 30 s and look around, also from low and from the side: it stays whole (blue
  while held). `mem_view 2`: the blue pales as it ages.
- Hands on the wheel (or the lap), turn the head 20-35 degrees: the `memory:` line counts a re-anchor "with
  hands" only beyond 40 degrees or 12 cm. The remembered zone stays on the hands, with no jump.
- Move the chair and the legs; hands on the lap: the knees next to them. Is the leg flicker still there?
- Wave an arm in front of the game: no second, late arm near the hand.
- Stop rules (design section 8): a held bottle loses parts, then `mem_outer_mm 120`, then `mem_margin_mm
  80`; still, back to `mem_roll 0`. More than 2 points of GPU load (on depth frames), `mem_size 384`.
  No blue near the hands at the wheel or the shifter: change `mem_pitch_deg`.
- Record: a burst with `mem_roll 1` (the display records have `memory`) and a `key_dump` (the memory
  files); `read_burst.py` prints the `room memory` line.
## U4: problem view and `sep:` log (2026-11-24)

Reduced scope of design §2.5: the existing leak map (head-anchored, 2° cells) drawn as a tint, and a log
line to judge the separation of hand and room. Not built: the rig-frame (QR) anchor, the static distance
per cell, the 3 x 3 pooling, vis(r), the "Hands / Arms" card and the 8 s showing at the end of setup.

- `debug.alvr.problem_view 1` (default 0: off, the picture is unchanged and nothing is uploaded). Depth
  passthrough only. The room shows through the dimmed game; each direction around the head is tinted from
  the leak map: nothing under `problem_view_lo`, amber hatching up to `problem_view_hi`, red cross-hatching
  above, grey dots where the cell is unknown (everything grey until the map has its first snapshot). Never
  while the play-area editor is active. It also learns the leak map, as `key_leak_map` does, but does not
  narrow the rim.
- `debug.alvr.problem_view_lo` / `problem_view_hi` (percent of cell leak): default `key_leak_map_lo` /
  `key_leak_map_hi` (3 / 15).
- `debug.alvr.problem_view_room` / `problem_view_dim` (percent): how much the room shows through the game
  (85) and how much the game is dimmed (50).
- `sep:` (`ALVR_CAM`, every 2 s with the key learner, while `problem_view` is on): `hands recall` (share of
  this pass's skin label colors that the LUT keeps), `leak L / R` (share of the colors around each hand that
  it keeps, the same as `leak` of the `auto key:` line), `arms` (no arm labels yet: `-`), `known cells` (of
  96 x 96) and the 10 leakiest cells as `yaw/pitch@leak` (degrees from the map anchor, yaw positive to the
  left of -Z, pitch positive up).

How it works: the CPU turns the leak map snapshot into one byte per cell (class: none, medium, high,
unknown) and uploads a 96 x 96 R8 texture when the snapshot changes. The stream shader (depth path) maps
each view ray, 0.8 m out from the eye, to the octahedral cell from the map anchor, as the learner does,
and draws the pattern in map space so the hatching stays fixed in the room.

On the headset (one minute): color key on, `setprop debug.alvr.problem_view 1`. The game dims and the room
is grey-dotted; look around for 10-20 s: the dots clear where the camera has seen the room, and amber / red
hatching stays where the key would let the room through (skin-coloured desk, wood, a beige wall). Hold a
hand over a red area: that is where the rim leaks. Open the play-area editor: the view goes away. `adb
logcat -s ALVR_CAM | grep sep:` shows recall near 1 and low leaks when the key separates well.
