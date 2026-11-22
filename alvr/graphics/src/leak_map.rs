//! Remembers which directions around the head show background that the color key lets through
//! ("leaks"), so the key zone can shrink before the hand gets there. Pure CPU, no depth: the map stores
//! directions from an anchor (the camera position at the first fill), so it is only right as long as the
//! head stays near the anchor. Cells store camera colors, not leak flags, so a new LUT re-evaluates the
//! whole map without new observations.
//!
//! Layout: octahedral map of the directions, 96 x 96 cells of about 2.1 degrees, the lower hemisphere
//! (desk, lap, hands) in the inner diamond without seams.

use crate::COLOR_LUT_RESOLUTION;
use alvr_common::glam::{Vec2, Vec3, Vec4};
use std::collections::VecDeque;

/// Cells per side of the octahedral map
pub const LEAK_MAP_SIZE: usize = 96;
pub const LEAK_MAP_CELLS: usize = LEAK_MAP_SIZE * LEAK_MAP_SIZE;
/// Cell value of a direction that is not known
pub const LEAK_UNKNOWN: u8 = 255;
/// Cell value of a direction that leaks completely
pub const LEAK_FULL: u8 = 254;
/// How long a cell is remembered by default, in seconds
pub const DEFAULT_LEAK_MEMORY: f32 = 120.0;
/// How many of the highest segment values are averaged by `LeakSnapshot::hand_leak`
pub const LEAK_TOP_SEGMENTS: usize = 7;

const TEXEL_COUNT: usize = COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION;
// Colors per cell, one per quarter of the cell
const SLOTS: usize = 4;
const UNKNOWN_TEXEL: u16 = u16::MAX;
// An observation is only used if no hand came near it within this time, in seconds
const COMMIT_DELAY: f32 = 1.0;
// Hands that touched a cell shortly before an observation also reject it (tracking that is lost while
// the hand is still there)
const HAND_GUARD: f32 = 0.25;
const MAX_PENDING: usize = 32;
// Exclusion around a hand, see `hand_exclusion`
const EXCLUSION_MARGIN: f32 = 0.03;
const EXCLUSION_TIME: f32 = 0.08;
// The camera rays are moved to the anchor assuming the background is this far away, in meters
const OBSERVE_DEPTH: f32 = 0.8;
// The anchor moves if the camera is farther than this for this long
const ANCHOR_TOLERANCE: f32 = 0.05;
const ANCHOR_DELAY: f32 = 1.0;
// Light change: share of the re-observed colors that moved by more than the texel distance
const LIGHT_CHANGE_TEXELS: usize = 2;
const LIGHT_CHANGE_FRACTION: f32 = 0.4;
const LIGHT_CHANGE_MIN_SAMPLES: usize = 100;
// Points on the eye ray, as multiples of the distance from the eye to the hand
const LOOKUP_DEPTHS: [f32; 4] = [1.15, 1.5, 2.5, 1000.0];
const TAPS: [(isize, isize); 5] = [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1)];
// Value above which a cell counts as leaking in the debug outputs
const LEAK_THRESHOLD: f32 = 0.5;

fn texel(value: u8) -> usize {
    (usize::from(value) * (COLOR_LUT_RESOLUTION - 1) + 127) / 255
}

// Same as color_model.rs: index into a 32^3 table, [b][g][r]
fn texel_index([r, g, b]: [u8; 3]) -> usize {
    (texel(b) * COLOR_LUT_RESOLUTION + texel(g)) * COLOR_LUT_RESOLUTION + texel(r)
}

fn texel_distance(a: u16, b: u16) -> usize {
    let (a, b) = (usize::from(a), usize::from(b));
    [
        1,
        COLOR_LUT_RESOLUTION,
        COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION,
    ]
    .into_iter()
    .map(|stride| (a / stride % COLOR_LUT_RESOLUTION).abs_diff(b / stride % COLOR_LUT_RESOLUTION))
    .max()
    .unwrap_or(0)
}

// Maximum over 3x3x3 texels: camera and compositor colors differ by about one texel
fn dilate(values: &[u8]) -> Vec<u8> {
    let mut current = values.to_vec();
    let mut next = vec![0; TEXEL_COUNT];
    for stride in [
        1,
        COLOR_LUT_RESOLUTION,
        COLOR_LUT_RESOLUTION * COLOR_LUT_RESOLUTION,
    ] {
        for (index, output) in next.iter_mut().enumerate() {
            let position = index / stride % COLOR_LUT_RESOLUTION;
            let mut value = current[index];
            if position > 0 {
                value = value.max(current[index - stride]);
            }
            if position + 1 < COLOR_LUT_RESOLUTION {
                value = value.max(current[index + stride]);
            }
            *output = value;
        }
        std::mem::swap(&mut current, &mut next);
    }

    current
}

/// Position in the octahedral map, in cells (0..LEAK_MAP_SIZE). The vector doesn't need to be
/// normalized. None for a zero or invalid vector.
fn grid_position(vector: Vec3) -> Option<Vec2> {
    let sum = vector.x.abs() + vector.y.abs() + vector.z.abs();
    if !(sum > 1e-12 && sum.is_finite()) {
        return None;
    }
    let (a, b, down) = (vector.x / sum, vector.z / sum, -vector.y / sum);
    let (a, b) = if down >= 0.0 {
        (a, b)
    } else {
        ((1.0 - b.abs()).copysign(a), (1.0 - a.abs()).copysign(b))
    };

    Some((Vec2::new(a, b) * 0.5 + 0.5) * LEAK_MAP_SIZE as f32)
}

fn cell_coordinates(position: Vec2) -> (usize, usize) {
    (
        (position.x as usize).min(LEAK_MAP_SIZE - 1),
        (position.y as usize).min(LEAK_MAP_SIZE - 1),
    )
}

fn cell_index(vector: Vec3) -> Option<usize> {
    let (x, y) = cell_coordinates(grid_position(vector)?);
    Some(y * LEAK_MAP_SIZE + x)
}

/// Unit direction of the center of a cell
fn cell_direction(index: usize) -> Vec3 {
    let (x, y) = (index % LEAK_MAP_SIZE, index / LEAK_MAP_SIZE);
    let p = (Vec2::new(x as f32 + 0.5, y as f32 + 0.5) / LEAK_MAP_SIZE as f32) * 2.0 - 1.0;
    let down = 1.0 - p.x.abs() - p.y.abs();
    let (a, b) = if down >= 0.0 {
        (p.x, p.y)
    } else {
        (
            (1.0 - p.y.abs()).copysign(p.x),
            (1.0 - p.x.abs()).copysign(p.y),
        )
    };

    Vec3::new(a, -down, b).normalize()
}

/// Neighbor cell. Across the outer edge of the octahedral map the edge folds onto itself.
fn neighbor(x: usize, y: usize, dx: isize, dy: isize) -> usize {
    let n = LEAK_MAP_SIZE as isize;
    let (mut x, mut y) = (x as isize + dx, y as isize + dy);
    if !(0..n).contains(&x) {
        x = x.clamp(0, n - 1);
        y = n - 1 - y;
    }
    if !(0..n).contains(&y) {
        y = y.clamp(0, n - 1);
        x = n - 1 - x;
    }

    (y.clamp(0, n - 1) * n + x.clamp(0, n - 1)) as usize
}

fn ray_hits_sphere(origin: Vec3, direction: Vec3, sphere: Vec4) -> bool {
    let to_center = sphere.truncate() - origin;
    let radius_squared = sphere.w * sphere.w;
    let distance_squared = to_center.length_squared();
    if distance_squared <= radius_squared {
        return true;
    }
    let along = to_center.dot(direction);

    along > 0.0 && distance_squared - along * along < radius_squared
}

/// Exclusion sphere of a located hand: its bounding sphere (xyz: position, w: radius, meters, stage
/// space) grown by 3 cm and by the distance it moves in 80 ms at `speed` (m/s, fastest joint).
pub fn hand_exclusion(bounds: Vec4, speed: f32) -> Vec4 {
    bounds
        .truncate()
        .extend(bounds.w + EXCLUSION_MARGIN + speed.max(0.0) * EXCLUSION_TIME)
}

#[derive(Clone, Copy)]
struct Cell {
    // LUT texel of the last color seen in each quarter of the cell
    texels: [u16; SLOTS],
    // Time of the last observation, seconds since the epoch
    stamp: f32,
    // Last time a hand was in front of the cell
    last_hand: f32,
}

const EMPTY_CELL: Cell = Cell {
    texels: [UNKNOWN_TEXEL; SLOTS],
    stamp: f32::NEG_INFINITY,
    last_hand: f32::NEG_INFINITY,
};

impl Cell {
    fn is_known(&self) -> bool {
        self.texels.iter().any(|texel| *texel != UNKNOWN_TEXEL)
    }
}

#[derive(Clone, Copy)]
struct Observation {
    cell: u16,
    slot: u8,
    texel: u16,
}

struct Pending {
    time: f32,
    observations: Vec<Observation>,
}

/// Learning side, owned by the color key thread
pub struct LeakMap {
    cells: Vec<Cell>,
    anchor: Option<Vec3>,
    away_since: Option<f32>,
    pending: VecDeque<Pending>,
    spare: Vec<Vec<Observation>>,
    epoch: Option<f64>,
    now: f32,
    // Alpha of the LUT (0..255), maximum over 3x3x3 texels
    leak: Option<Vec<u8>>,
    /// Cell lifetime in seconds
    pub memory: f32,
    light_resets: u32,
    anchor_moves: u32,
}

impl Default for LeakMap {
    fn default() -> Self {
        Self::new()
    }
}

impl LeakMap {
    pub fn new() -> Self {
        Self {
            cells: vec![EMPTY_CELL; LEAK_MAP_CELLS],
            anchor: None,
            away_since: None,
            pending: VecDeque::with_capacity(MAX_PENDING),
            spare: Vec::new(),
            epoch: None,
            now: 0.0,
            leak: None,
            memory: DEFAULT_LEAK_MEMORY,
            light_resets: 0,
            anchor_moves: 0,
        }
    }

    /// Forget all colors and the anchor. The alpha table and the counters stay.
    pub fn clear(&mut self) {
        self.cells.fill(EMPTY_CELL);
        self.anchor = None;
        self.away_since = None;
        self.drop_pending();
    }

    fn drop_pending(&mut self) {
        while let Some(pending) = self.pending.pop_front() {
            self.spare.push(pending.observations);
        }
    }

    fn reanchor(&mut self, position: Vec3) {
        self.cells.fill(EMPTY_CELL);
        self.drop_pending();
        self.anchor = Some(position);
        self.away_since = None;
        self.anchor_moves += 1;
    }

    /// Where the directions start (stage space, meters). None before the first observation.
    pub fn anchor(&self) -> Option<Vec3> {
        self.anchor
    }

    /// How often all cells were forgotten because the light changed
    pub fn light_resets(&self) -> u32 {
        self.light_resets
    }

    /// How often the anchor moved (which forgets all cells)
    pub fn anchor_moves(&self) -> u32 {
        self.anchor_moves
    }

    /// Add the colors of one camera image.
    /// - `camera_position`: stage space, meters.
    /// - `samples`: stage-space direction of the camera ray (any length) and RGB color of a pixel.
    /// - `exclusions`: spheres around the located hands (xyz: position, w: radius, meters, stage space),
    ///   see `hand_exclusion`. Rays that hit one are skipped and mark their cell as touched by a hand.
    /// - `now`: time of the image in seconds, any monotonic clock. Older times are ignored.
    ///
    /// The colors are used 1 s later, and only in cells that no hand touched in between.
    pub fn observe(
        &mut self,
        camera_position: Vec3,
        samples: &[(Vec3, [u8; 3])],
        exclusions: &[Vec4],
        now: f64,
    ) {
        let epoch = *self.epoch.get_or_insert(now);
        let time = (now - epoch) as f32;
        if time.is_nan() || time < self.now {
            return;
        }
        self.now = time;

        let mut anchor = *self.anchor.get_or_insert(camera_position);
        let mut is_away = camera_position.distance(anchor) > ANCHOR_TOLERANCE;
        if is_away {
            let since = *self.away_since.get_or_insert(time);
            if time - since >= ANCHOR_DELAY {
                self.reanchor(camera_position);
                anchor = camera_position;
                is_away = false;
            }
        } else {
            self.away_since = None;
        }

        let offset = camera_position - anchor;
        let mut observations = self.spare.pop().unwrap_or_default();
        observations.clear();
        for (direction, color) in samples {
            let Some(direction) = direction.try_normalize() else {
                continue;
            };
            let Some(position) = grid_position(offset + direction * OBSERVE_DEPTH) else {
                continue;
            };
            let (x, y) = cell_coordinates(position);
            let cell = y * LEAK_MAP_SIZE + x;
            if exclusions
                .iter()
                .any(|sphere| ray_hits_sphere(camera_position, direction, *sphere))
            {
                self.cells[cell].last_hand = time;
                continue;
            }
            if !is_away {
                let quarter = (position - position.floor()).cmpge(Vec2::splat(0.5));
                observations.push(Observation {
                    cell: cell as u16,
                    slot: u8::from(quarter.x) + 2 * u8::from(quarter.y),
                    texel: texel_index(*color) as u16,
                });
            }
        }

        // After the hands of this image are marked
        while self
            .pending
            .front()
            .is_some_and(|pending| pending.time <= time - COMMIT_DELAY)
        {
            let pending = self.pending.pop_front().unwrap();
            self.commit(&pending);
            self.spare.push(pending.observations);
        }

        if observations.is_empty() {
            self.spare.push(observations);
        } else {
            if self.pending.len() >= MAX_PENDING
                && let Some(oldest) = self.pending.pop_front()
            {
                self.spare.push(oldest.observations);
            }
            self.pending.push_back(Pending { time, observations });
        }

        let oldest = time - self.memory;
        for cell in &mut self.cells {
            if cell.stamp < oldest {
                cell.texels = [UNKNOWN_TEXEL; SLOTS];
            }
        }
    }

    fn commit(&mut self, pending: &Pending) {
        let oldest = self.now - self.memory;
        let mut reobserved = 0;
        let mut moved = 0;
        for observation in &pending.observations {
            let cell = &mut self.cells[usize::from(observation.cell)];
            if cell.last_hand >= pending.time - HAND_GUARD {
                continue;
            }
            let slot = &mut cell.texels[usize::from(observation.slot)];
            if *slot != UNKNOWN_TEXEL && cell.stamp >= oldest {
                reobserved += 1;
                if texel_distance(*slot, observation.texel) > LIGHT_CHANGE_TEXELS {
                    moved += 1;
                }
            }
            *slot = observation.texel;
            cell.stamp = pending.time;
        }

        if reobserved >= LIGHT_CHANGE_MIN_SAMPLES
            && moved as f32 > LIGHT_CHANGE_FRACTION * reobserved as f32
        {
            for cell in &mut self.cells {
                if cell.stamp != pending.time {
                    cell.texels = [UNKNOWN_TEXEL; SLOTS];
                }
            }
            self.light_resets += 1;
        }
    }

    /// Set the key: `alpha` has one value per LUT texel (0: removed, 255: shown), 32^3 entries indexed
    /// [b][g][r] like the LUT. Ignored if the length is wrong.
    pub fn set_alpha(&mut self, alpha: &[u8]) {
        if alpha.len() == TEXEL_COUNT {
            self.leak = Some(dilate(alpha));
        }
    }

    /// Same as `set_alpha`, from the RGBA bytes of the LUT (alpha is every 4th byte)
    pub fn set_alpha_from_lut(&mut self, lut: &[u8]) {
        if lut.len() == TEXEL_COUNT * 4 {
            let alpha = lut
                .chunks_exact(4)
                .map(|texel| texel[3])
                .collect::<Vec<_>>();
            self.set_alpha(&alpha);
        }
    }

    /// The map against the current alpha table. None before `set_alpha` or the first observation.
    pub fn snapshot(&self) -> Option<LeakSnapshot> {
        let leak = self.leak.as_ref()?;
        let anchor = self.anchor?;

        let mut cells = Box::new([LEAK_UNKNOWN; LEAK_MAP_CELLS]);
        for (value, cell) in cells.iter_mut().zip(&self.cells) {
            let (sum, count) = cell
                .texels
                .iter()
                .filter(|texel| **texel != UNKNOWN_TEXEL)
                .fold((0, 0), |(sum, count), texel| {
                    (sum + u32::from(leak[usize::from(*texel)]), count + 1)
                });
            if count > 0 {
                *value = ((sum * u32::from(LEAK_FULL) + count * 255 / 2) / (count * 255)) as u8;
            }
        }

        Some(LeakSnapshot { anchor, cells })
    }

    /// Fraction of the cells that have a color, for diagnostics
    pub fn known_fraction(&self) -> f32 {
        self.cells.iter().filter(|cell| cell.is_known()).count() as f32 / LEAK_MAP_CELLS as f32
    }
}

/// Read side, shared with the render thread
#[derive(Clone)]
pub struct LeakSnapshot {
    /// Where the directions start, stage space, meters
    pub anchor: Vec3,
    /// Octahedral map, row-major: 0 nothing leaks .. 254 everything leaks, 255 unknown
    pub cells: Box<[u8; LEAK_MAP_CELLS]>,
}

impl LeakSnapshot {
    /// All cells unknown
    pub fn unknown(anchor: Vec3) -> Self {
        Self {
            anchor,
            cells: Box::new([LEAK_UNKNOWN; LEAK_MAP_CELLS]),
        }
    }

    /// Fraction of the cells that are known
    pub fn known_fraction(&self) -> f32 {
        self.cells
            .iter()
            .filter(|value| **value != LEAK_UNKNOWN)
            .count() as f32
            / LEAK_MAP_CELLS as f32
    }

    /// Leak (0..1) of the single cell in this direction from the anchor (any length). None if unknown.
    pub fn direction_leak(&self, direction: Vec3) -> Option<f32> {
        let value = self.cells[cell_index(direction)?];
        (value != LEAK_UNKNOWN).then(|| f32::from(value) / f32::from(LEAK_FULL))
    }

    // Maximum over the depths of the 5-tap means. Counts the known and all taps.
    fn ray_leak(
        &self,
        eye: Vec3,
        point: Vec3,
        known: &mut usize,
        total: &mut usize,
    ) -> Option<f32> {
        let ray = point - eye;
        if ray.length_squared() <= 1e-8 {
            return None;
        }
        let start = eye - self.anchor;

        let mut best: Option<f32> = None;
        for depth in LOOKUP_DEPTHS {
            let Some(position) = grid_position(start + ray * depth) else {
                continue;
            };
            let (x, y) = cell_coordinates(position);
            let mut sum = 0;
            let mut count = 0;
            for (dx, dy) in TAPS {
                let value = self.cells[neighbor(x, y, dx, dy)];
                if value != LEAK_UNKNOWN {
                    sum += u32::from(value);
                    count += 1;
                }
            }
            *total += TAPS.len();
            *known += count;
            if count > 0 {
                let mean = sum as f32 / (count as f32 * f32::from(LEAK_FULL));
                best = Some(best.map_or(mean, |best| best.max(mean)));
            }
        }

        best
    }

    /// Leak (0..1) behind `point` as seen from `eye` (both stage space, meters): points on the eye ray
    /// at 1.15, 1.5, 2.5 and 1000 times the distance, the mean of 5 cells around each, the maximum over
    /// the depths. None if all of these cells are unknown. No allocation.
    pub fn lookup(&self, eye: Vec3, point: Vec3) -> Option<f32> {
        self.ray_leak(eye, point, &mut 0, &mut 0)
    }

    /// Leak (0..1) of a hand: for each point (e.g. segment midpoints, stage space, meters) the maximum
    /// of `lookup` over the `eyes` (stage space, meters), then the mean of the 7 highest points. None if
    /// fewer than half of the cells looked at are known. No allocation.
    pub fn hand_leak(&self, eyes: &[Vec3], points: &[Vec3]) -> Option<f32> {
        // Sorted, highest first
        let mut top = [f32::NEG_INFINITY; LEAK_TOP_SEGMENTS];
        let mut known = 0;
        let mut total = 0;
        for point in points {
            let mut value: Option<f32> = None;
            for eye in eyes {
                if let Some(leak) = self.ray_leak(*eye, *point, &mut known, &mut total) {
                    value = Some(value.map_or(leak, |value| value.max(leak)));
                }
            }
            if let Some(mut value) = value {
                for entry in &mut top {
                    if value > *entry {
                        std::mem::swap(&mut value, entry);
                    }
                }
            }
        }
        if total == 0 || known * 2 < total {
            return None;
        }

        let (sum, count) = top
            .iter()
            .filter(|value| value.is_finite())
            .fold((0.0, 0), |(sum, count), value| (sum + value, count + 1));
        (count > 0).then(|| sum / count as f32)
    }

    /// Unit directions from the anchor of up to `count` clusters of leaking cells, leakiest first, at
    /// least 3 cells apart. For debug probes.
    pub fn top_clusters(&self, count: usize) -> Vec<Vec3> {
        let mut candidates = Vec::new();
        for index in 0..LEAK_MAP_CELLS {
            if self.cells[index] == LEAK_UNKNOWN {
                continue;
            }
            let (x, y) = (index % LEAK_MAP_SIZE, index / LEAK_MAP_SIZE);
            let mut sum = 0;
            let mut known = 0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let value = self.cells[neighbor(x, y, dx, dy)];
                    if value != LEAK_UNKNOWN {
                        sum += u32::from(value);
                        known += 1;
                    }
                }
            }
            let score = sum as f32 / (known as f32 * f32::from(LEAK_FULL));
            if score > LEAK_THRESHOLD {
                candidates.push((score, index));
            }
        }
        candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));

        let mut taken = vec![false; LEAK_MAP_CELLS];
        let mut directions = Vec::new();
        for (_, index) in candidates {
            if directions.len() >= count {
                break;
            }
            if taken[index] {
                continue;
            }
            directions.push(cell_direction(index));
            let (x, y) = (index % LEAK_MAP_SIZE, index / LEAK_MAP_SIZE);
            for dy in -2..=2 {
                for dx in -2..=2 {
                    taken[neighbor(x, y, dx, dy)] = true;
                }
            }
        }

        directions
    }

    /// One character per direction from the anchor, `width` per line: '#' leaks, '.' doesn't, '?'
    /// unknown. For a 40 x 30 grid of camera rays.
    pub fn ascii_dump(&self, directions: &[Vec3], width: usize) -> String {
        let width = width.max(1);
        let mut text = String::with_capacity(directions.len() + directions.len() / width + 1);
        for (i, direction) in directions.iter().enumerate() {
            if i > 0 && i % width == 0 {
                text.push('\n');
            }
            text.push(match self.direction_leak(*direction) {
                None => '?',
                Some(leak) if leak > LEAK_THRESHOLD => '#',
                Some(_) => '.',
            });
        }

        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alvr_common::glam::Quat;
    use std::{f32::consts::PI, time::Instant};

    const PLAIN: [u8; 3] = [40, 40, 200];
    const WOOD: [u8; 3] = [200, 120, 80];
    const STEP: f64 = 0.06;

    fn alpha_table(leaking: &[[u8; 3]]) -> Vec<u8> {
        let mut alpha = vec![0; TEXEL_COUNT];
        for color in leaking {
            alpha[texel_index(*color)] = 255;
        }
        alpha
    }

    fn degrees(value: f32) -> f32 {
        value * PI / 180.0
    }

    // Looks at -z, pitched down, then turned left by `yaw`
    fn rotation(yaw: f32, pitch: f32) -> Quat {
        Quat::from_rotation_y(degrees(yaw)) * Quat::from_rotation_x(degrees(-pitch))
    }

    // Rays of every 8th pixel of the 320 x 240 camera, row by row
    fn camera_rays(rotation: Quat) -> Vec<Vec3> {
        const FOCAL: f32 = 216.9;
        let mut rays = Vec::new();
        for y in 0..30 {
            for x in 0..40 {
                let local =
                    Vec2::new(x as f32 * 8.0 + 4.0 - 160.0, y as f32 * 8.0 + 4.0 - 120.0) / FOCAL;
                rays.push((rotation * Vec3::new(local.x, -local.y, -1.0)).normalize());
            }
        }
        rays
    }

    struct Scene<'a> {
        position: Vec3,
        rotation: Quat,
        color: &'a dyn Fn(Vec3, Vec3) -> [u8; 3],
        exclusions: &'a [Vec4],
    }

    // Observe every 60 ms from step `start` to `end` (exclusive). Returns the next step.
    fn run(map: &mut LeakMap, scene: &Scene, start: usize, end: usize) -> usize {
        let rays = camera_rays(scene.rotation);
        for step in start..end {
            let samples = rays
                .iter()
                .map(|ray| (*ray, (scene.color)(scene.position, *ray)))
                .collect::<Vec<_>>();
            map.observe(
                scene.position,
                &samples,
                scene.exclusions,
                100.0 + step as f64 * STEP,
            );
        }
        end
    }

    fn uniform(color: [u8; 3]) -> impl Fn(Vec3, Vec3) -> [u8; 3] {
        move |_, _| color
    }

    // A small ball at 0.4 m in front of the camera, in `color`, plain behind
    fn ball(center: Vec3, color: [u8; 3]) -> impl Fn(Vec3, Vec3) -> [u8; 3] {
        move |origin, ray| {
            if ray_hits_sphere(origin, ray, center.extend(0.06)) {
                color
            } else {
                PLAIN
            }
        }
    }

    fn steps(seconds: f64) -> usize {
        (seconds / STEP).round() as usize
    }

    #[test]
    fn octahedral_round_trip() {
        let mut directions = vec![
            Vec3::X,
            -Vec3::X,
            Vec3::Y,
            -Vec3::Y,
            Vec3::Z,
            -Vec3::Z,
            Vec3::new(0.0, 0.5, -1.0),
            Vec3::new(1.0, 0.5, 0.0),
        ];
        // Fibonacci sphere
        let count = 20000;
        for i in 0..count {
            let y = 1.0 - 2.0 * (i as f32 + 0.5) / count as f32;
            let angle = i as f32 * PI * (3.0 - 5.0f32.sqrt());
            let radius = (1.0 - y * y).sqrt();
            directions.push(Vec3::new(radius * angle.cos(), y, radius * angle.sin()));
        }

        let mut used = vec![false; LEAK_MAP_CELLS];
        for direction in directions {
            let index = cell_index(direction).unwrap();
            used[index] = true;
            let center = cell_direction(index);
            assert_eq!(cell_index(center), Some(index), "{direction}");
            assert!(center.angle_between(direction.normalize()) < degrees(3.0));
            // The length doesn't matter
            assert_eq!(cell_index(direction * 7.5), Some(index));
        }
        assert!(used.iter().filter(|used| **used).count() > LEAK_MAP_CELLS * 9 / 10);
        assert_eq!(cell_index(Vec3::ZERO), None);
        assert_eq!(cell_index(Vec3::NAN), None);

        // Neighbors, also across the folded edge, are next to each other
        for index in 0..LEAK_MAP_CELLS {
            let (x, y) = (index % LEAK_MAP_SIZE, index / LEAK_MAP_SIZE);
            for (dx, dy) in TAPS {
                let other = neighbor(x, y, dx, dy);
                let angle = cell_direction(index).angle_between(cell_direction(other));
                assert!(angle < degrees(6.0), "{x} {y} {dx} {dy}: {angle}");
            }
        }
    }

    #[test]
    fn patch_is_found_again_after_turning() {
        let patch = rotation(12.0, 20.0) * -Vec3::Z;
        let color = |_: Vec3, ray: Vec3| {
            if ray.angle_between(patch) < degrees(6.0) {
                WOOD
            } else {
                PLAIN
            }
        };
        let view = |yaw| Scene {
            position: Vec3::ZERO,
            rotation: rotation(yaw, 20.0),
            color: &color,
            exclusions: &[],
        };
        let aside = rotation(12.0 - 20.0, 20.0) * -Vec3::Z;

        let mut first = LeakMap::new();
        first.set_alpha(&alpha_table(&[WOOD]));
        run(&mut first, &view(0.0), 0, steps(2.0));
        let mut second = LeakMap::new();
        second.set_alpha(&alpha_table(&[WOOD]));
        run(&mut second, &view(25.0), 0, steps(2.0));

        for map in [&first, &second] {
            let snapshot = map.snapshot().unwrap();
            assert!(snapshot.direction_leak(patch).unwrap() > 0.9);
            assert!(snapshot.direction_leak(aside).unwrap() < 0.1);
            let clusters = snapshot.top_clusters(5);
            assert!(clusters[0].angle_between(patch) < degrees(4.0));
        }

        // Where both views know a cell, they agree
        let (first, second) = (first.snapshot().unwrap(), second.snapshot().unwrap());
        let mut both = 0;
        let mut same = 0;
        for (a, b) in first.cells.iter().zip(second.cells.iter()) {
            if *a != LEAK_UNKNOWN && *b != LEAK_UNKNOWN {
                both += 1;
                same += usize::from((*a > 127) == (*b > 127));
            }
        }
        assert!(both > 300, "{both}");
        assert!(same as f32 > 0.97 * both as f32, "{same} of {both}");
    }

    #[test]
    fn excluded_hand_keeps_the_earlier_color() {
        let center = Vec3::new(0.0, -0.2, -0.35);
        let behind = center;
        let hand = ball(center, WOOD);
        let plain = uniform(PLAIN);
        let exclusions = [hand_exclusion(center.extend(0.06), 0.0)];

        let mut map = LeakMap::new();
        map.set_alpha(&alpha_table(&[WOOD]));
        let mut control = LeakMap::new();
        control.set_alpha(&alpha_table(&[WOOD]));
        let before = Scene {
            position: Vec3::ZERO,
            rotation: rotation(0.0, 30.0),
            color: &plain,
            exclusions: &[],
        };
        let step = run(&mut map, &before, 0, steps(2.0));
        run(&mut control, &before, 0, step);
        assert_eq!(map.snapshot().unwrap().direction_leak(behind), Some(0.0));

        let with_hand = Scene {
            color: &hand,
            exclusions: &exclusions,
            ..before
        };
        run(&mut map, &with_hand, step, step + steps(3.0));
        assert_eq!(map.snapshot().unwrap().direction_leak(behind), Some(0.0));

        // Without the exclusion the hand is learned
        let untracked = Scene {
            exclusions: &[],
            ..with_hand
        };
        run(&mut control, &untracked, step, step + steps(3.0));
        assert!(control.snapshot().unwrap().direction_leak(behind).unwrap() > 0.9);
    }

    #[test]
    fn short_hand_visit_leaves_no_trace() {
        let center = Vec3::new(0.0, -0.2, -0.35);
        let hand = ball(center, WOOD);
        let plain = uniform(PLAIN);
        let exclusions = [hand_exclusion(center.extend(0.06), 0.3)];
        let calm = Scene {
            position: Vec3::ZERO,
            rotation: rotation(0.0, 30.0),
            color: &plain,
            exclusions: &[],
        };
        // The first image of the hand before it is tracked
        let untracked = Scene {
            color: &hand,
            ..calm
        };
        let tracked = Scene {
            color: &hand,
            exclusions: &exclusions,
            ..calm
        };

        for is_tracked_later in [true, false] {
            let mut map = LeakMap::new();
            map.set_alpha(&alpha_table(&[WOOD]));
            let mut step = run(&mut map, &calm, 0, steps(2.0));
            step = run(&mut map, &untracked, step, step + 1);
            let visit = if is_tracked_later { &tracked } else { &calm };
            step = run(&mut map, visit, step, step + steps(0.3));
            // Until the untracked image is committed
            let committed = step - steps(0.3) - 1 + steps(1.0) + 1;
            step = run(&mut map, &calm, step, committed);

            let leak = map.snapshot().unwrap().direction_leak(center).unwrap();
            if is_tracked_later {
                assert_eq!(leak, 0.0);
            } else {
                assert!(leak > 0.2, "{leak}");
            }

            run(&mut map, &calm, step, step + steps(2.0));
            assert_eq!(map.snapshot().unwrap().direction_leak(center), Some(0.0));
        }
    }

    #[test]
    fn new_alpha_changes_the_snapshot() {
        let mut map = LeakMap::new();
        assert!(map.snapshot().is_none());
        let plain = uniform(PLAIN);
        let scene = Scene {
            position: Vec3::ZERO,
            rotation: rotation(0.0, 30.0),
            color: &plain,
            exclusions: &[],
        };
        run(&mut map, &scene, 0, steps(2.0));
        // No snapshot before the first LUT
        assert!(map.snapshot().is_none());

        let direction = rotation(0.0, 30.0) * -Vec3::Z;
        map.set_alpha(&alpha_table(&[WOOD]));
        assert_eq!(map.snapshot().unwrap().direction_leak(direction), Some(0.0));
        map.set_alpha(&alpha_table(&[PLAIN]));
        assert_eq!(map.snapshot().unwrap().direction_leak(direction), Some(1.0));
        // One texel away still leaks, three don't
        map.set_alpha(&alpha_table(&[[PLAIN[0], PLAIN[1] + 8, PLAIN[2]]]));
        assert_eq!(map.snapshot().unwrap().direction_leak(direction), Some(1.0));
        map.set_alpha(&alpha_table(&[[PLAIN[0], PLAIN[1] + 25, PLAIN[2]]]));
        assert_eq!(map.snapshot().unwrap().direction_leak(direction), Some(0.0));

        let mut lut = vec![0; TEXEL_COUNT * 4];
        lut[texel_index(PLAIN) * 4 + 3] = 255;
        map.set_alpha_from_lut(&lut);
        assert_eq!(map.snapshot().unwrap().direction_leak(direction), Some(1.0));
    }

    #[test]
    fn cells_expire_and_light_changes_reset() {
        let plain = uniform(PLAIN);
        let view = |yaw| Scene {
            position: Vec3::ZERO,
            rotation: rotation(yaw, 30.0),
            color: &plain,
            exclusions: &[],
        };
        let in_first = rotation(-20.0, 30.0) * -Vec3::Z;
        let in_second = rotation(80.0, 30.0) * -Vec3::Z;

        // Expiry
        let mut map = LeakMap::new();
        map.set_alpha(&alpha_table(&[WOOD]));
        map.memory = 3.0;
        let step = run(&mut map, &view(0.0), 0, steps(2.0));
        assert_eq!(map.snapshot().unwrap().direction_leak(in_first), Some(0.0));
        run(&mut map, &view(90.0), step, step + steps(4.0));
        let snapshot = map.snapshot().unwrap();
        assert_eq!(snapshot.direction_leak(in_first), None);
        assert_eq!(snapshot.direction_leak(in_second), Some(0.0));

        // Light change
        let mut map = LeakMap::new();
        map.set_alpha(&alpha_table(&[WOOD]));
        let mut step = run(&mut map, &view(0.0), 0, steps(2.0));
        step = run(&mut map, &view(60.0), step, step + steps(2.0));
        step = run(&mut map, &view(0.0), step, step + steps(2.0));
        let snapshot = map.snapshot().unwrap();
        assert_eq!(snapshot.direction_leak(in_first), Some(0.0));
        assert_eq!(snapshot.direction_leak(in_second), Some(0.0));
        assert_eq!(map.light_resets(), 0);

        let brighter = uniform([100, 100, 255]);
        let lit = Scene {
            color: &brighter,
            ..view(0.0)
        };
        run(&mut map, &lit, step, step + steps(1.5));
        let snapshot = map.snapshot().unwrap();
        assert_eq!(map.light_resets(), 1);
        assert_eq!(snapshot.direction_leak(in_first), Some(0.0));
        assert_eq!(snapshot.direction_leak(in_second), None);
    }

    #[test]
    fn small_moves_are_tolerated_large_ones_reanchor() {
        let plain = uniform(PLAIN);
        let at = |position| Scene {
            position,
            rotation: rotation(0.0, 30.0),
            color: &plain,
            exclusions: &[],
        };
        let direction = rotation(0.0, 30.0) * -Vec3::Z;

        let mut map = LeakMap::new();
        map.set_alpha(&alpha_table(&[WOOD]));
        let mut step = run(&mut map, &at(Vec3::ZERO), 0, steps(2.0));
        step = run(
            &mut map,
            &at(Vec3::new(0.03, 0.0, 0.0)),
            step,
            step + steps(2.0),
        );
        assert_eq!(map.anchor(), Some(Vec3::ZERO));
        assert_eq!(map.anchor_moves(), 0);
        assert_eq!(map.snapshot().unwrap().direction_leak(direction), Some(0.0));

        let moved = Vec3::new(0.1, 0.0, 0.0);
        step = run(&mut map, &at(moved), step, step + steps(0.5));
        assert_eq!(map.anchor(), Some(Vec3::ZERO));
        assert_eq!(map.snapshot().unwrap().direction_leak(direction), Some(0.0));
        step = run(&mut map, &at(moved), step, step + steps(0.6));
        assert_eq!(map.anchor(), Some(moved));
        assert_eq!(map.anchor_moves(), 1);
        assert_eq!(map.snapshot().unwrap().known_fraction(), 0.0);
        run(&mut map, &at(moved), step, step + steps(1.5));
        assert_eq!(map.snapshot().unwrap().direction_leak(direction), Some(0.0));

        map.clear();
        assert_eq!(map.anchor(), None);
        assert!(map.snapshot().is_none());
        assert_eq!(map.known_fraction(), 0.0);
    }

    #[test]
    fn right_eye_finds_a_patch_behind_the_hand() {
        // Left camera, a wall 1.2 m in front of the eyes with a wooden patch
        let camera = Vec3::new(-0.032, 0.0, -0.06);
        let wall = -1.2;
        let patch = Vec3::new(0.1, -0.25, wall);
        let color = move |origin: Vec3, ray: Vec3| {
            let hit = origin + ray * ((wall - origin.z) / ray.z);
            if hit.distance(patch) < 0.08 {
                WOOD
            } else {
                PLAIN
            }
        };
        let mut map = LeakMap::new();
        map.set_alpha(&alpha_table(&[WOOD]));
        let scene = Scene {
            position: camera,
            rotation: rotation(0.0, 10.0),
            color: &color,
            exclusions: &[],
        };
        run(&mut map, &scene, 0, steps(2.0));
        let snapshot = map.snapshot().unwrap();

        let eye = Vec3::new(0.032, 0.0, 0.0);
        let hand = eye + (patch - eye).normalize() * 0.4;
        assert!(snapshot.lookup(eye, hand).unwrap() > 0.5);
        // The direction of the hand from the anchor misses it
        assert!(snapshot.direction_leak(hand - camera).unwrap() < 0.5);
        // The plain wall beside it
        let beside = eye + (patch + Vec3::new(0.3, 0.0, 0.0) - eye).normalize() * 0.4;
        assert!(snapshot.lookup(eye, beside).unwrap() < 0.1);

        let mut points = [beside; 28];
        points[..10].fill(hand);
        assert!(snapshot.hand_leak(&[eye], &points).unwrap() > 0.5);
        points[..10].fill(beside);
        assert!(snapshot.hand_leak(&[eye], &points).unwrap() < 0.1);
    }

    #[test]
    fn mostly_unknown_cells_give_none() {
        let plain = uniform(PLAIN);
        let scene = Scene {
            position: Vec3::ZERO,
            rotation: rotation(0.0, 30.0),
            color: &plain,
            exclusions: &[],
        };
        let mut map = LeakMap::new();
        map.set_alpha(&alpha_table(&[WOOD]));
        run(&mut map, &scene, 0, steps(2.0));
        let snapshot = map.snapshot().unwrap();

        let eye = Vec3::ZERO;
        let known = rotation(0.0, 30.0) * -Vec3::Z * 0.4;
        let unknown = rotation(180.0, 30.0) * -Vec3::Z * 0.4;
        assert_eq!(snapshot.lookup(eye, known), Some(0.0));
        assert_eq!(snapshot.lookup(eye, unknown), None);
        assert_eq!(snapshot.lookup(eye, eye), None);

        let mut points = [unknown; 28];
        assert_eq!(snapshot.hand_leak(&[eye], &points), None);
        points[..10].fill(known);
        assert_eq!(snapshot.hand_leak(&[eye], &points), None);
        points[..20].fill(known);
        assert_eq!(snapshot.hand_leak(&[eye], &points), Some(0.0));
        assert_eq!(snapshot.hand_leak(&[eye], &[]), None);
        assert_eq!(LeakSnapshot::unknown(eye).hand_leak(&[eye], &points), None);
    }

    #[test]
    fn ascii_dump_marks_leaks_and_unknown() {
        let color = |_: Vec3, ray: Vec3| if ray.x > 0.0 { WOOD } else { PLAIN };
        let scene = Scene {
            position: Vec3::ZERO,
            rotation: rotation(0.0, 30.0),
            color: &color,
            exclusions: &[],
        };
        let mut map = LeakMap::new();
        map.set_alpha(&alpha_table(&[WOOD]));
        run(&mut map, &scene, 0, steps(2.0));
        let snapshot = map.snapshot().unwrap();

        let text = snapshot.ascii_dump(&camera_rays(rotation(0.0, 30.0)), 40);
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 30);
        assert!(lines.iter().all(|line| line.len() == 40));
        assert!(lines[15].starts_with("....") && lines[15].ends_with("####"));
        let turned = snapshot.ascii_dump(&camera_rays(rotation(180.0, 30.0)), 40);
        assert!(turned.chars().filter(|c| *c != '\n').all(|c| c == '?'));
    }

    // Prints timings, run with --nocapture (debug builds are much slower than the device build)
    #[test]
    fn leak_map_timings() {
        let color = |_: Vec3, ray: Vec3| if ray.x > 0.2 { WOOD } else { PLAIN };
        let hands = [
            Vec3::new(-0.15, -0.3, -0.35).extend(0.12),
            Vec3::new(0.15, -0.3, -0.35).extend(0.12),
        ];
        let mut map = LeakMap::new();
        map.set_alpha(&alpha_table(&[WOOD]));

        let rays = camera_rays(rotation(0.0, 30.0));
        let samples = rays
            .iter()
            .map(|ray| (*ray, color(Vec3::ZERO, *ray)))
            .collect::<Vec<_>>();
        let iterations = 100;
        let start = Instant::now();
        for step in 0..iterations {
            map.observe(Vec3::ZERO, &samples, &hands, step as f64 * STEP);
        }
        let observe = start.elapsed() / iterations;

        let start = Instant::now();
        let snapshot = map.snapshot().unwrap();
        let snapshot_time = start.elapsed();

        let eyes = [Vec3::new(-0.032, 0.0, 0.06), Vec3::new(0.032, 0.0, 0.06)];
        let points = (0..28)
            .map(|i| Vec3::new(-0.15 + i as f32 * 0.005, -0.3, -0.35))
            .collect::<Vec<_>>();
        let start = Instant::now();
        let mut total = 0.0;
        for _ in 0..iterations {
            for _hand in 0..2 {
                total += snapshot.hand_leak(&eyes, &points).unwrap_or(0.0);
            }
        }
        let frame = start.elapsed() / iterations;

        println!(
            "leak map: observe {observe:?} (1200 samples, 2 hands), snapshot {snapshot_time:?}, \
             lookups per frame {frame:?} (2 hands x 2 eyes x 28 points), checksum {total}"
        );
    }
}
