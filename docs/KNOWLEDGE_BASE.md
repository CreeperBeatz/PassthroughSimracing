# Passthrough for sim racing: knowledge base

What was tried on a Quest 3, what works, what doesn't, and why. Status: 2026-11-23, branch
`auto-color-key`; newest findings and next steps (2026-11-24 evening) in "Day of 2026-11-24: bursts, what
the depth can and cannot do". Everything after "Depth passthrough" is experimental and controlled with adb.
**Start with "Where it stands and how to continue"**, the rest is the detailed record.

Words used here:
- **measured**: numbers from the headset logs or the runtime.
- **observed**: judged by eye by the person in the headset, or on screenshots.
- **offline**: tested on screenshots, not live.
- **not tested**: compiles and passes the unit tests only.
- **capture**: a `key_dump` of the headset (camera image, joints, tables, depth), see "Settings".

## Where it stands (2026-11-23) and how to continue

**State.** The headset has the build of 83a11bc7. The code after it only changes two defaults to what
was used on the headset by property: `key_wrist_len` 0 and `key_rim_max` 8. With the build that is on
the headset, set those two again after a reboot (its defaults are 80 and 16). SteamVR home, hands in
view: 72 fps, GPU load 80 % (median), peaks of 95-97 % (measured).

**A session.** Streamer and SteamVR on the PC as in the README (the dashboard doesn't start SteamVR),
Passthrough = Depth (Quest 3), region distance 0.9 m. On the headset a 32x32x32 RGBA LUT at
`/data/local/tmp/alvr_color_key.lut` (`adb push`; a learned one from `key_lut_dump`, or one made with
`tools/color_key_wand.py`), `auto_key 1` before the app starts (it asks for the camera), `color_key` to a
new value other than 0 at any time. Everything else is a default.

**Verdict of the user on that state.**
- Hands: good enough. Parts of the hand are still missing, "but at this point it's not noticeable". No
  more work on filling the hand.
- What bothers now: "elements in view not precisely cut, gripping objects in playable area and still,
  background leakage". In detail: the forearm and things held in the hand (a bottle) should be completely
  visible, but are cut out where the color mask for the fingers is made; the cap and upper part of a
  gripped bottle disappear; what the depth mask shows has jagged edges ("the quality is bad when I hold an
  object next to a VR area"); the room still shows as thin lines and patches next to the hand.
- Decided by the user: `key_inset` / `key_falloff` 4 / 4; `key_confirm` 12 ("the best one so far");
  `key_smooth` 70 leaks; the zone grows with the speed of the wrist only (`key_speed` 2); the skeleton is
  for the hand up to the wrist, no arm extension from it (`key_wrist_len` 0).
- The real rig will have QR codes as anchors, and the play area is set around them, not around the head.

**Evening session of 2026-11-23** (same build, live settings only, the user judging by eye).
- `arm_shift 0` (plain depth for the forearm): first impression "the length of the arm looks like it tracks
  worse", after switching back to 1 "the arm tracking is actually a bit better" without the shift. A small
  difference either way; left at 0 on the headset, the default in the code is still 1. Not settled.
- `key_solid 0`: "no noticeable difference". Stays 6. The 18 % less room predicted from the camera images
  did not show.
- `key_inset 6` against 4, switched every 3 s for a minute: "no noticeable difference". Stays 4. 8 not
  tried. The user: "at this point the fine tuning is so precise, I need a side by side". Fine settings of
  the hand can no longer be judged one after the other: a comparison needs one setting per hand, which
  needs a build.
- The region "Cockpit volume" ran on the headset for the first time: a box typed into the settings
  (placed from the head pose and the desk plane of a capture; the setting is applied live, no restart).
  72 fps as before. The user: "it does stay, the borders are jagged, but it stays consistent depth wise".
  A recenter "only moves the SteamVR page": the box stays. Measured on captures before and after (the
  headset was off for 25 minutes in between, then the recenter): the desk at the same height, a wall at
  the same direction within about 2 degrees and the same place within about 1 cm in stage space. The
  headset is on an automatic stationary boundary and the runtime announced a stage update three times at
  the recenter without moving the frame; when it does redefine it (reboot, lost tracking, a new boundary)
  is not known. That is what an anchor has to cover.
- The jagged border of the box: where a face of the box cuts through a surface. A level cut through
  surfaces at different distances steps on screen even with perfect depth (the eye is not at the height of
  the cut); the roughness along it is the depth map. A box whose faces lie in free air around the rig
  shows no such border. `edge_feathering` 0.08 instead of 0.03 was set live, not judged.
- Horizon OS on the headset: v210.
- What the user wants from the anchors (their words, shortened): when the software starts it recognizes
  which rig it is looking at, and what the borders should be from the position of that marker; an option
  to control with pinch, scale and placing different objects what the rig / play area looks like, "tools
  similar to Virtual Desktop"; QR codes can be printed. "We still need better edge precision on arms and
  wheel".

**Day of 2026-11-24: bursts, what the depth can and cannot do.** Eight recorded bursts (3 s each, build
d98535d2), an independent check of the first three analyses, the lap analysis of the morning, the verdicts
of the user, and the plan for the arm. Labels: *measured* = on the headset or offline on recorded data (the
bursts), *inferred*, *not tested*. Where an analysis and its independent check differ, the check is used
and this is said. What other jobs wrote today is not repeated: see "Play area editor (v1)" (first and
second headset run), the `hand_removal` row and "Where the hole around the hands comes from" above, and
the `marker_carry` / `burst` entries in "Settings".
- **The bursts** (`debug.alvr.burst`, folders in `logs/bursts-20261124`, analyses in
  `logs/analysis-20261124-bursts/`; never committed, they show a private room). 1: head moving; 2: arm
  moving slowly; 3: head still, then pitching down, cardboard wheel at rest; 4: hands still on the wheel,
  head turning 3-6 deg; 5: wheel turned slowly; 6: wheel turned fast (138 deg/s median, camera); 7: hands
  still on the lap, head still (the stillest); 8: a fast wave of the arm (up to 2.6 m/s). The wheel is a
  cardboard stand-in with four holes, octagon 36 x 29 cm (measured from camera images). The user races a
  Moza or Simucube wheel that is not at the test desk.
- **Analyses and checks:** (1) arm, (2) stacking, (3) wheel and fill tiles, (4) the check of 1-3 with its
  own scripts. Files of the check: `logs/analysis-20261124-bursts/check/`.

*Depth cadence and age (measured, offline).*
- Capture times lie on a 33.333 ms grid (largest residual 0.009 ms), steps of 1x or 2x, rarely 3x: 15.2-21.0
  depth frames per second. The runtime makes it so: the depth swapchain index (4 images) steps by exactly 1
  for every frame the app got, so the app drops nothing (a missed frame would be a jump of 2). Bursts 2, 3
  and 7 have no display hitches and still 75-84 % double steps. The mask always used the newest frame that
  had arrived (1645 of 1645 display frames). The mask's depth input changes every 4.4 display frames
  (burst 7).
- Age at display: 124-152 ms (median per burst; the arm table gives p50 129-140, p90 159-172). Display is
  about 49.5 ms after render time in all 8 bursts. `hand_ns` equals `display_ns` in every frame, so
  `key_delay` was 0.
- Is the capture time right? Yes, within about 10-15 ms for the arm. Report 1: forearm test -16 ms (burst
  8), +2 ms (burst 2); the check with its own test: -8 ms (-12..-4) and +4 ms (-4..+16); burst 6 cannot be
  decided by either (the wheel gets into the arm piece). Not resolved: the pose reported with a depth
  frame looks 24-28 ms later than its content (pose stamped later than the image, or a clock offset
  between head and hand); the passthrough latency itself cannot be measured from this data and it decides
  the right target time of an arm correction.

*Stacking static depth does not pay (measured offline; the check wins).*
- Report 2 said that 8-16 frames halve or quarter the edge wobble of the lap in burst 7. Not confirmed:
  its stack was the first k even frames compared with all odd frames, so the curve falls because the time
  spans match. Against the later half of the burst: 1.24 / 1.16 / 1.13 / 1.07 texels at k = 1 / 4 / 8 /
  16 (no gain). In the causal test, the case a live mask meets (a vote of the last k frames against the
  next frame), stacking is worse: burst 7, 33 positions, 0.19 / 0.23 / 0.29 / 0.40 / 0.57 texels at k = 1 /
  2 / 4 / 8 / 16; burst 3 still part, 0.38 / 0.46 / 0.47 at k = 1 / 2 / 4.
- Why: a single frame is already steady (the edge moves 0.25 texel between frames in burst 7, 0.37 in the
  still part of burst 3; consecutive frames differ by a median 0.46 mm and 9.5 % of the texels, range
  3.3-18.3 %, are bit-identical). The runtime depth is already smoothed in time. A causal stack lags real
  change. Report 2 also found: registration after reprojecting with the reported poses is only 0.39 texel
  (burst 7, p90 0.72) and 0.6-0.7 texel elsewhere, sub-texel refinement needs about 1/4; other viewpoints
  disagree by 1.4-4.3 texels and the outline grows from +4 mm to +15-20 mm; 2x and 4x grids land within
  0.04 texel of 1x. A voxel grid of the play area (1.6 M voxels at 1 cm) would cost 19-100 M reads per
  frame, 6-28 times today's mask (inferred). Verdict: do not build a stack, a finer grid or a voxel grid.
- Objects come out about one texel too big: the wheel's upper edge lies 3.5-5.5 mm outside the camera
  outline (+4-5 mm growth, measured; the camera reference itself is good to only 3-5 mm). Eroding the
  in-area decision inward at depth edges is the cheaper fix (idea, not tested).
- The runtime's fill next to the hands would be learned by a stack as room; and a "static" test (within 2 cm
  over 4 frames, 12 cm from the joints) fails exactly at depth edges: 1.2-28 % of such texels at edges
  change in the next frame (burst 7: 1.2 %, burst 8: 28 %).

*The holes of the wheel (measured; the check agrees on the depth, and wins on what is behind).*
- Holes of 6-11 cm (17-30 texels across at 0.52 m) are never in the depth, neither in a frame nor in a
  stack: the depth lies on the cardboard plane over them, with a median dent of +4 to +34 mm, 0 % of samples
  more than 10 cm behind (check: 25 / 34 / 28 / 7 mm for upper left, upper right, lower left, lower
  right). The wheel is 100 % inside the play area, so all four holes show the room.
- The upper holes leak: behind them the play area extends only 11.5 cm, the desk top (0.728 m high) is
  244 mm behind the wheel along those rays and 0 % of it is inside the area, so the room shows where the
  game belongs. The upper right is partly a mug 4 cm behind it that is inside the area. The lower holes are
  fine: the area extends 53-55 cm behind them (under the desk), so the room is right either way.
- Report 3 said the background behind the holes is inside the area (upper left 83-100 % of rays). Wrong:
  its ray march hit the wheel's own smeared depth at exactly 0.05 m. The stacking report and the check hold.
- Only a model of the object (shape from the camera, a measured template or the editor) or the colour key
  can open such holes.
- Outline growth: see the stacking paragraph. Whether the user would notice a 1-3 mm steadier edge is not
  known.

*The arm is a timing problem (measured offline, bursts 8 / 2 / 6; ranking confirmed by the check, its
absolute numbers are 1.5-3 mm higher because its measure is coarser).* Median edge distance between the arm
of an earlier depth frame, moved, and the arm in the later frame, frame pairs about 100-133 ms apart, left
eye, with the real wrist (camera-record joints) as target; report 1 / check:

| variant | burst 8 | burst 2 | burst 6 |
|---|---|---|---|
| plain depth | 29.0 / 34.2 mm | 20.1 / 18.7 | 16.6 |
| translation by the wrist | 15.9 / 17.6 | 7.9 / 10.0 | 8.3 |
| rotation about an elbow 26 cm back, wrist pinned to the target | 7.6 / 9.5 | 4.4 / 7.5 | 7.0 |
| linear weight 1 to 0 over 30 cm | 7.6 / 9.4 | 4.1 / 7.3 | 6.5 |
| rotation, display-time joints as the target | 27.9 / 32.3 | 10.5 / 15.5 | 11.9 |

- Today's weight (smoothstep to 45 cm): 12.4 / 6.1 / 7.6 mm (report 1 only). Overlap in burst 8: 0.56 plain,
  0.80 corrected (check). The measure's floor on still pairs is 3.4-4.5 mm (report 1), the check's reads
  7-10 mm there.
- A pure rotation about a fixed elbow fails when the arm reaches forward (check: burst 2, 44.5 mm when the
  wrist moved 10-20 cm), the linear weight held in both bursts. Where the elbow is (direction and
  distance 26 vs 35 cm) matters little; 20 cm is worse. The elbow is not visible in the depth (the arm is
  seen 25 cm behind the wrist in 1 of 50 frames).
- How far the real wrist moved since the depth the mask used (every display frame, p50/p90/max, mm; report
  1): burst 8 plain 71/126/168, today's shift 54/92/131; burst 2 42/88/136 and 19/45/64; burst 6 36/60/78
  and 16/35/63; burst 5 21/38/54 and 9/18/27; burst 7 (still) 0-1/1/2. The recorded `arm_wrist` equals the
  display-time joint exactly (0.00 mm), so the shift can never be better than those joints.
- **The display-time joints trail the real wrist by 40-76 ms** (confirmed by the check; moving hands:
  burst 1 +44 / +46, burst 2 +40 / +50, burst 5 +48 / +52, burst 6 +54 / +58, burst 8 +74 / +76 ms; burst 8
  right: wrist error 61.8 mm rms with no lag, 31.3 mm at the best lag). They overshoot at reversals: burst 8
  typically +4-5 cm (range -0.2..+6.7, 9 reversals), burst 2 0-1.7 cm, burst 6 0-3.4 cm. By eye (burst 8,
  1.1-1.5 m/s) the camera-record joints sit on the hand, the display joints interpolated to the image time
  are about 10 cm off. Inferred: with `key_delay` 0 the runtime hands back about its newest estimate with
  no forward prediction. This contradicts "The pose at display time is already predicted" under "Hand
  tracking as the truth" in the pitfalls (note added there). Hands at rest (burst 7): step 0.02 / 0.17 /
  0.48 mm (p50 / p90 / max), 2 mm range in 3 s, no jitter.
- What moving the depth cannot fix: the runtime removes the forearm 0.6-2.6 cm behind the wrist (median;
  far fill takes 44-98 % of the samples 0-1 cm behind it, and 19-47 % still show far values at 5-8 cm with
  the hands on the wheel), sleeves and the upper arm (not in the test). No temporal smearing (the
  silhouette is 76 mm wide above 0.5 m/s, 78 mm below 0.15 m/s) and resolution is not the arm's problem
  (the forearm is 19-26 texels wide at 40-50 cm, inferred; outline noise 1-1.5 texels).
- Rests on 2 bursts for the check's numbers, left eye; the ground truth and the prediction share a
  segmentation and a wrist reference. In burst 8 the arm was 0.25-0.3 m from the eye.

*The wheel and the hands (measured offline).*
- Hands predict the wheel's rotation (both hands as one rigid body, in-plane error against keeping the old
  pose; truth is a camera template of the wheel): 124 ms pairs in burst 5, wheel turned 3.5 deg: 1.3 vs 3.6
  deg, rim 12 vs 25 mm; burst 6 consecutive pairs (69 ms, 10 deg): 1.5 vs 10.0 deg, 16 vs 38 mm; burst 6 at
  139 ms (19.9 deg): 3.0 (p90 6.6) vs 19.8 deg, 25 vs 74 mm; held still (burst 4): 0.8 vs 0.5 deg (noise).
  Turn in burst 6 by hands 0 / +98 / -94 / -38 deg against the wheel's 0 / +98 / -91 / -37. The check
  confirms the coarse turn (its hand angle 0 / +92 / -85 / -39 and the text on the wheel by eye) and did not
  reproduce the 1.3-3 deg figures (they need the camera template). Not independently checked in degrees.
- Rotation from the depth alone fails when the wheel is held: plane rms 3.8-4.3 mm is fine, rotation in
  the plane is 0.5 deg at rest without hands, 10.7 deg median (p90 19.6) held still, 18.4 deg in burst 5,
  lost in burst 6 (no holes in the depth, hands and fill cover the top, the octagon is nearly symmetric).
- Arithmetic for a turning wheel (not measured): 1.75 mm per degree at 100 mm, 2.9 mm at 165 mm; one depth
  texel (3.6 mm at 0.5 m) needs the angle to 1.3-2.1 deg; 124 ms of stale depth is about 17 deg at 138 deg/s.
  Hand joints are 50-55 ms old for the wheel too (angle rms between hands and wheel 8.0 -> 2.7-2.9 deg when
  the joints are shifted by +50-60 ms, burst 6); the camera-record joints equal the display joints 50 / 50
  / 70 ms later in bursts 5 / 6 / 8. Report 3 suggests trying `key_delay -50` on the headset (not tried).
- **The fill around the hands** (what the runtime puts where it removed the hands): share of wheel texels
  that show the far surface instead of the wheel, by distance from the hand capsules, bursts 4 / 5 / 6:
  87 / 69 / 73 % at 0-1 cm, 74 / 69 / 61 % at 1-2 cm, 62 / 62 / 47 % at 2-3, 49 / 49 / 33 % at 3-4, 18 / 28
  / 19 % at 4-5, 4 / 12 / 9 % at 5-6 cm, baseline without hands 1-3 %. On the lap (burst 7): 93 % from 0.5
  cm, 71 % at 1-2, 33-43 % at 2-3, 9-16 % at 3-4, 4-5 % at 4-5 cm. Reach: median 2.0-2.3 cm from the
  capsules on the wheel (p90 4.2-5.0, max 11-13), on the lap median 1.5 cm (p90 3.1-3.6). The check
  confirms 0-2 cm (88-92 % / 79-84 % / 68-72 % on the lap, its own profile) but with a per-texel reference
  (burst 5, left eye) finds 67 / 46 / 27 / 13 / 11 % at 0-1 ... 4-5 cm: the fill reaches several cm and
  its exact profile beyond 2 cm is not pinned down (beyond 2 cm it cannot tell the fill from the real
  floor beyond the knee). Value: the far surface behind (0.11-0.13 m behind the wheel, the desk top at
  0.75-0.79 m; on the lap the floor at 1.0-1.7 m, in blocks with smooth gradients), not an interpolation of
  the border. Shape: straight horizontal and vertical edge pieces (median run 8-10 texels, up to 79). No
  per-texel flag marks the fill and it is not on a fixed grid of the 320 x 320 texture (period-8 residue
  14-19 % against 12.5 % uniform; the edge rows still move with head and hands perfectly still).
- **The detector's precision claim is circular.** Report 3: within 6 cm of the hands, depth behind the
  plane and the outline placed by the hands covering the texel: precision 0.94-0.98, recall 0.87-0.92. The
  check: its truth is the outline placed from the camera plus depth more than 4 cm behind the plane; the
  detector is the same test with the outline placed by the hands. Even the row without a hand-distance rule
  scores 0.87-0.95 / 0.94-0.97. So the number measures how well the hands place the outline, not fill
  detection. The real precision needs a truth independent of the outline. Distance alone is weak (4 cm on
  the wheel: precision 0.32-0.38, recall 0.78-0.86).
- Stacking cannot open the holes (stack IoU with the true outline 0.80 at rest; hole cores open 0.21 / 0.54
  / 0.44 / 0.00 only at rest, 0.00-0.01 with the held wheel). Draw test with the later depth as the truth
  (favours plain depth): wheel IoU in the edge band, burst 4 / 5 / 6: plain 0.72 / 0.86 / 0.62, stack moved
  by the hands 0.75 / 0.84 / 0.70, stack at the true pose 0.77 / 0.84 / 0.75; whole wheel: plain wins.
- Rule of thumb for design: use the depth for the object's plane and position (rms about 4 mm), the hands
  (or later the PC's wheel angle) for its rotation, a model for its holes, and treat "within 6 cm of a hand
  and behind the hand" as unknown, not as background.

*The lap in burst 7 (measured offline).* The depth is steady: about 80 texels per eye per frame pair flip in
or out, of 16,400 within 8 cm of the hands (0.5 %); 32-34 blobs of median 1 texel; they sit median 0.3 cm
from the capsules and only 8-13 % within 3 cm of a fingertip. Flip rate per texel per frame pair: inside a
capsule 2.0-2.4 %, 0-1 cm 1.2-1.6 %, 1-3 cm 0.3-0.8 %, beyond 3 cm 0.2-0.3 %, far away 0.05-0.12 %. Depth
change between frames median 0.5 mm, joints step 0.01 mm. Around the fingertips the depth is constantly the
floor fill (inside the area in 8-9 % of the texel-frames, 0.5 % flips): steadily shown as game, not
flickering. Inferred: the flicker the user sees on the legs does not come from the depth or the capsules;
it is not explained yet (other mask inputs cannot be tested on this data).

*The lap analysis of the morning, what the KB did not have (build bd0dc0f7, captures 626 / 627, offline;
the first analysis and its independent check agree on the diagnosis).* The numpy copy of the mask shader
matches the dumped mask (r differs by more than 2/255 in 0 of 191,744 texels, 1 texel in one case). Which
mask term decided the legs shown as game (CUT = real knee skin or shorts drawn as game; texels 0.5-15 cm
from the render-time capsules):

| term | 626 | 627 | old laps 605, 620-623, 625 |
|---|---|---|---|
| CUT / LEAK (first analysis) | 2603 / 52 | 1526 / 229 | 11819 / 23505 (mostly the stale copy) |
| live depth: surface more than 12 cm behind the hand and outside the box (the fill) | 2598 | 1526 | 11754 |
| live depth: surface at the hand's distance, outside the box (box face) | 0 | 0 | 0 |
| live depth: lost by the 3x3 blend or feathering | 0 | 0 | 0 |
| solid fill (focus g) | 0 | 0 | 0 |
| hand zone (focus r) and key rejects | 0 | 0 | 17 |
| fill share of the 0.5-5 cm ring | 56-57 % | 5-6 % | 25-47 % |

- Deciding term: the live-depth fill. The check traced 20 texels by hand with a texel-by-texel copy of the
  shader: all have focus r = 0 and g = 0, the 3x3 surfaces lie 0.6-1.9 m behind the hand. Neither the hand
  zone (rim 0-3 mm) nor the key is asked there (where it could be asked it would accept bare knee skin:
  K >= 0.5 for 42-53 % of leg texels in the first analysis, 86-93 % of skin-coloured texels in the check).
  Knee skin in 626: 1574 of 10495 texels cut (15 %), 1556 within 3 cm of the capsules.
- Correction by the check: the remembered copy does have a surface on the ray (it is 100 % valid), but it
  was taken looking 92 deg away from the current view, so 97-100 % of the cut texels lie outside its view
  (626 left 2164 of 2164). Same result (coverage 0) but for design it means a copy taken at another heading
  can never cover the lap. The copy was 82 / 95 s old.
- Why 626 and 627 differ (two dumps 12 s apart): in 626 the fill covers the ring with the floor at
  1.2-1.6 m beyond the fingertips (knees gone); in 627 the tile holds the lap and knees show. Fits "only
  when behind my hands are my feet and behind them the VR area". Depth images still looked like removal on
  although the log said "off" (see the `hand_removal` row; the runtime ignores false, as the pitfalls say).
- Candidate rules, CUT / LEAK, both eyes summed, first analysis (check in brackets where it re-ran it):

| rule | 626 | 627 | 605 | 622 | 607 (phone at the desk edge) |
|---|---|---|---|---|---|
| today | 2603 / 52 [5165 / 59] | 1526 / 229 [2556 / 231] | 6216 / 510 [5530 / 1581] | 1265 / 5716 [1088 / 9562] | 0 / 1000 |
| live depth only (no copy) | 2607 / 51 | 1542 / 226 | 6221 / 507 | 3422 / 162 | 0 / 987 |
| B' (fill takes the nearest surface at the hand's distance) | 980 / 3500 | 1465 / 229 | 1178 / 1614 | 514 / 6092 | 0 / 1825 |
| D (fill decided by the existing colour key) | 1071 / 57 | 1520 / 229 | 6084 / 536 | 1119 / 5726 | 0 / 1218 |
| G (fill decided by colours learned from the live depth) | 1006 / 57 | 986 / 229 | 2409 / 890 | 1194 / 5735 | 0 / 1492 |
| recommended: live + G + D, R = 8 cm, support >= 0.3 | 931 / 77 [3518 / 127] | 870 / 233 [2003 / 234] | 2332 / 941 [1873 / 2131] | 2669 / 259 [1975 / 517] | 0 / 2167 [LEAK 405 -> 728] |

  Rule A (decide the hand's key zone widened by 1-3 cm) cannot be tested: the depth has no hands in it.
  Rule B literal changes nothing (no cut texel is in a zone). C (memory of other captures) helps but leaks
  old leg positions (legs move more than the tolerance in 12 s).
- **Verdict of the check: no rule to ship.** The gain of the recommended rule on the new laps is about half
  of what was reported (626 CUT -32 % against -64 %, 627 -22 % against -43 %). It makes things worse in
  three cases: (1) the phone at the desk edge (607): a room halo, about 600 texels per eye turn real at a
  median 4 cm from the hand, 94 % of them covered by static surfaces outside the box (median 21-23 cm
  out); the gate opens because support is 0.35 / 0.44, barely above 0.3, and the dismissal "the depth-only
  reference is probably wrong there" is not supported. (In the first analysis rule B' also shows a room
  halo around a raised hand; the support gate keeps the raised hand unchanged.) (2) Dropping the copy makes the old laps worse: CUT rises 1.5-2.8x on 622, 623 and 625. (3)
  The desk view (604, 608-619): support is 0.00 so the rule never acts, but without the copy 1000-1700 texels
  per eye next to resting hands turn from real to game (93-97 % were real only because of the copy). A hand
  on the thin wheel rim probably has support below 0.3 (estimate): nothing would change while driving.
  Keeping the copy ("today + G + D") avoids 2 and 3 but keeps the halo of 1. Any fill rule needs a
  direction-aware gate, not one per hand, and must be re-run on 607. Not tested: putting G into the learned
  key table (one table for every keyed texel) is a different design.
- Not measured: hand on the desk inside the camera view, a held bottle, the wheel (no captures); the
  colour reference is hand-made on the 320 x 240 image (knee/floor edge off by about 2 cm); G is learned and
  scored on the same frame.

*Verdicts and decisions of the user, 2026-11-24 (his words quoted).*
- Edge feathering 8 / 3 / 2 cm: "there isn't a massive difference where it matters - it matters when there
  is an object in the play area, and the zone behind it should be cut off - here the depth mask resolution
  is the problem. The other problem is for the arms - the depth mask moves at 30fps (with my whole length
  arm), but my hands move at 72fps." Left at 0.03, set with `tools/feather.sh`.
- `arm_shift 1` (the existing arm compensation): "no noticable difference at a glance, since the main issue
  is the low quality cutout, and the 30fps".
- The stacking idea: "Since background depth is constant, we could utilize a dlss type approach for the
  depth mask, stacking images, until we have a pretty precise idea of where static objects are". Tested
  offline: does not pay (above).
- The arm as a mask: "For the arm or other objects, why not a mask, like we did with the hand?" and then
  "or actually, if we can interpolate the depth updates to be 72FPS, it might be good enough". Agreed
  order: first only the arm's depth predicted forward every display frame with the wrist (step 1 of the arm
  plan below), the user judges it on the headset, the colour edge only if that is not enough.
- The wheel: the user races a Moza or Simucube wheel that is not at the test desk; the cardboard wheel with
  four holes is the stand-in.
- The remembered copy: "i notice that the depth mask doesn't quickly update for things that aren't actually
  static (like my chair, legs, or the bottle (gear shifter). This might be part of the reason for the leg
  bug, as I reproduced it by moving my bottle 10cm in another direction, and my hand dissaperared from the
  shifter again". Looking at the bottle with the hands out of view repaired it ("it seems like the depth
  mask updated, so it didnt reproduce"); "if I move it now 10cm to the left, it will break". Hypothesis of
  the session (not proven when this was written): the remembered copy is only refreshed when no hand is
  near the depth cameras' view for 1.5 s, so it is frozen while hands are in view. Captures 642 (before the
  move) / 643 (broken) and bursts 12 / 13 in `logs/dumps-20261124-bottle`, `logs/bursts-20261124`;
  analysis in `logs/analysis-20261124-bottle/`. Result of the bottle analysis (measured offline on captures 636-643 and bursts 10-13, with an
  independent check that confirmed the diagnosis; `logs/analysis-20261124-bottle/agent-report-1.md`,
  `agent-report-2.md` = the check, `results.md`, `verify/`). The rebuilt mask equals the captured one
  (the numpy shader copy now has the box layout and the arm motion). The hypothesis holds, with two more
  causes:
  - **The frozen copy.** The copies of 642 and 643 are byte-identical: taken before the move, with the
    bottle at its old place 12 cm away. The old place is drawn as room from the copy (LEAK), the new
    place has no copy surface.
  - **The runtime removes the held bottle with the hand.** In bursts 12 / 13 the live depth keeps only the
    bottom 3.6-5.5 cm of the 19.3 cm bottle while it is held; the cut cap and body texels are the
    runtime's fill (a surface 0.8-1.07 m away where the bottle is at 0.63-0.66 m). So "the cap disappears
    the moment I grab it" is the removal, hidden only while the copy has the bottle at the right place
    (639-642: 2-39 cap texels cut, 643: 348-401). The dark cap is rejected by the colour key, but only
    10-23 % of it lies in the hand zone.
  - **The box edge.** After the move the bottle stands about 2 cm past a side of the small shifter box:
    about 20 % of its silhouette is outside the area even with a perfect copy.
  - Fill reach for surfaces that did not move, across the depth ray from the capsules: 0-1 cm 80-87 %
    filled, 1-2 cm 56-66 %, 2-3 cm 12-44 %, beyond 4 cm about 0.
  - Rules tested offline (CUT / LEAK texels in 643; today 1309 / 2188): a rolling refresh of the copy
    beyond 6 cm from the hands ("A6") does not bring the bottle back (the cap stays cut for every margin:
    the live depth never contains it) but removes stale legs (lap captures: LEAK 2580 -> 977 and 1742 ->
    1241; the check: 1400-3700 texels per eye fixed in 620-625); a refresh at 0 cm destroys the held,
    unmoved bottle. Dropping copy surfaces that the live depth contradicts, and a band at the hand's
    depth, are rejected (they delete the held bottle, or make halos). Carrying what the runtime removed
    at the grab with the hand's rigid motion ("CR") fixes the moved bottle (cap cut 348 -> 9, old-place
    leak 1399 -> 163; the check's own version: 401 -> 24, 789 -> 1; fitted motion 8.9 cm, 17.8 deg,
    residual 0.94 cm).
  - Recommended order, nothing built: (1) the user widens the shifter box by 6-8 cm around every position
    of the shifter (adds a little desk clutter to the area); (2) A6 with the capsules at the depth's
    capture time, and never overwriting a nearer copy surface with a farther live value near a hand on
    the across-ray distance alone (from a lower or sideways head pose 11-27 % of a held bottle's rays lie
    beyond 6 cm: the removal would be written into the copy; not proven, needs a headset test); (3) CR,
    which needs real grab detection and a fresh copy at the grab (with a stale copy its "removed by the
    runtime" test marks thousands of points; a hand going from the lap to the bottle carried a patch of
    thigh along). Tested on ONE move only.

*The arm mask plan (design pass of the day).* The plan and the design will be in the repo as
`docs/arm-mask-build-plan.md` and `docs/arm-mask-design.md` (the main session copies them). Where plan and
offline simulation (`logs/analysis-20261124-arm-mask/`) disagree, the simulation wins.

**Step 1 is built (after 1503fb42), not yet judged on the headset.** `arm_shift 2` (the default stays 1),
`arm_hands`, `arm_target_ms`, `arm_history_ms`, `arm_weight_mm` (Settings), the `arm:` log line, the new
arm fields in bursts and depth dumps, and the learned key table in each burst (`alvr_dump_<n>_key.lut`,
burst format version 2; `read_burst.py` reads old and new bursts). The maths is
`alvr_graphics::arm_target_wrist`; the shader takes the linear weight from the `w` of the third `arms`
vector (no layout change). Unit tests pass (a GPU test: 20 cm along the arm moves by 1/3 of the
displacement, 35 cm not at all). To try: `arm_shift 2` with `arm_hands 1` (left new, right today), a fast
wave; then `arm_target_ms` 20 / 40 / 60.
- **Decision: build smaller. Step 1 now, the keyed zone later, in a reduced form, behind gates.**
- The numbers (offline, wrong pixels as % of the true arm area, bursts 8 / 2 / 6, passthrough latency
  assumed 49.5 ms):

| mask | cut + leak | cut + halo (visible errors) |
|---|---|---|
| today (`arm_shift`, display-time joints, smoothstep to 45 cm) | 58 / 32 / 27 | 51 / 25 / 12 |
| moved depth, past-time wrist, linear 30 cm weight (step 1) | 35 / 20 / 19 | 28 / 14 / 8 |
| + keyed zone 2 cm, skin + learned sleeve table, key only adds outside the moved depth (steps 2 + 3) | 33 / 14 / 17 | 23 / 7 / 4 |
| + keyed zone, key decides a band both ways (the design's core/band) | 32 / 15 / 17 | 25 / 14 / 12 |
| keyed zone with the skin table only | 88 / 100 / 63 | 85 / 100 / 61 |
| reference: depth with no delay | 16 / 18 / 12 | 11 / 11 / 5 |

- Step 1 carries most of the gain: past-time joints plus the linear weight cut the wrong pixels 1.4-1.7x
  and the edge error from 10-31 mm to 4-8 mm (offline). Choosing past-time over display-time joints
  matters more than the key (57 -> 35, 31 -> 20, 26 -> 19). The key adds only 2-5 points of cut + leak
  (the noise between variants is 2-3 points; the truth was built partly from colour, which favours colour
  methods), and can make things worse (b8: halo from 4 to 10 % over a bright wall and dark screens).
- **The user's arm wears a long plaid sleeve in all three bursts** (burst 8: pushed up, 26 % of the arm
  pixels bare skin). The skin table alone is useless for a keyed arm (cut + leak 88 / 100 / 63); a learned
  sleeve table is needed (step 3).
- Dropped from the design: the core/band split, the fitted tube for the depth correction, any layout change,
  the sweep of the zone. Do not build: a temporal stack, a geometric solid fill on the arm, choosing the
  arm's direction by colour, a flood-fill zone, a second keyed layer for sleeves, key-only arm edges.
- Gates before the keyed zone is drawn: G1 after step 1 the user still sees a fault the zone could fix
  (arm cut next to the wrist, or a coarse or late edge); G2 a proxy measurement with no new code adds at
  most 4 points of median GPU load with hands in view and no drops below 72 fps; G3 the arm fitter rejects
  at most 30 % of the frames with the hands on a wheel; G4 new bursts carrying the live colour table still
  show a lower cut + halo for the zone than for step 1 alone. The zone's likely main benefit (the 0.6-2.6
  cm gap next to the wrist, 5-8 cm on a wheel) was never scored offline.
- Properties of step 1 (built, see the note above; `debug.alvr.`): `arm_shift` 0 / 1 / 2 (2 = new; the default stays 1 until
  the user's verdict), `arm_hands` 3 (which arms use mode 2: for a side-by-side check), `arm_target_ms` 40,
  `arm_history_ms` 80, `arm_weight_mm` 300. The wrist is looked up in the past (display - 80 ms) and
  extrapolated by finite difference, at most 6 cm; the weight is linear 1 to 0 over 30 cm. Costs no GPU
  time, one more joint lookup per hand and frame, no uniform layout change. Expected: the edge stays with
  the arm instead of trailing, no 4-5 cm overshoot at reversals, little change when slow; the gap next to
  the wrist stays. Unknown: the passthrough latency (try `arm_target_ms` 20 / 40 / 60 while waving).

*Two facts from the day's sessions.*
- The stream between PC and headset dropped 9 times between 11:02 and 11:38 (4 network stalls of 1-2 s
  while the headset was worn, 4 session pauses), also seen in the streamer's log. The cause of the stalls is
  not investigated.
- The streamer's settings interface hung once after SteamVR had run for 17 hours (118 dead connections on
  port 8082) and needed a SteamVR restart.

*Next steps as of the evening of 2026-11-24.*
1. Build step 1 of the arm plan, with the live switch; the user judges on the headset. (Built, not yet
   installed or judged.)
2. A design pass on the session's top model for the mask around hands, arms and held objects, before the
   keyed zone or the change to the remembered copy is built (the user asked why design work was not given
   to it).
3. The remembered copy and held objects: as the bottle analysis above says (the user widens the shifter
   box; then the rolling refresh away from the hands; then carrying what the hand holds). The leg ghosts
   are expected to go with the rolling refresh.
4. The wheel model turned by the steering angle: on hold until a wheelbase is connected.
5. The play area editor is finished. The user after trying build fbe68b1b (two-hand add gesture, the
   room through the game while editing, select-only first pinch, hand ray): "ok space builder is perfect
   now, nothign to iterate. Next improvements would be outside of the area definition and always on how
   we display things inside it." By the user's eye the room now shows at 25 % and the game is dimmed by
   10 % while editing ("good enough"; the defaults since the commit after fbe68b1b). After an install the
   client has to be started by hand; the saved layout came back without scanning the code.
6. The user's leg flicker with still hands is not explained: it needs a recording of the drawn picture
   frame by frame.

**Night of 2026-11-23: which way the mask is wrong, measured on 26 captures.** Offline, on captures taken
the same evening with the box placed from a QR code active (build 34a0ea96). An analysis, then an
independent re-measurement; where they differ, the numbers of the re-measurement are used here.
- The principle and the question of the user: "treat the depth map as a ground truth, that is delayed.
  Think about it as GPS measurements. they come back delayed, but are ground truth. on GPS, accelerometer
  data is insantaneous. Here, chroma key is instantaneous." And: "you can validate via logs - am I
  removing something that should be VR or am I displaying something that should be real life as VR".
- Observed by the user with the box active: hands do not disappear outside the play area, arms do. With
  the hands on the lap the legs are cut out; the idea: "the chroma keyer should not work on areas that
  were consistently rendered in-zone". The edges of objects in the zone (desk, its underside, the arm)
  are rough and "around them, the area should appear as outside the playzone".
- The captures: `logs/dumps-20261123-box`, n = 600..625, on the development PC, never committed. Groups,
  sorted by looking at contact sheets (the re-measurement agrees):
  - 600-603: headset not worn, lying on a desk outside the box. Not used.
  - 604 and 608-619 ("desk view"): seated, looking at the monitor and the QR code. The hands are at the
    bottom edge of the eye view (hand texels present), outside the camera image.
  - 606 and 607 ("arm"): 606 right hand raised (joints up to 1.24 m/s); 607 a phone in the right hand at
    the desk edge, sleeve across the view. In 606 the right hand straddles a side face of the box: 5 of 56
    capsule points are outside, by at most 9.1 mm, the capsule surface reaches 15.8 mm outside.
  - 605, 620-623, 625 ("lap"): looking down, both hands on the yellow trousers. 624: lap, hands not
    tracked.
  - Screenshots are 0.4-1.7 s before the "depth dump n" line. The camera pass used for the key colour lies
    from 512 ms before to 16 ms after display (622).
- Measured, geometry: a numpy copy of the mask shader (`bx.py`) reproduces the dumped mask r, b and a
  channels of all 26 captures x 2 eyes (at most 1e-5 of texels differ by more than 2/255; a second copy
  written from the shader: at most 5.2e-6). With the depth rows flipped 45-82 % of the texels differ. The
  box is identical in all dumps. Static desk cells (5x5 cm, seen by 10 or more captures, 615 values):
  deviation from the cell median 3.2 mm (median), 11.4 mm (90 %); the per-capture offsets span 14.1 mm.
  Between captures, inliers within 5 cm: median residual 5.3 mm (609 to 619) to 19.3 mm (604 to 625), and
  some pairs carry a constant offset of +11 to +13 mm (604 to 609, 604 to 625). Left eye against right
  eye 5.5-7.3 mm.
- Measured, age of the depth at display (26 captures): median 123.8 ms, range 89.3-183.2 ms, IQR
  112.1-138.1. At render 73.2 ms (47.6-133.0), render to display 50.2 ms (30.4-51.1). Hand speed per
  capture: median 0.05 m/s, largest joint 1.5 m/s. Speed times age (arithmetic, not a measurement): a hand
  at 0.05 m/s moves 6 mm in 124 ms, at 0.5 m/s 6 cm, at 1 m/s 12 cm.
- Measured, the hand is not in the depth image (left eye, render-time capsules). Of the depth texels
  whose ray passes inside a capsule, the depth lies on the hand (within 3 cm) in 3 % (605), 0 % (606),
  11 % (607), 1 % (620), 10 % (621), 2 % (622), 0.1 % (623) and 0.1 % (625); in 65-100 % it lies more
  than 12 cm behind it. Around a resting hand on the lap the runtime also removes the thigh. Trousers
  texels (by camera colour) with no depth, or depth more than 12 cm behind the hand, pooled over 605,
  620-623 and 625: 88, 87, 76, 58, 33 and 19 % at 0-1, 1-2, 2-3, 3-4, 4-5 and 5-6 cm from the capsule
  surface. That is an average over uneven captures: 621 has 17 % at 0-2 cm, 622 and 623 have 93-98 % at
  2-4 cm and 47-71 % at 5-8 cm; with a colour class that includes shadowed trousers the profile is 90, 88,
  81, 72, 55, 38 %. The depth image shows a blocky hole filled with far values (the floor under the desk
  at 1.25-1.37 m) that reaches 8-15 cm on the side towards the knees in some captures. The first reading
  "3-5 cm, the thigh slopes away beyond" is wrong: the 33-36 % beyond 6 cm is that fill.
- Measured, direction of the errors by region (left eye, 604-625; "wrong" means against the current depth
  classified hard against the box; the colour of the key from the 320x240 camera image):
  - Edges of the box region, away from the hands: 66,825 of 443,637 edge texels (15.1 %) are shown as
    real that the depth puts outside. Of them 53,542 (80.1 %) come from the feathering of 0.08 m and
    13,283 (19.9 %) from the 3x3 blend of the depth mask (it grows in-box objects by one texel). Feathering
    leaks lie median 1.3 cm outside the box (90 %: 2.6 cm). For a flat surface the shader gives a mask of
    1.0 up to about 1.9 cm outside a face, 0.66 at 3 cm, 0.43 at 3.6 cm and 0 from 5.8 cm; with the key at
    0 the shown share reaches about 2.9 cm. Surfaces leaked by the blend lie median 1.3 cm outside, 90 %
    21.7 cm (depth steps; analysis only). Farther from edges, 23,198 of 2,137,056 texels (1.09 %) are
    shown real beyond the box, all from the feathering. Inside the box nothing is cut. The other way (game
    where the depth puts it inside), 3,038 texels (0.7 %), only where the depth has no data (analysis only).
  - Disc around the hands where the remembered copy of the play zone is used, more than 6 cm from the
    capsules: on the lap (605 and 620-625) 18,627 texels are shown real that the current depth puts
    outside. Without 605 (492 texels, none from the copy): 18,135 (15-24 % of the disc with depth, per
    capture), 87 % from the copy (mask channel a). There the current surface is the floor under the desk
    at 1.25-1.37 m, 9-22 cm outside the box; the copy's surface on the same ray is 54-63 cm nearer. The
    camera independently sees trousers at those texels in only 2.8-13.6 %, so the current depth is right
    and the copy is stale. The copy used for 620-625 was taken at 22:11:41.6 while the hands were not
    tracked, 5-90 s before. On the desk view (608-619) the copy adds coverage in 17-38 % of the disc, its
    surface 1.6-2.5 cm nearer than the current one; where only the copy makes a texel shown (734-2,014 per
    capture) the current surface is 4.3-6.0 cm outside the box. For 605-607 the copy (22:06:47, taken
    looking at the monitor) covers none of the disc. Its registration (94-96 % of the static parts within
    5 cm of the current depth, +11 to +15 mm; its lap only 16-43 %) was not re-measured.
  - Lap, trousers near the hands (colour class including shadowed trousers): in 605, with no copy over
    the lap, 43.9 % (left eye) and 48.0 % (right) of the trousers texels are shown as game (36-48 % over
    the colour classes tried); they lie median 1.7 cm (90 %: 3.5 cm) from the capsules and every one more
    than 5 mm from them is outside the key zone, with the depth removed. In 620-625 with a copy over the
    lap: 4,145 of 43,757 (9.5 %), per capture and eye from 0.4 % (620 left) to 34.8 % (623 right); in the
    1-6 cm ring 622 and 623 still lose 15-44 % of the trousers, although the copy covers 40-76 % of the
    ring trousers in 620-625. 624 (no hands): 707 of 14,305 (4.9 %), where the shins leave the box at its
    bottom face; that is the box, not an error (analysis only).
  - The key keeps almost none of the trousers colour: 0.1-1.6 % of trousers texels have K of 0.5 or more.
    K is from the camera image, not from the compositor's own image, so this is a proxy.
  - The hand itself: 441 of 34,427 known hand texels (1.3 %) are shown as game, 346 of them in 606 (26-32 %
    of that hand, raised and fast; low confidence, K from an image 387 ms old). Of 605 and 620-625 together,
    95 texels (analysis only). Hands shown as real that should be game: 0, because no hand is outside
    the box.
  - Counting all texels, about 143,000 are shown real that the depth puts outside against about 9,900
    shown game that it puts inside (14:1). That is a count of mask texels under the truth choices above,
    not of area, and its edge part is the soft edge against a hard box on the same depth.
- Measured on the same captures, not re-measured: current depth and the static map of the other captures
  agree on in/out for 99.7-100 % of interior texels but only 84-93 % of texels within 6 mask texels of an
  edge. Inside the key zone only 5 % of the texels have usable depth; where it is usable it never says
  "outside" where the key keeps a colour (0 of 1,780 texels) and says "inside" where the key drops the
  colour in 747. Thighs inside the box: 85-95 % of the trousers texels in 624 (including shadowed parts);
  97.9-100 % holds only for the brightly lit parts. In the lap captures with hands 37-88 % (the rest is
  the far fill near the hands and the shins and knees below and in front of the box). So places that are
  consistently inside exist and can be measured, but the edges of objects move from capture to capture.
- What cannot be decided from this data: a hand outside the box (none), a forearm crossing a face (none;
  606 is a hand at a face), the error of the depth's own edge against the real edge (no camera edges as
  truth), the band from 5 mm inside to 1 cm outside the capsules, the delay of a moving arm on single
  frames. Method caveats: the key value comes from a 320x240 camera image 12-512 ms from display; the edge
  "errors" are measured against a hard box on the same depth, so the feathering share is an error by
  construction; the 14:1 ratio counts texels.
- What follows (plan, not result):
  - (a) The feathering was set to 0.08 live on 2026-11-23 and never judged. Compare 0.02-0.03 on the
    headset first. It is a live setting.
  - (b) The whole-frame remembered copy is not a static map: body parts enter it and it goes stale.
    Replace it by a memory in the box frame of what is consistently inside the play area, updated only
    away from the tracked hands and arms, and use it where the hands remove the depth. An offline
    prototype on these captures is in progress (`logs/analysis-20261123-edges/memory`).
  - (c) The late depth cannot correct the key on the hand itself (it is not in it), only from about 4-5
    cm away from the hand surface outward.

**The memory in the box frame, offline (same night, one analysis, not re-checked): it does not fix the
lap.** Simulated on the 26 captures (`logs/analysis-20261123-edges/memory/results.md`): a 2 cm voxel grid
in the box frame, occupied and free evidence from the depth, no updates within 15 cm of the hands, a
place counts after two captures, used in the disc around the hands. Test set 605 / 622 / 623 / 625, both
eyes, 31,731 trousers texels near the hands: cut today 26.8 % (with the copy's leak of 30.9 % of the
should-be-game texels of the disc), live depth alone 45.5 % cut and 3.1 % leak, the memory as designed
45.5 % and 3.1 %: it changes nothing, because it never holds the lap. Only one capture (624) sees the lap
without hands, the legs move between captures (the best other capture has 18-56 % of a capture's thigh
voxels), and the hands rest at the same place, so the thigh near them is never updated. Allowing a single
capture to count gives 20.4 % cut and 5.8 % leak when 624 is learned beforehand, and about 40 % cut run in
time order. While driving the hands stay on the wheel: what is under and next to them is never seen
without them, so no memory can learn it. At static edges two memories built from different captures agree
on 0.895 of the edge texels, two single frames on 0.772. Conclusion: a memory can steady static edges; for
what is next to the hands it is the wrong tool.

**Where the hole around the hands comes from (code, not a measurement).** The runtime does not remove the
hands by itself: the client asks for it (`set_hand_removal`, XR_META_environment_depth) whenever the color
key is on, because the depth shows the hands where they were 124 ms ago. The uneven 3-15 cm zone that
goes with them (thigh, a held bottle, later the wheel rim under the hands) is the price of that choice.
In the user's picture (the depth is the late truth, the key the instant signal) the other way round is
worth a test: leave the hands in the depth map, so that a resting hand and everything next to it is
simply shown by the depth (a resting hand moves 6 mm in 124 ms), and let the key and the hand tracking
only correct a moving hand. `debug.alvr.hand_removal 0` was meant to do the first half live: expected
are legs and held things back next to the hands, and a hand that moves fast shown twice or with the room
where it was 124 ms ago. Tested on 2026-11-24 with `hand_removal 0`: the runtime still removed tracked
hands (captures 626 / 627, see the `hand_removal` row), so this switch cannot test the depth-first idea. If the first half holds on the headset, the second half is: depth texels that
lie on the hand's capsules as they were at the capture time of the depth are decided by the hand logic at
the display time, everything else by the depth; that takes the hand out tightly (about 1 cm) instead of
by 3-15 cm.

**Open for the user.** Way A or way B first (below)? Does "no arm extension" also mean `arm_shift 0` (the
depth is shifted along a 45 cm arm from the wrist) and dropping the 50 cm forearm that the learner
excludes from its samples? How many QR codes, and the size to keep after the first measurement? When can
captures be taken with a real wheel?

**Answered by the user, 2026-11-23 (evening).**
- Motion rigs are out of scope: a code never moves once placed.
- The codes are mounted on the rig (flat or standing) or shown on the monitor, facing the driver (a
  standing code), as the anchor. Where they sit still has to be in the camera's view when looking ahead
  (wheel deck or monitor stand, not the base behind the legs).
- Rule: a code is used as it is and measured precisely, and that measurement is the baseline. Its size
  can be changed after the first accurate measurement.

**What the day established** (from captures unless noted; points 1-4 and 6 were re-measured by a second,
independent analysis, which changed several first results; 5 is from the logs, 7 was measured with the
user, 8 on 2 captures).
1. *Room next to the hand.* 11-13 % of the hand's area in the camera image, mostly a fringe within 12 mm
   of the hand's edge. About 45 % of it (36-57 % depending on the hand mask and on the offset of point 2)
   is shown by the solid fill, which shows any color: the round ends of the always solid wrist capsules
   past the wrist and the heel of the palm, and confirmed capsules that sit beside the hand. The wrist
   bridge gave an estimated 16-23 % (its direction is not in the captures and was reconstructed; off now),
   the growth with the speed at least 17 %. Part of the "thin lines" may not be the key at all: in one
   screenshot a fringe runs along the sleeve 17-22 cm from the wrist, beyond any key zone, so most likely
   the coarse edge of the depth mask.
2. *The projected skeleton sits beside the hand in the camera image*, by 5-6 px (about 10-12 mm) in the
   first 27 bursts, as if the hand were 1.5-2.7 cm nearer than it is (the same as a 4.5-6.5 % scale about
   the image center, depending on the target; hands alone can't tell the two apart). Probably not a
   camera error: the crop arithmetic of `camera.rs` reproduces the logged intrinsics, no other crop or
   binning assumption predicts the fit, and one later set of captures (same boot, same calibration) shows
   no offset at all. Not excluded: the lens distortion and the active array are never read or logged. A
   time skew doesn't explain it (0-13 ms). Most likely the hand tracking places the hand too near,
   depending on pose or session. A fixed correction makes the set without offset worse (1.1 -> 2.0 px),
   so none is built. Where the offset is present, a replay with the projection moved by 2.7 cm raises
   the skin samples that land on the hand from 0.75 to 0.83 and the true segments voted in from 0.77 to
   0.90 (offline, tables learned with the old projection). How much of the 0.8 of its own skin samples
   that the key keeps on the headset this explains is open: that number also rises where there is no
   offset.
3. *A background model with places* (the user's idea: remember how each place of the background looks
   and compare) is not what the code does: the color table has no place, the leak map only rates leaks.
   Tested on 19 captures: as a gate on the key (drop a kept pixel that equals the remembered background
   at its place) it removes 35-37 % of the key's room leak for 1.5 % of the hand, 24 % without any depth;
   per capture 4-78 %, and skin over a beige desk lost 16 % of the hand. As the mask itself it fails on
   the near desk (wrongly marked background 13 -> 34 % without a depth fitted to the image). It finds
   what the key can't: sleeve 70 % (key 1 %), watch 70 % (10 %). A held bottle: one bottle, 3 captures,
   held where it usually stands, so nothing is known (captures of a bottle held in the air exist, see
   way A). The same flat room point varies by 2-3 levels (standard deviation; a tenth by 10 or more, and
   a quarter of the sightings fall into a neighbouring table cell) after a gain per frame; without the
   gain the exposure changes it by up to 48 %. None of this is built.
4. *A shape detector for the hand* cannot be exact: one camera pixel is 2 mm at the hand, a camera result
   is 45-160 ms old when used, and the camera outline inherits the offset of point 2. Measured in the
   camera image (probably optimistic: the outline and the hand mask it is scored against share their
   seeds): an outline that only limits the solid fill removes 21-28 % of the room for 0.6-1 % of the hand;
   flags per capsule side ("the camera sees an edge here") 19.5 % for 0.9 %, without a texture; a full
   outline that cuts everything 30-52 % for 3-6 %. What it cuts of a moving hand is not known (3 passes
   per speed range; 8-16 % above 0.25 m/s in those).
5. *The remembered playzone* can't help a carried object (it knows where things stood). With the new
   trigger it was copied whenever no hand was near the depth view for 1.5 s: 39 times in 20 minutes. But
   not reliably: in two streams the runtime kept the hands near the view for minutes, also during a 10 s
   sweep with the hands at the user's sides, and the copy was 150 s old when the bottle was gripped.
   While driving the hands never leave, so it is not refreshed then. Only useful for a wheel or shifter.
6. *The ring of background colors around the hand* is empty or under 50 colors in about 12 % of the log
   lines with a learned hand: the half-space that excludes the forearm removes everything when the arm
   direction is close to the line of sight (computed: empty up to 20-30 degrees). In 4 captures a bridge
   direction turned towards the camera caused it, but in the logs it is as frequent without a bent
   bridge. When no ring has colors (or a hand with a fast finger has fewer than 50 of its own) the
   reliability is 0 and the solid fill is off (mean weight 0.07 against 0.56); an empty ring of one hand
   is covered by the other hand's. No effect on the leak could be shown.
7. *The camera sees hands at the wheel position* (looking ahead): about 90 % of the voted segments are
   confirmed. A reach to the shifter leaves the image.
8. *The depth capture works on the headset* (GLES readback, 48 of 48), and the region really is a sphere
   around the eye (2 captures: the mask follows the radial distance in 99.8 % of the texels where sphere
   and plane differ).

**Added in the evening** (code reading, 8 captures with depth measured once with hand-drawn outlines, and
a search of public sources; not yet re-measured by a second analysis, so read the numbers as estimates).
9. *The color key never removes anything.* In the hand's zone a pixel is real if the depth mask says so,
   or the solid fill covers it, or the key keeps its color (`stream.wgsl`, the `max` of the three). The
   forearm and a held object are cut because the depth mask is already empty there, not because of the
   key: the user's "cut out where the color mask for the fingers is made" is the place, not the cause.
10. *The runtime removes more than the hand from the depth map.* Around the fingers about 3-5 cm of a held
    bottle are gone too (7 captures: no depth at the bottle's distance within 3 cm of the hand shape, most
    of it back from 5-8 cm), replaced by far values (median 0.8-1.5 m). The forearm is hardly touched: 1-2
    cm at the wrist. Of the bottle pixels that are not shown, 57-95 % lie outside the hand's key zone in 6
    of 7 captures: no setting of the key can show them.
11. *Continuing the object from the depth that remains* is an extrapolation: the bottle remains on one
    side of the hand only. A straight continuation in the camera image covered 24-68 % of the missing part
    in 5 captures (with 1-39 % of it beside the bottle) and failed in 2. A wheel rim passes through the
    hand on both sides and may behave differently: no captures.
12. *Depth against the camera image.* For a bottle held in the air the depth region reaches 2-3 camera px
    (4-6 mm) past the camera outline, 4-5 px at the worst tenth. Static depth edges fit the camera image
    best after a scale of about 5 % about the image centre, also in the set where the skeleton shows no
    offset: a hint, and no more, that part of point 2 is the camera model (the lens distortion is never
    read). Moving the border to camera edges means moving it by about as much as this is uncertain.
13. *No shortcut from the platform.* No Quest feature gives a native app a mask of the body, the arms or
    held objects; other cockpit tools show fixed windows or boxes placed by hand or with a controller,
    none shows arms or held objects cut out of the camera image. One report says the hands return to the
    depth map when hand tracking is turned off in the system settings (not tried here; the key needs the
    joints).
14. *For anchors:* the `openxr` 0.21.1 / `openxr-sys` 0.13.1 crates in use already contain
    `XR_EXT_spatial_entity`, `XR_EXT_spatial_marker_tracking` (QR, Micro QR, ArUco, AprilTag),
    `XR_EXT_spatial_anchor`, `XR_EXT_spatial_persistence` and `XR_EXT_future`: nothing to declare by hand.
    Meta's documents name `com.oculus.permission.USE_ANCHOR_API`, the log of the headset asks for
    `horizonos.permission.USE_ANCHOR_API`: declare both. QR tracking through the standard extension is
    reported working from OS v83 by one forum post; Meta describes QR poses as slow to update and not for
    moving things; nobody has published accuracy numbers. A published Quest 3 app built on ALVR keeps its
    passthrough shapes relative to one saved spatial anchor (`XR_FB_spatial_entity` with storage and query,
    the `com.oculus` permission) and lets the user place it by hand: saved anchors work for a native app of
    this kind, QR codes would only replace the manual realignment after the seat moves or the room setup is
    redone. The box of the region takes any pose in the shader; the settings only offer a yaw, and they come
    from the PC, so a calibration on the headset needs its own path.

    *Web search of 2026-11-23 on the QR route* (reported = forum, blog or sample code; documented = Meta or
    Khronos pages; nothing of it checked on the headset).
    - Documented (the spec, with Meta as author): `XR_EXT_spatial_marker_tracking` is ratified (OpenXR
      1.1.49) and needs `XR_EXT_spatial_entity` and `XR_EXT_future`. Reported by a Meta engineer in Meta's
      sample repository: both extensions are advertised on v205 and QR works from v83; a Godot post says
      v79 answered "QR not supported". v210 should list them, `capabilities ...` in the log says for sure.
    - Only QR codes are expected on Quest: Unity's package lists "QR Codes only" for Horizon OS, and the
      Godot post saw no ArUco or AprilTag. Documented: versions up to 10, no logo, no Micro QR; the payload
      is text or bytes; codes "relatively large and close", best in good light.
    - Documented: the spatial data permission is a runtime permission (`com.oculus.permission.USE_SCENE`,
      the user is asked once). Reported (Godot issue): a context created before the user accepted failed
      with `XR_ERROR_PERMISSION_INSUFFICIENT` and nothing worked until the app was restarted, so the
      context must be created after the grant. `USE_ANCHOR_API` is documented as a manifest entry; no
      Meta page names the `horizonos.` variant.
    - Documented: poses update at a lower rate than the frame rate, "not frame-perfect", not for moving
      codes; the pose of a moved code does update in the end. Reported (Meta forum, 2026-11, no reply):
      severe position jitter on a still printed code on a table. No numbers for size, distance, rate or
      accuracy are published; start from 10 cm and measure.
    - Documented: Meta's own QR sample suggests showing a code on a computer screen. Nothing is published
      about glare, flicker or light-on-dark codes on Quest; the general knowledge (moire between the pixel
      grid and the sensor, banding of a rolling shutter) says to use whole pixels per module, dark on
      light, a white margin of 4 modules, full brightness.
    - Documented (the spec): the options "static markers" (more accurate pose and size) and "fixed size
      for every code" exist only if the runtime lists their features. No source says whether Meta's does:
      the `capabilities ...` line answers it.
    - Documented (the spec): the pose is the centre of the code, +X to the right looking at its face, +Y to
      the top, +Z out of the front; the extents are the full width and height. QR codes have no id, only
      the payload.

**Method: what cost time today.**
- Offline tests on screenshots of the display with a skeleton placed by hand were wrong twice (no leak
  predicted for `key_smooth` 70; gains of a shape detector against a baseline the headset doesn't have).
  Test on captures, and let a second analysis re-measure the numbers a decision rests on: it found a
  screenshot matched to the wrong capture, an improvement that came from pooling two different sets, and
  fits that only held in-sample.
- Say which pixels: 4 px of a screenshot are 1-2 mm, one camera pixel is 2 mm at the hand.
- Captures: `adb logcat -s ALVR_CAM` running, `setprop debug.alvr.key_dump_burst 2`, then for each moment
  `setprop debug.alvr.key_dump <a new n>`; wait for `dump <n>/..` and `depth dump <n>: 9 files`, then
  `adb pull` the files folder of the app. Pair screenshots with captures by the time of those log lines,
  not by what the images show.
- Ask the user what they see before building for a number: the log said 21-30 % of the skin was dropped
  while the user no longer noticed missing parts, and wanted the leaks and the objects fixed.
- A live setting first, then a build: `key_inset`, `key_solid`, `key_confirm`, `key_smooth`,
  `key_wrist_len` can each be judged within a minute.

**Data and tools** (outside the repository, on the development PC: the captures show a private room and
are never committed).
- Captures of one room: 31 moments with the camera image and joints (poses, a cluttered desk, wheel
  position), 48 more with depth, mask and focus area in three sets: 2 of a gripped bottle; 21 taken while
  the compositor of SteamVR was dead (no game on the display, camera and depth are fine; the skeleton
  has no offset in this set); 25 with a sweep without hands, a bottle held in the air and gripped on the
  desk, and a sleeve (offset about 5 %). Screenshots of the display from the same minutes.
- A viewer that draws the capsules and the key's decision into the camera image, and the scripts and
  results of the analyses above (leak, ring, background model, camera model), each with its re-check.
- `logs/analysis-20261123-edges` (scripts b1-b7, `check/` for the re-measurement, `views/` and `memory/`)
  and the 26 captures in `logs/dumps-20261123-box`. `bx.py` reproduces the mask shader in numpy and was
  verified against all dumped masks.

**To continue, way A: iterate on what is there.** In the order of what it should bring:
1. Live, no build: is the skeleton tail gone with `key_wrist_len` 0; `arm_shift 0` if the arm still looks
   wrong (it shifts the depth along a 45 cm arm from the wrist); `key_solid 0` (predicted: 18 % less room
   for 1.25 % of the hand) and `key_inset` 6-8 (11-20 % for 0.65-1.2 %). Predictions from the camera
   images: only the user's eyes decide. Done in the evening session (above): `key_solid 0` and
   `key_inset 6` make no visible difference, `arm_shift 0` a small and unsettled one. The live settings of
   the hand are used up.
2. The background gate of point 3. First measure on the captures with depth: the registration with the
   depth of the sensor instead of a fitted one, the gate applied one pass late with the head pose of the
   display, a lamp change, and a session in the driving seat: behind hands that stay in place the
   background is never seen (about 16 cm of head motion is needed at 0.45 m), so the model needs a
   moment with the hands away, and hands that stay may end up in it; the turning wheel and the legs are
   not static. In the code the leak map already keeps colors per direction; it has no depth and moves
   with the head.
3. Edges of objects (the user's idea): move the border of the depth mask to the edges that the camera
   sees. Objects move little, so the late camera image doesn't matter if the result is kept in the room.
   Prototype on the captures with depth before any GPU work. A camera image of 320x240 is coarse (2 mm at
   0.45 m, about 5 mm at 1 m): the camera offers up to 1728x1728.
4. Held objects and the forearm near the hand: nothing works yet. Candidates: the difference from the
   remembered background on and near the forearm (sleeve 1 -> 45 %, but 21 % of the background near the
   forearm wrongly marked, mostly desk), and continuing an object from the part of it that the depth map
   still shows into the area the runtime removed. Captures of a bottle held in the air exist with depth
   and are not analysed yet: start there. Still missing: a wheel being turned with the hands on it.
5. Edge flags per capsule side for the solid fill (point 4), and capsule ends that don't stick out past
   the wrist.
6. The ring (point 6): the forearm as a wedge of capsules instead of a half-space, and keeping the last
   reliability for 0.5 s. On the captures no ring stays under 50 colors, but what it lets in of the arm
   is not known (0.7-25 % of the added pixels, depending on how the arm is outlined), and in a grip pose
   it samples the other, untracked fist as background. Low priority: no effect on the leak was shown.
7. An offset of the hands along the view for the learner only (point 2): move the joints and capsules,
   not the camera (the leak map, the scene samples and the captures use it). 0 by default, first as a
   property for tests (18-27 mm fits the first captures, 0 the set without offset), later chosen for
   still hands, smoothed over many passes, from image edges, within 0-30 mm. Check that segments on the
   hand and beside it still vote differently: that got worse in both later sets.

**To continue, way B: anchors on the rig and a mapped play area.** This turns the play area from a bubble
around the head into a box fixed to the rig, and most of way A gets easier in the rig's frame: the rig
and what is behind it can be learned once and stay valid, and only hands, arms, the turning wheel, the
shifter and held things have to be decided live.
1. What exists: the region "Cockpit volume" (an oriented box in stage space: center, yaw, size) in the
   settings and in the shader (`region_mask`, `world_to_volume`). Never calibrated, never used on the
   headset; entering stage coordinates by hand is not practical.
2. The anchors. The runtime lists `XR_EXT_spatial_marker_tracking` for this app (client log of
   2026-11-22, the list of available extensions). It hides `XR_EXT_spatial_entity`, `XR_EXT_spatial_anchor`,
   `XR_FB_spatial_entity` and the persistence extensions, because the manifest lacks
   `horizonos.permission.USE_ANCHOR_API` (`skipping extension ... missing uses-permission` in logcat).
   First step: add the permission, enable these extensions, and log whether QR codes are a supported
   marker type, how often and how exactly they are found, from which distance and angle. Not known: whether
   it works at all for a native app; Meta documents QR tracking for its own SDKs, as experimental and not
   for moving objects. Otherwise: find the codes in the passthrough camera image (it must be larger than
   320x240 for that; 22 ms late is fine for something that doesn't move), or touch points of the rig with
   a controller once and keep them as a spatial anchor. A world-anchored box breaks on a motion rig; the
   user ruled motion rigs out (2026-11-23, evening), so a code that never moves once placed is the case to
   build for. The codes sit on the rig (flat or standing) or are shown on the monitor facing the driver.
   A code is used as it is, measured precisely, and that is the baseline; its size can change after the
   first measurement.
3. From the anchors to the box: its pose every time the anchors are seen, its size once (settings), then
   `world_to_volume` as now. Test: the border stays on the rig when the head moves and after a recenter.
4. The map in the rig's frame, built while the hands are away (the user accepts a short moment for that):
   depth of the rig at a finer grid than one depth image (many images from a moving head), its edges
   from the camera, and the colors of what is behind it as seen from the seat. This is the background
   model of point 3 and the edge idea of way A with a fixed frame. The wheel turns and the legs move:
   they are not part of a static map.
5. The camera coverage (point 7) and the offset of the tracked hands (point 2) stay as they are.

## Short version

| Goal | Best approach so far | State |
|---|---|---|
| Desk, rig, wheel as real objects | Environment depth, "closer than X" region | Works, coarse edges |
| Hands | Live color key on the passthrough, in a small zone around the tracked hand, with automatically learned colors | Good enough for the user (2026-11-23): what is still missing is "not noticeable" |
| Holes in a still hand | Solid fill of the hand segments that the camera confirms, checked with a wider table (`key_confirm`) | "The best one so far" |
| Room next to the hand | Small zone, strict colors | Still shows as thin lines and patches; roughly 40-50 % of it (camera image) comes from the solid fill, not from the key |
| Forearms | Depth map only, shifted by the motion of the wrist (`arm_shift`) | Coarse, jagged edge, lags or jitters; cut out near the hand. Removed: the body tracking forearm (not accurate enough) and the keyed bridge from the wrist (a "skeleton tail") |
| Held objects | Remember the depth map without hands | Doesn't help: the upper part of a gripped bottle disappears. A memory can't follow a carried object and is only refreshed while the hands are away. Open |
| Edges of objects | Depth mask | Jagged ("the quality is bad"). Idea: move the border to the edges the camera sees. Open |
| GPU budget | - | 72 fps, 80 % load with hands in view, peaks of 95-97 % |

## How it works now

1. **Depth mask.** For each pixel find the real surface with `XR_META_environment_depth` and decide if
   it belongs to the region (distance or box). Computed at low resolution, upscaled. The arms come only
   from here: around each arm the depth is shifted by the motion of the wrist since the depth image was
   captured (`arm_shift`).
2. **Hand zones.** Capsules from the hand tracking joints, up to the wrist, no forearm. They say *where to
   look*.
3. **Color key.** The compositor applies a RGBA color table (LUT) to a second passthrough layer. It works
   on the image that is displayed, so its edge can't lag.
4. **Layers**, from the bottom: passthrough, game with holes (mask, solid fill), keyed passthrough, game
   with holes (mask, solid fill, rim). Both game layers are the same image: two alpha values are packed in
   one channel and unpacked with `XR_KHR_composition_layer_color_scale_bias` (2a and 2a - 1).
5. **Color learning.** The passthrough camera (Camera2, 320x240) shows what the tracked hand looks like and
   what is around it. A thread builds the LUT from that several times per second.
6. **Verification.** Each hand segment is compared with the camera image. Confirmed segments that don't
   move are solid, and the 6 wrist segments always are (`key_solid` 6). Solid shows any color: a large part
   of the room seen next to the hand comes from it (the round ends of the wrist capsules, capsules that
   sit beside the hand).

**Narrower rim over leaky backgrounds** (`key_leak`, **not tested**). The learning thread measures for
each hand which share of the colors 2-7 cm around it the key keeps. Where that share is high the
background looks like the hand and would show in the rim, so the rim of that hand shrinks, down to 3 mm
inside the hand. It narrows within about 0.2 s and widens slowly. With `key_leak` on, each hand is also
verified against its own background instead of both together. At rest the rim is already 0, so only the
soft edge goes; the gain is for moving hands. Leaks inside the hand capsules stay.

**Leak map** (`key_leak_map`, **not tested**). On the headset the ring above barely reacted (logged
`leak` 0.00-0.13 over a skin-colored wooden floor that visibly leaked), while a narrow zone everywhere
(`key_rim_max 0`) was clearly better over clutter, with slight clipping of the hand (observed). So the
map can narrow the rim alone: L = max(ring L, weight x map L). The learning thread turns 1200 camera
pixels per update into directions around the head (an octahedral map of about 2 degrees per cell,
anchored where the head was, `graphics/src/leak_map.rs`). Pixels behind a located hand (bounding sphere
+ 3 cm + 80 ms of its motion) or its forearm (spheres along 50 cm from the wrist, 10 cm radius + 80 ms of
motion) are skipped, and a color is only stored if no hand came near it for 1 s.
The cells keep colors, so each new LUT re-evaluates the whole map. At render time each hand looks up the
cells behind its 28 segments from both eyes; the mean of the 7 leakiest gives the map value, which goes
through `key_leak_map_lo` / `key_leak_map_hi` like the ring. Fewer than half of the cells known: the
ring alone. When the head moves more than 20 cm for 1 s, the map moves with it (as if everything was
0.8 m away). With the first limit of 5 cm and a full restart, normal head motion restarted the map 20
times in 80 s and it never knew more than 10 % of the room (measured, ff6c6022), so it never acted.
The map is forgotten when most colors change
(light), after `key_leak_memory`, and when the reference space changes (camera images from before the
change time are dropped). A hand that is in view but not
tracked is learned as room. Unknown: whether the camera registration is good enough for 2 degree cells.

**Growth along the motion** (`key_sweep`, on the headset since c566b48e, never compared with 0). Without growth (`key_rim_max 0`) leaks were
clearly better, but the cutout did not follow a fast hand immediately (observed). Now the zone of a
moving hand is the hand moved anywhere along the velocity of its fastest joint, up to speed x
`key_rim_error` (limit `key_sweep_max`) ahead and behind; `key_sweep_bias` shifts this ahead or behind
(0: the same, earlier tests could not tell whether the tracked pose is ahead of or behind the image).
`key_sweep_iso` of the old growth stays in all directions (fingers, rotation). Below 1 mm the growth is
in all directions as before. The solid fill is not moved, and over leaks the sweep narrows like the rim.
In the shader each capsule is tested once: seen along the pixel's ray, the moved axis is a parallelogram
whose closest point is found like the closest points of two segments. Exact for constant radius (unit
test against 16 moved copies on random rays); a tapered capsule moving along itself can miss the
thicker end by up to the difference of its radii (1.7 mm measured with 3 mm tapers). Per pixel near a
moving hand: per capsule 2 dot products, 1 division and about 12 other operations more, plus the old
test for solid capsules; no texture samples. The zone, the scissor rect and the layer crop are larger
along the motion. A moving hand is skipped by a test of the bounding sphere moved along the motion (one
capsule test per hand and pixel), not a sphere grown by the sweep in all directions. GPU time not
measured. The `key:` line of `ALVR_CAM` logs `rim` and `sweep`
(ahead / behind, mm) of each hand.

**Bridge from the wrist to the forearm** (`key_wrist_len`, off since 2026-11-23: on the headset the user
saw it as a tail of the skeleton that "messes up where the arm continues", and it gave 16-23 % of the room
shown next to the hand, estimated). How it works, if it is ever needed again: between the hand zone (it ends
at the wrist joint) and the forearm of the depth mask a wedge showed the game (observed): the runtime
removes the hand and a bit of the wrist from the depth map. Now each hand has one more capsule from the
wrist joint, `key_wrist_len` long, radius: the wrist joint radius x `key_wrist_scale`. The camera chooses
its direction: at each update the learning thread tries 21 directions in the space of the wrist joint
(the default, opposite to the middle metacarpal, bent -60..60 degrees towards the palm in 20 degree steps,
times -25 / 0 / 25 degrees towards the thumb). Each gets the share of points along it (1 cm apart from 3
cm to the length: 6 for 80 mm, at most 24) whose camera color the key keeps; points hidden by the hand
don't count. A new direction must be better by 0.1 (equal scores: the middle of the equal ones, then the
least bent), the direction moves 30% towards it per update. Below a score of `key_wrist_follow` (0
where the background around the hand looks like skin, like the verification) it moves back to the
default instead: a sleeve or a skin colored floor is not followed. Confidence: that score, faded out
while the smoothed direction is more than 5-15 degrees from the chosen one (the camera only checked that
one). Without fresh data for 1 s: the default direction, confidence 0. The render thread turns the
direction with the wrist at display time, so the bridge doesn't lag behind the hand. It is keyed like the
hand (rim, sweep, leak narrowing) and solid only with confidence >= `key_wrist_conf`, faded with the speed
like the verified segments. The speed is the larger of the wrist's and the speed of the bridge's end from
the turn of the wrist joint: that joint also turns when only the hand bends, then the bridge swings with
the hand until the camera follows (about 0.3 s); a turn about the forearm doesn't count. With that confidence
the background ring and the forearm exclusions of the leak map also use it instead of the default. A bend
towards or away from the camera can't be seen; any of those candidates covers the forearm in the image
(unit test). Risk: a confident wrong direction over skin-colored wood keeps that wood out of the
background. The depth mask is unchanged: the playzone around the hands stays the bounding sphere (the
runtime removes the hand, not the forearm), and the arm shift still starts 3 cm behind the wrist; both are
combined with the bridge by max. So a long bridge adds keyed skin ahead of a moving forearm, but the late
depth behind it still shows the room. Per pixel of the focus pass, per hand: a check of the radius, a
bounding test of the bridge (about 15 operations, 2 square roots), near it one capsule test like one of
the 28 (two for a moving bridge with solid fill); no texture samples. `key_wrist_len 0`: the output is the
same as before (GPU test). The `auto key` line of `ALVR_CAM` logs `wrist` flexion / deviation (degrees)
and the confidence of each hand.

**Remembered playzone** (`playzone`, `playzone_auto`, `playzone_view`). On the headset (83a11bc7): it is
remembered at the start of a stream and whenever no hand is near the depth view for 1.5 s (39 times in
20 minutes), and its age shows in the `key:` line. In two streams it was not refreshed for minutes ("a
hand is near the depth view", also with the hands at the user's sides): it was 150 s old when a bottle
was gripped, and the upper part of the bottle still disappeared. The tint view was not tried. The runtime
removes
tracked hands and what they hold from the depth map, so within 6 cm of a hand (bounding sphere, across the
view ray) the mask also shows what a copy of the depth map taken without hands shows. First test on the
headset (bottle on the desk, hands hidden for 5 s, then gripped): the cap disappeared. The log had
`tracked hands [true, true]` in all 96 lines, also in the 16 with no hand in view: the runtime keeps out of
view hands active, so the old trigger (1.5 s without tracked hands) most likely never fired and nothing was
remembered. Now the copy is taken when no hand (bounding sphere + 10 cm) was inside the view of either depth
camera for 1.5 s, then every 0.5 s while none is; `playzone` still requests one. While driving the hands
never leave, so the copy is never refreshed then. `ALVR_CAM` logs each requested copy and the first of an
automatic series (`playzone: remembered ...`), and every 2 s why there is none yet (`playzone: none
remembered, a hand is near the depth view`); the `key:` line ends with `playzone none` or its age.
`playzone_view 1` with the "Tint mask" debug view: what only the copy shows is half opaque blue, the area
near the hands where it is used faintly blue. Known limits: it only adds (an object moved while held stays
where it was, a ghost), it is forgotten only by a new copy, `playzone -1`, a recenter or a new stream, a hand
in view that the runtime doesn't track is remembered as an object, and the copy is a whole snapshot (no
per-texel update).

**Steadier verification** (`key_steady`, `key_confirm`, `key_smooth`). Aim: fewer holes
and less flicker on the hand without showing more of the room. One fast or untracked finger no longer
stops the verification of the whole hand (`key_steady`); the axis of a segment can be verified with a
less strict table (`key_confirm`), its sides stay with the key, so a hand tracked beside the real one is
found as before; a narrower skin smoothing with a lower floor (`key_smooth`). Exit and weights are
unchanged: a segment drops at once below a share of 0.5. The `auto key` line of `ALVR_CAM` ends with
`lost floor` / `bg`: the shares of this update's skin samples that the key drops because they are too
rare as skin, or because of background or scene colors (kept without them). Then `gate L ..., R ...`,
per hand since the last line: segments not verified because fast (`f`) or untracked (`u`), with fewer
than 8 points in the image (`o`), voted below 0.5 (`v`), confirmed (`c`), and joint velocities without
LINEAR_VALID (`nv`). With `key_confirm` also `confirm kept` of the skin and of the background.

## What works

- **The zone grows with the speed of the wrist only** (`key_speed` 2). With the fastest joint, moving
  fingers blew up the zone; with palm and knuckles, a hand that turns in place did. User: "it looks good".
- **Verifying the axis of a segment with a wider table** (`key_confirm` 12): 71 % of the voted segments
  confirmed instead of 65 %, fill weight 0.55 instead of 0.44, the background unchanged (measured); user:
  "the best one so far".
- **Captures** (`key_dump`): the camera image as the learner reads it with the joints of the same instant,
  the tables and votes, and from the render thread the depth in meters, the mask and the focus area of
  both eyes. The readback works on GLES. The poses of two passes 60-80 ms apart agree to about one camera
  pixel (91 % of textured patches within 1 px).
- **Hands at the wheel position are in the camera image**, see "Where it stands".
- **Color keying in the compositor.** An RGBA `XR_META_passthrough_color_lut` makes the passthrough
  really transparent by color (observed, control test with an inverted LUT). 72 fps in all variants of
  the first test, load 55% instead of 50% (measured). We found nobody else who uses it like this.
- **Screenshots have the colors of the LUT input.** A threshold in the LUT shows up at the same value in
  `screencap` images (measured). So LUTs can be designed and tested offline.
- **Low resolution depth mask.** 1/4 resolution: 4.3 ms and 49% load instead of 8-15 ms and 81-99%
  (measured). Blending the region test of 3x3 depth texels (B-spline) hides the depth grid on flat
  surfaces, but can't fix the outline of fingers.
- **Passthrough camera** next to depth, hand tracking and the key: 59 fps, images about 22 ms old
  (CLOCK_MONOTONIC, which is the OpenXR time). Colors match the screenshots within 1-8 levels (median)
  and 5-9 levels (mean error, coarse matching). Cost: +0.3 ms GPU (measured).
- **Learning colors live.** Skin from the palm (and fingers of a still hand), background from a ring of
  2-7 cm (without the side of the forearm: opposite to the fingers, from the hand joints), plus the whole
  scene without hands and arms. A lamp that is switched on is learned in less
  than 0.5 s (observed).
- **Remembering** backgrounds (60 s), scene (20 s) and skin in other places (45 s). Switching between
  simple backgrounds is not noticeable any more (observed).
- **Strict skin match.** A color must be about as common as typical skin colors. Offline, never seen
  clutter that is kept dropped from 67-90% to 2-27% with 92-100% of the skin kept.
- **Small zone.** Offline (hand pasted on clutter, zone of 4 cm): key only 16% leak; sparse areas removed
  7-10%; connected to the hand 5%; zone limited to 1 cm 3%, with 99% of the hand kept.
- **0-2 mm rim at rest, up to 8 mm when moving** (grows 45 ms x speed, `key_rim_max`; 16 until
  2026-11-23): nothing cut off at rest, fast
  motion "good enough" (observed).
- **Verified skeleton.** The solid fill comes back without the old halo, because wrong segments are not
  confirmed. But solid capsules that sit a little beside the hand still show the room, see "Where it
  stands", points 1 and 2.

## What doesn't work

- **A looser skin model** (`key_smooth` 70). Skin kept 0.70 -> 0.84, but the background around the hand
  1 -> 4 % and the scene 1 -> 11 %; user: "leaks a lot more". The offline simulation had predicted less
  leak in all 28 cases. The dark parts of the hand are rare colors, and making rare colors pass lets the
  room pass.
- **A keyed bridge from the wrist to the forearm.** A tail of the skeleton that disturbs where the arm
  continues (user), and an estimated 16-23 % of the room shown next to the hand. In 4 captures its
  direction emptied the ring of background colors.
- **Remembering the depth map for held objects.** See "Where it stands", point 5. The first trigger (no
  tracked hands) never fired. The second fires when the hands are away, not while they stay in front,
  and a carried object is not where it was remembered.
- **A fixed correction of the camera model.** See "Where it stands", point 2: the offset is not constant
  (none in one set with the same calibration) and most likely not the camera's.
- **The difference from a remembered background as the mask itself.** Too many false marks on a near
  desk without exact depth, and one pass late. As a gate on the key it looks promising offline (not
  built), see point 3.
- **Tighter solid fill by `key_inset`.** 2 / 2 and 3 / 3 against 4 / 4: fuller spread hand from the back,
  dark parts the same, background cut slightly less precisely; the user kept 4 / 4.
- **Noise on the hand: where it is** (measured 2026-11-23 on 8 hand crops of screenshots, 1 M pixels, and
  on the log). A still hand is clean inside (0-0.1 % holes deeper than 12 px). The loss is in the outer
  4 px, where only the key decides (6-28 % dropped, 29-49 % half transparent), and on moving hands
  (4-10 % dropped): shadowed sides, creases, nails, knuckles and very bright skin, same hue as the kept
  skin but darker. The key keeps a median 0.79 of its own skin samples on the headset, 0.94 in an offline
  copy of the model: the difference is not explained.
- **A shape detector for the hand: designed, not built** (2026-11-23). Two variants: an outline mask from
  the camera image as a texture, and capsules fitted to the camera (width, offset, tip). Offline best
  case (4 raw screenshots, simulated camera, skeleton placed by hand): hand kept 0.86 -> 0.93, darkest
  quarter 0.73 -> 0.87, room within 1 cm 2.1 -> 3.1 %. Limits: one camera pixel is about 2 mm at the hand
  and the loss band of a still hand is the outer 1-2 mm ("4 px" of a screenshot), so the edge stays with
  the key; a camera result is 45-160 ms old when used (5-15 mm at 0.1 m/s), so still and slow hands
  only. The simulation does not match the headset (it also predicted no leak for `key_smooth` 70), so
  nothing is built before a test on real captures (`key_dump`).
- **Camera coverage** (measured with the user, looking ahead): hands at the wheel position are inside
  the camera image and about 90 % of the voted segments are confirmed; a reach to the shifter leaves
  the image, there only the key decides.
- **`key_confirm` does not fill dark segments** (palm in shade, fist; user: no difference): 10 of the 15
  points of a segment, the ones beside the axis, vote with the key table.
- **Closing holes of the table in color space.** The kept colors are one block without holes: gains
  nothing (learned tables simulated, `skin.lut` / `skin2.lut` measured).
- **Widening the skin along the brightness.** One cell darker and brighter: +1.4 to +2.5 points of the
  hand for +5 to +10 points of the background next to it (simulated on 2 raw hands).
- **Depth alone for hands.** 320x320 texels at 30 Hz (measured), data about 20 ms old plus 11-15 ms of
  processing. Fingers are jagged, and hands lag roughly 65-90 ms (estimate).
- **Hand removal can't be turned off.** While hand tracking runs, the runtime removes tracked hands from
  the depth map, also after `xrSetEnvironmentDepthHandRemovalMETA(false)` (observed: the hand and the neck
  of a gripped bottle were missing). Others found the same:
  [openftgs-xr #608](https://github.com/samaust/openftgs-xr/issues/608).
- **A fixed color table.** Works until the light changes.
- **Keying by brightness.** Lit skin and a beige desk look the same.
- **Keying everything near.** Desk and wheel disappear. Key only around the hands.
- **Unknown colors: keep or remove?** Keeping them shows clutter, removing them cuts holes. Now removed.
- **An unverified solid hand.** Shows hand-shaped pieces of the room when the tracking is wrong, and the
  fingers of the model were too long because of the round ends of the capsules.
- **Hand tracking as the truth.** It is an estimate. Road to VR measured it 31 ms behind the passthrough
  image (2024). Shifting the pose 35 ms into the past made it as late as the depth map. The pose at display
  time is already predicted; 0 ms is the current setting. (2026-11-24, measured offline in bursts: the
  joints asked for the display time trail the real wrist by 40-76 ms, so they are not predicted; see "Day
  of 2026-11-24".)
- **Filtering the camera image directly** (noise, connectivity). Estimated 60-75 ms late, so it would
  cut moving hands. It is fine for colors, and for checks of slow hands.
- **Shifting the late depth of the arm by the motion of the wrist.** Jitters, and the edge stays coarse
  (observed). It is on again by default since the body tracking forearm was removed; `arm_shift 0` turns
  it off to compare with plain depth (not tested).
- **The forearm from Meta's body tracking** (`XR_FB_body_tracking`, elbow of the body API). The forearm
  zone took its direction from the estimated elbow. With the arm stretched out or the wrist bent down the
  estimate was off, the zone lay beside the real forearm and the color key showed the room there, e.g. a
  large patch of skin-colored parquet (observed on the headset). It also cost GPU time (11654b86, see
  below). Removed: the forearm is depth only again.
- **A wide zone for fast hands.** 12 cm around a fast hand near the face showed most of the room.
- **Skin-colored backgrounds** (wood, cardboard): a key that only sees color can't separate them.
- **`performance_level` GPU = SustainedHigh**, changed while streaming: the clock stayed at 456 MHz.

## GPU budget

Measured in the SteamVR home, 72 Hz, with color key on unless noted:

| Build | Content | fps | GPU load | App GPU time |
|---|---|---|---|---|
| - | Passthrough off | 72 | 29% | 2.9 ms |
| first | Depth at full resolution | 58-72 | 81-99% | 8-15 ms |
| a04e4975 | Depth at 1/4 resolution, no key | 71-73 | 48-56% | 4.3 ms |
| d8010e3b | Key, learning, wrist fill | 71-74 | 76-83% | 6.1 ms |
| 23701e5f | + verified skeleton, arm shift | 71-74 | 77-92% | 6.8 ms |
| 11654b86 | + forearm, playzone map | **58-73** | **88-99%** | **9.5 ms** |
| 83a11bc7 | Key, learning, verification, sweep, no body tracking forearm | 72 | 76% without hands (2 layers), 80% median and peaks of 96% with hands (4 layers) | about 7 ms |

- The last build ran at 456 MHz and 48-50 degrees, so heat may be part of it.
- Suspects: the forearm test for all 9 depth samples of every pixel, and the bigger area of the hand pass.
  The next commit reduced both and used 1/5 resolution. Since then the body tracking forearm (capsules, arm
  removal) has been removed, so its cost is gone. Measured 2026-11-23, see the table.
- 3609efde, no hands in view (measured): 4 layers were submitted; key off took TW from 4.69 to about
  2.0 ms and load from about 85% to 62%, App time barely changed. Boost or SustainedHigh requested
  while streaming: level stayed 2, 456 MHz.
- Since 4a0a59d5 (runs on the headset: `layers 2` without hands, `layers 4` with, in the `key:` line):
  the two extra layers only while a hand is in view (plus 0.3 s);
  hands behind or beside the head no longer count. The lower game layer only covers the hands plus
  3% of the view. Where the lower layer is cropped away or not used (2 layers), the shader writes the
  alpha so that the upper layer alone shows what both would (upper + (1 - upper) x lower), so soft mask
  edges don't change at the crop border. Only colors that the key keeps can still differ there, at soft
  mask edges outside the hand rect. The focus pass is skipped without hands. Switches below
  (`key_layers`, `key_crop`, `key_hands`, `arm_shift`) to measure each part; the performance level
  is also requested at stream start, results and the layers in use are logged with `ALVR_CAM`.
- CI builds the APK in the dev profile. Without `CARGO_PROFILE_DEV_OPT_LEVEL=3` one color update took
  312 ms instead of about 15 ms, and wgpu validation was on.

## Pitfalls (each of these cost a crash or an hour)

- **wgpu-hal (GLES) resolves immediates with the wrong module.** It looks up type handles of the compacted
  naga module in the original one. `struct Immediate` must be the first declaration of every shader,
  shared code is appended, never prepended. A test checks this.
- **The depth swapchain needs a current GL context** (`GraphicsContext::make_current`), otherwise the
  texture names are 0.
- **`xrAcquireEnvironmentDepthImageMETA`** fails in the frame in which the provider is created.
- **Logs:** while streaming the client logger only forwards errors. The learning thread logs with the
  tag `ALVR_CAM`.
- **APK signing:** every CI run has a new debug key. Uninstall `alvr.client.dev` first. Permissions and
  the pairing are lost.
- **adb over Wi-Fi** ends at every reboot of the headset: USB, `adb tcpip 5555`, then accept the prompt
  in the headset.
- **OpenXR games** (BeamNG.drive) need SteamVR as the active OpenXR runtime, see the README.
- **A hand that is in view but not tracked** is learned as an object of the playzone.
- **Hands out of view are still "tracked".** `relate_hand_joints` gives a pose as long as the runtime says
  the hand is active, also when it is out of the display view (measured: 96 of 96 `key:` lines with
  `tracked hands [true, true]`, 16 of them without a hand in view). Use the view, not that flag.
- **A GL error left behind aborts the app.** wgpu doesn't call glGetError, `check_error` (staging renderer)
  does and aborts. Code that may fail on the GL side (the readback of `key_dump`) must clear the flags itself
  (`GraphicsContext::take_gl_error`).
- **Leaving the play area** pauses the app and restarts the stream. The key comes back by itself.
- **Hands at the user's sides can stay "near the depth view"** for the runtime, for minutes. Nothing may
  rely on the hands being gone.
- **"Only a plane in front of me, no VR space"**: the compositor of SteamVR had died while the streamer
  and `vrserver` kept running, and the stream showed no scene. Close SteamVR and start it again; captures
  from such minutes are worthless.
- **adb over Wi-Fi also drops whenever the headset sleeps.** `adb disconnect`, then `adb connect` once it
  is on the head again; a log capture has to be restarted. Properties survive until a reboot.
- **A watcher on the log** acted on the last matching line, which was older than the watcher. Compare the
  time of the line.
- **No background colors around the hands** (all rings empty) switches the solid fill off (`key_steady`
  1), see "Where it stands", point 6.

## Settings

`adb shell setprop <name> <value>`, most of them apply within 0.5 s.

| Property (`debug.alvr.`) | Default | Meaning |
|---|---|---|
| `color_key` | 0 | 0 off, another value (re)loads `/data/local/tmp/alvr_color_key.lut` |
| `auto_key` | 0 | 1 before starting the app: learn the colors, asks for the camera permission |
| `key_rim` / `key_rim_error` / `key_rim_max` | 0 mm / 45 ms / 8 mm | Zone at rest, growth with the speed, limit. 8 since 2026-11-23: the value on the headset for every verdict of that day (16 before; 0 clips fast hands) |
| `key_speed` | 2 | Joints whose speed grows the zone. 2: the wrist only (the arm moves; user: "it looks good"), 1: palm, wrist and knuckles (fine for waving and closing the fingers, but a hand that turns in place grows it), 0: the fastest joint (moving fingers grow it too) |
| `key_sweep` | 1 | 1: the zone grows along the motion of the hand, 0: in all directions as before. On the headset since c566b48e, never compared with 0 |
| `key_sweep_max` / `key_sweep_iso` / `key_sweep_bias` | 40 mm / 25 % / 0 % | Limit of the growth along the motion (each side, before the bias), share of the growth that stays in all directions (limited by `key_rim_max`), -100..100: more ahead (positive) or behind |
| `key_inset` / `key_falloff` | 4 mm / 4 mm | Where the solid fill starts and how it fades out. 2 / 2 and 3 / 3 compared on the headset: a spread hand seen from the back is fuller, dark parts (palm, fist) are the same, the background is cut slightly less precisely; user: 4 / 4 is the best |
| `key_error` | 10 ms | How much the fill shrinks with the speed |
| `key_solid` | 6 | Segments that are always solid: 0 none, 6 wrist, 9 palm, 28 hand |
| `key_tight` / `key_bias` / `key_unknown` | 30 / 150 / 10 % | Strictness, preference for skin, unknown colors |
| `key_smooth` | 100 % | Smoothing of the skin colors, of a LUT texel (70..100). `key_tight` is scaled by its cube, so 70 is about a third of the floor: rarer skin colors are kept, not more around the common ones, but the tails of a widely spread skin (varied light) reach further (computed: 36 % more cells at 70). 100: as before. On the headset 70 leaks: skin kept 0.70 -> 0.84, but the background around the hand 1 -> 4 % (worst tenth 3 -> 16 %) and the scene 1 -> 11 %; user: "leaks a lot more". Values in between not tested |
| `key_confirm` | 12 % | `key_tight` of a second table that is never uploaded: the points on the axis of a segment are verified with it (the points beside it with the key), and the reliability comes from the share of the background around the hands that it keeps. Capped at `key_tight`, so it keeps everything the key keeps. 0: off. On the headset with 12: of the segments that are voted on 71 % are confirmed (65 % without), verified weight 0.55 (0.44), background unchanged; user: "the best one so far", in some cases the hand is still not full enough |
| `key_steady` | 1 | 1: a hand is verified while its palm (palm, wrist, knuckles) is tracked and slower than 0.5 m/s, each segment while its own two joints are too; its background is then sampled for the reliability only, and a hand with a fast finger is judged by its own background (fewer than 50 colors: no fill). Colors are still learned only from whole hands. 0: the whole hand or nothing, as before. Runs on the headset, not compared with 0 |
| `key_delay` / `key_scale` | 0 ms / 100 % | Hand pose offset (negative predicts), thickness |
| `playzone` / `playzone_auto` | 0 / 1 | New positive value: remember at the next depth image, -1 forgets it; the value at the start of the stream is not a request. Automatic after 1.5 s without a hand near the view of the depth cameras: fires at the start of a stream and when the hands are away; the runtime can keep hands near the view for minutes |
| `playzone_view` | 0 | 1: with the "Tint mask" debug view what only the remembered playzone shows is half opaque blue, where it is used (near the hands) faintly blue. Not tested |
| `key_layers` | 4 | 4: extra layers only with hands in view, 2: never, 5: always, 30 / 31: always three (no upper game layer / no keyed passthrough). Not tested |
| `key_crop` | 1 | Lower game layer only around the hands. 0: full, 2: y origin at the top (if 1 is upside down). Not tested |
| `key_hands` | 1 | 0: no hands in the GPU passes. Not tested |
| `hand_removal` | 1 | 1: with the key on the runtime takes the hands out of the depth map, and an uneven 3-15 cm around them (measured). 0: asks the runtime to keep the hands in the depth map, but it does NOT bring them back: measured on 2026-11-24 with the property at 0 (log `hand removal from the depth map: off` at 08:53:13.8), the runtime still removed tracked hands and filled tiles around them with far depth (captures 626 / 627, analysis in `logs/analysis-20261124-lap`). It stays as a probe. Log lines with the answer of the runtime, both ways: `hand removal from the depth map: on / off: the runtime answered <SUCCESS or ERROR_...>` on a change, `hand removal from the depth map at the start of the depth: on / off: the runtime answered ...` when the depth starts (until 2026-11-24 only a failed enable was logged, as an error, and the change line had no answer) |
| `arm_shift` | 1 | 1: the depth around each arm is shifted by the motion of the wrist (with the key only up to the wrist, the hands are removed from the depth map; plain depth for a wrist that moved more than 25 cm or has no valid position), the wrist located at the display time minus `key_delay`, the weight a smoothstep from 1 at the wrist to 0 at 45 cm. 0: plain depth. Any other value (negative, 3 and up) means 1, as before mode 2 existed. 2 (step 1 of `docs/arm-mask-build-plan.md`, built 2026-11-24, not yet run on the headset): the wrist is looked up in the past at H = display − max(`arm_history_ms`, `arm_target_ms`) and 30 ms before H, moved ahead to the target T = display − `arm_target_ms` with the finite-difference velocity (at most 6 cm), and the weight falls linearly from 1 to 0 over `arm_weight_mm`. Also without the key. 1 runs on the headset, 0 not tested. Line `arm: ...` on `ALVR_CAM` on each change of an `arm_*` property (the settings, "(new settings)") and every 2 s while an arm uses mode 2: per arm the extrapolation (median and p90, mm, only of frames with a velocity; "no velocity" if none had one) at the median speed of the finite difference, the failed joint lookups per kind of the frames since the last line (mode 2: `lookups failed H a, H-30 b, capture c of N frames`, H the history time, H-30 the lookup 30 ms before it for the velocity, capture the depth capture time; mode 1: `display a, capture c`; a failed H or display lookup skips the capture lookup; a failed H-30 lookup means no step that frame) and the `hand-to-arm wrist gap` (median and p90, mm, between the wrist of the hand capsules, display time minus `key_delay`, and the wrist the arm is placed with; "-" without the key's hands) |
| `arm_hands` | 3 | With `arm_shift 2`: the arms that use mode 2 (1 left, 2 right, 3 both, 0 none); the others use mode 1, for a side-by-side check |
| `arm_target_ms` | 40 ms | With `arm_shift 2`: the arm is placed for the display time minus this (the passthrough latency; try 20 / 40 / 60 while waving). 0..150 |
| `arm_history_ms` | 80 ms | With `arm_shift 2`: the wrist is looked up at the display time minus this, at least `arm_target_ms` (the runtime's newest estimate is late, the past is accurate). 0..200. If the H or H-30 lookups fail (`arm:` line), lower it (and check the tracking): a lookup too far in the past is the likely cause (not measured) |
| `arm_weight_mm` | 300 mm | With `arm_shift 2`: length of the linear weight along the arm. Offline best 26-35 cm, 20 cm worse. 100..600 |
| `key_leak` | 0 % | Narrower rim where the background around a hand looks like it, 0: off, 100: on. Needs `auto_key`. Not tested |
| `key_leak_lo` / `key_leak_hi` | 5 / 20 % | Share of the colors around the hand that the key keeps: where the narrowing starts / is complete |
| `key_leak_inset` | 3 mm | Rim at full narrowing, inside the hand. 0: down to the soft edge only |
| `key_leak_force` | 0 % | Narrow both hands this much, without the camera |
| `key_leak_view` | 0 | 1: with the "Tint mask" debug view the narrowed rim is red. 2: also the right hand is replaced by 28 spheres at the leakiest places of the leak map (learns the map). Not tested |
| `key_leak_map` | 0 % | Weight of the leak map, 0: off and not learned. Needs `auto_key`. Not tested |
| `key_leak_map_lo` / `key_leak_map_hi` | 3 / 15 % | Leak of the map cells behind the hand: where the narrowing starts / is complete |
| `key_leak_memory` | 120 s | How long the leak map remembers a direction |
| `key_leak_clear` | 0 | New value: forget the leak map |
| `key_leak_dump` | 0 | New value: log the leak map of the camera view, 40x30 (`#` leaks, `.` doesn't, `?` unknown, `H` hand). Not 0: the map is learned |
| `key_leak_probe` | 60 cm | Distance of the spheres of `key_leak_view 2` from the anchor of the map |
| `key_lut_dump` | 0 | New value: write the learned LUT to `/sdcard/Android/data/alvr.client.dev/files/alvr_learned_key_<time>.lut` |
| `key_dump` / `key_dump_burst` | 0 / 1 | New value `n` other than 0: write the next `key_dump_burst` (1..30) learning passes to the same folder, `alvr_dump_<n>_<00..>`, also without hands. Each line `dump <n>/<index>: <path>` on `ALVR_CAM`. The render thread also writes the depth of one frame, `alvr_dump_<n>_depth*` (below), also without `auto_key`; `depth dump <n>: ...` on `ALVR_CAM`. Works on the headset; 25 requests 3 s apart cost no visible frames |
| `burst` / `burst_ms` | 0 / 3000 ms | New value `n` other than 0 (the value at the start of the app is not a request, a new `n` during a burst ends it early and starts the new one): record `burst_ms` (100 to 6000) of consecutive frames into `alvr_burst_<n>/` in the files folder (format below): every new depth frame (both eyes), one record per display frame, the camera images of the key learner (only with `auto_key` and the key on), and one normal depth dump `alvr_dump_<n>_depth*` at the start in the same folder. Kept in RAM, written by a thread after the burst. Lines `burst: <n> started ...`, `burst: <n> ended after ...: <d> depth frames (<r> read back, <s> not recorded), <k> display frames, <c> camera images, ...; writing <MB> MB to <folder>`, `burst: <n> written: <files> files, <MB> MB in <folder> (<ms> ms)`, failures `burst: <n> depth frame <i>: <error>` (the first 3; all of them are counted in `failed readbacks` of the `ended` line), `burst: <n> cut short: the stream stopped` (the stream ended during the burst: what was recorded is written all the same), `depth dump <old>: replaced by burst <n>` (a `key_dump` still waiting gave way to the dump of the burst) and `burst: <n> writing to <folder> failed: <error>`. Only with Passthrough = Depth. Not tested on the headset |
| `key_wrist_len` | 0 mm | Keyed bridge from the wrist towards the forearm, 0: off, up to 300. Was 80 until 2026-11-23: the user sees it as a tail of the skeleton that "messes up where the arm continues"; the skeleton is for the hand up to the wrist only, the arm is not to be extended from it. The direction and the solid fill need `auto_key`, without it: opposite to the fingers, never solid |
| `key_wrist_scale` / `key_wrist_conf` | 100 / 60 % | Its radius (of the wrist joint radius), how sure the camera must be of its direction for the solid fill |
| `key_wrist_follow` | 30 % | Below this score of the best direction (share of points the key keeps, 0 over skin-like backgrounds) the bridge goes back to the default direction. 0: always the best one. Not tested |

The LUT file must exist even with `auto_key`: 32x32x32 RGBA bytes, indexed `[b][g][r]`. It is used until
enough colors are learned.

**`key_dump` files**, what the learner saw in one pass (the camera image with the hands of the same
instant, for offline tests of the learner):
- `.rgb`: width x height x 3 bytes, row major, the RGB of each pixel as the learner converts it from YUV.
- `_key.lut` (published key table, same format as `key_lut_dump`), `_key_alpha.lut` (alpha of the key
  table as the learner uses it, one byte per cell in the same cell order; the published one only follows
  it by steps of more than 10/255) and `_confirm.lut` (alpha of the confirm table, same layout, only while
  `key_confirm` is on): only with index 00 of a burst, `tables` in the JSON names the files that belong to
  each pass (null: not written). Files are overwritten by name: an older burst of the same `n` can leave
  higher indices behind, they have another `burst_ns`.
- `.json`: `n`, `index`, `count` (passes of the burst), `burst_ns` (`image_ns` of index 00), `image_ns`
  (CLOCK_MONOTONIC), `image_age_ms` (when the pass started),
  `pass_interval_ms`, `monotonic_ms`; `camera`: `width`, `height`, `fx`, `fy`, `cx`, `cy`, `pose` and
  `head` (`position` m, `orientation` quaternion xyzw, stage space at the time of the image; the camera
  looks at +z with +y down); `params` (`key_tight` ... as fractions, `key_steady` 0 / 1); `counts` (skin,
  background and scene samples of the pass, shares of skin and background kept by the key and the confirm
  table); `left` / `right`: `located`, `learned` (all joints tracked and slow), `verified`, `reliability`,
  `eligible`, `shares` (28, null: none) and `weights` (28, smoothed, after the pass); if located also
  `joints` (26: `position`, `radius`, `tracked`, `velocity`, `velocity_valid`, stage space, time of the
  image) and `capsules` (28 x start xyz r, end xyz r, as the learner uses them: tips pulled back, radii
  not scaled by `key_scale`). Numbers that are not finite are null.

A reader for offline tests: load the JSON, read the `.rgb` as a `height x width x 3` byte array, read the
tables of the burst from the file names in `tables` (key: `[b][g][r]` RGBA, alpha in the 4th byte;
key_alpha and confirm: one byte per cell; cell `r + 32 * g + 1024 * b` with each channel
`(v * 31 + 127) // 255`, a color is kept above 0.5, a byte of 128 or more). `shares` (key_alpha beside
the axis, confirm on it, key_alpha while confirm is off) and the `*_kept` counts use key_alpha and
confirm, not the published key. Keep only the files whose `burst_ns` matches the one of index 00.
Project the capsules with the camera pose and `fx`, `fy`, `cx`, `cy`: `pixel = (x / z * fx + cx, y / z * fy + cy)` for a point `(x, y, z)` in the
camera frame (`orientation` inverse times the point minus `position`).

**`key_dump` depth files** (render thread, works on the headset), once per new `n`, from the next frame
with a depth image. If nothing arrives within 1 s, or a copy or the file fails, one `depth dump <n>: <error>`
line and nothing else (`depth dump <n>: replaced by <m>` if a new value came before a depth frame). A dump may cost a frame or two: the first one compiles its shader, and the frame
whose readback is ready copies about 4 MB of mapped memory on the render thread (decoded and written on
another thread):
- `alvr_dump_<n>_depth_left.f32` / `_right`: the environment depth as the mask pass reads it, little-endian
  f32, meters from the plane of the depth camera (along its -z), 0: no data. `depth.width` x `depth.height`,
  rows in the order of the depth texture: row 0 is the bottom of the depth view (`flip_y` false). Converted on
  the CPU from the exact 16 bit values with `near` / `far` (`far` null: infinite).
- `alvr_dump_<n>_playzone_left.f32` / `_right`: the remembered depth, same layout (stored as 16 bit floats,
  0: no data), only if there is one. Its camera is `playzone_camera`, not the current one.
- `alvr_dump_<n>_mask_left.rgba` / `_right`: the depth mask, `mask.width` x `mask.height` x 4 bytes, row 0 is
  the top of the view. r: region coverage, g: proximity (1 at the eye, 0 at 3 m), b: depth data (0/1), a: the
  part of r that comes from the playzone.
- `alvr_dump_<n>_focus_left.rgba` / `_right`: the focus area, same layout (`focus` size). r: hand with its
  rim, g: solid fill, b: near a hand (where the playzone is used), a: narrowed rim.
- `alvr_dump_<n>_depth.json`: `n`; `display_ns` (predicted display time of the frame), `depth_capture_ns`
  (null: not reported), `now_ns` (when it was rendered), all XrTime, which is CLOCK_MONOTONIC on Quest like
  `image_ns` of the camera dumps; `depth` (`width`, `height`, `near`, `far`, `flip_y`), `mask`, `focus`
  (sizes); `playzone` (`age_s`, null: none); `region` (`mode` distance: `max_distance`, volume: `center`,
  `yaw_deg`, `size`; `feathering`; the region the mask used: a box placed from a marker has `source`
  "marker" and its `volume_to_world`, column major); `debug_view`; `color_key`; `eyes` (left, right): `eye_position`, `view`
  (`pose` and `tangents` left, right, down, up of the streamed view), `uv_to_world_dir` (column major 4x4:
  `dir = M * (u, v, 1, 0)`, uv from the top left), `depth_camera` and `playzone_camera` (`pose`, `tangents`,
  `world_to_view`, `view_to_world`), `files`; `hands` (`bounds` xyz r, `capsules` like the camera dumps,
  `rim`, null: none); `arms` (`wrist` (where the arm is placed: the wrist at `target_ns`), `direction`,
  `displacement`, and since 2026-11-24 `mode` (1 or 2, see `arm_shift`), `weight_len` (m, 0: the smoothstep
  of mode 1), `target_ns` (the time the arm is placed for), `history_ns` (when the wrist was looked up; =
  `target_ns` in mode 1), `wrist_history` (the wrist at `history_ns`), `extrapolated_mm` (from
  `wrist_history` to `wrist`); null: plain depth). Stage space, meters.

Depth texel (x, y) to the world: `tx = l + (x + 0.5) / width * (r - l)`, `ty = d + (y + 0.5) / height * (u - d)`
with the `tangents` of `depth_camera` (`y` from the bottom), point `view_to_world * (tx * z, ty * z, -z, 1)`
with `z` the value in meters. To a mask texel of an eye: the direction from `eye_position`, `uv` from
`uv_to_world_dir` (the tangents of `view` in the orientation of its pose); to the camera image of the camera
dumps: `pose` and `fx`, `fy`, `cx`, `cy` there, at its `image_ns`.

**`burst` files** (`debug.alvr.burst <n>`, added 2026-11-24, compiles for Android and passes the unit
tests, not run on the headset): consecutive frames for offline tests (stacking depth frames of static
things, moving the arm's depth with the wrist between depth frames, the tiles the runtime fills around
tracked hands, whether the camera sees motion before the depth). Everything is kept in RAM during the burst
(about 0.4 MB per depth frame as 16 bit values, 0.23 MB per camera image; at most 240 depth, 720 display,
200 camera records) and written by a thread after it. Folder `<files>/alvr_burst_<n>/` (`adb pull
/sdcard/Android/data/alvr.client.dev/files/alvr_burst_<n>`); files of an older burst with the same `n` are
overwritten by name, the index lists the ones of this burst. Reader: `tools/read_burst.py <folder>` (numpy
arrays and a summary: counts, the interval of the depth frames, the age of the depth at display), tested
on a synthetic burst written by the Rust unit test `depth_burst::tests::files_hold_the_depth_in_meters`
(`ALVR_BURST_TEST_DIR=<folder> cargo test -p alvr_graphics depth_burst` keeps it).
- `alvr_burst_<n>_depth_<iiii>_left.f32` / `_right`: depth frame `iiii` (from 0000), same layout as
  `alvr_dump_<n>_depth_left.f32` (little-endian f32, meters from the plane of the depth camera, 0: no
  data, row 0 is the bottom of the depth view with `flip_y` false), converted from the exact 16 bit values
  with the `near` / `far` of that frame. Read back like the depth dump (the pass of `depth_dump.wgsl`).
- `alvr_burst_<n>_camera_<iiii>.rgb`: a camera image of the key learner as in the `key_dump` files
  (`width` x `height` x 3 bytes).
- `alvr_dump_<n>_depth*`: one normal depth dump (above) from the first frame with a depth image after
  the start, with the mask and the focus area (the burst itself does not keep the mask: it can be rebuilt
  from the records).
- `alvr_dump_<n>_key.lut` (since version 2, only if there is one): the learned colour table of the key as
  it was last uploaded to the compositor before the start (32x32x32 RGBA, `[b][g][r]`, like
  `key_lut_dump`). None while the key is off, without `auto_key`, or before the learner published a table
  (then the table of the file is shown). After a `color_key` reload the file's table is shown until the
  learner publishes again, and the burst records none (since the review fix of 2026-11-24: a new
  `color_key` version clears the recorded table). None therefore means the file's table (or no key).
- `alvr_burst_<n>.json`, the index: `format` "alvr_burst", `version` 2 (1 until 2026-11-24: no `key_lut`
  and only `wrist`, `direction`, `displacement` per arm; `read_burst.py` reads both), `n`, `duration_ms` (asked),
  `start_ns` / `end_ns` (XrTime, CLOCK_MONOTONIC on Quest like every time here), `depth` (`width`,
  `height`, `flip_y`), `camera` (`width`, `height`, null: no image), `counts` (`depth`, `depth_read`,
  `depth_skipped`: new depth frames not recorded because the limit of 240 was reached (a new depth frame
  that finds 6 readbacks on their way is recorded, with `error` "too many readbacks on their way", and
  counted in `failed readbacks` of the log; `read_burst.py` lists it under "readback errors"), `display`, `camera`, `region`), `dump` (the JSON of the depth dump that was asked for; the line
  `depth dump <n>: ...` says whether it was written), `key_lut` (`file`, `version`: counts the learned
  tables uploaded since the stream started, `published_ns`: when it was uploaded, `bytes`; null: none),
  and `records`:
  every record in time order of `t_ns` (when the app got it; at equal times region, depth, display,
  camera). Each record has `kind` and `index` (from 0 per kind):
  - `depth`: a new depth frame (a new capture time, swapchain index or depth view pose; the first one of a
    burst may have come before the start: its `now_ns` is the start and its `display_ns` that of the first
    display frame, so `read_burst.py` leaves it out of the times when the app got the frames). `capture_ns` (as the runtime
    reports it, null: not reported), `display_ns` (predicted display time of the frame it came with
    first, passed to the acquire), `now_ns` (when the app got it, = `t_ns`), `swapchain_index`, `near`,
    `far` (null: infinite), `eyes` (left, right: `pose`, `tangents`, `world_to_view`, `view_to_world`
    like `depth_camera` of the dump; column major), `files` (left, right; null: not read back), `error`
    (why not, else null).
  - `display`: one display frame (72 Hz). `display_ns` (predicted), `now_ns` (when it was rendered),
    `depth` (index of the depth frame the mask used, null: none, or a frame past the depth limit),
    `region` (index of the region record
    that applies), `views` (the eyes located at the display time: `pose`, `tangents`; the mask casts its
    rays from their positions), `stream_views` (of the streamed image: the directions of the mask pixels),
    `eye_positions`, `hand_ns` (when the hands were located: the display time minus `key_delay`), `hands`
    (left, right, null: not located: `joints` 26 x [x, y, z, radius], `tracked` 26 x POSITION_TRACKED,
    `velocities` 26 x [x, y, z] or null without LINEAR_VALID; joint radii as reported, not scaled by
    `key_scale`; located by a second lookup at the same time and space as the key's, later in the frame,
    so the runtime may answer slightly differently), `capsules` (left, right, null: no hand in the mask or no key: `capsules` 28 x start xyz r,
    end xyz r exactly as the mask used them, scaled, after `marker_hands`; `rim`), `arms` (left, right, null:
    plain depth: `wrist`, `direction`, `displacement`, and since version 2 `mode`, `weight_len`,
    `target_ns`, `history_ns`, `wrist_history`, `extrapolated_mm` as in the depth dump JSON). Stage space,
    meters.
  - `camera`: one pass of the key learner (about every 60 ms, not every camera frame: the learner takes
    the newest image when it runs). `t_ns` = `now_ns` (CLOCK_MONOTONIC when the pass started),
    `image_ns`, `width`, `height`, `fx`, `fy`, `cx`, `cy`, `pose` and `head` (as in the `key_dump` JSON:
    stage space at the time of the image, the camera looks at +z with +y down), `file`, `hands` (like
    `display`, located at the time of the image).
  - `region`: the region the mask uses, at the start and each time it changes: `region` as in the depth
    dump JSON.

What a burst costs (in principle, not measured): per new depth frame (about 30 Hz) two passes of 320 x 320
texels into two kept RGBA8 textures, an 800 KB copy into one of up to 6 reused buffers, and on the render
thread, once it is mapped (a frame or two later), 800 KB read from mapped memory into 400 KB of 16 bit
values; per display frame the hand joints located once more (both hands, at the time of the key), the region
JSON built and compared, and a record of a few KB. The camera images are converted to RGB on the thread of
the learner (its pass is that much later). The conversion to meters and every file are done by the writer
thread after the burst (a 3 s burst: about 90 depth frames, 74 MB of depth files). The first readback of
an app run compiles the pipeline of the depth dump (a frame or two, as for `key_dump`). The records are in
stage space: a STAGE change during a burst moves their coordinates (it is in the log).

**Testing the leak map on the headset** (`adb logcat -s ALVR_CAM`, `auto_key 1`, debug view "Tint mask"):
- [ ] `key_leak_map 0`: same look and same App / TW times as before; no `leak map` lines in the log.
- [ ] Hands in the lap, `key_leak_map 100`, wait 5 s, then `key_leak_dump 1`, `2`, ...: `#` where wood
      and cardboard are, `.` on plain surfaces, `H` where the hands are. Switch a lamp and dump again.
- [ ] `key_leak_view 2`, right eye closed: the spheres sit on wood and cardboard. Turn the head 30 degrees
      and back: they stay on the objects. Lean 10 cm for 2 s: `anchor moves` goes up in the log.
- [ ] The left hand over a plain surface, then over wood: the `leak map:` line shows `map` above
      `key_leak_map_hi` and `L` near 1 over wood, below `key_leak_map_lo` over plain. Tune them from there.
- [ ] Fast arrival onto wood with `key_leak_map` 0 against 100: the rim is narrow from the start with 100.
- [ ] The `leak map ... ms` part of the `auto key` line and the `lookup ... us` of the render thread.
- [ ] `key_lut_dump 1`, then `adb pull` the file for offline tests.

**QR code markers and the box placed from them** (the tracker, the size gate and the box ran on the
headset, see "First run on the headset" and "Second run on the headset" below; the measured baseline of
the pose, the box kept or carried through a stage change (`marker_carry`), the hands limited by the box,
the changes of the size gate of 2026-11-24 and the outline of the anchor and the box are not tested: they
compile and pass the unit tests only. Carrying the box by the pose of a STAGE event was seen wrong on the
headset on 2026-11-24, see `marker_carry`).
Step one of way B: does the runtime give this
app QR code poses (`XR_EXT_spatial_entity` and `XR_EXT_spatial_marker_tracking`), how good are they, and
the box region placed from one. The manifest now declares `com.oculus.permission.USE_ANCHOR_API` and
`horizonos.permission.USE_ANCHOR_API`. The extensions are enabled at the start of the app if
`debug.alvr.marker` is not 0 (unset counts as on here) and the runtime lists `XR_EXT_future`,
`XR_EXT_spatial_entity` and `XR_EXT_spatial_marker_tracking` (all three or none; by name, so a missing
function turns the feature off instead of ending the app; if the runtime refuses them when the instance is
created, the app starts again without them). Without them, or with `marker 0` at the start, nothing
changes. Enabled extensions cost nothing by themselves: the tracker (QR detection and the snapshot calls)
runs only while `marker` is 1, or while it is unset and `marker_box` is 1.
`XR_EXT_spatial_anchor` and `XR_EXT_spatial_persistence` are only reported, not enabled.
Code: `extra_extensions/spatial_marker_ext.rs` (OpenXR), `marker_region.rs` (the box), geometry with tests
in `alvr_graphics` `marker_box.rs`.

Scope (the user, 2026-11-23, evening): no motion rigs, a code never moves once placed. The codes are
mounted on the rig (flat or standing) or shown on the monitor facing the driver (a standing code). A code
is used as it is and measured precisely; that measurement is the baseline, and its size may be changed
after the first measurement.

**First run on the headset (2026-11-23, build ce5ccfb9), measured** (log `alvr-20261123-2115-ce5ccfb9.log`,
Horizon OS v210). The runtime listed `XR_EXT_future`, `XR_EXT_spatial_entity`,
`XR_EXT_spatial_marker_tracking`, `XR_EXT_spatial_anchor` and `XR_EXT_spatial_persistence`, all yes. It
lists the capability `MARKER_TRACKING_QR_CODE` with the components `MARKER` and `BOUNDED_2D` and NO
features: no static markers and no fixed size, so `marker_static` and `marker_size` do nothing on Quest.
The spatial context was ready 136 ms after the request, the code was found 0.27 s later. The pose changes
about every 0.5 s; an update snapshot costs 0.1 ms. The printed code `RIG1` (144.2 mm), standing, was seen
from 0.65-1.0 m. When the runtime measures the size itself it reports 143.1-146.9 mm (median 145.6) and the
pose is good: standard deviation of the position 1.7 / 0.7 / 3.5 mm, heading 0.5 deg, over 52 summary lines
with the head moving. But for the SAME entity it often reports exactly 100.0 x 100.0 mm or exactly 50.0 x
50.0 mm (27 of 79 summary lines), also with the head still, sometimes for 30 s in a row, and then the pose
is wrong. With 100.0 mm the code is placed along the same line of sight but nearer (0.51 m instead of 0.69
m, scaled by about 100/145.6), 18 cm from the true place; with 50.0 mm about 1 m in front of the head,
facing it, up to 0.8 m from the true place. These are default sizes the runtime uses when it has not
measured the size. The tracking state is `tracking` in both cases, and it flips between tracking and paused
often. So the reported size tells good poses from bad ones; without a check a bad pose that lasts more than
`marker_move_ms` (1 s) would move the box by 18 cm. Hence the size gate below (not tested).

**A code on the monitor, same run, measured.** `tools/show_qr.py SCR1 --monitor 1 --mm 150` on a 21.5 inch
1920x1080 monitor: 21 modules of 29 px, 609 px, 151.3 mm by the monitor's own size (0.2484 mm per px); the
user measured 151 mm with a ruler. The headset found it at once (`new "SCR1"`) and reports 151.6 mm
(median; 146.2 mm while the code was shown at 146.1 mm for a moment, so it follows a change of the size).
Over 46 summary lines with a measured size, the head 0.48-0.88 m away and moving: standard deviation of the
position 1.3 / 0.4 / 2.4 mm, heading 2.2 deg (195-208 deg, the code was resized during that time: not
separated); the printed `RIG1` in the same two minutes: 1.4 / 0.9 / 2.4 mm and 0.5 deg. Default sizes: 1 of
47 lines for the monitor code, 2 of 33 for the print. So a code shown on the monitor works as an anchor;
its heading is the weaker part (2 deg move the far end of a 1 m box by 3.5 cm). White at full brightness,
room light on; a dimmer white, a smaller code and the game running on the same monitor are not tested.

**Second run on the headset (2026-11-23, build 34a0ea96), measured / observed** (log
`alvr-20261123-2136-34a0ea96.log`, the lines with `marker`). The size gate worked: the baseline of `SCR1`
on the monitor was learned as 150.9 mm, the user measured 151 mm with a ruler; the code dragged to a
laptop screen with smaller pixels was 98.3 mm by the screen's data, and the headset reported 98.3 mm
(measured). The box placed from `SCR1` works, and stays fixed to the room after the code is hidden
(observed by the user). The code is shown only for the measurement and then hidden (the user's choice),
so everything must work with the code out of view. While the code was measured the box moved by 1-4 cm
several times (`marker box: moved by ...`): the heading of the code read mostly 204-206 deg, single poses
197.8 to 213.8 deg; the pose refreshes about twice a second, so the 1 s average of that build held 2
distinct poses; `N outliers left out` grew to 48 in 30 s (measured). The user recentered:
`ReferenceSpaceChangePending type: STAGE, ... pose valid: true, pose in previous space (0.000, -0.003,
0.000) m, yaw -0.0 deg`, with LOCAL and LOCAL_FLOOR changed by a yaw of 237.9 deg at the same moment. The
box was dropped (`marker box: stage space changed, waiting for the marker`) and came back only because
the code was still on screen. At the start of the session there was a STAGE change with the pose
(0.286, -0.166, -0.523) m, yaw -76.7 deg (measured). The user: "while my arm disappears when outside the
playzone, my hands don't" (observed; the keyed hands ignored the region, see below). VrApi showed GPU
95 % and 65 C at 72 fps with the box (measured; the cause was not looked into).
What changed after it (2026-11-24, not tested: everything in this paragraph compiles and passes the unit
tests only): the pose is a measured baseline (a robust estimate over up to 60 s, frozen once measured)
instead of a 1 s average (a move away from a measured pose needs 5 poses over 3 s, and the same code
found again under a new entity id keeps it); the box is carried through a STAGE change with a valid pose
(since the afternoon of 2026-11-24 only with `marker_carry 1`, the default keeps its coordinates); the keyed hands
show only inside a marker box (`marker_hands`); a given size below 10 mm is malformed, the tolerance of
the size gate is logged and shown in the summary, and a resized code is learned again after 6 sizes.

**Size gate** (it worked in the second run, measured; the resize rule is not tested). Each code has a
baseline size: the one given with `marker_sizes`, else the learned one, the median width of the last 50
distinct poses whose size is not a default size (valid from 3 such poses). A resized code: after each new
distinct size that is not a default size, if the last 6 agree with each other (all within
`marker_size_tol` of their median) and all lie outside the band of the learned size, the history starts
again from those 6, so the learned size follows after 6 distinct poses (about 3 s) instead of 26 (about
13 s); with `marker_size_tol 0` this never happens. A size is a default size when width and height are both within 0.05 mm of the same whole
multiple of 50 mm. A pose is trusted when its width is within `marker_size_tol` (3 %) of the baseline; a
default size is never trusted, unless the given size is itself a multiple of 50 mm. Until a code has a
baseline none of its poses is trusted. Only trusted poses place the box, and the statistics of the summary
lines (pose changes, scatter, heading scatter) take the trusted poses only; the description of a code
shows the pose as last reported, with ` (size not trusted)` when it is not trusted.

| Property (`debug.alvr.`) | Default | Meaning |
|---|---|---|
| `marker` | unset | 1: look for QR codes while the session runs. Unset: only while `marker_box` is 1 (so a normal race pays nothing). 0: the tracker is stopped and a placed box is forgotten. The extensions are chosen at the start of the app: 0 at the start needs a restart for 1 to work. To restart the tracker (to apply `marker_static` / `marker_size`, or after a line starting `marker: off, ...`): `setprop debug.alvr.marker 0`, wait a second, then `1` |
| `marker_discovery` | 2 s | A discovery snapshot (finds new codes) this often without the runtime's discovery-recommended event; also right after the context is ready, after an event and after a stage change, but then at most every 250 ms |
| `marker_rate` | 10 Hz | Update snapshots (fresh poses of the known codes, stage space, at the display time of the frame). 0: none |
| `marker_static` | 0 | 1: tell the runtime the codes don't move, only if it lists that feature (Quest doesn't, measured; read when the tracker starts, see `marker`) |
| `marker_size` | unset (0) | Does nothing on Quest: it lists no fixed size feature (measured); for the baseline size use `marker_sizes`. Side length in mm, decimals allowed (e.g. `95.6`), 0 to 2000: black edge to black edge, without the white margin. The runtime takes EVERY code as this size, so set it only while codes of one size are in view; with it set the size in the `new ...` line is this value, not a measurement. Only if the runtime lists that feature. Read when the tracker starts (see `marker`): check the `creating the spatial context (...)` line. A value that is not a number is logged and ignored |
| `marker_sizes` | unset | The baseline size of the codes (see "Size gate"), mm with decimals, black edge to black edge: `144.2` for every code, or payload:mm pairs separated by commas, `RIG1:144.2,SCR1:150` (payloads case sensitive; a number without payload is the size of the other codes; at most 91 characters, an Android limit). 10 to 2000 mm: a smaller size is malformed (`144,2` gives 144 mm and the malformed entry `2`). Unset: learned from the sizes the runtime reports. A malformed entry is logged once and ignored. Polled, applies at once |
| `marker_size_tol` | 3 % | A pose is trusted when its width is within this of the baseline, 0 to 50. 0 turns the size gate off: every pose is trusted, as before. Polled, applies to the next pose; a change is logged |
| `marker_box` | 0 | 1: while a code is known, the depth mask uses a box placed from it instead of the region of the settings (also instead of "Distance") |
| `marker_id` | unset | Use the code with this payload (text, case sensitive: `RIG1`). A change is logged and the box is forgotten at once. Unset: the box stays with codes that carry the payload of the code used first, until `marker_box 0`, `marker 0`, or `marker_id` changes |
| `marker_window` | 60000 ms | Time window of the distinct poses of the estimate of the pose of the code, 100 to 600000 (at most 240 poses, about 2 minutes at 2 Hz). Was 1000 ms: a value below 8000 left over from before (setprop lasts until a reboot) means the pose is never measured, the log line says so. A change is logged and applies at once (a shorter window drops older poses at once; a measured estimate stays) |
| `marker_move_ms` | 1000 ms | Poses away from the estimate must agree for this long (and be at least 3) to be a move, 0 to 60000. Against a measured estimate a move needs at least 5 poses over at least 3 s (or `marker_move_ms` if longer). Longer: fewer false moves from a pose error that lasts a moment (a standing code's heading), a real move is followed later. A move also ends a measurement and starts a new one. A change is logged and applies at once |
| `marker_scan_poses` | 20 | Distinct poses that measure the pose of the code, 3 to 240; they must also span at least 8 s. A change is logged and applies to the next pose |
| `marker_scan` | unset | Another positive value measures the pose again from the next pose the runtime reports after the request (the box stays where it is until then; with the code hidden, until the code is seen again). The value at the start of the app is not a request (like `playzone`) |
| `marker_hands` | 1 | 1: with the color key and a marker box, the keyed hands show only inside the box, faded over 2 cm on each side of its surface. 0: everywhere, as before. The region of the settings (Distance, Cockpit volume) never limits the hands. A change is logged and applies at once |
| `marker_carry` | 0 | What a STAGE `ReferenceSpaceChangePending` with a valid pose does to the box, the poses of its estimate, the saved anchor and the frozen editor pose. 0: their coordinates are kept, the event and its pose are logged (on 2026-11-24 the code was seen at the same STAGE coordinates after such an event, the carried box was 83 cm off, see the pose paragraph below). 1: carried into the new space by the pose, as before. An event without a valid pose drops them either way. Read when the event comes. Not tested on the headset |
| `marker_box_x` / `_y` / `_z` | 0 / 150 / 0 mm | Center of the box from the code: to the right of the heading, up, along the heading. A flat code with its top away from you: +x is your right, +z away from you. A standing code facing you: +x is your LEFT, +z comes TOWARDS you (moving a code from flat to standing needs new values; the defaults fit a flat code, for a standing one start from z 400, y -150, see Testing) |
| `marker_box_w` / `_h` / `_d` | 1200 / 900 / 1000 mm | Width (across the heading), height, depth (along the heading) |
| `marker_box_yaw` | 0 deg | Added to the heading, counter-clockwise from above; turns the box about the code |
| `area_show` | unset | Outline of the code and the box over the stream (see "Outline of the anchor and the box"). Unset: for 8 s each time the pose of the code becomes measured, each time the box is carried over a STAGE change (`marker_carry 1` only) and each time a box is placed after there was none. 1 (any number but 0): always while there is a box. 0: never. Only with a marker box and Passthrough = Depth. Polled, applies at once |
| `area_line` | 12 | Width of the lines of the outline on screen, hundredths of a degree (12 = 0.12 deg, about 2 px at the center of a view about 2000 px wide), 2 to 100. Polled |
| `area_test` | unset | A test layout of the play area (see "Play area: boxes and domes") made from the box of the `marker_box_*` properties. 1: that box with a dome on its top (25 cm) and a second box on its +x side (40 cm beyond it, half as high, same floor). 8: that box cut into 2 x 2 x 2 boxes with domes on their 24 outer sides (15 cm), the worst case for the GPU. Unset, 0 or another number: the one box, as before. A change is logged. Polled |
| `area_dashed` | unset | 1: the play area in the outline is dashed (4 cm period, 60 % drawn, fixed in the room). It is dashed anyway while it is placed from the anchor saved in the layout file, before the code is seen in this run (the editor). Polled |
| `area_edit` | value at app start | Play area editor (see "Play area editor"). A change to 1 starts editing, a change to 0 ends it (and saves). The value at the start of the app is not a request: set 0, then 1. Only with a marker box, a play area placed and Passthrough = Depth (else logged: `area editor: can't edit`). Polled |
| `area_pinch_on` / `area_pinch_off` | 0 / 5 | Editor: gap in mm between the surfaces of the thumb and index tips (joint distance minus the two joint radii) below which a pinch closes (2 updates in a row) / above which it opens (for 80 ms). On -30 to 100 (negative: tips pressed together; contact measured -2 to -16 mm on 2026-11-24), off from on to 105 (may equal it). The editor logs the min/max gap every 5 s while editing. Polled |
| `area_edit_dim` | 10 | Editor: how much the game is dimmed while editing, percent (0 to 90). 60 until the room showed through the game (`area_edit_see`): 0.55 x 0.7 leaves the game at about the 0.4 the handles read well over. Polled |
| `area_edit_see` | 25 | Editor: how much the real room shows through the game outside the play area while editing, percent (0 to 100; 0: the game opaque there, as before). Inside the area the room shows fully as always; the lines of the editor stay opaque. Polled |
| `area_edit_grace` | 8 | Editor: how long editing waits for a stopped stream (Passthrough = Depth) to come back before it ends, seconds (0 to 60). Polled |
| `area_edit_hands_pc` | unset (0) | Editor: 1: the hands are sent to the PC also while editing; else no skeletons and no hand poses while editing. Polled |
| `area_file` | unset (= 1) | 0: the layout files of the play area are neither read nor written: the box of the `marker_box_*` properties (and `area_test`) again, edits last until the app stops. Unset or another value: a layout file of the code replaces that box. A change reads the file again. Polled |

The box frame is level (stage +Y up); only the heading comes from the code, never its tilt (a tilt error of
a few degrees would move the far end of the box by centimeters). Heading: a code lying flat (normal within
45 degrees of vertical) points to its top (the reading direction: the side with the two upper finder
squares); a standing code points out of its printed face. Do not mount a code with its normal between
about 35 and 55 degrees from vertical (read the angle in the `new ...` line): at 45 degrees the heading
switches between the top of the code and its normal, which are opposite for a code whose top points up
the slope, so the box turns by 180 degrees and x / z are mirrored. (There is no hysteresis; it is a
possible change after the first measurements.)

The pose is a measured baseline (not tested; it replaced the 1 s average after the second run). The code
never moves, so the estimate takes the distinct poses of the last `marker_window` (60 s, at most 240): the
median of the positions per axis, and for the heading the circular mean of the headings within 1.5
degrees of their circular median (the plain circular mean with fewer than 5 poses). A pose that is
repeated is counted once. Once `marker_scan_poses` (20) distinct poses spanning at least 8 s are in the
estimate, it is measured: logged once (`marker box: measured: ...`) and frozen, later poses near it are
counted but no longer change it; the code can then be hidden. Before that the box follows the running
estimate, and `moved by` is logged at most once a second. A single pose away from the estimate (more than
2 cm or 3 degrees) is left out and counted as an outlier; 3 or more poses in a row away from the estimate
that agree with each other and span `marker_move_ms` (1 s) are a move: a new measurement starts from them;
a pose away from the estimate that the runtime reports unchanged without a gap for 2 s is also taken as a
move (a gap: no pose for more than 0.5 s, e.g. the code was out of view; with `marker_rate` below 2 Hz or
poses from the discoveries only this rule never applies). Against a measured estimate a move needs more
(the code never moves once placed): at least 5 such poses spanning at least 3 s (or `marker_move_ms` if
longer), and the estimate of those poses must itself be more than 2 cm or 3 degrees away; a pose held
unchanged is no move then. With a heading scatter of 2.2 degrees (the code on the monitor) three poses in
a row more than 3 degrees off on one side come about once in 1000 poses, five about once in 100000.
`marker_scan` measures again on request. The
pose is kept while the code is not seen, and also while there is no tracker (it failed, or the session
stopped and started again); the same code found again under a new entity id (`lost #<id>` and a new
`new ...` line, or a new tracker) keeps it too, a measured pose included: the move rule decides whether
its poses differ. Only `marker_box 0`, `marker 0` or a new `marker_id` forget it. A change of
the STAGE space (`ReferenceSpaceChangePending` for STAGE; events for LOCAL or VIEW are ignored here) with
a valid pose now KEEPS the coordinates of the box (`debug.alvr.marker_carry` 0, the default since
2026-11-24, not tested on the headset yet): the kept pose, the poses of the estimate, the anchor of the
layout file and the pose frozen by the editor stay as they are, and at the change time the event is
logged with the pose it reported (`marker box: stage space changed, coordinates kept (marker_carry 0; the
reported pose (x, y, z) m, yaw d deg is not applied)`, and `area editor: stage space changed, coordinates
of the saved anchor and the frozen pose kept (...)` while the editor has one, at most once a minute with
the number left out since). The reason, measured on
2026-11-24 (log `alvr-20261124-0839-bd0dc0f7.log` around 08:46:01): a STAGE event reported the pose
(-0.130, 0.007, 0.167) m, yaw 39.4 deg in the previous space; the box was carried by it (`box carried over
(moved by (0.256, -0.007, -0.144) m, yaw -39.4 deg ...)`), but the code measured 4 s later sat at the SAME
STAGE coordinates as before the event (and as the evening before, across sleep and reinstall), so the box
had to jump back 83 cm. Ordinary recenters send only LOCAL / LOCAL_FLOOR events and are not affected.
`marker_carry 1` (read when the event comes) is the old behaviour: the kept pose and the poses of the
estimate become p_new = inverse(pose_in_previous_space) * p_old, the heading changes by minus the yaw of
that pose, a measured estimate stays measured (`marker box: stage space changed, box carried over (moved
by ...; marker_carry 1)`); the saved anchor and the frozen pose are carried the same way. Each STAGE
event with a valid pose also logs `STAGE change with a valid pose: the box keeps its coordinates
(marker_carry 0)` (or `is carried by it (marker_carry 1)`) right after the `ReferenceSpaceChangePending`
line. If the pose in the new space disagrees, five poses there over 3 s are a move and measure again.
Without a valid pose the box is kept up to the change time and dropped from then until the code is seen
again, as before, whatever `marker_carry` says. The tracker itself still drops its poses at a STAGE change
until the codes are seen again (`marker: stage space changed, ...`); the box does not need them. A kept
box is not counted as carried, so the outline is not shown for it (`area_show` unset). The box applies
with Passthrough = Depth only, and live; the settings stay as they are.

With the color key on, the keyed hand zones (the capsules around the hand joints, drawn by
`focus_area.wgsl`) did not use the region (`stream.wgsl`: `max(mask, hand.g)` and `focus`): keyed hands
showed everywhere, while forearms, wheel and body show only inside the box (second run, observed). Now
(not tested), with a marker box and `marker_hands` 1, the CPU limits the hands before they go to the GPU
(`limit_hand_to_region` in `depth_mask.rs`): each capsule end gets a weight from its distance to the
surface of the box (1 inside, 0 outside, smooth from 2 cm inside to 2 cm outside, like the region of the
depth mask), and its radius goes from the joint radius (weight 1) down to minus the reach of the zones
(weight 0: neither the rim nor the solid fill shows). The radius is linear along a capsule, so a capsule
that crosses the surface tapers off; one that reaches past 2 cm outside is cut there. A hand half inside
the box: the part inside looks as before, at the surface the fingers or the palm get thinner over a few
centimeters (at the surface about half as wide) and vanish about 1-2 cm outside; the part outside shows
the game, like the forearm. A long capsule (palm, up to about 6 cm) tapers over its whole length inside
the box when it crosses the surface, so a palm at the surface may look thinner a few cm early. The
bridge from the wrist (`key_wrist_len`) is limited the same way (its end radius is now its own uniform,
and a flag per hand says whether it has a bridge, so that a fading bridge with negative radii still shows
its rim, like the capsules of the hand).
No new pass, no new texture, no per-pixel cost (only the capsule ends change, about 120 points per frame
on the CPU). The region of the settings never limits the hands. A hand
without its forearm means the box ends there, not a wrong heading. The default box is centred on the
code: 0.5 m either way along the heading, 0.6 m to each side, 0.30 m below to 0.60 m above. For a code
standing on a monitor that is 0.5 m behind the screen and only 0.5 m towards the driver, with the monitor
itself inside. Example for a monitor code: `marker_box_z 600`, `marker_box_d 1100` (5 cm in front of the
screen to 1.15 m towards the driver), `marker_box_y -100`, `marker_box_h 1400` (0.8 m below to 0.6 m above
the code, lap included). Not tried.
The debug view "Tint mask" paints the box green only without the color key (`color_key 0`); with the key
loaded the box is plain passthrough.

**Outline of the anchor and the box (not tested).** After the code is scanned the headset shows where
the anchor and the play area are: thin opaque lines drawn into the game image, over the stream and over
the passthrough hole alike (`area_show`, `area_line`). Drawn in stage space: the outline of the code, a
square of the size the runtime reports, at the estimated position, turned to the estimated heading with
the tilt of the last pose used (amber while the pose is being measured, green once it is measured and
frozen); at its centre the axes of the box frame: the heading the box uses (with `marker_box_yaw`, 20 cm,
blue), up (10 cm, magenta) and to the right of the heading (10 cm, red); and the 12 edges of the box the
mask uses (white). A box kept without a tracker or carried over a stage change is drawn the same way.
How: one more pipeline in the existing main pass of each eye, right after the stream draw (no new pass,
swapchain or composition layer). The segments of the frame (19) go into one 12 KB uniform buffer, each
clipped on the CPU against the near plane of the eye (0.1 m + 5 mm; edges that pass behind the head are
cut there, segments wholly behind are dropped), and are drawn with one instanced draw per eye as quads
facing the screen, a constant width in pixels. Alpha 1 without blending: in the premultiplied depth mode
that is opaque, and with the color key alpha 1 is the packed value for "both game layers opaque" (lower
2a, upper 2a - 1, also after the rewrite outside the lower rect), so a line shows over game, passthrough
and keyed hands; the colors get the sRGB correction of the stream. The camera is the one of the depth
mask: the eye position located at display time (the one `depth_uniforms` gets) with the orientation and
fov of the view the stream is drawn into (on Quest the PC's view, as the mask), so the white lines lie on
the border of the hole (an outline from the PC's eye position would be offset from it by the prediction
error and swim with head motion). Nothing is uploaded or drawn while it is hidden (the CPU checks a few
flags per frame). The GPU test `overlay_draws_the_box_outline_over_the_stream` draws a box over the
stream on the PC: line color along an edge, every pixel away from the lines unchanged. The test
`overlay_camera_matches_the_rays_of_the_mask` checks the camera: a point on the ray of a stream pixel
(from the eye position of the depth input, in the direction `depth_uniforms` uses) lands on the place
where `reprojection_transform` draws that pixel, also with input and output views that differ in
orientation, position and fov.
Since the play area (2026-11-24) the outline draws the whole area instead of the 12 edges: see "Play
area: boxes and domes". The buffer now holds 256 segments per eye (16 KB, 32 bytes a segment: the color
packed into one float, a negative width for a dashed line).
Test on the headset (box on, Passthrough = Depth, `setprop debug.alvr.area_show 1` or wait for
`marker box: measured:`): the square sits on the real code and is turned like it (the heading line
points to the top of a flat code, out of the face of a standing one); the white outline runs along the
border where the real world ends and the game begins; with the head moved sideways by 10 cm the lines
stay glued to the desk and do not swim; both eyes fuse the lines into one (no double image); the frame
rate and the GPU load (VrApi) with and without `area_show 1`. If the lines are too thin or too thick:
`area_line`.

**Play area: boxes and domes (2026-11-24, not tested on the headset).** The region of a marker is now a
layout of up to 8 boxes (`alvr/graphics/src/play_area.rs`, pure geometry with unit tests). Area frame:
the level frame of the code that `marker_box` builds (+x to the right of the heading, +y up, -z along
the heading), turned by the yaw of the layout (`marker_box_yaw` for the box of the properties). Every box
is level; since the second build of 2026-11-24 each box has its own yaw (`AreaBox::yaw`, about the
vertical axis through its center, counter-clockwise seen from above, 0: axis aligned): its frame is the
area frame moved to its center and turned by it, and its half extents, sides, domes, handles and drags
are those of that frame (`AreaBox::to_local` / `to_area` / `side_normal`). Each of the 6 sides can bulge out as a
dome of height h (0: flat): the part beyond the side of a capsule that cuts the side along the largest
stadium inside it (a circle on a square side: a spherical cap). Its rim radius b is half the shorter edge
of the side, the capsule axis runs along the longer edge (half length a - b), radius R = (b^2 + h^2) / 2h,
axis d = R - h below the side; h is at most b (half ball or half cylinder), below 5 mm the side is flat.
The corners of a side stay flat. Without `area_test` the layout is the one box of the properties: the
same box as before (tested: `single_box_matches_the_marker_box` compares the old box distance and the
new one on random placements, within 2e-5 m).
The mask: `region_mask` in depth_mask.wgsl takes the signed distance of the union (`area_signed_distance`,
a line-by-line copy of `PlayArea::signed_distance`) with the same feather as before. It is exact outside
(the feather keeps its meaning in meters) and has the right sign inside (under a dome never deeper than
the opposite side of the box, so a box thinner than the capsule of its dome has no jump at its middle;
`thin_box_has_no_jump_under_a_dome`). Cost: a bounding box test of
the whole area first, then a loop over the used boxes only, which stops once a point is inside by the
feather; a box evaluates its domes only between -feather and its tallest dome + feather (outside it
returns the box distance minus the tallest dome, a lower bound). And the 9 samples of a mask texel lie
on one ray of the depth camera, each at its own depth: the region is evaluated once at the middle depth,
and only if that point is within feather + spread of the surface are the 9 classified one by one (the
spread is half the depth range along the ray). Exact: the mask is the same, but a flat surface costs one
evaluation instead of 9 (also for the old single box and the Distance region). Not measured on the
headset: compare App GPU time with `area_test` unset, 1 and 8 on a fixed scene, several runs each.
Hands limited to the area (`marker_hands`) use the same distance on the CPU (`limit_hand_to_region`).
Boxes that only touch have a distance of 0 on the side they share: the mask shows the real room there
(coverage 0.5, full from 0.35 on), but hands would be thinned along it, so a box added next to another
reaches 5 cm into it (`ADDED_BOX_OVERLAP`).
Uniform block (`DepthUniforms`, Rust `depth_uniforms`, asserted 4144 bytes; 4016 before the box yaw):
`world_to_volume` became
`world_to_area` (same place: stage to the area frame, or to the box of the settings), `volume_half_extents`
became `area` (x: number of boxes, 0 with the Distance region); at the end after `hand_bridge_end` come
`area_bounds` (2 vectors: center and half extents of all boxes with domes) and `area_boxes`, 8 x
`AreaBox` of 7 vectors (center + tallest dome, half extents, R of the + and - sides, d of the + and -
sides, `turn`: cosine and sine of the yaw of the box). `area_box_distance` turns the point back into the
frame of the box (the same formula as `turn_y` in play_area.rs) and goes on as before; the bounds of a
turned box are those of its turned corners (they hold it). The GPU test
`play_area_mask_matches_the_rust_distance` has a box turned by 35 deg whose cut by the wall lies 10 cm
beside the unturned one (checked: with the sign of the sine flipped in the shader the test fails). The
box of the settings ("Cockpit volume") is sent as one box centered in its frame.
The outline (`area_outline` in overlay.rs): the edges of the boxes without the parts strictly inside
another box (any yaw: each edge is cut where it enters and leaves the other box in that box's frame;
across a face it runs along it must be inside by 1 mm), and of each dome its rim and two arcs over its top
(across and along the side, drawn in the frame of the box), 8 segments
per half circle, fewer (down to 2) when the layout would not fit in the 249 segments left per eye; the
edges come first, the rest is cut and logged. Dumps (`key_dump`) write the first box as before (its
region turned by its yaw) plus `area_to_world` and `boxes` (center, half, `yaw_deg`, the 6 dome heights
as used).
The layout as text (`PlayArea::to_text` / `PlayArea::parse`), saved and read by the editor (see "Play
area editor", which adds an `anchor` line):
```
alvr_play_area 1
# comments
payload RIG1
yaw_deg 0.00
box cx cy cz hx hy hz [dome+x dome-x dome+y dome-y dome+z dome-z]
box_yaw_deg 30.00
```
meters in the area frame, half extents, one box line per box (6 or 12 numbers). `box_yaw_deg` (degrees)
turns the box of the line before it and is written only for a turned box. The version stays 1, readable
both ways: a file without the line reads unturned, and a build from before it reads a new file with
the line left out as unknown (logged as `unknown, left out`), the boxes unturned. A `box_yaw_deg` without
a box before it is left out with a warning, one that is not a number is an error, the angle is wrapped
to -180..180. Unknown lines are kept
aside, values out of the limits are clamped (with a warning), boxes after the 8th left out; a wrong
first line, another version, a box line that does not read or no box is an error.

**Play area editor (2026-11-24, not tested on the headset).** `alvr/client_openxr/src/area_editor.rs`
(state, files, logs) on top of `alvr/graphics/src/area_edit.rs` (pure: handles, picking, drags, pinch,
gesture, glyphs, undo, file name and text; unit tests). Only with a marker box and Passthrough = Depth.
- Modes: hidden (racing; only the gesture is watched, the hands are located every frame at the display
  time, in STAGE), shown (the outline for 3 s after editing), editing (game dimmed by `area_edit_dim`,
  handles drawn, the mask follows every drag at once, the pose of the code frozen and, over STAGE
  changes at their change time, kept or carried (`marker_carry`) like the box of the marker region; the
  hands are not limited to
  the area while editing, since the handles are on and outside it).
- Start and end: the two-hand gesture, both hands pinched with both palms turned away from the eyes
  (cos > 0.5 between the normal out of the palm, -Y of the palm joint, and eyes-to-palm), the two
  pinch points within 20 cm of each other, each 15-65 cm from the eyes and within 45 deg of the view
  direction, held 1.5 s (drops under 0.15 s ignored; after it fired the pose must be let go). The outline
  brightens and an amber square closes between the hands. Driving guard: hands on a wheel are about 30
  cm apart, palms towards the rim and not pinched; the gesture is logged each time it is held or let go.
  Also: `area_edit` 1/0, the done handle (green check mark 12 cm above the code), 120 s without a grab
  (counted from the end of the last grab; pinches not on a handle don't count), the area gone (also
  `marker_box 0`). Ending saves only if something was changed: entering and leaving without a change
  writes no file, and without a file the box of the properties stays the layout.
- Hands: a hand is used only while its thumb and index tips are POSITION_TRACKED and its cursor (the
  midpoint of the two tips) is within 50 deg of the view direction; the hand that holds a grab within 75
  deg (an arm reaching for a far side leaves the middle of the view; the Quest 3 sees about 110 deg).
  Pinch on the gap between the tip surfaces with hysteresis (`area_pinch_on/_off`, 0 / 5 mm since the
  first headset run); a closed pinch of a hand that is lost opens after 0.3 s, of the hand that grabs
  after 1 s. A frame without the eyes (head pose not valid) counts as both hands not seen, at the last
  view (up to 1.5 s), so a grab then ends like a lost hand (the last displacement kept). The release
  line says why the hand was lost (`tips not tracked`, `<n> deg from the view direction`, `no pose`).
- Handles of the selected box: a square on each side (on top of its dome; drag along the normal: that
  side moves, the opposite stays), a small triangle beside it (up the side, along the longer edge on
  top and bottom; 35 % of the half extent, 4-12 cm) for the dome (drag out: higher, up to the rim; below
  1.5 cm: flat), a diamond at the center (moves the box), a curved arrow beside the center (turns the
  box, see "Turning") and a red "x" 9 cm above the center ("+" beside it until the third build; a new
  box is now a two-hand pinch, see "Play area editor (v1)"). The
  other boxes show only a dim diamond; the first pinch on a handle of another box only selects it (cyan
  edges; until the third build that pinch also dragged it). Undo (an
  arrow) beside done above the code while there is something to take back (20 steps, per editing
  session). Handle size max(1.5 cm, 0.012 x distance), 1.3x and amber when lit, cyan when grabbed, with a
  dashed guide line along the normal for side and dome drags. Cursors: a small plus, a square while
  pinched.
- Picking: the nearest handle within 8 cm of the cursor; handles up to 1.5 cm farther than the nearest
  go by the line of sight from the eyes through the cursor; none in reach: the handle within 4 deg of
  that line (ties within 1.5 deg: nearer to the eyes). The lit one stays against a 1 cm better one, up to
  10 cm away and 6 deg off. It is kept while the fingers close (gap below on + 10 mm).
- Drags: relative to the cursor at the pinch (no jump), 8 mm dead zone caught up over the next 3 cm.
  A side or dome drag goes along the normal of the side of the box as it is turned. On release the
  value of 0.12 s before the release was confirmed is kept (the fingers move the cursor while they
  open), the last one when the hand was lost. Buttons act on any release of a usable hand, however far
  it moved (until the first headset run they needed a tap under 2 cm: 14 of 15 presses were rejected,
  the hand moved 22-186 mm between closing and opening the fingers). In the first two builds "+" added
  a box on the side of the selected box the line of sight goes through first (from inside the box: where it leaves,
  from outside: where it enters), else the next side facing the eyes whose new box would not have its
  center in another box (in the frame of the box); `PlayArea::add_box` sizes it and gives it the yaw of
  the box it is added to. "x": held 0.8 s with the cursor within 8 cm of where the pinch closed removes
  the box (a red square closes around it), not on the last box (it was 2 cm: held pinches drifted 23-50
  mm on 2026-11-24, a hand pulled away on purpose 189 mm), only with the hand usable the whole time (the
  pinch of a lost hand stays closed for 1 s, longer than the hold; else the square goes and it takes a
  new pinch). Undo and done: a release.
- Turning (`HandleKind::Turn`, the curved arrow): beside the center at its height, towards the +x +z
  corner of the box, 60 % of the shorter horizontal half extent away from its vertical axis (6-15 cm).
  The box turns about its vertical axis by the angle the cursor sweeps around that axis (added up per
  frame, so more than half a turn works; a frame with the cursor nearer than 3 cm to the axis adds
  nothing, so a grab near the axis turns once the cursor is out and crossing the axis does not flip the
  box), relative to the pinch (no jump; nothing adds up within the 8 mm dead zone, the turn starts where
  the cursor leaves it), with detents at multiples of 15 deg within 2 deg.
  While grabbed: a dashed line from the center through the handle as it was, a line through it now
  (green on a detent) and the arc between them (up to 6 segments). Release: `release turn of box #i:
  yaw <deg> deg (turned by <deg> deg{, on a detent}), ...`. A box added next to a turned one is turned
  like it; `describe_box` adds `, yaw <deg> deg` for a turned box.
- One hand or two (2026-11-24, after the third build; `PinchRoles` in area_edit.rs): the user's rule is
  "pinch to resize is always with one hand. adding a new box or exiting edit mode is always with 2". A
  grab in progress is therefore never dropped by the other hand or by the gesture (until then a gesture
  pose while one hand grabbed dropped the grab and put the layout back; the leave gesture of the second
  headset run won over a pinch on done that way). See "Gestures", Pinch. Ends of a grab other than its
  pinch keep what it changed so far (as an undo step, saved): editing ends, the stream stops, the hands go
  (no input for 1.5 s), a STAGE change: `grab of <handle> of box #i ended (<reason>), kept as it was: ...`.
- Stream interruptions: without a stream with Passthrough = Depth editing pauses (`editing paused: ...`,
  a grab kept, pinches reset) and goes on when it is back within `area_edit_grace` (8 s; `editing goes on
  after <s> s without the stream`), counted from the last frame with the stream (so a paused session,
  which has no frames, counts too); else it ends (`editing ended (no stream with Passthrough = Depth for
  <s> s, ...)`, saved).
- The PC: while editing the client sends no hand skeletons and no pose of a hand tracked as a hand
  (`StreamContext::withhold_hands`; controllers are sent; `area_edit_hands_pc 1` sends them). The PC then
  sees both hands as not tracked, as when they are out of view (with `steamvr_input_2_0` the hand
  trackers disconnect; the last pose stays in the game). The streamer of 2026-11-24 has
  `hand_tracking_interaction` off, so pinches never became trigger or menu clicks there
  (`hand_gestures.rs` runs only with it on); this keeps them away also with it on.
- Dimming: `DepthRenderInput.game_dim` goes to `lighting2.w` of the uniform block (unused before: no
  layout change, the block stayed 4016 bytes; 4144 since the box yaw); `depth_passthrough` in stream.wgsl multiplies the game color
  by 1 - dim in both the plain and the color key path. Tested on the PC GPU (`editing_dims_the_game`).
  The room through the game (third build): `DepthRenderInput.game_see` goes to `area.y` (unused before,
  the block stays 4144 bytes). Plain path: the room shown is max(mask, see), so rgb = game x (1 - dim)
  x (1 - shown) and alpha = 1 - shown x brightness (premultiplied). Color key path: the packed alpha can
  hold only one layer part way while the other is 0 or 1, so the lower layer gets min(1 - solid, 1 -
  see) and the upper one opens (0); outside the rect of the lower layer the upper one alone shows 1 -
  see; inside it (around the hands) colors the key keeps show fully instead of at `see`. The overlay is
  drawn after the stream, opaque, so the outline and the handles stay fully visible. Tested on the PC
  GPU (`editing_shows_the_room_through_the_game`: both paths, the mask, the overlay).
- Layout file: one per code, `area-<payload, other than [A-Za-z0-9_-] as _, at most 40>-<FNV-1a of the
  payload, 8 hex>.txt` (`area-no-payload.txt` for a code without payload), in
  `/sdcard/Android/data/alvr.client.dev/files/areas/` and `/data/local/tmp/alvr-areas/`. Read once the
  payload is known (`marker_id`, else the code seen first; before any code is seen and without
  `marker_id`: the payload named in the newest `area-*.txt` of the two folders): the newer of the two
  that reads. Written to both by a saver thread (only the newest pending layout, temporary file then
  rename) on each released grab that changed something, add, remove, undo, end of editing after a
  change, and when the code gets measured while a file is used. The text of `PlayArea::to_text` plus `anchor x y z heading_deg`, the pose of the code
  measured last (stage space). With a file its layout replaces the box of the properties (and
  `area_test`); `area_file 0` goes back. The CI uninstall wipes the first folder; the second survives it.
  The app most likely can only read the second (SELinux gives apps no write access to /data/local/tmp,
  `chmod` does not change that; reading works, as for the LUT file): the failed write is logged once per
  run. `tools/install_apk.sh` copies `files/areas/area-*.txt` there with `adb shell` before it
  uninstalls (by hand: `adb pull` before, `adb push` to `/data/local/tmp/alvr-areas/` after).
- Restore: before the code is seen in this run the area is placed from the anchor of the file (STAGE
  coordinates stayed the same across sleep and reinstall on 2026-11-24; a STAGE change with a valid pose
  keeps its coordinates, or carries it at its change time with `marker_carry 1`; one without a valid pose
  drops it until the code is seen); once the code is seen, from the
  code. The outline is dashed while placed from the saved anchor (the code outline is amber while the
  pose is being measured). Restoring before the code is seen needs `marker_box 1`; without `marker_id`
  the file is the newest one (its payload), so after a reboot only `marker_box 1` must be set again.
- Not done: no snapping or minimum overlap of moved boxes (boxes that only touch thin a limited hand
  along the shared side); no turning of the whole layout in the headset (its yaw only in the file; each
  box turns on its own).

## Play area editor (v1)

Summary of the two paragraphs above ("Play area: boxes and domes", "Play area editor"), written for the
first headset test. Built 2026-11-24 (aafcbcd3, d98535d2), first run on the Quest the same morning (see
"First headset run"); the second build of that day (box yaw, buttons, pinch at contact, pauses over
stream interruptions, hands kept from the PC) ran on the Quest at noon (bcfefe28, "Second headset
run"); the third build (new box by a two-hand pinch, the room through the game, select first, hand
rays) is checked only on the PC (`cargo test -p alvr_graphics`, 211 tests incl. GPU and shader parse;
`cargo test` / `clippy -p alvr_client_openxr`, also `check` / `clippy` for `aarch64-linux-android`).

**Second headset run (2026-11-24, build bcfefe28, log `alvr-20261124-1235-bcfefe28.log`).** Two editing
sessions (12:47-12:50), both started and ended with the gesture; 19 grabs (center, turn, sides, domes),
17 releases, no hand lost, no stream pause. The leave gesture twice began with one hand on the done
handle (`grab done of box #0`, then `dropped (two-hand gesture)`): the gesture still won. The user:
"overall, it works well, but i have 3 things 1. adding a new box should be a gesture, not a button,
example pitch [pinch] and scale with your hands for initial position, then move around with the other
controls 2. the real world should still be visible (say at 40-50% opasity) when editing. 3. first click
on another box should just select it, not move it around. Optional: sometimes I want to extend an area
beyond where my hand can reach. I think the meta variant with a pointer is a good approach (the meta
pointer in XR space, their default one)". What the third build does for each:
1. New box: the "+" handle is gone. Both hands pinched on empty space and pulled more than 20 cm apart:
   a dashed ghost box spans the two pinch points (opposite corners, level, yaw 0 in the area frame, each
   side at least 15 cm) and follows both hands; when a hand lets go it becomes a real, selected box
   (undo removes it, saved). Within 20 cm the same two pinches may be the gesture that ends editing; once
   the points were farther apart in this pinch the gesture's timer is cancelled until a pinch opens.
   Points nearer than 10 cm on release, 8 boxes or a center over 3 m from the code along an axis of its
   frame: no box (logged; the
   ghost is dim / red). Details under "Gestures".
2. The room through the game: outside the play area the real world shows through the game at 45 %
   (`area_edit_see`), inside the area fully as before; the game dimmed by 30 % instead of 60 %
   (`area_edit_dim`); the lines of the editor stay opaque.
3. Select first: a pinch on a handle of a box that is not the selected one only selects it (`selected box
   #i ...: the next pinch edits it`): no drag, no undo step, nothing saved. In this run selecting box #2
   moved it by 3 mm (`release center of box #2: moved by 0.003 m`).
4. Far reach: a ray from each hand like the system pointer (the aim pose of XR_FB_hand_tracking_aim, now
   enabled; else from an estimated shoulder through the pinch point), drawn thin with a dot where it
   meets the handle it would take. Handles within 8 cm of the pinch point keep priority; otherwise the
   handle nearest to the ray within 5 deg is lit. A handle taken by the ray follows the ray, not the
   small motion of the hand.

**First headset run (2026-11-24, build d98535d2, log `alvr-20261124-1047-d98535d2.log`).** It worked end
to end: 10 times into editing with the two-hand gesture, 130 grabs, sides, domes and moves, 3 boxes
added and 1 removed, the file saved after each change (`/data/local/tmp/alvr-areas` could not be
written: `Permission denied`, logged once, as expected). The user: "other stuff is good enough for now -
it gets the job done". What the log showed:
- Buttons: 25 of 29 presses of "+", undo and done were rejected as `not a tap (moved 13-307 mm)`: the
  hand moves while the fingers close and open. Now any release counts for a button. The remove hold
  failed 5 times at 23-50 mm of drift with the 2 cm limit, now 8 cm.
- Pinch: with on 10 mm the pinch closed while the fingers were still 1-2 cm apart ("the tap registers
  while my fingers are still around 1-2cm away"); at contact the gap measured -2 to -16 mm. After live
  changes (2/15, 0/15, 0/5 mm) the user kept 0 / 5 mm: now the defaults; the properties may go negative
  and off may equal on.
- 14 drags ended `(hand lost)` (mostly the ±x sides and a dome reached 0.5 m to the side): the reason was
  not logged (out of the 50 deg cone or tips not tracked); now the hand that grabs may be 75 deg off the
  view and is kept 1 s, and the line says why.
- 4 times editing ended `(Passthrough is not Depth)`, once in the middle of a drag (11:05:15, the drag
  was dropped): each time the stream had stopped (`marker box: placed but not used`, then `hand removal
  ... at the start of the depth` of a new stream 2-12 s later). The streamer log
  (`session_log.txt` of the 241f051e streamer) has `Client disconnected ... (os error 10053/10054)` at the
  same moments (its clock is about 1.5 s ahead), no restart request and no settings change; the stream
  came back with Passthrough = Depth each time. Two kinds: at 11:04:13, 11:05:15, 11:08:28 and 11:17:51
  the client kept rendering (no gap in the log, frames went stale for 1-2 s): a network stall that
  tripped the 2 s keepalive of the connection; at 11:10:58, 11:11:45, 11:24:50 and 11:28:17 the log has a
  gap of 5-33 s before and the marker tracker was created again: the OpenXR session paused (headset
  taken off or asleep). Not the pinches: `hand_tracking_interaction` is off on the streamer, so
  `hand_gestures.rs` never turned them into clicks. Now editing pauses and goes on (`area_edit_grace`).
- STAGE changes with only a vertical part (+0.15 to +0.46 m, then back within 1.5 s) came 46 times in
  35 min, each logged by the editor; with `marker_carry 0` they are ignored. The editor now logs at most
  one such line a minute (with the number left out).

**What it is.** After the code is scanned, the play area (the part of the real room shown through the
game) is drawn in the headset as an outline together with the code. With a two-hand pinch the person
enters edit mode: the game is dimmed, handles appear on the boxes, and with a one-hand pinch the sides
are pulled, domes raised, boxes moved, added and removed. The mask, the hand limit and the outline follow
the hand at once. The layout is saved in a file per code and comes back after a restart.

**Shape model.** Up to 8 boxes, all level in the area frame (the level frame of the code, turned by the
yaw of the layout, which exists only in the file), each turned by its own yaw about its vertical axis
(the curved arrow handle; 0: axis-aligned). Each of the 6 sides can carry a
dome (a spherical cap on a square side) of a height; 0 or under 5 mm is flat, the maximum is half the
shorter edge of the side, the corners stay flat. Limits: half extent 2.5 cm to 2.5 m, center within
3 m. The mask is the signed distance of the union with the usual feather.

**Layout file.** Text, `PlayArea::to_text` plus the anchor line:
```
alvr_play_area 1
payload RIG1
yaw_deg 0.00
box cx cy cz hx hy hz [dome+x dome-x dome+y dome-y dome+z dome-z]
box_yaw_deg 30.00
anchor x y z heading_deg
```
Metres, area frame, half extents in the frame of the box; `box_yaw_deg` turns the box line before it
(only written for a turned box; older builds leave it out as unknown and read the box unturned, files
without it read unturned: the version stays 1). Name `area-<payload>-<8 hex>.txt` (`area-no-payload.txt` without a
payload). Folders: `/sdcard/Android/data/alvr.client.dev/files/areas/` (lost by every CI uninstall) and
`/data/local/tmp/alvr-areas/` (survives, but the app probably cannot write it; `tools/install_apk.sh`
copies the first into it before the uninstall). The newer file that reads wins. A file replaces the
`marker_box_*` box and `area_test`; with no file the layout is that one box, as before.

**Gestures (numbers from the code).**
- Start/end: both hands pinched, palms away from the eyes (cos at least 0.5), pinch points at most 20 cm
  apart, each 15-65 cm from the eyes and within 45 deg of the view, held 1.5 s (drops under 0.15 s ignored).
  While editing only with both pinches free (the two-hand pinch, see Pinch): a pinch on a handle, or one
  that does nothing, never ends editing. Also `area_edit` 1/0, the done handle, 120 s without a grab, loss
  of the play area.
- Pinch: closes below a tip gap of 0 mm (2 updates; the tips pressed together), opens above 5 mm for
  80 ms; a lost hand opens after 0.3 s, the hand that grabs (and both hands while they pull a new box
  open) after 1 s. A hand is used when both tips are tracked and the cursor is within 50 deg of the view
  (75 deg for the hand that grabs and for both hands pulling a new box open: at 50 deg a box wider than
  about 0.7 m took a hand out of view and was made 0.3 s later).
  One hand or two (`PinchRoles`, `PinchStart`), because the user said "pinch to resize is always with one
  hand. adding a new box or exiting edit mode is always with 2": a pinch works handles (grab, select,
  undo, done, the hold of "x") only while the other hand is open; alone on empty space it is free and
  does nothing (a single pinch never adds a box or ends editing). Whenever both hands are pinched it is
  the two-hand pinch (a new box or the gesture that ends editing) or nothing, wherever the pinches closed
  (empty space, a near handle, a far handle by the ray, a button, another box). Two hands never close in
  the same update, so when a hand closes while the other is pinched: (a) the other pinch free: both are
  the two-hand pinch, the new one free whatever it closed on (no grab, no selection, no button); (b) the
  other pinch on a handle, near or by the ray, for at most 0.5 s (`GRAB_YIELD_S`, was `FAR_GRAB_YIELD_S`
  for the ray only) and its grab not out of its dead zone nor done (the box not removed): it gives way
  (the grab dropped, no change; a button does nothing on release; a selection made stays) and both are
  the two-hand pinch; (c) else (a drag older than 0.5 s or already moving, a button or selection held
  longer, a removed box, or the other pinch doing nothing itself) the new pinch does nothing until it
  opens: no grab, no selection, no button, not free, and the grab of the other hand goes on undisturbed
  (no handle lit, no ray drawn for that hand). Both closing in the same update count as (a) or (b). The
  pinches closed when editing begins (the gesture that started it) do nothing until they open: no box,
  editing does not end again.
- New box (third build; `TwoHandPinch`, `new_box_between`, `new_box_refusal` in area_edit.rs): the
  two-hand pinch, both pinches free (see Pinch: also a second pinch on a handle, and a first one on a
  handle that gave way; not a pinch closed before editing began). While the
  pinch points have stayed within 20 cm (`GESTURE_MAX_SPAN`, the span of the gesture) it is undecided:
  the gesture that ends editing may run (amber square). Once they were more than 20 cm apart in this
  pinch it is a new box: the gesture's timer is cancelled at once (its square goes) and does not run
  again until a pinch opens, also if the hands come back together with the palms away. The ghost: dashed
  box edges, the two pinch points at opposite corners, level and unturned in the area frame (the frame of
  the code, mounted square on the rig; the yaw of the selected box was not taken because which box is
  selected is mostly whichever was edited last), each side at least 15 cm (`NEW_BOX_MIN_SIZE`; a flat
  pull gives a 15 cm thick box), green, dim while the points are nearer than 10 cm (`NEW_BOX_MIN_SPAN`),
  red without room (8 boxes, or its center more than 3 m from the code along an axis of its frame). Made
  when a hand lets go, from the points of 0.12 s before the release (the last ones when a hand was lost):
  selected, an undo step, saved. Dropped without a box when editing pauses or ends. Pinched at two
  corners more than 20 cm apart: a new box at once. Hands held up for a box often have a handle at a
  pinch point or under a ray: the first pinch on a handle gives way within 0.5 s (see Pinch, (b)).
- Select first: a pinch on any handle of a box other than the selected one selects it and does
  nothing else in that pinch (`pinch_action`); the next pinch acts. Undo and done are always taken.
- Picking: nearest handle within 8 cm of the pinch point (ties within 1.5 cm by the line of sight);
  out of reach the handle nearest to the ray of the hand within 5 deg (ties 1.5 deg: nearer to the
  hand), kept up to 8 deg (was the line of sight from the eyes, 4 / 6 deg: the ray of a hand shakes
  more); hysteresis 1 cm / 10 cm.
- Hand ray: the aim pose of XR_FB_hand_tracking_aim (COMPUTED and VALID, -Z of the pose), chained to
  `xrLocateHandJointsEXT` (`extra_extensions/hand_tracking_aim_fb.rs`); without it (extension missing,
  or not valid for that hand) from a shoulder 15 cm below, 18 cm to the side of and 8 cm behind the eyes
  (level frame of the head) through the pinch point. Drawn only while editing, for a hand that is not
  near a handle and not part of a two-hand pinch: white 1 m without a handle, amber to a dot where it
  passes the handle it lights, cyan to the handle it holds; half the width of the outline.
- Far drag (`FarDrag`): a side or a dome goes to where the ray passes nearest to its axis (the line
  through the handle along the normal of the side), added up per update, relative to the grab; while the
  ray is within 20 deg of parallel to that axis the hand's motion along the axis times the gain moves it
  instead (so a far side can be pushed straight away; out of parallel again it goes on from there, no
  jump). The center: on the sphere around the hand at the distance of the grab. The turn handle: where
  the ray meets the level plane of the handle (ray at least 15 deg steep, within twice the grab
  distance), else the point at the grab distance; a change between the two starts the sweep anew (no
  jump). Its dead zone is measured from where the ray pointed at the grab (up to 5 deg beside the
  handle), not from the handle. Gain: distance of the handle from the hand at the grab over 0.45 m, at
  least 1; the dead zone and the ramp of the drag are that many times longer. Remove, undo and done
  picked by the ray work as near ones (the hold of "x" counts the hand's motion).
- Drag: 8 mm dead zone caught up over 3 cm; release keeps the value of 0.12 s earlier; dome under 1.5 cm
  snaps flat; side and dome drags along the turned normal.
- Turn: the angle swept around the vertical axis of the box (3 cm dead radius), detents every 15 deg
  within 2 deg, guide lines while grabbed.
- Handles: square per side, triangle for the dome, diamond for moving, curved arrow for turning, red "x"
  (held 0.8 s within 8 cm, not on the last box), undo (20 steps) and done above the code (any release).
  "+" until the third build. Game dimmed 30 % (60 % before) and the room through it at 45 % outside the
  area.
- Stream interruptions: editing pauses up to 8 s (`area_edit_grace`), a grab is kept; the hands are not
  sent to the PC while editing.

**Properties (all `debug.alvr.*`, polled).**

| Property | Default | Meaning |
|---|---|---|
| `area_edit` | value at app start | change to 1 starts editing, to 0 ends it and saves; set 0 then 1 |
| `area_pinch_on` | 0 mm | tip gap below which a pinch closes, -30 to 100 (negative: tips pressed together; contact measured -2 to -16) |
| `area_pinch_off` | 5 mm | tip gap above which a pinch opens, from `area_pinch_on` to 105 (may equal it) |
| `area_edit_dim` | 10 % | dimming of the game while editing, 0 to 90 (60 until the third build, 30 in it; 10 chosen by the user's eye on 2026-11-24) |
| `area_edit_see` | 25 % | the real room through the game outside the area while editing, 0 to 100 (0: opaque, as before; 45 in the third build, the user then chose 25: "good enough") |
| `area_edit_grace` | 8 s | how long editing waits for a stopped stream to come back before it ends, 0 to 60 |
| `area_edit_hands_pc` | unset (0) | 1: the hands are sent to the PC also while editing; else no skeletons and no hand poses while editing |
| `area_file` | unset (on) | 0: no layout file read or written, the property box again |
| `area_test` | unset | 1: test layout (box with a dome and a second box); 8: 8 boxes, 24 domes; else the one box |
| `area_dashed` | unset | 1: area drawn dashed (4 cm period, 60 % drawn) |
| `area_show` | unset | outline always (1) or never (0); the editor's own reasons override 0 |

**Log lines (stable prefixes `area editor:`, `play area:`, `area:`).**

| Line | Meaning |
|---|---|
| `play area: <n> box(es), yaw ..: #0 at (..) m, w x h x d m, dome +y ..` | layout at start and on every change of the properties |
| `play area: test layout 1 or 8`, `... area_test <n> is not a layout` | `area_test` changed |
| `area: outline cut, <n> segments of the play area left out (256 per eye)` | outline over the budget |
| `area editor: no layout file for "<payload>"`, `loaded <path> (...)` | file looked for / read; says which of the two folders won |
| `area editor: play area placed from the code`, `... from the anchor saved in the layout file` | placement; the second is drawn dashed |
| `area editor: two-hand gesture held (1.5 s to start or end editing)`, `... let go after <s> s` | the gesture is recognised; at most every 2 s; the false-trigger check |
| `area editor: editing (gesture or area_edit 1), game dimmed by <n> %, the room through it at <n> % outside the area, placed from ..` | edit mode on (the room part since the third build) |
| `area editor: the room shows through the game at <n> % outside the play area while editing (area_edit_see)` | `area_edit_see` changed |
| `area editor: selected box #i (left or right hand{, by the ray}): the next pinch edits it` | a pinch on another box only selected it |
| `area editor: new box: both hands pinched on empty space, <n> cm apart (more than 20 cm: ...)` | a two-hand pinch became a new box (the ghost appears, the gesture's timer stops for this pinch) |
| `area editor: added box #n (two-hand pinch{ (a hand lost)}, the pinch points <n> cm apart): #n at ..` | the new box was made when a hand let go |
| `area editor: no new box{ (a hand lost)}: the pinch points were <mm> mm apart, a new box needs 100 mm` / `8 boxes already, at most 8` / `its center would be more than 3 m from the code along an axis of its frame` / `the hands were not seen while it was pulled open` / `editing ended` / `stream stopped` / `no hands` | no box made |
| `area editor: grab of <handle> of box #i dropped (the left or right pinch{ by the ray} gives way: the right or left hand pinched too: both pinches are the two-hand pinch, ...)` | a young grab (near or by the ray, not out of its dead zone) gave way to the two-hand pinch; until the user's one-hand/two-hand rule only by the ray and only for a pinch on empty space (`... pinched on empty space: both pinches make a new box`) |
| `area editor: the left or right pinch{ by the ray} gives way (the right or left hand pinched too: ...)` | a young selection gave way to the two-hand pinch (the box stays selected) |
| `area editor: the left or right pinch does nothing until it opens: the right or left pinch holds <handle> of box #i (<s> s{, moved or done})` / `selected a box <s> s ago` / `does nothing either` (...) | a second pinch while the other hand works a handle that no longer gives way (one hand resizing is not disturbed) |
| `area editor: editing ended (<reason>)`, with `, nothing changed` when so | edit mode off; reasons: gesture, done handle, area_edit 0, no grab for 120 s, marker box off, no play area, `no stream with Passthrough = Depth for <s> s, more than area_edit_grace <n> s`, another code, area_file changed |
| `area editor: editing paused: no stream with Passthrough = Depth (...)`, `editing goes on after <s> s without the stream (it is back)` | a stream interruption while editing |
| `area editor: pinch gap min/max over 5 s: left a/b mm, right .. (...); rays from the aim pose / estimated: left a/b, right c/d updates` | measured gaps, every 5 s while editing; the ray part (third build) says whether the runtime gives the aim pose |
| `area editor: grab <handle> of box #i (left or right hand{, by the ray, <m> m away{, dead zone x<gain>}})` | a pinch took a handle (`side +x`, `dome +y`, `center`, `turn`, `remove`, `undo`, `done`; `add` until the third build); by the ray: out of reach, it follows the ray |
| `area editor: release <handle> of box #i{ (hand lost: tips not tracked \| <n> deg from the view direction \| no pose)}: moved, dome, moved by <m> m, yaw <deg> deg (turned by <deg> deg{, on a detent})` or `no change` | drag or turn result; why a hand was lost |
| `area editor: grab of <handle> of box #i ended (<reason>), kept as it was: ...` or `..., nothing changed`; `grab of .. dropped (two-hand gesture)` until the one-hand/two-hand rule | a grab ended other than by its pinch (editing ended, stream stopped, no hands, stage space changed) |
| `area editor: release <button>{ (hand lost: ..)}: nothing done (...)` | a button released while the hand was lost |
| `area editor: removed box #i`, `remove .. not done (held <s> s, moved <mm> mm; it needs 0.8 s held within 80 mm)`, `undo (n more)`, `nothing to undo` | box buttons (`added box #n on side ..` / `no room for a box next to ..` of "+" until the third build) |
| `area editor: stage space changed, coordinates of the saved anchor and the frozen pose kept (...){; <n> more such changes since the last line, not logged (...)}` | at most once a minute |
| `area editor: editing waits <n> s for the stream to come back`, `the hands are (not) sent to the PC ... (area_edit_hands_pc ..)` | those properties changed |
| `area editor: saved <path> (<why>, n boxes, ..)`, `could not save <path>: <err>` | per folder; a failure is logged once |
| `area editor: can't edit (area_edit 1): ..` | no play area, or Passthrough is not Depth |

**Headset checklist for the third build** (box on, code measured, Passthrough = Depth; property
defaults; read the log after each step).
1. Enter editing: outside the play area the room shows through the game at about half (the game a
   little darker), inside it fully; the outline and the handles as crisp as before. `area_edit_see 0`
   brings the opaque game back, `area_edit_see 70` more room. Log: `editing (gesture), game dimmed by 30
   %, the room through it at 45 % outside the area`.
2. Rays: with no handle near a hand, a thin white line from each hand along the pointer; the first `pinch
   gap` line says `rays from the aim pose / estimated: left n/0` (aim pose used) or `0/n` (estimated:
   tell). Point a ray at a far handle (the far side of a box): it turns amber with a dot on it; bring the
   hand within 8 cm of a near handle: that one wins and the ray goes.
3. Far drag: pinch while the ray lights a far side and sweep the ray: the side follows the ray
   (`grab side .. by the ray, <m> m away, dead zone x<n>`), no jump at the pinch, the hand's tremble
   not amplified much. Pull a far side straight away from you (ray along its axis): it moves with the
   hand's push / pull. Far center: the box moves on a sphere at its distance; far turn: point around the
   box. Release: `release ..` as before.
4. Select first: with one box selected, pinch the diamond of another box: it turns cyan (`selected box
   #i (...): the next pinch edits it`), does not move, no `release` / `saved` line; the next pinch moves
   it.
5. New box: pinch both hands on empty space close together, then pull them apart: past 20 cm a dashed
   green box spans the hands (`new box: both hands pinched ...`), it follows both hands; let go: a real
   box, selected (`added box #n (two-hand pinch, ...)`, `saved`); undo removes it. A flat pull gives a
   15 cm thick box. Pinch at two corners far apart: the ghost at once.
6. Leave gesture still works: hands together, palms away, held 1.5 s: amber square, `editing ended
   (gesture)`, no ghost. Do the gesture, pull apart before 1.5 s, come back together: no leave in that
   pinch, the square gone (a new box instead); release under 10 cm: `no new box: the pinch points were
   <mm> mm apart ...`.
7. At 8 boxes the ghost is red and release logs `no new box: 8 boxes already`.
7b. One hand or two (the user's rule): drag a side with one hand and, while it moves, pinch with the
   other hand (also on a handle, also palms away near the first): the drag goes on, no ghost, no amber
   square, `the <hand> pinch does nothing until it opens: ...`; release: `release ..` with the change.
   Pinch one hand on a handle (near or by the ray, also done) and the other within 0.5 s: `.. gives
   way ..`, then a ghost when pulled apart or the leave gesture; done does nothing on release.
8. Everything else of the second build: turn, remove hold, undo / done, a stream drop during a drag
   (`editing paused`, `goes on`), restart restores the layout.

**Headset checklist for the second build** (box on, code measured, Passthrough = Depth; property
defaults; read the log after each step).
1. Pinch: the cursor turns into a square only when the fingertips touch (not 1-2 cm before); `pinch gap`
   lines show the gaps. Two-hand gesture still starts editing.
2. Buttons: "+", undo and done each work on the first press even with a sloppy hand (`added box`, `undo`,
   `editing ended (done handle)`); "x" held 0.8 s removes without holding the hand very still.
3. Turn: pinch the curved arrow beside the center, move the hand around the box: the box turns live
   with no jump at the pinch, the amber guide and arc show the angle, the line turns green at 0/15/30
   deg; release: `release turn of box #i: yaw .. deg (turned by .. deg, on a detent)`. The hole in the
   game follows the turned box exactly (also its domes).
4. On a turned box: pull a side (it moves along its own normal, guide line along it), raise a dome,
   "+" (the new box is turned like it, on the side looked at), move it. Restart the app: `loaded ...`
   with `yaw .. deg` in the layout line, the box still turned.
5. Far sides: grab a ±x side or dome and pull it out to the side while looking ahead: no `(hand lost)`
   unless the hand really leaves the view; when it does, the line says why.
6. Stream interruption: during a drag, lift the headset off the eyes for 2-3 s and put it back (the
   session pauses and the stream reconnects, 5-7 s in all): `editing paused`, `grab of .. ended (stream
   stopped), kept as it was`, then `editing goes on after .. s`; the dimmed editor is back with the change
   kept. Off for 20 s: `editing ended (no stream ... for .. s ...)`, saved.
7. The PC while editing: in SteamVR the hands show as not tracked (no hand models moving with the
   pinches), back after editing; `area_edit_hands_pc 1` sends them again.
8. Log: STAGE pairs give at most one `area editor: stage space changed ...` line per minute.

**Headset test checklist of the first build** (box on, code measured, Passthrough = Depth; read the log
after each step).
1. `setprop debug.alvr.area_test 1`, `area_show 1`: a dome on the box and a second lower box, the real
   room inside both; log `play area: test layout 1`. Then `area_test 0`.
2. Unset `area_test`: the mask looks as before (one box, no domes); `area_dashed 1` makes the dashes stay put in the room.
3. Look at the log of a normal lap with hands on the wheel: no `two-hand gesture held` line.
4. Pinch with both hands in front of the face, palms forward, close together: dim outline, amber square
   closing, after 1.5 s the game darkens. Log `two-hand gesture held`, `editing (gesture)`. If it does not
   start: `area_edit 0` then `1`, read `pinch gap` lines, set `area_pinch_on/_off`.
5. Handles: cyan box, squares, triangles, diamond, "+", "x" (2+ boxes), check mark above the code; a
   plus cursor at each hand. Point at a square: it turns amber (also by looking through the fingers).
6. Pinch a square and pull: no jump at the pinch, the hole follows live, guide line; release: `grab side`,
   `release side ..: moved`, `saved ..` (note whether /data/local/tmp is written or logged once as failed).
7. Triangle: a dome grows (arcs, room under it), pushed back it goes flat. Diamond: the box moves.
8. "+" tap: a box on the side looked at (`added box`). "x" held 0.8 s: `removed box`; a quick tap: `not done`. Arrow: `undo`.
9. Check mark: `editing ended (done handle)`; the outline stays 3 s. Entering and leaving with no change: `nothing changed`, no `saved`.
10. Restart the app with the code hidden: `loaded ..`, `play area placed from the anchor saved in the
    layout file`, area dashed in the right place; show the code: `placed from the code`, solid.
11. Reinstall with `tools/install_apk.sh`, `marker_box 1`: `loaded /data/local/tmp/alvr-areas/..`.
    `area_file 0`: the property box again.
12. Cost: App GPU time with `area_test` unset, 1, 8; and editing against not editing (hands located every frame).

**Not verified yet.** Verified by the first run: the gesture starts and ends editing, drags, domes,
moves, add/remove, saving; the app can not write /data/local/tmp/alvr-areas. By the second run: turning,
the pinch at contact, sides / domes / centers on turned and unturned boxes, the gesture winning over a
grab of done. Not yet on the Quest: everything of the third build (the new box gesture, the room through
the game in both paths, select first, the hand rays: whether the runtime gives the aim pose with
XR_FB_hand_tracking_aim enabled, the far drags and their gain); of the second build the button changes,
the pause over stream interruptions, the hands kept from the PC; false triggers while driving; the
dashed restore and the saved anchor in the right place; the dimming in the colour key path; GPU cost of
the shader (area_test 1 and 8, now with the turn of each box) and the 16 KB overlay uniform block on
Adreno; dashes on lines clipped at the near plane crawl. Known gaps: moved boxes are not snapped to
overlap, the whole layout does not turn in the headset, undo is cleared on each entry; with the colour
key the colours it keeps show fully (not at 45 %) around the hands while editing (the packed alpha of
the two game layers can't hold both part way).

**The printed codes** (2026-11-23). Three codes with the payloads `RIG1`, `RIG2` and `RIG3`, black edge on
the paper 144.2, 95.6 and 67.4 mm. The printer scaled the page to 96 %: the 100 mm check bar measured
96 mm, so these are the measured sizes, not the designed ones. `tools/make_qr.py` makes the sheets
(outside the repository, on the development PC). The size to give to `marker_size` is the black edge
measured with a ruler, not the designed one.

**A code on the monitor.** `tools/show_qr.py [payload] [--monitor N] [--px N | --mm X] [--pitch X]
[--white N]` (outside the repository, on the development PC) shows a code on a monitor at a whole number
of pixels per module; the default payload is `SCR1`. Keys: `+` / `-` size, arrows move, `m` next monitor,
`b` / `B` dimmer / brighter white, `c` shows a bar of a round number of pixels to measure the monitor's mm
per pixel with a ruler, Esc ends. It prints the size by the monitor's own data, which is only a first
guess: measure the black edge with a ruler, that number is the marker size. Not tested on the headset.

Log lines (`ALVR_CAM`, all start with `marker`):
- At the start: `marker: the runtime lists XR_EXT_future yes, ... XR_EXT_spatial_persistence no; enabled: ...`.
  Then, rarely: `marker: off, the instance can't be created with the extensions: <error>` (the app runs
  without them), `marker: off, functions not found: <error>`.
- `marker: stopped (debug.alvr.marker 0)` or `marker: stopped (marker_box 0 and debug.alvr.marker unset)`
  when the tracker is dropped; `marker: the extensions are only enabled at the start of the app, restart
  it` after `marker 1` when the app started with `marker 0`.
- Once per session: `capabilities [...]; QR code: components [...], features [...]` (does the runtime
  list the static and the fixed size features?), `creating the spatial context (static markers: <bool>,
  fixed size: <mm> mm | no)`, `spatial context ready after <ms>`, `first discovery-recommended event after
  <s>`. `no view space, lines without the head: ...` if the head reference space can't be made.
- `marker_size "<text>" is not a number, ignored`.
- `marker_sizes entry "<entry>" is not <payload>:<mm> or <mm>, ignored`, once each time the property
  changes to a text with that entry.
- `size baseline of "<payload>" #<id>: <mm> mm (given)` or `(learned from <n> poses)` when a code gets a
  baseline, when it changes between given and learned, when the given size changes, and when the learned
  one changes by more than 0.5 mm. With a given size, once the runtime has measured 3 poses that are not a
  default size: `"<payload>" #<id>: given <mm> mm, the runtime measures <mm> mm (<+-%> %)` (its median).
- `marker: size tolerance <n> %` when `marker_size_tol` changes (also at the start when it is set to other
  than 3).
- `marker: "<payload>" #<id>: the last 6 sizes agree at <mm> mm, outside the baseline <mm> mm +-<n> %:
  size history restarted (a resized code)` (the size is their median, the baseline the learned one); a
  `size baseline ... (learned from 6 poses)` line follows.
- `discovery: <n> markers (<n> tracking), <ms> after the request, ...` when the number changes.
- A code is described everywhere as `"<payload>" #<id> <state> at (x, y, z) m, normal <deg> deg from
  vertical (lying|standing), heading <deg> deg, <w> x <h> mm{ (size not trusted)}` (stage space; position
  to 0.1 mm, heading to 0.01 deg, size as the runtime reports it; the pose as last reported, trusted or
  not, see "Size gate"). The heading is the one the box would use (degrees about
  vertical, counter-clockwise from above, 0 = stage -Z), printed in -90..270 so that a code facing the
  driver (about 180) doesn't jump between +180 and -180; the same range in the `marker box:` and
  `ReferenceSpaceChangePending` lines. Lines using it: `new <description><head>`, `<old state> -> <new state>: <description><head>`
  (also `tracking -> paused`), `lost #<id>`, `payload changed ...` (seen by discoveries; update snapshots
  read a payload only while it is unknown). A pose also counts as changed when only its orientation
  changes.
- The head part, added when a head sample and a pose exist: `, head <m> m away, <deg> deg off the normal,
  head up to <m/s> m/s and <deg/s> deg/s in 2 s` (the head pose in the view reference space, sampled at
  20 Hz; with fewer than 2 head samples `head up to - in 2 s`). It tells at which distance and angle the
  code was seen, and how fast the head moved: a bad pose with a fast head is not a bad code.
- `the payload buffer id of #<id> changed from <a> to <b> (logged once) ...`: the runtime gives new buffer
  ids per snapshot (the payload cache is cleared at 64 entries).
- `unknown tracking state <n> of #<id> (logged once, taken as paused)`; `invalid pose from the runtime for
  #<id> (logged once, dropped): center (...), orientation (...), extents <w> x <h>` (a non-finite pose).
- Every 2 s while codes are known: `<n> known; discoveries <n> (...); update snapshots <rate>/s, <ms>
  each` (or `none`, or `failed (poses from the discoveries)`), and per code `  <description><head>, pose
  changes every <ms> (<n> > 0.1 mm / 0.01 deg, <n> bit-exact in 2 s), scatter x/y/z <mm> over <k>
  distinct poses in 10 s, heading scatter <deg> deg (range <deg>), seen <s> ago, <n> poses of another
  size left out in 2 s{ (last <w> mm)}, baseline <mm> mm +-<n> % (given|learned)` (the band of the
  trusted sizes, e.g. `baseline 145.6 mm +-3 % (learned)`); without a baseline the end is `, <n> poses
  left out in 2 s{ (last <w> mm)}, no size baseline yet`, with `marker_size_tol 0` it is `, size gate off
  (marker_size_tol 0), baseline <mm> mm (given|learned) | no size baseline yet`. The poses left out are
  distinct poses (a repeat of the pose before is not counted again), the size is the width. "pose
  changes", the scatter and the heading scatter count trusted poses only, "seen" is the last pose
  reported. "pose changes" counts
  the changes of more than 0.1 mm or 0.01 deg, and how often they come is how often the runtime really
  updates the pose (update snapshots repeat the last one; the bit-exact count includes smaller changes,
  which show whether the runtime changes the pose by tiny amounts). The scatter is the standard
  deviation of the position over the distinct poses of the last 10 s (`scatter x/y/z - (no distinct
  pose) in 10 s, heading scatter -` with none); the heading scatter is that of the heading, with its
  peak-to-peak range, about the circular mean. A code without a pose yet: `  <description>, no pose yet`.
  Every 10 s while none is known: `0 known; ...`.
- `<OpenXR call> took <ms> ms (...)` when a call takes more than 2 ms (at most every 2 s per call).
- `<OpenXR call>: <result> (<code>)` once per call and result code. `off, ...` when it gives up: restart
  with `setprop debug.alvr.marker 0`, a second, `1`. Update snapshots that fail no longer turn the tracker
  off: one line `update snapshots can't be created: none from now on, poses come from the discoveries
  only (debug.alvr.marker 0, then 1 tries again)` and the poses then come from the discoveries only.
- `marker box: marker_id is now "<text>" | unset`, `marker box: averaging window <s> s`, `marker box: a
  move must hold <s> s`, `marker box: a measurement takes <n> poses over 8 s` (when `marker_window` /
  `marker_move_ms` / `marker_scan_poses` are set to other than the defaults). A window below 8 s adds
  ` (shorter than the 8 s a measurement needs: never measured)`.
- `marker box: measuring again (marker_scan <n>)` for a new positive `marker_scan`.
- `marker box: the hands show inside the box only (marker_hands 1)` or `marker box: the hands show also
  outside the box (marker_hands 0)` when `marker_hands` changes.
- `marker box: measured: position (x, y, z) m, heading <deg> deg from <n> poses over <s> s, scatter x/y/z
  <mm>/<mm>/<mm> mm, heading scatter <deg> deg`, once per measurement: the frozen estimate, the distinct
  poses in it, the time they span, and the standard deviation of their positions and headings (the heading
  scatter about their circular mean, before the 1.5 degree trim). `marker box: the marker moved, measuring
  its pose again` when a move ends a measurement.
- `marker box: {keeping the last pose, }waiting for <the marker "<id>" | a marker with a pose>{, none has
  a trusted pose yet (size gate)} (known: ...)` every 10 s while no code fits (also while a pose is kept).
  The middle part comes when codes that fit have poses but none of them a trusted one; the descriptions
  after `known:` show their sizes. Only trusted poses place the box.
- `marker box: <n> poses away from the average left out so far`, at most every 2 s when the count grew.
- `marker box: placed at (x, y, z) m, heading <deg> deg, size ..., marker #<id> at (...), <n> samples
  over <s> s (window <s> s), <n> outliers left out`, again as `moved by <cm> cm, at ...` when a corner of
  the box moved by more than 1 cm since the last line (before the pose is measured at most once a
  second; once measured it moves only with a move or `marker_scan`). The samples are distinct poses only; `over <s> s` is the time from the
  oldest to the newest of them (it can be longer than the window when the runtime stops refreshing the
  pose, and is 0 with one sample).
- `marker box: used by the depth mask` or `marker box: placed but not used (no stream, or Passthrough is
  not Depth)` when that changes. It says "used" also when the environment depth failed to start (a missing
  permission); that failure is logged on its own.
- `marker box: off (marker_box 0), the region of the settings applies` and `marker box: off
  (debug.alvr.marker 0), the region of the settings applies`, when a box that was placed is dropped for
  that reason. `marker box: no tracker, keeping the last pose` once when the tracker goes (it failed, or
  the session stopped) while a box is placed: the box stays.
- `area: outline shown (measured | carried over | placed | area_show 1)` when the outline of the code
  and the box appears or shows for another reason, `area: outline hidden` when it goes (after 8 s, with
  `area_show 0`, without a box, or when Passthrough is not Depth).
- `area: outline cut, <n> segments of the play area left out (256 per eye)` when the outline of the
  play area does not fit (`area_test 8`).
- `play area: <n> box(es), yaw <deg> deg: #0 at (x, y, z) m, w x h x d m, dome +y 0.250 m; #1 ...` (area
  frame) at the start and each time the layout changes (properties); `play area: test layout 1 | 8`,
  `play area: the box of the marker_box properties (area_test 0)` or `play area: area_test <n> is not a
  layout (1, 8), the one box` when `area_test` changes. With a layout file these describe the box of
  the properties, which is then not used.
- Editor (`area editor:`): `editing (gesture | area_edit 1), game dimmed by <n> %, placed from <the
  code | the saved anchor>: <layout>`; `editing ended (gesture | done handle | area_edit 0 | no grab for
  120 s | marker box off | no play area | no stream with Passthrough = Depth for <s> s, more than
  area_edit_grace <n> s | another code | area_file changed), <layout>{, nothing changed}` (`Passthrough is
  not Depth` in the first build); `editing paused: no stream with Passthrough = Depth (...)` and `editing
  goes on after <s> s without the stream (it is back)`; `the edited layout replaces the box of the marker_box properties from now
  on (...)` at the first change of the box of the properties; `stage space changed without a pose, the
  saved anchor is dropped until the code is seen`; `no code seen yet and no marker_id: the layout of
  "<payload>", saved last (<path>)` or `...: the newest layout file (<path>) names no payload, waiting for
  a code`; `can't list <dir>: <error>`; `can't
  edit (area_edit 1): no play area (...)`; `two-hand gesture held (1.5 s to start | end editing)` and
  `two-hand gesture let go after <s> s` (at most every 2 s); `grab <side +x | dome +y | center | turn |
  remove | undo | done> of box #<i> (left | right hand{, by the ray, <m> m away{, dead zone x<n>}})`
  (`add` until the third build); `selected box #<i> (left | right hand{, by the ray}): the next pinch
  edits it`; `new box: both hands pinched on empty space, <n> cm apart (...)`, `added box #<n>
  (two-hand pinch{ (a hand lost)}, the pinch points <n> cm apart): <box>` or `no new box{ (a hand
  lost)}: <reason>`; `the room shows through the game at <n> % outside the play area while editing
  (area_edit_see)`; `release <handle> of box #<i>{ (hand
  lost: tips not tracked | <n> deg from the view direction | no pose)}: moved <m> m | dome <m> m | moved by
  <m> m | yaw <deg> deg (turned by <deg> deg{, on a detent}), #<i> at (x, y, z) m, w x h x d m{, yaw <deg>
  deg}{, dome ...}` or `...: no change`; `release <add | undo | done>{ (hand lost: ...)}: nothing done (a
  button acts only when the pinch opens with the hand in view)` (`not a tap (moved <mm> mm)` in the first
  build); `grab of <handle> of box #<i> dropped (two-hand gesture)` (until the one-hand/two-hand
  rule; now `dropped (the <hand> pinch{ by the ray} gives way: ...)`, and `the <hand> pinch does nothing
  until it opens: ...`); `grab of <handle> of box #<i> ended
  (editing ended | stream stopped | no hands | stage space changed), kept as it was: <box> | nothing
  changed`; `stage space changed, coordinates of the saved anchor and the frozen pose kept (marker_carry 0;
  ...){; <n> more such changes since the last line, not logged (at most one line per 60 s)}`; `added box #<n> on side
  <s> of box #<i>: ...` or `no room for a box next to box #<i> (...)`; `removed box #<i>, <layout>` or
  `remove of box #<i> not done (held <s> s, moved <mm> mm; ...)`; `undo (<n> more), <layout>` or `nothing
  to undo`; `saved <path> (<why>, <n> boxes, with the anchor | no anchor measured yet)` or `could not
  save <path>: <error> (not logged again for this folder until a save there works)`; `not saved (<why>): area_file 0 | the payload of the
  code is not known yet`; `loaded <path> (the only one found | the newer of the two | the newer one does
  not read, written <h> h ago): <layout>; anchor (x, y, z) m, heading <deg> deg | none`, then its warnings,
  unknown lines, a payload that differs, and `the marker_box_* properties and area_test are not used while
  a layout file is`; `no layout file for "<payload>" (<name> in <dir> or <dir>): the box of the
  marker_box properties`; `<path> does not read: <error>`; `can't read <path>: <error>`; `play area
  placed from the code | from the anchor saved in the layout file, the code at (...) m, heading <deg> deg
  (dashed until the code is seen) | no play area placed`; `pinch gap min/max over 5 s: left <a>/<b>
  mm, right - mm (closes below 0, opens above 5)` while editing; `a pinch closes below a gap of <n> mm
  and opens above <n> mm`, `the game is dimmed by <n> % while editing`, `editing waits <n> s for the
  stream to come back`, `the hands are not sent to the PC while editing (area_edit_hands_pc 0)` / `... are
  sent to the PC also while editing (area_edit_hands_pc 1)`, `layout files are (not) used
  (area_file ...)` when those properties change.
- `area: outline shown (editing | editing ended | edit gesture)` from the editor (these override
  `area_show 0`).
- `ReferenceSpaceChangePending type: <type>, change time <ns> ns, pose valid: <bool>, pose in previous
  space (x, y, z) m, yaw <deg> deg` (now in `ALVR_CAM`, for every reference space type; only STAGE ones
  give `marker: stage space changed, poses dropped until the markers are seen again`; with a valid pose
  also `STAGE change with a valid pose: the box keeps its coordinates (marker_carry 0)` or `... is carried
  by it (marker_carry 1)`; and, at the change time while a box is kept, with a valid pose `marker box:
  stage space changed, coordinates kept (marker_carry 0; the reported pose (x, y, z) m, yaw <deg> deg is
  not applied)` or with `marker_carry 1` `marker box: stage space changed, box carried over (moved by (x,
  y, z) m, yaw <deg> deg in the new space; marker_carry 1)` (the change of the coordinates of the box
  centre and of its heading, the box itself should stay in the room), or `marker box: stage space
  changed, waiting for the marker` without a valid pose).

Testing (`adb logcat -s ALVR_CAM` running):
- [ ] The printed codes `RIG1`, `RIG2`, `RIG3` (black edge 144.2, 95.6, 67.4 mm; the payload is case
      sensitive, `marker_id RIG1`), plain black on white, no logo. Lay one flat on the desk in good light.
- [ ] `setprop debug.alvr.marker 1` (the measuring steps run without the box; unset, the tracker runs only
      with `marker_box 1`). Start the app: the first line says which extensions the runtime lists. If `XR_EXT_spatial_entity`
      is `no`, look for `missing uses-permission` in the full logcat.
- [ ] `capabilities ...` must list `MARKER_TRACKING_QR_CODE`; a `ERROR_PERMISSION_INSUFFICIENT` line
      means the spatial data permission (asked at the first depth passthrough) is missing: it retries
      every 10 s. Note whether the features list the static and the fixed size feature.
- [ ] After a line starting `marker: off, ...`: `setprop debug.alvr.marker 0`, wait a second,
      `setprop debug.alvr.marker 1` restarts the tracker.
- [ ] Accuracy of the size first, with `marker_size` unset: look at each code from 40-80 cm:
      `new "RIG1" ...`. Compare the size in the line with the measured one (144.2, 95.6, 67.4 mm) for
      each of the three codes; read the normal angle (about 0 on a level desk), the pose changes interval
      and the scatter, once with the head still and once while moving it (the head part says how fast).
      Then from further away and at a flat angle (the head part gives the distance and the angle).
- [ ] Size gate, `marker_sizes` unset: `size baseline of "RIG1" #1: ... mm (learned from 3 poses)`
      near the ruler size. While the runtime reports 100.0 or 50.0 mm, the description says `(size not
      trusted)`, the summary counts the poses left out, and the box does not move. Then `setprop
      debug.alvr.marker_sizes RIG1:144.2,SCR1:<ruler mm>`: `size baseline ... (given)` and the line with
      what the runtime measures against it. `marker_size_tol 0` turns the gate off to compare. (The
      `marker_size` step of before does nothing on Quest: no fixed size feature, measured.)
- [ ] Turn the head away for 10 s and back: `paused` / `tracking` lines, and whether the position comes
      back to the same millimeters.
- [ ] Passthrough = Depth, then `setprop debug.alvr.marker_box 1`: `marker box: placed ...`, then
      `marker box: used by the depth mask`. The depth mask shows the box: the real desk and what stands
      on it inside the box, the game outside. The debug view "Tint mask" marks it green only without the
      color key (`setprop debug.alvr.color_key 0`, and `auto_key 0`, live; hands are then limited by the
      box too); with the key loaded the box is plain passthrough, not green. Move the code by 5 cm: the
      box follows after about 1 s (`marker_move_ms`; at least 3 poses at the new place) while the pose is
      not yet measured, after about 3 s (at least 5 poses) once it is measured, then `the marker moved,
      measuring its pose again` and `moved by`. With the code on the monitor, switch the monitor to the
      game: the box stays. Count the `moved by` lines while racing with the code fixed: each one is a
      false move; if there are any, try `marker_move_ms 6000`.
- [ ] Measured baseline: show the code for about 15 s. `marker box: measured: ...` must come after about
      10 s (20 poses at 2 Hz); note its scatter and heading scatter. Before it at most one `moved by` per
      second, after it none while the code stays (also with single poses at 198 or 214 deg). Hide the
      code: the box stays, also after `setprop debug.alvr.marker_scan 1` while it is hidden (no
      `moved by` until the code is shown again). `setprop debug.alvr.marker_scan 2` with the code shown:
      `measuring again`, a new `measured:` line; compare the two positions and headings (how repeatable
      is a measurement?).
- [ ] Hands (`marker_hands`, color key on): reach out of the box with one hand. The hand must vanish
      at the box surface like the forearm, thinning over a few cm, not popping. Then `setprop
      debug.alvr.marker_hands 0`: the hand shows outside again (as in the second run). Look at the palm at
      the surface: does it thin out too early? Note the GPU load (VrApi) with 1 and 0.
- [ ] Origin: with the code flat, set `marker_box_y 0`, `marker_box_h 100`, `marker_box_w 150`,
      `marker_box_d 150`. The tinted patch must sit centred on the code, not on a corner. A shift of half
      the code size means the runtime's origin is not the centre; note the direction.
- [ ] Heading, flat code (default sizes, or `marker_box_w 400` so the long axis is obvious):
      `marker_box_w 400`: the box must get narrow ACROSS the reading direction of the code (left-right as
      you read it); if it narrows along it, the heading is off by 90 degrees. `marker_box_z 300`: the box
      must move 30 cm towards the TOP edge of the code (the side with two finder squares). `marker_box_x
      300`: it must move to the right of the code as read. Turn the paper by 90 degrees: the `heading` in
      the `marker box:` line changes by 90 degrees and the box turns with it. If a direction is wrong,
      note which one and try `marker_box_yaw 180` / `90`. Reset x / z to 0 afterwards.
- [ ] Change `marker_box_y` / `_w` live.
- [ ] Standing code (paper upright, or the code shown on the monitor facing you, see "A code on the
      monitor"): `new ...` must say `standing` and a normal of about 90 deg (a monitor leaning back:
      60-90). `marker_box_z 300` must bring the box TOWARDS you, out of the screen. `marker_box_x 300`
      must move it to your LEFT. The default box is made for a flat code: around a standing code it
      reaches 0.5 m behind the screen and only 0.5 m in front, from 0.30 m below to 0.60 m above the
      code. Start from `marker_box_z 400` (0.1 m behind the screen to 0.9 m in front with `_d` 1000;
      make it your distance to the screen minus 300 or so) and `marker_box_y -150` (0.60 m below to 0.30
      m above the code with `_h` 900, for the wheel and the hands). Compare the `marker box: placed at
      ...` centre with the `marker #.. at ...` position in the same line to check the offset.
- [ ] Recenter with the code hidden after `measured:`: note the `ReferenceSpaceChangePending type:`
      lines (ordinary recenters send only LOCAL / LOCAL_FLOOR: nothing happens to the box). A STAGE one
      with `pose valid: true` must give `marker box: stage space changed, coordinates kept (marker_carry
      0; ...)` and the box must stay at the same place in the room (judge it against the rig). Then show
      the code again: no `the marker moved, measuring its pose again` line (the new poses fit the kept
      estimate). If the box is off after such an event and the code shows it at new coordinates, try
      `marker_carry 1` at the next one (the old carrying, `box carried over (...; marker_carry 1)`). A
      `pose valid: false` change still drops the box until the code is seen again.
- [ ] Outline (`area_show` unset, box on, code shown): `area: outline shown (placed)`, amber square on
      the code, then after `marker box: measured:` `area: outline shown (measured)`, green, and `area:
      outline hidden` 8 s later. `setprop debug.alvr.area_show 1`: the square on the real code and
      turned like it; the white lines along the border of the passthrough; move the head 10 cm sideways:
      the lines stay on the desk; both eyes fuse them. Note fps and GPU load (VrApi) with `area_show 1`
      and `0`. Recenter: the outline stays on the rig (`area: outline shown (carried over)` only after
      a STAGE event with `marker_carry 1`; a kept box shows no outline for it).
- [ ] Play area (box on, `area_show 1`): `setprop debug.alvr.area_test 1`: `play area: test layout 1`
      and the layout line; a white dome over the top of the box and a lower box on its +x side; the real
      room shows inside both and under the dome, the lines on the border of the hole. `area_dashed 1`:
      the area dashed, the dashes fixed in the room. `area_test 8`: 8 boxes with 24 domes (the outline
      is cut, logged). App GPU time with `area_test` 0, 1 and 8, several runs each; and with `area_test`
      unset the mask must look as before.
- [ ] Editor, no layout file yet (box on, code measured, `area_show` unset): the log has `area
      editor: no layout file for "SCR1" (...)`. Hands on the wheel, driving a lap: no `two-hand gesture
      held` line (if there is one, note what the hands did). Both hands up in front of the face, pinch
      thumb and index on both, palms turned forward, the pinches close together: the outline appears
      dim, brightens, an amber square closes between the hands, after 1.5 s `editing (gesture)` and the
      game darker. If it does not start: read the `pinch gap` lines (only while editing, so first try
      `setprop debug.alvr.area_edit 0` then `1`) and set `area_pinch_on/_off`.
- [ ] Editor, handles: a cyan box with white squares on its sides, triangles beside them, a diamond
      at its center, "+" and (with 2+ boxes) a red "x" above it; a green check and (after a change) an
      arrow above the code; a small plus at each hand. Point at a square: it turns amber and larger (also
      for one out of reach, by looking through the fingers at it). Pinch and pull: `grab side ...`, the
      side follows the hand (no jump at the pinch), the hole in the game follows live, a dashed guide
      line; release: `release side ...: moved ...` and `saved ...` (note whether
      /data/local/tmp/alvr-areas could be written or `could not save` is logged once). Triangle: pull out, a dome grows (arcs in the
      outline, room shows under it); push back: flat. Diamond: the box moves. "+" tap: a box on the side
      looked at; "x" held 0.8 s: removed, a quick tap: `not done`. Arrow: undo. Check mark: `editing
      ended (done handle)`, the outline stays 3 s. Judge: hand jitter while dragging, the dead zone, the
      value kept on release (does it jump back when the fingers open?), handle sizes, whether wrong
      handles light.
- [ ] Editor, restore: restart the app with the code hidden: `loaded .../area-SCR1-....txt`, `play
      area placed from the anchor saved in the layout file` and the area dashed in the right place; show
      the code: `placed from the code`, solid at once. Restart without `marker_id`: the same, after `no
      code seen yet and no marker_id: the layout of ...`. Enter and leave editing without a change:
      `nothing changed`, no `saved`. Reinstall a CI build with tools/install_apk.sh: `kept ...` lines,
      then `loaded /data/local/tmp/alvr-areas/...`. `setprop debug.alvr.area_file 0`: the box of the properties again.
- [ ] Editor cost: App GPU time and CPU frame time editing vs not (the hands are located every frame
      while the marker box is on and Passthrough = Depth, also while racing).

## Open problems, in order

The order of the user on 2026-11-23; how to go on with each is in "Where it stands and how to continue".

1. The forearm and held objects are cut out near the hand. Nothing works yet (2026-11-24: the arm is a
   timing problem, step 1 of the arm plan is next, not built; no fill rule to ship; see "Day of
   2026-11-24"). Measured on the lap: the
   runtime removes the depth on the hand and 3-5 cm of thigh around it (uneven, up to 8-15 cm towards
   the knees), the key zone is mm wide and keeps 0.1-1.6 % of the trousers colour, so the hole comes
   from the depth removal.
2. Jagged edges of what the depth mask shows (sleeve, bottle, later the rig). Measured: at the box
   region's edges 80 % of the surfaces shown real that the depth puts outside come from the 0.08 m
   feathering (median 1.3 cm outside), 20 % from the 3x3 blend; feathering 0.08 not judged yet
   (2026-11-24: 8 / 3 / 2 cm judged "no massive difference", left at 0.03; the limit is the depth
   resolution; the holes of a wheel are never in the depth).
3. The room next to the hand: thin lines and patches. Roughly 40-50 % of it from the solid fill. In the
   box captures on the lap, 87 % of the real shown around the hands beyond 6 cm comes from the stale
   remembered copy (old legs where the floor under the desk is).
4. The play area around anchors on the rig (QR codes) instead of around the head: the box region exists,
   the calibration doesn't.
5. A real drive with a wheel. Hands on a wheel were only approximated with a bottle and a monitor, and
   the playzone map was never tried with a wheel or a shifter.
6. GPU budget: 80 % with hands in view, peaks of 95-97 %. The extra layers of the key are a big part
   (key off: about 85 -> 62 % in one measurement); anything new on the GPU has to be cheap.
7. Never tested on the headset: the lighting match, the leak map (`key_leak_map`), the narrower rim
   (`key_leak`). The box region ran once and is not judged.
8. A way to compare two settings of the hand side by side (one per hand): asked for by the user.

## Ideas, not agreed and not built (2026-11-24)

Ideas of the user about the hand key, with my evaluation from the code. Nothing measured yet; after arm
step 1 in the agreed order.

**1. A width filter on the key** (user: "if lines are less than X pixels wide, remove the area/pixels, so
we capture more of the hands, but then remove leakage from the background, without fetching the camera
feed"). Not possible as stated: the key is a LUT that the compositor applies per camera pixel
(`set_color_key` in `extra_extensions/passthrough_fb.rs`); a LUT sees one pixel's colour, never its
neighbours, and the keyed result exists only inside the compositor (`stream.wgsl` only sets the alpha of
the game layers around the keyed layer). A morphological opening needs that result, so it means reading
the camera (the learning thread already reads camera pixels, so the route exists) and keying ourselves:
then the key is as late as those frames and loses what makes it the "accelerometer" of the fusion.
Fingers at lap distance are several cm wide, so an opening sized for speckle would keep them but round
the tips and close the gaps. Ways to the same goal without the camera: a rim that follows the finger
capsules tightly, so the colour range can be wide and a leak a few cm away falls outside the rim; a depth
veto inside the rim where the depth is sure a texel lies well behind the hand (static background, so its
age matters less; fails next to a fast hand). Cheap check first: an opening offline on the lap captures
626 / 627 (`logs/dumps-20261124-lap`): mostly thin speckle (a width filter would help) or wide patches of
trousers / desk colour (only rim shape or depth help). The user's own finding (the flicker appears where
the VR area is behind the hands) points to the second.

**2. Spatial chunks, each with its own hand + background mask** (user: "make a spacial hand + background
mask, where the space is divided into spacial chunks and each chunk has its own mask"). Two parts of very
different cost:
- *Own policy per chunk: possible and cheap.* Chunks in the frame of the QR code (like the editor boxes),
  each with a rule which signal decides: e.g. lap chunk = never trust the key, only depth and the moved
  hand shape; wheel chunk = key with a wide range (the VR area is behind); shifter chunk = remembered copy
  + hand. Only the alpha of the existing layers changes, no new layer. The rules could be learned per
  chunk from the late depth (where it shows the key leaked, the chunk trusts the key less), as in
  the user's principle "depth is delayed ground truth, the key is instantaneous". Related: the leak map (`key_leak_map`) already rates leaks per direction around
  the head and narrows the rim, but it is not in the anchor frame and never acted on the headset.
- *Own colour table per chunk: only with more passthrough layers.* There is one keyed layer for the whole
  view with one LUT; stacking several full-view keyed layers does not work (each applies everywhere, and
  hiding it outside its chunk also hides the chunks below). The way that works is projected passthrough
  (XR_FB_passthrough projected surfaces + XR_FB_triangle_mesh, as in the cockpit-passthrough fork, see
  `external/cockpit-passthrough/NOTES.md`): one mesh + one style (LUT) per chunk. Costs: the camera image
  is pasted onto the mesh, so things off the surface look flat / shifted (a hand 10-20 cm above a lap
  mesh is drawn on the lap) unless the mesh follows the hand every frame; each passthrough layer is
  another full composite (the key's layers are already a large part of the GPU budget, open problem 6);
  not verified that a projected layer takes our LUT and combines with the reconstruction keyed layer.
  Gain: the lap leak exists because one global table must keep skin colours that appear in the
  background somewhere; per chunk the background is known (trousers, desk, VR area), so the table can
  drop what lies behind the hands there.
- My recommendation: first the policy per chunk (tests "distrust the key on the lap" on the leg problem
  at no new cost); a projected layer with its own LUT only if a chunk truly needs other colours, after a
  short headset probe (does the runtime allow it, how bad is the warp).

## Privacy

Camera images are only used in memory to learn colors. Nothing is stored or sent. Only on request
(`key_lut_dump`, `key_dump`) the learned tables, camera images and depth maps of one moment are written to
the files folder of the app on the headset. Average colors and
sample counts are written to the log of the headset.

## Sources

- [Meta: depth occlusion and its limits](https://developers.meta.com/horizon/documentation/unreal/unreal-depthapi-occlusions/)
- [Meta: masking passthrough with tracked hands](https://developers.meta.com/vr/resources/hand-representation/)
- [Meta: passthrough color LUTs](https://developers.meta.com/horizon/documentation/native/android/mobile-passthrough-customization)
- [Meta: passthrough camera API (native)](https://developers.meta.com/horizon/documentation/native/android/pca-native-documentation)
- [Road to VR: hand tracking and passthrough latency](https://roadtovr.com/apple-vision-pro-meta-quest-3-hand-tracking-latency-comparison/)
- [Godden et al.: hand tracking accuracy and latency](https://ericg5.github.io/ResearchPortfolio/papers/characterization)
- [Nokia XR Lab: RGB-D body segmentation](https://www.frontiersin.org/journals/virtual-reality/articles/10.3389/frvir.2026.1757111/full)
- [PassthroughForge](https://www.passthroughforge.com/)
- [Meta: QR code detection (MRUK)](https://developers.meta.com/horizon/documentation/unity/unity-mr-utility-kit-qrcode-detection)
- [Meta: spatial data permission (native)](https://developers.meta.com/horizon/documentation/native/native-spatial-data-perm/)
- [Meta: spatial anchors API reference (native)](https://developers.meta.com/horizon/documentation/native/android/openxr-spatial-anchors-api-ref)
- [Meta: experimental features (native)](https://developers.meta.com/horizon/documentation/native/android/mobile-experimental-features/)
- [Android XR: XR_EXT_spatial_marker_tracking](https://developer.android.com/develop/xr/openxr/extensions/XR_EXT_spatial_marker_tracking)
- [Android XR: XR_EXT_spatial_entity](https://developer.android.com/develop/xr/openxr/extensions/XR_EXT_spatial_entity)
- [Khronos: XrSpatialMarkerDataEXT](https://registry.khronos.org/OpenXR/specs/1.1/man/html/XrSpatialMarkerDataEXT.html)
- [Khronos: OpenXR SDK changelog](https://github.com/KhronosGroup/OpenXR-SDK-Source/blob/main/CHANGELOG.SDK.md)
- [Meta sample repository: QR on v205, answer of a Meta engineer](https://github.com/oculus-samples/Unity-MRUtilityKitSample/issues/15)
- [Godot forum: QR tracking on Quest 3 (v79 no, v83 yes)](https://forum.godotengine.org/t/qrcode-tracking-via-openxr-spatial-entities-extension-on-meta-quest-3/123395)
- [Godot issue: permission before the context](https://github.com/godotengine/godot/issues/123467)
- [Godot: spatial entities, update snapshots](https://docs.godotengine.org/en/stable/tutorials/xr/openxr_spatial_entities.html)
- [Unity forum: marker tracking platforms (QR only on Horizon OS)](https://discussions.unity.com/t/when-does-the-ar-marker-tracking-get-more-supported-plattforms/1703253)
- [UploadVR: QR codes in MRUK v78](https://uploadvr.com/quest-3-mruk-update-decode-track-qr-codes-unity)
- [Meta forum: QR position jitter on a still code](https://communityforums.atmeta.com/questions-and-discussions-26/quest-3-qr-code-tracking-severe-position-jitter-182318)
- [Meta forum: QR code detection discussion](https://communityforums.atmeta.com/questions-and-discussions-26/mr-qrcode-detection-173577)
- [Display-camera channels (moire, rolling shutter)](https://arxiv.org/pdf/1501.02528)
- [Microsoft: QR code tracking sizes (other platform)](https://learn.microsoft.com/en-us/windows/mixed-reality/qr-code-tracking)
