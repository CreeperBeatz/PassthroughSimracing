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
| Forearms | Same as the hands, elbow from body tracking | Smooth edge on screenshots, GPU cost too high, see below |
| Held objects (wheel, shifter) | Remember the depth map without hands | Not tested on the headset |
| GPU budget | - | **Main open problem**: 58-73 fps with the last tested build |

## How it works now

1. **Depth mask.** For each pixel find the real surface with `XR_META_environment_depth` and decide if
   it belongs to the region (distance or box). Computed at low resolution, upscaled.
2. **Hand zones.** Capsules from the hand tracking joints, plus the forearm. They say *where to look*.
3. **Color key.** The compositor applies a RGBA color table (LUT) to a second passthrough layer. It works
   on the image that is displayed, so its edge can't lag.
4. **Layers**, from the bottom: passthrough, game with holes (mask, solid fill), keyed passthrough, game
   with holes (mask, solid fill, rim). Both game layers are the same image: two alpha values are packed in
   one channel and unpacked with `XR_KHR_composition_layer_color_scale_bias` (2a and 2a - 1).
5. **Color learning.** The passthrough camera (Camera2, 320x240) shows what the tracked hand looks like and
   what is around it. A thread builds the LUT from that several times per second.
6. **Verification.** Each hand segment is compared with the camera image. Only confirmed segments that
   don't move are solid.

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
  2-7 cm, plus the whole scene without hands and arms. A lamp that is switched on is learned in less
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
- **Shifting the late depth of the arm by the motion of the wrist.** Jitters, and the edge stays coarse.
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
  The next commit reduces both and uses 1/5 resolution. **Not tested.**
- 3609efde, no hands in view (measured): 4 layers were submitted; key off took TW from 4.69 to about
  2.0 ms and load from about 85% to 62%, App time barely changed. Boost or SustainedHigh requested
  while streaming: level stayed 2, 456 MHz.
- The next commit (**not tested**): the two extra layers only while a hand is in view (plus 0.3 s);
  hands behind or beside the head no longer count. The lower game layer only covers the hands plus
  3% of the view; outside it the upper layer is opaque or both are transparent, so only soft mask edges
  can differ. Without the extra layers the game layer uses the upper alpha (2a - 1), the same as the
  four layers away from the hands. The focus pass is skipped without hands. Switches below
  (`key_layers`, `key_crop`, `key_hands`, `arm_removal`) to measure each part; the performance level
  is also requested at stream start, results and the layers in use are logged with `ALVR_CAM`.
- Arm removal stays on all 9 depth samples: once per pixel, the 3x3 blend would be kept or dropped
  as a whole at the edge of the arm in the depth map, so that edge would snap to the depth texels.
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
| `key_arm_rim` | 8 mm | Zone of the forearm |
| `key_inset` / `key_falloff` | 4 mm / 4 mm | Where the solid fill starts and how it fades out |
| `key_error` | 10 ms | How much the fill shrinks with the speed |
| `key_solid` | 6 | Segments that are always solid: 0 none, 6 wrist, 9 palm, 28 hand |
| `key_tight` / `key_bias` / `key_unknown` | 30 / 150 / 10 % | Strictness, preference for skin, unknown colors |
| `key_delay` / `key_scale` | 0 ms / 100 % | Hand pose offset (negative predicts), thickness |
| `playzone` / `playzone_auto` | 0 / 1 | New value: remember now, -1 forgets it (not tested). Automatic after 1.5 s without hands |
| `key_layers` | 4 | 4: extra layers only with hands in view, 2: never, 5: always, 30 / 31: always three (no upper game layer / no keyed passthrough). Not tested |
| `key_crop` | 1 | Lower game layer only around the hands. 0: full, 2: y origin at the top (if 1 is upside down). Not tested |
| `key_hands` / `arm_removal` | 1 / 1 | 0: no hands in the GPU passes / no forearm removal in the depth mask. Not tested |

The LUT file must exist even with `auto_key`: 32x32x32 RGBA bytes, indexed `[b][g][r]`. It is used until
enough colors are learned.

## Open problems, in order

1. GPU budget (see above). The four layers are most likely a big part of it.
2. Still hand with spread fingers: background is visible between them.
3. Complex and skin-colored backgrounds.
4. Seam at the elbow, forearm accuracy, sleeves and watches.
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
