//! The camera's spline evaluation (quadratic and cubic B-splines): a point (with its roll as the fourth
//! value) or the normalised direction of a path at a place 0..1, with the original's arithmetic. Its x87 code
//! keeps intermediates as doubles and rounds to float where it stores, so the sums below run in f64 and round
//! where the original does.
//!
//! A table is a count (i32) and, from +0x10, points of four floats. The evaluation runs on the emulated VU
//! registers; what it leaves in the result register's fourth lane is returned with the output, because the
//! camera's vector sums read that lane back (see `path_tick`).

/// A path table: the count and the points (read as the original reads memory; past the table, zero).
#[derive(Clone, Copy, Debug)]
pub struct Pts<'a>(pub &'a [u8]);

impl Pts<'_> {
    pub fn count(&self) -> i32 {
        self.0.get(0..4).map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap()))
    }
    pub fn point(&self, i: i32) -> [f32; 4] {
        std::array::from_fn(|k| {
            let at = 0x10 + i as isize * 16 + k as isize * 4;
            if at < 0 {
                return 0.0;
            }
            let at = at as usize;
            self.0.get(at..at + 4).map_or(0.0, |b| f32::from_le_bytes(b.try_into().unwrap()))
        })
    }
}

/// What an evaluation did: the output written (false: left as it was) and the value it left in the VU result
/// register's fourth lane (None: untouched).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eval {
    pub wrote: bool,
    pub vu_w: Option<f32>,
}

const UNTOUCHED: Eval = Eval { wrote: false, vu_w: None };
const WROTE: Eval = Eval { wrote: true, vu_w: None };

/// The place clamped to 0..1 as both evaluators do (a NaN stays).
fn clamp_place(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

/// The segment of `segs` that the place falls in and the place within it.
fn segment(t: f32, segs: i32) -> (i32, f32) {
    let n = segs as f32;
    let len = 1.0 / n;
    let mut seg = (t as f64 / len as f64) as i32;
    let mut u = ((t as f64 - (seg as f32) as f64 * len as f64) * n as f64) as f32;
    while u < 0.0 {
        u = (u as f64 + 1.0) as f32;
        seg -= 1;
    }
    while u > 1.0 {
        u = (u as f64 - 1.0) as f32;
        seg += 1;
    }
    if seg < 0 {
        seg = 0;
        u = 0.0;
    }
    if seg >= segs {
        seg = segs - 1;
        u = 1.0;
    }
    (seg, u)
}

/// The VU multiply-accumulate chain: Σ w_i·p_i per lane, each step rounded to float.
fn blend(p: &[[f32; 4]], w: &[f32]) -> [f32; 4] {
    let mut acc: [f32; 4] = std::array::from_fn(|k| w[0] * p[0][k]);
    for i in 1..p.len() {
        acc = std::array::from_fn(|k| (acc[k] as f64 + w[i] as f64 * p[i][k] as f64) as f32);
    }
    acc
}

/// The normalised xyz of a VU vector (1 / sqrt of the summed squares; zero length gives zero), its fourth lane
/// then set to vf0.w + vf0.x = 1.
fn vu_normalize(e: [f32; 4]) -> [f32; 4] {
    let sq: [f32; 4] = std::array::from_fn(|k| e[k] * e[k]);
    let s = (sq[0] + sq[1]) + sq[2];
    let q = if s.abs() == 0.0 { 0.0 } else { 1.0 / s.abs().sqrt() };
    [e[0] * q, e[1] * q, e[2] * q, 1.0]
}

/// A quadratic B-spline over n − 2 segments; `pos` false gives the direction.
pub fn quadratic(out: &mut [f32; 4], pts: Pts, t: f32, pos: bool) -> Eval {
    let t = clamp_place(t);
    let n = pts.count();
    if (0..=2).contains(&n) {
        if pos {
            if n < 2 || t == 0.0 || t == 1.0 {
                // The original assigns its local output pointer here and returns: the output stays.
                return UNTOUCHED;
            }
            let (a, b) = (pts.point(0), pts.point(1));
            let v = 1.0 - t;
            *out = std::array::from_fn(|k| ((v as f64 * a[k] as f64 + t as f64) + b[k] as f64) as f32);
            return WROTE;
        }
        if n < 2 {
            *out = [1.0, 0.0, 0.0, 1.0];
            return WROTE;
        }
        let (a, b) = (pts.point(0), pts.point(1));
        let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2], 0.0];
        let mut o = ffx_normalize(d);
        o[3] = 1.0;
        *out = o;
        return WROTE;
    }
    if n < 0 {
        // A negative count reads before the table; no script table has one.
        return UNTOUCHED;
    }
    let (seg, u) = segment(t, n - 2);
    let p = [pts.point(seg), pts.point(seg + 1), pts.point(seg + 2)];
    if pos {
        let v = 1.0 - u;
        let w = [v * v * 0.5, (u as f64 * v as f64 + 0.5) as f32, u * u * 0.5];
        *out = blend(&p, &w);
        WROTE
    } else {
        let w = [(u as f64 - 1.0) as f32, (1.0 - u as f64 * 2.0) as f32, u];
        let o = vu_normalize(blend(&p, &w));
        *out = o;
        Eval { wrote: true, vu_w: Some(o[3]) }
    }
}

/// A cubic B-spline over n − 3 segments, or the
/// quadratic one for up to three points.
pub fn cubic(out: &mut [f32; 4], pts: Pts, t: f32, pos: bool) -> Eval {
    let t = clamp_place(t);
    let n = pts.count();
    if (0..=3).contains(&n) {
        return quadratic(out, pts, t, pos);
    }
    let (seg, u) = segment(t, n - 3);
    let p = [pts.point(seg), pts.point(seg + 1), pts.point(seg + 2), pts.point(seg + 3)];
    let (u64_, c23, c16) = (u as f64, 0.666_666_7f32 as f64, 0.166_666_67f32 as f64);
    if pos {
        let v = (1.0 - u) as f64;
        let w0 = (v * v * v / 6.0) as f32;
        let u2 = u * u;
        let uh = u / 2.0;
        let h = u2 * uh;
        let w1 = ((h as f64 - u2 as f64) + c23) as f32;
        let w2 = (((-(h as f64)) + u2 as f64 / 2.0) + uh as f64 + c16) as f32;
        let w3 = (h as f64 / 3.0) as f32;
        let r = blend(&p, &[w0, w1, w2, w3]);
        *out = r;
        Eval { wrote: true, vu_w: Some(r[3]) }
    } else {
        let w = [
            ((u64_ * -0.5 * u64_ + u64_) - 0.5) as f32,
            (u64_ * 1.5 * u64_ - u64_ * 2.0) as f32,
            ((u64_ * -1.5 * u64_ + u64_) + 0.5) as f32,
            (u64_ * 0.5 * u64_) as f32,
        ];
        let d = blend(&p, &w);
        *out = vu_normalize(d);
        Eval { wrote: true, vu_w: Some(d[3]) }
    }
}

/// Xyz over their length when the squared length is finite and above the smallest normal
/// float, else all four lanes zero.
fn ffx_normalize(v: [f32; 4]) -> [f32; 4] {
    let (x, y, z) = (v[0] as f64, v[1] as f64, v[2] as f64);
    let l2 = ((x * x + y * y) + z * z) as f32;
    if !(l2.is_finite() && f32::MIN_POSITIVE < l2) {
        return [0.0; 4];
    }
    let s = ((y * y + x * x) + z * z) as f32;
    let k = 1.0 / s.sqrt();
    [v[0] * k, v[1] * k, k * v[2], v[3]]
}

/// A path's point or direction by its mode (filter +0x1c: both low bits set, cubic).
pub fn eval(out: &mut [f32; 4], pts: Pts, cubic_mode: bool, t: f32, pos: bool) -> Eval {
    if cubic_mode { cubic(out, pts, t, pos) } else { quadratic(out, pts, t, pos) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(points: &[[f32; 4]]) -> Vec<u8> {
        let mut v = vec![0u8; 0x10];
        v[0..4].copy_from_slice(&(points.len() as i32).to_le_bytes());
        for p in points {
            for x in p {
                v.extend_from_slice(&x.to_le_bytes());
            }
        }
        v
    }

    #[test]
    fn quadratic_middle_of_three_points() {
        let t = table(&[[0.0, 0.0, 0.0, 0.0], [2.0, 0.0, 0.0, 1.0], [4.0, 0.0, 0.0, 2.0]]);
        let mut o = [9.0; 4];
        assert!(quadratic(&mut o, Pts(&t), 0.5, true).wrote);
        assert_eq!(o, [2.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn two_points_at_ends_leave_output() {
        let t = table(&[[0.0; 4], [1.0; 4]]);
        let mut o = [7.0; 4];
        assert_eq!(quadratic(&mut o, Pts(&t), 0.0, true), UNTOUCHED);
        assert_eq!(o, [7.0; 4]);
    }

    #[test]
    fn direction_is_unit_with_w_one() {
        let t = table(&[[0.0; 4], [0.0, 0.0, 3.0, 0.0], [0.0, 0.0, 6.0, 0.0]]);
        let mut o = [0.0; 4];
        let e = quadratic(&mut o, Pts(&t), 0.25, false);
        assert_eq!(o, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(e.vu_w, Some(1.0));
    }
}
