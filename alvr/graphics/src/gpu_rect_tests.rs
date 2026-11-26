//! Stage S3 (`debug.alvr.gpu_rect`, `debug.alvr.fast_srgb`, gpu_rect.rs): the safety net. A child of tests.rs
//! (its helpers). Random scenes near a rig-like layout: the depth mask rendered without the scissor must be
//! 0 outside the rectangle in every channel the main pass reads, the scissored mask must equal it inside,
//! and the main pass with the rectangles (and the lower layer's crop) must write exactly today's pixels,
//! which must also be those of the stream shader before stage S3. A failing scene is a bug in the bound.

use super::*;
use crate::{
    gpu_rect::{self, EyeRect, RectCloud, RectField, RectInput},
    motion_cloud_gpu::create_cloud_read_bind_group,
    room_memory::MemoryInput,
};
use alvr_common::glam::EulerRot;

// The swapchain of the scenes: the mask and the focus area are a fifth of it (48 x 40), so the main pass
// samples them between texels as on the headset
const RECT_VIEW: UVec2 = UVec2::new(240, 200);
const RECT_MASK: UVec2 = UVec2::new(48, 40);
const RECT_DEPTH: u32 = 64;
const RECT_CELLS: u32 = RECT_DEPTH / 2;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1_u64 << 24) as f32
    }

    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.next()
    }

    fn chance(&mut self, p: f32) -> bool {
        self.next() < p
    }

    fn vec(&mut self, low: Vec3, high: Vec3) -> Vec3 {
        Vec3::new(
            self.range(low.x, high.x),
            self.range(low.y, high.y),
            self.range(low.z, high.z),
        )
    }

    fn unit(&mut self) -> Vec3 {
        loop {
            let v = self.vec(Vec3::splat(-1.0), Vec3::splat(1.0));
            if v.length_squared() > 0.01 && v.length_squared() <= 1.0 {
                return v.normalize();
            }
        }
    }

    fn byte(&mut self) -> u8 {
        (self.next() * 256.0).min(255.0) as u8
    }
}

/// The stream shader as it was before stage S3: its lines taken out again
fn shader_without_s3(source: String) -> String {
    let mut source = source;
    let tap_block_start =
        "    // `debug.alvr.gpu_rect` (gpu_rect.rs): outside the rectangle of the mask taps";
    let tap_block_end = "    let debug_view = u32(depth_u.region.w + 0.5);";
    let start = source.find(tap_block_start).unwrap();
    let end = source[start..].find(tap_block_end).unwrap() + start;
    source.replace_range(start..end, "");
    let helpers_start = "// `debug.alvr.gpu_rect`: whether uv lies in a rectangle";
    let helpers_end = "fn depth_passthrough(";
    let start = source.find(helpers_start).unwrap();
    let end = source[start..].find(helpers_end).unwrap() + start;
    source.replace_range(start..end, "");
    let consts_start = "// `FAST_SRGB`: ((color + 0.055) / 1.055)^2.4";
    let consts_end = "const DIV12: f32";
    let start = source.find(consts_start).unwrap();
    let end = source[start..].find(consts_end).unwrap() + start;
    source.replace_range(start..end, "");
    let override_start = "// `debug.alvr.fast_srgb` (stage S3";
    let override_end = "override ENABLE_UPSCALING";
    let start = source.find(override_start).unwrap();
    let end = source[start..].find(override_end).unwrap() + start;
    source.replace_range(start..end, "\n");
    for (new, old) in [
        (
            "        var highValues: vec3f;\n        if FAST_SRGB {\n            let c3 = vec3f(FAST_SRGB_C3);\n            \
             let c2 = vec3f(FAST_SRGB_C2);\n            let c1 = vec3f(FAST_SRGB_C1);\n            let c0 = \
             vec3f(FAST_SRGB_C0);\n            highValues = ((c3 * color + c2) * color + c1) * color + c0;\n        \
             } else {\n            highValues = pow((color + vec3f(0.055)) * DIV1, GAMMA);\n        }\n",
            "        let highValues = pow((color + vec3f(0.055)) * DIV1, GAMMA);\n",
        ),
        (
            "    var mask_sample = vec4f(0.0);\n    if mask_tap {\n        mask_sample = \
             textureSampleLevel(depth_mask, stream_sampler, uv, 0.0);\n    }\n",
            "    let mask_sample = textureSampleLevel(depth_mask, stream_sampler, uv, 0.0);\n",
        ),
        (
            "        var hand = vec4f(0.0);\n        if focus_tap {\n            hand = \
             textureSampleLevel(focus_area, stream_sampler, uv, 0.0);\n        }\n",
            "        let hand = textureSampleLevel(focus_area, stream_sampler, uv, 0.0);\n",
        ),
    ] {
        assert_eq!(source.matches(new).count(), 1, "{new}");
        source = source.replace(new, old);
    }
    // The uniform block (depth_common.wgsl, appended) keeps its new members: not read any more
    assert!(
        !source.contains("depth_u.gpu_rect")
            && !source.contains("FAST_SRGB")
            && !source.contains("_tap"),
        "{source}"
    );
    source
}

/// The cloud of a scene: its params, the field's camera, whether it may open, the field's cells, the reach
/// times
struct SceneCloud {
    params: CloudParams,
    camera: ViewParams,
    open: bool,
    cells: Vec<u8>,
    reach_s: [f32; 2],
}

struct RectScene {
    config: DepthPassthroughConfig,
    input: DepthRenderInput,
    views: [ViewParams; 2],
    view_idx: usize,
    playzone_views: Option<[ViewParams; 2]>,
    // Depth of each eye (R32Float texel values), the playzone (RGBA32F)
    depth: [Vec<f32>; 2],
    playzone: Vec<f32>,
    cloud: Option<SceneCloud>,
    stream: Vec<u8>,
    srgb: bool,
    crop_lower: bool,
    pad: f32,
    /// `cloud_own` is on (with capture-time capsules on the depth's surface)
    own: bool,
}

fn depth_fov() -> Fov {
    Fov {
        left: -0.942,
        right: 0.698,
        up: 0.768,
        down: -0.960,
    }
}

/// A random depth image of a camera (texel values of `encode_depth`, 0: no data): a wall, rectangles of
/// near surfaces (some at 10-20 cm), holes, and surfaces put on the faces of the boxes
fn random_depth(r: &mut Lcg, camera: &ViewParams, boxes: &[(Mat4, Vec3)]) -> (Vec<f32>, Vec<f32>) {
    let size = RECT_DEPTH as usize;
    let wall = r.range(0.8, 4.0);
    let mut distance = vec![wall; size * size];
    let mut hit = vec![false; size * size];
    for _ in 0..(r.next() * 7.0) as usize {
        let x0 = (r.next() * size as f32) as usize;
        let y0 = (r.next() * size as f32) as usize;
        let w = 1 + (r.next() * 30.0) as usize;
        let h = 1 + (r.next() * 30.0) as usize;
        let near = if r.chance(0.4) {
            r.range(0.1, 0.2)
        } else {
            r.range(0.2, 1.5)
        };
        let hole = r.chance(0.15);
        for y in y0..(y0 + h).min(size) {
            for x in x0..(x0 + w).min(size) {
                distance[y * size + x] = if hole {
                    0.0
                } else {
                    near + r.range(-0.01, 0.01)
                };
            }
        }
    }
    // The boxes' faces: rays of the camera hit by a box take its near face (often at the region's border)
    if r.chance(0.6) {
        let to_world = depth_mask::pose_to_mat4(camera.pose);
        let (lo, hi) = (
            Vec2::new(camera.fov.left.tan(), camera.fov.down.tan()),
            Vec2::new(camera.fov.right.tan(), camera.fov.up.tan()),
        );
        let shift = r.range(-0.03, 0.03);
        for y in 0..size {
            for x in 0..size {
                let uv = Vec2::new(
                    (x as f32 + 0.5) / size as f32,
                    (y as f32 + 0.5) / size as f32,
                );
                let tangent = lo + (hi - lo) * uv;
                let dir = to_world.transform_vector3(tangent.extend(-1.0));
                for (box_to_world, half) in boxes {
                    let inverse = box_to_world.inverse();
                    let o = inverse.transform_point3(camera.pose.position);
                    let d = inverse.transform_vector3(dir);
                    let t1 = (-*half - o) / d;
                    let t2 = (*half - o) / d;
                    let near = t1.min(t2).max_element();
                    let far = t1.max(t2).min_element();
                    if far >= near.max(0.0) && near > 0.0 {
                        let z = near + shift;
                        if z > 0.1 && z < distance[y * size + x] {
                            distance[y * size + x] = z;
                            hit[y * size + x] = true;
                        }
                    }
                }
            }
        }
        // The worst case of the bound: a near occluder right beside the box (the 3 x 3 neighbourhood of a
        // texel on the occluder reaches the box; the occluder seen from the eye is shifted by the camera's
        // offset over its distance)
        if r.chance(0.6) {
            let ring = 1 + (r.next() * 6.0) as usize;
            let occluder = r.range(0.1, 0.16);
            let before = hit.clone();
            for y in 0..size {
                for x in 0..size {
                    if before[y * size + x] {
                        continue;
                    }
                    let near_hit = (y.saturating_sub(ring)..(y + ring + 1).min(size)).any(|ny| {
                        (x.saturating_sub(ring)..(x + ring + 1).min(size))
                            .any(|nx| before[ny * size + nx])
                    });
                    if near_hit {
                        distance[y * size + x] = occluder;
                    }
                }
            }
        }
    }
    let encoded = distance
        .iter()
        .map(|&z| {
            if z <= 0.0 {
                0.0
            } else {
                depth_mask::encode_depth(z, NEAR, f32::INFINITY)
            }
        })
        .collect();
    (encoded, distance)
}

fn random_scene(seed: u64) -> RectScene {
    let r = &mut Lcg(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5bd1_e995);
    for _ in 0..3 {
        r.next();
    }
    let head = Vec3::new(
        r.range(-0.05, 0.05),
        1.15 + r.range(-0.05, 0.05),
        r.range(-0.05, 0.05),
    );
    let orientation = Quat::from_euler(
        EulerRot::YXZ,
        r.range(-0.7, 0.7),
        r.range(-0.9, 0.3),
        r.range(-0.08, 0.08),
    );
    let fov = Fov {
        left: -r.range(0.75, 0.95),
        right: r.range(0.6, 0.8),
        up: r.range(0.65, 0.8),
        down: -r.range(0.8, 0.97),
    };
    let views = [-0.032, 0.032].map(|x| ViewParams {
        pose: Pose {
            orientation,
            position: head + orientation * Vec3::new(x, 0.0, 0.0),
        },
        fov,
    });
    let eye_positions =
        views.map(|view| view.pose.position + r.vec(Vec3::splat(-0.004), Vec3::splat(0.004)));
    let view_idx = usize::from(r.chance(0.3));

    // The play area: 1-4 boxes below and in front of the head, with domes, turned
    let area_to_world = Mat4::from_rotation_translation(
        Quat::from_rotation_y(r.range(-0.6, 0.6)),
        head + Vec3::new(0.0, -0.55, -0.45),
    );
    let random_box = |r: &mut Lcg| {
        let mut area_box = AreaBox::new(
            r.vec(Vec3::new(-0.45, -0.25, -0.35), Vec3::new(0.45, 0.35, 0.35)),
            r.vec(Vec3::splat(0.04), Vec3::new(0.3, 0.25, 0.3)),
        );
        area_box.yaw = r.range(-1.0, 1.0);
        for dome in &mut area_box.domes {
            if r.chance(0.4) {
                *dome = r.range(0.0, 0.15);
            }
        }
        area_box
    };
    let mut area = PlayArea::single(random_box(r));
    for _ in 0..(r.next() * 4.0) as usize {
        area.push_box(random_box(r));
    }
    let boxes: Vec<(Mat4, Vec3)> = area
        .boxes()
        .iter()
        .map(|area_box| {
            (
                area_to_world
                    * Mat4::from_rotation_translation(
                        Quat::from_rotation_y(area_box.yaw),
                        area_box.center,
                    ),
                area_box.half,
            )
        })
        .collect();
    let region = PlacedArea {
        area_to_world,
        area,
    };

    let depth_views = eye_positions.map(|eye| {
        let offset = if r.chance(0.2) {
            Vec3::ZERO
        } else {
            r.unit() * r.range(0.0, 0.025)
        };
        ViewParams {
            pose: Pose {
                orientation: orientation
                    * Quat::from_euler(
                        EulerRot::YXZ,
                        r.range(-0.06, 0.06),
                        r.range(-0.06, 0.06),
                        r.range(-0.02, 0.02),
                    ),
                position: eye + offset,
            },
            fov: depth_fov(),
        }
    });
    let depth = [0, 1].map(|i| random_depth(r, &depth_views[i], &boxes).0);

    // The hands, the colour key
    let ahead = |r: &mut Lcg, near: f32, far: f32| {
        head + orientation
            * Vec3::new(
                r.range(-0.35, 0.35),
                r.range(-0.4, 0.15),
                -r.range(near, far),
            )
    };
    let key = r.chance(0.7);
    let color_key = key.then(|| {
        let mut hands = [None; 2];
        let mut bridges = [None; 2];
        for hand in 0..2 {
            if !r.chance(0.6) {
                continue;
            }
            let centre = if r.chance(0.7) {
                ahead(r, 0.2, 0.7)
            } else {
                head + r.unit() * r.range(0.2, 0.8)
            };
            let radius = r.range(0.008, 0.025);
            let shape = HandShape {
                segments: std::array::from_fn(|_| {
                    [
                        (centre + r.unit() * r.range(0.0, 0.08)).extend(radius),
                        (centre + r.unit() * r.range(0.0, 0.08)).extend(radius),
                    ]
                }),
            };
            let sweep = if r.chance(0.3) {
                HandSweep {
                    direction: r.unit(),
                    forward: r.range(0.0, 0.05),
                    backward: r.range(0.0, 0.05),
                }
            } else {
                HandSweep::NONE
            };
            hands[hand] = Some((
                shape,
                HandZones {
                    core: r.range(-0.01, 0.01),
                    falloff: r.range(0.002, 0.01),
                    rim: r.range(0.0, 0.02),
                    solid: std::array::from_fn(|_| r.next()),
                    sweep,
                },
            ));
            if r.chance(0.3) {
                bridges[hand] = WristBridge::new(
                    centre,
                    r.unit(),
                    r.range(0.05, 0.2),
                    r.range(0.01, 0.04),
                    r.next(),
                );
            }
        }
        ColorKeyInput {
            hands,
            leak: [r.range(0.0, 0.5), r.range(0.0, 0.5)],
            bridges,
        }
    });

    let memory = (key && r.chance(0.4)).then(|| MemoryInput {
        params: crate::MemoryParams {
            size: 64,
            skip_moving: r.chance(0.5),
            view: (r.next() * 3.0) as u8,
            ..Default::default()
        },
        capture_ns: None,
        hands: [None; 2],
        hand_sources: [crate::CaptureHand::None; 2],
        reach: [r.range(0.0, 0.1), r.range(0.0, 0.1)],
        hands_away: false,
    });
    let playzone_views = (key && r.chance(0.6)).then(|| {
        let camera = ViewParams {
            pose: Pose {
                orientation: orientation * Quat::from_rotation_y(r.range(-0.4, 0.4)),
                position: head + r.vec(Vec3::splat(-0.1), Vec3::splat(0.1)),
            },
            fov: depth_fov(),
        };
        [camera; 2]
    });
    let playzone = (0..RECT_DEPTH * RECT_DEPTH)
        .flat_map(|_| {
            [
                if r.chance(0.2) {
                    0.0
                } else {
                    r.range(0.2, 3.0)
                },
                if r.chance(0.5) { 1.0 } else { 0.0 },
                if r.chance(0.5) { 1.0 } else { 0.0 },
                r.range(0.0, 8.0),
            ]
        })
        .collect();

    let mut arms = [None; 2];
    let mut arm_gaps = [None; 2];
    for hand in 0..2 {
        if r.chance(0.5) {
            arms[hand] = Some(ArmMotion {
                wrist: ahead(r, 0.2, 0.6),
                direction: r.unit(),
                displacement: r.unit() * r.range(0.0, 0.25),
                weight_len: if r.chance(0.5) {
                    r.range(0.1, 0.4)
                } else {
                    0.0
                },
                mode: 1,
                target_ns: 0,
                history_ns: 0,
                wrist_history: Vec3::ZERO,
                extrapolated: 0.0,
            });
        }
        if r.chance(0.3) {
            arm_gaps[hand] = Some(ArmGap {
                wrist: ahead(r, 0.2, 0.6),
                axis: r.unit(),
                length: r.range(0.02, 0.08),
            });
        }
    }
    let random_window = |r: &mut Lcg| RegionOverride {
        volume_to_world: Mat4::from_rotation_translation(
            Quat::from_rotation_y(r.range(-1.0, 1.0)),
            ahead(r, 0.25, 0.9),
        ),
        half_extents: r.vec(Vec3::splat(0.03), Vec3::splat(0.15)),
    };
    let mut lever_windows = [None; MAX_LEVER_WINDOWS];
    if r.chance(0.3) {
        for window in lever_windows
            .iter_mut()
            .take(1 + usize::from(r.chance(0.5)))
        {
            *window = Some(LeverWindow {
                region: random_window(r),
                open: r.range(0.2, 1.0),
            });
        }
    }
    let mut green_windows = [None; MAX_GREEN_WINDOWS];
    if r.chance(0.25) {
        for window in green_windows
            .iter_mut()
            .take(1 + usize::from(r.chance(0.5)))
        {
            *window = Some(random_window(r));
        }
    }

    // The depth cloud: a field built from a random image of an older camera the way field_main builds it
    // (window_distance is the reference of rows_main and field_main), held speeds random everywhere
    let config = DepthPassthroughConfig {
        region: DepthMaskRegion::Distance { max_distance: 1.0 },
        edge_feathering: r.range(0.005, 0.04),
        lighting: Switch::Disabled,
        debug_view: if r.chance(0.25) {
            DepthPassthroughDebugView::TintMask
        } else {
            DepthPassthroughDebugView::Off
        },
        flip_depth_y: r.chance(0.3),
    };
    let cloud = (key && r.chance(0.5)).then(|| {
        let window = 4 + (r.next() * 21.0) as u8;
        let truncation = cloud_truncation(window);
        let params = CloudParams {
            mode: 1 + u8::from(r.chance(0.4)),
            cap: if r.chance(0.1) {
                truncation + 4.0
            } else {
                r.range(2.0, truncation)
            },
            fixed: if r.chance(0.2) {
                r.range(2.0, truncation)
            } else {
                0.0
            },
            fetch_m: r.range(0.3, 0.6),
            trail: [0.0, 6.0, 24.0][(r.next() * 3.0) as usize],
            trail_hands: r.chance(0.5),
            window,
            ..CloudParams::default()
        };
        let camera = ViewParams {
            pose: Pose {
                orientation: orientation
                    * Quat::from_euler(
                        EulerRot::YXZ,
                        r.range(-0.08, 0.08),
                        r.range(-0.08, 0.08),
                        0.0,
                    ),
                position: eye_positions[view_idx] + r.unit() * r.range(0.0, 0.04),
            },
            fov: depth_fov(),
        };
        let (_, distance) = random_depth(r, &camera, &boxes);
        let size = RECT_DEPTH as usize;
        let to_world = depth_mask::pose_to_mat4(camera.pose);
        let (lo, hi) = (
            Vec2::new(camera.fov.left.tan(), camera.fov.down.tan()),
            Vec2::new(camera.fov.right.tan(), camera.fov.up.tan()),
        );
        let world_to_area = region.area_to_world.inverse();
        let inside: Vec<bool> = (0..size * size)
            .map(|i| {
                let (x, y) = (i % size, i / size);
                let mut uv = Vec2::new(
                    (x as f32 + 0.5) / size as f32,
                    (y as f32 + 0.5) / size as f32,
                );
                if config.flip_depth_y {
                    uv.y = 1.0 - uv.y;
                }
                let z = distance[i];
                if z <= 0.0 {
                    return false;
                }
                let tangent = lo + (hi - lo) * uv;
                let point = to_world.transform_point3((tangent * z).extend(-z));
                region.area.signed_distance(
                    world_to_area.transform_point3(point),
                    config.edge_feathering,
                ) <= 0.0
            })
            .collect();
        let (cells, cells_x, cells_y) = cells_inside(&inside, size, size);
        let phi = window_distance(&cells, cells_x, cells_y, window);
        let cells = phi
            .iter()
            .flat_map(|&phi| {
                [
                    (255.0 * (0.5 + 0.5 * phi / truncation))
                        .round()
                        .clamp(0.0, 255.0) as u8,
                    if r.chance(0.7) {
                        128 + r.byte() / 2
                    } else {
                        r.byte()
                    },
                    r.byte(),
                    r.byte(),
                ]
            })
            .collect();
        SceneCloud {
            params,
            camera,
            open: r.chance(0.9),
            cells,
            reach_s: [r.range(0.0, 0.4), r.range(0.0, 0.4)],
        }
    });
    let key_crop = (key && r.chance(0.15)).then(|| {
        let min = Vec2::new(r.range(0.0, 0.5), r.range(0.0, 0.5));
        [(min, min + Vec2::splat(0.4)); 2]
    });

    let input = DepthRenderInput {
        frame: r.chance(0.95).then_some(DepthFrame {
            swapchain_index: 0,
            near: NEAR,
            far: f32::INFINITY,
            depth_views,
        }),
        eye_positions,
        color_key,
        arms,
        arm_gaps,
        edge_cut: if r.chance(0.3) {
            r.range(0.17, 0.84)
        } else {
            0.0
        },
        lever_windows,
        green_windows,
        capture_playzone: false,
        playzone_view: r.chance(0.3),
        memory,
        cloud: cloud.as_ref().map(|cloud| CloudInput {
            params: cloud.params,
            capture_ns: Some(0),
            display_ns: 0,
            shown: true,
            own_gate: [0.0; 2],
        }),
        key_lower_rects: key_crop,
        region_override: Some(region),
        game_dim: if r.chance(0.2) {
            r.range(0.0, 1.0)
        } else {
            0.0
        },
        game_see: if r.chance(0.1) {
            r.range(0.0, 1.0)
        } else {
            0.0
        },
    };
    let stream = (0..32 * 32)
        .flat_map(|_| [r.byte(), r.byte(), r.byte(), 255])
        .collect();

    let mut scene = RectScene {
        config,
        input,
        views,
        view_idx,
        playzone_views,
        depth,
        playzone,
        cloud,
        stream,
        srgb: r.chance(0.5),
        crop_lower: key && r.chance(0.7),
        pad: if r.chance(0.3) {
            0.0
        } else {
            r.range(0.0, 0.03)
        },
        own: false,
    };
    // Drawn last, so that the scenes before `cloud_own` are unchanged
    random_own(r, &mut scene);
    scene
}

/// `debug.alvr.cloud_own` in 60 % of the scenes with the cloud: the room memory's capture-time capsules of
/// each hand (often from the history, the rule's case) lie on a surface of this eye's depth image, the
/// gates are random (often full), the distance 5-40 mm. A scene without the memory gets one
fn random_own(r: &mut Lcg, scene: &mut RectScene) {
    let (Some(cloud), Some(frame)) = (&mut scene.input.cloud, scene.input.frame) else {
        return;
    };
    if !r.chance(0.6) {
        return;
    }
    cloud.params.own = r.range(0.005, 0.04);
    cloud.own_gate = [0, 1].map(|_| if r.chance(0.5) { 1.0 } else { r.next() });
    let mut memory = scene.input.memory.unwrap_or(MemoryInput {
        params: crate::MemoryParams {
            size: 64,
            ..Default::default()
        },
        capture_ns: None,
        hands: [None; 2],
        hand_sources: [crate::CaptureHand::None; 2],
        reach: [0.0; 2],
        hands_away: false,
    });
    // A point of the depth image as the mask reads it (row 0 at the bottom unless flipped)
    let camera = frame.depth_views[scene.view_idx];
    let size = RECT_DEPTH as usize;
    let depth = &scene.depth[scene.view_idx];
    let to_world = depth_mask::pose_to_mat4(camera.pose);
    let (lo, hi) = (
        Vec2::new(camera.fov.left.tan(), camera.fov.down.tan()),
        Vec2::new(camera.fov.right.tan(), camera.fov.up.tan()),
    );
    for hand in 0..2 {
        memory.hands[hand] = None;
        memory.hand_sources[hand] = crate::CaptureHand::None;
        if !r.chance(0.8) {
            continue;
        }
        let (x, y) = (
            (r.next() * size as f32) as usize,
            (r.next() * size as f32) as usize,
        );
        let encoded = depth[y.min(size - 1) * size + x.min(size - 1)];
        if encoded <= 0.0 {
            continue;
        }
        // encode_depth with an infinite far plane, inverted
        let z = 2.0 * NEAR / (2.0 - 2.0 * encoded);
        let mut ndc = Vec2::new(
            (x as f32 + 0.5) / size as f32,
            (y as f32 + 0.5) / size as f32,
        );
        if scene.config.flip_depth_y {
            ndc.y = 1.0 - ndc.y;
        }
        let tangent = lo + (hi - lo) * ndc;
        let centre = to_world.transform_point3((tangent * z).extend(-z));
        let radius = r.range(0.006, 0.02);
        memory.hands[hand] = Some(HandShape {
            segments: std::array::from_fn(|_| {
                [
                    (centre + r.unit() * r.range(0.0, 0.05)).extend(radius),
                    (centre + r.unit() * r.range(0.0, 0.05)).extend(radius),
                ]
            }),
        });
        memory.hand_sources[hand] = if r.chance(0.85) {
            crate::CaptureHand::Exact
        } else {
            crate::CaptureHand::Fallback
        };
    }
    scene.input.memory = Some(memory);
    scene.own = true;
}

fn texture(
    device: &Device,
    size: UVec2,
    layers: u32,
    format: TextureFormat,
    usage: TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&TextureDescriptor {
        label: None,
        size: Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: layers,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

fn pixels(bytes: Vec<u8>) -> Vec<[u8; 4]> {
    bytes
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect()
}

/// What a scene rendered: the mask without and with the scissor, the main pass without the rectangles
/// (today's uniforms), with them, and with the stream shader before stage S3
struct RectRun {
    rects: [EyeRect; 2],
    mask_full: Vec<[u8; 4]>,
    mask_scissored: Vec<[u8; 4]>,
    today: Vec<[u8; 4]>,
    rect: Vec<[u8; 4]>,
    before: Vec<[u8; 4]>,
    trail: bool,
    zone: bool,
}

/// The pipelines of the scenes: [no sRGB correction, with it], each with the shader before stage S3
struct RectPipelines {
    pipelines: [StreamPipelines; 2],
    before: [wgpu::RenderPipeline; 2],
}

fn rect_pipelines(device: &Device) -> RectPipelines {
    let constants = |srgb: bool| {
        [
            ("ENABLE_SRGB_CORRECTION", f64::from(u8::from(srgb))),
            ("ENCODING_GAMMA", 0.0),
            ("ENABLE_FFE", 0.0),
        ]
    };
    let pipelines = [false, true].map(|srgb| {
        StreamPipelines::new(device, TextureFormat::Rgba8Unorm, &constants(srgb), false)
    });
    let before = [0, 1].map(|i| {
        stream::create_main_pipeline_with_source(
            device,
            [
                &pipelines[i].view_layout,
                &pipelines[i].depth_layout,
                &pipelines[i].problem_layout,
            ],
            TextureFormat::Rgba8Unorm,
            &constants(i == 1),
            shader_without_s3(stream::stream_shader_source()),
        )
    });
    RectPipelines { pipelines, before }
}

fn run_scene(
    device: &Device,
    queue: &Queue,
    all: &RectPipelines,
    scene: &RectScene,
    border: bool,
) -> RectRun {
    let pipelines = &all.pipelines[usize::from(scene.srgb)];
    let view_idx = scene.view_idx;
    let input = &scene.input;
    let hand_rects = input.color_key.map_or([None; 2], |key| {
        let reach = input.memory.map_or([0.0; 2], |memory| memory.reach);
        [0, 1].map(|i| {
            depth_mask::hands_view_rect_reach(&scene.views[i], input.eye_positions[i], &key, reach)
        })
    });

    // Today's uniforms, and with the rectangles (and the lower layer's crop)
    let today_uniforms = depth_mask::depth_uniforms(
        &scene.config,
        &scene.views,
        input,
        scene.playzone_views.as_ref(),
        false,
    );
    let rect_cloud = scene.cloud.as_ref().map(|cloud| RectCloud {
        params: cloud.params,
        eyes: [Some(RectField {
            camera: cloud.camera,
            open: cloud.open,
            same_region: true,
        }); 2],
        depth_size: UVec2::splat(RECT_DEPTH),
    });
    let rects = gpu_rect::active_rects(&RectInput {
        config: &scene.config,
        input,
        stream_views: &scene.views,
        playzone: scene.playzone_views.is_some(),
        cloud: rect_cloud,
        hand_rects,
        mask_size: RECT_MASK,
        focus_size: RECT_MASK,
        pad: scene.pad,
        hold: None,
    });
    let params = gpu_rect::GpuRectParams {
        border,
        pad: scene.pad,
    };
    let mut rect_uniforms = today_uniforms;
    gpu_rect::write_uniforms(
        depth_mask::gpu_rect_uniforms(&mut rect_uniforms),
        &params,
        &rects,
    );
    if scene.crop_lower && input.key_lower_rects.is_none() {
        let crops = [0, 1].map(|i| {
            gpu_rect::lower_crop(
                &rects[i],
                scene.views[i].fov,
                scene.views[i].fov,
                RECT_VIEW,
                false,
            )
            .1
        });
        depth_mask::set_key_lower(&mut rect_uniforms, crops);
    }

    let sampled = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    let stream_texture = texture(
        device,
        UVec2::splat(32),
        1,
        TextureFormat::Rgba8Unorm,
        sampled,
    );
    write_texture_2d(queue, &stream_texture, &scene.stream, 32 * 4, 32, 0);
    let stream_view = stream_texture.create_view(&TextureViewDescriptor::default());
    let sampler = device.create_sampler(&SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let foveation_buffer = device.create_buffer(&BufferDescriptor {
        label: None,
        size: FOVEATION_UNIFORM_SIZE as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let probe_local = stream::create_light_probe_texture(device, LIGHT_PROBE_SIZE)
        .create_view(&TextureViewDescriptor::default());
    let probe_global = stream::create_light_probe_texture(device, 1)
        .create_view(&TextureViewDescriptor::default());
    let masks = [0, 1].map(|_| stream::create_depth_mask_texture(device, RECT_VIEW));
    let mask_views = masks
        .each_ref()
        .map(|mask| mask.create_view(&TextureViewDescriptor::default()));
    let focus = stream::create_focus_area_texture(device, RECT_VIEW);
    let focus_view = focus.create_view(&TextureViewDescriptor::default());
    let view_group = |mask: &wgpu::TextureView| {
        device.create_bind_group(&BindGroupDescriptor {
            label: None,
            layout: &pipelines.view_layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: BindingResource::TextureView(&stream_view),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: BindingResource::Sampler(&sampler),
                },
                BindGroupEntry {
                    binding: 2,
                    resource: foveation_buffer.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 3,
                    resource: BindingResource::TextureView(&probe_local),
                },
                BindGroupEntry {
                    binding: 4,
                    resource: BindingResource::TextureView(&probe_global),
                },
                BindGroupEntry {
                    binding: 5,
                    resource: BindingResource::TextureView(mask),
                },
                BindGroupEntry {
                    binding: 6,
                    resource: BindingResource::TextureView(&focus_view),
                },
            ],
        })
    };

    let depth_texture = texture(
        device,
        UVec2::splat(RECT_DEPTH),
        2,
        TextureFormat::R32Float,
        sampled,
    );
    for layer in 0..2 {
        write_texture_2d(
            queue,
            &depth_texture,
            &floats_to_bytes(&scene.depth[layer as usize]),
            RECT_DEPTH * 4,
            RECT_DEPTH,
            layer,
        );
    }
    let depth_view = stream::depth_array_view(&depth_texture);
    let uniform_buffers = [today_uniforms, rect_uniforms].map(|uniforms| {
        let buffer = stream::create_depth_uniform_buffer(device);
        queue.write_buffer(&buffer, 0, &floats_to_bytes(&uniforms));
        buffer
    });
    let depth_groups = uniform_buffers
        .each_ref()
        .map(|buffer| stream::create_depth_bind_group(device, pipelines, buffer, &depth_view));

    let playzone_texture = texture(
        device,
        UVec2::splat(RECT_DEPTH),
        1,
        TextureFormat::Rgba32Float,
        sampled,
    );
    queue.write_texture(
        playzone_texture.as_image_copy(),
        &floats_to_bytes(&scene.playzone),
        TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(RECT_DEPTH * 16),
            rows_per_image: Some(RECT_DEPTH),
        },
        Extent3d {
            width: RECT_DEPTH,
            height: RECT_DEPTH,
            depth_or_array_layers: 1,
        },
    );
    let playzone_view = playzone_texture.create_view(&TextureViewDescriptor::default());
    let playzone_group = stream::create_playzone_bind_group(
        device,
        pipelines,
        &playzone_view,
        &focus_view,
        &sampler,
    );
    let cloud_group = match &scene.cloud {
        Some(cloud) => {
            let field = texture(
                device,
                UVec2::splat(RECT_CELLS),
                1,
                TextureFormat::Rgba8Unorm,
                sampled,
            );
            write_texture_2d(queue, &field, &cloud.cells, RECT_CELLS * 4, RECT_CELLS, 0);
            let eye = CloudEyeUniform {
                field: cloud.camera,
                prev: cloud.camera,
                dt_s: 0.067,
                decay: 0.7,
                reset: false,
            };
            let buffer = crate::motion_cloud_gpu::create_cloud_uniform_buffer(device);
            let data = cloud_uniforms(
                &[Some(eye); 2],
                &cloud.params,
                cloud.reach_s,
                [cloud.open; 2],
            );
            queue.write_buffer(&buffer, 0, &floats_to_bytes(&data));
            create_cloud_read_bind_group(
                device,
                &pipelines.cloud_read_layout,
                &field.create_view(&TextureViewDescriptor::default()),
                &buffer,
            )
        }
        None => stream::create_cloud_dummy_read_bind_group(device, queue, pipelines),
    };
    let problem_view = crate::problem_view::ProblemViewGpu::new(device, &pipelines.problem_layout);

    let mut encoder = device.create_command_encoder(&Default::default());
    if input.color_key.is_some() {
        let group = stream::create_focus_area_bind_group(device, pipelines, &uniform_buffers[0]);
        stream::encode_focus_area(
            &mut encoder,
            pipelines,
            &focus_view,
            &group,
            view_idx as u32,
            RECT_MASK,
            hand_rects[view_idx],
        );
    }
    for (i, scissor) in [None, Some(rects[view_idx].scissor)]
        .into_iter()
        .enumerate()
    {
        stream::encode_depth_mask_scissored(
            &mut encoder,
            pipelines,
            &mask_views[i],
            &depth_groups[i],
            &playzone_group,
            &cloud_group,
            view_idx as u32,
            scissor,
        );
    }
    let targets = [0, 1, 2].map(|_| {
        texture(
            device,
            RECT_VIEW,
            1,
            TextureFormat::Rgba8Unorm,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
        )
    });
    let view_groups = [view_group(&mask_views[0]), view_group(&mask_views[1])];
    for (target, pipeline, view_group, depth_group) in [
        (
            &targets[0],
            &pipelines.main,
            &view_groups[0],
            &depth_groups[0],
        ),
        (
            &targets[1],
            &pipelines.main,
            &view_groups[1],
            &depth_groups[1],
        ),
        (
            &targets[2],
            &all.before[usize::from(scene.srgb)],
            &view_groups[0],
            &depth_groups[0],
        ),
    ] {
        let target_view = target.create_view(&TextureViewDescriptor::default());
        let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: LoadOp::Clear(Color::TRANSPARENT),
                    store: StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        let transform = Mat4::from_scale(Vec3::new(2.0, 2.0, 1.0));
        pass.set_pipeline(pipeline);
        pass.set_immediates(0, &floats_to_bytes(&transform.to_cols_array()));
        pass.set_immediates(64, &(view_idx as u32).to_le_bytes());
        pass.set_immediates(68, &3_u32.to_le_bytes());
        pass.set_immediates(72, &1_f32.to_le_bytes());
        pass.set_immediates(stream::EDGE_FILL_CONST_OFFSET, &0_f32.to_le_bytes());
        pass.set_bind_group(0, view_group, &[]);
        pass.set_bind_group(1, depth_group, &[]);
        pass.set_bind_group(2, &problem_view.bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
    queue.submit([encoder.finish()]);

    let read = |texture: &wgpu::Texture, size: UVec2| {
        pixels(read_rgba(
            device,
            queue,
            device.create_command_encoder(&Default::default()),
            texture,
            size,
        ))
    };
    let shown = scene.cloud.as_ref().is_some_and(|_| {
        depth_mask::cloud_shown(
            &scene.config,
            input.cloud.as_ref(),
            depth_mask::cloud_runs(&scene.config, input),
        )
    });
    RectRun {
        rects,
        mask_full: read(&masks[0], RECT_MASK),
        mask_scissored: read(&masks[1], RECT_MASK),
        today: read(&targets[0], RECT_VIEW),
        rect: read(&targets[1], RECT_VIEW),
        before: read(&targets[2], RECT_VIEW),
        zone: shown,
        trail: shown
            && scene
                .cloud
                .as_ref()
                .is_some_and(|cloud| cloud.params.trail > 0.0),
    }
}

/// Two scenes of five are bare: only the boxes and the depth (no hands, arms, gaps, levers, green zone,
/// cloud, no pad), so that the bound of the depth alone is tight and a missing term shows
fn bare(mut scene: RectScene) -> RectScene {
    if let Some(key) = &mut scene.input.color_key {
        key.hands = [None; 2];
        key.bridges = [None; 2];
    }
    scene.input.arms = [None; 2];
    scene.input.arm_gaps = [None; 2];
    scene.input.lever_windows = [None; MAX_LEVER_WINDOWS];
    scene.input.green_windows = [None; MAX_GREEN_WINDOWS];
    scene.input.cloud = None;
    scene.cloud = None;
    scene.pad = 0.0;
    scene.own = false;
    scene
}

// The safety net of `debug.alvr.gpu_rect` (see the top of this file). Prints how many scenes ran, how many
// fell back to the full view, and the share of the view the rectangle covered
#[test]
fn gpu_rect_bounds_every_scene() {
    let (device, queue) = gpu_or_skip!();
    let pipelines = rect_pipelines(&device);
    let scenes = 400;
    let (mut full, mut empty, mut share_sum, mut share_max, mut nonzero_mask) =
        (0, 0, 0.0_f64, 0.0_f32, 0);
    let mut reasons = std::collections::BTreeMap::new();
    // `cloud_own`: the scenes with it on, and the mask texels it changed in them
    let (mut own_scenes, mut own_changed) = (0, 0);
    for seed in 0..scenes {
        let scene = random_scene(seed);
        let scene = if seed % 5 < 2 { bare(scene) } else { scene };
        let run = run_scene(&device, &queue, &pipelines, &scene, false);
        if scene.own {
            own_scenes += 1;
            let mut off = random_scene(seed);
            off.input.cloud.as_mut().unwrap().params.own = 0.0;
            let off = run_scene(&device, &queue, &pipelines, &off, false);
            own_changed += run
                .mask_full
                .iter()
                .zip(&off.mask_full)
                .filter(|(a, b)| a != b)
                .count();
        }
        let rect = run.rects[scene.view_idx];
        if let Some(reason) = rect.full {
            full += 1;
            *reasons.entry(reason.name()).or_insert(0) += 1;
        } else {
            let share = rect.share(RECT_MASK);
            share_sum += f64::from(share);
            share_max = share_max.max(share);
            if share == 0.0 {
                empty += 1;
            }
        }
        let (min, max) = rect.scissor;
        for y in 0..RECT_MASK.y {
            for x in 0..RECT_MASK.x {
                let i = (y * RECT_MASK.x + x) as usize;
                let texel = run.mask_full[i];
                nonzero_mask += usize::from(texel[0] > 0);
                let inside = x >= min.x && x < max.x && y >= min.y && y < max.y;
                if inside {
                    assert_eq!(run.mask_scissored[i], texel, "scene {seed}: mask {x} {y}");
                } else {
                    assert_eq!(run.mask_scissored[i], [0; 4], "scene {seed}: clear {x} {y}");
                    // r and a always; g with the cloud's zone, b with its trail (else the proximity and the
                    // depth flag, read only by the Show depth view)
                    assert!(
                        texel[0] == 0
                            && texel[3] == 0
                            && (!run.zone || texel[1] == 0)
                            && (!run.trail || texel[2] == 0),
                        "scene {seed}: mask texel {x} {y} = {texel:?} outside {:?} ({:?})",
                        rect.scissor,
                        rect.full
                    );
                }
            }
        }
        assert!(
            run.today == run.before,
            "scene {seed}: the shader before stage S3 differs"
        );
        if run.today != run.rect {
            let i = run
                .today
                .iter()
                .zip(&run.rect)
                .position(|(a, b)| a != b)
                .unwrap();
            eprintln!(
                "key {} cloud {} see {} dim {} view {:?} crop {} key_crop {:?} mask0 {:?} srgb {}",
                scene.input.color_key.is_some(),
                scene.cloud.is_some(),
                scene.input.game_see,
                scene.input.game_dim,
                scene.config.debug_view,
                scene.crop_lower,
                scene.input.key_lower_rects,
                run.mask_full[0],
                scene.srgb
            );
            panic!(
                "scene {seed}: pixel {} {}: today {:?}, with the rectangle {:?} ({:?})",
                i as u32 % RECT_VIEW.x,
                i as u32 / RECT_VIEW.x,
                run.today[i],
                run.rect[i],
                rect
            );
        }
    }
    let bounded = scenes as usize - full;
    eprintln!(
        "gpu_rect: {scenes} scenes, {full} full view ({reasons:?}), {empty} empty, bounded share of the view \
         mean {:.1} % max {:.1} %, {nonzero_mask} mask texels with coverage; cloud_own in \
         {own_scenes} scenes, {own_changed} mask texels changed by it",
        100.0 * share_sum / bounded.max(1) as f64,
        100.0 * share_max
    );
    assert!(bounded > scenes as usize / 2);
    assert!(own_scenes >= 20 && own_changed > 0);
}

// `gpu_rect 2` with the Tint mask view draws the rectangles' borders, and nothing else changes
#[test]
fn gpu_rect_border_in_the_tint_view() {
    let (device, queue) = gpu_or_skip!();
    let pipelines = rect_pipelines(&device);
    let mut drawn = 0;
    for seed in 0..40 {
        let mut scene = random_scene(1000 + seed);
        scene.config.debug_view = DepthPassthroughDebugView::TintMask;
        let run = run_scene(&device, &queue, &pipelines, &scene, true);
        if run.rects[scene.view_idx].full.is_some() {
            continue;
        }
        let border = run
            .rect
            .iter()
            .zip(&run.today)
            .filter(|(rect, today)| rect != today)
            .inspect(|(rect, _)| {
                assert!(
                    [[255_u8, 217, 0, 255], [0, 230, 230, 255]]
                        .iter()
                        .any(|colour| {
                            colour.iter().zip(*rect).all(|(a, b)| a.abs_diff(*b) <= 1)
                        }),
                    "{rect:?}"
                )
            })
            .count();
        drawn += border;
    }
    assert!(drawn > 0);
}

// `fast_srgb`: the main pass with the cubic writes within one 8-bit step of today's into an sRGB target
// (the swapchain's encoding), over every 8-bit input
#[test]
fn fast_srgb_within_a_step_on_the_gpu() {
    let (device, queue) = gpu_or_skip!();
    let format = TextureFormat::Rgba8UnormSrgb;
    let constants = [
        ("ENABLE_SRGB_CORRECTION", 1.0),
        ("ENCODING_GAMMA", 0.0),
        ("ENABLE_FFE", 0.0),
    ];
    let pipelines = StreamPipelines::new(&device, format, &constants, false);
    let fast_constants = [
        ("ENABLE_SRGB_CORRECTION", 1.0),
        ("ENCODING_GAMMA", 0.0),
        ("ENABLE_FFE", 0.0),
        ("FAST_SRGB", 1.0),
    ];
    let fast = stream::create_main_pipeline(
        &device,
        [
            &pipelines.view_layout,
            &pipelines.depth_layout,
            &pipelines.problem_layout,
        ],
        format,
        &fast_constants,
    );
    // 256 x 1 texels: r = i, g = 255 - i, b = i / 2; read at the texel centres
    let sampled = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;
    let stream_texture = texture(
        &device,
        UVec2::new(256, 1),
        1,
        TextureFormat::Rgba8Unorm,
        sampled,
    );
    let texels: Vec<u8> = (0..256)
        .flat_map(|i| [i as u8, 255 - i as u8, (i / 2) as u8, 255])
        .collect();
    write_texture_2d(&queue, &stream_texture, &texels, 256 * 4, 1, 0);
    let stream_view = stream_texture.create_view(&TextureViewDescriptor::default());
    let sampler = device.create_sampler(&SamplerDescriptor::default());
    let foveation_buffer = device.create_buffer(&BufferDescriptor {
        label: None,
        size: FOVEATION_UNIFORM_SIZE as u64,
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let probe = stream::create_light_probe_texture(&device, 1)
        .create_view(&TextureViewDescriptor::default());
    let mask = stream::create_depth_mask_texture(&device, UVec2::splat(5))
        .create_view(&TextureViewDescriptor::default());
    let view_group = device.create_bind_group(&BindGroupDescriptor {
        label: None,
        layout: &pipelines.view_layout,
        entries: &[
            BindGroupEntry {
                binding: 0,
                resource: BindingResource::TextureView(&stream_view),
            },
            BindGroupEntry {
                binding: 1,
                resource: BindingResource::Sampler(&sampler),
            },
            BindGroupEntry {
                binding: 2,
                resource: foveation_buffer.as_entire_binding(),
            },
            BindGroupEntry {
                binding: 3,
                resource: BindingResource::TextureView(&probe),
            },
            BindGroupEntry {
                binding: 4,
                resource: BindingResource::TextureView(&probe),
            },
            BindGroupEntry {
                binding: 5,
                resource: BindingResource::TextureView(&mask),
            },
            BindGroupEntry {
                binding: 6,
                resource: BindingResource::TextureView(&mask),
            },
        ],
    });
    let dummy_depth = stream::create_dummy_depth_texture(&device);
    let uniforms = stream::create_depth_uniform_buffer(&device);
    let depth_group = stream::create_depth_bind_group(
        &device,
        &pipelines,
        &uniforms,
        &stream::depth_array_view(&dummy_depth),
    );
    let problem_view = crate::problem_view::ProblemViewGpu::new(&device, &pipelines.problem_layout);
    let mut outputs = vec![];
    for pipeline in [&pipelines.main, &fast] {
        let target = texture(
            &device,
            UVec2::new(256, 1),
            1,
            format,
            TextureUsages::RENDER_ATTACHMENT | TextureUsages::COPY_SRC,
        );
        let target_view = target.create_view(&TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: LoadOp::Clear(Color::TRANSPARENT),
                        store: StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let transform = Mat4::from_scale(Vec3::new(2.0, 2.0, 1.0));
            pass.set_pipeline(pipeline);
            pass.set_immediates(0, &floats_to_bytes(&transform.to_cols_array()));
            pass.set_immediates(64, &0_u32.to_le_bytes());
            // Blend mode, alpha 1: the corrected colour as it is
            pass.set_immediates(68, &0_u32.to_le_bytes());
            pass.set_immediates(72, &1_f32.to_le_bytes());
            pass.set_immediates(stream::EDGE_FILL_CONST_OFFSET, &0_f32.to_le_bytes());
            pass.set_bind_group(0, &view_group, &[]);
            pass.set_bind_group(1, &depth_group, &[]);
            pass.set_bind_group(2, &problem_view.bind_group, &[]);
            pass.draw(0..4, 0..1);
        }
        outputs.push(pixels(read_rgba(
            &device,
            &queue,
            encoder,
            &target,
            UVec2::new(256, 1),
        )));
    }
    let largest = outputs[0]
        .iter()
        .zip(&outputs[1])
        .flat_map(|(a, b)| (0..3).map(move |c| a[c].abs_diff(b[c])))
        .max()
        .unwrap();
    eprintln!("fast_srgb on the GPU: largest difference {largest} step(s) of 255");
    assert!(largest <= 1, "{largest}");
    // Decoded by the shader, encoded again by the target: about the input
    for (i, pixel) in outputs[0].iter().enumerate() {
        assert!(pixel[0].abs_diff(i as u8) <= 1, "{i}: {pixel:?}");
    }
}

// The worst case of the depth's bound, built on purpose: the depth camera 2 cm left of the eye, a box ahead
// on the right, an occluder 12 cm in front of the camera filling the depth image left of the box. A view
// ray onto the occluder is looked up 2 cm / 12 cm (about 9 degrees) further right in the depth image, so
// texels well left of the box's projection find the box in their 3 x 3 neighbourhood. The rectangle must
// hold them (the angle term of the bound), and the scene must really put coverage there
#[test]
fn gpu_rect_holds_a_near_occluder() {
    let (device, queue) = gpu_or_skip!();
    let pipelines = rect_pipelines(&device);
    let mut exercised = 0;
    for (offset, seed) in [(0.02, 0), (0.022, 1), (0.016, 2), (0.019, 3)] {
        let mut scene = bare(random_scene(5000 + seed));
        let eye = Vec3::new(0.0, 1.2, 0.0);
        let fov = Fov {
            left: -0.8,
            right: 0.8,
            up: 0.8,
            down: -0.8,
        };
        let view = ViewParams {
            pose: Pose {
                orientation: Quat::IDENTITY,
                position: eye,
            },
            fov,
        };
        scene.views = [view; 2];
        scene.view_idx = 0;
        scene.input.eye_positions = [eye; 2];
        scene.input.color_key = None;
        scene.input.memory = None;
        scene.input.game_see = 0.0;
        scene.input.key_lower_rects = None;
        scene.playzone_views = None;
        scene.config.debug_view = DepthPassthroughDebugView::Off;
        scene.config.flip_depth_y = false;
        scene.config.edge_feathering = 0.005;
        let camera = ViewParams {
            pose: Pose {
                orientation: Quat::IDENTITY,
                position: eye + Vec3::new(-offset, 0.0, 0.0),
            },
            fov: depth_fov(),
        };
        let half = Vec3::new(0.1, 0.1, 0.1);
        let center = Vec3::new(0.12, 0.0, -0.5);
        scene.input.region_override = Some(PlacedArea {
            area_to_world: Mat4::from_translation(eye),
            area: PlayArea::single(AreaBox::new(center, half)),
        });
        let size = RECT_DEPTH as usize;
        let (lo, hi) = (
            Vec2::new(camera.fov.left.tan(), camera.fov.down.tan()),
            Vec2::new(camera.fov.right.tan(), camera.fov.up.tan()),
        );
        // In the camera's frame (same orientation): the box's near face at z = 0.4, x from 0.02 + offset
        let box_min = (center - half - camera.pose.position + eye).truncate();
        let box_max = (center + half - camera.pose.position + eye).truncate();
        let depth: Vec<f32> = (0..size * size)
            .map(|i| {
                let uv = Vec2::new(
                    ((i % size) as f32 + 0.5) / size as f32,
                    ((i / size) as f32 + 0.5) / size as f32,
                );
                let tangent = lo + (hi - lo) * uv;
                let at_face = tangent * 0.4;
                let z = if at_face.cmpge(box_min).all() && at_face.cmple(box_max).all() {
                    0.4
                } else if at_face.x < box_min.x && at_face.y.abs() < 0.2 {
                    0.12
                } else {
                    3.0
                };
                depth_mask::encode_depth(z, NEAR, f32::INFINITY)
            })
            .collect();
        scene.depth = [depth.clone(), depth];
        if let Some(frame) = &mut scene.input.frame {
            frame.depth_views = [camera; 2];
        } else {
            scene.input.frame = Some(DepthFrame {
                swapchain_index: 0,
                near: NEAR,
                far: f32::INFINITY,
                depth_views: [camera; 2],
            });
        }
        let run = run_scene(&device, &queue, &pipelines, &scene, false);
        // The box's left edge seen from the camera, as a mask column
        let edge_tangent = (center.x - half.x + offset) / (-(center.z) - half.z);
        let edge_u = (edge_tangent - fov.left.tan()) / (fov.right.tan() - fov.left.tan());
        let leftmost = (0..RECT_MASK.y)
            .flat_map(|y| (0..RECT_MASK.x).map(move |x| (x, y)))
            .filter(|(x, y)| run.mask_full[(y * RECT_MASK.x + x) as usize][0] > 0)
            .map(|(x, _)| x)
            .min()
            .unwrap();
        let leftmost_u = (leftmost as f32 + 0.5) / RECT_MASK.x as f32;
        eprintln!(
            "near occluder, camera {offset} m off: coverage from u {leftmost_u:.3}, the box's edge from the \
             camera at u {edge_u:.3}, rectangle {:?}",
            run.rects[0].scissor
        );
        exercised += usize::from(leftmost_u < edge_u - 0.04);
        let (min, max) = run.rects[0].scissor;
        for y in 0..RECT_MASK.y {
            for x in 0..RECT_MASK.x {
                let texel = run.mask_full[(y * RECT_MASK.x + x) as usize];
                let inside = x >= min.x && x < max.x && y >= min.y && y < max.y;
                assert!(
                    inside || texel[0] == 0,
                    "{offset}: texel {x} {y} {texel:?} outside {min} {max}"
                );
            }
        }
        assert!(run.today == run.rect);
    }
    // The scene puts coverage well off the box (else it tests nothing)
    assert!(exercised > 0);
}
