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
