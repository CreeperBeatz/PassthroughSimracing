// Ablations of the stream renderer (stage S0 of docs/gpu-optimization.md, "Measuring (2026-11-26)"): each
// removes one piece of GPU work so that its cost shows in the headset's own performance numbers. The
// picture may be wrong while one is set. The default (every property unset) is today's renderer exactly:
// every pass runs, the main pass uses today's pipeline and immediates, the readback keeps its interval

use crate::room_memory_gpu::MEMORY_STATS_INTERVAL;
use std::time::Duration;

/// The room memory's statistics readback interval today
pub const DEFAULT_READBACK_MS: u32 = MEMORY_STATS_INTERVAL.as_millis() as u32;
/// Largest `dbg_readback_ms`
pub const MAX_READBACK_MS: u32 = 60_000;
/// Largest `dbg_mask_every`
pub const MAX_MASK_EVERY: u32 = 8;

/// What the renderer leaves out (`debug.alvr.dbg_*`, read by the client every 0.5 s)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RenderAblation {
    /// `dbg_skip_mask 1`: the depth mask pass does not run; the mask textures keep their last content
    pub skip_mask: bool,
    /// `dbg_mask_every N`: the depth mask pass of each eye runs only every Nth display frame (eye 1 half
    /// a period after eye 0). 1: every frame (today's)
    pub mask_every: u32,
    /// `dbg_skip_focus 1`: the hands' focus pass does not run (the focus textures keep their content)
    pub skip_focus: bool,
    /// `dbg_plain 1`: the main pass takes the shader's Blend path with alpha 1: one stream tap, no mask,
    /// focus or light-probe tap, no alpha packing
    pub plain: bool,
    /// `dbg_no_srgb 1`: the main pass uses a second pipeline built with `ENABLE_SRGB_CORRECTION` false
    pub no_srgb: bool,
    /// `dbg_readback_ms`: the interval of the room memory's statistics readback. 0: never
    pub readback_ms: u32,
}

impl Default for RenderAblation {
    fn default() -> Self {
        Self {
            skip_mask: false,
            mask_every: 1,
            skip_focus: false,
            plain: false,
            no_srgb: false,
            readback_ms: DEFAULT_READBACK_MS,
        }
    }
}

impl RenderAblation {
    /// From the raw property values (None: unset). Out of range values are clamped
    pub fn from_values(
        skip_mask: Option<i64>,
        mask_every: Option<i64>,
        skip_focus: Option<i64>,
        plain: Option<i64>,
        no_srgb: Option<i64>,
        readback_ms: Option<i64>,
    ) -> Self {
        let on = |value: Option<i64>| value.is_some_and(|value| value > 0);

        Self {
            skip_mask: on(skip_mask),
            mask_every: mask_every.map_or(1, |every| every.clamp(1, MAX_MASK_EVERY as i64) as u32),
            skip_focus: on(skip_focus),
            plain: on(plain),
            no_srgb: on(no_srgb),
            readback_ms: readback_ms.map_or(DEFAULT_READBACK_MS, |ms| {
                ms.clamp(0, MAX_READBACK_MS as i64) as u32
            }),
        }
    }

    /// Whether anything differs from today's renderer
    pub fn is_active(&self) -> bool {
        *self != Self::default()
    }

    /// Whether the depth mask of `eye` runs in display frame `frame` (counted by the renderer)
    pub fn mask_runs(&self, frame: u64, eye: usize) -> bool {
        if self.skip_mask {
            return false;
        }
        let every = u64::from(self.mask_every.max(1));
        // Eye 1 half a period later, so that the two eyes don't fall on the same frame
        let offset = if eye == 0 { 0 } else { every / 2 };

        (frame + offset).is_multiple_of(every)
    }

    /// The statistics readback interval, None: never
    pub fn readback_interval(&self) -> Option<Duration> {
        (self.readback_ms > 0).then(|| Duration::from_millis(u64::from(self.readback_ms)))
    }

    /// For the `stream:` and `ablation:` lines
    pub fn text(&self) -> String {
        format!(
            "dbg_skip_mask {}, dbg_mask_every {}, dbg_skip_focus {}, dbg_plain {}, dbg_no_srgb {}, \
             dbg_readback_ms {}{}",
            u8::from(self.skip_mask),
            self.mask_every,
            u8::from(self.skip_focus),
            u8::from(self.plain),
            u8::from(self.no_srgb),
            self.readback_ms,
            if self.is_active() {
                ""
            } else {
                " (all today's)"
            }
        )
    }
}

/// The pipeline constants of the main pass with `dbg_no_srgb` (the others as given)
pub(crate) fn no_srgb_constants(constants: &[(&'static str, f64)]) -> Vec<(&'static str, f64)> {
    constants
        .iter()
        .map(|&(name, value)| {
            if name == "ENABLE_SRGB_CORRECTION" {
                (name, 0.0)
            } else {
                (name, value)
            }
        })
        .collect()
}
