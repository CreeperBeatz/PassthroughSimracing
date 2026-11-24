# Universal mask: build notes

One section per unit of the build order in `design/universal/final.md` (section 5), in the order they were
built. Each says what changed, the properties it adds (all default off: a build with them installed behaves
like the one before unless switched on), and what is measured, assumed or left out.

## U0: clock pair in bursts (2026-11-24)

**What.** Every burst index now carries the headset's clocks read back to back at the start and at the end
of the recording, so logs of other machines (the laptop's logcat in wall time, the rig PC's input log) can
be put on the time of the burst records. Index `version` 5, new top-level field (the same format, one
field more: `design/universal/final.md` asked for the pair in the version 4 index; the version goes up as
it did for every field added before, so a reader can tell bursts with the pair from those without):

```
"clocks": {"start": {"realtime_ns": ..., "monotonic_ns": ..., "xr_ns": ..., "read_ns": ...},
           "end":   {...}}
```

- Read in this order: `CLOCK_MONOTONIC`, `CLOCK_REALTIME`, the runtime's XrTime now, `CLOCK_MONOTONIC`
  again. `read_ns` is the time between the two monotonic reads: every value of the triple lies within it
  of the others (a few microseconds expected [assumed, the first burst shows it]).
- `start.xr_ns` is the burst's `start_ns`, `end.xr_ns` its `end_ns`. Every `*_ns` of the records is XrTime.
- `end` without XrTime (`xr_ns` null): the stream stopped during the burst (no session to ask). A burst
  ended early by a new one gets the clocks the new one started with.
- The client's XrTime "now" is the runtime's conversion of a `CLOCK_MONOTONIC` read
  (`xrConvertTimespecTimeToTimeKHR`, openxr crate). That XrTime equals `CLOCK_MONOTONIC` on Quest is
  assumed in `tools/log_inputs.py`; `xr_ns - monotonic_ns` of the pair now shows it.
- `CLOCK_REALTIME` is the headset's wall clock as its NTP sets it. The rig PC is a separate machine on
  another network: its own wall clock error against the headset's is not covered by the pair (the adb clock
  probe of `log_inputs.py`, or motion in view, still measures that).

The same triple is on the log lines (tag ALVR_CAM), as `key=value` at the end of the line:
`burst: <n> started, ...; clocks realtime_ns=<n> monotonic_ns=<n> xr_ns=<n> read_ns=<n>` and
`burst: <n> ended after ...; clocks ...` (`null`: not read).

**Reader.** `tools/read_burst.py` (and its copy `docs/tools/read_burst.py`): `burst["clocks"]` (`start` /
`end`, None for older bursts), `xr_to_realtime(burst, t_ns)` (XrTime to headset Unix ns, int64, linear
between the two pairs, constant outside them), and a `clocks:` line in the summary (UTC start / end,
realtime - XrTime, XrTime - monotonic, read time, the offset's change over the burst, a warning when it is
over 1 ms: an NTP step). `realtime_ns` (about 1.8e18) is above 2^53: Python's `json` reads it exactly, a
float-based JSON reader (JavaScript, jq) rounds it by about 256 ns. Bursts of version 4 and older still
read; their summary says to use the epoch time of the "started" logcat line instead.

**Properties.** None: recording only. Cost: two triples per burst, nothing per frame.

**Tests.** `depth_burst::tests::index_is_valid_json_in_time_order` (the `clocks` object and version 5),
`clocks_text_is_one_key_value_per_clock`; `burst::tests::read_clocks_passes_xr_and_reads_wall_clock` (on
the PC build only the wall clock and the given XrTime are read: `CLOCK_MONOTONIC` is read on Android only).

**Not tested.** The Android clock read (`clock_gettime`) is compile-checked only; `xr_to_realtime` was
checked by hand on a synthetic version 5 burst. The first headset burst shows the real `read_ns` and
`xr_ns - monotonic_ns`.

**Outside the repo.** The header of `tools/log_inputs.py` (ALIGNMENT, method 2) now points to the index's
`clocks` and `xr_to_realtime`, with the logcat line as the fallback for older bursts.
