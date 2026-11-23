//! One pass of the color key learner as JSON, for `debug.alvr.key_dump` of the client. Numbers that are
//! not finite are written as null.

use crate::{HAND_SEGMENTS, HandShape};
use alvr_common::{Pose, glam::Vec3};

/// A hand joint at the time of the camera image, in stage space
#[derive(Clone, Copy, Default)]
pub struct DumpJoint {
    pub position: Vec3,
    pub radius: f32,
    /// POSITION_TRACKED
    pub tracked: bool,
    pub velocity: Vec3,
    /// LINEAR_VALID
    pub velocity_valid: bool,
}

#[derive(Clone, Copy)]
pub struct LocatedHand {
    pub joints: [DumpJoint; 26],
    /// The capsules as the learner uses them
    pub shape: HandShape,
}

/// A hand in one pass of the learner
pub struct DumpHand {
    /// None: not located
    pub located: Option<LocatedHand>,
    /// Its colors are learned
    pub learned: bool,
    pub verified: bool,
    /// The segments that are verified
    pub eligible: [bool; HAND_SEGMENTS],
    /// Of this pass, None: none at all
    pub shares: Option<[Option<f32>; HAND_SEGMENTS]>,
    /// Smoothed, after this pass
    pub weights: [f32; HAND_SEGMENTS],
    pub reliability: f32,
}

pub struct KeyDump {
    pub n: i64,
    pub index: u32,
    /// Passes of the burst
    pub count: u32,
    /// `image_ns` of index 0, tells the bursts of the same `n` apart
    pub burst_ns: i64,
    /// CLOCK_MONOTONIC
    pub image_ns: i64,
    /// Age of the image when the pass started
    pub image_age_ms: f64,
    /// Since the previous pass
    pub pass_interval_ms: f32,
    /// CLOCK_MONOTONIC at the start of the pass
    pub monotonic_ms: f64,
    pub width: u32,
    pub height: u32,
    /// fx, fy, cx, cy in pixels
    pub intrinsics: [f32; 4],
    /// Stage space at the time of the image, the camera looks at +z with +y down
    pub camera: Pose,
    pub head: Pose,
    /// Name and value
    pub params: Vec<(&'static str, f32)>,
    /// Name and value
    pub counts: Vec<(&'static str, f32)>,
    /// File names of the tables of this burst, None: not written
    pub key_table: Option<String>,
    /// The key table as the learner uses it, see `ColorModel::key_table`
    pub key_alpha_table: Option<String>,
    pub confirm_table: Option<String>,
    /// Left, right
    pub hands: [DumpHand; 2],
}

pub(crate) fn number(value: f32) -> String {
    if value.is_finite() {
        value.to_string()
    } else {
        "null".into()
    }
}

pub(crate) fn number64(value: f64) -> String {
    if value.is_finite() {
        value.to_string()
    } else {
        "null".into()
    }
}

pub(crate) fn string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub(crate) fn array(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(","))
}

pub(crate) fn numbers(values: &[f32]) -> String {
    array(values.iter().map(|value| number(*value)))
}

pub(crate) fn object<'a>(fields: impl IntoIterator<Item = (&'a str, String)>) -> String {
    let fields = fields
        .into_iter()
        .map(|(name, value)| format!("{}:{value}", string(name)))
        .collect::<Vec<_>>();

    format!("{{{}}}", fields.join(","))
}

pub(crate) fn pose(pose: &Pose) -> String {
    object([
        ("position", numbers(&pose.position.to_array())),
        ("orientation", numbers(&pose.orientation.to_array())),
    ])
}

impl DumpHand {
    pub fn to_json(&self) -> String {
        let shares = self.shares.unwrap_or([None; HAND_SEGMENTS]);
        let mut fields = vec![
            ("located", self.located.is_some().to_string()),
            ("learned", self.learned.to_string()),
            ("verified", self.verified.to_string()),
            ("reliability", number(self.reliability)),
            ("eligible", array(self.eligible.map(|e| e.to_string()))),
            (
                "shares",
                array(shares.map(|share| share.map_or("null".into(), number))),
            ),
            ("weights", numbers(&self.weights)),
        ];
        if let Some(hand) = &self.located {
            let joints = hand.joints.map(|joint| {
                object([
                    ("position", numbers(&joint.position.to_array())),
                    ("radius", number(joint.radius)),
                    ("tracked", joint.tracked.to_string()),
                    ("velocity", numbers(&joint.velocity.to_array())),
                    ("velocity_valid", joint.velocity_valid.to_string()),
                ])
            });
            fields.push(("joints", array(joints)));
            // Start xyz r, end xyz r
            let capsules = hand
                .shape
                .segments
                .map(|[start, end]| numbers(&[start.to_array(), end.to_array()].concat()));
            fields.push(("capsules", array(capsules)));
        }

        object(fields)
    }
}

impl KeyDump {
    pub fn to_json(&self) -> String {
        let [fx, fy, cx, cy] = self.intrinsics;
        let file = |name: &Option<String>| name.as_deref().map_or("null".into(), string);
        let named = |values: &[(&'static str, f32)]| {
            object(values.iter().map(|(name, value)| (*name, number(*value))))
        };

        object([
            ("n", self.n.to_string()),
            ("index", self.index.to_string()),
            ("count", self.count.to_string()),
            ("burst_ns", self.burst_ns.to_string()),
            ("image_ns", self.image_ns.to_string()),
            ("image_age_ms", number64(self.image_age_ms)),
            ("pass_interval_ms", number(self.pass_interval_ms)),
            ("monotonic_ms", number64(self.monotonic_ms)),
            (
                "camera",
                object([
                    ("width", self.width.to_string()),
                    ("height", self.height.to_string()),
                    ("fx", number(fx)),
                    ("fy", number(fy)),
                    ("cx", number(cx)),
                    ("cy", number(cy)),
                    ("pose", pose(&self.camera)),
                    ("head", pose(&self.head)),
                ]),
            ),
            ("params", named(&self.params)),
            ("counts", named(&self.counts)),
            (
                "tables",
                object([
                    ("key", file(&self.key_table)),
                    ("key_alpha", file(&self.key_alpha_table)),
                    ("confirm", file(&self.confirm_table)),
                ]),
            ),
            ("left", self.hands[0].to_json()),
            ("right", self.hands[1].to_json()),
        ])
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alvr_common::glam::{Quat, Vec4};

    // Minimal JSON parser: Some(rest of the text) after one valid value
    fn value(text: &str) -> Option<&str> {
        let text = text.trim_start();
        let rest = match text.chars().next()? {
            '{' => {
                let mut rest = text[1..].trim_start();
                if let Some(rest) = rest.strip_prefix('}') {
                    return Some(rest);
                }
                loop {
                    rest = value(rest).filter(|_| rest.starts_with('"'))?;
                    rest = value(rest.trim_start().strip_prefix(':')?)?.trim_start();
                    match rest.chars().next()? {
                        ',' => rest = rest[1..].trim_start(),
                        '}' => return Some(&rest[1..]),
                        _ => return None,
                    }
                }
            }
            '[' => {
                let mut rest = text[1..].trim_start();
                if let Some(rest) = rest.strip_prefix(']') {
                    return Some(rest);
                }
                loop {
                    rest = value(rest)?.trim_start();
                    match rest.chars().next()? {
                        ',' => rest = &rest[1..],
                        ']' => return Some(&rest[1..]),
                        _ => return None,
                    }
                }
            }
            '"' => {
                let mut escaped = false;
                let end = text[1..].char_indices().find(|(_, c)| {
                    let is_end = !escaped && *c == '"';
                    escaped = !escaped && *c == '\\';
                    is_end
                })?;
                &text[end.0 + 2..]
            }
            _ => {
                let end = text
                    .find(|c: char| matches!(c, ',' | ']' | '}') || c.is_whitespace())
                    .unwrap_or(text.len());
                let token = &text[..end];
                let is_number = token.parse::<f64>().is_ok()
                    && !token.starts_with('+')
                    && token
                        .chars()
                        .all(|c| c.is_ascii_digit() || "-.eE+".contains(c));
                if !(is_number || ["null", "true", "false"].contains(&token)) {
                    return None;
                }
                &text[end..]
            }
        };

        Some(rest)
    }

    pub(crate) fn is_valid_json(text: &str) -> bool {
        value(text).is_some_and(|rest| rest.trim().is_empty())
    }

    #[test]
    fn parser_rejects_invalid_json() {
        assert!(is_valid_json(
            r#"{"a":[1,-2.5,null,true],"b":{"c":"x\"y"}}"#
        ));
        assert!(!is_valid_json("[NaN]"));
        assert!(!is_valid_json("[inf]"));
        assert!(!is_valid_json(r#"{"a" 1}"#));
        assert!(!is_valid_json("[1,]"));
    }

    fn hand(located: bool) -> DumpHand {
        let mut shares = [Some(0.5); HAND_SEGMENTS];
        shares[3] = None;
        shares[4] = Some(f32::NAN);
        let mut joints = [DumpJoint::default(); 26];
        joints[1].position = Vec3::new(0.1, f32::INFINITY, -0.2);
        joints[2].velocity = Vec3::splat(f32::NAN);

        DumpHand {
            located: located.then(|| LocatedHand {
                joints,
                shape: HandShape::from_joints(&[Vec4::new(0.0, 1.0, 0.0, 0.01); 26]),
            }),
            learned: located,
            verified: located,
            eligible: [located; HAND_SEGMENTS],
            shares: located.then_some(shares),
            weights: [0.25; HAND_SEGMENTS],
            reliability: f32::NAN,
        }
    }

    #[test]
    fn hand_json_is_valid_with_null_for_nan() {
        for located in [false, true] {
            let json = hand(located).to_json();
            assert!(is_valid_json(&json), "{json}");
            assert!(!json.contains("NaN") && !json.contains("inf"), "{json}");
            assert!(json.contains(r#""reliability":null"#), "{json}");
            assert_eq!(json.contains(r#""joints":"#), located);
            assert_eq!(json.contains(r#""capsules":"#), located);
        }
        let json = hand(true).to_json();
        assert!(
            json.contains(r#""shares":[0.5,0.5,0.5,null,null,0.5"#),
            "{json}"
        );
        assert!(json.contains(r#""position":[0.1,null,-0.2]"#), "{json}");
        assert!(json.contains(r#""velocity":[null,null,null]"#), "{json}");
        // 28 capsules of 8 numbers
        let capsules = json.split(r#""capsules":"#).nth(1).unwrap();
        assert_eq!(capsules.matches('[').count(), 1 + HAND_SEGMENTS);
        assert!(capsules.starts_with("[[0,1,0,0.01,0,1,0,0.01],"), "{json}");
    }

    #[test]
    fn pass_json_is_valid() {
        let dump = KeyDump {
            n: 3,
            index: 1,
            count: 2,
            burst_ns: 1_234_500_000_000,
            image_ns: 1_234_567_890_123,
            image_age_ms: 21.5,
            pass_interval_ms: f32::INFINITY,
            monotonic_ms: 1_234_589.4,
            width: 320,
            height: 240,
            intrinsics: [200.0, 200.0, 160.0, 120.0],
            camera: Pose {
                orientation: Quat::IDENTITY,
                position: Vec3::new(0.0, 1.6, 0.0),
            },
            head: Pose::IDENTITY,
            params: vec![("key_tight", 0.3), ("key_steady", 1.0)],
            counts: vec![("skin", 120.0), ("skin_kept", f32::NAN)],
            key_table: Some("alvr_dump_3_00_key.lut".into()),
            key_alpha_table: Some("alvr_dump_3_00_key_alpha.lut".into()),
            confirm_table: None,
            hands: [hand(false), hand(true)],
        };
        let json = dump.to_json();
        assert!(is_valid_json(&json), "{json}");
        assert!(!json.contains("NaN") && !json.contains("inf"), "{json}");
        assert!(json.contains(r#""pass_interval_ms":null"#), "{json}");
        assert!(json.contains(r#""image_ns":1234567890123"#), "{json}");
        assert!(json.contains(r#""burst_ns":1234500000000"#), "{json}");
        assert!(json.contains(r#""skin_kept":null"#), "{json}");
        assert!(
            json.contains(
                r#""tables":{"key":"alvr_dump_3_00_key.lut","key_alpha":"alvr_dump_3_00_key_alpha.lut","confirm":null}"#
            ),
            "{json}"
        );
    }
}
