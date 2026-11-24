//! Screen slots: which camera record a slot shows, its two rectangles (each a place and size, moved by channels
//! like the camera's), the two screen shakes, and the per-frame step that settles them.
//!
//! Slot layout (0x1c0 bytes): +0x1c screen distance, +0x30/+0x34 shake offsets, +0x40/+0x50 the rectangles now
//! (x, y, w, h), +0x60/+0x64 and +0x70/+0x74 their centre offsets, +0x80/+0x90 their targets, +0xa0/+0xb0 their
//! move starts, +0xc0 on, +0xc1 record shown, +0xc2/+0xc3 how each rectangle follows the other, +0xc4 a value set
//! with the first rectangle, +0xd0 −1, +0xdc + k·0x40 + j·0x10 the move channels, +0x15c/+0x160 shake phases,
//! +0x164/+0x168 their steps, +0x16c/+0x190 the shake channels.

use super::process::CamEnv;
use super::{CamMem, SCREEN, SCREENS};

/// The frame size the screen code centres on (the PS2 frame).
pub const FRAME: [f32; 2] = [512.0, 416.0];

fn slot(i: u32) -> u32 {
    SCREENS + i * SCREEN
}

impl CamMem {
    /// A slot on or off.
    pub fn screen_set_on(&mut self, i: u32, on: u8) {
        if i < 3 {
            self.set_u8(slot(i) + 0xc0, on);
        }
    }

    /// A slot on or off and the record it shows.
    pub fn screen_set_on_record(&mut self, i: u32, on: u8, rec: u8) {
        if i < 3 {
            self.set_u8(slot(i) + 0xc0, on);
            self.set_u8(slot(i) + 0xc1, rec);
        }
    }

    /// The record a slot shows.
    pub fn screen_set_record(&mut self, i: u32, rec: u8) {
        if i < 3 {
            self.set_u8(slot(i) + 0xc1, rec);
        }
    }

    /// How rectangle `k` follows the other (+0xc2 + k).
    pub fn screen_set_follow(&mut self, i: u32, k: i32, v: u8) {
        if i < 3 {
            self.set_u8((slot(i) as i32 + 0xc2 + k) as u32, v);
        }
    }

    /// The first rectangle's target and place (integers made floats), and the value kept with it.
    pub fn screen_set_rect_a(&mut self, i: u32, r: [i32; 4], extra: u32) {
        if i < 3 {
            let s = slot(i);
            self.set_vec4(s + 0x80, r.map(|v| v as f32));
            let v = self.vec4(s + 0x80);
            self.set_vec4(s + 0x40, v);
            self.set_u32(s + 0xc4, extra);
        }
    }

    /// The second rectangle's target and place.
    pub fn screen_set_rect_b(&mut self, i: u32, r: [i32; 4]) {
        if i < 3 {
            let s = slot(i);
            self.set_vec4(s + 0x90, r.map(|v| v as f32));
            let v = self.vec4(s + 0x90);
            self.set_vec4(s + 0x50, v);
        }
    }

    /// Time rectangle `k`'s four channels over `frames` in `mode`; the move starts from where it is.
    pub fn screen_move_frames(&mut self, i: u32, k: u32, frames: i16, mode: i16) {
        if i >= 3 {
            return;
        }
        let s = slot(i);
        let k = k & 1;
        let ch = s + 0xdc + k * 0x40;
        for j in 0..4 {
            self.set_i16(ch + j * 0x10 + 0xc, frames);
            self.set_i16(ch + j * 0x10, mode);
            self.set_i16(ch + j * 0x10 + 0xe, 0);
        }
        let v = self.vec4(s + 0x40 + k * 0x10);
        self.set_vec4(s + 0xa0 + k * 0x10, v);
    }

    /// Time rectangle `k`'s channels with a speed profile (speeding up, steady, slowing down frames).
    pub fn screen_move_profile(&mut self, i: u32, k: u32, accel: i16, steady: i16, decel: i16) {
        if i >= 3 {
            return;
        }
        let s = slot(i);
        let k = k & 1;
        let ch = s + 0xdc + k * 0x40;
        let total = decel.wrapping_add(steady).wrapping_add(accel);
        for j in 0..4 {
            let c = ch + j * 0x10;
            self.set_i16(c + 4, 0);
            self.set_i16(c, 2);
            self.set_i16(c + 0xc, total);
            self.set_i16(c + 0xa, decel);
            self.set_i16(c + 6, accel);
            self.set_i16(c + 8, steady);
            self.set_i16(c + 0xe, 0);
        }
        let v = self.vec4(s + 0x40 + k * 0x10);
        self.set_vec4(s + 0xa0 + k * 0x10, v);
    }

    /// How many of rectangle `k`'s channels still run.
    pub fn screen_moving(&self, i: u32, k: u32) -> i32 {
        if i >= 3 {
            return 0;
        }
        let ch = slot(i) + 0xdc + (k & 1) * 0x40;
        (0..4).filter(|j| self.i16(ch + j * 0x10 + 0xc) != 0).count() as i32
    }

    /// Stop rectangle `k`'s moves: its channels idle and its target where it is now.
    pub fn screen_move_stop(&mut self, i: u32, k: u32) {
        if i >= 3 {
            return;
        }
        let s = slot(i);
        let k = k & 1;
        for j in 0..4 {
            let c = s + 0xdc + k * 0x40 + j * 0x10;
            self.set_u16(c, 0);
            self.set_u32(c + 0xc, 0);
            let v = self.u32(s + 0x40 + k * 0x10 + 4 * j);
            self.set_u32(s + 0x80 + k * 0x10 + 4 * j, v);
        }
    }

    /// Start the slot's screen shakes (mask bits 0, 1): mode, frames, amplitude and jitter; phase 0 and its step.
    #[allow(clippy::too_many_arguments)]
    pub fn screen_shake(
        &mut self,
        i: u32,
        mask: u32,
        mode: u8,
        step: f32,
        frames: i16,
        amp: u16,
        jitter: u8,
    ) {
        if i >= 3 {
            return;
        }
        let s = slot(i);
        for k in 0..2 {
            if mask & (1 << k) != 0 {
                let ch = s + 0x16c + k * 0x24;
                self.set_u32(ch + 8, 0);
                self.set_u8(ch, mode);
                self.set_u8(ch + 1, jitter);
                self.set_u16(ch + 2, amp);
                self.set_i16(ch + 4, frames);
                self.set_i16(ch + 6, frames);
                self.set_u32(s + 0x15c + 4 * k, 0);
                self.set_f32(s + 0x164 + 4 * k, step);
            }
        }
    }

    /// Start spring shakes (mode `mode`, 9 from the scripts) on the slot's channels in `mask`: `steps` spring
    /// steps a frame, `frames` long, speed and pull scaled by a random share when `spread` is set (its sign
    /// flips the speed), and the fade.
    #[allow(clippy::too_many_arguments)]
    pub fn screen_shake_spring(
        &mut self,
        env: &mut dyn CamEnv,
        i: u32,
        mask: u32,
        mode: u8,
        steps: u8,
        frames: i32,
        speed: f32,
        spread: i32,
        pull: f32,
        fade: u8,
    ) {
        if i >= 3 || frames == 0 {
            return;
        }
        let s = slot(i);
        let (mut speed, mut pull, mut spread) = (speed, pull, spread);
        let mut held = speed;
        let mut bit = 1u32;
        for k in 0..2 {
            if mask & bit != 0 {
                let ch = s + 0x16c + k * 0x24;
                self.set_u32(ch + 0x10, 0);
                self.set_u8(ch + 0x20, steps);
                self.set_f32(ch + 0xc, speed);
                self.set_u8(ch, mode);
                self.set_f32(ch + 0x1c, pull);
                self.set_i16(ch + 4, frames as i16);
                self.set_i16(ch + 6, frames as i16);
                self.set_i16(ch + 0x22, spread as i16);
                if spread < 0 {
                    spread = -spread;
                    speed = -speed;
                    held = speed;
                }
                if spread != 0 {
                    let share = (0x100 - env.rng9() % spread) as f32;
                    speed = (share as f64 * held as f64 * 0.003_906_25) as f32;
                    pull = (pull as f64 * share as f64 * 0.003_906_25) as f32;
                    held = speed;
                }
                self.set_f32(ch + 0x14, speed);
                self.set_u8(ch + 0x21, fade);
                self.set_f32(ch + 0x18, pull);
            }
            bit = bit.rotate_left(1);
        }
    }

    /// The slot's screen shakes running.
    pub fn screen_shaking(&self, i: u32) -> bool {
        i < 3 && (0..2).any(|k| self.u8(slot(i) + 0x16c + k * 0x24) != 0)
    }

    /// Stop the slot's screen shakes (and their offsets).
    pub fn screen_shake_clear(&mut self, i: u32) {
        if i < 3 {
            let s = slot(i);
            self.set_u8(s + 0x16c, 0);
            self.set_u32(s + 0x30, 0);
            self.set_u8(s + 0x190, 0);
            self.set_u32(s + 0x34, 0);
        }
    }

    /// All slots' kept value at +0xd0 back to −1.
    pub fn screens_reset_d0(&mut self) {
        for i in 0..3 {
            self.set_i32(slot(i) + 0xd0, -1);
        }
    }

    /// The heading of the current record (the last screen slot stepped) on x/z from its eye
    /// (side 1's output, +0x66c) to its focus (side 0's, +0x50), as the camera's polar angle makes it: atan(dz / dx), less π for
    /// dx ≤ 0, ±π/2 (by dz's sign) for dx = 0.
    pub fn heading(&self) -> f32 {
        let r = self.screen_record;
        let dx = self.f32(r + 0x50) - self.f32(r + 0x66c);
        let dz = self.f32(r + 0x58) - self.f32(r + 0x674);
        if dx == 0.0 {
            return if dz < 0.0 { -1.570_796_4 } else { 1.570_796_4 };
        }
        let a = f64::from(dz / dx).atan() as f32;
        if dx <= 0.0 { a - std::f32::consts::PI } else { a }
    }

    /// The current screen slot's two shake offsets (what an effect's timeline sets each frame).
    pub fn set_screen_shake(&mut self, offsets: [u32; 2]) {
        let s = self.screen_slot;
        self.set_u32(s + 0x30, offsets[0]);
        self.set_u32(s + 0x34, offsets[1]);
    }

    /// The frame's settling of a slot's rectangles and shakes (the part of the screen step on the island): each
    /// rectangle takes its target, its centre offset from the frame, follows the other as asked (1: the other's
    /// size; 2: the other's place, no offset; 3: centred), runs its four channels; the shakes step; then every
    /// place, size and offset is cut to a whole number. Returns false when the slot is off.
    pub fn screen_step(&mut self, env: &mut dyn CamEnv, i: u32) -> bool {
        if i >= 3 {
            return false;
        }
        let s = slot(i);
        if self.u8(s + 0xc0) == 0 {
            return false;
        }
        self.screen_record = super::RECORDS + u32::from(self.u8(s + 0xc1)) * super::RECORD;
        self.screen_slot = s;
        for k in 0..2u32 {
            let cur = s + 0x40 + k * 0x10;
            let other = s + 0x50 - k * 0x10;
            let off = s + 0x60 + k * 0x10;
            let t = self.vec4(s + 0x80 + k * 0x10);
            self.set_vec4(cur, t);
            let ow = self.f32(other + 8);
            let oh = self.f32(other + 0xc);
            self.set_f32(off, ((ow as f64 - FRAME[0] as f64) * 0.5) as f32);
            self.set_f32(off + 4, ((oh as f64 - FRAME[1] as f64) * 0.5) as f32);
            match self.u8(s + 0xc2 + k) {
                1 => {
                    self.set_f32(cur + 8, ow);
                    self.set_f32(cur + 0xc, oh);
                }
                2 => {
                    self.set_f32(off, 0.0);
                    self.set_f32(off + 4, 0.0);
                    let v = self.vec4(other);
                    self.set_vec4(cur, v);
                }
                _ => {}
            }
            for j in 0..4 {
                let start = self.f32(cur + 4 * j + 0x60);
                self.channel_step(false, cur + 4 * j, start, s + 0xdc + k * 0x40 + j * 0x10);
            }
            if self.u8(s + 0xc2 + k) == 3 {
                let x = (self.f32(cur) as f64 + (FRAME[0] as f64 - self.f32(cur + 8) as f64) * 0.5) as f32;
                self.set_f32(cur, x);
                let y = (((FRAME[1] as f64 - self.f32(cur + 0xc) as f64) * 0.5) + self.f32(cur + 4) as f64)
                    as f32;
                self.set_f32(cur + 4, y);
            }
        }
        for k in 0..2u32 {
            let ch = s + 0x16c + k * 0x24;
            if self.u8(ch) != 0 {
                let phase = self.f32(s + 0x15c + 4 * k);
                self.shake_step(env, ch, phase, s + 0x30 + 4 * k);
                let v = self.f32(s + 0x164 + 4 * k) + phase;
                self.set_f32(s + 0x15c + 4 * k, v);
            }
        }
        for k in 0..2u32 {
            for j in 0..4 {
                let a = s + 0x40 + k * 0x10 + 4 * j;
                self.set_f32(a, self.f32(a).trunc());
            }
            for j in 0..2 {
                let a = s + 0x60 + k * 0x10 + 4 * j;
                self.set_f32(a, self.f32(a).trunc());
            }
        }
        true
    }
}
