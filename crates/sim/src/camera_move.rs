//! Timed moves of the script camera: each of the eye's and the focus's three values (x, y, z, or heading, pitch
//! and distance from the other one) and the roll and the screen distance move from where a move was timed to
//! wherever the scripts set them, over a number of frames, along a profile.
//!
//! A move is timed before or after the place is set: the start is taken from the place as it is when the move
//! time is set, and each tick the value is placed between that start and the latest target. Without a running
//! move a set takes effect at once.

/// Pi and a full turn as the original's single-precision constants (held in doubles).
const HALF_TURN: f64 = 3.1415927410125732;
const TURN: f64 = 6.2831854820251465;

/// `b` moved by whole turns to within half a turn of `a` (to a − π .. a + π), with the original's float steps.
pub fn angle_near(a: f32, b: f32) -> f32 {
    let d = b - a;
    let turns = (d as f64 / TURN) as i32 as f32;
    let mut r = (d as f64 - turns as f64 * TURN) as f32;
    if r < 0.0 {
        r = (r as f64 + TURN) as f32;
    }
    if (r as f64) < HALF_TURN { (a as f64 + r as f64) as f32 } else { ((a as f64 - TURN) + r as f64) as f32 }
}

/// The point at a heading, pitch (radians) and distance from `from`.
pub fn polar_point(from: [f32; 3], [yaw, pitch, dist]: [f32; 3]) -> [f32; 3] {
    let (cp, sp) = (pitch.cos(), pitch.sin());
    [from[0] + dist * (cp * yaw.cos()), from[1] + dist * sp, from[2] + dist * (cp * yaw.sin())]
}

/// The heading, pitch and distance of `to` seen from `from` (the inverse of [`polar_point`]): the heading is
/// atan(dz / dx), less a half turn when dx is not positive (±quarter turn when dx is 0), the pitch atan of the
/// height over the ground distance.
pub fn to_polar(from: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    const QUARTER: f32 = 1.5707964;
    let [dx, dy, dz] = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let dist = (dz * dz + dy * dy + dx * dx).sqrt();
    let ground = (dz * dz + dx * dx).sqrt();
    let yaw = if dx == 0.0 {
        if dz < 0.0 { -QUARTER } else { QUARTER }
    } else if dx <= 0.0 {
        (dz / dx).atan() - std::f32::consts::PI
    } else {
        (dz / dx).atan()
    };
    let r = dy / ground;
    let pitch = if !r.is_finite() {
        if dy > 0.0 { QUARTER } else { -QUARTER }
    } else if ground <= 0.0 {
        r.atan() - std::f32::consts::PI
    } else {
        r.atan()
    };
    [yaw, pitch, dist]
}

/// One value's move: mode 1 linear, 2 speeding up, steady, slowing down, 3 along a half cosine. Idle when
/// `frames` is 0.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Channel {
    pub mode: i16,
    /// Mode 2: the move starts at the speed that would cover the whole way in this many frames (0: from rest).
    pub lead: i16,
    pub accel: i16,
    pub steady: i16,
    pub decel: i16,
    pub frames: i16,
    pub count: i16,
}

impl Channel {
    /// A move over `frames` frames in `mode` (the profile values are kept).
    pub fn set(&mut self, mode: i16, frames: i16) {
        self.mode = mode;
        self.frames = frames;
        self.count = 0;
    }

    /// A speeding up / steady / slowing down move (frames: the sum).
    pub fn set_profile(&mut self, accel: i16, steady: i16, decel: i16, lead: i16) {
        *self = Channel {
            mode: 2,
            lead,
            accel,
            steady,
            decel,
            frames: decel.wrapping_add(steady).wrapping_add(accel),
            count: 0,
        };
    }

    pub fn active(&self) -> bool {
        self.frames != 0
    }

    /// One tick: `value` comes in as the target and leaves as the moved value (an angle is first brought within
    /// half a turn of the start). Returns whether the channel ran.
    pub fn step(&mut self, angle: bool, value: &mut f32, start: f32) -> bool {
        if self.frames == 0 {
            return false;
        }
        let target = if angle { angle_near(start, *value) } else { *value };
        let (t, s) = (target as f64, start as f64);
        let (count, frames) = (self.count as f64, self.frames as f64);
        let moved = match self.mode {
            1 => Some(((t - s) * count / frames + s) as f32),
            3 => {
                let c = (count * HALF_TURN / frames).cos() as f32;
                Some(((1.0 - c as f64) * (t - s) * 0.5 + s) as f32)
            }
            2 => Some(self.profile(t, s)),
            _ => None,
        };
        // (Another mode leaves the value as it came.)
        if let Some(v) = moved {
            *value = v;
        }
        self.count = self.count.wrapping_add(1);
        if self.frames < self.count {
            self.frames = 0;
        }
        true
    }

    /// Mode 2: a constant acceleration over `accel` frames, a steady speed over `steady`, and the same
    /// deceleration over `decel`, sized to arrive at the target (counts in whole frames, as the original).
    fn profile(&self, t: f64, s: f64) -> f32 {
        let lead = if self.lead == 0 { 0.0 } else { (t - s) / self.lead as f64 };
        let (a, st, d) = (self.accel as i32, self.steady as i32, self.decel as i32);
        let span = ((a - d / 2) * d + (a / 2 + st) * a) as f64;
        let acc = if span == 0.0 { 0.0 } else { ((t - s) - self.frames as f64 * lead) / span };
        let gain = acc * a as f64;
        let after_accel = s + (gain * 0.5 + lead) * a as f64;
        let top = lead + gain;
        let c = self.count as i32;
        let v = if c < a {
            c as f64 * (acc * c as f64 * 0.5 + lead) + s
        } else if a + st < c {
            if c < a + st + d {
                let x = (c - st - a) as f64;
                x * (top - x * acc * 0.5) + st as f64 * top + after_accel
            } else {
                t
            }
        } else {
            (c - a) as f64 * top + after_accel
        };
        v as f32
    }
}

/// The moves of one camera part (the eye or the focus).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PartMove {
    /// Move by heading, pitch and distance around the other part instead of along x, y and z.
    pub by_angles: bool,
    pub channels: [Channel; 3],
    /// Where the move started: as a point, and as heading, pitch and distance from the other part.
    pub from: [f32; 3],
    pub from_polar: [f32; 3],
    /// The part's heading, pitch and distance from the other part as last set or moved.
    pub polar: [f32; 3],
}

impl PartMove {
    pub fn active(&self) -> bool {
        self.channels.iter().any(Channel::active)
    }

    /// Time the channels `mask` names (bits 0–2) over `frames` frames in `mode`.
    pub fn set(&mut self, by_angles: bool, mask: u8, mode: i16, frames: i16) {
        self.by_angles = by_angles;
        for (i, ch) in self.channels.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                ch.set(mode, frames);
            }
        }
    }

    /// Time the channels `mask` names with a speed profile.
    pub fn set_profile(&mut self, by_angles: bool, mask: u8, [accel, steady, decel, lead]: [i16; 4]) {
        self.by_angles = by_angles;
        for (i, ch) in self.channels.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                ch.set_profile(accel, steady, decel, lead);
            }
        }
    }

    /// Take the start of a move from where the part is (`at`, `anchor` the other part): from its heading,
    /// pitch and distance when the part is placed that way (`by_polar`), else from the point.
    pub fn start(&mut self, mask: u8, at: [f32; 3], anchor: [f32; 3], by_polar: bool) {
        if mask & 7 == 0 {
            return;
        }
        if by_polar {
            for (i, (from, now)) in self.from_polar.iter_mut().zip(self.polar).enumerate() {
                if mask & (1 << i) != 0 {
                    *from = now;
                }
            }
            self.from = polar_point(anchor, self.polar);
        } else {
            for (i, (from, at)) in self.from.iter_mut().zip(at).enumerate() {
                if mask & (1 << i) != 0 {
                    *from = at;
                }
            }
            self.from_polar = to_polar(anchor, at);
        }
    }

    /// One tick: the part's place this tick for its target (`polar` when it is placed by angles around
    /// `anchor`).
    pub fn apply(&mut self, target: [f32; 3], anchor: [f32; 3], polar: Option<[f32; 3]>) -> [f32; 3] {
        if let Some(p) = polar {
            self.polar = p;
        }
        let [a, b, c] = &mut self.channels;
        if self.by_angles {
            if polar.is_none() {
                self.polar = to_polar(anchor, target);
            }
            let [fy, fp, fd] = self.from_polar;
            let [y, p, d] = &mut self.polar;
            let moved = a.step(true, y, fy) | b.step(true, p, fp) | c.step(false, d, fd);
            if moved || polar.is_some() { polar_point(anchor, self.polar) } else { target }
        } else {
            let mut t = target;
            let [x, y, z] = &mut t;
            a.step(false, x, self.from[0]);
            b.step(false, y, self.from[1]);
            c.step(false, z, self.from[2]);
            t
        }
    }

    /// Stop every channel.
    pub fn stop(&mut self) {
        for ch in &mut self.channels {
            ch.frames = 0;
        }
    }
}

/// The move of a single value (the roll, an angle; the screen distance): the value as last set or moved, where
/// its move started, and its channel.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ValueMove {
    pub value: f32,
    pub from: f32,
    pub channel: Channel,
}

impl ValueMove {
    /// One tick towards `target`.
    pub fn apply(&mut self, target: f32, angle: bool) -> f32 {
        self.value = target;
        self.channel.step(angle, &mut self.value, self.from);
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_and_cosine_moves_reach_the_target_and_stop() {
        let mut ch = Channel::default();
        ch.set(1, 4);
        let mut seen = vec![];
        while ch.active() {
            let mut v = 8.0;
            ch.step(false, &mut v, 0.0);
            seen.push(v);
        }
        // Count 0..=4: the start on the first tick, the target on the fifth.
        assert_eq!(seen, [0.0, 2.0, 4.0, 6.0, 8.0]);
        ch.set(3, 2);
        let mut v = 10.0;
        ch.step(false, &mut v, 0.0);
        assert_eq!(v, 0.0);
        let mut v = 10.0;
        ch.step(false, &mut v, 0.0);
        assert!((v - 5.0).abs() < 1e-5);
    }

    #[test]
    fn a_speed_profile_speeds_up_holds_and_slows_down_to_the_target() {
        let mut ch = Channel::default();
        ch.set_profile(30, 480, 30, 0);
        assert_eq!(ch.frames, 540);
        let mut out = vec![];
        while ch.active() {
            let mut v = 153.0;
            ch.step(false, &mut v, 0.0);
            out.push(v);
        }
        assert_eq!(out.len(), 541);
        // Distance 153 over 30·30/2 + 480·30 + 30·30/2 = 15300 acceleration units: acceleration 0.01, top speed
        // 0.3.
        assert!((out[30] - 4.5).abs() < 1e-4);
        assert!((out[510] - (4.5 + 480.0 * 0.3)).abs() < 1e-3);
        assert!((out[539] - 153.0).abs() < 0.01);
        assert_eq!(out[540], 153.0);
        let steps: Vec<f32> = out.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(steps[100] > steps[5] && steps[100] > steps[535]);
    }

    #[test]
    fn angles_come_within_half_a_turn_and_polar_points_round_trip() {
        assert_eq!(angle_near(0.0, 0.0), 0.0);
        assert!((angle_near(0.0, 7.0) - (7.0 - 6.2831855)).abs() < 1e-5);
        assert!((angle_near(0.0, 4.0) - (4.0 - 6.2831855)).abs() < 1e-5);
        assert!((angle_near(0.0, -1.0) + 1.0).abs() < 1e-6);
        assert!((angle_near(0.0, -4.0) - (-4.0 + 6.2831855)).abs() < 1e-5);
        let from = [1.0, 2.0, 3.0];
        for p in [[0.3, 0.2, 10.0], [2.5, -0.4, 4.0], [-1.2, 0.1, 7.0]] {
            let q = to_polar(from, polar_point(from, p));
            assert!((angle_near(p[0], q[0]) - p[0]).abs() < 1e-5 && (q[1] - p[1]).abs() < 1e-5);
            assert!((q[2] - p[2]).abs() < 1e-4);
        }
    }

    #[test]
    fn a_part_moving_by_angles_keeps_its_distance_from_the_other() {
        let mut m = PartMove { polar: [0.0, 0.0, 10.0], ..Default::default() };
        m.start(7, [10.0, 0.0, 0.0], [0.0; 3], true);
        m.set(true, 7, 1, 2);
        let half = m.apply([0.0; 3], [0.0; 3], Some([1.0, 0.0, 10.0]));
        assert_eq!(half, [10.0, 0.0, 0.0]);
        let mid = m.apply([0.0; 3], [0.0; 3], Some([1.0, 0.0, 10.0]));
        assert!(((mid[0] * mid[0] + mid[2] * mid[2]).sqrt() - 10.0).abs() < 1e-4);
        assert!((mid[2].atan2(mid[0]) - 0.5).abs() < 1e-5);
    }
}
