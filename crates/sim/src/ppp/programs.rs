//! The particle programs, by the names the effects' program tables give them. Each works on the object's
//! program work (object + 0xa0 + the offsets of the reference's work pair: A first, B second) and the current
//! key of its track (+0 its time; then its values).
//!
//! Families (all skip their step while the engine is paused):
//! - rates: on the key's time B += the key's values, then A += B: floats from key +0x10 (pppAccele, pppMove,
//!   pppSclAccele, pppSclMove), integers from +0x10 (pppAngAccele, pppAngMove), four i16 from +8 (pppColAccele,
//!   pppColMove);
//! - values: on the key's time A += the key's values: floats (pppPoint, pppScale), integers (pppAngle), i16
//!   (pppColor).
//!
//! - random: a draw (symmetric: d₂ + d₁ or 2d₁, used as r·p − p; up: d₁ or (d₁ + d₂)/2 and down: the same
//!   negated, used as r·p), the second draw when the key's flag byte is set; kept in A (one value, or one per
//!   component for the S variants) and applied to the target the key names (+4: a work offset, or −1 the
//!   engine's scratch vector): float vectors (key +0x10, flag +0x20; drawn at age 0, applied then and on the
//!   key's time), one float (pppRandFloat: key +8, flag +0xc), integers (IV: key +0x10 as floats, flag +0x20;
//!   drawn and applied on the key's time, truncated), i16 (HCV: key +8, flag +0x10, likewise).
//!
//! Inits: the float ones set their vector (B for rates, A for values) to (0, 0, 0, 1); the integer ones to
//! zero; the i16 ones zero their 8 bytes at the vector plus the engine's scratch offset, which they then clear.

use super::{Program, Run};

/// The reference's two work offsets (A, B).
fn works(r: &Run, rf: u32) -> (u32, u32) {
    let pair = r.m.u32(rf + 0xc);
    (r.m.u32(pair), r.m.u32(pair + 4))
}

fn at(o: u32, w: u32) -> u32 {
    o.wrapping_add(w).wrapping_add(0xa0)
}

/// The value kinds: four floats or four integers at key +0x10, four i16 at key +8.
#[derive(Clone, Copy)]
enum Kind {
    Float,
    Int,
    Short,
}

fn add(r: &mut Run, kind: Kind, dst: u32, src: u32) {
    match kind {
        Kind::Float => {
            for i in 0..4 {
                let v = r.m.f32(src + 4 * i) + r.m.f32(dst + 4 * i);
                r.m.set_f32(dst + 4 * i, v);
            }
        }
        Kind::Int => {
            for i in 0..4 {
                let v = r.m.u32(dst + 4 * i).wrapping_add(r.m.u32(src + 4 * i));
                r.m.set_u32(dst + 4 * i, v);
            }
        }
        Kind::Short => {
            for i in 0..4 {
                let v = r.m.u16(dst + 2 * i).wrapping_add(r.m.u16(src + 2 * i));
                r.m.set_u16(dst + 2 * i, v);
            }
        }
    }
}

fn key_values(kind: Kind, key: u32) -> u32 {
    match kind {
        Kind::Short => key + 8,
        _ => key + 0x10,
    }
}

/// A rate: B += the key's values on its time, then A += B.
fn rate(r: &mut Run, kind: Kind, o: u32, key: u32, rf: u32) {
    let (a, b) = works(r, rf);
    if r.paused {
        return;
    }
    let (a, b) = (at(o, a), at(o, b));
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        add(r, kind, b, key_values(kind, key));
    }
    add(r, kind, a, b);
}

/// A value: A += the key's values on its time.
fn value(r: &mut Run, kind: Kind, o: u32, key: u32, rf: u32) {
    if r.paused || r.m.u32(key) != r.m.u32(o + 0xc) {
        return;
    }
    let (a, _) = works(r, rf);
    add(r, kind, at(o, a), key_values(kind, key));
}

fn rate_f(r: &mut Run, o: u32, k: u32, rf: u32) {
    rate(r, Kind::Float, o, k, rf)
}
fn rate_i(r: &mut Run, o: u32, k: u32, rf: u32) {
    rate(r, Kind::Int, o, k, rf)
}
fn rate_s(r: &mut Run, o: u32, k: u32, rf: u32) {
    rate(r, Kind::Short, o, k, rf)
}
fn value_f(r: &mut Run, o: u32, k: u32, rf: u32) {
    value(r, Kind::Float, o, k, rf)
}
fn value_i(r: &mut Run, o: u32, k: u32, rf: u32) {
    value(r, Kind::Int, o, k, rf)
}
fn value_s(r: &mut Run, o: u32, k: u32, rf: u32) {
    value(r, Kind::Short, o, k, rf)
}

#[derive(Clone, Copy)]
enum Draw {
    Sym,
    Up,
    Down,
}

fn draw(r: &mut Run, d: Draw, second: bool) -> f32 {
    let d1 = r.vu_draw();
    match d {
        Draw::Sym if second => r.vu_draw() + d1,
        Draw::Sym => d1 + d1,
        Draw::Up if second => ((f64::from(r.vu_draw()) + f64::from(d1)) * 0.5) as f32,
        Draw::Up => d1,
        Draw::Down if second => ((f64::from(-d1) - f64::from(r.vu_draw())) * 0.5) as f32,
        Draw::Down => -d1,
    }
}

/// The key's target: a work offset of the object, or (−1) the engine's scratch vector.
fn target(r: &Run, o: u32, key: u32) -> u32 {
    match r.m.u32(key + 4) {
        0xffff_ffff => super::SCRATCH_VECTOR,
        w => at(o, w),
    }
}

/// The C runtime's float to integer conversion (truncation).
fn ftol(x: f64) -> i64 {
    x.trunc() as i64
}

/// The random target kinds: float vectors, integer vectors (3), i16 vectors (4), one float; `each` draws per
/// component (the S variants).
#[derive(Clone, Copy, PartialEq)]
enum RKind {
    FloatVec,
    IntVec,
    ShortVec,
    Float,
}

fn random(r: &mut Run, d: Draw, kind: RKind, each: bool, o: u32, key: u32, rf: u32) {
    if r.paused {
        return;
    }
    let (n, flag_at) = match kind {
        RKind::FloatVec => (3, 0x20),
        RKind::IntVec => (3, 0x20),
        RKind::ShortVec => (4, 0x10),
        RKind::Float => (1, 0xc),
    };
    let (a, _) = works(r, rf);
    let a = at(o, a);
    let second = r.m.u8(key + flag_at) != 0;
    let draws_at_birth = matches!(kind, RKind::FloatVec | RKind::Float);
    let age = r.m.u32(o + 0xc);
    let fresh = if draws_at_birth { age == 0 } else { r.m.u32(key) == age };
    if !fresh && (!draws_at_birth || r.m.u32(key) != age) {
        return;
    }
    if fresh {
        let k = if each { n } else { 1 };
        for i in 0..k {
            let v = draw(r, d, second);
            r.m.set_f32(a + 4 * i, v);
        }
    }
    let dst = target(r, o, key);
    for i in 0..n {
        let rv = f64::from(r.m.f32(a + if each { 4 * i } else { 0 }));
        match kind {
            RKind::FloatVec => {
                let p = f64::from(r.m.f32(key + 0x10 + 4 * i));
                let t = match d {
                    Draw::Sym => (rv * p - p) as f32,
                    _ => (p * rv) as f32,
                };
                let v = t + r.m.f32(dst + 4 * i);
                r.m.set_f32(dst + 4 * i, v);
            }
            RKind::Float => {
                let p = f64::from(r.m.f32(key + 8));
                let v = (rv * p - p + f64::from(r.m.f32(dst))) as f32;
                r.m.set_f32(dst, v);
            }
            RKind::IntVec => {
                let q = f64::from(r.m.u32(key + 0x10 + 4 * i) as i32 as f32);
                let t = match d {
                    Draw::Sym => ftol(rv * q - q),
                    _ => ftol(q * rv),
                };
                let v = r.m.u32(dst + 4 * i).wrapping_add(t as u32);
                r.m.set_u32(dst + 4 * i, v);
            }
            RKind::ShortVec => {
                let q = f64::from(r.m.u16(key + 8 + 2 * i) as i16);
                let t = match d {
                    Draw::Sym => ftol(rv * q - q),
                    _ => ftol(q * rv),
                };
                let v = r.m.u16(dst + 2 * i).wrapping_add(t as u16);
                r.m.set_u16(dst + 2 * i, v);
            }
        }
    }
}

macro_rules! rand_fn {
    ($name:ident, $d:expr, $k:expr, $each:expr) => {
        fn $name(r: &mut Run, o: u32, key: u32, rf: u32) {
            random(r, $d, $k, $each, o, key, rf)
        }
    };
}
rand_fn!(rand_fv, Draw::Sym, RKind::FloatVec, false);
rand_fn!(rand_up_fv, Draw::Up, RKind::FloatVec, false);
rand_fn!(rand_down_fv, Draw::Down, RKind::FloatVec, false);
rand_fn!(srand_fv, Draw::Sym, RKind::FloatVec, true);
rand_fn!(srand_up_fv, Draw::Up, RKind::FloatVec, true);
rand_fn!(srand_down_fv, Draw::Down, RKind::FloatVec, true);
rand_fn!(rand_float, Draw::Sym, RKind::Float, false);
rand_fn!(rand_iv, Draw::Sym, RKind::IntVec, false);
rand_fn!(rand_up_iv, Draw::Up, RKind::IntVec, false);
rand_fn!(rand_down_iv, Draw::Down, RKind::IntVec, false);
rand_fn!(rand_hcv, Draw::Sym, RKind::ShortVec, false);
rand_fn!(rand_up_hcv, Draw::Up, RKind::ShortVec, false);
rand_fn!(rand_down_hcv, Draw::Down, RKind::ShortVec, false);
rand_fn!(srand_hcv, Draw::Sym, RKind::ShortVec, true);
rand_fn!(srand_up_hcv, Draw::Up, RKind::ShortVec, true);
rand_fn!(srand_down_hcv, Draw::Down, RKind::ShortVec, true);

// The matrix programs: the object's matrix (+0x10, 16 floats, the translation at +0x40) built from the
// reference's works (A the translation, B three angles in 1/0x10000 turns, C the scale).

type Mat = crate::chr::elements::Mat;

/// The engine's turn matrices about X / Y / Z for an angle (the sine table at angle >> 4).
fn turn(axis: u8, angle: u32) -> Mat {
    let t = crate::traced_tables::sine();
    let s = t[(angle >> 4 & 0xfff) as usize];
    let c = t[(angle.wrapping_add(0x4000) >> 4 & 0xfff) as usize];
    let mut m = [0.0; 16];
    m[15] = 1.0;
    match axis {
        b'x' => {
            m[0] = 1.0;
            m[5] = c;
            m[6] = s;
            m[9] = -s;
            m[10] = c;
        }
        b'y' => {
            m[0] = c;
            m[2] = -s;
            m[5] = 1.0;
            m[8] = s;
            m[10] = c;
        }
        _ => {
            m[0] = c;
            m[1] = s;
            m[4] = -s;
            m[5] = c;
            m[10] = 1.0;
        }
    }
    m
}

fn get_mat(r: &Run, at: u32) -> Mat {
    let mut m = [0.0; 16];
    for (i, v) in m.iter_mut().enumerate() {
        *v = r.m.f32(at + 4 * i as u32);
    }
    m
}

fn put_mat(r: &mut Run, at: u32, m: &Mat) {
    for (i, v) in m.iter().enumerate() {
        r.m.set_f32(at + 4 * i as u32, *v);
    }
}

/// The three works of a matrix reference.
fn works3(r: &Run, rf: u32) -> (u32, u32, u32) {
    let pair = r.m.u32(rf + 0xc);
    (r.m.u32(pair), r.m.u32(pair + 4), r.m.u32(pair + 8))
}

/// The engine's matrix set-ups by angle order (0 XYZ, 1 XZY, 2 YXZ, 3 YZX, 4 ZXY, 5 ZYX; others XYZ): the turn
/// matrix of the three angles.
pub(crate) fn set_matrix(order: u8, ang: [u32; 3]) -> Mat {
    let o = match order {
        1 => *b"yzx",
        2 => *b"zxy",
        3 => *b"xzy",
        4 => *b"yxz",
        5 => *b"xyz",
        _ => *b"zyx",
    };
    turns(o, ang)
}

/// The turn matrix for angles (x, y, z) with the turns multiplied in `order` (the first two, then the third).
fn turns(order: [u8; 3], ang: [u32; 3]) -> Mat {
    let a = |ax: u8| ang[usize::from(ax - b'x')];
    let first = crate::chr::elements::mul(&turn(order[0], a(order[0])), &turn(order[1], a(order[1])));
    crate::chr::elements::mul(&first, &turn(order[2], a(order[2])))
}

fn matrix(r: &mut Run, order: [u8; 3], o: u32, rf: u32) {
    let (wa, wb, wc) = works3(r, rf);
    let (wa, wb, wc) = (at(o, wa), at(o, wb), at(o, wc));
    let ang = [r.m.u32(wb), r.m.u32(wb + 4), r.m.u32(wb + 8)];
    let mut m = turns(order, ang);
    let sc = [r.m.f32(wc), r.m.f32(wc + 4), r.m.f32(wc + 8)];
    for (row, k) in sc.iter().enumerate() {
        for c in 0..3 {
            m[row * 4 + c] *= k;
        }
    }
    for i in 0..3 {
        m[12 + i] = r.m.f32(wa + 4 * i as u32);
    }
    put_mat(r, o + 0x10, &m);
}

macro_rules! mat_fn {
    ($name:ident, $order:expr) => {
        fn $name(r: &mut Run, o: u32, _key: u32, rf: u32) {
            matrix(r, $order, o, rf)
        }
    };
}
// (Multiplication orders, from the names: XYZ = Z · Y · X, and so on.)
mat_fn!(matrix_xyz, *b"zyx");
mat_fn!(matrix_xzy, *b"yzx");
mat_fn!(matrix_yxz, *b"zxy");
mat_fn!(matrix_yzx, *b"xzy");
mat_fn!(matrix_zxy, *b"yxz");
mat_fn!(matrix_zyx, *b"xyz");

/// PppMatrixLoc: the unit matrix with the translation A.
fn matrix_loc(r: &mut Run, o: u32, _key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let wa = at(o, wa);
    let mut m = crate::chr::elements::IDENTITY;
    for i in 0..3 {
        m[12 + i] = r.m.f32(wa + 4 * i as u32);
    }
    put_mat(r, o + 0x10, &m);
}

/// PppMatrixScl: the unit matrix with the scale B on its diagonal and the translation A.
fn matrix_scl(r: &mut Run, o: u32, _key: u32, rf: u32) {
    let (wa, wb) = works(r, rf);
    let (wa, wb) = (at(o, wa), at(o, wb));
    let mut m = crate::chr::elements::IDENTITY;
    m[0] = r.m.f32(wb);
    m[5] = r.m.f32(wb + 4);
    m[10] = r.m.f32(wb + 8);
    for i in 0..3 {
        m[12 + i] = r.m.f32(wa + 4 * i as u32);
    }
    put_mat(r, o + 0x10, &m);
}

/// PppParMatrix: with a target in the key, the object's translation through its parent's matrix (object
/// +4): each row's three products summed and stored, then the translation added.
fn par_matrix(r: &mut Run, o: u32, key: u32, _rf: u32) {
    if r.m.u32(key + 4) == 0xffff_ffff {
        return;
    }
    let parent = r.m.u32(o + 4);
    let p = [r.m.f32(o + 0x40), r.m.f32(o + 0x44), r.m.f32(o + 0x48)].map(f64::from);
    let pm = get_mat(r, parent + 0x10);
    for i in 0..4 {
        let d = |k: usize| f64::from(pm[k]);
        let t = (p[2] * d(8 + i) + p[1] * d(4 + i) + p[0] * d(i)) as f32;
        let v = (d(12 + i) + f64::from(t)) as f32;
        r.m.set_f32(o + 0x40 + 4 * i as u32, v);
    }
}

fn vec4(r: &Run, at: u32) -> [f32; 4] {
    [r.m.f32(at), r.m.f32(at + 4), r.m.f32(at + 8), r.m.f32(at + 12)]
}

fn put_vec4(r: &mut Run, at: u32, v: [f32; 4]) {
    for (i, x) in v.into_iter().enumerate() {
        r.m.set_f32(at + 4 * i as u32, x);
    }
}

/// The rows of `m` scaled by `k` (x, y, z), their fourth elements kept.
fn scale_rows(m: &mut Mat, k: [f32; 3]) {
    for (row, s) in k.iter().enumerate() {
        for c in 0..3 {
            m[row * 4 + c] *= s;
        }
    }
}

/// PppKeParMatR: the parent's (object +4) turn without its scale, then the object's translation through the
/// parent's whole matrix.
fn ke_par_mat_r(r: &mut Run, o: u32, _key: u32, _rf: u32) {
    use crate::chr::elements::{apply4, mul, normalize};
    let parent = r.m.u32(o + 4);
    let pm = get_mat(r, parent + 0x10);
    let row = |k: usize| normalize([pm[k * 4], pm[k * 4 + 1], pm[k * 4 + 2], pm[k * 4 + 3]]);
    let mut n = [0.0; 16];
    for k in 0..3 {
        n[k * 4..k * 4 + 4].copy_from_slice(&row(k));
    }
    let v = apply4(&pm, vec4(r, o + 0x40));
    let m = mul(&n, &get_mat(r, o + 0x10));
    put_mat(r, o + 0x10, &m);
    put_vec4(r, o + 0x40, v);
}

/// PppKeOfsMatXYZ: the XYZ turn of B scaled by C, about the offset A +0x10 + A +0x20: the translation is A plus
/// the offset less the turned offset.
fn ke_ofs_mat_xyz(r: &mut Run, o: u32, _key: u32, rf: u32) {
    use crate::chr::elements::apply4;
    let (wa, wb, wc) = works3(r, rf);
    let (wa, wb, wc) = (at(o, wa), at(o, wb), at(o, wc));
    let mut m = turns(*b"zyx", [r.m.u32(wb), r.m.u32(wb + 4), r.m.u32(wb + 8)]);
    scale_rows(&mut m, [r.m.f32(wc), r.m.f32(wc + 4), r.m.f32(wc + 8)]);
    m[12] = 0.0;
    m[13] = 0.0;
    m[14] = 0.0;
    m[15] = 1.0;
    let (p, q) = (vec4(r, wa + 0x10), vec4(r, wa + 0x20));
    let off = [p[0] + q[0], p[1] + q[1], p[2] + q[2], p[3] + q[3]];
    let t = apply4(&m, off);
    for i in 0..3 {
        let v = ((f64::from(r.m.f32(wa + 4 * i as u32)) + f64::from(off[i])) - f64::from(t[i])) as f32;
        m[12 + i] = v;
    }
    m[15] = 1.0;
    put_mat(r, o + 0x10, &m);
}

/// PppKeMatSN: the XYZ turn of the change of the angles B since the last frame (kept at A +0x80) into the
/// matrix at A +0x40, its first two translation words then zero.
fn ke_mat_sn(r: &mut Run, o: u32, _key: u32, rf: u32) {
    let (wa, wb) = works(r, rf);
    let (wa, wb) = (at(o, wa), at(o, wb));
    let prev = [r.m.u32(wa + 0x80), r.m.u32(wa + 0x84), r.m.u32(wa + 0x88)];
    let now = [r.m.u32(wb), r.m.u32(wb + 4), r.m.u32(wb + 8)];
    for (i, v) in now.iter().enumerate() {
        r.m.set_u32(wa + 0x80 + 4 * i as u32, *v);
    }
    let d = [prev[0].wrapping_sub(now[0]), prev[1].wrapping_sub(now[1]), prev[2].wrapping_sub(now[2])];
    let m = turns(*b"zyx", d);
    put_mat(r, wa + 0x40, &m);
    r.m.set_u32(wa + 0x70, 0);
    r.m.set_u32(wa + 0x74, 0);
}

fn ke_mat_sn_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    set(r, at(o, wa) + 0x80, &[0; 3]);
}

/// PppKeMatPht: the XYZ-ordered (X · Y · Z) turn of B scaled by C with the translation A, kept in work D; the
/// object's matrix is C on the diagonal with D's translation.
fn ke_mat_pht(r: &mut Run, o: u32, _key: u32, rf: u32) {
    let pair = r.m.u32(rf + 0xc);
    let (wa, wb, wc, wd) = (r.m.u32(pair), r.m.u32(pair + 4), r.m.u32(pair + 8), r.m.u32(pair + 12));
    let (wa, wb, wc, wd) = (at(o, wa), at(o, wb), at(o, wc), at(o, wd));
    let mut m = turns(*b"xyz", [r.m.u32(wb), r.m.u32(wb + 4), r.m.u32(wb + 8)]);
    let k = [r.m.f32(wc), r.m.f32(wc + 4), r.m.f32(wc + 8)];
    scale_rows(&mut m, k);
    for i in 0..3 {
        m[12 + i] = r.m.f32(wa + 4 * i as u32);
    }
    put_mat(r, wd, &m);
    let mut om = crate::chr::elements::IDENTITY;
    om[0] = k[0];
    om[5] = k[1];
    om[10] = k[2];
    put_mat(r, o + 0x10, &om);
    let t = vec4(r, wd + 0x30);
    put_vec4(r, o + 0x40, t);
}

// Spawners: objects made in another sub-part slot of the running part (the key names it), parented to the
// spawning object (child +4).

/// A child in slot `index` of the running part, parented to `o` (0 when the pools are full).
fn child(r: &mut Run, o: u32, index: u32) -> u32 {
    let slot = r.slots.wrapping_add(index.wrapping_mul(0x10));
    let c = super::create(r, slot);
    if c != 0 {
        r.m.set_u32(c + 4, o);
    }
    c
}

/// PppPointAp: every key +0xc frames (countdown byte at B +1), a child in slot key +4 at A's position (into the
/// child's work at key +8); no child for −1.
fn point_ap(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, wb) = works(r, rf);
    if r.paused {
        return;
    }
    let (wa, cd) = (at(o, wa), at(o, wb) + 1);
    if r.m.u8(cd) == 0 {
        let slot = r.m.u32(key + 4);
        if slot == 0xffff_ffff {
            return;
        }
        let c = child(r, o, slot);
        if c == 0 {
            return;
        }
        let w = at(c, r.m.u32(key + 8));
        for i in 0..3 {
            let v = r.m.u32(wa + 4 * i);
            r.m.set_u32(w + 4 * i, v);
        }
        let n = r.m.u8(key + 0xc);
        r.m.set_u8(cd, n);
    }
    let n = r.m.u8(cd).wrapping_sub(1);
    r.m.set_u8(cd, n);
}

fn point_ap_init(r: &mut Run, o: u32, rf: u32) {
    let (_, wb) = works(r, rf);
    r.m.set_u8(at(o, wb) + 1, 0);
}

/// A child in slot key +0xc at a mesh vertex (three i16) through the spawner's matrix, into the child's
/// work at key +0x10.
fn vertex_child(r: &mut Run, o: u32, key: u32, vtx: u32) {
    use crate::chr::elements::apply4;
    let slot = r.m.u32(key + 0xc);
    if slot == 0xffff_ffff {
        return;
    }
    let c = child(r, o, slot);
    if c == 0 {
        return;
    }
    let v = [0, 2, 4].map(|k| f32::from(r.m.u16(vtx + k) as i16));
    let p = apply4(&get_mat(r, o + 0x10), [v[0], v[1], v[2], 1.0]);
    let w = at(c, r.m.u32(key + 0x10));
    for (i, x) in p.iter().take(3).enumerate() {
        r.m.set_f32(w + 4 * i as u32, *x);
    }
}

/// PppVertexAp: every key +7 frames (countdown u16 at A +2), key +6 children at vertices of mesh key +4 (the
/// group's mesh list: its packet record's vertices, or the object's own at +0x90, and its index list): in
/// order from the cursor at A (mode key +8 = 0), or at random (mode 1).
fn vertex_ap(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let wa = at(o, wa);
    let mesh = r.m.u16(key + 4) as i16;
    if r.paused || mesh < 0 {
        return;
    }
    if r.m.u16(wa + 2) == 0 {
        let meshes = r.env.lists[2] + u32::from(mesh as u16) * 8;
        let verts = match r.m.u32(o + 0x90) {
            0 => {
                let rec = (r.m.u16(meshes) as i16 as i32 as u32).wrapping_mul(0x20);
                r.m.u32(r.env.lists[0].wrapping_add(rec) + 0x14)
            }
            v => v,
        };
        let count = r.m.u16(meshes + 2) as i16;
        let list = r.m.u32(meshes + 4);
        let n = r.m.u8(key + 6);
        match r.m.u8(key + 8) {
            0 => {
                for _ in 0..n {
                    if i32::from(count) <= i32::from(r.m.u16(wa)) {
                        r.m.set_u16(wa, 0);
                    }
                    let cur = r.m.u16(wa);
                    let idx = u32::from(r.m.u16(list + u32::from(cur) * 2));
                    r.m.set_u16(wa, cur.wrapping_add(1));
                    vertex_child(r, o, key, verts + idx * 6);
                }
            }
            1 => {
                for _ in 0..n {
                    let d = r.vu_draw();
                    let k = ftol(f64::from(count) * f64::from(d)) as u32;
                    let idx = u32::from(r.m.u16(list.wrapping_add(k.wrapping_mul(2))));
                    vertex_child(r, o, key, verts + idx * 6);
                }
            }
            _ => {}
        }
        let mut wait = u16::from(r.m.u8(key + 7));
        if r.vertex_ap_slow {
            wait = (f64::from(wait) * f64::from(1.8f32)).round_ties_even() as u16;
        }
        r.m.set_u16(wa + 2, wait);
    }
    let v = r.m.u16(wa + 2).wrapping_sub(1);
    r.m.set_u16(wa + 2, v);
}

fn vertex_ap_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    r.m.set_u32(at(o, wa), 0);
}

/// The group's mesh `mesh` (its 8-byte entry: packet record, vertex count, index list): its vertices (the
/// object's own at +0x90 first when `own`), count and list.
fn mesh(r: &Run, o: u32, mesh: i16, own: bool) -> (u32, i16, u32) {
    let e = r.env.lists[2].wrapping_add((mesh as i32 as u32).wrapping_mul(8));
    let rec = (r.m.u16(e) as i16 as i32 as u32).wrapping_mul(0x20);
    let verts = match if own { r.m.u32(o + 0x90) } else { 0 } {
        0 => r.m.u32(r.env.lists[0].wrapping_add(rec) + 0x14),
        v => v,
    };
    (verts, r.m.u16(e + 2) as i16, r.m.u32(e + 4))
}

fn vertex_at(r: &Run, verts: u32, idx: u32) -> [f32; 3] {
    [0, 2, 4].map(|k| f32::from(r.m.u16(verts + idx * 6 + k) as i16))
}

/// PppVertexApLc: pppVertexAp with the children at the vertex's own coordinates.
fn vertex_ap_lc(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let wa = at(o, wa);
    let m_ = r.m.u16(key + 4) as i16;
    if r.paused || m_ < 0 {
        return;
    }
    if r.m.u16(wa + 2) == 0 {
        let (verts, count, list) = mesh(r, o, m_, true);
        let n = r.m.u8(key + 6);
        let mode = r.m.u8(key + 8);
        for _ in 0..n {
            let idx = match mode {
                0 => {
                    if i32::from(count) <= i32::from(r.m.u16(wa)) {
                        r.m.set_u16(wa, 0);
                    }
                    let cur = r.m.u16(wa);
                    r.m.set_u16(wa, cur.wrapping_add(1));
                    u32::from(r.m.u16(list + u32::from(cur) * 2))
                }
                1 => {
                    let d = r.vu_draw();
                    let k = ftol(f64::from(count) * f64::from(d)) as u32;
                    u32::from(r.m.u16(list.wrapping_add(k.wrapping_mul(2))))
                }
                _ => break,
            };
            let v = vertex_at(r, verts, idx);
            let slot = r.m.u32(key + 0xc);
            if slot == 0xffff_ffff {
                continue;
            }
            let c = child(r, o, slot);
            if c == 0 {
                continue;
            }
            if r.vertex_lc_extend {
                let left = |r: &Run, x: u32| {
                    let def = r.m.u32(r.m.u32(x + 8));
                    (r.m.u32(def + 0x14).wrapping_sub(r.m.u32(x + 0xc)) as i32) / 0x1000
                };
                if left(r, o) <= left(r, c) {
                    let def = r.m.u32(r.m.u32(o + 8));
                    let e = r.m.u32(def + 0x14);
                    r.m.set_u32(def + 0x14, e.wrapping_add(0x1000));
                }
            }
            let w = at(c, r.m.u32(key + 0x10));
            for (i, x) in v.iter().enumerate() {
                r.m.set_f32(w + 4 * i as u32, *x);
            }
        }
        let wait = u16::from(r.m.u8(key + 7));
        r.m.set_u16(wa + 2, wait);
    }
    let v = r.m.u16(wa + 2).wrapping_sub(1);
    r.m.set_u16(wa + 2, v);
}

/// PppVertexApAt: pppVertexAp's timing; each child (slot key +0xc) gets a vertex number in its u16 work at
/// key +0x10: the cursor (mode 0) or a random one (mode 1).
fn vertex_ap_at(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let wa = at(o, wa);
    let m_ = r.m.u16(key + 4) as i16;
    if r.paused || m_ < 0 {
        return;
    }
    if r.m.u16(wa + 2) == 0 {
        let (_, count, _) = mesh(r, o, m_, false);
        let n = r.m.u8(key + 6);
        let mode = r.m.u8(key + 8);
        for _ in 0..n {
            let v = match mode {
                0 => {
                    if i32::from(count) <= i32::from(r.m.u16(wa)) {
                        r.m.set_u16(wa, 0);
                    }
                    let cur = r.m.u16(wa);
                    r.m.set_u16(wa, cur.wrapping_add(1));
                    cur
                }
                1 => {
                    let d = r.vu_draw();
                    ftol(f64::from(count) * f64::from(d)) as u16
                }
                _ => break,
            };
            let slot = r.m.u32(key + 0xc);
            if slot == 0xffff_ffff {
                continue;
            }
            let c = child(r, o, slot);
            if c != 0 {
                let w = at(c, r.m.u32(key + 0x10));
                r.m.set_u16(w, v);
            }
        }
        let wait = u16::from(r.m.u8(key + 7));
        r.m.set_u16(wa + 2, wait);
    }
    let v = r.m.u16(wa + 2).wrapping_sub(1);
    r.m.set_u16(wa + 2, v);
}

/// PppVertexAttend: B = the vertex (number in A's u16) of mesh key +0xc through the parent's matrix.
fn vertex_attend(r: &mut Run, o: u32, key: u32, rf: u32) {
    use crate::chr::elements::apply4;
    let m_ = r.m.u16(key + 0xc) as i16;
    if r.m.u32(key + 4) == 0xffff_ffff || m_ < 0 {
        return;
    }
    let (wa, wb) = works(r, rf);
    let parent = r.m.u32(o + 4);
    let (verts, _, list) = mesh(r, o, m_, false);
    let idx = u32::from(r.m.u16(list + u32::from(r.m.u16(at(o, wa))) * 2));
    let v = vertex_at(r, verts, idx);
    let p = apply4(&get_mat(r, parent + 0x10), [v[0], v[1], v[2], 1.0]);
    let b = at(o, wb);
    for (i, x) in p.iter().take(3).enumerate() {
        r.m.set_f32(b + 4 * i as u32, *x);
    }
}

/// PppPointRAp: every key +0x1c frames (countdown byte at B +1), a child (slot key +0xc) on a sphere of radius
/// key +4 about A: latitude ±90° and longitude from two draws; its position into its work at key +0x10, its
/// velocity (the direction · key +8) at key +0x18.
fn point_r_ap(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, wb) = works(r, rf);
    if r.paused {
        return;
    }
    let (wa, cd) = (at(o, wa), at(o, wb) + 1);
    if r.m.u8(cd) == 0 {
        let slot = r.m.u32(key + 0xc);
        if slot == 0xffff_ffff {
            return;
        }
        let c = child(r, o, slot);
        if c == 0 {
            return;
        }
        let t = crate::traced_tables::sine();
        let sin = |a: u32| t[(a >> 4 & 0xfff) as usize];
        let cos = |a: u32| t[(a.wrapping_add(0x4000) >> 4 & 0xfff) as usize];
        let rad = r.m.f32(key + 4);
        let d1 = f64::from(r.vu_draw());
        let a1 = ftol(d1 * 32768.0 - 16384.0) as u32;
        let (p1, p2) = (cos(a1) * rad, sin(a1) * rad);
        let d2 = f64::from(r.vu_draw());
        let a2 = ftol(d2 * 32768.0 * 2.0) as u32;
        let (x, z) = (p1 * sin(a2), p1 * cos(a2));
        let w = at(c, r.m.u32(key + 0x10));
        let base = [r.m.f32(wa), r.m.f32(wa + 4), r.m.f32(wa + 8)];
        for (i, v) in [x, p2, z].iter().enumerate() {
            r.m.set_f32(w + 4 * i as u32, v + base[i]);
        }
        let vk = r.m.f32(key + 8);
        let wv = at(c, r.m.u32(key + 0x18));
        for (i, v) in [vk * x, p2 * vk, z * vk].iter().enumerate() {
            r.m.set_f32(wv + 4 * i as u32, *v);
        }
        let n = r.m.u8(key + 0x1c);
        r.m.set_u8(cd, n);
    }
    let n = r.m.u8(cd).wrapping_sub(1);
    r.m.set_u8(cd, n);
}

// The born family: children at vertices of a mesh, walked in order or at random, through a callback per
// program (the shared walk "pppKeBornRnd2").

/// The callback: (run, spawner, key, vertex address) → whether to go on.
type BornFn = fn(&mut Run, u32, u32, u32) -> bool;

/// The walk: `count` vertices of mesh `mesh` from the cursor (u16 at `cursor`) or at random, each through `cb`;
/// the cursor is kept when all were made (true), not when a callback stops it.
#[allow(clippy::too_many_arguments)]
fn born_walk(
    r: &mut Run,
    o: u32,
    key: u32,
    cb: BornFn,
    cursor: u32,
    mesh_n: i16,
    random: bool,
    count: u8,
) -> bool {
    let e = r.env.lists[2].wrapping_add((mesh_n as i32 as u32).wrapping_mul(8));
    let verts = match r.m.u32(o + 0x90) {
        0 if r.m.u16(e) as i16 == -0x6fe8 => 0,
        0 => {
            let rec = (r.m.u16(e) as i16 as i32 as u32).wrapping_mul(0x20);
            r.m.u32(r.env.lists[0].wrapping_add(rec) + 0x14)
        }
        v => v,
    };
    let n = i32::from(r.m.u16(e + 2) as i16);
    let pick = |r: &mut Run| if n == 0 { 0 } else { r.vu_int().wrapping_rem(n) as u32 };
    let mut idx = if random { pick(r) } else { u32::from(r.m.u16(cursor)) };
    let list = r.m.u32(e + 4);
    for _ in 0..count {
        let v = verts.wrapping_add(u32::from(r.m.u16(list.wrapping_add(idx.wrapping_mul(2)))) * 6);
        if !cb(r, o, key, v) {
            return false;
        }
        idx = if random {
            pick(r)
        } else if n <= idx as i32 + 1 {
            0
        } else {
            idx + 1
        };
    }
    r.m.set_u16(cursor, idx as u16);
    true
}

/// The vertex as a point (x, y, z, 1).
fn vertex4(r: &Run, v: u32) -> [f32; 4] {
    let p = vertex_at(r, v, 0);
    [p[0], p[1], p[2], 1.0]
}

fn set3(r: &mut Run, at_: u32, v: &[f32]) {
    for (i, x) in v.iter().take(3).enumerate() {
        r.m.set_f32(at_ + 4 * i as u32, *x);
    }
}

/// The born children of the away and away-in-place walks (slot key +0x10): at the vertex through the spawner's matrix (work
/// key +0x14), moving away from the spawner (key +8 · the unit direction, work key +0x1c). Not for magic 0x2a9.
fn born_away(r: &mut Run, o: u32, key: u32, v: u32) -> bool {
    use crate::chr::elements::{apply4, normalize};
    if r.magic == 0x2a9 {
        return true;
    }
    let c = child(r, o, r.m.u32(key + 0x10));
    if c == 0 {
        return false;
    }
    let p = apply4(&get_mat(r, o + 0x10), vertex4(r, v));
    set3(r, at(c, r.m.u32(key + 0x14)), &p);
    let t = vec4(r, o + 0x40);
    let d = normalize([p[0] - t[0], p[1] - t[1], p[2] - t[2], p[3] - t[3]]);
    let k = r.m.f32(key + 8);
    let d = [k * d[0], d[1] * k, d[2] * k];
    set3(r, at(c, r.m.u32(key + 0x1c)), &d);
    true
}

/// At the vertex through the matrix (work key +0x10), the unit direction away from the
/// spawner (work key +0x18).
fn born_dir(r: &mut Run, o: u32, key: u32, v: u32) -> bool {
    use crate::chr::elements::{apply4, normalize};
    let c = child(r, o, r.m.u32(key + 0xc));
    if c == 0 {
        return false;
    }
    let p = apply4(&get_mat(r, o + 0x10), vertex4(r, v));
    set3(r, at(c, r.m.u32(key + 0x10)), &p);
    let t = vec4(r, o + 0x40);
    let d = normalize([p[0] - t[0], p[1] - t[1], p[2] - t[2], p[3] - t[3]]);
    set3(r, at(c, r.m.u32(key + 0x18)), &d);
    true
}

/// The engine's two-argument arc tangent: of (a, b), None at the origin.
fn atan_ab(a: f32, b: f32) -> Option<f32> {
    let prod = a * b;
    // (The engine's 3.141592654 and 1.570796327, as floats.)
    let pi = f64::from(f32::from_bits(0x40490fdb));
    let half = f64::from(f32::from_bits(0x3fc90fdb));
    if prod > 0.0 {
        let base = if a <= 0.0 { -pi } else { 0.0 };
        let t = (f64::from(b) / f64::from(a)).atan() as f32;
        return Some(t + base as f32);
    }
    if prod < 0.0 {
        let base = if b <= 0.0 { -half } else { half };
        let t = (f64::from(a) / f64::from(b)).atan() as f32;
        return Some((base - f64::from(t)) as f32);
    }
    if a == 0.0 {
        if b == 0.0 {
            return None;
        }
        return Some(if b <= 0.0 { -half } else { half } as f32);
    }
    Some(if a <= 0.0 { pi } else { 0.0 } as f32)
}

/// The angle as the engine's turn units (· 32768 / 3.141592654, truncated).
fn turn_units(a: f32) -> u32 {
    // (The engine's 3.141592654 as a double.)
    ftol(f64::from(a) * 32768.0 / f64::from_bits(0x400921fb54524550)) as u32
}

/// At the vertex's own coordinates (work key +0x10), facing it (work key +0x18: the
/// angles about X and Y, 0 about Z... as the engine orders them: +0 from the horizontal length and −z, +8
/// from (−y, x), +4 zero).
fn born_face(r: &mut Run, o: u32, key: u32, v: u32) -> bool {
    let c = child(r, o, r.m.u32(key + 0xc));
    if c == 0 {
        return false;
    }
    let p = vertex4(r, v);
    set3(r, at(c, r.m.u32(key + 0x10)), &p);
    let w = at(c, r.m.u32(key + 0x18));
    if let Some(a) = atan_ab(-p[1], p[0]) {
        r.m.set_u32(w + 8, turn_units(a));
    }
    let len = ((f64::from((f64::from(p[0]) * f64::from(p[0]) + f64::from(p[1]) * f64::from(p[1])) as f32))
        .sqrt()) as f32;
    if let Some(a) = atan_ab(len, -p[2]) {
        r.m.set_u32(w, turn_units(a));
    }
    r.m.set_u32(w + 4, 0);
    true
}

/// PppKeBornRnd5 / pppKeBornRnd6: every key interval frames (countdown byte at A +2) a walk; the countdown
/// runs down only after a walk that made all its children.
fn born_timed(r: &mut Run, o: u32, key: u32, rf: u32, six: bool) {
    let (wait, s1, s2, mesh_at, rnd_at, cnt_at, cb): (u32, u32, u32, u32, u32, u32, BornFn) =
        if six { (7, 0xc, 0x14, 4, 8, 6, born_face) } else { (0xd, 0x10, 0x18, 4, 0xe, 0xc, born_away) };
    if r.paused || r.m.u8(key + wait) == 0 {
        return;
    }
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    let cd = a + 2;
    let go = if r.m.u8(cd) != 0 {
        true
    } else {
        let w = r.m.u8(key + wait);
        r.m.set_u8(cd, w);
        r.m.u32(key + s1) != 0xffff_ffff
            && r.m.u32(key + s2) != 0xffff_ffff
            && born_walk(
                r,
                o,
                key,
                cb,
                a,
                r.m.u16(key + mesh_at) as i16,
                r.m.u8(key + rnd_at) != 0,
                r.m.u8(key + cnt_at),
            )
    };
    if go {
        let n = r.m.u8(cd).wrapping_sub(1);
        r.m.set_u8(cd, n);
    }
}

fn born5(r: &mut Run, o: u32, k: u32, rf: u32) {
    born_timed(r, o, k, rf, false)
}
fn born6(r: &mut Run, o: u32, k: u32, rf: u32) {
    born_timed(r, o, k, rf, true)
}

/// PppKeBornRnd2 / pppKeBornRnd3: a walk on frames where the integer draw's second byte has none of the key's
/// mask bits (mask 0xff: never).
fn born_chance(r: &mut Run, o: u32, key: u32, rf: u32, three: bool) {
    let (mask_at, s1, s2, rnd_at, cnt_at, cb): (u32, u32, u32, u32, u32, BornFn) =
        if three { (7, 0xc, 0x14, 8, 6, born_dir) } else { (0xd, 0x10, 0x18, 0xe, 0xc, born_away) };
    let mask = r.m.u8(key + mask_at);
    if r.paused || mask == 0xff {
        return;
    }
    if (r.vu_int() >> 8) as u8 & mask != 0 {
        return;
    }
    if r.m.u32(key + s1) == 0xffff_ffff || r.m.u32(key + s2) == 0xffff_ffff {
        return;
    }
    let (wa, _) = works(r, rf);
    let (mesh_n, random, count) = (r.m.u16(key + 4) as i16, r.m.u8(key + rnd_at) != 0, r.m.u8(key + cnt_at));
    born_walk(r, o, key, cb, at(o, wa), mesh_n, random, count);
}

fn born2(r: &mut Run, o: u32, k: u32, rf: u32) {
    born_chance(r, o, k, rf, false)
}
fn born3(r: &mut Run, o: u32, k: u32, rf: u32) {
    born_chance(r, o, k, rf, true)
}

fn born_init_timed(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    r.m.set_u16(at(o, wa), 0);
    r.m.set_u8(at(o, wa) + 2, 0);
}

fn born_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    r.m.set_u16(at(o, wa), 0);
}

/// A vector's length (x² + y² + z², the double square root, as a float).
fn length(v: [f32; 4]) -> f32 {
    let d = |x: f32| f64::from(x) * f64::from(x);
    ((d(v[0]) + d(v[1]) + d(v[2])).sqrt()) as f32
}

fn sub4(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]]
}

/// The address a key's target names: work `w` of the first object of slot `slot` of the running part, or (−1)
/// the scratch vector.
fn key_target(r: &Run, slot: u32, w: u32) -> u32 {
    if slot == 0xffff_ffff {
        return super::SCRATCH_VECTOR;
    }
    let first = r.m.u32(r.slots.wrapping_add(slot.wrapping_mul(0x10)) + 4);
    first.wrapping_add(w).wrapping_add(0xa0)
}

/// PppKeGrvTgt: on the key's time, B = (radius key +4, strength key +8, the address of A): a gravity target.
fn ke_grv_tgt(r: &mut Run, o: u32, key: u32, rf: u32) {
    if r.m.u32(o + 0xc) != r.m.u32(key) {
        return;
    }
    let (wa, wb) = works(r, rf);
    let b = at(o, wb);
    let (k1, k2) = (r.m.u32(key + 4), r.m.u32(key + 8));
    r.m.set_u32(b, k1);
    r.m.set_u32(b + 4, k2);
    r.m.set_u32(b + 8, at(o, wa));
}

fn ke_grv_tgt_init(r: &mut Run, o: u32, rf: u32) {
    let (_, wb) = works(r, rf);
    r.m.set_u32(at(o, wb) + 8, 0);
}

/// PppKeGrvEff: on the key's time C = the gravity target the key names (slot key +4, work key +8); from then on
/// (not on the first frame) the velocity B is pulled toward the target's position within its radius:
/// (1 − distance / radius) · strength along the unit direction.
fn ke_grv_eff(r: &mut Run, o: u32, key: u32, rf: u32) {
    use crate::chr::elements::normalize;
    let (wa, wb, wc) = works3(r, rf);
    let c = at(o, wc);
    if r.m.u32(o + 0xc) == r.m.u32(key) {
        let t = key_target(r, r.m.u32(key + 4), r.m.u32(key + 8));
        r.m.set_u32(c, t);
        if r.m.u32(o + 0xc) == 0 {
            return;
        }
    }
    let t = r.m.u32(c);
    if r.paused || t == super::SCRATCH_VECTOR {
        return;
    }
    let pos = vec4(r, r.m.u32(t + 8));
    let d = sub4(pos, vec4(r, at(o, wa)));
    let dist = length(d);
    let dn = normalize(d);
    let rad = r.m.f32(t);
    if dist < rad {
        let k = ((1.0 - f64::from(dist) / f64::from(rad)) * f64::from(r.m.f32(t + 4))) as f32;
        let b = at(o, wb);
        for (i, x) in dn.iter().take(3).enumerate() {
            let v = r.m.f32(b + 4 * i as u32) + x * k;
            r.m.set_f32(b + 4 * i as u32, v);
        }
    }
}

/// PppKeHmgEff: on the key's time C = (the turn rate key +4, the target the key names: slot +8, work +0xc);
/// from then on the velocity B turns toward the target: its direction blended with the target's by the
/// rate (nudged on a random axis when they are opposite), at B's speed.
fn ke_hmg_eff(r: &mut Run, o: u32, key: u32, rf: u32) {
    use crate::chr::elements::normalize;
    let (wa, wb, wc) = works3(r, rf);
    let c = at(o, wc);
    if r.m.u32(o + 0xc) == r.m.u32(key) {
        let k1 = r.m.u32(key + 4);
        r.m.set_u32(c, k1);
        let t = key_target(r, r.m.u32(key + 8), r.m.u32(key + 0xc));
        r.m.set_u32(c + 4, t);
        if r.m.u32(o + 0xc) == 0 {
            return;
        }
    }
    let t = r.m.u32(c + 4);
    if r.paused || t == super::SCRATCH_VECTOR {
        return;
    }
    let b = at(o, wb);
    let mut d = normalize(sub4(vec4(r, t), vec4(r, at(o, wa))));
    let v = vec4(r, b);
    let speed = length(v);
    let vn = normalize(v);
    let sum = [vn[0] + d[0], vn[1] + d[1], vn[2] + d[2], vn[3] + d[3]];
    if sum.iter().all(|x| x.to_bits() == 0) {
        let k = r.vu_int().wrapping_rem(3) as usize;
        d[k] += 0.05;
    }
    let rate = f64::from(r.m.f32(c));
    let inv = 1.0 - rate;
    let mix = |a: f32, b_: f32| (f64::from(a) * rate + inv * f64::from(b_)) as f32;
    let m = [
        mix(d[0], vn[0]),
        mix(d[1], vn[1]),
        mix(d[2], vn[2]),
        (inv * f64::from(vn[3]) + f64::from(d[3]) * rate) as f32,
    ];
    let m = normalize(m);
    set3(r, b, &[m[0] * speed, m[1] * speed, m[2] * speed]);
}

/// PppKeMvYpEff: A's angles from the velocity B (when it is not zero): about Z from (−vy, vx), about X from the
/// horizontal length and −vz.
fn ke_mv_yp_eff(r: &mut Run, o: u32, _key: u32, rf: u32) {
    if r.paused {
        return;
    }
    let (wa, wb) = works(r, rf);
    let (a, b) = (at(o, wa), at(o, wb));
    let v = vec4(r, b);
    if v[1] == 0.0 && v[0] == 0.0 && v[2] == 0.0 {
        return;
    }
    if let Some(t) = atan_ab(-v[1], v[0]) {
        r.m.set_u32(a + 8, turn_units(t));
    }
    let len = ((f64::from((f64::from(v[0]) * f64::from(v[0]) + f64::from(v[1]) * f64::from(v[1])) as f32))
        .sqrt()) as f32;
    if let Some(t) = atan_ab(len, -v[2]) {
        r.m.set_u32(a, turn_units(t));
    }
}

/// PppKeDrct: on the key's time A = the key's three integers (+0x10).
fn ke_drct(r: &mut Run, o: u32, key: u32, rf: u32) {
    if r.m.u32(o + 0xc) != r.m.u32(key) {
        return;
    }
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    for i in 0..3 {
        let v = r.m.u32(key + 0x10 + 4 * i);
        r.m.set_u32(a + 4 * i, v);
    }
}

fn ke_drct_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    r.m.set_f32(at(o, wa) + 0xc, 1.0);
}

fn nothing(_: &mut Run, _: u32, _: u32) {}

/// PppKeLnsLpSft: A (two floats) += the key's (+4, +8) on its time (not used on the first frame); then B +0x9c
/// += A.x and B +0xa8 += A.y.
fn ke_lns_lp_sft(r: &mut Run, o: u32, key: u32, rf: u32) {
    if r.paused {
        return;
    }
    let (wa, wb) = works(r, rf);
    let (a, b) = (at(o, wa), at(o, wb));
    if r.m.u32(o + 0xc) == r.m.u32(key) {
        for i in 0..2 {
            let v = r.m.f32(key + 4 + 4 * i) + r.m.f32(a + 4 * i);
            r.m.set_f32(a + 4 * i, v);
        }
        if r.m.u32(o + 0xc) == 0 {
            return;
        }
    }
    let v = r.m.f32(b + 0x9c) + r.m.f32(a);
    r.m.set_f32(b + 0x9c, v);
    let v = r.m.f32(a + 4) + r.m.f32(b + 0xa8);
    r.m.set_f32(b + 0xa8, v);
}

fn zero_a2(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    set(r, at(o, wa), &[0; 2]);
}

/// PppNeiLightEikyo: A (six floats) += the key's (+4) on its time; then two ramps: A1 += A0, A2 += A1;
/// A4 += A3, A5 += A4.
fn nei_light_eikyo(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    if r.paused {
        return;
    }
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        for i in 0..6 {
            let v = r.m.f32(key + 4 + 4 * i) + r.m.f32(a + 4 * i);
            r.m.set_f32(a + 4 * i, v);
        }
    }
    let f = |r: &Run, i: u32| r.m.f32(a + 4 * i);
    let v1 = f(r, 1) + f(r, 0);
    r.m.set_f32(a + 4, v1);
    let v2 = v1 + f(r, 2);
    r.m.set_f32(a + 8, v2);
    let v4 = f(r, 3) + f(r, 4);
    r.m.set_f32(a + 16, v4);
    let v5 = v4 + f(r, 5);
    r.m.set_f32(a + 20, v5);
}

fn zero_a6(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    set(r, at(o, wa), &[0; 6]);
}

/// PppNeiPointLight: on the key's time A +0xc / +0x10 = the addresses of B and C, A (three floats) += the
/// key's (+4); then the ramp A1 += A0, A2 += A1.
fn nei_point_light(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, wb, wc) = works3(r, rf);
    let a = at(o, wa);
    if r.paused {
        return;
    }
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        r.m.set_u32(a + 0xc, at(o, wb));
        r.m.set_u32(a + 0x10, at(o, wc));
        for i in 0..3 {
            let v = r.m.f32(key + 4 + 4 * i) + r.m.f32(a + 4 * i);
            r.m.set_f32(a + 4 * i, v);
        }
    }
    let v1 = r.m.f32(a) + r.m.f32(a + 4);
    r.m.set_f32(a + 4, v1);
    let v2 = v1 + r.m.f32(a + 8);
    r.m.set_f32(a + 8, v2);
}

fn zero_a3(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    set(r, at(o, wa), &[0; 3]);
}

/// PppKeAcmCic / pppKeAcmCicEx: unless the key's byte +4 is 2, the scene's accumulation set to (0, 1).
fn ke_acm_cic(r: &mut Run, _o: u32, key: u32, _rf: u32) {
    if r.m.u8(key + 4) != 2 {
        r.render.push(super::RenderCall::Accumulation(0, 1));
    }
}

fn nothing_part(_: &mut Run, _: &mut super::Part) {}

/// The average of a model's polygon centres (the model: u16 +0x10 polygon count, +4 its groups, +8 its
/// vertices, offsets from the model): groups of {type byte +1, i16 count +2, polygons from +0x10}: type 0
/// triangles of 0x14 bytes, 2 triangles of 0x20 (vertex numbers at +0xc), 4 quads of 0x18, 6 quads of 0x28
/// (at +0x10); w = 1.
fn model_centre(r: &Run, model: u32) -> [f32; 4] {
    let total = r.m.u16(model + 0x10) as i16;
    let mut g = model.wrapping_add(r.m.u32(model + 4));
    let verts = model.wrapping_add(r.m.u32(model + 8));
    let mut acc = [0.0f32; 4];
    let mut left = i32::from(total);
    while left > 0 {
        let n = r.m.u16(g + 2) as i16;
        let (stride, first, k) = match r.m.u8(g + 1) {
            0 => (0x14, 0xc, 3),
            2 => (0x20, 0xc, 3),
            4 => (0x18, 0x10, 4),
            6 => (0x28, 0x10, 4),
            _ => (0, 0, 0),
        };
        let mut poly = g + 0x10;
        if k != 0 {
            for _ in 0..n.max(0) {
                let mut c = [0.0f32; 3];
                for j in 0..k {
                    let vi = u32::from(r.m.u16(poly + first + 2 * j));
                    let v = vertex_at(r, verts, vi);
                    for (x, y) in c.iter_mut().zip(v) {
                        *x += y;
                    }
                }
                for (i, x) in c.iter().enumerate() {
                    acc[i] += x / k as f32;
                }
                poly += stride;
            }
        }
        left -= i32::from(n);
        g = poly;
        if k == 0 {
            break;
        }
    }
    if total > 0 {
        let t = f32::from(total);
        for x in acc.iter_mut().take(3) {
            *x /= t;
        }
    }
    acc[3] = 1.0;
    acc
}

/// PppKeOfsPt: on the key's time A += the key's floats (+0x10) and A +0x20 = its integers (+0x20); at age 0 first
/// A +0x10 = the centre of packet record key +0x2c's model (none for 0xffff).
fn ke_ofs_pt(r: &mut Run, o: u32, key: u32, rf: u32) {
    if r.paused || r.m.u32(o + 0xc) != r.m.u32(key) {
        return;
    }
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    if r.m.u32(o + 0xc) == 0 {
        let rec = r.m.u32(key + 0x2c);
        if rec == 0xffff {
            return;
        }
        let model = r.m.u32(r.env.lists[0].wrapping_add(rec.wrapping_mul(0x20)) + 0x1c);
        let c = model_centre(r, model);
        put_vec4(r, a + 0x10, c);
    }
    for i in 0..3 {
        let v = r.m.f32(key + 0x10 + 4 * i) + r.m.f32(a + 4 * i);
        r.m.set_f32(a + 4 * i, v);
    }
    for i in 0..3 {
        let v = r.m.u32(key + 0x20 + 4 * i);
        r.m.set_u32(a + 0x20 + 4 * i, v);
    }
}

fn ke_ofs_pt_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    set(r, a, &[0; 3]);
    r.m.set_u32(a + 0x2c, 0);
}

// The draw stage (each object's second entry, run in the update): the object's draw matrix at +0x50 (16 floats)
// and its kind at +0x9c (0: through the view; 1: facing the camera).

/// PppDrawMatrix: the view matrix times the object's.
fn draw_matrix(r: &mut Run, o: u32, _key: u32, _rf: u32) {
    r.m.set_u8(o + 0x9c, 0);
    let m = crate::chr::elements::mul(&r.view, &get_mat(r, o + 0x10));
    put_mat(r, o + 0x50, &m);
}

/// The facing matrix: the object's rows scaled by the part's scale, its position through the camera matrix.
fn front(r: &mut Run, o: u32) -> Mat {
    let src = get_mat(r, o + 0x10);
    let mut m = [0.0; 16];
    for k in 0..3 {
        let s = r.part_scale[k];
        for c in 0..4 {
            m[k * 4 + c] = s * src[k * 4 + c];
        }
    }
    let p = crate::chr::elements::apply4(&r.camera, vec4(r, o + 0x40));
    m[12..16].copy_from_slice(&p);
    m
}

/// PppDrawMatrixFront.
fn draw_matrix_front(r: &mut Run, o: u32, _key: u32, _rf: u32) {
    r.m.set_u8(o + 0x9c, 1);
    let m = front(r, o);
    put_mat(r, o + 0x50, &m);
}

/// PppKeDMatFr: the facing matrix (the kind byte left as it is), also copied into work B.
fn ke_d_mat_fr(r: &mut Run, o: u32, _key: u32, rf: u32) {
    let m = front(r, o);
    put_mat(r, o + 0x50, &m);
    let (_, wb) = works(r, rf);
    put_mat(r, at(o, wb), &m);
}

// The model draws: the object's colour (four i16 at a work plus the engine's scratch offset, which they then
// clear) times the part's (each product >> 19, one byte), nothing when the alpha byte is 0; the model of image
// record key +4 (0xffff: none) of the group's +0x14 list; the draw flags from the key and the model's flag
// byte; then a render request with the object's draw matrix and buffers.

/// The packed colour of the i16 colour at `at`.
fn packed_colour(r: &mut Run, at_: u32) -> [u8; 4] {
    let base = at_.wrapping_add(r.env.scratch_offset);
    r.env.scratch_offset = 0;
    std::array::from_fn(|i| {
        let c = i32::from(r.m.u16(base + 2 * i as u32) as i16);
        let p = i32::from(r.part_colour[i] as i16);
        (c.wrapping_mul(p) >> 19 & 0x1fff) as u8
    })
}

/// The image record of model number `n`.
fn image_record(r: &Run, n: u32) -> u32 {
    r.env.lists[0].wrapping_add(n.wrapping_mul(0x20))
}

/// The texture page of the model's first packet.
fn first_page(r: &Run, model: u32) -> u32 {
    r.m.u32(model.wrapping_add(r.m.u32(model + 4)) + 8) & 0x3fff
}

/// The render state a blend kind gives a new entry.
pub(super) fn blend_state(blend: u8) -> u32 {
    match blend {
        0x41 => 0x100,
        0x42 => 0x10,
        0x46 => 0x20,
        0x48 => 2,
        0x88 => 0x200,
        _ => 0,
    }
}

/// The draw flags from the semi / extra bits and the model's flag byte (bit 0: textured, bit 1).
fn model_flags(semi: bool, extra: bool, model_byte: u8) -> u32 {
    let mut f = if semi {
        4
    } else if extra {
        0x80
    } else {
        0
    };
    if model_byte & 1 == 0 {
        f |= 8;
    }
    if model_byte & 2 == 0 {
        f |= 0x10;
    }
    f
}

struct ModelArgs {
    model_no: u32,
    flags: u32,
    blend: u8,
    texture_word: u16,
    offsets: (u32, u32),
    colour: [u8; 4],
    work: u32,
    create: u32,
    finish: bool,
    fade: Option<(u32, u32, f32)>,
}

fn emit_model(r: &mut Run, o: u32, a: ModelArgs) {
    let record = image_record(r, a.model_no);
    let model = r.m.u32(record + 0x1c);
    let draw = super::ModelDraw {
        object: o,
        ref_index: r.ref_index,
        record,
        model,
        flags: a.flags,
        blend: a.blend,
        texture_word: a.texture_word,
        offsets: a.offsets,
        colour: a.colour,
        matrix: get_mat(r, o + 0x50),
        work: a.work,
        work_matrix: (a.work != 0).then(|| get_mat(r, a.work)),
        vertices: r.m.u32(o + 0x90),
        second: r.m.u32(o + 0x94),
        part_word: r.part_word,
        create: a.create,
        semi: a.flags & 4 != 0,
        finish: a.finish,
        fade: a.fade,
    };
    r.render.push(super::RenderCall::Model(Box::new(draw)));
}

/// PppDrawMdl3: key +4 the model, +8 its switches (4: semi-transparent with the plain blend; 2: semi-transparent,
/// the plain blend when the model is untextured; 1: flag 0x80), +9 / +0xb flags 0x200 / 0x100, +0xa facing the
/// camera, +0xc the third work as the entry's matrix; the blend is the model's (+0x16) otherwise. Remaster
/// adjustments for some magic files while their own effect runs: 0x296 is drawn solid, 0x241 semi-transparent
/// on pages 0x36c0 / 0x3700; the render state of a new entry follows the blend, the work, the texture flag and
/// (0x17a, 0x110, 0x111, 0x231, and the map effects of five events) its own exceptions.
fn draw_mdl3(r: &mut Run, o: u32, key: u32, rf: u32) {
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let (wa, _, wc) = works3(r, rf);
    let colour = packed_colour(r, at(o, wa));
    if colour[3] == 0 {
        return;
    }
    let model = r.m.u32(image_record(r, n) + 0x1c);
    let mb = r.m.u8(model);
    let sw = r.m.u8(key + 8);
    let mut blend = r.m.u8(model + 0x16);
    let mut f = model_flags(sw & 6 != 0, sw & 1 != 0, mb);
    if sw & 4 != 0 {
        f = 0xc | (f & 0x10);
        blend = 0x44;
    } else if sw & 2 != 0 && mb & 1 == 0 {
        blend = 0x44;
    }
    if r.m.u8(key + 9) != 0 {
        f |= 0x200;
    }
    if r.m.u8(key + 0xb) != 0 {
        f |= 0x100;
    }
    if r.m.u8(key + 0xa) != 0 {
        f |= 0x2000;
    }
    let work = if r.m.u8(key + 0xc) != 0 { at(o, wc) } else { 0 };
    let own = r.scene.own_magic;
    let loaded = r.magic != u32::MAX;
    if loaded && r.magic == 0x296 && own {
        f &= !4;
    }
    if loaded && r.magic == 0x241 && own && matches!(first_page(r, model), 0x36c0 | 0x3700) {
        f |= 4;
    }
    let mut c = blend_state(blend);
    if sw & 4 != 0 || f & 0x2000 == 0 {
        c |= u32::from(work != 0);
    }
    if f & 8 != 0 {
        c |= 0x40;
    }
    if r.magic == 0x17a && own && first_page(r, model) == 0x37e0 {
        c |= 0x40;
    }
    let map_index = r.scene.map_part;
    if (r.magic == 0x110 || r.magic == 0x111) && own {
        if f & 0x10 != 0 {
            c |= 0x8000;
        }
        let (top, low) = if r.magic == 0x110 { (0x1e, 0x1a) } else { (0x1f, 0x1c) };
        if n == top {
            c = 0x1000_0000;
        } else if n == low {
            c = 0x8000_0000;
        }
    } else if loaded && r.magic == 0x231 && own {
        c |= 0x800;
    } else {
        match r.scene.event {
            0x17c if map_index == Some(0x19) => c = c & !1 | 0x800,
            0x3e if matches!(r.scene.map_part_id, 0x2c..=0x2f) => c = c & !1 | 0x800,
            0xc6 if map_index == Some(5) => c &= !1,
            0xc2 | 0xff if map_index == Some(0) => c &= !1,
            _ => {}
        }
    }
    let args = ModelArgs {
        model_no: n,
        flags: f,
        blend,
        texture_word: 0,
        offsets: (0, 0),
        colour,
        work,
        create: c,
        finish: false,
        fade: None,
    };
    emit_model(r, o, args);
}

/// PppDrawMdlSemi3: key +4 the model, +8 the blend (0: the render state 2), +0xa its switches (2:
/// semi-transparent; 1: flag 0x80), +0xb / +0xd flags 0x200 / 0x100, +0xc facing the camera, +0xe the third
/// work as the entry's matrix; an extra pass after each update.
fn draw_mdl_semi3(r: &mut Run, o: u32, key: u32, rf: u32) {
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let (wa, _, wc) = works3(r, rf);
    let colour = packed_colour(r, at(o, wa));
    if colour[3] == 0 {
        return;
    }
    let model = r.m.u32(image_record(r, n) + 0x1c);
    let sw = r.m.u8(key + 0xa);
    let mut f = model_flags(sw & 2 != 0, sw & 1 != 0, r.m.u8(model));
    if r.m.u8(key + 0xb) != 0 {
        f |= 0x200;
    }
    if r.m.u8(key + 0xd) != 0 {
        f |= 0x100;
    }
    if r.m.u8(key + 0xc) != 0 {
        f |= 0x2000;
    }
    let work = if r.m.u8(key + 0xe) != 0 { at(o, wc) } else { 0 };
    let blend = r.m.u8(key + 8);
    let mut c = if blend == 0 { 2 } else { blend_state(blend) };
    if f & 0x2000 == 0 {
        c |= u32::from(work != 0);
    }
    if f & 8 != 0 {
        c |= 0x40;
    }
    let args = ModelArgs {
        model_no: n,
        flags: f,
        blend,
        texture_word: 0,
        offsets: (0, 0),
        colour,
        work,
        create: c,
        finish: true,
        fade: None,
    };
    emit_model(r, o, args);
}

/// Three floats ramping: v1 += v2, v0 += v1; on the key's time each += the key's three floats at `k`.
fn ramp3(r: &mut Run, o: u32, key: u32, v: u32, k: u32, accel: Option<f32>) {
    if r.paused {
        return;
    }
    let v1 = accel.unwrap_or_else(|| r.m.f32(v + 8)) + r.m.f32(v + 4);
    r.m.set_f32(v + 4, v1);
    let v0 = r.m.f32(v) + v1;
    r.m.set_f32(v, v0);
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        r.m.set_f32(v, v0 + r.m.f32(k));
        r.m.set_f32(v + 4, v1 + r.m.f32(k + 4));
        if accel.is_none() {
            r.m.set_f32(v + 8, r.m.f32(k + 8) + r.m.f32(v + 8));
        }
    }
}

/// PppKeMdlTfd3: the third work ramps (key +8); then (model key +4) the fading draw: the fade from the key's
/// words +0x14 / +0x18 and the ramp's value, texture word key byte +0x1c · 4, switches the bytes +0x1d..+0x21
/// together (2 semi-transparent, 1 flag 0x80, 4 / 8 flags 0x200 / 0x100, 0x10 facing the camera, 0x20 the
/// fourth work as the entry's matrix), colour from the first work.
fn ke_mdl_tfd3(r: &mut Run, o: u32, key: u32, rf: u32) {
    let pair = r.m.u32(rf + 0xc);
    let (wa, wc, wd) = (r.m.u32(pair), r.m.u32(pair + 8), r.m.u32(pair + 0xc));
    let c = at(o, wc);
    ramp3(r, o, key, c, key + 8, None);
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let fade = (r.m.u32(key + 0x14), r.m.u32(key + 0x18), r.m.f32(c));
    let sw = (0x1d..=0x21).fold(0u8, |acc, b| acc | r.m.u8(key + b));
    let word = u16::from(r.m.u8(key + 0x1c)) << 2;
    draw_mdl(r, o, n, at(o, wa), word, (0, 0), sw, at(o, wd), fade);
}

/// PppDrawMdl (the model draw other programs call): switches `sw` as pppKeMdlTfd3 gives them; the blend is the
/// model's; a new entry's render state also gets 0x80.
#[allow(clippy::too_many_arguments)]
fn draw_mdl(
    r: &mut Run,
    o: u32,
    n: u32,
    colour_at: u32,
    word: u16,
    offsets: (u32, u32),
    sw: u8,
    work_at: u32,
    fade: (u32, u32, f32),
) {
    let colour = packed_colour(r, colour_at);
    if colour[3] == 0 {
        return;
    }
    let model = r.m.u32(image_record(r, n) + 0x1c);
    let mut f = model_flags(sw & 2 != 0, sw & 1 != 0, r.m.u8(model));
    if sw & 4 != 0 {
        f |= 0x200;
    }
    if sw & 8 != 0 {
        f |= 0x100;
    }
    if sw & 0x10 != 0 {
        f |= 0x2000;
    }
    let work = if sw & 0x20 != 0 { work_at } else { 0 };
    let blend = r.m.u8(model + 0x16);
    let mut c = blend_state(blend);
    if f & 0x2000 == 0 {
        c |= u32::from(work != 0);
    }
    if f & 8 != 0 {
        c |= 0x40;
    }
    let args = ModelArgs {
        model_no: n,
        flags: f,
        blend,
        texture_word: word,
        offsets: (offsets.0 << 16, offsets.1 << 16),
        colour,
        work,
        create: c | 0x80,
        finish: false,
        fade: Some(fade),
    };
    emit_model(r, o, args);
}

/// PppKeMdlDtt (not on event 0x13e): the first work ramps (key +8; for key modes (+0x14) 4 and up its third
/// float is a pointer, and the ramp has no acceleration while that is set); then (model key +4) a distortion
/// draw when the part's alpha times the second work's is not 0.
fn ke_mdl_dtt(r: &mut Run, o: u32, key: u32, rf: u32) {
    if r.scene.event == 0x13e {
        return;
    }
    let (wa, wb, wc) = works3(r, rf);
    let a = at(o, wa);
    let mode = r.m.u8(key + 0x14);
    let pointer = if mode >= 4 { r.m.u32(a + 8) } else { 0 };
    let accel = (mode >= 4 && pointer != 0).then_some(0.0);
    let accel = accel.or_else(|| (mode >= 4).then(|| r.m.f32(a + 8)));
    ramp3(r, o, key, a, key + 8, accel);
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let b = at(o, wb);
    let ch = |r: &Run, i: u32| {
        i32::from(r.part_colour[i as usize]).wrapping_mul(i32::from(r.m.u16(b + 2 * i) as i16))
    };
    if ch(r, 3) == 0 {
        return;
    }
    let colour = std::array::from_fn(|i| (f64::from(ch(r, i as u32)) * 1.4901161193847656e-8) as f32);
    let model = r.m.u32(image_record(r, n) + 0x1c);
    let work = at(o, wc);
    let draw = super::DistortDraw {
        object: o,
        ref_index: r.ref_index,
        model,
        colour,
        scale: r.part_scale[0] * r.m.f32(a),
        mode,
        kind: r.m.u8(key + 0x15),
        work,
        work_matrix: get_mat(r, work),
        vertices: r.m.u32(o + 0x90),
        pointer: (mode >= 4 && pointer != 0).then(|| (pointer, r.m.u32(pointer))),
    };
    r.render.push(super::RenderCall::Distort(Box::new(draw)));
}

fn ke_mdl_dtt_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    set(r, at(o, wa), &[0; 3]);
}

fn ke_mdl_tfd3_init(r: &mut Run, o: u32, rf: u32) {
    let (_, _, wc) = works3(r, rf);
    set(r, at(o, wc), &[0; 3]);
}

/// PppKeZCrct: into the first work, the object's matrix with its rows scaled by the key's (+0x20) and its
/// position, by the key's mode (+0x30): 0 the position offset by the key's (+0x10), then through the camera;
/// 1 through the camera, then offset; 2 through the frame matrix, the position replaced by the part's scale
/// plus the scale times the key's offset (w the part's), then through the world matrix; others unchanged.
fn ke_z_crct(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let src = get_mat(r, o + 0x10);
    let mut m = src;
    for row in 0..3 {
        let s = r.m.f32(key + 0x20 + 4 * row as u32);
        for c in 0..3 {
            m[row * 4 + c] = s * src[row * 4 + c];
        }
    }
    let pos = vec4(r, o + 0x40);
    m[12..16].copy_from_slice(&pos);
    let off = vec4(r, key + 0x10);
    let el = crate::chr::elements::mul;
    match r.m.u8(key + 0x30) {
        0 => {
            for i in 0..4 {
                m[12 + i] += off[i];
            }
            m = el(&r.camera, &m);
        }
        1 => {
            m = el(&r.camera, &m);
            for i in 0..4 {
                m[12 + i] += off[i];
            }
        }
        2 => {
            m = el(&r.frame, &m);
            let s = r.part_scale;
            for i in 0..3 {
                m[12 + i] = s[i] + s[i] * off[i];
            }
            m[15] = s[3];
            m = el(&r.world, &m);
        }
        _ => {}
    }
    put_mat(r, at(o, wa), &m);
}

/// PppVtMime: the first work ramps (key +0xc); then, with two meshes (key +4 / +8, records of the group's +0x14
/// list: +0 the vertex count, +0x14 the vertices, three i16 each), each vertex blended (first + (second − first)
/// · the ramp's value, truncated) into a buffer of the pool (made once, at the work's +0xc) that becomes the
/// object's vertex buffer.
fn vt_mime(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    ramp3(r, o, key, a, key + 0xc, None);
    let (n1, n2) = (r.m.u32(key + 4), r.m.u32(key + 8));
    if n1 == 0xffff && n2 == 0xffff {
        return;
    }
    let (r1, r2) = (image_record(r, n1), image_record(r, n2));
    let (v1, v2) = (r.m.u32(r1 + 0x14), r.m.u32(r2 + 0x14));
    let t = r.m.f32(a);
    let count = r.m.u32(r1);
    if r.m.u32(a + 0xc) == 0 {
        let p = r.env.alloc(r.m, count.wrapping_mul(8));
        r.m.set_u32(a + 0xc, p);
        if p == 0 {
            return;
        }
    }
    let buf = r.m.u32(a + 0xc);
    for i in 0..count.wrapping_mul(3) {
        let x = i32::from(r.m.u16(v1 + 2 * i) as i16);
        let y = i32::from(r.m.u16(v2 + 2 * i) as i16);
        let d = (y - x) as f32;
        let v = (f64::from(d) * f64::from(t) + f64::from(x)).trunc() as i32;
        r.m.set_u16(buf + 2 * i, v as u16);
    }
    r.m.set_u32(o + 0x90, buf);
}

fn vt_mime_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    set(r, at(o, wa), &[0; 4]);
}

fn vt_mime_restart(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    set(r, at(o, wa), &[0; 3]);
}

fn vt_mime_free(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    let p = r.m.u32(a + 0xc);
    if p != 0 {
        r.env.free(r.m, p);
        r.m.set_u32(a + 0xc, 0);
    }
}

// The loop family: values that add the key's on its time like the plain ones, but whose restart takes the
// first key's values off again (so a looping object starts over from where it began). The first key is kept at
// A +0x10 (set on the first update, cleared at the init).

fn loop_add(r: &mut Run, o: u32, key: u32, rf: u32, float: bool) {
    if r.paused {
        return;
    }
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    if r.m.u32(a + 0x10) == 0 {
        set(r, a, &[0; 3]);
        r.m.set_u32(a + 0x10, key);
    }
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        for i in 0..3 {
            if float {
                let v = r.m.f32(key + 0x10 + 4 * i) + r.m.f32(a + 4 * i);
                r.m.set_f32(a + 4 * i, v);
            } else {
                let v = r.m.u32(a + 4 * i).wrapping_add(r.m.u32(key + 0x10 + 4 * i));
                r.m.set_u32(a + 4 * i, v);
            }
        }
    }
}

fn loop_back(r: &mut Run, o: u32, rf: u32, float: bool) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    let first = r.m.u32(a + 0x10);
    for i in 0..3 {
        if float {
            let v = r.m.f32(a + 4 * i) - r.m.f32(first + 0x10 + 4 * i);
            r.m.set_f32(a + 4 * i, v);
        } else {
            let v = r.m.u32(a + 4 * i).wrapping_sub(r.m.u32(first + 0x10 + 4 * i));
            r.m.set_u32(a + 4 * i, v);
        }
    }
}

/// PppAngleLoop: integers.
fn angle_loop(r: &mut Run, o: u32, key: u32, rf: u32) {
    loop_add(r, o, key, rf, false)
}
fn angle_loop_back(r: &mut Run, o: u32, rf: u32) {
    loop_back(r, o, rf, false)
}
/// PppPointLoop / pppScaleLoop: floats.
fn float_loop(r: &mut Run, o: u32, key: u32, rf: u32) {
    loop_add(r, o, key, rf, true)
}
fn float_loop_back(r: &mut Run, o: u32, rf: u32) {
    loop_back(r, o, rf, true)
}
fn loop_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    r.m.set_u32(at(o, wa) + 0x10, 0);
}

/// PppDrawMdlLoop: a model whose texture scrolls: the third work keeps the first key (+0), u and v (+4, +0x10;
/// from the key's +8 / +0x14 at the start) with their speeds and accelerations (+8 / +0xc, +0x14 / +0x18,
/// added from the key's +0xc / +0x10 / +0x18 / +0x1c on its time); each frame the speeds take their
/// accelerations and u, v their speeds, kept in [0, 32768); the draw takes them · 65536. Key +0x20 the blend
/// (0xff: the model's), +0x22 the switches (2 semi-transparent, 1 flag 0x80), +0x23 / +0x24 flags 0x200 /
/// 0x100, +0x25 facing the camera (and not flag 0x10).
fn draw_mdl_loop(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _, wc) = works3(r, rf);
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let c = at(o, wc);
    if r.m.u32(c) == 0 {
        r.m.set_u32(c + 4, r.m.u32(key + 8));
        r.m.set_u32(c + 0x10, r.m.u32(key + 0x14));
        set(r, c + 8, &[0; 2]);
        set(r, c + 0x14, &[0; 2]);
        r.m.set_u32(c, key);
    }
    if !r.paused {
        if r.m.u32(key) == r.m.u32(o + 0xc) {
            for (w, k) in [(8, 0xc), (0xc, 0x10), (0x14, 0x18), (0x18, 0x1c)] {
                let v = r.m.f32(key + k) + r.m.f32(c + w);
                r.m.set_f32(c + w, v);
            }
        }
        let du = r.m.f32(c + 0xc) + r.m.f32(c + 8);
        r.m.set_f32(c + 8, du);
        let dv = r.m.f32(c + 0x18) + r.m.f32(c + 0x14);
        r.m.set_f32(c + 0x14, dv);
        let u = du + r.m.f32(c + 4);
        r.m.set_f32(c + 4, (f64::from(u) % 32768.0) as f32);
        let v = r.m.f32(c + 0x10) + dv;
        r.m.set_f32(c + 0x10, (f64::from(v) % 32768.0) as f32);
    }
    let colour = packed_colour(r, at(o, wa));
    if colour[3] == 0 {
        return;
    }
    let model = r.m.u32(image_record(r, n) + 0x1c);
    let sw = r.m.u8(key + 0x22);
    let mut f = model_flags(sw & 2 != 0, sw & 1 != 0, r.m.u8(model));
    if r.m.u8(key + 0x23) != 0 {
        f |= 0x200;
    }
    if r.m.u8(key + 0x24) != 0 {
        f |= 0x100;
    }
    if r.m.u8(key + 0x25) != 0 {
        f = f & !0x10 | 0x2000;
    }
    let blend = match r.m.u8(key + 0x20) {
        0xff => r.m.u8(model + 0x16),
        b => b,
    };
    let mut cr = blend_state(blend);
    if f & 8 != 0 {
        cr |= 0x40;
    }
    let u = (f64::from(r.m.f32(c + 4)) * 65536.0).trunc() as i32 as u32;
    let v = (65536.0 * f64::from(r.m.f32(c + 0x10))).trunc() as i32 as u32;
    let args = ModelArgs {
        model_no: n,
        flags: f,
        blend,
        texture_word: 0,
        offsets: (u, v),
        colour,
        work: 0,
        create: cr,
        finish: false,
        fade: None,
    };
    emit_model(r, o, args);
}

fn draw_mdl_loop_init(r: &mut Run, o: u32, rf: u32) {
    let (_, _, wc) = works3(r, rf);
    r.m.set_u32(at(o, wc), 0);
}

fn draw_mdl_loop_restart(r: &mut Run, o: u32, rf: u32) {
    let (_, _, wc) = works3(r, rf);
    let c = at(o, wc);
    set(r, c + 8, &[0; 2]);
    set(r, c + 0x14, &[0; 2]);
}

/// PppKeZCrctShp: into the first work, the object's rows scaled by the part's scale and its position (times
/// the key's scale +0x20, w its own) by the key's mode (+0x30): 0 offset by the key's (+0x10), then through the
/// camera (turn and position, w taken as 1); 1 through the camera, then offset; 2 the part's scale plus the
/// scale times the key's offset (w the part's).
fn ke_z_crct_shp(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let src = get_mat(r, o + 0x10);
    let s = r.part_scale;
    let mut m = src;
    for row in 0..3 {
        for c in 0..3 {
            m[row * 4 + c] = s[row] * src[row * 4 + c];
        }
    }
    let p = [
        r.m.f32(key + 0x20) * src[12],
        r.m.f32(key + 0x24) * src[13],
        r.m.f32(key + 0x28) * src[14],
        src[15],
    ];
    let off = vec4(r, key + 0x10);
    let through = |m: &Mat, v: [f32; 4]| -> [f32; 4] {
        let d = f64::from;
        let x = ((d(v[0]) * d(m[0]) + d(v[1]) * d(m[4])) + d(v[2]) * d(m[8])) as f32;
        let y = ((d(m[9]) * d(v[2]) + d(m[5]) * d(v[1])) + d(m[1]) * d(v[0])) as f32;
        let z = ((d(m[10]) * d(v[2]) + d(m[6]) * d(v[1])) + d(m[2]) * d(v[0])) as f32;
        let w = ((d(v[2]) * d(m[11]) + d(v[1]) * d(m[7])) + d(v[0]) * d(m[3])) as f32;
        [x + m[12], m[13] + y, m[14] + z, m[15] + w]
    };
    let pos = match r.m.u8(key + 0x30) {
        0 => through(&r.camera, [p[0] + off[0], p[1] + off[1], p[2] + off[2], off[3] + p[3]]),
        1 => {
            let q = through(&r.camera, p);
            [q[0] + off[0], q[1] + off[1], q[2] + off[2], off[3] + q[3]]
        }
        2 => [s[0] + s[0] * off[0], s[1] + s[1] * off[1], s[2] + s[2] * off[2], s[3]],
        _ => [0.0; 4],
    };
    m[12..16].copy_from_slice(&pos);
    put_mat(r, at(o, wa), &m);
}

/// A shape's frame record (the list entry's shape: +6 the frame count, frames of 8 bytes from +0x10: i16 offset
/// of the frame's sprites, i16 duration, flags (0x80 loop)) and the blend of the frame's sprites.
fn shape_frame(r: &Run, shape: u32, frame: u32) -> (u32, u8) {
    let rec = shape + (frame + 2) * 8;
    let sprites = shape.wrapping_add(r.m.u16(rec) as i16 as i32 as u32);
    let packet = sprites.wrapping_add(r.m.u32(sprites + 4));
    (rec, r.m.u8(packet + 4))
}

/// A shape's frame step: the timer (u16 A +0) by the key's speed (i16 +8); past the frame's duration the next
/// frame; past the last, the first again (loop flag) or the last held.
fn shape_step(r: &mut Run, a: u32, key: u32, shape: u32, rec: u32) {
    if r.paused {
        return;
    }
    let t = r.m.u16(a).wrapping_add(r.m.u16(key + 8));
    r.m.set_u16(a, t);
    let dur = r.m.u16(rec + 2) as i16 as i32;
    if dur <= i32::from(t) {
        let f = r.m.u16(a + 2).wrapping_add(1);
        r.m.set_u16(a + 2, f);
        r.m.set_u16(a, t.wrapping_sub(dur as u16));
        if (r.m.u16(shape + 6) as i16 as i32) <= i32::from(f) {
            if r.m.u8(rec + 4) & 0x80 != 0 {
                r.m.set_u32(a, 0);
                return;
            }
            r.m.set_u16(a, 0);
            r.m.set_u16(a + 2, f.wrapping_sub(1));
        }
    }
}

/// One shape draw (the third path for pppDrawShape, the plain one for pppDrawShapeX): shape key +4 (0xffff:
/// none) of the group's +0x18 list, the timer and frame in the first work, the colour from the second.
fn draw_shape_one(r: &mut Run, o: u32, key: u32, rf: u32, path3: bool) {
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let pair = r.m.u32(rf + 0xc);
    let (wa, wb, wd) = (r.m.u32(pair), r.m.u32(pair + 4), r.m.u32(pair + 0xc));
    let a = at(o, wa);
    let entry = r.env.lists[1].wrapping_add(4 * n);
    let shape = r.m.u32(entry);
    let mut flags = 0x80u16;
    let mut work = 0;
    if !path3 {
        if r.m.u8(key + 0xc) != 0 {
            flags |= 0x800;
        }
        if r.m.u8(key + 0xd) != 0 {
            flags |= 0x400;
        }
        if r.m.u8(key + 0xe) != 0 {
            work = at(o, wd);
        }
    }
    let colour = packed_colour(r, at(o, wb));
    let frame = u32::from(r.m.u16(a + 2));
    let (rec, mut blend) = shape_frame(r, shape, frame);
    let magic = r.magic;
    let k = r.ref_index;
    let mut create = None;
    if path3 {
        if magic == 499 {
            blend = 0x48;
        }
        create = Some(blend_state(blend));
    } else {
        if (magic == 0x241 && k == 9)
            || ((magic == 0x251 || magic == 0x267) && k == 0xd)
            || magic == 0x1e6
            || magic == 0x1dd
        {
            work = 0;
        }
        if magic == 0x1a8 && blend == b'D' {
            blend = 0x48;
        }
        let mut c = blend_state(blend);
        let skip = r.scene.event == 0x6c && r.scene.map_part_id == 0x4d && k == 8;
        if (r.scene.event == 0x137 && magic == 0x13c) || (magic == 0x241 && k == 9) {
            c |= 0x800;
        }
        if !skip {
            create = Some(c);
        }
    }
    let draw = super::ShapeDraw {
        object: o,
        ref_index: k,
        entry,
        shape,
        frame,
        flags,
        colour,
        matrix: get_mat(r, o + 0x50),
        work_matrix: (work != 0).then(|| get_mat(r, work)),
        create,
        path3,
    };
    r.render.push(super::RenderCall::Shape(Box::new(draw)));
    shape_step(r, a, key, shape, rec);
}

/// PppDrawShape / pppDrawShapeX (part-level entries): unless the definition skips its draws (+0xc bit 0), each
/// object of the slot with its key moved on to the next when that one's time has come, then drawn.
fn draw_shape_group(r: &mut Run, s: u32, def: u32, rf: u32, k: u32, path3: bool) {
    if r.m.u32(def + 0xc) & 1 != 0 {
        return;
    }
    let stride = u32::from(r.m.u16(rf + 4));
    let mut o = r.m.u32(s + 4);
    for _ in 0..r.m.u16(s + 0xc) {
        let slot_at = o.wrapping_add(r.m.u32(def + 0x20)).wrapping_add(k * 4);
        let mut key = r.m.u32(slot_at);
        let next = key.wrapping_add(stride);
        if r.m.u32(next) == r.m.u32(o + 0xc) {
            r.m.set_u32(slot_at, next);
            key = next;
        }
        draw_shape_one(r, o, key, rf, path3);
        o = r.m.u32(o);
    }
}

fn draw_shape(r: &mut Run, s: u32, def: u32, rf: u32, k: u32) {
    draw_shape_group(r, s, def, rf, k, true)
}
fn draw_shape_x(r: &mut Run, s: u32, def: u32, rf: u32, k: u32) {
    draw_shape_group(r, s, def, rf, k, false)
}
fn zero_a1(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    r.m.set_u32(at(o, wa), 0);
}

// The lens family. pppKeLnsLp keeps a light's state in its first work (the "Th" engine): +0 a matrix, +0x30 the
// object's place through the camera, +0x40 a turn towards the light on the screen (and its row +0x70 placed
// along it), +0x7c the alpha, +0x80 the object's position, +0x8e a near margin byte, +0x8f facing (the
// object's own direction at +0x90), +0x9c / +0xa8 ramped values (+0xa8 the fade distance), +0xa4 the distance
// on the screen, +0xac 1. pppKeLnsFls draws a shape frame along it.

fn d(x: f32) -> f64 {
    f64::from(x)
}

/// The three- and two-dimensional lengths (summed in the original's order).
fn len3(v: [f32; 4]) -> f32 {
    (((d(v[0]) * d(v[0]) + d(v[1]) * d(v[1])) + d(v[2]) * d(v[2])).sqrt()) as f32
}
fn len2(v: [f32; 4]) -> f32 {
    ((d(v[0]) * d(v[0]) + d(v[1]) * d(v[1])).sqrt()) as f32
}

fn sub4f(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]]
}

fn scale4(v: [f32; 4], k: f32) -> [f32; 4] {
    [k * v[0], v[1] * k, v[2] * k, k * v[3]]
}

/// The light state's update (the "Th" engine) for the light work at `a` and the position at `pos`.
fn ke_th(r: &mut Run, a: u32, pos: u32) {
    let camera = r.camera;
    let cp = r.camera_pos;
    let facing = r.m.u8(a + 0x8f) != 0;
    let (mut p, mut q, mut dir);
    if !facing {
        let mut t = IDENTITY_M;
        t[12] = r.m.f32(a + 0x80) + cp[0];
        t[13] = r.m.f32(a + 0x84) + cp[1];
        t[14] = r.m.f32(a + 0x88) + cp[2];
        p = crate::chr::elements::apply4(&t, [0.0, 0.0, 0.0, 1.0]);
        p = scale4(p, 1.0 / p[3]);
        let mut u = IDENTITY_M;
        u[12..16].copy_from_slice(&cp);
        q = crate::chr::elements::apply4(&u, [0.0, 0.0, 0.0, 1.0]);
        q = scale4(q, 1.0 / q[3]);
        dir = sub4f(q, p);
        dir[1] *= 0.923_076_87;
        dir[2] = 512.0;
    } else {
        let mut m = camera;
        m[12] = 0.0;
        m[13] = 0.0;
        m[14] = 0.0;
        p = crate::chr::elements::apply4(&m, vec4(r, pos));
        q = scale4([camera[12], camera[13], camera[14], camera[15]], -1.0);
        dir = sub4f(q, p);
    }
    set(r, a, &IDENTITY_M.map(f32::to_bits));
    let at_cam = crate::chr::elements::apply4(&camera, vec4(r, pos));
    put_vec4(r, a + 0x30, at_cam);
    if dir[0] != 0.0 || dir[1] != 0.0 {
        let f = len2(dir);
        let rows = [dir[0] / f, dir[1] / f, 0.0, 0.0, dir[1] / f, -dir[0] / f, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        for (i, v) in rows.iter().enumerate() {
            r.m.set_f32(a + 0x40 + 4 * i as u32, *v);
        }
    }
    put_vec4(r, a + 0x70, at_cam);
    let z = if facing { r.m.f32(a + 0x38) } else { r.m.f32(a + 0x88) + cp[2] };
    let z = (d(z) * 0.001953125) as f32;
    r.m.set_f32(a + 0x78, z);
    let n = crate::chr::elements::normalize(dir);
    r.m.set_f32(a + 0x70, z * n[0]);
    r.m.set_f32(a + 0x74, z * n[1]);
    let v = sub4f(q, p);
    let l3 = len3(v);
    r.m.set_f32(a + 0xa4, len2(v));
    let nv = crate::chr::elements::normalize(v);
    let dot = |x: [f32; 4], y: [f32; 4]| ((d(x[2]) * d(y[2]) + d(x[0]) * d(y[0])) + d(x[1]) * d(y[1])) as f32;
    let alpha = if !facing {
        let fade = r.m.f32(a + 0xa8);
        if 0.0 < fade {
            let f = r.m.f32(a + 0xa4) / fade;
            if 1.0 < f { 0.0 } else { 1.0 - f }
        } else {
            0.0
        }
    } else {
        let t = (((1.0 - 0.5 * (1.0 - d(dot([0.0, 0.0, 1.0, 0.0], nv)))) as f32) as f64 / 0.3) as f32;
        let mut al = if d(t) <= 1.0 { (1.0 - d(t)) as f32 } else { 0.0 };
        let k = r.m.f32(a + 0x9c);
        if k > 0.0 {
            let d2 = dot(vec4(r, a + 0x90), nv);
            let f = if k == 0.0 { 0.0 } else { ((0.5 * (1.0 - d(d2))) as f32) / k };
            let g = if d(f) <= 1.0 { (1.0 - d(f)) as f32 } else { 0.0 };
            al *= g;
        }
        let near = r.m.f32(a + 0xa8) - f32::from(r.m.u8(a + 0x8e));
        if near <= 0.0 {
            0.0
        } else {
            let f = l3 / near;
            (if f < 1.0 { 1.0 - f } else { 0.0 }) * al
        }
    };
    r.m.set_f32(a + 0x7c, alpha);
}

const IDENTITY_M: Mat = crate::chr::elements::IDENTITY;

/// PppKeLnsLp: on the key's time +0xa8 += key +8, +0x8e = key byte +0xc, +0x9c += key +4, +0x8f = key byte
/// +0xd; facing: +0x90 = the object's z axis (w kept); +0x80 = the object's position; then the light state.
fn ke_lns_lp(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        r.m.set_f32(a + 0xa8, r.m.f32(key + 8) + r.m.f32(a + 0xa8));
        r.m.set_u8(a + 0x8e, r.m.u8(key + 0xc));
        r.m.set_f32(a + 0x9c, r.m.f32(key + 4) + r.m.f32(a + 0x9c));
        r.m.set_u8(a + 0x8f, r.m.u8(key + 0xd));
    }
    if r.m.u8(a + 0x8f) != 0 {
        let v = crate::chr::elements::apply4(&get_mat(r, o + 0x10), [0.0, 0.0, 1.0, 1.0]);
        for (i, x) in v.iter().take(3).enumerate() {
            r.m.set_f32(a + 0x90 + 4 * i as u32, *x);
        }
    }
    for i in 0..3 {
        r.m.set_u32(a + 0x80 + 4 * i, r.m.u32(o + 0x40 + 4 * i));
    }
    ke_th(r, a, o + 0x40);
}

fn ke_lns_lp_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    set(r, a + 0x40, &IDENTITY_M.map(f32::to_bits));
    r.m.set_f32(a + 0xac, 1.0);
    r.m.set_u32(a + 0x9c, 0);
    r.m.set_u32(a + 0xa8, 0);
}

fn ke_lns_lp_restart(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    r.m.set_u32(a + 0x9c, 0);
    r.m.set_u32(a + 0xa8, 0);
}

/// PppKeLnsFls: on the key's time the first work takes the light (+0 the address of the second work) and the
/// shape (+4 the list entry of key +4, its timer +8 set to the first frame's duration and its frame +0xa to 0 when
/// it changes; none for 0xffff); then a flare: the shape frame along the light, scaled (key +0xc, by the
/// distance on the screen over key u16 +0x10: modes key +0x12 / +0x13 for x / y: 1 the fade, 2 one plus the
/// ratio) and by the part's scale, coloured by the key's bytes +8..+0xb times the part's and the light's alpha.
fn ke_lns_fls(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, wb) = works(r, rf);
    let a = at(o, wa);
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        r.m.set_u32(a, at(o, wb));
        let n = r.m.u32(key + 4);
        if n == 0xffff {
            r.m.set_u32(a + 4, 0);
        } else {
            let entry = r.env.lists[1].wrapping_add(4 * n);
            if r.m.u32(a + 4) != entry {
                r.m.set_u32(a + 4, entry);
                let shape = r.m.u32(entry);
                r.m.set_u16(a + 8, r.m.u16(shape + 0x12));
                r.m.set_u8(a + 0xa, 0);
            }
        }
    }
    let pc = r.part_colour;
    let ch = |i: usize, b: u32| ((u32::from(pc[i]) * u32::from(r.m.u8(key + b))) >> 12) as u8;
    let colour = [ch(0, 8), ch(1, 9), ch(2, 0xa), ch(3, 0xb)];
    r.m.set_u8(a + 0xb, r.ref_index as u8);
    let id = r.m.u8(a + 0xb) as i8 as i32 as u32;
    let lp = r.m.u32(a);
    let entry = r.m.u32(a + 4);
    if lp == super::SCRATCH_VECTOR || entry == 0 || r.m.f32(lp + 0x7c) == 0.0 {
        r.render.push(super::RenderCall::Hide { object: o, ref_index: id });
        return;
    }
    let ratio = r.m.f32(lp + 0xa4) / f32::from(r.m.u16(key + 0x10));
    let fade = if ratio <= 1.0 { 1.0 - ratio } else { 0.0 };
    let mut m = if r.m.u8(lp + 0x8f) == 0 {
        let mut t = IDENTITY_M;
        t[12] = r.m.f32(lp + 0x80) + r.camera_pos[0];
        t[13] = r.m.f32(lp + 0x84) + r.camera_pos[1];
        t[14] = r.m.f32(lp + 0x88) + r.camera_pos[2];
        t
    } else {
        get_mat(r, lp)
    };
    let size = r.m.f32(key + 0xc);
    let by = |mode: u8| match mode {
        1 => size * fade,
        2 => size * (ratio + 1.0),
        _ => size,
    };
    m[0] = by(r.m.u8(key + 0x12));
    m[5] = by(r.m.u8(key + 0x13));
    let ps = r.part_scale;
    for c in 0..4 {
        m[c] *= ps[0];
        m[4 + c] *= ps[1];
    }
    let alpha = (f32::from(colour[3]) * r.m.f32(lp + 0x7c) * fade).round_ties_even() as i32 as u8;
    // The shape's frame step (by 0x200 a frame) before the draw.
    let shape = r.m.u32(entry);
    let frame = u32::from(r.m.u8(a + 0xa));
    let rec = shape + frame * 8;
    let t = r.m.u16(a + 8).wrapping_add(0x200);
    r.m.set_u16(a + 8, t);
    let dur = r.m.u16(rec + 0x12) as i16;
    if dur <= t as i16 {
        r.m.set_u16(a + 8, t.wrapping_sub(dur as u16));
        let f = r.m.u8(a + 0xa).wrapping_add(1);
        r.m.set_u8(a + 0xa, f);
        if (r.m.u16(shape + 6) as i16) <= i16::from(f) {
            r.m.set_u16(a + 8, 0);
            if r.m.u8(rec + 0x14) & 0x80 != 0 {
                r.m.set_u8(a + 0xa, 0);
            } else {
                r.m.set_u8(a + 0xa, f.wrapping_sub(1));
            }
        }
    }
    let frame = u32::from(r.m.u8(a + 0xa));
    let (_, blend) = shape_frame(r, shape, frame);
    let draw = super::ShapeDraw {
        object: o,
        ref_index: id,
        entry,
        shape,
        frame,
        flags: 0,
        colour: [colour[0], colour[1], colour[2], alpha],
        matrix: m,
        work_matrix: None,
        create: Some(blend_state(blend) | 0x800),
        path3: false,
    };
    r.render.push(super::RenderCall::Shape(Box::new(draw)));
}

fn ke_lns_fls_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    r.m.set_u32(a, super::SCRATCH_VECTOR);
    r.m.set_u32(a + 4, 0);
}

/// A copy of `n` bytes at `at`.
fn bytes(r: &Run, at_: u32, n: u32) -> Vec<u8> {
    (0..n).map(|i| r.m.u8(at_.wrapping_add(i))).collect()
}

fn geometry(
    r: &mut Run,
    program: &'static str,
    o: u32,
    key: u32,
    rf: u32,
    sizes: (u32, u32, u32),
    extra: Vec<u8>,
) {
    let (wa, wb) = works(r, rf);
    let n = r.m.u32(key + 4);
    let entry = r.env.lists[1].wrapping_add(4 * n);
    let draw = super::GeometryDraw {
        program,
        object: o,
        ref_index: r.ref_index,
        key: bytes(r, key, sizes.0),
        work: bytes(r, at(o, wa), sizes.1),
        colour_work: bytes(r, at(o, wb), sizes.2),
        entry,
        shape: r.m.u32(entry),
        matrix: get_mat(r, o + 0x10),
        age: r.m.u32(o + 0xc),
        part_scale: r.part_scale,
        part_colour: r.part_colour,
        camera: r.camera,
        extra,
    };
    r.render.push(super::RenderCall::Geometry(Box::new(draw)));
}

/// The tails' position history: on the object's first frame every entry is its position (w 1); then the
/// ring's index steps back (from `len` when 0) and that entry takes the position (w kept).
fn tail_ring(r: &mut Run, o: u32, ring: u32, index: u32, len: u8) {
    let pos = vec4(r, o + 0x40);
    if r.m.u32(o + 0xc) == 0 {
        for i in 0..u32::from(len) {
            put_vec4(r, ring + 0x10 * i, [pos[0], pos[1], pos[2], 1.0]);
        }
    }
    if r.m.u8(index) == 0 {
        r.m.set_u8(index, len);
    }
    let k = r.m.u8(index).wrapping_sub(1);
    r.m.set_u8(index, k);
    let e = ring + 0x10 * u32::from(k);
    for i in 0..3 {
        r.m.set_f32(e + 4 * i, pos[i as usize]);
    }
}

/// PppKeShpTail2 (update): the first work's byte +0 is the ring's length, +1 its index, the ring from +0x10.
fn ke_shp_tail2(r: &mut Run, o: u32, _key: u32, rf: u32) {
    if r.paused {
        return;
    }
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    let len = r.m.u8(a);
    tail_ring(r, o, a + 0x10, a + 1, len);
}

/// PppKeShpTail2 (draw): the trail (its geometry built by the renderer from the ring), then the shape's frame
/// step (timer u16 +2 by key i16 +8, frame u16 +4).
fn ke_shp_tail2_draw(r: &mut Run, o: u32, key: u32, rf: u32) {
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let len = u32::from(r.m.u8(at(o, works(r, rf).0)));
    geometry(r, "pppKeShpTail2", o, key, rf, (0x24, 0x10 + 0x10 * len, 8), Vec::new());
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    let shape = r.m.u32(r.env.lists[1].wrapping_add(4 * n));
    let rec = shape + 0x10 + 8 * u32::from(r.m.u16(a + 4));
    if !r.paused {
        let t = r.m.u16(a + 2).wrapping_add(r.m.u16(key + 8));
        r.m.set_u16(a + 2, t);
        let dur = r.m.u16(rec + 2) as i16 as i32;
        if dur <= i32::from(t) {
            let f = r.m.u16(a + 4).wrapping_add(1);
            r.m.set_u16(a + 4, f);
            r.m.set_u16(a + 2, t.wrapping_sub(dur as u16));
            if (r.m.u16(shape + 6) as i16 as i32) <= i32::from(f) {
                if r.m.u8(rec + 4) & 0x80 != 0 {
                    r.m.set_u32(a + 2, 0);
                    return;
                }
                r.m.set_u16(a + 2, 0);
                r.m.set_u16(a + 4, f.wrapping_sub(1));
            }
        }
    }
}

fn ke_shp_tail2_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    r.m.set_u32(a + 2, 0);
    r.m.set_u16(a, 0x1f);
}

/// PppKeShpTail3 (update): the ring of 0x1c positions from +0x30 (index byte +0x1f8) and two colours of four
/// i16 (+0 and +8) with their rates (+0x10, +0x20) and accelerations (+0x18, +0x28), on the key's time each
/// taking the key's (+0x20.., +0x28.., +0x30.., +0x40.., +0x38.., +0x48..).
fn ke_shp_tail3(r: &mut Run, o: u32, key: u32, rf: u32) {
    if r.paused {
        return;
    }
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    tail_ring(r, o, a + 0x30, a + 0x1f8, 0x1c);
    let h = |r: &Run, i: u32| r.m.u16(a + 2 * i);
    let mut v: Vec<u16> = (0..24).map(|i| h(r, i)).collect();
    for set in [0usize, 4] {
        let (val, rate, acc) = (set, 8 + 2 * set, 12 + 2 * set);
        for c in 0..4 {
            v[val + c] = v[val + c].wrapping_add(v[rate + c]);
            v[rate + c] = v[rate + c].wrapping_add(v[acc + c]);
        }
    }
    if r.m.u32(o + 0xc) == r.m.u32(key) {
        let k = |r: &Run, off: u32| r.m.u16(key + off);
        for c in 0..4u32 {
            let ci = c as usize;
            v[ci] = v[ci].wrapping_add(k(r, 0x20 + 2 * c));
            v[8 + ci] = v[8 + ci].wrapping_add(k(r, 0x30 + 2 * c));
            v[12 + ci] = v[12 + ci].wrapping_add(k(r, 0x38 + 2 * c));
            v[4 + ci] = v[4 + ci].wrapping_add(k(r, 0x28 + 2 * c));
            v[16 + ci] = v[16 + ci].wrapping_add(k(r, 0x40 + 2 * c));
            v[20 + ci] = v[20 + ci].wrapping_add(k(r, 0x48 + 2 * c));
        }
    }
    for (i, x) in v.iter().enumerate() {
        r.m.set_u16(a + 2 * i as u32, *x);
    }
}

/// PppKeShpTail3 (draw, unpaused only): the trail (built by the renderer), then the time +0x1f0 += key +8.
fn ke_shp_tail3_draw(r: &mut Run, o: u32, key: u32, rf: u32) {
    if r.paused {
        return;
    }
    geometry(r, "pppKeShpTail3", o, key, rf, (0x50, 0x200, 8), Vec::new());
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    r.m.set_u32(a + 0x1f0, r.m.u32(a + 0x1f0).wrapping_add(r.m.u32(key + 8)));
}

/// The init: the index, time and a C-runtime random seed (+0x1f6), the colours cleared.
fn ke_shp_tail3_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    r.m.set_u8(a + 0x1f8, 0);
    r.m.set_u16(a + 0x1f4, 0);
    r.m.set_u32(a + 0x1f0, 0);
    let v = r.crt.rand();
    r.m.set_u16(a + 0x1f6, v as u16);
    set(r, a, &[0; 12]);
}

fn ke_shp_tail3_restart(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    r.m.set_u8(a + 0x1f8, 0);
    r.m.set_u16(a + 0x1f4, 0);
    r.m.set_u32(a + 0x1f0, 0);
    set(r, a, &[0; 12]);
}

/// The random vector helpers of the shape field: each of x, y, z += the symmetric draw of the key's (times a
/// unit draw when `scaled`); and += (r · k − k) with r a double symmetric draw.
fn rand_add(r: &mut Run, v: u32, k: u32, scaled: bool) {
    for i in 0..3 {
        let x = r.m.f32(k + 4 * i);
        let d = if scaled {
            let s = r.vu_sym(x);
            r.vu_unit(1.0) * s
        } else {
            r.vu_sym(x)
        };
        let out = (d + f64::from(r.m.f32(v + 4 * i))) as f32;
        r.m.set_f32(v + 4 * i, out);
    }
}

fn rand_spread(r: &mut Run, v: u32, k: u32) {
    let a = r.vu_sym(1.0) as f32;
    let f = ((f64::from(a)) + f64::from(a)) as f32;
    let kv = |i: u32| r.m.f32(k + 4 * i);
    let (k0, k1, k2) = (kv(0), kv(1), kv(2));
    r.m.set_f32(v, r.m.f32(v) + (f * k0 - k0));
    r.m.set_f32(v + 4, (k1 * f - k1) + r.m.f32(v + 4));
    r.m.set_f32(v + 8, (f * k2 - k2) + r.m.f32(v + 8));
}

/// The shape field's spawn: every key byte +0xc1 frames, key byte +0xc0 sprites at the next index (of key +8):
/// live; timer, frame, age 0; position (+0x10) about key +0x10 by the random key +0x10 (scaled by the key's flag
/// +0x20); velocity (+0x20) key +0x40 ± key +0x50; +0x30 key +0x70 spread by key +0x80; its rate +0x40 key +0x90;
/// +0x50 random by key +0xb0, whose z (degrees, wrapped into [0, 360]) turns the sprite's matrix (+0x60).
fn field_spawn(r: &mut Run, a: u32, key: u32) {
    let wait = r.m.u32(a + 0x48);
    let wait = if wait == 0 {
        for _ in 0..r.m.u8(key + 0xc0) {
            let p = r.m.u32(a + 0x40) + r.m.u32(a + 0x4c) * 0xa0;
            r.m.set_u32(p, r.m.u32(p) | 1);
            r.m.set_u32(p + 4, 0);
            r.m.set_u32(p + 8, 0);
            put_vec4(r, p + 0x10, [0.0, 0.0, 0.0, 1.0]);
            for (dst, src) in [(0x20, 0x40), (0x30, 0x70), (0x40, 0x90)] {
                for i in 0..3 {
                    r.m.set_u32(p + dst + 4 * i, r.m.u32(key + src + 4 * i));
                }
                r.m.set_u32(p + dst + 0xc, 0);
            }
            rand_add(r, p + 0x20, key + 0x50, false);
            rand_spread(r, p + 0x30, key + 0x80);
            put_vec4(r, p + 0x50, [0.0, 0.0, 0.0, 1.0]);
            rand_add(r, p + 0x50, key + 0xb0, false);
            let t = r.m.f32(p + 0x58);
            let t = if 0.0 > t {
                (f64::from(t) + 360.0) as f32
            } else if 360.0 < t {
                (f64::from(t) - 360.0) as f32
            } else {
                t
            };
            let ang = ((f64::from(t) * 4096.0 * 16.0) / 360.0).trunc() as i32 as u32;
            put_mat(r, p + 0x60, &turn(b'z', ang));
            put_vec4(r, p + 0x10, [0.0, 0.0, 0.0, 1.0]);
            let scaled = r.m.u8(key + 0x20) != 0;
            rand_add(r, p + 0x10, key + 0x10, scaled);
            let next = r.m.u32(a + 0x4c) + 1;
            r.m.set_u32(a + 0x4c, if r.m.u32(key + 8) <= next { 0 } else { next });
        }
        u32::from(r.m.u8(key + 0xc1))
    } else {
        wait
    };
    r.m.set_u32(a + 0x48, wait.wrapping_sub(1));
}

/// The shape field's step: each live sprite ages (dying at the life +0x54), its velocity takes key +0x60 and its
/// position the velocity, +0x40 takes key +0xa0 and +0x30 that; its frame steps (key i16 +0xc) through the shape.
fn field_step(r: &mut Run, a: u32, key: u32, shape: u32) {
    let ps = r.m.u32(a + 0x40);
    for i in 0..r.m.u32(key + 8) {
        let p = ps + i * 0xa0;
        if r.m.u32(p) & 1 == 0 {
            continue;
        }
        let frame = r.m.u16(p + 6);
        let age = r.m.u32(p + 8).wrapping_add(1);
        r.m.set_u32(p + 8, age);
        if r.m.u32(a + 0x54) <= age {
            r.m.set_u32(p, r.m.u32(p) & !1);
        }
        for i in 0..3 {
            let v = r.m.f32(key + 0x60 + 4 * i) + r.m.f32(p + 0x20 + 4 * i);
            r.m.set_f32(p + 0x20 + 4 * i, v);
        }
        for i in 0..4 {
            let v = r.m.f32(p + 0x10 + 4 * i) + r.m.f32(p + 0x20 + 4 * i);
            r.m.set_f32(p + 0x10 + 4 * i, v);
        }
        for i in 0..3 {
            let v = r.m.f32(key + 0xa0 + 4 * i) + r.m.f32(p + 0x40 + 4 * i);
            r.m.set_f32(p + 0x40 + 4 * i, v);
        }
        for i in 0..3 {
            let v = r.m.f32(p + 0x30 + 4 * i) + r.m.f32(p + 0x40 + 4 * i);
            r.m.set_f32(p + 0x30 + 4 * i, v);
        }
        let t = r.m.u16(key + 0xc).wrapping_add(r.m.u16(p + 4));
        r.m.set_u16(p + 4, t);
        let rec = shape + 0x10 + 8 * u32::from(frame);
        let dur = r.m.u16(rec + 2) as i16 as i32;
        if dur <= i32::from(t) {
            let f = frame.wrapping_add(1);
            r.m.set_u16(p + 4, t.wrapping_sub(dur as u16));
            r.m.set_u16(p + 6, f);
            if (r.m.u16(shape + 6) as i16 as i32) <= i32::from(f) {
                if r.m.u8(rec + 4) & 0x80 == 0 {
                    r.m.set_u16(p + 4, 0);
                    r.m.set_u16(p + 6, frame);
                } else {
                    r.m.set_u32(p + 4, 0);
                }
            }
        }
    }
}

/// The shape field's draw: each live sprite turned to face the camera (the world's and the frame's own turns
/// undone), then turned and scaled by its own matrix and scale, at its position plus the object's third work
/// vector, in the world and frame; its colour is the field's history for its age times the part's. On most
/// maps every sprite goes to the field's one entry; on the maps that keep their entries each sprite is a
/// shape draw of its own (keyed by the reference index plus 0x32 a sprite). Map 0x1ca draws no field whose
/// origin falls outside a screen window.
fn draw_field_sprites(r: &mut Run, o: u32, key: u32, rf: u32, a: u32, entry: u32, shape: u32) {
    use super::driver::rigid_inverse;
    use crate::chr::elements::{IDENTITY, mul, normalize};
    let (_, _, wc) = works3(r, rf);
    let (event, part, map) = (r.scene.event, r.scene.map_part, r.map_id);
    let mut flags = 0u16;
    if r.m.u8(key + 0xc2) != 0 {
        flags = 0x800;
    }
    if r.m.u8(key + 0xc3) != 0 {
        flags |= 0x400;
    }
    let no_work = (event == 0xdb && part == Some(0x12)) || (event == 0x16 && part == Some(2));
    let with_work = r.m.u8(key + 0xc4) != 0 && !no_work;
    // The world's and the frame's turns (the frame's axes normalised), translations cleared, inverted.
    let mut world_turn = r.world;
    world_turn[12..15].fill(0.0);
    let mut frame_turn = r.frame;
    for c in 0..3 {
        let axis = normalize([
            frame_turn[c * 4],
            frame_turn[c * 4 + 1],
            frame_turn[c * 4 + 2],
            frame_turn[c * 4 + 3],
        ]);
        frame_turn[c * 4..c * 4 + 4].copy_from_slice(&axis);
    }
    frame_turn[12..15].fill(0.0);
    let unturn = mul(&rigid_inverse(&frame_turn), &rigid_inverse(&world_turn));
    // The key's work offset: its scale (1 when all three are 0) and its place.
    let mut offset = IDENTITY;
    let key_scale = [r.m.f32(key + 0xe0), r.m.f32(key + 0xe4), r.m.f32(key + 0xe8)];
    if key_scale != [0.0; 3] {
        offset[0] = key_scale[0];
        offset[5] = key_scale[1];
        offset[10] = key_scale[2];
    }
    offset[12] = r.m.f32(key + 0xd0);
    offset[13] = r.m.f32(key + 0xd4);
    offset[14] = r.m.f32(key + 0xd8);
    let world_frame = mul(&r.world, &r.frame);
    if map == 0x1ca {
        let p = mul(&r.screen, &world_frame);
        let (x, y, z) = (p[12] / p[15], p[13] / p[15], p[14] / p[15]);
        // Outside the window: below its low edge or at or past its high one (a NaN is neither, and draws).
        let outside = |v: f32, low: f32, high: f32| v < low || high <= v;
        if outside(x, 1604.7, 2491.3) || outside(y, 1756.8, 2339.2) || 4_194_304.0 <= z || z <= 0.0 {
            return;
        }
    }
    let first_blend = |r: &Run, frame: u16| {
        let pieces = shape.wrapping_add(r.m.u16(shape + 0x10 + u32::from(frame) * 8) as i16 as i32 as u32);
        r.m.u8(pieces.wrapping_add(r.m.u32(pieces + 4)) + 4)
    };
    let (sprite_list, history, third) = (r.m.u32(a + 0x40), r.m.u32(a + 0x50), at(o, wc));
    let separate = field_keeps_entries(map);
    let mut sprites = Vec::new();
    for i in 0..r.m.u32(key + 8) {
        let p = sprite_list + i * 0xa0;
        if r.m.u32(p) & 1 == 0 {
            continue;
        }
        let frame = r.m.u16(p + 6);
        let colour = packed_colour(r, history.wrapping_add(r.m.u32(p + 8).wrapping_mul(8)));
        let mut scaled = IDENTITY;
        scaled[0] = r.m.f32(p + 0x30);
        scaled[5] = r.m.f32(p + 0x34);
        scaled[10] = r.m.f32(p + 0x38);
        let facing = mul(&unturn, &mul(&get_mat(r, p + 0x60), &scaled));
        let mut place = IDENTITY;
        for k in 0..3 {
            place[12 + k] = r.m.f32(p + 0x10 + 4 * k as u32) + r.m.f32(third + 4 * k as u32);
        }
        if map == 0xc04 && event == 0x77 && matches!(part, Some(4 | 5)) {
            place[14] += 6.0;
        }
        let placed = mul(&place, &facing);
        let matrix = mul(&world_frame, &placed);
        let work_matrix = with_work.then(|| match r.m.u8(key + 0xf0) {
            0 => mul(&world_frame, &mul(&place, &mul(&offset, &facing))),
            1 => mul(&offset, &mul(&r.world, &mul(&r.frame, &placed))),
            2 => mul(&r.world, &mul(&offset, &mul(&r.frame, &placed))),
            _ => facing,
        });
        if separate {
            let draw = super::ShapeDraw {
                object: o,
                ref_index: r.ref_index.wrapping_add(i * 0x32),
                entry,
                shape,
                frame: u32::from(frame),
                flags,
                colour,
                matrix,
                work_matrix,
                create: Some(blend_state(first_blend(r, frame))),
                path3: false,
            };
            r.render.push(super::RenderCall::Shape(Box::new(draw)));
        } else {
            sprites.push(super::FieldSprite { frame, colour, matrix, work_matrix });
        }
    }
    if separate {
        return;
    }
    let mut create = blend_state(first_blend(r, 0));
    if event == 0xdb && part == Some(0x12) {
        create |= 0x800;
    }
    let draw = super::ShapeFieldDraw { object: o, ref_index: r.ref_index, shape, flags, create, sprites };
    r.render.push(super::RenderCall::ShapeField(Box::new(draw)));
}

/// Maps whose shape fields keep their render entries (they are freed with the sprites instead).
fn field_keeps_entries(map: u32) -> bool {
    matches!(map, 0x76f | 0x18e7 | 0x5c8 | 0x1f7 | 0x4c4 | 0x1b8a | 0x324 | 0x1e8a | 0x1139)
}

/// PppDrawShapeField: a field of sprites (key +8 of them, 0xa0 bytes each in the pool at the first work's +0x40,
/// with a 0xc-byte buffer at +0x44 and a colour history of the life's frames at +0x50, made on the first
/// draw): the second work's colour into the history while the object is young; near the definition's end the
/// object's age held while sprites live; unless paused (and the part not draining) the spawn; the draw; the
/// step.
fn draw_shape_field(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (wa, wb) = works(r, rf);
    let a = at(o, wa);
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let entry = r.env.lists[1].wrapping_add(4 * n);
    let shape = r.m.u32(entry);
    if shape == 0 {
        return;
    }
    if r.m.u32(a + 0x40) == 0 {
        let count = r.m.u32(key + 8);
        let p = r.env.alloc(r.m, count.wrapping_mul(0xa0));
        r.m.set_u32(a + 0x40, p);
        if p == 0 {
            return;
        }
        for i in 0..count * 0xa0 {
            r.m.set_u8(p + i, 0);
        }
        let q = r.env.alloc(r.m, 0xc);
        r.m.set_u32(a + 0x44, q);
        if q == 0 {
            return;
        }
        let life = r.m.u32(a + 0x54);
        let h = r.env.alloc(r.m, life << 3);
        r.m.set_u32(a + 0x50, h);
        if h == 0 {
            return;
        }
        for i in 0..life << 3 {
            r.m.set_u8(h + i, 0);
        }
    }
    let age = r.m.u32(o + 0xc) as i32;
    let t = ((age + (age >> 31 & 0xfff)) >> 12) as u32;
    if r.m.u32(a + 0x58) == 0 && t < r.m.u32(a + 0x54) {
        let h = r.m.u32(a + 0x50) + t * 8;
        let b = at(o, wb);
        for i in 0..4 {
            r.m.set_u16(h + 2 * i, r.m.u16(b + 2 * i));
        }
    }
    let def = r.m.u32(r.m.u32(o + 8));
    if (r.m.u32(def + 0x1c).wrapping_sub(0x1000) as i32) <= age {
        let ps = r.m.u32(a + 0x40);
        let count = r.m.u32(key + 8);
        if (0..count).any(|i| r.m.u32(ps + i * 0xa0) & 1 != 0) {
            r.m.set_u32(o + 0xc, (age - 0x1000) as u32);
        }
        r.m.set_u32(a + 0x58, 1);
    }
    if !r.paused && !r.part_draining {
        field_spawn(r, a, key);
    }
    draw_field_sprites(r, o, key, rf, a, entry, shape);
    if !r.paused {
        field_step(r, a, key, shape);
    }
}

fn draw_shape_field_init(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    set(r, a, &IDENTITY_M.map(f32::to_bits));
    set(r, a + 0x40, &[0; 5]);
    let def = r.m.u32(r.m.u32(o + 8));
    let span = r.m.u32(def + 0x1c).wrapping_sub(r.m.u32(def + 0x18)) as i32;
    r.m.set_u32(a + 0x54, (((span + (span >> 31 & 0xfff)) >> 12) + 1) as u32);
    r.m.set_u32(a + 0x58, 0);
}

fn draw_shape_field_free(r: &mut Run, o: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let a = at(o, wa);
    if !field_keeps_entries(r.map_id) {
        r.render.push(super::RenderCall::Destroy { object: o, ref_index: r.ref_index });
    }
    for off in [0x40, 0x44, 0x50] {
        let p = r.m.u32(a + off);
        if p != 0 {
            r.env.free(r.m, p);
        }
    }
}

/// Maps with a sea (pppDrawMdlSea draws only there).
fn sea_map(map: u32) -> bool {
    matches!(
        map,
        0x199
            | 0x17e6
            | 0x24c9
            | 0xa95
            | 0x245b
            | 0xa27
            | 0x170a
            | 0xe58
            | 0x1b3b
            | 0xb3
            | 0x1acd
            | 0x99
            | 0xe6c
            | 0xf48
    )
}

/// PppDrawMdlSea (on the sea maps): the third work (+0x40 a wave phase vector, +0x50.. u and v with speeds and
/// accelerations, +0x68 the model's resting vertices, +0x6c a byte per vertex, +0x70 the key, +0x74 / +0x75 /
/// +0x76 the texture animation's wait, frame and count, +0x77 waves on), made on the first draw; unless paused,
/// the scroll ramps (key +8..+0x1c), and with waves each flagged vertex of the model moved from rest by the sine
/// of (phase + x + z) · key +0x40 / +0x44 / +0x48, times key +0x20 / +0x24 / +0x28, the phase moving by key
/// +0x30..; the draw (always semi-transparent; key +0x52 the blend (0xff: the model's), +0x54 flag 0x10, +0x55
/// facing, +0x56 / +0x57 flags 0x200 / 8).
fn draw_mdl_sea(r: &mut Run, o: u32, key: u32, rf: u32) {
    if !sea_map(r.map_id) {
        return;
    }
    let n = r.m.u32(key + 4);
    if n == 0xffff {
        return;
    }
    let (wa, _, wc) = works3(r, rf);
    let c = at(o, wc);
    let model = r.m.u32(image_record(r, n) + 0x1c);
    if r.m.u32(c + 0x70) == 0 && !sea_setup(r, key, c, model) {
        return;
    }
    if !r.paused {
        if r.m.u32(key) == r.m.u32(o + 0xc) {
            for i in 0..6 {
                let v = r.m.f32(key + 8 + 4 * i) + r.m.f32(c + 0x50 + 4 * i);
                r.m.set_f32(c + 0x50 + 4 * i, v);
            }
        }
        let du = r.m.f32(c + 0x58) + r.m.f32(c + 0x54);
        r.m.set_f32(c + 0x54, du);
        let dv = r.m.f32(c + 0x64) + r.m.f32(c + 0x60);
        r.m.set_f32(c + 0x60, dv);
        let u = du + r.m.f32(c + 0x50);
        r.m.set_f32(c + 0x50, (f64::from(u) % 32768.0) as f32);
        let v = dv + r.m.f32(c + 0x5c);
        r.m.set_f32(c + 0x5c, (f64::from(v) % 32768.0) as f32);
        if r.m.u8(c + 0x77) != 0 {
            let base = r.m.u32(c + 0x68);
            let flags = r.m.u32(c + 0x6c);
            let verts = model.wrapping_add(r.m.u32(model + 8));
            let t = crate::traced_tables::sine();
            let sine = |x: i32| t[((x >> 4) & 0xfff) as usize];
            let ftol = |x: f64| x.trunc() as i32;
            for i in 0..(r.m.u16(model + 0x12) as i16).max(0) as u32 {
                if r.m.u8(flags + i) == 0 {
                    continue;
                }
                let b = [r.m.u16(base + 6 * i), r.m.u16(base + 6 * i + 2), r.m.u16(base + 6 * i + 4)];
                let s = (i32::from(b[0] as i16) + i32::from(b[2] as i16)) as f32;
                let ph = |k: u32| f64::from(r.m.f32(c + 0x40 + 4 * k));
                let kf = |off: u32| f64::from(r.m.f32(key + off));
                let e1 = ftol((ph(1) + f64::from(s)) * kf(0x44));
                let e2 = ftol((ph(2) + f64::from(s)) * kf(0x48));
                let e3 = ftol((f64::from(s) + ph(0)) * kf(0x40));
                let x = ftol(f64::from(sine(e3)) * kf(0x20)) as u16;
                let y = ftol(f64::from(sine(e1)) * kf(0x24)) as u16;
                let z = ftol(f64::from(sine(e2)) * kf(0x28)) as u16;
                let out = verts + 6 * i;
                r.m.set_u16(out, x.wrapping_add(b[0]));
                r.m.set_u16(out + 2, y.wrapping_add(b[1]));
                r.m.set_u16(out + 4, z.wrapping_add(b[2]));
            }
            for k in 0..3 {
                let v = r.m.f32(key + 0x30 + 4 * k) + r.m.f32(c + 0x40 + 4 * k);
                r.m.set_f32(c + 0x40 + 4 * k, v);
            }
        }
        if r.m.u8(key + 0x51) != 0 {
            let w = r.m.u8(c + 0x74).wrapping_add(1);
            r.m.set_u8(c + 0x74, w);
            if r.m.u8(key + 0x51) <= w {
                // The texture animation's next frame (the map's texture animation objects, which drive the
                // remaster's texture swap) goes to the renderer.
                r.render.push(super::RenderCall::TextureStep { id: u32::from(r.m.u8(key + 0x50)) });
                r.m.set_u8(c + 0x74, 0);
            }
        }
    }
    let colour = packed_colour(r, at(o, wa));
    if colour[3] == 0 {
        return;
    }
    let mut f = if r.m.u8(key + 0x54) != 0 { 0x14 } else { 4 };
    if r.m.u8(key + 0x55) != 0 {
        f = f & !0x10 | 0x2000;
    }
    if r.m.u8(key + 0x56) != 0 {
        f |= 0x200;
    }
    if r.m.u8(key + 0x57) != 0 {
        f |= 8;
    }
    let blend = match r.m.u8(key + 0x52) {
        0xff => r.m.u8(model + 0x16),
        b => b,
    };
    let mut cr = blend_state(blend);
    if f & 8 != 0 {
        cr |= 0x40;
    }
    let u = (f64::from(r.m.f32(c + 0x50)) * 65536.0).trunc() as i32 as u32;
    let v = (65536.0 * f64::from(r.m.f32(c + 0x5c))).trunc() as i32 as u32;
    let args = ModelArgs {
        model_no: n,
        flags: f,
        blend,
        texture_word: 0,
        offsets: (u, v),
        colour,
        work: 0,
        create: cr,
        finish: true,
        fade: None,
    };
    emit_model(r, o, args);
}

/// The sea's set-up: its ramps cleared, the key kept, the texture animation's wait from key +0x51; waves when
/// key +0x20..+0x28 are not all 0: the model's resting vertices copied into the pool (6 bytes each) and a flag
/// byte of 1 per vertex.
fn sea_setup(r: &mut Run, key: u32, c: u32, model: u32) -> bool {
    set(r, c + 0x40, &[0; 3]);
    set(r, c + 0x50, &[0; 6]);
    r.m.set_u32(c + 0x68, 0);
    r.m.set_u32(c + 0x6c, 0);
    r.m.set_u32(c + 0x70, key);
    r.m.set_u8(c + 0x74, r.m.u8(key + 0x51));
    r.m.set_u16(c + 0x75, 0);
    let waves = !(r.m.f32(key + 0x20) == 0.0 && r.m.f32(key + 0x24) == 0.0 && r.m.f32(key + 0x28) == 0.0);
    r.m.set_u8(c + 0x77, u8::from(waves));
    if waves {
        let count = (r.m.u16(model + 0x12) as i16) as i32 as u32;
        let base = r.env.alloc(r.m, count.wrapping_mul(6));
        r.m.set_u32(c + 0x68, base);
        if base == 0 {
            return false;
        }
        let flags = r.env.alloc(r.m, count);
        r.m.set_u32(c + 0x6c, flags);
        if flags == 0 {
            return false;
        }
        for i in 0..count {
            r.m.set_u8(flags + i, 1);
        }
        let verts = model.wrapping_add(r.m.u32(model + 8));
        for i in 0..count * 3 {
            r.m.set_u16(base + 2 * i, r.m.u16(verts + 2 * i));
        }
    }
    set(r, c, &IDENTITY_M.map(f32::to_bits));
    true
}

fn draw_mdl_sea_init(r: &mut Run, o: u32, rf: u32) {
    let (_, _, wc) = works3(r, rf);
    let c = at(o, wc);
    set(r, c + 0x68, &[0; 3]);
}

fn draw_mdl_sea_restart(r: &mut Run, o: u32, rf: u32) {
    let (_, _, wc) = works3(r, rf);
    let c = at(o, wc);
    for off in [0x54, 0x58, 0x60, 0x64] {
        r.m.set_u32(c + off, 0);
    }
}

fn draw_mdl_sea_free(r: &mut Run, o: u32, rf: u32) {
    let (_, _, wc) = works3(r, rf);
    let c = at(o, wc);
    for off in [0x68, 0x6c] {
        let p = r.m.u32(c + off);
        if p != 0 {
            r.env.free(r.m, p);
        }
    }
}

const ONE_W: [u32; 4] = [0, 0, 0, 0x3f80_0000];

fn set(r: &mut Run, dst: u32, v: &[u32]) {
    for (i, x) in v.iter().enumerate() {
        r.m.set_u32(dst + 4 * i as u32, *x);
    }
}

fn init_b_one(r: &mut Run, o: u32, rf: u32) {
    let (_, b) = works(r, rf);
    set(r, at(o, b), &ONE_W);
}
fn init_b_zero(r: &mut Run, o: u32, rf: u32) {
    let (_, b) = works(r, rf);
    set(r, at(o, b), &[0; 4]);
}
fn init_b_short(r: &mut Run, o: u32, rf: u32) {
    let (_, b) = works(r, rf);
    let dst = at(o, b.wrapping_add(r.env.scratch_offset));
    set(r, dst, &[0; 2]);
    r.env.scratch_offset = 0;
}
fn init_a_one(r: &mut Run, o: u32, rf: u32) {
    let (a, _) = works(r, rf);
    set(r, at(o, a), &ONE_W);
}
fn init_a_zero(r: &mut Run, o: u32, rf: u32) {
    let (a, _) = works(r, rf);
    set(r, at(o, a), &[0; 4]);
}
fn init_a_short(r: &mut Run, o: u32, rf: u32) {
    let (a, _) = works(r, rf);
    let dst = at(o, a.wrapping_add(r.env.scratch_offset));
    set(r, dst, &[0; 2]);
    r.env.scratch_offset = 0;
}

fn random_update(name: &str) -> Option<super::ObjectFn> {
    Some(match name {
        "pppRandFV" => rand_fv,
        "pppRandUpFV" => rand_up_fv,
        "pppRandDownFV" => rand_down_fv,
        "pppSRandFV" => srand_fv,
        "pppSRandUpFV" => srand_up_fv,
        "pppSRandDownFV" => srand_down_fv,
        "pppRandFloat" => rand_float,
        "pppRandIV" => rand_iv,
        "pppRandUpIV" => rand_up_iv,
        "pppRandDownIV" => rand_down_iv,
        "pppRandHCV" => rand_hcv,
        "pppRandUpHCV" => rand_up_hcv,
        "pppRandDownHCV" => rand_down_hcv,
        "pppSRandHCV" => srand_hcv,
        "pppSRandUpHCV" => srand_up_hcv,
        "pppSRandDownHCV" => srand_down_hcv,
        _ => return None,
    })
}

/// The program called `name`, when it is ported.
pub fn by_name(name: &'static str) -> Option<Program> {
    type Upd = fn(&mut Run, u32, u32, u32);
    type Ini = fn(&mut Run, u32, u32);
    let p = |update: Upd, init: Ini| Program {
        name,
        update: Some(update),
        init: Some(init),
        restart: Some(init),
        ..Program::default()
    };
    let upd = |update: Upd| Program { name, update: Some(update), ..Program::default() };
    Some(match name {
        "pppAccele" | "pppMove" | "pppSclAccele" | "pppSclMove" => p(rate_f, init_b_one),
        "pppAngAccele" | "pppAngMove" => p(rate_i, init_b_zero),
        "pppColAccele" | "pppColMove" => p(rate_s, init_b_short),
        "pppPoint" | "pppScale" => p(value_f, init_a_one),
        "pppAngle" => p(value_i, init_a_zero),
        "pppColor" => p(value_s, init_a_short),
        "pppMatrixXYZ" => upd(matrix_xyz),
        "pppMatrixXZY" => upd(matrix_xzy),
        "pppMatrixYXZ" => upd(matrix_yxz),
        "pppMatrixYZX" => upd(matrix_yzx),
        "pppMatrixZXY" => upd(matrix_zxy),
        "pppMatrixZYX" => upd(matrix_zyx),
        "pppMatrixLoc" => upd(matrix_loc),
        // (The loop matrices run the same code as the plain ones.)
        "pppMatrixLoop" => upd(matrix_xyz),
        "pppDrawMatrixLoop" => Program { name, second: Some(draw_matrix), ..Program::default() },
        "pppAngleLoop" => Program {
            name,
            update: Some(angle_loop),
            init: Some(loop_init),
            restart: Some(angle_loop_back),
            ..Program::default()
        },
        "pppPointLoop" | "pppScaleLoop" => Program {
            name,
            update: Some(float_loop),
            init: Some(loop_init),
            restart: Some(float_loop_back),
            ..Program::default()
        },
        "pppDrawMdlLoop" => Program {
            name,
            second: Some(draw_mdl_loop),
            init: Some(draw_mdl_loop_init),
            restart: Some(draw_mdl_loop_restart),
            ..Program::default()
        },
        "pppKeZCrctShp" => Program { name, second: Some(ke_z_crct_shp), ..Program::default() },
        "pppDrawShape" => Program {
            name,
            group: Some(draw_shape),
            init: Some(zero_a1),
            restart: Some(zero_a1),
            ..Program::default()
        },
        "pppDrawShapeX" => Program {
            name,
            group: Some(draw_shape_x),
            init: Some(zero_a1),
            restart: Some(zero_a1),
            ..Program::default()
        },
        "pppKeLnsLp" => Program {
            name,
            second: Some(ke_lns_lp),
            init: Some(ke_lns_lp_init),
            restart: Some(ke_lns_lp_restart),
            ..Program::default()
        },
        "pppKeLnsFls" => {
            Program { name, second: Some(ke_lns_fls), init: Some(ke_lns_fls_init), ..Program::default() }
        }
        "pppKeShpTail2" => Program {
            name,
            update: Some(ke_shp_tail2),
            second: Some(ke_shp_tail2_draw),
            init: Some(ke_shp_tail2_init),
            ..Program::default()
        },
        "pppKeShpTail3" => Program {
            name,
            update: Some(ke_shp_tail3),
            second: Some(ke_shp_tail3_draw),
            init: Some(ke_shp_tail3_init),
            restart: Some(ke_shp_tail3_restart),
            ..Program::default()
        },
        "pppDrawShapeField" => Program {
            name,
            second: Some(draw_shape_field),
            init: Some(draw_shape_field_init),
            free: Some(draw_shape_field_free),
            ..Program::default()
        },
        "pppDrawMdlSea" => Program {
            name,
            second: Some(draw_mdl_sea),
            init: Some(draw_mdl_sea_init),
            restart: Some(draw_mdl_sea_restart),
            free: Some(draw_mdl_sea_free),
            ..Program::default()
        },
        // (No entry points.)
        "pppSMatrix" => Program { name, ..Program::default() },
        "pppMatrixScl" => upd(matrix_scl),
        "pppParMatrix" => upd(par_matrix),
        "pppKeParMatR" => upd(ke_par_mat_r),
        "pppKeOfsMatXYZ" => upd(ke_ofs_mat_xyz),
        "pppKeMatSN" => {
            Program { name, update: Some(ke_mat_sn), init: Some(ke_mat_sn_init), ..Program::default() }
        }
        "pppKeMatPht" => upd(ke_mat_pht),
        "pppPointAp" => {
            Program { name, update: Some(point_ap), init: Some(point_ap_init), ..Program::default() }
        }
        "pppVertexApLc" => Program {
            name,
            update: Some(vertex_ap_lc),
            init: Some(vertex_ap_init),
            restart: Some(vertex_ap_init),
            ..Program::default()
        },
        "pppVertexApAt" => Program {
            name,
            update: Some(vertex_ap_at),
            init: Some(vertex_ap_init),
            restart: Some(vertex_ap_init),
            ..Program::default()
        },
        "pppVertexAttend" => upd(vertex_attend),
        "pppDrawMatrix" => Program { name, second: Some(draw_matrix), ..Program::default() },
        "pppDrawMatrixFront" => Program { name, second: Some(draw_matrix_front), ..Program::default() },
        "pppKeDMatFr" => Program { name, second: Some(ke_d_mat_fr), ..Program::default() },
        "pppDrawMdl3" => Program { name, second: Some(draw_mdl3), ..Program::default() },
        "pppDrawMdlSemi3" => Program { name, second: Some(draw_mdl_semi3), ..Program::default() },
        "pppKeZCrct" => Program { name, second: Some(ke_z_crct), ..Program::default() },
        "pppKeMdlDtt" => Program {
            name,
            second: Some(ke_mdl_dtt),
            init: Some(ke_mdl_dtt_init),
            restart: Some(ke_mdl_dtt_init),
            ..Program::default()
        },
        "pppKeMdlTfd3" => Program {
            name,
            second: Some(ke_mdl_tfd3),
            init: Some(ke_mdl_tfd3_init),
            restart: Some(ke_mdl_tfd3_init),
            ..Program::default()
        },
        "pppVtMime" => Program {
            name,
            second: Some(vt_mime),
            init: Some(vt_mime_init),
            restart: Some(vt_mime_restart),
            free: Some(vt_mime_free),
            ..Program::default()
        },
        "pppKeOfsPt" => p(ke_ofs_pt, ke_ofs_pt_init),
        "pppKeLnsLpSft" => p(ke_lns_lp_sft, zero_a2),
        "pppNeiLightEikyo" => p(nei_light_eikyo, zero_a6),
        "pppNeiPointLight" => p(nei_point_light, zero_a3),
        "pppKeAcmCic" | "pppKeAcmCicEx" => upd(ke_acm_cic),
        "pppKeAcmSolid" => Program { name, post: Some(nothing_part), ..Program::default() },
        "pppKeGrvTgt" => {
            Program { name, update: Some(ke_grv_tgt), init: Some(ke_grv_tgt_init), ..Program::default() }
        }
        "pppKeGrvEff" => {
            Program { name, update: Some(ke_grv_eff), init: Some(nothing), ..Program::default() }
        }
        "pppKeHmgEff" => {
            Program { name, update: Some(ke_hmg_eff), init: Some(nothing), ..Program::default() }
        }
        "pppKeMvYpEff" => upd(ke_mv_yp_eff),
        "pppKeDrct" => {
            Program { name, update: Some(ke_drct), init: Some(ke_drct_init), ..Program::default() }
        }
        "pppKeBornRnd5" => {
            Program { name, update: Some(born5), init: Some(born_init_timed), ..Program::default() }
        }
        "pppKeBornRnd6" => {
            Program { name, update: Some(born6), init: Some(born_init_timed), ..Program::default() }
        }
        "pppKeBornRnd2" => Program { name, update: Some(born2), init: Some(born_init), ..Program::default() },
        "pppKeBornRnd3" => Program { name, update: Some(born3), init: Some(born_init), ..Program::default() },
        "pppPointRAp" => {
            Program { name, update: Some(point_r_ap), init: Some(point_ap_init), ..Program::default() }
        }
        "pppVertexAp" => {
            Program { name, update: Some(vertex_ap), init: Some(vertex_ap_init), ..Program::default() }
        }
        _ if random_update(name).is_some() => {
            Program { name, update: random_update(name), ..Program::default() }
        }
        _ => return None,
    })
}
