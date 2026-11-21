//! Polar placement and measure with the original's arithmetic: a point at a heading, pitch and distance from an
//! anchor, and a point's heading, pitch and distance seen from one. Sines and cosines are taken in double and
//! rounded to float, as the original's calls return them.

/// A quarter turn as the original's float constant.
const QUARTER: f32 = 1.570_796_4;
/// Pi as the original's double constant (the float value widened).
const PI_D: f64 = 3.141_592_741_012_573_2;

fn sin(x: f32) -> f32 {
    (x as f64).sin() as f32
}
fn cos(x: f32) -> f32 {
    (x as f64).cos() as f32
}

/// The point at heading `yaw`, pitch `pitch` (radians) and distance `dist` from the anchor (none: from the
/// origin). The fourth value is 0·dist.
pub fn place(anchor: Option<[f32; 3]>, yaw: f32, pitch: f32, dist: f32) -> [f32; 4] {
    let (cp, sp) = (cos(pitch), sin(pitch));
    let (cy, sy) = (cos(yaw), sin(yaw));
    let off = [cy * cp * dist, sp * dist, cp * sy * dist, 0.0 * dist];
    match anchor {
        Some(a) => [a[0] + off[0], a[1] + off[1], a[2] + off[2], off[3]],
        None => off,
    }
}

/// The heading, pitch and distance of `point` from the anchor (none: from the origin). The heading is
/// atan(dz / dx), less a half turn when dx is not positive (a quarter turn either way when dx is 0); the pitch
/// atan of the height over the ground distance, likewise.
pub fn measure(point: [f32; 3], anchor: Option<[f32; 3]>) -> [f32; 3] {
    let [dx, dy, dz] = match anchor {
        Some(a) => [point[0] - a[0], point[1] - a[1], point[2] - a[2]],
        None => point,
    };
    let (x2, y2, z2) = (dx * dx, dy * dy, dz * dz);
    let full2 = z2 + (x2 + y2);
    let ground2 = z2 + x2;
    let dist = full2.abs().sqrt() + 0.0;
    let ground = 0.0 + ground2.abs().sqrt();
    let yaw = if dx == 0.0 {
        if dz < 0.0 { -QUARTER } else { QUARTER }
    } else {
        let a = ((dz / dx) as f64).atan() as f32;
        if dx > 0.0 { a } else { (a as f64 - PI_D) as f32 }
    };
    let q = dy / ground;
    let mut pitch = if !q.is_finite() {
        if dy < 0.0 || dy.is_nan() { -QUARTER } else { QUARTER }
    } else {
        let a = (q as f64).atan() as f32;
        if ground > 0.0 { a } else { (a as f64 - PI_D) as f32 }
    };
    let mut dist = dist;
    if dist < 0.0 {
        dist = -dist;
        pitch = -pitch;
    }
    [yaw, pitch, dist]
}
