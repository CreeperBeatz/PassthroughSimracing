//! Learns the colors for the passthrough color key from images of the passthrough camera: what the
//! tracked hands look like, and what is behind them. Only the colors are learned from the (late)
//! camera images. The key itself is applied by the compositor to the image it displays.

use crate::{HandShape, ray_capsule_distance};
use alvr_common::glam::{Quat, Vec2, Vec3};

pub const COLOR_LUT_RESOLUTION: usize = 32;
const TEXEL_COUNT: usize = COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION;

// How long colors are remembered, in seconds
const SKIN_MEMORY: f32 = 5.0;
const BACKGROUND_MEMORY: f32 = 3.0;
// Backgrounds that were behind the hand before, and everything that the camera sees
const PAST_BACKGROUND_MEMORY: f32 = 60.0;
const SCENE_MEMORY: f32 = 20.0;
const PAST_BACKGROUND_WEIGHT: f32 = 0.4;
const SCENE_WEIGHT: f32 = 0.5;
// Width of the smoothing of the color densities, in LUT texels. The camera colors are a bit different
// from the colors seen by the compositor (about one texel).
const SKIN_SMOOTHING: f32 = 1.4;
const BACKGROUND_SMOOTHING: f32 = 1.3;
// Densities below this are not significant, see `ColorModel::unknown_keep`
const FLOOR: f32 = 1e-6;
const LIKELIHOOD_SOFTNESS: f32 = 0.7;
// How much of the new LUT is used at each update, to avoid visible jumps
const BLEND: f32 = 0.35;
// How much of the samples may come from the wrong side, because the hand tracking is not exact
const CONTAMINATION: f32 = 0.25;
// The scene may contain arms and hands that are not tracked: it must not remove skin colors
const SCENE_CONTAMINATION: f32 = 1.0;
const MIN_SAMPLES: usize = 300;

struct Density {
    // Sums up to 1 (or it's all zeros)
    values: Vec<f32>,
    memory: f32,
    sample_count: usize,
}

impl Density {
    fn new(memory: f32) -> Self {
        Self {
            values: vec![0.0; TEXEL_COUNT],
            memory,
            sample_count: 0,
        }
    }

    fn add(&mut self, colors: &[[u8; 3]], elapsed: f32) {
        // Without samples nothing is forgotten, e.g. when the tracking is lost
        if colors.is_empty() {
            return;
        }

        let keep = if self.sample_count == 0 {
            0.0
        } else {
            (-elapsed / self.memory).exp()
        };
        for value in &mut self.values {
            *value *= keep;
        }
        let weight = (1.0 - keep) / colors.len() as f32;
        for color in colors {
            self.values[texel_index(*color)] += weight;
        }
        self.sample_count = self.sample_count.saturating_add(colors.len());
    }
}

fn texel(value: u8) -> usize {
    (usize::from(value) * (COLOR_LUT_RESOLUTION - 1) + 127) / 255
}

fn texel_index([r, g, b]: [u8; 3]) -> usize {
    (texel(b) * COLOR_LUT_RESOLUTION + texel(g)) * COLOR_LUT_RESOLUTION + texel(r)
}

// Separable gaussian, clamped at the borders
fn smooth(values: &[f32], sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil() as isize;
    let mut kernel = (-radius..=radius)
        .map(|i| (-0.5 * (i as f32 / sigma).powi(2)).exp())
        .collect::<Vec<_>>();
    let sum = kernel.iter().sum::<f32>();
    kernel.iter_mut().for_each(|k| *k /= sum);

    let mut current = values.to_vec();
    let mut next = vec![0.0; TEXEL_COUNT];
    for stride in [
        1,
        COLOR_LUT_RESOLUTION,
        COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION,
    ] {
        for (index, output) in next.iter_mut().enumerate() {
            let position = (index / stride % COLOR_LUT_RESOLUTION) as isize;
            let line_start = index - position as usize * stride;
            *output = (-radius..=radius)
                .zip(&kernel)
                .map(|(offset, weight)| {
                    let p = (position + offset).clamp(0, COLOR_LUT_RESOLUTION as isize - 1);
                    current[line_start + p as usize * stride] * weight
                })
                .sum();
        }
        std::mem::swap(&mut current, &mut next);
    }

    current
}

pub struct ColorModel {
    skin: Density,
    /// Around the hands
    background: Density,
    past_background: Vec<f32>,
    scene: Density,
    alpha: Option<Vec<f32>>,
    /// Probability (0..1) that a color that is seen neither on the hand nor in the background belongs to
    /// the hand. High values keep e.g. watches and lit fingertips, low values remove clutter.
    pub unknown_keep: f32,
    /// How much skin is preferred if skin and background look the same. High values avoid holes in the
    /// hand, low values avoid leaks.
    pub skin_bias: f32,
}

impl Default for ColorModel {
    fn default() -> Self {
        Self {
            skin: Density::new(SKIN_MEMORY),
            background: Density::new(BACKGROUND_MEMORY),
            past_background: vec![0.0; TEXEL_COUNT],
            scene: Density::new(SCENE_MEMORY),
            alpha: None,
            unknown_keep: 0.1,
            skin_bias: 1.5,
        }
    }
}

impl ColorModel {
    /// `elapsed`: seconds since the last call
    pub fn add_samples(&mut self, samples: &HandColorSamples, elapsed: f32) {
        self.skin.add(&samples.skin, elapsed);
        self.background.add(&samples.background, elapsed);
        self.scene.add(&samples.scene, elapsed);

        if !samples.background.is_empty() {
            let keep = (-elapsed / PAST_BACKGROUND_MEMORY).exp();
            for (past, current) in self.past_background.iter_mut().zip(&self.background.values) {
                *past = f32::max(*past * keep, *current);
            }
        }
    }

    /// Returns a RGBA LUT indexed by `[b][g][r]`, or None if there are not enough samples yet
    pub fn update_lut(&mut self) -> Option<Vec<u8>> {
        if self.skin.sample_count < MIN_SAMPLES || self.background.sample_count < MIN_SAMPLES {
            return None;
        }

        let skin = smooth(&self.skin.values, SKIN_SMOOTHING);
        let background = self
            .background
            .values
            .iter()
            .zip(&self.past_background)
            .map(|(current, past)| f32::max(*current, past * PAST_BACKGROUND_WEIGHT))
            .collect::<Vec<_>>();
        let background = smooth(&background, BACKGROUND_SMOOTHING);
        let scene = smooth(&self.scene.values, BACKGROUND_SMOOTHING);
        let backgrounds = background.iter().zip(&scene);
        let skin_bias = self.skin_bias;
        let unknown_keep = self.unknown_keep.clamp(0.01, 0.99);
        let skin_floor = FLOOR * (unknown_keep / (1.0 - unknown_keep)).powf(LIKELIHOOD_SOFTNESS);
        let target = skin
            .iter()
            .zip(backgrounds)
            .map(|(skin, (background, scene))| {
                // Remove the part of each that can be explained by samples of the other one
                let scene = (scene * SCENE_WEIGHT - skin * SCENE_CONTAMINATION).max(0.0);
                let (skin, background) = (
                    (skin - background * CONTAMINATION).max(0.0),
                    (background - skin * CONTAMINATION).max(scene),
                );
                let score = ((background + FLOOR) / (skin * skin_bias + skin_floor)).ln();
                let keep = 1.0 / (1.0 + (score / LIKELIHOOD_SOFTNESS).clamp(-30.0, 30.0).exp());
                // Make it steeper
                ((keep - 0.3) / 0.4).clamp(0.0, 1.0)
            });

        let alpha = if let Some(mut alpha) = self.alpha.take() {
            for (alpha, target) in alpha.iter_mut().zip(target) {
                *alpha += (target - *alpha) * BLEND;
            }
            alpha
        } else {
            target.collect()
        };

        let channel = |i: usize| (i * 255 / (COLOR_LUT_RESOLUTION - 1)) as u8;
        let lut = alpha
            .iter()
            .enumerate()
            .flat_map(|(i, alpha)| {
                [
                    channel(i % COLOR_LUT_RESOLUTION),
                    channel(i / COLOR_LUT_RESOLUTION % COLOR_LUT_RESOLUTION),
                    channel(i / (COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION)),
                    (alpha * 255.0).round() as u8,
                ]
            })
            .collect();
        self.alpha = Some(alpha);

        Some(lut)
    }

    /// Fraction of the colors that are kept by the current LUT, for diagnostics
    pub fn kept_fraction(&self, colors: &[[u8; 3]]) -> f32 {
        let Some(alpha) = &self.alpha else {
            return 1.0;
        };
        let kept = colors
            .iter()
            .filter(|color| alpha[texel_index(**color)] > 0.5)
            .count();

        kept as f32 / colors.len().max(1) as f32
    }
}

/// Pinhole camera in world space. The camera looks at +z, with +x to the right and +y down.
#[derive(Clone, Copy, Debug)]
pub struct CameraView {
    pub position: Vec3,
    pub orientation: Quat,
    pub focal_length: Vec2,
    pub center: Vec2,
}

impl CameraView {
    fn ray(&self, pixel: Vec2) -> Vec3 {
        self.orientation
            * ((pixel - self.center) / self.focal_length)
                .extend(1.0)
                .normalize()
    }

    /// Pixel and distance along the view axis. None if the point is behind the camera
    pub fn project(&self, point: Vec3) -> Option<(Vec2, f32)> {
        let local = self.orientation.inverse() * (point - self.position);
        (local.z > 0.05).then(|| {
            (
                local.truncate() / local.z * self.focal_length + self.center,
                local.z,
            )
        })
    }
}

#[derive(Clone, Copy)]
pub struct SampledHand {
    pub shape: HandShape,
    pub wrist: Vec3,
    /// From the hand to the arm
    pub arm_direction: Vec3,
}

#[derive(Default)]
pub struct HandColorSamples {
    pub skin: Vec<[u8; 3]>,
    pub background: Vec<[u8; 3]>,
    /// Everything except the hands and arms
    pub scene: Vec<[u8; 3]>,
}

/// Collect the colors inside of the hands, and around them. `color` returns the RGB color of a pixel.
pub fn sample_hand_colors(
    camera: &CameraView,
    width: u32,
    height: u32,
    hands: &[Option<SampledHand>; 2],
    color: impl Fn(u32, u32) -> [u8; 3],
) -> HandColorSamples {
    // Distances from the surface of the hand, with a margin for tracking and timing errors
    const SKIN_MAX: f32 = -0.005;
    const BACKGROUND_MIN: f32 = 0.02;
    const BACKGROUND_MAX: f32 = 0.07;
    const STEP: usize = 2;
    const SCENE_STEP: usize = 8;
    const ARM_LENGTH: f32 = 0.5;
    const ARM_RADIUS: f32 = 0.1;

    let mut samples = HandColorSamples::default();

    for y in (0..height).step_by(SCENE_STEP) {
        for x in (0..width).step_by(SCENE_STEP) {
            let ray = camera.ray(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
            let is_hand_or_arm = hands.iter().flatten().any(|hand| {
                hand.shape.ray_distance(camera.position, ray) < BACKGROUND_MIN
                    || ray_capsule_distance(
                        camera.position,
                        ray,
                        hand.wrist.extend(ARM_RADIUS),
                        (hand.wrist + hand.arm_direction * ARM_LENGTH).extend(ARM_RADIUS),
                    ) < 0.0
            });
            if !is_hand_or_arm {
                samples.scene.push(color(x, y));
            }
        }
    }

    for (hand_index, hand) in hands.iter().enumerate() {
        let Some(hand) = hand else {
            continue;
        };
        let other_hand = hands[1 - hand_index].as_ref();

        let bounds = hand.shape.bounds();
        let Some((center, depth)) = camera.project(bounds.truncate()) else {
            continue;
        };
        let radius = camera.focal_length * (bounds.w + BACKGROUND_MAX) / depth;
        let min = (center - radius).max(Vec2::ZERO).as_uvec2();
        let max = (center + radius)
            .max(Vec2::ZERO)
            .as_uvec2()
            .min(alvr_common::glam::UVec2::new(width, height));

        for y in (min.y..max.y).step_by(STEP) {
            for x in (min.x..max.x).step_by(STEP) {
                let ray = camera.ray(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                let distance = hand.shape.ray_distance(camera.position, ray);

                if hand.shape.palm_ray_distance(camera.position, ray) < SKIN_MAX {
                    samples.skin.push(color(x, y));
                } else if (BACKGROUND_MIN..BACKGROUND_MAX).contains(&distance) {
                    // The arm is not background
                    let to_wrist = hand.wrist - camera.position;
                    let point = camera.position + ray * to_wrist.dot(ray);
                    let is_arm = (point - hand.wrist).dot(hand.arm_direction) > -0.03;
                    let is_other_hand = other_hand.is_some_and(|other| {
                        other.shape.ray_distance(camera.position, ray) < BACKGROUND_MIN
                            || (point - other.wrist).length() < 0.15
                    });
                    if !is_arm && !is_other_hand {
                        samples.background.push(color(x, y));
                    }
                }
            }
        }
    }

    samples
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HAND_SEGMENTS;
    use alvr_common::glam::Vec4;

    const SKIN: [u8; 3] = [150, 110, 85];
    const WHITE_WALL: [u8; 3] = [105, 92, 80];
    const GREEN_WALL: [u8; 3] = [62, 56, 10];

    fn noisy(color: [u8; 3], count: usize) -> Vec<[u8; 3]> {
        // Deterministic noise of +-6
        (0..count)
            .map(|i| {
                let offset = |k: usize| ((i * (7 + 4 * k)) % 13) as i32 - 6;
                [0, 1, 2].map(|c| (i32::from(color[c]) + offset(c)).clamp(0, 255) as u8)
            })
            .collect()
    }

    fn samples(skin: [u8; 3], background: [u8; 3]) -> HandColorSamples {
        HandColorSamples {
            skin: noisy(skin, 600),
            background: noisy(background, 600),
            scene: vec![],
        }
    }

    fn alpha(lut: &[u8], color: [u8; 3]) -> u8 {
        lut[texel_index(color) * 4 + 3]
    }

    #[test]
    fn keeps_the_hand_and_removes_the_background() {
        let mut model = ColorModel::default();
        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        let lut = model.update_lut().unwrap();

        assert_eq!(lut.len(), TEXEL_COUNT * 4);
        assert_eq!(alpha(&lut, SKIN), 255);
        assert_eq!(alpha(&lut, WHITE_WALL), 0);
        // Never seen: clutter, or a watch
        assert!(alpha(&lut, [20, 40, 200]) < 80);

        let mut keep_unknown = ColorModel {
            unknown_keep: 0.95,
            ..Default::default()
        };
        keep_unknown.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        let keep_unknown_lut = keep_unknown.update_lut().unwrap();
        assert_eq!(alpha(&keep_unknown_lut, [20, 40, 200]), 255);
        assert_eq!(alpha(&keep_unknown_lut, WHITE_WALL), 0);

        // The colors are not changed
        let index = texel_index(SKIN) * 4;
        for c in 0..3 {
            assert!((i32::from(lut[index + c]) - i32::from(SKIN[c])).abs() <= 5);
        }
    }

    #[test]
    fn needs_enough_samples() {
        let mut model = ColorModel::default();
        assert!(model.update_lut().is_none());
        model.add_samples(
            &HandColorSamples {
                skin: noisy(SKIN, 1000),
                ..Default::default()
            },
            0.1,
        );
        assert!(model.update_lut().is_none());
    }

    #[test]
    fn adapts_to_changes() {
        let mut model = ColorModel::default();
        for _ in 0..20 {
            model.add_samples(&samples(SKIN, GREEN_WALL), 0.12);
            model.update_lut();
        }

        // A lamp is switched on
        let bright = |c: [u8; 3]| c.map(|v| (f32::from(v) * 1.5).min(255.0) as u8);
        let mut lut = vec![];
        for _ in 0..40 {
            model.add_samples(&samples(bright(SKIN), bright(GREEN_WALL)), 0.12);
            lut = model.update_lut().unwrap();
        }
        assert!(alpha(&lut, bright(SKIN)) > 240);
        assert!(alpha(&lut, bright(GREEN_WALL)) < 15);
    }

    #[test]
    fn keeps_the_hand_if_the_background_looks_the_same() {
        let mut model = ColorModel::default();
        model.add_samples(&samples(SKIN, SKIN), 0.1);
        let lut = model.update_lut().unwrap();

        assert!(alpha(&lut, SKIN) > 100, "{}", alpha(&lut, SKIN));

        // Preferring the hand
        let mut model = ColorModel {
            skin_bias: 3.0,
            ..Default::default()
        };
        model.add_samples(&samples(SKIN, SKIN), 0.1);
        let lut = model.update_lut().unwrap();
        assert!(alpha(&lut, SKIN) > 240, "{}", alpha(&lut, SKIN));
    }

    #[test]
    fn tolerates_samples_from_the_wrong_side() {
        let mixed = |main, other| {
            let mut colors = noisy(main, 850);
            colors.extend(noisy(other, 150));
            colors
        };
        let mut model = ColorModel::default();
        model.add_samples(
            &HandColorSamples {
                skin: mixed(SKIN, GREEN_WALL),
                background: mixed(GREEN_WALL, SKIN),
                scene: vec![],
            },
            0.1,
        );
        let lut = model.update_lut().unwrap();

        assert_eq!(alpha(&lut, SKIN), 255);
        assert_eq!(alpha(&lut, GREEN_WALL), 0);
        // Noisy pixels are still clearly on one side
        for color in noisy(SKIN, 200) {
            assert!(alpha(&lut, color) > 200, "{color:?}");
        }
        for color in noisy(GREEN_WALL, 200) {
            assert!(alpha(&lut, color) < 55, "{color:?}");
        }
    }

    #[test]
    fn remembers_previous_backgrounds() {
        let mut model = ColorModel::default();
        for _ in 0..20 {
            model.add_samples(&samples(SKIN, GREEN_WALL), 0.12);
            model.update_lut();
        }
        // 30 seconds in front of another wall
        let mut lut = vec![];
        for _ in 0..250 {
            model.add_samples(&samples(SKIN, WHITE_WALL), 0.12);
            lut = model.update_lut().unwrap();
        }

        assert_eq!(alpha(&lut, GREEN_WALL), 0);
        assert_eq!(alpha(&lut, WHITE_WALL), 0);
        assert_eq!(alpha(&lut, SKIN), 255);
    }

    #[test]
    fn learns_many_backgrounds_from_the_scene() {
        // A complex scene, the hand is in front of the white wall
        let colors = [GREEN_WALL, [200, 30, 40], [30, 90, 200], [20, 20, 20]];
        let mut scene = colors
            .iter()
            .flat_map(|color| noisy(*color, 200))
            .collect::<Vec<_>>();
        // The other arm is in the view, and not tracked
        scene.extend(noisy(SKIN, 150));

        let mut model = ColorModel {
            unknown_keep: 0.95,
            ..Default::default()
        };
        let mut lut = vec![];
        for _ in 0..20 {
            let mut samples = samples(SKIN, WHITE_WALL);
            samples.scene = scene.clone();
            model.add_samples(&samples, 0.12);
            lut = model.update_lut().unwrap();
        }

        for color in colors {
            assert_eq!(alpha(&lut, color), 0, "{color:?}");
        }
        assert_eq!(alpha(&lut, SKIN), 255);
    }

    #[test]
    fn nothing_is_forgotten_without_tracking() {
        let mut model = ColorModel::default();
        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        let before = model.update_lut().unwrap();
        model.add_samples(&HandColorSamples::default(), 1000.0);
        let after = model.update_lut().unwrap();

        assert_eq!(before, after);
    }

    #[test]
    fn samples_come_from_the_right_place() {
        // A horizontal hand, 0.5 m in front of the camera. The arm is on the right
        let segment = [
            Vec4::new(-0.05, 0.0, 0.5, 0.02),
            Vec4::new(0.05, 0.0, 0.5, 0.02),
        ];
        let hand = SampledHand {
            shape: HandShape {
                segments: [segment; HAND_SEGMENTS],
            },
            wrist: Vec3::new(0.05, 0.0, 0.5),
            arm_direction: Vec3::X,
        };
        let camera = CameraView {
            position: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            focal_length: Vec2::splat(217.0),
            center: Vec2::new(160.0, 120.0),
        };

        // Encode the pixel position in the color. 320 doesn't fit in a byte: x / 2
        let result = sample_hand_colors(&camera, 320, 240, &[Some(hand), None], |x, y| {
            [(x / 2) as u8, y as u8, 0]
        });
        assert!(result.skin.len() > 50, "{}", result.skin.len());
        assert!(result.background.len() > 200, "{}", result.background.len());

        for [x, y, _] in &result.skin {
            let (x, y) = (f32::from(*x) * 2.0 - 160.0, f32::from(*y) - 120.0);
            // 0.016 m and 0.066 m at 0.5 m
            assert!(y.abs() < 8.0 && x.abs() < 31.0, "{x} {y}");
        }
        assert!(result.scene.len() > 600, "{}", result.scene.len());
        for [x, y, _] in &result.scene {
            let (x, y) = (f32::from(*x) * 2.0 - 160.0, f32::from(*y) - 120.0);
            // Neither the hand nor the arm (radius 0.1 m, to the right)
            let arm_distance = Vec2::new((21.7 - x).max(0.0), y).length();
            let hand_distance = Vec2::new((x.abs() - 21.7).max(0.0), y).length();
            assert!(arm_distance > 41.0 && hand_distance > 16.0, "{x} {y}");
        }
        for [x, y, _] in &result.background {
            let (x, y) = (f32::from(*x) * 2.0 - 160.0, f32::from(*y) - 120.0);
            // Distance from the segment: at least 0.04 m at 0.5 m
            let distance = Vec2::new((x.abs() - 21.7).max(0.0), y).length();
            assert!(distance > 16.0, "{x} {y}");
            // Not on the arm
            assert!(x < 10.0, "{x} {y}");
        }
    }

    #[test]
    fn camera_projection_matches_the_rays() {
        let camera = CameraView {
            position: Vec3::new(0.1, 1.2, -0.3),
            orientation: Quat::from_euler(alvr_common::glam::EulerRot::YXZ, 0.5, -0.3, 0.1),
            focal_length: Vec2::new(217.0, 215.0),
            center: Vec2::new(160.3, 119.5),
        };
        let pixel = Vec2::new(40.0, 200.0);
        let point = camera.position + camera.ray(pixel) * 0.7;
        let (projected, _) = camera.project(point).unwrap();
        assert!(projected.distance(pixel) < 0.01);
    }
}
