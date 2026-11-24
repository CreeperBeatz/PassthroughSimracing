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
#          rim (K, 2), arm_wrist / arm_direction / arm_displacement (K, 2, 3)
# camera:  image_ns, now_ns (C,), intrinsics (C, 4) fx fy cx cy, position (C, 3), orientation (C, 4),
#          head_position, head_orientation, joints (C, 2, 26, 4), rgb (C, H, W, 3) uint8
# regions: list of the region JSON objects; index: the whole index JSON
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
    }
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

    return {
        "index": index,
        "folder": folder,
        "depth": depth,
        "display": display,
        "camera": camera,
        "regions": [r["region"] for r in by_kind["region"]],
    }


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
