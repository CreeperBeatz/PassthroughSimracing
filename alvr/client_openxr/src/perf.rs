// Stage S0 of docs/gpu-optimization.md ("Measuring (2026-11-26)"): the `stream:` line once at every stream
// start, the `perf:` line every 2 s while streaming, the ablation properties (`debug.alvr.dbg_*`, see
// alvr_graphics::RenderAblation) and the fixed foveation of the eye swapchains (`debug.alvr.ffr*`). Only
// measurements: with every property unset the renderer and the layers are today's

use crate::passthrough::debug_property;
use alvr_common::{
    AlvrFoveatedEncodingParams, Fov,
    glam::{Quat, UVec2},
};
use alvr_graphics::{GpuRectParams, RenderAblation};
use std::time::Duration;

/// The `perf:` line is logged this often while streaming
pub const PERF_LOG_INTERVAL: Duration = Duration::from_secs(2);
// Values kept per measure and window: 2 s at 120 Hz is 240, the rest is dropped (and counted)
const MAX_SAMPLES: usize = 512;

/// `debug.alvr.dbg_*`, polled every 0.5 s
pub fn read_ablation() -> RenderAblation {
    RenderAblation::from_values(
        debug_property("debug.alvr.dbg_skip_mask"),
        debug_property("debug.alvr.dbg_mask_every"),
        debug_property("debug.alvr.dbg_skip_focus"),
        debug_property("debug.alvr.dbg_plain"),
        debug_property("debug.alvr.dbg_no_srgb"),
        debug_property("debug.alvr.dbg_readback_ms"),
    )
}

/// Stage S3: `debug.alvr.gpu_rect` (0/1/2) with `debug.alvr.gpu_rect_pad` (thousandths of the view) and
/// `debug.alvr.fast_srgb` (0/1), polled every 0.5 s with the ablation properties. Unset: (None, false),
/// today's renderer
pub fn read_gpu_work() -> (Option<GpuRectParams>, bool) {
    (
        alvr_graphics::gpu_rect_params(
            debug_property("debug.alvr.gpu_rect"),
            debug_property("debug.alvr.gpu_rect_pad"),
        ),
        debug_property("debug.alvr.fast_srgb").is_some_and(|value| value > 0),
    )
}

/// `debug.alvr.ffr` 1..4 (1 low, 2 medium, 3 high, 4 high-top), `debug.alvr.ffr_dynamic` and
/// `debug.alvr.ffr_scaled_bin`, read when the eye swapchains are created (stream start)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FfrParams {
    /// XrFoveationLevelFB: 1 low, 2 medium, 3 high, 4 high-top (Meta's value past the spec's three)
    pub level: i32,
    /// XR_FOVEATION_DYNAMIC_LEVEL_ENABLED_FB: the runtime lowers the level while there is headroom
    pub dynamic: bool,
    /// XR_SWAPCHAIN_CREATE_FOVEATION_SCALED_BIN_BIT_FB at creation (what upstream ALVR used until 2024)
    pub scaled_bin: bool,
}

impl FfrParams {
    pub fn read() -> Option<Self> {
        Self::from_values(
            debug_property("debug.alvr.ffr"),
            debug_property("debug.alvr.ffr_dynamic"),
            debug_property("debug.alvr.ffr_scaled_bin"),
        )
    }

    /// None: off (unset, 0 or negative). Levels above 4 are 4
    pub fn from_values(
        level: Option<i64>,
        dynamic: Option<i64>,
        scaled_bin: Option<i64>,
    ) -> Option<Self> {
        let level = level.filter(|level| *level > 0)?.min(4) as i32;

        Some(Self {
            level,
            dynamic: dynamic.is_some_and(|value| value > 0),
            scaled_bin: scaled_bin.is_some_and(|value| value > 0),
        })
    }

    pub fn level_name(&self) -> &'static str {
        match self.level {
            1 => "low",
            2 => "medium",
            3 => "high",
            _ => "high-top",
        }
    }

    pub fn text(&self) -> String {
        format!(
            "ffr {} ({}), ffr_dynamic {}, ffr_scaled_bin {}",
            self.level,
            self.level_name(),
            u8::from(self.dynamic),
            u8::from(self.scaled_bin)
        )
    }
}

/// What became of `debug.alvr.ffr` at the stream start
#[derive(Clone, Debug, PartialEq)]
pub enum FfrOutcome {
    /// Unset: the swapchains are created as today
    Off,
    /// The profile is applied to both eye swapchains
    Applied(FfrParams),
    /// Not applied (the swapchains as today, or created but without the profile), and why
    NotApplied(FfrParams, String),
}

impl FfrOutcome {
    pub fn text(&self) -> String {
        match self {
            Self::Off => "ffr off (today's)".into(),
            Self::Applied(params) => format!("{}: applied", params.text()),
            Self::NotApplied(params, reason) => format!("{}: NOT applied, {reason}", params.text()),
        }
    }
}

/// The facts of the `stream:` line
pub struct StreamFacts<'a> {
    pub view_resolution: UVec2,
    pub swapchain_resolution: UVec2,
    /// GL enum
    pub swapchain_format: u32,
    /// None: upscaling off
    pub upscale_factor: Option<f32>,
    pub foveated_encoding: Option<&'a AlvrFoveatedEncodingParams>,
    pub refresh_hint: f32,
    /// The runtime's current rate, None: it does not say
    pub refresh_now: Option<f32>,
    /// Of the located views (left, right), None: not located yet
    pub fovs: Option<[Fov; 2]>,
    pub mask_size: UVec2,
    /// "Depth", "Blend", ... None: no passthrough
    pub passthrough: Option<&'static str>,
    /// The server setting `lighting` of the depth passthrough, None: not depth
    pub lighting: Option<bool>,
    /// The colour key's layer mode: (key_layers, key_crop), None: key off
    pub key: Option<(i64, i64)>,
    pub ablation: &'a RenderAblation,
    pub ffr: &'a FfrOutcome,
    /// `debug.alvr.edge_fill` (tenths of a degree, 0: off)
    pub edge_fill: u32,
    /// The custom reprojection path (YVR), where the edge fill is not applied
    pub custom_reprojection: bool,
    /// Stage S3: `debug.alvr.gpu_rect` (None: off) and `debug.alvr.fast_srgb`
    pub gpu_rect: Option<GpuRectParams>,
    pub fast_srgb: bool,
}

pub fn gl_format_name(format: u32) -> String {
    match format {
        0x8C43 => "SRGB8_ALPHA8".into(),
        0x8058 => "RGBA8".into(),
        0x881A => "RGBA16F".into(),
        other => format!("{other:#x}"),
    }
}

fn fov_text(fov: &Fov) -> String {
    let deg = |angle: f32| angle.to_degrees();
    format!(
        "l {:.1} r {:.1} u {:.1} d {:.1} deg (tan {:.3} {:.3} {:.3} {:.3})",
        deg(fov.left),
        deg(fov.right),
        deg(fov.up),
        deg(fov.down),
        fov.left.tan(),
        fov.right.tan(),
        fov.up.tan(),
        fov.down.tan()
    )
}

/// The `stream:` line
pub fn stream_text(facts: &StreamFacts) -> String {
    let upscale = facts
        .upscale_factor
        .map_or("off".to_string(), |factor| format!("x{factor:.2}"));
    let ffe = facts.foveated_encoding.map_or("off".to_string(), |ffe| {
        format!(
            "on (encoded {}x{}, center size {:.2}/{:.2}, center shift L {:.2}/{:.2} R {:.2}/{:.2}, \
             edge ratio {:.1}/{:.1}, view ratio {:.3}/{:.3})",
            ffe.encoded_view_resolution[0],
            ffe.encoded_view_resolution[1],
            ffe.center_size[0],
            ffe.center_size[1],
            ffe.center_shifts[0][0],
            ffe.center_shifts[0][1],
            ffe.center_shifts[1][0],
            ffe.center_shifts[1][1],
            ffe.edge_ratio[0],
            ffe.edge_ratio[1],
            ffe.view_ratio[0],
            ffe.view_ratio[1],
        )
    });
    let refresh = facts
        .refresh_now
        .map_or(format!("{:.0} Hz asked", facts.refresh_hint), |now| {
            format!("{now:.0} Hz ({:.0} asked)", facts.refresh_hint)
        });
    let fov = facts
        .fovs
        .map_or("not located".to_string(), |[left, right]| {
            format!("L {}, R {}", fov_text(&left), fov_text(&right))
        });
    let lighting = match facts.lighting {
        Some(true) => "on",
        Some(false) => "off",
        None => "-",
    };
    let key = facts.key.map_or("off".to_string(), |(layers, crop)| {
        format!("on, key_layers {layers}, key_crop {crop}")
    });
    let edge_fill = match (facts.edge_fill, facts.custom_reprojection) {
        (0, _) => "edge_fill off (today's)".to_string(),
        (tenths, false) => format!(
            "edge_fill {tenths} ({:.1} deg per side)",
            tenths as f32 * 0.1
        ),
        (tenths, true) => format!("edge_fill {tenths}: NOT applied (custom reprojection)"),
    };

    format!(
        "stream: view {}x{}, swapchain {}x{} {}, upscale {upscale}, FFE {ffe}, refresh {refresh}, \
         fov {fov}, depth mask {}x{}, passthrough {}, lighting {lighting}, colour key {key}, {}, {}, \
         {edge_fill}, {}",
        facts.view_resolution.x,
        facts.view_resolution.y,
        facts.swapchain_resolution.x,
        facts.swapchain_resolution.y,
        gl_format_name(facts.swapchain_format),
        facts.mask_size.x,
        facts.mask_size.y,
        facts.passthrough.unwrap_or("off"),
        facts.ablation.text(),
        facts.ffr.text(),
        alvr_graphics::gpu_work_text(facts.gpu_rect.as_ref(), facts.fast_srgb),
    )
}

/// The head turn (degrees) between the orientation a frame was rendered for and the one located for its
/// display time
pub fn turn_deg(rendered: Quat, displayed: Quat) -> f32 {
    // atan2 of the relative rotation stays exact for small turns (acos of the dot product does not in f32)
    let relative = rendered.normalize().inverse() * displayed.normalize();

    (2.0 * relative.xyz().length().atan2(relative.w.abs())).to_degrees()
}

// Values of one measure in the window, in a fixed array
struct Samples {
    values: [f32; MAX_SAMPLES],
    len: usize,
    dropped: u32,
}

impl Samples {
    const fn new() -> Self {
        Self {
            values: [0.0; MAX_SAMPLES],
            len: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, value: f32) {
        if self.len < MAX_SAMPLES {
            self.values[self.len] = value;
            self.len += 1;
        } else {
            self.dropped += 1;
        }
    }

    fn clear(&mut self) {
        self.len = 0;
        self.dropped = 0;
    }

    // Median, 90th percentile and maximum (sorts the values in place), None without values
    fn stats(&mut self) -> Option<(f32, f32, f32)> {
        let values = &mut self.values[..self.len];
        if values.is_empty() {
            return None;
        }
        values.sort_unstable_by(f32::total_cmp);
        let at = |q: f32| values[((values.len() - 1) as f32 * q).round() as usize];

        Some((at(0.5), at(0.9), values[values.len() - 1]))
    }

    // "<median> ms p90 <p90>" (with "max <max>" if asked), or "-"
    fn text(&mut self, unit: &str, decimals: usize, with_max: bool) -> String {
        match self.stats() {
            None => "-".into(),
            Some((median, p90, max)) if with_max => {
                format!("{median:.decimals$} {unit} p90 {p90:.decimals$} max {max:.decimals$}")
            }
            Some((median, p90, _)) => format!("{median:.decimals$} {unit} p90 {p90:.decimals$}"),
        }
    }
}

/// The client's statistics of a frame (alvr_client_core, the ones sent to the streamer)
#[derive(Clone, Copy, Default)]
pub struct FrameLatencies {
    pub decode: Duration,
    pub decoder_queue: Duration,
    pub total: Duration,
}

/// The window of the `perf:` line. No allocation per frame: fixed arrays, reset after each line
pub struct PerfLog {
    display_frames: u32,
    new_frames: u32,
    repeats: u32,
    // Display time minus the drawn frame's timestamp (ms)
    age_ms: Samples,
    // Head turn between the rendered-for pose and the located display pose (degrees)
    turn_deg: Samples,
    // Between the display times of frames with a new decoded frame (ms)
    interval_ms: Samples,
    // Between the timestamps of new decoded frames (ms)
    source_interval_ms: Samples,
    decode_ms: Samples,
    queue_ms: Samples,
    total_ms: Samples,
    // Kept across windows
    last_new_display_ns: Option<i64>,
    last_timestamp_ns: Option<i64>,
}

impl Default for PerfLog {
    fn default() -> Self {
        Self {
            display_frames: 0,
            new_frames: 0,
            repeats: 0,
            age_ms: Samples::new(),
            turn_deg: Samples::new(),
            interval_ms: Samples::new(),
            source_interval_ms: Samples::new(),
            decode_ms: Samples::new(),
            queue_ms: Samples::new(),
            total_ms: Samples::new(),
            last_new_display_ns: None,
            last_timestamp_ns: None,
        }
    }
}

impl PerfLog {
    /// Whether a decoded frame was drawn yet (the rendered-for pose of the views is known)
    pub fn has_frame(&self) -> bool {
        self.last_timestamp_ns.is_some()
    }

    /// One display frame (one `render`): its display time, the timestamp of a new decoded frame (None:
    /// the previous one is drawn again) and the head turn of the drawn frame (None: unknown)
    pub fn display_frame(
        &mut self,
        display_ns: i64,
        new_frame_ns: Option<i64>,
        turn_deg: Option<f32>,
    ) {
        self.display_frames += 1;
        if let Some(timestamp_ns) = new_frame_ns {
            self.new_frames += 1;
            if let Some(last) = self.last_new_display_ns {
                self.interval_ms.push((display_ns - last) as f32 * 1e-6);
            }
            if let Some(last) = self.last_timestamp_ns {
                self.source_interval_ms
                    .push((timestamp_ns - last) as f32 * 1e-6);
            }
            self.last_new_display_ns = Some(display_ns);
            self.last_timestamp_ns = Some(timestamp_ns);
        } else if self.last_timestamp_ns.is_some() {
            self.repeats += 1;
        }
        if let Some(timestamp_ns) = self.last_timestamp_ns {
            self.age_ms.push((display_ns - timestamp_ns) as f32 * 1e-6);
        }
        if let Some(turn) = turn_deg {
            self.turn_deg.push(turn);
        }
    }

    /// The client's statistics of a new frame
    pub fn latencies(&mut self, latencies: &FrameLatencies) {
        self.decode_ms.push(latencies.decode.as_secs_f32() * 1000.0);
        self.queue_ms
            .push(latencies.decoder_queue.as_secs_f32() * 1000.0);
        self.total_ms.push(latencies.total.as_secs_f32() * 1000.0);
    }

    /// The `perf:` line of the window that lasted `elapsed_s`, then a new window. `prediction`: the
    /// client's head prediction offset now
    pub fn text(&mut self, elapsed_s: f32, prediction: Duration) -> String {
        let rate = |count: u32| count as f32 / elapsed_s.max(1e-3);
        let dropped = [
            &self.age_ms,
            &self.turn_deg,
            &self.interval_ms,
            &self.source_interval_ms,
            &self.decode_ms,
            &self.queue_ms,
            &self.total_ms,
        ]
        .iter()
        .map(|samples| samples.dropped)
        .max()
        .unwrap_or(0);
        let text = format!(
            "perf: display {:.1} fps, new frames {:.1} fps, repeated {} of {} display frames, frame age \
             at display {}, head turn render->display {}, new frame interval at display {}, \
             timestamps {}, decode {}, decoder queue {}, total latency {}, prediction {:.1} ms{}",
            rate(self.display_frames),
            rate(self.new_frames),
            self.repeats,
            self.display_frames,
            self.age_ms.text("ms", 1, false),
            self.turn_deg.text("deg", 2, false),
            self.interval_ms.text("ms", 1, true),
            self.source_interval_ms.text("ms", 1, true),
            self.decode_ms.text("ms", 1, false),
            self.queue_ms.text("ms", 1, false),
            self.total_ms.text("ms", 0, false),
            prediction.as_secs_f32() * 1000.0,
            if dropped > 0 {
                format!(" ({dropped} values past {MAX_SAMPLES} not counted)")
            } else {
                String::new()
            }
        );

        self.display_frames = 0;
        self.new_frames = 0;
        self.repeats = 0;
        for samples in [
            &mut self.age_ms,
            &mut self.turn_deg,
            &mut self.interval_ms,
            &mut self.source_interval_ms,
            &mut self.decode_ms,
            &mut self.queue_ms,
            &mut self.total_ms,
        ] {
            samples.clear();
        }

        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    #[test]
    fn ffr_unset_is_off() {
        assert_eq!(FfrParams::from_values(None, None, None), None);
        assert_eq!(FfrParams::from_values(Some(0), Some(1), Some(1)), None);
        assert_eq!(FfrParams::from_values(Some(-2), None, None), None);
        assert_eq!(FfrOutcome::Off.text(), "ffr off (today's)");
    }

    #[test]
    fn ffr_levels() {
        let high = FfrParams::from_values(Some(3), None, None).unwrap();
        assert_eq!(
            high,
            FfrParams {
                level: 3,
                dynamic: false,
                scaled_bin: false
            }
        );
        assert_eq!(high.level_name(), "high");
        let top = FfrParams::from_values(Some(9), Some(1), Some(1)).unwrap();
        assert_eq!(top.level, 4);
        assert_eq!(top.level_name(), "high-top");
        assert!(top.dynamic && top.scaled_bin);
        assert_eq!(
            FfrOutcome::NotApplied(high, "no XR_FB_foveation".into()).text(),
            "ffr 3 (high), ffr_dynamic 0, ffr_scaled_bin 0: NOT applied, no XR_FB_foveation"
        );
        assert_eq!(
            FfrOutcome::Applied(top).text(),
            "ffr 4 (high-top), ffr_dynamic 1, ffr_scaled_bin 1: applied"
        );
    }

    #[test]
    fn gl_formats_are_named() {
        assert_eq!(gl_format_name(0x8C43), "SRGB8_ALPHA8");
        assert_eq!(gl_format_name(0x8058), "RGBA8");
        assert_eq!(gl_format_name(0x881A), "RGBA16F");
        assert_eq!(gl_format_name(0x1234), "0x1234");
    }

    #[test]
    fn turn_between_poses() {
        assert!(turn_deg(Quat::IDENTITY, Quat::IDENTITY) < 1e-3);
        let turned = Quat::from_rotation_y(2f32.to_radians());
        assert!((turn_deg(Quat::IDENTITY, turned) - 2.0).abs() < 1e-3);
        // q and -q are the same rotation
        assert!(turn_deg(turned, -turned) < 1e-3);
        // Small turns are resolved
        let small = Quat::from_rotation_x(0.01f32.to_radians());
        assert!((turn_deg(turned, turned * small) - 0.01).abs() < 1e-4);
    }

    #[test]
    fn stream_line_has_the_facts() {
        let ablation = RenderAblation::default();
        let ffr = FfrOutcome::Off;
        let a = 0.8_f32;
        let fov = Fov {
            left: -a,
            right: a,
            up: a,
            down: -a,
        };
        let text = stream_text(&StreamFacts {
            view_resolution: UVec2::new(2144, 2240),
            swapchain_resolution: UVec2::new(2144, 2240),
            swapchain_format: 0x8C43,
            upscale_factor: None,
            foveated_encoding: None,
            refresh_hint: 72.0,
            refresh_now: Some(72.0),
            fovs: Some([fov, fov]),
            mask_size: UVec2::new(428, 448),
            passthrough: Some("Depth"),
            lighting: Some(false),
            key: Some((5, 0)),
            ablation: &ablation,
            ffr: &ffr,
            edge_fill: 0,
            custom_reprojection: false,
            gpu_rect: None,
            fast_srgb: false,
        });
        for part in [
            "stream: view 2144x2240, swapchain 2144x2240 SRGB8_ALPHA8, upscale off, FFE off",
            "refresh 72 Hz (72 asked)",
            "l -45.8 r 45.8 u 45.8 d -45.8 deg (tan -1.030 1.030 1.030 -1.030)",
            "depth mask 428x448, passthrough Depth, lighting off",
            "colour key on, key_layers 5, key_crop 0",
            "dbg_readback_ms 2000 (all today's), ffr off (today's), edge_fill off (today's), \
             gpu_rect 0, fast_srgb 0",
        ] {
            assert!(text.contains(part), "{part} not in {text}");
        }
    }

    #[test]
    fn perf_window_counts_frames() {
        let mut log = PerfLog::default();
        assert!(!log.has_frame());
        // Before the first decoded frame: displayed, not repeated, no age
        log.display_frame(0, None, None);
        // Display every 14 ms; new frames at 0, 1, 3 (frame 2 repeats the one before)
        log.display_frame(14 * MS, Some(10 * MS), Some(1.0));
        log.display_frame(28 * MS, Some(24 * MS), Some(2.0));
        log.display_frame(42 * MS, None, Some(3.0));
        log.display_frame(56 * MS, Some(52 * MS), Some(4.0));
        log.latencies(&FrameLatencies {
            decode: Duration::from_millis(5),
            decoder_queue: Duration::from_millis(1),
            total: Duration::from_millis(60),
        });
        assert!(log.has_frame());
        let text = log.text(1.0, Duration::from_millis(50));
        for part in [
            "perf: display 5.0 fps, new frames 3.0 fps, repeated 1 of 5 display frames",
            // Ages 4, 4, 18, 4
            "frame age at display 4.0 ms p90 18.0",
            "head turn render->display 3.00 deg p90 4.00",
            // Display intervals 14, 28; timestamp intervals 14, 28
            "new frame interval at display 28.0 ms p90 28.0 max 28.0",
            "timestamps 28.0 ms p90 28.0 max 28.0",
            "decode 5.0 ms p90 5.0, decoder queue 1.0 ms p90 1.0, total latency 60 ms p90 60",
            "prediction 50.0 ms",
        ] {
            assert!(text.contains(part), "{part} not in {text}");
        }

        // A new window: empty, but the last frame is still known
        let text = log.text(2.0, Duration::ZERO);
        assert!(text.starts_with(
            "perf: display 0.0 fps, new frames 0.0 fps, repeated 0 of 0 display frames, frame age at display -"
        ));
        log.display_frame(70 * MS, None, None);
        log.display_frame(84 * MS, Some(80 * MS), None);
        let text = log.text(1.0, Duration::ZERO);
        assert!(text.contains("repeated 1 of 2"), "{text}");
        assert!(
            text.contains("new frame interval at display 28.0 ms"),
            "{text}"
        );
    }

    #[test]
    fn perf_samples_stop_at_the_limit() {
        let mut log = PerfLog::default();
        for i in 0..(MAX_SAMPLES as i64 + 10) {
            log.display_frame(i * 14 * MS, Some(i * 14 * MS), None);
        }
        let text = log.text(1.0, Duration::ZERO);
        assert!(text.contains("(10 values past 512 not counted)"), "{text}");
    }

    #[test]
    fn stream_line_has_the_edge_fill() {
        let ablation = RenderAblation::default();
        let ffr = FfrOutcome::Off;
        let facts = |edge_fill, custom_reprojection| StreamFacts {
            view_resolution: UVec2::new(2144, 2240),
            swapchain_resolution: UVec2::new(2144, 2240),
            swapchain_format: 0x8C43,
            upscale_factor: None,
            foveated_encoding: None,
            refresh_hint: 72.0,
            refresh_now: None,
            fovs: None,
            mask_size: UVec2::new(428, 448),
            passthrough: Some("Depth"),
            lighting: None,
            key: None,
            ablation: &ablation,
            ffr: &ffr,
            edge_fill,
            custom_reprojection,
            gpu_rect: alvr_graphics::gpu_rect_params(Some(1), Some(30)),
            fast_srgb: true,
        };
        let line = stream_text(&facts(20, false));
        assert!(
            line.ends_with(", edge_fill 20 (2.0 deg per side), gpu_rect 1 (pad 30), fast_srgb 1"),
            "{line}"
        );
        let line = stream_text(&facts(20, true));
        assert!(
            line.contains(", edge_fill 20: NOT applied (custom reprojection), "),
            "{line}"
        );
        let line = stream_text(&facts(0, true));
        assert!(line.contains(", edge_fill off (today's), "), "{line}");
    }
}
