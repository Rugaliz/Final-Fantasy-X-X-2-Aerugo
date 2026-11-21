//! Character filters (types 0xb–0xd, 0xf, 0x10): a side follows up to two characters —
//! event workers for most handles, battle characters for kind 5 — at their average position, lowered by a share
//! of their height; with a variant the side is placed around that point at a heading measured between the two
//! characters (or between the sides), a pitch and a distance from their size.

use super::CamMem;
use super::polar;
use super::process::CamEnv;

/// An f32 widened for an x87 expression.
fn d(x: f32) -> f64 {
    x as f64
}

impl CamMem {
    /// A character query. Event handles (code bit 0x100) ask for worker `id` (−0x15
    /// and −5: every shown worker; −2: the running one); battle handles sum over the characters of `group`.
    /// Returns how many answered; `out` is written only then.
    pub(super) fn chr_info(env: &dyn CamEnv, code: u32, group: u32, id: i32, out: &mut [f32; 4]) -> i32 {
        if code & 0x100 == 0 {
            *out = [0.0; 4];
            return env.battle_chr_info(code & 0xff, group, id, out);
        }
        let what = code & 0xff;
        let count = env.worker_count();
        let mut acc = [0.0f32; 3];
        let n = if id == -0x15 || id == -5 {
            if count < 1 {
                return 0;
            }
            let mut k = 0;
            for i in 0..count {
                if env.worker_shown(i) {
                    chr_value(env, i, what, &mut acc);
                    k += 1;
                }
            }
            if k < 1 {
                return 0;
            }
            k
        } else {
            let id = if id == -2 { env.current_worker() } else { id };
            if id < 0 || count <= id {
                return 0;
            }
            chr_value(env, id, what, &mut acc);
            1
        };
        let f = n as f32;
        *out = [acc[0] / f, acc[1] / f, acc[2] / f, 0.0];
        1
    }

    /// On a side (`variant` 0–4 for types 0xb, 0xc, 0xd, 0xf, 0x10).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn chr_filter(
        &mut self,
        env: &dyn CamEnv,
        handle: u32,
        variant: u32,
        side: u32,
        filter: u32,
        other: u32,
        other_filter: u32,
    ) {
        if self.i8(other_filter) == 1 {
            let anchor = self.u32(other + 0x8c);
            let p = self.vec3(other + 0x60);
            self.polar_to(other + 0x50, anchor, p);
        }
        let ev = if handle & 0xf_0000 != 0x5_0000 { 0x100 } else { 0 };
        let mut sum = [0.0f32; 3];
        let (mut height, mut yaw, mut size) = (0.0f32, 0.0f32, 0.0f32);
        let mut count = 0;
        // The characters' positions are locals; one not found keeps what was there (only read when both are).
        let mut pos = [[0.0f32; 4]; 2];
        for k in 0..2u32 {
            let id = i32::from(self.i16(filter + 0x10 + 2 * k));
            let group = self.u32(filter + 0x14 + 4 * k);
            if id == -1 {
                continue;
            }
            if Self::chr_info(env, ev | 9, group, id, &mut pos[k as usize]) > 0 {
                count += 1;
                let p = pos[k as usize];
                sum = [sum[0] + p[0], p[1] + sum[1], p[2] + sum[2]];
                let mut v = [0.0f32; 4];
                Self::chr_info(env, ev | 10, group, id, &mut v);
                height += v[0];
                Self::chr_info(env, ev | 7, group, id, &mut v);
                yaw += v[0];
                Self::chr_info(env, ev | 0xb, group, id, &mut v);
                size += v[0];
            }
        }
        if count < 1 {
            let v = self.vec3(filter + 0x30);
            self.set_vec3(side + 0x50, v);
            self.set_f32(side + 0x6c, 0.0);
            return;
        }
        let c = count as f32;
        sum = sum.map(|v| v / c);
        height /= c;
        yaw /= c;
        size /= c;
        self.set_vec3(side + 0x50, sum);
        self.set_f32(side + 0x54, (d(sum[1]) - d(self.f32(filter + 0x5c)) * d(height)) as f32);
        if variant != 0 {
            let mut iv = -1;
            if count >= 2 && self.u32(filter + 0x14) != self.u32(filter + 0x18) {
                let a = [pos[1][0], pos[1][1], pos[1][2]];
                let p = [pos[0][0], pos[0][1], pos[0][2]];
                yaw = polar::measure(p, Some(a))[0];
                iv = 0;
            } else if self.u8(other_filter + 2) == 0xc
                && (self.u32(filter + 0x18) | self.u32(filter + 0x14))
                    == (self.u32(other_filter + 0x18) | self.u32(other_filter + 0x14))
            {
                iv = 1;
            }
            let measure = ev != 0
                && iv < 0
                && self.u8(side) != 0
                && (self.f32(side + 0x50) != self.f32(other + 0x50)
                    || self.f32(side + 0x58) != self.f32(other + 0x58));
            if measure {
                yaw = self.polar_of(side + 0x50, other + 0x50)[0];
            } else if iv != 0 && ev != 0 {
                if self.u8(filter + 6) == 0 {
                    yaw = self.f32(filter + 0x64);
                } else {
                    self.set_u8(filter + 6, 0);
                    self.set_f32(filter + 0x64, yaw);
                }
            }
            size = if variant == 2 || variant == 4 {
                (d(self.f32(filter + 0x60)) * 10.0 + d(size)) as f32
            } else {
                self.f32(filter + 0x60) * size
            };
            if self.u8(filter) == 0 {
                let h = self.f32(filter + 0x40) + yaw;
                let off = polar::place(None, h, self.f32(filter + 0x44), size);
                for k in 0..3 {
                    let v = self.f32(side + 0x50 + 4 * k) + off[k as usize];
                    self.set_f32(side + 0x50 + 4 * k, v);
                }
            } else {
                let h = self.f32(filter + 0x40) + yaw;
                self.set_f32(side + 0x60, h);
                let p = self.f32(filter + 0x44);
                self.set_f32(side + 0x64, p);
                self.set_f32(side + 0x68, size);
                let anchor = self.u32(side + 0x8c);
                self.polar_to(side + 0x50, anchor, [h, p, size]);
            }
        }
        let v = self.vec3(side + 0x50);
        self.set_vec3(filter + 0x30, v);
        self.set_f32(side + 0x6c, 0.0);
    }
}

/// One worker's value added to `acc` (1, 8, 9: position; 2: facing; 3, 10: 18; 4, 0xb: 10; 7: facing plus
/// a half turn). Only the first lane of a single value is read by the camera.
fn chr_value(env: &dyn CamEnv, w: i32, what: u32, acc: &mut [f32; 3]) {
    let v = match what {
        1 | 8 | 9 => {
            let p = env.worker_pos(w);
            *acc = [acc[0] + p[0], acc[1] + p[1], p[2] + acc[2]];
            return;
        }
        2 => env.worker_yaw(w),
        3 | 10 => 18.0,
        4 | 0xb => 10.0,
        7 => (d(env.worker_yaw(w)) + d(std::f32::consts::PI)) as f32,
        _ => return,
    };
    acc[0] += v;
}
