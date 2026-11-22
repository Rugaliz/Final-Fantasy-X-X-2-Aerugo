//! The camera's view matrix as the original builds it from the eye's and the focus's view points, an up vector
//! and the roll: forward = the focus seen from the eye, side = forward × up, up = forward × side (each normalised),
//! the transposed basis with the translation −(eye · basis), then the roll turning the first two columns. Row-major
//! 4×4, points as rows (p · M). An eye on the focus gives a matrix of zeros with 1 in the corner.
//!
//! Each product or sum the original stores is rounded to float here; a sum taking a product straight from the
//! FPU keeps that product unrounded.

/// An f32 widened for an x87 expression.
fn d(x: f32) -> f64 {
    x as f64
}

/// 1 / length (0 for a zero vector), the squares summed (y² + x²) + z², each step rounded.
fn inv_len(v: [f32; 3]) -> f32 {
    let s = (v[1] * v[1] + v[0] * v[0]) + v[2] * v[2];
    if s.abs() == 0.0 { 0.0 } else { 1.0 / s.abs().sqrt() }
}

fn normalized(v: [f32; 3]) -> [f32; 3] {
    let q = inv_len(v);
    [v[0] * q, v[1] * q, q * v[2]]
}

/// `a × b` as the original rounds it: each component f32(f32(p) − q) with the second product unrounded.
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    let p = [a[1] * b[2], b[0] * a[2], b[1] * a[0]];
    [
        (d(p[0]) - d(b[1]) * d(a[2])) as f32,
        (d(p[1]) - d(a[0]) * d(b[2])) as f32,
        (d(p[2]) - d(b[0]) * d(a[1])) as f32,
    ]
}

/// The view matrix for an eye and a focus (view points, four lanes compared), an up vector and a roll.
pub fn view_matrix(eye: [f32; 4], focus: [f32; 4], up: [f32; 4], roll: f32) -> [f32; 16] {
    if (0..4).all(|k| eye[k].to_bits() == focus[k].to_bits()) {
        let mut m = [0.0; 16];
        m[15] = 1.0;
        return m;
    }
    let c = (d(roll)).cos() as f32;
    let s = (d(roll)).sin() as f32;
    let f = normalized([focus[0] - eye[0], focus[1] - eye[1], focus[2] - eye[2]]);
    let side = normalized(cross(f, [up[0], up[1], up[2]]));
    let u = normalized(cross(f, side));
    // The transposed basis (each value plus the VU zero register's 0, which turns −0 into 0).
    let r0 = [side[0], u[0] + 0.0, f[0] + 0.0];
    let r1 = [side[1] + 0.0, u[1], f[1] + 0.0];
    let r2 = [side[2] + 0.0, u[2] + 0.0, f[2]];
    // The translation: −(e.x · r0 + e.y · r1 + e.z · r2), accumulated as the VU does (one rounding a step).
    let t: [f32; 3] = std::array::from_fn(|k| {
        let acc = eye[0] * r0[k];
        let acc = (d(eye[1]) * d(r1[k]) + d(acc)) as f32;
        let acc = (d(eye[2]) * d(r2[k]) + d(acc)) as f32;
        0.0 - acc
    });
    // The roll on the first two columns of each row: (x·c − y·s, x·s + y·c).
    let turn = |x: f32, y: f32| {
        let (xc, xs) = (x * c, x * s);
        ((d(-s) * d(y) + d(xc)) as f32, (d(y) * d(c) + d(xs)) as f32)
    };
    let mut m = [0.0f32; 16];
    for (row, (x, y, z, w)) in [
        (r0[0], r0[1], r0[2], 0.0),
        (r1[0], r1[1], r1[2], 0.0),
        (r2[0], r2[1], r2[2], 0.0),
        (t[0], t[1], t[2], 1.0),
    ]
    .into_iter()
    .enumerate()
    {
        let (a, b) = turn(x, y);
        m[row * 4..row * 4 + 4].copy_from_slice(&[a, b, z, w]);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eye_on_focus_is_the_corner_matrix() {
        let m = view_matrix([1.0, 2.0, 3.0, 0.0], [1.0, 2.0, 3.0, 0.0], [0.0, -1.0, 0.0, 0.0], 0.0);
        assert_eq!(m[15], 1.0);
        assert!(m[..15].iter().all(|&v| v == 0.0));
    }

    #[test]
    fn looking_down_z_puts_forward_in_the_third_column() {
        let m = view_matrix([0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 10.0, 0.0], [0.0, -1.0, 0.0, 0.0], 0.0);
        assert_eq!([m[2], m[6], m[10]], [0.0, 0.0, 1.0]);
    }
}
