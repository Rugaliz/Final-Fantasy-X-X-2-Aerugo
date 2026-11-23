//! The effects' drivers: each effect file's own program around the particle runtime (its set-up, the run the
//! engine calls after the scripts, the draw it calls in the draw phase, and the frame callback the draw
//! registers). Each driver is ported once per shape; the effect files that share a shape differ only in where
//! their data lies, which [`Layout`] holds.
//!
//! The looping part (effects 662 and 711): one part placed facing the camera, restarted whenever it ends, run
//! from a frame callback that the event's bit 0 starts and bit 1 ends (the end frees the part and tells the
//! event system the effect is done). After 800 frames of its timeline it sets the particle runtime's pause, so
//! the effect holds its last frame.

use super::relocate;
use super::{Env, Part, Program, RenderCall, Run, SCRATCH_VECTOR, Scene, Space, VuRand};
use crate::chr::elements::{IDENTITY, Mat, apply4, mul};
use aerugo_formats::magic::DllData;

/// Where an effect file's driver keeps its data: the OEF2 file, the program table, the work buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub oef2: u32,
    pub table: u32,
    pub work: u32,
    pub work_size: u32,
}

/// The driver shapes ported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    LoopingPart,
}

/// The driver and data layout of magic file `magic`, when its shape is ported.
pub fn driver(magic: u32) -> Option<(Shape, Layout)> {
    let work_size = 0x8_0000;
    Some(match magic {
        662 => (
            Shape::LoopingPart,
            Layout { oef2: 0x1000_4020, table: 0x100c_0768, work: 0x100c_1340, work_size },
        ),
        711 => (
            Shape::LoopingPart,
            Layout { oef2: 0x1000_4020, table: 0x100b_c5c8, work: 0x100b_d140, work_size },
        ),
        _ => return None,
    })
}

/// What a driver reads from (and changes in) the engine.
#[derive(Debug)]
pub struct Host<'a> {
    /// The frame's tick count is not zero.
    pub ticking: bool,
    /// The effect system is busy (battle effect commands hold it; never in the field).
    pub busy: bool,
    /// The event bits the scripts set.
    pub event_bits: u32,
    /// The camera's view matrix.
    pub view: Mat,
    /// The particle runtime's pause.
    pub paused: &'a mut bool,
    pub vu: &'a mut VuRand,
    pub crt: &'a mut crate::field::face::CrtRand,
    /// The current event and the remaster's map id (for the draw adjustments).
    pub event: u32,
    pub map_id: u32,
}

/// What a driver call asks of the event system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Signals {
    /// The effect is ready (the loader's running state becomes signalled).
    pub ready: bool,
    /// The effect is done (the loader frees it).
    pub done: bool,
}

/// A loaded effect and its driver's state.
#[derive(Debug, Clone)]
pub struct Effect {
    pub magic: u32,
    pub shape: Shape,
    pub layout: Layout,
    pub space: Space,
    pub env: Env,
    pub table: Vec<Program>,
    /// The program names the runtime does not have yet.
    pub missing: Vec<String>,
    pub group: u32,
    pub part: Part,
    /// The run's three steps done: the busy check, the ready call, the ready answer.
    pub checked: bool,
    pub ready_sent: bool,
    pub ready: bool,
    /// The frame callback registered (once; `callback_on` while it runs).
    pub callback_set: bool,
    pub callback_on: bool,
    /// The timeline: its entry running, its frame count; the callback's frame count; the draw's idle count.
    pub timeline_on: bool,
    pub frames: u32,
    pub callback_frames: u32,
    pub idle: u32,
    /// The engine matrices the driver keeps for the programs: the world (the camera's view), the frame (the
    /// part's placement), the camera (world · frame) and the view (the same).
    pub world: Mat,
    pub frame: Mat,
    pub camera: Mat,
    pub view: Mat,
    /// The render requests of the last callback.
    pub render: Vec<RenderCall>,
}

/// A program name with the static lifetime programs carry (each distinct name kept once).
fn intern(name: &str) -> &'static str {
    static NAMES: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(n) = names.iter().find(|n| **n == name) {
        return n;
    }
    let n: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.push(n);
    n
}

impl PartialEq for Effect {
    fn eq(&self, o: &Effect) -> bool {
        let names = |e: &Effect| e.table.iter().map(|p| p.name).collect::<Vec<_>>();
        self.magic == o.magic
            && self.space == o.space
            && self.env == o.env
            && names(self) == names(o)
            && self.part == o.part
            && (
                self.checked,
                self.ready_sent,
                self.ready,
                self.callback_set,
                self.callback_on,
                self.timeline_on,
            ) == (o.checked, o.ready_sent, o.ready, o.callback_set, o.callback_on, o.timeline_on)
            && (self.frames, self.callback_frames, self.idle) == (o.frames, o.callback_frames, o.idle)
            && (self.world, self.frame, self.camera, self.view) == (o.world, o.frame, o.camera, o.view)
            && self.render == o.render
    }
}

/// The rigid inverse of a matrix: its turn transposed and the translation turned back (w row 0, 0, 0, 1).
fn rigid_inverse(p: &Mat) -> Mat {
    let d = f64::from;
    let mut o = [0.0; 16];
    for i in 0..3 {
        for j in 0..3 {
            o[i * 4 + j] = p[j * 4 + i];
        }
        let s = (d(p[4 * i]) * d(p[12]) + d(p[4 * i + 1]) * d(p[13])) + d(p[4 * i + 2]) * d(p[14]);
        o[12 + i] = (-s) as f32;
    }
    o[15] = 1.0;
    o
}

impl Effect {
    /// The set-up (the overlay's first entry): the OEF2 file's tables, its particle group 0 bound to the
    /// program table, the instance over the work buffer, the timeline (one entry from frame 0, until ended).
    pub fn load(magic: u32, dll: &DllData) -> Option<Effect> {
        let (shape, layout) = driver(magic)?;
        let mut space = Space::default();
        space.regions.push(super::Region { base: dll.va, bytes: dll.bytes.clone() });
        space.regions.push(super::Region { base: SCRATCH_VECTOR, bytes: vec![0; 16] });
        let names = dll.program_names(layout.table);
        let mut missing = Vec::new();
        let table = names
            .iter()
            .map(|n| {
                let name = intern(n);
                super::programs::by_name(name).unwrap_or_else(|| {
                    missing.push(n.clone());
                    Program { name, ..Program::default() }
                })
            })
            .collect();
        relocate::oef2_set(&mut space, layout.oef2);
        let group = relocate::oef2_particle(&mut space, layout.oef2, 0);
        relocate::bind(&mut space, group, layout.table);
        let lists = [space.u32(group + 0x14), space.u32(group + 0x18), space.u32(group + 0x1c)];
        let env = Env::new(&mut space, group, lists, layout.work, layout.work_size);
        Some(Effect {
            magic,
            shape,
            layout,
            space,
            env,
            table,
            missing,
            group,
            part: Part::default(),
            checked: false,
            ready_sent: false,
            ready: false,
            callback_set: false,
            callback_on: false,
            timeline_on: true,
            frames: 0,
            callback_frames: 0,
            idle: 0,
            world: IDENTITY,
            frame: IDENTITY,
            camera: IDENTITY,
            view: IDENTITY,
            render: Vec::new(),
        })
    }

    fn run_with<T>(&mut self, h: &mut Host, own: bool, f: impl FnOnce(&mut Run, &mut Part) -> T) -> T {
        let mut part = std::mem::take(&mut self.part);
        let mut r = Run {
            m: &mut self.space,
            env: &mut self.env,
            table_va: self.layout.table,
            table: &self.table,
            paused: *h.paused,
            vu: h.vu,
            crt: h.crt,
            objects: 0,
            slots: 0,
            part_scale: [1.0; 4],
            part_colour: [0x1000; 4],
            part_word: 0,
            view: self.view,
            camera: self.camera,
            frame: self.frame,
            world: self.world,
            screen: IDENTITY,
            camera_pos: [0.0; 4],
            ref_index: 0,
            part_draining: false,
            map_id: h.map_id,
            vertex_ap_slow: false,
            vertex_lc_extend: false,
            magic: self.magic,
            scene: Scene { event: h.event, own_magic: own, map_part: None, map_part_id: 0 },
            render: std::mem::take(&mut self.render),
        };
        let out = f(&mut r, &mut part);
        self.render = std::mem::take(&mut r.render);
        self.part = part;
        out
    }

    /// The part start with the driver's placement: (−16, −13, 259) facing the camera, scale 0.0916.
    fn start_part(&mut self, h: &mut Host) {
        let group = self.group;
        self.run_with(h, false, |r, part| {
            let record = r.m.u32(r.m.u32(group + 0x10));
            *part = Part::start(r, 0x1000, record);
            let s = f32::from_bits(0x3dbb_b0e6);
            part.place = [-16.0, -13.0, 259.0, 1.0, 0.0, 0.0, 0.0, f32::from_bits(1), s, s, s, 1.0];
        });
    }

    /// The timeline entry's update: the matrices (the frame: the camera's turn undone, at the part's position
    /// through it, scaled by the part's scale; the camera and the view: world · frame), the pause after 800
    /// frames, the part's step with the magic's own flag set (a finished part is freed and started again).
    fn update(&mut self, h: &mut Host) {
        let inv = rigid_inverse(&h.view);
        let mut m = inv;
        let pos = [self.part.place[0], self.part.place[1], self.part.place[2], self.part.place[3]];
        m[12..16].copy_from_slice(&apply4(&inv, pos));
        let mut scale = IDENTITY;
        let s = self.part.place[8];
        scale[0] = s;
        scale[5] = s;
        scale[10] = s;
        self.frame = mul(&m, &scale);
        self.camera = mul(&self.world, &self.frame);
        self.view = self.camera;
        if self.frames > 800 {
            *h.paused = true;
        }
        let done = self.run_with(h, true, |r, part| part.update(r, 0x30));
        if done {
            self.run_with(h, false, |r, part| part.free(r));
            self.start_part(h);
        }
    }

    /// The run (after the scripts, while the loader runs the effect): on a ticking frame, once the effect
    /// system is free, the ready call; ready once it is answered.
    pub fn run(&mut self, h: &Host) -> Signals {
        let mut s = Signals::default();
        if !h.ticking {
            return s;
        }
        if !self.checked {
            if h.busy {
                return s;
            }
            self.checked = true;
        }
        if !self.ready_sent {
            self.ready_sent = true;
            s.ready = true;
        }
        self.ready = true;
        s
    }

    /// The draw (the draw phase, while the loader runs the effect) and then the frame callback when it is on:
    /// event bit 0 turns the callback on (once); bit 1 turns it off, ends the timeline and reports the effect
    /// done.
    pub fn draw(&mut self, h: &mut Host) -> Signals {
        let mut s = Signals::default();
        if !self.ready {
            return s;
        }
        self.world = h.view;
        if h.event_bits & 1 != 0 && !self.callback_set {
            self.callback_frames = 0;
            self.callback_on = true;
            self.callback_set = true;
        }
        if h.event_bits & 2 != 0 {
            self.callback_on = false;
            self.end_timeline(h);
            s.done = true;
        } else {
            self.idle = self.idle.wrapping_add(1);
        }
        if self.callback_on {
            self.callback(h);
        }
        s
    }

    fn end_timeline(&mut self, h: &mut Host) {
        if self.timeline_on {
            self.timeline_on = false;
            self.run_with(h, false, |r, part| part.free(r));
        }
    }

    /// The frame callback: the timeline's step (its entry started on frame 0, updated every frame; the frame
    /// count moves on ticking frames), then the callback's own count.
    fn callback(&mut self, h: &mut Host) {
        self.render.clear();
        if self.timeline_on {
            if self.frames == 0 {
                self.start_part(h);
            }
            self.update(h);
            if h.ticking {
                self.frames = self.frames.wrapping_add(1);
            }
        }
        self.callback_frames = self.callback_frames.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rigid_inverse_undoes_a_turn_and_move() {
        let m = [0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 5.0, 6.0, 7.0, 1.0];
        let back = mul(&rigid_inverse(&m), &m);
        for (i, v) in back.iter().enumerate() {
            assert!((v - IDENTITY[i]).abs() < 1e-6, "{i}: {v}");
        }
    }
}
