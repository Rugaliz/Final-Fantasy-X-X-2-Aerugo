//! The map's particle effects as natives switch them. The PS2 map package names the effect table at +0x38: a u16
//! count at +4, then 0x50-byte records whose i16 id is at +0x64 (from the table) and whose start value is at
//! +0x50. Each record has a run state (0x80 bytes each: +0 the state, +0x1d the mode) and a second one (0x4c bytes:
//! +0 flags, +4, +8, +0xc the stick position, +0x3c a place). 8002(id, on) restarts every effect with that id
//! (state 0, mode 0) or stops it (mode 1); 800d(id, v) clears (v ≠ 0) or sets (v = 0) flag 0x10 of each. Both
//! return 0 for off and the value otherwise. A reset releases the effect's particles and sets state 0xfffff000.

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;

use super::FieldState;

/// Flag 800d sets.
pub const FLAG_10: u32 = 0x10;

/// One effect of the map's table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapEffect {
    pub id: i16,
    /// The record's second id (+0x66), which 8035/8036 switch by.
    pub group: i16,
    /// The run state (+0; from the record, 8002 on sets 0, a reset 0xfffff000) and mode (+0x1d: 0 running, 1
    /// stopped, 2 set by 8003).
    pub state: u32,
    pub mode: u8,
    /// The second record: flags (+0), two values (+4, +8), the place 805b gives (+0x3c).
    pub flags: u32,
    pub value4: u32,
    pub value8: u32,
    pub place: [f32; 3],
}

/// The map's effects in table order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MapEffects(pub Vec<MapEffect>);

/// The map effects' global switch (800c / 0226; the table's setup turns it on).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectsOn(pub bool);

impl Default for EffectsOn {
    fn default() -> Self {
        EffectsOn(true)
    }
}

impl MapEffects {
    /// The effect table of a `mapout.vpa` package (none without one).
    pub fn from_map_package(vpa: &[u8]) -> MapEffects {
        let u32_at = |at: usize| vpa.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let u16_at = |at: usize| vpa.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        if !vpa.starts_with(b"MAP1") {
            return MapEffects::default();
        }
        let Some(table) = u32_at(0x38).filter(|&t| t != 0).map(|t| t as usize) else {
            return MapEffects::default();
        };
        let n = usize::from(u16_at(table + 4).unwrap_or(0));
        MapEffects(
            (0..n)
                .map_while(|i| {
                    let at = table + i * 0x50;
                    Some(MapEffect {
                        id: u16_at(at + 0x64)? as i16,
                        group: u16_at(at + 0x66)? as i16,
                        state: u32_at(at + 0x50)?,
                        mode: 0,
                        flags: 0,
                        value4: 0,
                        value8: 0,
                        place: [0.0; 3],
                    })
                })
                .collect(),
        )
    }

    fn each_group(&mut self, group: i32, f: impl Fn(&mut MapEffect)) {
        self.0.iter_mut().filter(|e| e.group == group as i16).for_each(f);
    }

    fn each(&mut self, id: i32, f: impl Fn(&mut MapEffect)) {
        self.0.iter_mut().filter(|e| e.id == id as i16).for_each(f);
    }

    /// The reset state.
    pub const RESET: u32 = 0xffff_f000;

    /// −1 no effect with the id, 0 reset, 1 running, 2 stopped (the first record with the id).
    pub fn status(&self, id: i32) -> i32 {
        match self.0.iter().find(|e| e.id == id as i16) {
            None => -1,
            Some(e) if e.state == Self::RESET => 0,
            Some(e) => i32::from(e.mode != 0) + 1,
        }
    }

    /// The first record with the id (by the u16 id, as the lookups compare it).
    fn first(&mut self, id: i32) -> Option<&mut MapEffect> {
        self.0.iter_mut().find(|e| e.id as u16 == id as u16)
    }
}

/// The worker record's four map effect bind slots (+0xb18, 0x10 bytes each: u16 effect id (bit 15: a position is
/// set), u16 part, then the position).
const SLOTS: u32 = 0xb18;

/// The worker (kind 1 only) takes the first free slot (id | effect, part), then the effect's second record
/// is bound by the worker's bind kind: 1 with a live body → flags |= 1, +4 the body (its worker), +8 the part
///; 4 → flags |= 2, +4 the part, +8 = 0; 5 → flags |= 8.
fn bind_effect(g: &mut FieldState, worker: usize, id: i32, part: i32) {
    let w = &mut g.workers.list[worker];
    if w.kind != 1 {
        return;
    }
    let Some(k) = (0..4).find(|&k| w.u16(SLOTS + k * 0x10) & 0x7fff == 0) else { return };
    let a = SLOTS + k * 0x10;
    w.set_u16(a, w.u16(a) | id as u16);
    w.set_i16(a + 2, part as i16);
    let bind = w.bind();
    let live = g.chrs.get(worker).is_some_and(Option::is_some);
    let Some(e) = g.map_effects.first(id) else { return };
    match bind {
        1 if live => {
            e.flags |= 1;
            e.value4 = worker as u32;
            e.value8 = part as u32;
        }
        4 => {
            e.flags |= 2;
            e.value4 = part as u32;
            e.value8 = 0;
        }
        5 => {
            e.flags |= 8;
            e.value4 = part as u32;
            e.value8 = 0;
        }
        _ => {}
    }
}

pub fn register(n: &mut Natives<FieldState>) {
    // 8003(id): state 0 and mode 2 for each effect with the id; returns 1.
    n.register(
        0x8003,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            g.map_effects.each(id, |e| {
                e.state = 0;
                e.mode = 2;
            });
            Value::Int(1)
        }),
    );
    // 8005(id, any) / 8004(id): wait while the effect runs (status 1), or is stopped (2) with `any` ≠ 0 (8004
    // passes 1).
    fn effect_wait(g: &mut FieldState, c: &mut aerugo_script::vm::NativeCall<'_>) -> u32 {
        let (id, any) = (c.scratch[0], c.scratch[1]);
        let r = g.map_effects.status(i32::from(id as i16));
        if r == 1 || (r == 2 && any != 0) { 0 } else { 5 }
    }
    n.register(
        0x8005,
        Native {
            init: Some(|_, c| {
                c.scratch[1] = c.stack.pop_int();
                c.scratch[0] = c.stack.pop_int();
            }),
            wait: Some(effect_wait),
            result: None,
        },
    );
    n.register(
        0x8004,
        Native {
            init: Some(|_, c| {
                c.scratch[1] = 1;
                c.scratch[0] = c.stack.pop_int();
            }),
            wait: Some(effect_wait),
            result: None,
        },
    );
    // 8007: every effect whose id is not 0 restarts (state 0, mode 0); 8008: every one whose id is
    // not 0 is reset (its particles released, state 0xfffff000). Both return 1.
    n.register(
        0x8007,
        Native::result(|g, _| {
            g.map_effects.0.iter_mut().filter(|e| e.id != 0).for_each(|e| {
                e.state = 0;
                e.mode = 0;
            });
            Value::Int(1)
        }),
    );
    n.register(
        0x8008,
        Native::result(|g, _| {
            g.map_effects.0.iter_mut().filter(|e| e.id != 0).for_each(|e| e.state = MapEffects::RESET);
            Value::Int(1)
        }),
    );
    // 805f(id) / 8060(group): reset the effects with that id / second id; return 1.
    n.register(
        0x805F,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            g.map_effects.each(id, |e| e.state = MapEffects::RESET);
            Value::Int(1)
        }),
    );
    n.register(
        0x8060,
        Native::result(|g, c| {
            let grp = c.stack.pop_int();
            g.map_effects.each_group(grp, |e| e.state = MapEffects::RESET);
            Value::Int(1)
        }),
    );
    // 8059(id, v): the first effect with the id: second flags |= 2, +4 = v, +8 = 0; returns 1.
    n.register(
        0x8059,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as u32;
            let id = c.stack.pop_int();
            if let Some(e) = g.map_effects.first(id) {
                e.flags |= 2;
                e.value4 = v;
                e.value8 = 0;
            }
            Value::Int(1)
        }),
    );
    // 805b(id, x, y, z): the first effect with the id: its stick position set and its
    // place (+0x3c) = (x, y, z) as popped (z, y, x from the top); returns 1.
    n.register(
        0x805B,
        Native::result(|g, c| {
            let a = c.stack.pop_float();
            let b = c.stack.pop_float();
            let d = c.stack.pop_float();
            let id = c.stack.pop_int();
            if let Some(e) = g.map_effects.first(id) {
                e.place = [a, b, d];
            }
            Value::Int(1)
        }),
    );
    // 8066(v): the map effects' global value = v; returns 1.
    n.register(
        0x8066,
        Native::result(|g, c| {
            g.map_effect_230fd34 = c.stack.pop_int();
            Value::Int(1)
        }),
    );
    // 8009(effect, part): bind a map effect to the worker; returns 1.
    n.register(
        0x8009,
        Native::result(|g, c| {
            let part = c.stack.pop_int();
            let id = c.stack.pop_int();
            bind_effect(g, c.worker, id, part);
            Value::Int(1)
        }),
    );
    // 0196(effect): the same for objects bound as kind 4 or 5, the part being the bind id; returns 1.
    n.register(
        0x0196,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            let w = &g.workers.list[c.worker];
            if matches!(w.bind(), 4 | 5) {
                let part = i32::from(w.u16(crate::workers::BIND_ID) as i16);
                bind_effect(g, c.worker, id, part);
            }
            Value::Int(1)
        }),
    );
    // 800a(effect, x, y, z): the place a bound map effect sits at (the slot's position, bit 15 of its id).
    n.register(
        0x800A,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            let id = f64::from(c.stack.pop_float()).trunc() as i64 as u32;
            let w = &mut g.workers.list[c.worker];
            if w.kind == 1 {
                for k in 0..4 {
                    let a = SLOTS + k * 0x10;
                    if u32::from(w.u16(a)) & 0xffff_7fff == id {
                        w.set_f32(a + 4, x);
                        w.set_f32(a + 8, y);
                        w.set_u16(a, w.u16(a) | 0x8000);
                        w.set_f32(a + 0xc, z);
                    }
                }
            }
            Value::Int(1)
        }),
    );
    // 800b(effect): let a map effect go (0: every slot, though the effect side then clears effect 0).
    n.register(
        0x800B,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as u32;
            let w = &mut g.workers.list[c.worker];
            if w.kind == 1 {
                let mut freed = Vec::new();
                for k in 0..4 {
                    let a = SLOTS + k * 0x10;
                    let id = u32::from(w.u16(a));
                    if (v == 0 && id & 0x7fff != 0) || (v != 0 && id & 0x7fff == v) {
                        w.set_u16(a, 0);
                        freed.push(v as i32);
                    }
                }
                for id in freed {
                    if let Some(e) = g.map_effects.first(id) {
                        e.flags &= !0xf;
                        e.value8 = 0;
                        e.value4 = 0;
                    }
                }
            }
            Value::Int(1)
        }),
    );
    // 8035(group, on) / 8036(group, v): 8002 and 800d by the second id.
    n.register(
        0x8035,
        Native::result(|g, c| {
            let on = c.stack.pop_int();
            let grp = c.stack.pop_int();
            if on == 0 {
                g.map_effects.each_group(grp, |e| e.mode = 1);
                return Value::Int(0);
            }
            g.map_effects.each_group(grp, |e| {
                e.state = 0;
                e.mode = 0;
            });
            Value::Int(on)
        }),
    );
    n.register(
        0x8036,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let grp = c.stack.pop_int();
            if v == 0 {
                g.map_effects.each_group(grp, |e| e.flags |= FLAG_10);
                return Value::Int(0);
            }
            g.map_effects.each_group(grp, |e| e.flags &= !FLAG_10);
            Value::Int(v)
        }),
    );
    // 800c(on) / 0226(on) / c036(on): the map effects on or off; return the switch.
    for id in [0x800Cu16, 0x0226, 0xC036] {
        n.register(
            id,
            Native::result(|g, c| {
                let on = c.stack.pop_int() != 0;
                g.map_effects_on = EffectsOn(on);
                Value::Int(i32::from(on))
            }),
        );
    }
    // 805c(on): a map display switch (the remaster's map draw is refreshed); returns 1.
    n.register(
        0x805C,
        Native::result(|g, c| {
            g.map_switch_5eb8 = u8::from(c.stack.pop_int() != 0);
            Value::Int(1)
        }),
    );
    n.register(
        0x8002,
        Native::result(|g, c| {
            let on = c.stack.pop_int();
            let id = c.stack.pop_int();
            if on == 0 {
                g.map_effects.each(id, |e| e.mode = 1);
                return Value::Int(0);
            }
            g.map_effects.each(id, |e| {
                e.state = 0;
                e.mode = 0;
            });
            Value::Int(on)
        }),
    );
    n.register(
        0x800D,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let id = c.stack.pop_int();
            if v == 0 {
                g.map_effects.each(id, |e| e.flags |= FLAG_10);
                return Value::Int(0);
            }
            g.map_effects.each(id, |e| e.flags &= !FLAG_10);
            Value::Int(v)
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effects_by_id() {
        let mut d = vec![0u8; 0x200];
        d[..4].copy_from_slice(b"MAP1");
        d[0x38..0x3c].copy_from_slice(&0x40u32.to_le_bytes());
        d[0x44..0x46].copy_from_slice(&2u16.to_le_bytes());
        d[0x40 + 0x64..0x40 + 0x66].copy_from_slice(&7i16.to_le_bytes());
        d[0x40 + 0x50..0x40 + 0x54].copy_from_slice(&9u32.to_le_bytes());
        let mut m = MapEffects::from_map_package(&d);
        assert_eq!(m.0.len(), 2);
        assert_eq!((m.0[0].id, m.0[0].state, m.0[1].id), (7, 9, 0));
        m.each(7, |e| e.mode = 1);
        assert!(m.0[0].mode == 1 && m.0[1].mode == 0);
    }
}
