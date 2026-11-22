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

/// pppMatrixLoc: the unit matrix with the translation A.
fn matrix_loc(r: &mut Run, o: u32, _key: u32, rf: u32) {
    let (wa, _) = works(r, rf);
    let wa = at(o, wa);
    let mut m = crate::chr::elements::IDENTITY;
    for i in 0..3 {
        m[12 + i] = r.m.f32(wa + 4 * i as u32);
    }
    put_mat(r, o + 0x10, &m);
}

/// pppMatrixScl: the unit matrix with the scale B on its diagonal and the translation A.
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

/// pppParMatrix: with a target in the key, the object's translation through its parent's matrix (object
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

/// pppKeParMatR: the parent's (object +4) turn without its scale, then the object's translation through the
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

/// pppKeOfsMatXYZ: the XYZ turn of B scaled by C, about the offset A +0x10 + A +0x20: the translation is A plus
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

/// pppKeMatSN: the XYZ turn of the change of the angles B since the last frame (kept at A +0x80) into the
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

/// pppKeMatPht: the XYZ-ordered (X · Y · Z) turn of B scaled by C with the translation A, kept in work D; the
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

/// pppPointAp: every key +0xc frames (countdown byte at B +1), a child in slot key +4 at A's position (into the
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

/// 757770: a child in slot key +0xc at a mesh vertex (three i16) through the spawner's matrix, into the child's
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

/// pppVertexAp: every key +7 frames (countdown u16 at A +2), key +6 children at vertices of mesh key +4 (the
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

/// pppVertexApLc: pppVertexAp with the children at the vertex's own coordinates (757ed0).
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

/// pppVertexApAt: pppVertexAp's timing; each child (slot key +0xc) gets a vertex number in its u16 work at
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

/// pppVertexAttend: B = the vertex (number in A's u16) of mesh key +0xc through the parent's matrix.
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

/// pppPointRAp: every key +0x1c frames (countdown byte at B +1), a child (slot key +0xc) on a sphere of radius
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

/// The born children of 758840 / 758fc0 (slot key +0x10): at the vertex through the spawner's matrix (work
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

/// 7589f0 (slot key +0xc): at the vertex through the matrix (work key +0x10), the unit direction away from the
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

/// The engine's two-argument arc tangent (7590e0): of (a, b), None at the origin.
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

/// 7592a0 (slot key +0xc): at the vertex's own coordinates (work key +0x10), facing it (work key +0x18: the
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

/// pppKeBornRnd5 / pppKeBornRnd6: every key interval frames (countdown byte at A +2) a walk; the countdown
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

/// pppKeBornRnd2 / pppKeBornRnd3: a walk on frames where the integer draw's second byte has none of the key's
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

/// pppKeGrvTgt: on the key's time, B = (radius key +4, strength key +8, the address of A): a gravity target.
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

/// pppKeGrvEff: on the key's time C = the gravity target the key names (slot key +4, work key +8); from then on
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

/// pppKeHmgEff: on the key's time C = (the turn rate key +4, the target the key names: slot +8, work +0xc);
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

/// pppKeMvYpEff: A's angles from the velocity B (when it is not zero): about Z from (−vy, vx), about X from the
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

/// pppKeDrct: on the key's time A = the key's three integers (+0x10).
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

/// pppKeLnsLpSft: A (two floats) += the key's (+4, +8) on its time (not used on the first frame); then B +0x9c
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

/// pppNeiLightEikyo: A (six floats) += the key's (+4) on its time; then two ramps: A1 += A0, A2 += A1;
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

/// pppNeiPointLight: on the key's time A +0xc / +0x10 = the addresses of B and C, A (three floats) += the
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

/// pppKeAcmCic / pppKeAcmCicEx: unless the key's byte +4 is 2, the scene's accumulation set to (0, 1).
fn ke_acm_cic(r: &mut Run, _o: u32, key: u32, _rf: u32) {
    if r.m.u8(key + 4) != 2 {
        r.render.push(super::RenderCall::Accumulation(0, 1));
    }
}

fn nothing_part(_: &mut Run, _: &mut super::Part) {}

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
