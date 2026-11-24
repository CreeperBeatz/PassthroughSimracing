# Arm mask: code map and design

Status 2026-11-24. This is a design: nothing is built. Line numbers are for the current working tree of
`ALVR` (branch `auto-color-key`, HEAD bcfefe28, with the play area editor job's uncommitted changes). Words
as in the KB: **measured** (headset logs or bursts), **offline** (computed on recorded data), **inferred**,
**not tested**. The offline zone check in 3a is one analysis and was not re-checked.

## 0. Decisions in short

- **Zone:** a tapered capsule per forearm. It is fitted on the CPU to the arm's texels of the newest depth
  frame, which is read back. It starts at the wrist and is moved with the wrist using the measured linear
  weight (1 at the wrist, 0 at 30 cm). The key uses only its first 18 cm, plus a 15 mm margin. The capsule
  goes into the uniform slot of the old wrist bridge, so the rect, the marker-box limit and the focus pass
  pick it up with little new code. Picked over "moved depth texels as the zone" (3a).
- **Arm texels:** in the CPU fit, the arm is a depth-connected piece behind the wrist plane, taken 3-25 cm
  from the wrist. It is cut to a 6.5 cm tube around its own fitted line, then quality-gated. This recipe is
  the one already used on the bursts. In the mask pass, a texel counts as arm if its depth point lies
  inside the fitted 3D capsule (+1 cm). One test per mask texel, never per depth sample.
- **Core and edge:** inside the zone, the moved depth keeps the core of the arm (15 mm inside the fitted
  surface, any colour). The band between core and margin is handed to the colour key, and only where
  the depth texel is arm. A desk or wheel in the play area next to the arm keeps its depth mask.
- **Fallback:** a reliability R per arm, from the learner. It is high when the table keeps the arm's
  colours and drops the arm's own background. R = 0 means no band and no keyed zone, so exactly the new
  depth correction (mode 1).
- **Wrist target:** the joints are looked up for a past time (`display − 80 ms`), where the runtime's
  history is accurate. They are then extrapolated at constant speed to `display − arm_target_ms`. The
  passthrough latency λ is unknown, so the default is 40 ms, a live property.
- **Sleeves:** a separate density class "arm by depth" in the learner, learned only from the capsule core
  of a slow arm with a fresh fit. It is merged into the one uploaded LUT only while it passes leak gates
  against the arm's background, the hand rings and the scene. One LUT, because the compositor has one
  colour map per passthrough layer, and a second keyed layer would need another game layer.
- **Switch:** `debug.alvr.arm_mask` 0 / 1 / 2 (today / new depth correction / plus keyed zone), default 0
  in the first build. `arm_mask_hands` puts the new mode on one arm only, for a side-by-side comparison
  (the user asked for one setting per hand).

## 1. How the hand's keyed mask works today (code map)

### 1.1 Where the capsules come from, and at which time

| What | Where | Notes |
|---|---|---|
| Hand time for the key zones | `client_openxr/src/stream.rs:825-826` | `time = display_time − key_delay` (default 0, `passthrough.rs:292-294`). Measured on the bursts: these joints are the runtime's newest estimate, 44-76 ms behind the real hand, no forward prediction, 4-5 cm overshoot at reversals |
| Joint lookup | `stream.rs:890-894` (`relate_hand_joints`, stage space) | joint radii × `key_scale` |
| Capsules | `graphics/src/depth_mask.rs:145-164` (`HandShape::from_joints`), joint pairs `SEGMENT_JOINTS` `:32` | 28 tapered capsules per hand, the tips pulled back by their radius. 6 wrist, 9 palm (`:28-29`) |
| Rim growth with speed | `stream.rs:903-930` (speed of the wrist only, `key_speed 2`, grows at once, decays ×0.9), `depth_mask.rs:265-297` (`for_speed`: rim += speed × 45 ms ≤ 8 mm, sweep along the motion ≤ 40 mm, core shrinks speed × 10 ms) | |
| Solid weights | `stream.rs:940-967` | verified segments × stillness (0.08-0.25 m/s), plus `key_solid` (6 wrist segments always solid) |
| Leak narrowing | `stream.rs:979-1007`, `depth_mask.rs:301` `for_leak` | off by default (`key_leak 0`) |
| Bridge (off) | `stream.rs:1009-1068`, `depth_mask.rs:322-361` (`WristBridge`) | one more capsule from the wrist. `key_wrist_len 0` since 2026-11-23 |
| Limited to the marker box | `depth_mask.rs:383-443` `limit_hand_to_region` | capsule-end radii taper over ±2 cm at the box surface, also for the bridge |
| Into the uniforms | `depth_mask.rs:644-818` `depth_uniforms`: zones `:737-741`, solid `:742-744`, bounds `:745-750`, capsules `:751-758`, arms `:759-766`, key_lower rects `:786-791`, sweep `:792-796`, bridge `:797-814` | struct in `graphics/resources/depth_common.wgsl:45-95` |

### 1.2 The focus texture (`graphics/resources/focus_area.wgsl`)

- 1/5 of the view resolution (`graphics/src/stream.rs:75`). Rendered before the mask, because the mask
  reads it (`stream.rs:1328-1352`). Scissored to the hands' rect (`encode_focus_area`, `stream.rs:1767-1806`,
  scissor at `:1801`). Without hands it is only cleared.
- Per pixel, per hand: a bounding-sphere test (swept along the motion for a moving hand, `:167-176`). Near
  the hand, all 28 capsules (`:181-202`, `capsule_zones` `:110-140`, swept test `:68-106`). Then the bridge
  capsule with its own bound (`:204-233`).
- Channels (`:9-10`, output `:251`):
  - **r** = the hand with its rim. 1 inside, fading out over the outer half of the rim plus a 3 mm soft
    edge (`:244-247`). This is where the key may decide.
  - **g** = solid. Capsules with a solid weight, fading from `core` (−4 mm, `key_inset`) over `falloff` (4
    mm). Any colour is shown.
  - **b** = near a hand: bounding sphere + 3-6 cm (`:236-239`). The remembered playzone is used there
    (`depth_mask.wgsl:55-63`).
  - **a** = debug only: how much the rim is narrowed (`key_leak_view`).

### 1.3 How the stream shader combines them (`graphics/resources/stream.wgsl`, `depth_passthrough` `:123-207`)

- `mask = smoothstep(0.05, 0.35, depth_mask.r)` (`:131-132`, edge constants `:120-121`).
- Key path (`:144-191`): `solid = max(mask, focus.g)`, `lower = 1 − solid`, `upper = 1 − max(solid, focus.r)`,
  packed `alpha = 0.5 (lower + upper)` (`:181`).
- Layers, bottom to top (`client_openxr/src/lib.rs:936-958`):
  1. passthrough
  2. lower game layer, alpha scale/bias 2a (`client_openxr/src/graphics.rs:100`)
  3. keyed passthrough, with the LUT (XR_META_passthrough_color_lut)
  4. upper game layer, 2a − 1 (`graphics.rs:101`)
- Without hands, only layers 1 and 4 (or 2 uncropped). `key_layers`, `extra_layers` `passthrough.rs:321-327`,
  hold 0.3 s.
- The packing is exact only when `upper = 0` or `lower = 1`. Both partial rounds towards "key decides".
- **The key never removes anything.** Where the lower layer is open (mask or solid), any colour shows.
  Where only the upper layer is open (rim), layer 3 shows the colours the LUT keeps, and the lower game
  layer covers the rest.
- **The rect is one rect per eye, the union of both hands, not one per hand.**
  - `hands_view_rect` (`graphics/src/depth_mask.rs:845-912`) covers all 28 capsules of both hands with rim
    + 1 cm, both ends of the sweep, the bridge, and the 6 cm playzone sphere.
  - It is used three times:
    - the focus scissor;
    - the crop of the lower game layer (`client_openxr/src/stream.rs:1505-1530`, `crop_view` `depth_mask.rs:917-969`, `KEY_CROP_MARGIN` 3 %): a projection view with a smaller imageRect and the matching fov;
    - `key_lower` in the uniforms, so the shader rewrites alpha outside the rect: `upper + (1 − upper) lower` (`stream.wgsl:182-189`).
  - It is a rect because a projection layer's sub-image is a rectangle, and the compositor's cost of a
    layer scales with its area.
  - The keyed passthrough (layer 3) is always full view. The upper game layer is always full view.
- Consequence for any keyed zone: **it must lie inside the lower rect.** Outside it there is no lower game
  layer under the keyed passthrough, so an open upper layer shows the whole room.

### 1.4 How the colour tables are learned and verified

- Learner thread: `client_openxr/src/auto_color_key.rs`, `learn_loop` `:215-652`, one pass every 60 ms, LUT
  every 180 ms (`:29-30`). Model and tables: `graphics/src/color_model.rs`.
- **Hands at the image time:** `sample()` `auto_color_key.rs:971-1163` locates the joints for
  `frame.timestamp_ns` (`:981`, `:1013-1017`), a past time. The bursts measured these joints as accurate.
- **Sample classes** (`color_model.rs:733-890`):
  - skin: camera pixels inside the palm (−5 mm), and the fingers of a still hand (−4 mm). Only from fully
    tracked hands slower than 0.5 m/s (`auto_color_key.rs:1115`).
  - background: the ring 2-7 cm around the hand capsules (`:875-887`). Excluded:
    - the forearm, as the **half-space** behind the wrist plane −3 cm along `arm_direction` (`:877-879`);
    - the other hand.

    `arm_direction` is opposite to the fingers, or the bridge direction (`auto_color_key.rs:1061-1073`).
    KB point 6: this empties the ring when the arm points along the line of sight.
  - scene: every 8th pixel, excluding the hands and a **50 cm × 10 cm tube** along the same `arm_direction`
    (`:766-787`).
  - Memories: skin 5 s (past 45 s, ×0.5), background 3 s (past 60 s, ×0.4), scene 20 s (×0.5)
    (`color_model.rs:11-21`).
- **"Background"** means what is around the hand in the image (the ring), plus everything else in view
  (the scene). The LUT keeps a colour by the likelihood ratio skin : max(background, scene). Each class is
  reduced by the other with contamination 0.25. A floor scales with the typical skin density × `key_tight`.
  Steepened, blended 35 % per update (`update_lut` `color_model.rs:208-302`, target `:255-260`).
- **Confirm table** (`key_confirm` 12 %): the same formula with a lower floor, never uploaded, never
  stricter than the key (`:239-283`).
- **Verification** (`segment_shares` `:309-357`): 15 points per segment.
  - The axis points are checked with the confirm table, the points beside the axis with the key table.
  - `verification_weight = reliability × smoothstep(0.5, 0.8, share)` (`:496-498`).
  - reliability = `1 − smoothstep(0.35, 0.65, background kept by the confirm table)` (`:490-492`).
  - Weights drop at once and rise slowly (`auto_color_key.rs:410-446`). They are published via `Verified`
    (`:481-486`) to the render thread (`stream.rs:830-833`).
- **Leak:**
  - per hand: the share of the hand's ring kept by the key, held 1 s (`auto_color_key.rs:359-372`), for
    `key_leak`.
  - leak map: `graphics/src/leak_map.rs`, colours per direction, about 2° cells. It skips pixels behind a
    hand (sphere + 3 cm + 80 ms) and the forearm (6 spheres along 50 cm, 10 cm radius, `leak_map.rs:185-204`).
    Off by default.
- One LUT is published (`auto_color_key.rs:559-570`, only on a change > 10/255) and goes to the compositor
  (`passthrough.rs:66-73`).

### 1.5 What each part costs (KB "GPU budget", measured, SteamVR home, 72 Hz)

| State | GPU load | App GPU time |
|---|---|---|
| Passthrough off | 29 % | 2.9 ms |
| Depth mask at 1/4 resolution, no key | 48-56 % | 4.3 ms |
| 83a11bc7, no hands (2 layers) | 76 % | about 7 ms |
| 83a11bc7, hands in view (4 layers, cropped lower layer) | 80 % median, peaks 96 % | about 7 ms |
| 3609efde, no hands, 4 full-size layers: key on → off | TW 4.69 → 2.0 ms, load about 85 → 62 % | barely changed |
| 11654b86, body-tracking forearm (capsules tested for all 9 depth samples) + playzone map | 88-99 %, 58-73 fps | 9.5 ms |

- The extra layers are compositor time (TW), not app time.
- Cropping the lower layer to the hands brought the four layers to about +4 points over two.
- The passthrough camera adds 0.3 ms.
- With the marker box (34a0ea96): VrApi showed 95 % at 65 °C (not looked into).

### 1.6 How `arm_shift` moves the depth today

- CPU, `client_openxr/src/stream.rs:757-815` (`arm_motions`), called at `:1348-1350` with the capture time
  of the depth frame.
  - Wrist "now" = `locate_hand_joints(display_time − key_delay)` (`:773-776`, `:787-800`): the laggy newest
    estimate. Wrist "then" = the same at `capture_time` (`:801`), which is accurate.
  - Displacement = now − then, dropped above 25 cm (`:765`, `:802`).
  - Direction = towards a fixed elbow guess, head + yaw × (±0.2, −0.5, 0.05) m (`:767`, `:806-811`).
    Measured: 14-51° (median per burst) off the forearm axis in the depth.
- GPU, `graphics/resources/depth_mask.wgsl`:
  - the surface search starts at the arm if the ray passes within 14 cm of it (`initial_distance`
    `:209-239`);
  - each lookup is inverse-warped: `project_to_depth(point − displacement)`, `hit + displacement`
    (`:117-137`, `:175`, `:184`, `:196`).
  - Weight (`arm_displacement` `:246-276`): `(1 − smoothstep(0, 0.45 m, along)) × smoothstep(hand_edge, along)`
    `× (1 − smoothstep(8.4, 14 cm, aside))`. The last factor takes the distance from the axis anywhere
    between its old and new position. `hand_edge` with the key is −3 to +2 cm (`:249-252`).
- **Which texels count as arm:** every surface within 8-14 cm of the guessed axis, up to 45 cm from the
  wrist, including the desk or the thighs under a resting forearm. There is no segmentation.
- Offline (agent report 1, re-checked in report 4):
  - today's weight with an ideal wrist gives 12.4 / 6.1 / 7.6 mm edge error (bursts 8 / 2 / 6);
  - the linear 30 cm weight gives 7.6 / 4.1 / 6.5;
  - with the display-time joints as the target: 27.9 / 10.5 / 11.9.

### 1.7 Everything a new zone can reuse

- The bridge slot: uniforms `hand_bridge` / `hand_bridge_end`, its focus code, its rect entry
  (`hands_view_rect` `.chain(&bridge)`) and its box limit. `key_wrist_len` 0 leaves it unused.
- The burst depth readback: `depth_dump.wgsl` and `BurstReadbacks` (`graphics/src/stream.rs:1136-1157`,
  `depth_burst.rs:45`, 6 in flight). It runs on the headset: the 2026-11-24 bursts recorded every depth
  frame for 3 s, and bursts 2, 3, 7 had no display hitches.
- The learner's past-time joints, and its `Verified` hand-off.
- `ColorModel`'s density / target machinery.

## 2. Why the two earlier forearm attempts were removed

**A. The forearm from Meta's body tracking** (bb1f7ac6, cost-reduced in 1d328e2c, removed in 118e0f87; KB
"What doesn't work" and GPU table).
- What it did:
  - forearm capsules from the wrist to the body API's estimated elbow, keyed like the hand (`key_arm_rim` 8 mm);
  - solid where the camera confirmed it;
  - where a quarter of the forearm was confirmed, the forearm was removed from the depth map (`arm_removal`), so the arm then came from the key and the solid fill alone.
- Failure 1, **geometry from an estimate, not a measurement:** with the arm stretched out or the wrist bent
  down, the elbow estimate was off. The zone lay beside the real forearm (observed on the headset). Two
  things followed:
  - the key showed the room inside the misplaced zone: a large patch of skin-coloured parquet, the halo;
  - the arm's own depth was removed where the zone claimed it.
- Failure 2, **cost:** the forearm test ran for all 9 depth samples of every mask texel, and the hand pass
  covered a bigger area. Result: 58-73 fps, 88-99 % load, 9.5 ms. The cheaper version (1d328e2c) was never
  measured before removal.
- **What the new design must not do:**
  - take the zone's direction or position from a model guess;
  - remove the arm's depth because a zone claims it;
  - test arm geometry per depth sample.

  The new zone comes from the arm's own depth (fitted), and the depth is only handed to the key in a
  band where the depth texel is itself arm. The arm test is one test per mask texel.

**B. The keyed bridge from the wrist** (9383baa6, off by default since 3e5ae77c).
- What it did: one capsule from the wrist joint, 80 mm, the wrist radius. Its direction was chosen by the
  camera among 21 candidates by the share of key-kept colours along each. It was keyed like the hand, and
  solid when confident.
- Failures:
  - **"a tail of the skeleton that messes up where the arm continues"** (user): a fixed-length stub in a
    colour-chosen direction did not agree with where the depth showed the forearm (late, from 3 cm behind
    the wrist). The user saw two different arms meeting at an angle.
  - **its solid fill showed any colour.** It gave an estimated 16-23 % of the room seen next to the hand.
  - the colour scoring could follow skin-coloured backgrounds (the `key_wrist_follow` guard existed for
    this).
  - its direction fed the ring exclusion. In 4 captures, a direction turned towards the camera emptied the
    ring of background colours.
- **What the new design must not do:**
  - draw a zone that is not the same object as the depth arm;
  - make solid fill from geometry;
  - choose geometry by colour;
  - let an uncertain direction change what the learner calls background.

  The new zone is fitted to the depth arm and moved by the same law as the depth. Its core is the depth
  itself. Its direction comes from depth. The learner uses it only when the fit is good.

## 3. Design

### 3a. The zone: "where is the arm now", cheaply

**Candidates.**

| | (i) moved depth texels as the zone | (ii) capsule fitted to the depth arm, moved with the wrist |
|---|---|---|
| Source | arm label per depth texel (segmentation on the GPU), warped like `arm_shift`, dilated by a margin | CPU fit of the newest depth frame (read back, one eye): axis, radius at the wrist and at 18 cm, quality |
| Shape | exact silhouette (sleeve folds, cuff), but with the depth's jagged edge and the runtime's fill | tapered cylinder. Offline below: at margin 0 it already holds 81-90 % of the arm with 3-5 cm² of excess |
| The gap at the wrist (hand removal: arm missing 0.6-2.6 cm behind the wrist, 5-8 cm at grips) | not covered: the zone has the same hole as the depth | covered: the capsule starts at the wrist joint |
| Arm resting on desk or thighs | segmentation leaks into what the arm touches. The zone covers the desk | the tube constraint and the fit-quality gates reject it, then fallback |
| GPU per frame | label pass (connectivity is hard on GPU) + 9 label taps per mask texel (the cost pattern of failure A) + a dilation pass or wide taps in the full-res stream pass | one 320×320 readback pass per depth frame + one capsule test per focus pixel near the arm + one point-in-capsule test per mask texel near the arm |
| CPU | none | readback (400 KB per depth frame, mapped 1-2 frames later) + the fit (< 1 ms, inferred; in a worker) |
| Failure A (zone beside the arm) | cannot happen except by timing | could happen with a bad fit. Mitigated by gates, by smoothing, and by R |
| Failure B (tail, second arm) | cannot happen | cannot happen: same object, same motion law |
| Usable by the learner (sleeve samples at the camera's time) | no (GPU only) | yes: the model is on the CPU and moves to the image time with past-time joints |

**Pick (ii).** It is cheaper on the GPU, it covers the wrist gap that today cuts the arm next to the hand,
it gives the learner the arm, and it reuses the bridge plumbing.

**Offline check of (ii)** (`logs/analysis-20261124-arm-mask/zone_check.py`, output `zone_check.txt`).
- Method:
  - bursts 8 (wave, arm at 0.27 m), 2 (slower), 6 (wheel); left eye; depth frame pairs 60-200 ms apart;
  - capsule fitted to frame k: silhouette half-width as the radius, the centre line pushed back from the visible surface;
  - moved with the linear weight to frame j;
  - truth = the arm segment of frame j, 4-15 cm behind the wrist.
- Two targets:
  - "ideal": the wrist at capture j, from the camera-record spline;
  - "pred": the history up to 60 ms before, extrapolated at constant speed.

| burst (pairs) | target | margin 0: arm inside / excess | margin 15 mm | margin 20 mm | core 15 mm inside: share that is arm (p10) |
|---|---|---|---|---|---|
| 8 (131) | not moved | 0.56 / 44 cm² | 0.75 (p10 0.35) / 73 | 0.81 / 81 | 0.74 (0.21) |
| 8 | ideal | 0.82 / 4 | **0.98 (p10 0.91) / 32** | 1.00 (0.96) / 47 | 1.00 (0.95) |
| 8 | pred | 0.82 / 6 | 0.99 (p10 0.79) / 34 | 1.00 (0.85) / 50 | 0.99 (0.68) |
| 2 (119) | ideal | 0.86 / 5 | 0.99 (p10 0.93) / 49 | 1.00 (0.96) / 70 | 1.00 (0.87) |
| 2 | pred | 0.85 / 9 | 0.99 (p10 0.93) / 51 | 1.00 (0.96) / 71 | 1.00 (0.88) |
| 6 (228) | ideal | 0.88 / 47 | 1.00 (p10 0.82) / 94 | 1.00 (0.82) / 109 | 0.81 (0.48) |

- Excess is measured over the 11 cm stretch 4-15 cm behind the wrist.
- Read as:
  - a capsule moved with an accurate wrist holds 98-99 % of the arm at a 15 mm margin. The excess is about
    1.5 cm on each side, 30-50 cm² per 11 cm of arm;
  - unmoved, it holds only 56-75 %. So the zone needs the motion correction;
  - the extrapolated target costs little at the median but more in the worst tenth (p10 0.79 in burst 8).
- Burst 6's excess and core share are pessimistic: with the hands on the wheel, the depth arm is missing
  in about half of the texels 0-8 cm behind the wrist (gap.txt), and pieces of the wheel enter the
  segment. So the truth is incomplete there.
- Caveats:
  - the same segmentation makes the fit and the truth;
  - the left eye only;
  - the truth is the later depth frame, not the camera image;
  - the "pred" history is the 14 Hz camera-record spline, not the runtime's 72 Hz history.

**Defaults from this:** `arm_margin` 15 mm, `arm_inset` 15 mm (the core covers about half of the arm),
keyed length 18 cm.

**The fit, per new depth frame, left eye** (the recipe of `analysis-20261124-bursts/arm/armseg.py`, which
found the arm in bursts 8, 2 and 6, failed in 7 where the arms merge with the thighs, and caught pieces of
the wheel or legs in a few frames of 6):
1. Inputs from the render thread when the readback is asked for:
   - the capture time t_c;
   - the depth view pose and tangents, near / far;
   - per hand: the wrist and the middle metacarpal located at t_c (a past time, accurate).
2. Window: the texels within 30 cm of the wrist's projection (a box in the image), converted to metres and
   to 3D.
3. Depth-connected pieces: 4-neighbours join if |Δz| ≤ max(3 cm, 6 % z). Far fill and no-data break them.
4. The arm piece is the piece with the most texels 4-18 cm from the wrist, counting only texels more than
   2 cm behind the wrist plane (normal: wrist → middle metacarpal). That excludes the wheel rim in front
   of the hand and a held object.
5. Keep texels 3-25 cm from the wrist. Fit a 3D line (PCA). Keep texels within 6.5 cm of it. Fit again,
   twice.
6. Cross-sections every 2 cm from 3 to 18 cm:
   - sideways extent as seen from the depth camera (3rd-97th percentile) → half-width;
   - centre = the middle of the extent, pushed back from the visible surface by about the half-width
     (0.8 × in the check).

   A line through the centres gives the axis, and a linear fit of the half-widths gives r0 (at the wrist)
   and r1 (at 18 cm), clamped to 20-70 / 20-80 mm.
7. Gates:
   - at least 150 texels and elongation > 1.3;
   - the line passes within 3 cm of the wrist;
   - r0 and r1 within the clamps without clamping by more than 1 cm;
   - the direction within 75° of "opposite to the fingers";
   - a change of more than 30° from the last accepted fit within 200 ms needs two agreeing fits in a row.

   Measured from frame to frame: 8-16° (p50 / p90, burst 8), up to 37-40° p90 in bursts 5 and 6.
8. Smoothing:
   - the direction is low-passed (30 % per fit), after moving the old fit to the new t_c with the motion law below;
   - the radii are low-passed (20 % per fit, faster when growing).
9. Output `ArmModel { t_c, wrist_c, start_c, far_c, r0, r1, quality, points, reason }`.
   - `start_c` is the axis point at the wrist's level;
   - `far_c = start_c + 0.30 m × axis` is the point the linear weight leaves fixed.

**The motion law (one for everything).** A model taken at t_c is moved to any time t with the wrist
displacement D = wrist(t) − wrist(t_c):
- start_t = start_c + D, far_t = far_c;
- axis_t = normalize(far_t − start_t), radii unchanged.

This is the linear weight 1 → 0 over 30 cm applied to the axis. The same law moves the depth texels in the
mask pass, so zone and depth agree by construction. Uses:
- t = the capture time of the depth frame the mask uses (it may be one frame newer than the fitted one);
- t = the display target (3b);
- t = the camera image's time (learner, 3d).

**Arm texels versus desk, wheel and thighs, live:**
- In the fit: depth connectivity, behind the wrist plane, the 6.5 cm tube, and the quality gates.
  - A forearm resting on the desk: the piece can include the desk. The tube cut and the cross-section
    percentiles keep the radius honest. A radius above 70 mm or a poor line is rejected.
  - Arms on the thighs (burst 7): rejected.
  - A wheel rim near the wrist is mostly removed by the runtime (3-5 cm around the fingers); a piece
    beside the forearm enters only within the tube.
- In the mask pass (GPU): a mask texel is "arm" if the raw depth point of its centre sample (before
  displacement) lies within the capsule at the depth frame's capture time, with radius r(s) + 1 cm and
  hand_edge ≤ s ≤ 30 cm. The weight is soft over the last 1 cm.
  - This is the "within a tube" test, at the fitted radius. The desk under the arm lies farther from the
    axis than r + 1 cm. A rim beside the arm stays outside unless it touches the arm's surface.
  - "Nearer than its surroundings" is already part of step 3 (a depth break separates the arm from what
    is behind it). It is not repeated per texel.
- **Fallback when there is no good fit:** use the last accepted model while it is younger than 300 ms,
  moved by the law above. After that, the arm is drawn like `arm_mask 0` (today's shift with the head
  guess), and there is no keyed zone.

### 3b. The wrist target

- Measured:
  - the joints for the display time are the newest estimate, 44-76 ms behind the real wrist, with a 4-5
    cm overshoot at reversals;
  - joints looked up for a past time (the learner's image times) sit on the hand;
  - a constant-speed extrapolation over 60 ms from the history gave 5.6-8.3 mm against 4.4-7.6 mm with an
    exact target (report 1).
- **Target time T = display − λ**, λ = the passthrough latency (camera to photon). The keyed edge shows the
  arm at T, so the zone and the depth core must show it at T too.
- λ is not measured. Inferred range 13-45 ms: Road to VR put hand tracking 31 ms behind the passthrough
  image, and the newest estimate is 44-76 ms old. Default `arm_target_ms` = **40**: the larger end means
  less extrapolation and less overshoot.
- **Lookup:**
  - H = display − max(`arm_history_ms`, λ), default `arm_history_ms` = 80. This is older than the
    runtime's measured lag of up to 76 ms, so it lies in the accurate history.
  - wrist(H) and wrist(H − 30 ms); v = their finite difference / 30 ms. The reported joint velocities are
    1.5-2× the finite differences, measured, so they are not used.
  - wrist(T) = wrist(H) + v × (H → T), the step capped at 6 cm.
  - D_target = wrist(T) − wrist(t_c) of the model (and of the depth frame used).
- **Where:** `client_openxr/src/stream.rs` `arm_motions` (`:757-815`). It replaces the lookup at
  `display_time − key_delay` (`:773-776`, `:800`). The capture-time lookup (`:801`) stays.
  - Cost: one extra `xrLocateHandJointsEXT` per hand per frame (H, H − 30 ms, t_c instead of now, t_c).
  - The same function also gives the target to the zone (3c) and the burst records.
- **Sweep instead of a perfect λ:** the zone grows along the wrist velocity by ±`arm_sweep_ms` (20 ms) ×
  speed, at most 30 mm on each side. It uses the existing swept-capsule test with the hand's sweep
  direction (`hand_sweep[hand].xyz`, the wrist velocity with `key_speed 2`) and its own lengths. The core is
  not swept.
- **Not changed:** the hand capsules keep the display-time joints (the user is happy with the hands).
  After this build the arm may be more on time than the hand during fast moves. The log reports the gap
  between the hand's wrist joint and the arm's target (3f), to decide later whether the hands should move
  to the same target.
- **Measuring λ (not tried):** screenshots (`screencap` contains the passthrough, KB "What works") during a
  fast wave with a burst running. The hand position in the screenshot against the history gives λ. Live:
  `arm_target_ms` 20 / 40 / 60 while waving; the user says whether the arm's edge leads or trails.

### 3c. The key inside the zone; layers and rects

- **Table:** the skin table already applies everywhere the upper layer is open. A bare forearm needs
  nothing new. Sleeves need 3d.
- **The zone in the focus pass** (`focus_area.wgsl`, in place of the bridge block `:204-233`), per hand:
  - capsule A (zone): start_T (r0 + margin) → end_T at `arm_key_len` along the moved axis (radius r(L) +
    margin). Swept by the arm sweep.
  - `Z = 1 − smoothstep(−SOFTNESS, 0, dist_A)`, written as `r = max(r, Z × R)`. R is the arm's reliability,
    0..1 (3e).
  - core: the same capsule unswept, `C = 1 − smoothstep(−d_c − SOFTNESS, −d_c, dist_A0)` with
    d_c = margin + inset (the core is `inset` inside the fitted surface).
  - band handed to the key: `S = R × Z × (1 − C)`, faded to 0 over the last 3 cm of `arm_key_len`. It is
    written to **a** in mode 2. `a` is debug-only today (the narrowed-rim tint of the untested
    `key_leak_view`); in mode 2 that tint shows the band instead.
  - No geometric solid: `g` is untouched (lesson B).
  - Cost per focus pixel, per hand: a bound test (about 15 operations, two square roots), and near the
    arm one swept and one plain capsule test (about 60 operations). No texture reads.
- **Uniforms:**
  - `hand_bridge[2h]` = start_T, r0 + margin;
  - `hand_bridge[2h+1]` = end_T, R (the bridge's "solid" slot);
  - `hand_bridge_end` = end radii + "has arm";
  - **new, appended:** `arm_key: array<vec4f, 2>` = (sweep back, sweep ahead, d_c, mode).
  - mask pass: `arms[3h].w` = r0, `arms[3h+1].w` = r1, `arms[3h+2].w` = the weight length (unused w
    components today).
  - `arms[3h].xyz` becomes the target wrist, `arms[3h+1].xyz` the moved axis direction (instead of the
    elbow guess). Displacement as today, but to the target.
  - The block grows by 32 bytes (`DEPTH_UNIFORMS_SIZE` assert, `depth_mask.rs:592-607`).
  - The bridge (`key_wrist_len`) and the arm zone exclude each other: with `arm_mask` 2 the arm takes the
    slot, logged once if both are set.
- **Rects:** the arm capsule enters `hands_view_rect` through the bridge's `.chain(&bridge)`, with both
  sweep ends. So the focus scissor, the lower-layer crop and `key_lower` grow by themselves.
  - Inferred size: 18 cm of forearm at 40-50 cm reaches about 20° beyond the hand, mostly down and
    outwards. While driving, the union of both hands already spans the width between them, so the rect
    grows mostly downwards. A guess is 1.5-2.5× the area.
  - Not known: the compositor's cost per area of the lower layer.
  - Bounds from measurements:
    - full-size lower + keyed layers about 23 points of load (85 → 62 %, 3609efde);
    - with the crop, the hands' four layers about +4 points (76 → 80 %).
  - Kept small by:
    - only 18 cm keyed (beyond that the depth is late by little: the weight goes to 0);
    - no arm zone in the rect while R = 0;
    - per-hand rects are not proposed: two lower layers cost a layer's fixed overhead, worth it only with
      the hands far apart.
  - The rect area with and without the arm zone is logged (3f) and judged against the GPU numbers.
- **Marker box:** `limit_hand_to_region` (`depth_mask.rs:383-443`) already tapers the bridge at the box
  surface. The arm zone gets the same, so a keyed arm never shows outside the box, like the depth arm.

### 3d. Sleeve colours: the class "arm by depth"

- **Separate densities** in `ColorModel`: `arm` (5 s, past 45 s × 0.5, like skin) and `arm_background`
  (3 s, past 60 s × 0.4). They are not merged into skin:
  - the tightness floor is relative to the skin's typical density, and a sleeve would widen it;
  - `segment_shares` and the confirm table must keep judging hands by skin only (a sleeve colour must not
    confirm a finger);
  - the arm needs its own gates.
- **One uploaded LUT:** `published = max(skin_alpha, gate × arm_alpha)`.
  - `arm_alpha` uses the same `target()` with skin := arm, background := max(arm_background, background,
    scene) minus arm × 0.25, and a floor from the arm's typical density × `key_tight`.
  - `alpha` (skin) stays the internal key table for verification.
  - Key dumps add `_arm_alpha.lut` and the counts.
- **When it may learn** (all of these):
  - `arm_mask` 2 and `arm_sleeve` 1;
  - a model accepted within 100 ms of the camera image, moved to the image time by the law in 3a (both
    lookups in the past);
  - the wrist slower than 0.25 m/s at the image time (finite differences);
  - the hand fully tracked.
- **Arm samples:** camera pixels whose ray passes through the capsule core (radius − max(15 mm, 0.4 r)),
  5-20 cm from the wrist, at least 2 cm from the hand shape (`ray_distance` ≥ 0.02), not within the other
  hand or arm. Every 2nd pixel, as for the hand.
- **Arm background:** pixels 2-6 cm outside the capsule surface over the same stretch, excluding the hand
  (+2 cm), the other hand and arm, and the core of anything keyed.
- **Kept from learning the background:**
  - core only, slow only, fresh fit only (a stale or bad fit gives no samples rather than wrong ones);
  - the contamination subtraction against `arm_background`;
  - the scene sampler and the ring exclusion use the fitted capsule + 3 cm instead of the 50 cm guessed
    tube and the half-space (`color_model.rs:775-783`, `:877-879`), else sleeve pixels land in the scene
    and fight the arm class. This also fills the ring when the arm points along the line of sight
    (KB point 6). It applies only while the model is good, otherwise the old exclusions apply;
  - the leak map's `arm_exclusions` take the fitted direction too.
- **The merge gate** (one LUT also acts on the hand rims), all of these:
  - at least 300 arm samples;
  - `arm_alpha` keeps ≥ 0.6 of the arm samples and ≤ `arm_leak_hi` of the arm background;
  - the merged table keeps no more of the hand rings than the skin table + 2 points;
  - ≤ 5 % of the scene.

  Re-checked at every LUT update. Gate off: `published = skin_alpha`, and the arms rely on skin and depth
  only.

### 3e. Where the key cannot decide: the moved depth, per pixel

- **R per arm** (learner, every pass, published with `Verified`):
  - `R = (1 − smoothstep(arm_leak_lo, arm_leak_hi, bg_kept)) × smoothstep(0.5, 0.8, arm_kept)`. Both shares
    are measured with the *published* table on the arm samples and arm background of this pass. The
    gates are looser than for learning: speed < 0.5 m/s, model ≤ 150 ms.
  - It falls at once and rises 30 % per pass. It is held 1 s without data, then 0.
  - The render thread turns R into 0 / 1 with hysteresis (on > 0.6, off < 0.4), faded over 0.15 s. Fades
    only during switches, because the packed alpha rounds when two partials meet.
- **The mask pass** (`depth_mask.wgsl` `current_mask`):
  - arm displacement: the weight becomes linear, `clamp(1 − along / weight_len, 0, 1)`, along the moved
    axis, with `aside < r(s) + 2 cm` (smooth over the last 1 cm) instead of 8.4-14 cm. `initial_distance`
    uses the same axis and radius.
  - new output: `A = coverage × arm(center hit)` (3a), one point-to-segment distance per texel, only when a
    model exists and the ray passes within r + 3 cm of the axis.
  - Channel: **b** takes A. "Has depth" moves into g: g = proximity with 0 meaning no depth, at least
    2/255 where there is depth. Only the ShowDepth debug view reads b and g today (`stream.wgsl:134-142`).
    The dump format note changes (mask b, focus a).
- **The stream shader**, key path (`stream.wgsl:144-157`), mode 2 only:
  - `handed = mask_sample.b × hand.a` (the depth arm in the band, times S)
  - `mask = smoothstep(0.05, 0.35, mask_sample.r − handed)`
  - the rest as today: `solid = max(mask, hand.g)`, `upper` includes `hand.r` (which includes Z × R).
- **What each region shows:**
  - core: the depth (any colour, folds, a watch);
  - band:
    - over the arm: only key-kept colours (the arm, not its late or coarse depth outline);
    - beside the arm: the depth outline that lies off the arm is handed to the key and becomes game;
    - over a desk or wheel in the play area next to the arm: the mask stays (it is not arm);
  - outside the zone: the moved depth.
- Packed alpha in the band: lower = 1 − mask (may be partial), upper = 0, so it is exact. At the zone's
  outer soft edge: lower 1 (no mask there over the game), upper partial, also exact. Over in-area surfaces
  the lower layer is open (0). Only where the zone's soft edge meets a soft mask edge do both partials
  round, as for the hands today.
- **R = 0:** no zone in the focus or the rect, nothing handed. The arm is the moved depth of mode 1.
- Without `auto_key` there is no R, so mode 2 acts as 1 (logged).
- In the plain path (no colour key), mode 2 acts as 1.

### 3f. Properties, log lines, what to record

**Properties** (`debug.alvr.`, polled like the others, `passthrough.rs` `ColorKeyParams` / `StreamContext::poll_properties`):

| Property | Default | Meaning |
|---|---|---|
| `arm_mask` | 0 | 0: today (`arm_shift` decides). 1: new depth correction: fitted axis and tube, past-time wrist, linear weight, also without the key. 2: + keyed zone and band (needs `color_key`, R needs `auto_key`; else as 1, logged) |
| `arm_mask_hands` | 3 | 1: left arm only, 2: right only, 3: both. The other arm stays as `arm_mask 0`, for side by side |
| `arm_target_ms` | 40 | λ: the arm is placed for display − this |
| `arm_history_ms` | 80 | wrist looked up at display − max(this, λ), extrapolated to the target |
| `arm_weight_len` | 300 mm | length of the linear weight (measured best 26-35 cm) |
| `arm_key_len` | 180 mm | keyed part of the zone from the wrist |
| `arm_margin` / `arm_inset` | 15 / 15 mm | zone beyond the fitted surface / core inside it |
| `arm_sweep_ms` | 20 ms | zone growth along the wrist velocity, each side, ≤ 30 mm |
| `arm_sleeve` | 1 | learn the arm class (mode 2) |
| `arm_leak_lo` / `arm_leak_hi` | 10 / 25 % | arm background kept by the table: where R starts falling / is 0 |

Recommendation: **default 0** in the first build. Both earlier forearm changes were rejected by the user's
eye, so the user switches 0 / 1 / 2 live, and with `arm_mask_hands 1` sees the old and the new arm at the
same time. After the verdict the default becomes the winner. `arm_shift` stays for mode 0.

**Log lines** (`ALVR_CAM`, prefix `arm:`, every 2 s and on changes):
- `arm: mode 2 (left only), target display-40 ms from history -80 ms, weight 300 mm, key 180 mm, margin 15, inset 15, sweep 20 ms`
  when properties change.
- Per hand:
  `arm: L fit ok 512 pts, r 33/41 mm, axis 18 deg from the old elbow guess, model age 47 ms (1 frame), resid 4 mm; rejected 3 (few 1, wide 0, jump 2, lap 0); target: extrapolated 12 mm at 0.31 m/s, hand-to-arm wrist gap 9 mm (p90 21); R 1 (arm kept 0.86, bg kept 0.04, n 640/410)`.
- `arm: rect area of the lower layer L/R eye 18/17 % of the view, 27/26 % with the arm zones` (both computed
  on the CPU, the second only while used), `arm: readback 0.3 ms, fit 0.4 ms (p90 over 2 s)`.
- Learner `auto key:` line: `, arm L n 640 kept 0.86 bg 0.04, R ...; sleeve merged yes|no (<which gate>)`.

**Bursts and dumps** (to judge it offline as on 2026-11-24):
- Display records:
  - `arm_mode`;
  - per hand: `arm_model` (t_c, depth index, start_c, far_c, r0, r1, quality, points, reason);
  - `arm_target_ns`, `arm_wrist_target`, `arm_extrapolated_mm`, `arm_capsule` (start, end, radii, sweep,
    exactly as sent), `arm_R`;
  - `arms` keeps wrist / direction / displacement, now to the target.
- Camera records: the model moved to the image time, the arm and arm-background sample counts, and R's
  two shares.
- The depth dump JSON `arms`: the same fields. Mask b = arm share, focus a = handed band (mode 2).
- The key dump: `_arm_alpha.lut`, and counts `arm`, `arm_background`, `arm_kept`, `arm_bg_kept`.
- **What to record** (bursts, 3 s each, both modes or with `arm_mask_hands 1`): the four motions of today
  (wave as b8, slow arm as b2, the cardboard wheel as b6, still on the lap as b7), plus a long sleeve, plus
  an arm over the skin-coloured parquet / cardboard (the halo case of failure A).
- **Measures:**
  - zone: containment and excess against the camera arm outline;
  - share of the band's arm pixels kept by the LUT;
  - halo: background pixels in the zone kept by the LUT;
  - core precision;
  - fit rejection rate per motion;
  - extrapolation error against the later camera-record wrist.

  GPU: VrApi load, App and TW times with modes 0 / 1 / 2 and `key_crop` 0 / 1 (the full-size bound), hands
  in view, several runs each.

### 3g. Data flow and cost

1. **Render thread, each frame:**
   - `arm_motions`: lookups at H, H − 30 ms, t_c. It takes the latest `ArmModel`, moves it to t_c of the
     depth frame used and to T, and fills `ArmMotion` and the arm capsule in `ColorKeyInput`.
   - On a new depth frame (15-21 per second), it asks for a one-eye readback (the `depth_dump.wgsl` pass,
     the left layer only).
   - It polls finished readbacks and hands the u16 buffer, pose, tangents, near / far and the joints at
     t_c to the fitter.
2. **Fitter thread (new, small):** the fit of 3a, writes `Arc<Mutex<[Option<ArmModel>; 2]>>`.
3. **Learner:** reads the models, moves them to the image time, samples arm and arm background, gates,
   merges, and publishes R with `Verified`.
4. **GPU:** the focus pass (zone, band), the mask pass (linear tube, A), the stream pass (two operations).

| Part | Cost (in principle, not measured) |
|---|---|
| Readback | one 320×320 pass + 400 KB copy per depth frame; 400 KB read from mapped memory 1-2 frames later. Bursts did this for both eyes every depth frame without hitches |
| Fit | a window of about 120×120 texels, union-find, two PCAs, 8 cross-sections: well under 1 ms (with `CARGO_PROFILE_DEV_OPT_LEVEL=3`, see Pitfalls) |
| Joint lookups | +1 per hand per frame |
| Focus pass | +2 capsule tests per pixel near an arm, in a larger scissor rect (1/5 resolution, about 0.2 M pixels per eye) |
| Mask pass | 1 point-to-segment test per mask texel near an arm. Displacement unchanged in kind |
| Stream pass | 1 multiply, 1 subtract per pixel in the key path |
| Compositor | the lower layer's rect grows (3c). The real risk |

Budget levers if needed:
- shorter `arm_key_len`;
- no arm zone in the rect while R = 0;
- `playzone_auto 0`. The remembered copy was measured to be stale near the hands: 87 % of the wrong real
  beyond 6 cm. It costs a copy pass and lookups in the mask; its cost was never measured on its own.

### 3h. Build order (each step can be compared live)

1. **Mode 1:** the readback + fitter, the past-time target, the linear weight along the fitted axis, the
   fitted tube, the logs and burst fields.
   - Offline check on new bursts, then the user compares 0 / 1 per arm.
   - Useful on its own: the measured gain of the target and the weight was 2-4× on the edge error.
2. **Mode 2 without sleeves:** the zone and band in focus / mask / stream, R from skin only (`arm_sleeve 0`
   path), the rect growth logged. GPU measured.
3. **Sleeves:** the arm class, the exclusions with the fitted capsule, the merge gate.

**Files** (for the build plan):
- `client_openxr/src/stream.rs`: `arm_motions`, readback requests, the property poll;
- a new `client_openxr/src/arm_fit.rs` (or a pure `graphics/src/arm_fit.rs` with unit tests on synthetic
  depth);
- `client_openxr/src/auto_color_key.rs`, `graphics/src/color_model.rs`, `graphics/src/leak_map.rs`
  (exclusions);
- `graphics/src/depth_mask.rs`: `ArmMotion` fields, the arm capsule in `ColorKeyInput`, uniforms, rect,
  box limit;
- `graphics/src/stream.rs`: readback of one eye;
- `graphics/resources/depth_common.wgsl`, `depth_mask.wgsl`, `focus_area.wgsl`, `stream.wgsl`;
- `client_openxr/src/burst.rs`, `graphics/src/depth_burst.rs`, `depth_dump.rs`, `key_dump.rs`;
- `passthrough.rs` (properties); `docs/KNOWLEDGE_BASE.md`.

**Overlap:** the editor job has uncommitted changes in `depth_common.wgsl`, `stream.wgsl`, `depth_mask.rs`,
`graphics/src/stream.rs`, `client_openxr/src/stream.rs` and the uniform layout (4144 bytes). Start the build
after that job has committed, and re-read the line numbers then.

## 4. Risks, and what would make me stop

| Risk | Sign | Stop / fall back when |
|---|---|---|
| Layer cost of the grown lower rect | VrApi load, TW time, the rect area line | mode 2 adds more than 4 points of median load, or frames drop at 72 Hz with hands in view. Then shorten `arm_key_len` to 12 cm; if still over, drop mode 2 and keep mode 1 |
| Halo: background that looks like skin or sleeve shows inside the zone (failure A's parquet) | `bg kept` in the `arm:` line; the parquet / cardboard bursts; the user | R does not drop it within about 0.2 s, or the user sees patches. If R is 0 most of the time in the user's room, mode 2 brings nothing: stop it, keep mode 1 |
| Ragged band: the key keeps only part of the arm (it keeps about 0.79 of skin on the hand) | holes along the arm's edge, flicker | the user prefers the solid depth edge of mode 1. Then a larger `arm_inset` is not the fix (more band); go back to mode 1 |
| A second arm (failure B): band and core disagree | a ghost edge or a step along the arm | seen once by the user: stop and check the motion law / model age |
| Fit fails while driving (wheel or thighs merge into the arm piece, burst 6 / 7 style) | `rejected` counts | more than 30 % rejected with the hands on a wheel: the zone source is not reliable there. Rethink (e.g. the axis from the hand direction, width from depth) before going on |
| Unknown λ | the edge leads or trails in a wave | no λ of 20 / 40 / 60 looks right: measure λ (3b) before tuning anything else |
| Hand and arm on different targets | the `hand-to-arm wrist gap` p90; a step at the wrist | the gap p90 exceeds about 2 cm and the user sees it: a separate decision to move the hands to the history target |
| Sleeve class contaminates the one LUT | `background kept` / `leak` of the hands rise with the merge on | the hand ring kept share rises by more than 2 points: the gate should already block it. If not, stop the sleeve part |
| Readback or fitter load | the `readback` / `fit` timing line, frame timing | more than 1 ms on the render thread or visible hitches |
| The marker box | an arm keyed outside the box | must taper like the hand. If not, it is a bug, fix it before any test |

Things I would not do:
- a temporal stack of depth frames for the arm (report 4: it lags, 0.19 → 0.40 texels);
- a geometric solid fill on the arm;
- choosing the arm's direction by colour;
- a second keyed passthrough layer for a separate sleeve table.

## 5. Open for the user

- Long sleeves while driving, or bare forearms? This decides whether step 3 matters.
- Which edge is preferred: a coarse but solid depth edge, or a sharp keyed edge with some flicker (as on
  the hands)? The `arm_mask_hands` comparison is meant to answer this.
- A burst session with the arm over the skin-coloured floor and with a long sleeve, for the halo and
  sleeve checks.
