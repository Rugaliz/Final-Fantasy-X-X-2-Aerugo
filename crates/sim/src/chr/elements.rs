//! The character engine's element (bone) matrices, as its batch builds them each frame, with the original's
//! x87 arithmetic: every product and sum of a statement in double precision, rounded to single where it is
//! stored.
//!
//! Matrices are the PS2 library's: 16 floats, four column vectors (x, y, z, w), translation in the fourth.
//! - An element's local matrix is its scale, then turns about X, Y and Z (each applied on the left), with its
//!   translation in column 3; its matrix is its parent's times the local one (the root's parent is the
//!   identity): model space.
//! - The body's world matrix (+0x1d0): the model scale ((block +0x30 / +0x38) · 0.01 times the body's scale),
//!   the tilt about X when one is set, the turn about Y of −facing − π/2, at the position plus the draw offset;
//!   or, for a body hung on another body's element, that element's matrix scaled and offset, in the other's world.
//! - Option points (+0x524 + key · 0x10): each option position of the model at its element, its offset scaled,
//!   in the world.

/// A 4 × 4 matrix in the PS2 library's layout.
pub type Mat = [f32; 16];

pub const IDENTITY: Mat = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn d(x: f32) -> f64 {
    f64::from(x)
}

/// `a · b`. Each element sums its first three products, is stored, then takes the fourth.
pub fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut out = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            let three = ((d(a[r]) * d(b[c * 4]) + d(a[4 + r]) * d(b[c * 4 + 1]))
                + d(a[8 + r]) * d(b[c * 4 + 2])) as f32;
            out[c * 4 + r] = (d(a[12 + r]) * d(b[c * 4 + 3]) + d(three)) as f32;
        }
    }
    out
}

/// `m · v`, summed in order.
pub fn apply4(m: &Mat, v: [f32; 4]) -> [f32; 4] {
    std::array::from_fn(|r| {
        (((d(m[r]) * d(v[0]) + d(m[4 + r]) * d(v[1])) + d(m[8 + r]) * d(v[2])) + d(m[12 + r]) * d(v[3]))
            as f32
    })
}

/// The cosine and sine an angle turns by (the runtime's double functions, rounded to single).
fn cos_sin(angle: f32) -> (f64, f64) {
    (d((d(angle).cos()) as f32), d((d(angle).sin()) as f32))
}

/// `m` turned about X (rows y, z of every column).
pub fn rot_x(m: &Mat, angle: f32) -> Mat {
    let (c, s) = cos_sin(angle);
    let mut o = *m;
    for k in 0..4 {
        let (y, z) = (d(m[k * 4 + 1]), d(m[k * 4 + 2]));
        o[k * 4 + 1] = (y * c - z * s) as f32;
        o[k * 4 + 2] = (y * s + z * c) as f32;
    }
    o
}

/// `m` turned about Y.
pub fn rot_y(m: &Mat, angle: f32) -> Mat {
    let (c, s) = cos_sin(angle);
    let mut o = *m;
    for k in 0..4 {
        let (x, z) = (d(m[k * 4]), d(m[k * 4 + 2]));
        o[k * 4] = (x * c + z * s) as f32;
        o[k * 4 + 2] = (z * c - x * s) as f32;
    }
    o
}

/// `m` turned about Z.
pub fn rot_z(m: &Mat, angle: f32) -> Mat {
    let (c, s) = cos_sin(angle);
    let mut o = *m;
    for k in 0..4 {
        let (x, y) = (d(m[k * 4]), d(m[k * 4 + 1]));
        o[k * 4] = (x * c - y * s) as f32;
        o[k * 4 + 1] = (x * s + y * c) as f32;
    }
    o
}

/// An element's values (+0xe4 rotation in radians, +0xf4 translation, +0x104 scale).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Joint {
    pub rotation: [f32; 3],
    pub translation: [f32; 3],
    pub scale: [f32; 3],
}

/// A motion's channel values (turns, 1/4096 units, scale) as element values; `scale` is the model
/// block's +0x38 · 0.001.
pub fn joint_from_channels(v: [f32; 9], scale: f32) -> Joint {
    let pi = d(super::BODY_PI);
    let rot = |x: f32| super::wrap(((d(x) * 2.0) * pi) as f32);
    let trans = |x: f32| ((d(x) * d(scale)) * 4096.0) as f32;
    Joint {
        rotation: [rot(v[0]), rot(v[1]), rot(v[2])],
        translation: [trans(v[3]), trans(v[4]), trans(v[5])],
        scale: [v[6], v[7], v[8]],
    }
}

/// For the channels marked: each moved channel's value into its element value.
pub fn apply_channels(j: &mut Joint, v: [f32; 9], dirty: [bool; 9], scale: f32) {
    let c = joint_from_channels(v, scale);
    for k in 0..3 {
        if dirty[k] {
            j.rotation[k] = c.rotation[k];
        }
        if dirty[3 + k] {
            j.translation[k] = c.translation[k];
        }
        if dirty[6 + k] {
            j.scale[k] = c.scale[k];
        }
    }
}

/// The blink (blink mode 0 in play: it runs while the body's +0x4d4 is set, and winds down to open
/// otherwise): +0x4dc the closure now (tenths of a degree), +0x4de its target, +0x4e1 the frames until the next
/// target, +0x4e3 the step. Targets: a CRT rand (>> 5) % 100 − 30, under 0 none. The eyelid element (option key
/// 0xd) takes the block's turn at rest with the closure added on the block's axis (negated by its sign byte).
/// Returns the eyelid texture frame (1 when the closure passes 10, else 0) for the face player.
pub fn blink(
    ch: &mut super::Chr,
    rand: &mut crate::field::face::CrtRand,
    joints: &mut [Joint],
    eye: Option<i32>,
    block: (u8, u8, u8, [f32; 3]),
) -> Option<i32> {
    let timers = |ch: &mut super::Chr, rand: &mut crate::field::face::CrtRand| {
        ch.set_u8(0x4e1, (((rand.rand() >> 5) & 3) + 1) as u8);
        ch.set_u8(0x4e3, ((((rand.rand() >> 5) & 0xff) % 0x14) + 1) as u8);
    };
    if ch.u8(0x4d4) != 0 {
        if ch.u8(0x4e1) == 0 {
            let mut t = (rand.rand() >> 5) as i16;
            if t < 0 {
                t = t.wrapping_neg();
            }
            t = t % 100 - 0x1e;
            ch.set_i16(0x4de, t.max(0));
            timers(ch, rand);
        }
    } else {
        if ch.i16(0x4dc) == ch.i16(0x4de) && ch.i16(0x4de) == 0 {
            return None;
        }
        ch.set_i16(0x4de, 0);
        timers(ch, rand);
    }
    let (now, target) = (ch.i16(0x4dc), ch.i16(0x4de));
    ch.set_u8(0x4e1, ch.u8(0x4e1).wrapping_sub(1));
    let step = i16::from(ch.u8(0x4e3));
    if now != target {
        let next = if target < now { now.wrapping_sub(step) } else { now.wrapping_add(step) };
        let past = if target < now { next < target } else { target < next };
        ch.set_i16(0x4dc, if past { target } else { next });
    }
    if let Some(j) = eye.and_then(|e| usize::try_from(e).ok()).and_then(|e| joints.get_mut(e)) {
        let mut a = ((f64::from(ch.i16(0x4dc)) as f32 as f64) / 10.0) as f32;
        if block.2 != 0 {
            a = -a;
        }
        let r = super::wrap(((d(a) * d(super::BODY_PI)) / 180.0) as f32);
        j.rotation = block.3;
        if let Some(x) = j.rotation.get_mut(usize::from(block.1)) {
            *x = (d(*x) + d(r)) as f32;
        }
    }
    Some(i32::from(10 < ch.i16(0x4dc)))
}

/// The element loop: each element's matrix from its parent's and its values (`parents[i]` None: the root).
pub fn element_matrices(parents: &[Option<usize>], joints: &[Joint]) -> Vec<Mat> {
    element_matrices_with(parents, joints, None)
}

/// The element loop with element `neck.0` blended toward the neck matrix (after its own product).
pub fn element_matrices_with(
    parents: &[Option<usize>],
    joints: &[Joint],
    neck: Option<(usize, Mat, f32)>,
) -> Vec<Mat> {
    let mut out: Vec<Mat> = Vec::with_capacity(joints.len());
    for (i, j) in joints.iter().enumerate() {
        let mut l = IDENTITY;
        l[0] = j.scale[0];
        l[5] = j.scale[1];
        l[10] = j.scale[2];
        l = rot_x(&l, j.rotation[0]);
        l = rot_y(&l, j.rotation[1]);
        l = rot_z(&l, j.rotation[2]);
        l[12] = j.translation[0];
        l[13] = j.translation[1];
        l[14] = j.translation[2];
        let parent = parents.get(i).copied().flatten().and_then(|p| out.get(p).copied()).unwrap_or(IDENTITY);
        let mut e = mul(&parent, &l);
        if let Some((n, m, w)) = &neck
            && *n == i
        {
            neck_blend(&mut e, m, *w);
        }
        out.push(e);
    }
    out
}

/// The world matrix of a body standing on its own (no parent and no matrix of its own): `b30` / `b38`
/// the model block's +0x30 / +0x38, `tilt` the X turn when +0x160 is set, `place` the position plus the draw
/// offset.
pub fn body_world(
    b30: f32,
    b38: f32,
    scale: [f32; 3],
    tilt: Option<f32>,
    facing: f32,
    place: [f32; 3],
) -> Mat {
    // The constants are single-precision values held as doubles (0.01f, 1.570796f).
    let s = ((d(b30) / d(b38)) * d(0.01)) as f32;
    let mut m = IDENTITY;
    m[0] = (d(s) * d(scale[0])) as f32;
    m[5] = (d(scale[1]) * d(s)) as f32;
    m[10] = (d(s) * d(scale[2])) as f32;
    if let Some(t) = tilt {
        m = rot_x(&m, t);
    }
    // 1.570796 as a float.
    let turn = super::wrap((d(-facing) - d(f32::from_bits(0x3fc9_0fd8))) as f32);
    m = rot_y(&m, turn);
    m[12] = place[0];
    m[13] = place[1];
    m[14] = place[2];
    m
}

/// The world matrix of a body hung on another body's element (+0x19c set): the element's matrix scaled
/// by (1 / the other's block +0x30) · +0x1b4, its translation × 1000 and moved by the offset (+0x1a4.., × the
/// other's block +0x30), in the other's world.
pub fn hung_world(
    parent_world: &Mat,
    parent_elem: &Mat,
    parent_b30: f32,
    scale_1b4: f32,
    off: [f32; 4],
) -> Mat {
    let k = ((1.0 / d(parent_b30)) * d(scale_1b4)) as f32;
    let mut l = IDENTITY;
    l[0] = k;
    l[5] = k;
    l[10] = k;
    let mut m = mul(parent_elem, &l);
    let v = [
        (d(parent_b30) * d(off[0])) as f32,
        (d(parent_b30) * d(off[1])) as f32,
        (d(parent_b30) * d(off[2])) as f32,
        off[3],
    ];
    for t in &mut m[12..15] {
        *t = (d(*t) * 1000.0) as f32;
    }
    let p = apply4(&m, v);
    m[12] = p[0];
    m[13] = p[1];
    m[14] = p[2];
    mul(parent_world, &m)
}

/// The world matrix of element `e` (none past the last); `listed` (the element is in the relation's list)
/// takes its translation × 1000 first.
pub fn element_matrix(world: &Mat, elems: &[Mat], e: usize, listed: bool) -> Option<Mat> {
    let mut m = elems.get(e).copied()?;
    if listed {
        for t in &mut m[12..15] {
            *t = (d(*t) * 1000.0) as f32;
        }
    }
    Some(mul(world, &m))
}

/// The world position of element `e` (the body's own past the last element).
pub fn element_position(world: &Mat, elems: &[Mat], e: usize, listed: bool) -> [f32; 3] {
    let w = element_matrix(world, elems, e, listed).unwrap_or(*world);
    [w[12], w[13], w[14]]
}

/// The option points (0x16 of x, y, z, w): each option position at its element (translation × 1000,
/// then in the world); kind 0x4000 entries add their offset scaled by (10000 / b30) / b30 · b38.
pub fn option_points(
    world: &Mat,
    elems: &[Mat],
    entries: &[(u16, i16, [f32; 3])],
    b30: f32,
    b38: f32,
) -> [[f32; 4]; 0x16] {
    let mut pts = [[0.0f32; 4]; 0x16];
    let k = (((10000.0 / d(b30)) / d(b30)) * d(b38)) as f32;
    for &(key, elem, v) in entries {
        let i = usize::from(key & 0x3fff);
        let mut s = IDENTITY;
        s[15] = 1000.0;
        // (A negative element reads outside the list in the original; none of the data has one.)
        let e = usize::try_from(elem).ok().and_then(|e| elems.get(e)).copied().unwrap_or(IDENTITY);
        let mut m = mul(&e, &s);
        m[15] = 1.0;
        let m = mul(world, &m);
        let p = match key & 0xc000 {
            0 | 0x8000 => Some([m[12], m[13], m[14]]),
            0x4000 => {
                let off = [(d(k) * d(v[0])) as f32, (d(v[1]) * d(k)) as f32, (d(k) * d(v[2])) as f32, 1.0];
                let p = apply4(&m, off);
                Some([p[0], p[1], p[2]])
            }
            _ => None,
        };
        if let Some(slot) = pts.get_mut(i) {
            if let Some(p) = p {
                slot[..3].copy_from_slice(&p);
            }
            slot[3] = 1.0;
        }
    }
    pts
}

/// Option point `part` when it is set (or the first one when that has a w), else the body's
/// position (+0x200, w 1).
pub fn point(pts: &[[f32; 4]; 0x16], part: usize, body: [f32; 3]) -> [f32; 4] {
    if let Some(p) = pts.get(part)
        && p.iter().any(|&x| x != 0.0)
    {
        return *p;
    }
    if pts[0][3] == 0.0 { [body[0], body[1], body[2], 1.0] } else { pts[0] }
}

/// The runtime's double functions on single values, rounded back to single.
fn f(x: f64) -> f32 {
    x as f32
}

/// `v` normalised (x, y, z; w kept), or all zero when its squared length is NaN, infinite or
/// not above the smallest normal float.
pub fn normalize(v: [f32; 4]) -> [f32; 4] {
    let l2 = f((d(v[0]) * d(v[0]) + d(v[1]) * d(v[1])) + d(v[2]) * d(v[2]));
    if l2.is_nan() || !l2.is_finite() || l2 <= f32::MIN_POSITIVE {
        return [0.0; 4];
    }
    let inv = f(1.0 / d(f(d(l2).sqrt())));
    [f(d(v[0]) * d(inv)), f(d(v[1]) * d(inv)), f(d(inv) * d(v[2])), v[3]]
}

/// `a × b` (x, y, z; `w` kept from the destination).
fn cross(a: [f32; 4], b: [f32; 4], w: f32) -> [f32; 4] {
    [
        f(d(a[1]) * d(b[2]) - d(a[2]) * d(b[1])),
        f(d(a[2]) * d(b[0]) - d(a[0]) * d(b[2])),
        f(d(a[0]) * d(b[1]) - d(a[1]) * d(b[0])),
        w,
    ]
}

/// A turn matrix whose X column points along `v`: f = |v|, s = |up × f| (up = (0, −1, 0)), u = |s × f|;
/// columns f, u, s.
pub fn look_matrix(v: [f32; 3]) -> Mat {
    let up = [0.0, -1.0, 0.0, 0.0];
    let fw = normalize([v[0], v[1], v[2], 1.0]);
    let side = normalize(cross(up, fw, 1.0));
    let upv = normalize(cross(side, fw, side[3]));
    let mut m = IDENTITY;
    m[..3].copy_from_slice(&fw[..3]);
    m[4..7].copy_from_slice(&upv[..3]);
    m[8..11].copy_from_slice(&side[..3]);
    m
}

/// A turn matrix as a quaternion (x, y, z, w).
pub fn quat_from(m: &Mat) -> [f32; 4] {
    let tr = f((d(m[0]) + d(m[5])) + d(m[10]));
    if tr > 0.0 {
        let s = f((d(tr) + 1.0).sqrt());
        let k = f(0.5 / d(s));
        return [
            f((d(m[6]) - d(m[9])) * d(k)),
            f((d(m[8]) - d(m[2])) * d(k)),
            f((d(m[1]) - d(m[4])) * d(k)),
            f(d(s) * 0.5),
        ];
    }
    let next = [1usize, 2, 0];
    let mut i = usize::from(m[0] < m[5]);
    if m[i * 5] < m[10] {
        i = 2;
    }
    let (j, k) = (next[i], next[next[i]]);
    let s = f(((d(m[i * 5]) - (d(m[k * 5]) + d(m[j * 5]))) + 1.0).sqrt());
    let mut q = [0.0f32; 4];
    q[i] = f(d(s) * 0.5);
    let kf = if s == 0.0 { s } else { f(0.5 / d(s)) };
    q[3] = f((d(m[k + j * 4]) - d(m[j + k * 4])) * d(kf));
    q[j] = f((d(m[i + j * 4]) + d(m[j + i * 4])) * d(kf));
    q[k] = f((d(m[i + k * 4]) + d(m[k + i * 4])) * d(kf));
    q
}

/// The slerp from `a` toward `b` by `t` (`b` flipped onto `a`'s side; linear within 0.0001).
pub fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut c = f(((d(a[0]) * d(b[0]) + d(a[1]) * d(b[1])) + d(a[2]) * d(b[2])) + d(a[3]) * d(b[3]));
    let mut b = b;
    if c < 0.0 {
        c = -c;
        b = b.map(|x| -x);
    }
    let (k0, k1) = if 1.0 - d(c) <= 0.0001 {
        (f(1.0 - d(t)), t)
    } else {
        let th = f(d(c).acos());
        let s = f(d(th).sin());
        let k0 = f(d(f(((1.0 - d(t)) * d(th)).sin())) / d(s));
        let k1 = f(d(f((d(th) * d(t)).sin())) / d(s));
        (k0, k1)
    };
    std::array::from_fn(|i| f(d(k0) * d(a[i]) + d(k1) * d(b[i])))
}

/// A quaternion as a turn matrix.
pub fn mat_from_quat(q: [f32; 4]) -> Mat {
    let (x, y, z, w) = (d(q[0]), d(q[1]), d(q[2]), d(q[3]));
    let (x2, y2, z2) = (d(f(x + x)), d(f(y + y)), d(f(z + z)));
    let (xx, xy, xz) = (d(f(x * x2)), d(f(x * y2)), d(f(x * z2)));
    let (yy, yz, zz) = (d(f(y * y2)), d(f(y * z2)), d(f(z * z2)));
    let (wx, wy, wz) = (d(f(x2 * w)), d(f(y2 * w)), d(f(z2 * w)));
    let mut m = [0.0f32; 16];
    m[0] = f(1.0 - (zz + yy));
    m[4] = f(xy - wz);
    m[8] = f(xz + wy);
    m[1] = f(xy + wz);
    m[5] = f(1.0 - (zz + xx));
    m[9] = f(yz - wx);
    m[2] = f(xz - wy);
    m[6] = f(yz + wx);
    m[10] = f(1.0 - (xx + yy));
    m[15] = 1.0;
    m
}

/// An element's matrix toward the neck's by `weight` (1: the neck's turn), its translation kept.
pub fn neck_blend(e: &mut Mat, neck: &Mat, weight: f32) {
    if weight == 0.0 {
        return;
    }
    let t = [e[12], e[13], e[14]];
    if weight == 1.0 {
        *e = *neck;
    } else {
        let q = slerp(quat_from(e), quat_from(neck), weight);
        *e = mat_from_quat(q);
    }
    e[12] = t[0];
    e[13] = t[1];
    e[14] = t[2];
}

/// A matrix kept in a character record.
pub fn mat_at(ch: &super::Chr, at: u32) -> Mat {
    std::array::from_fn(|i| ch.f32(at + 4 * i as u32))
}

pub fn set_mat(ch: &mut super::Chr, at: u32, m: &Mat) {
    for (i, v) in m.iter().enumerate() {
        ch.set_f32(at + 4 * i as u32, *v);
    }
}

fn deg(x: f32) -> f32 {
    f((d(x) * 180.0) / d(super::BODY_PI))
}

fn atan2(y: f32, x: f32) -> f32 {
    f(d(y).atan2(d(x)))
}

/// What the neck step needs besides the record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NeckInput {
    /// The body's option points 0x13 / 0x14 / 9 (from the last batch), as the option point lookup gives them.
    pub p13: Option<[f32; 4]>,
    pub p14: Option<[f32; 4]>,
    pub p9: [f32; 4],
    /// The target point (modes 1–3; mode 2 None when the target body is gone).
    pub target: Option<[f32; 4]>,
    /// The block's +0x40.
    pub side: u8,
}

/// The neck's weight eased toward +0x414, then by mode
/// (+0x40d: 1 the camera, 2 a body, 3 a point, 4 fixed angles) its matrix +0x46c; returns the matrix and weight
/// element 9 blends toward (none: cleared).
pub fn neck(ch: &mut super::Chr, inp: &NeckInput) -> Option<(Mat, f32)> {
    let w = f(((d(ch.f32(0x414)) - d(ch.f32(0x418))) / d(ch.f32(0x420))) + d(ch.f32(0x418)));
    ch.set_f32(0x418, w);
    if d(w) > d(0.99) {
        ch.set_f32(0x418, 1.0);
    }
    if ch.f32(0x418) <= 0.01 {
        ch.set_f32(0x418, 0.0);
    }
    if ch.f32(0x418) == 0.0 {
        return None;
    }
    let facing = ch.f32(super::FACING);
    let half_pi = super::wrap(f32::from_bits(0x3fc9_0fd8));
    let mode = ch.i8(0x40d);
    if mode == 4 {
        let m = rot_z(&IDENTITY, ch.f32(0x4c4));
        let m = rot_y(&m, ch.f32(0x4c0));
        set_mat(ch, 0x46c, &m);
    } else {
        // (A mode outside 1–4 reads an unset target in the original; it is taken as the origin here.)
        let target = if (1..=3).contains(&mode) { inp.target? } else { [0.0; 4] };
        let mut b = 0.0f32;
        if let (Some(p13), Some(p14)) = (inp.p13, inp.p14) {
            let dx = f(d(p13[0]) - d(p14[0]));
            let dz = f(d(p13[2]) - d(p14[2]));
            let a = atan2(dz, dx);
            b = f((d(a) + d(half_pi)) - d(facing));
            b = deg(super::wrap(b));
        }
        let dv = normalize([
            f(d(inp.p9[0]) - d(target[0])),
            f(d(inp.p9[1]) - d(target[1])),
            f(d(inp.p9[2]) - d(target[2])),
            inp.p9[3],
        ]);
        let yaw = atan2(dv[2], dv[0]);
        let mut y = deg(super::wrap(f((d(yaw) - d(half_pi)) - d(facing))));
        let side = if ch.u32(0x4d0) == 0 { 15.0f32 } else { -15.0 };
        let lo = (d(side) - 90.0) + d(b);
        let hi1 = d(ch.f32(0x424)) + d(b);
        if lo < d(y) && d(y) < hi1 {
            ch.set_u32(0x4d0, 1);
            y = f(hi1);
        } else {
            let hi2 = d(ch.f32(0x428)) + d(b);
            if d(y) <= lo || hi2 < d(y) {
                ch.set_u32(0x4d0, 0);
                y = f(hi2);
            }
        }
        if y < -90.0 {
            y = f(d(y) + 360.0);
        }
        let cur = ch.f32(0x41c);
        let dd = d(y) - d(cur);
        let step = if dd.abs() <= 90.0 { dd / 1.5 } else { dd * 0.25 };
        ch.set_f32(0x41c, f(step + d(cur)));
        let r = super::wrap(f((d(ch.f32(0x41c)) * d(super::BODY_PI)) / 180.0));
        let vx = f(d(r).sin());
        let mut vz = f(d(r).cos());
        let h = f((d(dv[0]) * d(dv[0]) + d(dv[2]) * d(dv[2])).sqrt());
        let p = atan2(dv[1], h);
        let mut vy = f(d(p).sin());
        if inp.side == 1 {
            vz = -vz;
            vy = -vy;
        }
        set_mat(ch, 0x46c, &look_matrix([vx, vy, vz]));
    }
    let m = mul(&mat_at(ch, 0x42c), &mat_at(ch, 0x46c));
    set_mat(ch, 0x46c, &m);
    if ch.f32(0x4bc) != 0.0 {
        let q0 = [ch.f32(0x4ac), ch.f32(0x4b0), ch.f32(0x4b4), ch.f32(0x4b8)];
        let q = slerp(quat_from(&m), q0, ch.f32(0x4bc));
        set_mat(ch, 0x46c, &mat_from_quat(q));
        ch.set_f32(0x4bc, f(d(ch.f32(0x4bc)) - 0.25));
    }
    Some((mat_at(ch, 0x46c), ch.f32(0x418)))
}

/// At allocation: the neck limits (20°, 160°); with an element for option key 9, its base matrix +0x42c
/// = its parent's (the model at rest) × its own turns, the neck matrix +0x46c = it and its quaternion +0x4ac;
/// the neck's yaw +0x41c = 90°.
pub fn neck_init(ch: &mut super::Chr, parents: &[Option<usize>], joints: &[Joint], elem9: Option<usize>) {
    ch.set_f32(0x424, 20.0);
    ch.set_f32(0x428, 160.0);
    let Some(e) = elem9.filter(|&e| e < joints.len()) else { return };
    let elems = element_matrices(parents, joints);
    let j = &joints[e];
    let l = rot_z(&rot_y(&rot_x(&IDENTITY, j.rotation[0]), j.rotation[1]), j.rotation[2]);
    let parent = parents.get(e).copied().flatten().and_then(|p| elems.get(p)).copied().unwrap_or(IDENTITY);
    let m = mul(&parent, &l);
    set_mat(ch, 0x42c, &m);
    set_mat(ch, 0x46c, &m);
    let q = quat_from(&m);
    for (k, v) in q.iter().enumerate() {
        ch.set_f32(0x4ac + 4 * k as u32, *v);
    }
    ch.set_f32(0x41c, deg(super::wrap(f32::from_bits(0x3fc9_0fd8))));
}

/// One cofactor term of the engine's inverse: matrix entry `entry` times the product of entries `pair`.
type CofactorTerm = (usize, (usize, usize));

/// The engine's cofactors, in its order of terms: each is (the three added terms) − (the three subtracted
/// ones).
const COFACTORS: [([CofactorTerm; 3], [CofactorTerm; 3]); 16] = [
    ([(13, (6, 11)), (9, (7, 14)), (5, (10, 15))], [(5, (14, 11)), (9, (6, 15)), (13, (7, 10))]),
    ([(13, (3, 10)), (1, (14, 11)), (9, (2, 15))], [(13, (2, 11)), (1, (10, 15)), (9, (3, 14))]),
    ([(5, (3, 14)), (1, (6, 15)), (13, (2, 7))], [(5, (2, 15)), (1, (7, 14)), (13, (6, 3))]),
    ([(9, (6, 3)), (1, (7, 10)), (5, (2, 11))], [(9, (2, 7)), (1, (6, 11)), (5, (3, 10))]),
    ([(12, (7, 10)), (8, (6, 15)), (4, (14, 11))], [(12, (6, 11)), (8, (7, 14)), (4, (10, 15))]),
    ([(12, (2, 11)), (8, (3, 14)), (0, (10, 15))], [(12, (3, 10)), (8, (2, 15)), (0, (14, 11))]),
    ([(12, (6, 3)), (0, (7, 14)), (4, (2, 15))], [(12, (2, 7)), (0, (6, 15)), (4, (3, 14))]),
    ([(0, (6, 11)), (4, (3, 10)), (8, (2, 7))], [(0, (7, 10)), (4, (2, 11)), (8, (6, 3))]),
    ([(11, (5, 12)), (7, (13, 8)), (15, (4, 9))], [(11, (4, 13)), (7, (9, 12)), (15, (8, 5))]),
    ([(15, (1, 8)), (11, (0, 13)), (3, (9, 12))], [(15, (0, 9)), (3, (13, 8)), (11, (1, 12))]),
    ([(15, (0, 5)), (7, (1, 12)), (3, (4, 13))], [(15, (1, 4)), (7, (0, 13)), (3, (5, 12))]),
    ([(11, (1, 4)), (7, (0, 9)), (3, (8, 5))], [(11, (0, 5)), (7, (1, 8)), (3, (4, 9))]),
    ([(6, (9, 12)), (10, (4, 13)), (14, (8, 5))], [(10, (5, 12)), (6, (13, 8)), (14, (4, 9))]),
    ([(10, (1, 12)), (14, (0, 9)), (2, (13, 8))], [(2, (9, 12)), (10, (0, 13)), (14, (1, 8))]),
    ([(2, (5, 12)), (14, (1, 4)), (6, (0, 13))], [(2, (4, 13)), (14, (0, 5)), (6, (1, 12))]),
    ([(6, (1, 8)), (2, (4, 9)), (10, (0, 5))], [(2, (8, 5)), (10, (1, 4)), (6, (0, 9))]),
];

/// The engine's 4×4 inverse (sceVu0InversMatrix4x4, behind the identity when a column is all zero): the
/// cofactors over the determinant (all zero when it is 0). The original runs on the x87; here each pair
/// product and each cofactor is kept in single precision, the sums in double.
pub fn vu0_inverse(m: &Mat) -> Mat {
    if (0..4).any(|c| m[c * 4..c * 4 + 4].iter().all(|&v| v == 0.0)) {
        return IDENTITY;
    }
    let pair_product = |(i, j): (usize, usize)| d((d(m[i]) * d(m[j])) as f32);
    let terms = |list: &[CofactorTerm; 3]| {
        list.iter().map(|&(entry, pair)| d(m[entry]) * pair_product(pair)).fold(0.0, |sum, t| sum + t)
    };
    let cofactors: [f32; 16] = std::array::from_fn(|k| {
        let (added, subtracted) = &COFACTORS[k];
        (terms(added) - terms(subtracted)) as f32
    });
    let determinant = (((d(m[12]) * d(cofactors[3]) + d(m[8]) * d(cofactors[2])) + d(m[0]) * d(cofactors[0]))
        + d(m[4]) * d(cofactors[1])) as f32;
    let scale = if determinant == 0.0 { 0.0 } else { (1.0 / d(determinant)) as f32 };
    cofactors.map(|c| c * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_products() {
        let m = rot_y(&IDENTITY, 0.5);
        assert_eq!(mul(&IDENTITY, &m), m);
        assert_eq!(apply4(&IDENTITY, [1.0, 2.0, 3.0, 1.0]), [1.0, 2.0, 3.0, 1.0]);
    }
}
