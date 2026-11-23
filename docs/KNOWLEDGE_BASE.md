# Passthrough for sim racing: knowledge base

What was tried on a Quest 3, what works, what doesn't, and why. Status: 2026-11-23, branch
`auto-color-key`. Everything after "Depth passthrough" is experimental and controlled with adb.
**Start with "Where it stands and how to continue"**, the rest is the detailed record.

Words used here:
- **measured**: numbers from the headset logs or the runtime.
- **observed**: judged by eye by the person in the headset, or on screenshots.
- **offline**: tested on screenshots, not live.
- **not tested**: compiles and passes the unit tests only.
- **capture**: a `key_dump` of the headset (camera image, joints, tables, depth), see "Settings".

## Where it stands (2026-11-23) and how to continue

**State.** The headset has the build of 83a11bc7. The code after it only changes two defaults to what
was used on the headset by property: `key_wrist_len` 0 and `key_rim_max` 8. With the build that is on
the headset, set those two again after a reboot (its defaults are 80 and 16). SteamVR home, hands in
view: 72 fps, GPU load 80 % (median), peaks of 95-97 % (measured).

**A session.** Streamer and SteamVR on the PC as in the README (the dashboard doesn't start SteamVR),
Passthrough = Depth (Quest 3), region distance 0.9 m. On the headset a 32x32x32 RGBA LUT at
`/data/local/tmp/alvr_color_key.lut` (`adb push`; a learned one from `key_lut_dump`, or one made with
`tools/color_key_wand.py`), `auto_key 1` before the app starts (it asks for the camera), `color_key` to a
new value other than 0 at any time. Everything else is a default.

**Verdict of the user on that state.**
- Hands: good enough. Parts of the hand are still missing, "but at this point it's not noticeable". No
  more work on filling the hand.
- What bothers now: "elements in view not precisely cut, gripping objects in playable area and still,
  background leakage". In detail: the forearm and things held in the hand (a bottle) should be completely
  visible, but are cut out where the color mask for the fingers is made; the cap and upper part of a
  gripped bottle disappear; what the depth mask shows has jagged edges ("the quality is bad when I hold an
  object next to a VR area"); the room still shows as thin lines and patches next to the hand.
- Decided by the user: `key_inset` / `key_falloff` 4 / 4; `key_confirm` 12 ("the best one so far");
  `key_smooth` 70 leaks; the zone grows with the speed of the wrist only (`key_speed` 2); the skeleton is
  for the hand up to the wrist, no arm extension from it (`key_wrist_len` 0).
- The real rig will have QR codes as anchors, and the play area is set around them, not around the head.

**Evening session of 2026-11-23** (same build, live settings only, the user judging by eye).
- `arm_shift 0` (plain depth for the forearm): first impression "the length of the arm looks like it tracks
  worse", after switching back to 1 "the arm tracking is actually a bit better" without the shift. A small
  difference either way; left at 0 on the headset, the default in the code is still 1. Not settled.
- `key_solid 0`: "no noticeable difference". Stays 6. The 18 % less room predicted from the camera images
  did not show.
- `key_inset 6` against 4, switched every 3 s for a minute: "no noticeable difference". Stays 4. 8 not
  tried. The user: "at this point the fine tuning is so precise, I need a side by side". Fine settings of
  the hand can no longer be judged one after the other: a comparison needs one setting per hand, which
  needs a build.
- The region "Cockpit volume" ran on the headset for the first time: a box typed into the settings
  (placed from the head pose and the desk plane of a capture; the setting is applied live, no restart).
  72 fps as before. The user: "it does stay, the borders are jagged, but it stays consistent depth wise".
  A recenter "only moves the SteamVR page": the box stays. Measured on captures before and after (the
  headset was off for 25 minutes in between, then the recenter): the desk at the same height, a wall at
  the same direction within about 2 degrees and the same place within about 1 cm in stage space. The
  headset is on an automatic stationary boundary and the runtime announced a stage update three times at
  the recenter without moving the frame; when it does redefine it (reboot, lost tracking, a new boundary)
  is not known. That is what an anchor has to cover.
- The jagged border of the box: where a face of the box cuts through a surface. A level cut through
  surfaces at different distances steps on screen even with perfect depth (the eye is not at the height of
  the cut); the roughness along it is the depth map. A box whose faces lie in free air around the rig
  shows no such border. `edge_feathering` 0.08 instead of 0.03 was set live, not judged.
- Horizon OS on the headset: v210.
- What the user wants from the anchors (their words, shortened): when the software starts it recognizes
  which rig it is looking at, and what the borders should be from the position of that marker; an option
  to control with pinch, scale and placing different objects what the rig / play area looks like, "tools
  similar to Virtual Desktop"; QR codes can be printed. "We still need better edge precision on arms and
  wheel".

**Open for the user.** Way A or way B first (below)? Does "no arm extension" also mean `arm_shift 0` (the
depth is shifted along a 45 cm arm from the wrist) and dropping the 50 cm forearm that the learner
excludes from its samples? How many QR codes, and the size to keep after the first measurement? When can
captures be taken with a real wheel?

**Answered by the user, 2026-11-23 (evening).**
- Motion rigs are out of scope: a code never moves once placed.
- The codes are mounted on the rig (flat or standing) or shown on the monitor, facing the driver (a
  standing code), as the anchor. Where they sit still has to be in the camera's view when looking ahead
  (wheel deck or monitor stand, not the base behind the legs).
- Rule: a code is used as it is and measured precisely, and that measurement is the baseline. Its size
  can be changed after the first accurate measurement.

**What the day established** (from captures unless noted; points 1-4 and 6 were re-measured by a second,
independent analysis, which changed several first results; 5 is from the logs, 7 was measured with the
user, 8 on 2 captures).
1. *Room next to the hand.* 11-13 % of the hand's area in the camera image, mostly a fringe within 12 mm
   of the hand's edge. About 45 % of it (36-57 % depending on the hand mask and on the offset of point 2)
   is shown by the solid fill, which shows any color: the round ends of the always solid wrist capsules
   past the wrist and the heel of the palm, and confirmed capsules that sit beside the hand. The wrist
   bridge gave an estimated 16-23 % (its direction is not in the captures and was reconstructed; off now),
   the growth with the speed at least 17 %. Part of the "thin lines" may not be the key at all: in one
   screenshot a fringe runs along the sleeve 17-22 cm from the wrist, beyond any key zone, so most likely
   the coarse edge of the depth mask.
2. *The projected skeleton sits beside the hand in the camera image*, by 5-6 px (about 10-12 mm) in the
   first 27 bursts, as if the hand were 1.5-2.7 cm nearer than it is (the same as a 4.5-6.5 % scale about
   the image center, depending on the target; hands alone can't tell the two apart). Probably not a
   camera error: the crop arithmetic of `camera.rs` reproduces the logged intrinsics, no other crop or
   binning assumption predicts the fit, and one later set of captures (same boot, same calibration) shows
   no offset at all. Not excluded: the lens distortion and the active array are never read or logged. A
   time skew doesn't explain it (0-13 ms). Most likely the hand tracking places the hand too near,
   depending on pose or session. A fixed correction makes the set without offset worse (1.1 -> 2.0 px),
   so none is built. Where the offset is present, a replay with the projection moved by 2.7 cm raises
   the skin samples that land on the hand from 0.75 to 0.83 and the true segments voted in from 0.77 to
   0.90 (offline, tables learned with the old projection). How much of the 0.8 of its own skin samples
   that the key keeps on the headset this explains is open: that number also rises where there is no
   offset.
3. *A background model with places* (the user's idea: remember how each place of the background looks
   and compare) is not what the code does: the color table has no place, the leak map only rates leaks.
   Tested on 19 captures: as a gate on the key (drop a kept pixel that equals the remembered background
   at its place) it removes 35-37 % of the key's room leak for 1.5 % of the hand, 24 % without any depth;
   per capture 4-78 %, and skin over a beige desk lost 16 % of the hand. As the mask itself it fails on
   the near desk (wrongly marked background 13 -> 34 % without a depth fitted to the image). It finds
   what the key can't: sleeve 70 % (key 1 %), watch 70 % (10 %). A held bottle: one bottle, 3 captures,
   held where it usually stands, so nothing is known (captures of a bottle held in the air exist, see
   way A). The same flat room point varies by 2-3 levels (standard deviation; a tenth by 10 or more, and
   a quarter of the sightings fall into a neighbouring table cell) after a gain per frame; without the
   gain the exposure changes it by up to 48 %. None of this is built.
4. *A shape detector for the hand* cannot be exact: one camera pixel is 2 mm at the hand, a camera result
   is 45-160 ms old when used, and the camera outline inherits the offset of point 2. Measured in the
   camera image (probably optimistic: the outline and the hand mask it is scored against share their
   seeds): an outline that only limits the solid fill removes 21-28 % of the room for 0.6-1 % of the hand;
   flags per capsule side ("the camera sees an edge here") 19.5 % for 0.9 %, without a texture; a full
   outline that cuts everything 30-52 % for 3-6 %. What it cuts of a moving hand is not known (3 passes
   per speed range; 8-16 % above 0.25 m/s in those).
5. *The remembered playzone* can't help a carried object (it knows where things stood). With the new
   trigger it was copied whenever no hand was near the depth view for 1.5 s: 39 times in 20 minutes. But
   not reliably: in two streams the runtime kept the hands near the view for minutes, also during a 10 s
   sweep with the hands at the user's sides, and the copy was 150 s old when the bottle was gripped.
   While driving the hands never leave, so it is not refreshed then. Only useful for a wheel or shifter.
6. *The ring of background colors around the hand* is empty or under 50 colors in about 12 % of the log
   lines with a learned hand: the half-space that excludes the forearm removes everything when the arm
   direction is close to the line of sight (computed: empty up to 20-30 degrees). In 4 captures a bridge
   direction turned towards the camera caused it, but in the logs it is as frequent without a bent
   bridge. When no ring has colors (or a hand with a fast finger has fewer than 50 of its own) the
   reliability is 0 and the solid fill is off (mean weight 0.07 against 0.56); an empty ring of one hand
   is covered by the other hand's. No effect on the leak could be shown.
7. *The camera sees hands at the wheel position* (looking ahead): about 90 % of the voted segments are
   confirmed. A reach to the shifter leaves the image.
8. *The depth capture works on the headset* (GLES readback, 48 of 48), and the region really is a sphere
   around the eye (2 captures: the mask follows the radial distance in 99.8 % of the texels where sphere
   and plane differ).

**Added in the evening** (code reading, 8 captures with depth measured once with hand-drawn outlines, and
a search of public sources; not yet re-measured by a second analysis, so read the numbers as estimates).
9. *The color key never removes anything.* In the hand's zone a pixel is real if the depth mask says so,
   or the solid fill covers it, or the key keeps its color (`stream.wgsl`, the `max` of the three). The
   forearm and a held object are cut because the depth mask is already empty there, not because of the
   key: the user's "cut out where the color mask for the fingers is made" is the place, not the cause.
10. *The runtime removes more than the hand from the depth map.* Around the fingers about 3-5 cm of a held
    bottle are gone too (7 captures: no depth at the bottle's distance within 3 cm of the hand shape, most
    of it back from 5-8 cm), replaced by far values (median 0.8-1.5 m). The forearm is hardly touched: 1-2
    cm at the wrist. Of the bottle pixels that are not shown, 57-95 % lie outside the hand's key zone in 6
    of 7 captures: no setting of the key can show them.
11. *Continuing the object from the depth that remains* is an extrapolation: the bottle remains on one
    side of the hand only. A straight continuation in the camera image covered 24-68 % of the missing part
    in 5 captures (with 1-39 % of it beside the bottle) and failed in 2. A wheel rim passes through the
    hand on both sides and may behave differently: no captures.
12. *Depth against the camera image.* For a bottle held in the air the depth region reaches 2-3 camera px
    (4-6 mm) past the camera outline, 4-5 px at the worst tenth. Static depth edges fit the camera image
    best after a scale of about 5 % about the image centre, also in the set where the skeleton shows no
    offset: a hint, and no more, that part of point 2 is the camera model (the lens distortion is never
    read). Moving the border to camera edges means moving it by about as much as this is uncertain.
13. *No shortcut from the platform.* No Quest feature gives a native app a mask of the body, the arms or
    held objects; other cockpit tools show fixed windows or boxes placed by hand or with a controller,
    none shows arms or held objects cut out of the camera image. One report says the hands return to the
    depth map when hand tracking is turned off in the system settings (not tried here; the key needs the
    joints).
14. *For anchors:* the `openxr` 0.21.1 / `openxr-sys` 0.13.1 crates in use already contain
    `XR_EXT_spatial_entity`, `XR_EXT_spatial_marker_tracking` (QR, Micro QR, ArUco, AprilTag),
    `XR_EXT_spatial_anchor`, `XR_EXT_spatial_persistence` and `XR_EXT_future`: nothing to declare by hand.
    Meta's documents name `com.oculus.permission.USE_ANCHOR_API`, the log of the headset asks for
    `horizonos.permission.USE_ANCHOR_API`: declare both. QR tracking through the standard extension is
    reported working from OS v83 by one forum post; Meta describes QR poses as slow to update and not for
    moving things; nobody has published accuracy numbers. A published Quest 3 app built on ALVR keeps its
    passthrough shapes relative to one saved spatial anchor (`XR_FB_spatial_entity` with storage and query,
    the `com.oculus` permission) and lets the user place it by hand: saved anchors work for a native app of
    this kind, QR codes would only replace the manual realignment after the seat moves or the room setup is
    redone. The box of the region takes any pose in the shader; the settings only offer a yaw, and they come
    from the PC, so a calibration on the headset needs its own path.

    *Web search of 2026-11-23 on the QR route* (reported = forum, blog or sample code; documented = Meta or
    Khronos pages; nothing of it checked on the headset).
    - Documented (the spec, with Meta as author): `XR_EXT_spatial_marker_tracking` is ratified (OpenXR
      1.1.49) and needs `XR_EXT_spatial_entity` and `XR_EXT_future`. Reported by a Meta engineer in Meta's
      sample repository: both extensions are advertised on v205 and QR works from v83; a Godot post says
      v79 answered "QR not supported". v210 should list them, `capabilities ...` in the log says for sure.
    - Only QR codes are expected on Quest: Unity's package lists "QR Codes only" for Horizon OS, and the
      Godot post saw no ArUco or AprilTag. Documented: versions up to 10, no logo, no Micro QR; the payload
      is text or bytes; codes "relatively large and close", best in good light.
    - Documented: the spatial data permission is a runtime permission (`com.oculus.permission.USE_SCENE`,
      the user is asked once). Reported (Godot issue): a context created before the user accepted failed
      with `XR_ERROR_PERMISSION_INSUFFICIENT` and nothing worked until the app was restarted, so the
      context must be created after the grant. `USE_ANCHOR_API` is documented as a manifest entry; no
      Meta page names the `horizonos.` variant.
    - Documented: poses update at a lower rate than the frame rate, "not frame-perfect", not for moving
      codes; the pose of a moved code does update in the end. Reported (Meta forum, 2026-11, no reply):
      severe position jitter on a still printed code on a table. No numbers for size, distance, rate or
      accuracy are published; start from 10 cm and measure.
    - Documented: Meta's own QR sample suggests showing a code on a computer screen. Nothing is published
      about glare, flicker or light-on-dark codes on Quest; the general knowledge (moire between the pixel
      grid and the sensor, banding of a rolling shutter) says to use whole pixels per module, dark on
      light, a white margin of 4 modules, full brightness.
    - Documented (the spec): the options "static markers" (more accurate pose and size) and "fixed size
      for every code" exist only if the runtime lists their features. No source says whether Meta's does:
      the `capabilities ...` line answers it.
    - Documented (the spec): the pose is the centre of the code, +X to the right looking at its face, +Y to
      the top, +Z out of the front; the extents are the full width and height. QR codes have no id, only
      the payload.

**Method: what cost time today.**
- Offline tests on screenshots of the display with a skeleton placed by hand were wrong twice (no leak
  predicted for `key_smooth` 70; gains of a shape detector against a baseline the headset doesn't have).
  Test on captures, and let a second analysis re-measure the numbers a decision rests on: it found a
  screenshot matched to the wrong capture, an improvement that came from pooling two different sets, and
  fits that only held in-sample.
- Say which pixels: 4 px of a screenshot are 1-2 mm, one camera pixel is 2 mm at the hand.
- Captures: `adb logcat -s ALVR_CAM` running, `setprop debug.alvr.key_dump_burst 2`, then for each moment
  `setprop debug.alvr.key_dump <a new n>`; wait for `dump <n>/..` and `depth dump <n>: 9 files`, then
  `adb pull` the files folder of the app. Pair screenshots with captures by the time of those log lines,
  not by what the images show.
- Ask the user what they see before building for a number: the log said 21-30 % of the skin was dropped
  while the user no longer noticed missing parts, and wanted the leaks and the objects fixed.
- A live setting first, then a build: `key_inset`, `key_solid`, `key_confirm`, `key_smooth`,
  `key_wrist_len` can each be judged within a minute.

**Data and tools** (outside the repository, on the development PC: the captures show a private room and
are never committed).
- Captures of one room: 31 moments with the camera image and joints (poses, a cluttered desk, wheel
  position), 48 more with depth, mask and focus area in three sets: 2 of a gripped bottle; 21 taken while
  the compositor of SteamVR was dead (no game on the display, camera and depth are fine; the skeleton
  has no offset in this set); 25 with a sweep without hands, a bottle held in the air and gripped on the
  desk, and a sleeve (offset about 5 %). Screenshots of the display from the same minutes.
- A viewer that draws the capsules and the key's decision into the camera image, and the scripts and
  results of the analyses above (leak, ring, background model, camera model), each with its re-check.

**To continue, way A: iterate on what is there.** In the order of what it should bring:
1. Live, no build: is the skeleton tail gone with `key_wrist_len` 0; `arm_shift 0` if the arm still looks
   wrong (it shifts the depth along a 45 cm arm from the wrist); `key_solid 0` (predicted: 18 % less room
   for 1.25 % of the hand) and `key_inset` 6-8 (11-20 % for 0.65-1.2 %). Predictions from the camera
   images: only the user's eyes decide. Done in the evening session (above): `key_solid 0` and
   `key_inset 6` make no visible difference, `arm_shift 0` a small and unsettled one. The live settings of
   the hand are used up.
2. The background gate of point 3. First measure on the captures with depth: the registration with the
   depth of the sensor instead of a fitted one, the gate applied one pass late with the head pose of the
   display, a lamp change, and a session in the driving seat: behind hands that stay in place the
   background is never seen (about 16 cm of head motion is needed at 0.45 m), so the model needs a
   moment with the hands away, and hands that stay may end up in it; the turning wheel and the legs are
   not static. In the code the leak map already keeps colors per direction; it has no depth and moves
   with the head.
3. Edges of objects (the user's idea): move the border of the depth mask to the edges that the camera
   sees. Objects move little, so the late camera image doesn't matter if the result is kept in the room.
   Prototype on the captures with depth before any GPU work. A camera image of 320x240 is coarse (2 mm at
   0.45 m, about 5 mm at 1 m): the camera offers up to 1728x1728.
4. Held objects and the forearm near the hand: nothing works yet. Candidates: the difference from the
   remembered background on and near the forearm (sleeve 1 -> 45 %, but 21 % of the background near the
   forearm wrongly marked, mostly desk), and continuing an object from the part of it that the depth map
   still shows into the area the runtime removed. Captures of a bottle held in the air exist with depth
   and are not analysed yet: start there. Still missing: a wheel being turned with the hands on it.
5. Edge flags per capsule side for the solid fill (point 4), and capsule ends that don't stick out past
   the wrist.
6. The ring (point 6): the forearm as a wedge of capsules instead of a half-space, and keeping the last
   reliability for 0.5 s. On the captures no ring stays under 50 colors, but what it lets in of the arm
   is not known (0.7-25 % of the added pixels, depending on how the arm is outlined), and in a grip pose
   it samples the other, untracked fist as background. Low priority: no effect on the leak was shown.
7. An offset of the hands along the view for the learner only (point 2): move the joints and capsules,
   not the camera (the leak map, the scene samples and the captures use it). 0 by default, first as a
   property for tests (18-27 mm fits the first captures, 0 the set without offset), later chosen for
   still hands, smoothed over many passes, from image edges, within 0-30 mm. Check that segments on the
   hand and beside it still vote differently: that got worse in both later sets.

**To continue, way B: anchors on the rig and a mapped play area.** This turns the play area from a bubble
around the head into a box fixed to the rig, and most of way A gets easier in the rig's frame: the rig
and what is behind it can be learned once and stay valid, and only hands, arms, the turning wheel, the
shifter and held things have to be decided live.
1. What exists: the region "Cockpit volume" (an oriented box in stage space: center, yaw, size) in the
   settings and in the shader (`region_mask`, `world_to_volume`). Never calibrated, never used on the
   headset; entering stage coordinates by hand is not practical.
2. The anchors. The runtime lists `XR_EXT_spatial_marker_tracking` for this app (client log of
   2026-11-22, the list of available extensions). It hides `XR_EXT_spatial_entity`, `XR_EXT_spatial_anchor`,
   `XR_FB_spatial_entity` and the persistence extensions, because the manifest lacks
   `horizonos.permission.USE_ANCHOR_API` (`skipping extension ... missing uses-permission` in logcat).
   First step: add the permission, enable these extensions, and log whether QR codes are a supported
   marker type, how often and how exactly they are found, from which distance and angle. Not known: whether
   it works at all for a native app; Meta documents QR tracking for its own SDKs, as experimental and not
   for moving objects. Otherwise: find the codes in the passthrough camera image (it must be larger than
   320x240 for that; 22 ms late is fine for something that doesn't move), or touch points of the rig with
   a controller once and keep them as a spatial anchor. A world-anchored box breaks on a motion rig; the
   user ruled motion rigs out (2026-11-23, evening), so a code that never moves once placed is the case to
   build for. The codes sit on the rig (flat or standing) or are shown on the monitor facing the driver.
   A code is used as it is, measured precisely, and that is the baseline; its size can change after the
   first measurement.
3. From the anchors to the box: its pose every time the anchors are seen, its size once (settings), then
   `world_to_volume` as now. Test: the border stays on the rig when the head moves and after a recenter.
4. The map in the rig's frame, built while the hands are away (the user accepts a short moment for that):
   depth of the rig at a finer grid than one depth image (many images from a moving head), its edges
   from the camera, and the colors of what is behind it as seen from the seat. This is the background
   model of point 3 and the edge idea of way A with a fixed frame. The wheel turns and the legs move:
   they are not part of a static map.
5. The camera coverage (point 7) and the offset of the tracked hands (point 2) stay as they are.

## Short version

| Goal | Best approach so far | State |
|---|---|---|
| Desk, rig, wheel as real objects | Environment depth, "closer than X" region | Works, coarse edges |
| Hands | Live color key on the passthrough, in a small zone around the tracked hand, with automatically learned colors | Good enough for the user (2026-11-23): what is still missing is "not noticeable" |
| Holes in a still hand | Solid fill of the hand segments that the camera confirms, checked with a wider table (`key_confirm`) | "The best one so far" |
| Room next to the hand | Small zone, strict colors | Still shows as thin lines and patches; roughly 40-50 % of it (camera image) comes from the solid fill, not from the key |
| Forearms | Depth map only, shifted by the motion of the wrist (`arm_shift`) | Coarse, jagged edge, lags or jitters; cut out near the hand. Removed: the body tracking forearm (not accurate enough) and the keyed bridge from the wrist (a "skeleton tail") |
| Held objects | Remember the depth map without hands | Doesn't help: the upper part of a gripped bottle disappears. A memory can't follow a carried object and is only refreshed while the hands are away. Open |
| Edges of objects | Depth mask | Jagged ("the quality is bad"). Idea: move the border to the edges the camera sees. Open |
| GPU budget | - | 72 fps, 80 % load with hands in view, peaks of 95-97 % |

## How it works now

1. **Depth mask.** For each pixel find the real surface with `XR_META_environment_depth` and decide if
   it belongs to the region (distance or box). Computed at low resolution, upscaled. The arms come only
   from here: around each arm the depth is shifted by the motion of the wrist since the depth image was
   captured (`arm_shift`).
2. **Hand zones.** Capsules from the hand tracking joints, up to the wrist, no forearm. They say *where to
   look*.
3. **Color key.** The compositor applies a RGBA color table (LUT) to a second passthrough layer. It works
   on the image that is displayed, so its edge can't lag.
4. **Layers**, from the bottom: passthrough, game with holes (mask, solid fill), keyed passthrough, game
   with holes (mask, solid fill, rim). Both game layers are the same image: two alpha values are packed in
   one channel and unpacked with `XR_KHR_composition_layer_color_scale_bias` (2a and 2a - 1).
5. **Color learning.** The passthrough camera (Camera2, 320x240) shows what the tracked hand looks like and
   what is around it. A thread builds the LUT from that several times per second.
6. **Verification.** Each hand segment is compared with the camera image. Confirmed segments that don't
   move are solid, and the 6 wrist segments always are (`key_solid` 6). Solid shows any color: a large part
   of the room seen next to the hand comes from it (the round ends of the wrist capsules, capsules that
   sit beside the hand).

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

**Growth along the motion** (`key_sweep`, on the headset since c566b48e, never compared with 0). Without growth (`key_rim_max 0`) leaks were
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

**Bridge from the wrist to the forearm** (`key_wrist_len`, off since 2026-11-23: on the headset the user
saw it as a tail of the skeleton that "messes up where the arm continues", and it gave 16-23 % of the room
shown next to the hand, estimated). How it works, if it is ever needed again: between the hand zone (it ends
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

**Remembered playzone** (`playzone`, `playzone_auto`, `playzone_view`). On the headset (83a11bc7): it is
remembered at the start of a stream and whenever no hand is near the depth view for 1.5 s (39 times in
20 minutes), and its age shows in the `key:` line. In two streams it was not refreshed for minutes ("a
hand is near the depth view", also with the hands at the user's sides): it was 150 s old when a bottle
was gripped, and the upper part of the bottle still disappeared. The tint view was not tried. The runtime
removes
tracked hands and what they hold from the depth map, so within 6 cm of a hand (bounding sphere, across the
view ray) the mask also shows what a copy of the depth map taken without hands shows. First test on the
headset (bottle on the desk, hands hidden for 5 s, then gripped): the cap disappeared. The log had
`tracked hands [true, true]` in all 96 lines, also in the 16 with no hand in view: the runtime keeps out of
view hands active, so the old trigger (1.5 s without tracked hands) most likely never fired and nothing was
remembered. Now the copy is taken when no hand (bounding sphere + 10 cm) was inside the view of either depth
camera for 1.5 s, then every 0.5 s while none is; `playzone` still requests one. While driving the hands
never leave, so the copy is never refreshed then. `ALVR_CAM` logs each requested copy and the first of an
automatic series (`playzone: remembered ...`), and every 2 s why there is none yet (`playzone: none
remembered, a hand is near the depth view`); the `key:` line ends with `playzone none` or its age.
`playzone_view 1` with the "Tint mask" debug view: what only the copy shows is half opaque blue, the area
near the hands where it is used faintly blue. Known limits: it only adds (an object moved while held stays
where it was, a ghost), it is forgotten only by a new copy, `playzone -1`, a recenter or a new stream, a hand
in view that the runtime doesn't track is remembered as an object, and the copy is a whole snapshot (no
per-texel update).

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

- **The zone grows with the speed of the wrist only** (`key_speed` 2). With the fastest joint, moving
  fingers blew up the zone; with palm and knuckles, a hand that turns in place did. User: "it looks good".
- **Verifying the axis of a segment with a wider table** (`key_confirm` 12): 71 % of the voted segments
  confirmed instead of 65 %, fill weight 0.55 instead of 0.44, the background unchanged (measured); user:
  "the best one so far".
- **Captures** (`key_dump`): the camera image as the learner reads it with the joints of the same instant,
  the tables and votes, and from the render thread the depth in meters, the mask and the focus area of
  both eyes. The readback works on GLES. The poses of two passes 60-80 ms apart agree to about one camera
  pixel (91 % of textured patches within 1 px).
- **Hands at the wheel position are in the camera image**, see "Where it stands".
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
- **0-2 mm rim at rest, up to 8 mm when moving** (grows 45 ms x speed, `key_rim_max`; 16 until
  2026-11-23): nothing cut off at rest, fast
  motion "good enough" (observed).
- **Verified skeleton.** The solid fill comes back without the old halo, because wrong segments are not
  confirmed. But solid capsules that sit a little beside the hand still show the room, see "Where it
  stands", points 1 and 2.

## What doesn't work

- **A looser skin model** (`key_smooth` 70). Skin kept 0.70 -> 0.84, but the background around the hand
  1 -> 4 % and the scene 1 -> 11 %; user: "leaks a lot more". The offline simulation had predicted less
  leak in all 28 cases. The dark parts of the hand are rare colors, and making rare colors pass lets the
  room pass.
- **A keyed bridge from the wrist to the forearm.** A tail of the skeleton that disturbs where the arm
  continues (user), and an estimated 16-23 % of the room shown next to the hand. In 4 captures its
  direction emptied the ring of background colors.
- **Remembering the depth map for held objects.** See "Where it stands", point 5. The first trigger (no
  tracked hands) never fired. The second fires when the hands are away, not while they stay in front,
  and a carried object is not where it was remembered.
- **A fixed correction of the camera model.** See "Where it stands", point 2: the offset is not constant
  (none in one set with the same calibration) and most likely not the camera's.
- **The difference from a remembered background as the mask itself.** Too many false marks on a near
  desk without exact depth, and one pass late. As a gate on the key it looks promising offline (not
  built), see point 3.
- **Tighter solid fill by `key_inset`.** 2 / 2 and 3 / 3 against 4 / 4: fuller spread hand from the back,
  dark parts the same, background cut slightly less precisely; the user kept 4 / 4.
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
| 83a11bc7 | Key, learning, verification, sweep, no body tracking forearm | 72 | 76% without hands (2 layers), 80% median and peaks of 96% with hands (4 layers) | about 7 ms |

- The last build ran at 456 MHz and 48-50 degrees, so heat may be part of it.
- Suspects: the forearm test for all 9 depth samples of every pixel, and the bigger area of the hand pass.
  The next commit reduced both and used 1/5 resolution. Since then the body tracking forearm (capsules, arm
  removal) has been removed, so its cost is gone. Measured 2026-11-23, see the table.
- 3609efde, no hands in view (measured): 4 layers were submitted; key off took TW from 4.69 to about
  2.0 ms and load from about 85% to 62%, App time barely changed. Boost or SustainedHigh requested
  while streaming: level stayed 2, 456 MHz.
- Since 4a0a59d5 (runs on the headset: `layers 2` without hands, `layers 4` with, in the `key:` line):
  the two extra layers only while a hand is in view (plus 0.3 s);
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
- **Hands out of view are still "tracked".** `relate_hand_joints` gives a pose as long as the runtime says
  the hand is active, also when it is out of the display view (measured: 96 of 96 `key:` lines with
  `tracked hands [true, true]`, 16 of them without a hand in view). Use the view, not that flag.
- **A GL error left behind aborts the app.** wgpu doesn't call glGetError, `check_error` (staging renderer)
  does and aborts. Code that may fail on the GL side (the readback of `key_dump`) must clear the flags itself
  (`GraphicsContext::take_gl_error`).
- **Leaving the play area** pauses the app and restarts the stream. The key comes back by itself.
- **Hands at the user's sides can stay "near the depth view"** for the runtime, for minutes. Nothing may
  rely on the hands being gone.
- **"Only a plane in front of me, no VR space"**: the compositor of SteamVR had died while the streamer
  and `vrserver` kept running, and the stream showed no scene. Close SteamVR and start it again; captures
  from such minutes are worthless.
- **adb over Wi-Fi also drops whenever the headset sleeps.** `adb disconnect`, then `adb connect` once it
  is on the head again; a log capture has to be restarted. Properties survive until a reboot.
- **A watcher on the log** acted on the last matching line, which was older than the watcher. Compare the
  time of the line.
- **No background colors around the hands** (all rings empty) switches the solid fill off (`key_steady`
  1), see "Where it stands", point 6.

## Settings

`adb shell setprop <name> <value>`, most of them apply within 0.5 s.

| Property (`debug.alvr.`) | Default | Meaning |
|---|---|---|
| `color_key` | 0 | 0 off, another value (re)loads `/data/local/tmp/alvr_color_key.lut` |
| `auto_key` | 0 | 1 before starting the app: learn the colors, asks for the camera permission |
| `key_rim` / `key_rim_error` / `key_rim_max` | 0 mm / 45 ms / 8 mm | Zone at rest, growth with the speed, limit. 8 since 2026-11-23: the value on the headset for every verdict of that day (16 before; 0 clips fast hands) |
| `key_speed` | 2 | Joints whose speed grows the zone. 2: the wrist only (the arm moves; user: "it looks good"), 1: palm, wrist and knuckles (fine for waving and closing the fingers, but a hand that turns in place grows it), 0: the fastest joint (moving fingers grow it too) |
| `key_sweep` | 1 | 1: the zone grows along the motion of the hand, 0: in all directions as before. On the headset since c566b48e, never compared with 0 |
| `key_sweep_max` / `key_sweep_iso` / `key_sweep_bias` | 40 mm / 25 % / 0 % | Limit of the growth along the motion (each side, before the bias), share of the growth that stays in all directions (limited by `key_rim_max`), -100..100: more ahead (positive) or behind |
| `key_inset` / `key_falloff` | 4 mm / 4 mm | Where the solid fill starts and how it fades out. 2 / 2 and 3 / 3 compared on the headset: a spread hand seen from the back is fuller, dark parts (palm, fist) are the same, the background is cut slightly less precisely; user: 4 / 4 is the best |
| `key_error` | 10 ms | How much the fill shrinks with the speed |
| `key_solid` | 6 | Segments that are always solid: 0 none, 6 wrist, 9 palm, 28 hand |
| `key_tight` / `key_bias` / `key_unknown` | 30 / 150 / 10 % | Strictness, preference for skin, unknown colors |
| `key_smooth` | 100 % | Smoothing of the skin colors, of a LUT texel (70..100). `key_tight` is scaled by its cube, so 70 is about a third of the floor: rarer skin colors are kept, not more around the common ones, but the tails of a widely spread skin (varied light) reach further (computed: 36 % more cells at 70). 100: as before. On the headset 70 leaks: skin kept 0.70 -> 0.84, but the background around the hand 1 -> 4 % (worst tenth 3 -> 16 %) and the scene 1 -> 11 %; user: "leaks a lot more". Values in between not tested |
| `key_confirm` | 12 % | `key_tight` of a second table that is never uploaded: the points on the axis of a segment are verified with it (the points beside it with the key), and the reliability comes from the share of the background around the hands that it keeps. Capped at `key_tight`, so it keeps everything the key keeps. 0: off. On the headset with 12: of the segments that are voted on 71 % are confirmed (65 % without), verified weight 0.55 (0.44), background unchanged; user: "the best one so far", in some cases the hand is still not full enough |
| `key_steady` | 1 | 1: a hand is verified while its palm (palm, wrist, knuckles) is tracked and slower than 0.5 m/s, each segment while its own two joints are too; its background is then sampled for the reliability only, and a hand with a fast finger is judged by its own background (fewer than 50 colors: no fill). Colors are still learned only from whole hands. 0: the whole hand or nothing, as before. Runs on the headset, not compared with 0 |
| `key_delay` / `key_scale` | 0 ms / 100 % | Hand pose offset (negative predicts), thickness |
| `playzone` / `playzone_auto` | 0 / 1 | New positive value: remember at the next depth image, -1 forgets it; the value at the start of the stream is not a request. Automatic after 1.5 s without a hand near the view of the depth cameras: fires at the start of a stream and when the hands are away; the runtime can keep hands near the view for minutes |
| `playzone_view` | 0 | 1: with the "Tint mask" debug view what only the remembered playzone shows is half opaque blue, where it is used (near the hands) faintly blue. Not tested |
| `key_layers` | 4 | 4: extra layers only with hands in view, 2: never, 5: always, 30 / 31: always three (no upper game layer / no keyed passthrough). Not tested |
| `key_crop` | 1 | Lower game layer only around the hands. 0: full, 2: y origin at the top (if 1 is upside down). Not tested |
| `key_hands` | 1 | 0: no hands in the GPU passes. Not tested |
| `arm_shift` | 1 | 1: the depth around each arm is shifted by the motion of the wrist (with the key only up to the wrist, the hands are removed from the depth map; plain depth for a wrist that moved more than 25 cm or has no valid position), 0: plain depth. Also without the key. 1 runs on the headset, 0 not tested |
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
| `key_dump` / `key_dump_burst` | 0 / 1 | New value `n` other than 0: write the next `key_dump_burst` (1..30) learning passes to the same folder, `alvr_dump_<n>_<00..>`, also without hands. Each line `dump <n>/<index>: <path>` on `ALVR_CAM`. The render thread also writes the depth of one frame, `alvr_dump_<n>_depth*` (below), also without `auto_key`; `depth dump <n>: ...` on `ALVR_CAM`. Works on the headset; 25 requests 3 s apart cost no visible frames |
| `key_wrist_len` | 0 mm | Keyed bridge from the wrist towards the forearm, 0: off, up to 300. Was 80 until 2026-11-23: the user sees it as a tail of the skeleton that "messes up where the arm continues"; the skeleton is for the hand up to the wrist only, the arm is not to be extended from it. The direction and the solid fill need `auto_key`, without it: opposite to the fingers, never solid |
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

**`key_dump` depth files** (render thread, works on the headset), once per new `n`, from the next frame
with a depth image. If nothing arrives within 1 s, or a copy or the file fails, one `depth dump <n>: <error>`
line and nothing else (`depth dump <n>: replaced by <m>` if a new value came before a depth frame). A dump may cost a frame or two: the first one compiles its shader, and the frame
whose readback is ready copies about 4 MB of mapped memory on the render thread (decoded and written on
another thread):
- `alvr_dump_<n>_depth_left.f32` / `_right`: the environment depth as the mask pass reads it, little-endian
  f32, meters from the plane of the depth camera (along its -z), 0: no data. `depth.width` x `depth.height`,
  rows in the order of the depth texture: row 0 is the bottom of the depth view (`flip_y` false). Converted on
  the CPU from the exact 16 bit values with `near` / `far` (`far` null: infinite).
- `alvr_dump_<n>_playzone_left.f32` / `_right`: the remembered depth, same layout (stored as 16 bit floats,
  0: no data), only if there is one. Its camera is `playzone_camera`, not the current one.
- `alvr_dump_<n>_mask_left.rgba` / `_right`: the depth mask, `mask.width` x `mask.height` x 4 bytes, row 0 is
  the top of the view. r: region coverage, g: proximity (1 at the eye, 0 at 3 m), b: depth data (0/1), a: the
  part of r that comes from the playzone.
- `alvr_dump_<n>_focus_left.rgba` / `_right`: the focus area, same layout (`focus` size). r: hand with its
  rim, g: solid fill, b: near a hand (where the playzone is used), a: narrowed rim.
- `alvr_dump_<n>_depth.json`: `n`; `display_ns` (predicted display time of the frame), `depth_capture_ns`
  (null: not reported), `now_ns` (when it was rendered), all XrTime, which is CLOCK_MONOTONIC on Quest like
  `image_ns` of the camera dumps; `depth` (`width`, `height`, `near`, `far`, `flip_y`), `mask`, `focus`
  (sizes); `playzone` (`age_s`, null: none); `region` (`mode` distance: `max_distance`, volume: `center`,
  `yaw_deg`, `size`; `feathering`; the region the mask used: a box placed from a marker has `source`
  "marker" and its `volume_to_world`, column major); `debug_view`; `color_key`; `eyes` (left, right): `eye_position`, `view`
  (`pose` and `tangents` left, right, down, up of the streamed view), `uv_to_world_dir` (column major 4x4:
  `dir = M * (u, v, 1, 0)`, uv from the top left), `depth_camera` and `playzone_camera` (`pose`, `tangents`,
  `world_to_view`, `view_to_world`), `files`; `hands` (`bounds` xyz r, `capsules` like the camera dumps,
  `rim`, null: none); `arms` (`wrist`, `direction`, `displacement`, null: plain depth). Stage space, meters.

Depth texel (x, y) to the world: `tx = l + (x + 0.5) / width * (r - l)`, `ty = d + (y + 0.5) / height * (u - d)`
with the `tangents` of `depth_camera` (`y` from the bottom), point `view_to_world * (tx * z, ty * z, -z, 1)`
with `z` the value in meters. To a mask texel of an eye: the direction from `eye_position`, `uv` from
`uv_to_world_dir` (the tangents of `view` in the orientation of its pose); to the camera image of the camera
dumps: `pose` and `fx`, `fy`, `cx`, `cy` there, at its `image_ns`.

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

**QR code markers and the box placed from them** (not tested on the headset: compiles and passes the unit
tests only; nothing in this section ran on the headset yet). Step one of way B: does the runtime give this
app QR code poses (`XR_EXT_spatial_entity` and `XR_EXT_spatial_marker_tracking`), how good are they, and
the box region placed from one. The manifest now declares `com.oculus.permission.USE_ANCHOR_API` and
`horizonos.permission.USE_ANCHOR_API`. The extensions are enabled at the start of the app if
`debug.alvr.marker` is not 0 (unset counts as on here) and the runtime lists `XR_EXT_future`,
`XR_EXT_spatial_entity` and `XR_EXT_spatial_marker_tracking` (all three or none; by name, so a missing
function turns the feature off instead of ending the app; if the runtime refuses them when the instance is
created, the app starts again without them). Without them, or with `marker 0` at the start, nothing
changes. Enabled extensions cost nothing by themselves: the tracker (QR detection and the snapshot calls)
runs only while `marker` is 1, or while it is unset and `marker_box` is 1.
`XR_EXT_spatial_anchor` and `XR_EXT_spatial_persistence` are only reported, not enabled.
Code: `extra_extensions/spatial_marker_ext.rs` (OpenXR), `marker_region.rs` (the box), geometry with tests
in `alvr_graphics` `marker_box.rs`.

Scope (the user, 2026-11-23, evening): no motion rigs, a code never moves once placed. The codes are
mounted on the rig (flat or standing) or shown on the monitor facing the driver (a standing code). A code
is used as it is and measured precisely; that measurement is the baseline, and its size may be changed
after the first measurement.

| Property (`debug.alvr.`) | Default | Meaning |
|---|---|---|
| `marker` | unset | 1: look for QR codes while the session runs. Unset: only while `marker_box` is 1 (so a normal race pays nothing). 0: the tracker is stopped and a placed box is forgotten. The extensions are chosen at the start of the app: 0 at the start needs a restart for 1 to work. To restart the tracker (to apply `marker_static` / `marker_size`, or after a line starting `marker: off, ...`): `setprop debug.alvr.marker 0`, wait a second, then `1` |
| `marker_discovery` | 2 s | A discovery snapshot (finds new codes) this often without the runtime's discovery-recommended event; also right after the context is ready, after an event and after a stage change, but then at most every 250 ms |
| `marker_rate` | 10 Hz | Update snapshots (fresh poses of the known codes, stage space, at the display time of the frame). 0: none |
| `marker_static` | 0 | 1: tell the runtime the codes don't move, only if it lists that feature (read when the tracker starts, see `marker`) |
| `marker_size` | unset (0) | Side length in mm, decimals allowed (e.g. `95.6`), 0 to 2000: black edge to black edge, without the white margin. The runtime takes EVERY code as this size, so set it only while codes of one size are in view; with it set the size in the `new ...` line is this value, not a measurement. Only if the runtime lists that feature. Read when the tracker starts (see `marker`): check the `creating the spatial context (...)` line. A value that is not a number is logged and ignored |
| `marker_box` | 0 | 1: while a code is known, the depth mask uses a box placed from it instead of the region of the settings (also instead of "Distance") |
| `marker_id` | unset | Use the code with this payload (text, case sensitive: `RIG1`). A change is logged and the box is forgotten at once. Unset: the box stays with codes that carry the payload of the code used first, until `marker_box 0`, `marker 0`, or `marker_id` changes |
| `marker_window` | 1000 ms | Length of the average of the pose of the code, 100 to 60000. A change is logged and applies at once (a shorter window drops older poses at once) |
| `marker_move_ms` | 1000 ms | Poses away from the average must agree for this long (and be at least 3) to be a move, 0 to 60000. Longer: fewer false moves from a pose error that lasts a moment (a standing code's heading), a real move is followed later. A change is logged and applies at once |
| `marker_box_x` / `_y` / `_z` | 0 / 150 / 0 mm | Center of the box from the code: to the right of the heading, up, along the heading. A flat code with its top away from you: +x is your right, +z away from you. A standing code facing you: +x is your LEFT, +z comes TOWARDS you (moving a code from flat to standing needs new values; the defaults fit a flat code, for a standing one start from z 400, y -150, see Testing) |
| `marker_box_w` / `_h` / `_d` | 1200 / 900 / 1000 mm | Width (across the heading), height, depth (along the heading) |
| `marker_box_yaw` | 0 deg | Added to the heading, counter-clockwise from above; turns the box about the code |

The box frame is level (stage +Y up); only the heading comes from the code, never its tilt (a tilt error of
a few degrees would move the far end of the box by centimeters). Heading: a code lying flat (normal within
45 degrees of vertical) points to its top (the reading direction: the side with the two upper finder
squares); a standing code points out of its printed face. Do not mount a code with its normal between
about 35 and 55 degrees from vertical (read the angle in the `new ...` line): at 45 degrees the heading
switches between the top of the code and its normal, which are opposite for a code whose top points up
the slope, so the box turns by 180 degrees and x / z are mirrored. (There is no hysteresis; it is a
possible change after the first measurements.)

The pose is the average of the poses of the last `marker_window` (1 s) while the code is still. A pose
that is repeated is counted once. A single pose away from the average (more than 2 cm or 3 degrees) is
left out and counted as an outlier; 3 or more poses in a row away from the average that agree with each
other and span `marker_move_ms` (1 s) are a move, and the average starts again from them; a pose away from
the average that the runtime reports unchanged without a gap for 2 s is also taken as a move (a gap: no
pose for more than 0.5 s, e.g. the code was out of view; with `marker_rate` below 2 Hz or poses from the
discoveries only this rule never applies). The pose is kept while the code is not seen, and also while
there is no tracker (it failed, or the session stopped and started again); only `marker_box 0`,
`marker 0` or a new `marker_id` forget it. A change of the
STAGE space (`ReferenceSpaceChangePending` for STAGE; events for LOCAL or VIEW are ignored here) keeps the
box up to the change time of the event and drops it from then until the code is seen again; the average
starts again with the first pose in the new space. Quest announced STAGE changes three times at a
recenter without moving the frame (Evening session), so a recenter may still show the region of the
settings for about one update (~100 ms) with the code in view, and until the code is seen again otherwise.
The box applies with Passthrough = Depth only, and live; the settings stay as they are.

With the color key on, the hand zones do not use the region (`stream.wgsl`: `max(mask, hand.g)` and
`focus`): keyed hands show everywhere, while forearms, wheel and body show only inside the box. A hand
without its forearm means the box ends there, not a wrong heading. The default box is centred on the
code: 0.5 m either way along the heading, 0.6 m to each side, 0.30 m below to 0.60 m above. For a code
standing on a monitor that is 0.5 m behind the screen and only 0.5 m towards the driver, with the monitor
itself inside. Example for a monitor code: `marker_box_z 600`, `marker_box_d 1100` (5 cm in front of the
screen to 1.15 m towards the driver), `marker_box_y -100`, `marker_box_h 1400` (0.8 m below to 0.6 m above
the code, lap included). Not tried.
The debug view "Tint mask" paints the box green only without the color key (`color_key 0`); with the key
loaded the box is plain passthrough.

**The printed codes** (2026-11-23). Three codes with the payloads `RIG1`, `RIG2` and `RIG3`, black edge on
the paper 144.2, 95.6 and 67.4 mm. The printer scaled the page to 96 %: the 100 mm check bar measured
96 mm, so these are the measured sizes, not the designed ones. `tools/make_qr.py` makes the sheets
(outside the repository, on the development PC). The size to give to `marker_size` is the black edge
measured with a ruler, not the designed one.

**A code on the monitor.** `tools/show_qr.py [payload] [--monitor N] [--px N | --mm X] [--pitch X]
[--white N]` (outside the repository, on the development PC) shows a code on a monitor at a whole number
of pixels per module; the default payload is `SCR1`. Keys: `+` / `-` size, arrows move, `m` next monitor,
`b` / `B` dimmer / brighter white, `c` shows a bar of a round number of pixels to measure the monitor's mm
per pixel with a ruler, Esc ends. It prints the size by the monitor's own data, which is only a first
guess: measure the black edge with a ruler, that number is the marker size. Not tested on the headset.

Log lines (`ALVR_CAM`, all start with `marker`):
- At the start: `marker: the runtime lists XR_EXT_future yes, ... XR_EXT_spatial_persistence no; enabled: ...`.
  Then, rarely: `marker: off, the instance can't be created with the extensions: <error>` (the app runs
  without them), `marker: off, functions not found: <error>`.
- `marker: stopped (debug.alvr.marker 0)` or `marker: stopped (marker_box 0 and debug.alvr.marker unset)`
  when the tracker is dropped; `marker: the extensions are only enabled at the start of the app, restart
  it` after `marker 1` when the app started with `marker 0`.
- Once per session: `capabilities [...]; QR code: components [...], features [...]` (does the runtime
  list the static and the fixed size features?), `creating the spatial context (static markers: <bool>,
  fixed size: <mm> mm | no)`, `spatial context ready after <ms>`, `first discovery-recommended event after
  <s>`. `no view space, lines without the head: ...` if the head reference space can't be made.
- `marker_size "<text>" is not a number, ignored`.
- `discovery: <n> markers (<n> tracking), <ms> after the request, ...` when the number changes.
- A code is described everywhere as `"<payload>" #<id> <state> at (x, y, z) m, normal <deg> deg from
  vertical (lying|standing), heading <deg> deg, <w> x <h> mm` (stage space; position to 0.1 mm, heading to
  0.01 deg, size as the runtime reports it). The heading is the one the box would use (degrees about
  vertical, counter-clockwise from above, 0 = stage -Z), printed in -90..270 so that a code facing the
  driver (about 180) doesn't jump between +180 and -180; the same range in the `marker box:` and
  `ReferenceSpaceChangePending` lines. Lines using it: `new <description><head>`, `<old state> -> <new state>: <description><head>`
  (also `tracking -> paused`), `lost #<id>`, `payload changed ...` (seen by discoveries; update snapshots
  read a payload only while it is unknown). A pose also counts as changed when only its orientation
  changes.
- The head part, added when a head sample and a pose exist: `, head <m> m away, <deg> deg off the normal,
  head up to <m/s> m/s and <deg/s> deg/s in 2 s` (the head pose in the view reference space, sampled at
  20 Hz; with fewer than 2 head samples `head up to - in 2 s`). It tells at which distance and angle the
  code was seen, and how fast the head moved: a bad pose with a fast head is not a bad code.
- `the payload buffer id of #<id> changed from <a> to <b> (logged once) ...`: the runtime gives new buffer
  ids per snapshot (the payload cache is cleared at 64 entries).
- `unknown tracking state <n> of #<id> (logged once, taken as paused)`; `invalid pose from the runtime for
  #<id> (logged once, dropped): center (...), orientation (...), extents <w> x <h>` (a non-finite pose).
- Every 2 s while codes are known: `<n> known; discoveries <n> (...); update snapshots <rate>/s, <ms>
  each` (or `none`, or `failed (poses from the discoveries)`), and per code `  <description><head>, pose
  changes every <ms> (<n> > 0.1 mm / 0.01 deg, <n> bit-exact in 2 s), scatter x/y/z <mm> over <k>
  distinct poses in 10 s, heading scatter <deg> deg (range <deg>), seen <s> ago`. "pose changes" counts
  the changes of more than 0.1 mm or 0.01 deg, and how often they come is how often the runtime really
  updates the pose (update snapshots repeat the last one; the bit-exact count includes smaller changes,
  which show whether the runtime changes the pose by tiny amounts). The scatter is the standard
  deviation of the position over the distinct poses of the last 10 s (`scatter x/y/z - (no distinct
  pose) in 10 s, heading scatter -` with none); the heading scatter is that of the heading, with its
  peak-to-peak range, about the circular mean. A code without a pose yet: `  <description>, no pose yet`.
  Every 10 s while none is known: `0 known; ...`.
- `<OpenXR call> took <ms> ms (...)` when a call takes more than 2 ms (at most every 2 s per call).
- `<OpenXR call>: <result> (<code>)` once per call and result code. `off, ...` when it gives up: restart
  with `setprop debug.alvr.marker 0`, a second, `1`. Update snapshots that fail no longer turn the tracker
  off: one line `update snapshots can't be created: none from now on, poses come from the discoveries
  only (debug.alvr.marker 0, then 1 tries again)` and the poses then come from the discoveries only.
- `marker box: marker_id is now "<text>" | unset`, `marker box: averaging window <s> s`, `marker box: a
  move must hold <s> s` (when `marker_window` / `marker_move_ms` are set to other than the defaults).
- `marker box: {keeping the last pose, }waiting for <the marker "<id>" | a marker with a pose> (known:
  ...)` every 10 s while no code fits (also while a pose is kept).
- `marker box: <n> poses away from the average left out so far`, at most every 2 s when the count grew.
- `marker box: placed at (x, y, z) m, heading <deg> deg, size ..., marker #<id> at (...), <n> samples
  over <s> s (window <s> s), <n> outliers left out`, again as `moved by <cm> cm, at ...` when a corner of
  the box moved by more than 1 cm. The samples are distinct poses only; `over <s> s` is the time from the
  oldest to the newest of them (it can be longer than the window when the runtime stops refreshing the
  pose, and is 0 with one sample).
- `marker box: used by the depth mask` or `marker box: placed but not used (no stream, or Passthrough is
  not Depth)` when that changes. It says "used" also when the environment depth failed to start (a missing
  permission); that failure is logged on its own.
- `marker box: off (marker_box 0), the region of the settings applies` and `marker box: off
  (debug.alvr.marker 0), the region of the settings applies`, when a box that was placed is dropped for
  that reason. `marker box: no tracker, keeping the last pose` once when the tracker goes (it failed, or
  the session stopped) while a box is placed: the box stays.
- `ReferenceSpaceChangePending type: <type>, change time <ns> ns, pose valid: <bool>, pose in previous
  space (x, y, z) m, yaw <deg> deg` (now in `ALVR_CAM`, for every reference space type; only STAGE ones
  give `marker: stage space changed` and `marker box: stage space changed, waiting for the marker`).

Testing (`adb logcat -s ALVR_CAM` running):
- [ ] The printed codes `RIG1`, `RIG2`, `RIG3` (black edge 144.2, 95.6, 67.4 mm; the payload is case
      sensitive, `marker_id RIG1`), plain black on white, no logo. Lay one flat on the desk in good light.
- [ ] `setprop debug.alvr.marker 1` (the measuring steps run without the box; unset, the tracker runs only
      with `marker_box 1`). Start the app: the first line says which extensions the runtime lists. If `XR_EXT_spatial_entity`
      is `no`, look for `missing uses-permission` in the full logcat.
- [ ] `capabilities ...` must list `MARKER_TRACKING_QR_CODE`; a `ERROR_PERMISSION_INSUFFICIENT` line
      means the spatial data permission (asked at the first depth passthrough) is missing: it retries
      every 10 s. Note whether the features list the static and the fixed size feature.
- [ ] After a line starting `marker: off, ...`: `setprop debug.alvr.marker 0`, wait a second,
      `setprop debug.alvr.marker 1` restarts the tracker.
- [ ] Accuracy of the size first, with `marker_size` unset: look at each code from 40-80 cm:
      `new "RIG1" ...`. Compare the size in the line with the measured one (144.2, 95.6, 67.4 mm) for
      each of the three codes; read the normal angle (about 0 on a level desk), the pose changes interval
      and the scatter, once with the head still and once while moving it (the head part says how fast).
      Then from further away and at a flat angle (the head part gives the distance and the angle).
- [ ] Then one code at a time: `setprop debug.alvr.marker_size 144.2`, restart the tracker (`marker 0`,
      a second, `1`), check the `creating the spatial context (... fixed size: 144.2 mm)` line (if it
      says `no`, the runtime does not list the feature: nothing changes), and compare the scatter and
      the position with the run without it. Only one size in view at a time: the size applies to every
      code. Change the size (the user may change it after the first measurement) the same way.
- [ ] Turn the head away for 10 s and back: `paused` / `tracking` lines, and whether the position comes
      back to the same millimeters.
- [ ] Passthrough = Depth, then `setprop debug.alvr.marker_box 1`: `marker box: placed ...`, then
      `marker box: used by the depth mask`. The depth mask shows the box: the real desk and what stands
      on it inside the box, the game outside. The debug view "Tint mask" marks it green only without the
      color key (`setprop debug.alvr.color_key 0`, and `auto_key 0`, live; hands are then limited by the
      box too); with the key loaded the box is plain passthrough, not green, and the keyed hands are
      never limited by it. Move the code by 5 cm: the box follows after about 1 s (`marker_move_ms`;
      at least 3 poses at the new place), then `moved by`. With the code on the monitor, switch the
      monitor to the game: the box stays. Count the `moved by` lines while racing with the code fixed:
      each one is a false move; if there are any, try `marker_move_ms 3000`.
- [ ] Origin: with the code flat, set `marker_box_y 0`, `marker_box_h 100`, `marker_box_w 150`,
      `marker_box_d 150`. The tinted patch must sit centred on the code, not on a corner. A shift of half
      the code size means the runtime's origin is not the centre; note the direction.
- [ ] Heading, flat code (default sizes, or `marker_box_w 400` so the long axis is obvious):
      `marker_box_w 400`: the box must get narrow ACROSS the reading direction of the code (left-right as
      you read it); if it narrows along it, the heading is off by 90 degrees. `marker_box_z 300`: the box
      must move 30 cm towards the TOP edge of the code (the side with two finder squares). `marker_box_x
      300`: it must move to the right of the code as read. Turn the paper by 90 degrees: the `heading` in
      the `marker box:` line changes by 90 degrees and the box turns with it. If a direction is wrong,
      note which one and try `marker_box_yaw 180` / `90`. Reset x / z to 0 afterwards.
- [ ] Change `marker_box_y` / `_w` live. Try `marker_window` 300 and 3000: the box is steadier with the
      longer window and follows a move later.
- [ ] Standing code (paper upright, or the code shown on the monitor facing you, see "A code on the
      monitor"): `new ...` must say `standing` and a normal of about 90 deg (a monitor leaning back:
      60-90). `marker_box_z 300` must bring the box TOWARDS you, out of the screen. `marker_box_x 300`
      must move it to your LEFT. The default box is made for a flat code: around a standing code it
      reaches 0.5 m behind the screen and only 0.5 m in front, from 0.30 m below to 0.60 m above the
      code. Start from `marker_box_z 400` (0.1 m behind the screen to 0.9 m in front with `_d` 1000;
      make it your distance to the screen minus 300 or so) and `marker_box_y -150` (0.60 m below to 0.30
      m above the code with `_h` 900, for the wheel and the hands). Compare the `marker box: placed at
      ...` centre with the `marker #.. at ...` position in the same line to check the offset.
- [ ] Recenter: note the `ReferenceSpaceChangePending type:` lines (only STAGE ones give `marker: stage
      space changed`); after them the box is gone (`marker box: stage space changed, waiting for the
      marker`) until the code is seen again, then back at the same place. With the code out of view
      during a recenter the region of the settings stays: if that happens in practice, keeping the box
      through STAGE changes is the next step.

## Open problems, in order

The order of the user on 2026-11-23; how to go on with each is in "Where it stands and how to continue".

1. The forearm and held objects are cut out near the hand. Nothing works yet.
2. Jagged edges of what the depth mask shows (sleeve, bottle, later the rig).
3. The room next to the hand: thin lines and patches. Roughly 40-50 % of it from the solid fill.
4. The play area around anchors on the rig (QR codes) instead of around the head: the box region exists,
   the calibration doesn't.
5. A real drive with a wheel. Hands on a wheel were only approximated with a bottle and a monitor, and
   the playzone map was never tried with a wheel or a shifter.
6. GPU budget: 80 % with hands in view, peaks of 95-97 %. The extra layers of the key are a big part
   (key off: about 85 -> 62 % in one measurement); anything new on the GPU has to be cheap.
7. Never tested on the headset: the lighting match, the leak map (`key_leak_map`), the narrower rim
   (`key_leak`). The box region ran once and is not judged.
8. A way to compare two settings of the hand side by side (one per hand): asked for by the user.

## Privacy

Camera images are only used in memory to learn colors. Nothing is stored or sent. Only on request
(`key_lut_dump`, `key_dump`) the learned tables, camera images and depth maps of one moment are written to
the files folder of the app on the headset. Average colors and
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
- [Meta: QR code detection (MRUK)](https://developers.meta.com/horizon/documentation/unity/unity-mr-utility-kit-qrcode-detection)
- [Meta: spatial data permission (native)](https://developers.meta.com/horizon/documentation/native/native-spatial-data-perm/)
- [Meta: spatial anchors API reference (native)](https://developers.meta.com/horizon/documentation/native/android/openxr-spatial-anchors-api-ref)
- [Meta: experimental features (native)](https://developers.meta.com/horizon/documentation/native/android/mobile-experimental-features/)
- [Android XR: XR_EXT_spatial_marker_tracking](https://developer.android.com/develop/xr/openxr/extensions/XR_EXT_spatial_marker_tracking)
- [Android XR: XR_EXT_spatial_entity](https://developer.android.com/develop/xr/openxr/extensions/XR_EXT_spatial_entity)
- [Khronos: XrSpatialMarkerDataEXT](https://registry.khronos.org/OpenXR/specs/1.1/man/html/XrSpatialMarkerDataEXT.html)
- [Khronos: OpenXR SDK changelog](https://github.com/KhronosGroup/OpenXR-SDK-Source/blob/main/CHANGELOG.SDK.md)
- [Meta sample repository: QR on v205, answer of a Meta engineer](https://github.com/oculus-samples/Unity-MRUtilityKitSample/issues/15)
- [Godot forum: QR tracking on Quest 3 (v79 no, v83 yes)](https://forum.godotengine.org/t/qrcode-tracking-via-openxr-spatial-entities-extension-on-meta-quest-3/123395)
- [Godot issue: permission before the context](https://github.com/godotengine/godot/issues/123467)
- [Godot: spatial entities, update snapshots](https://docs.godotengine.org/en/stable/tutorials/xr/openxr_spatial_entities.html)
- [Unity forum: marker tracking platforms (QR only on Horizon OS)](https://discussions.unity.com/t/when-does-the-ar-marker-tracking-get-more-supported-plattforms/1703253)
- [UploadVR: QR codes in MRUK v78](https://uploadvr.com/quest-3-mruk-update-decode-track-qr-codes-unity)
- [Meta forum: QR position jitter on a still code](https://communityforums.atmeta.com/questions-and-discussions-26/quest-3-qr-code-tracking-severe-position-jitter-182318)
- [Meta forum: QR code detection discussion](https://communityforums.atmeta.com/questions-and-discussions-26/mr-qrcode-detection-173577)
- [Display-camera channels (moire, rolling shutter)](https://arxiv.org/pdf/1501.02528)
- [Microsoft: QR code tracking sizes (other platform)](https://learn.microsoft.com/en-us/windows/mixed-reality/qr-code-tracking)
