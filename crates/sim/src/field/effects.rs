//! Event effects (00db / 00dc / 00de): the event system's side of the effect loader, as traced. The loader's
//! state goes 0 idle → 1 loading (00db) → 3 loaded (its tick, the frame after) → 4 running (00dc) → 5
//! signalled (by the effect program) → 6 freed → 0.
//!
//! The effect's driver runs as the original's: loaded when 00dc starts it (from the magic file's data), its run
//! after the scripts (the ready signal: state 4 → 5), its draw in the draw phase (its frame callback: the
//! particles and their render requests; the end it reports: state 6). An effect whose driver is not ported, or
//! whose file is missing, signals on its first run and never ends.

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
    /// The running effect's driver and the engine's vector-unit random generator.
    pub effect: Option<Box<crate::ppp::driver::Effect>>,
    pub vu: crate::ppp::VuRand,
}

/// The effect `id` from the magic files, when its driver is ported.
fn load_effect(g: &FieldState, id: i32) -> Option<Box<crate::ppp::driver::Effect>> {
    let magic = u32::try_from(id).ok()?;
    crate::ppp::driver::driver(magic)?;
    let file = g.vfs.as_ref()?.read(&format!("FFX/magic_{magic:04}.dll")).ok()?;
    let dll = aerugo_formats::magic::DllData::read(&file)?;
    crate::ppp::driver::Effect::load(magic, &dll).map(Box::new)
}

/// Run `f` on the running effect's driver with the engine around it.
fn with_driver<T>(
    g: &mut FieldState,
    f: impl FnOnce(&mut crate::ppp::driver::Effect, &mut crate::ppp::driver::Host) -> T,
) -> Option<T> {
    let mut e = g.effects.effect.take()?;
    let mut paused = g.map_effect_230fd34 != 0;
    let mut vu = g.effects.vu;
    let mut h = crate::ppp::driver::Host {
        ticking: true,
        busy: false,
        event_bits: g.engine.event_bits,
        view: g.cam_view.unwrap_or(crate::chr::elements::IDENTITY),
        screen: g.cam_screen.unwrap_or(crate::chr::elements::IDENTITY),
        paused: &mut paused,
        vu: &mut vu,
        crt: &mut g.crt_rand,
        event: u32::from(g.scene.unwrap_or(0)),
        map_id: g.map_id,
    };
    let out = f(&mut e, &mut h);
    if paused {
        g.map_effect_230fd34 = 1;
    }
    g.effects.vu = vu;
    g.effects.effect = Some(e);
    Some(out)
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

/// After the scripts: the running effect's run (its ready signal: state 4 → 5; without a driver, on the first
/// run), then the loader's process (a freed effect goes).
pub fn frame_after_scripts(g: &mut FieldState) {
    if matches!(g.effects.state, 4 | 5) {
        let ready = match with_driver(g, |e, h| e.run(h)) {
            Some(s) => s.ready,
            None => true,
        };
        if ready && g.effects.state == 4 {
            g.effects.state = 5;
        }
    }
    if g.effects.state == 6 {
        if g.effects.effect.is_some() {
            effect_free(g);
        }
        g.effects.state = 0;
        g.effects.effect = None;
    }
}

/// The effect system's reset when an event effect is freed, as far as the field keeps its state: the scene's
/// colour transform goes off (display flags bit 2) with its multipliers back at 1. (The effect-script and
/// sound state it also clears is not kept here.)
fn effect_free(g: &mut FieldState) {
    g.map_display &= !4;
    g.globals.colour_mul = [1.0; 4];
}

/// The draw phase: the running effect's draw (its frame callback runs the particles; the end it reports frees
/// the effect: state 6).
pub fn frame_draw(g: &mut FieldState) {
    if !matches!(g.effects.state, 4 | 5) {
        return;
    }
    if with_driver(g, |e, h| e.draw(h)).is_some_and(|s| s.done) {
        g.effects.state = 6;
    }
    if let Some(e) = &g.effects.effect {
        let scene = vfx_scene(g, e.magic, None);
        let dir = format!("ffx_data/gamedata/ps3data/magic/magic_{:04}/tex/d3d11", e.magic);
        for c in &e.render {
            if let crate::ppp::RenderCall::Model(d) = c {
                g.vfx.push((crate::ppp::vfx::model(&e.space, d, &scene), dir.clone()));
            }
        }
    }
}

/// The scene the effect geometry is built in (the camera's screen matrix and inverse view, the ids).
pub fn vfx_scene(g: &FieldState, magic: u32, map_part: Option<u32>) -> crate::ppp::vfx::Scene {
    let view = g.cam_view.unwrap_or(crate::chr::elements::IDENTITY);
    crate::ppp::vfx::Scene {
        screen: g.cam_screen.unwrap_or(crate::chr::elements::IDENTITY),
        inverse_view: crate::ppp::driver::rigid_inverse(&view),
        magic,
        map_id: g.map_id,
        event: u32::from(g.scene.unwrap_or(0)),
        map_part,
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
                    g.effects.effect = load_effect(g, g.effects.id);
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
