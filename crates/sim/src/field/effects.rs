//! Event effects (00db / 00dc / 00de): the event system's side of the effect loader, as traced. The loader's
//! state goes 0 idle → 1 loading (00db) → 3 loaded (its tick, the frame after) → 4 running (00dc) → 5
//! signalled (by the effect program) → 6 freed → 0.
//!
//! The program's signal is ported: an event effect's run (after the scripts, each unpaused frame) signals on its
//! first run, as nothing in the field holds the effect system busy (only battle effect commands set that).
//!
//! Not ported yet: the rest of the effect programs (their particle runtime, the end that frees the effect, the
//! draw). Nothing is drawn.

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;

use super::FieldState;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effects {
    /// The loader's state (see the module notes) and the effect loaded.
    pub state: u8,
    pub id: i32,
    /// The event system's effect flags: `hold` (an effect holds the event), `flag_b70`, `request` (the
    /// loader's request: 1 load, 4 freed).
    pub hold: u32,
    pub flag_b70: u32,
    pub request: u32,
    /// The bodies' effects held off (0244) / let on (0245): bit 0x800 of the effect system's flags.
    pub et_flags: u32,
}

/// Whether the calling worker is bound to a body (+0xaa 1, +0x9c set).
fn has_body(g: &FieldState, w: usize) -> bool {
    g.actors.get(w).is_some_and(|a| a.bind_kind == 1) && g.chrs.get(w).is_some_and(Option::is_some)
}

fn place_flags(g: &mut FieldState, w: usize, set: u16, clear: u16) -> u16 {
    let Some(wk) = g.workers.list.get_mut(w) else { return 0 };
    let v = wk.u16(crate::workers::PLACE) & !clear | set;
    wk.set_u16(crate::workers::PLACE, v);
    v
}

/// A part effect of the body on (0 → 0x20, 1 → 0x40, 2 → 0x80 in +0x38).
fn part_on(g: &mut FieldState, w: usize, k: usize) {
    if has_body(g, w) {
        place_flags(g, w, 0x20 << k, 0);
    }
}

/// A part effect off (with none of the three left, the body's effects come off the effect table).
fn part_off(g: &mut FieldState, w: usize, k: usize) {
    if has_body(g, w) {
        place_flags(g, w, 0, 0x20 << k);
    }
}

fn part(g: &mut FieldState, w: usize, k: usize, on: bool) -> Value {
    if on {
        part_on(g, w, k)
    } else {
        part_off(g, w, k)
    }
    Value::Int(1)
}

/// Before the scripts: the loader's tick (an effect loading becomes loaded the frame after).
pub fn frame_start(g: &mut FieldState) {
    if g.effects.state == 1 {
        g.effects.state = 3;
    }
}

/// After the scripts: the running effect's program (its first run signals: state 4 → 5), then the loader's
/// process (a freed effect goes).
pub fn frame_after_scripts(g: &mut FieldState) {
    if g.effects.state == 4 {
        g.effects.state = 5;
    }
    if g.effects.state == 6 {
        g.effects.state = 0;
    }
}

pub fn register(n: &mut Natives<FieldState>) {
    // 00db(id): load event effect `id`; the wait is done once it is loaded (state 3; 1 on scene 0x10e).
    n.register(
        0x00DB,
        Native {
            init: Some(|g, c| {
                c.scratch[0] = c.stack.pop_int();
                g.effects.flag_b70 = 0;
                g.effects.hold = 1;
            }),
            wait: Some(|g, c| {
                g.effects.request = 1;
                if g.effects.state == 0 {
                    g.effects.id = c.scratch[0];
                    g.effects.state = 1;
                }
                match g.effects.state {
                    1 if g.scene == Some(0x10e) => 5,
                    3 => 5,
                    _ => 0,
                }
            }),
            result: None,
        },
    );
    // 00dc: start the loaded effect (state 3 → 4) and wait until it signals (state 5).
    n.register(
        0x00DC,
        Native {
            init: None,
            wait: Some(|g, _| match g.effects.state {
                3 => {
                    g.effects.state = 4;
                    0
                }
                5 => {
                    g.effects.hold = 0;
                    5
                }
                _ => 0,
            }),
            result: None,
        },
    );
    // 00de: wait until the effect is gone (state 0 or 6).
    n.register(
        0x00DE,
        Native {
            init: None,
            wait: Some(|g, _| match g.effects.state {
                0 | 6 => {
                    g.effects.request = 4;
                    5
                }
                _ => 0,
            }),
            result: None,
        },
    );
    // 01ce / 01cf: the body's effects shown (+0x38 |= 0x10) / hidden (&= ~0x10); return 1. 0205 / 0207 /
    // 0209 and 0206 / 020a: part effects on / off.
    n.register(
        0x01CE,
        Native::result(|g, c| {
            let w = c.worker;
            if has_body(g, w) {
                place_flags(g, w, 0x10, 0);
            }
            Value::Int(1)
        }),
    );
    n.register(
        0x01CF,
        Native::result(|g, c| {
            let w = c.worker;
            if has_body(g, w) {
                place_flags(g, w, 0, 0x10);
            }
            Value::Int(1)
        }),
    );
    n.register(0x0205, Native::result(|g, c| part(g, c.worker, 0, true)));
    n.register(0x0207, Native::result(|g, c| part(g, c.worker, 1, true)));
    n.register(0x0209, Native::result(|g, c| part(g, c.worker, 2, true)));
    n.register(0x0206, Native::result(|g, c| part(g, c.worker, 0, false)));
    n.register(0x020A, Native::result(|g, c| part(g, c.worker, 2, false)));
    // 0244 / 0245: every body's effects held off (&= ~0x800) / let on again (|= 0x800);
    // return 1. (The hold-off's other steps belong to the effect runtime, not ported.)
    n.register(
        0x0244,
        Native::result(|g, _| {
            g.effects.et_flags &= !0x800;
            Value::Int(1)
        }),
    );
    n.register(
        0x0245,
        Native::result(|g, _| {
            g.effects.et_flags |= 0x800;
            Value::Int(1)
        }),
    );
    // 8067(a, b, c, _): the blitzball special shot effect (the effect runtime: not ported); returns 1.
    n.register(
        0x8067,
        Native::result(|_, c| {
            for _ in 0..4 {
                c.stack.pop_int();
            }
            Value::Int(1)
        }),
    );
}
