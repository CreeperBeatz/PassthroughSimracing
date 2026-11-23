# Passthrough for sim racing: knowledge base

What was tried on a Quest 3, what works, what doesn't, and why. Status: 2026-11-22, branch
`auto-color-key`. Everything after "Depth passthrough" is experimental and controlled with adb.

Words used here:
- **measured**: numbers from the headset logs or the runtime.
- **observed**: judged by eye by the person in the headset, or on screenshots.
- **offline**: tested on screenshots, not live.
- **not tested**: compiles and passes the unit tests only.

## Short version

| Goal | Best approach so far | State |
|---|---|---|
| Desk, rig, wheel as real objects | Environment depth, "closer than X" region | Works, coarse edges |
| Hands | Live color key on the passthrough, in a small zone around the tracked hand, with automatically learned colors | Over simple backgrounds "basically solved" (observed) |
| Holes in a still hand | Solid fill of the hand segments that the camera confirms | Better, some noise left |
| Hands over complex backgrounds | Same, stricter colors, small zone | Leaks much lower, not gone |
| Forearms | Depth map only, shifted by the motion of the wrist (`arm_shift`), plus a keyed bridge from the wrist whose direction the camera chooses | Coarse edge, lags or jitters. The body tracking forearm was removed: not accurate enough. Bridge not tested |
| Held objects (wheel, shifter) | Remember the depth map without hands | Not tested on the headset |
| GPU budget | - | **Main open problem**: 58-73 fps with the last tested build |

## How it works now

1. **Depth mask.** For each pixel find the real surface with `XR_META_environment_depth` and decide if
   it belongs to the region (distance or box). Computed at low resolution, upscaled. The arms come only
   from here: around each arm the depth is shifted by the motion of the wrist since the depth image was
   captured (`arm_shift`).
2. **Hand zones.** Capsules from the hand tracking joints, no forearm, plus a short bridge from the wrist
   (see below). They say *where to look*.
3. **Color key.** The compositor applies a RGBA color table (LUT) to a second passthrough layer. It works
   on the image that is displayed, so its edge can't lag.
4. **Layers**, from the bottom: passthrough, game with holes (mask, solid fill), keyed passthrough, game
   with holes (mask, solid fill, rim). Both game layers are the same image: two alpha values are packed in
   one channel and unpacked with `XR_KHR_composition_layer_color_scale_bias` (2a and 2a - 1).
5. **Color learning.** The passthrough camera (Camera2, 320x240) shows what the tracked hand looks like and
   what is around it. A thread builds the LUT from that several times per second.
6. **Verification.** Each hand segment is compared with the camera image. Only confirmed segments that
   don't move are solid.

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

**Growth along the motion** (`key_sweep`, **not tested**). Without growth (`key_rim_max 0`) leaks were
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

**Bridge from the wrist to the forearm** (`key_wrist_len`, **not tested**). Between the hand zone (it ends
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
- **0-2 mm rim at rest, up to 16 mm when moving** (grows 45 ms x speed): nothing cut off at rest, fast
  motion "good enough" (observed).
- **Verified skeleton.** The solid fill comes back without the old halo, because wrong segments are not
  confirmed.

## What doesn't work

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
  time is already predicted; 0 ms is the current setting.
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

- The last build ran at 456 MHz and 48-50 degrees, so heat may be part of it.
- Suspects: the forearm test for all 9 depth samples of every pixel, and the bigger area of the hand pass.
  The next commit reduced both and used 1/5 resolution. Since then the body tracking forearm (capsules, arm
  removal) has been removed, so its cost is gone. **Not measured.**
- 3609efde, no hands in view (measured): 4 layers were submitted; key off took TW from 4.69 to about
  2.0 ms and load from about 85% to 62%, App time barely changed. Boost or SustainedHigh requested
  while streaming: level stayed 2, 456 MHz.
- The next commit (**not tested**): the two extra layers only while a hand is in view (plus 0.3 s);
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
- **Leaving the play area** pauses the app and restarts the stream. The key comes back by itself.

## Settings

`adb shell setprop <name> <value>`, most of them apply within 0.5 s.

| Property (`debug.alvr.`) | Default | Meaning |
|---|---|---|
| `color_key` | 0 | 0 off, another value (re)loads `/data/local/tmp/alvr_color_key.lut` |
| `auto_key` | 0 | 1 before starting the app: learn the colors, asks for the camera permission |
| `key_rim` / `key_rim_error` / `key_rim_max` | 0 mm / 45 ms / 16 mm | Zone at rest, growth with the speed, limit |
| `key_speed` | 2 | Joints whose speed grows the zone. 2: the wrist only (the arm moves; not tested), 1: palm, wrist and knuckles (fine for waving and closing the fingers, but a hand that turns in place grows it), 0: the fastest joint (moving fingers grow it too) |
| `key_sweep` | 1 | 1: the zone grows along the motion of the hand, 0: in all directions as before. Not tested |
| `key_sweep_max` / `key_sweep_iso` / `key_sweep_bias` | 40 mm / 25 % / 0 % | Limit of the growth along the motion (each side, before the bias), share of the growth that stays in all directions (limited by `key_rim_max`), -100..100: more ahead (positive) or behind |
| `key_inset` / `key_falloff` | 4 mm / 4 mm | Where the solid fill starts and how it fades out. 2 / 2 and 3 / 3 compared on the headset: a spread hand seen from the back is fuller, dark parts (palm, fist) are the same, the background is cut slightly less precisely; user: 4 / 4 is the best |
| `key_error` | 10 ms | How much the fill shrinks with the speed |
| `key_solid` | 6 | Segments that are always solid: 0 none, 6 wrist, 9 palm, 28 hand |
| `key_tight` / `key_bias` / `key_unknown` | 30 / 150 / 10 % | Strictness, preference for skin, unknown colors |
| `key_smooth` | 100 % | Smoothing of the skin colors, of a LUT texel (70..100). `key_tight` is scaled by its cube, so 70 is about a third of the floor: rarer skin colors are kept, not more around the common ones, but the tails of a widely spread skin (varied light) reach further (computed: 36 % more cells at 70). 100: as before. On the headset 70 leaks: skin kept 0.70 -> 0.84, but the background around the hand 1 -> 4 % (worst tenth 3 -> 16 %) and the scene 1 -> 11 %; user: "leaks a lot more". Values in between not tested |
| `key_confirm` | 12 % | `key_tight` of a second table that is never uploaded: the points on the axis of a segment are verified with it (the points beside it with the key), and the reliability comes from the share of the background around the hands that it keeps. Capped at `key_tight`, so it keeps everything the key keeps. 0: off. On the headset with 12: of the segments that are voted on 71 % are confirmed (65 % without), verified weight 0.55 (0.44), background unchanged; user: "the best one so far", in some cases the hand is still not full enough |
| `key_steady` | 1 | 1: a hand is verified while its palm (palm, wrist, knuckles) is tracked and slower than 0.5 m/s, each segment while its own two joints are too; its background is then sampled for the reliability only, and a hand with a fast finger is judged by its own background (fewer than 50 colors: no fill). Colors are still learned only from whole hands. 0: the whole hand or nothing, as before. Runs on the headset, not compared with 0 |
| `key_delay` / `key_scale` | 0 ms / 100 % | Hand pose offset (negative predicts), thickness |
| `playzone` / `playzone_auto` | 0 / 1 | New value: remember now, -1 forgets it (not tested). Automatic after 1.5 s without hands |
| `key_layers` | 4 | 4: extra layers only with hands in view, 2: never, 5: always, 30 / 31: always three (no upper game layer / no keyed passthrough). Not tested |
| `key_crop` | 1 | Lower game layer only around the hands. 0: full, 2: y origin at the top (if 1 is upside down). Not tested |
| `key_hands` | 1 | 0: no hands in the GPU passes. Not tested |
| `arm_shift` | 1 | 1: the depth around each arm is shifted by the motion of the wrist (with the key only up to the wrist, the hands are removed from the depth map; plain depth for a wrist that moved more than 25 cm or has no valid position), 0: plain depth. Also without the key. Not tested |
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
| `key_dump` / `key_dump_burst` | 0 / 1 | New value `n` other than 0: write the next `key_dump_burst` (1..30) learning passes to the same folder, `alvr_dump_<n>_<00..>`, also without hands. Each line `dump <n>/<index>: <path>` on `ALVR_CAM`. Not tested |
| `key_wrist_len` | 80 mm | Keyed bridge from the wrist towards the forearm, 0: off (as before), up to 300. The direction and the solid fill need `auto_key`, without it: opposite to the fingers, never solid. Not tested |
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

## Open problems, in order

1. GPU budget (see above). The four layers are most likely a big part of it.
2. Still hand with spread fingers: background is visible between them.
3. Complex and skin-colored backgrounds.
4. Forearms: depth only, coarse and late (shifted: jitters, plain: lags). Sleeves and watches. The wrist
   bridge (not tested) should close the gap at the wrist; whether a long one (up to 250 mm) can replace
   the edge of the depth is open, the late depth behind a moving forearm still shows the room.
5. Playzone map: verify with a wheel or a shifter. Turned spokes and levers are filled where they were.
6. Cockpit volume instead of a distance, with calibration. Lighting match was never tested on the headset.
7. A real drive with a wheel. Hands on a wheel were only approximated with a bottle and a monitor.

## Privacy

Camera images are only used in memory to learn colors. Nothing is stored or sent. Average colors and
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
