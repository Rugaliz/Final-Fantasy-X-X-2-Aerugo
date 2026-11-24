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
    /// The event system's effect flags: `hold` (an effect holds the event), `effect_flag`, `request` (the
    /// loader's request: 1 load, 4 freed).
    pub hold: u32,
    pub effect_flag: u32,
    pub request: u32,
    /// The bodies' effects held off (0244) / let on (0245): bit 0x800 of the effect system's flags.
    pub et_flags: u32,
    /// The running effect's driver and the engine's vector-unit random generator.
    pub effect: Option<Box<crate::ppp::driver::Effect>>,
    pub vu: crate::ppp::VuRand,
    /// The shape entries the effect's draws have made (kept until destroyed or the effect is freed).
    pub shape_entries: ShapeEntries,
    /// The map display a timeline effect keeps when it starts and puts back when it is freed: the fog block's
    /// colour, distances and background bytes, and the remaster's background colour.
    pub display_kept: Option<KeptDisplay>,
}

/// Which shape entry a draw updates: a program's (its object and reference index) or a driver's own sprite's
/// (its index in the pool and its shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShapeKey {
    Program { object: u32, ref_index: u32 },
    Sprite { index: u32, shape: u32 },
}

/// The remaster's shape entries: made on a shape's first draw (with that draw's render state and texture) and
/// updated by every later one, until destroyed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShapeEntries(pub std::collections::BTreeMap<ShapeKey, crate::ppp::vfx::ShapeEntry>);

impl ShapeEntries {
    /// The frame's shape and sprite draws (and entry releases) of `calls` into meshes for the renderer, with
    /// textures from `dir`. A draw with no entry yet makes one unless it makes none this frame (then nothing is
    /// drawn).
    pub fn draws(
        &mut self,
        vfx: &mut Vec<(crate::ppp::vfx::Mesh, String)>,
        space: &crate::ppp::Space,
        calls: &[crate::ppp::RenderCall],
        scene: &crate::ppp::vfx::Scene,
        dir: &str,
    ) {
        use crate::ppp::RenderCall;
        use crate::ppp::vfx::{ShapeInput, shape, shape_entry};
        for c in calls {
            let (key, input) = match c {
                RenderCall::Shape(d) => {
                    if d.path3 {
                        static ONCE: std::sync::atomic::AtomicBool =
                            std::sync::atomic::AtomicBool::new(false);
                        if !ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            log::error!(
                                "GAP: the third shape path (pppDrawShape's own update) is not ported"
                            );
                        }
                        continue;
                    }
                    let key = ShapeKey::Program { object: d.object, ref_index: d.ref_index };
                    let input = ShapeInput {
                        key: (d.object, d.ref_index),
                        shape: d.shape,
                        frame: d.frame,
                        flags: d.flags,
                        colour: d.colour,
                        matrix: d.matrix,
                        work_matrix: d.work_matrix,
                        create: d.create.unwrap_or(0),
                    };
                    if d.create.is_none() && !self.0.contains_key(&key) {
                        continue;
                    }
                    (key, input)
                }
                RenderCall::Sprite(d) => (
                    ShapeKey::Sprite { index: d.index, shape: d.shape },
                    ShapeInput {
                        key: (d.shape, d.index),
                        shape: d.shape,
                        frame: u32::from(d.frame),
                        flags: (d.param_flags >> 16) as u16,
                        colour: d.colour,
                        matrix: d.matrix,
                        work_matrix: d.work_matrix,
                        create: d.create,
                    },
                ),
                RenderCall::Destroy { object, ref_index } => {
                    self.0.remove(&ShapeKey::Program { object: *object, ref_index: *ref_index });
                    continue;
                }
                RenderCall::SpriteRelease { index, shape } => {
                    self.0.remove(&ShapeKey::Sprite { index: *index, shape: *shape });
                    continue;
                }
                _ => continue,
            };
            let entry =
                self.0.entry(key).or_insert_with(|| shape_entry(space, input.shape, input.create, scene));
            for mesh in shape(space, &input, entry, scene) {
                vfx.push((mesh, dir.to_owned()));
            }
        }
    }
}

/// The display state a timeline effect's set-up keeps and its free entry restores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeptDisplay {
    pub fog_rgba: [u8; 4],
    pub fog_limit: f32,
    pub fog_near: f32,
    pub fog_far: f32,
    pub back: [u8; 4],
    pub background: Option<[f32; 3]>,
}

impl KeptDisplay {
    fn keep(g: &FieldState) -> KeptDisplay {
        let f = &g.ps_fog;
        KeptDisplay {
            fog_rgba: f.rgba,
            fog_limit: f.limit,
            fog_near: f.near,
            fog_far: f.far,
            back: f.back,
            background: g.background,
        }
    }

    /// The timeline driver's free entry: the kept fog and background back, the motion blur off.
    fn restore(&self, g: &mut FieldState) {
        let f = &mut g.ps_fog;
        f.rgba = self.fog_rgba;
        f.limit = self.fog_limit;
        f.near = self.fog_near;
        f.far = self.fog_far;
        f.back = self.back;
        g.background = self.background;
        g.screen.blur(0);
    }
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
    let mut paused = g.particle_pause != 0;
    let mut vu = g.effects.vu;
    let mut crt = g.crt_rand;
    let out = {
        let field: &FieldState = g;
        let mut h = crate::ppp::driver::Host {
            ticking: true,
            busy: false,
            event_bits: field.engine.event_bits,
            view: field.cam_view.unwrap_or(crate::chr::elements::IDENTITY),
            screen: field.cam_screen.unwrap_or(crate::chr::elements::IDENTITY),
            paused: &mut paused,
            vu: &mut vu,
            crt: &mut crt,
            event: u32::from(field.scene.unwrap_or(0)),
            map_id: field.map_id,
            bodies: field,
        };
        f(&mut e, &mut h)
    };
    g.crt_rand = crt;
    if paused {
        g.particle_pause = 1;
    }
    g.effects.vu = vu;
    g.effects.effect = Some(e);
    Some(out)
}

/// The characters' bodies as the effect drivers see them: the worker playing an actor (the first whose +0x40
/// is it) when it is bound to a body, and its bodies' element matrices.
impl crate::ppp::driver::Bodies for FieldState {
    fn actor_body(&self, actor: i32) -> Option<usize> {
        let w = self.workers.list.iter().position(|wk| wk.i32(0x40) == actor)?;
        has_body(self, w).then_some(w)
    }

    fn body_element_matrix(&self, worker: usize, element: i32) -> Option<crate::chr::elements::Mat> {
        FieldState::body_element_matrix(self, worker, element)
    }
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
        if let Some(shake) = g.effects.effect.as_ref().and_then(|e| e.screen_shake) {
            g.cam.set_screen_shake(shake);
        }
        if ready && g.effects.state == 4 {
            g.effects.state = 5;
        }
    }
    if g.effects.state == 6 {
        if g.effects.effect.is_some() {
            effect_free(g);
        }
        if let Some(kept) = g.effects.display_kept.take() {
            kept.restore(g);
        }
        g.effects.shape_entries.0.clear();
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
    // A timeline effect sends its ready call from its draw entry.
    let signals = with_driver(g, |e, h| e.draw(h));
    if signals.is_some_and(|s| s.ready) && g.effects.state == 4 {
        g.effects.state = 5;
    }
    if signals.is_some_and(|s| s.done) {
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
        g.effects.shape_entries.draws(&mut g.vfx, &e.space, &e.render, &scene, &dir);
    }
}

/// The scene the effect geometry is built in (the camera's screen matrix and inverse view, the ids).
pub fn vfx_scene(g: &FieldState, magic: u32, map_part: Option<u32>) -> crate::ppp::vfx::Scene {
    let view = g.cam_view.unwrap_or(crate::chr::elements::IDENTITY);
    crate::ppp::vfx::Scene {
        screen: g.cam_screen.unwrap_or(crate::chr::elements::IDENTITY),
        view,
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
                g.effects.effect_flag = 0;
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
                    let timeline = g.effects.effect.as_ref().is_some_and(|e| e.timeline.is_some());
                    g.effects.display_kept = timeline.then(|| KeptDisplay::keep(g));
                    if g.effects.effect.is_none() {
                        log::error!(
                            "GAP: event effect {} has no ported driver (or no file): it signals at once and never ends",
                            g.effects.id
                        );
                    }
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
