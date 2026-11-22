//! Positional sounds: 16 slots, each a sound and a source description in the script's memory, heard from the
//! camera (or from the player's place, facing as the camera does). Each frame, before the scripts, the slots'
//! volume (by distance) and pan (by angle) are worked out and sent to the sound layer; 0109 does it at once and
//! starts the sound.
//!
//! Source description (in the script's data): u16 flags (bit 0 a second point: the sound comes from the nearest
//! point of the segment, bit 1 follow a worker, bit 2 heard from the player, bit 15 distance on the ground plane),
//! u8 far and near volume (+2, +3), f32 pan scale (+4), the volume's near and far distance (+8, +0xc) and the
//! pan's (+0x10, +0x14).

use aerugo_script::memory::{Memory, Place};
use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;

use super::FieldState;
use super::sound::SoundState;
use crate::workers::{Workers, math};

fn d(x: f32) -> f64 {
    f64::from(x)
}

/// One slot.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Slot {
    pub sound: i32,
    /// The source description.
    pub source: Option<Place>,
    /// The place (or, following a worker, the worker in the first two bytes) and the second point.
    pub place: [f32; 3],
    pub worker: u16,
    pub second: [f32; 3],
    /// The last pan and volume sent.
    pub pan: u8,
    pub volume: u8,
}

/// The slots, the fade level (0–1) and its step per frame, the two listener matrices.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PosSounds {
    pub slots: [Slot; 16],
    pub level: f32,
    pub rate: f32,
    pub camera: [f32; 16],
    pub player: [f32; 16],
}

fn bytes(mem: &Memory, p: Place, off: usize, n: usize) -> Option<&[u8]> {
    match p {
        Place::Global(b, o) => mem.global[b].get(o + off..o + off + n),
        Place::Script(o) => mem.script.get(o + off..o + off + n),
        Place::Registers(w, o) => mem.registers.get(w)?.get(o + off..o + off + n),
    }
}

fn src_u16(mem: &Memory, p: Place, off: usize) -> u16 {
    bytes(mem, p, off, 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

fn src_u8(mem: &Memory, p: Place, off: usize) -> u8 {
    bytes(mem, p, off, 1).map_or(0, |b| b[0])
}

fn src_f32(mem: &Memory, p: Place, off: usize) -> f32 {
    bytes(mem, p, off, 4).map_or(0.0, |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn set_src_u16(mem: &mut Memory, p: Place, v: u16) {
    let b = match p {
        Place::Global(b, o) => mem.global[b].get_mut(o..o + 2),
        Place::Script(o) => mem.script.get_mut(o..o + 2),
        Place::Registers(w, o) => mem.registers.get_mut(w).and_then(|r| r.get_mut(o..o + 2)),
    };
    if let Some(b) = b {
        b.copy_from_slice(&v.to_le_bytes());
    }
}

/// The inverse of a rigid matrix (rows, points as rows): the rotation transposed, the translation turned back.
fn invert(m: &[f32; 16]) -> [f32; 16] {
    let mut o = [0.0f32; 16];
    o[12] = -((d(m[13]) * d(m[1]) + d(m[12]) * d(m[0]) + d(m[14]) * d(m[2])) as f32);
    o[13] = -((d(m[6]) * d(m[14]) + d(m[4]) * d(m[12]) + d(m[5]) * d(m[13])) as f32);
    o[14] = -((d(m[9]) * d(m[13]) + d(m[8]) * d(m[12]) + d(m[10]) * d(m[14])) as f32);
    o[0] = m[0];
    o[1] = m[4];
    o[2] = m[8];
    o[4] = m[1];
    o[5] = m[5];
    o[6] = m[9];
    o[8] = m[2];
    o[9] = m[6];
    o[10] = m[10];
    o[15] = 1.0;
    o
}

/// A point (w 1) through a matrix (points as rows).
fn xform(m: &[f32; 16], p: [f32; 3]) -> [f32; 4] {
    let c = 1.0f64;
    let w3 = (d(p[2]) * d(m[11]) + d(p[1]) * d(m[7]) + d(m[3]) * d(p[0])) as f32;
    [
        (c * d(m[12]) + d(p[2]) * d(m[8]) + d(p[1]) * d(m[4]) + d(m[0]) * d(p[0])) as f32,
        (c * d(m[13]) + d(p[2]) * d(m[9]) + d(p[1]) * d(m[5]) + d(m[1]) * d(p[0])) as f32,
        (c * d(m[14]) + d(p[2]) * d(m[10]) + d(p[1]) * d(m[6]) + d(m[2]) * d(p[0])) as f32,
        (c * d(m[15]) + d(w3)) as f32,
    ]
}

/// Where along a→b the origin lies (clamped to the segment).
fn nearest(a: [f32; 4], b: [f32; 4]) -> f32 {
    let ab = [d(b[0]) - d(a[0]), d(b[1]) - d(a[1]), d(b[2]) - d(a[2])];
    let len2 = (ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2]) as f32;
    if len2 == 0.0 {
        return 0.0;
    }
    ((-(d(a[0]) * ab[0] + d(a[1]) * ab[1] + d(a[2]) * ab[2])) as f32 / len2).clamp(0.0, 1.0)
}

fn ftol(x: f64) -> i32 {
    x.trunc() as i64 as i32
}

impl PosSounds {
    /// The listeners for this frame: the camera's view, and the camera turned the same way standing at the player.
    pub fn listen(&mut self, view: Option<[f32; 16]>, player: Option<[f32; 3]>) {
        let Some(v) = view else { return };
        self.camera = v;
        self.player = match player {
            None => v,
            Some(p) => {
                let mut l = invert(&v);
                l[12] = p[0];
                l[13] = p[1];
                l[14] = p[2];
                invert(&l)
            }
        };
    }

    /// One slot's volume and pan worked out and sent (`play`: the sound starts with them). Negated comparisons
    /// are the original's (they differ for NaN).
    #[allow(clippy::too_many_arguments, clippy::neg_cmp_op_on_partial_ord)]
    pub fn calc(
        &mut self,
        i: usize,
        play: bool,
        mem: &Memory,
        workers: &Workers,
        scene: u16,
        story: u16,
        s: &mut SoundState,
    ) {
        let slot = self.slots[i];
        let Some(src) = slot.source.filter(|_| slot.sound != 0) else { return };
        let mut flags = src_u16(mem, src, 0);
        let m = if flags & 4 == 0 { self.camera } else { self.player };
        let (p, q) = if flags & 2 == 0 {
            (slot.place, slot.second)
        } else {
            let w = usize::from(slot.worker);
            let wk = workers.list.get(w);
            let pos = wk.map_or([0.0; 3], crate::workers::Worker::position);
            let second = if wk.is_some_and(|wk| matches!(wk.kind, 2 | 5)) {
                flags |= 1;
                wk.map_or([0.0; 3], |wk| {
                    use crate::workers::p;
                    [wk.pf(p::PITCH), wk.pf(p::YAW), wk.pf(p::ROLL)]
                })
            } else {
                slot.second
            };
            (pos, second)
        };
        let mut a = xform(&m, p);
        if flags & 1 != 0 {
            let b = xform(&m, q);
            let t = nearest(a, b);
            let u = (1.0 - d(t)) as f32;
            for k in 0..3 {
                a[k] = (d(b[k]) * d(t) + d((d(u) * d(a[k])) as f32)) as f32;
            }
        }
        let (x, y, z) = (a[0], a[1], a[2]);
        let d2 = if flags & 0x8000 == 0 {
            (d((d(z) * d(z)) as f32) + d((d(y) * d(y)) as f32) + d(x) * d(x)) as f32
        } else {
            (d((d(z) * d(z)) as f32) + d(x) * d(x)) as f32
        };
        let dist = (d(d2).sqrt() as f32) + 0.0;
        // The angle folded into ±π/2 (behind the listener mirrors to the front).
        let mut ang = math::heading(0.0, 0.0, z, x);
        let pi = d(std::f32::consts::PI);
        if d(ang) > d(std::f32::consts::FRAC_PI_2) {
            ang = (pi - d(ang)) as f32;
        }
        if d(ang) < -d(std::f32::consts::FRAC_PI_2) {
            ang = (-pi - d(ang)) as f32;
        }
        let (pmin, pmax) = (src_f32(mem, src, 0x10), src_f32(mem, src, 0x14));
        let f = if !(pmin < dist) {
            0.0
        } else if !(dist < pmax) || pmax == pmin {
            1.0
        } else {
            (((d(dist) - d(pmin)) / (d(pmax) - d(pmin))) as f32).clamp(0.0, 1.0)
        };
        let g = (d(src_f32(mem, src, 4)) * d(f)) as f32;
        let pan_ang = (d(ang) * d(g)) as f32;
        let (vmin, vmax) = (src_f32(mem, src, 8), src_f32(mem, src, 0xc));
        let (far, near) = (f32::from(src_u8(mem, src, 2)), f32::from(src_u8(mem, src, 3)));
        let volume = if !(vmin < dist) && vmin != dist {
            ftol(d(near) * d(self.level))
        } else if !(dist <= vmax) || vmax == vmin {
            ftol(d(far) * d(self.level))
        } else {
            let t = ((d(dist) - d(vmin)) / (d(vmax) - d(vmin))) as f32;
            let v = ((1.0 - d(t)) * d(near) + d(t) * d(far)) as f32;
            ftol(d(v) * d(self.level)).clamp(0, 0x7f)
        };
        let pan = ftol((pi + d(pan_ang)) * 127.0 / d(2.0 * std::f32::consts::PI)).clamp(0, 0x7f);
        let sound = slot.sound & 0x7fff_ffff;
        if !play {
            s.cmds.push(super::sound::SoundCmd { id: 0x3b, args: [sound, volume & 0x7f, 0, 0] });
            s.cmds.push(super::sound::SoundCmd { id: 0x3d, args: [sound, pan & 0x7f, 0, 0] });
        } else {
            // A fix for one map: these sounds play only from three places.
            let fixed = scene == 0x76f
                && (story == 2000 || story == 0x7c6)
                && (0x11df9..0x11dfd).contains(&slot.sound);
            let at = [f32::from_bits(0x4192_45c0), 20.78476, 19.003876];
            if fixed && !at.contains(&x) {
                self.slots[i].pan = pan as u8;
                self.slots[i].volume = volume as u8;
                return;
            }
            if !s.seq_off {
                s.seq_count = s.seq_count.wrapping_add(1).max(1);
                s.cmds.push(super::sound::SoundCmd { id: 0x33, args: [sound, volume & 0x7f, pan & 0x7f, 0] });
                s.seq_played |= 1;
            }
        }
        self.slots[i].pan = pan as u8;
        self.slots[i].volume = volume as u8;
    }

    /// The frame's step: the fade level, then every slot.
    pub fn frame(&mut self, mem: &Memory, workers: &Workers, scene: u16, story: u16, s: &mut SoundState) {
        self.level = (d(self.rate) + d(self.level)) as f32;
        self.level = self.level.clamp(0.0, 1.0);
        for i in 0..16 {
            self.calc(i, false, mem, workers, scene, story, s);
        }
    }
}

fn slot_index(n: i32) -> usize {
    n.clamp(0, 15) as usize
}

pub fn register(n: &mut Natives<FieldState>) {
    // 0106(slot, sound, source): a slot's sound and source description.
    n.register(
        0x0106,
        Native::result(|g, c| {
            let desc = c.stack.pop_int();
            let sound = c.stack.pop_int();
            let i = slot_index(c.stack.pop_int());
            let place = super::shapes::var_place(g, c, desc);
            let s = &mut g.pos_sounds.slots[i];
            s.sound = sound;
            s.source = Some(place);
            Value::Int(1)
        }),
    );
    // 0107(slot, worker): the sound follows a worker.
    n.register(
        0x0107,
        Native::result(|g, c| {
            let w = c.stack.pop_int();
            let i = slot_index(c.stack.pop_int());
            if let Some(src) = g.pos_sounds.slots[i].source {
                let f = src_u16(c.memory, src, 0) | 2;
                set_src_u16(c.memory, src, f);
                g.pos_sounds.slots[i].worker = w as u16;
            }
            Value::Int(1)
        }),
    );
    // 0108(slot, second, x, y, z): the sound's place (or its second point); it stops following.
    n.register(
        0x0108,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            let second = c.stack.pop_int();
            let i = slot_index(c.stack.pop_int());
            if let Some(src) = g.pos_sounds.slots[i].source {
                let f = src_u16(c.memory, src, 0) & 0xfffd;
                set_src_u16(c.memory, src, f);
                let s = &mut g.pos_sounds.slots[i];
                if second != 0 {
                    s.second = [x, y, z];
                } else {
                    s.place = [x, y, z];
                }
            }
            Value::Int(1)
        }),
    );
    // 0109(slot): start the slot's sound where it is heard from.
    n.register(
        0x0109,
        Native::result(|g, c| {
            let i = slot_index(c.stack.pop_int());
            let scene = g.scene.unwrap_or(0);
            let story = c.memory.global[0].get(0xbec..0xbee).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            let (ps, ws, ss) = (&mut g.pos_sounds, &g.workers, &mut g.sound);
            ps.calc(i, true, c.memory, ws, scene, story, ss);
            Value::Int(1)
        }),
    );
    // 010a(slot): stop the slot's sound (command 0x36) and clear the slot.
    n.register(
        0x010A,
        Native::result(|g, c| {
            let i = slot_index(c.stack.pop_int());
            let s = g.pos_sounds.slots[i];
            if s.sound != 0 {
                g.sound
                    .cmds
                    .push(super::sound::SoundCmd { id: 0x36, args: [s.sound & 0x7fff_ffff, 0, 0, 0] });
                g.pos_sounds.slots[i] = Slot::default();
            }
            Value::Int(1)
        }),
    );
}
