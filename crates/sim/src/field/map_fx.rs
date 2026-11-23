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
    /// The record's part (index into the table's parts), its culling distances (+0x38 far, +0x3c and +0x40 the
    /// near and side margins), its draw-matrix kind (+0x4a) and angle order (+0x4b).
    pub part_index: u32,
    pub cull: [f32; 3],
    pub draw_kind: u8,
    pub order: u8,
    /// The running part (its placement from the record: +0x40 position, +0x50 angles, +0x60 scale).
    pub part: crate::ppp::Part,
    /// A reset freed the part's particles (done at the next frame's run, before anything allocates).
    pub free_pending: bool,
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

impl MapEffect {
    /// A reset: the part's particles released and the state 0xfffff000.
    pub fn reset(&mut self) {
        self.state = MapEffects::RESET;
        self.free_pending = true;
    }
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
                    let r = at + 0x20;
                    let mut place = [0.0f32; 12];
                    for (k, v) in place.iter_mut().enumerate() {
                        *v = f32::from_bits(u32_at(r + 4 * k)?);
                    }
                    let f = |o: usize| u32_at(r + o).map(f32::from_bits);
                    Some(MapEffect {
                        part_index: u32_at(r + 0x34)?,
                        cull: [f(0x38)?, f(0x3c)?, f(0x40)?],
                        draw_kind: *vpa.get(r + 0x4a)?,
                        order: *vpa.get(r + 0x4b)?,
                        part: crate::ppp::Part { place, ..crate::ppp::Part::default() },
                        free_pending: false,
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
            g.map_effects.0.iter_mut().filter(|e| e.id != 0).for_each(MapEffect::reset);
            Value::Int(1)
        }),
    );
    // 805f(id) / 8060(group): reset the effects with that id / second id; return 1.
    n.register(
        0x805F,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            g.map_effects.each(id, MapEffect::reset);
            Value::Int(1)
        }),
    );
    n.register(
        0x8060,
        Native::result(|g, c| {
            let grp = c.stack.pop_int();
            g.map_effects.each_group(grp, MapEffect::reset);
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

/// The draw phase's map effects: run while they are on and the map is shown (display bit 0).
pub fn frame_draw(g: &mut FieldState) {
    if !g.map_effects_on.0 || g.map_display & 1 == 0 {
        return;
    }
    let Some(mut rt) = g.map_runtime.take() else { return };
    let mut h = MapHost {
        view: g.cam_view.unwrap_or(IDENTITY),
        screen: g.cam_screen.unwrap_or(IDENTITY),
        paused: g.map_effect_230fd34 != 0,
        vu: &mut g.effects.vu,
        crt: &mut g.crt_rand,
        event: u32::from(g.scene.unwrap_or(0)),
        magic: g.effects.effect.as_ref().map_or(u32::MAX, |e| e.magic),
        colour: g.globals.colour_mul,
        map_id: g.map_id,
    };
    rt.frame(&mut g.map_effects, &mut h);
    let magic = h.magic;
    let scene = super::effects::vfx_scene(g, magic, Some(0));
    for c in &rt.render {
        if let RenderCall::Model(d) = c {
            g.vfx.push((crate::ppp::vfx::model(&rt.space, d, &scene), String::new()));
        }
    }
    g.map_runtime = Some(rt);
}

use crate::chr::elements::{IDENTITY, Mat, apply4, mul, normalize};
use crate::ppp::{
    Env, Pool, Program, Region, RenderCall, Run, SCRATCH_VECTOR, Scene, Space, VuRand, relocate,
};

/// Where the map effects' data lies in their address space (Aerugo's own places): the table (the package from
/// its +0x38 offset on), the work buffer and the second pool, and the key of the engine's program table.
const TABLE_BASE: u32 = 0x4000_0000;
const WORK: u32 = 0x5000_0000;
const WORK_SIZE: u32 = 0x4_0000;
const POOL2: u32 = 0x5800_0000;
const ENGINE_TABLE: u32 = 0x6000_0000;

/// The map effects' particle runtime: the table bound to the engine's own programs, the environment over its
/// work buffer (and a second pool when the table's byte +1 asks for one: that byte · 0x2000 bytes).
#[derive(Debug, Clone)]
pub struct MapRuntime {
    pub space: Space,
    pub env: Env,
    pub table: Vec<Program>,
    pub missing: Vec<&'static str>,
    /// Some record draws through the wood matrix (draw kind 2).
    pub wood: bool,
    /// The engine matrices of the last effect run (the lens and thunder programs read the screen and the
    /// camera position).
    pub render: Vec<RenderCall>,
}

impl PartialEq for MapRuntime {
    fn eq(&self, o: &MapRuntime) -> bool {
        let names = |r: &MapRuntime| r.table.iter().map(|p| p.name).collect::<Vec<_>>();
        self.space == o.space && self.env == o.env && names(self) == names(o) && self.render == o.render
    }
}

/// What the map effects read from the engine each frame.
#[derive(Debug)]
pub struct MapHost<'a> {
    /// The camera's view and screen matrices.
    pub view: Mat,
    pub screen: Mat,
    /// The particle runtime's pause.
    pub paused: bool,
    pub vu: &'a mut VuRand,
    pub crt: &'a mut super::face::CrtRand,
    /// The current event and magic number.
    pub event: u32,
    pub magic: u32,
    /// The scene's colour transform multipliers (the parts' colour: each at most 1, · 4096).
    pub colour: [f32; 4],
    /// The remaster's map id.
    pub map_id: u32,
}

/// The screen-quadrant code of a projected point (x beyond ±0.8888889: 1 / 2; y beyond ±0.5: 4 / 8).
fn quadrant(x: f32, y: f32) -> u32 {
    let mut c = if x < -0.888_888_9 {
        1
    } else if x >= 0.888_888_9 {
        2
    } else {
        0
    };
    if y < -0.5 {
        c |= 4;
    } else if y >= 0.5 {
        c |= 8;
    }
    c
}

/// The culling test of an effect's place (0: visible): none with a negative far distance; past the far
/// distance or behind the near margin, culled (0xf); else the four corners of its margins projected, culled
/// when all fall beyond the same side.
fn culled(world: &Mat, pos: [f32; 4], cull: [f32; 3]) -> u32 {
    if cull[0] < 0.0 {
        return 0;
    }
    let v = apply4(world, pos);
    if v[2].partial_cmp(&cull[0]).is_none_or(|o| o.is_gt()) {
        return 0xf;
    }
    let z = cull[1] + v[2];
    if 0.0f32.partial_cmp(&z).is_none_or(|o| o.is_ge()) {
        return 0xf;
    }
    let f = 1.0 / z;
    let y_lo = v[1] * f - f * cull[2];
    let x_lo = f * v[0] - f * cull[1];
    let x_hi = f * cull[1] + f * v[0];
    let y_hi = f * cull[2] + v[1] * f;
    quadrant(x_hi, y_hi) & quadrant(x_lo, y_hi) & quadrant(x_hi, y_lo) & quadrant(x_lo, y_lo)
}

/// The record's frame matrix: its angles turned in its order, rows scaled by its scale, at its position.
fn record_frame(e: &MapEffect) -> Mat {
    let p = &e.part.place;
    let ang = [p[4].to_bits(), p[5].to_bits(), p[6].to_bits()];
    let mut m = crate::ppp::programs::set_matrix(e.order, ang);
    for row in 0..3 {
        let s = p[8 + row];
        for c in 0..3 {
            m[row * 4 + c] *= s;
        }
    }
    m[12] = p[0];
    m[13] = p[1];
    m[14] = p[2];
    m
}

/// The wood matrix: the rows of the world's up row, the turn of it about the vertical, their cross, and the
/// world's position row, each normalised (the position row kept).
fn wood_matrix(world: &Mat) -> Mat {
    let up = normalize([world[4], world[5], world[6], 0.0]);
    let side = normalize([up[1], -up[0], 0.0, 0.0]);
    let c = |a: [f32; 4], b: [f32; 4]| {
        [a[1] * b[2] - b[1] * a[2], b[0] * a[2] - b[2] * a[0], b[1] * a[0] - a[1] * b[0], 0.0]
    };
    let third = normalize(c(side, up));
    let mut m = [0.0; 16];
    m[0..4].copy_from_slice(&side);
    m[4..8].copy_from_slice(&up);
    m[8..12].copy_from_slice(&third);
    m[12..16].copy_from_slice(&world[12..16]);
    m
}

impl MapRuntime {
    /// The set-up from a `mapout.vpa` package (the table at its +0x38 offset), none without one.
    pub fn new(vpa: &[u8]) -> Option<MapRuntime> {
        let u32_at = |at: usize| vpa.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if !vpa.starts_with(b"MAP1") {
            return None;
        }
        let t = u32_at(0x38).filter(|&t| t != 0)? as usize;
        let bytes = vpa.get(t..)?.to_vec();
        let pool2 = u32::from(*bytes.get(1)?) << 13;
        let mut space = Space::default();
        space.regions.push(Region { base: TABLE_BASE, bytes });
        space.regions.push(Region { base: SCRATCH_VECTOR, bytes: vec![0; 16] });
        space.regions.push(Region { base: WORK, bytes: vec![0; WORK_SIZE as usize] });
        if pool2 != 0 {
            space.regions.push(Region { base: POOL2, bytes: vec![0; pool2 as usize] });
        }
        relocate::bind(&mut space, TABLE_BASE, ENGINE_TABLE);
        let h = TABLE_BASE;
        let lists = [space.u32(h + 0x14), space.u32(h + 0x18), space.u32(h + 0x1c)];
        let mut env = Env::new(&mut space, h, lists, WORK, WORK_SIZE);
        if pool2 != 0 {
            env.pool2 = Some(Pool::init(&mut space, POOL2, pool2));
        }
        let mut missing = Vec::new();
        let table = crate::traced_tables::ENGINE_PROGRAMS
            .iter()
            .map(|&n| {
                crate::ppp::programs::by_name(n).unwrap_or_else(|| {
                    missing.push(n);
                    Program { name: n, ..Program::default() }
                })
            })
            .collect();
        let n = u32::from(space.u16(h + 4));
        let wood = (0..n).any(|i| space.u8(h + 0x20 + i * 0x50 + 0x4a) == 2);
        Some(MapRuntime { space, env, table, missing, wood, render: Vec::new() })
    }

    /// One frame of the map effects (the draw phase, while they are on): for each record not hidden (800d),
    /// not reset and not culled (records with id 0 only; three events keep some): its start countdown (the
    /// part starts when it runs out), its placement and colour, the part's step (a finished part is freed and
    /// the record reset).
    pub fn frame(&mut self, fx: &mut MapEffects, h: &mut MapHost) {
        self.render.clear();
        let world = h.view;
        let wood = if self.wood { wood_matrix(&world) } else { IDENTITY };
        for i in 0..fx.0.len() {
            if fx.0[i].free_pending {
                fx.0[i].free_pending = false;
                let mut part = fx.0[i].part;
                self.with_run(h, Scene::default(), |r| part.free(r));
                fx.0[i].part = part;
            }
        }
        let header = TABLE_BASE;
        for i in 0..fx.0.len() {
            let e = fx.0[i];
            if e.flags & FLAG_10 != 0 {
                self.hide(&e);
                continue;
            }
            if e.state == MapEffects::RESET {
                continue;
            }
            let pos = [e.part.place[0], e.part.place[1], e.part.place[2], e.part.place[3]];
            if e.id == 0 && culled(&world, pos, e.cull) != 0 {
                let keep = match h.event {
                    0x136 => matches!(i, 2 | 6 | 10),
                    0x176 => i == 0xd,
                    _ => false,
                };
                if !keep {
                    continue;
                }
            }
            let scene = Scene {
                event: h.event,
                own_magic: false,
                map_part: Some(i as u32),
                map_part_id: u32::from(e.id as u16),
            };
            let mut e = e;
            if (e.state as i32) >= 0 {
                e.state = e.state.wrapping_sub(1);
                if (e.state as i32) >= 0 {
                    fx.0[i] = e;
                    continue;
                }
                let mode = e.mode;
                let place = e.part.place;
                let mut part = e.part;
                let record = {
                    let parts = self.space.u32(header + 0x10);
                    self.space.u32(parts + 4 * e.part_index)
                };
                self.with_run(h, scene, |r| {
                    part.free(r);
                    part = crate::ppp::Part::start(r, 0x1000, record);
                });
                part.place = place;
                if part.status == MapEffects::RESET {
                    e.state = MapEffects::RESET;
                }
                e.part = part;
                e.mode = if mode == 2 { 1 } else { 0 };
            }
            // The placement: the frame matrix (or the stick matrix, flag 0x100) and the draw matrices.
            let frame = if e.flags & 0xf == 0 && e.flags & 0x100 != 0 {
                let mut m = IDENTITY;
                m[12..15].copy_from_slice(&e.place);
                m
            } else {
                record_frame(&e)
            };
            let mut camera_pos = [0.0; 4];
            let camera = match e.draw_kind {
                1 => {
                    camera_pos = apply4(&world, [frame[12], frame[13], frame[14], frame[15]]);
                    frame
                }
                2 => mul(&wood, &frame),
                _ => mul(&world, &frame),
            };
            for k in 0..4 {
                let c = (h.colour[k].min(1.0) * 4096.0).trunc();
                e.part.colour[k] = c as i32 as u16;
            }
            e.part.drained = e.mode != 0;
            let mut part = e.part;
            let done = self.with_run_m(h, scene, (camera, camera, frame, world, camera_pos), |r| {
                part.update_map(r, 0x30)
            });
            if done {
                self.with_run(h, scene, |r| part.free(r));
                e.state = MapEffects::RESET;
            }
            e.part = part;
            fx.0[i] = e;
        }
    }

    /// The hidden record's objects: every render entry of every reference of their definitions made invisible.
    fn hide(&mut self, e: &MapEffect) {
        let p = &e.part;
        for k in 0..p.subs.0 {
            if p.slots == 0 {
                break;
            }
            let s = p.slots + k * 0x10;
            let o = self.space.u32(s + 4);
            if o == 0 {
                continue;
            }
            let def = self.space.u32(self.space.u32(o + 8));
            let refs = self.space.u16(def + 0x26) as i16;
            for j in 0..refs.max(0) as u32 {
                if self.space.u32(def + 0x28 + j * 0x10) != 0 {
                    self.render.push(RenderCall::Hide { object: o, ref_index: j });
                }
            }
        }
    }

    fn with_run<T>(&mut self, h: &mut MapHost, scene: Scene, f: impl FnOnce(&mut Run) -> T) -> T {
        self.with_run_m(h, scene, (IDENTITY, IDENTITY, IDENTITY, h.view, [0.0; 4]), f)
    }

    fn with_run_m<T>(
        &mut self,
        h: &mut MapHost,
        scene: Scene,
        (view, camera, frame, world, camera_pos): (Mat, Mat, Mat, Mat, [f32; 4]),
        f: impl FnOnce(&mut Run) -> T,
    ) -> T {
        let mut r = Run {
            m: &mut self.space,
            env: &mut self.env,
            table_va: ENGINE_TABLE,
            table: &self.table,
            paused: h.paused,
            vu: h.vu,
            crt: h.crt,
            objects: 0,
            slots: 0,
            part_scale: [1.0; 4],
            part_colour: [0x1000; 4],
            part_word: 0,
            view,
            camera,
            frame,
            world,
            screen: h.screen,
            camera_pos,
            ref_index: 0,
            part_draining: false,
            map_id: h.map_id,
            vertex_ap_slow: false,
            vertex_lc_extend: false,
            magic: h.magic,
            scene,
            render: std::mem::take(&mut self.render),
        };
        let out = f(&mut r);
        self.render = std::mem::take(&mut r.render);
        out
    }
}
