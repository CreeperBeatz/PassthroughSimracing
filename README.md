# PassthroughSimracing

**Mixed-reality sim racing on Quest 3: sit in the virtual car, but see your real hands, steering wheel and body.**

Racing sims in VR are incredibly immersive, until you reach for a button box, a shifter, or your
drink and can't see your own hands. This project streams the game from your PC (Assetto Corsa,
BeamNG.drive, or any SteamVR title) and blends in the real world on the headset, **per pixel, using
depth**:

- Anything physically close to you (your hands, arms, legs, the wheel, the rig) is shown as the real
  world.
- Everything else is the game: the cockpit, the track, the world outside.
- Wave a hand in front of your face and you see it, with the game still rendered behind it.

## Why depth instead of a cut-out?

Existing tools (for example Virtual Desktop's passthrough cut-outs) remove a fixed plane or region of
the image. That works for a keyboard on a desk, but in a cockpit it breaks down. Your hands move in
3D, cross in front of the dash, and come towards your face.

This project uses the Quest 3 depth sensor ([Meta Depth API](https://developers.meta.com/horizon/documentation/native/android/mobile-depth/))
instead. For every pixel, it finds the real surface you're looking at and decides whether that
surface belongs to "you and your rig" or to the background. The mask follows your hands
automatically.

## How it works

```
PC (SteamVR + game)                          Quest 3
───────────────────                          ─────────────────────────────────────────────
Renders the game only     ── video ──▶     decode stream
(no extra load)                             │
                                            ├─ read the depth map (XR_META_environment_depth)
                                            ├─ per pixel: find the real surface along the ray
                                            ├─ inside the "keep real" region? → cut out the game
                                            └─ composite over the passthrough camera layer
```

All of the masking runs **on the headset**, inside the streaming client. That has two benefits:

- The PC only renders the game, at no extra cost.
- The real world is composited with the headset's native passthrough latency, not the streaming
  latency. Your hands don't lag behind the stream.

The streaming itself is built on [ALVR](https://github.com/alvr-org/ALVR), the open-source PC VR
streamer. On Quest, only one immersive app can run at a time, so the masking has to live inside
the streaming client. ALVR is the only open-source client where that is possible.

### Keep-real region

| Mode | What is shown as real | Setup |
| --- | --- | --- |
| **Distance** | Every surface closer than *N* metres to your eyes | None |
| **Cockpit volume** | Every surface inside a box around your rig | Box position and size (calibration planned) |

### Lighting the real world from the game

A real body that stays fully lit while the car drives into a tunnel breaks the illusion. With
lighting enabled, the client builds a small map of the game's lighting from the streamed image and
dims your real body and rig to match: tunnels, night driving and, with *locality*, regional shadows
such as a window frame.

The headset can only darken the passthrough image per pixel, not tint it. A base brightness below 1
leaves headroom so that bright game scenes can still make the passthrough brighter.

## Status

> **Early development. Not yet tested on a headset.**

| Feature | State |
| --- | --- |
| Depth passthrough mode (distance / cockpit box) | Implemented, shader-tested on desktop GPU |
| Game-driven lighting (global + local) | Implemented, shader-tested on desktop GPU |
| Debug views (depth visualization, mask tint) | Implemented |
| First on-device test | **Next** |
| Rig calibration (markers on the rig or controller touch points, spatial anchors) | Planned |
| Tracked hand mesh merged into the mask (sharper fingers) | Planned |
| Static rig geometry in the mask (stable wheel edges) | Planned |
| Per-region colour tint from the game (passthrough colour LUT) | Idea |
| Motion rig support | Idea |

## Requirements

- **Meta Quest 3 or 3S.** The depth map comes from Meta's Depth API.
- A Windows PC that can run SteamVR and ALVR (NVIDIA/AMD/Intel GPU with hardware encoding).
- A reasonably lit room. Passthrough and depth estimation both get worse in the dark.

## Getting a build

Every push builds through GitHub Actions
([Depth passthrough build](https://github.com/CreeperBeatz/PassthroughSimracing/actions/workflows/depth-passthrough.yml)). Each run produces two
artifacts:

- `alvr-client-quest-apk`: install on the Quest with SideQuest or `adb install`.
- `alvr-streamer-windows`: the PC streamer and dashboard. Use this instead of official ALVR: the
  depth passthrough settings only exist in this build.

**OpenXR games (e.g. BeamNG.drive):** SteamVR must be the active OpenXR runtime on the PC, otherwise the
game reports that no VR headset is connected (`XR_ERROR_FORM_FACTOR_UNAVAILABLE`). This is common if the
Meta Horizon / Quest Link software is installed. Set it once on every PC: *SteamVR Settings > OpenXR > Set
SteamVR as OpenXR runtime*, then restart the game. The setting is system-wide. To go back, set Meta Horizon
Link as the runtime in the Meta app. Native OpenVR (SteamVR) games don't need this.

Then, in the dashboard, go to **Settings → Video → Passthrough** and select **Depth (Quest 3)**.
Accept the *spatial data* permission prompt on the headset.

To build locally, follow ALVR's [build guide](https://github.com/alvr-org/ALVR/wiki/Building-From-Source).

## Where the code is

| Path | What |
| --- | --- |
| `alvr/graphics/resources/stream.wgsl` | Per-pixel depth mask and lighting (fragment shader) |
| `alvr/graphics/resources/light_probe_*.wgsl` | Game lighting probe passes |
| `alvr/graphics/src/depth_mask.rs` | Uniforms: rays, depth views, cockpit box |
| `alvr/graphics/src/tests.rs` | GPU tests with a synthetic depth scene, GLSL ES translation tests |
| `alvr/client_openxr/src/extra_extensions/environment_depth_meta.rs` | `XR_META_environment_depth` wrapper |
| `alvr/session/src/settings.rs` | `PassthroughMode::Depth` settings |

## Credits and license

This project is a fork of [ALVR](https://github.com/alvr-org/ALVR) by alvr-org and contributors,
which does all of the heavy lifting for streaming. For general ALVR setup and troubleshooting, see
the [ALVR wiki](https://github.com/alvr-org/ALVR/wiki).

Licensed under the [MIT License](LICENSE), the same as ALVR.
