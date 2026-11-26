# Green-screen key (`debug.alvr.green_boxes`)

## What it is for

On the rig the top of the real steering wheel's rim is seen in front of the rig PC's monitor. The depth mask cuts
the rim there with blocky, late edges: the depth image is coarse (320 x 320) and 125-170 ms old. The rig PC now
shows a pure green rectangle on that monitor behind the rim (a separate tool). With this feature the headset shows
the **game** wherever the passthrough camera sees that green, and the **real picture** wherever anything else is
in front of it (rim, hands, arms, sleeves). The cut is the compositor's own colour LUT, per display pixel and at
display rate: no depth, no lag.

Everything is off unless `debug.alvr.green_boxes` names a box: unset or empty, the layer list, every shader output
and the picture are today's, nothing is read per frame except one property poll a second, and no `green:` line is
written.

## How it works

A passthrough layer can be keyed by colour (the alpha channel of an `XR_META_passthrough_color_lut` LUT), a game
layer cannot. So "green -> game" needs a game layer **under** a passthrough layer whose LUT makes green
transparent. The existing keyed layer cannot do it: its LUT keeps only skin.

1. **The zone.** A play-area box, named by its index, drawn in the headset's editor as a thin slab on the monitor's
   green rectangle. Every view ray that passes through the box belongs to the zone, whatever the depth says (the
   same ray-against-box slab test as the lever windows, always open: no hand gate, no timing). In the zone the depth
   mask is forced to coverage 1 (both game layers get a hole), cloud zone 0, trail 0 and memory-only 0
   (`depth_mask.wgsl`, `in_green_zone` in `depth_common.wgsl`). The box's domes are ignored: the slab test uses
   the box only.
2. **The green-keyed base passthrough.** While the zone is on, the bottom passthrough layer is a passthrough layer
   of its own with a colour LUT style: RGB identity, alpha 0 for "monitor green", 255 for everything else
   (`PassthroughLayer::update_green_key`). The plain base layer is never styled: it is simply not submitted while
   the green one is, and when the zone goes off the green layer and its LUT are destroyed and the plain one is
   submitted again, exactly as today. A rule change updates the LUT in place; the runtime is called only on a
   change. The skin-keyed layer and its LUT are untouched.
3. **The under layer.** One more projection layer at the very bottom: the same two swapchains as the other game
   layers (no extra rendering), the same pose, opaque (no blend flag; its alpha is ignored), cropped per eye to the
   zone's rectangle in the view plus `green_pad`. Where the base passthrough's green is keyed out, its game picture
   shows. Outside its crop it does not exist, which is right at the bottom of the stack.

The layer stack while the zone is on, from the bottom (the four-layer key, `key_layers` 5 / `cloud 1`):

1. **under layer**: the game, opaque, only inside the zone's rectangle (+ pad) of each eye
2. **base passthrough, green-keyed**: the camera, transparent where it is monitor green
3. **lower game layer**: hole in the mask, the inside of the hands, and the whole zone
4. **skin-keyed passthrough**: the existing colour key
5. **upper game layer**: hole that also includes the rim of the hands, and the whole zone

With fewer key layers (`key_layers` 2/4 without hands, 30, 31) the under layer is still first, below the base
passthrough, in every branch (`green_screen::layer_order`, tested).

The game colour must really be in the swapchain where the game layers are transparent. On the colour-key path of
`stream.wgsl` it is (rgb = the game colour, not premultiplied, the two alphas packed). Without the colour key the
picture is premultiplied by (1 - shown), so the game colour is gone inside the mask. So the zone works **only
while the colour-key layers are in use** (`debug.alvr.color_key` with its LUT file, `XR_META_passthrough_color_lut`
and `XR_KHR_composition_layer_color_scale_bias`, Passthrough = Depth). It is also off on the YVR custom-reprojection
path (layer crops are disabled there) and without a play area placed from the marker. In every such case the
`green:` line says why.

Checked in `stream.wgsl`'s key path for the zone (mask r 1, g/b/a 0): `solid` is 1, so both packed alphas are 0;
the cloud zone and trail read 0; the playzone debug tint reads `remembered` 0; `see` (editor) only lowers the lower
alpha; the `!in_lower` repacking gives 0.5 = upper layer open, lower layer absent. The one thing that could make a
game layer opaque in the zone was the problem view (`lower_alpha = max(lower_alpha, problem.a)`): it is now skipped
in the zone. The editor's outline overlay is drawn opaque on top, also over the zone (intended).

### The LUT rule

For each LUT texel, its colour r, g, b (0..255, the identity grid `i x 255 / 31`) and m = max(r, b):

    s = min((g - floor) / soft, (g - ratio x m - delta) / soft)
    alpha = 1 - clamp(s, 0, 1)

Green must be bright enough (`floor`) and clearly above red and blue (`ratio`, `delta`); `soft` is the width of the
ramp. Keyed with the defaults: pure green, camera-like greens (40/230/60, 60/200/70, 20/140/30). Kept: black, greys,
white, skin tones, red, blue, yellow, and a washed-out bloomed green (170/245/170). Tested in
`alvr/graphics/src/green_screen.rs`.

## Properties

All are read every second while streaming (live; a change is applied at once, the LUT updated in place).

| Property | Default | Range | Meaning |
|---|---|---|---|
| `debug.alvr.green_boxes` | unset (off) | box indices, at most 2 (`3` or `3,5`; commas, spaces or semicolons, optional `#`) | play-area boxes of the zone, the `#<index>` of the editor's log. Unset or empty: completely off. Extra or invalid words are listed as `ignored` |
| `debug.alvr.green_floor` | 70 | 0..255 | the least green |
| `debug.alvr.green_ratio` | 135 | 0..1000 (hundredths) | green must exceed this x max(red, blue) ... |
| `debug.alvr.green_delta` | 25 | -255..255 | ... plus this |
| `debug.alvr.green_soft` | 24 | 1..255 | width of the ramp from kept to keyed |
| `debug.alvr.green_pad` | 30 | 0..300 (thousandths of the view) | the under layer's crop grows by this each side, over the mask's soft border |
| `debug.alvr.green_under` | 1 | 0..2 | 1: the under layer cropped to the zone. 0: none (keyed green then shows black: a debugging aid). 2: uncropped (to tell a crop error from anything else) |

What it needs: `debug.alvr.color_key` non-zero with its LUT file (the colour-key layers), Passthrough = Depth, the
play area placed from the marker. For the camera part of the log: `debug.alvr.auto_key 1` before the app starts
(the camera learner).

Debug views: with the streamer's debug view "Tint mask" the game colour is tinted blue inside the zone; as the under
layer shows the same colour, exactly the pixels where the camera's green was keyed are blue. The problem view does
not draw in the zone.

## The `green:` line

On `ALVR_CAM`, every 2 s while `debug.alvr.green_boxes` is set, and at once when the properties, on/off, the boxes
used or the layers submitted change. Example (on):

    green: on, boxes #2 at (0.000, 0.700, -0.500) m, 0.400 x 0.200 x 0.060 m; #6 (no such box, 3 boxes); key floor 70 ratio 1.35 delta 25 soft 24, pad 0.030, under 1 (cropped to the zone); view L u 0.410-0.630 v 0.220-0.350, R outside; under layer submitted; base layer green-keyed; camera: 200 px, green 0.75, between 0.05, green p10/50/90 r 30/40/50 g 190/200/220 b 40/50/60

- `boxes`: each box by index, centre and size in the area frame (as the editor's log), or why it is missing (a box
  removed in the editor shifts the indices: a missing index is left out, none left = off).
- `key`, `pad`, `under`: the properties as applied; `ignored:` the words of `green_boxes` that are no box.
- `view L/R`: the zone's rectangle in each eye's view uv (origin top-left), before the pad; `outside` (the eye
  gets a one-pixel under layer: a projection layer needs both views), `full` (a corner is behind or too close to
  the eye plane: the whole view, to be safe).
- `under layer`: submitted, uncropped, off (`green_under 0`), or not submitted because the zone is in neither view.
- `base layer`: green-keyed, or plain (the first frame, or the LUT failed: an error line `green: the green-keyed
  base layer failed: ...` says so).
- `camera`: what the passthrough camera (left, 320 x 240) saw through the zone at the learner's last pass: the
  pixels whose ray from the camera crosses a box, on a grid of at most about 300 over the boxes' rectangle in the
  image: how many, the share the rule calls green (s >= 1), the share in between (0 < s < 1), and the 10th / 50th /
  90th percentile of r, g, b of the green ones and of the rest. Or: `the learner is not running`, `no pass with the
  zone in the last 1 s`, `the zone is not in the camera image`.

Off: `green: off (<reason>), boxes ...; key ...` with the reason: Passthrough is not Depth; the custom
reprojection path (YVR); the colour-key layers are not in use; no play area placed; none of the boxes exists.
Switching the property off writes `green: off (no box in debug.alvr.green_boxes)` once.

## Setup at the rig, step by step

1. Start the green patch on the rig monitor (the separate tool), behind the top of the wheel rim as seen from the
   seat.
2. Make sure the colour key runs (`debug.alvr.color_key`, the usual settings) and, for the camera numbers,
   `debug.alvr.auto_key 1` was set before the app started.
3. In the headset's editor draw a thin box on the patch, a little **inside** its edges: where the box is larger than
   the patch the real monitor picture (not green) shows instead of the game. A few cm deep is enough; it can sit
   just in front of the screen surface.
4. Read the box's index from the editor's log line (`#<index> at ...`).
5. `adb shell setprop debug.alvr.green_boxes <index>`.
6. Read the `green:` line: `on`, the boxes, `under layer submitted`, `base layer green-keyed`, and the camera part.
   The camera's `green` share should be near 1 when nothing is in front of the patch, and drop by the rim's share
   when the rim is in front.
7. Tune: if the green share is low, look at the green class's / rest's percentiles: raise the patch brightness, or
   lower `green_floor` / `green_ratio` / `green_delta`. If real things that are not the patch go to the game, raise
   them. `green_soft` widens or narrows the ramp at the edge of the rule.
8. For a look: the streamer's debug view "Tint mask" shows the keyed pixels blue. `green_under 0` shows keyed green
   black; `green_under 2` uncrops the under layer.

## What to expect

- The rim, hands and arms in front of the patch are cut by colour, at display rate, with the compositor's per-pixel
  edges: the game shows exactly where the camera sees the green.
- The zone is everything along the view rays through the box, in front of it too: anything green in front of the
  patch (a green sleeve) also shows the game there.
- Anything real and green inside a play-area box elsewhere in view goes black (outside the under layer's crop) or
  shows the game (inside it): the base passthrough is green-keyed everywhere while the zone is on. Away from the
  play-area boxes the game layers cover the base passthrough, so nothing changes there.
- One more (cropped) projection layer, and the base passthrough becomes a LUT-styled layer: some compositor time
  (not measured; the four key layers cost 4.65 ms in `docs/gpu-optimization.md`).

## Not verified (PC only: tests, clippy, the Android build)

- Nothing ran on the headset. Not seen: whether the Quest runtime takes a second LUT-styled passthrough layer and a
  fifth composition layer, whether the base passthrough's LUT alpha blends over the projection layer below it as
  the skin-keyed layer blends over the lower game layer, and what it costs.
- The new shader code is only translated by naga (`*_translates_to_gles` tests) and run on the PC GPU (the zone's
  mask and picture: `green_zone_opens_both_layers_through_its_box`); the headset's GLES compiler is stricter.
- The image-rect y origin of the under layer follows the lower layer's crop (`key_crop 2`: top, otherwise bottom),
  as that is the convention in use; `green_under 2` tells a crop error apart.
- The camera's colours against the compositor's (1-8 levels median, KNOWLEDGE_BASE), camera auto exposure on an
  emitting green patch, and whether the compositor interpolates the LUT, are not measured: the thresholds are
  starting values to tune from the `green:` line.
- Whether a box drawn a little inside the patch, with `green_pad` 30, covers the mask's soft border in practice.
