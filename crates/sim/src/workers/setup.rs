//! Making a worker's record when the event loads, and resetting a level's move and turn records when a request
//! starts on it or the level starts over.

use super::{
    CAMERA, CHR, FLAGS, MODE9_FRAMES, MOVE, MOVES, PLACE, ROT, ROTS, SIGNAL_MASK, SIGNAL_TABLE, Worker, mv,
    p, rt,
};

const PI: f32 = std::f32::consts::PI;
/// π/3 and π/64 as the original stores them.
const THIRD: f32 = f32::from_bits(0x3f86_0a92);
const TURN_RATE: f32 = f32::from_bits(0x3d49_0fdb);

impl Worker {
    /// The record as the event's loading sets it up: engine flags, the position block's defaults by kind, each
    /// level's records (characters), the signal mask.
    pub fn setup(kind: u8, id: u16) -> Worker {
        let mut w = Worker::new(kind, id);
        w.set_u16(PLACE, w.u16(PLACE) & 0xe020 | 0x20);
        w.set_u32(CHR, 0);
        w.set_u32(CAMERA, 0);
        w.set_i32(0x40, -1);
        w.set_u32(SIGNAL_TABLE, 0);
        w.set_u8(0xaf, 0xf);
        w.set_f32(0xb4, -10000.0);
        // Each level's context starts with the level's own records (characters only).
        for l in 0..if w.two_levels() { 2 } else { 9 } {
            w.reset_level(l);
            if kind == 1 {
                let (m, r) = (MOVES + l as u32 * MOVE, ROTS + l as u32 * ROT);
                let c = w.ctx(l);
                w.set_u32(c + super::CTX_ROT, r);
                w.set_u32(c + super::CTX_MOVE, m);
                w.set_i16(m + mv::EASE, 0x5a);
                w.set_i16(m + mv::COUNT, 0);
                w.set_f32(m + mv::SPEED, 0.0);
                w.set_f32(m + mv::TURN_YAW, PI);
                w.set_f32(m + mv::TURN_PITCH, PI);
                for f in [rt::SPEED, rt::SPEED2, rt::PITCH_SPEED, rt::ROLL_SPEED] {
                    w.set_f32(r + f, TURN_RATE);
                }
                w.set_i16(r + rt::EASE, 0x5a);
                w.set_i16(r + rt::COUNT, 0);
                w.set_f32(m + mv::RADIUS, 1.0);
            }
        }
        if kind == 1 {
            for i in 0..2 {
                w.set_u32(0xaf4 + i * 4, 0);
                w.set_u32(0xafc + i * 4, 0);
                w.set_u16(0xb04 + i * 2, 0);
                w.set_u32(0xb08 + i * 4, 1);
            }
            w.set_u16(0xb14, 0);
            w.set_u16(FLAGS, w.u16(FLAGS) | 0x80);
            for a in (0xb18..0xb58).step_by(4) {
                w.set_u32(a, 0);
            }
        }
        match kind {
            1 | 2 | 3 | 5 | 6 => {
                for f in [p::X, p::Y, p::Z, p::SPEED, p::HEIGHT, p::PREV, p::PREV + 4, p::PREV + 8] {
                    w.set_pf(f, 0.0);
                }
                let b = w.pb();
                w.set_i16(b + p::TARGET, -1);
                w.set_pf(p::TALK_HEIGHT, -1.0);
                for f in [p::DIR_YAW, p::DIR_PITCH, p::YAW, p::PITCH, p::ROLL] {
                    w.set_pf(f, 0.0);
                }
                w.set_pf(p::TALK_ANGLE, THIRD);
                w.set_pf(p::R4C, THIRD);
                if kind == 1 {
                    w.set_pf(p::R44, 1.0);
                    w.set_pf(p::TALK_RADIUS, 2.0);
                    w.set_pf(p::RADIUS, 1.5);
                    w.set_pf(p::HEIGHT, 5.0);
                    w.set_u16(MODE9_FRAMES, 10);
                    for a in (0xae0..0xaf4).step_by(4) {
                        w.set_u32(a, 0);
                    }
                } else {
                    w.set_pf(p::R44, 0.0);
                    w.set_pf(p::TALK_RADIUS, 0.0);
                    w.set_pf(p::RADIUS, 1.0);
                    w.set_pf(p::HEIGHT, -1.0);
                }
            }
            4 => w.set_u16(FLAGS, 0x48e),
            _ => {}
        }
        w.set_u8(
            SIGNAL_MASK,
            match kind {
                1 => 0xf7,
                2 | 3 | 5 | 6 => 0xff,
                _ => 0,
            },
        );
        w
    }

    /// A level starts (a request starts on it, or it starts over): a character's level takes its own move and
    /// turn records back, both idle, the move starting and ending where the worker stands.
    pub fn reset_level(&mut self, level: i32) {
        if self.kind != 1 {
            return;
        }
        let l = level.max(0) as u32;
        let (m, r) = (MOVES + l * MOVE, ROTS + l * ROT);
        let c = self.ctx(level);
        self.set_u32(c + super::CTX_ROT, r);
        self.set_u32(c + super::CTX_MOVE, m);
        self.set_u16(m + mv::ID, 0);
        self.set_u16(r + rt::ID, 0);
        self.set_u16(m + mv::MODE, 0);
        self.set_u16(r + rt::MODE, 0);
        self.set_f32(m + mv::T, 0.0);
        self.set_f32(m + mv::TOTAL, 0.0);
        self.set_f32(m + mv::F14, 0.0);
        let pos = self.position();
        for k in 0..3 {
            self.set_f32(m + mv::START + k * 4, pos[k as usize]);
            self.set_f32(m + mv::TARGET + k * 4, pos[k as usize]);
        }
        self.set_f32(m + mv::F34, 0.0);
        self.set_f32(m + mv::F18, 0.0);
        self.set_f32(m + mv::F44, 0.0);
        self.set_f32(m + mv::F48, 0.0);
        for k in 0..6 {
            self.set_f32(r + rt::TARGET + k * 4, 0.0);
        }
    }
}
