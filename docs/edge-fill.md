# Edge fill (`debug.alvr.edge_fill`)

## The strip

The game layers are submitted with the pose and the fov the PC rendered the frame for, for that frame's
display time. The headset's compositor turns them to the pose of the actual display time. When the head turned
in between, the layer's edge moves into the view on the side the head turns towards, and the full-view
passthrough layer at the bottom of every stack shows a strip of the real room there (sometimes with a black
wedge beyond it, where the passthrough itself ends).

Measured (`edge-strip` analysis of the rig video of 2026-11-25 and 18 bursts): the pose lag between the
rendered and the displayed view is 0-0.8 deg median, up to 2.3 deg at p90 and 3.7 deg at p99; in the screen
recording the strip is 3-40 px wide while turning right (up to 175 px), about 4 px with the head still. The
recording shows it only at the right edge (the nasal edge of the left eye); in the headset it appears on every
edge the head turns towards.

## The fix

The fix keeps today's timing: the same pose, the same display time, and the compositor still does the
rotation. It only gives the game layers more picture beyond the edge.

- **Wider layers.** While `debug.alvr.edge_fill` is on, every game layer is submitted with the output fov
  widened by the margin on all four sides: the lower and the upper colour-key layer, the single layer when
  the key has no extra layers, the plain layer without the key, and the green-screen under layer
  (`debug.alvr.green_boxes`). The pose, the swapchains and their sizes stay the same.
- **The main pass** draws the stream view into the wider view. The quad of `reprojection_transform` grows
  by the needed amount of stream uv on every side (an immediate, `edge_fill` in `stream.wgsl`; 0 = today's
  quad). Fragments outside the target are clipped, so the pass costs no more fragments.
  - Inside the stream view (uv 0..1), every output pixel gets the colour and alpha that today's shader gives
    at that stream uv: the mask, the hands' focus, the zones and the alpha packing are all sampled there.
  - In the margin there is no game picture. The pixel gets the stream colour sampled at the clamped uv, which
    smears the border outward (the clamp comes before the foveated-encoding decode), with alpha 1. Alpha 1
    packs to an opaque lower and upper layer, and on the premultiplied path (no key) it is opaque too. No
    mask, focus or zone lookup is done there, so no room can show in the margin.
  - Except while the play area is edited or the problem view is on (`see` above 0): the margin then shows
    the room through the dimmed game by `see` as the rest of the view outside the mask does (premultiplied:
    rgb and alpha times 1 - see; key: lower 1 - see and upper 0 where the lower layer covers the margin,
    uncropped or inside its crop, else lower 1 and upper 1 - see). The leak map's tint is not drawn there.
  - With the streamer's debug view **Tint mask** the margin is **magenta**, to see where it shows.
  - Only the Depth passthrough mode makes the margin opaque. Blend and the chroma keys get the smeared colour
    with their usual alpha rule.
- **Converted rectangles.**
  - The lower layer's crop (`key_crop 1 / 2`) is computed from the hands' rect (stream uv), converted to the
    output view first (`stream_rect_to_output`).
  - The crop's pixels go back to stream uv for the shader's `key_lower` test (`output_rect_to_stream`).
  - The green-screen zone's rect is computed directly in the view the layers are submitted with, as it was
    in S1. With the fill on, that is the wider view, so the under layer's crop is right without a
    conversion.
  - The shader-side rectangles (`key_lower` / `in_lower`, the focus scissor, the hands' rects) stay in
    stream uv.
  - Both views have the same pose, so the map between them is linear per axis in tangent space
    (`alvr/graphics/src/edge_fill.rs`, tested).
- **Not on the custom reprojection path (YVR).** It draws into the display-time view, and the layer crops
  are off there anyway. The property is then logged as not applied.

## The property

| Property | Default | Range | Read |
|---|---|---|---|
| `debug.alvr.edge_fill` | unset = 0 = off (today's layers, quad and picture exactly) | 0..80, tenths of a degree per side (20 = 2.0 deg); negative is 0, above 80 is 80 | every 0.5 s while streaming, with the `dbg_*` properties. Live: each frame's layers carry their own fov, so a change takes effect at the next frame |

Sizing: the `perf:` line's `head turn render->display <median> deg p90 <p90>` is the pose lag the margin
has to cover. 20 (2 deg) covers the p90 measured so far, 40 (4 deg) the p99.

## The price

The same swapchain pixels cover a wider angle, so the game's pixel density drops. The swapchain is linear in
the tangent of the angle, so the loss is the ratio of the tangent ranges. On the Quest 3's left eye (tangents
-1.376 / 0.839 / 0.966 / -1.428):

| Margin | Density loss per axis (x / y) | Swapchain pixels in the margin (x / y, of 2144 / 2240) |
|---|---|---|
| 0.5 deg (5) | 1.8 % / 1.8 % | 39 / 40 |
| 1 deg (10) | 3.6 % / 3.6 % | 76 / 80 |
| 2 deg (20) | 7.0 % / 7.0 % | 151 / 158 |
| 3 deg (30) | 10.4 % / 10.4 % | 223 / 234 |
| 4 deg (40) | 13.7 % / 13.7 % | 293 / 308 |

This is about 3.5 % per degree, not the "margin / half-fov per side" estimate (2 deg: 4 %) of the plan. That
estimate counts degrees, but the swapchain's pixels are spread evenly in tangent, and at the edges of the
view (40-55 deg) one degree spans much more tangent than at the centre. The change line logs the loss for the
current stream view.

GPU: the same draw per eye, about 0 (the grown quad's extra part is clipped). The compositor's work is
unchanged: it samples per display pixel.

## Log lines (ALVR_CAM)

- The `stream:` line (once per stream start) ends with `edge_fill off (today's)`, `edge_fill 20 (2.0 deg per
  side)`, or `edge_fill 20: NOT applied (custom reprojection)`.
- On every change while streaming:

      edge fill: 2.0 deg per side (debug.alvr.edge_fill 20): the game layers' fov is widened, the margin shows the stream's border smeared outward, opaque, game pixel density -7.0 % x, -7.0 % y (left view)
      edge fill: off (debug.alvr.edge_fill unset or 0): the game layers have the stream's fov (today's)
      edge fill: 2.0 deg asked (debug.alvr.edge_fill 20), NOT applied: the custom reprojection path (YVR) draws into the display-time view

- With the fill on, the `green:` line's `view L/R` rectangles are in the wider output view's uv (the view
  the layers are submitted with).

## How to check on the headset

1. Without the fill (`setprop debug.alvr.edge_fill 0`), with the game running: hold the head still, then turn
   it steadily to the right at about 30-60 deg/s.
   - Expected: a strip of the room at the right edge (both eyes), and at the left edge when turning left.
   - Take `tools/progress.py shot edge-before "turning right"` or a Quest recording.
   - Note the `perf:` line's `head turn render->display` numbers.
2. `setprop debug.alvr.edge_fill 20` and the streamer's debug view **Tint mask**, then the same turn.
   - The strip should become magenta: the margin is now where the room was.
   - Magenta only while turning, and on the side turned towards, confirms the cause.
   - If room still shows beyond the magenta during fast turns, the margin is too small: raise it towards the
     `perf:` p90 / max (30, 40).
   - If magenta or room shows with the head still, something else is involved (not expected).
3. Tint mask off, the same turn: a smeared game edge, no room.
   - Take `tools/progress.py shot edge-after "turning right, edge_fill 20"`.
4. With the colour key's lower-layer crop (`key_crop 1` or `2`, only without the cloud) and with
   `debug.alvr.green_boxes`: the hands and the green zone must stay where they were. A shift means a crop
   conversion error. `key_crop 0` or `green_under 2` (uncropped) tells it apart.
5. Look at the game's sharpness: 7 % less density at 2 deg.

## Not verified (PC only: tests, clippy, the Android build)

- Nothing ran on the headset. Not seen: whether the compositor takes layers whose fov is wider than the
  display, and the strip's disappearance.
- The shader is only translated by naga (`stream_shader_translates_to_gles`) and run on the PC GPU, with
  the main pass alone and the renderer's transform:
  - `edge_fill_fills_the_margin_and_keeps_the_view`: the inner region equals the off picture resampled, and
    the margin is opaque with the border colour (magenta with Tint mask), on the key and the no-key path.
  - `edge_fill_margin_shows_the_room_while_editing`: while editing, the margin's colour and alpha, without
    the key, with the key uncropped and with a crop that reaches into the margin.
  - `edge_fill_off_is_the_shader_before`: with the fill off, the main pass is bit for bit the shader without
    the edge-fill lines, on 15 cases: key, no key, key crop, the editing look, Tint mask, lighting.
  - The headset's GLES compiler is stricter.
- The full `StreamRenderer::render` (GL context, swapchains) and the OpenXR layer submission do not run on
  the PC: the widened fov, the crop conversion in `stream.rs` and the y orientation of the converted crops on
  the device are only covered by the pure functions' tests.
- The overlay (play-area outline) is drawn with the output view's fov and so follows the wider view by
  construction; not looked at.
