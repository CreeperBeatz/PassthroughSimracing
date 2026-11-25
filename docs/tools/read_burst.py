# Read a burst of consecutive frames (`setprop debug.alvr.burst <n>`, see the knowledge base) into numpy
# arrays and print a summary: counts, the interval between depth frames, the age of the depth at display.
# Usage: read_burst.py <burst folder (alvr_burst_<n>) or its alvr_burst_<n>.json> [--no-images]
# As a module: from read_burst import load_burst; burst = load_burst(path); burst["depth"]["meters"] ...
#
# Arrays (F depth frames, K display frames, C camera images; missing values are NaN, -1 or False):
# depth:   meters (F, 2, H, W) float32 (rows as in the depth texture, row 0 = bottom of the depth view when
#          flip_y is false; 0: no data; NaN: frame not read back), read (F,) bool, capture_ns, display_ns,
#          now_ns (F,) int64 (-1: not reported; display_ns and now_ns of frame 0 are of the first display
#          frame: it may have come before the start), near, far (F,) (far inf: infinite), swapchain_index (F,),
#          position (F, 2, 3), orientation (F, 2, 4) xyzw, tangents (F, 2, 4) left right down up,
#          view_to_world / world_to_view (F, 2, 4, 4) (row major here: M @ [x, y, z, 1])
# display: display_ns, now_ns, hand_ns (K,), depth (K,) index into depth (-1: none or past the depth
#          limit), region (K,) index,
#          eye_positions (K, 2, 3), view_position (K, 2, 3), view_orientation (K, 2, 4), view_tangents
#          (K, 2, 4), stream_orientation (K, 2, 4), stream_tangents (K, 2, 4), joints (K, 2, 26, 4) xyz r,
#          tracked (K, 2, 26), velocities (K, 2, 26, 3), capsules (K, 2, 28, 8) start xyz r end xyz r,
#          rim (K, 2), arm_wrist / arm_direction / arm_displacement (K, 2, 3), and of step 1 of the arm plan
#          (burst version 2): arm_mode (K, 2) (0: no arm, plain depth: arm_shift 0, no hand tracker, a failed
#          lookup or a wrist step over 25 cm; 1: display-time wrist, 2: past-time target),
#          arm_weight_len (K, 2) (m, 0: smoothstep of mode 1), arm_target_ns / arm_history_ns (K, 2) (-1: no
#          arm), arm_wrist_history (K, 2, 3), arm_extrapolated_mm (K, 2). Version 1 bursts only had mode 1:
#          their arms read as mode 1, weight 0, target and history = hand_ns (the time mode 1 used), the
#          history wrist = arm_wrist, extrapolated 0. Steps 1 and 2 of the mask design (burst version 3):
#          edge_cut (K,) (`debug.alvr.edge_cut` in percent as the mask used it, 20: today's; older bursts
#          read as 20), gap_length_mm (K, 2) (wrist gap, NaN: off or an older burst), gap_axis (K, 2, 3)
#          (unit vector from the middle knuckle to the wrist, up the arm), gap_wrist (K, 2, 3) (where the
#          gap starts: the arm's wrist when the arm has a motion), gap_set_mm (K, 2) (the gap each arm was
#          set to, 0: off or an older burst; set while gap_length_mm is NaN: the joints were not located).
#          Step 1b of the arm plan, the arm's own sample history (burst version 4; older bursts: NaN / -1 /
#          False): arm_capture_wrist (K, 2, 3) (the wrist the history gave for the depth's capture time, the
#          arm's place in the depth; NaN: not mode 2, a buffer miss, or plain depth for a displacement over
#          25 cm), arm_estimator (K, 2) (-1: not mode 2, or an older burst), arm_reset (K, 2) (the history was emptied in this frame: the hand was lost). In mode 2
#          arm_history_ns / arm_wrist_history are the newest sample and arm_target_ns the time the placed
#          wrist stands for. The room memory (`debug.alvr.mem_roll`, U3; burst version 6, older bursts and
#          frames with it off: mem_on False, NaN, -1): mem_on (K,) bool, mem_position (K, 3), mem_yaw_deg,
#          mem_pitch_deg (K,) (its camera, 120 x 120 degrees, fixed in stage space), mem_world_to_view (K, 4,
#          4) (row major), mem_size (K,), mem_anchors (K, 2) (anchored cleanly / with hands since it was
#          created), mem_updates (K, 2) per eye, mem_frozen / mem_no_live / mem_empty / mem_moving (K,) (shares
#          of the left eye's texels from its last stats, NaN: none yet), mem_stats_age_ms (K,),
#          mem_oldest_frozen_s (K,), mem_hands (K, 2) (where the capsules at the depth's capture time came
#          from: 0 none, 1 the arm's history, 2 the last known ones grown 3 cm; -1 off), mem_margin_mm (K,).
#          The state is the renderer's after the frame. The depth cloud (`debug.alvr.cloud`, burst version 7;
#          older bursts and frames with it off or not running: cloud_on False, -1, NaN): cloud (K,) the record
#          as a dict or None, cloud_on (K,) bool, cloud_shown (K,) bool (False in today's phase of cloud_ab),
#          cloud_mode (K,), cloud_field_capture_ns / cloud_prev_capture_ns (K, 2) per eye (-1: none),
#          cloud_dt_ms, cloud_head_deg_s, cloud_a_ms, cloud_reach_ms (K, 2), cloud_reset (K, 2) (-1 off, 0
#          measured, 1 start, 2 dt, 3 head, 4 region), cloud_stale (K, 2) bool (the field was older than
#          300 ms: no zone; False where the record has no `stale`)
# samples: the arm's own hand samples (burst version 4; empty for older bursts), a list per hand (0 left,
#          1 right) of dicts of arrays in time order: time_ns (when the sample shows the hand), seen_ns (when
#          the app got it first), wrist (N, 3), capture_time (N,) bool (time_ns is the runtime's capture
#          time, else seen_ns - arm_sample_age_ms), display (N,) index of the display frame that got it
#          (0 also for the samples the history held when the burst started: the first record has them all),
#          segment (N,) count of arm_reset up to that frame (no interval across a reset)
# camera:  image_ns, now_ns (C,), intrinsics (C, 4) fx fy cx cy, position (C, 3), orientation (C, 4),
#          head_position, head_orientation, joints (C, 2, 26, 4), rgb (C, H, W, 3) uint8
# regions: list of the region JSON objects; index: the whole index JSON
# key_lut: the learned colour table of the key last uploaded before the start (version 2), None: none (or a
#          version 1 burst): version, published_ns, rgba (32, 32, 32, 4) uint8 indexed [b][g][r]
# clocks:  {"start": ..., "end": ...} (burst version 5; None for older bursts): the headset's clocks read back
#          to back (CLOCK_MONOTONIC, CLOCK_REALTIME, XrTime, CLOCK_MONOTONIC again) when the burst started
#          (its xr_ns is start_ns) and ended: dicts of realtime_ns (Unix time, ns), monotonic_ns, xr_ns (None:
#          not read: the stream stopped before the end), read_ns (time between the two monotonic reads: the
#          pair is good to that). Every *_ns of the records is XrTime. xr_to_realtime(burst, t_ns) puts XrTime
#          on the headset's wall clock (Unix ns, int64); for older bursts use the epoch time of the
#          "burst: <n> started" logcat line (about +-10 ms [assumed, not measured]). realtime_ns (~1.8e18)
#          is above 2^53: parse the index with Python's json (ints exact), not a float-based JSON reader
# load_memory_dump(dump_json_path): the room memory of a depth dump (`debug.alvr.key_dump`, or the dump at the
#          start of a burst) taken with `mem_roll 1`, None without: texels (2, S, S, 4) float32 per eye (r:
#          distance from the camera plane, 0 nothing; g: moving 0..1; b: frozen 0/1; a: seconds since the
#          last live write), rows in the order of the texture (row 0 = the down side of the camera unless
#          flip_y), status (the JSON), world_to_view / view_to_world (4, 4) row major, tangents (left, right,
#          down, up)
import datetime
import json
import os
import sys

import numpy as np


def _index_path(path):
    if os.path.isdir(path):
        names = [f for f in os.listdir(path) if f.startswith("alvr_burst_") and f.endswith(".json") and f.count("_") == 2]
        if len(names) != 1:
            raise FileNotFoundError("no single alvr_burst_<n>.json in %s: %s" % (path, names))
        return os.path.join(path, names[0])
    return path


def _optional_ns(value):
    return -1 if value is None else int(value)


def _float(value):
    return np.nan if value is None else float(value)


def _floats(values, count):
    if values is None:
        return [np.nan] * count
    return [_float(v) for v in values]


def _hands(hands, joints, tracked, velocities, k):
    for side, hand in enumerate(hands):
        if hand is None:
            continue
        joints[k, side] = [_floats(j, 4) for j in hand["joints"]]
        tracked[k, side] = hand["tracked"]
        velocities[k, side] = [_floats(v, 3) for v in hand["velocities"]]


def load_burst(path, images=True):
    index_path = _index_path(path)
    folder = os.path.dirname(index_path)
    index = json.load(open(index_path))
    records = index["records"]
    by_kind = {kind: [r for r in records if r["kind"] == kind] for kind in ("depth", "display", "camera", "region")}
    for kind, items in by_kind.items():
        items.sort(key=lambda r: r["index"])
        if [r["index"] for r in items] != list(range(len(items))):
            raise ValueError("%s records are not numbered 0..%d" % (kind, len(items) - 1))
    times = [r["t_ns"] for r in records]
    if any(b < a for a, b in zip(times, times[1:])):
        raise ValueError("the records are not in time order")

    h, w = index["depth"]["height"], index["depth"]["width"]
    depth_records = by_kind["depth"]
    f = len(depth_records)
    depth = {
        "read": np.zeros(f, bool),
        "capture_ns": np.array([_optional_ns(r["capture_ns"]) for r in depth_records], np.int64),
        "display_ns": np.array([r["display_ns"] for r in depth_records], np.int64),
        "now_ns": np.array([r["now_ns"] for r in depth_records], np.int64),
        "near": np.array([r["near"] for r in depth_records], np.float64),
        "far": np.array([np.inf if r["far"] is None else r["far"] for r in depth_records], np.float64),
        "swapchain_index": np.array([r["swapchain_index"] for r in depth_records], np.int64),
        "position": np.array([[e["pose"]["position"] for e in r["eyes"]] for r in depth_records], np.float64).reshape(f, 2, 3),
        "orientation": np.array([[e["pose"]["orientation"] for e in r["eyes"]] for r in depth_records], np.float64).reshape(f, 2, 4),
        "tangents": np.array([[e["tangents"] for e in r["eyes"]] for r in depth_records], np.float64).reshape(f, 2, 4),
        # The JSON is column major (glam), transposed to row major
        "view_to_world": np.array([[np.reshape(e["view_to_world"], (4, 4)).T for e in r["eyes"]] for r in depth_records]).reshape(f, 2, 4, 4),
        "world_to_view": np.array([[np.reshape(e["world_to_view"], (4, 4)).T for e in r["eyes"]] for r in depth_records]).reshape(f, 2, 4, 4),
        "error": [r["error"] for r in depth_records],
    }
    if images:
        meters = np.full((f, 2, h, w), np.nan, np.float32)
        for i, r in enumerate(depth_records):
            if r["files"] is None:
                continue
            for eye, name in enumerate(r["files"]):
                meters[i, eye] = np.fromfile(os.path.join(folder, name), dtype="<f4").reshape(h, w)
            depth["read"][i] = True
        depth["meters"] = meters
    else:
        depth["read"][:] = [r["files"] is not None for r in depth_records]

    display_records = by_kind["display"]
    k = len(display_records)
    display = {
        "display_ns": np.array([r["display_ns"] for r in display_records], np.int64),
        "now_ns": np.array([r["now_ns"] for r in display_records], np.int64),
        "hand_ns": np.array([r["hand_ns"] for r in display_records], np.int64),
        "depth": np.array([-1 if r["depth"] is None else r["depth"] for r in display_records], np.int64),
        "region": np.array([-1 if r["region"] is None else r["region"] for r in display_records], np.int64),
        "eye_positions": np.array([r["eye_positions"] for r in display_records], np.float64).reshape(k, 2, 3),
        "view_position": np.array([[v["pose"]["position"] for v in r["views"]] for r in display_records], np.float64).reshape(k, 2, 3),
        "view_orientation": np.array([[v["pose"]["orientation"] for v in r["views"]] for r in display_records], np.float64).reshape(k, 2, 4),
        "view_tangents": np.array([[v["tangents"] for v in r["views"]] for r in display_records], np.float64).reshape(k, 2, 4),
        "stream_orientation": np.array([[v["pose"]["orientation"] for v in r["stream_views"]] for r in display_records], np.float64).reshape(k, 2, 4),
        "stream_tangents": np.array([[v["tangents"] for v in r["stream_views"]] for r in display_records], np.float64).reshape(k, 2, 4),
        "joints": np.full((k, 2, 26, 4), np.nan),
        "tracked": np.zeros((k, 2, 26), bool),
        "velocities": np.full((k, 2, 26, 3), np.nan),
        "capsules": np.full((k, 2, 28, 8), np.nan),
        "rim": np.full((k, 2), np.nan),
        "arm_wrist": np.full((k, 2, 3), np.nan),
        "arm_direction": np.full((k, 2, 3), np.nan),
        "arm_displacement": np.full((k, 2, 3), np.nan),
        "arm_mode": np.zeros((k, 2), np.int64),
        "arm_weight_len": np.full((k, 2), np.nan),
        "arm_target_ns": np.full((k, 2), -1, np.int64),
        "arm_history_ns": np.full((k, 2), -1, np.int64),
        "arm_wrist_history": np.full((k, 2, 3), np.nan),
        "arm_extrapolated_mm": np.full((k, 2), np.nan),
        "edge_cut": np.full(k, 20.0),
        "gap_length_mm": np.full((k, 2), np.nan),
        "gap_axis": np.full((k, 2, 3), np.nan),
        "gap_wrist": np.full((k, 2, 3), np.nan),
        "gap_set_mm": np.zeros((k, 2)),
        "arm_capture_wrist": np.full((k, 2, 3), np.nan),
        "arm_estimator": np.full((k, 2), -1, np.int64),
        "arm_reset": np.zeros((k, 2), bool),
        "mem_on": np.zeros(k, bool),
        "mem_position": np.full((k, 3), np.nan),
        "mem_yaw_deg": np.full(k, np.nan),
        "mem_pitch_deg": np.full(k, np.nan),
        "mem_world_to_view": np.full((k, 4, 4), np.nan),
        "mem_size": np.full(k, -1, np.int64),
        "mem_anchors": np.full((k, 2), -1, np.int64),
        "mem_updates": np.full((k, 2), -1, np.int64),
        "mem_frozen": np.full(k, np.nan),
        "mem_no_live": np.full(k, np.nan),
        "mem_empty": np.full(k, np.nan),
        "mem_moving": np.full(k, np.nan),
        "mem_stats_age_ms": np.full(k, np.nan),
        "mem_oldest_frozen_s": np.full(k, np.nan),
        "mem_hands": np.full((k, 2), -1, np.int64),
        "mem_margin_mm": np.full(k, np.nan),
        "cloud": [None] * k,
        "cloud_on": np.zeros(k, bool),
        "cloud_shown": np.zeros(k, bool),
        "cloud_mode": np.full(k, -1, np.int64),
        "cloud_field_capture_ns": np.full((k, 2), -1, np.int64),
        "cloud_prev_capture_ns": np.full((k, 2), -1, np.int64),
        "cloud_dt_ms": np.full((k, 2), np.nan),
        "cloud_reset": np.full((k, 2), -1, np.int64),
        "cloud_head_deg_s": np.full((k, 2), np.nan),
        "cloud_a_ms": np.full((k, 2), np.nan),
        "cloud_reach_ms": np.full((k, 2), np.nan),
        "cloud_stale": np.zeros((k, 2), bool),
    }
    new_samples = [[], []]
    for i, r in enumerate(display_records):
        _hands(r["hands"], display["joints"], display["tracked"], display["velocities"], i)
        for side, hand in enumerate(r["capsules"]):
            if hand is not None:
                display["capsules"][i, side] = [_floats(c, 8) for c in hand["capsules"]]
                display["rim"][i, side] = _float(hand["rim"])
        for side, arm in enumerate(r["arms"]):
            if arm is not None:
                display["arm_wrist"][i, side] = _floats(arm["wrist"], 3)
                display["arm_direction"][i, side] = _floats(arm["direction"], 3)
                display["arm_displacement"][i, side] = _floats(arm["displacement"], 3)
                # Version 1 had no fields of step 1: mode 1 at the time the hands were located
                display["arm_mode"][i, side] = arm.get("mode", 1)
                display["arm_weight_len"][i, side] = _float(arm.get("weight_len", 0.0))
                display["arm_target_ns"][i, side] = arm.get("target_ns", r["hand_ns"])
                display["arm_history_ns"][i, side] = arm.get("history_ns", r["hand_ns"])
                display["arm_wrist_history"][i, side] = _floats(arm.get("wrist_history", arm["wrist"]), 3)
                display["arm_extrapolated_mm"][i, side] = _float(arm.get("extrapolated_mm", 0.0))
        # Version 3: the edge cut and the wrist gaps (older bursts: today's edge, no gap)
        display["edge_cut"][i] = _float(r.get("edge_cut", 20.0))
        for side, gap in enumerate(r.get("arm_gaps") or [None, None]):
            if gap is not None:
                display["gap_length_mm"][i, side] = _float(gap["length_mm"])
                display["gap_axis"][i, side] = _floats(gap["axis"], 3)
                display["gap_wrist"][i, side] = _floats(gap["wrist"], 3)
        display["gap_set_mm"][i] = _floats(r.get("arm_gap_set_mm", [0.0, 0.0]), 2)
        # Version 4: the arm's own sample history
        for side, hand in enumerate(r.get("arm_samples") or [None, None]):
            if hand is None:
                continue
            display["arm_reset"][i, side] = hand["reset"]
            display["arm_estimator"][i, side] = hand["estimator"]
            if hand["capture_wrist"] is not None:
                display["arm_capture_wrist"][i, side] = _floats(hand["capture_wrist"], 3)
            for sample in hand["new"]:
                new_samples[side].append((sample["time_ns"], sample["seen_ns"], _floats(sample["wrist"], 3), sample["capture_time"], i))
        # Version 6: the room memory
        memory = r.get("memory")
        if memory is not None:
            display["mem_on"][i] = True
            if memory["camera"] is not None:
                display["mem_position"][i] = _floats(memory["camera"]["position"], 3)
                display["mem_yaw_deg"][i] = _float(memory["camera"]["yaw_deg"])
                display["mem_pitch_deg"][i] = _float(memory["camera"]["pitch_deg"])
                display["mem_world_to_view"][i] = np.reshape(memory["camera"]["view"]["world_to_view"], (4, 4)).T
            display["mem_size"][i] = memory["size"]
            display["mem_anchors"][i] = [memory["anchors_clean"], memory["anchors_with_hands"]]
            display["mem_updates"][i] = memory["updates"]
            stats = memory["stats"]
            if stats is not None:
                for key in ("frozen", "no_live", "empty", "moving"):
                    display["mem_" + key][i] = _float(stats[key])
                display["mem_stats_age_ms"][i] = _float(stats["age_ms"])
                display["mem_oldest_frozen_s"][i] = _float(stats["oldest_frozen_s"])
            display["mem_hands"][i] = [{"none": 0, "exact": 1, "fallback": 2}[h] for h in memory.get("hands", ["none", "none"])]
            display["mem_margin_mm"][i] = _float(memory.get("margin_mm"))
        # Version 7: the depth cloud
        cloud = r.get("cloud")
        if cloud is not None:
            display["cloud"][i] = cloud
            display["cloud_on"][i] = True
            display["cloud_shown"][i] = cloud["shown"]
            display["cloud_mode"][i] = cloud["mode"]
            for eye, field in enumerate(cloud["eyes"]):
                display["cloud_field_capture_ns"][i, eye] = _optional_ns(field["field_capture_ns"])
                display["cloud_prev_capture_ns"][i, eye] = _optional_ns(field["prev_capture_ns"])
                display["cloud_dt_ms"][i, eye] = _float(field["dt_ms"])
                display["cloud_reset"][i, eye] = {None: 0, "start": 1, "dt": 2, "head": 3, "region": 4}[field["reset"]]
                display["cloud_head_deg_s"][i, eye] = _float(field["head_deg_s"])
                display["cloud_a_ms"][i, eye] = _float(field["a_ms"])
                display["cloud_reach_ms"][i, eye] = _float(field["reach_ms"])
                display["cloud_stale"][i, eye] = bool(field.get("stale", False))
    # A reset in a record comes before its new samples
    resets = np.cumsum(display["arm_reset"], axis=0)
    samples = [
        {
            "time_ns": np.array([s[0] for s in side], np.int64),
            "seen_ns": np.array([s[1] for s in side], np.int64),
            "wrist": np.array([s[2] for s in side], np.float64).reshape(len(side), 3),
            "capture_time": np.array([s[3] for s in side], bool),
            "display": np.array([s[4] for s in side], np.int64),
            "segment": np.array([resets[s[4], hand] for s in side], np.int64),
        }
        for hand, side in enumerate(new_samples)
    ]

    camera_records = by_kind["camera"]
    c = len(camera_records)
    camera = {
        "image_ns": np.array([r["image_ns"] for r in camera_records], np.int64),
        "now_ns": np.array([r["now_ns"] for r in camera_records], np.int64),
        "intrinsics": np.array([[r["fx"], r["fy"], r["cx"], r["cy"]] for r in camera_records], np.float64).reshape(c, 4),
        "position": np.array([r["pose"]["position"] for r in camera_records], np.float64).reshape(c, 3),
        "orientation": np.array([r["pose"]["orientation"] for r in camera_records], np.float64).reshape(c, 4),
        "head_position": np.array([r["head"]["position"] for r in camera_records], np.float64).reshape(c, 3),
        "head_orientation": np.array([r["head"]["orientation"] for r in camera_records], np.float64).reshape(c, 4),
        "joints": np.full((c, 2, 26, 4), np.nan),
        "tracked": np.zeros((c, 2, 26), bool),
        "velocities": np.full((c, 2, 26, 3), np.nan),
    }
    for i, r in enumerate(camera_records):
        _hands(r["hands"], camera["joints"], camera["tracked"], camera["velocities"], i)
    if images and c:
        ch, cw = index["camera"]["height"], index["camera"]["width"]
        camera["rgb"] = np.stack(
            [np.fromfile(os.path.join(folder, r["file"]), dtype=np.uint8).reshape(ch, cw, 3) for r in camera_records]
        )

    key_lut = None
    lut = index.get("key_lut")
    if lut is not None:
        key_lut = {
            "version": lut["version"],
            "published_ns": lut["published_ns"],
            "rgba": np.fromfile(os.path.join(folder, lut["file"]), dtype=np.uint8).reshape(32, 32, 32, 4),
        }

    return {
        "index": index,
        "folder": folder,
        "depth": depth,
        "display": display,
        "camera": camera,
        "regions": [r["region"] for r in by_kind["region"]],
        "key_lut": key_lut,
        "samples": samples,
        # Version 5
        "clocks": {
            "start": (index.get("clocks") or {}).get("start"),
            "end": (index.get("clocks") or {}).get("end"),
        },
    }


def _clock_pairs(burst):
    pairs = [c for c in (burst["clocks"]["start"], burst["clocks"]["end"]) if c is not None and c["xr_ns"] is not None]
    if not pairs:
        raise ValueError("burst %d has no clock pair (burst version %d, 5 has them)" % (burst["index"]["n"], burst["index"].get("version", 1)))
    return pairs


def xr_to_realtime(burst, t_ns):
    """XrTime (any *_ns of a burst) to the headset's CLOCK_REALTIME, Unix ns (int64). Linear between the
    start and the end pair (NTP may slew the wall clock), constant outside them (the offset of the nearer
    pair: a step of the wall clock during the burst is not carried past it), the start pair alone when the
    end has no XrTime. Good to the pairs' read_ns plus the headset's NTP error against the other machine's
    clock."""
    t = np.asarray(t_ns, np.int64)
    pairs = _clock_pairs(burst)
    x0, offset0 = pairs[0]["xr_ns"], pairs[0]["realtime_ns"] - pairs[0]["xr_ns"]
    if len(pairs) == 1 or pairs[1]["xr_ns"] == x0:
        return t + np.int64(offset0)
    x1, offset1 = pairs[1]["xr_ns"], pairs[1]["realtime_ns"] - pairs[1]["xr_ns"]
    # Integers around the start: ns of Unix time do not fit a float64 exactly
    since = np.clip(t - x0, 0, x1 - x0)
    change = np.rint(since.astype(np.float64) * ((offset1 - offset0) / (x1 - x0))).astype(np.int64)
    return t + np.int64(offset0) + change


def _utc(realtime_ns):
    time = datetime.datetime.fromtimestamp(realtime_ns // 1_000_000_000, datetime.timezone.utc)
    return "%s.%06d" % (time.strftime("%Y-%m-%d %H:%M:%S"), (realtime_ns % 1_000_000_000) // 1000)


def _clocks_summary(burst):
    clocks = burst["clocks"]
    start, end = clocks["start"], clocks["end"]
    if start is None and end is None:
        return 'clocks: none recorded (burst version %d, 5 has them): use the epoch time of the "burst: %d started" logcat line' % (
            burst["index"].get("version", 1),
            burst["index"]["n"],
        )
    parts = []
    for name, c in (("start", start), ("end", end)):
        if c is None:
            parts.append("%s none" % name)
            continue
        part = "%s %s UTC" % (name, _utc(c["realtime_ns"]))
        if c["xr_ns"] is not None:
            part += ", realtime - XrTime %d ns" % (c["realtime_ns"] - c["xr_ns"])
            if c["monotonic_ns"] is not None:
                part += ", XrTime - monotonic %d ns" % (c["xr_ns"] - c["monotonic_ns"])
        else:
            part += ", no XrTime (the stream stopped)"
        if c["read_ns"] is not None:
            part += ", read in %.1f us" % (c["read_ns"] / 1e3)
        parts.append(part)
    if start is not None and end is not None and start["xr_ns"] is not None and end["xr_ns"] is not None:
        drift = (end["realtime_ns"] - end["xr_ns"]) - (start["realtime_ns"] - start["xr_ns"])
        parts.append("offset change %.1f us over %.2f s" % (drift / 1e3, (end["xr_ns"] - start["xr_ns"]) / 1e9))
        if abs(drift) > 1_000_000:
            parts.append("WARNING: the wall clock moved by over 1 ms against XrTime during the burst (an NTP step?): xr_to_realtime spreads it over the burst")
    return "clocks: " + "; ".join(parts)


def _stats(values, unit="ms"):
    values = np.asarray(values, np.float64)
    values = values[np.isfinite(values)]
    if len(values) == 0:
        return "none"
    return "n %d, mean %.1f, median %.1f, min %.1f, p95 %.1f, max %.1f %s" % (
        len(values),
        values.mean(),
        np.median(values),
        values.min(),
        np.percentile(values, 95),
        values.max(),
        unit,
    )


def load_memory_dump(path):
    """The room memory of a depth dump (its alvr_dump_<n>_depth.json), None if it has none (mem_roll off)"""
    with open(path, encoding="utf-8") as f:
        dump = json.load(f)
    memory = dump.get("memory")
    if memory is None:
        return None
    status = memory["status"]
    size = status["size"]
    folder = os.path.dirname(path)
    texels = np.stack(
        [np.fromfile(os.path.join(folder, name), dtype="<f4").reshape(size, size, 4) for name in memory["files"]]
    )
    view = status["camera"]["view"]
    return {
        "status": status,
        "texels": texels,
        "world_to_view": np.reshape(view["world_to_view"], (4, 4)).T,
        "view_to_world": np.reshape(view["view_to_world"], (4, 4)).T,
        "tangents": np.array(view["tangents"], np.float64),
    }


def summary(burst):
    index, depth, display, camera = burst["index"], burst["depth"], burst["display"], burst["camera"]
    counts = index["counts"]
    lines = [
        "burst %d: %d ms asked, %.2f s recorded, depth %dx%d (flip_y %s), depth dump %s"
        % (
            index["n"],
            index["duration_ms"],
            (index["end_ns"] - index["start_ns"]) / 1e9,
            index["depth"]["width"],
            index["depth"]["height"],
            index["depth"]["flip_y"],
            index["dump"],
        ),
        "records: %d depth frames (%d read back, %d not recorded), %d display frames, %d camera images, %d regions"
        % (counts["depth"], counts["depth_read"], counts["depth_skipped"], counts["display"], counts["camera"], counts["region"]),
    ]
    lines.append(_clocks_summary(burst))
    errors = sorted({e for e in depth["error"] if e})
    if errors:
        lines.append("readback errors: " + "; ".join(errors))

    capture = depth["capture_ns"]
    reported = capture >= 0
    lines.append("depth frame interval (capture times): " + _stats(np.diff(capture[reported]) / 1e6))
    # Depth frame 0 may have come before the start: its now_ns and display_ns are of the first display
    # frame, so it is left out of the times when the app got them
    later = np.arange(len(capture)) > 0
    lines.append("depth frame interval (when the app got them, from frame 1): " + _stats(np.diff(depth["now_ns"][later]) / 1e6))
    lines.append("depth age when the app got it (from frame 1): " + _stats((depth["now_ns"] - capture)[reported & later] / 1e6))

    used = display["depth"]
    has = used >= 0
    if has.any():
        used_capture = capture[used[has]]
        ok = used_capture >= 0
        age = (display["display_ns"][has] - used_capture)[ok] / 1e6
        lines.append("depth age at display (display time - capture time of the depth the mask used): " + _stats(age))
        frames_per_depth = np.bincount(used[has], minlength=len(capture))
        lines.append(
            "display frames per depth frame: %s, display frames without depth: %d"
            % (_stats(frames_per_depth[frames_per_depth > 0], ""), int((~has).sum()))
        )
    lines.append("display frame interval: " + _stats(np.diff(display["display_ns"]) / 1e6))
    tracked_hands = (~np.isnan(display["joints"][..., 0, 0])).sum(axis=0)
    lines.append("display frames with a located hand: left %d, right %d" % tuple(tracked_hands))
    modes = display["arm_mode"]
    arm_parts = []
    for side, name in enumerate(("left", "right")):
        counts = np.bincount(modes[:, side], minlength=3)
        part = "%s: no arm (plain depth: off or failed) %d, mode 1 %d, mode 2 %d" % (
            name,
            counts[0],
            counts[1],
            counts[2],
        )
        new = modes[:, side] == 2
        if new.any():
            part += ", mode 2 extrapolated median %.1f mm, max %.1f mm" % (
                np.median(display["arm_extrapolated_mm"][new, side]),
                display["arm_extrapolated_mm"][new, side].max(),
            )
        arm_parts.append(part)
    lines.append("display frames per arm mode (burst version %d): %s" % (index.get("version", 1), "; ".join(arm_parts)))
    sample_parts = []
    for side, name in enumerate(("left", "right")):
        hand = burst["samples"][side]
        if not len(hand["time_ns"]):
            continue
        # Intervals within a segment only: none across a reset (the hand lost in between)
        same = np.diff(hand["segment"]) == 0
        part = "%s %d, interval %s" % (name, len(hand["time_ns"]), _stats((np.diff(hand["time_ns"]) / 1e6)[same]))
        timed = hand["capture_time"]
        if timed.any():
            part += ", age when first seen (captureTime, %d samples) %s" % (
                int(timed.sum()),
                _stats((hand["seen_ns"] - hand["time_ns"])[timed] / 1e6),
            )
        part += ", resets %d" % int(display["arm_reset"][:, side].sum())
        sample_parts.append(part)
    if sample_parts:
        estimators = np.unique(display["arm_estimator"][display["arm_estimator"] >= 0])
        lines.append(
            "arm's own hand samples (estimator %s): %s"
            % ("/".join(str(e) for e in estimators), "; ".join(sample_parts))
        )
    cuts = np.unique(display["edge_cut"])
    gap_parts = []
    for side, name in enumerate(("left", "right")):
        on = np.isfinite(display["gap_length_mm"][:, side])
        set_on = display["gap_set_mm"][:, side] > 0
        if on.any() or set_on.any():
            part = "%s %d frames" % (name, int(on.sum()))
            if on.any():
                part += ", %.0f mm" % np.median(display["gap_length_mm"][on, side])
            missed = int((set_on & ~on).sum())
            if missed:
                part += ", %d frames set but not drawn (joints not located)" % missed
            gap_parts.append(part)
        else:
            gap_parts.append("%s off" % name)
    lines.append(
        "mask: edge cut %s %%; wrist gap %s"
        % ("/".join("%.0f" % c for c in cuts) if len(cuts) else "-", ", ".join(gap_parts))
    )
    on = display["mem_on"]
    if on.any():
        last = np.flatnonzero(on)[-1]
        anchors = display["mem_anchors"][last]
        part = "room memory (mem_roll 1) in %d of %d display frames, %d x %d, margin %.0f mm, anchors %d clean / %d with hands" % (
            int(on.sum()),
            len(on),
            display["mem_size"][last],
            display["mem_size"][last],
            display["mem_margin_mm"][last],
            anchors[0],
            anchors[1],
        )
        frozen = display["mem_frozen"][on]
        if np.isfinite(frozen).any():
            part += ", left eye frozen share %s, no live value median %.0f %%, moving median %.1f %%" % (
                _stats(100.0 * frozen[np.isfinite(frozen)], "%"),
                100.0 * np.nanmedian(display["mem_no_live"][on]),
                100.0 * np.nanmedian(display["mem_moving"][on]),
            )
        hands = display["mem_hands"][on]
        part += "; capsules at the capture time (history / last known / none): " + ", ".join(
            "%s %d/%d/%d" % (name, (hands[:, side] == 1).sum(), (hands[:, side] == 2).sum(), (hands[:, side] == 0).sum())
            for side, name in enumerate(("left", "right"))
        )
        lines.append(part)
    elif index.get("version", 1) >= 6:
        lines.append("room memory: off")
    on = display["cloud_on"]
    if on.any():
        last = display["cloud"][np.flatnonzero(on)[-1]]
        resets = display["cloud_reset"][on, 0]
        lines.append(
            "depth cloud (cloud %d) in %d of %d display frames, shown in %d, gain %g cap %g hold %g dead %g lambda %g fetch %g fixed %g ab %s onesided %s trail %s; left eye a %s, reach %s, stale in %d frames, frames whose left field came from a reset pair (start / dt / head / region) %d/%d/%d/%d"
            % (
                last["mode"],
                int(on.sum()),
                len(on),
                int(display["cloud_shown"].sum()),
                last["gain"],
                last["cap"],
                last["hold_ms"],
                last["dead"],
                last["lambda_ms"],
                last["fetch_mm"],
                last["fixed"],
                last["ab"],
                last.get("onesided", "-"),
                last.get("trail", "-"),
                _stats(display["cloud_a_ms"][on, 0]),
                _stats(display["cloud_reach_ms"][on, 0]),
                int(display["cloud_stale"][on, 0].sum()),
                (resets == 1).sum(),
                (resets == 2).sum(),
                (resets == 3).sum(),
                (resets == 4).sum(),
            )
        )
    elif index.get("version", 1) >= 7:
        lines.append("depth cloud: off")
    key_lut = burst.get("key_lut")
    if key_lut is None:
        lines.append("key table: none recorded")
    else:
        lines.append(
            "key table: version %d, uploaded %.0f ms before the start, %d of 32768 colours kept (alpha >= 128)"
            % (
                key_lut["version"],
                (index["start_ns"] - key_lut["published_ns"]) / 1e6,
                int((key_lut["rgba"][..., 3] >= 128).sum()),
            )
        )
    if len(camera["image_ns"]):
        lines.append("camera image interval: " + _stats(np.diff(camera["image_ns"]) / 1e6))
        lines.append("camera image age when the pass started: " + _stats((camera["now_ns"] - camera["image_ns"]) / 1e6))
    if "meters" in depth and depth["read"].any():
        meters = depth["meters"][depth["read"]]
        valid = meters > 0
        lines.append(
            "depth texels with data: %.1f %%, median distance %.3f m"
            % (100.0 * valid.mean(), float(np.median(meters[valid])) if valid.any() else float("nan"))
        )
    return "\n".join(lines)


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 1:
        print("usage: read_burst.py <burst folder (alvr_burst_<n>) or its alvr_burst_<n>.json> [--no-images]")
        sys.exit(2)
    burst = load_burst(args[0], images="--no-images" not in sys.argv)
    print(summary(burst))


if __name__ == "__main__":
    main()
