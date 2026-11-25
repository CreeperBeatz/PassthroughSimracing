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
// from the colors seen by the compositor (about one texel). The one of the skin is
// `ColorModel::skin_smoothing`, within these limits
const MIN_SKIN_SMOOTHING: f32 = 0.7;
// Wider than 1 keeps more colors around the skin colors: no use
const MAX_SKIN_SMOOTHING: f32 = 1.0;
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
// Fewer colors around a hand tell nothing about its background
const MIN_RING_COLORS: usize = 50;
// A segment is only checked if this many of its points are in the image
const MIN_SEGMENT_POINTS: usize = 8;
/// The values of `skin_bias` that `AutoBias` chooses from
pub const AUTO_BIAS_CANDIDATES: [f32; 8] = [0.30, 0.45, 0.60, 0.80, 1.00, 1.25, 1.50, 2.00];
// Another candidate is only chosen if its score is better by more than this
const AUTO_BIAS_HYSTERESIS: f32 = 0.01;
// Scores this close are equal (rounding of the sums)
const AUTO_BIAS_TIE: f32 = 1e-4;
// Share of the way (in log space) from the bias in use to the chosen candidate at each update
const AUTO_BIAS_STEP: f32 = 0.25;

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

/// `debug.alvr.key_auto`: the model chooses `skin_bias` itself at each LUT update, from the colours of
/// the hands, around them and of the scene. Of `AUTO_BIAS_CANDIDATES` the one with the best
/// skin kept - `weight` x leak, where the leak is the larger of the background kept and `scene_share`
/// x the scene kept. The bias in use moves towards it gradually.
#[derive(Clone, Copy, Debug)]
pub struct AutoBias {
    /// How much a leak costs against the skin that is kept
    pub weight: f32,
    /// How much of the scene that is kept counts as a leak (0..1)
    pub scene_share: f32,
    // Index of the chosen candidate
    chosen: Option<usize>,
    status: Option<AutoBiasStatus>,
}

/// What `AutoBias` chose at the last LUT update
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutoBiasStatus {
    /// The `skin_bias` of the last LUT
    pub bias: f32,
    /// The chosen candidate, which the bias moves towards
    pub candidate: f32,
    /// Of the chosen candidate: the shares of the skin, background and scene densities that its targets
    /// keep, and the leak it was judged by
    pub skin_kept: f32,
    pub background_kept: f32,
    pub scene_kept: f32,
    pub leak: f32,
}

impl AutoBias {
    pub fn new(weight: f32, scene_share: f32) -> Self {
        Self {
            weight,
            scene_share,
            chosen: None,
            status: None,
        }
    }

    /// None before the first LUT
    pub fn status(&self) -> Option<AutoBiasStatus> {
        self.status
    }

    // `kept`: for each candidate the kept shares of skin, background and scene (see `kept_shares`).
    // Returns the bias for this LUT
    fn update(&mut self, kept: &[[f32; 3]; AUTO_BIAS_CANDIDATES.len()]) -> f32 {
        let leak = |kept: &[f32; 3]| kept[1].max(self.scene_share * kept[2]);
        let score = |index: usize| kept[index][0] - self.weight * leak(&kept[index]);
        // Of equal ones the one that prefers the skin most: e.g. when all of them keep all of the skin and
        // none of the room, which a higher bias does with more of the skin colours that were not seen
        let max_score = (0..kept.len()).map(score).fold(f32::MIN, f32::max);
        let best = (0..kept.len())
            .rev()
            .find(|index| score(*index) >= max_score - AUTO_BIAS_TIE)
            .unwrap_or(0);
        let chosen = match self.chosen {
            Some(current) if score(best) <= score(current) + AUTO_BIAS_HYSTERESIS => current,
            _ => best,
        };
        let candidate = AUTO_BIAS_CANDIDATES[chosen];
        let bias = self
            .status
            .map_or(candidate, |status| {
                (status.bias.ln() + (candidate.ln() - status.bias.ln()) * AUTO_BIAS_STEP).exp()
            })
            .clamp(
                AUTO_BIAS_CANDIDATES[0],
                AUTO_BIAS_CANDIDATES[kept.len() - 1],
            );
        self.chosen = Some(chosen);
        self.status = Some(AutoBiasStatus {
            bias,
            candidate,
            skin_kept: kept[chosen][0],
            background_kept: kept[chosen][1],
            scene_kept: kept[chosen][2],
            leak: leak(&kept[chosen]),
        });

        bias
    }
}

// For each of `AUTO_BIAS_CANDIDATES`: the shares of the `skin`, `background` and `scene` densities that
// its targets keep, 0 for a density that is all zeros. `densities` gives the skin and background of a
// cell as the target sees them, `target` the target of these with a bias. Cells where all three are 0
// are skipped
fn kept_shares(
    skin: &[f32],
    background: &[f32],
    scene: &[f32],
    densities: impl Fn(usize) -> (f32, f32),
    target: impl Fn(f32, f32, f32) -> f32,
) -> [[f32; 3]; AUTO_BIAS_CANDIDATES.len()] {
    let mut sums = [[0.0_f64; 3]; AUTO_BIAS_CANDIDATES.len()];
    let mut totals = [0.0_f64; 3];
    for i in 0..TEXEL_COUNT {
        let weights = [skin[i], background[i], scene[i]].map(f64::from);
        if weights == [0.0; 3] {
            continue;
        }
        for (total, weight) in totals.iter_mut().zip(weights) {
            *total += weight;
        }
        let (skin_left, background) = densities(i);
        // Without skin the bias changes nothing
        let same = (skin_left == 0.0).then(|| target(0.0, background, 1.0));
        for (sums, bias) in sums.iter_mut().zip(AUTO_BIAS_CANDIDATES) {
            let kept = f64::from(same.unwrap_or_else(|| target(skin_left, background, bias)));
            for (sum, weight) in sums.iter_mut().zip(weights) {
                *sum += weight * kept;
            }
        }
    }

    sums.map(|sums| {
        [0, 1, 2].map(|k| {
            if totals[k] > 0.0 {
                (sums[k] / totals[k]) as f32
            } else {
                0.0
            }
        })
    })
}

// Move the alpha of a table towards the new targets, or start with them
fn blend(alpha: Option<Vec<f32>>, targets: Vec<f32>) -> Vec<f32> {
    if let Some(mut alpha) = alpha {
        for (alpha, target) in alpha.iter_mut().zip(targets) {
            *alpha += (target - *alpha) * BLEND;
        }
        alpha
    } else {
        targets
    }
}

pub struct ColorModel {
    skin: Density,
    past_skin: Vec<f32>,
    /// Around the hands
    background: Density,
    past_background: Vec<f32>,
    scene: Density,
    alpha: Option<Vec<f32>>,
    /// Like `alpha` with `confirm_tightness`, never uploaded. None while it is off
    confirm: Option<Vec<f32>>,
    /// Cells that the last table would keep if no background or scene color was seen there, for the log
    kept_without_background: Vec<bool>,
    /// Probability (0..1) that a color that is seen neither on the hand nor in the background belongs to
    /// the hand. High values keep e.g. watches and lit fingertips, low values remove clutter.
    pub unknown_keep: f32,
    /// How much skin is preferred if skin and background look the same. High values avoid holes in the
    /// hand, low values avoid leaks.
    pub skin_bias: f32,
    /// How similar to the skin a color must be, as a fraction of the typical density of the skin colors.
    /// Low values accept similar colors that were never seen, which shows complex backgrounds.
    pub tightness: f32,
    /// Width of the smoothing of the skin colors, in LUT texels (0.7..1.5). The tightness is scaled by
    /// its cube: a narrower one spreads less of each skin color to its neighbours, so rarer skin colors
    /// can be kept without keeping more around the common ones.
    pub skin_smoothing: f32,
    /// Tightness of a second table that only confirms the hand segments (see `segment_shares`), with
    /// the same colors and formula. Below `tightness` it keeps everything the key keeps and more of
    /// the rare skin colors. 0: off, the key table is used.
    pub confirm_tightness: f32,
    /// `debug.alvr.key_auto`: chooses the bias instead of `skin_bias`. None: off, `skin_bias` is used
    pub auto_bias: Option<AutoBias>,
}

impl Default for ColorModel {
    fn default() -> Self {
        Self {
            skin: Density::new(SKIN_MEMORY),
            background: Density::new(BACKGROUND_MEMORY),
            past_background: vec![0.0; TEXEL_COUNT],
            scene: Density::new(SCENE_MEMORY),
            alpha: None,
            confirm: None,
            kept_without_background: vec![false; TEXEL_COUNT],
            unknown_keep: 0.1,
            skin_bias: 1.5,
            past_skin: vec![0.0; TEXEL_COUNT],
            tightness: 0.2,
            skin_smoothing: 1.0,
            confirm_tightness: 0.0,
            auto_bias: None,
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
        let skin_smoothing = self
            .skin_smoothing
            .clamp(MIN_SKIN_SMOOTHING, MAX_SKIN_SMOOTHING);
        let skin = smooth(&skin, skin_smoothing);
        // Density that is typical for the colors of the skin
        let skin_sum = skin.iter().sum::<f32>().max(1e-6);
        let typical_density = skin.iter().map(|d| d * d).sum::<f32>() / skin_sum;
        // If unknown colors are kept, there is no limit of the similarity
        let floor_for = |tightness: f32| {
            if self.unknown_keep > 0.5 {
                MIN_FLOOR
            } else {
                f32::max(
                    typical_density * tightness * skin_smoothing.powi(3),
                    MIN_FLOOR,
                )
            }
        };
        let floor = floor_for(self.tightness);
        // Never stricter than the key, so that it keeps at least what the key keeps
        let confirm_floor = (self.confirm_tightness > 0.0)
            .then(|| floor_for(self.confirm_tightness.min(self.tightness)));
        let background = self
            .background
            .values
            .iter()
            .zip(&self.past_background)
            .map(|(current, past)| f32::max(*current, past * PAST_BACKGROUND_WEIGHT))
            .collect::<Vec<_>>();
        let background = smooth(&background, BACKGROUND_SMOOTHING);
        let scene = smooth(&self.scene.values, BACKGROUND_SMOOTHING);
        let unknown_keep = self.unknown_keep.clamp(0.01, 0.99);
        let unknown_ratio = (unknown_keep / (1.0 - unknown_keep)).powf(LIKELIHOOD_SOFTNESS);
        let skin_floor = floor * unknown_ratio;
        let target_with_bias = |skin: f32, background: f32, floor: f32, skin_bias: f32| {
            let score = ((background + floor) / (skin * skin_bias + floor * unknown_ratio)).ln();
            let keep = 1.0 / (1.0 + (score / LIKELIHOOD_SOFTNESS).clamp(-30.0, 30.0).exp());
            // Make it steeper
            ((keep - 0.3) / 0.4).clamp(0.0, 1.0)
        };
        // Remove the part of each that can be explained by samples of the other one
        let densities = |i: usize| {
            let scene = (scene[i] * SCENE_WEIGHT - skin[i] * SCENE_CONTAMINATION).max(0.0);
            (
                (skin[i] - background[i] * CONTAMINATION).max(0.0),
                (background[i] - skin[i] * CONTAMINATION).max(scene),
            )
        };
        // Chosen from how much of the current colours of the skin, around the hands and of the scene
        // (not smoothed, the scene not reduced by the skin) each candidate would keep
        let skin_bias = match &mut self.auto_bias {
            Some(auto_bias) => auto_bias.update(&kept_shares(
                &self.skin.values,
                &self.background.values,
                &self.scene.values,
                densities,
                |skin, background, bias| target_with_bias(skin, background, floor, bias),
            )),
            None => self.skin_bias,
        };
        let target = |skin: f32, background: f32, floor: f32| {
            target_with_bias(skin, background, floor, skin_bias)
        };

        let mut targets = Vec::with_capacity(TEXEL_COUNT);
        let mut confirm_targets = Vec::with_capacity(confirm_floor.map_or(0, |_| TEXEL_COUNT));
        for (i, kept_without_background) in self.kept_without_background.iter_mut().enumerate() {
            let (skin_left, background) = densities(i);
            targets.push(target(skin_left, background, floor));
            if let Some(confirm_floor) = confirm_floor {
                confirm_targets.push(target(skin_left, background, confirm_floor));
            }
            // Same as a target above 0.5 with no background and no scene
            *kept_without_background = skin[i] * skin_bias + skin_floor > floor;
        }

        // The confirm table starts from the key table, so that it always keeps at least as much
        self.confirm = confirm_floor.map(|_| {
            let start = self.confirm.take().or_else(|| self.alpha.clone());
            blend(start, confirm_targets)
        });
        let alpha = blend(self.alpha.take(), targets);

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

    /// Check the tracked hand against the image: for each segment, the share of its points whose color
    /// is kept (0..1), see `verification_weight`. The points on its axis are checked with the confirm
    /// table if it is on, the points beside the axis always with the key table, so that a hand tracked
    /// beside the real one is found as well as with the key alone. None for segments that are not
    /// `eligible` or have too few points in the image. None without a LUT.
    pub fn segment_shares(
        &self,
        camera: &CameraView,
        width: u32,
        height: u32,
        hand: &SampledHand,
        eligible: &[bool; HAND_SEGMENTS],
        color: impl Fn(u32, u32) -> [u8; 3],
    ) -> Option<[Option<f32>; HAND_SEGMENTS]> {
        let alpha = self.alpha.as_ref()?;
        let confirm = self.confirm.as_ref().unwrap_or(alpha);

        Some(std::array::from_fn(|segment| {
            if !eligible[segment] {
                return None;
            }
            let [start, end] = hand.shape.segments[segment];
            let axis = (end - start).truncate();
            let middle = (start + end).truncate() / 2.0;
            let side = axis
                .cross(middle - camera.position)
                .normalize_or(camera.orientation * Vec3::X);

            let mut point_count = 0;
            let mut skin_count = 0;
            for along in [0.1, 0.3, 0.5, 0.7, 0.9] {
                for (offset, table) in [(-0.6, alpha), (0.0, confirm), (0.6, alpha)] {
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
                    if table[texel_index(color(pixel.x as u32, pixel.y as u32))] > 0.5 {
                        skin_count += 1;
                    }
                }
            }

            (point_count >= MIN_SEGMENT_POINTS).then(|| skin_count as f32 / point_count as f32)
        }))
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

    /// The alpha of the key table as `segment_shares` and `kept_fraction` use it, one byte per cell in
    /// the order of the LUT. The published LUT only follows it by steps of more than `MIN_LUT_CHANGE`
    /// of the client. None without a LUT
    pub fn key_table(&self) -> Option<Vec<u8>> {
        self.alpha.as_deref().map(table_bytes)
    }

    /// The alpha of the confirm table, one byte per cell in the order of the LUT. None while it is off
    pub fn confirm_table(&self) -> Option<Vec<u8>> {
        self.confirm.as_deref().map(table_bytes)
    }

    /// Fraction of the colors that are kept by the current LUT, for diagnostics
    pub fn kept_fraction(&self, colors: &[[u8; 3]]) -> f32 {
        self.alpha
            .as_ref()
            .map_or(1.0, |alpha| kept_by(alpha, colors))
    }

    /// Same as `kept_fraction`, for the colors around one hand: how much of its background would show
    /// in the key. None without a learned LUT or with too few colors.
    pub fn leak_fraction(&self, colors: &[[u8; 3]]) -> Option<f32> {
        (self.alpha.is_some() && colors.len() >= MIN_RING_COLORS)
            .then(|| self.kept_fraction(colors))
    }

    /// Same as `kept_fraction`, with the confirm table if it is on: what `segment_shares` sees as skin
    pub fn confirm_kept_fraction(&self, colors: &[[u8; 3]]) -> f32 {
        match &self.confirm {
            Some(confirm) => kept_by(confirm, colors),
            None => self.kept_fraction(colors),
        }
    }

    /// Same as `leak_fraction`, with the confirm table if it is on
    pub fn confirm_leak_fraction(&self, colors: &[[u8; 3]]) -> Option<f32> {
        (self.alpha.is_some() && colors.len() >= MIN_RING_COLORS)
            .then(|| self.confirm_kept_fraction(colors))
    }

    /// Of the colors, the fractions that the key doesn't keep: [too rare as skin colors, removed because
    /// of the background or the scene]. The second ones would be kept by the last table if no
    /// background or scene color was seen there. For diagnostics, [0, 0] without a LUT.
    pub fn lost_fractions(&self, colors: &[[u8; 3]]) -> [f32; 2] {
        let Some(alpha) = &self.alpha else {
            return [0.0; 2];
        };
        let mut lost = [0; 2];
        for color in colors {
            let index = texel_index(*color);
            if alpha[index] <= 0.5 {
                lost[usize::from(self.kept_without_background[index])] += 1;
            }
        }

        lost.map(|count| count as f32 / colors.len().max(1) as f32)
    }

    /// What `auto_bias` chose at the last LUT update. None while it is off or before the first LUT
    pub fn auto_bias_status(&self) -> Option<AutoBiasStatus> {
        self.auto_bias.as_ref().and_then(AutoBias::status)
    }

    /// How much of the room the current LUT keeps, measured on the colours of a pass: the larger of the
    /// share of the `background` colours and `scene_share` x the share of the `scene` colours that it
    /// keeps (as `kept_fraction`). Without colours of one kind only the other one counts. None without a
    /// LUT or without any colours
    pub fn measured_leak(
        &self,
        background: &[[u8; 3]],
        scene: &[[u8; 3]],
        scene_share: f32,
    ) -> Option<f32> {
        let alpha = self.alpha.as_ref()?;
        let background = (!background.is_empty()).then(|| kept_by(alpha, background));
        let scene = (!scene.is_empty()).then(|| scene_share * kept_by(alpha, scene));

        match (background, scene) {
            (Some(background), Some(scene)) => Some(background.max(scene)),
            (background, scene) => background.or(scene),
        }
    }
}

fn table_bytes(alpha: &[f32]) -> Vec<u8> {
    alpha
        .iter()
        .map(|alpha| (alpha * 255.0).round() as u8)
        .collect()
}

fn kept_by(alpha: &[f32], colors: &[[u8; 3]]) -> f32 {
    let kept = colors
        .iter()
        .filter(|color| alpha[texel_index(**color)] > 0.5)
        .count();

    kept as f32 / colors.len().max(1) as f32
}

/// How much the camera can tell about the hand (0..1): nothing if most of the colors around it look like
/// it (`background_kept`, 0..1)
pub fn verification_reliability(background_kept: f32) -> f32 {
    1.0 - smoothstep(0.35, 0.65, background_kept)
}

/// Weight of a segment for the solid fill (0..1): from its share (see `ColorModel::segment_shares`, None:
/// 0) and the `verification_reliability`
pub fn verification_weight(share: Option<f32>, reliability: f32) -> f32 {
    share.map_or(0.0, |share| reliability * smoothstep(0.5, 0.8, share))
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
    /// background looks like the hand or while the smoothed direction is still far from the candidate
    pub confidence: f32,
}

impl BridgeChoice {
    /// How much better another candidate must be
    pub const HYSTERESIS: f32 = 0.1;
    /// Share of the chosen direction that is taken at each update
    const SMOOTHING: f32 = 0.3;

    /// `scores` of the candidates of `axes`, see `ColorModel::bridge_scores`. `reliability`: see
    /// `verification_reliability`. `follow`: below this score (times the reliability) the camera doesn't
    /// really see the forearm, the direction goes back to the default. Returns false if there was nothing
    /// to choose from (no fresh data).
    pub fn update(
        &mut self,
        axes: &WristAxes,
        scores: &[Option<f32>; BRIDGE_CANDIDATES],
        reliability: f32,
        follow: f32,
    ) -> bool {
        let Some(index) = choose_bridge(scores, self.index) else {
            return false;
        };
        let score = scores[index].unwrap_or(0.0) * reliability.clamp(0.0, 1.0);
        // Not sure: not towards what only looks most like skin (a sleeve, a skin colored background)
        let (target, score) = if score >= follow {
            (axes.candidate(index), score)
        } else {
            (axes.default, 0.0)
        };
        let current = self.direction.unwrap_or(axes.default);
        let direction = current.lerp(target, Self::SMOOTHING).normalize_or(target);
        self.index = Some(index);
        self.direction = Some(direction);
        // The camera only checked the target: no confidence until the smoothed direction is near it
        let off = direction.angle_between(target).to_degrees();
        self.confidence = score * (1.0 - smoothstep(5.0, 15.0, off));

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

// Distances from the surface of the hand, with a margin for tracking and timing errors
const SKIN_MAX: f32 = -0.005;
const FINGER_MAX: f32 = -0.004;
const BACKGROUND_MIN: f32 = 0.02;
const BACKGROUND_MAX: f32 = 0.07;

/// Collect the colors inside of the hands, and around them. `color` returns the RGB color of a pixel.
pub fn sample_hand_colors(
    camera: &CameraView,
    width: u32,
    height: u32,
    hands: &[Option<SampledHand>; 2],
    color: impl Fn(u32, u32) -> [u8; 3],
) -> HandColorSamples {
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
        let start = samples.background.len();
        sample_hand(
            camera,
            width,
            height,
            hand,
            hands[1 - hand_index].as_ref(),
            &color,
            Some(&mut samples.skin),
            &mut samples.background,
        );
        samples.background_counts[hand_index] = samples.background.len() - start;
    }

    samples
}

/// Only the colors around one hand, like the background of `sample_hand_colors`: for a hand whose
/// colors are not learned. `other_hand` is not background.
pub fn sample_ring_colors(
    camera: &CameraView,
    width: u32,
    height: u32,
    hand: &SampledHand,
    other_hand: Option<&SampledHand>,
    color: impl Fn(u32, u32) -> [u8; 3],
) -> Vec<[u8; 3]> {
    let mut background = Vec::new();
    sample_hand(
        camera,
        width,
        height,
        hand,
        other_hand,
        &color,
        None,
        &mut background,
    );

    background
}

// Skin (if wanted) and background colors of one hand
#[expect(clippy::too_many_arguments)]
fn sample_hand(
    camera: &CameraView,
    width: u32,
    height: u32,
    hand: &SampledHand,
    other_hand: Option<&SampledHand>,
    color: &impl Fn(u32, u32) -> [u8; 3],
    mut skin: Option<&mut Vec<[u8; 3]>>,
    background: &mut Vec<[u8; 3]>,
) {
    const STEP: usize = 2;

    let bounds = hand.shape.bounds();
    let Some((center, depth)) = camera.project(bounds.truncate()) else {
        return;
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

            // Without skin the palm test is not needed: skin is never in the ring
            if let Some(skin) = skin.as_deref_mut() {
                let is_skin = hand.shape.palm_ray_distance(camera.position, ray) < SKIN_MAX
                    || (hand.is_still && distance < FINGER_MAX);
                if is_skin {
                    skin.push(color(x, y));
                    continue;
                }
            }
            if (BACKGROUND_MIN..BACKGROUND_MAX).contains(&distance) {
                // The arm is not background
                let to_wrist = hand.wrist - camera.position;
                let point = camera.position + ray * to_wrist.dot(ray);
                let is_arm = (point - hand.wrist).dot(hand.arm_direction) > -0.03;
                let is_other_hand = other_hand.is_some_and(|other| {
                    other.shape.ray_distance(camera.position, ray) < BACKGROUND_MIN
                        || (point - other.wrist).length() < 0.15
                });
                if !is_arm && !is_other_hand {
                    background.push(color(x, y));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HAND_SEGMENTS;
    use alvr_common::glam::Vec4;

    const SKIN: [u8; 3] = [150, 110, 85];
    const WHITE_WALL: [u8; 3] = [105, 92, 80];
    const GREEN_WALL: [u8; 3] = [62, 56, 10];
    // The default of debug.alvr.key_wrist_follow
    const FOLLOW: f32 = 0.3;

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

    // The next alpha of the key table, with the formula from before `skin_smoothing`
    fn reference_alpha(model: &ColorModel) -> Vec<f32> {
        let skin = model
            .skin
            .values
            .iter()
            .zip(&model.past_skin)
            .map(|(current, past)| f32::max(*current, past * PAST_SKIN_WEIGHT))
            .collect::<Vec<_>>();
        let skin = smooth(&skin, 1.0);
        let skin_sum = skin.iter().sum::<f32>().max(1e-6);
        let typical_density = skin.iter().map(|d| d * d).sum::<f32>() / skin_sum;
        let floor = if model.unknown_keep > 0.5 {
            MIN_FLOOR
        } else {
            f32::max(typical_density * model.tightness, MIN_FLOOR)
        };
        let background = model
            .background
            .values
            .iter()
            .zip(&model.past_background)
            .map(|(current, past)| f32::max(*current, past * PAST_BACKGROUND_WEIGHT))
            .collect::<Vec<_>>();
        let background = smooth(&background, BACKGROUND_SMOOTHING);
        let scene = smooth(&model.scene.values, BACKGROUND_SMOOTHING);
        let unknown_keep = model.unknown_keep.clamp(0.01, 0.99);
        let skin_floor = floor * (unknown_keep / (1.0 - unknown_keep)).powf(LIKELIHOOD_SOFTNESS);

        (0..TEXEL_COUNT)
            .map(|i| {
                let scene = (scene[i] * SCENE_WEIGHT - skin[i] * SCENE_CONTAMINATION).max(0.0);
                let (skin, background) = (
                    (skin[i] - background[i] * CONTAMINATION).max(0.0),
                    (background[i] - skin[i] * CONTAMINATION).max(scene),
                );
                let score = ((background + floor) / (skin * model.skin_bias + skin_floor)).ln();
                let keep = 1.0 / (1.0 + (score / LIKELIHOOD_SOFTNESS).clamp(-30.0, 30.0).exp());
                let target = ((keep - 0.3) / 0.4).clamp(0.0, 1.0);
                model
                    .alpha
                    .as_ref()
                    .map_or(target, |alpha| alpha[i] + (target - alpha[i]) * BLEND)
            })
            .collect()
    }

    #[test]
    fn default_skin_kernel_is_unchanged() {
        let bright = |c: [u8; 3]| c.map(|v| (f32::from(v) * 1.5).min(255.0) as u8);
        let mut mixed = samples(SKIN, GREEN_WALL);
        mixed.skin.extend(noisy(GREEN_WALL, 150));
        mixed.background.extend(noisy(SKIN, 150));
        mixed.scene = noisy([200, 30, 40], 300);
        let steps = [
            (samples(SKIN, WHITE_WALL), 3),
            (samples(SKIN, SKIN), 2),
            (mixed, 3),
            (samples(bright(SKIN), bright(GREEN_WALL)), 3),
        ];
        // The confirm table doesn't change the key table
        for (unknown_keep, skin_bias, tightness, confirm_tightness) in [
            (0.1, 1.5, 0.3, 0.0),
            (0.1, 1.5, 0.3, 0.12),
            (0.95, 3.0, 1e-4, 0.0),
        ] {
            let mut model = ColorModel {
                unknown_keep,
                skin_bias,
                tightness,
                skin_smoothing: 1.0,
                confirm_tightness,
                ..Default::default()
            };
            for (samples, count) in &steps {
                for _ in 0..*count {
                    model.add_samples(samples, 0.12);
                    let expected = reference_alpha(&model);
                    model.update_lut().unwrap();
                    assert!(model.alpha.as_ref().unwrap() == &expected);
                }
            }
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

    // The client's defaults of `key_tight` and `key_confirm`, `key_auto` with `weight` and `scene_share`
    fn auto_model(weight: f32, scene_share: f32) -> ColorModel {
        ColorModel {
            tightness: 0.3,
            confirm_tightness: 0.12,
            auto_bias: Some(AutoBias::new(weight, scene_share)),
            ..Default::default()
        }
    }

    // Hand and room in distant colours
    fn apart_samples() -> HandColorSamples {
        let mut samples = samples(SKIN, GREEN_WALL);
        samples.scene = [GREEN_WALL, [200, 30, 40], [30, 90, 200]]
            .iter()
            .flat_map(|color| noisy(*color, 200))
            .collect();
        samples
    }

    // Skin of two tones; around the hands mostly the main tone (denser there than on the hand) and a wall,
    // the same colours in the scene
    const DARK_SKIN: [u8; 3] = [118, 78, 58];
    fn skin_like_samples() -> HandColorSamples {
        let mut skin = noisy(SKIN, 450);
        skin.extend(noisy(DARK_SKIN, 150));
        let mut background = noisy(SKIN, 480);
        background.extend(noisy(WHITE_WALL, 120));
        HandColorSamples {
            skin,
            scene: background.clone(),
            background,
            ..Default::default()
        }
    }

    #[test]
    fn key_auto_off_is_todays_lut() {
        let mut mixed = samples(SKIN, GREEN_WALL);
        mixed.skin.extend(noisy(GREEN_WALL, 150));
        mixed.background.extend(noisy(SKIN, 150));
        mixed.scene = noisy([200, 30, 40], 300);
        let steps = [
            (samples(SKIN, WHITE_WALL), 3),
            (skin_like_samples(), 3),
            (mixed, 3),
            (apart_samples(), 3),
        ];
        let mut today = ColorModel {
            tightness: 0.3,
            confirm_tightness: 0.12,
            ..Default::default()
        };
        // Off unless it is set
        assert!(ColorModel::default().auto_bias.is_none());
        assert!(today.auto_bias.is_none());
        let mut on = auto_model(3.0, 0.5);
        for (samples, count) in &steps {
            for _ in 0..*count {
                today.add_samples(samples, 0.12);
                on.add_samples(samples, 0.12);
                // The formula from before `key_auto`, with `skin_bias`
                let expected = reference_alpha(&today);
                let lut = today.update_lut().unwrap();
                assert!(today.alpha.as_ref().unwrap() == &expected);
                let bytes = expected
                    .iter()
                    .map(|alpha| (alpha * 255.0).round() as u8)
                    .collect::<Vec<_>>();
                assert!(lut.chunks(4).map(|texel| texel[3]).eq(bytes));
                assert_eq!(today.auto_bias_status(), None);
                on.update_lut().unwrap();
            }
        }
        // Not the same with it on (the skin-like steps choose another bias)
        assert!(on.auto_bias_status().is_some());
        assert!(on.alpha != today.alpha);
    }

    #[test]
    fn key_auto_prefers_skin_when_colours_are_apart() {
        let mut model = auto_model(3.0, 0.5);
        assert_eq!(model.auto_bias_status(), None);
        for _ in 0..20 {
            model.add_samples(&apart_samples(), 0.12);
            model.update_lut().unwrap();
        }
        let status = model.auto_bias_status().unwrap();
        assert!(status.candidate >= 1.5, "{status:?}");
        assert!(status.skin_kept >= 0.9, "{status:?}");
        assert!(status.leak <= 0.02, "{status:?}");
        assert!(status.bias >= 1.5, "{status:?}");
    }

    #[test]
    fn key_auto_goes_strict_when_the_background_is_skin_like() {
        let samples = skin_like_samples();
        let mut fixed = ColorModel {
            tightness: 0.3,
            confirm_tightness: 0.12,
            ..Default::default()
        };
        let mut auto = auto_model(3.0, 0.5);
        for _ in 0..30 {
            fixed.add_samples(&samples, 0.12);
            fixed.update_lut().unwrap();
            auto.add_samples(&samples, 0.12);
            auto.update_lut().unwrap();
        }
        let status = auto.auto_bias_status().unwrap();
        let kept = |model: &ColorModel| {
            [
                model.kept_fraction(&samples.skin),
                model.kept_fraction(&samples.background),
                model.kept_fraction(&samples.scene),
            ]
        };
        let ([fixed_skin, fixed_background, _], [auto_skin, auto_background, auto_scene]) =
            (kept(&fixed), kept(&auto));
        println!(
            "fixed 1.5: skin kept {fixed_skin:.3} background kept {fixed_background:.3}; auto \
             {status:?}: skin kept {auto_skin:.3} background kept {auto_background:.3} scene kept \
             {auto_scene:.3}"
        );
        assert!(status.candidate <= 0.6, "{status:?}");
        assert!(fixed_background >= 0.5, "{fixed_background}");
        assert!(auto_background <= 0.1, "{auto_background}");
        // Most of the darker tone of the hand (a quarter of its colours) stays
        assert!(auto_skin >= 0.12, "{auto_skin}");
    }

    #[test]
    fn key_auto_moves_gradually_and_holds() {
        let mut model = auto_model(3.0, 0.5);
        for _ in 0..20 {
            model.add_samples(&apart_samples(), 0.12);
            model.update_lut().unwrap();
        }
        let mut last = model.auto_bias_status().unwrap();
        assert!(last.candidate >= 1.5, "{last:?}");
        // The hands go in front of a skin-like backdrop
        let mut steps = 0;
        for _ in 0..60 {
            model.add_samples(&skin_like_samples(), 0.12);
            model.update_lut().unwrap();
            let status = model.auto_bias_status().unwrap();
            let moved = status.bias.ln() - last.bias.ln();
            let distance = status.candidate.ln() - last.bias.ln();
            assert!(
                moved.abs() <= AUTO_BIAS_STEP * distance.abs() + 1e-5 && moved * distance >= 0.0,
                "{last:?} {status:?}"
            );
            if status.bias < last.bias {
                steps += 1;
            }
            last = status;
        }
        assert!(last.candidate <= 0.6, "{last:?}");
        assert!(last.bias < 0.65, "{last:?}");
        // Gradually: not in a few updates
        assert!(steps >= 8, "{steps}");

        // Scores that differ by less than the hysteresis keep the candidate
        let mut auto_bias = AutoBias::new(1.0, 0.5);
        let mut kept = [[0.5, 0.0, 0.0]; AUTO_BIAS_CANDIDATES.len()];
        kept[2] = [0.8, 0.0, 0.0];
        auto_bias.update(&kept);
        assert_eq!(
            auto_bias.status().unwrap().candidate,
            AUTO_BIAS_CANDIDATES[2]
        );
        kept[5] = [0.809, 0.0, 0.0];
        auto_bias.update(&kept);
        assert_eq!(
            auto_bias.status().unwrap().candidate,
            AUTO_BIAS_CANDIDATES[2]
        );
        kept[5] = [0.82, 0.0, 0.0];
        auto_bias.update(&kept);
        let status = auto_bias.status().unwrap();
        assert_eq!(status.candidate, AUTO_BIAS_CANDIDATES[5]);
        // A quarter of the way from 0.60 to 1.25, in log space
        let expected = (0.6_f32.ln() + (1.25_f32.ln() - 0.6_f32.ln()) * 0.25).exp();
        assert!((status.bias - expected).abs() < 1e-5, "{status:?}");
    }

    #[test]
    fn key_auto_weight_and_scene_share() {
        // More skin costs more leak
        let kept = std::array::from_fn(|i| {
            let x = i as f32 / (AUTO_BIAS_CANDIDATES.len() - 1) as f32;
            [0.3 + 0.7 * x, 0.5 * x * x, 0.9 * x]
        });
        // Weight 0: the most skin
        let mut auto_bias = AutoBias::new(0.0, 0.5);
        auto_bias.update(&kept);
        let status = auto_bias.status().unwrap();
        assert_eq!(status.candidate, 2.0);
        assert!((status.skin_kept - 1.0).abs() < 1e-6);
        // The scene counts: strict
        let mut auto_bias = AutoBias::new(3.0, 1.0);
        auto_bias.update(&kept);
        assert_eq!(auto_bias.status().unwrap().candidate, 0.3);
        // Scene share 0: only the background counts. 0.7 x - 1.5 x^2 is best at x = 0.23, of the
        // candidates at 2/7 (0.60)
        let mut auto_bias = AutoBias::new(3.0, 0.0);
        auto_bias.update(&kept);
        let status = auto_bias.status().unwrap();
        assert_eq!(status.candidate, 0.6);
        assert_eq!(status.leak, status.background_kept);

        // In the model: weight 0 keeps the most skin of the candidates
        let mut model = auto_model(0.0, 0.5);
        for _ in 0..5 {
            model.add_samples(&skin_like_samples(), 0.12);
            model.update_lut().unwrap();
        }
        let status = model.auto_bias_status().unwrap();
        assert_eq!(status.candidate, 2.0, "{status:?}");
        // A scene of skin colours with scene share 0: the leak is the background's alone
        let mut model = auto_model(3.0, 0.0);
        let mut samples = apart_samples();
        samples.scene = noisy(SKIN, 600);
        for _ in 0..5 {
            model.add_samples(&samples, 0.12);
            model.update_lut().unwrap();
        }
        let status = model.auto_bias_status().unwrap();
        assert_eq!(status.leak, status.background_kept, "{status:?}");
        assert!(status.scene_kept > 0.5, "{status:?}");
        assert!(status.candidate >= 1.5, "{status:?}");
    }

    #[test]
    fn measured_leak_of_background_and_scene() {
        let mut model = ColorModel::default();
        assert_eq!(model.measured_leak(&noisy(SKIN, 100), &[], 0.5), None);
        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        model.update_lut().unwrap();
        let skin = noisy(SKIN, 100);
        let wall = noisy(WHITE_WALL, 100);
        assert_eq!(model.measured_leak(&[], &[], 0.5), None);
        assert_eq!(model.measured_leak(&wall, &wall, 0.5), Some(0.0));
        assert_eq!(model.measured_leak(&skin, &wall, 0.5), Some(1.0));
        assert_eq!(model.measured_leak(&wall, &skin, 0.5), Some(0.5));
        assert_eq!(model.measured_leak(&[], &skin, 0.5), Some(0.5));
        assert_eq!(model.measured_leak(&skin, &[], 0.0), Some(1.0));
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
            verify(&model, &camera, &hand, 0.0, color),
            [0.0; HAND_SEGMENTS]
        );

        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        model.update_lut();
        let result = verify(&model, &camera, &hand, 0.0, color);
        assert_eq!(result[0], 1.0);
        assert_eq!(result[HAND_SEGMENTS - 1], 0.0);

        // The background looks the same
        let result = verify(&model, &camera, &hand, 0.8, color);
        assert_eq!(result[0], 0.0);

        // Outside of the image
        let far = CameraView {
            center: Vec2::new(1000.0, 120.0),
            ..camera
        };
        assert_eq!(verify(&model, &far, &hand, 0.0, color)[0], 0.0);

        // Segments that are not eligible are not checked
        let mut eligible = [true; HAND_SEGMENTS];
        eligible[0] = false;
        let shares = model
            .segment_shares(&camera, 320, 240, &hand, &eligible, color)
            .unwrap();
        assert_eq!(shares[0], None);
        assert_eq!(shares[1], Some(1.0));
    }

    // The verification before the confirm table: all points with the key table
    fn reference_verify(
        model: &ColorModel,
        camera: &CameraView,
        hand: &SampledHand,
        background_kept: f32,
        color: impl Fn(u32, u32) -> [u8; 3],
    ) -> [f32; HAND_SEGMENTS] {
        let Some(alpha) = &model.alpha else {
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
                    if pixel.x < 0.0 || pixel.y < 0.0 || pixel.x >= 320.0 || pixel.y >= 240.0 {
                        continue;
                    }
                    point_count += 1;
                    if alpha[texel_index(color(pixel.x as u32, pixel.y as u32))] > 0.5 {
                        skin_count += 1;
                    }
                }
            }
            if point_count < 8 {
                return 0.0;
            }

            reliability * smoothstep(0.5, 0.8, skin_count as f32 / point_count as f32)
        })
    }

    // A shadow on the skin: a few percent of the skin samples
    const SHADOW: [u8; 3] = [90, 66, 51];

    fn shadow_samples(shadow_count: usize) -> HandColorSamples {
        let mut samples = samples(SKIN, WHITE_WALL);
        samples.skin.extend([SHADOW].repeat(shadow_count));
        samples
    }

    // Horizontal fingers, 0.5 m in front of the camera, 1 cm radius, at these heights (pixels from the
    // center of the image, about 4 pixels per cm)
    fn finger_scene(heights: impl Fn(usize) -> f32) -> (CameraView, SampledHand) {
        let camera = CameraView {
            position: Vec3::ZERO,
            orientation: Quat::IDENTITY,
            focal_length: Vec2::splat(217.0),
            center: Vec2::new(160.0, 120.0),
        };
        let segments = std::array::from_fn(|i| {
            let y = heights(i) / 217.0 * 0.5;
            [
                Vec4::new(-0.05, y, 0.5, 0.01),
                Vec4::new(0.05, y, 0.5, 0.01),
            ]
        });
        let hand = SampledHand {
            shape: HandShape { segments },
            wrist: Vec3::new(0.05, 0.0, 0.5),
            arm_direction: Vec3::X,
            is_still: true,
        };

        (camera, hand)
    }

    #[test]
    fn confirm_off_verifies_like_before() {
        // The segments move across the finger in the image: all shares from 0 to 1
        let (camera, hand) = finger_scene(|i| i as f32 * 0.5 - 7.0);
        // The axis of the finger is in the shadow
        let color = |_: u32, y: u32| match y {
            119..=121 => SHADOW,
            114..=126 => SKIN,
            _ => WHITE_WALL,
        };
        let mut model = ColorModel {
            tightness: 0.3,
            ..Default::default()
        };
        model.add_samples(&shadow_samples(20), 0.1);
        model.update_lut();
        assert!(model.confirm.is_none());
        assert_eq!(model.kept_fraction(&[SHADOW]), 0.0);
        for background_kept in [0.0, 0.5, 1.0] {
            assert_eq!(
                verify(&model, &camera, &hand, background_kept, color),
                reference_verify(&model, &camera, &hand, background_kept, color)
            );
        }
        let before = verify(&model, &camera, &hand, 0.0, color);
        assert!(before.iter().any(|weight| *weight > 0.0 && *weight < 1.0));

        // The confirm table never verifies less
        model.confirm_tightness = 0.12;
        for _ in 0..10 {
            model.add_samples(&shadow_samples(20), 0.1);
            model.update_lut();
        }
        let after = verify(&model, &camera, &hand, 0.0, color);
        for (before, after) in before.iter().zip(after) {
            assert!(after >= *before, "{before:?} {after:?}");
        }
        assert!(
            after.iter().sum::<f32>() > before.iter().sum::<f32>() + 1.0,
            "{before:?} {after:?}"
        );
    }

    #[test]
    fn confirm_table_keeps_what_the_key_keeps() {
        let mut mixed = samples(SKIN, GREEN_WALL);
        mixed.skin.extend(noisy(GREEN_WALL, 150));
        mixed.background.extend(noisy(SKIN, 150));
        mixed.scene = noisy([200, 30, 40], 300);
        for samples in [shadow_samples(20), mixed, samples(SKIN, SKIN)] {
            // Below the key tightness, and above it (capped at it)
            for (skin_smoothing, confirm_tightness) in [(1.0, 0.12), (0.7, 0.12), (1.0, 0.5)] {
                // On from the start, and switched on later
                for first_confirmed in [0, 3] {
                    let mut model = ColorModel {
                        tightness: 0.3,
                        skin_smoothing,
                        ..Default::default()
                    };
                    for update in 0..8 {
                        model.confirm_tightness = if update >= first_confirmed {
                            confirm_tightness
                        } else {
                            0.0
                        };
                        model.add_samples(&samples, 0.12);
                        model.update_lut();
                        let Some(confirm) = &model.confirm else {
                            continue;
                        };
                        let alpha = model.alpha.as_ref().unwrap();
                        for (alpha, confirm) in alpha.iter().zip(confirm) {
                            assert!(*alpha <= 0.5 || *confirm > 0.5, "{alpha} {confirm}");
                        }
                    }
                }
            }
        }

        // Off: not built
        let mut model = ColorModel::default();
        model.add_samples(&shadow_samples(20), 0.1);
        model.update_lut();
        assert!(model.confirm.is_none());
    }

    #[test]
    fn confirm_table_only_checks_the_axis() {
        let (camera, hand) = finger_scene(|_| 0.0);
        let mut model = ColorModel {
            tightness: 0.3,
            confirm_tightness: 0.12,
            ..Default::default()
        };
        model.add_samples(&shadow_samples(20), 0.1);
        model.update_lut();
        assert_eq!(model.kept_fraction(&[SHADOW]), 0.0);
        assert_eq!(model.confirm_kept_fraction(&[SHADOW]), 1.0);

        // A shadow along the axis is confirmed
        let shadow_line = |_: u32, y: u32| match y {
            119..=121 => SHADOW,
            114..=126 => SKIN,
            _ => WHITE_WALL,
        };
        assert_eq!(verify(&model, &camera, &hand, 0.0, shadow_line)[0], 1.0);
        // Beside the axis only the key counts: not a hand
        let all_shadow = |_: u32, _: u32| SHADOW;
        assert_eq!(verify(&model, &camera, &hand, 0.0, all_shadow)[0], 0.0);
        // Moved off the hand: as with the key alone
        let (_, moved) = finger_scene(|_| 4.0);
        assert_eq!(
            verify(&model, &camera, &moved, 0.0, shadow_line)[0],
            reference_verify(&model, &camera, &moved, 0.0, shadow_line)[0]
        );
    }

    #[test]
    fn reliability_comes_from_the_confirm_table() {
        // The key keeps nothing, the confirm table keeps the skin colored background
        let mut model = ColorModel {
            tightness: 10.0,
            confirm_tightness: 0.12,
            ..Default::default()
        };
        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        model.update_lut();
        let ring = noisy(SKIN, 200);
        assert_eq!(model.kept_fraction(&ring), 0.0);
        let kept = model.confirm_leak_fraction(&ring).unwrap();
        assert_eq!(verification_reliability(kept), 0.0, "{kept}");
        assert_eq!(model.leak_fraction(&ring), Some(0.0));
    }

    #[test]
    fn narrow_skin_kernel_keeps_rare_skin_colors() {
        // A narrow skin peak and a rare skin color (7% of the typical density)
        let mut skin = vec![SKIN; 600];
        skin.extend([SHADOW; 15]);
        let samples = HandColorSamples {
            skin,
            background: noisy(GREEN_WALL, 600),
            ..Default::default()
        };
        let lut = |skin_smoothing: f32| {
            let mut model = ColorModel {
                tightness: 0.3,
                skin_smoothing,
                ..Default::default()
            };
            model.add_samples(&samples, 0.1);
            model.update_lut().unwrap()
        };
        let (wide, narrow) = (lut(1.0), lut(0.7));

        assert!(alpha(&wide, SHADOW) < 128, "{}", alpha(&wide, SHADOW));
        assert!(alpha(&narrow, SHADOW) > 128, "{}", alpha(&narrow, SHADOW));
        // Around the peak nothing more
        let center = SKIN.map(|v| texel(v) as i32);
        for offset in 0..7 * 7 * 7 {
            let texels = [offset % 7, offset / 7 % 7, offset / 49].map(|o| o - 3);
            let color = [0, 1, 2].map(|c| {
                let texel = (center[c] + texels[c]).clamp(0, COLOR_LUT_RESOLUTION as i32 - 1);
                (texel * 255 / (COLOR_LUT_RESOLUTION as i32 - 1)) as u8
            });
            assert!(
                alpha(&narrow, color) <= alpha(&wide, color),
                "{texels:?} {} {}",
                alpha(&narrow, color),
                alpha(&wide, color)
            );
        }
        // Clamped
        assert_eq!(lut(0.1), lut(0.7));
    }

    #[test]
    fn lost_skin_colors_are_split_by_reason() {
        let mut model = ColorModel::default();
        assert_eq!(model.lost_fractions(&[SKIN]), [0.0, 0.0]);
        let mixed = |main, other| {
            let mut colors = noisy(main, 850);
            colors.extend(noisy(other, 150));
            colors
        };
        model.add_samples(
            &HandColorSamples {
                skin: mixed(SKIN, GREEN_WALL),
                background: mixed(GREEN_WALL, SKIN),
                ..Default::default()
            },
            0.1,
        );
        model.update_lut();

        assert_eq!(model.lost_fractions(&[SKIN]), [0.0, 0.0]);
        // Seen as skin, but more in the background
        assert_eq!(model.lost_fractions(&[GREEN_WALL]), [0.0, 1.0]);
        // Never seen
        assert_eq!(model.lost_fractions(&[[20, 40, 200]]), [1.0, 0.0]);
        assert_eq!(
            model.lost_fractions(&[SKIN, GREEN_WALL, [20, 40, 200], [20, 40, 200]]),
            [0.5, 0.25]
        );
    }

    // Weights of all segments
    fn verify(
        model: &ColorModel,
        camera: &CameraView,
        hand: &SampledHand,
        background_kept: f32,
        color: impl Fn(u32, u32) -> [u8; 3],
    ) -> [f32; HAND_SEGMENTS] {
        let reliability = verification_reliability(background_kept);
        model
            .segment_shares(camera, 320, 240, hand, &[true; HAND_SEGMENTS], color)
            .map_or([0.0; HAND_SEGMENTS], |shares| {
                shares.map(|share| verification_weight(share, reliability))
            })
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
                    assert!(choice.update(&axes, &scores, 1.0, FOLLOW));
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
                unsure.update(&axes, &scores, verification_reliability(0.8), FOLLOW);
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
        assert!(!BridgeChoice::default().update(&axes, &scores(&model, &camera), 1.0, FOLLOW));

        model.add_samples(&samples(SKIN, WHITE_WALL), 0.1);
        model.update_lut();
        let mut choice = BridgeChoice::default();
        assert!(choice.update(&axes, &scores(&model, &camera), 1.0, FOLLOW));
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
        choice.update(&axes, &scores, 1.0, FOLLOW);
        assert_eq!(choice.index, Some(10));

        // Slightly better elsewhere: kept
        scores[16] = Some(0.85);
        choice.update(&axes, &scores, 1.0, FOLLOW);
        assert_eq!(choice.index, Some(10));
        assert_eq!(choice.confidence, 0.8);
        assert_eq!(choose_bridge(&scores, None), Some(16));

        // Better by more than the hysteresis: changed. The smoothed direction is still 40 degrees off,
        // nobody checked it: not sure until it gets there
        scores[16] = Some(0.95);
        choice.update(&axes, &scores, 1.0, FOLLOW);
        assert_eq!(choice.index, Some(16));
        assert_eq!(choice.confidence, 0.0);
        let mut updates = 1;
        while choice.confidence < 0.95 - 1e-6 {
            let last = choice.confidence;
            choice.update(&axes, &scores, 1.0, FOLLOW);
            assert!(choice.confidence >= last);
            updates += 1;
            assert!(updates < 15, "{}", choice.confidence);
        }
        assert!(choice.direction.unwrap().angle_between(axes.candidate(16)) < 5f32.to_radians());
        assert!(updates >= 4, "{updates}");

        // The current one can't be seen: the best one
        scores[16] = None;
        assert_eq!(choose_bridge(&scores, Some(16)), Some(10));

        // The direction moves there step by step
        let target = axes.candidate(16);
        let mut choice = BridgeChoice::default();
        scores[16] = Some(1.0);
        choice.update(&axes, &scores, 1.0, FOLLOW);
        let first = choice.direction.unwrap();
        assert!(first.angle_between(axes.default) > 0.05);
        assert!(first.angle_between(target) > 0.3);
        for _ in 0..20 {
            choice.update(&axes, &scores, 1.0, FOLLOW);
        }
        assert!(choice.direction.unwrap().angle_between(target) < 0.01);
    }

    #[test]
    fn unsure_bridge_goes_back_to_the_default() {
        let (_, _, _, axes) = bridge_scene(Vec3::Y, Vec3::NEG_Z);
        let bent = 16;
        let mut sure = [Some(0.1); BRIDGE_CANDIDATES];
        sure[bent] = Some(1.0);
        let mut choice = BridgeChoice::default();
        for _ in 0..20 {
            choice.update(&axes, &sure, 1.0, FOLLOW);
        }
        assert!(
            choice
                .direction
                .unwrap()
                .angle_between(axes.candidate(bent))
                < 0.01
        );

        // The background looks like skin: the same scores tell nothing, back to the default
        for _ in 0..20 {
            choice.update(&axes, &sure, verification_reliability(0.8), FOLLOW);
            assert_eq!(choice.confidence, 0.0);
        }
        assert!(choice.direction.unwrap().angle_between(axes.default) < 0.01);

        // A sleeve: only noise is kept, the best of it is not followed
        let mut noise = [Some(0.05); BRIDGE_CANDIDATES];
        noise[bent] = Some(0.25);
        let mut choice = BridgeChoice::default();
        for _ in 0..5 {
            choice.update(&axes, &noise, 1.0, FOLLOW);
        }
        assert_eq!(choice.index, Some(bent));
        assert!(choice.direction.unwrap().angle_between(axes.default) < 1e-4);
        assert_eq!(choice.confidence, 0.0);

        // Follow 0: always the best one, as before
        choice.update(&axes, &noise, 1.0, 0.0);
        assert!(choice.direction.unwrap().angle_between(axes.default) > 0.05);
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
