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
        _ if random_update(name).is_some() => {
            Program { name, update: random_update(name), ..Program::default() }
        }
        _ => return None,
    })
}
