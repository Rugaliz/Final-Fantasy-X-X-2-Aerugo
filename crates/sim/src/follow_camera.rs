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

/// A point of an eye table: near its position the eye takes its heading change, pitch, distance and height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathPoint {
    pub pos: [f32; 3],
    /// Divides the point's distance from the focus (a larger one makes the point count more far away).
    pub reach: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub height: f32,
}

/// The eye from a table: its heading is from the focus towards `anchor` plus `yaw`, and its heading change,
/// pitch, distance and height blend the table's points, nearer points weighing more (with `squared`, by the
/// square of the distance).
#[derive(Debug, Clone, PartialEq)]
pub struct EyePath {
    pub anchor: [f32; 2],
    pub yaw: f32,
    pub squared: bool,
    pub points: Vec<PathPoint>,
}

impl EyePath {
    /// Read a table: a count, then 0x30-byte records from +0x10 (position, reach, heading change, pitch,
    /// distance, a scratch word, height); byte 4 of the header asks for squared distances.
    pub fn read(data: &[u8], anchor: [f32; 2], yaw: f32) -> Option<EyePath> {
        let u = |at: usize| data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let f = |at: usize| u(at).map(f32::from_bits);
        let n = u(0)? as usize;
        let points = (0..n.min(64))
            .map(|i| {
                let r = 0x10 + i * 0x30;
                Some(PathPoint {
                    pos: [f(r)?, f(r + 4)?, f(r + 8)?],
                    reach: f(r + 0xC)?,
                    yaw: f(r + 0x10)?,
                    pitch: f(r + 0x14)?,
                    dist: f(r + 0x18)?,
                    height: f(r + 0x20)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(EyePath { anchor, yaw, squared: data.get(4).is_some_and(|b| b & 1 != 0), points })
    }

    /// The eye for a focus.
    fn eye(&self, focus: [f32; 3]) -> [f32; 3] {
        let weights: Vec<f32> = self
            .points
            .iter()
            .map(|p| {
                let d = [0, 2].iter().map(|&k| (p.pos[k] - focus[k]).powi(2)).sum::<f32>().sqrt() + 1.0;
                let d = if self.squared { d * d } else { d };
                if p.reach != 0.0 { d / p.reach } else { d }
            })
            .collect();
        let total: f32 = weights.iter().sum();
        let (mut sum, mut yaw, mut pitch, mut dist, mut height) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (p, w) in self.points.iter().zip(&weights) {
            let f = if *w != 0.0 { total / w } else { 0.0 };
            sum += f;
            yaw += f * p.yaw;
            pitch += f * p.pitch;
            dist += f * p.dist;
            height += f * p.height;
        }
        if sum == 0.0 {
            return focus;
        }
        let heading = (self.anchor[1] - focus[2]).atan2(self.anchor[0] - focus[0]) + self.yaw + yaw / sum;
        let (pitch, dist) = (pitch / sum, dist / sum);
        [
            focus[0] + dist * pitch.cos() * heading.cos(),
            focus[1] + dist * pitch.sin() + height / sum,
            focus[2] + dist * pitch.cos() * heading.sin(),
        ]
    }
}

/// A path of points the camera runs along: a quadratic B-spline, or a cubic one.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub points: Vec<[f32; 3]>,
    pub cubic: bool,
}

impl Path {
    /// Read a table: a count, then points of four floats from +0x10.
    pub fn read(data: &[u8], cubic: bool) -> Option<Path> {
        let f = |at: usize| data.get(at..at + 4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
        let n = data.get(0..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()))? as usize;
        let points = (0..n.min(256))
            .map(|i| Some([f(0x10 + i * 16)?, f(0x14 + i * 16)?, f(0x18 + i * 16)?]))
            .collect::<Option<Vec<_>>>()?;
        (!points.is_empty()).then_some(Path { points, cubic })
    }

    /// The point at `t` (0..1 along the whole path).
    pub fn at(&self, t: f32) -> [f32; 3] {
        let p = &self.points;
        let n = p.len();
        let t = t.clamp(0.0, 1.0);
        if n == 1 {
            return p[0];
        }
        if n == 2 {
            return std::array::from_fn(|k| (1.0 - t) * p[0][k] + t * p[1][k]);
        }
        let (order, segs) = if self.cubic && n > 3 { (4, n - 3) } else { (3, n - 2) };
        let x = t * segs as f32;
        let (seg, u) =
            if x as usize >= segs { (segs - 1, 1.0) } else { (x as usize, x - (x as usize) as f32) };
        let w: Vec<f32> = if order == 4 {
            let v = 1.0 - u;
            let u2 = u * u;
            let u3 = u2 * u;
            vec![
                v * v * v / 6.0,
                u3 / 2.0 - u2 + 2.0 / 3.0,
                u2 / 2.0 - u3 / 2.0 + u / 2.0 + 1.0 / 6.0,
                u3 / 6.0,
            ]
        } else {
            let v = 1.0 - u;
            vec![v * v * 0.5, u * v + 0.5, u * u * 0.5]
        };
        std::array::from_fn(|k| w.iter().enumerate().map(|(i, w)| w * p[seg + i][k]).sum())
    }

    /// The nearest point of the path to `q`, and where along the path it is: a coarse search over ten steps per
    /// point, then halving steps around the best.
    pub fn nearest(&self, q: [f32; 3]) -> ([f32; 3], f32) {
        let d2 = |p: [f32; 3]| (0..3).map(|k| (p[k] - q[k]).powi(2)).sum::<f32>();
        let steps = (self.points.len() * 10).max(1);
        let mut best = (0.0f32, f32::MAX);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let d = d2(self.at(t));
            if d < best.1 {
                best = (t, d);
            }
        }
        let mut step = 1.0 / steps as f32;
        for _ in 0..40 {
            step *= 0.5;
            for t in [best.0 - step, best.0 + step] {
                let t = t.clamp(0.0, 1.0);
                let d = d2(self.at(t));
                if d < best.1 {
                    best = (t, d);
                }
            }
        }
        (self.at(best.0), best.0)
    }
}

/// The point at a heading, pitch and distance (radians, radians, units) from `from`.
pub fn polar_point(from: [f32; 3], [yaw, pitch, dist]: [f32; 3]) -> [f32; 3] {
    [
        from[0] + dist * pitch.cos() * yaw.cos(),
        from[1] + dist * pitch.sin(),
        from[2] + dist * pitch.cos() * yaw.sin(),
    ]
}

/// The heading, pitch and distance of `to` from `from` (the inverse of [`polar_point`]).
pub fn to_polar(from: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    let d = [to[0] - from[0], to[1] - from[1], to[2] - from[2]];
    let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let pitch = if dist > 0.0 { (d[1] / dist).clamp(-1.0, 1.0).asin() } else { 0.0 };
    [d[2].atan2(d[0]), pitch, dist]
}

/// A move in progress blends from its start to the new place, a share of count / frames a tick.
fn blend(m: &mut Option<([f32; 3], u32, u32)>, to: [f32; 3]) -> [f32; 3] {
    match m {
        Some((from, frames, done)) => {
            let t = *done as f32 / (*frames).max(1) as f32;
            *done += 1;
            let out = std::array::from_fn(|k| from[k] + (to[k] - from[k]) * t);
            if *done > *frames {
                *m = None;
            }
            out
        }
        None => to,
    }
}

/// Most limits a camera part keeps.
const MAX_LIMITS: usize = 5;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct FollowCamera {
    /// Workers whose average position the focus follows (up to two), and the offset added.
    pub follow: Option<([Option<usize>; 2], [f32; 3])>,
    pub limits: [Option<Limit>; MAX_LIMITS],
    /// Per axis, the focus covers 1 / divisor of the way to its target each tick (0: at once).
    pub ease: [f32; 3],
    pub orbit: Option<Orbit>,
    /// The eye on a table of points instead (when set).
    pub path: Option<EyePath>,
    /// A rail for the focus (it keeps `k` of its distance from it), and where along it the focus is.
    pub rail: Option<(Path, f32)>,
    rail_t: f32,
    /// The eye on a path at the focus's place along its rail.
    pub eye_rail: Option<Path>,
    /// The eye at a heading, pitch and distance from the focus (radians, radians, units).
    pub polar: Option<[f32; 3]>,
    /// The focus as eased so far.
    focus: Option<[f32; 3]>,
    /// Moves from where the focus and the eye were to their new places: start, frame count, frames done.
    pub focus_move: Option<([f32; 3], u32, u32)>,
    pub eye_move: Option<([f32; 3], u32, u32)>,
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
        if let Some((rail, k)) = &self.rail {
            let (on, t) = rail.nearest(target);
            self.rail_t = t;
            target = std::array::from_fn(|i| on[i] + k * (target[i] - on[i]));
        }
        let focus = match self.focus {
            Some(old) => std::array::from_fn(|k| {
                let d = self.ease[k];
                if d != 0.0 { old[k] + (target[k] - old[k]) / d } else { target[k] }
            }),
            None => target,
        };
        self.focus = Some(focus);
        let eye = self.eye_rail.as_ref().map(|r| r.at(self.rail_t));
        let eye = eye.or_else(|| self.path.as_ref().map(|p| p.eye(focus))).or_else(|| {
            self.orbit.map(|o| {
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
            })
        });
        let eye = eye.or_else(|| self.polar.map(|p| polar_point(focus, p)));
        let focus = blend(&mut self.focus_move, focus);
        let eye = eye.map(|e| blend(&mut self.eye_move, e));
        Some((focus, eye))
    }

    /// The polar eye around a focus the scripts placed (while not following), moving there when a move is timed.
    pub fn polar_eye(&mut self, focus: [f32; 3]) -> Option<[f32; 3]> {
        let to = polar_point(focus, self.polar?);
        Some(blend(&mut self.eye_move, to))
    }

    /// Drop every way the eye was placed (before a script sets a new one).
    pub fn clear_eye(&mut self) {
        self.orbit = None;
        self.path = None;
        self.eye_rail = None;
        self.polar = None;
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
    fn paths_pass_near_their_points_and_find_the_nearest() {
        let p = Path { points: vec![[0.0; 3], [0.0; 3], [10.0, 0.0, 0.0], [10.0, 0.0, 0.0]], cubic: false };
        assert_eq!(p.at(0.0), [0.0; 3]);
        assert_eq!(p.at(1.0), [10.0, 0.0, 0.0]);
        let (on, t) = p.nearest([5.0, 0.0, 3.0]);
        assert!((on[0] - 5.0).abs() < 0.01 && on[2].abs() < 1e-5, "{on:?}");
        assert!((t - 0.5).abs() < 0.01);
    }

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
