//! A frame of the camera system: for every screen slot that is on, its camera record
//! once — each side's active filter sets the target (loop 1), then the timed moves, the acceleration limit, the
//! easing, the output and the shakes (loop 2), then the roll and the screen distance.
//!
//! Arithmetic follows the original's x87 code: intermediates are doubles and round to float where the original
//! stores them.

use super::spline::{self, Pts};
use super::{BATTLE_DISTANCE, BATTLE_TARGETS, CamMem, FILTER, RECORD, RECORDS, SCREEN, SCREENS, SIDE};
use crate::camera_move::angle_near;

/// What the camera reads from the rest of the game.
pub trait CamEnv {
    /// A worker's position (an index out of range: the running worker's).
    fn worker_pos(&self, w: i32) -> [f32; 3];
    /// A worker's facing.
    fn worker_yaw(&self, w: i32) -> f32;
    /// The event's worker count.
    fn worker_count(&self) -> i32;
    /// Worker `i` is a character that is shown (its kind byte 1 and a model).
    fn worker_shown(&self, i: i32) -> bool;
    /// The running worker (−1 none).
    fn current_worker(&self) -> i32;
    /// A battle character query (kind 5 handles): the sum over the mask's characters; returns how many.
    fn battle_chr_info(&self, _what: u32, _mask: u32, _id: i32, _out: &mut [f32; 4]) -> i32 {
        0
    }
    /// The game's random number generator, table 9.
    fn rng9(&mut self) -> i32;
    /// The battle takes the camera (its targets at [`super::BATTLE_TARGETS`]).
    fn battle_override(&self) -> bool {
        false
    }
    /// The battle's screen distance override.
    fn distance_override(&self) -> Option<f32> {
        None
    }
    /// The battle's character selection for a selector (a mask of battle characters); fields have no battle
    /// characters, so the battle supplies this.
    fn battle_select(&self, _selector: i32) -> u32 {
        0
    }
}

/// An f32 widened for an x87 expression.
fn d(x: f32) -> f64 {
    x as f64
}

impl CamMem {
    /// The bytes of a path or table a filter names (+8): a recorded buffer in the island, or a script table.
    pub(super) fn filter_data(&self, id: u32) -> Option<Vec<u8>> {
        if (super::BASE..super::END).contains(&id) {
            let n = self.i32(id).clamp(0, 0x20) as u32;
            let mut out = Vec::with_capacity(16 + 16 * n as usize);
            for k in 0..(4 + 4 * n) {
                out.extend_from_slice(&self.u32(id + 4 * k).to_le_bytes());
            }
            Some(out)
        } else {
            self.tables.get(&id).cloned()
        }
    }

    /// One frame of every shown camera.
    pub fn process(&mut self, env: &mut dyn CamEnv) {
        for r in 0..3 {
            self.set_u8(RECORDS + r * RECORD + 0xc89, 1);
        }
        for slot in 0..3u32 {
            let s = SCREENS + slot * SCREEN;
            if self.u8(s + 0xc0) == 0 {
                continue;
            }
            let r = u32::from(self.u8(s + 0xc1));
            if r >= 3 {
                continue;
            }
            let rec = RECORDS + r * RECORD;
            if self.u8(rec + 0xc89) == 0 {
                continue;
            }
            self.set_u8(rec + 0xc89, 0);
            let sub = self.active_sub(r);
            let handle = self.u32(rec + 0xc68 + sub * 8);
            for i in 0..2u32 {
                let side = rec + 0x30 + i * SIDE;
                let other = rec + 0x30 + (1 - i) * SIDE;
                let filter = side + 0xc0 + sub * FILTER;
                let other_filter = other + 0xc0 + sub * FILTER;
                let ty = self.u8(filter + 2);
                if ty != 0 {
                    self.set_u8(side + 2, 0);
                    self.set_u8(side + 4, 1);
                    self.set_u8(side + 5, 1);
                    self.run_filter(env, handle, i, side, filter, other, other_filter, ty);
                }
                if i32::from(self.i8(rec + 0xc8c)) != sub as i32 {
                    self.move_acc(side, filter);
                }
                let polar = i32::from(self.i8(filter)) as u32;
                let changed = self.check_change(side, other, filter, polar);
                self.set_u8(filter + 1, changed as u8);
            }
            for i in 0..2u32 {
                let side = rec + 0x30 + i * SIDE;
                let filter = side + 0xc0 + sub * FILTER;
                if env.battle_override() {
                    let v = self.vec4(BATTLE_TARGETS + i * 0x10);
                    self.set_vec4(side + 0x50, v);
                    self.set_vec4(side + 0x20, v);
                    self.set_vec4(side + 0x10, v);
                } else if self.u8(filter + 2) != 0 {
                    self.output_side(env, rec, sub, side, filter);
                }
            }
            // The battle's special camera runs here while a battle has it on; the field never
            // does.
            self.roll_and_distance(env, rec, sub, s);
        }
    }

    /// Loop 1's filter by type: 1 fixed, 2 polar, 3 the workers' average, 4–6 measured around the anchor, 7 polar
    /// added, 8 a table of points, 9 a table around a measured heading, 0xa the workers' facing, 0xb–0xd and
    /// 0xf–0x10 characters (battle and attached), 0xe a path, 0x11 the eye on a path at the focus's rail place.
    #[allow(clippy::too_many_arguments)]
    fn run_filter(
        &mut self,
        env: &mut dyn CamEnv,
        handle: u32,
        side_index: u32,
        side: u32,
        filter: u32,
        other: u32,
        other_filter: u32,
        ty: u8,
    ) {
        match ty {
            1 => {
                self.set_f32(side + 0x6c, 0.0);
                let v = self.vec4(filter + 0x30);
                self.set_vec4(side + 0x50, v);
            }
            3 => self.set_act(env, side, filter),
            4 => self.set_hypot(side, filter, 0),
            5 => self.set_hypot(side, filter, 1),
            6 => self.set_hypot(side, filter, 2),
            2 | 7 => {
                if ty == 7 {
                    self.set_u8(side + 4, 0);
                    self.set_u8(side + 5, 0);
                }
                self.set_f32(side + 0x6c, 0.0);
                let p = self.vec3(filter + 0x40);
                self.set_vec3(side + 0x60, p);
            }
            8 => self.set_point(side, filter),
            9 => {
                let anchor = self.u32(side + 0x8c);
                let yaw = self.polar_of(filter + 0x30, anchor)[0];
                self.set_f32(filter + 0x40, self.f32(filter + 0x50) + yaw);
                self.set_point(side, filter);
            }
            0xa => self.set_act2(env, side, filter, other_filter),
            0xb => self.chr_filter(env, handle, 0, side, filter, other, other_filter),
            0xc => self.chr_filter(env, handle, 1, side, filter, other, other_filter),
            0xd => self.chr_filter(env, handle, 2, side, filter, other, other_filter),
            0xf => self.chr_filter(env, handle, 3, side, filter, other, other_filter),
            0x10 => self.chr_filter(env, handle, 4, side, filter, other, other_filter),
            0xe => self.path_tick(handle, side_index, side, filter),
            0x11 => self.eye_on_path(side, filter, other_filter),
            _ => {}
        }
    }

    /// The target is the filter's offset plus the average position of up to two workers
    /// (filter +0xc, −1 none; an unknown index counts as the running worker).
    pub(super) fn set_act(&mut self, env: &dyn CamEnv, side: u32, filter: u32) {
        let mut sum = [0.0f32; 3];
        let mut n = 0;
        for k in 0..2 {
            let w = self.i16(filter + 0xc + 2 * k);
            if w != -1 {
                let p = env.worker_pos(i32::from(w));
                sum = [sum[0] + p[0], sum[1] + p[1], sum[2] + p[2]];
                n += 1;
            }
        }
        if n > 1 {
            let f = n as f32;
            sum = sum.map(|v| v / f);
        }
        let o = self.vec3(filter + 0x30);
        self.set_vec3(side + 0x50, [o[0] + sum[0], o[1] + sum[1], o[2] + sum[2]]);
        self.set_f32(side + 0x6c, 0.0);
    }

    /// The heading from the anchor to the filter's
    /// point; the pitch the filter's (0) or the measured one (1, 2), plus the measured distance over the divisor
    /// at +0x54; the distance the filter's plus the measured one over the divisor at +0x58 (2: the measured
    /// distance less that).
    pub(super) fn set_hypot(&mut self, side: u32, filter: u32, variant: u8) {
        let anchor = self.u32(side + 0x8c);
        let [yaw, pitch, dist] = self.polar_of(filter + 0x30, anchor);
        self.set_f32(side + 0x60, yaw);
        let div_p = self.f32(filter + 0x54);
        let div_d = self.f32(filter + 0x58);
        let base_p = if variant == 0 { self.f32(filter + 0x44) } else { pitch };
        self.set_f32(side + 0x64, base_p);
        if div_p != 0.0 {
            self.set_f32(side + 0x64, (d(dist) / d(div_p) + d(base_p)) as f32);
        }
        let d0 = self.f32(filter + 0x48);
        if variant == 2 {
            let set = if div_d != 0.0 { (d(dist) / d(div_d) + d(d0)) as f32 } else { d0 };
            self.set_f32(side + 0x68, dist - set);
        } else {
            self.set_f32(side + 0x68, d0);
            if div_d != 0.0 {
                self.set_f32(side + 0x68, (d(d0) + d(dist) / d(div_d)) as f32);
            }
        }
        self.set_f32(side + 0x6c, 0.0);
    }

    /// The eye from a table of points (filter +8): each point's weight is its masked
    /// distance from the anchor copy (+0x30) plus one (squared with flag bit 0) over its reach, kept in the
    /// table; the heading (plus the filter's), pitch, distance and height blend the points by the total over each
    /// weight.
    pub(super) fn set_point(&mut self, side: u32, filter: u32) {
        let id = self.u32(filter + 8);
        let Some(mut t) = self.filter_data(id) else { return };
        let f =
            |t: &[u8], o: usize| t.get(o..o + 4).map_or(0.0, |b| f32::from_le_bytes(b.try_into().unwrap()));
        let n = t.get(0..4).map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap()));
        let flags = self.u8(filter + 3);
        let anchor = self.vec3(side + 0x30);
        let mut total = 0.0f32;
        for i in 0..n.max(0) as usize {
            let e = 0x10 + i * 0x30;
            let dx = if flags & 2 != 0 { 0.0 } else { anchor[0] - f(&t, e) };
            let dy = if flags & 4 != 0 { 0.0 } else { anchor[1] - f(&t, e + 4) };
            let dz = if flags & 8 != 0 { 0.0 } else { anchor[2] - f(&t, e + 8) };
            let s = ((d(dx) * d(dx) + d(dy) * d(dy)) + d(dz) * d(dz)).sqrt() as f32 + 1.0;
            let w =
                if flags & 1 != 0 { (d(s) * d(s) / d(f(&t, e + 0xc))) as f32 } else { s / f(&t, e + 0xc) };
            if let Some(b) = t.get_mut(e + 0x1c..e + 0x20) {
                b.copy_from_slice(&w.to_le_bytes());
            }
            total += w;
        }
        let (mut sum, mut yaw, mut pitch, mut dist, mut height) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for i in 0..n.max(0) as usize {
            let e = 0x20 + i * 0x30;
            let k = total / f(&t, e + 0xc);
            yaw = (d(yaw) + d(k) * d(f(&t, e))) as f32;
            pitch = (d(f(&t, e + 4)) * d(k) + d(pitch)) as f32;
            dist = (d(f(&t, e + 8)) * d(k) + d(dist)) as f32;
            height = (d(f(&t, e + 0x10)) * d(k) + d(height)) as f32;
            sum += k;
        }
        if !(super::BASE..super::END).contains(&id) {
            self.tables.insert(id, t);
        }
        self.set_f32(side + 0x60, (d(self.f32(filter + 0x40)) + d(yaw) / d(sum)) as f32);
        self.set_f32(side + 0x64, pitch / sum);
        self.set_f32(side + 0x68, dist / sum);
        self.set_f32(side + 0x6c, height / sum);
    }

    /// Heading = the filter's plus the average facing of up to two workers (the other
    /// side's filter names them); pitch and distance from the filter.
    pub(super) fn set_act2(&mut self, env: &dyn CamEnv, side: u32, filter: u32, other_filter: u32) {
        let mut sum = 0.0f32;
        let mut n = 0;
        for k in 0..2 {
            let w = self.i16(other_filter + 0xc + 2 * k);
            if w != -1 {
                sum = (d(env.worker_yaw(i32::from(w))) + d(sum)) as f32;
                n += 1;
            }
        }
        if n > 0 {
            sum /= n as f32;
        }
        self.set_f32(side + 0x6c, 0.0);
        self.set_f32(side + 0x60, self.f32(filter + 0x40) + sum);
        let v = self.f32(filter + 0x44);
        self.set_f32(side + 0x64, v);
        let v = self.f32(filter + 0x48);
        self.set_f32(side + 0x68, v);
    }

    /// A path step. The place is clamped to 0..1; the point there (mode +0x1c: low
    /// bits 3 cubic; bit 2 looks ahead along the direction, back when the step is negative) is the target and
    /// its fourth value the roll; the place moves on by the step; the side is fixed there when the place was
    /// clamped or the step is zero.
    pub(super) fn path_tick(&mut self, handle: u32, side_index: u32, side: u32, filter: u32) {
        let mut t = self.f32(filter + 0x68);
        let below = t < 0.0;
        if below {
            t = 0.0;
        }
        let above = 1.0 < t;
        if above {
            t = 1.0;
        }
        self.set_f32(filter + 0x68, t);
        let mode = self.u8(filter + 0x1c);
        let cubic = mode & 3 == 3;
        let data = self.filter_data(self.u32(filter + 8)).unwrap_or_default();
        let pts = Pts(&data);
        // The point is a local the evaluation may leave as it was (the original's uninitialised stack).
        let mut p = [0.0f32; 4];
        let e = spline::eval(&mut p, pts, cubic, t, true);
        if let Some(w) = e.vu_w {
            self.vu_w = w;
        }
        if mode & 4 != 0 {
            let mut dir = [0.0f32; 4];
            let e = spline::eval(&mut dir, pts, cubic, t, false);
            if let Some(w) = e.vu_w {
                self.vu_w = w;
            }
            let step = self.f32(filter + 0x6c);
            // The sum and difference are VU operations: their fourth lane is the register's, as the
            // evaluations left it.
            p = if !step.is_nan() && step >= 0.0 {
                [p[0] + dir[0], p[1] + dir[1], p[2] + dir[2], self.vu_w]
            } else {
                [p[0] - dir[0], p[1] - dir[1], p[2] - dir[2], self.vu_w]
            };
        }
        self.set_vec4(side + 0x50, p);
        self.set_roll(handle, p[3]);
        self.set_f32(side + 0x6c, 0.0);
        let step = self.f32(filter + 0x6c);
        self.set_f32(filter + 0x68, step + t);
        if below || above || step == 0.0 {
            let v = self.vec4(side + 0x50);
            self.set_vec(handle, side_index, v);
        }
    }

    /// The eye on its own path at the focus's place along its rail (the other filter's +0x24).
    pub(super) fn eye_on_path(&mut self, side: u32, filter: u32, other_filter: u32) {
        let mode = self.u8(filter + 0x1c);
        let data = self.filter_data(self.u32(filter + 8)).unwrap_or_default();
        let t = self.f32(other_filter + 0x24);
        let mut p = [0.0f32; 4];
        let e = spline::eval(&mut p, Pts(&data), mode & 3 == 3, t, true);
        if let Some(w) = e.vu_w {
            self.vu_w = w;
        }
        self.set_f32(side + 0x6c, 0.0);
        self.set_vec4(side + 0x50, p);
    }

    /// Each rail entry starts its search again from the
    /// target and takes its place.
    fn move_acc(&mut self, side: u32, filter: u32) {
        if self.u8(filter + 4) == 0 {
            return;
        }
        let mut k = 0u32;
        loop {
            let ty = self.i32(filter + 0x90 + k * 0x20);
            if ty == 4 || ty == 5 {
                if self.u8(filter) == 1 {
                    let anchor = self.u32(side + 0x8c);
                    let p = self.vec3(side + 0x60);
                    self.polar_to(side + 0x50, anchor, p);
                }
                self.set_u8(filter + 0x1d, 0);
                let data = self.filter_data(self.u32(filter + 8)).unwrap_or_default();
                let q = self.vec3(side + 0x50);
                self.rail_start(Pts(&data), q, self.u8(filter + 0x1e) != 0);
                let (place, _) = self.rail_nearest(Pts(&data), q, ty != 4);
                self.set_f32(filter + 0x24, place);
            }
            k += 1;
            if k >= u32::from(self.u8(filter + 4)) {
                break;
            }
        }
    }

    /// From the change check: a rail's search starts again from the target.
    pub(super) fn rail_restart(&mut self, filter: u32, q: [f32; 3]) {
        let data = self.filter_data(self.u32(filter + 8)).unwrap_or_default();
        self.rail_start(Pts(&data), q, self.u8(filter + 0x1e) != 0);
    }

    /// The filter's limit entries on the target (lines on the ground, heights, rails); returns how many
    /// applied.
    pub(super) fn limit_lines(&mut self, side: u32, filter: u32) -> u32 {
        let mut applied = 0;
        if self.u8(filter + 4) == 0 {
            return 0;
        }
        let mut k = 0u32;
        loop {
            let e = filter + k * 0x20;
            let ty = self.i32(e + 0x90);
            let c = self.f32(e + 0x94);
            let len2 = self.f32(e + 0x98);
            let kk = self.f32(e + 0x9c);
            let dx = self.f32(e + 0xa0);
            let dz = self.f32(e + 0xa4);
            let a = self.f32(e + 0xa8);
            let b = self.f32(e + 0xac);
            match ty {
                0 | 2 => {
                    let (px, pz) = (self.f32(side + 0x50), self.f32(side + 0x58));
                    let t = if len2 == 0.0 {
                        0.0
                    } else {
                        ((d(dx) * d(px) + d(pz) * d(dz) + d(c)) / d(len2)) as f32
                    };
                    let mut nx = (d(dx) * d(t) + d(a)) as f32;
                    let mut nz = (d(t) * d(dz) + d(b)) as f32;
                    let ox = px - nx;
                    let oz = pz - nz;
                    if ty == 0 {
                        let dist = (d(ox) * d(ox) + d(oz) * d(oz)).sqrt() as f32;
                        let s = if kk == 0.0 { 0.0 } else { (d(dist) * d(kk)).atan() as f32 / kk };
                        if dist != 0.0 {
                            nx = (d(s) * d(ox) / d(dist) + d(nx)) as f32;
                            nz = (d(s) * d(oz) / d(dist) + d(nz)) as f32;
                        }
                    } else {
                        nx = (d(kk) * d(ox) + d(nx)) as f32;
                        nz = (d(kk) * d(oz) + d(nz)) as f32;
                    }
                    // The side of the line (the cross product's y below zero).
                    if ((d(dz) * d(ox) - d(dx) * d(oz)) as f32) < 0.0 {
                        applied += 1;
                        self.set_f32(side + 0x50, nx);
                        self.set_f32(side + 0x58, nz);
                    }
                }
                1 => {
                    let below = a - self.f32(side + 0x54);
                    if below > 0.0 {
                        let s = if kk == 0.0 { 0.0 } else { (d(below) * d(kk)).atan() as f32 / kk };
                        applied += 1;
                        self.set_f32(side + 0x54, a - s);
                    }
                    let over = self.f32(side + 0x54) - b;
                    if over > 0.0 {
                        let s = if kk == 0.0 { 0.0 } else { (d(over) * d(kk)).atan() as f32 / kk };
                        applied += 1;
                        self.set_f32(side + 0x54, b + s);
                    }
                }
                3 => {
                    let over = self.f32(side + 0x54) - a;
                    if over > 0.0 {
                        applied += 1;
                        self.set_f32(side + 0x54, (d(over) * d(kk) + d(a)) as f32);
                    }
                    let under = self.f32(side + 0x54) - b;
                    if under < 0.0 {
                        applied += 1;
                        self.set_f32(side + 0x54, (d(under) * d(kk) + d(b)) as f32);
                    }
                }
                4 | 5 => {
                    let data = self.filter_data(self.u32(filter + 8)).unwrap_or_default();
                    let q = self.vec3(side + 0x50);
                    let (place, near) = self.rail_nearest(Pts(&data), q, ty == 5);
                    self.set_f32(filter + 0x24, place);
                    let k = self.f32(filter + 0x20);
                    let off = [(q[0] - near[0]) * k, (q[1] - near[1]) * k, (q[2] - near[2]) * k];
                    self.set_vec3(side + 0x50, [near[0] + off[0], near[1] + off[1], near[2] + off[2]]);
                    applied += 1;
                }
                _ => {}
            }
            k += 1;
            if k >= u32::from(self.u8(filter + 4)) {
                break;
            }
        }
        applied
    }

    /// Loop 2 for one side: the timed moves (by angles around the anchor, or along the axes), the acceleration
    /// limit, the easing, the output, the shakes and the view point.
    fn output_side(&mut self, env: &mut dyn CamEnv, rec: u32, sub: u32, side: u32, filter: u32) {
        let anchor = self.u32(side + 0x8c);
        let mut c = self.i8(filter + 1);
        let by = self.i8(side + 1);
        let ch = side + super::set::CHANNELS;
        let mut n;
        if by == 1 {
            if c == 0 {
                let p = self.polar_of(side + 0x50, anchor);
                self.set_vec3(side + 0x60, p);
            }
            n = self.channel_step(self.i8(side + 4) != 0, side + 0x60, self.f32(side + 0x80), ch) as i32;
            n += self.channel_step(self.i8(side + 5) != 0, side + 0x64, self.f32(side + 0x84), ch + 0x10)
                as i32;
            n += self.channel_step(false, side + 0x68, self.f32(side + 0x88), ch + 0x20) as i32;
        } else {
            if c == 1 {
                let p = self.vec3(side + 0x60);
                self.polar_to(side + 0x50, anchor, p);
            }
            n = self.channel_step(false, side + 0x50, self.f32(side + 0x70), ch) as i32;
            n += self.channel_step(false, side + 0x54, self.f32(side + 0x74), ch + 0x10) as i32;
            n += self.channel_step(false, side + 0x58, self.f32(side + 0x78), ch + 0x20) as i32;
        }
        if n != 0 {
            c = by;
        }
        if c == 1 {
            let p = self.vec3(side + 0x60);
            self.polar_to(side + 0x50, anchor, p);
        }
        if self.u8(side + 3) != 0 {
            // The step towards the target, then the sum, its length and the cap. The sums'
            // fourth lanes are the VU register's (set from the caller's stack here); nothing reads a side's.
            let step = move_target_step(self.vec3(side + 0x50), self.vec3(side + 0x20), self.f32(side + 0xc));
            let acc = self.vec3(side + 0x40);
            let mut acc = [acc[0] + step[0], acc[1] + step[1], acc[2] + step[2]];
            let len = vec_len(acc);
            let cap = self.f32(side + 8);
            if cap < len {
                acc = vec_normalize(acc).map(|v| cap * v);
            }
            self.set_vec3(side + 0x40, acc);
            let out = self.vec3(side + 0x20);
            self.set_vec3(side + 0x50, [out[0] + acc[0], out[1] + acc[1], out[2] + acc[2]]);
        }
        if i32::from(self.i8(rec + 0xc8c)) == sub as i32 {
            for k in 0..3 {
                let div = self.f32(filter + 0x70 + 4 * k);
                if div != 0.0 {
                    let o = self.f32(side + 0x20 + 4 * k);
                    let v = ((d(self.f32(side + 0x50 + 4 * k)) - d(o)) / d(div) + d(o)) as f32;
                    self.set_f32(side + 0x50 + 4 * k, v);
                    if v != o {
                        self.set_u8(side + 2, 1);
                    }
                }
            }
        }
        let t = self.vec4(side + 0x50);
        self.set_vec4(side + 0x20, t);
        self.set_vec4(filter + 0x80, t);
        for k in 0..3 {
            let sh = side + 0x5b0 + k * 0x24;
            if self.u8(sh) != 0 {
                let phase = self.f32(side + 0xa0 + 4 * k);
                self.shake_step(env, sh, phase, side + 0x90 + 4 * k);
                let v = self.f32(side + 0xb0 + 4 * k) + phase;
                self.set_f32(side + 0xa0 + 4 * k, v);
            }
        }
        let out = self.vec3(side + 0x20);
        let off = self.vec3(side + 0x90);
        self.set_vec3(side + 0x10, [out[0] + off[0], out[1] + off[1], out[2] + off[2]]);
        if self.u8(rec + 0xc8a) != 0 {
            // The view point scaled about the record's point (+0x10) by its scale (+0x20). Its setter is
            // referenced nowhere in the executable, so this never runs.
            for k in 0..4 {
                let p = self.f32(rec + 0x10 + 4 * k);
                let v = self.f32(side + 0x10 + 4 * k);
                self.set_f32(side + 0x10 + 4 * k, (d(self.f32(rec + 0x20 + 4 * k)) * d(v - p) + d(p)) as f32);
            }
        }
    }

    /// The roll and screen distance of a frame: the active sub-camera's targets through their channels, the roll
    /// eased by the eye filter's divisor (+0x7c) when the sub-camera stayed; the outputs and the screen slot's
    /// distance.
    fn roll_and_distance(&mut self, env: &dyn CamEnv, rec: u32, sub: u32, screen: u32) {
        self.set_u8(rec + 0xc8b, 1);
        let v = self.f32(rec + 0xcb8 + sub * 0x30);
        self.set_f32(rec + 0xc94, v);
        let v = self.f32(rec + 0xcc0 + sub * 0x30);
        self.set_f32(rec + 0xca0, v);
        for k in 0..3 {
            let v = self.f32(rec + 0xcc8 + sub * 0x30 + 4 * k);
            self.set_f32(rec + 0xcac + 4 * k, v);
        }
        let wrap = self.i8(rec + 0xc8b) != 0;
        self.channel_step(wrap, rec + 0xc94, self.f32(rec + 0xc98), rec + 0xd78);
        self.channel_step(false, rec + 0xca0, self.f32(rec + 0xca4), rec + 0xd88);
        let eye_filter = rec + 0x30 + SIDE + 0xc0 + sub * FILTER;
        let div = self.f32(eye_filter + 0x7c);
        if i32::from(self.i8(rec + 0xc8c)) == sub as i32 && div != 0.0 {
            let out = self.f32(rec + 0xc90);
            let near = angle_near(out, self.f32(rec + 0xc94));
            let v = ((d(near) - d(out)) / d(div) + d(out)) as f32;
            self.set_f32(rec + 0xc94, v);
            if v != out {
                self.set_u8(rec + 0x30 + SIDE + 2, 1);
            }
        }
        let roll = self.f32(rec + 0xc94);
        self.set_f32(rec + 0xc90, roll);
        self.set_f32(rec + 0xcd4 + sub * 0x30, roll);
        if let Some(v) = env.distance_override() {
            self.set_f32(rec + 0xca0, v);
        } else if env.battle_override() {
            let v = self.f32(BATTLE_DISTANCE);
            self.set_f32(rec + 0xca0, v);
        }
        let dist = self.f32(rec + 0xca0);
        self.set_f32(rec + 0xc9c, dist);
        self.set_f32(rec + 0xcd8 + sub * 0x30, dist);
        self.set_u8(rec + 0xc8c, sub as u8);
        self.set_f32(screen + 0x1c, dist);
    }

    /// One channel step on a value in the island (see camera_move::Channel).
    pub(super) fn channel_step(&mut self, angle: bool, value: u32, start: f32, ch: u32) -> bool {
        let mut c = crate::camera_move::Channel {
            mode: self.i16(ch),
            lead: self.i16(ch + 4),
            accel: self.i16(ch + 6),
            steady: self.i16(ch + 8),
            decel: self.i16(ch + 0xa),
            frames: self.i16(ch + 0xc),
            count: self.i16(ch + 0xe),
        };
        let mut v = self.f32(value);
        let ran = c.step(angle, &mut v, start);
        self.set_f32(value, v);
        self.set_i16(ch + 0xc, c.frames);
        self.set_i16(ch + 0xe, c.count);
        ran
    }

    /// One shake channel's offset this frame (channel: +0 mode, +1 jitter span, +2 amplitude, +4 frames
    /// left, +6 frames, +8 the held value, +0x10 a spring's value, speed and pull, +0x20 its steps a frame, +0x21
    /// its fade).
    pub(super) fn shake_step(&mut self, env: &mut dyn CamEnv, ch: u32, phase: f32, out: u32) {
        let mode = self.u8(ch);
        if mode == 0 {
            return;
        }
        let mut j = 0i32;
        let span = i32::from(self.u8(ch + 1));
        if span != 0 {
            j = (span >> 1) - env.rng9() % span;
        }
        let amp = f64::from(self.u16(ch + 2));
        let jf = d((j + 0x20) as f32);
        let frames = self.i16(ch + 6);
        let sin = (d(phase).sin()) as f32;
        let cos = |k: f64| ((d(phase) + k).cos()) as f32;
        // Frames left down; past the end the channel stops (modes 7–9 only when it has a length).
        let tail = |m: &mut CamMem, need_len: bool| {
            let l = m.i16(ch + 4).wrapping_sub(1);
            m.set_i16(ch + 4, l);
            if l < 0 && (!need_len || frames > 0) {
                m.set_u8(ch, 0);
                m.set_f32(out, 0.0);
            }
        };
        match mode {
            1..=4 => {
                if frames == 0 {
                    return;
                }
                let base = if mode <= 2 { d(sin) } else { (d(sin) + d(cos(0.3))) * 0.5 };
                let a = (base * amp * jf * 0.03125) as f32;
                let l = self.i16(ch + 4);
                let by =
                    if mode % 2 == 1 { f64::from(l) } else { d((i32::from(frames) - i32::from(l)) as f32) };
                self.set_f32(out, (d(a) * by / f64::from(frames)) as f32);
                tail(self, false);
            }
            5 | 6 => {
                let r = env.rng9() & 0xf;
                let sc = d(sin) + d(cos(0.1));
                if frames == 0 {
                    return;
                }
                let l = self.i16(ch + 4);
                let a = if l & 1 != 0 || r == 0 {
                    (0.5 * d(self.f32(ch + 8))) as f32
                } else {
                    (sc * 0.5 * amp * jf * 0.03125) as f32
                };
                let by = if mode == 5 { f64::from(l) } else { d((i32::from(frames) - i32::from(l)) as f32) };
                self.set_f32(out, (d(a) * by / f64::from(frames)) as f32);
                self.set_f32(ch + 8, a);
                tail(self, false);
            }
            7 | 8 => {
                if frames == 0 {
                    return;
                }
                let base = if mode == 7 { d(sin) } else { (d(sin) + d(cos(0.1))) * 0.5 };
                self.set_f32(out, (base * amp * jf * 0.03125) as f32);
                tail(self, true);
            }
            9 => {
                if frames == 0 {
                    return;
                }
                for _ in 0..self.u8(ch + 0x20) {
                    let v = self.f32(ch + 0x14) + self.f32(ch + 0x10);
                    self.set_f32(ch + 0x10, v);
                    let mut p = self.f32(ch + 0x18);
                    if v > 0.0 {
                        p = -p;
                    }
                    self.set_f32(ch + 0x14, p + self.f32(ch + 0x14));
                }
                let v = self.f32(ch + 0x10);
                let l = self.i16(ch + 4);
                let v = match self.u8(ch + 0x21) {
                    1 => (f64::from(l) * d(v) / f64::from(frames)) as f32,
                    2 => (d((i32::from(frames) - i32::from(l)) as f32) * d(v) / f64::from(frames)) as f32,
                    _ => v,
                };
                self.set_f32(out, v);
                tail(self, true);
            }
            _ => {}
        }
    }
}

/// The step towards the target at `speed` (the direction normalised; zero length gives
/// zero).
pub(super) fn move_target_step(target: [f32; 3], from: [f32; 3], speed: f32) -> [f32; 3] {
    let dv = [target[0] - from[0], target[1] - from[1], target[2] - from[2]];
    let q = inv_len(dv);
    dv.map(|v| v * q * speed)
}

/// The VU length's 1 / sqrt (0 for a zero vector): squares and sums each rounded ((y² + x²) + z²).
fn inv_len(v: [f32; 3]) -> f32 {
    let l2 = (v[1] * v[1] + v[0] * v[0]) + v[2] * v[2];
    if l2.abs() == 0.0 { 0.0 } else { 1.0 / l2.abs().sqrt() }
}

/// A vector's length.
fn vec_len(v: [f32; 3]) -> f32 {
    ((v[1] * v[1] + v[0] * v[0]) + v[2] * v[2]).abs().sqrt()
}

/// A vector normalised (zero stays zero).
fn vec_normalize(v: [f32; 3]) -> [f32; 3] {
    let q = inv_len(v);
    v.map(|x| q * x)
}
