//! The camera functions the scripts' camera natives call: each sets a filter type and its values on the handle's
//! sub-camera, and when the handle is the record's active one, runs the filter once and applies what no move holds
//! back.

use super::process::CamEnv;
use super::{CamMem, HANDLES_K5, HANDLES_K7, RECORD, RECORDS};

impl CamMem {
    /// The handle is its record's active one.
    pub fn is_active_handle(&self, h: u32) -> bool {
        self.is_active(h)
    }

    /// Type 4: the heading from the anchor to a ground point (x, z) at a set pitch, plus the measured distance
    /// over the pitch divisor; the distance set plus the measured one over its divisor.
    #[allow(clippy::too_many_arguments)]
    pub fn set_hypot_ground(
        &mut self,
        h: u32,
        side: u32,
        [x, z]: [f32; 2],
        pitch: f32,
        pitch_div: f32,
        dist: f32,
        dist_div: f32,
    ) {
        if !self.filter_awake(h) {
            return;
        }
        let f = CamMem::filter(h, side);
        self.set_u8(f + 7, 0);
        self.set_u8(f, 1);
        self.set_u8(f + 2, 4);
        self.set_f32(f + 0x30, x);
        self.set_f32(f + 0x38, z);
        self.set_f32(f + 0x44, pitch);
        self.set_f32(f + 0x48, dist);
        self.set_f32(f + 0x54, pitch_div);
        self.set_f32(f + 0x58, dist_div);
        if self.is_active(h) {
            self.set_hypot(CamMem::side(h, side), f, 0);
            self.apply_set(h, side & 1, 1, 0);
        }
    }

    /// Types 5 and 6: the heading and pitch from the anchor to a point; the distance set plus the measured one
    /// over its divisor (6: the measured distance less that).
    #[allow(clippy::too_many_arguments)]
    pub fn set_hypot_point(
        &mut self,
        h: u32,
        side: u32,
        less: bool,
        p: [f32; 3],
        pitch_div: f32,
        dist: f32,
        dist_div: f32,
    ) {
        if !self.filter_awake(h) {
            return;
        }
        let f = CamMem::filter(h, side);
        self.set_u8(f + 7, 0);
        self.set_u8(f, 1);
        self.set_u8(f + 2, 5 + u8::from(less));
        self.set_vec3(f + 0x30, p);
        self.set_f32(f + 0x48, dist);
        self.set_f32(f + 0x54, pitch_div);
        self.set_f32(f + 0x58, dist_div);
        if self.is_active(h) {
            self.set_hypot(CamMem::side(h, side), f, if less { 2 } else { 1 });
            self.apply_set(h, side & 1, 1, 0);
        }
    }

    /// Type 3: the average position of up to two workers (−1 none) plus an offset.
    pub fn set_act_workers(&mut self, env: &dyn CamEnv, h: u32, side: u32, a: i16, b: i16, off: [f32; 4]) {
        if !self.filter_awake(h) {
            return;
        }
        let f = CamMem::filter(h, side);
        self.set_u8(f + 7, 0);
        self.set_u8(f, 0);
        self.set_u8(f + 2, 3);
        self.set_i16(f + 0xc, a);
        self.set_i16(f + 0xe, b);
        self.set_vec4(f + 0x30, off);
        if self.is_active(h) {
            self.set_act(env, CamMem::side(h, side), f);
            self.apply_set(h, side & 1, 0, 0);
        }
    }

    /// Type 0xa: the heading from the other filter's workers' facing plus `yaw`, at a pitch and distance.
    pub fn set_act_facing(&mut self, env: &dyn CamEnv, h: u32, side: u32, yaw: f32, pitch: f32, dist: f32) {
        if !self.filter_awake(h) {
            return;
        }
        let side = side & 1;
        let f = CamMem::filter(h, side);
        self.set_f32(f + 0x40, yaw);
        self.set_u8(f + 7, 0);
        self.set_f32(f + 0x44, pitch);
        self.set_u8(f, 1);
        self.set_f32(f + 0x48, dist);
        self.set_u8(f + 2, 10);
        if self.is_active(h) {
            let other_filter = CamMem::filter(h, 1 - side);
            self.set_act2(env, CamMem::side(h, side), f, other_filter);
            self.apply_set(h, side, 1, 0);
        }
    }

    /// A height limit entry (up to five): `linear` 1 keeps `k` of what is past a bound, else a soft bound.
    pub fn add_limit_height(&mut self, h: u32, side: u32, linear: bool, min: f32, max: f32, k: f32) {
        if !self.filter_awake(h) {
            return;
        }
        let f = CamMem::filter(h, side);
        let n = u32::from(self.u8(f + 4));
        if n >= 5 {
            return;
        }
        let e = f + n * 0x20;
        self.set_f32(e + 0xa8, min);
        self.set_f32(e + 0xac, max);
        self.set_f32(e + 0x9c, k);
        self.set_u32(e + 0x90, u32::from(linear) * 2 + 1);
        self.set_u8(f + 4, n as u8 + 1);
    }

    /// A rail entry (up to five): the target is drawn to `k` of its distance from a path (mode 3: cubic); its
    /// search starts again next frame.
    pub fn add_rail(&mut self, h: u32, side: u32, path: u32, k: f32, mode: i32) {
        if !self.filter_awake(h) {
            return;
        }
        let f = CamMem::filter(h, side);
        let n = u32::from(self.u8(f + 4));
        if n >= 5 {
            return;
        }
        let e = f + n * 0x20;
        let cubic = mode == 3;
        self.set_u8(f + 0x1e, u8::from(cubic));
        self.set_u32(e + 0x90, if cubic { 5 } else { 4 });
        self.set_u8(f + 4, n as u8 + 1);
        self.set_f32(f + 0x20, k);
        self.set_u32(f + 8, path);
        self.set_f32(f + 0x24, 0.0);
        self.set_u8(f + 0x1d, 1);
    }

    /// Easing divisors of a sub-camera's side (x, y, z, and the eye's roll).
    pub fn set_easing(&mut self, h: u32, side: u32, d: [f32; 4]) {
        if self.filter_awake(h) {
            let f = CamMem::filter(h, side);
            self.set_vec4(f + 0x70, d);
        }
    }

    /// The acceleration limit (active handle only): the cap and speed, the accumulated move starting from the
    /// anchor towards the output at `start`.
    pub fn set_accel(&mut self, h: u32, side: u32, start: f32, cap: f32, speed: f32) {
        if !self.is_active(h) {
            return;
        }
        let s = CamMem::side(h, side);
        self.set_f32(s + 8, cap);
        self.set_u8(s + 3, 1);
        self.set_f32(s + 0xc, speed);
        let anchor = self.u32(s + 0x8c);
        let from = self.vec3(anchor);
        let to = self.vec3(s + 0x20);
        let step = super::process::move_target_step(to, from, start);
        self.set_vec3(s + 0x40, step);
        let w = self.f32(s + 0x2c);
        self.set_f32(s + 0x4c, w);
    }

    /// Types 8 and 9: the eye from a table of points (the table's flags plus `squared`), its heading `yaw`.
    pub fn set_point_table(&mut self, h: u32, side: u32, squared: u8, table: u32, yaw: f32) {
        if !self.filter_awake(h) {
            return;
        }
        let f = CamMem::filter(h, side);
        self.set_u8(f + 7, 0);
        self.set_u8(f, 1);
        self.set_u8(f + 2, 8);
        let flags = self.tables.get(&table).and_then(|t| t.get(4).copied()).unwrap_or(0);
        self.set_f32(f + 0x40, yaw);
        self.set_u8(f + 3, flags | squared);
        self.set_u32(f + 8, table);
        if self.is_active(h) {
            self.set_point(CamMem::side(h, side), f);
            self.apply_set(h, side & 1, 1, 0);
        }
    }

    /// Type 9: as type 8 with the heading measured from the anchor to (x, z) plus `yaw`.
    #[allow(clippy::too_many_arguments)]
    pub fn set_point_table_around(
        &mut self,
        h: u32,
        side: u32,
        squared: u8,
        table: u32,
        yaw: f32,
        x: f32,
        z: f32,
    ) {
        if !self.filter_awake(h) {
            return;
        }
        let s = CamMem::side(h, side);
        let f = CamMem::filter(h, side);
        self.set_u8(f + 7, 0);
        self.set_u8(f, 1);
        self.set_u8(f + 2, 9);
        let flags = self.tables.get(&table).and_then(|t| t.get(4).copied()).unwrap_or(0);
        self.set_u32(f + 8, table);
        self.set_u8(f + 3, flags | squared);
        self.set_f32(f + 0x30, x);
        self.set_f32(f + 0x38, z);
        self.set_f32(f + 0x50, yaw);
        if self.is_active(h) {
            let anchor = self.u32(s + 0x8c);
            let m = self.polar_of(f + 0x30, anchor)[0];
            self.set_f32(f + 0x40, self.f32(f + 0x50) + m);
            self.set_point(s, f);
            self.apply_set(h, side & 1, 1, 0);
        }
    }

    /// The moves a set leaves out (filter +7).
    pub fn set_skip_mask(&mut self, h: u32, side: u32, mask: u8) {
        if self.filter_awake(h) {
            let f = CamMem::filter(h, side);
            self.set_u8(f + 7, mask);
        }
    }

    /// Type 0xe: follow a path from place `start` by `step` a frame.
    pub fn follow_path(&mut self, h: u32, side: u32, path: u32, start: f32, step: f32, mode: u8) {
        if !self.filter_awake(h) || path == 0 {
            return;
        }
        let side = side & 1;
        let f = CamMem::filter(h, side);
        self.set_f32(f + 0x68, start);
        self.set_u8(f + 7, 0);
        self.set_u8(f, 0);
        self.set_f32(f + 0x6c, step);
        self.set_u8(f + 2, 0xe);
        self.set_u8(f + 0x1c, mode);
        self.set_u32(f + 8, path);
        if self.is_active(h) {
            self.path_tick(h, side, CamMem::side(h, side), f);
            self.apply_set(h, side, 0, 0);
        }
    }

    /// Type 0x11: the side on a path at its own filter's rail place.
    pub fn set_eye_rail_path(&mut self, h: u32, side: u32, path: u32, mode: u8) {
        if !self.filter_awake(h) || path == 0 {
            return;
        }
        let side = side & 1;
        let f = CamMem::filter(h, side);
        self.set_u8(f + 7, 0);
        self.set_u8(f, 0);
        self.set_u8(f + 2, 0x11);
        self.set_u8(f + 0x1c, mode);
        self.set_u32(f + 8, path);
        if self.is_active(h) {
            self.eye_on_path(CamMem::side(h, side), f, f);
            self.apply_set(h, side, 0, 0);
        }
    }

    /// Start a side's shakes (active handle only) on the channels in `mask` (bits 0–2: x, y, z).
    #[allow(clippy::too_many_arguments)]
    pub fn shake(
        &mut self,
        h: u32,
        side: u32,
        mask: u32,
        mode: u8,
        step: f32,
        frames: i16,
        amp: u16,
        jitter: u8,
    ) {
        if !self.is_active(h) {
            return;
        }
        let s = CamMem::side(h, side);
        for k in 0..3 {
            if mask & (1 << k) != 0 {
                let ch = s + 0x5b0 + k * 0x24;
                self.set_u32(ch + 8, 0);
                self.set_u8(ch, mode);
                self.set_u8(ch + 1, jitter);
                self.set_u16(ch + 2, amp);
                self.set_i16(ch + 4, frames);
                self.set_i16(ch + 6, frames);
                self.set_u32(s + 0xa0 + 4 * k, 0);
                self.set_f32(s + 0xb0 + 4 * k, step);
            }
        }
    }

    /// Stop a side's shakes and clear their offsets (active handle only).
    pub fn shake_clear(&mut self, h: u32, side: u32) {
        if !self.is_active(h) {
            return;
        }
        let s = CamMem::side(h, side);
        for k in 0..3 {
            self.set_u8(s + 0x5b0 + k * 0x24, 0);
            self.set_u32(s + 0x90 + 4 * k, 0);
        }
    }

    /// A side's shakes running (any channel of the handle's record).
    pub fn shaking(&self, h: u32, side: u32) -> bool {
        let s = CamMem::side(h & 0xff, side);
        (0..3).any(|k| self.u8(s + 0x5b0 + k * 0x24) != 0)
    }

    /// The priority adjust of every slot holding the handle (unless the battle camera holds the system).
    pub fn set_priority_adjust(&mut self, h: u32, v: i8) {
        let rec = RECORDS + (h & 0xff) * RECORD;
        for sub in 0..4 {
            let s = rec + 0xc68 + sub * 8;
            if self.u32(s) == h {
                self.set_i8(s + 6, v);
            }
        }
    }

    /// Free every record's character and battle cameras (kinds 5 and 7).
    pub fn free_chr_cameras(&mut self) {
        for r in 0..3 {
            for table in [HANDLES_K5, HANDLES_K7] {
                let h = self.u32(table + r * 4);
                self.free_handle(h);
                self.set_u32(table + r * 4, 0);
            }
        }
    }

    /// The record's per-sub-camera value at +0xcc8 (below 0: its bits with 2), marked set; the current one too
    /// when the handle is active.
    pub fn set_sub_value(&mut self, h: u32, v: i32) {
        if !self.filter_awake(h) {
            return;
        }
        let rec = CamMem::record(h);
        let sub = (h >> 8) & 0xff;
        self.set_u32(rec + 0xca8, 1);
        let v = if v < 0 { self.u32(rec + 0xcc8 + sub * 0x30) | 2 } else { v as u32 };
        self.set_u32(rec + 0xcc8 + sub * 0x30, v);
        if self.is_active(h) {
            self.set_u32(rec + 0xcac, v);
        }
    }

    /// One of the record's two per-sub-camera side values (+0xccc / +0xcd0), marked set; the current one too.
    pub fn set_sub_side_value(&mut self, h: u32, which: u32, v: u32) {
        if !self.filter_awake(h) {
            return;
        }
        let rec = CamMem::record(h);
        let sub = (h >> 8) & 0xff;
        self.set_u32(rec + 0xca8, 1);
        self.set_u32(rec + 0xccc + (which & 1) * 4 + sub * 0x30, v);
        if self.is_active(h) {
            self.set_u32(rec + 0xcb0 + (which & 1) * 4, v);
        }
    }

    /// Attach a side to up to two characters (types 0xb–0xd, 0xf, 0x10 by `variant`): ids, the height share,
    /// and for the variants a heading, pitch and size factor. Battle handles (kind 5) pick characters by the
    /// battle's selectors.
    #[allow(clippy::too_many_arguments)]
    pub fn attach_chr(
        &mut self,
        env: &dyn CamEnv,
        h: u32,
        side: u32,
        variant: u32,
        a: i32,
        b: i32,
        share: f32,
        yaw: f32,
        pitch: f32,
        size: f32,
    ) {
        if !self.filter_awake(h) {
            return;
        }
        let side = side & 1;
        let f = CamMem::filter(h, side);
        self.set_u8(f, 0);
        match variant {
            0 => self.set_u8(f + 2, 0xb),
            1 => self.set_u8(f + 2, 0xc),
            2 => self.set_u8(f + 2, 0xd),
            3 => {
                self.set_u8(f + 2, 0xf);
                self.set_u8(f, 1);
            }
            4 => {
                self.set_u8(f + 2, 0x10);
                self.set_u8(f, 1);
            }
            _ => {}
        }
        self.set_i16(f + 0x10, a as i16);
        self.set_i16(f + 0x12, b as i16);
        if h & 0xf_0000 == 0x5_0000 {
            self.set_u32(f + 0x14, env.battle_select(a));
            self.set_u32(f + 0x18, env.battle_select(b));
        } else {
            self.set_u32(f + 0x14, a as u32);
            self.set_u32(f + 0x18, b as u32);
        }
        self.set_f32(f + 0x5c, share);
        if variant != 0 {
            self.set_f32(f + 0x40, yaw);
            self.set_f32(f + 0x44, pitch);
            self.set_f32(f + 0x60, size);
        }
        let other = CamMem::side(h, 1 - side);
        let v = self.vec3(other + 0x30);
        self.set_vec3(f + 0x30, v);
        self.set_u8(f + 6, 1);
        self.set_u32(f + 0x64, 0);
        if self.is_active(h) {
            let other_filter = CamMem::filter(h, 1 - side);
            self.chr_filter(env, h, variant, CamMem::side(h, side), f, other, other_filter);
            self.apply_set(h, side, 0, 0);
        }
    }
}
