# Mask v2: the design (lead)

Status 2026-11-24. A design, nothing is built. It starts after arm step 1 (`arm_shift 2`, being built) and
replaces everything after step 1 of `ALVR/docs/arm-mask-build-plan.md`. It merges
`design-a-prediction.md` ("A") and `design-b-appearance.md` ("B"), with five new checks by the lead ("L1-L5").

Words: **measured** = on the headset, or offline on recorded data by an earlier job (KB). **A / B checked** =
computed by designer A or B today. **lead checked** = computed for this document; scripts and outputs are in
`logs/analysis-20261124-design/lead/` of the project folder (local only, not in this repository).
**estimated**, **unchecked** = nobody has tested it.

## 1. The idea

1. The depth is the truth, but late (124-152 ms) and without the hands and what they hold. So the picture is
   drawn from **things with a fast cue**, and the depth only corrects them. This is A's rule and the user's
   principle (depth = GPS, fast cue = accelerometer). The cues are: the head pose (the room), the wrist (the
   forearm, step 1), the palm (a held thing), the steering angle (the wheel).
2. Everything is drawn for one moment: `T = display - arm_target_ms`, the moment the passthrough picture
   shows. Step 1 finds that number by eye. All later pieces use it.
3. **Geometry first, colour last.** Every piece in the main line works in the small mask (428 x 448 per eye)
   or on the CPU. No new layer, no larger keyed rect, no camera image on the GPU. B's colour pieces are kept
   as later, gated options, because their own numbers show no gain beyond noise for the arm and because the
   lead's check shows the edge matte is unsafe on a cluttered desk.
4. The camera is used where it is strong and its delay does not matter: to **learn a map** of something that
   moves in a known way. The lead's check shows this works for the wheel: a hole map learned from colour in
   the wheel's own turning frame finds 84-95 % of the hole cells with no green screen.
5. Wrong room over the game (LEAK) and flicker are worse than a late edge. So every new piece either only
   adds what a strict test allows, or has a log-only step before it is drawn, and falls back to today's
   picture with its switch at 0.

The pieces, in build order:

| # | piece | from | fixes |
|---|---|---|---|
| 1 | edge cut: draw the mask edge on the object, not 4-6 mm outside it | lead | the rim of room around still objects (part of "depth mask resolution") |
| 2 | wrist gap: the forearm's depth continued to the hand | A, changed | the cut strip between hand and sleeve |
| 3 | room memory: rolling, in a wide camera fixed to the rig, read only where frozen | A, changed | stale chair / legs / bottle, the cap at the grab, knees next to resting hands |
| 4 | carried things: what a closed hand holds rides the palm | A, with a new claim test | the moved shifter / bottle |
| 5 | wheel map: outline and holes learned in the wheel's frame, turned by the angle | A + B idea, lead checked | holes of the wheel, late spokes |
| 6 | later, each behind a gate: erosion, edge matte, remembered colour and key safety, arm band, hands on the same clock | B (and A) | what is left |

## 2. What was taken, changed and dropped

| problem | A | B | this design |
|---|---|---|---|
| arm after step 1 | wrist gap by a shifted lookup (A checked: strip covered 78.5 -> 90.7 %) | keyed arm band with a safety map and a sleeve class (B checked: 27/22, 15/9, 17/6 against 30/25, 19/13, 19/9 for the moved depth: "differences are noise") | A's gap, rewritten so it can only add and also works for a still arm. B's band is the last option (step 9): it costs 2-5 points of GPU load and a sleeve table for a plaid sleeve, for no scored gain |
| held things | fresh memory at the grab, then ride the palm; grip cue from joints; veto untested | change map as a vote; per-point test weak (28-48 % against 8-12 %) | A's mechanism. The claim is made on the CPU from a readback and must pass a shape test (L3). B's colour vote is dropped: it needs the whole remembered-picture machinery for a weak signal from one move |
| stale legs, chair | rolling memory, updated in place, frozen within 6 cm of a hand | depends on "the rolling refresh of the other plan" | A's memory, but in a wide camera fixed to the rig from the start (L4), and read only where it is frozen |
| coarse edges of still objects | not fixed | two-colour matte from the camera texture | First the edge cut (free, L1). The matte only later and only for clean outlines: at the near desk its edge is two-valued (L1) |
| wheel with holes | carrier = steering angle, hole map by colour variance; unchecked, on hold | the same, unchecked, on hold | Same idea, now checked on the cardboard wheel (L2). Can be built and viewed before a wheelbase is there, with the hands as the angle |
| legs and chair in motion | no prediction (it would flicker) | - | Agreed. They stay one depth frame late |
| one clock for hands and arm | `hand_target` | - | Kept as a late option. Today's display-time joints lag 40-76 ms, which is about the passthrough's own delay, so the hands already sit close to the picture |

Dropped for good: A's re-anchor by head turn (`mem_anchor_deg`) and its arm-tube rule; B's change map as a
solid-fill veto and as "the object left"; B's remembered picture as the base of the design; the arm fitter
and the keyed zone of the earlier plan (its steps 2a, 2b, 3). Still do not build: a depth stack, a voxel
grid, a flood-fill zone, a geometric solid fill on the arm, depth-only prediction of legs.

## 3. New checks by the lead

All offline on the recorded bursts and captures. Scripts `l1`..`l5` with their `.txt` in the lead folder.

**L1. Still objects are drawn 4-6 mm too big, and the reason is not the camera model**
(`l1_edge_registration.py`, `l1b_hist.py`; the same border pairs and edge measures as B's `x3`).
- B found that the camera edge and the depth border disagree by "-10..+7 mm, cause not known". The KB had a
  hint of a 5 % scale error of the camera model. Tested: fit `offset = a + s * (distance from the image
  centre along the border normal)`. Result: s = +0.1 % (burst 7), -1.9 % (11), +0.1 % (13), -0.3 % (3),
  pooled -0.7 % (bootstrap -1.0..-0.4 %), 280 pairs. **There is no 5 % scale error in these bursts.**
- Correction of B's burst 3 row (+1.3 / +3.3 / +5.7 mm): it mixed in frames where the head moved. With the
  still part only (11 depth frames, 174 pairs) the offset is -6.2 / -4.4 / -2.4 mm (p10 / p50 / p90): the
  depth border lies 1.2 texels outside the camera edge, with a spread of only 1.4 mm after the fit.
- Two clean cases, one tight group each: the lap at 0.73 m (burst 7, 36 pairs): camera edge 0.43 texel
  (2.4 mm) inside the depth border; the cardboard wheel at 0.43 m (burst 3): 1.23 texel (4.1 mm) inside.
  This agrees with the KB (objects come out about one texel too big, +3.5-5.5 mm).
- The drawn edge is even further out. The mask blends 3 x 3 depth texels and `stream.wgsl` cuts at coverage
  0.05-0.35. For a straight edge that cut lies 0.37 texel outside the texel border (arithmetic of the
  B-spline weights). So today the drawn edge is **0.76 texel (4.2 mm) outside the camera edge on the lap
  and 1.70 texel (5.6 mm) on the wheel**. A cut at coverage 0.8 lies 0.37 texel inside the border: it would
  land at -0.02 texel (lap) and -0.96 texel (wheel) from the camera edge.
- The near, cluttered desk (bursts 11 and 13, surface at 0.30 m, 28 and 42 pairs) is different. In burst 13
  the offsets form **two groups** about 1 cm apart: 17 pairs at -10..-4 mm and 25 at -2..+6 mm. In burst 11
  there is one group at +2..+8 mm and a tail down to -14 mm. A matte would jump between two edges there. So
  B's matte is safe only for clean outlines; B's failure mode "a texture edge inside the object is stronger
  than its rim" (or a wrong depth for the near object) hits about 40 % of those pairs.
- Limits: few pairs (36-174 per burst), left eye, 320 x 240 camera, the camera edge itself is good to about
  one camera pixel (1.4-3.4 mm). Whether the camera picture and the compositor's passthrough picture agree
  is still unchecked (step 7).

**L2. A hole map of the wheel can be learned from the camera in the wheel's turning frame**
(`l2_wheel_holes.py`, picture `l2_map_b6.png`). Both designers proposed it, neither tested it.
- Data: bursts 6 (wheel turned over 190 degrees in 3 s, 40 camera images), 5 (60 degrees, 41 images), 4
  (held still). The wheel's pose per image is the one the earlier wheel analysis fitted
  (`campose_b<n>.npz`); it stands in for "plane from the depth + steering angle".
- Learner: a 4 mm grid in the wheel frame. Each image gives each cell one colour; pixels under a hand
  capsule + 1.5 cm are skipped. Per cell: the spread of its colour over the images (largest channel std).
- Result, hole cells against solid cells (8 mm borders left out): spread p50 54 against 12 levels (burst 6),
  52 against 10 (burst 5). AUC 0.984 and 0.979. With a cut at 30 levels: **95 % of the hole cells found, 5.9 %
  of the solid cells called hole (burst 6); 84 % and 1.8 % (burst 5).** All four holes are found (per hole
  at a cut of 20: 95-100 % in burst 6, 83-100 % in burst 5).
- The map also shows the wheel's outline: everything outside the octagon has a high spread. Cells under
  the hands are never seen (the hands turn with the wheel) and stay "unknown".
- It needs turning: with the first 10 images (44 degrees) 49-55 % of the hole cells, with 97 degrees 72-79 %.
  On a still wheel it finds nothing (AUC 0.66), as it should.
- It forgives a wrong angle: a random angle error of 3 / 6 degrees per image gives AUC 0.988 / 0.986 (false
  hole 6.3 / 9.1 % at a cut of 30). A centre error of 1 cm: false hole 11.7 %.
- Dividing out the brightness makes it worse (AUC 0.67): keep plain colours.
- Limits: one cardboard wheel with a bright, patterned background behind it; the pose came from the same
  camera images (through a template with holes), so it is better than a real angle would be at first; the
  arms were not masked out. A dark wheel in front of a dark background, a display or LEDs on the wheel
  (they would read as holes) and a real wheelbase are unchecked.

**L3. A claim can be tested by its shape before anything is carried** (`l3_claim_shape.py`).
- A's rule marks, near a closed hand, every remembered texel the runtime removed. A's own check says a fist
  passes the grip cue, and the wrong claim (a patch of thigh travelling with the fist) could only be undone
  later by a veto that could not be tested.
- Test: a held thing stands out from what is around it; a patch of thigh or desk continues smoothly. Take
  the 2-texel ring around the claim **in the memory** and count the ring texels whose remembered depth is
  within 3 cm of the claim next to them ("continuous").
- Right claims (bottle held, memory has it at the right place: start dumps of bursts 10, 11, 12 and captures
  641, 642; both eyes, 10 cases): **22-36 % continuous**. Wrong claims (hand on the lap going to the bottle
  640; lap captures 620-623, 625, 626, 627; the phone at the desk edge 607; 15 cases): **53-97 %**.
- A limit of 40 % separates them, but the gap (36 against 53) is small, it is one bottle, and the lap copies
  were stale (5-95 s old). After the move with a stale memory (burst 13, capture 643) the value is 45-53 %:
  such a claim would be refused, which is the safe side. So this is a gate in front of the claim, not a
  proof. The log-only step (step 4) has to confirm it with a fresh memory.

**L4. A memory kept in a past depth view does not cover the hands; the memory camera must be wide and aimed
at the work space** (`l4_views.py`).
- The depth view equals the eye view: 94 x 99 degrees (tangents -1.38, 0.84, -1.43, 0.97).
- With the memory camera left at the first depth frame of a burst, the hands' bounding spheres (+ 6 cm) are
  fully inside it in only 55-77 % of the hand-frames on the wheel (bursts 5 and 6, head still within 2.5
  degrees: the hands sit at the lower edge of the view), and in 0-19 % once the head turns 22-35 degrees
  (bursts 1, 2, 3). The KB already had the extreme case: a copy taken looking 92 degrees away covers none
  of the lap.
- A's check of the in-place memory (100 % of the held bottle kept with a 6 cm margin) stands, but it used
  the copy's own camera for 3 s with a still head. It supports "do not re-project every frame". It does not
  support "the last snapshot's camera is good enough". So A's later step E (a wide camera fixed to the rig)
  is the starting point here, and A's re-anchor by head turn is not needed.

**L5. The rim of the hand zone is not the leg flicker** (`l5_rim.py`). In burst 7 (hands still on the lap) the
rim is 0.1-1.0 mm and changes by more than 0.5 mm in 10-12 of 217 frames; capsule points step 0.01-0.02 mm
(p50). That is sub-millimetre. With the KB's result (not the depth, not the joints) the suspects left are
the per-capsule solid weights and the colour table (about 5 updates per second), which the bursts do not
record per frame. Step 3 tests this on the headset.

**Reading of the designers' own evidence.**
- A's wrist gap check is sound for the arm (a forward model, which is what "add only" needs). What it did
  not score, the background that the term as written would also shift, is removed by the rewrite in 4.2.
- A's design writes the live depth into the memory outside the margin, arm included, and the mask takes
  `max(live, memory)`. The arm in the memory is not moved by step 1, so near the hand the late arm would
  come back. A's fix (an arm tube around the elbow guess, which is 13-38 degrees off) is replaced by "read
  the memory only where it is frozen" (4.3).
- B's arm numbers do not carry a build: B says so. B's rim-of-the-hand gain is real in one burst only (b8:
  24.9 -> 13.4 %; 11.5 -> 9.3 and 12.9 -> 11.9 % in the others).
- B's timing table is useful and kept: an app-side camera mask is 30-40 ms behind the shown picture at best
  (122-137 ms today), so nothing that moves fast may be decided from our camera image.

## 4. The mechanism

All switches are `debug.alvr.` properties, polled like `arm_shift`. At 0 each gives today's picture.

### 4.1 Edge cut (`edge_cut`)

- Today: `stream.wgsl`, `mask = smoothstep(0.05, 0.35, mask_sample.r)`. The comment says it is centred
  below 0.5 to grow fingers. Fingers are drawn by the hand zone now, so the growth only makes a rim of room
  around every object (L1: 4.2-5.6 mm).
- New: the centre of the cut comes from a uniform (a free component, for example `area.z`; 0 means 0.2):
  `lo = clamp(cut - 0.15, 0.02, 0.69); mask = smoothstep(lo, lo + 0.3, r)`. Property `edge_cut` in percent:
  20 today, 50, 80. The edge moves inward by 0.37 depth texel from 20 to 50 and by 0.74 from 20 to 80
  (2.7 mm at 0.5 m, 5.3 mm at 1 m).
- Known price: a line one depth texel wide (3.6 mm at 0.5 m: a cable) has a peak coverage of 0.75. It starts
  to fade above a cut of 60 and is almost gone at 80. The arm gets 2-3 mm thinner on each side.
- If the user wants more than 80 gives (the wheel in burst 3 would still be 0.96 texel too big): step 6a, a
  real erosion by one texel at depth steps (`edge_inset`), from a 5 x 5 read in the border branch only.

### 4.2 Wrist gap (`arm_gap_mm`, `arm_gap_hands`)

The runtime removes the forearm 0.6-2.6 cm behind the wrist (5-8 cm with hands on a wheel, measured). Step 1
moves the arm but cannot bring back what is not in the depth.

A's idea: a ray that passes just behind the wrist reads the forearm's depth `G` further up the arm, so the
arm is continued to the hand like a tube. Kept. Three changes:

1. **It only adds.** A wrote it as one more term inside `arm_displacement`. That also shifts the background
   and objects within 8 cm of the wrist, and it sits behind the `continue` that skips still arms. Here it is
   its own small function, called from `fragment_main` after `current_mask`:
   `result.r = max(result.r, gap_coverage(origin, dir, hand))`.
2. `gap_coverage`, per hand with a gap:
   - the gap segment runs from the wrist to `wrist + G * axis`; `axis` = from the middle knuckle to the
     wrist (joints at the target time; A checked: 90.7 % covered against 90.3 % for the elbow guess, and less
     leak, 5.9 against 7.3 % of the strip);
   - skip unless the ray passes within 5.5 cm of that segment (one ray-capsule test);
   - `Q` = the ray's point nearest the segment, `along` = its place on it (0..G);
   - look the depth up at `Q + axis * (G - along)`, minus step 1's displacement for that place (the depth
     still shows the arm where it was);
   - accept the surface found only if it lies within 6 cm of that lookup point (an arm is there, not the
     wall behind); then the coverage is `region_mask` of the surface moved back by `axis * (G - along)`;
   - blend the 2 x 2 texels around the lookup, so the edge is not blocky.
3. Uniforms: wrist and step 1's displacement are in `arms`; the knuckle axis and `G` need one `vec4` per
   hand (`arm_gap: array<vec4f, 2>`, the uniform size test changes from 4144 to 4176 bytes), or the unused
   bridge slots while `key_wrist_len` is 0.

Default `arm_gap_mm` 30. A checked (bursts 8 / 2 / 6, 98 image-hands, strip 0-6 cm behind the wrist): arm
covered 78.5 % -> 90.7 %, visible leak 3.4 -> 5.9 % of the strip. 50 mm covers 94.0 % but leaks 11.9 %: only
worth trying with the hands on a wheel. Not fixed: a gap longer than `G`; the first cm next to the capsules
when the knuckle axis is off (median 9-35 degrees from the fitted arm axis).

### 4.3 Room memory (`mem_roll`, `mem_margin_mm`, `mem_view`)

Today `playzone_copy.wgsl` copies the whole depth image only when no hand was near the depth view for 1.5 s.
While hands are in view the copy is frozen everywhere and in whatever camera it was taken.

**The memory camera** is fixed to the rig (the area frame), not to a past head pose:
- position: the middle of the two depth cameras at the moment it is anchored;
- direction: fixed, towards the work space (from the head to the centre of the play-area bounds; if that is
  nearer than 30 cm, the area's forward axis pitched down by `mem_pitch_deg`, default 35);
- field of view 120 x 120 degrees, texture 512 x 512 (0.0068 per texel in tangent units, the same as the
  depth's 0.0069). A head turn changes nothing in it.
- Anchored again: (a) cleanly, everything from the live depth, when no hand is near the depth view for 1.5 s
  (today's trigger) and the head is more than 3 cm from the anchor; (b) with hands in view only when the
  head is more than 12 cm from the anchor, by one re-projection pass that keeps the frozen texels (A
  checked: 50 such passes lose 2-21 % of a held object, so one pass now and then is harmless).
- First build: keep the two textures and `playzone_eyes` (one per eye, both with this camera, each fed by
  its own depth image). Merging them into one is a later saving.

**Channels** (`Rgba16Float`, as today): `r` distance (0: nothing), `g` owner (step 5 of the build), `b` frozen
(1: kept this update because a hand is near), `a` seconds since the last live write (for the debug view and
an optional age limit `mem_max_age_s`, default off).

**Update pass** `playzone_update.wgsl`, once per eye for every new depth frame (15-21 per second), old texture
in, new texture out:

```
z_old  = old memory at this texel
live   = the live depth surface along this texel's ray (2 fixed-point steps, start at z_old or 0.7 m);
         valid only if the surface found lies within 1.5 texel widths of the ray (else the head's offset
         from the anchor hides or uncovers it: no value)
target = the remembered point if z_old > 0, else the live point
near   = the depth camera's ray to `target` passes within mem_margin (6 cm) of a hand capsule,
         capsules located AT THE DEPTH'S CAPTURE TIME (step 1 already looks the joints up there)
no live value:        keep z_old, frozen 0
not near:             write live, frozen 0, age 0
near and z_old == 0:  write live, frozen 0        (the fill is better than nothing)
near:                 keep z_old, frozen 1
```

Optional outer ring, `mem_outer_mm` (default 0 = off, try 120): between the margin and this distance a
texel is kept (frozen 1) only if the live value is more than 8 cm behind `z_old`; anything nearer or about
equal is written. Reason: the runtime's fill is always the far background (11 cm to 1 m behind), and it
reaches past 6 cm in a few percent of the texels (max 11-13 cm, measured). Unchecked.

**Reading** (`depth_mask.wgsl`, `playzone_coverage`): only texels with frozen = 1 count. A texel that is not
frozen equals the live depth by construction, so nothing is lost, and the arm (written outside the margin,
not moved by step 1) can never be drawn twice. The near-hand zone (`focus_area.wgsl`, `near`) grows by the
wrist's travel since the depth capture (step 1 has that number), so the zone still covers the place where
the runtime removed things 130 ms ago when the hand moves fast.

A's evidence carries over: in place and frozen within 6 cm keeps 100 % of a held bottle over 3 s (3 cm: 78-86
%); a refresh away from the hands removes stale legs (LEAK 2580 -> 977 and 1742 -> 1241 texels; 1400-3700
texels per eye in the older laps). A's warning also carries over: a remembered surface is 8-19 mm off after
the head has moved 3-13 cm, so the memory is a fallback for the frozen zone only, and that zone stays small.

### 4.4 Carried things (`carry` 0 / 1 / 2)

A's state machine per hand, on the CPU next to `arm_motions`:

| state | enters when | what it does |
|---|---|---|
| IDLE | start; the hand opens; tracking lost for 0.5 s | nothing rides; owned texels are dropped |
| HOLD | grip cue on for 150 ms and wrist slower than 0.15 m/s | `pose_ref` = palm pose at the capture time of the current depth frame; a claim is made, and made again every 0.5 s |
| MOVE | the palm is more than 1 cm or 3 degrees from `pose_ref` | the claim rides the palm |

- Grip cue (A checked): the tips of index, middle and ring are more than 25 mm above the palm plane. 0 % of
  the lap frames, 100 % of the bottle frames, 51-100 % of the wheel frames. A fist passes it.
- **The claim is made on the CPU**, not in the update pass. At HOLD the memory and the depth of one frame are
  read back (the burst readback path does this already; about 400 KB each, once per 0.5 s per holding hand).
  Claim = frozen texels whose live depth is more than 5 cm behind the memory (or missing), whose remembered
  point is within 20 cm of the palm. Then:
  - connected pieces under 30 texels are dropped;
  - **shape test (L3):** the claim is refused if more than 40 % of the ring around it continues smoothly in
    the memory. Bottle 22-36 %, lap and desk patches 53-97 %;
  - refused if larger than `carry_max_texels` (default 2000);
  - refused where the wheel map (4.5) owns the texel.
  The owner mask is uploaded into the memory's `g` (or a small R8 texture of the same size).
  The same Rust function runs offline on bursts, so it can be tested before it is drawn.
- Riding (A): in MOVE, `depth_mask.wgsl` does a second memory lookup with the ray moved back by
  `M = pose_ref * pose_T^-1` (one `mat4` per hand) that accepts only texels owned by that hand; the normal
  lookup skips owned texels, so the old place empties. `pose_T` is the palm's rigid pose (6 palm joints)
  predicted to `T` like the wrist, **rotation included** (A checked: error of a point 12 cm from the palm,
  40 ms ahead: 12.4 mm with the old pose, 7.7 translation only, 5.1 full pose; at rest 0.02-0.2 mm steps).
- Release: owned texels are dropped, not written back. The object shows again from the live depth once the
  hand is 6 cm away.
- Veto by the delayed truth (A): an owned texel whose old place is trusted again (no hand within the margin)
  and still shows the same surface (within 3 cm) did not move with the hand: back to the room.
- Measured before on ONE move (642 -> 643): cap cut 348 -> 9 texels (check 401 -> 24), old-place leak
  1399 -> 163 (check 789 -> 1). With a wrong grab: 137 texels of floating leak. That is why `carry 1` (log
  only) comes first.

### 4.5 The wheel (`wheel` 0 / 1 / 2 / 3)

The depth never has the holes (6-11 cm holes are filled with the wheel's plane, measured) and it is 17
degrees late at 138 degrees per second. The colour key cannot tell wheel from room. So the wheel is drawn
from a map that turns with it.

- **Disc**: centre, axis and outer radius in the rig frame, found once and stored with the play area. Axis
  = the normal of the plane fitted to the depth on the wheel (measured rms 4 mm). Centre = the fixed point
  of the palms' motion while both hands turn the wheel once left and right; with a wheelbase the same fit
  uses the PC angle. Radius = the largest radius with solid cells in the map + 2 cm.
- **Angle at `T`**: the steering angle from the PC, sent with the tracking data and extrapolated to `T` with
  its own speed. Without a wheelbase: both palms as one rigid body (measured 1.3-3.0 degrees error against
  3.6-19.8 for the old depth), one palm when only one hand holds.
- **Map**: 128 x 128 cells of 4 mm in the wheel frame: unknown, solid or open. Learned on the camera thread
  (`auto_color_key.rs`), from the 320 x 240 frames it already has: per cell the running mean and spread of
  the colour, with the angle of the image's own time. Skipped: pixels under a hand capsule + 1.5 cm and
  inside the forearm stub. A cell is decided after 8 samples spread over 45 degrees of turning: open above
  30 levels of spread, solid below 20, else unknown. A 3 x 3 majority cleans single cells. Unknown counts
  as solid, which is today's picture. Lead checked (L2): 84-95 % of hole cells found, 1.8-5.9 % of solid
  cells called open, and it forgives 3-6 degrees and 1 cm.
- **Drawing** (`depth_mask.wgsl`, one test per ray: does it hit the disc):
  - `wheel 2` (only opens): where the depth surface lies within 3 cm of the wheel plane and the cell is
    open, the wheel's depth is ignored. What is behind is decided by the play area alone: if the area goes
    on for more than 30 cm behind the wheel along the ray, the room shows (the legs under the wheel), else
    the game shows (the dash through the upper openings). No depth is needed behind the hole, and the depth
    has none.
  - `wheel 3` (the map decides): a solid cell is shown as room even if the depth is late there; an open
    cell as above. Hands and arms in front of the wheel are inside the play area anyway. This also removes
    the late edge of the spokes and of the outline.
- Hands on the wheel: the wheel map owns the disc, so the carry rule (4.4) makes no claims there.

### 4.6 Later pieces, each behind its gate

- **Erosion** (`edge_inset`): see 4.1.
- **Edge matte** (B's piece B, `edge_matte`): only where the depth step is clean: the inside and outside
  groups of the 3 x 3 are each within 2 cm, the surface is farther than 0.4 m, the colour contrast is over
  30 levels, and the matte's edge lies within 1.5 depth texels of the depth border (else the depth decides).
  The last rule is the guard against the two-valued edges of L1. Needs the camera texture on the GPU and the
  registration measurement of step 7.
- **Remembered colour and key safety for the hand's rim** (B's piece A, use 1): the memory pass can also
  store the camera colour of each remembered point; the hand's rim then closes where the table would keep
  the remembered background. Gain measured by B in one burst of three. Only if the user still names
  "lines and patches of room next to the hand".
- **Arm band with a sleeve class** (B's piece A, use 2): only if, after steps 1 and 2, the user still sees
  a coarse or late arm edge, and the rect proxy of the old plan shows at most 4 points of GPU load.
- **Hands on the same clock** (A's `hand_target`): only if the user sees a step between hand and arm during
  fast moves.

### 4.7 Which signal decides which pixel

| where on screen | fast cue (72 fps) | delayed truth | pass |
|---|---|---|---|
| hand capsules and rim | hand joints, colour key | none | `focus_area.wgsl`, unchanged |
| 0 to G behind the wrist | wrist + knuckle axis | the forearm's depth G up the arm | `gap_coverage`, new |
| forearm to 30 cm | predicted wrist (step 1) | live depth | `arm_displacement`, step 1 |
| near a hand, frozen texel, owned, hand moving | predicted palm pose | memory | second lookup, new |
| near a hand, frozen texel | head pose | memory | `playzone_coverage`, frozen only |
| the wheel disc | steering angle | wheel map (learned from colour), wheel plane from depth | new |
| everything else | head pose | live depth, edge cut | `current_mask`, `stream.wgsl` |

## 5. What the user should see, complaint by complaint

- **"An object in the play area, the zone behind it should be cut off: the depth mask resolution."** Step 1:
  the rim of room around still objects shrinks by 3-5 mm, so the game starts at the object. Not fixed by
  step 1: the texel-sized bumps along the edge and its 1 mm wobble. Those need the erosion (step 6a) and
  the matte (step 8), and the matte only works on clean, high-contrast outlines.
- **"The depth mask moves at 30 fps with my whole arm, my hands move at 72 fps."** Step 1 of the arm plan
  moves the forearm every frame. Step 2 here closes the strip next to the wrist. Not fixed: upper arm and
  elbow (no cue), the sleeve's fine edge (one depth texel).
- **"Why not a mask, like the hand?" / "interpolate the depth to 72 fps."** The second is what is built.
  The first (a keyed arm) is step 9 and needs a learned sleeve table; its offline gain is within noise.
- **"The depth mask doesn't quickly update for things that aren't static (chair, legs, bottle)."** Step 3:
  the memory is at most one depth frame old everywhere except within 6 cm of a hand. A moved chair or leg
  leaves no ghost. The bottle's old place empties when the hand is 6 cm away.
- **"The moment I grab it, the cap disappears."** Step 3: the memory is live until the hand arrives, so the
  cap is always remembered where it is grabbed. Step 5: when it is moved, it rides the palm.
- **Legs cut out next to hands on the lap.** Step 3: the thigh next to the hand is remembered from just
  before the hand landed, and the wide rig camera keeps the lap from the last time it was in view. Not
  fixed: a lap that was never seen without hands; legs that move under resting hands.
- **Leg flicker with still hands.** Not explained. Step 3 makes the zone next to still hands come from a
  frozen memory, which cannot flicker. If it still flickers, the cause is the solid fill or the colour
  table, and the step-3 burst records them.
- **"A steering wheel with holes."** Step 6: the holes show the game where the play area ends behind them,
  and stay open while the wheel turns. No green screen. Not fixed: holes before the wheel has been turned
  about 45-90 degrees; a wheel that has the colour of what is behind it.
- Also not fixed: an object picked up that was never in the memory; the first 130-200 ms after something
  without a cue starts to move; thin or see-through things.

## 6. Cost per frame

Mask 428 x 448 = 191,744 texels per eye. All figures are **estimated**; each step measures its own.

| piece | GPU | CPU |
|---|---|---|
| edge cut | 0 | 0 |
| wrist gap | 1 capsule test per hand per mask texel; 4 depth reads only in the strip | 0 |
| room memory | 1 pass per eye per new depth frame: 262,144 texels at 512 x 512, each 1 memory read + 3 depth reads + 2 sphere tests, the capsule loop only near a hand. About 9 M texels per second for both eyes against 27.6 M for the mask pass: about a third of the mask pass. Half of that with 384 x 384 or one shared texture | capsules at capture time: joints step 1 already locates |
| carry | 1 more memory lookup for near-hand mask texels while a closed hand moves (12-30 k of 192 k per eye): +4-10 % of the mask pass. Readback of 2 x 0.4-1 MB every 0.5 s while a hand holds | claim: one labelling of a 320-512 square window, under 1 ms; two rigid fits of 6 points per frame |
| wheel | 1 ray-disc test per mask texel; 1 read of a 128 x 128 map on the disc | learner: about 12,000 cells per camera image (12-14 per second), 1-2 ms on the camera thread |
| later: matte, remembered colour | camera texture upload (230 KB, 14 per second), 3-12 fetches at border texels | - |
| later: arm band | lower layer rect grows: +2 to +5 points of GPU load (B) | sleeve class |

No new compositor layer and no change of the hand rect in steps 1-6.

## 7. Failure modes and switches

| piece | switch | failure | how it shows |
|---|---|---|---|
| edge cut | `edge_cut` 20 / 50 / 80 | cut too high | thin things vanish (cables, a thin lever); the game shows a hair inside an object's edge |
| wrist gap | `arm_gap_mm` 0 / 30 / 50, `arm_gap_hands` 1 / 2 / 3 | knuckle axis off | the strip fills only partly, or 1-2 cm of room beside the wrist in front of the game |
| | | the sleeve cuff is wider than the wrist | a slightly too wide wrist |
| room memory | `mem_roll` 0 / 1, `mem_margin_mm` 60, `mem_outer_mm` 0 / 120, `mem_pitch_deg` 35; `mem_view 1` tints what comes from the memory blue, darker with age | the fill reaches past the margin and is written | a held, unmoved object loses parts after seconds (today it does not): set `mem_outer_mm 120` |
| | | margin too large | a ghost of a moved thing near the hand until the hand leaves |
| | | head far from the anchor | edges near the hand off by 1-2 cm until the next anchor |
| | | the hand pulls back over its own forearm, then moves sideways | a stub of remembered arm (up to 6 cm) next to the hand for about 0.2 s. If seen: do not write texels inside a forearm stub (wrist to 20 cm along the knuckle axis, radius 6 cm) |
| | | the wide camera is aimed wrongly | no blue in `mem_view` near the hands at the wheel or the shifter: change `mem_pitch_deg` |
| carry | `carry` 0 / 1 (log) / 2 (draw), `carry_wrap_mm` 25, `carry_shape_pct` 40, `carry_view 1` tints owned texels orange | wrong claim passes the shape test | a patch of room travels with a fist until the hand opens or the veto acts |
| | | right claim refused | as after step 3: the object is cut while it is moved |
| | | palm prediction error | the far end of the object swims by 5-10 mm during the move, not at rest |
| | | release | the object is cut between opening the hand and moving it 6 cm away |
| wheel | `wheel` 0 / 1 (learn + view) / 2 (open holes) / 3 (map decides), `wheel_view 1` draws the map on the wheel | angle late or wrong | holes and spokes slide against the real wheel while turning |
| | | solid cell called open | a piece of spoke or rim shows the game |
| | | a display or LEDs on the wheel | they read as holes: the game shows there |
| | | wheel and background of one colour | holes stay closed, as today |

## 8. Build order

Each step is one small build with its live switch, judged by the user on the headset before the next one.
For every step: a progress screenshot (`tools/progress.py shot`), one burst with the switch on and one with
it off, and the GPU load (`vrstat`) on and off.

**Step 0 (running, no new build): arm step 1.** Switch `arm_shift 1 / 2`, `arm_hands 1`. Also find
`arm_target_ms` (20 / 40 / 60) by eye during a wave: that number is `T` for every later piece. Record:
bursts as b8 and b2 with the arm fields. Stop rule: none here; if no value looks right, measure the
passthrough delay first (a screenshot during a wave with a burst running).

**Step 1: edge cut.** Switch `edge_cut 20 / 50 / 80`. About 5 shader lines and one uniform component.
The user looks at a still object in front of the game (the cardboard wheel's top, the bottle on the desk,
the monitor stand), head still and moving: is the rim of room around it gone, does anything thin vanish,
does the edge flicker more? Record: screenshots at the three values with the head still; bursts as b3 and
b7. Stop rule: if 80 eats objects or the user sees no difference, keep the best value and do not build the
erosion (step 6a).

**Step 2: wrist gap.** Switch `arm_gap_mm 0 / 30 / 50` with `arm_gap_hands 1` (left arm new, right arm as
before). About 40 shader lines, one `vec4` per hand. Can share a build with step 1. The user looks at hands
on the wheel and on the lap, sleeve down and pushed up: is the strip between hand and arm closed; is there
room beside the wrist in front of the game? Record: bursts as b4, b2, b8 with the gap fields; one with a bare
forearm. Stop rule: room beside the wrist at 30: try 20; still there: leave it off. After this step ask the
old gate G1 again: is the arm's edge still too coarse or too late? If no, the arm band of step 9 is dead.

**Step 3: room memory.** Switch `mem_roll 0 / 1`, `mem_view 1`. One new pass, ping-pong textures, the rig
camera, the frozen flag in the read. The user:
- moves the bottle 10 cm and lets go: the ghost goes when the hand leaves; grabs again: whole;
- moves the chair and the legs; puts the hands on the lap and looks at the knees next to them;
- holds the bottle still for 30 s and looks around, also from low and from the side: it must stay whole;
- waves an arm in front of the game: no second, late arm near the hand;
- sits with still hands on the lap: is the leg flicker still there?
Record: captures before and after as 642 / 643 and 626 / 627; a 3 s burst holding the bottle while turning
the head; the memory texture (all four channels) in the dumps; from this build on, the per-capsule solid
weights and the colour table's version in every burst display record. Stop rules: the held bottle loses
parts: `mem_outer_mm 120`, then `mem_margin_mm 80`; still: back to `mem_roll 0` and re-think. More than 2
points of GPU load: 384 x 384, then one shared texture.

**Step 4: carry, log only.** Switch `carry 1`. The state machine, the readback, the claim with the shape
test, one log line per HOLD (`carry: R HOLD, claim 512 texels, continuous 27 %, accepted`). Nothing to see.
The user spends 5 minutes on lap, cardboard wheel, bottle as shifter, drinking from the bottle. Record: the
log; three bursts with the bottle grabbed and moved. Stop rule (gate for step 5): accepted claims that are
not a held thing (lap, air, the wheel disc) in more than 1 of 20 HOLDs, or real grabs refused in more than
1 of 5: fix the test offline on the new bursts before drawing anything.

**Step 5: carry, drawn.** Switch `carry 2`, `carry_view 1`. One `mat4` per hand, the second lookup, release,
veto. The user moves the bottle 5, 10 and 20 cm and turns it: the cap stays, no ghost at the old place;
lifts a fist from the lap to the wheel in front of the game: no patch of room; shifts with the bottle.
Record: a burst of the move with the carry state per display frame; score cap cut and old-place leak like
the bottle check. Stop rule: any travelling patch of room the user notices in normal use: back to `carry 1`.

**Step 6: wheel map.** (a) `wheel 1`: disc fit, learner, `wheel_view 1`. On the cardboard wheel with the
hands as the angle: does the drawn map show the four holes after a few turns, and stay on them while
turning? Record: the map as a file with each burst; bursts as b5 and b6. Stop rule: the map does not settle
or slides by more than 2 cm: wait for a wheelbase (the cardboard wheel has no fixed axis; its centre moved
2-5 cm in burst 6). (b) with a wheelbase: the angle from the PC; `wheel 2` (open the holes), then
`wheel 3`. The user turns the wheel slowly and fast: holes show the game where the desk is behind them,
spokes do not trail. Stop rule: holes or spokes slide against the wheel at speed: fix the angle's timing
before anything else. Step 6a can be done any time after step 1 if the user asks for a tighter edge:
`edge_inset 1`.

**Step 7: registration, no build.** With the headset still (on a stand): three screenshots and three
captures of the QR code and of a few sharp objects at 0.4-0.8 m inside the play area. Offline: map the camera
image to the eye view with the depth and measure how far its edges lie from the same edges in the
screenshot. Gate for step 8: under 2 mm at 0.5 m, or a constant offset.

**Step 8: edge matte, gated.** Only if the user still names coarse object edges after steps 1 and 6a, and
step 7 passes. Camera texture, `edge_matte 0 / 1 / 2` (2 = left half of each view only), with the guards of
4.6. Stop rule: any jumping edge.

**Step 9: colour options, gated.** Key safety for the hand's rim; the arm band; `hand_target`. Each only
when the user names the fault it fixes (4.6).

Why this order: steps 1 and 2 are a few lines each and answer the two complaints the user named first.
Step 3 is the base of everything about chair, legs, bottle and shifter, and the first test of the leg
flicker. Steps 4 and 5 are the riskiest piece, so they are split into log and draw. Step 6 is the user's
worry about the real rig and its learner can be seen working before a wheelbase arrives.

## 9. Questions only the user or the headset can answer

1. After arm step 1: which `arm_target_ms` looks right during a wave? Is the strip next to the wrist the
   fault that is left, or the arm's edge as a whole?
2. `edge_cut`: which value looks right? Are there thin things in the play area that must stay (cables, a
   thin shifter rod)?
3. Does a held bottle stay whole for 30 s while looking around, also from low and from the side
   (`mem_roll 1`)? This tells how far the runtime's removal reaches from other head poses; no recording
   answers it.
4. Is the leg flicker still there with `mem_roll 1` and still hands? When exactly does it show?
5. The real rig: which wheel (round, open top, formula), which colours, a display or LEDs on it? What should
   show through the openings: the game also in the lower half, or the legs?
6. The steering angle: is a wheelbase connected to the PC during tests, and where can the angle be read
   (a DirectInput axis, the vendor's SDK, SimHub)?
7. The shifter and handbrake: what kind, how far do they travel, and does the play-area box around them
   have 6-8 cm of room on every side?
8. What does the user wear while racing: long sleeves, short sleeves, gloves? Gloves decide whether the
   hand's colour key works at all; sleeves decide whether step 9's arm band is ever worth it.
9. How far does the head move sideways and forward while driving? More than 12 cm often means the memory is
   anchored again often.
10. Does a screenshot (`screencap`) show the passthrough picture as the eye sees it? Steps 0 and 7 need it.
11. GPU load of the memory pass at 512 x 512 on the headset (step 3).

## 10. Files

- This design: `design/mask-v2/design.md` of the project folder (this file is its copy in the repository).
- The two designs it merges: `design-a-prediction.md`, `design-b-appearance.md` (same folder), with their
  checks in `logs\analysis-20261124-design\design-a-prediction\` and `design-b-appearance\`.
- The lead's checks: `logs\analysis-20261124-design\lead\`: `l1_edge_registration.py`, `l1b_hist.py`,
  `l2_wheel_holes.py` (maps `l2_map_b4/5/6.npz`, pictures `l2_map_b*.png`), `l3_claim_shape.py`,
  `l4_views.py`, `l5_rim.py`, each with a `.txt`. They reuse, without changing them, the readers and truths
  of `analysis-20261124-arm-mask`, `analysis-20261124-bottle`, `analysis-20261124-bursts\wheel-and-tiles`
  and B's `x3_static_edges.py`.
- Nothing under `ALVR` was edited, adb was not run, nothing was committed or uploaded.
