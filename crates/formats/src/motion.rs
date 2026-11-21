//! PS2 motion groups (`*.mgrp`): per-joint keyframe channels that animate both the original and the HD
//! skeletons (HD joints are named `SkeletonNode_<PS2 joint>`). See `docs/formats/motion.md`.
//!
//! Each joint has nine channels: rotation X, Y, Z (1/4096 turn), translation X, Y, Z (1/4096 units, scaled
//! by the model), scale X, Y, Z (1/4096). A channel is zero, one, a constant, or a delta-coded stream with
//! one value per frame.

const CHANNELS_PER_JOINT: usize = 9;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MotionError {
    #[error("file ends early ({0})")]
    Truncated(&'static str),
    #[error("bad {0}")]
    Bad(&'static str),
}

type Result<T> = std::result::Result<T, MotionError>;

fn u16_at(d: &[u8], at: usize) -> Result<u16> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(MotionError::Truncated("u16"))
}

fn i16_at(d: &[u8], at: usize) -> Result<i16> {
    u16_at(d, at).map(|v| v as i16)
}

fn u32_at(d: &[u8], at: usize) -> Result<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(MotionError::Truncated("u32"))
}

/// One channel's values, in 1/4096 units.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
    Zero,
    One,
    Constant(i16),
    /// One value per frame.
    Frames(Vec<i16>),
}

impl Channel {
    /// Raw value at a frame (1/4096 units; `One` is 4096).
    pub fn raw(&self, frame: usize) -> i32 {
        match self {
            Channel::Zero => 0,
            Channel::One => 4096,
            Channel::Constant(v) => i32::from(*v),
            Channel::Frames(f) => f.get(frame.min(f.len().saturating_sub(1))).copied().map_or(0, i32::from),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Motion {
    pub frames: u16,
    pub joints: u16,
    /// Playback rate in 1/256 frames per tick (0x1E00 = 30 frames per second at 30 ticks).
    pub rate: u16,
    /// `joints × 9` channels.
    pub channels: Vec<Channel>,
    /// What happens at given frames (sounds, face patterns, …).
    pub events: Vec<MotionEvent>,
}

/// A motion event: it fires once the motion's frame reaches `start` (unless the frame was already at `end` or
/// later). Kinds: 1 sound, 2 the character's callback, 3 face pattern (`id`: kind << 8 | value), 5 effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MotionEvent {
    pub kind: u8,
    pub start: i16,
    pub end: i16,
    pub id: u16,
}

/// A joint's pose at one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JointPose {
    /// Radians.
    pub rotation: [f32; 3],
    /// Raw translation (1/4096 units); multiply by the model's translation scale.
    pub translation: [f32; 3],
    pub scale: [f32; 3],
}

/// Decode one channel's delta stream into per-frame values.
fn decode_stream(data: &[u8], frames: usize) -> Vec<i16> {
    let mut out = Vec::with_capacity(frames);
    let (mut value, mut delta, mut run, mut i) = (0i16, 0i16, 0u16, 0usize);
    for _ in 0..frames {
        if run == 0 {
            match data.get(i).copied() {
                None => {}
                Some(b) if b & 0x80 == 0 => {
                    // 7-bit signed delta.
                    delta = (((b << 1) as i8) >> 1) as i16;
                    i += 1;
                }
                Some(b) if b & 0x40 == 0 => {
                    // Repeat the previous delta for this and the next n frames.
                    run = u16::from(b & 0x3F);
                    i += 1;
                }
                Some(b) => {
                    // 14-bit signed delta over two bytes.
                    let hi = data.get(i + 1).copied().unwrap_or(0) as i8 as i16;
                    let v = (hi << 6) | i16::from(b & 0x3F);
                    delta = (v << 2) >> 2;
                    i += 2;
                }
            }
        } else {
            run -= 1;
        }
        value = value.wrapping_add(delta);
        out.push(value);
    }
    out
}

impl Motion {
    /// Parse a motion header at `at` (offsets inside it are relative to `at`).
    pub fn parse(d: &[u8], at: usize) -> Result<Motion> {
        let frames = u16_at(d, at)?;
        let joints = u16_at(d, at + 2)?;
        let rate = u16_at(d, at + 4)?;
        let event_count = i16_at(d, at + 6)?.max(0) as usize;
        let bits_at = at + u32_at(d, at + 8)? as usize;
        let mut values = at + u32_at(d, at + 12)? as usize;
        if joints == 0 || joints > 1024 || frames > 30_000 {
            return Err(MotionError::Bad("motion header"));
        }
        let n = joints as usize * CHANNELS_PER_JOINT;
        let mut channels = Vec::with_capacity(n);
        for c in 0..n {
            let word = u32_at(d, bits_at + (c / 16) * 4)?;
            let kind = (word >> ((c % 16) * 2)) & 3;
            channels.push(match kind {
                0 => Channel::Zero,
                1 => Channel::One,
                2 => {
                    let v = i16_at(d, values)?;
                    values += 2;
                    Channel::Constant(v)
                }
                _ => {
                    let size = u16_at(d, values)? as usize;
                    if size < 2 {
                        return Err(MotionError::Bad("channel size"));
                    }
                    let data =
                        d.get(values + 2..values + size).ok_or(MotionError::Truncated("channel data"))?;
                    values += size;
                    Channel::Frames(decode_stream(data, frames.max(1) as usize))
                }
            });
        }
        // Events: {kind, extra words, start, end, id} and the extra words.
        let mut events = Vec::with_capacity(event_count.min(256));
        if event_count > 0 {
            let mut e = at + u32_at(d, at + 0x10)? as usize;
            for _ in 0..event_count.min(256) {
                let kind = *d.get(e).ok_or(MotionError::Truncated("event"))?;
                let words = *d.get(e + 1).ok_or(MotionError::Truncated("event"))? as i8;
                events.push(MotionEvent {
                    kind,
                    start: i16_at(d, e + 2)?,
                    end: i16_at(d, e + 4)?,
                    id: u16_at(d, e + 6)?,
                });
                e = e.wrapping_add_signed(isize::from(words) * 4 + 8);
            }
        }
        Ok(Motion { frames, joints, rate, channels, events })
    }

    /// Pose of a joint at an integer frame.
    pub fn joint(&self, joint: usize, frame: usize) -> JointPose {
        let c = |k: usize| self.channels.get(joint * CHANNELS_PER_JOINT + k).map_or(0, |ch| ch.raw(frame));
        let angle = |k: usize| (c(k) & 0xFFF) as f32 / 4096.0 * std::f32::consts::TAU;
        JointPose {
            rotation: [angle(0), angle(1), angle(2)],
            translation: [c(3) as f32, c(4) as f32, c(5) as f32],
            scale: [c(6) as f32 / 4096.0, c(7) as f32 / 4096.0, c(8) as f32 / 4096.0],
        }
    }

    /// Pose at a fractional frame: rotations take the short way round, the rest is linear.
    pub fn joint_at(&self, joint: usize, frame: f32) -> JointPose {
        let last = self.frames.max(1) as usize - 1;
        let f0 = (frame.floor().max(0.0) as usize).min(last);
        let f1 = (f0 + 1).min(last);
        let t = (frame - f0 as f32).clamp(0.0, 1.0);
        let (a, b) = (self.joint(joint, f0), self.joint(joint, f1));
        let lerp = |x: f32, y: f32| x + (y - x) * t;
        let angle = |x: f32, y: f32| {
            let mut d = (y - x) % std::f32::consts::TAU;
            if d > std::f32::consts::PI {
                d -= std::f32::consts::TAU;
            } else if d < -std::f32::consts::PI {
                d += std::f32::consts::TAU;
            }
            x + d * t
        };
        JointPose {
            rotation: [0, 1, 2].map(|k| angle(a.rotation[k], b.rotation[k])),
            translation: [0, 1, 2].map(|k| lerp(a.translation[k], b.translation[k])),
            scale: [0, 1, 2].map(|k| lerp(a.scale[k], b.scale[k])),
        }
    }
}

impl JointPose {
    /// Column-major local matrix: translation · Rz · Ry · Rx · scale (the games' composition order).
    pub fn matrix(&self, translation_scale: f32) -> [f32; 16] {
        let [rx, ry, rz] = self.rotation;
        let (sx, cx) = rx.sin_cos();
        let (sy, cy) = ry.sin_cos();
        let (sz, cz) = rz.sin_cos();
        // R = Rz · Ry · Rx, columns.
        let r = [
            [cy * cz, cy * sz, -sy],
            [sx * sy * cz - cx * sz, sx * sy * sz + cx * cz, sx * cy],
            [cx * sy * cz + sx * sz, cx * sy * sz - sx * cz, cx * cy],
        ];
        let mut m = [0.0f32; 16];
        for (c, col) in r.iter().enumerate() {
            for (k, v) in col.iter().enumerate() {
                m[c * 4 + k] = v * self.scale[c];
            }
        }
        m[12] = self.translation[0] * translation_scale;
        m[13] = self.translation[1] * translation_scale;
        m[14] = self.translation[2] * translation_scale;
        m[15] = 1.0;
        m
    }
}

/// PS2 joint index of an HD joint named `SkeletonNode_<n>`.
pub fn ps2_joint(hd_name: &str) -> Option<usize> {
    hd_name.strip_prefix("SkeletonNode_")?.parse().ok()
}

/// Archive folder of the PS2 motion groups that belong to an HD character model
/// (`…/ps3data/chr/<kind>/<id>/mdl/d3d11/<id>.dae.phyre` → `ffx_ps2/<game>/master/jppc/chr/<kind>/<id>/mot/`).
pub fn motion_dir(model_path: &str) -> Option<String> {
    let game = if model_path.starts_with("ffx-2_data/") { "ffx2" } else { "ffx" };
    let rest = model_path.split("/ps3data/").nth(1)?;
    let (chr_dir, _) = rest.split_once("/mdl/")?;
    chr_dir.starts_with("chr/").then(|| format!("ffx_ps2/{game}/master/jppc/{chr_dir}/mot/"))
}

/// Least-squares factor from motion translations (first frame) to an HD skeleton's default-pose
/// translations, over the joints both share. About 0.01 for every character checked. A check on the decoder
/// only: the game's scale is the character's `.chr` value (`aerugo_sim::field::chr_motion_scale`).
pub fn fit_translation_scale(names: &[String], default_pose: &[[f32; 16]], motion: &Motion) -> f32 {
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for (i, name) in names.iter().enumerate() {
        let Some(j) = ps2_joint(name).filter(|&j| j < motion.joints as usize) else { continue };
        let Some(l) = default_pose.get(i) else { continue };
        let r = motion.joint(j, 0).translation;
        for k in 0..3 {
            num += f64::from(l[12 + k]) * f64::from(r[k]);
            den += f64::from(r[k]) * f64::from(r[k]);
        }
    }
    if den > 0.0 { (num / den) as f32 } else { 0.01 }
}

/// A motion group file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MotionGroup {
    /// Character id the group belongs to.
    pub chr_id: u16,
    /// (motion number, sequence offset) pairs.
    pub entries: Vec<(u16, u32)>,
    /// Each entry's jump table (offset in `data`: i16 offsets from the entry's sequence).
    pub entry_jumps: Vec<u32>,
    /// Each motion record's frame table (offset in `data`: i16 frame numbers the sequences pick by index).
    pub record_frames: Vec<u32>,
    /// The motion file's bytes (sequences and tables are read from it by offset).
    pub data: std::sync::Arc<[u8]>,
    pub motions: Vec<Motion>,
    /// For each entry, the motion record its sequence plays first (sequence code 1 and a 16-bit record index;
    /// code 2 takes one byte, codes 3 and 6 three).
    pub entry_motion: Vec<Option<usize>>,
}

impl MotionGroup {
    /// The first group of a motion file.
    pub fn parse(d: &[u8]) -> Result<MotionGroup> {
        MotionGroup::parse_at(&std::sync::Arc::from(d), u32_at(d, 0xC)? as usize)
    }

    /// Every group of a motion file: the count is at +4 and the descriptors follow one another (0x14 bytes each)
    /// from the offset at +0xC. Event motion files hold one group per character the event animates.
    pub fn parse_all(d: &[u8]) -> Result<Vec<MotionGroup>> {
        let first = u32_at(d, 0xC)? as usize;
        let count = (u32_at(d, 4)? as usize).clamp(1, 256);
        let data: std::sync::Arc<[u8]> = std::sync::Arc::from(d);
        (0..count).map(|g| MotionGroup::parse_at(&data, first + g * 0x14)).collect()
    }

    fn parse_at(data: &std::sync::Arc<[u8]>, desc: usize) -> Result<MotionGroup> {
        let d: &[u8] = data;
        if desc + 0x14 > d.len() {
            return Err(MotionError::Truncated("group descriptor"));
        }
        let chr_id = u16_at(d, desc + 4)?;
        let entry_count = i16_at(d, desc + 8)?.max(0) as usize;
        let motion_count = i16_at(d, desc + 10)?.max(0) as usize;
        let entries_at = u32_at(d, desc + 0xC)? as usize;
        let records_at = u32_at(d, desc + 0x10)? as usize;
        let mut entries = Vec::with_capacity(entry_count.min(4096));
        let mut entry_jumps = Vec::with_capacity(entry_count.min(4096));
        for i in 0..entry_count.min(4096) {
            let e = entries_at + i * 16;
            entries.push((u16_at(d, e)?, u32_at(d, e + 0xC)?));
            entry_jumps.push(u32_at(d, e + 8)?);
        }
        let mut motions = Vec::with_capacity(motion_count.min(4096));
        let mut record_frames = Vec::with_capacity(motion_count.min(4096));
        for i in 0..motion_count.min(4096) {
            let header = u32_at(d, records_at + i * 16 + 0xC)? as usize;
            motions.push(Motion::parse(d, header)?);
            record_frames.push(u32_at(d, records_at + i * 16 + 8)?);
        }
        let entry_motion = entries
            .iter()
            .map(|&(_, seq)| {
                let mut at = seq as usize;
                for _ in 0..16 {
                    match *d.get(at)? {
                        1 => return Some(i16_at(d, at + 1).ok()?.max(0) as usize),
                        2 => at += 1,
                        3 | 6 => at += 3,
                        _ => return None,
                    }
                }
                None
            })
            .collect();
        Ok(MotionGroup {
            chr_id,
            entries,
            entry_jumps,
            record_frames,
            data: data.clone(),
            motions,
            entry_motion,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn deltas_runs_and_wide_values() {
        // +3, repeat ×2 (this frame and two more), -1, wide +1000.
        let wide = 1000i16;
        let data = [0x03, 0x82, 0x7F, 0xC0 | (wide & 0x3F) as u8, (wide >> 6) as u8];
        let v = decode_stream(&data, 7);
        assert_eq!(v, vec![3, 6, 9, 12, 11, 1011, 2011]);
    }

    #[test]
    fn negative_wide_delta() {
        let w = -2000i16;
        let data = [0xC0 | (w & 0x3F) as u8, ((w >> 6) & 0xFF) as u8];
        assert_eq!(decode_stream(&data, 1), vec![-2000]);
    }

    #[test]
    fn header_and_channel_kinds() {
        // One joint, 2 frames: rot X animated, rot Y constant 1024, scale channels one, rest zero.
        let mut d = vec![0u8; 0x10];
        d[0..2].copy_from_slice(&2u16.to_le_bytes());
        d[2..4].copy_from_slice(&1u16.to_le_bytes());
        d[8..12].copy_from_slice(&0x10u32.to_le_bytes());
        d[12..16].copy_from_slice(&0x18u32.to_le_bytes());
        let kinds: u32 = 3 | (2 << 2) | (1 << 12) | (1 << 14) | (1 << 16);
        d.extend_from_slice(&kinds.to_le_bytes());
        d.extend_from_slice(&[0; 4]);
        // Values in channel order: channel 0 stream (size includes itself), channel 1 constant.
        d.extend_from_slice(&4u16.to_le_bytes());
        d.extend_from_slice(&[0x10, 0x10]);
        d.extend_from_slice(&1024i16.to_le_bytes());
        let m = Motion::parse(&d, 0).unwrap();
        assert_eq!(m.channels[0], Channel::Frames(vec![16, 32]));
        assert_eq!(m.channels[1], Channel::Constant(1024));
        assert_eq!(m.channels[6], Channel::One);
        let p = m.joint(0, 0);
        assert!((p.rotation[1] - std::f32::consts::FRAC_PI_2).abs() < 1e-6);
        assert_eq!(p.scale, [1.0; 3]);
    }

    #[test]
    fn joint_matrix_order() {
        // 90° about Z then translation: X axis maps to Y.
        let p = JointPose {
            rotation: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
            translation: [100.0, 0.0, 0.0],
            scale: [1.0; 3],
        };
        let m = p.matrix(0.01);
        assert!((m[0]).abs() < 1e-6 && (m[1] - 1.0).abs() < 1e-6);
        assert!((m[12] - 1.0).abs() < 1e-6);
        // Rx is applied before Rz: X then Z rotation of the Y axis.
        let p = JointPose {
            rotation: [std::f32::consts::FRAC_PI_2, 0.0, std::f32::consts::FRAC_PI_2],
            translation: [0.0; 3],
            scale: [1.0; 3],
        };
        let m = p.matrix(1.0);
        // Y axis -> (Rx) Z axis -> (Rz) Z axis.
        assert!((m[6] - 1.0).abs() < 1e-6, "{m:?}");
    }

    #[test]
    fn motion_dirs() {
        assert_eq!(
            motion_dir("ffx_data/gamedata/ps3data/chr/sum/s026/mdl/d3d11/s026.dae.phyre").as_deref(),
            Some("ffx_ps2/ffx/master/jppc/chr/sum/s026/mot/")
        );
        assert_eq!(
            motion_dir("ffx-2_data/gamedata/ps3data/chr/pc/c001/mdl/d3d11/c001.dae.phyre").as_deref(),
            Some("ffx_ps2/ffx2/master/jppc/chr/pc/c001/mot/")
        );
        assert_eq!(motion_dir("ffx_data/gamedata/ps3data/map/a/mdl/d3d11/a.dae.phyre"), None);
    }

    proptest! {
        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = MotionGroup::parse(&bytes);
            let _ = Motion::parse(&bytes, 0);
        }
    }
}
