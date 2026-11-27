//! The green-screen key (`debug.alvr.green_boxes`, docs/green-screen.md). The rig PC shows a pure green
//! rectangle on its monitor behind the top of the steering wheel's rim. A play-area box drawn as a thin
//! slab on it, named by its index, is the zone:
//! - the depth mask shows the room along every view ray through the box, whatever the depth says, and
//!   hands nothing there to the colour key (alvr_graphics `DepthRenderInput::green_windows`): both game
//!   layers get a hole;
//! - the bottom passthrough layer is a layer of its own with a colour LUT that makes "monitor green"
//!   transparent (`PassthroughLayer::update_green_key`, the rule of `alvr_graphics::GreenKeyParams`);
//! - under it, at the very bottom of the stack, a projection layer shows the game (the same two
//!   swapchains, opaque), cropped to the zone's rectangle in each view (`alvr_graphics::zone_view_rect`).
//!
//! So where the camera sees the green, the game shows; where anything else is in front of it (rim,
//! hands, arms) the real picture shows, cut by the compositor's per-pixel colour key at display rate.
//! It needs the colour-key layers (the key's path of stream.wgsl keeps the game colour where the game
//! layers are transparent; the path without it premultiplies it away), Passthrough = Depth, a play area
//! placed from the marker, and not the YVR custom reprojection (crops are off there). Otherwise it is
//! off and the `green:` line says why. Unset (the default): nothing is read per frame, no layer, no
//! line, today's picture.

use crate::lever::parse_box_list;
use alvr_common::{
    ViewParams,
    glam::{Mat4, Vec3},
    parking_lot::Mutex,
};
use alvr_graphics::{
    GreenCameraStats, GreenKeyParams, MAX_GREEN_WINDOWS, PlacedArea, RegionOverride, ZoneRect,
};
use std::{
    fmt::Write,
    sync::Arc,
    time::{Duration, Instant},
};

/// The properties are read this often
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// The `green:` line is logged this often while `debug.alvr.green_boxes` is set, and at once on a change
pub const LOG_INTERVAL: Duration = Duration::from_secs(2);
/// What the camera saw is not reported if it is older
pub const CAMERA_STATS_MAX_AGE: Duration = Duration::from_secs(1);
/// `debug.alvr.green_pad`, thousandths of the view
pub const DEFAULT_PAD: i64 = 30;
pub const MAX_PAD: i64 = 300;

/// `debug.alvr.green_*`
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GreenParams {
    /// Play-area boxes of the zone (`#<index>` of the area editor's log, from 0), sorted, at most
    /// `MAX_GREEN_WINDOWS`. Empty: off
    pub boxes: Vec<usize>,
    /// Words of `debug.alvr.green_boxes` that are no box index, or boxes beyond the second
    pub ignored: Vec<String>,
    /// `green_floor`, `green_ratio`, `green_delta`, `green_soft`
    pub key: GreenKeyParams,
    /// `green_pad`: the under layer's crop grows by this many thousandths of the view each side (0..300,
    /// default 30), over the soft border of the mask
    pub pad: i64,
    /// `green_under`: 1 (default) the under layer cropped to the zone, 0 none (keyed green shows black),
    /// 2 uncropped
    pub under: i64,
}

impl Default for GreenParams {
    fn default() -> Self {
        Self::from_values(None, None, None, None, None, None, None)
    }
}

impl GreenParams {
    #[cfg(target_os = "android")]
    pub fn read() -> Self {
        use crate::passthrough::{debug_property, debug_property_string};

        Self::from_values(
            debug_property_string("debug.alvr.green_boxes").as_deref(),
            debug_property("debug.alvr.green_floor"),
            debug_property("debug.alvr.green_ratio"),
            debug_property("debug.alvr.green_delta"),
            debug_property("debug.alvr.green_soft"),
            debug_property("debug.alvr.green_pad"),
            debug_property("debug.alvr.green_under"),
        )
    }

    #[cfg(not(target_os = "android"))]
    pub fn read() -> Self {
        Self::default()
    }

    pub fn from_values(
        boxes: Option<&str>,
        floor: Option<i64>,
        ratio: Option<i64>,
        delta: Option<i64>,
        soft: Option<i64>,
        pad: Option<i64>,
        under: Option<i64>,
    ) -> Self {
        let (mut boxes, mut ignored) = parse_box_list(boxes.unwrap_or(""));
        if boxes.len() > MAX_GREEN_WINDOWS {
            ignored.extend(
                boxes
                    .drain(MAX_GREEN_WINDOWS..)
                    .map(|index| format!("#{index} (more than {MAX_GREEN_WINDOWS} boxes)")),
            );
        }
        Self {
            boxes,
            ignored,
            key: GreenKeyParams::from_values(floor, ratio, delta, soft),
            pad: pad.unwrap_or(DEFAULT_PAD).clamp(0, MAX_PAD),
            under: under.unwrap_or(1).clamp(0, 2),
        }
    }

    /// Whether `debug.alvr.green_boxes` names a box
    pub fn is_on(&self) -> bool {
        !self.boxes.is_empty()
    }

    /// `green_pad` in view uv
    pub fn pad_uv(&self) -> f32 {
        self.pad as f32 / 1000.0
    }

    // The settings part of the `green:` line
    fn text(&self) -> String {
        let mut text = format!(
            "key {}, pad {:.3}, under {}",
            self.key.text(),
            self.pad_uv(),
            match self.under {
                0 => "0 (none: keyed green shows black)",
                2 => "2 (uncropped)",
                _ => "1 (cropped to the zone)",
            }
        );
        if !self.ignored.is_empty() {
            let _ = write!(text, ", ignored: {}", self.ignored.join(" "));
        }
        text
    }
}

/// Why the zone is off while `debug.alvr.green_boxes` is set
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GreenOff {
    NotDepth,
    CustomReprojection,
    NoColorKey,
    NoArea,
    /// None of the boxes exists; the play area has this many
    NoBox(usize),
}

impl GreenOff {
    fn text(self) -> String {
        match self {
            Self::NotDepth => "Passthrough is not Depth".into(),
            Self::CustomReprojection => {
                "the custom reprojection path (YVR) does not crop layers".into()
            }
            Self::NoColorKey => {
                "the colour-key layers are not in use (debug.alvr.color_key and its \
                                 LUT file, XR_META_passthrough_color_lut, \
                                 XR_KHR_composition_layer_color_scale_bias, depth passthrough)"
                    .into()
            }
            Self::NoArea => "no play area placed (the marker's boxes)".into(),
            Self::NoBox(count) => format!("none of the boxes exists ({count} boxes)"),
        }
    }
}

/// What the stream is like in this frame
#[derive(Clone, Copy, Debug)]
pub struct GreenConditions {
    /// Passthrough = Depth
    pub depth: bool,
    /// The colour-key layers are in use
    pub color_key: bool,
    /// The YVR path
    pub custom_reprojection: bool,
}

/// The zone's boxes in stage space, packed at the start
pub type GreenWindows = [Option<RegionOverride>; MAX_GREEN_WINDOWS];
/// The play-area index of each of them
pub type GreenIndices = [Option<usize>; MAX_GREEN_WINDOWS];

/// The zone's boxes and the indices they came from. Err: why it is off. A missing index is left out
/// (logged); none left: off
pub fn resolve_zone(
    params: &GreenParams,
    conditions: GreenConditions,
    area: Option<&PlacedArea>,
) -> Result<(GreenWindows, GreenIndices), GreenOff> {
    if !conditions.depth {
        return Err(GreenOff::NotDepth);
    }
    if conditions.custom_reprojection {
        return Err(GreenOff::CustomReprojection);
    }
    if !conditions.color_key {
        return Err(GreenOff::NoColorKey);
    }
    let area = area.ok_or(GreenOff::NoArea)?;
    let mut windows = [None; MAX_GREEN_WINDOWS];
    let mut indices = [None; MAX_GREEN_WINDOWS];
    let mut count = 0;
    for &index in params.boxes.iter().take(MAX_GREEN_WINDOWS) {
        if index < area.area.len() {
            windows[count] = Some(area.box_region(index));
            indices[count] = Some(index);
            count += 1;
        }
    }
    if count == 0 {
        return Err(GreenOff::NoBox(area.area.len()));
    }

    Ok((windows, indices))
}

/// The zone as the camera learner gets it (stage space)
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GreenCameraZone {
    boxes: [RegionOverride; MAX_GREEN_WINDOWS],
    count: usize,
    pub key: GreenKeyParams,
}

impl GreenCameraZone {
    pub fn new(windows: &[Option<RegionOverride>; MAX_GREEN_WINDOWS], key: GreenKeyParams) -> Self {
        let mut boxes = [RegionOverride {
            volume_to_world: Mat4::IDENTITY,
            half_extents: Vec3::ZERO,
        }; MAX_GREEN_WINDOWS];
        let mut count = 0;
        for window in windows.iter().flatten() {
            boxes[count] = *window;
            count += 1;
        }
        Self { boxes, count, key }
    }

    #[cfg_attr(all(not(target_os = "android"), not(test)), expect(dead_code))]
    pub fn boxes(&self) -> &[RegionOverride] {
        &self.boxes[..self.count]
    }
}

/// Shared with the camera learner (auto_color_key.rs): the zone (written by the render thread while it
/// is on, None otherwise) and what the camera saw through it at its last pass
#[derive(Default)]
pub struct GreenCameraShared {
    #[cfg_attr(all(not(target_os = "android"), not(test)), expect(dead_code))]
    pub zone: Option<GreenCameraZone>,
    pub stats: Option<(Instant, GreenCameraStats)>,
}

pub type SharedGreenCamera = Arc<Mutex<GreenCameraShared>>;

/// The layers of a frame, from the bottom (lib.rs)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    /// The opaque game layer under the green-keyed passthrough (`debug.alvr.green_boxes`)
    GreenUnder,
    /// The base passthrough: the green-keyed one while the zone is on, else the plain one
    Passthrough,
    /// The colour key's game layer below the keyed passthrough
    Lower,
    /// The colour-keyed passthrough
    ColorKey,
    /// The colour key's game layer above it
    Upper,
    /// The game layer without the colour key
    Plain,
}

pub const MAX_LAYERS: usize = 5;

/// The layer kinds of a frame, from the bottom
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayerOrder {
    kinds: [LayerKind; MAX_LAYERS],
    len: usize,
}

impl LayerOrder {
    fn new(kinds: &[LayerKind]) -> Self {
        let mut order = Self {
            kinds: [LayerKind::Plain; MAX_LAYERS],
            len: kinds.len().min(MAX_LAYERS),
        };
        order.kinds[..order.len].copy_from_slice(&kinds[..order.len]);
        order
    }

    // The under layer at the bottom
    fn under(kinds: &[LayerKind]) -> Self {
        let mut order = Self::new(&[LayerKind::GreenUnder]);
        let len = kinds.len().min(MAX_LAYERS - 1);
        order.kinds[1..=len].copy_from_slice(&kinds[..len]);
        order.len = len + 1;
        order
    }

    pub fn as_slice(&self) -> &[LayerKind] {
        &self.kinds[..self.len]
    }
}

/// The colour key's layers in a frame (lib.rs): `debug.alvr.key_layers`, `debug.alvr.key_crop`, and
/// whether the extra layers are used (`ColorKeyParams::extra_layers`)
#[derive(Clone, Copy, Debug)]
pub struct KeyLayout {
    pub layers: i64,
    pub crop: i64,
    pub extra: bool,
}

/// The layers of a frame: as before the green-screen key, with its under layer first in every branch of
/// the colour key when `green_under`. `passthrough`: a passthrough layer exists; `key`: the colour key's
/// layers are used (with its keyed layer)
pub fn layer_order(passthrough: bool, key: Option<KeyLayout>, green_under: bool) -> LayerOrder {
    use LayerKind::*;

    if !passthrough {
        return LayerOrder::new(&[Plain]);
    }
    let Some(key) = key else {
        return LayerOrder::new(&[Passthrough, Plain]);
    };
    // Without the extra layers, look like the four layers away from the hands: there only the upper one
    // is visible if the lower one is cropped (the shader writes the alpha for this)
    let single = if key.crop > 0 { Upper } else { Lower };
    let order: &[LayerKind] = if !key.extra {
        &[Passthrough, single]
    } else {
        match key.layers {
            30 => &[Passthrough, Lower, ColorKey],
            31 => &[Passthrough, Lower, Upper],
            _ => &[Passthrough, Lower, ColorKey, Upper],
        }
    };
    if green_under {
        LayerOrder::under(order)
    } else {
        LayerOrder::new(order)
    }
}

/// The zone in one frame
#[derive(Clone, Copy, Debug)]
pub struct GreenFrame {
    /// The boxes in stage space, packed at the start (for the depth mask), or why it is off
    pub state: Result<GreenWindows, GreenOff>,
    /// The box index of each of them
    pub indices: GreenIndices,
    /// The rule of the green-keyed base layer while the zone is on
    pub key: Option<GreenKeyParams>,
    /// Where the zone is in each view (the game layers' pose and fov); Outside while off
    pub rects: [ZoneRect; 2],
    /// The rectangle of the under layer in each view, None: no under layer (off, `green_under 0`, or
    /// the zone in neither view)
    pub under: Option<[ZoneRect; 2]>,
    /// `green_pad` in view uv
    pub pad: f32,
}

/// What lib.rs submitted in the frame before, for the `green:` line
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GreenLayers {
    pub base_keyed: bool,
    pub under_submitted: bool,
}

// What a `green:` line said: on or why off, the boxes used, the layers submitted
type LoggedState = (Result<(), GreenOff>, GreenIndices, GreenLayers);

/// The green-screen key of a stream: the properties, the camera learner's share, the `green:` line
pub struct GreenScreen {
    params: GreenParams,
    last_poll: Option<Instant>,
    params_changed: bool,
    last_log: Instant,
    // What the last line said of the state, another one is logged at once
    logged: Option<LoggedState>,
    camera: SharedGreenCamera,
    camera_zone: Option<GreenCameraZone>,
    layers: GreenLayers,
}

impl Default for GreenScreen {
    fn default() -> Self {
        Self {
            params: GreenParams::default(),
            last_poll: None,
            params_changed: false,
            last_log: Instant::now(),
            logged: None,
            camera: SharedGreenCamera::default(),
            camera_zone: None,
            layers: GreenLayers::default(),
        }
    }
}

impl GreenScreen {
    /// For the camera learner
    #[cfg_attr(not(target_os = "android"), expect(dead_code))]
    pub fn camera(&self) -> SharedGreenCamera {
        Arc::clone(&self.camera)
    }

    /// The properties, every `POLL_INTERVAL`
    pub fn poll(&mut self) {
        if self
            .last_poll
            .is_some_and(|time| time.elapsed() < POLL_INTERVAL)
        {
            return;
        }
        self.last_poll = Some(Instant::now());
        let params = GreenParams::read();
        if params != self.params {
            self.params = params;
            self.params_changed = true;
        }
    }

    /// What lib.rs submitted in the last frame
    pub fn report_layers(&mut self, layers: GreenLayers) {
        self.layers = layers;
    }

    /// The zone of this frame, None while `debug.alvr.green_boxes` is unset. `views`: the pose and fov
    /// the game layers are submitted with. Gives the zone to the camera learner when it changes
    pub fn frame(
        &mut self,
        conditions: GreenConditions,
        area: Option<&PlacedArea>,
        views: &[ViewParams; 2],
    ) -> Option<GreenFrame> {
        if !self.params.is_on() {
            if self.camera_zone.take().is_some() {
                *self.camera.lock() = GreenCameraShared::default();
            }
            if self.params_changed {
                self.params_changed = false;
                self.logged = None;
                crate::stream_log("green: off (no box in debug.alvr.green_boxes)");
            }
            return None;
        }
        let resolved = resolve_zone(&self.params, conditions, area);
        let (state, indices) = match resolved {
            Ok((windows, indices)) => (Ok(windows), indices),
            Err(off) => (Err(off), [None; MAX_GREEN_WINDOWS]),
        };
        let mut rects = [ZoneRect::Outside; 2];
        let mut under = None;
        if let Ok(windows) = &state {
            let mut boxes = [RegionOverride {
                volume_to_world: Mat4::IDENTITY,
                half_extents: Vec3::ZERO,
            }; MAX_GREEN_WINDOWS];
            let mut count = 0;
            for window in windows.iter().flatten() {
                boxes[count] = *window;
                count += 1;
            }
            rects = views
                .each_ref()
                .map(|view| alvr_graphics::zone_view_rect(view, &boxes[..count]));
            under = match self.params.under {
                0 => None,
                2 => Some([ZoneRect::Full; 2]),
                _ => rects.iter().any(ZoneRect::is_visible).then_some(rects),
            };
        }

        let camera_zone = state
            .ok()
            .map(|windows| GreenCameraZone::new(&windows, self.params.key));
        if camera_zone != self.camera_zone {
            self.camera_zone = camera_zone;
            *self.camera.lock() = GreenCameraShared {
                zone: camera_zone,
                stats: None,
            };
        }

        Some(GreenFrame {
            state,
            indices,
            key: state.ok().map(|_| self.params.key),
            rects,
            under,
            pad: self.params.pad_uv(),
        })
    }

    /// The `green:` line every `LOG_INTERVAL`, and at once when the properties, the state, the boxes
    /// used or the layers submitted change. `learner`: whether the camera learner runs
    pub fn log(&mut self, frame: &GreenFrame, learner: bool, area: Option<&PlacedArea>) {
        let summary = (frame.state.map(|_| ()), frame.indices, self.layers);
        let changed = self.params_changed || self.logged != Some(summary);
        if !changed && self.last_log.elapsed() < LOG_INTERVAL {
            return;
        }
        self.params_changed = false;
        self.logged = Some(summary);
        self.last_log = Instant::now();
        let stats = self.camera.lock().stats;
        // The footer of the tuning menu (tuning_mode.rs): the share of the zone's pixels that are green
        if let Some((time, stats)) = stats
            && time.elapsed() <= CAMERA_STATS_MAX_AGE
            && stats.in_image
            && stats.pixels > 0
        {
            crate::tuning_mode::publish_green_share(stats.green as f32 / stats.pixels as f32);
        }
        crate::stream_log(&log_text(
            &self.params,
            frame,
            self.layers,
            area,
            learner,
            stats.map(|(time, stats)| (time.elapsed(), stats)),
        ));
    }
}

/// The `green:` line. `stats`: what the camera saw at its last pass and how long ago
pub fn log_text(
    params: &GreenParams,
    frame: &GreenFrame,
    layers: GreenLayers,
    area: Option<&PlacedArea>,
    learner: bool,
    stats: Option<(Duration, GreenCameraStats)>,
) -> String {
    let mut text = match frame.state {
        Ok(_) => "green: on".to_string(),
        Err(off) => format!("green: off ({})", off.text()),
    };
    let boxes = params
        .boxes
        .iter()
        .map(|&index| match area {
            Some(area) if index < area.area.len() => area.area.describe_box(index),
            Some(area) => format!("#{index} (no such box, {} boxes)", area.area.len()),
            None => format!("#{index}"),
        })
        .collect::<Vec<_>>()
        .join("; ");
    let _ = write!(text, ", boxes {boxes}; {}", params.text());
    if frame.state.is_err() {
        return text;
    }
    let _ = write!(
        text,
        "; view L {}, R {}; under layer {}; base layer {}",
        frame.rects[0].text(),
        frame.rects[1].text(),
        match (layers.under_submitted, frame.under.is_some(), params.under) {
            (true, _, 2) => "submitted (uncropped)",
            (true, _, _) => "submitted",
            (false, _, 0) => "off (green_under 0)",
            (false, false, _) => "not submitted (the zone is in neither view)",
            (false, true, _) => "not submitted (the base layer is not keyed)",
        },
        if layers.base_keyed {
            "green-keyed"
        } else {
            "plain (not keyed yet, or its LUT failed)"
        }
    );
    let camera = match (learner, stats) {
        (false, _) => {
            "the learner is not running (debug.alvr.auto_key 1 before the app starts)".to_string()
        }
        (true, Some((age, stats))) if age <= CAMERA_STATS_MAX_AGE => stats.text(),
        (true, _) => format!(
            "no pass with the zone in the last {:.0} s",
            CAMERA_STATS_MAX_AGE.as_secs_f32()
        ),
    };
    let _ = write!(text, "; camera: {camera}");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use LayerKind::*;
    use alvr_common::{
        Fov, Pose,
        glam::{Quat, Vec2},
    };
    use alvr_graphics::{AreaBox, PlayArea};

    #[test]
    fn params_defaults_parse_and_clamps() {
        let off = GreenParams::default();
        assert!(!off.is_on());
        assert_eq!(off.pad, 30);
        assert_eq!(off.under, 1);
        assert_eq!(off.key, GreenKeyParams::default());

        let params = GreenParams::from_values(
            Some("#3, 1 x 9 1"),
            None,
            None,
            None,
            None,
            Some(900),
            Some(7),
        );
        assert_eq!(params.boxes, vec![1, 3]);
        assert_eq!(params.ignored, vec!["x".to_string(), "9".to_string()]);
        assert_eq!((params.pad, params.under), (300, 2));
        assert!((params.pad_uv() - 0.3).abs() < 1e-6);

        // At most two boxes
        let params =
            GreenParams::from_values(Some("4,2,0"), None, None, None, None, Some(-1), Some(-1));
        assert_eq!(params.boxes, vec![0, 2]);
        assert_eq!(params.ignored, vec!["#4 (more than 2 boxes)".to_string()]);
        assert_eq!((params.pad, params.under), (0, 0));
        assert!(
            params
                .text()
                .starts_with("key floor 70 ratio 1.35 delta 25 soft 24, pad 0.000, under 0")
        );
        assert!(
            GreenParams::from_values(Some(""), None, None, None, None, None, None)
                .boxes
                .is_empty()
        );
    }

    fn area(count: usize) -> PlacedArea {
        let mut area = PlayArea::single(AreaBox::new(
            Vec3::new(0.0, 0.5, 0.0),
            Vec3::new(0.3, 0.3, 0.3),
        ));
        for i in 1..count {
            area.push_box(AreaBox::new(
                Vec3::new(0.0, 0.5 + 0.1 * i as f32, -0.5),
                Vec3::new(0.2, 0.1, 0.03),
            ));
        }
        PlacedArea {
            area_to_world: Mat4::from_translation(Vec3::new(0.0, 0.0, -1.0)),
            area,
        }
    }

    const ON: GreenConditions = GreenConditions {
        depth: true,
        color_key: true,
        custom_reprojection: false,
    };

    // Every reason to be off, in order; a missing index (a box removed in the editor) is left out, never
    // a panic or a stale box
    #[test]
    fn zone_resolves_or_says_why_not() {
        let params = GreenParams::from_values(Some("2,5"), None, None, None, None, None, None);
        let area = area(3);
        let resolve =
            |conditions, area: Option<&PlacedArea>| resolve_zone(&params, conditions, area);
        assert_eq!(
            resolve(GreenConditions { depth: false, ..ON }, Some(&area)),
            Err(GreenOff::NotDepth)
        );
        assert_eq!(
            resolve(
                GreenConditions {
                    custom_reprojection: true,
                    ..ON
                },
                Some(&area)
            ),
            Err(GreenOff::CustomReprojection)
        );
        assert_eq!(
            resolve(
                GreenConditions {
                    color_key: false,
                    ..ON
                },
                Some(&area)
            ),
            Err(GreenOff::NoColorKey)
        );
        assert_eq!(resolve(ON, None), Err(GreenOff::NoArea));
        assert_eq!(resolve(ON, Some(&self::area(2))), Err(GreenOff::NoBox(2)));

        let (windows, indices) = resolve(ON, Some(&area)).unwrap();
        assert_eq!(indices, [Some(2), None]);
        assert_eq!(windows, [Some(area.box_region(2)), None]);
        assert!((windows[0].unwrap().center() - Vec3::new(0.0, 0.7, -1.5)).length() < 1e-6);
    }

    fn views() -> [ViewParams; 2] {
        [-0.032, 0.032].map(|x| ViewParams {
            pose: Pose {
                orientation: Quat::IDENTITY,
                position: Vec3::new(x, 0.6, 0.0),
            },
            fov: Fov {
                left: -0.9,
                right: 0.8,
                up: 0.8,
                down: -0.9,
            },
        })
    }

    #[test]
    fn frame_of_each_under_mode() {
        let area = area(3);
        let mut green = GreenScreen::default();
        // Unset: nothing
        assert!(green.frame(ON, Some(&area), &views()).is_none());

        green.params = GreenParams::from_values(Some("2"), None, None, None, None, None, None);
        let frame = green.frame(ON, Some(&area), &views()).unwrap();
        assert!(frame.state.is_ok());
        assert_eq!(frame.key, Some(GreenKeyParams::default()));
        assert!(matches!(frame.rects[0], ZoneRect::Part(..)));
        assert_eq!(frame.under, Some(frame.rects));
        assert!((frame.pad - 0.03).abs() < 1e-6);
        // The learner gets the zone
        let zone = green.camera.lock().zone.unwrap();
        assert_eq!(zone.boxes(), &[area.box_region(2)]);

        // Looking away (the box beside both views): no under layer, the base still keyed
        let away = views().map(|view| ViewParams {
            pose: Pose {
                orientation: Quat::from_rotation_y(60_f32.to_radians()),
                ..view.pose
            },
            ..view
        });
        let frame = green.frame(ON, Some(&area), &away).unwrap();
        assert_eq!(frame.under, None);
        assert!(frame.key.is_some());

        green.params.under = 2;
        let frame = green.frame(ON, Some(&area), &away).unwrap();
        assert_eq!(frame.under, Some([ZoneRect::Full; 2]));
        green.params.under = 0;
        assert_eq!(green.frame(ON, Some(&area), &views()).unwrap().under, None);

        // Off: no key, no under layer, the learner's zone emptied
        let frame = green
            .frame(
                GreenConditions {
                    color_key: false,
                    ..ON
                },
                Some(&area),
                &views(),
            )
            .unwrap();
        assert_eq!(frame.state, Err(GreenOff::NoColorKey));
        assert_eq!((frame.key, frame.under), (None, None));
        assert!(green.camera.lock().zone.is_none());
    }

    #[test]
    fn layer_order_of_every_branch() {
        let order =
            |passthrough, key, under| layer_order(passthrough, key, under).as_slice().to_vec();
        let key = |layers, crop, extra| {
            Some(KeyLayout {
                layers,
                crop,
                extra,
            })
        };
        // As before the green screen
        assert_eq!(order(false, None, false), [Plain]);
        assert_eq!(order(true, None, false), [Passthrough, Plain]);
        assert_eq!(order(true, key(4, 1, false), false), [Passthrough, Upper]);
        assert_eq!(order(true, key(2, 0, false), false), [Passthrough, Lower]);
        assert_eq!(
            order(true, key(4, 1, true), false),
            [Passthrough, Lower, ColorKey, Upper]
        );
        assert_eq!(
            order(true, key(5, 0, true), false),
            [Passthrough, Lower, ColorKey, Upper]
        );
        assert_eq!(
            order(true, key(30, 0, true), false),
            [Passthrough, Lower, ColorKey]
        );
        assert_eq!(
            order(true, key(31, 1, true), false),
            [Passthrough, Lower, Upper]
        );
        // The under layer first in each branch of the key
        assert_eq!(
            order(true, key(4, 1, false), true),
            [GreenUnder, Passthrough, Upper]
        );
        assert_eq!(
            order(true, key(2, 0, false), true),
            [GreenUnder, Passthrough, Lower]
        );
        assert_eq!(
            order(true, key(5, 0, true), true),
            [GreenUnder, Passthrough, Lower, ColorKey, Upper]
        );
        assert_eq!(
            order(true, key(30, 0, true), true),
            [GreenUnder, Passthrough, Lower, ColorKey]
        );
        assert_eq!(
            order(true, key(31, 0, true), true),
            [GreenUnder, Passthrough, Lower, Upper]
        );
        // Never without the key
        assert_eq!(order(true, None, true), [Passthrough, Plain]);
        assert_eq!(order(false, None, true), [Plain]);
    }

    #[test]
    fn log_line() {
        let area = area(3);
        let params = GreenParams::from_values(Some("2,6"), None, None, None, None, None, None);
        let frame = GreenFrame {
            state: Ok([Some(area.box_region(2)), None]),
            indices: [Some(2), None],
            key: Some(params.key),
            rects: [
                ZoneRect::Part(Vec2::new(0.41, 0.22), Vec2::new(0.63, 0.35)),
                ZoneRect::Outside,
            ],
            under: None,
            pad: 0.03,
        };
        let layers = GreenLayers {
            base_keyed: true,
            under_submitted: true,
        };
        let stats = GreenCameraStats {
            in_image: true,
            pixels: 200,
            green: 150,
            between: 10,
            green_rgb: Some([[30, 40, 50], [190, 200, 220], [40, 50, 60]]),
            rest_rgb: None,
        };
        let text = log_text(
            &params,
            &frame,
            layers,
            Some(&area),
            true,
            Some((Duration::from_millis(100), stats)),
        );
        assert_eq!(
            text,
            "green: on, boxes #2 at (0.000, 0.700, -0.500) m, 0.400 x 0.200 x 0.060 m; #6 (no such \
             box, 3 boxes); key floor 70 ratio 1.35 delta 25 soft 24, pad 0.030, under 1 (cropped to \
             the zone); view L u 0.410-0.630 v 0.220-0.350, R outside; under layer submitted; base \
             layer green-keyed; camera: 200 px, green 0.75, between 0.05, green p10/50/90 r 30/40/50 g \
             190/200/220 b 40/50/60"
        );
        // Old stats, no learner
        assert!(
            log_text(
                &params,
                &frame,
                layers,
                Some(&area),
                true,
                Some((Duration::from_secs(3), stats))
            )
            .ends_with("camera: no pass with the zone in the last 1 s")
        );
        assert!(
            log_text(&params, &frame, layers, Some(&area), false, None)
                .contains("camera: the learner is not running")
        );
        // Off: the reason, the settings, nothing of the views
        let off = GreenFrame {
            state: Err(GreenOff::NoArea),
            indices: [None; 2],
            key: None,
            rects: [ZoneRect::Outside; 2],
            under: None,
            pad: 0.03,
        };
        assert_eq!(
            log_text(&params, &off, GreenLayers::default(), None, true, None),
            "green: off (no play area placed (the marker's boxes)), boxes #2; #6; key floor 70 ratio \
             1.35 delta 25 soft 24, pad 0.030, under 1 (cropped to the zone)"
        );
    }
}
