//! Lever windows (U2 of `design/universal/final.md`, section 4.3). A lever (H or sequential shifter,
//! handbrake) moves only while it is held or just after it is let go. At rest the live depth is right.
//! Held, the runtime removes it from the depth with the hand: of a held 19.3 cm bottle only the bottom
//! 3.6-5.5 cm stayed [measured, KNOWLEDGE_BASE bottle analysis], and the hands are removed whatever
//! `debug.alvr.hand_removal` says [measured, `logs/analysis-20261125-e1`]. So no depth test can draw a
//! held lever. Instead, while a tracked hand is near the play-area box of a lever, every view ray that
//! crosses that box shows the real room, whatever the depth says (the window, `LeverWindow`).
//!
//! - Gate: open while a joint of a tracked hand is within `debug.alvr.lever_gate_mm` (130) of the box
//!   (inside counts as 0). Hands are located at the display time. The key locates its hands earlier
//!   (display time minus `passthrough_delay_ms`), so the gate opens a little sooner than the key sees the
//!   hand: the safe side.
//! - Late close: after the hand left, the window stays open until the depth frame on screen was
//!   captured at least `debug.alvr.lever_close_ms` (300) after the hand left. Then the depth shows the
//!   lever again, also after a spring returned it [assumed: 300 ms covers a spring return; the rig's L2
//!   bursts measure it]. If no such depth frame comes, it closes 1 s later anyway. It then fades out over
//!   150 ms instead of popping, from the coverage where the mask edge is full (`mask_edge_top`, which
//!   follows `debug.alvr.edge_cut`), so the whole fade is seen.
//! - Shrink (`debug.alvr.lever_shrink 1`): the window is the bounding box of the palm's path inside the
//!   box while the gate was open, plus 2 cm on each side, in x and z only: it keeps the whole height of
//!   the box. Down to the floor of the box for the lever's rest extent (its shaft goes from the base up
//!   to the knob the palm holds), and up to its top because the palm joint is the centre of the palm: a
//!   fist round a tall sequential stick or a handbrake grip leaves the top of the lever some cm above it,
//!   and the runtime removes what sticks out above a held object too [assumed from the bottle, not
//!   measured on a lever]. Six numbers in the frame of the box, kept for the session: the play-area file
//!   belongs to the editor, which rewrites it without them. Nothing learned yet, or 0 (the default): the
//!   whole box. A change of the box (the editor, another layout) forgets what was learned.
//! - Not built: the channel binding of section 4.3 (a PC input that changed in the last 0.5 s also opens
//!   the gate). `LeverWindows::channel_changed` is the hook for it (U6, the PC reader); nothing calls it
//!   yet.
//!
//! Pure maths and state, so it can be tested on the PC. Positions are in the area frame (`PlayArea`).

use alvr_common::glam::{Mat4, Vec3};
use alvr_graphics::{
    AreaBox, LeverWindow, MAX_AREA_BOXES, MAX_LEVER_WINDOWS, PlacedArea, RegionOverride,
};
use std::fmt::Write;

pub const DEFAULT_GATE_MM: i64 = 130;
pub const MAX_GATE_MM: i64 = 500;
pub const DEFAULT_CLOSE_MS: i64 = 300;
pub const MAX_CLOSE_MS: i64 = 3000;
/// Margin around the palm's path (meters): the held bottle stood 2 cm outside its box [measured, KB]
pub const SHRINK_MARGIN: f32 = 0.02;
/// If no depth frame captured after the close time comes (no depth, a stalled provider), the window
/// closes this long after `lever_close_ms` anyway (ns)
pub const MAX_DEPTH_WAIT_NS: i64 = 1_000_000_000;
/// The fade after the close (ns) [assumed, set by eye]
pub const FADE_NS: i64 = 150_000_000;
/// A PC input bound to a lever keeps its gate open this long after it changed (ns, section 4.3)
pub const CHANNEL_HOLD_NS: i64 = 500_000_000;
/// A change stamped up to this much after the display time still counts (the PC clock mapped to XrTime
/// is off by some ms); one further ahead is a mapping error and is ignored, or it would hold the gate
/// open until then (ns) [assumed]
pub const CHANNEL_AHEAD_NS: i64 = 50_000_000;
/// XR_HAND_JOINT_PALM_EXT
const PALM_JOINT: usize = 0;

/// `debug.alvr.lever_*`. Unset, no box is a lever and nothing is done (no hand lookup, no window)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeverParams {
    /// Play-area boxes that are levers (`#<index>` of the area editor's log, from 0), sorted, at most
    /// `MAX_LEVER_WINDOWS`. Empty: off
    pub boxes: Vec<usize>,
    /// Words of `debug.alvr.lever_boxes` that are no box index, or boxes beyond the fourth, for the log
    pub ignored: Vec<String>,
    pub gate_mm: i64,
    pub close_ms: i64,
    /// The learned volume instead of the whole box
    pub shrink: bool,
    /// `debug.alvr.lever_forget`: another value forgets the learned volumes
    pub forget: i64,
}

impl Default for LeverParams {
    fn default() -> Self {
        Self::from_values(None, None, None, None, None)
    }
}

impl LeverParams {
    #[cfg(target_os = "android")]
    pub fn read() -> Self {
        use crate::passthrough::{debug_property, debug_property_string};

        Self::from_values(
            debug_property_string("debug.alvr.lever_boxes").as_deref(),
            debug_property("debug.alvr.lever_gate_mm"),
            debug_property("debug.alvr.lever_close_ms"),
            debug_property("debug.alvr.lever_shrink"),
            debug_property("debug.alvr.lever_forget"),
        )
    }

    #[cfg(not(target_os = "android"))]
    pub fn read() -> Self {
        Self::default()
    }

    pub fn from_values(
        boxes: Option<&str>,
        gate_mm: Option<i64>,
        close_ms: Option<i64>,
        shrink: Option<i64>,
        forget: Option<i64>,
    ) -> Self {
        let (boxes, ignored) = parse_box_list(boxes.unwrap_or(""));
        Self {
            boxes,
            ignored,
            gate_mm: gate_mm.unwrap_or(DEFAULT_GATE_MM).clamp(0, MAX_GATE_MM),
            close_ms: close_ms.unwrap_or(DEFAULT_CLOSE_MS).clamp(0, MAX_CLOSE_MS),
            shrink: shrink.unwrap_or(0) != 0,
            forget: forget.unwrap_or(0),
        }
    }

    pub fn is_on(&self) -> bool {
        !self.boxes.is_empty()
    }

    fn gate(&self) -> f32 {
        self.gate_mm as f32 / 1000.0
    }

    fn close_ns(&self) -> i64 {
        self.close_ms * 1_000_000
    }

    /// The `lever:` line of the settings, at the start and on each change
    pub fn text(&self) -> String {
        if !self.is_on() {
            let mut text = "lever: off (no box in debug.alvr.lever_boxes)".to_string();
            if !self.ignored.is_empty() {
                let _ = write!(text, ", ignored: {}", self.ignored.join(" "));
            }
            return text;
        }
        let boxes = self
            .boxes
            .iter()
            .map(|index| format!("#{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let mut text = format!(
            "lever: boxes {boxes}, gate {} mm, close {} ms after the depth shows the hand gone, {}",
            self.gate_mm,
            self.close_ms,
            if self.shrink {
                "window: the palm's path (lever_shrink 1)"
            } else {
                "window: the whole box"
            }
        );
        if !self.ignored.is_empty() {
            let _ = write!(text, ", ignored: {}", self.ignored.join(" "));
        }
        text
    }
}

/// `debug.alvr.lever_boxes`: box indices separated by commas, spaces or semicolons ("1", "1,3"). Indices
/// of 0..`MAX_AREA_BOXES`, each once, sorted, at most `MAX_LEVER_WINDOWS`. Returns them and the words
/// left out
pub fn parse_box_list(text: &str) -> (Vec<usize>, Vec<String>) {
    let mut boxes = Vec::new();
    let mut ignored = Vec::new();
    for word in text
        .split([',', ' ', ';'])
        .map(|word| word.trim().trim_start_matches('#'))
        .filter(|word| !word.is_empty())
    {
        match word.parse::<usize>() {
            Ok(index) if index < MAX_AREA_BOXES => {
                if !boxes.contains(&index) {
                    boxes.push(index);
                }
            }
            _ => ignored.push(word.to_string()),
        }
    }
    boxes.sort_unstable();
    if boxes.len() > MAX_LEVER_WINDOWS {
        ignored.extend(
            boxes
                .drain(MAX_LEVER_WINDOWS..)
                .map(|index| format!("#{index} (more than {MAX_LEVER_WINDOWS} levers)")),
        );
    }

    (boxes, ignored)
}

/// A tracked hand at the display time: its joints with a valid position, area frame (None: not
/// valid). Index: XrHandJointEXT
#[derive(Clone, Copy, Debug)]
pub struct LeverHand {
    pub joints: [Option<Vec3>; 26],
}

impl LeverHand {
    /// Distance of the nearest valid joint from the box (meters, 0 inside). None: no valid joint
    fn distance(&self, area_box: &AreaBox) -> Option<f32> {
        self.joints
            .iter()
            .flatten()
            .map(|&joint| area_box.signed_distance(joint, f32::INFINITY).max(0.0))
            .reduce(f32::min)
    }
}

/// The bounding box of the palm's path inside a lever's box, in the frame of the box (meters)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LearnedVolume {
    pub min: Vec3,
    pub max: Vec3,
}

impl LearnedVolume {
    /// The window: the path plus `SHRINK_MARGIN` in x and z, within the box, and the whole height of
    /// the box (the lever's rest extent below the palm, the top of the lever above it). Min and max
    /// corner in the frame of the box
    pub fn window(&self, half: Vec3) -> (Vec3, Vec3) {
        let mut min = (self.min - Vec3::splat(SHRINK_MARGIN)).max(-half);
        min.y = -half.y;
        let mut max = (self.max + Vec3::splat(SHRINK_MARGIN)).min(half);
        max.y = half.y;

        (min, max.max(min))
    }
}

// What the `lever:` line reports of one lever since the last one
#[derive(Default)]
struct LeverLog {
    frames: u32,
    open_frames: u32,
    opens: u32,
    // From the hand leaving to the close (display time, ms), and how many closed by the 1 s limit
    close_delays_ms: Vec<f32>,
    timeouts: u32,
    // Nearest hand (meters) over the frames
    nearest: Option<f32>,
    // The learned volume was forgotten: the box changed
    box_changed: bool,
}

#[derive(Default)]
struct LeverState {
    // The box the volume was learned in (area frame). Another box forgets it
    area_box: Option<AreaBox>,
    learned: Option<LearnedVolume>,
    open: bool,
    // Display time of the first frame without a hand near, while open
    left_ns: Option<i64>,
    // Display time of the close, for the fade
    closed_ns: Option<i64>,
    // U6: the last change of a PC input bound to this lever (XrTime)
    channel_ns: Option<i64>,
    log: LeverLog,
}

impl LeverState {
    // The gate, the late close and the fade from `fade_start`: how much the window shows (0..1)
    fn openness(
        &mut self,
        near: bool,
        display_ns: i64,
        depth_capture_ns: Option<i64>,
        close_ns: i64,
        fade_start: f32,
    ) -> f32 {
        if near {
            if !self.open {
                self.log.opens += 1;
            }
            self.open = true;
            self.left_ns = None;
            self.closed_ns = None;
            return 1.0;
        }
        if self.open {
            let left_ns = *self.left_ns.get_or_insert(display_ns);
            let caught_up =
                depth_capture_ns.is_some_and(|capture_ns| capture_ns >= left_ns + close_ns);
            let timed_out = display_ns >= left_ns + close_ns + MAX_DEPTH_WAIT_NS;
            if !caught_up && !timed_out {
                return 1.0;
            }
            self.open = false;
            self.left_ns = None;
            self.closed_ns = Some(display_ns);
            self.log
                .close_delays_ms
                .push((display_ns - left_ns) as f32 * 1e-6);
            if !caught_up {
                self.log.timeouts += 1;
            }
        }
        if let Some(closed_ns) = self.closed_ns {
            let fade = (display_ns - closed_ns) as f32 / FADE_NS as f32;
            if (0.0..1.0).contains(&fade) {
                return fade_start * (1.0 - fade);
            }
            self.closed_ns = None;
        }

        0.0
    }

    fn learn(&mut self, area_box: &AreaBox, palm: Vec3) {
        let local = area_box.to_local(palm);
        if !local.is_finite() || local.abs().cmpgt(area_box.half).any() {
            return;
        }
        self.learned = Some(match self.learned {
            Some(volume) => LearnedVolume {
                min: volume.min.min(local),
                max: volume.max.max(local),
            },
            None => LearnedVolume {
                min: local,
                max: local,
            },
        });
    }

    // Leaves the window shut (the box is no lever now, or no area is placed). The learned volume stays
    fn shut(&mut self) {
        self.open = false;
        self.left_ns = None;
        self.closed_ns = None;
    }

    // Whether a bound PC input changed recently enough to hold the gate (U6)
    fn channel_near(&self, display_ns: i64) -> bool {
        self.channel_ns.is_some_and(|time_ns| {
            (-CHANNEL_AHEAD_NS..=CHANNEL_HOLD_NS).contains(&(display_ns - time_ns))
        })
    }
}

/// The lever windows of the session
#[derive(Default)]
pub struct LeverWindows {
    states: [LeverState; MAX_AREA_BOXES],
    last_forget: Option<i64>,
}

impl LeverWindows {
    /// The windows of this frame, the open ones first. `area`: the play area placed from the marker
    /// (None: no window). `hands`: area frame, at the display time. `depth_capture_ns`: the capture time
    /// of the depth frame this frame shows (XrTime, None: unknown). `fade_start`: the coverage the fade
    /// after a close starts from, where the mask edge is full (`alvr_graphics::mask_edge_top`)
    pub fn update(
        &mut self,
        params: &LeverParams,
        area: Option<&PlacedArea>,
        hands: &[Option<LeverHand>; 2],
        display_ns: i64,
        depth_capture_ns: Option<i64>,
        fade_start: f32,
    ) -> [Option<LeverWindow>; MAX_LEVER_WINDOWS] {
        let mut windows = [None; MAX_LEVER_WINDOWS];
        if self.last_forget.is_some_and(|last| last != params.forget) {
            for state in &mut self.states {
                state.learned = None;
            }
        }
        self.last_forget = Some(params.forget);

        for (index, state) in self.states.iter_mut().enumerate() {
            let area_box = area.and_then(|area| area.area.boxes().get(index).copied());
            let (Some(area), Some(area_box)) =
                (area, area_box.filter(|_| params.boxes.contains(&index)))
            else {
                state.shut();
                continue;
            };

            if state.area_box != Some(area_box) {
                if state.learned.is_some() {
                    state.log.box_changed = true;
                }
                state.learned = None;
                state.area_box = Some(area_box);
            }

            let nearest = hands
                .iter()
                .flatten()
                .filter_map(|hand| hand.distance(&area_box))
                .reduce(f32::min);
            let hand_near = nearest.is_some_and(|distance| distance <= params.gate());
            let channel_near = state.channel_near(display_ns);
            if hand_near {
                for palm in hands
                    .iter()
                    .flatten()
                    .filter_map(|hand| hand.joints[PALM_JOINT])
                {
                    state.learn(&area_box, palm);
                }
            }
            let open = state.openness(
                hand_near || channel_near,
                display_ns,
                depth_capture_ns,
                params.close_ns(),
                fade_start,
            );

            state.log.frames += 1;
            if state.open {
                state.log.open_frames += 1;
            }
            if let Some(distance) = nearest {
                state.log.nearest = Some(state.log.nearest.map_or(distance, |n| n.min(distance)));
            }

            if open > 0.0
                && let Some(slot) = windows.iter_mut().find(|slot| slot.is_none())
            {
                *slot = Some(LeverWindow {
                    region: window_region(area, index, state.learned.filter(|_| params.shrink)),
                    open,
                });
            }
        }

        windows
    }

    /// The properties changed (switched off, other boxes): every window shut at once, no fade, and the
    /// counts of the `lever:` line start again. Without this a window open when switched off would show
    /// again, with no hand, when switched on. The learned volumes stay
    pub fn shut_all(&mut self) {
        for state in &mut self.states {
            state.shut();
            state.log = LeverLog::default();
        }
    }

    /// U6 hook: a PC input bound to the lever of box `index` changed at `time_ns` (XrTime). It keeps the
    /// gate open for `CHANNEL_HOLD_NS`, also without a tracked hand (a change stamped more than
    /// `CHANNEL_AHEAD_NS` after the display time does not). The binding (three changes with a
    /// hand inside, see section 4.3) is not built: nothing calls this yet
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the hook for the PC input channels of U6")
    )]
    pub fn channel_changed(&mut self, index: usize, time_ns: i64) {
        if let Some(state) = self.states.get_mut(index) {
            state.channel_ns = Some(state.channel_ns.map_or(time_ns, |last| last.max(time_ns)));
        }
    }

    /// The learned volume of box `index` (frame of the box), if any
    #[cfg(test)]
    pub fn learned(&self, index: usize) -> Option<LearnedVolume> {
        self.states.get(index).and_then(|state| state.learned)
    }

    /// The `lever:` line since the last one (and empties its counts). `area`: as for `update`
    pub fn log_text(&mut self, params: &LeverParams, area: Option<&PlacedArea>) -> String {
        let Some(area) = area else {
            return "lever: no play area placed (the marker's boxes), no window".to_string();
        };
        let mut parts = Vec::new();
        for &index in &params.boxes {
            let Some(area_box) = area.area.boxes().get(index) else {
                parts.push(format!("#{index}: no such box ({} boxes)", area.area.len()));
                continue;
            };
            let state = &mut self.states[index];
            let log = std::mem::take(&mut state.log);
            let mut text = format!(
                "#{index}: open {:.0} % of {} frames, {} open{}",
                if log.frames > 0 {
                    100.0 * log.open_frames as f32 / log.frames as f32
                } else {
                    0.0
                },
                log.frames,
                log.opens,
                if log.opens == 1 { "" } else { "s" },
            );
            if !log.close_delays_ms.is_empty() {
                let mean =
                    log.close_delays_ms.iter().sum::<f32>() / log.close_delays_ms.len() as f32;
                let _ = write!(
                    text,
                    ", {} close{} {mean:.0} ms after the hand left",
                    log.close_delays_ms.len(),
                    if log.close_delays_ms.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                );
                if log.timeouts > 0 {
                    let _ = write!(text, " ({} by the 1 s limit: no newer depth)", log.timeouts);
                }
            }
            match log.nearest {
                Some(nearest) => {
                    let _ = write!(text, ", nearest hand {:.0} mm", nearest * 1000.0);
                }
                None => text.push_str(", no hand"),
            }
            if state.open {
                text.push_str(", open now");
            }
            let size = area_box.half * 2.0 * 100.0;
            match state.learned {
                Some(volume) => {
                    let (min, max) = volume.window(area_box.half);
                    let path = (volume.max - volume.min) * 100.0;
                    let window = (max - min) * 100.0;
                    let _ = write!(
                        text,
                        ", palm path {:.0} x {:.0} x {:.0} cm, window {:.0} x {:.0} x {:.0} of {:.0} x {:.0} x \
                         {:.0} cm ({})",
                        path.x,
                        path.y,
                        path.z,
                        window.x,
                        window.y,
                        window.z,
                        size.x,
                        size.y,
                        size.z,
                        if params.shrink {
                            "used"
                        } else {
                            "not used, lever_shrink 0"
                        }
                    );
                    let _ = write!(
                        text,
                        ", min {:.3} {:.3} {:.3} max {:.3} {:.3} {:.3} m",
                        volume.min.x,
                        volume.min.y,
                        volume.min.z,
                        volume.max.x,
                        volume.max.y,
                        volume.max.z
                    );
                }
                None => {
                    let _ = write!(
                        text,
                        ", nothing learned: the whole box {:.0} x {:.0} x {:.0} cm",
                        size.x, size.y, size.z
                    );
                }
            }
            if log.box_changed {
                text.push_str(", the box changed: learned volume forgotten");
            }
            parts.push(text);
        }

        format!("lever: {}", parts.join("; "))
    }
}

/// The window of box `index`: the whole box, or the learned volume's window inside it (no domes)
fn window_region(
    area: &PlacedArea,
    index: usize,
    learned: Option<LearnedVolume>,
) -> RegionOverride {
    let whole = area.box_region(index);
    let Some(volume) = learned else {
        return whole;
    };
    let (min, max) = volume.window(whole.half_extents);

    RegionOverride {
        volume_to_world: whole.volume_to_world * Mat4::from_translation((min + max) * 0.5),
        half_extents: (max - min) * 0.5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_common::glam::Quat;
    use alvr_graphics::PlayArea;

    const MS: i64 = 1_000_000;
    // Today's edge (`debug.alvr.edge_cut` unset)
    const FADE_START: f32 = 0.35;

    impl LeverWindows {
        // `update` with today's edge
        fn step(
            &mut self,
            params: &LeverParams,
            area: Option<&PlacedArea>,
            hands: &[Option<LeverHand>; 2],
            display_ns: i64,
            depth_capture_ns: Option<i64>,
        ) -> [Option<LeverWindow>; MAX_LEVER_WINDOWS] {
            self.update(
                params,
                area,
                hands,
                display_ns,
                depth_capture_ns,
                FADE_START,
            )
        }
    }

    // Two boxes: #0 the cockpit, #1 a shifter box 20 x 30 x 20 cm, turned by 20 degrees, its centre 25
    // cm up and 40 cm to the right of the code. The code 1 m ahead of the stage origin, turned by 90
    fn area() -> PlacedArea {
        let mut area = PlayArea::single(AreaBox::new(Vec3::new(0.0, 0.3, 0.0), Vec3::splat(0.5)));
        let mut shifter = AreaBox::new(Vec3::new(0.4, 0.25, 0.0), Vec3::new(0.1, 0.15, 0.1));
        shifter.yaw = 20_f32.to_radians();
        area.push_box(shifter).unwrap();
        PlacedArea {
            area_to_world: Mat4::from_rotation_translation(
                Quat::from_rotation_y(90_f32.to_radians()),
                Vec3::new(0.0, 0.0, -1.0),
            ),
            area,
        }
    }

    fn shifter() -> AreaBox {
        area().area.boxes()[1]
    }

    // A hand with all joints at one point of the frame of the shifter box
    fn hand_at(local: Vec3) -> Option<LeverHand> {
        Some(LeverHand {
            joints: [Some(shifter().to_area(local)); 26],
        })
    }

    fn params(boxes: &str) -> LeverParams {
        LeverParams::from_values(Some(boxes), None, None, None, None)
    }

    #[test]
    fn unset_is_off() {
        let unset = LeverParams::default();
        assert!(!unset.is_on());
        assert_eq!(unset.gate_mm, 130);
        assert_eq!(unset.close_ms, 300);
        assert!(!unset.shrink);
        assert_eq!(LeverParams::read(), unset);

        // Off: no window, whatever the hands do
        let mut levers = LeverWindows::default();
        let area = area();
        let hands = [hand_at(Vec3::ZERO), hand_at(Vec3::ZERO)];
        let windows = levers.step(&unset, Some(&area), &hands, 0, Some(0));
        assert_eq!(windows, [None; MAX_LEVER_WINDOWS]);
        assert!(levers.learned(1).is_none());
    }

    #[test]
    fn box_list_reads_indices() {
        assert_eq!(parse_box_list(""), (vec![], vec![]));
        assert_eq!(parse_box_list("1"), (vec![1], vec![]));
        assert_eq!(
            parse_box_list("#3, 1;1 x 9"),
            (vec![1, 3], vec!["x".into(), "9".into()])
        );
        let (boxes, ignored) = parse_box_list("7 6 5 4 3");
        assert_eq!(boxes, vec![3, 4, 5, 6]);
        assert_eq!(ignored.len(), 1);
        assert!(ignored[0].starts_with("#7"));

        let clamped = LeverParams::from_values(Some("2"), Some(-5), Some(99999), Some(1), Some(3));
        assert_eq!(clamped.gate_mm, 0);
        assert_eq!(clamped.close_ms, MAX_CLOSE_MS);
        assert!(clamped.shrink);
        assert!(clamped.text().contains("#2"));
    }

    // The gate opens within 130 mm of the box, the window is the whole box (turned with it, placed by the
    // code), and stays open until the depth shown was captured 300 ms after the hand left
    #[test]
    fn gate_and_late_close() {
        let params = params("1");
        let area = area();
        let mut levers = LeverWindows::default();
        let far = [hand_at(Vec3::new(0.0, 0.0, 0.24)), None];
        let near = [hand_at(Vec3::new(0.0, 0.0, 0.22)), None];

        assert_eq!(
            levers.step(&params, Some(&area), &far, 0, Some(0)),
            [None; 4]
        );
        let windows = levers.step(&params, Some(&area), &near, 10 * MS, Some(-100 * MS));
        let window = windows[0].unwrap();
        assert_eq!(window.open, 1.0);
        assert_eq!(window.region, area.box_region(1));
        assert!(windows[1..].iter().all(Option::is_none));

        // The hand leaves at 20 ms. Open while the depth shown was captured before 320 ms
        let mut time = 20 * MS;
        let mut capture = -80 * MS;
        while capture < 320 * MS {
            let windows = levers.step(&params, Some(&area), &far, time, Some(capture));
            assert_eq!(windows[0].unwrap().open, 1.0, "{time}");
            time += 14 * MS;
            capture += 14 * MS;
        }
        // Then the fade from the top of the mask edge, over 150 ms
        let windows = levers.step(&params, Some(&area), &far, time, Some(capture));
        assert_eq!(windows[0].unwrap().open, FADE_START);
        let windows = levers.step(&params, Some(&area), &far, time + 75 * MS, Some(capture));
        assert!((windows[0].unwrap().open - FADE_START * 0.5).abs() < 1e-6);
        // A hand back during the fade opens it fully at once
        let windows = levers.step(&params, Some(&area), &near, time + 100 * MS, None);
        assert_eq!(windows[0].unwrap().open, 1.0);
        // It leaves again at 110 ms: the late close counts from then
        let left = time + 110 * MS;
        let windows = levers.step(&params, Some(&area), &far, left, Some(left));
        assert_eq!(windows[0].unwrap().open, 1.0);
        let windows = levers.step(
            &params,
            Some(&area),
            &far,
            left + 400 * MS,
            Some(left + 300 * MS),
        );
        assert_eq!(windows[0].unwrap().open, FADE_START);
        let windows = levers.step(
            &params,
            Some(&area),
            &far,
            left + 550 * MS,
            Some(left + 450 * MS),
        );
        assert_eq!(windows, [None; 4]);

        let text = levers.log_text(&params, Some(&area));
        assert!(text.starts_with("lever: #1: open"), "{text}");
        assert!(text.contains("close"), "{text}");
    }

    // Without a depth frame captured after the hand left the window closes 1 s after the close time
    #[test]
    fn closes_without_new_depth() {
        let params = params("1");
        let area = area();
        let mut levers = LeverWindows::default();
        let near = [None, hand_at(Vec3::ZERO)];
        let gone = [None, None];
        levers.step(&params, Some(&area), &near, 0, Some(0));
        let left = 10 * MS;
        let limit = left + 300 * MS + MAX_DEPTH_WAIT_NS;
        let windows = levers.step(&params, Some(&area), &gone, left, Some(0));
        assert_eq!(windows[0].unwrap().open, 1.0);
        let windows = levers.step(&params, Some(&area), &gone, limit - MS, Some(0));
        assert_eq!(windows[0].unwrap().open, 1.0);
        let windows = levers.step(&params, Some(&area), &gone, limit, Some(0));
        assert_eq!(windows[0].unwrap().open, FADE_START);
        assert!(levers.log_text(&params, Some(&area)).contains("1 s limit"));
    }

    // The palm's path inside the box, plus 2 cm in x and z, within the box, the whole height of the box.
    // Used only with lever_shrink 1; forgotten when the box changes or lever_forget changes
    #[test]
    fn shrink_to_the_palm_path() {
        let area = area();
        let half = shifter().half;
        let mut levers = LeverWindows::default();
        let mut params = params("1");
        // Along an H: x -6..6 cm, z -5..5 cm, at a height of 10 cm. One point outside the box (left out)
        let path = [
            Vec3::new(0.0, 0.1, 0.0),
            Vec3::new(-0.06, 0.1, -0.05),
            Vec3::new(-0.06, 0.1, 0.05),
            Vec3::new(0.06, 0.1, 0.05),
            Vec3::new(0.0, 0.2, 0.0),
        ];
        for (step, local) in path.iter().enumerate() {
            levers.step(
                &params,
                Some(&area),
                &[hand_at(*local), None],
                step as i64 * 10 * MS,
                None,
            );
        }
        let learned = levers.learned(1).unwrap();
        assert!(
            (learned.min - Vec3::new(-0.06, 0.1, -0.05))
                .abs()
                .max_element()
                < 1e-5
        );
        assert!(
            (learned.max - Vec3::new(0.06, 0.1, 0.05))
                .abs()
                .max_element()
                < 1e-5
        );
        let (min, max) = learned.window(half);
        assert!((min - Vec3::new(-0.08, -0.15, -0.07)).abs().max_element() < 1e-5);
        // Up to the top of the box, not the palm's 10 cm + 2: the top of a held stick is above the palm
        assert!((max - Vec3::new(0.08, 0.15, 0.07)).abs().max_element() < 1e-5);

        // lever_shrink 0: the whole box. 1: the window, placed in the box's frame
        let at = |levers: &mut LeverWindows, params: &LeverParams| {
            levers.step(
                params,
                Some(&area),
                &[hand_at(Vec3::ZERO), None],
                100 * MS,
                None,
            )[0]
            .unwrap()
            .region
        };
        assert_eq!(at(&mut levers, &params), area.box_region(1));
        params.shrink = true;
        let region = at(&mut levers, &params);
        assert!(
            (region.half_extents - Vec3::new(0.08, 0.15, 0.07))
                .abs()
                .max_element()
                < 1e-5
        );
        let expected_center = area
            .area_to_world
            .transform_point3(shifter().to_area(Vec3::ZERO));
        assert!(
            (region.center() - expected_center).length() < 1e-5,
            "{}",
            region.center()
        );

        // Hands that pass near the box without entering it teach nothing (a fresh box)
        let mut other = LeverWindows::default();
        other.step(
            &params,
            Some(&area),
            &[hand_at(Vec3::new(0.0, 0.0, 0.15)), None],
            0,
            None,
        );
        assert!(other.learned(1).is_none());
        assert_eq!(
            other.step(
                &params,
                Some(&area),
                &[hand_at(Vec3::new(0.0, 0.0, 0.15)), None],
                MS,
                None
            )[0]
            .unwrap()
            .region,
            area.box_region(1)
        );

        // lever_forget: a new value forgets
        params.forget = 1;
        levers.step(&params, Some(&area), &[None, None], 200 * MS, None);
        assert!(levers.learned(1).is_none());

        // Moving the box (the editor) forgets
        levers.step(
            &params,
            Some(&area),
            &[hand_at(Vec3::ZERO), None],
            300 * MS,
            None,
        );
        assert!(levers.learned(1).is_some());
        let mut moved = area;
        moved.area.move_box(1, Vec3::new(0.0, 0.0, 0.05));
        levers.step(&params, Some(&moved), &[None, None], 310 * MS, None);
        assert!(levers.learned(1).is_none());
        assert!(
            levers
                .log_text(&params, Some(&moved))
                .contains("the box changed")
        );
    }

    // Only the boxes named, no area or no such box: no window; the open windows packed first
    #[test]
    fn windows_only_for_named_boxes() {
        let area = area();
        let mut levers = LeverWindows::default();
        let inside_both = [
            Some(LeverHand {
                joints: [Some(Vec3::new(0.0, 0.3, 0.0)); 26],
            }),
            hand_at(Vec3::ZERO),
        ];
        let windows = levers.step(&params("1"), Some(&area), &inside_both, 0, None);
        assert_eq!(windows[0].unwrap().region, area.box_region(1));
        assert!(windows[1].is_none());
        let windows = levers.step(&params("0 1"), Some(&area), &inside_both, MS, None);
        assert_eq!(windows[0].unwrap().region, area.box_region(0));
        assert_eq!(windows[1].unwrap().region, area.box_region(1));
        // Box 5 does not exist
        let windows = levers.step(&params("5"), Some(&area), &inside_both, 2 * MS, None);
        assert_eq!(windows, [None; 4]);
        assert!(
            levers
                .log_text(&params("5"), Some(&area))
                .contains("no such box")
        );
        // No area: none, and the open ones are shut (no fade later)
        assert_eq!(
            levers.step(&params("0 1"), None, &inside_both, 3 * MS, None),
            [None; 4]
        );
        assert_eq!(
            levers.step(&params("0 1"), Some(&area), &[None, None], 4 * MS, None),
            [None; 4]
        );
        assert!(levers.log_text(&params("1"), None).contains("no play area"));
    }

    // The fade starts where the mask edge is full for the edge cut in use: with edge_cut 50 at 0.65, so
    // that it is not below the cut (invisible, a pop)
    #[test]
    fn fade_follows_the_edge_cut() {
        let params = params("1");
        let area = area();
        let mut levers = LeverWindows::default();
        let top = alvr_graphics::mask_edge_top(alvr_graphics::edge_cut_centre(50));
        assert!((top - 0.65).abs() < 1e-6);
        assert_eq!(
            alvr_graphics::mask_edge_top(alvr_graphics::edge_cut_centre(20)),
            FADE_START
        );
        let near = [hand_at(Vec3::ZERO), None];
        levers.update(&params, Some(&area), &near, 0, Some(0), top);
        levers.update(&params, Some(&area), &[None, None], 10 * MS, Some(0), top);
        let windows = levers.update(
            &params,
            Some(&area),
            &[None, None],
            400 * MS,
            Some(310 * MS),
            top,
        );
        assert_eq!(windows[0].unwrap().open, top);
        let windows = levers.update(
            &params,
            Some(&area),
            &[None, None],
            475 * MS,
            Some(310 * MS),
            top,
        );
        assert!((windows[0].unwrap().open - top * 0.5).abs() < 1e-6);
    }

    // Switched off while open, then on again: no window without a hand, no fade, fresh counts. What was
    // learned stays
    #[test]
    fn shut_all_forgets_an_open_window() {
        let params = params("1");
        let area = area();
        let mut levers = LeverWindows::default();
        let windows = levers.step(&params, Some(&area), &[hand_at(Vec3::ZERO), None], 0, None);
        assert_eq!(windows[0].unwrap().open, 1.0);
        levers.shut_all();
        assert_eq!(
            levers.step(&params, Some(&area), &[None, None], 60_000 * MS, Some(0)),
            [None; 4]
        );
        assert!(levers.learned(1).is_some());
        let text = levers.log_text(&params, Some(&area));
        assert!(text.contains("open 0 % of 1 frames, 0 opens"), "{text}");
    }

    // U6 hook: a bound input keeps the gate open for 0.5 s without a hand
    #[test]
    fn channel_keeps_the_gate_open() {
        let area = area();
        let params = params("1");
        let mut levers = LeverWindows::default();
        levers.channel_changed(1, 0);
        let windows = levers.step(
            &params,
            Some(&area),
            &[None, None],
            400 * MS,
            Some(400 * MS),
        );
        assert_eq!(windows[0].unwrap().open, 1.0);
        // Then the late close as for a hand: open until the depth was captured 300 ms after
        let windows = levers.step(
            &params,
            Some(&area),
            &[None, None],
            600 * MS,
            Some(700 * MS),
        );
        assert_eq!(windows[0].unwrap().open, 1.0);
        let windows = levers.step(
            &params,
            Some(&area),
            &[None, None],
            950 * MS,
            Some(900 * MS),
        );
        assert_eq!(windows[0].unwrap().open, FADE_START);

        // A change stamped seconds after the display time (a wrong clock mapping) does not hold the gate;
        // one a few ms ahead does
        let mut levers = LeverWindows::default();
        levers.channel_changed(1, 5_000 * MS);
        assert_eq!(
            levers.step(&params, Some(&area), &[None, None], 0, Some(0)),
            [None; 4]
        );
        let mut levers = LeverWindows::default();
        levers.channel_changed(1, 20 * MS);
        let windows = levers.step(&params, Some(&area), &[None, None], 0, Some(0));
        assert_eq!(windows[0].unwrap().open, 1.0);
    }
}
