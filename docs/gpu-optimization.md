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
