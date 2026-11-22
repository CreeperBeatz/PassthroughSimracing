//! Learns the colors for the passthrough color key from images of the passthrough camera: what the
//! tracked hands look like, and what is behind them. Only the colors are learned from the (late)
//! camera images. The key itself is applied by the compositor to the image it displays.

use crate::{HAND_SEGMENTS, HandShape, ray_capsule_distance};
use alvr_common::glam::{Quat, Vec2, Vec3};

pub const COLOR_LUT_RESOLUTION: usize = 32;
const TEXEL_COUNT: usize = COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION;

// How long colors are remembered, in seconds
const SKIN_MEMORY: f32 = 5.0;
const BACKGROUND_MEMORY: f32 = 3.0;
// Backgrounds that were behind the hand before, and everything that the camera sees
const PAST_BACKGROUND_MEMORY: f32 = 60.0;
// The hand looks different in other places of the room
const PAST_SKIN_MEMORY: f32 = 45.0;
const PAST_SKIN_WEIGHT: f32 = 0.5;
const SCENE_MEMORY: f32 = 20.0;
const PAST_BACKGROUND_WEIGHT: f32 = 0.4;
const SCENE_WEIGHT: f32 = 0.5;
// Width of the smoothing of the color densities, in LUT texels. The camera colors are a bit different
// from the colors seen by the compositor (about one texel).
const SKIN_SMOOTHING: f32 = 1.0;
const BACKGROUND_SMOOTHING: f32 = 1.3;
// Densities below this are never significant
const MIN_FLOOR: f32 = 1e-6;
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
    past_skin: Vec<f32>,
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
    /// How similar to the skin a color must be, as a fraction of the typical density of the skin colors.
    /// Low values accept similar colors that were never seen, which shows complex backgrounds.
    pub tightness: f32,
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
            past_skin: vec![0.0; TEXEL_COUNT],
            tightness: 0.2,
        }
    }
}

impl ColorModel {
    /// `elapsed`: seconds since the last call
    pub fn add_samples(&mut self, samples: &HandColorSamples, elapsed: f32) {
        self.skin.add(&samples.skin, elapsed);
        self.background.add(&samples.background, elapsed);
        self.scene.add(&samples.scene, elapsed);

        if !samples.skin.is_empty() {
            let keep = (-elapsed / PAST_SKIN_MEMORY).exp();
            for (past, current) in self.past_skin.iter_mut().zip(&self.skin.values) {
                *past = f32::max(*past * keep, *current);
            }
        }
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

        let skin = self
            .skin
            .values
            .iter()
            .zip(&self.past_skin)
            .map(|(current, past)| f32::max(*current, past * PAST_SKIN_WEIGHT))
            .collect::<Vec<_>>();
        let skin = smooth(&skin, SKIN_SMOOTHING);
        // Density that is typical for the colors of the skin
        let skin_sum = skin.iter().sum::<f32>().max(1e-6);
        let typical_density = skin.iter().map(|d| d * d).sum::<f32>() / skin_sum;
        // If unknown colors are kept, there is no limit of the similarity
        let floor = if self.unknown_keep > 0.5 {
            MIN_FLOOR
        } else {
            f32::max(typical_density * self.tightness, MIN_FLOOR)
        };
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
        let skin_floor = floor * (unknown_keep / (1.0 - unknown_keep)).powf(LIKELIHOOD_SOFTNESS);
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
                let score = ((background + floor) / (skin * skin_bias + skin_floor)).ln();
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

    /// Check the tracked hand against the image. Returns for each segment if it is really there (0..1).
    /// `background_kept`: fraction of the colors around the hand that look like the hand. If that is
    /// high, nothing can be said.
    pub fn verify_hand(
        &self,
        camera: &CameraView,
        width: u32,
        height: u32,
        hand: &SampledHand,
        background_kept: f32,
        color: impl Fn(u32, u32) -> [u8; 3],
    ) -> [f32; HAND_SEGMENTS] {
        const MIN_POINTS: usize = 8;

        let Some(alpha) = &self.alpha else {
            return [0.0; HAND_SEGMENTS];
        };
        let reliability = verification_reliability(background_kept);

        hand.shape.segments.map(|[start, end]| {
            let axis = (end - start).truncate();
            let middle = (start + end).truncate() / 2.0;
            let side = axis
                .cross(middle - camera.position)
                .normalize_or(camera.orientation * Vec3::X);

            let mut point_count = 0;
            let mut skin_count = 0;
            for along in [0.1, 0.3, 0.5, 0.7, 0.9] {
                for offset in [-0.6, 0.0, 0.6] {
                    let radius = start.w + (end.w - start.w) * along;
                    let point = start.truncate() + axis * along + side * radius * offset;
                    let Some((pixel, _)) = camera.project(point) else {
                        continue;
                    };
                    if pixel.x < 0.0
                        || pixel.y < 0.0
                        || pixel.x >= width as f32
                        || pixel.y >= height as f32
                    {
                        continue;
                    }
                    point_count += 1;
                    if alpha[texel_index(color(pixel.x as u32, pixel.y as u32))] > 0.5 {
                        skin_count += 1;
                    }
                }
            }
            if point_count < MIN_POINTS {
                return 0.0;
            }

            reliability * smoothstep(0.5, 0.8, skin_count as f32 / point_count as f32)
        })
    }

    /// Score of each candidate direction of the wrist bridge (see `WristAxes`): the share of the points
    /// along it, from a few cm behind the wrist to `length`, whose color the key keeps. The hand pose
    /// must be from the time of the image, like for `verify_hand`. None: no LUT yet, or too few of its
    /// points are in the image and not hidden by the hand.
    #[expect(clippy::too_many_arguments)]
    pub fn bridge_scores(
        &self,
        camera: &CameraView,
        width: u32,
        height: u32,
        hand: &SampledHand,
        wrist_orientation: Quat,
        axes: &WristAxes,
        length: f32,
        color: impl Fn(u32, u32) -> [u8; 3],
    ) -> [Option<f32>; BRIDGE_CANDIDATES] {
        let Some(alpha) = &self.alpha else {
            return [None; BRIDGE_CANDIDATES];
        };
        let distances = bridge_sample_distances(length);

        std::array::from_fn(|index| {
            let direction = wrist_orientation * axes.candidate(index);
            let mut point_count = 0;
            let mut skin_count = 0;
            for distance in &distances {
                let point = hand.wrist + direction * *distance;
                let Some((pixel, _)) = camera.project(point) else {
                    continue;
                };
                if pixel.x < 0.0
                    || pixel.y < 0.0
                    || pixel.x >= width as f32
                    || pixel.y >= height as f32
                {
                    continue;
                }
                // Behind the hand (or in front of it): the pixel shows the hand
                let ray = (point - camera.position).normalize_or_zero();
                if hand.shape.ray_distance(camera.position, ray) < 0.0 {
                    continue;
                }
                point_count += 1;
                if alpha[texel_index(color(pixel.x as u32, pixel.y as u32))] > 0.5 {
                    skin_count += 1;
                }
            }

            // At least half of the points
            (point_count >= 3 && 2 * point_count >= distances.len())
                .then(|| skin_count as f32 / point_count as f32)
        })
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

    /// Same as `kept_fraction`, for the colors around one hand: how much of its background would show
    /// in the key. None without a learned LUT or with too few colors.
    pub fn leak_fraction(&self, colors: &[[u8; 3]]) -> Option<f32> {
        const MIN_COLORS: usize = 50;

        (self.alpha.is_some() && colors.len() >= MIN_COLORS).then(|| self.kept_fraction(colors))
    }
}

/// How much the camera can tell about the hand (0..1): nothing if most of the colors around it look like
/// it (`background_kept`, 0..1)
pub fn verification_reliability(background_kept: f32) -> f32 {
    1.0 - smoothstep(0.35, 0.65, background_kept)
}

fn smoothstep(min: f32, max: f32, value: f32) -> f32 {
    let x = ((value - min) / (max - min)).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
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
    /// Unit direction of the ray through a pixel, world space
    pub fn ray(&self, pixel: Vec2) -> Vec3 {
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
    /// From the hand to the arm: opposite to the fingers (wrist minus middle finger metacarpal)
    pub arm_direction: Vec3,
    /// The fingers are only tracked well enough if the hand doesn't move
    pub is_still: bool,
}

// Candidate directions of the wrist bridge, about the default: bent towards the palm (positive) or the
// back of the hand, and towards the thumb (positive) or the little finger, in degrees
pub const BRIDGE_FLEXIONS: [f32; 7] = [-60.0, -40.0, -20.0, 0.0, 20.0, 40.0, 60.0];
pub const BRIDGE_DEVIATIONS: [f32; 3] = [-25.0, 0.0, 25.0];
pub const BRIDGE_CANDIDATES: usize = BRIDGE_FLEXIONS.len() * BRIDGE_DEVIATIONS.len();
// Points along the bridge: from here (or half of it if it is shorter), about this far apart
const BRIDGE_SAMPLE_START: f32 = 0.03;
const BRIDGE_SAMPLE_SPACING: f32 = 0.01;
const BRIDGE_MIN_SAMPLES: usize = 4;
const BRIDGE_MAX_SAMPLES: usize = 24;

/// Flexion and deviation of a candidate, in degrees
pub fn bridge_candidate_angles(index: usize) -> (f32, f32) {
    (
        BRIDGE_FLEXIONS[index / BRIDGE_DEVIATIONS.len()],
        BRIDGE_DEVIATIONS[index % BRIDGE_DEVIATIONS.len()],
    )
}

/// Distances from the wrist of the points that are checked along a bridge of this length: about 1 cm
/// apart, 6 for 80 mm, more for longer bridges
pub fn bridge_sample_distances(length: f32) -> Vec<f32> {
    let start = BRIDGE_SAMPLE_START.min(0.5 * length);
    let count = (((length - start) / BRIDGE_SAMPLE_SPACING).round().max(0.0) as usize + 1)
        .clamp(BRIDGE_MIN_SAMPLES, BRIDGE_MAX_SAMPLES);

    (0..count)
        .map(|i| start + (length - start) * i as f32 / (count - 1) as f32)
        .collect()
}

/// Axes of the hand in the local space of the wrist joint, to bend the default direction of the forearm
#[derive(Clone, Copy, Debug)]
pub struct WristAxes {
    /// From the hand to the arm: opposite to the fingers (wrist minus middle finger metacarpal)
    pub default: Vec3,
    /// Out of the palm, across `default`
    pub palm: Vec3,
    /// Towards the thumb, across both
    pub thumb: Vec3,
}

impl WristAxes {
    /// Unit vectors from rough directions. None if they are (almost) parallel
    pub fn new(default: Vec3, palm: Vec3, thumb: Vec3) -> Option<Self> {
        let default = default.try_normalize()?;
        let palm = (palm - default * palm.dot(default)).try_normalize()?;
        let side = default.cross(palm);
        let thumb = if side.dot(thumb) < 0.0 { -side } else { side };

        Some(Self {
            default,
            palm,
            thumb,
        })
    }

    /// Unit vector, bent by `flexion` towards the palm and by `deviation` towards the thumb (degrees)
    pub fn direction(&self, flexion: f32, deviation: f32) -> Vec3 {
        let (flexion, deviation) = (flexion.to_radians(), deviation.to_radians());
        let bent = self.default * flexion.cos() + self.palm * flexion.sin();

        bent * deviation.cos() + self.thumb * deviation.sin()
    }

    pub fn candidate(&self, index: usize) -> Vec3 {
        let (flexion, deviation) = bridge_candidate_angles(index);
        self.direction(flexion, deviation)
    }

    /// Flexion and deviation of a direction, in degrees
    pub fn angles(&self, direction: Vec3) -> (f32, f32) {
        let direction = direction.normalize_or(self.default);
        (
            direction
                .dot(self.palm)
                .atan2(direction.dot(self.default))
                .to_degrees(),
            direction
                .dot(self.thumb)
                .clamp(-1.0, 1.0)
                .asin()
                .to_degrees(),
        )
    }
}

/// The best candidate, if there is one. The current one is kept unless another one is better by
/// `BridgeChoice::HYSTERESIS`. Of equal scores the one in the middle of equal ones wins (the bridge is
/// thin, so near the forearm several candidates see only skin), then the least bent one.
pub fn choose_bridge(
    scores: &[Option<f32>; BRIDGE_CANDIDATES],
    current: Option<usize>,
) -> Option<usize> {
    let columns = BRIDGE_DEVIATIONS.len();
    let rank = |index: usize| {
        let (row, column) = (index / columns, index % columns);
        let score = |row: Option<usize>, column: Option<usize>| {
            let index = row? * columns + column?;
            (column? < columns && index < BRIDGE_CANDIDATES)
                .then(|| scores[index])
                .flatten()
        };
        let neighbours = [
            score(row.checked_sub(1), Some(column)),
            score(Some(row + 1), Some(column)),
            score(Some(row), column.checked_sub(1)),
            score(Some(row), Some(column + 1)),
        ]
        .into_iter()
        .flatten()
        .sum::<f32>();
        let (flexion, deviation) = bridge_candidate_angles(index);

        (neighbours, -(flexion.abs() + deviation.abs()))
    };
    let best = (0..BRIDGE_CANDIDATES)
        .filter_map(|index| Some((index, scores[index]?)))
        .reduce(|best, candidate| {
            let is_better =
                candidate.1 > best.1 || (candidate.1 == best.1 && rank(candidate.0) > rank(best.0));
            if is_better { candidate } else { best }
        })?;

    match current.and_then(|index| Some((index, scores[index]?))) {
        Some((index, score)) if best.1 < score + BridgeChoice::HYSTERESIS => Some(index),
        _ => Some(best.0),
    }
}

/// Direction of the wrist bridge chosen by the camera, for one hand
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BridgeChoice {
    /// The chosen candidate
    pub index: Option<usize>,
    /// Smoothed, in the local space of the wrist joint. None: the default direction
    pub direction: Option<Vec3>,
    /// How sure the camera is about it (0..1): the score of the chosen candidate, 0 where the
    /// background looks like the hand
    pub confidence: f32,
}

impl BridgeChoice {
    /// How much better another candidate must be
    pub const HYSTERESIS: f32 = 0.1;
    /// Share of the chosen direction that is taken at each update
    const SMOOTHING: f32 = 0.3;

    /// `scores` of the candidates of `axes`, see `ColorModel::bridge_scores`. `reliability`: see
    /// `verification_reliability`. Returns false if there was nothing to choose from (no fresh data).
    pub fn update(
        &mut self,
        axes: &WristAxes,
        scores: &[Option<f32>; BRIDGE_CANDIDATES],
        reliability: f32,
    ) -> bool {
        let Some(index) = choose_bridge(scores, self.index) else {
            return false;
        };
        let target = axes.candidate(index);
        let current = self.direction.unwrap_or(axes.default);
        self.index = Some(index);
        self.direction = Some(current.lerp(target, Self::SMOOTHING).normalize_or(target));
        self.confidence = scores[index].unwrap_or(0.0) * reliability.clamp(0.0, 1.0);

        true
    }

    /// Flexion and deviation of the smoothed direction (degrees), for the log
    pub fn angles(&self, axes: &WristAxes) -> (f32, f32) {
        self.direction
            .map_or((0.0, 0.0), |direction| axes.angles(direction))
    }
}

#[derive(Default)]
pub struct HandColorSamples {
    pub skin: Vec<[u8; 3]>,
    pub background: Vec<[u8; 3]>,
    /// How many of the background colors belong to each hand, the first hand comes first
    pub background_counts: [usize; 2],
    /// Everything except the hands and arms
    pub scene: Vec<[u8; 3]>,
}

impl HandColorSamples {
    /// The background colors around one hand
    pub fn hand_background(&self, hand: usize) -> &[[u8; 3]] {
        let start = self.background_counts[..hand].iter().sum::<usize>();
        self.background
            .get(start..start + self.background_counts[hand])
            .unwrap_or(&[])
    }
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
    const FINGER_MAX: f32 = -0.004;
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

                let is_skin = hand.shape.palm_ray_distance(camera.position, ray) < SKIN_MAX
                    || (hand.is_still && distance < FINGER_MAX);
                if is_skin {
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
                        samples.background_counts[hand_index] += 1;
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
            ..Default::default()
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
                ..Default::default()
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
    fn similar_colors_are_removed_if_they_were_never_seen() {
        let mut model = ColorModel::default();
        for _ in 0..20 {
            model.add_samples(&samples(SKIN, WHITE_WALL), 0.12);
            model.update_lut();
        }
        let lut = model.update_lut().unwrap();

        // Camera noise
        for color in noisy(SKIN, 200) {
            assert!(alpha(&lut, color) > 200, "{color:?} {}", alpha(&lut, color));
        }
        // Wood, cardboard, mixed colors at edges
        for color in [
            [190, 145, 105],
            [150, 130, 120],
            [110, 75, 45],
            [175, 100, 85],
        ] {
            assert!(alpha(&lut, color) < 30, "{color:?} {}", alpha(&lut, color));
        }
    }

    #[test]
    fn remembers_the_skin_in_other_places() {
        let lit = SKIN.map(|v| (f32::from(v) * 1.4) as u8);
        let mut model = ColorModel::default();
        for _ in 0..20 {
            model.add_samples(&samples(lit, WHITE_WALL), 0.12);
            model.update_lut();
        }
        // 15 seconds in the shade
        let mut lut = vec![];
        for _ in 0..125 {
            model.add_samples(&samples(SKIN, GREEN_WALL), 0.12);
            lut = model.update_lut().unwrap();
        }

        assert!(alpha(&lut, SKIN) > 240);
        assert!(alpha(&lut, lit) > 200, "{}", alpha(&lut, lit));
        assert!(alpha(&lut, WHITE_WALL) < 15);
        assert!(alpha(&lut, GREEN_WALL) < 15);
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
            // Only the scene can remove colors
            tightness: 1e-4,
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
            is_still: false,
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
    fn background_is_split_per_hand() {
        // Two horizontal hands 0.5 m in front of the camera, left and right. The arms point down
        let hand = |x: f32| {
            let segment = [
                Vec4::new(x - 0.03, 0.0, 0.5, 0.02),
                Vec4::new(x + 0.03, 0.0, 0.5, 0.02),
            ];
            SampledHand {
                shape: HandShape {
                    segments: [segment; HAND_SEGMENTS],
                },
                wrist: Vec3::new(x, 0.02, 0.5),
                arm_direction: Vec3::Y,
                is_still: false,
            }
        };
        let camera = CameraView {
            position: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            focal_length: Vec2::splat(217.0),
            center: Vec2::new(160.0, 120.0),
        };

        // Encode the pixel position in the color. 320 doesn't fit in a byte: x / 2
        let result = sample_hand_colors(
            &camera,
            320,
            240,
            &[Some(hand(-0.15)), Some(hand(0.15))],
            |x, y| [(x / 2) as u8, y as u8, 0],
        );
        let counts = result.background_counts;
        assert!(counts[0] > 100 && counts[1] > 100, "{counts:?}");
        assert_eq!(counts[0] + counts[1], result.background.len());

        // Left hand at x = 160 - 65, right hand at x = 160 + 65
        for [x, _, _] in result.hand_background(0) {
            assert!(f32::from(*x) * 2.0 < 160.0, "{x}");
        }
        for [x, _, _] in result.hand_background(1) {
            assert!(f32::from(*x) * 2.0 > 160.0, "{x}");
        }
    }

    #[test]
    fn leak_fraction_needs_a_lut_and_enough_colors() {
        let mut model = ColorModel::default();
        // Without a LUT nothing is known
        assert_eq!(model.leak_fraction(&noisy(SKIN, 200)), None);

        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        model.update_lut().unwrap();
        assert_eq!(model.leak_fraction(&[SKIN; 49]), None);
        assert_eq!(model.leak_fraction(&[SKIN; 200]), Some(1.0));
        assert_eq!(model.leak_fraction(&[WHITE_WALL; 200]), Some(0.0));

        let mut half = vec![SKIN; 100];
        half.extend([WHITE_WALL; 100]);
        assert_eq!(model.leak_fraction(&half), Some(0.5));
    }

    #[test]
    fn verification_finds_misplaced_fingers() {
        // Two fingers, one above the other, 0.5 m in front of the camera
        let finger = |y: f32| {
            [
                Vec4::new(-0.05, y, 0.5, 0.01),
                Vec4::new(0.05, y, 0.5, 0.01),
            ]
        };
        let mut segments = [finger(-0.05); HAND_SEGMENTS];
        segments[HAND_SEGMENTS - 1] = finger(0.05);
        let hand = SampledHand {
            shape: HandShape { segments },
            wrist: Vec3::new(0.05, 0.0, 0.5),
            arm_direction: Vec3::X,
            is_still: true,
        };
        let camera = CameraView {
            position: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            focal_length: Vec2::splat(217.0),
            center: Vec2::new(160.0, 120.0),
        };
        // The image only has the upper finger
        let color = |_: u32, y: u32| {
            if (90..106).contains(&y) {
                SKIN
            } else {
                WHITE_WALL
            }
        };

        let mut model = ColorModel::default();
        assert_eq!(
            model.verify_hand(&camera, 320, 240, &hand, 0.0, color),
            [0.0; HAND_SEGMENTS]
        );

        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        model.update_lut();
        let result = model.verify_hand(&camera, 320, 240, &hand, 0.0, color);
        assert_eq!(result[0], 1.0);
        assert_eq!(result[HAND_SEGMENTS - 1], 0.0);

        // The background looks the same
        let result = model.verify_hand(&camera, 320, 240, &hand, 0.8, color);
        assert_eq!(result[0], 0.0);

        // Outside of the image
        let far = CameraView {
            center: Vec2::new(1000.0, 120.0),
            ..camera
        };
        assert_eq!(model.verify_hand(&far, 320, 240, &hand, 0.0, color)[0], 0.0);
    }

    // A hand 0.5 m in front of the camera, the fingers to the left. In the wrist space (turned in some
    // way) the default direction of the forearm is to the right, the palm and the thumb face these
    // directions (camera space, +y is down in the image).
    fn bridge_scene(palm: Vec3, thumb: Vec3) -> (CameraView, SampledHand, Quat, WristAxes) {
        let camera = CameraView {
            position: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            focal_length: Vec2::splat(217.0),
            center: Vec2::new(160.0, 120.0),
        };
        let wrist = Vec3::new(0.0, 0.0, 0.5);
        let segment = [wrist.extend(0.02), Vec3::new(-0.08, 0.0, 0.5).extend(0.02)];
        let hand = SampledHand {
            shape: HandShape {
                segments: [segment; HAND_SEGMENTS],
            },
            wrist,
            arm_direction: Vec3::X,
            is_still: true,
        };
        let orientation = Quat::from_euler(alvr_common::glam::EulerRot::YXZ, 0.7, -0.4, 1.1);
        let to_local = orientation.inverse();
        let axes = WristAxes::new(to_local * Vec3::X, to_local * palm, to_local * thumb).unwrap();

        (camera, hand, orientation, axes)
    }

    // Skin where the ray passes the hand or a forearm in `direction` (world space, 2.5 cm radius)
    fn forearm_image(
        camera: CameraView,
        hand: SampledHand,
        direction: Option<Vec3>,
    ) -> impl Fn(u32, u32) -> [u8; 3] {
        move |x, y| {
            let ray = camera.ray(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
            let on_arm = direction.is_some_and(|direction| {
                let end = hand.wrist + direction * 0.3;
                ray_capsule_distance(
                    camera.position,
                    ray,
                    hand.wrist.extend(0.025),
                    end.extend(0.025),
                ) < 0.0
            });
            if on_arm || hand.shape.ray_distance(camera.position, ray) < 0.0 {
                SKIN
            } else {
                WHITE_WALL
            }
        }
    }

    #[test]
    fn bridge_candidates_bend_the_default() {
        let (_, _, orientation, axes) = bridge_scene(Vec3::Y, Vec3::NEG_Z);
        assert_eq!(BRIDGE_CANDIDATES, 21);
        let candidates = (0..BRIDGE_CANDIDATES)
            .map(|index| orientation * axes.candidate(index))
            .collect::<Vec<_>>();
        // The default, 40 degrees towards the palm (down in the image), 25 towards the thumb
        assert!((candidates[10] - Vec3::X).length() < 1e-5);
        let (sin, cos) = 40_f32.to_radians().sin_cos();
        assert!((candidates[16] - Vec3::new(cos, sin, 0.0)).length() < 1e-5);
        assert!(candidates[11].z < -0.4);
        for (index, candidate) in candidates.iter().enumerate() {
            assert!((candidate.length() - 1.0).abs() < 1e-5);
            let (flexion, deviation) = axes.angles(orientation.inverse() * *candidate);
            assert_eq!(
                (flexion.round(), deviation.round()),
                bridge_candidate_angles(index)
            );
        }

        // About 1 cm apart from 3 cm on, 6 points for 80 mm
        let distances = bridge_sample_distances(0.08);
        assert_eq!(distances.len(), 6);
        assert!((distances[0] - 0.03).abs() < 1e-6 && (distances[5] - 0.08).abs() < 1e-6);
        assert_eq!(bridge_sample_distances(0.25).len(), 23);
        assert_eq!(bridge_sample_distances(0.02).len(), 4);
    }

    #[test]
    fn bridge_follows_a_bent_forearm() {
        let mut model = ColorModel::default();
        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        model.update_lut();

        // The palm faces down, then the camera. A bend towards the camera can't be seen (any of them
        // covers the forearm in the image), the other one must be found
        let down = (Vec3::Y, Vec3::NEG_Z);
        let front = (Vec3::NEG_Z, Vec3::Y);
        let cases = [
            (down, (40.0, 0.0)),
            (down, (-60.0, 0.0)),
            (down, (0.0, 0.0)),
            (down, (-20.0, 25.0)),
            (front, (0.0, 25.0)),
            (front, (20.0, -25.0)),
        ];
        for ((palm, thumb), (flexion, deviation)) in cases {
            let (camera, hand, orientation, axes) = bridge_scene(palm, thumb);
            for length in [0.08, 0.25] {
                let direction = orientation * axes.direction(flexion, deviation);
                let image = forearm_image(camera, hand, Some(direction));
                let scores = model.bridge_scores(
                    &camera,
                    320,
                    240,
                    &hand,
                    orientation,
                    &axes,
                    length,
                    &image,
                );
                let mut choice = BridgeChoice::default();
                for _ in 0..15 {
                    assert!(choice.update(&axes, &scores, 1.0));
                }

                // Within one step of the real bend
                let (chosen_flexion, chosen_deviation) =
                    bridge_candidate_angles(choice.index.unwrap());
                let (smooth_flexion, smooth_deviation) = choice.angles(&axes);
                let label = format!(
                    "{flexion} {deviation} {length}: chose {chosen_flexion} {chosen_deviation}, \
                     smoothed {smooth_flexion} {smooth_deviation}, {scores:.2?}"
                );
                if palm == down.0 {
                    assert!((chosen_flexion - flexion).abs() <= 20.0, "{label}");
                    assert!((smooth_flexion - flexion).abs() <= 21.0, "{label}");
                } else {
                    assert!((chosen_deviation - deviation).abs() <= 25.0, "{label}");
                    assert!((smooth_deviation - deviation).abs() <= 26.0, "{label}");
                }
                assert!(choice.confidence > 0.9, "{label}: {}", choice.confidence);

                // The background looks like the hand: no confidence
                let mut unsure = choice;
                unsure.update(&axes, &scores, verification_reliability(0.8));
                assert_eq!(unsure.confidence, 0.0);
            }
        }
    }

    #[test]
    fn bridge_without_a_visible_forearm_is_unsure() {
        let (camera, hand, orientation, axes) = bridge_scene(Vec3::Y, Vec3::NEG_Z);
        let mut model = ColorModel::default();
        let image = forearm_image(camera, hand, None);
        let scores = |model: &ColorModel, camera: &CameraView| {
            model.bridge_scores(camera, 320, 240, &hand, orientation, &axes, 0.08, &image)
        };

        // Nothing learned yet
        assert_eq!(scores(&model, &camera), [None; BRIDGE_CANDIDATES]);
        assert!(!BridgeChoice::default().update(&axes, &scores(&model, &camera), 1.0));

        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        model.update_lut();
        let mut choice = BridgeChoice::default();
        assert!(choice.update(&axes, &scores(&model, &camera), 1.0));
        assert!(choice.confidence < 0.2, "{}", choice.confidence);
        // With equal scores the default
        assert_eq!(choice.index.map(bridge_candidate_angles), Some((0.0, 0.0)));

        // Out of the image
        let far = CameraView {
            center: Vec2::new(1000.0, 120.0),
            ..camera
        };
        assert_eq!(scores(&model, &far), [None; BRIDGE_CANDIDATES]);
    }

    #[test]
    fn bridge_choice_has_hysteresis() {
        let (_, _, _, axes) = bridge_scene(Vec3::Y, Vec3::NEG_Z);
        let mut scores = [Some(0.2); BRIDGE_CANDIDATES];
        scores[10] = Some(0.8);
        let mut choice = BridgeChoice::default();
        choice.update(&axes, &scores, 1.0);
        assert_eq!(choice.index, Some(10));

        // Slightly better elsewhere: kept
        scores[16] = Some(0.85);
        choice.update(&axes, &scores, 1.0);
        assert_eq!(choice.index, Some(10));
        assert_eq!(choice.confidence, 0.8);
        assert_eq!(choose_bridge(&scores, None), Some(16));

        // Better by more than the hysteresis: changed
        scores[16] = Some(0.95);
        choice.update(&axes, &scores, 1.0);
        assert_eq!(choice.index, Some(16));
        assert_eq!(choice.confidence, 0.95);

        // The current one can't be seen: the best one
        scores[16] = None;
        assert_eq!(choose_bridge(&scores, Some(16)), Some(10));

        // The direction moves there step by step
        let target = axes.candidate(16);
        let mut choice = BridgeChoice::default();
        scores[16] = Some(1.0);
        choice.update(&axes, &scores, 1.0);
        let first = choice.direction.unwrap();
        assert!(first.angle_between(axes.default) > 0.05);
        assert!(first.angle_between(target) > 0.3);
        for _ in 0..20 {
            choice.update(&axes, &scores, 1.0);
        }
        assert!(choice.direction.unwrap().angle_between(target) < 0.01);
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
