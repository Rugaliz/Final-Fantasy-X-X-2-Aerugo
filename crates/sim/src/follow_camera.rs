//! The field camera that follows the player: the focus sits on followed workers (plus an offset), is pulled back
//! towards limit lines and kept in a height range, and eases after its target; the eye hangs around the focus,
//! on the far side of an anchor point, higher and farther the farther the focus is from that anchor.

/// A limit on the focus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Limit {
    /// On one side of the line from `start` along `dir` (ground plane), the focus keeps only `k` of its distance
    /// from the line.
    Line { start: [f32; 2], dir: [f32; 2], k: f32 },
    /// Above `max` or below `min` (height), the focus keeps only `k` of what it is past the bound.
    Height { min: f32, max: f32, k: f32 },
}

impl Limit {
    fn apply(&self, p: &mut [f32; 3]) {
        match *self {
            Limit::Line { start, dir, k } => {
                let len2 = dir[0] * dir[0] + dir[1] * dir[1];
                let c = -(start[0] * dir[0] + start[1] * dir[1]);
                let t = if len2 != 0.0 { (p[2] * dir[1] + dir[0] * p[0] + c) / len2 } else { 0.0 };
                let on = [start[0] + t * dir[0], start[1] + t * dir[1]];
                let d = [p[0] - on[0], p[2] - on[1]];
                // Only on the limited side.
                if dir[1] * d[0] - dir[0] * d[1] < 0.0 {
                    p[0] = on[0] + k * d[0];
                    p[2] = on[1] + k * d[1];
                }
            }
            Limit::Height { min, max, k } => {
                if p[1] - max > 0.0 {
                    p[1] = (p[1] - max) * k + max;
                }
                if p[1] - min < 0.0 {
                    p[1] = (p[1] - min) * k + min;
                }
            }
        }
    }
}

/// The eye around the focus: its heading is from the focus towards `anchor` (ground plane); with `d` the ground
/// distance from the focus to the anchor, the pitch is `pitch + d / pitch_div` and the distance
/// `dist + d / dist_div` (a zero divisor leaves that part out).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Orbit {
    pub anchor: [f32; 2],
    pub pitch: f32,
    pub pitch_div: f32,
    pub dist: f32,
    pub dist_div: f32,
}

/// Most limits a camera part keeps.
const MAX_LIMITS: usize = 5;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FollowCamera {
    /// Workers whose average position the focus follows (up to two), and the offset added.
    pub follow: Option<([Option<usize>; 2], [f32; 3])>,
    pub limits: [Option<Limit>; MAX_LIMITS],
    /// Per axis, the focus covers 1 / divisor of the way to its target each tick (0: at once).
    pub ease: [f32; 3],
    pub orbit: Option<Orbit>,
    /// The focus as eased so far.
    focus: Option<[f32; 3]>,
}

impl FollowCamera {
    pub fn add_limit(&mut self, limit: Limit) {
        if let Some(slot) = self.limits.iter_mut().find(|l| l.is_none()) {
            *slot = Some(limit);
        }
    }

    pub fn clear_limits(&mut self) {
        self.limits = [None; MAX_LIMITS];
    }

    /// One tick: the focus and (with an orbit) the eye for followed workers at `positions`, or None while not
    /// following.
    pub fn update(
        &mut self,
        positions: impl Fn(usize) -> Option<[f32; 3]>,
    ) -> Option<([f32; 3], Option<[f32; 3]>)> {
        let (workers, offset) = self.follow?;
        let found: Vec<[f32; 3]> = workers.iter().flatten().filter_map(|&w| positions(w)).collect();
        if found.is_empty() {
            return None;
        }
        let n = found.len() as f32;
        let mut target: [f32; 3] =
            std::array::from_fn(|k| found.iter().map(|p| p[k]).sum::<f32>() / n + offset[k]);
        for l in self.limits.iter().flatten() {
            l.apply(&mut target);
        }
        let focus = match self.focus {
            Some(old) => std::array::from_fn(|k| {
                let d = self.ease[k];
                if d != 0.0 { old[k] + (target[k] - old[k]) / d } else { target[k] }
            }),
            None => target,
        };
        self.focus = Some(focus);
        let eye = self.orbit.map(|o| {
            let (dx, dz) = (o.anchor[0] - focus[0], o.anchor[1] - focus[2]);
            let d = (dx * dx + dz * dz).sqrt();
            let yaw = dz.atan2(dx);
            let pitch = o.pitch + if o.pitch_div != 0.0 { d / o.pitch_div } else { 0.0 };
            let dist = o.dist + if o.dist_div != 0.0 { d / o.dist_div } else { 0.0 };
            [
                focus[0] + dist * pitch.cos() * yaw.cos(),
                focus[1] + dist * pitch.sin(),
                focus[2] + dist * pitch.cos() * yaw.sin(),
            ]
        });
        Some((focus, eye))
    }

    /// Stop following (the scripts take the camera back).
    pub fn stop(&mut self) {
        self.follow = None;
        self.focus = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_focus_eases_after_the_followed_worker() {
        let mut c = FollowCamera {
            follow: Some(([Some(0), None], [0.0, -10.0, 0.0])),
            ease: [2.0, 0.0, 2.0],
            ..FollowCamera::default()
        };
        let (f, _) = c.update(|_| Some([0.0, 0.0, 0.0])).unwrap();
        assert_eq!(f, [0.0, -10.0, 0.0]);
        let (f, _) = c.update(|_| Some([10.0, 0.0, 0.0])).unwrap();
        assert_eq!(f, [5.0, -10.0, 0.0]);
    }

    #[test]
    fn a_line_limit_pulls_back_on_one_side_only() {
        let l = Limit::Line { start: [0.0, 0.0], dir: [1.0, 0.0], k: 0.5 };
        let (mut a, mut b) = ([3.0, 0.0, 4.0], [3.0, 0.0, -4.0]);
        l.apply(&mut a);
        l.apply(&mut b);
        // One of the two sides is pulled half way back to the line; the other is left alone.
        assert!(
            (a == [3.0, 0.0, 2.0] && b == [3.0, 0.0, -4.0])
                || (a == [3.0, 0.0, 4.0] && b == [3.0, 0.0, -2.0])
        );
    }

    #[test]
    fn the_eye_sits_beyond_the_focus_away_from_the_anchor_side() {
        let mut c = FollowCamera {
            follow: Some(([Some(0), None], [0.0; 3])),
            orbit: Some(Orbit { anchor: [10.0, 0.0], pitch: 0.0, pitch_div: 0.0, dist: 5.0, dist_div: 0.0 }),
            ..FollowCamera::default()
        };
        let eye = c.update(|_| Some([0.0; 3])).unwrap().1.unwrap();
        assert!((eye[0] - 5.0).abs() < 1e-5 && eye[2].abs() < 1e-5, "{eye:?}");
    }
}
