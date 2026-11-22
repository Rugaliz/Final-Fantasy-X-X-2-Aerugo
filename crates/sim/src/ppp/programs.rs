//! The particle programs, by the names the effects' program tables give them. Each works on the object's
//! program work (object + 0xa0 + the offsets of the reference's work pair) and the current key of its track.
//!
//! Ported: pppAccele, pppMove (the same code: work B += the key's four floats when the key is the object's
//! age, then work A += work B), pppAngAccele, pppAngMove (the same on integers). Their inits set work B: the
//! float vector (0, 0, 0, 1) for the float ones, the zero vector for the integer ones.

use super::{Program, Run};

/// The reference's two work offsets.
fn works(r: &mut Run, rf: u32) -> (u32, u32) {
    let pair = r.m.u32(rf + 0xc);
    (r.m.u32(pair), r.m.u32(pair + 4))
}

/// pppAccele / pppMove: float rates.
fn accele(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (a, b) = works(r, rf);
    if r.paused {
        return;
    }
    let (a, b) = (o.wrapping_add(a).wrapping_add(0xa0), o.wrapping_add(b).wrapping_add(0xa0));
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        for i in 0..4 {
            let v = r.m.f32(key + 0x10 + 4 * i) + r.m.f32(b + 4 * i);
            r.m.set_f32(b + 4 * i, v);
        }
    }
    for i in 0..4 {
        let v = r.m.f32(b + 4 * i) + r.m.f32(a + 4 * i);
        r.m.set_f32(a + 4 * i, v);
    }
}

/// pppAngAccele / pppAngMove: integer rates.
fn ang_accele(r: &mut Run, o: u32, key: u32, rf: u32) {
    let (a, b) = works(r, rf);
    if r.paused {
        return;
    }
    let (a, b) = (o.wrapping_add(a).wrapping_add(0xa0), o.wrapping_add(b).wrapping_add(0xa0));
    if r.m.u32(key) == r.m.u32(o + 0xc) {
        for i in 0..4 {
            let v = r.m.u32(b + 4 * i).wrapping_add(r.m.u32(key + 0x10 + 4 * i));
            r.m.set_u32(b + 4 * i, v);
        }
    }
    for i in 0..4 {
        let v = r.m.u32(a + 4 * i).wrapping_add(r.m.u32(b + 4 * i));
        r.m.set_u32(a + 4 * i, v);
    }
}

/// Work B of the reference set to `v` (the inits).
fn set_b(r: &mut Run, o: u32, rf: u32, v: [u32; 4]) {
    let (_, b) = works(r, rf);
    let at = o.wrapping_add(b).wrapping_add(0xa0);
    for (i, x) in v.into_iter().enumerate() {
        r.m.set_u32(at + 4 * i as u32, x);
    }
}

fn init_float(r: &mut Run, o: u32, rf: u32) {
    set_b(r, o, rf, [0, 0, 0, 1.0f32.to_bits()]);
}

fn init_int(r: &mut Run, o: u32, rf: u32) {
    set_b(r, o, rf, [0; 4]);
}

/// The program called `name`, when it is ported.
pub fn by_name(name: &'static str) -> Option<Program> {
    let p = |update: fn(&mut Run, u32, u32, u32), init: fn(&mut Run, u32, u32)| Program {
        name,
        update: Some(update),
        init: Some(init),
        restart: Some(init),
        ..Program::default()
    };
    match name {
        "pppAccele" | "pppMove" => Some(p(accele, init_float)),
        "pppAngAccele" | "pppAngMove" => Some(p(ang_accele, init_int)),
        _ => None,
    }
}
