//! The engine's angle and vector helpers as the original computes them: each statement is worked out at double
//! precision and stored as a float, and the angle constants are the float values of π and its fractions.

const PI: f32 = std::f32::consts::PI;
const TAU: f32 = 2.0 * std::f32::consts::PI;
const HALF_PI: f32 = std::f32::consts::FRAC_PI_2;

fn d(x: f32) -> f64 {
    f64::from(x)
}

/// An angle brought into [−π, π] by whole turns.
pub fn wrap(mut a: f32) -> f32 {
    while d(a) < -d(PI) {
        a = (d(a) + d(TAU)) as f32;
    }
    while d(a) > d(PI) {
        a = (d(a) - d(TAU)) as f32;
    }
    a
}

/// The heading from (x1, z1) to (x2, z2): atan2 of the z and x differences, with the axis cases exact.
pub fn heading(x1: f32, z1: f32, x2: f32, z2: f32) -> f32 {
    let dx = x2 - x1;
    let dz = z2 - z1;
    if dx.abs() == 0.0 {
        if d(dz) < 0.0 { -HALF_PI } else { HALF_PI }
    } else if dz.abs() == 0.0 {
        if d(dx) < 0.0 { PI } else { -0.0 }
    } else {
        d(dz).atan2(d(dx)) as f32
    }
}

/// The heading and the pitch from one point to another.
pub fn angles_to(a: [f32; 3], b: [f32; 3]) -> (f32, f32) {
    let yaw = heading(a[0], a[2], b[0], b[2]);
    let dx = b[0] - a[0];
    let dz = b[2] - a[2];
    let dist = (d(dx) * d(dx) + d(dz) * d(dz)).sqrt() as f32;
    let pitch = heading(0.0, a[1], dist, b[1]);
    (yaw, pitch)
}

/// The smaller of |a| and `limit`.
pub fn clamp_abs(a: f32, limit: f32) -> f32 {
    if d(a.abs()) < d(limit) { a.abs() } else { limit }
}

/// Turn `from` toward `to` by at most `step`, the short way.
pub fn turn_toward(from: f32, to: f32, step: f32) -> f32 {
    let diff = wrap(to - from);
    let s = clamp_abs(diff, step);
    let out = if s == step { if d(diff) >= 0.0 { from + s } else { from - s } } else { to };
    wrap(out)
}

/// Turn `from` toward `to` by `step` in one direction (the sign of `step`), stopping on it.
pub fn turn_one_way(from: f32, to: f32, step: f32) -> f32 {
    let a = from - to;
    let b = (d(from) + d(step) - d(to)) as f32;
    let prod = a * b;
    let out = if prod == 0.0 || d(prod) <= 0.0 { to } else { from + step };
    wrap(out)
}

/// A turn by `step`: 0 the short way, 1 increasing, 2 decreasing.
pub fn turn_by(from: f32, to: f32, step: f32, way: u8) -> f32 {
    match way {
        1 => turn_one_way(from, to, step),
        2 => turn_one_way(from, to, (-d(step)) as f32),
        _ => turn_toward(from, to, step),
    }
}

/// The squared length of a vector.
pub fn len2(x: f32, y: f32, z: f32) -> f32 {
    (d(x) * d(x) + d(y) * d(y) + d(z) * d(z)) as f32
}

pub fn sqrt(x: f32) -> f32 {
    d(x).sqrt() as f32
}

/// A step of `len` along a heading and pitch (pitch folded into ±π/2), w 1.
pub fn dir_vec(len: f32, mut yaw: f32, pitch: f32) -> [f32; 4] {
    let mut pitch = wrap(pitch);
    if d(pitch) > d(HALF_PI) {
        yaw = (d(yaw) + d(PI)) as f32;
        pitch = (d(PI) - d(pitch)) as f32;
    }
    if d(pitch) < -d(HALF_PI) {
        yaw = (d(yaw) + d(PI)) as f32;
        pitch = (d(pitch) + d(PI)) as f32;
    }
    let yaw = wrap(yaw);
    let sp = d(pitch).sin() as f32;
    let y = (d(sp) * d(len)) as f32;
    let cp = d(pitch).cos() as f32;
    let flat = (d(cp) * d(len)) as f32;
    let cy = d(yaw).cos() as f32;
    let x = (d(cy) * d(flat)) as f32;
    let sy = d(yaw).sin() as f32;
    let z = (d(sy) * d(flat)) as f32;
    [x, y, z, 1.0]
}

/// The point (x2, z2) seen from (x1, z1) lies within `half` of `facing`.
pub fn faces(facing: f32, half: f32, x1: f32, z1: f32, x2: f32, z2: f32) -> bool {
    let h = heading(x1, z1, x2, z2);
    let off = wrap(h - facing);
    let a = clamp_abs(off, half);
    d(a) < d(half)
}

/// The side of the line (x1, z1)–(x2, z2) a point lies on (the cross product's sign).
pub fn side(x1: f32, z1: f32, x2: f32, z2: f32, px: f32, pz: f32) -> f32 {
    ((((d(px) * d(z1) + d(x2) * d(pz) + d(x1) * d(z2)) - d(x1) * d(pz)) - d(x2) * d(z1)) - d(px) * d(z2))
        as f32
}

/// The path a→b crosses the line c→d on the ground plane: 1 or −1 by the side `b` lies on, 0 none.
pub fn crossing(a: [f32; 3], b: [f32; 3], c: [f32; 3], e: [f32; 3]) -> i32 {
    let s1 = side(a[0], a[2], b[0], b[2], c[0], c[2]);
    let s2 = side(a[0], a[2], b[0], b[2], e[0], e[2]);
    // Both products at most 0 (a touch counts).
    let p = s1 * s2;
    if p <= 0.0 {
        let t1 = side(c[0], c[2], e[0], e[2], a[0], a[2]);
        let t2 = side(c[0], c[2], e[0], e[2], b[0], b[2]);
        let q = t1 * t2;
        if q <= 0.0 {
            return if d(s2) > 0.0 { 1 } else { -1 };
        }
    }
    0
}

/// The height gap where a→b meets the line c→e (−1 when parallel).
pub fn crossing_height(a: [f32; 3], b: [f32; 3], c: [f32; 3], e: [f32; 3]) -> f32 {
    let mut ux = b[0] - a[0];
    let uz = b[2] - a[2];
    let vx = e[0] - c[0];
    let vz = e[2] - c[2];
    let wx = c[0] - a[0];
    let wz = c[2] - a[2];
    let den = (d(ux) * d(vz) - d(vx) * d(uz)) as f32;
    if den == 0.0 {
        return -1.0;
    }
    let s = ((d(wx) * d(uz) - d(ux) * d(wz)) / d(den)) as f32;
    if ux == 0.0 {
        ux = ((d(vz) * d(s) + d(wz)) / d(uz)) as f32;
    } else {
        ux = ((d(vx) * d(s) + d(wx)) / d(ux)) as f32;
    }
    let t = ux;
    (((d(e[1] - c[1]) * d(s) + d(c[1])) - (d(b[1] - a[1]) * d(t) + d(a[1]))).abs()) as f32
}

/// Clamp to [0, 1].
pub fn unit(x: f32) -> f32 {
    if d(x) < 0.0 {
        0.0
    } else if d(x) > 1.0 {
        1.0
    } else {
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_and_heading_match_the_axis_cases() {
        assert_eq!(wrap(4.0), (4.0f64 - f64::from(TAU)) as f32);
        assert_eq!(heading(0.0, 0.0, 0.0, -1.0), -HALF_PI);
        assert_eq!(heading(0.0, 0.0, 1.0, 0.0).to_bits(), (-0.0f32).to_bits());
        assert_eq!(turn_toward(0.0, 1.0, 0.25), 0.25);
        assert_eq!(turn_toward(0.0, 0.1, 0.25), 0.1);
    }
}
