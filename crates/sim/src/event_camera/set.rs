//! Setting a camera: targets, roll and screen distance, filters, move times, stopping and resetting, and the
//! script paths. Each function names the original it translates.

use super::{CamMem, PATH_BUFFERS, PATH_COUNTS, RECORD, RECORDS};
use crate::camera_move::angle_near;

/// A side's three move channels and the roll / screen distance channels (+0xd78 / +0xd88 of the record).
pub const CHANNELS: u32 = 0x580;

impl CamMem {
    /// The record's active handle equals `h` (the check the setters make before applying at once).
    pub(super) fn is_active(&self, h: u32) -> bool {
        self.active_handle(h & 0xff) == h
    }

    /// `out` = the point at heading, pitch and distance from the anchor (the address at side +0x8c; 0:
    /// from the origin), w 0.
    pub fn polar_to(&mut self, out: u32, anchor: u32, [yaw, pitch, dist]: [f32; 3]) {
        let from = (anchor != 0).then(|| self.vec3(anchor));
        let p = super::polar::place(from, yaw, pitch, dist);
        self.set_vec4(out, p);
    }

    /// Heading, pitch and distance of the point at `point` from the anchor.
    pub fn polar_of(&self, point: u32, anchor: u32) -> [f32; 3] {
        let from = (anchor != 0).then(|| self.vec3(anchor));
        super::polar::measure(self.vec3(point), from)
    }

    /// A polar filter places the target around the anchor; the target goes to the other
    /// side's +0x30; a rail search runs when asked; the other side's height add lifts the target. Returns the
    /// polar flag left after the rail and height checks.
    pub fn check_change(&mut self, side: u32, other: u32, filter: u32, polar: u32) -> u32 {
        let mut polar = polar;
        if polar == 1 {
            let anchor = self.u32(side + 0x8c);
            let p = self.vec3(side + 0x60);
            self.polar_to(side + 0x50, anchor, p);
        }
        let t = self.vec3(side + 0x50);
        self.set_vec3(other + 0x30, t);
        if self.u8(filter + 0x1d) != 0 {
            self.set_u8(filter + 0x1d, 0);
            self.rail_restart(filter, t);
        }
        if self.u8(filter + 4) != 0 && self.u8(filter + 5) == 0 && self.limit_lines(side, filter) != 0 {
            polar = 0;
        }
        let add = self.f32(other + 0x6c);
        if add == 0.0 {
            return polar;
        }
        self.set_f32(side + 0x54, self.f32(side + 0x54) + add);
        0
    }

    /// After a set, apply at once what no move holds back. `what` 1: the roll, 2: the
    /// screen distance (when their channels are idle); else the side: its check, then each idle axis's output and
    /// filter copy take the target (unless a channel moves by angles or the side is acceleration-limited).
    pub fn apply_set(&mut self, h: u32, side_index: u32, polar: u32, what: u32) -> bool {
        let rec = CamMem::record(h);
        let sub = (h >> 8) & 0xff;
        match what {
            1 => {
                if self.i16(rec + 0xd84) == 0 {
                    let v = self.f32(rec + 0xc94);
                    self.set_f32(rec + 0xc90, v);
                    self.set_f32(rec + 0xcd4 + sub * 0x30, v);
                    return true;
                }
                false
            }
            2 => {
                if self.i16(rec + 0xd94) == 0 {
                    let v = self.f32(rec + 0xca0);
                    self.set_f32(rec + 0xc9c, v);
                    self.set_f32(rec + 0xcd8 + sub * 0x30, v);
                    return true;
                }
                false
            }
            _ => {
                let side = CamMem::side(h, side_index);
                let other = CamMem::side(h, 1 - (side_index & 1));
                let by_angles = self.u8(side + 1) == 1;
                let mut free = self.u8(side + 3) == 0;
                for k in 0..3 {
                    if self.i16(side + CHANNELS + 0xc + k * 0x10) != 0 {
                        free = free && !by_angles;
                    }
                }
                let filter = side + 0xc0 + sub * super::FILTER;
                self.check_change(side, other, filter, polar);
                if free {
                    for k in 0..3 {
                        if self.i16(side + CHANNELS + 0xc + k * 0x10) == 0 {
                            let v = self.f32(side + 0x50 + 4 * k);
                            self.set_f32(side + 0x20 + 4 * k, v);
                            self.set_f32(filter + 0x80 + 4 * k, v);
                        }
                    }
                    return true;
                }
                false
            }
        }
    }

    /// A side fixed at a point (filter type 1, its value at filter +0x30); applied at once
    /// when the handle is the active one.
    pub fn set_vec(&mut self, h: u32, side_index: u32, v: [f32; 4]) {
        if !self.filter_awake(h) {
            return;
        }
        let side = CamMem::side(h, side_index);
        let filter = CamMem::filter(h, side_index);
        self.set_u8(filter + 7, 0);
        self.set_u8(filter, 0);
        self.set_u8(filter + 2, 1);
        self.set_vec4(filter + 0x30, v);
        if self.is_active(h) {
            self.set_f32(side + 0x6c, 0.0);
            self.set_vec4(side + 0x50, v);
            self.apply_set(h, side_index & 1, 0, 0);
        }
    }

    /// The sub-camera's roll target; applied when active (output too when its channel is
    /// idle).
    pub fn set_roll(&mut self, h: u32, roll: f32) {
        if !self.filter_awake(h) {
            return;
        }
        let rec = CamMem::record(h);
        let sub = (h >> 8) & 0xff;
        self.set_u8(rec + 0x713 + sub * super::FILTER, 0);
        self.set_f32(rec + 0xcb8 + sub * 0x30, roll);
        if self.is_active(h) {
            self.set_f32(rec + 0xc94, roll);
            if self.i16(rec + 0xd84) == 0 {
                self.set_f32(rec + 0xc90, roll);
                self.set_f32(rec + 0xcd4 + sub * 0x30, roll);
            }
        }
    }

    /// The sub-camera's screen distance target, as the roll.
    pub fn set_screen_distance(&mut self, h: u32, d: f32) {
        if !self.filter_awake(h) {
            return;
        }
        let rec = CamMem::record(h);
        let sub = (h >> 8) & 0xff;
        self.set_u8(rec + 0x713 + sub * super::FILTER, 0);
        self.set_f32(rec + 0xcc0 + sub * 0x30, d);
        if self.is_active(h) {
            self.set_f32(rec + 0xca0, d);
            if self.i16(rec + 0xd94) == 0 {
                self.set_f32(rec + 0xc9c, d);
                self.set_f32(rec + 0xcd8 + sub * 0x30, d);
            }
        }
    }

    /// A side placed at a heading, pitch (radians) and distance around the other (filter type 2,
    /// polar flag, values at filter +0x40).
    pub fn set_polar(&mut self, h: u32, side_index: u32, p: [f32; 3]) {
        if !self.filter_awake(h) {
            return;
        }
        let side = CamMem::side(h, side_index);
        let filter = CamMem::filter(h, side_index);
        self.set_u8(filter + 7, 0);
        self.set_u8(filter, 1);
        self.set_vec3(filter + 0x40, p);
        self.set_u8(filter + 2, 2);
        if self.is_active(h) {
            self.set_f32(side + 0x6c, 0.0);
            self.set_vec3(side + 0x60, p);
            self.apply_set(h, side_index & 1, 1, 0);
        }
    }

    /// Add to a side's heading, pitch and distance (measured from its target when it was not placed
    /// that way); filter type 7.
    pub fn add_polar(&mut self, h: u32, side_index: u32, d: [f32; 3]) {
        if !self.filter_awake(h) {
            return;
        }
        let side = CamMem::side(h, side_index);
        let filter = CamMem::filter(h, side_index);
        let was_polar = self.u8(filter);
        self.set_u8(filter + 7, 0);
        if was_polar == 0 {
            let anchor = self.u32(side + 0x8c);
            let p = self.polar_of(side + 0x50, anchor);
            self.set_vec3(filter + 0x40, p);
        }
        self.set_u8(filter, 1);
        self.set_u8(filter + 2, 7);
        let p = self.vec3(filter + 0x40);
        self.set_vec3(filter + 0x40, [d[0] + p[0], p[1] + d[1], p[2] + d[2]]);
        if self.is_active(h) {
            self.set_f32(side + 0x6c, 0.0);
            let p = self.vec3(filter + 0x40);
            self.set_vec3(side + 0x60, p);
            self.apply_set(h, side_index & 1, 1, 0);
        }
    }

    /// Add a limit line (up to five) to a side's sub-camera filter: from (x0, z0) to
    /// (x1, z1), its side flag and the share kept past it.
    pub fn add_limit_line(&mut self, h: u32, side_index: u32, flag: i32, p: [f32; 4], k: f32) {
        if !self.filter_awake(h) {
            return;
        }
        let filter = CamMem::filter(h, side_index);
        let n = u32::from(self.u8(filter + 4));
        if n >= 5 {
            return;
        }
        let e = filter + n * 0x20;
        let [x0, z0, x1, z1] = p;
        let (dx, dz) = (x1 - x0, z1 - z0);
        self.set_u32(e + 0x90, if flag != 0 { 2 } else { 0 });
        self.set_f32(e + 0xa0, dx);
        self.set_f32(e + 0xa4, dz);
        self.set_f32(e + 0xa8, x0);
        self.set_f32(e + 0xac, z0);
        let (x0d, z0d, dxd, dzd) = (x0 as f64, z0 as f64, dx as f64, dz as f64);
        self.set_f32(e + 0x94, (-x0d * dxd - z0d * dzd) as f32);
        self.set_f32(e + 0x98, (dzd * dzd + dxd * dxd) as f32);
        self.set_f32(e + 0x9c, k);
        self.set_u8(filter + 4, n as u8 + 1);
    }

    /// Clear a side's limit lines.
    pub fn clear_limits(&mut self, h: u32, side_index: u32) {
        if self.filter_awake(h) {
            self.set_u8(CamMem::filter(h, side_index) + 4, 0);
        }
    }

    /// A side's filter byte +5 (limits held).
    pub fn set_limits_held(&mut self, h: u32, side_index: u32, v: u8) {
        if self.filter_awake(h) {
            self.set_u8(CamMem::filter(h, side_index) + 5, v & 1);
        }
    }

    /// Turn a side's acceleration limit off and clear its accumulated move (active handle only).
    pub fn clear_accel(&mut self, h: u32, side_index: u32) {
        if !self.is_active(h) {
            return;
        }
        let side = CamMem::side(h, side_index);
        self.set_u8(side + 3, 0);
        self.set_vec3(side + 0x40, [0.0; 3]);
    }

    /// Time the channels the skip mask leaves over `frames` in `mode`, by angles or
    /// not; the eye side also its roll and screen distance; then the start is taken and the skip mask cleared.
    pub fn set_move_frames(&mut self, h: u32, side_index: u32, by_angles: u8, frames: i16, mode: i16) {
        if !self.is_active(h) {
            return;
        }
        let rec = CamMem::record(h);
        let side = CamMem::side(h, side_index);
        let filter = CamMem::filter(h, side_index);
        let skip = self.u8(filter + 7);
        self.set_u8(side + 1, by_angles);
        if side_index & 1 == 1 {
            if skip & 8 == 0 {
                self.set_i16(rec + 0xd78, mode);
                self.set_i16(rec + 0xd84, frames);
                self.set_i16(rec + 0xd86, 0);
            }
            if skip & 0x10 == 0 {
                self.set_i16(rec + 0xd88, mode);
                self.set_i16(rec + 0xd94, frames);
                self.set_i16(rec + 0xd96, 0);
            }
        }
        for k in 0..3 {
            if skip & (1 << k) == 0 {
                let ch = side + CHANNELS + k * 0x10;
                self.set_i16(ch, mode);
                self.set_i16(ch + 0xc, frames);
                self.set_i16(ch + 0xe, 0);
            }
        }
        self.move_frame(rec, side_index & 1 == 1, side, filter);
        self.set_u8(filter + 7, 0);
    }

    /// As above with the speed profile (mode 2: speeding up, steady, slowing down
    /// frames, and the lead).
    pub fn set_move_time(&mut self, h: u32, side_index: u32, by_angles: u8, prof: [i16; 4]) {
        if !self.is_active(h) {
            return;
        }
        let [a, s, d, lead] = prof;
        let total = d.wrapping_add(s).wrapping_add(a);
        let rec = CamMem::record(h);
        let side = CamMem::side(h, side_index);
        let filter = CamMem::filter(h, side_index);
        let skip = self.u8(filter + 7);
        self.set_u8(side + 1, by_angles);
        let set = |m: &mut CamMem, ch: u32| {
            m.set_i16(ch, 2);
            m.set_i16(ch + 0xc, total);
            m.set_i16(ch + 0xe, 0);
            m.set_i16(ch + 4, lead);
            m.set_i16(ch + 6, a);
            m.set_i16(ch + 8, s);
            m.set_i16(ch + 0xa, d);
        };
        if side_index & 1 == 1 {
            if skip & 8 == 0 {
                set(self, rec + 0xd78);
            }
            if skip & 0x10 == 0 {
                set(self, rec + 0xd88);
            }
        }
        for k in 0..3 {
            if skip & (1 << k) == 0 {
                set(self, side + CHANNELS + k * 0x10);
            }
        }
        self.move_frame(rec, side_index & 1 == 1, side, filter);
        self.set_u8(filter + 7, 0);
    }

    /// The start of the timed moves. A polar target: the polar start per unmasked
    /// channel, then the point start from it; else the point start per unmasked channel, then the polar start
    /// measured from it. The eye side also takes the roll and screen distance starts.
    pub fn move_frame(&mut self, rec: u32, eye: bool, side: u32, filter: u32) {
        let skip = self.u8(filter + 7);
        let anchor = self.u32(side + 0x8c);
        let mut n = 0;
        if self.u8(filter) == 1 {
            for k in 0..3 {
                if skip & (1 << k) == 0 {
                    n += 1;
                    let v = self.f32(side + 0x60 + 4 * k);
                    self.set_f32(side + 0x80 + 4 * k, v);
                }
            }
            if n != 0 {
                let p = self.vec3(side + 0x60);
                self.polar_to(side + 0x70, anchor, p);
            }
        } else {
            for k in 0..3 {
                if skip & (1 << k) == 0 {
                    n += 1;
                    let v = self.f32(side + 0x50 + 4 * k);
                    self.set_f32(side + 0x70 + 4 * k, v);
                }
            }
            if n != 0 {
                let p = self.polar_of(side + 0x50, anchor);
                self.set_vec3(side + 0x80, p);
            }
        }
        if eye {
            if skip & 8 == 0 {
                let v = self.f32(rec + 0xc94);
                self.set_f32(rec + 0xc98, v);
            }
            if skip & 0x10 == 0 {
                let v = self.f32(rec + 0xca0);
                self.set_f32(rec + 0xca4, v);
            }
        }
    }

    /// Stop a side's channels (the eye's roll and screen distance too) and fix it where it
    /// is (unless flag 8).
    pub fn move_stop(&mut self, h: u32, which: u32) {
        if !self.is_active(h) {
            return;
        }
        let rec = CamMem::record(h);
        let side_index = which & 1;
        let side = CamMem::side(h, side_index);
        if side_index == 1 {
            self.set_i16(rec + 0xd84, 0);
            self.set_i16(rec + 0xd94, 0);
        }
        for k in 0..3 {
            self.set_i16(side + CHANNELS + 0xc + k * 0x10, 0);
        }
        if which & 8 == 0 {
            let v = self.vec4(side + 0x50);
            self.set_vec(h, side_index, v);
            if side_index == 1 {
                let d = self.f32(rec + 0xca0);
                self.set_screen_distance(h, d);
                let r = self.f32(rec + 0xc94);
                self.set_roll(h, r);
            }
        }
    }

    /// Bit 0 a channel runs (the eye: also its roll or screen distance), bit 1 the side
    /// changed this frame, bit 2 the active sub-camera's filter follows a path.
    pub fn is_moving(&self, h: u32, side_index: u32) -> u32 {
        let rec = CamMem::record(h);
        let side = CamMem::side(h, side_index);
        let active = self.active_sub(h & 0xff);
        let mut v = 0;
        if side_index & 1 == 1 && (self.i16(rec + 0xd84) != 0 || self.i16(rec + 0xd94) != 0) {
            v = 1;
        }
        if (0..3).any(|k| self.i16(side + CHANNELS + 0xc + k * 0x10) != 0) {
            v = 1;
        }
        if self.u8(side + 2) != 0 {
            v |= 2;
        }
        if self.u8(side + 0xc2 + active * super::FILTER) == 0xe {
            v |= 4;
        }
        v
    }

    /// A side's output and target back to its filter's output copy; the eye also its roll
    /// and screen distance from the sub-camera's copies.
    pub fn reset(&mut self, h: u32, side_index: u32) {
        if !self.is_active(h) {
            return;
        }
        let rec = CamMem::record(h);
        let sub = (h >> 8) & 0xff;
        let side = CamMem::side(h, side_index);
        let filter = CamMem::filter(h, side_index);
        let v = self.vec4(filter + 0x80);
        self.set_vec4(side + 0x20, v);
        self.set_vec4(side + 0x50, v);
        if side_index == 1 {
            let r = self.f32(rec + 0xcd4 + sub * 0x30);
            self.set_f32(rec + 0xc94, r);
            self.set_f32(rec + 0xc90, r);
            let d = self.f32(rec + 0xcd8 + sub * 0x30);
            self.set_f32(rec + 0xca0, d);
            self.set_f32(rec + 0xc9c, d);
        }
    }

    /// Append a side's target and the wrapped roll to its path buffer (at most 32).
    pub fn spline_record(&mut self, h: u32, side_index: u32) {
        if !self.is_active(h) {
            return;
        }
        let s = side_index & 1;
        let n = self.i32(PATH_COUNTS + s * 4);
        if n >= 0x20 {
            return;
        }
        let n = n + 1;
        self.set_i32(PATH_COUNTS + s * 4, n);
        let side = CamMem::side(h, s);
        let at = PATH_BUFFERS + (s * 0x21 + n as u32) * 0x10;
        let v = self.vec3(side + 0x50);
        self.set_vec3(at, v);
        let roll = self.f32(CamMem::record(h) + 0xc94);
        self.set_f32(at + 0xc, angle_near(0.0, roll));
    }

    /// 604a / 6048: clear a side's path buffer count (when the handle is active).
    pub fn spline_clear(&mut self, h: u32, side_index: u32) {
        if self.is_active(h) {
            self.set_i32(PATH_COUNTS + (side_index & 1) * 4, 0);
        }
    }

    /// Path record header for a recorded path: the buffer's header count is its point count when there
    /// are at least three; returns its address.
    pub fn spline_buffer(&mut self, side_index: u32) -> Option<u32> {
        let s = side_index & 1;
        let n = self.i32(PATH_COUNTS + s * 4);
        if n < 3 {
            return None;
        }
        let at = PATH_BUFFERS + s * 0x210;
        self.set_i32(at, n);
        Some(at)
    }

    /// Record r's base (for natives).
    pub fn record_base(r: u32) -> u32 {
        RECORDS + r * RECORD
    }
}
