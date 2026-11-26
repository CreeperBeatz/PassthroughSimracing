// Stage S2, `debug.alvr.edge_fill` (docs/edge-fill.md): the game layers are submitted with the pose the PC
// rendered for; the compositor turns them to the display-time pose, and on a head turn the layer's edge
// moves into the view, where the full-view passthrough below shows a strip of the room. With the fill on,
// the game layers are submitted with an output fov wider by a margin on all four sides (same pose, same
// swapchains and sizes) and the main pass draws the stream view inside it; the margin gets the colour of
// the stream's border pixels (smeared outward), opaque. Off (unset or 0): the output view is the stream
// view, and nothing here is used.
//
// Both views have the same pose, so a pixel's tangents are the same in both and the map between their uv
// (origin top-left, as the stream quad of `reprojection_transform`) is linear per axis.

use alvr_common::{Fov, glam::Vec2};

/// Largest `debug.alvr.edge_fill` (tenths of a degree): 8 degrees
pub const EDGE_FILL_MAX_TENTHS: u32 = 80;

// A widened side never reaches 90 degrees (the tangent would be infinite)
const MAX_SIDE_ANGLE: f32 = 85.0 * std::f32::consts::PI / 180.0;

// The stream quad grows this much (stream uv) beyond what the output view needs, so that rounding never
// leaves an output pixel at the border uncovered. Fragments outside the target are clipped: no cost
const QUAD_SLACK: f32 = 0.01;

/// `debug.alvr.edge_fill` as applied (tenths of a degree): unset, 0 or negative is 0 (off), above
/// `EDGE_FILL_MAX_TENTHS` is that
pub fn edge_fill_tenths(value: Option<i64>) -> u32 {
    value.map_or(0, |value| {
        value.clamp(0, i64::from(EDGE_FILL_MAX_TENTHS)) as u32
    })
}

/// The margin of `tenths` in radians
pub fn edge_fill_radians(tenths: u32) -> f32 {
    (tenths as f32 * 0.1).to_radians()
}

/// `fov` grown by `margin` (radians) on all four sides, each side at most 85 degrees from the axis
pub fn widen_fov(fov: Fov, margin: f32) -> Fov {
    let margin = margin.max(0.0);

    Fov {
        left: (fov.left - margin).max(-MAX_SIDE_ANGLE),
        right: (fov.right + margin).min(MAX_SIDE_ANGLE),
        up: (fov.up + margin).min(MAX_SIDE_ANGLE),
        down: (fov.down - margin).max(-MAX_SIDE_ANGLE),
    }
}

// The tangents of a view: left, right, up, down
fn tangents(fov: Fov) -> [f32; 4] {
    [
        fov.left.tan(),
        fov.right.tan(),
        fov.up.tan(),
        fov.down.tan(),
    ]
}

// uv (origin top-left) -> tangents (x right, y up) of the view, and back
fn uv_to_tangent(fov: Fov, uv: Vec2) -> Vec2 {
    let [left, right, up, down] = tangents(fov);
    Vec2::new(left + uv.x * (right - left), up - uv.y * (up - down))
}

fn tangent_to_uv(fov: Fov, tangent: Vec2) -> Vec2 {
    let [left, right, up, down] = tangents(fov);
    Vec2::new(
        (tangent.x - left) / (right - left),
        (up - tangent.y) / (up - down),
    )
}

/// The uv of the output view (origin top-left) that shows the stream view's `uv`. Both views have the same
/// pose; the stream view's 0..1 lies inside the wider output view
pub fn stream_to_output_uv(stream: Fov, output: Fov, uv: Vec2) -> Vec2 {
    tangent_to_uv(output, uv_to_tangent(stream, uv))
}

/// Inverse of `stream_to_output_uv`: the stream uv drawn at the output view's `uv` (outside 0..1 in the
/// margin)
pub fn output_to_stream_uv(stream: Fov, output: Fov, uv: Vec2) -> Vec2 {
    tangent_to_uv(stream, uv_to_tangent(output, uv))
}

/// A rectangle (min and max uv, origin top-left) of the stream view in the output view's uv
pub fn stream_rect_to_output(stream: Fov, output: Fov, rect: (Vec2, Vec2)) -> (Vec2, Vec2) {
    (
        stream_to_output_uv(stream, output, rect.0),
        stream_to_output_uv(stream, output, rect.1),
    )
}

/// A rectangle (min and max uv, origin top-left) of the output view in the stream view's uv (may reach
/// outside 0..1)
pub fn output_rect_to_stream(stream: Fov, output: Fov, rect: (Vec2, Vec2)) -> (Vec2, Vec2) {
    (
        output_to_stream_uv(stream, output, rect.0),
        output_to_stream_uv(stream, output, rect.1),
    )
}

/// How far (stream uv) the stream quad of the main pass must reach beyond 0..1 on every side so that it
/// covers the whole output view: 0 when the output view is the stream view or inside it (the quad is then
/// today's)
pub fn edge_fill_quad_extension(stream: Fov, output: Fov) -> f32 {
    let min = output_to_stream_uv(stream, output, Vec2::ZERO);
    let max = output_to_stream_uv(stream, output, Vec2::ONE);
    let needed = (-min).max(max - Vec2::ONE).max_element();
    if needed.is_finite() && needed > 0.0 {
        needed + QUAD_SLACK
    } else {
        0.0
    }
}

/// The share of pixel density lost per axis (x, y) at the view's centre: the same number of swapchain
/// pixels covers the wider tangent range (0.07: 7 % fewer pixels per degree)
pub fn edge_fill_density_loss(stream: Fov, output: Fov) -> Vec2 {
    let [left, right, up, down] = tangents(stream);
    let [out_left, out_right, out_up, out_down] = tangents(output);

    Vec2::ONE
        - Vec2::new(
            (right - left) / (out_right - out_left),
            (up - down) / (out_up - out_down),
        )
}

/// The line logged when `debug.alvr.edge_fill` changes. `stream`: a stream view's fov for the density
/// (None: not known yet); `custom_reprojection`: the YVR path, where the fill is not applied
pub fn edge_fill_text(tenths: u32, custom_reprojection: bool, stream: Option<Fov>) -> String {
    if tenths == 0 {
        return "edge fill: off (debug.alvr.edge_fill unset or 0): the game layers have the stream's fov \
                (today's)"
            .into();
    }
    let degrees = tenths as f32 * 0.1;
    if custom_reprojection {
        return format!(
            "edge fill: {degrees:.1} deg asked (debug.alvr.edge_fill {tenths}), NOT applied: the custom \
             reprojection path (YVR) draws into the display-time view"
        );
    }
    let density = stream.map_or(String::new(), |fov| {
        let loss = edge_fill_density_loss(fov, widen_fov(fov, edge_fill_radians(tenths)));
        format!(
            ", game pixel density -{:.1} % x, -{:.1} % y (left view)",
            100.0 * loss.x,
            100.0 * loss.y
        )
    });

    format!(
        "edge fill: {degrees:.1} deg per side (debug.alvr.edge_fill {tenths}): the game layers' fov is \
         widened, the margin shows the stream's border smeared outward, opaque{density}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The left eye of the Quest 3 (burst JSONs): 54 deg temporal, 40 nasal, 44 up, 55 down
    fn quest_left() -> Fov {
        Fov {
            left: (-1.376_f32).atan(),
            right: 0.839_f32.atan(),
            up: 0.966_f32.atan(),
            down: (-1.428_f32).atan(),
        }
    }

    fn close(a: Vec2, b: Vec2) -> bool {
        (a - b).abs().max_element() < 1e-5
    }

    #[test]
    fn property_values() {
        assert_eq!(edge_fill_tenths(None), 0);
        assert_eq!(edge_fill_tenths(Some(0)), 0);
        assert_eq!(edge_fill_tenths(Some(-5)), 0);
        assert_eq!(edge_fill_tenths(Some(20)), 20);
        assert_eq!(edge_fill_tenths(Some(80)), 80);
        assert_eq!(edge_fill_tenths(Some(500)), 80);
        assert!((edge_fill_radians(20) - 2f32.to_radians()).abs() < 1e-7);
        assert_eq!(edge_fill_radians(0), 0.0);
    }

    #[test]
    fn fov_grows_on_every_side() {
        let fov = quest_left();
        let margin = 2f32.to_radians();
        let wide = widen_fov(fov, margin);
        assert!((wide.left - (fov.left - margin)).abs() < 1e-6);
        assert!((wide.right - (fov.right + margin)).abs() < 1e-6);
        assert!((wide.up - (fov.up + margin)).abs() < 1e-6);
        assert!((wide.down - (fov.down - margin)).abs() < 1e-6);

        // Never 90 degrees or more
        let wide = widen_fov(fov, 60f32.to_radians());
        for side in [wide.left, wide.right, wide.up, wide.down] {
            assert!(side.abs() <= MAX_SIDE_ANGLE + 1e-6);
            assert!(side.tan().is_finite());
        }
        // A negative margin is none
        let same = widen_fov(fov, -0.1);
        assert_eq!(
            [same.left, same.right, same.up, same.down],
            [fov.left, fov.right, fov.up, fov.down]
        );
    }

    #[test]
    fn uv_round_trip() {
        let stream = quest_left();
        let output = widen_fov(stream, 3f32.to_radians());
        for uv in [
            Vec2::ZERO,
            Vec2::ONE,
            Vec2::new(0.25, 0.8),
            Vec2::new(-0.03, 1.02),
            Vec2::new(0.5, 0.5),
        ] {
            let there = stream_to_output_uv(stream, output, uv);
            assert!(
                close(output_to_stream_uv(stream, output, there), uv),
                "{uv}"
            );
            let back = output_to_stream_uv(stream, output, uv);
            assert!(close(stream_to_output_uv(stream, output, back), uv), "{uv}");
        }
        // The same view: the identity
        for uv in [Vec2::ZERO, Vec2::new(0.3, 0.7), Vec2::ONE] {
            assert!(close(stream_to_output_uv(stream, stream, uv), uv));
        }
    }

    #[test]
    fn centre_maps_to_centre() {
        // A symmetric view: the centre stays the centre
        let a = 0.8_f32;
        let stream = Fov {
            left: -a,
            right: a,
            up: a,
            down: -a,
        };
        let output = widen_fov(stream, 2f32.to_radians());
        let centre = Vec2::splat(0.5);
        assert!(close(stream_to_output_uv(stream, output, centre), centre));
        assert!(close(output_to_stream_uv(stream, output, centre), centre));

        // An asymmetric view: the view axis (tangent 0) stays the view axis
        let stream = quest_left();
        let output = widen_fov(stream, 2f32.to_radians());
        let axis = tangent_to_uv(stream, Vec2::ZERO);
        assert!(close(
            uv_to_tangent(output, stream_to_output_uv(stream, output, axis)),
            Vec2::ZERO
        ));
    }

    #[test]
    fn stream_view_lies_inside_by_the_margin() {
        let stream = quest_left();
        let margin = 2f32.to_radians();
        let output = widen_fov(stream, margin);
        let (min, max) = stream_rect_to_output(stream, output, (Vec2::ZERO, Vec2::ONE));
        assert!(min.cmpgt(Vec2::ZERO).all() && max.cmplt(Vec2::ONE).all());

        // Each side of the stream view is `margin` (angle) inside the same side of the output view
        let angle = |fov: Fov, uv: Vec2| {
            let tangent = uv_to_tangent(fov, uv);
            Vec2::new(tangent.x.atan(), tangent.y.atan())
        };
        let outer_min = angle(output, Vec2::ZERO);
        let outer_max = angle(output, Vec2::ONE);
        let inner_min = angle(output, min);
        let inner_max = angle(output, max);
        // uv min is left and up, max is right and down
        assert!((inner_min.x - outer_min.x - margin).abs() < 1e-5);
        assert!((outer_max.x - inner_max.x - margin).abs() < 1e-5);
        assert!((outer_min.y - inner_min.y - margin).abs() < 1e-5);
        assert!((inner_max.y - outer_max.y - margin).abs() < 1e-5);
    }

    #[test]
    fn rectangles_convert_both_ways() {
        let stream = quest_left();
        let output = widen_fov(stream, 4f32.to_radians());
        let rect = (Vec2::new(0.2, 0.55), Vec2::new(0.7, 0.9));
        let there = stream_rect_to_output(stream, output, rect);
        // Smaller in the wider view, still ordered
        assert!(there.0.cmplt(there.1).all());
        assert!((there.1 - there.0).cmplt(rect.1 - rect.0).all());
        let back = output_rect_to_stream(stream, output, there);
        assert!(close(back.0, rect.0) && close(back.1, rect.1));

        // The whole output view reaches outside the stream view on every side
        let whole = output_rect_to_stream(stream, output, (Vec2::ZERO, Vec2::ONE));
        assert!(whole.0.cmplt(Vec2::ZERO).all() && whole.1.cmpgt(Vec2::ONE).all());
    }

    #[test]
    fn quad_covers_the_output_view() {
        let stream = quest_left();
        assert_eq!(edge_fill_quad_extension(stream, stream), 0.0);

        for degrees in [0.5_f32, 2.0, 8.0] {
            let output = widen_fov(stream, degrees.to_radians());
            let extension = edge_fill_quad_extension(stream, output);
            let (min, max) = output_rect_to_stream(stream, output, (Vec2::ZERO, Vec2::ONE));
            assert!(min.cmpge(Vec2::splat(-extension)).all(), "{degrees}");
            assert!(max.cmple(Vec2::splat(1.0 + extension)).all(), "{degrees}");
            // Not much more than needed
            let needed = (-min).max(max - Vec2::ONE).max_element();
            assert!(extension <= needed + QUAD_SLACK + 1e-6);
        }
    }

    #[test]
    fn density_drops_with_the_margin() {
        let stream = quest_left();
        assert_eq!(edge_fill_density_loss(stream, stream), Vec2::ZERO);
        let loss = edge_fill_density_loss(stream, widen_fov(stream, 2f32.to_radians()));
        // About 7 % per axis at 2 degrees on the Quest 3's view
        assert!(loss.x > 0.06 && loss.x < 0.08, "{loss}");
        assert!(loss.y > 0.06 && loss.y < 0.08, "{loss}");
    }

    #[test]
    fn log_lines() {
        assert_eq!(
            edge_fill_text(0, false, None),
            "edge fill: off (debug.alvr.edge_fill unset or 0): the game layers have the stream's fov \
             (today's)"
        );
        let on = edge_fill_text(20, false, Some(quest_left()));
        assert!(
            on.starts_with("edge fill: 2.0 deg per side (debug.alvr.edge_fill 20)"),
            "{on}"
        );
        assert!(on.contains("game pixel density -7.0 % x, -7.0 % y"), "{on}");
        assert!(!edge_fill_text(20, false, None).contains("density"));
        assert!(edge_fill_text(20, true, None).contains("NOT applied"));
    }
}
