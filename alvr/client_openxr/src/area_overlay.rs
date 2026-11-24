//! Outline of the anchor (the QR code) and of the play area placed from it, drawn over the stream.
//! Only with a marker box and Passthrough = Depth. Experimental, controlled with adb (polled about
//! twice a second):
//! - `debug.alvr.area_show`: unset: for 8 s each time the pose of the code becomes measured, each time
//!   the box is carried over a stage change and each time a box is placed after there was none. 1:
//!   always while there is a box. 0: never
//! - `debug.alvr.area_line <hundredths of a degree>`: width of the lines on screen (12, 2 to 100)
//! - `debug.alvr.area_dashed 1`: the play area dashed (it is anyway while it is placed from the anchor
//!   saved in the layout file, before the code is seen in this run, see area_editor.rs)
//!
//! While the play area is edited, and for a moment after, the outline shows whatever `area_show` says,
//! with the handles of the editor over it (area_editor.rs). While the two-hand gesture of the editor is
//! held the outline shows and brightens.
//!
//! Drawn in stage space: the outline of the code (its reported size, at the estimated pose; amber while
//! the pose is measured, green once it is measured), the axes of the area at the code (heading 20 cm,
//! up and to the right of the heading 10 cm) and the play area the mask uses (white): the edges of
//! its boxes without the parts inside another box, and of each dome its rim and two arcs over it.

use crate::{area_editor::AreaEditor, marker_region::MarkerRegion, passthrough::debug_property};
use alvr_graphics::{
    MAX_OVERLAY_SEGMENTS, OVERLAY_AMBER, OVERLAY_GREEN, OverlaySegment, PlacedArea,
};
use std::time::{Duration, Instant};

const PROPERTY_POLL_INTERVAL: Duration = Duration::from_millis(500);
// How long the outline shows after an event (unset `area_show`)
const SHOW_DURATION: Duration = Duration::from_secs(8);
const DEFAULT_LINE: i64 = 12;
// Segments of the code and the axes: the rest of an eye is for the editor and the play area
const ANCHOR_SEGMENTS: usize = 4 + 3;

pub struct AreaOverlay {
    // `debug.alvr.area_show`. None: automatic
    show: Option<bool>,
    // Of the lines (radians)
    width: f32,
    // `debug.alvr.area_dashed`
    dashed: bool,
    // Segments of the play area left out to stay within the segments of an eye, as last logged
    left_out: usize,
    last_poll: Option<Instant>,
    // Automatic: shown until then, and why
    auto_until: Option<Instant>,
    auto_reason: &'static str,
    // Of the last frame, for the events
    had_box: bool,
    was_measured: bool,
    carried: u64,
    // Why the outline is shown, as last logged. None: hidden
    shown: Option<&'static str>,
    segments: Vec<OverlaySegment>,
    // Of the editor, in this frame
    editor_segments: Vec<OverlaySegment>,
}

impl AreaOverlay {
    pub fn new() -> Self {
        Self {
            show: None,
            width: alvr_graphics::overlay_width(DEFAULT_LINE as f32),
            dashed: false,
            left_out: 0,
            last_poll: None,
            auto_until: None,
            auto_reason: "",
            had_box: false,
            was_measured: false,
            carried: 0,
            shown: None,
            segments: Vec::new(),
            editor_segments: Vec::new(),
        }
    }

    fn poll_properties(&mut self) {
        if self
            .last_poll
            .is_some_and(|time| time.elapsed() < PROPERTY_POLL_INTERVAL)
        {
            return;
        }
        self.last_poll = Some(Instant::now());

        // Unset (or not a number): automatic
        self.show = debug_property("debug.alvr.area_show").map(|value| value != 0);
        let line = debug_property("debug.alvr.area_line")
            .unwrap_or(DEFAULT_LINE)
            .clamp(2, 100);
        self.width = alvr_graphics::overlay_width(line as f32);
        self.dashed = debug_property("debug.alvr.area_dashed").unwrap_or(0) != 0;
    }

    /// The lines to draw in this frame, in stage space. Empty: nothing is shown. `region`: the play
    /// area of the marker the mask uses (None: no area). `depth_passthrough`: the stream uses
    /// Passthrough = Depth. `editor`: its handles are drawn over the outline
    pub fn update(
        &mut self,
        marker_region: &MarkerRegion,
        region: Option<PlacedArea>,
        depth_passthrough: bool,
        editor: &AreaEditor,
    ) -> &[OverlaySegment] {
        self.poll_properties();

        // The events start the automatic display also while it can't show
        let anchor = marker_region.anchor();
        let measured = anchor.is_some_and(|anchor| anchor.measured);
        let carried = marker_region.carried_count();
        let mut event = None;
        if region.is_some() && !self.had_box {
            event = Some("placed");
        }
        if region.is_some() && carried != self.carried {
            event = Some("carried over");
        }
        if measured && !self.was_measured {
            event = Some("measured");
        }
        self.had_box = region.is_some();
        self.was_measured = measured;
        self.carried = carried;
        if let Some(reason) = event {
            self.auto_until = Some(Instant::now() + SHOW_DURATION);
            self.auto_reason = reason;
        }

        let shown = match self.show {
            _ if region.is_none() || !depth_passthrough => None,
            _ if editor.forced_show().is_some() => editor.forced_show(),
            Some(false) => None,
            Some(true) => Some("area_show 1"),
            None => self
                .auto_until
                .is_some_and(|until| Instant::now() < until)
                .then_some(self.auto_reason),
        };
        if shown != self.shown {
            self.shown = shown;
            crate::stream_log(&match shown {
                Some(reason) => format!("area: outline shown ({reason})"),
                None => "area: outline hidden".into(),
            });
        }

        self.segments.clear();
        if let (Some(region), Some(_)) = (region, shown) {
            // Without a code seen in this run (placed from the anchor of the layout file): no outline
            // of the code
            if let Some(anchor) = anchor {
                let color = if anchor.measured {
                    OVERLAY_GREEN
                } else {
                    OVERLAY_AMBER
                };
                self.segments.extend(alvr_graphics::code_outline(
                    anchor.position,
                    anchor.orientation,
                    anchor.size,
                    color,
                    self.width,
                ));
            }
            // The frame of the area at the code: the heading it uses, with `marker_box_yaw`
            let (_, rotation, position) = region.area_to_world.to_scale_rotation_translation();
            self.segments.extend(alvr_graphics::anchor_axes(
                position,
                rotation.normalize(),
                self.width,
            ));
            self.editor_segments.clear();
            editor.draw(self.width, &mut self.editor_segments);
            self.editor_segments
                .truncate(MAX_OVERLAY_SEGMENTS - ANCHOR_SEGMENTS);
            let (outline, left_out) = alvr_graphics::area_outline(
                &region,
                editor.outline_color(),
                self.width,
                self.dashed || editor.dashed(),
                MAX_OVERLAY_SEGMENTS - ANCHOR_SEGMENTS - self.editor_segments.len(),
            );
            self.segments.extend(outline);
            // Over the outline
            self.segments.extend_from_slice(&self.editor_segments);
            if left_out != self.left_out {
                self.left_out = left_out;
                if left_out > 0 {
                    crate::stream_log(&format!(
                        "area: outline cut, {left_out} segments of the play area left out ({} per \
                         eye)",
                        MAX_OVERLAY_SEGMENTS
                    ));
                }
            }
        }

        &self.segments
    }
}
