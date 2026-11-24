# Mask v2: independent check of design.md

Status 2026-11-24. Checked against the code in `ALVR` as it is now (head 1503fb42 plus the uncommitted arm
step 1 in the working tree) and against the recorded bursts and captures. My scripts were written before I
read the lead's scripts. They are in `logs\analysis-20261124-design\check\` (local only):
`c1_edges.py` (+ `c1b_dirs.py`, `c1c_sides.py`), `c3_claims.py`, `c4_views.py` (+ `c4b_rig.py`,
`c4c_dbg.py`), `c6_nodata.py`, each with a `.txt` next to it. Nothing under `ALVR` was edited. I did not run
adb, did not commit, did not upload anything and did not spawn subagents.

Sign rule in this file: an edge offset is **positive when the object is drawn too big** (the camera edge
lies inside the drawn or depth edge).

## 1. Verdict in short

- The order is right, and step 1 (`edge_cut`) is the first build I would trust. The code it needs exists
  and works the way the design says. The direction of the effect holds up in my own measurement.
- The wheel numbers in L1 are overstated. The idea that "80 is not enough for the wheel" is not supported
  by my measurement.
- `edge_cut` changes more than the design says. It also pulls the play-area box borders inward, and it
  cuts into some objects that are not drawn too big today.
- The L4 numbers reproduce, but only the head-turn half supports the wide memory camera. The aim rule
  for that camera ("from the head to the centre of the play-area bounds") does not work for this play
  area.
- The L3 claim test separates the cases in my rerun too. The limit of 40 % sits on the edge of the
  correct cases.
- Three practical gaps: the steering angle's path (PC to headset, not "with the tracking data"), the
  readback size for carry, and the memory pass's load arriving as a spike on depth frames.

## 2. Claims about the code

| claim | result | evidence |
|---|---|---|
| `stream.wgsl` cuts the mask with `smoothstep(0.05, 0.35, r)` | **confirmed** | `MASK_EDGE_MIN/MAX` = 0.05 / 0.35, stream.wgsl:120-132. The same constants are also used in the debug tint at line 167, so that line needs the same uniform |
| `area.z` is a free uniform component the stream pass can read | **confirmed** | `depth_uniforms()` writes `[boxes, see, 0, 0]` (depth_mask.rs, after `world_to_area`); stream.wgsl binds `DepthUniforms` at group 1. Only stream.wgsl uses the cut (grep) |
| the mask blends 3 x 3 depth texels with quadratic B-spline weights | **confirmed** | depth_mask.wgsl:139-190 |
| the cut at 0.2 lies 0.37 texel outside a straight texel border; 0.8 lies 0.37 inside; 20 -> 80 moves the edge 0.74 texel (2.7 mm at 0.5 m) | **confirmed** | c1_edges.txt: 0.2 at -0.367, 0.5 at 0.000, 0.8 at +0.368 texel |
| a one-texel line peaks at coverage 0.75 and is almost gone at 80 | **confirmed** | peak 0.750. Drawn width 1.73 / 1.00 / 0.00 texel at 0.2 / 0.5 / 0.8 (at 80 it shows at about 26 % strength). A two-texel line goes from 2.73 to 1.26 texel |
| the wrist-gap term would sit behind the `continue` that skips still arms | **confirmed** | depth_mask.wgsl:257 |
| uniform size test 4144 -> 4176 bytes | **confirmed** | depth_mask.rs:1199 `assert_eq!(DEPTH_UNIFORMS_SIZE, 4144)`; two more `vec4` add 32 bytes |
| `playzone_copy.wgsl` copies the whole depth image only when no hand was near the depth view for 1.5 s | **confirmed** | stream.rs (client) `PLAYZONE_HANDS_TIMEOUT` 1500 ms, `sphere_in_view` with a margin, repeated every `PLAYZONE_INTERVAL` while no hand is near |
| memory textures are `Rgba16Float`, one per eye, each with its own camera | **confirmed** | `PLAYZONE_FORMAT = LIGHT_PROBE_FORMAT` (Rgba16Float). Today they are at depth resolution and only `r` is used |
| the mask reads the memory only where the near-hand zone (`focus_area.b`) is above 0 | **confirmed** | depth_mask.wgsl:56-63. The near zone is the hand's bounding sphere + 6 cm (`PLAYZONE_RANGE` in focus_area.wgsl), softened from +3 to +6 cm |
| mask is 428 x 448 per eye and runs every frame (27.6 M texels/s) | **confirmed** | dump JSON mask 428 x 448, `DEPTH_MASK_DOWNSCALE` 5; `encode_depth_mask` runs on every frame |
| "capsules at the depth's capture time: step 1 already looks the joints up there" | **partly** | `arm_motions` looks up only the **wrist** at the capture time (client stream.rs:1023). The memory pass needs all capsules at that time: one more `locate_hand_joints` per frame. Easy, but not already there |
| wheel learner on the camera thread of `auto_color_key.rs`, 320 x 240 RGB | **confirmed** | that thread already converts frames to 320 x 240 RGB |
| steering angle "sent with the tracking data" | **wrong direction** | tracking goes from the headset to the PC. The angle has to go from the PC to the headset. `ServerControlPacket` has `ReservedBuffer` on the control socket, but there is no angle message, no timestamp mapped to the headset clock, and no PC-side reader (DirectInput, the vendor SDK or SimHub). None of this is in the build steps |
| carry readback "about 400 KB each" | **wrong at 512 x 512** | 400 KB is today's 320 x 320 f32 playzone dump. The new memory is 512 x 512 x 4 x 2 bytes = 2 MB per eye. Either extract `r` + `b` first (1 MB) or read 2 x 2 MB every 0.5 s. The cost table's "0.4-1 MB" is low |

## 3. Claims about the data (my own reruns)

### L1, still objects drawn too big (c1_edges, c1b, c1c)

My method: clean depth steps of more than 10 cm, with the object inside the burst's play-area boxes and
the background outside them, left eye, head still. I simulate the shader's coverage along a scan across
each step (9 depths on the lookup ray, region feathering 3 cm). Then I find the strongest colour edge along
the same scan in the camera image nearest in time. The lead used 36 to 174 border pairs per burst; I used
110 to 2122 samples per burst.

| burst | camera edge inside the depth texel border | today's drawn edge (cut 0.2) outside the camera edge | at cut 0.8 | lead |
|---|---|---|---|---|
| 7, lap 0.71 m (1775 samples, 47 frames) | +0.35 texel (+1.5 mm), p10/p90 -2.7/+3.1 mm | **+0.76 texel, +3.5 mm** | -0.07 texel (-0.3 mm) | +0.43, **+0.76 texel / +4.2 mm**, -0.02 |
| 3, wheel 0.48 m, still part (2122 samples, 11 frames) | +0.65 texel (+2.2 mm) | **+1.05 texel, +3.7 mm** | **+0.27 texel (+0.9 mm)** | +1.23, **+1.70 texel / +5.6 mm**, +0.96 texel |
| 11, desk, edges at 0.70 m (144 samples) | -0.23 texel (-1.1 mm), two groups at -4 and +2 mm | +0.6 mm | **-2.9 mm (cuts into the object)** | the lead's pairs were at 0.31 m: +2.7 mm at 0.2 and +4.5 mm at 0.8, in the opposite sense (camera edge outside the drawn edge) |
| 13, desk, edges at 0.66 m (110 samples) | -0.75 texel (-3.4 mm) | **-1.8 mm** (already inside) | **-5.1 mm** | lead at 0.30 m: -0.9 mm at 0.2, +1.2 mm at 0.8 (opposite sense) |

- **Confirmed on the lap:** the same 0.76 texel in texels. The mm differ because of the texel size used
  (4.9 against 5.6 mm). A cut at 0.8 lands on the camera edge.
- **Wheel: the direction is confirmed, the size is not.** My offset is about 0.65 texel (2 mm, about one
  camera pixel) smaller than the lead's. By my numbers a cut of 80 leaves the wheel only +0.9 mm too big,
  not +3.2 mm, so the case for the erosion of step 6a is weaker than the design says. Neither measure is
  better than about one camera pixel (2.2 mm at 0.48 m, 3.3 mm at 0.71 m).
- **It is growth, not a shift:** on the lap and the wheel the offset has the same sign and size on every
  side of the object (c1c: +0.8..+1.9 mm on the lap, +2.1..+2.6 mm on the wheel). That supports
  "no camera model error", although I did not repeat the scale fit.
- **Not every object is drawn too big.** In bursts 11 and 13 the clean edges I found (one side of an
  object at 0.66-0.70 m) are drawn at or inside the camera edge already, and a cut of 80 would cut 3-5 mm
  into them. The lead's own burst 11 output points the same way. The design uses bursts 11 and 13 only
  against the matte. It does not draw the consequence for `edge_cut`.
- **Risk ruled out:** depth texels with no data hardly ever lower the coverage (c6: 0.02-0.09 % of the
  in-area texels would be lost at 0.8 only because of a no-data neighbour; edges themselves account for
  0.9-2.5 %).

### L4, the memory camera (c4_views, c4b, c4c)

| burst (head turn) | first depth view holds the hand (+6 cm), bounding sphere | the CURRENT eye view, same test | of the hand zone the eye actually sees, all of it in the first depth view | fixed 120 deg rig camera, sane aim |
|---|---|---|---|---|
| 1 (24 deg) | 0 % | 0 % | 0 % of hand-frames | 95 % |
| 2 (35 deg) | 17 % | 43 % | 49 % | 76 % |
| 3 (34 deg) | 0 % | 32 % | 8 % | 100 % |
| 4 (6 deg) | 50 % | 56 % | 50 % | 100 % |
| 5 (2 deg) | 67 % | 65 % | **100 %** | 100 % |
| 6 (2 deg) | 44 % | 44 % | **96 %** | 100 % |
| 7, 11, 13 (still) | 50 % | 50-53 % | **100 %** | 100 % |
| 8 (wave) | 16 % | 19 % | 85 % | 5 % |

- **The numbers reproduce** (lead: 0 / 19 / 0 / 50 / 77 / 55 % for bursts 1-6).
- **The wheel half of the argument is wrong.** With the head still (bursts 5 and 6) the current eye view
  fails the same test just as often, because the hands sit at the lower edge of the view. The part of the
  hand zone that is actually on screen is covered 96-100 % by the old depth view. "55-77 % on the wheel"
  is not evidence against a memory kept in a past depth view.
- **The head-turn half is confirmed:** 0-8 % in bursts 1 and 3, against 95-100 % for a rig camera with a
  fixed aim. So the wide camera is still the right choice, but only head turns justify it.
- **The aim rule is broken for this play area.** The volume centre is 0.16-0.43 m from the head (the area
  includes the seat). Across the bursts it sits on both sides of the design's 30 cm threshold
  (0.29 / 0.31 m). Where the "head to centre" rule applies, it aims sideways (burst 7) or backwards
  (burst 9), 90-180 degrees away from the hands. Use a fixed aim from the anchor or QR frame.
  Also, "the area's forward axis" does not exist in the region data: it has to be defined.
- A fast wave (burst 8) leaves a 120 degree cone aimed at the work space (5 %). This matters little,
  because the hands are then mostly outside the area.
- The depth view equals the eye view (94 x 99 degrees, the same tangents): **confirmed**.

### L3, claim shape test (c3_claims)

My own claim: remembered texels that the live depth no longer shows (more than 5 cm behind, or no data),
within **20 cm of the hand's centre** (the rule of design 4.4), in pieces of at least 30 texels.

- Correct claims (bursts 10, 11, 12, captures 641, 642; 10 cases): ring continuous **27-40 %** (p50 34).
  Lead: 22-36 %.
- Wrong claims (640, 607, 620-623, 625, 626, 627; 27 cases): **53-100 %** (p50 77). Lead: 53-97 %.
- After the move with a stale memory (643): 42-43 %. Lead: 45-53 %.
- **The separation reproduces, but the margin is thin.** One correct case is at exactly 40 % (burst 12,
  right eye) and one at 38 %. With the limit at 40 %, about 1 in 10 real grabs sits on the line. The
  result holds for a ring of 1-3 texels and a tolerance of 2-5 cm.
- **The lead's L3 used a different claim from 4.4:** "within the margin of a hand", not 20 cm of the palm.
  With 20 cm the wrong claims on the lap become large (1,500-11,000 texels), and `carry_max_texels` 2000
  refuses most of them on size alone. So the shape test really only decides the small cases: 640
  (55-64 %), 626/627 and 607. 640 is the closest.
- Suggestion: a limit of 45-47 % together with the size cap. And state which claim definition step 4
  logs.

### Not rerun (unverifiable by me today)

- **L2 wheel map:** not rerun. Its own output limits the claims. The scores leave out 8 mm around every
  hole border, which is where angle and centre errors show. At a cut of 30, 17 % of the "open" cells are
  wrong (precision 0.83). A centre error of 1 cm doubles the false holes (5.9 -> 11.7 %), so "forgives
  1 cm" overstates it. The scores count only cells seen at least 8 times (73 % of the solid cells in
  burst 6).
- **L5, A's wrist-gap numbers (78.5 -> 90.7 %, leak 3.4 -> 5.9 %), A's memory numbers, B's numbers:** not
  rerun.
- "The display-time joints lag about the passthrough's own delay": the knowledge base says the passthrough
  delay has not been measured, so this is an assumption.

## 4. Cost claims

| piece | design | check |
|---|---|---|
| edge cut | 0 | **confirmed**: one uniform component, the same number of instructions |
| wrist gap | 1 capsule test per hand per mask texel | plausible: the same kind of loop as `initial_distance` |
| room memory | about a third of the mask pass | **the arithmetic is right** (7.9-11 M against 27.6 M texels/s), **but it is an average**. The pass runs only on frames with a new depth image (every 3.4-4.4 frames), so it adds a spike of 2 x 262k texels to those frames (1.4 times the mask pass's texel count, each texel cheaper). With GPU peaks already at 95-97 % (KB), the risk is dropped frames on depth frames, which an averaged `vrstat` load can hide. Measure the frame time on depth frames, or spread the update over 2-4 frames by rows, or start at 384 x 384 |
| carry | +4-10 % of the mask pass, 0.4-1 MB readback | the lookup cost is plausible. The readback is 2 MB per eye unless it is reduced first. The hitch risk of a GLES readback on the render thread is not measured |
| wheel | 1-2 ms on the camera thread | plausible |

## 5. The first build step I would trust: step 1, `edge_cut`

Why:

- It is the smallest change: a constant pair becomes `area.z`, which is free, and the switch at 20
  reproduces today exactly.
- The effect points the right way on the lap and the wheel in an independent measurement, and the same
  on every side of the object, so it is growth and not a shift.
- No-data texels do not get in the way.

Change before building:

1. Offer **50 / 65 / 80**. By my numbers 50 still leaves +1.7 to +2.2 mm, and 80 lands at -0.3 / +0.9 mm
   on the lap and the wheel but -3 / -5 mm on the desk edges of bursts 11 and 13.
2. Use the uniform in the debug tint line too (stream.wgsl:167).
3. Add two things to the user's test: a place where a play-area **box face cuts through a surface** (desk,
   legs), and an object close at the desk. The cut moves those box borders about 2.2 cm inward at
   `edge_feathering` 0.03 (about 6 cm at 0.08) and shrinks the remembered zone near the hands by about
   0.8 cm. The design does not mention either.

Step 2 (wrist gap) can share the build. It rests on A's numbers, which I did not rerun.

## 6. Risks the design misses

1. **`edge_cut` is global.** Besides objects, it moves every soft region border: box faces cutting through
   surfaces (about ±1.1 cm per side of today's position at feathering 3 cm) and the near-hand memory zone.
   Some objects (bursts 11 and 13) are already drawn at or inside their camera edge, and 80 cuts 3-5 mm
   into them. "The game starts at the object" holds for the lap and the wheel only.
2. **The memory camera's aim:** the "head to volume centre" rule flips by 90-180 degrees when the volume
   holds the user. It needs a fixed aim from the anchor frame and a defined "forward".
3. **The motivation in L4 is half wrong.** It does not change the decision, but the test for step 3 should
   be a head turn of 20-35 degrees with the hands on the wheel, not "hands at the lower edge".
4. **GPU spikes on depth frames** from the memory pass (and from the carry readback), hidden by averaged
   load numbers.
5. **Steering angle transport:** a PC-side reader, a server-to-client message and a clock mapping are
   missing from the plan for `wheel 2/3`.
6. **The claim limit sits on the edge of the correct cases** (40 % against 40 % observed), and the claim
   definition differs between L3 and 4.4.
7. **The wheel map is scored only on hole interiors.** The openings of a real wheel next to the spokes are
   narrow, so the 8 mm band that was left out is exactly where it would fail. Wrong open cells
   (precision 0.83) would show as spots of game in the spokes with `wheel 3`.
8. **Edge measurements are good to about one camera pixel** (2-3 mm). The lead's and my wheel offsets
   differ by that much, so the user's eye at 50 / 65 / 80 should decide, not the offline number.
9. **Capsules at capture time are not "already" available.** One more joint lookup per frame is needed for
   the memory pass and the near-zone growth.
