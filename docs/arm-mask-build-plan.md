# Arm mask: build plan

Status 2026-11-24. Step 1 is built (`arm_shift 2`, commit d6764b85) and not yet judged on the
headset; nothing else is. Everything after step 1 is replaced by `docs/mask-v2-design.md` (checked in
`docs/mask-v2-check.md`). It merges the design (`design/arm-mask/design.md`) and
the offline simulation (`logs/analysis-20261124-arm-mask/results.md`, `s7_summary.md`). An implementer should
not need either of them. Where they disagree, the simulation wins. Where the design proposes something the
simulation did not test, this plan marks it **untested**.

Line numbers are for `ALVR` at **fbe68b1b** (branch `auto-color-key`). The tree was clean when I read it: the
editor job has committed. Re-read the lines if more editor commits land first. Words as in the KB:
**measured** (headset or bursts), **offline** (computed on recorded data), **inferred**, **untested**.

## 1. Decision: build smaller

**Build step 1 now. Build the keyed zone later, in a reduced form, and only after the gates in 1.3 pass.**

### 1.1 What the numbers say

Offline wrong pixels are counted as cut + leak, in % of the true arm area, for bursts b8 / b2 / b6 (wave /
slower arm / cardboard wheel), at λ 49.5 ms.

| Mask | cut + leak | cut + halo (visible errors) |
|---|---|---|
| today (`arm_shift`, display-time joints, smoothstep to 45 cm) | 58 / 32 / 27 | 51 / 25 / 12 |
| moved depth, past-time wrist, linear 30 cm weight (**step 1**) | 35 / 20 / 19 | 28 / 14 / 8 |
| + keyed zone 2 cm, skin + learned sleeve table, key only **adds** outside the moved depth (**steps 2 + 3**) | 33 / 14 / 17 | 23 / 7 / 4 |
| + keyed zone, key decides a band both ways (the design's core/band) | 32 / 15 / 17 | 25 / 14 / 12 |
| keyed zone with the **skin table only** (key-only) | 88 / 100 / 63 | 85 / 100 / 61 |
| reference: depth with no delay | 16 / 18 / 12 | 11 / 11 / 5 |

- **Step 1 carries most of the gain.** Past-time joints plus the linear weight cut today's wrong pixels
  by 1.4-1.7×. The edge error falls from 10-31 mm to 4-8 mm (report 1: 7.6 / 4.1 / 6.5 mm with an exact
  wrist, 7.6 / 5.6 / 8.3 mm extrapolated 60 ms).
  - It costs no GPU time and needs no new pass.
  - Choosing past-time over display-time joints matters more than the key: 57 → 35, 31 → 20, 26 → 19. At
    λ 0 the gap doubles.
- **The key adds little on top: 2-5 points of cut + leak.** The simulation's noise between variants is 2-3
  points, so only b2's gain is clearly real.
  - It does better on the visible errors (cut + halo): 28 → 23, 14 → 7, 8 → 4.
  - But the truth was built partly from colour, which favours colour methods.
- **The skin table alone is useless on the user's arm.** All three bursts show a long plaid sleeve (b8 has it
  pushed up, with 26 % of arm pixels bare skin). A keyed arm needs the learned sleeve table (step 3).
- **The key can also make things worse.** Over a bright wall and dark screens (b8), the halo rises from 4 to
  10 % of the arm area. The keyed zone also grows the rect of the extra layers, and the GPU is already at
  78-95 %.
- **The keyed zone's likely main benefit was never scored.** The runtime removes the forearm 0.6-2.6 cm
  behind the wrist, and 5-8 cm with the hands on a wheel (19-47 % of samples). The simulation did not
  score the 3 cm next to the wrist. Only a headset test can show whether the zone fixes this gap.

### 1.2 What is dropped from the design

These parts of the design are dropped, because the simulation argues against them or does not support them:
- **The core/band split** (`arm_inset`, the mask pass's `b` = arm share, `handed` in `stream.wgsl`). The
  "key only adds" variant is equal on cut + leak and better on visible errors: 23 / 7 / 4 against
  25 / 14 / 12. It is also what the hand's rim does today: the key never removes anything.
- **The fitted tube and axis for the depth correction** in step 1. Report 1 found that the rotation point
  along the code's elbow guess, along the depth axis, or along the reversed hand direction gives the same
  result within about 1 mm.
- **Any uniform layout change in step 2b.** The bridge slot holds everything the reduced zone needs.
- **The sweep of the zone.** It was not tested. Add it only if bursts show the zone missing fast arms.

### 1.3 Gates before step 2b (the keyed zone is drawn)

- **G1:** after step 1, the user still sees a fault that the zone could fix: the arm cut next to the wrist,
  or a coarse or late arm edge.
- **G2:** a proxy measurement with no new code (step 2-pre) adds at most 4 points of median GPU load with
  hands in view, and no drops below 72 fps.
- **G3:** the step 2a fitter rejects at most 30 % of the frames with the hands on a wheel (its log line).
- **G4:** new bursts now carry the live colour table (added in step 1). Re-running `s4_sim.py` with that table
  still shows a lower cut + halo for the zone than for step 1 alone.

Step 3 (sleeves) is needed only if the user drives in long sleeves. All bursts so far suggest that.

## 2. Code facts this plan rests on (verified by reading, fbe68b1b)

| # | Fact | Where |
|---|---|---|
| F1 | The arm shift's "now" wrist is `locate_hand_joints(display_time − key_delay)`, with `key_delay` defaulting to 0, so it uses the laggy newest estimate. The "then" wrist is looked up at the depth capture time. Displacements over 25 cm are dropped. The direction comes from a head-relative elbow guess, `ELBOW_OFFSET (0.2, −0.5, 0.05)` | `client_openxr/src/stream.rs:758-815` (now `:773-776`, `:800`, then `:801`, guess `:767`, `:806-811`); default `passthrough.rs:292-294`; called with the capture time at `stream.rs:1348-1350` |
| F2 | The shader weight is `(1 − smoothstep(0, ARM_LENGTH 0.45, along)) × smoothstep(hand_edge) × (1 − smoothstep(0.6·0.14, 0.14, aside))`. The `w` of the three `arms` vec4s is written as 0 and is unused | `graphics/resources/depth_mask.wgsl:241-276`; `graphics/src/depth_mask.rs:759-766` (`v.extend(0.0)`) |
| F3 | The learner looks up the joints at the camera image's own time (a past time) | `client_openxr/src/auto_color_key.rs:981` |
| F4 | The bridge slot's end `.w` is the **solid** weight, and solid becomes focus `g` (shown in any colour). The bridge's rim uses the **hand's** `zones` and sweep. The block only runs while the hand is tracked (`bounds.w > 0`) | `graphics/resources/focus_area.wgsl:204-233` (solid = `bridge_end.w` at `:223`), `capsule_zones` `:110-140` (inner `:134-138`), hand loop `:151-156`; uniforms `depth_mask.rs:797-814` |
| F5 | `hands_view_rect` chains the bridge capsule (with the hand's rim + 1 cm and both sweep ends) into the one rect per eye. That rect is used for the focus scissor, the lower layer's crop and `key_lower`. `limit_hand_to_region` limits the bridge to the marker box like the hand | `depth_mask.rs:845-912` (`.chain(&bridge)` `:870-874`), `:383-443` (bridge `:431-440`) |
| F6 | The burst depth readback exists: `request_burst_depth(token)` reads both eyes of the next render's depth through the depth-dump pass, and `poll_burst_depth` returns them. There is **one** request slot: a second request replaces the first | `graphics/src/stream.rs:1136-1154`; used at `client_openxr/src/stream.rs:722`, `:1598` |
| F7 | The uniform block is 4144 bytes, asserted in a test. `ArmMotion` is built in two other places | `depth_mask.rs:1153`; `graphics/src/tests.rs:1674` (`late_arms_are_moved`, `:1650`), `graphics/src/depth_burst.rs:768` |

**Correction to the design (from F4):** the design sends R in the bridge's "solid" slot. That would give a
geometric solid fill in any colour: the bridge's removed failure, against lesson B. In step 2b the slot's
end `.w` must be **0**, and R travels in the flag (step 2b, Uniforms).

## 3. Build steps

### Step 1: the past-time wrist target and the linear weight

This is useful alone, costs no GPU time, needs no readback, and changes no uniform layout.

**Properties** (`debug.alvr.`, polled with `arm_shift` in `StreamContext::poll_properties`, `stream.rs:640-648`):

| Property | Default | Meaning |
|---|---|---|
| `arm_shift` | **1** | Today an on/off flag, read with `!= 0`; becomes 0 / 1 / 2. 0: plain depth. 1: today (display-time wrist, smoothstep to 45 cm). 2: new (past-time target, linear weight) |
| `arm_hands` | 3 | Which arms get mode 2 (1 left, 2 right, 3 both). The others use mode 1, for a side-by-side check |
| `arm_target_ms` | 40 | λ: place the arm for `display − this` (clamp 0..150) |
| `arm_history_ms` | 80 | Look up the wrist at `display − max(this, arm_target_ms)` (clamp 0..200) |
| `arm_weight_mm` | 300 | Length of the linear weight (clamp 100..600). Measured best 26-35 cm; 20 cm is worse |

The default stays 1 (today) until the user's verdict, then becomes 2. Both earlier forearm changes were
rejected by the user's eye, so the user switches live first.

**Rules** (`client_openxr/src/stream.rs`, `arm_motions`, per hand, mode 2):
1. `T = display − arm_target_ms`, `H = display − max(arm_history_ms, arm_target_ms)`.
2. Look up `w_H = wrist(H)` and `w_P = wrist(H − 30 ms)`, with the existing `wrist` closure (`:787-799`).
3. Find the step from H to T:
   - `v = (w_H − w_P) / 0.030 s`, by finite difference. The reported joint velocities are 1.5-2× too large
     (measured), so they are not used.
   - `step = v × (T − H)` in seconds, length capped at `MAX_EXTRAPOLATION = 0.06 m`.
   - If `w_P` is invalid or `T ≤ H`, the step is 0.
4. `wrist_T = w_H + step`, and `displacement = wrist_T − wrist(capture_time)`. Drop it above 25 cm, as today.
5. `direction` stays the elbow guess (F1). Report 1 found its precision does not matter.
6. `ArmMotion` gets a new field `weight_len: f32`: 0 means today's weight (mode 1), and `arm_weight_mm / 1000`
   in mode 2. `wrist` = `wrist_T`.
7. Put the pure maths in `graphics/src/depth_mask.rs` as
   `pub fn arm_target_wrist(w_h: Vec3, w_p: Option<Vec3>, gap_s: f32, ahead_s: f32, cap: f32) -> (Vec3, f32)`.
   It returns the wrist and the extrapolated distance, so it can be unit tested.

Per frame that is 3 lookups per hand instead of 2: H, H − 30 ms and the capture time. It is +1
`xrLocateHandJointsEXT` per hand per frame.

**Uniforms** (`depth_mask.rs:759-766`): write `displacement.extend(arm.weight_len)` instead of `extend(0.0)`
for the third vec4. Layout and size are unchanged.

**Shader** (`depth_mask.wgsl` `arm_displacement`, `:270`):
```wgsl
let weight_len = depth_u.arms[hand * 3u + 2u].w;
var along_weight = 1.0 - smoothstep(0.0, ARM_LENGTH, along);              // mode 1: today
if weight_len > 0.0 { along_weight = clamp(1.0 - along / weight_len, 0.0, 1.0); }   // mode 2
let weight = along_weight * smoothstep(hand_edge.x, hand_edge.y, along)
           * (1.0 - smoothstep(0.6 * ARM_RADIUS, ARM_RADIUS, aside));
```
`initial_distance` (`:209-239`) stays as it is. It only places the start of the search.

**Burst / dump fields** (`graphics/src/depth_burst.rs:285-310`, `depth_dump.rs:515-557`, `burst.rs`): per arm,
add the following to the existing `wrist / direction / displacement`:
- `mode`, `weight_len`;
- `target_ns`, `history_ns`, `wrist_history`, `extrapolated_mm`.

Also add **the published key LUT** to the burst's start dump, as `_key.lut` with its version. The bursts of
2026-11-24 had no table, so the simulation had to invent one (G4 needs it).

**Log line** (`ALVR_CAM`, every 2 s and on property change):
```
arm: mode 2 (left; right 1), target display-40 ms from history -80 ms, weight 300 mm; L extrapolated 12 mm p90 31 at 0.31 m/s, lookups failed 0/144, hand-to-arm wrist gap 9 mm p90 21; R ...
```
The `hand-to-arm wrist gap` is the distance between the display-time wrist (the hand capsules) and
`wrist_T`. It tells whether the hands should later move to the same target.

**Tests:**
- `arm_target_wrist`: no step when `T ≤ H` or `w_P` is missing; the cap holds; constant motion is
  reproduced exactly.
- `late_arms_are_moved` (`tests.rs:1650`): add `weight_len: 0.0` to `motion` (F7) and add a mode-2 case.
  - A surface 20 cm along the arm moves by 1/3 of the displacement.
  - At 35 cm it does not move. Today's smoothstep still moves it by about 13 %.
- Update `depth_burst.rs:768`.
- Run the existing uniform size test.

**Headset (user):**
- `arm_shift 2` with `arm_hands 1`: the left arm is new, the right is today. A fast wave should show the
  difference.
- Expected for the new arm:
  - the edge stays with the arm instead of trailing it;
  - no 4-5 cm overshoot when the arm reverses;
  - little change while moving slowly.
- Also expected: the gap next to the wrist (0.6-2.6 cm) stays.
- Possible: during fast moves the arm may now be ahead of the hand capsules (watch the `hand-to-arm gap`).
- λ check: `arm_target_ms 20 / 40 / 60` while waving. The user says whether the edge leads or trails.

### Step 1b: the arm's own sample history (built 2026-11-24, not yet run on the headset)

**Why.** The wave bursts 14 and 15 (`logs/analysis-20261125-wave/report.md`) showed that the runtime doesn't
answer `xrLocateHandJointsEXT` for past times. Any time before its newest sample S returns S; any later time
returns its prediction. Step 1 took `wrist(H) − wrist(H − 30 ms)` and `wrist(capture)`: the difference was 0
in about 90 % of frames, and the rest jumped 11-16 cm (the 60 mm cap in a random direction). The real wrist
is about S 35 ms later (32-38 ms measured).

**What changed** (`client_openxr/src/arm_history.rs`, `stream.rs` `update_arm_histories`, `arm_motions`,
`arm_gaps`):
- Each hand keeps its own history of samples (300 ms): time, time first seen, wrist, middle knuckle. It is
  filled every frame that has a depth mask. A sample is new when its capture time changes, or without one
  when the joints change.
- **Sample time.** The runtime lists `XR_META_hand_tracking_unextrapolated_poses`, so it is enabled (by name
  in `lib.rs`, logged as `arm: the runtime lists ...`). One lookup at now, with its request chained, returns
  the sample as it is and its `captureTime`. The structures are defined in
  `extra_extensions/hand_tracking_unextrapolated_meta.rs` (`openxr` 0.21 has no bindings; type numbers
  1000693000 / 1000693001 from the registry). A capture time is used only if the sample was 0-300 ms old
  when first seen. Otherwise, or without the extension, the time is the time first seen minus
  `arm_sample_age_ms`. The sample is then read with a lookup 100 ms before now: the runtime returns its
  newest sample there, and the time asked for is not used.
- **The arm in the depth** = the history interpolated at the depth's capture time. If the history has no
  sample that early (the hand just came back, or mode 2 just started), or the capture time falls in a gap
  of more than 100 ms between two samples (the hand stayed active but was not sampled), the arm gets plain
  depth for that frame: no blend of the old and the new place. This counts as a buffer miss.
- **The placed wrist** = `arm_estimator`, at most 6 cm from the newest sample (the old cap):
  - 0 (default) hold: the newest sample;
  - 1 line: through the two newest samples, at display − `arm_target_ms`;
  - 2 parabola: the newest sample moved with the slope at that sample of a least-squares parabola over the last
    120 ms (with 3 samples a line), to the same time.
  - 1 and 2 extrapolate for 150 ms at most, and use no velocity across a gap of more than 100 ms between
    samples, nor before the history holds 3 samples (the first samples after a hand comes back are often off).
- **Loss.** A hand that is not located, or has no valid wrist or middle knuckle, empties its history. When it
  comes back, the first frames are misses (plain depth), then a held sample with no velocity. So a hand that
  comes back gives no jump from its old place. Changing `arm_sample_age_ms` or `arm_capture_time` also
  empties both histories.
- **Wrist gap.** In mode 2 it starts at the placed wrist, along the newest sample's wrist → knuckle axis.
  Without a motion it uses the history at the capture time (the oldest sample if the capture time is
  earlier; no wrist gap when it falls in a gap of the history). Mode 1 and the case without a depth image
  are as before.
- Mode 1 (`arm_shift 1`, still the default) is unchanged and still uses its two lookups: it stays the
  comparison. Its capture-time lookup still returns the runtime's newest sample, not the wrist at the
  capture time (13 / 35 mm off at p50 / p90 in burst 14, 46 / 103 mm in burst 15). Moving mode 1 onto the
  history too is one line (`arm_histories[i].at(capture_ns)`), left for the user to decide: it changes the
  default and removes the old reference. The linear weight was already in mode 2 on both arms with `arm_hands 3` (the default).

**Properties** (new or changed):

| Property | Default | Meaning |
|---|---|---|
| `arm_estimator` | 0 | 0 hold, 1 line, 2 parabola (above) |
| `arm_sample_age_ms` | 35 | Age of a sample when first seen, for its time without a capture time (0..150) |
| `arm_capture_time` | 1 | 0: never use the runtime's `captureTime` |
| `arm_target_ms` | 40 | Only for estimators 1 and 2 now |
| `arm_history_ms` | - | No longer read |

**Log** (`ALVR_CAM`, every 2 s while an arm uses mode 2):
```
arm: mode 2 (both), estimator 0 (hold the newest sample), weight 300 mm, sample times from captureTime, else seen-35 ms; L 137 samples every 14.6 ms p90 28.9, age 36 ms p90 41 (captureTime), buffer misses 0 of 143 frames, resets 0, hand-to-arm wrist gap 30 mm p90 52; R ...
```
Estimators 1 and 2 add `placed <mm> p90 <mm> from the newest sample at <m/s>` (ahead, or back when the
target is inside the history; the burst field `extrapolated_mm` is the same distance).

**Bursts** (version 4, `graphics/src/depth_burst.rs`): each display record has `arm_samples`, one per hand:
- `reset`;
- `new`: the samples first seen in this frame, each with `time_ns`, `seen_ns`, `wrist` and `capture_time`.
  The first record of a burst holds every sample the history had then, so the capture-time wrist of its
  first frames can be rebuilt;
- `capture_wrist`: the wrist used for the capture time (null: not mode 2, a miss, or plain depth for a
  displacement over 25 cm);
- `estimator`: -1 for an arm not in mode 2.

Samples are only collected while a burst records.

In mode 2, `history_ns` / `wrist_history` of the arm are the newest sample, and `target_ns` is the time the
placed wrist stands for. `read_burst.py` reads all of this as `burst["samples"][hand]` and the display arrays
`arm_capture_wrist`, `arm_estimator` and `arm_reset`; the sample intervals of its summary don't span a
reset. Older bursts still read.

**Tests:** `arm_history.rs` (12: new samples, 300 ms, interpolation at the capture time, no wrist inside a
gap, each estimator, the cap and the horizon, no velocity over gaps or from the first two samples, reset
without a jump), `stream.rs` (the `arm:` texts, which
property changes empty the histories), `depth_burst.rs` (the version 4 record).

**Headset (user):** `arm_shift 2`, `arm_hands 1` (left new, right mode 1), one wave burst. First check the log:
- the sample age is about 35 ms;
- there are no misses after the first frames.

Then judge by eye: hold (0), then 1 and 2 with `arm_target_ms` 40 / 60. The offline scores (`fix_drawn.txt`)
expect hold to beat plain depth at every λ, and the parabola to be better at λ about 40 ms. Those scores
capped the step at 15 cm; the build caps it at 6 cm. In a fast wave (burst 15: 1.6 m/s, about 50 ms ahead
is 8 cm) estimators 1 and 2 will often stop at the cap, so they can trail more than the scores say (the
`placed` p90 of the `arm:` line shows it: 60 mm means capped).

### Step 2-pre: measure the cost of the grown rect with no code (adb by the main session or the user)

The existing bridge grows the rect like an arm zone would (F5):
- `key_wrist_len 180`, `key_wrist_scale 250` (about 4-5 cm radius; check the radius in the log);
- `key_wrist_conf 100`, so its solid fill practically never turns on.

With hands in view (driving pose and a wave), log the VrApi load, the App and TW times and fps:
- bridge off, then on;
- `key_crop 1`.

What it shows (the bridge's colour-chosen direction) is not what step 2 will show. This step measures only
the cost. Gate G2: at most +4 points of median load and no drops. Afterwards set `key_wrist_len 0`.

### Step 2a: readback and the arm fitter, log only

Properties: `arm_key` 0 (off) / 1 (fit and log only) / 2 (draw the zone, step 2b), default 0;
`arm_key_len` 180 mm; `arm_margin` 15 mm.

**Readback** (`graphics/src/stream.rs`, `client_openxr/src/stream.rs`):
- On each new depth frame with `arm_key ≥ 1`, request a readback (both eyes, as bursts do; 400 KB; fit the
  left eye).
- F6: the single request slot is shared with bursts. Give arm requests their own token range (high bit
  set). When a burst asks for the same frame, one readback answers both. `poll_burst_depth`'s results are
  dispatched by token.
- Store with the token: `t_c`, the left depth view's pose and tangents, near / far, and per hand the wrist
  and the middle metacarpal located **at t_c** (past time, accurate).

**Fitter** (new pure module `graphics/src/arm_fit.rs`, run in a small worker thread in `client_openxr`).
Writes `Arc<Mutex<[Option<ArmModel>; 2]>>`. The recipe is the one that found the arm in bursts 8, 2 and 6
(`analysis-20261124-bursts/arm/armseg.py`, `analysis-20261124-arm-mask/zone_check.py`):
1. Take a window: the texels within 30 cm of the wrist's projection, in metres and 3D.
2. Make depth-connected pieces. 4-neighbours join if |Δz| ≤ max(3 cm, 6 % z); far fill and no-data break
   them.
3. The arm piece is the piece with the most texels 4-18 cm from the wrist, among texels more than 2 cm
   behind the wrist plane (normal: wrist → middle metacarpal).
4. Keep its texels 3-25 cm from the wrist. Fit a 3D line by PCA. Keep the texels within 6.5 cm of the line,
   and refit twice.
5. Take cross-sections every 2 cm from 3 to 18 cm:
   - half-width = half the sideways extent seen from the depth camera (3rd-97th percentile);
   - centre = the middle, pushed back from the surface by 0.8 × the half-width.
   A line through the centres gives the axis. A linear fit of the half-widths gives `r0` (wrist) and `r1`
   (18 cm), clamped to 20-70 / 20-80 mm.
6. Gates. Reject the fit, with the reason counted, if any of these fails:
   - at least 150 texels;
   - elongation > 1.3;
   - the line passes within 3 cm of the wrist;
   - no radius clamped by more than 1 cm;
   - the axis within 75° of "opposite to the fingers";
   - a turn of more than 30° from the last accepted fit within 200 ms needs two agreeing fits in a row.
7. Smoothing:
   - the direction is low-passed by 30 % per fit, after the old model has been moved to the new `t_c`
     (rule 8);
   - the radii by 20 % per fit, faster when growing.
8. **The motion law**, one for everything. `ArmModel { t_c, wrist_c, start_c, far_c = start_c + 0.30 m ×
   axis, r0, r1, points, reason }` is moved to a time t with `D = wrist(t) − wrist_c`:
   - `start_t = start_c + D`, `far_t = far_c`;
   - `axis_t = normalize(far_t − start_t)`, radii unchanged.
   This is the same linear 30 cm law as step 1. If `arm_weight_mm` changes, use it for `far_c` too.
9. A model is used while it is younger than 300 ms. After that there is no zone.

**Per frame** (render thread): move the newest model to T with step 1's `wrist_T`. Build the zone capsule:
- start: `start_T`, radius `r0 + margin`;
- end: `start_T + arm_key_len × axis_T`, radius `r(L) + margin`, where `r(L) = r0 + (r1 − r0) × L / 0.18`.

In mode 1 the capsule is computed but not drawn. Compute the rect area per eye **with and without** it, by
calling `hands_view_rect` twice. That gives G2's area ratio for the real arm.

**Log** (every 2 s):
```
arm: L fit ok 512 pts, r 33/41 mm, axis 18 deg from the elbow guess, model age 47 ms; rejected 3 of 40 (few 1, wide 0, wrist 0, jump 2); rect L/R 18/17 % of the view, 27/26 % with the arm zones; readback 0.3 ms, fit 0.4 ms (p90)
```

**Burst fields:** per arm, `arm_model` (as above, plus the depth frame index) and `arm_capsule` (start,
end, radii, as it would be sent).

**Tests** (`arm_fit.rs`, synthetic depth):
- a tapered cylinder behind a wrist gives the axis within 3° and the radius within 3 mm;
- a cylinder lying on a plane (a desk): the radius stays honest, or the fit is rejected as too wide;
- two arms crossing: each wrist gets its own piece;
- a flat plane only: rejected;
- the motion law: `far` stays fixed, `start` moves by D;
- the 30° jump rule.

**Headset:** nothing changes visually. Run the four motions and read the `rejected` counts (G3).

### Step 2b: draw the keyed zone (skin table only), `arm_key 2`

The key may decide inside a capsule around the arm. The key only **adds** colours there, so the moved
depth of step 1 stays shown in full. There is no core and no band, and nothing is handed from the depth
to the key.

**Uniforms** (bridge slot, `depth_mask.rs:797-814`, no layout change):
- `hand_bridge[2h] = (start_T, r0 + margin)`;
- `hand_bridge[2h+1] = (end_T, 0.0)`. **The solid weight must be 0** (F4);
- `hand_bridge_end[h]` = the end radius;
- `hand_bridge_end[2 + h]` = **2 + R** (R in 0..1). Values: 0 = nothing, 1 = the old bridge, ≥ 2 = the arm
  zone.
- Carry R and a flag on `WristBridge` (e.g. `kind: Bridge | Arm { reliability }`). It then goes through
  `hands_view_rect` and `limit_hand_to_region` unchanged.
- With `arm_key 2` and `key_wrist_len > 0`, the arm wins. Log it once.

**Focus pass** (`focus_area.wgsl`, the bridge block `:204-233`): keep today's code for the flag value 1. For
values ≥ 1.5:
```wgsl
let reliability = depth_u.hand_bridge_end[2u + hand] - 2.0;
// bound test as today, with the margin baked into the radii and no hand rim
let d = ray_capsule_distance(origin, dir, bridge_start, vec4f(bridge_end.xyz, bridge_end_radius));
outer = max(outer, reliability * (1.0 - smoothstep(-SOFTNESS, 0.0, d)));
```
- It does not merge into the hand's `distance`, so it does not get the hand rim. It never writes `inner`
  (g) or `near` (b).
- Outside a marker box, `limit_hand_to_region` makes the radii negative by the hand's reach, so `d > 0`
  there and nothing shows (F5). Its bound test must use `bridge_radius + SOFTNESS > 0` without `zones.x`.

**Rect:** `hands_view_rect` includes the capsule through `.chain(&bridge)`. When R = 0 the render thread sends
no arm capsule, so the rect does not grow.

**R per arm** (learner `client_openxr/src/auto_color_key.rs`, published next to `Verified`):
1. Move the newest model to the camera image's time t_img. Both lookups are past times (F3), so they are
   accurate.
2. It is used only if the model is at most 150 ms from t_img and the wrist is below 0.5 m/s at t_img.
3. Arm samples: camera pixels whose ray passes inside the capsule shrunk by max(10 mm, 0.4 r), 5-18 cm from
   the wrist, at least 2 cm from the hand shape, not in the other hand or arm. Every 2nd pixel.
4. Arm background: pixels 2-6 cm outside the capsule surface over the same stretch, excluding the hands
   (+2 cm) and the other arm.
5. Count with the **published** LUT: `arm_kept` and `bg_kept`. Use them only if there are ≥ 100 of each.
6. `R = (1 − smoothstep(arm_leak_lo 0.10, arm_leak_hi 0.25, bg_kept)) × smoothstep(0.5, 0.8, arm_kept)`.
7. R falls at once and rises by 30 % per pass. Without data it is held for 1 s, then set to 0.
8. Render thread: on above 0.6, off below 0.4, faded over 0.15 s.
9. Without `auto_key` there is no R, so `arm_key 2` acts as 1 (logged).

Offline the thresholds give the right answer, but R itself is **untested**:
- in the zone, the learned tables kept 27 % of the background in b8 (a real halo), so R would be 0;
- they kept 3-4 % in b2 and 5-7 % in b6 (harmless), so R would be 1.

With the skin table only, `arm_kept` on a sleeve is about 0, so R = 0. The zone then appears only on bare
forearms, which also saves its cost when it would add nothing.

**Properties added:** `arm_leak_lo` 10 %, `arm_leak_hi` 25 %. `arm_margin` is clamped to 5..25 mm: the
simulation found 3-5 cm margins only add leak.

**Log:** extend `arm:` with `R 1 (arm kept 0.86, bg kept 0.04, n 640/410)`, and the learner's `auto key:` line
with `arm L n ... kept ... bg ...`.

**Burst fields:** `arm_R` and `arm_capsule` as sent. Camera records: the model at t_img, the sample counts
and both shares. Depth dump focus `r` now contains the zone.

**Tests:**
- focus render test: with flag 2.0 + R = 1, `r` = 1 inside the capsule and `g` = 0; with R = 0, `r` = 0;
  flag 1 behaves as today;
- `limit_hand_to_region` with an arm capsule half outside a box: `r` = 0 outside;
- `hands_view_rect` grows with an arm capsule and not with R = 0;
- R from synthetic shares (the smoothsteps and the hysteresis).

**Headset:** `arm_key 2` with `arm_hands 1`, and the forearm **bare** (sleeve pushed up). Things to look at:
- whether the 1-3 cm gap next to the wrist fills in, and whether the arm edge gets sharper;
- with a sleeve: no change, because R = 0 (the `R` log should show 0);
- over the skin-coloured parquet or cardboard: no patches. R should drop within about 0.2 s.

The comparison: `arm_key 0 / 2` live, or the left arm against the right.

### Step 3: the sleeve class (only if the user drives in sleeves)

`graphics/src/color_model.rs`, `client_openxr/src/auto_color_key.rs`. Property `arm_sleeve` 0 / 1, default 1
once built. The merge gate protects the hands.

1. **New densities** in `ColorModel`: `arm` (5 s, past 45 s × 0.5) and `arm_background` (3 s, past 60 s ×
   0.4), next to the constants at `color_model.rs:11-21`.
   - They are not merged into skin, so `segment_shares` and the confirm table keep judging hands by skin
     only. A sleeve colour must not confirm a finger.
2. **Learning:** the step 2b arm samples and arm background, only when all of these hold:
   - `arm_key 2` and `arm_sleeve 1`;
   - a model within 100 ms of t_img;
   - the wrist **slower than 0.3 m/s** at t_img (the simulation's tested threshold; the design said 0.25);
   - the hand fully tracked.
   The core sampling from the capsule is **untested**: the simulation sampled the moved silhouette shrunk by
   1 cm, then a 2-6 cm ring.
3. **The arm table:** `arm_alpha` = the same `target()` with skin := arm, background := max(arm_background,
   background, scene), contamination 0.25, floor from the arm's typical density × `key_tight`.
4. **One uploaded LUT:** `published = max(skin_alpha, gate × arm_alpha)`. The compositor has one colour map
   per passthrough layer. `alpha` (skin) stays the internal table for verification.
5. **Merge gate**, re-checked at every LUT update. All of these must hold, otherwise `published = skin_alpha`:
   - ≥ 300 arm samples;
   - `arm_alpha` keeps ≥ 0.6 of the arm samples and ≤ `arm_leak_hi` of the arm background;
   - the merged table keeps no more of the hand rings than the skin table + 2 points;
   - it keeps ≤ 5 % of the scene samples.
   Offline this gate would have **blocked** b8, where the sleeve table kept 12.4 % of all background (the
   dark screens), and passed b2 (0.3 %) and b6 (4.4 %). The gate itself is **untested**.
6. Key dumps: `_arm_alpha.lut` and the counts `arm`, `arm_background`, `arm_kept`, `arm_bg_kept`, plus whether
   it merged and which gate blocked it.
7. **Optional 3b (untested):** while the model is good, the scene sampler's 50 × 10 cm tube
   (`color_model.rs:766-787`) and the ring's half-space (`:877-879`) use the fitted capsule + 3 cm instead of
   `arm_direction`. The leak map's `arm_exclusions` do the same. Do this only if the sleeve keeps losing to
   its own pixels in the scene class: the log shows `arm_alpha` keeping < 0.6 of the arm while slow.

**Log:** `auto key: ..., arm L n 640 kept 0.86 bg 0.04, sleeve merged yes|no (<gate>)`.

**Tests:**
- `ColorModel`: with arm samples of a colour unlike skin, the merged table keeps that colour and
  `segment_shares` is unchanged;
- a sleeve colour that is also in the scene above 5 %: no merge;
- the hand-ring gate.

**Headset:** a long sleeve, `arm_key 2`. While slow, the `sleeve merged` log should turn yes within a few
seconds. Then the zone (R → 1) keys the sleeve's edge. Expected from the simulation:
- 77-86 % of the sleeve is kept by colour, with holes in a plaid. Those holes do not show, because the
  moved depth underneath stays;
- dark sleeves in front of screens: no merge (the gate).

## 4. What the user sees, and the live switch per step

| Step | Switch to compare | Expected on the headset |
|---|---|---|
| 1 | `arm_shift 1 / 2`, or `arm_hands 1` (left new, right today) | Fast arm: the edge on the arm instead of trailing by 2-5 cm and overshooting at reversals. Slow: about the same. Gap next to the wrist unchanged. Tune `arm_target_ms` 20 / 40 / 60 |
| 2-pre | `key_wrist_len 0 / 180` | Nothing to judge visually (a stub in a camera-chosen direction); cost only |
| 2a | `arm_key 0 / 1` | Nothing visible; logs only |
| 2b | `arm_key 1 / 2`, `arm_hands 1` | Bare forearm: the wrist gap filled by colour, a sharper edge for 18 cm behind the wrist. Sleeve: nothing (R = 0) |
| 3 | `arm_sleeve 0 / 1` (with `arm_key 2`) | Sleeve edge keyed. No change in front of dark screens or bright walls when the gate blocks |

## 5. Cost per step

Measured anchors, from KB "GPU budget", SteamVR home, 72 Hz:
- 2 layers without hands: 76 %;
- 4 layers with hands and a cropped lower layer: 80 % median, peaks of 96 %, App about 7 ms;
- 4 full-size layers, key on against off: TW 4.69 against 2.0 ms, about 85 against 62 %;
- the target budget the task gives: 78-95 % already.

| Step | GPU | CPU | Compositor |
|---|---|---|---|
| 1 | 0: one smoothstep becomes a clamp | +1 `xrLocateHandJointsEXT` per hand per frame | 0 |
| 2-pre | measurement only | | |
| 2a | one depth-dump pass + a 400 KB copy per depth frame (15-21/s). Inferred < 0.1 ms. The bursts did this every depth frame (the design cites bursts 2, 3, 7 as hitch-free; check the others) | fit < 1 ms (inferred) in a worker; 400 KB mapped per frame | 0 |
| 2b | focus pass: +1 capsule test per pixel in the scissor (1/5 resolution). The scissor grows with the rect | learner: 2 more sample sets per pass | **The rect of the lower game layer grows.** Inferred: if the extra layers' cost is proportional to area, today's cropped rect is about 4/23 ≈ 17 % of full size. A 1.5-2.5× larger rect (the design's guess) would add **about +2 to +6 points** of median load (82-86 %), and the 96 % peaks would reach 100 %, so drops are likely. Nothing is added while R = 0. Step 2-pre and the 2a rect log replace this guess |
| 3 | 0 | learner: one more density class and target per LUT update (every 180 ms); inferred a few ms per update with `CARGO_PROFILE_DEV_OPT_LEVEL=3` | 0 (same LUT, same layers) |

Levers if 2b is over budget, in order:
1. `arm_key_len` 120 mm;
2. `arm_margin` 10 mm;
3. no zone for the arm farther from the head (the rect grows less);
4. drop 2b and keep step 1.

## 6. What to record in bursts to judge it afterwards

Every burst carries the step 1 fields and the published LUT (new). From step 2a on it also carries
`arm_model` and `arm_capsule`, and from 2b on `arm_R` and the camera-record arm counts.

**Motions** (3 s each; with `arm_hands 1`, so both modes are in each burst, or once per mode):
1. a fast wave (as b8);
2. a slow arm (as b2);
3. both hands turning the cardboard wheel (as b6), plus a 30 s driving run for the fitter's rejection rate;
4. hands still on the lap (as b7: the fitter should reject, and nothing should move);
5. **untested so far:** a bare forearm over the wooden desk and the wheel;
6. a long sleeve (dark, and a pale one);
7. the arm over the skin-coloured parquet / cardboard (the halo case);
8. the arm in front of the bright wall / dark screens (b8's halo case).

**Measures** (the 2026-11-24 scripts: `analysis-20261124-bursts/arm/`, `analysis-20261124-arm-mask/s4_sim.py`,
`zone_check.py`):
- step 1: edge distance of the moved arm against the later depth frame. Mode 2 should be near the offline
  4-8 mm, against 10-31 mm for mode 1. Also cut + leak against the camera truth, with the **recorded**
  displacement, and the extrapolation error of `wrist_T` against the later camera-record wrist;
- 2a: containment and excess of `arm_capsule` against the camera arm (offline: 98-99 % at 15 mm with an
  accurate wrist, p10 0.79 extrapolated in b8), the rejection rate per motion, and the hand-to-arm gap;
- 2b / 3: cut + halo with the live LUT (G4), the halo in motions 7-8, R's on-time, and whether the 0-3 cm
  next to the wrist fills (now scored: drop the simulation's don't-care band where the zone covers it);
- GPU: VrApi load, App and TW times with `arm_shift 1/2`, `arm_key 0/1/2`, `key_crop 0/1`, hands in view,
  several runs each.

**λ:** take a `screencap` during a fast wave with a burst running. The hand in the screenshot against the
joint history gives the passthrough latency. This has not been tried; it fixes `arm_target_ms`.

## 7. Open risks

| Risk | Sign | Act when |
|---|---|---|
| Unknown λ: the target time may be off by 20-30 ms | the edge leads or trails during a wave | No `arm_target_ms` of 20 / 40 / 60 looks right: measure λ (6) before tuning anything else. Offline, the ranking held for λ 0-100 ms |
| The history lookup at display − 80 ms fails or is not accurate on the headset (only measured offline from the learner's image times) | `lookups failed` in the log, jitter | Raise `arm_history_ms` to 100-120. The extrapolation then grows (cap 6 cm) |
| Hand and arm on different targets (the hand capsules keep display-time joints, as the user wants) | the `hand-to-arm gap` p90; a step at the wrist | The gap p90 exceeds about 2 cm and the user sees it: decide separately whether the hands move to the history target |
| The rect cost of 2b | 2-pre, the rect log, VrApi | More than +4 points median, or drops at 72 Hz: use the levers in 5, else no 2b |
| The fit fails in the driving pose (the wheel or the legs join the arm piece, as in 14 of 35 b6 image-hands of the simulation) | the `rejected` counts | More than 30 % rejected on the wheel: stop before 2b. The zone source is not reliable there |
| Halo over skin- or sleeve-like backgrounds (b8: wall, screens) | `bg kept` in the log; motions 7-8 | R does not drop within about 0.2 s, or the user sees patches: stop 2b. If R is 0 most of the time in the user's room, 2b brings nothing |
| The sleeve merge leaks onto the hand rims (one LUT) | the hands' ring kept share rises | It rises by more than 2 points: the gate should block it; if not, stop step 3 |
| A second arm (failure B): the zone and the depth arm disagree | a ghost edge at an angle | Seen once: check the motion law and the model age. Both use the same law, so it should not happen |
| Readback contention with bursts (one slot, F6) | a burst's depth frames missing while `arm_key ≥ 1` | Fix the token dispatch before recording |
| The simulation's limits | | The truth was built from the same depth and colour as the methods: 2-3 point gaps are noise. Left camera only, at 320×240. Arm limited to 45 cm. The 3 cm next to the wrist (the zone's main value) and bare arms over wood were not scored. No GPU numbers |

Do not build (same as the design, confirmed by the simulation where it was tested):
- a temporal stack of depth frames (report 4: it lags);
- a geometric solid fill on the arm;
- choosing the arm's direction by colour;
- a flood-fill zone from the depth piece (the simulation: in b6 it grabs the wheel; the zone must be a
  capsule);
- a second keyed layer for sleeves;
- key-only arm edges: they leave holes in patterned sleeves (b2 recall 82 against 87, b6 83 against 92).
