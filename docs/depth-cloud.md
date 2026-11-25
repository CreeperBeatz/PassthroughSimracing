# Depth cloud (`debug.alvr.cloud`)

## What it is and why

The request: a cloud around moving things that comes from the depth alone and lives in screen space, whose size
comes from how far the pixels moved on screen; no hand or arm skeleton.

The depth image is 125-170 ms old when it is shown, so a moving arm is drawn where it was. Today the live colour
key decides only in a cloud around the tracked hands. The depth cloud measures, on every new depth image, how far
each edge of "real things in the play region" against "game" moved since the image before, with the head's own
motion cancelled, and opens a band around the edges that moved. Inside the band the live colour key decides per
pixel, as it does around the hands. An edge that does not move gets no band: the picture there is today's. It works
for anything that moves against the game (arm, sleeve, glove, shifter, handbrake, a carried thing); things that
move in front of the wheel or the lap are solid in the depth already and get no band.

Everything is off unless `debug.alvr.cloud` is set: unset, the picture, the layers, the arm shift, the log and the
burst records are today's.

## How it works

Per new depth image and eye (eye 0 on the display frame the image arrives, eye 1 on the next one, both at once if
eye 1 is still waiting), three small GPU passes run before the mask. An image is new when the runtime hands over
another one; an image with the capture time of eye 0's current field is the same image located again (slightly
moved poses) and is not new.

1. **mark** (320 x 320, a state texture per eye, ping-pong). Each depth texel is marked "in the play region" or not.
   The same world ray is followed back into the previous depth image of this eye in three fixed-point steps (as
   the room memory reprojects), so the head's own motion is cancelled. Where the previous surface is not on the
   ray (a texel uncovered beside a near thing), the texel is "unknown" and claims no change. A flip test drops
   texels that changed and changed back within three depth images (noise).
2. **rows** (160 x 160 cells of 2 x 2 texels). Along each row, the distance to the nearest in and out cell, for the
   new mask and for the previous one, within a window of 12 cells.
3. **field** (160 x 160, two per eye). Along each column, the exact distance to the edge of both masks inside the
   window. The difference of the two distances is how far the nearest edge moved in this pair. Below a deadband
   (2.5 texels per pair) it counts as no motion; above it, divided by the time between the images, it is a speed
   in texels per second. The field keeps the largest speed of the last about 200 ms per world ray (the held speed
   decays with `cloud_hold_ms`); below 15 texels per second it is 0.

**The one-sided rule.** In some pairs a cell sees an edge in only one of the two masks inside its window: an edge
that entered or left the window or the image, or one that moved more than 12 cells in one pair. `cloud_onesided`
says what such a cell does: 0, it claims no speed anywhere; 1 (default), it claims none only in a 12-cell band
along the border of the image, where edges enter and leave during head motion; 2, it claims a speed everywhere.
Offline, mode 0 everywhere costs 3-4 points of "held" in the driving recordings and halves it in the fast wave,
because a fast edge is one-sided too. The border-only default is reasoned, not measured: no recording has head
motion.

**A pair claims no motion** (a reset) and clears the held speed too when the eye has no field yet, when the two
images are less than 25 or more than 220 ms apart or a capture time is unknown, or when the region changed. It
claims no motion but keeps the held speed (decayed over the real time between the images) when the images are
110-220 ms apart (one skipped image: e.g. two images on consecutive display frames make eye 1 skip one, so its
pair spans two intervals while eye 0's spans one; both eyes keep their history) and when the head turned faster
than `cloud_head_max` between the two depth poses. The cloud does not run without the colour key, without depth,
or with the distance region (its edge moves with the eye). When depth passthrough is switched off or the depth
swapchain goes away, its textures (about 4 MB) are dropped.

**Per display frame** (72 /s) each eye gets a reach time: `cloud_gain` % of how far the live picture is ahead of
that eye's field (its age minus `cloud_lambda_ms`, the passthrough latency, at most 250 ms). **A stale field opens
nothing:** when that eye's field is older than 300 ms (display time minus its capture time, before the latency),
because the depth stalled (no new image, or the runtime keeps returning the same one), its zone is closed, also
with `cloud_fixed`. The field itself stays; the next image after such a gap is more than 220 ms from it and starts
over (a reset that clears the history). The mask looks up the
display ray in the field at `cloud_fetch_mm` (independent of the depth lookup), through the camera of that eye's
own field. The zone is open where the distance to the edge is below speed x reach time (at most `cloud_cap`
texels, with a soft edge of 1.5 texels), on both sides of the edge. It goes into the mask's g channel; coverage
(r, b, a) is never changed. The stream pass raises the key's focus with it only inside the lower key layer, so
the zone opens the upper game layer and the keyed passthrough shows what the colour table keeps. `solid` always
wins. The Tint-mask view paints the zone green.

**The trail** (`cloud_trail N`, off by default). Inside the depth mask, where the late depth still covers the place
the arm has left, the plain passthrough shows the room behind the arm (a ghost) and the key cannot turn it into
game. With N > 0 the zone on the mask side of an edge that moved reaches at most N texels (and at most the reach),
and in that border the depth's part of `solid` gives way by the zone: the key decides there as on the game side
(colours it keeps stay real, everything else becomes game). The hands' own solid fill stays. It is the first form of
"the trailing side handed to the key", for both sides of the edge alike. Its cost: inside the border, parts of the
arm whose colours the key does not keep (a sleeve) and static things in the region right beside a moving edge turn
into game while the edge moves. The border is where the field puts the mask side: with eye and depth camera apart,
the field (looked up at `cloud_fetch_mm`) and the mask can disagree by a little, and there the open zone of the game
side can also reach into the mask. The Tint-mask view paints the border green like the rest of the zone; the
playzone tint still follows the full mask.

While the cloud is on, an unset `key_layers` is 5 (the extra layers over the whole view) and an unset `key_crop`
is 0 (not cropped to the hands): the zone only acts where the lower layer is drawn. Set values win.

## Switches

All `adb shell setprop debug.alvr.<name> <value>`, read every 500 ms; each change is logged.

| property | default | range | meaning |
|---|---|---|---|
| `cloud` | 0 | 0..2 | 0 or unset: off. 1: the depth cloud; today's arm shift is off while its zone shows (not in today's phase of `cloud_ab`, not with the Show depth view, which hides the zone). 2: the cloud on top of today's arm shift (comparison, not skeleton-free) |
| `cloud_gain` | 150 | 0..400 | % of measured speed x age |
| `cloud_cap` | 14 | 4..24 | largest reach, depth texels (14 texels = 5.5 degrees = 4.8 cm at 0.5 m) |
| `cloud_hold_ms` | 200 | 0..500 | decay of the held speed (0: the newest pair only) |
| `cloud_dead` | 25 | 15..60 | deadband, tenths of a texel per depth pair |
| `cloud_lambda_ms` | 40 | 0..120 | passthrough latency taken off the depth's age |
| `cloud_fetch_mm` | 450 | 200..1500 | distance at which a display ray is looked up in the field |
| `cloud_head_max` | 120 | 0..1000 | deg/s of head turn above which a pair claims no motion (0: no guard) |
| `cloud_fixed` | 0 | 0..24 | N > 0: a constant reach of N texels wherever something moved recently |
| `cloud_onesided` | 1 | 0..2 | the one-sided rule above |
| `cloud_trail` | 0 | 0..24 | N > 0: inside the depth mask, a border of at most N texels (and at most the reach) along edges that moved is decided by the key instead of showing the room (the trail above). 0: off |
| `cloud_ab` | 0 | 0, 1 | 1: 2 s the cloud as set, 2 s today's picture (arm shift as set), with a green / red mark; the passes keep running |
| `key_layers`, `key_crop` | 4, 1 | | unset while `cloud` is 1 or 2: 5 and 0 |

## The log line

Tag `ALVR_CAM`, every 2 s while `cloud` is set, and the parameters on each change:

```
cloud: mode 1, gain 150 cap 14 hold 200 dead 2.5 lambda 40 fetch 450 fixed 0 ab 0 onesided 1 trail 0 | depth 17.0 /s
(33 ms 41 %, 67 ms 59 %, 100 ms 0 %), age 136 ms, a 96 ms, stale 0 | pairs 34: reset 2 (start 1, dt 0, head 1,
region 0) | layers 5 crop 0
```

`depth`: new pairs of the left eye per second and the share of each interval between depth images (over its pairs
other than start and dt resets); `age` and `a`: the mean age of the left eye's field at display and how far the live
picture is ahead of it (`-` without a field); `stale`: the display frames on which the left eye's field was older
than 300 ms (no zone: the depth stalled); `pairs`: the left eye's pairs since the last line (one per depth image;
eye 1 has the same a frame later, the burst record has both) and why some claimed no motion. When the renderer
created the cloud again (another depth size, switched off and on) the line counts from 0. When it cannot run:
`cloud: mode 1, off: no colour key`, `off: distance region` or `off: no depth` (also while depth passthrough is
off).

## The burst record

Index version 7. Each display record has `"cloud": null` while the cloud is off or not running, else
`{"mode", "shown", "gain", "cap", "hold_ms", "dead", "lambda_ms", "fetch_mm", "fixed", "ab", "onesided", "trail",
"eyes": [{"field_capture_ns", "prev_capture_ns", "dt_ms", "reset": null | "start" | "dt" | "head" | "region",
"head_deg_s", "a_ms", "reach_ms", "stale"} x 2]}` (`stale`: the field was older than 300 ms on that frame, no
zone; `reach_ms` is then 0). With these, the depth frames and their poses, the field of every display
frame can be rebuilt offline. `docs/tools/read_burst.py` reads it as `display["cloud_*"]` and still reads older
versions.

## Known limits

- **Lone noise in the image interior can open a brief band** with the default `cloud_onesided 1`: a patch of depth
  noise that flips into or out of the region where no edge was within 12 cells has an edge in one mask only, and
  outside the border band such a cell is measured. The flip test drops it on the pair that removes it again, but the
  pair that brings it claims a speed, held for about 200 ms. This is the price of not losing fast motion (a fast edge
  is one-sided too); `cloud_onesided 0` removes it at that cost.
- **Recentring** (a change of the reference space) forgets the fields: until each eye's next pair there is no zone.
  A recentre whose change takes effect at a later time is not waited for: the fields are forgotten when the event
  arrives, so the pair that spans the actual change compares images from two reference spaces and can claim motion
  (only a turn above `cloud_head_max` is caught).
- **The first `cloud 1`** compiles the three pipelines of the passes on the render thread: one hitch when it is
  switched on. It is also the first real check of these shaders on the headset's GLES compiler (the PC tests only
  translate them to GLSL ES and run them through another backend).

## What v1 leaves out, and what comes after

Left out on purpose: the trailing side as such (the late copy behind the arm stays as today unless `cloud_trail`
hands a border of the mask to the key, on both sides of the edge alike; the field keeps the direction for a
trailing-only rule); any change to the colour learner (a "mover" class); cropping and switching the extra layers from the zone
(v1 runs them over the whole view, about 85 % GPU load against 76-80 % today); a stats readback of the field; the
field as a burst file, a speed debug view, a second lookup at the mover's depth, a neighbour maximum of the speed,
a half-view A/B, the distance region.

Order after v1: (1) this build on the headset; (2) a stats readback, then crop and layer switching from the zone's
rectangle, and a "mover" colour class learned from depth labels at capture time with its keep and leak published;
(3) the trailing side, only with a measured key quality of 0.8 or more (`cloud_trail` is its first, switchable
form, ahead of that measurement); (4) a second lookup at the mover's depth and
a neighbour maximum of the speed behind a coherence test; (5) motion from the live camera inside the zone, the
only way under the 125-240 ms onset delay.

## What the offline check says

An offline check on seven recordings of one room, left eye only, re-implemented independently and confirmed.

- **Still scene:** the zone is 0.10 % of the image; 4.6 % of display frames are above 0.3 % (the limit was 5 %).
  With `cloud_lambda_ms 0` it is 9.2 % of frames.
- **Held** (share of what the late depth misses that lies in the zone), defaults, for the six moving recordings:
  73 / 51 / 57 / 60 / 67 / 18 % (one arm driving-like, one arm slow, two arms with a held wheel, wave, wave with
  both arms, fast wave). The zone is 1.4-6.9 % of the image while something moves.
- **With a perfect key** the arm's cut drops from 34 to 5 % in the driving-like recording (37 to 10 % and 39 to
  13 % in the two waves). The zone geometry is right.
- **With today's skin key on a plaid sleeve there is no gain:** the result equals plain depth, so `cloud 1` is
  19 / 17 points worse than today's arm shift in the driving-like recording and the wave. With the recorded
  colour table on bare arms the zone costs 5-7 points of leak.
- **Only 36 % of the edge is live at 0.15-0.4 m/s** even with a perfect key (8 % with the skin key).
- **Thresholds that failed:** against today's arm shift with a perfect key, the cloud is not 5 points better in
  two of three recordings (+11.9, +2.8, +2.1); and the on-time edge share at 0.15-0.4 m/s is below 50 %. `cloud_cap
  8` does not rescue the leak and costs 10-25 points of held. Speed-sized beats `cloud_fixed` at equal area by
  1.6-7 points.
- **Verdict: go for bare arms only.** Start speed-sized with the defaults; `cloud 1` with bare forearms, `cloud 2`
  first with sleeves.
- **A "mover" colour class** learned from the depth (not in v1): one recording carries the whole gain (the arm's
  cut 34 to 13 % at +0.6 points of leak); in the two-arm recording it equals today's arm, in the wave it is worse,
  and with bare arms over a skin-like backdrop it keeps 2 % of the arm.

## Headset test (about 10 minutes)

Before: the colour key on, the play area placed, sleeves pushed up for steps 1-5. Baseline: 20 s of GPU load with
everything unset.

| step | set | do (30-60 s) | look for |
|---|---|---|---|
| 1 zone at rest | `cloud 1`, debug view Tint mask | hands still on the wheel; then turn the head left and right; repeat the head turn with `cloud_onesided 0` and `2`, then back to 1 | no green beyond the hands' rim, also while turning. Green speckle at rest: stop and report. Does 0 or 2 change the head turn? |
| 2 zone in motion | same | one arm from the wheel to the shifter and back, in front of the game; then across wheel and lap | a green band on both sides of the forearm only where it borders game, growing with speed, gone 0.3-0.5 s after stopping; none over wheel and lap |
| 3 the picture | Tint mask off | the same, bare forearm | is the forearm's edge on time? room patches beside the arm? |
| 4 A/B | `cloud_ab 1` | steer and shift for 60 s | green mark: cloud, red: today. Which looks more like one piece? |
| 5 size | `cloud_ab 0`; `cloud_gain 100`, then `250`; `cloud_cap 8`, then `20` | wave, then driving | where clipping starts, where room patches start |
| 6 constant band | `cloud_fixed 8`, back to 0 after | driving motion | any difference to speed-sized? |
| 7 clutter | defaults | arm in front of desk, wood, cardboard outside the play area | patches in the band? then `cloud_cap 8` |
| 8 sleeves | sleeves down, `cloud 2` first, `cloud_ab 1`; then `cloud 1` | steer and shift | is `cloud 2` at least as good as today? `cloud 1` is expected worse (no arm shift) |
| 9 other movers | Tint mask on | shifter, handbrake, a carried bottle | does the band appear on the moving thing and vanish at rest? |
| 10 record + cost | `cloud 1`; `debug.alvr.burst <n>` during a wave, driving and a head turn; 20 s of GPU load | | the `cloud:` line (resets, `a`), GPU load against the baseline |

After: `cloud 0` and `cloud_ab 0`. One word per step: better / same / worse than today. If a bare arm with a
working key does not look better in steps 3-4, sleeves will not either: stop there.

## First headset run (2026-11-25)

Build a9a6191b, `cloud 1` with the defaults, bare forearm, a room the play area was placed in that
day, the stream over a laptop hotspot.

- **The shaders run on the headset.** The first `cloud 1` compiled the three pipelines without an
  error; this was the one thing the PC tests could not check.
- **The log** over about five minutes: depth 17 images per second (mostly 67 ms pairs), 141 ms old at
  display, no stale field. Resets: the head guard on 87 pairs (the head turns of step 1), a depth gap
  on 3, none for the region.
- **At rest and while turning the head** (mask tint on), the user: "no green tint, except some very
  minor details, like moving cables". So the border rule and the head guard hold in this room.
- **Arm moving in front of the game**, the user: the band appears on both sides and goes away, "there
  is some jitter, but generally yes. The forearm's edge feels a bit better, but there is ghosting in
  the background in some cases". And: "there were room patches and it was jittery at times".
  And on what the zone cannot do: "The color mask doesn't make pixels from the depth mask into VR
  space, and that feels more like a limitation. Maybe a small border around the depth mask, and then
  a color mask?" This is the reason for `cloud_trail` (the trail in "How it works").
- **Frame rate:** not a clean test. For part of the run the headset's GPU was clocked down and the
  client ran at 56-68 fps with the cloud off as well; later it was back at 72.

Reading (not measured): the room patches are the key keeping room colours inside the band, which the
offline check predicted (5-7 points of leak with the real table). The jitter fits a band that is
re-anchored to the depth edge 15-17 times a second and grows between two depth images: with a key
that kept only the arm this would not show, with patches inside it does. The ghosting is most likely
the late depth copy behind the arm, which v1 leaves as it is.

Not judged yet: the A/B against today's arm, a smaller band (`cloud_cap 8`), the constant band
(`cloud_fixed 8`), a longer hold, `cloud 2`, sleeves.

## Literature

Read at abstract or snippet level; mechanisms only, no reported numbers used.

| mechanism here | from | what was changed |
|---|---|---|
| extent = speed x time, zero for zero speed, sized by the largest speed that can reach a pixel | "A Reconstruction Filter for Plausible Motion Blur" (McGuire, Hennessy, Bukowski, Osman, I3D 2012) | no velocity buffer: the speed is the difference of two distance fields; "largest" is over time (the hold), not over tiles; a cap replaces "conservative" |
| compare the depth now with the previous depth brought over by the known camera pose | "Moving object detection for visual odometry in a dynamic environment based on occlusion accumulation" (Kim, Kim, Kim, ICRA 2020); the disocclusion mask of FidelityFX Super Resolution 2 (GDC 2022 slides, snippet) | the in-region mask is compared along the world ray, uncovered texels are "unknown"; a flip test instead of accumulation |
| a decaying memory of recent motion per place | "The Representation and Recognition of Human Movement Using Temporal Templates" (Davis, Bobick, CVPR 1997) | stores speed, carried along the world ray, re-anchored to the current edge |
| depth-certain regions plus an unknown band that colour decides | "Real-Time Video Matting for Mixed Reality Using Depth Generated Trimaps" (Beato, Pillat, Hughes, GRAPP 2012); "Video Matting of Complex Scenes" (Chuang et al., SIGGRAPH 2002) | the band is sized by measured speed x age, zero at rest, decided by the compositor's colour table; their fixed band is `cloud_fixed` |
| band width follows local motion, zero when still, not updated where unreliable | "Adaptive Trimap Propagation for Video Matting" (US 2014/0003719), variable-bandwidth "chatter reduction" patents (US 8971584 and relatives), as a pointer only | the deadband, "unknown = no change", the resets |
| the key acts only in the band, never in the certain core | compositing practice (matting notes) | the zone only raises the focus; `solid` wins |
| score at the time the result is shown, split by speed | "Towards Streaming Perception" (Li, Wang, Ramanan, ECCV 2020); "StreamYOLO" (Yang et al., 2022) | the offline check |
| exact distance in two passes, rows then columns | "Distance Transforms of Sampled Functions" (Felzenszwalb, Huttenlocher), cited from memory | a plain windowed minimum, exact inside 12 cells |

Deliberately not used:

| idea | from | why not |
|---|---|---|
| a one-sided band along the motion | Kim et al. | the direction of one pair is 150-200 ms old: one-sided bands held 20-65 % against 27-89 % both-sided |
| TileMax / NeighborMax, spreading each seed's speed (jump flooding, Rong and Tan, I3D 2006) | McGuire 2012; "A Fast and Stable Feature-Aware Motion Blur Filter" (Guertin, McGuire, Nowrouzezahrai, HPG 2014) | spreads noise: 1.9 % / 0.44 % of a still image against 0.10 %; kept for step 4 |
| motion from the fast sensor (camera flow, block matching) | "Temporal Upsampling of Depth Maps Using a Hybrid Camera" (Yuan, Gao, Fu, Xia, TVCG 2019); AutoDepthNet; DIS optical flow | the request is depth only; step 5 |
| moving the mask forward (warp, splat, extrapolation) | "Mask Propagation for Efficient Video Semantic Segmentation" (Weng et al., NeurIPS 2023); "Softmax Splatting" (Niklaus, Liu, CVPR 2020); "Image-space bidirectional scene reprojection" (Yang et al.); GFFE | knows nothing of newly covered pixels; the arm shift is this idea |
| trust from the disagreement of warped mask and live key | DVSNet (Xu et al., CVPR 2018), TapLab | the key mask exists only inside the compositor |
| handing the late copy to the key | "Improving Video Segmentation by Fusing Depth Cues and ViBe" (Zhou et al., Sensors 2017) | removes real pixels where the key does not know the mover; step 3, gated |
| a budget on the uncertain area from an error map | "Real-Time High-Resolution Background Matting" (Lin et al., CVPR 2021) | needs the learner's leak number; step 2 |
| colour from depth-confident labels away from the boundary | "Modelling depth for nonparametric foreground segmentation using RGBD devices" (Moya-Alcover et al.); "HandSeg" (Bojja et al.) | the source for the mover class, step 2 |
| matting solvers, scene flow, sub-texel distance transforms, CNN arm segmentation | Chuang et al.; Jaimez et al. (ICRA 2015); Gustavson and Strand; egocentric arm segmentation papers | no budget at 72 fps on the headset |

Not found in any of these: a key band sized by measured screen motion x data age and applied per display frame; a
compositor colour table as the in-band classifier; the flip test on the region mask before the distance fields.
