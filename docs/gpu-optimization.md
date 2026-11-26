# GPU optimization: measurements and ideas (2026-11-26, not implemented)

Context: the first rig run (2026-11-25 evening, build `e918f6bd`, the judged depth-cloud settings) spent
374 of 2357 s in the runtime's GPU/THERMAL warning. The user asked to bring the GPU load down. The
measurements below come first. The ideas are saved for later at the user's request ("save the
optimization ideas you have in notes, we wont implement now").

## Measurements

Setup: the laptop streamer, the SteamVR home, the headset on its USB cable (AC power) and on a desk,
proximity sensor held "close" by adb, so no hands were in view. Build `e918f6bd` with the judged
properties (`cloud 1`, `cloud_window 24`, `cloud_cap 48`, `cloud_trail 24`, `key_auto 1`,
`key_layers 5`, `key_crop 0`, `mem_roll 1`, `arm_shift 2`). Each row is a property change from that
base, 6 s to settle, then VrApi averaged over 20-26 s (`tools/avg.sh`). GPU at level 2 = 456 MHz unless
stated. Raw output: `logs/gpu-ablation-20261126.txt`, `logs/gpu-ablation2-20261126.txt` (project
folder).

| Setup | App GPU ms | Compositor (TW) ms | GPU load | Layers |
|---|---|---|---|---|
| Judged base (measured 3 times) | 6.70-6.78 | 4.65-4.67 | 85 % | 4 |
| `cloud_window 12` | 6.73 | 4.67 | 85 % | 4 |
| `mem_roll 0` | 6.71 | 4.67 | 85 % | 4 |
| `key_hands 0` | 6.77 | 4.66 | 85 % | 4 |
| `arm_shift 0` | 6.75 | 4.65 | 85 % | 4 |
| `key_crop 1` (lower game layer cropped to the hands) | 6.80 | 4.00 | 81 % | 4 |
| `cloud 0` (layers 5, crop 0 kept) | 6.34 | 4.67 | 82 % | 4 |
| `cloud 0`, `key_layers 4`, `key_crop 1` | 6.33 | 4.00 | 77 % | 4 |
| `key_layers 2` (no extra layers) | 6.76 | 2.10 | 66 % | 2 |
| `cloud 0`, `color_key 0` | 5.29 | 2.09 | 55 % | 2 |
| Judged base, `debug.oculus.gpuLevel 3` | 6.34 | 4.36 | 80 % | 4 |
| Judged base, `debug.oculus.gpuLevel 4` (545 MHz) | 5.79 | 3.99 | 73 % | 4 |

What the numbers say:
- **The colour key's compositor layers are most of the cost.** Two extra full-screen projection
  layers (lower game, keyed passthrough; the upper game layer is the normal one) add 2.6 ms of
  compositor time, about 30 GPU points with the key's own 1.0 ms of app time.
- **The full lower game layer costs about 0.65 ms of compositor time.** The cloud forces
  `key_crop 0`; cropping it saves about 4 points.
- **The cloud's passes cost about 0.45 ms of app time** (3 points). The 24-cell window costs about
  0.05 ms more than 12.
- **The room memory, arm shift and hand passes cost about nothing measurable.**
- **The depth-mask path alone costs 5.3 ms of app time** (key off). A table from 2026-11-23 had 2.9 ms
  with passthrough off (another build), so the mask, playzone, light probes and the stream shader's
  extra work cost roughly 2.4 ms. That figure is not split further.
- **Raising the GPU level helps less than expected.** Level 4 is only 545 MHz: 85 % -> 73 %.

Not covered: hands in view, a real game (decoder load), battery power (the rig run), the rig's own
streamer settings.

## Ideas, in order of gain per risk

**A. Crop the lower game layer to the region where the key can act** (safe, about -0.65 ms compositor
time, -4 points; the picture is unchanged).
- With the cloud on, the region is the hands' rects plus the cloud zone. The zone needs a bounding box
  from the GPU: a small readback of the field or mask g channel, 1-2 frames late, padded by the cap.
  This is the "stats readback" of step 2 in `docs/depth-cloud.md`.
- A cheaper CPU proxy: the hand rects extended to the bottom edge of the view, where the forearms are.
  It would miss the cloud on things other than arms (shifter, handbrake).

**B. Confine the keyed passthrough to that region, then crop the upper game layer to it as well**
(the big one: up to about -1.5 to -2 ms compositor time, -10 to -15 points; quality risk).
- Today the keyed passthrough is a full-screen passthrough layer. Its LUT keeps skin-like colours
  anywhere in view, so the upper game layer must cover the whole view to hide them.
- With XR_FB_passthrough projected surfaces (XR_FB_triangle_mesh + a geometry instance), the system
  draws the passthrough only on a mesh. The other Quest 3 fork does this for its cockpit
  (`external/cockpit-passthrough/NOTES.md`); write our own code, don't copy theirs.
- New layer order: passthrough (full) / game with the solid holes (full) / keyed passthrough projected
  on a mesh covering region R / game again, cropped to R.
- Outside R it is 2 layers. Inside R it is today's 4.
- Risks:
  - A projected layer maps the camera image onto the mesh, not onto the real depth. A view-facing quad
    at about 0.5 m puts a forearm at 0.3 m off by about 2 degrees (about 1 cm). Rough estimate: 3 cm
    camera-to-eye offset, not measured.
  - A mesh from the hand joints (palm plane) and the arm's depth would be better. The runtime removes
    the hands from the depth map, so the depth alone can't place it.
  - Unknown whether the colour LUT (`XR_META_passthrough_color_lut`) applies to a projected layer.
  - Unknown whether the compositor's cost scales with the mesh's footprint.
- Probe first: one build with a switch, a quad mesh on the hands' rect, LUT on, compositor time and a
  screenshot compared.
- It is a design step for a new approach (Fable), then implementation (opus).

**C. Split the 5.3 ms of app time of the depth path.**
- Per-pass GPU timing: wgpu's GLES backend has no timestamp queries. Options: ablation switches per
  pass, or `GL_EXT_disjoint_timer_query`.
- Candidates:
  - the stream shader at full eye resolution (mask, focus and lighting samples per pixel);
  - the light probes;
  - the depth mask resolution (`DEPTH_MASK_DOWNSCALE`);
  - the playzone copy.

**D. Small cuts in the cloud** (about 0.45 ms at most).
- Skip the cloud's passes when no depth edge moved and no hand is near.
- Run them only on new depth images (25 per second), if they don't already.

**E. GPU level for rig sessions** (no code).
- `adb shell setprop debug.oculus.gpuLevel 4` brought 85 % -> 73 % at 545 MHz.
- It costs heat and battery, and the rig run was already in thermal warning 16 % of the time. A fan
  or cooler on the headset, or AC power, may matter more than any of this.
- The app's own XR_EXT_performance_settings request (sustained high or boost) did not raise the level
  in earlier tests (KNOWLEDGE_BASE, GPU budget).

## How to measure again

Start the streamer and SteamVR, start the client, then
`adb shell am broadcast -a com.oculus.vrpowermanager.prox_close` (undo with `prox_open`). Set the
properties of one row, wait 6 s, then `ANDROID_SERIAL=<serial> sh tools/avg.sh 26 "<label>"` (project
folder). Repeat the base at the end: the temperature drifts.

## Measuring (2026-11-26)

Stage S0: the client now tells what it streams and how frames arrive, and can leave out one piece of GPU
work at a time. Nothing is optimised. With every property below unset the layers, the shaders' output
and the picture are today's (the only change is two more log lines). Not yet run on the headset.

### Properties

| Property | Default | Range | Read | What it removes |
|---|---|---|---|---|
| `debug.alvr.dbg_skip_mask` | 0 | 0/1 | live (0.5 s) | the depth mask pass (pass 12 of the phase-1 pass table); the mask textures keep their last content, so the holes freeze where they were |
| `debug.alvr.dbg_mask_every` | 1 | 1..8 | live | the mask pass of each eye runs only every Nth display frame (eye 1 half a period after eye 0); in between the mask keeps its content |
| `debug.alvr.dbg_skip_focus` | 0 | 0/1 | live | the hands' focus pass (pass 11; it runs only with the key and a hand in view); the focus textures keep their content |
| `debug.alvr.dbg_plain` | 0 | 0/1 | live | the depth work of the main pass (pass 14): its immediates select the shader's Blend path with alpha 1, so one stream tap, no mask / focus / probe tap, no alpha packing. Same pipeline, no shader change; the game covers everything |
| `debug.alvr.dbg_no_srgb` | 0 | 0/1 | live | the 3 `pow` of the sRGB correction in the main pass: a second main pipeline built with `ENABLE_SRGB_CORRECTION` false at its first use (one hitch). Colours wrong |
| `debug.alvr.dbg_readback_ms` | 2000 | 0..60000 | live | the interval of the room memory's statistics readback (stats pass + copy + `map_async`, `room_memory_gpu.rs`); 0 = never (the `memory:` line then keeps old statistics) |
| `debug.alvr.ffr` | 0 (off) | 0..4 | stream start | fixed foveated rendering of the eye swapchains: 1 low, 2 medium, 3 high, 4 high-top (Meta's fifth level, given to the runtime as the raw value 4) |
| `debug.alvr.ffr_dynamic` | 0 | 0/1 | stream start | with `ffr`: `XR_FOVEATION_DYNAMIC_LEVEL_ENABLED_FB`, the runtime may lower the level while there is headroom |
| `debug.alvr.ffr_scaled_bin` | 0 | 0/1 | stream start | with `ffr`: create the swapchains with `XR_SWAPCHAIN_CREATE_FOVEATION_SCALED_BIN_BIT_FB` (what upstream ALVR used, see below); 0 chains the create info with no flag |

No `dbg_skip_memory` / `dbg_skip_cloud`: the existing switches already remove those passes completely.
- `mem_roll 0` (or unset): `room_memory::read_params` is None, `plan_memory` drops the memory and its
  textures, so the memory's clear / reproject / update / stats passes and the readback do not run. Side
  effect: today's playzone copy comes back (only on a capture) and the mask reads the copy instead of the
  memory.
- `cloud 0` (or unset): `cloud_input` is None, `plan_cloud` drops the cloud, its mark / rows / field passes
  do not run and the mask reads the 1 x 1 dummy field. Side effect: `cloud 0` flips the defaults of
  `key_layers` (5 -> 4) and `key_crop` (0 -> 1), which changes the compositor layers. Set `key_layers 5`
  and `key_crop 0` explicitly when the cloud's app cost is wanted alone (the table above did).

### Log lines (tag `ALVR_CAM`)

`stream:` once at every stream start, at the first frame with located views (illustrative numbers):

    stream: view 2144x2240, swapchain 2144x2240 SRGB8_ALPHA8, upscale off, FFE on (encoded 1472x1536, center size 0.45/0.40, center shift L 0.40/0.10 R -0.40/0.10, edge ratio 4.0/5.0, view ratio 0.687/0.686), refresh 72 Hz (72 asked), fov L l -51.0 r 41.0 u 43.0 d -52.0 deg (tan -1.235 0.869 0.933 -1.280), R l -41.0 r 51.0 u 43.0 d -52.0 deg (tan -0.869 1.235 0.933 -1.280), depth mask 428x448, passthrough Depth, lighting off, colour key on, key_layers 5, key_crop 0, dbg_skip_mask 0, dbg_mask_every 1, dbg_skip_focus 0, dbg_plain 0, dbg_no_srgb 0, dbg_readback_ms 2000 (all today's), ffr off (today's)

- view: the server's `view_resolution`; swapchain: the eye swapchains (view x `upscale_factor` only with
  upscaling) and their GL format; FFE: the negotiated foveated encoding (the staging texture has the
  encoded size); refresh: the runtime's current rate and the one asked for; fov: the located views
  (degrees and tangents); depth mask: per eye (view / 5); lighting: the server setting of the depth
  passthrough (`-` when not depth); colour key: its layer mode at the first frame (`off` if the key was
  not on yet; the `key:` line follows it); then the values of this stage's properties and what became of
  `ffr` (`applied`, or `NOT applied, <why>`).

`ablation:` on each change of a `dbg_*` property, with all their values:

    ablation: dbg_skip_mask 1, dbg_mask_every 1, dbg_skip_focus 0, dbg_plain 0, dbg_no_srgb 0, dbg_readback_ms 2000 (new settings)

`perf:` every 2 s while streaming (illustrative numbers):

    perf: display 72.0 fps, new frames 71.5 fps, repeated 1 of 144 display frames, frame age at display 3.1 ms p90 16.9, head turn render->display 0.42 deg p90 1.30, new frame interval at display 13.9 ms p90 13.9 max 27.8, timestamps 13.9 ms p90 14.0 max 27.9, decode 4.2 ms p90 6.0, decoder queue 0.6 ms p90 1.4, total latency 58 ms p90 64, prediction 55.0 ms

- display / new frames: `render` calls and new decoded frames per second; repeated: display frames that
  drew the previous decoded frame again (out of all display frames of the window).
- frame age at display: the display (vsync) time minus the drawn frame's timestamp. The timestamp is the
  time the PC rendered for: the client's poll time plus its prediction offset. So about 0 means the
  prediction hit the display time; a repeated frame adds one display interval; a steady positive value
  means the prediction is too short.
- head turn render->display: the angle between the orientation the frame was rendered for (the server's
  view pose, left eye) and the one located for the display time. It is what the compositor's
  reprojection has to correct, and what opens a gap at the picture's border.
- new frame interval at display: between the display times of consecutive new frames (what the eye
  sees: 13.9 ms at 72 Hz, 27.8 = one repeat). timestamps: between the frames' own timestamps (the PC's
  pacing). A clean timestamp series with a rough display series points at the network or the decoder.
- decode, decoder queue, total latency: `alvr_client_core`'s statistics of each new frame, the ones it
  sends to the streamer (read through a new read-only accessor `frame_statistics`; nothing sent changed).
  prediction: the head prediction offset in use.
- Fixed arrays of 512 values per measure, no allocation per frame; values past 512 in a window are
  counted at the end of the line.

### Fixed foveation of the eye swapchains: why it was dead, how it is wired now

History (`git log -S`, `git blame` of `client_openxr/src/graphics.rs`):
- `27c86ca8` (#1646, 2023-05-30) added client-side foveation: `create_swapchain_with_foveation(info,
  SCALED_BIN)` and `swapchain.update_foveation(profile)`. Both were functions of ALVR's own fork of the
  `openxr` crate (`alvr-org/openxrs` rev `f1f47ff`); the renderer was then the C++ GLES one.
- `707b41a6` (#2366, 2024-08-31, "Move extra xr ext wrappers in-tree") switched to upstream
  `Ralith/openxrs` 0.19, which has neither function. The branch was deleted, `create_swapchain` got
  `#[allow(unused_variables)]` and kept the `foveation` argument. The profile is still created in
  `stream.rs` from the server setting `clientside_foveation`, and nothing uses it. No comment in the code
  or the commit says this was meant; it reads as a casualty of the crate switch.
- The crate in use (0.21.1) still chains nothing in `Session::create_swapchain` (`next: null`), but has the
  raw types and function pointers (`sys::SwapchainCreateInfoFoveationFB`, `sys::SwapchainStateFoveationFB`,
  `exts().fb_swapchain_update_state.update_swapchain`) and `Session::create_foveation_profile`.

Now (`perf.rs`, `graphics.rs`), with `debug.alvr.ffr` set at stream start the client
1. creates a profile from the properties (`XrFoveationLevelProfileCreateInfoFB`: level, vertical offset 0,
   dynamic), only if XR_FB_foveation, XR_FB_foveation_configuration and XR_FB_swapchain_update_state are
   enabled. They are requested at instance creation as before (`lib.rs`: `selected_exts` intersected with
   what the runtime lists), so nothing changes there when `ffr` is unset;
2. creates both eye swapchains through the raw `xrCreateSwapchain` with `XrSwapchainCreateInfoFoveationFB`
   chained (flags 0, or SCALED_BIN with `ffr_scaled_bin 1`); if that fails, as today without the chain;
3. applies the profile with `xrUpdateSwapchainFB` (`XrSwapchainStateFoveationFB`), then drops it (Meta's
   samples destroy the profile after the update too).

The `stream:` line says `applied` or `NOT applied, <why>` (missing extension, profile refused, create or
update error). Unset: the swapchains are created exactly as before, the server's unused profile included.
The lobby's swapchains never get it. Whether the Quest driver applies the foveation to a swapchain that
wgpu renders into through its own framebuffer is not known until it is measured: compare app GPU time.

What foveation does to this renderer (to look for on the headset):
- Only the main pass renders into the swapchain, so only it is foveated: in the periphery the driver
  shades bins at a lower resolution (QCOM texture foveation) and upscales them. The staging copy, the
  mask, focus, memory and cloud passes render into their own textures and cost the same.
- The alpha channel is not a plain coverage: with the colour key it packs two layers' alphas (0 hole,
  0.5 rim, 1 game, `stream.wgsl`), unpacked by the scale/bias of the two game layers. Where a bin is
  upscaled, alphas between packed levels get interpolated: a 0 -> 1 edge passes 0.5, which unpacks as
  "rim" (lower layer opaque, upper open), a thin band where the keyed passthrough shows; a 0.5 -> 1 edge
  gives 0.75 (upper layer half open). Expect faint keyed fringes along hole edges in the periphery, mostly
  invisible where the key keeps nothing. Without the key (premultiplied alpha, `1 - shown`) the
  interpolation is only a softer edge.
- The mask edge gets coarser in the periphery: the mask is sampled per shaded pixel, and with fewer shaded
  pixels its smoothstep edge is resolved on the coarser grid (stair steps of the bin's scale). The rig's
  edges near the view's lower border (wheel rim, forearms) sit in the periphery of the higher levels.
- The game image in the periphery is already low resolution from FFE (foveated encoding); FFR on top
  costs little visible game detail there, but its centre is the lens centre, not FFE's shifted centre.
- `ffr_dynamic 1` lets the level change at runtime: compare timings only with it off.

### Measurement recipe

Same protocol as "How to measure again" (prox_close, one row's properties, 6 s, `tools/avg.sh 26`, the
base again at the end). Keep the judged base properties, then set ONE of these at a time:

| Set | Isolates | Expected if the phase-1 ranking holds |
|---|---|---|
| `dbg_skip_mask 1` | the depth mask pass | app GPU -0.3..0.8 ms |
| `dbg_mask_every 2`, then `3` | how the mask cost scales with its rate | about 1/2, then 2/3 of the `dbg_skip_mask` saving |
| `dbg_skip_focus 1` (hands in view) | the focus pass | small |
| `dbg_plain 1` | the depth-specific work of the main pass (mask and focus taps, alpha packing) | -0.3..0.8 ms |
| `dbg_no_srgb 1` | the sRGB `pow` of the main pass | -0.1..0.3 ms |
| `dbg_plain 1` + `dbg_no_srgb 1` | what is left: the bare full-resolution stream copy (staging + main pass) | about 2.5-3 ms remain |
| `mem_roll 0` | the room memory's passes and readback | measured about 0 (table above) |
| `cloud 0` with `key_layers 5`, `key_crop 0` | the cloud's passes | measured -0.45 ms |
| `dbg_readback_ms 0`, then `100` | whether the readback stalls the queue | look at the GPU % peaks, not the mean |
| `ffr 1..4` (a new stream each) | the main pass at full resolution under foveation | the larger part of the fixed full-resolution cost if it dominates |

Read the `perf:` line next to each row: an ablation must not change `display fps` or `repeated`, else the
row measures a frame-rate change, not a pass.

### Measured on the headset (2026-11-26, build `e282ddca`)

The judged settings (`cloud 1`, `cloud_window 24`, `cloud_cap 48`, `cloud_trail 24`, `key_auto 1`, `key_layers 5`,
`key_crop 0`, `mem_roll 1`), no hands in view, GPU level 2. One property set at a time from that base, the
protocol above.

| Set | App GPU ms | Compositor ms | GPU load | What it says |
|---|---|---|---|---|
| Base | 6.69-6.81 | 4.6 | 84-85 % | |
| `dbg_skip_mask 1` | 4.92 | | | the depth mask pass costs about 1.8 ms |
| `dbg_mask_every 3` | 5.60 | | | two thirds of the mask's cost gone: it scales with the rate |
| `dbg_plain 1` | 5.62 | | | the mask tap, the focus tap and the alpha packing of the main pass cost about 1.1 ms |
| `dbg_no_srgb 1` | 6.35 | | | the 3 `pow` of the sRGB correction cost about 0.4 ms |
| `dbg_plain 1` + `dbg_no_srgb 1` | 5.39 | | | |
| `dbg_skip_focus 1` | 6.57 | | | the focus pass: 0.15 ms |
| `dbg_readback_ms 0` | 6.68 | | | the memory's readback: nothing |
| all four (`dbg_skip_mask`, `dbg_plain`, `dbg_no_srgb`, `dbg_skip_focus`) | 3.28 | | | the fixed full-resolution part: staging copy + bare main pass |
| `ffr 1`..`4`, with and without `ffr_scaled_bin` | 6.4-6.7 | | | `applied` in the log, no measurable effect |

- The phase-1 ranking was wrong about the mask: it is the largest single piece (1.8 ms), not 0.3-0.8 ms. The
  depth-specific work of the main pass (1.1 ms) is second, the sRGB correction (0.4 ms) third.
- **Foveation as wired does nothing**: the runtime accepts the profile (`applied`), the app time does not move
  at any level. Either the driver does not foveate a framebuffer that wgpu renders into, or the main pass is
  not bound by shading in the periphery. Not worth more work before the rest is cut.
- 3.28 ms is the floor of the current pipeline at this resolution; everything above it is the depth path.

## Stage S3: work removal (2026-11-26; measured in the evening, see "Measured on the headset (2026-11-26 evening" below)

Three properties, each off by default. With all unset the layers, every shader's output and the picture
are what they were at `90b11cae` (the uniform block grows by 80 bytes of zeros, the stream shader gets
branches that are not taken; tests below). Code: `alvr/graphics/src/gpu_rect.rs`, `stream.wgsl`
(`depth_passthrough`, the sRGB block), `stream.rs` (`render`, `encode_depth_mask_scissored`), the client's
`stream.rs` (`gpu_rect_request`, the lower layer's crop, the `rect:` line).

| Property | Default | Range | Read | What it does |
|---|---|---|---|---|
| `debug.alvr.gpu_rect` | 0 (off) | 0, 1, 2 | live (0.5 s) | 1: the active rectangle (below). 2: as 1, and with the streamer's Tint mask debug view the borders of the rectangles are drawn (yellow: mask taps, cyan: focus taps, about 13 px wide at 2144 px) |
| `debug.alvr.gpu_rect_pad` | 20 | 0..250 (thousandths of the view per side) | live, with `gpu_rect` | padding of the rectangle before it is rounded out to mask texels |
| `debug.alvr.fast_srgb` | 0 (off) | 0/1 | live; the pipeline is built at its first use (one hitch) | the sRGB correction's 3 `pow` become a cubic (`dbg_no_srgb` wins if both are set) |

`debug.alvr.fast_mask` (part C, a per-texel early-out inside the mask shader) was not built: see the end.

### The active rectangle (`gpu_rect`)

Per eye and per frame the CPU bounds the part R of the stream view outside which the depth mask is 0 in every
channel the main pass reads and the focus area is 0. Then:
1. The depth mask pass is scissored to R rounded out to mask texels. Its `LoadOp::Clear` clears the whole
   texture every frame (wgpu-hal GLES sets the scissor to the full target before the clear), so the rest is 0.
2. The main pass takes the mask tap only inside R grown by one mask texel (its bilinear tap reaches the next
   texel), the focus tap only inside the focus pass's scissor grown by one focus texel. Where a tap is
   skipped its value is taken as 0, which is exactly what the tap would read. Outside both, with no debug view,
   no room through the game (area editor), no problem view and no `cloud_ab` mark, the shader returns the
   game colour with alpha 1 at once, which is what the full path computes from zeros (key path: packed 1 in
   both layers; premultiplied path: rgb unchanged, alpha 1).
3. With the colour key's extra layers and `key_crop 0` (the cloud's default; not `key_layers 30`, not on the
   custom reprojection path) the lower game layer is cropped to the union of both tap rectangles (the
   existing `crop_view` / `key_lower` machinery). The keyed passthrough can only show where focus > 0 or the
   cloud zone is (mask g / b), both inside it; outside it the shader's alpha is 1 in both packings, so the
   picture is the same.

What R contains (each bound in `gpu_rect.rs`; the comment at its top has the derivation):
- the green-screen boxes and the open lever boxes seen from the eye (slab tests: rays through the box);
- the region: the depth mask classifies points on ONE ray of the depth camera C (through Q = E + t dir, t the
  refined distance, at least 0.05 m; with the depth's near plane n at least n cos(asin(|C-E|/0.05)) - |C-E|),
  each moved by the arm shift D, against the boxes grown by their tallest dome + the feathering. With D = 0
  the direction of Q - C lies in the cone of C over the grown boxes and the view ray is within asin(|C-E|/t)
  of it: the cone's rectangle grown by that angle (converted to tangents per axis; a test checks the growth
  against random directions). The recorded dumps put C 1.4 mm from the eye (median), 7 mm at p90, 23 mm at
  p99: the angle is 1.6 deg median, 8.5 deg at p90;
- where D is not 0 (the arm shift, up to 25 cm): the swept arm (four spheres of 14 cm at the wrist end and the
  weight's end, now and moved back) seen from the eye;
- the depth cloud's zone and trail (mask g, b): the field's camera C_F over the texels of M grown by the cap
  (or `cloud_fixed`) + 8 depth texels, seen from the eye at the lookup distance (`cloud_fetch`) with the angle
  asin(|C_F - E| / fetch); with `cloud 2` also the swept arms;
- the wrist gaps (a capsule of 5.5 cm around the gap segment);
- near the hands (focus b, with a playzone): the focus pass's rectangle and one texel.

The full view instead (logged as the reason): the distance region, the Show depth view, the problem view, the
area editor (also whenever `game_see` > 0), a depth dump or burst in that frame (and while a burst runs), the
`cloud_ab` mark, `dbg_skip_mask` / `dbg_mask_every` / `dbg_skip_focus` (a texture keeps an older frame),
the depth camera 5 cm or more from the eye (or too far for the angle bound), the eye within 2 cm of a lever or
green box, `cloud_cap` / `cloud_fixed` above the window's truncation (2 x `cloud_window` texels: beyond it
the zone is not tied to an edge; the judged 48 with window 24 is exactly at it), a cloud field of another
region or whose cone reaches behind the view, a growth angle above 25 deg.

Mask g and b without the cloud hold the proximity and the depth flag everywhere; only the Show depth view
reads them (a fallback) and the depth dump (a fallback). Outside R the scissored mask holds 0 there instead:
the picture is unchanged, the texture's content outside R is not.

When the eye is inside (or within centimetres of) a box, rays in every direction can meet surface inside it,
so R covers the whole view. The play areas of the dumps of 2026-11-24 had the eye inside a box (every dump).
The rig's area of 2026-11-25 ended with three boxes (wheel and dash, lap, side): whether the eye is outside
them is what the `rect:` line will show.

### The `rect:` line (`ALVR_CAM`, every 2 s while `gpu_rect` is on and the stream is Depth)

    rect: L u 0.000-1.000 v 0.425-1.000, R u 0.000-0.958 v 0.450-1.000 (last frame, mask pass); share of the view mean 54.3 % max 61.0 % over 144 frames; full view 3 frames (most: depth camera off the eye, 4 eye frames); lower layer cropped 144 frames

- per eye the mask pass's rectangle of the last frame in view uv (or `full (<reason>)`, or `empty`);
- the share of the mask's texels rendered, mean of both eyes, its mean and maximum over the window (a full
  view counts as 100 %);
- how many frames had at least one eye at the full view, and the most frequent reason (counted per eye);
- in how many frames the lower layer was cropped.

A change of `gpu_rect`, `gpu_rect_pad` or `fast_srgb` logs `gpu work: gpu_rect 1 (pad 20), fast_srgb 1 (new
settings)`; the `stream:` line ends with the values at the start (`..., gpu_rect 0, fast_srgb 0`).

### `fast_srgb`

stream.wgsl decodes sRGB to linear: below 0.04045 c / 12.92 (unchanged, cheap), above ((c + 0.055) / 1.055)^2.4.
The upper branch becomes c3 c^3 + c2 c^2 + c1 c + c0 (0.330696, 0.65613381, 0.014212526, 0.0015492375), a
minimax fit of the error after the swapchain's own sRGB encoding. The suggested polynomial (no constant term,
over the whole range) is 5 steps off in the dark (0.04-0.1). Largest error over 0..1 in f32: 0.285 of an 8-bit
step after the encoding, at most 1 step after rounding (`fast_srgb_within_a_step`); on the desktop GPU into an
`Rgba8UnormSrgb` target over all 256 input levels: 0 steps (`fast_srgb_within_a_step_on_the_gpu`). The
overlay keeps its `pow` (a few pixels).

### What the tests prove (on the PC: naga and the desktop GPU)

- `gpu_rect_bounds_every_scene`: 400 random scenes (head poses near a rig-like layout of 1-4 boxes with
  domes and yaw, the depth camera up to 2.5 cm off the eye and turned, random depth images with near surfaces
  of 10-20 cm, holes and the boxes' faces, near occluders right beside the boxes, hands in and out of view
  with sweeps and bridges, arm shifts up to 25 cm, wrist gaps, lever windows, a green zone, a cloud field
  built from a random older image by the reference of field_main with random held speeds, a random playzone /
  room memory texture, flip_y, edge cut, Tint mask view, `key_crop`, sRGB on and off; two scenes of five
  bare: only boxes and depth, no pad). In every scene: the mask rendered without the scissor is 0 outside R
  (r and a always, g with the zone, b with the trail), the scissored mask equals it inside and is 0 outside,
  the main pass with the rectangles (and the lower layer's crop) writes exactly today's pixels, and today's
  pixels equal those of the stream shader before stage S3. Last run: 400 scenes, 42 at the full view (34 area
  editor, 8 cloud reach above its window), 21 empty; the rectangle covered 77.7 % of the view on average and
  100 % at most (the random scenes are crowded: arms of 14 cm radius moved 25 cm, hands, levers; this is not a
  measure of the rig).
- `gpu_rect_holds_a_near_occluder`: the worst case built on purpose (the camera 1.6-2.2 cm off the eye, an
  occluder 12 cm in front of it beside the box): coverage appears 4 mask texels (about 8 deg) left of the box
  as the camera sees it; R holds it. With the angle term removed this test fails, and with the one-texel
  margin of the main pass's taps removed the random scenes fail (both tried, then restored).
- `gpu_rect_border_in_the_tint_view`: `gpu_rect 2` changes only border pixels, in the two colours.
- Unit tests of gpu_rect.rs: the hull cut by the near plane holds every point of the box, the angle growth
  holds every direction within the angle (random directions), the fallbacks, the focus scissor is the focus
  pass's, the lower crop holds the rectangle, the `rect:` line, the constants mirrored from depth_mask.wgsl.
- All earlier GPU tests (bit-identical comparisons with the shaders before the edge fill and others) still
  pass with every property unset.

### What must be measured on the headset

With the judged settings, `gpu_rect 0` then `1` (then `0` again for the drift), and `fast_srgb 0` / `1`, each
while looking ahead (game), down at the wheel, and up:
- app GPU time and compositor time (`tools/avg.sh`), GPU load;
- the `rect:` line: the share of the view and the full-view reasons (if the eye sits in a box, R is the full
  view and nothing is saved: then shrink the boxes around the head, or see what holds it);
- the `perf:` line must not change (`display fps`, `repeated`);
- `gpu_rect 2` with the Tint mask view: the yellow border must enclose every tinted (masked) region at all
  times, also while turning the head fast; a hole or a cut tint at the border is a bug in the bound;
- `fast_srgb 1`: a dark gradient (night scene, tunnel) side by side with `0`, no banding.

Expected if R covers the lower half while looking ahead: about 0.9 ms of the mask's 1.8 ms, most of the main
pass's 1.1 ms outside R, 0.65 ms of compositor time for the lower layer's crop; `fast_srgb` up to 0.4 ms.

### Measured on the headset (2026-11-26 evening, build `bfb5f3e2`, at home)

The build streams at 72 fps; the headset's shader compiler accepts the shaders of S3, the edge fill and
`cloud_own`. One property at a time, 15 to 20 s averages (`tools/avg.sh`), GPU level 2. The drift between two
base runs of one series was up to 0.4 ms (the headset warmed from 37 to 58 C in 20 minutes), so differences
below that are not findings.

| Case | Setting | App ms | TW ms | GPU % | `rect:` share |
|---|---|---|---|---|---|
| No box placed (region of the settings), on the desk, 4 layers | base | 7.36, repeat 6.93 | 3.99 / 4.02 | 84 / 82 | |
| | `gpu_rect 1` | 7.27 | 3.72 | 82 | 100 % |
| | `fast_srgb 1` | 7.47 | 3.72 | 84 | |
| | both | 7.64 | 3.68 | 85 | 100 % |
| | `edge_fill 20` | 7.24 | 3.91 | 83 | |
| | all three | 7.28 | 3.68 | 82 | 100 % |
| One thin box 0.6 m away, worn, hands in view at times, green key on, 5 layers | base | 6.68, repeat 6.75 | 4.30 / 4.23 | 82 | |
| | `gpu_rect 1` | 6.46 | 4.41 | 81 | mean 70 %, max 100 % |
| | `gpu_rect 1` + `fast_srgb 1` | 7.54 (fps 69, min 60) | 4.32 | 87 | 100 % |

What this says:
- `gpu_rect`: 0.2 ms at a 70 % share, nothing at 100 %. The compositor time of the cropped lower layer went
  from 4.0 to 3.7 ms in the first series. Hands in view widen the rectangle (their focus region is part of
  it), and a box around the head makes it the full view: the rig layout of 2026-11-25 has box #0 around the
  driver's head. For driving (both hands and the wheel box in view) the rectangle cannot be small, so this
  cut does not pay as built.
- `fast_srgb`: no gain; every run with it was equal to or slower than its base. Not worth switching on.
- `edge_fill 20`: no measurable cost.
- The one bad sample (fps 69, minimum 60) was the run with both cuts on at a 100 % share at 57 C; not
  repeated, cause not known (thermal step or the settings).
- Raw lines: `logs/gpu-ablation-s3-20261126.txt` (outside the repo).

### Part C (`fast_mask`), not built

A per-texel early-out in the mask shader (a ray-against-grown-box slab test before the 12 depth loads) is not
exact: the mask classifies points on the depth camera's ray through the refined point, not on the view ray
(see the derivation above), so a view ray that misses every grown box still gets coverage beside a close
occluder (`gpu_rect_holds_a_near_occluder` shows it 8 deg off). An exact per-texel test would be the same cone
test per texel with the angle asin(|C - E| / t), plus the arm, gap, hand, lever, green and cloud terms; the
scissor already removes the texels outside their union, so the per-texel version would only save inside R
between the sources. Left out.
