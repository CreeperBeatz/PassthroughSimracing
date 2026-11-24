//! Problem view (`debug.alvr.problem_view`) and the `sep:` log, both read from the leak map.
//!
//! The view tints each direction around the head by how much the color key lets the room leak there:
//! nothing under `lo`, amber hatching up to `hi`, red cross-hatching above, grey dots where the map does
//! not know the direction. The CPU turns the leak map into one byte per cell (`problem_cells`), uploaded
//! as a 96 x 96 texture that the stream shader looks up along each view ray, see stream.wgsl.

use crate::{
    LEAK_FULL, LEAK_MAP_CELLS, LEAK_MAP_SIZE, LEAK_UNKNOWN, LeakSnapshot, leak_map::cell_direction,
};
use alvr_common::glam::Vec3;
use std::cell::Cell;
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingResource, BindingType, Buffer, BufferBindingType,
    BufferDescriptor, BufferUsages, Device, Extent3d, Origin3d, Queue, ShaderStages,
    TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect, TextureDescriptor,
    TextureDimension, TextureFormat, TextureSampleType, TextureUsages, TextureViewDimension,
};

// Uniforms of stream.wgsl `ProblemView`: xyz anchor, w on (0/1); x room, y dim
const UNIFORMS_SIZE: u64 = 32;

/// Class of a cell in the problem view
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemClass {
    None,
    Medium,
    High,
    Unknown,
}

impl ProblemClass {
    /// Texel value of the class (R8Unorm), decoded by stream.wgsl as round(value * 3)
    pub fn texel(self) -> u8 {
        match self {
            ProblemClass::None => 0,
            ProblemClass::Medium => 85,
            ProblemClass::High => 170,
            ProblemClass::Unknown => 255,
        }
    }
}

/// Leak (0..1) of a cell value of the leak map, None if unknown
pub fn cell_leak(value: u8) -> Option<f32> {
    (value != LEAK_UNKNOWN).then(|| f32::from(value) / f32::from(LEAK_FULL))
}

/// Class of a cell value: under `lo` nothing, from `lo` medium, from `hi` high (leaks 0..1)
pub fn problem_class(value: u8, lo: f32, hi: f32) -> ProblemClass {
    match cell_leak(value) {
        None => ProblemClass::Unknown,
        Some(leak) if leak >= hi.max(lo) => ProblemClass::High,
        Some(leak) if leak >= lo => ProblemClass::Medium,
        Some(_) => ProblemClass::None,
    }
}

/// One texel per cell of the leak map (row-major like `LeakSnapshot::cells`), see `ProblemClass::texel`
pub fn problem_cells(snapshot: &LeakSnapshot, lo: f32, hi: f32) -> Vec<u8> {
    snapshot
        .cells
        .iter()
        .map(|value| problem_class(*value, lo, hi).texel())
        .collect()
}

/// Number of known cells of the map
pub fn known_cells(snapshot: &LeakSnapshot) -> usize {
    snapshot
        .cells
        .iter()
        .filter(|value| **value != LEAK_UNKNOWN)
        .count()
}

/// The `count` leakiest known cells: unit direction from the anchor and leak (0..1), leakiest first
/// (ties: lower index first). Cells that don't leak at all are left out.
pub fn leakiest_cells(snapshot: &LeakSnapshot, count: usize) -> Vec<(Vec3, f32)> {
    let mut cells = (0..LEAK_MAP_CELLS)
        .filter_map(|index| {
            let leak = cell_leak(snapshot.cells[index])?;
            (leak > 0.0).then_some((index, leak))
        })
        .collect::<Vec<_>>();
    cells.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    cells.truncate(count);

    cells
        .into_iter()
        .map(|(index, leak)| (cell_direction(index), leak))
        .collect()
}

/// Yaw (degrees, 0 ahead along -Z, positive to the left) and pitch (degrees, positive up) of a
/// stage-space direction, for the log
pub fn direction_degrees(direction: Vec3) -> (f32, f32) {
    let direction = direction.normalize_or_zero();
    let yaw = if direction.x.abs() + direction.z.abs() > 1e-6 {
        (-direction.x).atan2(-direction.z).to_degrees()
    } else {
        0.0
    };
    let pitch = direction.y.clamp(-1.0, 1.0).asin().to_degrees();

    (yaw, pitch)
}

/// Text of the `sep:` log: per hand class the recall (share of fresh hand label pixels the LUT keeps,
/// None without labels) and the observed leak of each hand, then the known cells and the leakiest cells
/// as yaw/pitch@leak.
pub fn sep_log(recall: Option<f32>, leak: [f32; 2], snapshot: Option<&LeakSnapshot>) -> String {
    let recall = recall.map_or_else(|| "-".to_string(), |recall| format!("{recall:.2}"));
    let mut text = format!(
        "sep: hands recall {recall} leak L {:.2} R {:.2}, arms recall - leak -",
        leak[0], leak[1]
    );
    match snapshot {
        Some(snapshot) => {
            text.push_str(&format!(
                ", known cells {}, leakiest",
                known_cells(snapshot)
            ));
            for (direction, leak) in leakiest_cells(snapshot, 10) {
                let (yaw, pitch) = direction_degrees(direction);
                text.push_str(&format!(" {yaw:.0}/{pitch:.0}@{leak:.2}"));
            }
        }
        None => text.push_str(", no leak map"),
    }

    text
}

/// Upload of the problem view: the leak map anchor and one texel per cell (`problem_cells`)
pub struct ProblemViewData<'a> {
    pub anchor: Vec3,
    pub cells: &'a [u8],
    /// How much the room shows through the game (0..1) and how much the game is dimmed (0..1)
    pub room: f32,
    pub dim: f32,
}

/// Bind group 2 of the main stream pipeline: the cells and the uniforms
pub(crate) fn problem_view_layout(device: &Device) -> BindGroupLayout {
    device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("problem view"),
        entries: &[
            BindGroupLayoutEntry {
                binding: 0,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Texture {
                    sample_type: TextureSampleType::Float { filterable: false },
                    view_dimension: TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            BindGroupLayoutEntry {
                binding: 1,
                visibility: ShaderStages::FRAGMENT,
                ty: BindingType::Buffer {
                    ty: BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

/// GPU side: a 96 x 96 R8 texture and 32 bytes of uniforms. Zeroed at creation (off); nothing is
/// written while it stays off.
pub(crate) struct ProblemViewGpu {
    texture: wgpu::Texture,
    buffer: Buffer,
    pub bind_group: BindGroup,
    is_on: Cell<bool>,
}

impl ProblemViewGpu {
    pub fn new(device: &Device, layout: &BindGroupLayout) -> Self {
        let size = LEAK_MAP_SIZE as u32;
        let texture = device.create_texture(&TextureDescriptor {
            label: Some("problem view"),
            size: Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: TextureFormat::R8Unorm,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let buffer = device.create_buffer(&BufferDescriptor {
            label: Some("problem view"),
            size: UNIFORMS_SIZE,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let view = texture.create_view(&Default::default());
        let bind_group = device.create_bind_group(&BindGroupDescriptor {
            label: Some("problem view"),
            layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(&view),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });

        Self {
            texture,
            buffer,
            bind_group,
            is_on: Cell::new(false),
        }
    }

    /// Show the cells (`problem_cells`, ignored if the length is wrong), or turn the view off (None)
    pub fn write(&self, queue: &Queue, data: Option<ProblemViewData>) {
        let Some(data) = data.filter(|data| data.cells.len() == LEAK_MAP_CELLS) else {
            if self.is_on.replace(false) {
                queue.write_buffer(&self.buffer, 0, &[0; UNIFORMS_SIZE as usize]);
            }
            return;
        };
        self.is_on.set(true);
        let size = LEAK_MAP_SIZE as u32;
        queue.write_texture(
            TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            data.cells,
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size),
                rows_per_image: Some(size),
            },
            Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
        );
        let floats = problem_uniforms(&data);
        let bytes = floats
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        queue.write_buffer(&self.buffer, 0, &bytes);
    }
}

fn problem_uniforms(data: &ProblemViewData) -> [f32; 8] {
    [
        data.anchor.x,
        data.anchor.y,
        data.anchor.z,
        1.0,
        data.room.clamp(0.0, 1.0),
        data.dim.clamp(0.0, 1.0),
        0.0,
        0.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LEAK_MAP_SIZE;

    fn snapshot_with(values: &[(usize, u8)]) -> LeakSnapshot {
        let mut snapshot = LeakSnapshot::unknown(Vec3::ZERO);
        for (index, value) in values {
            snapshot.cells[*index] = *value;
        }
        snapshot
    }

    #[test]
    fn classes_follow_the_thresholds() {
        let (lo, hi) = (0.03, 0.15);
        assert_eq!(problem_class(LEAK_UNKNOWN, lo, hi), ProblemClass::Unknown);
        assert_eq!(problem_class(0, lo, hi), ProblemClass::None);
        assert_eq!(problem_class(5, lo, hi), ProblemClass::None); // 0.020
        assert_eq!(problem_class(8, lo, hi), ProblemClass::Medium); // 0.031
        assert_eq!(problem_class(38, lo, hi), ProblemClass::Medium); // 0.1496
        assert_eq!(problem_class(39, lo, hi), ProblemClass::High); // 0.154
        assert_eq!(problem_class(LEAK_FULL, lo, hi), ProblemClass::High);
        // hi below lo: everything from lo is high
        assert_eq!(problem_class(8, 0.03, 0.01), ProblemClass::High);
    }

    #[test]
    fn texels_decode_to_the_class() {
        for (class, expected) in [
            (ProblemClass::None, 0),
            (ProblemClass::Medium, 1),
            (ProblemClass::High, 2),
            (ProblemClass::Unknown, 3),
        ] {
            // As stream.wgsl: round(value / 255 * 3)
            assert_eq!(
                (f32::from(class.texel()) / 255.0 * 3.0).round() as u32,
                expected
            );
        }
    }

    #[test]
    fn cells_and_counts() {
        let snapshot = snapshot_with(&[(0, 0), (1, 20), (2, LEAK_FULL), (3, 100)]);
        let cells = problem_cells(&snapshot, 0.03, 0.15);
        assert_eq!(cells.len(), LEAK_MAP_CELLS);
        assert_eq!(&cells[..5], &[0, 85, 170, 170, 255]);
        assert_eq!(known_cells(&snapshot), 4);
    }

    #[test]
    fn leakiest_are_sorted_and_limited() {
        let values = (0..15).map(|i| (i * 7, (i * 10) as u8)).collect::<Vec<_>>();
        let snapshot = snapshot_with(&values);
        let top = leakiest_cells(&snapshot, 10);
        assert_eq!(top.len(), 10);
        assert!((top[0].1 - 140.0 / 254.0).abs() < 1e-6);
        assert!(top.windows(2).all(|pair| pair[0].1 >= pair[1].1));
        assert!(
            top.iter()
                .all(|(direction, _)| (direction.length() - 1.0).abs() < 1e-4)
        );
        // Cells with no leak are left out
        assert!(leakiest_cells(&snapshot_with(&[(0, 0)]), 10).is_empty());
    }

    #[test]
    fn degrees_of_directions() {
        let close =
            |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01;
        assert!(close(direction_degrees(Vec3::NEG_Z), (0.0, 0.0)));
        assert!(close(direction_degrees(Vec3::NEG_X), (90.0, 0.0)));
        assert!(close(direction_degrees(Vec3::NEG_Y), (0.0, -90.0)));
        // The center cell of the octahedral map looks straight down
        let center = (LEAK_MAP_SIZE / 2) * LEAK_MAP_SIZE + LEAK_MAP_SIZE / 2;
        assert!(direction_degrees(cell_direction(center)).1 < -85.0);
    }

    #[test]
    fn sep_log_text() {
        let snapshot = snapshot_with(&[(0, LEAK_FULL), (1, 0)]);
        let text = sep_log(Some(0.912), [0.1, 0.25], Some(&snapshot));
        assert!(text.starts_with("sep: hands recall 0.91 leak L 0.10 R 0.25"));
        assert!(text.contains("known cells 2, leakiest "));
        assert!(text.ends_with("@1.00"));
        let text = sep_log(None, [0.0, 0.0], None);
        assert!(text.contains("recall -"));
        assert!(text.ends_with("no leak map"));
    }
}
