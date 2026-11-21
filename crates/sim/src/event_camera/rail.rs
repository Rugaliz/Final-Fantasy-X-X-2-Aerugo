//! Rails: the nearest point of a path to a target. The search keeps one place and one path length for every
//! rail (two globals in the original), so they live with the island: a start measures the path and finds the
//! nearest sample, and each search moves the place by shrinking steps towards the target.

use super::CamMem;
use super::spline::{Pts, eval};

/// Squared distance of a point from the target with the original's x87 sum ((x² + y²) + z²), rounded to float.
fn dist2(p: [f32; 4], q: [f32; 3]) -> f32 {
    let d = [p[0] - q[0], p[1] - q[1], p[2] - q[2]].map(f64::from);
    ((d[0] * d[0] + d[1] * d[1]) + d[2] * d[2]) as f32
}

// The searches compare as the original's x87 code does, where NaN takes the other branch.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
impl CamMem {
    /// Measure the path (the sum of the steps between samples ten per point apart) and take the place of
    /// the sample nearest the target, then refine it with a search.
    pub(super) fn rail_start(&mut self, pts: Pts, q: [f32; 3], cubic: bool) {
        self.rail_len = 0.0;
        let n = pts.count();
        let mut best = 999_999.0f32;
        let mut t = 0.0f32;
        let step = 1.0 / (n.wrapping_mul(10)) as f32;
        self.rail_place = 0.0;
        // The samples are locals the evaluation may leave as they were; they start at zero here.
        let mut prev = [0.0f32; 4];
        let mut cur = [0.0f32; 4];
        eval(&mut prev, pts, cubic, 0.0, true);
        loop {
            eval(&mut cur, pts, cubic, t, true);
            let d = [cur[0] - prev[0], cur[1] - prev[1], cur[2] - prev[2]].map(f64::from);
            let seg = ((d[0] * d[0] + d[1] * d[1]) + d[2] * d[2]).sqrt() as f32;
            self.rail_len += seg;
            let d2 = dist2(cur, q);
            if d2 < best {
                best = d2;
                self.rail_place = t;
            }
            prev = [cur[0], cur[1], cur[2], prev[3]];
            t += step;
            if !(t <= 1.0) {
                break;
            }
        }
        self.rail_nearest(pts, q, cubic);
    }

    /// Forty steps from the kept place, each 10 / length over (i² + 1): towards the nearer of the two
    /// neighbours, or staying when the place itself is nearest. Returns the place (when a step leaves 0..1, the
    /// place it stepped from, while the kept place stays outside) and the nearest point.
    pub(super) fn rail_nearest(&mut self, pts: Pts, q: [f32; 3], cubic: bool) -> (f32, [f32; 3]) {
        let k0 = (10.0 / self.rail_len as f64) as f32;
        let (mut a, mut b, mut c) = ([0.0f32; 4], [0.0f32; 4], [0.0f32; 4]);
        let mut out = [0.0f32; 3];
        let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
        for i in 0..40i32 {
            let step = k0 / (i * i + 1) as f32;
            eval(&mut a, pts, cubic, self.rail_place - step, true);
            let da = dist2(a, q);
            eval(&mut b, pts, cubic, self.rail_place + step, true);
            let db = dist2(b, q);
            eval(&mut c, pts, cubic, self.rail_place, true);
            let dc = dist2(c, q);
            if db == da {
                return (self.rail_place, xyz(c));
            }
            if db > da {
                if !(dc > da) {
                    out = xyz(c);
                    continue;
                }
                self.rail_place -= step;
                if self.rail_place < 0.0 {
                    return (step + self.rail_place, xyz(c));
                }
                out = xyz(a);
            } else {
                if !(dc > db) {
                    out = xyz(c);
                    continue;
                }
                self.rail_place += step;
                if 1.0 < self.rail_place {
                    return (self.rail_place - step, xyz(c));
                }
                out = xyz(b);
            }
        }
        (self.rail_place, out)
    }
}
