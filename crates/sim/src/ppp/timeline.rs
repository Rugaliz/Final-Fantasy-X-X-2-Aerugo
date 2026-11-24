//! The timeline effects: effect files whose driver plays a small script (the timeline) that starts particle parts
//! and drives the engine, the same 53-opcode interpreter in 25 effect files (638 and 689 among them).
//!
//! The driver's life:
//! - set-up (when the effect starts): the display state it may change is kept (the field side does that), the
//!   task pool cleared;
//! - before it is set up, its draw entry waits for the effect system, sends the ready call once and, when the
//!   answer comes, sets up: the timeline from its script with nothing in flight, two tasks — the script task and
//!   the parts task — and from then on its run and draw entries are the task runners;
//! - run (after the scripts): the screen shake offsets start at zero, the script task steps the timeline (when
//!   the timeline ends the task goes and the effect remembers the end), then the shake offsets go to the screen;
//! - draw (the draw phase): the parts task updates and draws every part in flight; once none is left and the
//!   timeline has ended, the effect reports itself done.
//!
//! The timeline runs while its state is "running", one opcode after another; a wait holds it for a number of
//! frames (0xffff: for good), the callback state hands each frame to a driver function, and the per-frame
//! callback runs after it every frame.
//!
//! Some timeline files also keep sprites of their own (effect 688: drops rising from a character's body). The
//! script sets the spawner as its per-frame callback; a third task, run before the parts task in the draw
//! phase, moves and draws every live sprite through the engine's shape entries; when the effect ends, every
//! sprite's entry is given back.

use super::driver::{Effect, Host, Signals};
use super::{Part, RenderCall, Space, SpriteDraw};
use crate::chr::elements::{IDENTITY, Mat, apply4, mul, rot_x, rot_y, rot_z};

/// How many sprites a timeline file's pool holds.
pub const SPRITE_POOL: usize = 0x200;

/// The OEF2 table kind that holds the shapes a driver draws itself.
const SHAPE_KIND: u32 = 9;

/// Where a timeline effect file keeps its timeline: the script and the table of part records (0x1c bytes each:
/// position x, y, z, w, then the scale, the turn about Y and the program index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineLayout {
    pub script: u32,
    pub part_records: u32,
    /// The driver function the script's call opcode names to start a part, and what it does.
    pub start_part_fn: u32,
    pub part_start: PartStart,
    /// The sprites the file spawns itself, when it has them.
    pub sprites: Option<SpriteLayout>,
}

/// What a file's start-part function does: the part it starts (the one register 0 names, or always the same
/// one) and, when it gives the part one, the fade-out callback's frame count (the part lives that many frames,
/// its colour fading out over the last 64).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartStart {
    pub fixed_part: Option<usize>,
    pub fade_out: Option<i32>,
}

impl PartStart {
    /// The part register 0 names, no callback.
    pub const BY_REGISTER: PartStart = PartStart { fixed_part: None, fade_out: None };
}

/// Where a timeline file keeps what its sprites need: the driver function the script sets as its per-frame
/// callback (one sprite each call), the actor whose body they rise from (its body is looked up once, at the
/// set-up), the table of the five body elements a sprite starts at, and the slot of its shape in the OEF2's
/// shape table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpriteLayout {
    pub spawn_fn: u32,
    pub actor: i32,
    pub elements: u32,
    pub shape_slot: u32,
}

/// One of a timeline file's own sprites (0x74 bytes in the original's pool).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sprite {
    pub live: bool,
    /// Its world position (w 1), velocity and acceleration (the velocity takes the acceleration, the position
    /// the velocity, each frame), and the damping the acceleration takes first (with flag bit 1).
    pub position: [f32; 4],
    pub velocity: [f32; 3],
    pub acceleration: [f32; 3],
    pub damping: f32,
    /// Its turn about Z (kept in [−π, π]) and its turn per frame.
    pub angle: f32,
    pub spin: f32,
    /// The shape it draws, whether its frames still play, the frame (−1 before the first) and the frame's
    /// time left (in 0x200 steps).
    pub shape: u32,
    pub animating: bool,
    pub frame: i16,
    pub frame_time: i16,
    /// Its scale, the scale it grows to (a factor) over `grow_frames` (0: there at once), and the frames grown.
    pub scale: f32,
    pub grow_to: f32,
    pub grow_frames: i32,
    pub grown: i32,
    /// The lift the work matrix takes on Y (flag bit 0 asks for the work matrix).
    pub lift: f32,
    /// Frames left to live (negative: until its frames stop playing), and frames before it starts moving.
    pub life: i32,
    pub delay: i32,
    /// Its colour fading from `colour_from` to `colour_to` over `fade_frames` (0: `colour_to` at once).
    pub colour_from: [u8; 4],
    pub colour_to: [u8; 4],
    pub fade_frames: i32,
    pub faded: i32,
    pub flags: u16,
}

/// A timeline file's sprite pool: the sprites, where the next search for a free one starts, and the shapes of
/// every sprite as they were each time the search came round past the end (their entries are given back too).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpritePool {
    pub sprites: Vec<Sprite>,
    pub cursor: usize,
    pub rounds: Vec<Vec<u32>>,
    /// The worker whose body the sprites rise from (looked up at the set-up).
    pub body: Option<usize>,
    /// The draw parameters' flags word (cleared, then bit 23 set, by the script task each frame).
    pub param_flags: u32,
}

impl SpritePool {
    fn new() -> SpritePool {
        SpritePool { sprites: vec![Sprite::default(); SPRITE_POOL], ..SpritePool::default() }
    }

    /// A free sprite (searching on from the cursor, round past the end at most once), reset: no growth or
    /// fade, grow-to 1, colours 0x80 grey, no delay or flags. Coming round past the end keeps every sprite's
    /// shape first.
    fn take(&mut self) -> Option<usize> {
        let n = self.sprites.len();
        let mut cursor = self.cursor;
        for _ in 0..n {
            let i = cursor;
            cursor += 1;
            if n <= cursor {
                self.rounds.push(self.sprites.iter().map(|s| s.shape).collect());
                cursor = 0;
            }
            let s = &mut self.sprites[i];
            if !s.live {
                s.flags = 0;
                s.grow_frames = 0;
                s.spin = 0.0;
                s.colour_from = [0x80; 4];
                s.colour_to = [0x80; 4];
                s.delay = 0;
                s.faded = 0;
                s.grow_to = 1.0;
                s.live = true;
                self.cursor = cursor;
                return Some(i);
            }
        }
        None
    }
}

/// What the timeline is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScriptState {
    /// Finished.
    Ended,
    #[default]
    Running,
    /// Waiting out its frame count.
    Waiting,
    /// Handing each frame to its callback.
    InCallback,
}

/// The timeline interpreter's context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Script {
    pub pc: u32,
    pub state: ScriptState,
    /// The wait's frames left (0xffff: for good).
    pub wait: u16,
    /// Loops: where each starts again and how many times it still runs (0xffff: for ever).
    pub loop_starts: [u32; 4],
    pub loop_counts: [u16; 4],
    pub loop_depth: u8,
    /// Subroutine returns.
    pub returns: [u32; 4],
    pub call_depth: u8,
    pub callback: u32,
    pub frame_callback: u32,
    pub registers: [u32; 8],
}

/// A part the timeline started: the particle part and how the driver places it each frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TimelinePart {
    pub part: Part,
    /// Turns about X, Y and Z.
    pub turn: [f32; 3],
    /// The place it is put at (added to the turned origin), and the point its frame takes the origin from.
    pub offset: [f32; 4],
    pub origin: [f32; 4],
    /// The colour the driver hands the programs (four floats, 1.0 each at start).
    pub colour: [f32; 4],
    /// Frames before it starts updating, frames updated, its in-flight and held switches.
    pub delay: u32,
    pub frames: u32,
    pub in_flight: bool,
    pub held: bool,
    /// The fade-out callback's frames left, when the part has the callback (each frame one less; under 64 the
    /// part's colour is that many 64ths; at 0 the part stops).
    pub fade_out: Option<i32>,
    /// Stopping: the next update frees the part without stepping it.
    pub stopping: bool,
}

/// A timeline effect's driver state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Timeline {
    pub layout: Option<TimelineLayout>,
    pub ready_sent: bool,
    pub set_up: bool,
    pub script: Script,
    /// The script task is still in the task list; the timeline has ended.
    pub script_task: bool,
    pub script_ended: bool,
    /// Part records by index, and the parts in flight in the order they started.
    pub records: Vec<TimelinePart>,
    pub in_flight: Vec<usize>,
    /// The screen shake offsets the frame's opcodes set (both 0 unless one sets them).
    pub shake: [u32; 2],
    /// The file's own sprites (when its layout has them; made at the set-up).
    pub sprites: Option<SpritePool>,
}

impl Timeline {
    pub fn new(layout: TimelineLayout) -> Timeline {
        Timeline { layout: Some(layout), ..Timeline::default() }
    }
}

/// What a timeline frame asks of the field beyond the signals: the shake offsets for the current screen slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimelineOut {
    pub shake: Option<[u32; 2]>,
}

impl Effect {
    /// The run entry: before the set-up nothing; after it the script task, between the shake offsets' reset and
    /// their hand-over to the screen.
    pub(super) fn timeline_run(&mut self, h: &mut Host) -> (Signals, TimelineOut) {
        let Some(mut t) = self.timeline.take() else { return Default::default() };
        let mut out = TimelineOut::default();
        if t.set_up {
            t.shake = [0, 0];
            if t.script_task
                && let Some(pool) = t.sprites.as_mut()
            {
                pool.param_flags = 0x0080_0000;
            }
            if t.script_task && self.step_script(&mut t, h) {
                t.script_ended = true;
                t.script_task = false;
            }
            out.shake = Some(t.shake);
        }
        self.timeline = Some(t);
        (Signals::default(), out)
    }

    /// The draw entry: before the set-up the ready sequence (the effect system free, the ready call once, the
    /// set-up when it is answered — at once for an event effect); after it the parts task, and the effect done
    /// once no part is in flight and the timeline has ended.
    pub(super) fn timeline_draw(&mut self, h: &mut Host) -> Signals {
        let Some(mut t) = self.timeline.take() else { return Signals::default() };
        let mut s = Signals::default();
        if !t.set_up {
            if !h.busy {
                if !t.ready_sent {
                    t.ready_sent = true;
                    s.ready = true;
                }
                self.ready = true;
                self.set_up_timeline(&mut t, h);
            }
        } else {
            if t.sprites.is_some() {
                self.update_sprites(&mut t, h);
            }
            self.world = h.view;
            let none_left = self.update_parts(&mut t, h);
            if none_left && t.script_ended {
                s.done = true;
                self.release_sprites(&t);
            }
        }
        self.timeline = Some(t);
        s
    }

    /// The set-up: the sprite pool (with the body its sprites rise from) when the file has one, the timeline
    /// at its script's start with nothing in flight, the script task in the list, the part records' state
    /// cleared.
    fn set_up_timeline(&mut self, t: &mut Timeline, h: &mut Host) {
        let Some(layout) = t.layout else { return };
        t.sprites = layout.sprites.map(|sl| {
            let mut pool = SpritePool::new();
            pool.body = h.bodies.actor_body(sl.actor);
            pool
        });
        t.script = Script { pc: layout.script, ..Script::default() };
        t.script_task = true;
        t.script_ended = false;
        t.in_flight.clear();
        t.set_up = true;
    }

    /// One frame of the timeline: opcodes while it runs, then the wait's count (by the frame's ticks), the
    /// callback state and the per-frame callback. True once it has ended.
    fn step_script(&mut self, t: &mut Timeline, h: &mut Host) -> bool {
        while t.script.state == ScriptState::Running {
            match self.script_opcode(t, h) {
                Some(pc) => t.script.pc = pc,
                None => {
                    t.script.state = ScriptState::Ended;
                    break;
                }
            }
        }
        match t.script.state {
            ScriptState::Ended => return true,
            ScriptState::Waiting => {
                let step = u16::from(h.ticking);
                if t.script.wait != 0xffff {
                    if step < t.script.wait {
                        t.script.wait -= step;
                    } else {
                        t.script.wait = 0;
                        t.script.state = ScriptState::Running;
                    }
                }
            }
            ScriptState::InCallback => self.driver_function(t, h, t.script.callback),
            ScriptState::Running => {}
        }
        if t.script.frame_callback != 0 {
            self.driver_function(t, h, t.script.frame_callback);
        }
        false
    }

    /// One opcode at the timeline's pc; the next pc, or none when the opcode is not ported (reported; the
    /// timeline stops there).
    fn script_opcode(&mut self, t: &mut Timeline, h: &mut Host) -> Option<u32> {
        let pc = t.script.pc;
        let op = self.space.u16(pc);
        let arg = self.space.u16(pc + 2);
        let word = |e: &Effect, k: u32| e.space.u32(pc + 4 * k);
        if op == 7 {
            // run a driver function
            let f = word(self, 1);
            self.driver_function(t, h, f);
            return Some(pc + 8);
        }
        let s = &mut t.script;
        Some(match op {
            // wait(frames)
            0 => {
                s.state = ScriptState::Waiting;
                s.wait = arg;
                pc + 4
            }
            // jump(to)
            1 => word(self, 1),
            // jump to the place a register holds
            2 => s.registers[usize::from(arg & 7)],
            // loop(times) … loop end
            3 => {
                let d = usize::from(s.loop_depth & 3);
                s.loop_counts[d] = arg;
                s.loop_starts[d] = pc + 4;
                s.loop_depth = s.loop_depth.wrapping_add(1);
                pc + 4
            }
            4 => {
                let d = usize::from(s.loop_depth.wrapping_sub(1) & 3);
                if s.loop_counts[d] != 0xffff {
                    s.loop_counts[d] = s.loop_counts[d].wrapping_sub(1);
                }
                if s.loop_counts[d] == 0 {
                    s.loop_depth = s.loop_depth.wrapping_sub(1);
                    pc + 4
                } else {
                    s.loop_starts[d]
                }
            }
            // call(subroutine) / return
            5 => {
                s.returns[usize::from(s.call_depth & 3)] = pc + 8;
                s.call_depth = s.call_depth.wrapping_add(1);
                word(self, 1)
            }
            6 => {
                s.call_depth = s.call_depth.wrapping_sub(1);
                s.returns[usize::from(s.call_depth & 3)]
            }
            // hand the frames to a callback / set the per-frame callback
            8 => {
                s.state = ScriptState::InCallback;
                s.callback = word(self, 1);
                pc + 8
            }
            9 => {
                s.frame_callback = word(self, 1);
                pc + 8
            }
            // register = value / register = the place after this opcode
            10 => {
                s.registers[usize::from(arg & 7)] = word(self, 1);
                pc + 8
            }
            11 => {
                s.registers[usize::from(arg & 7)] = pc + 4;
                pc + 4
            }
            // end
            12 => {
                s.state = ScriptState::Ended;
                pc
            }
            _ => {
                log::error!("GAP: effect {} timeline opcode {op} (at {pc:#x}) is not ported", self.magic);
                return None;
            }
        })
    }

    /// A driver function a script names (by its address in the effect file): the ones ported, else reported.
    fn driver_function(&mut self, t: &mut Timeline, h: &mut Host, f: u32) {
        let Some(layout) = t.layout else { return };
        if f == layout.start_part_fn {
            let n = layout.part_start.fixed_part.unwrap_or(t.script.registers[0] as usize);
            self.start_timeline_part(t, h, n);
            if let Some(p) = t.records.get_mut(n) {
                p.fade_out = layout.part_start.fade_out;
            }
        } else if let Some(sl) = layout.sprites.filter(|sl| sl.spawn_fn == f) {
            self.spawn_sprite(t, h, sl);
        } else {
            log::error!(
                "GAP: effect {} driver function {f:#x} (from its timeline) is not ported",
                self.magic
            );
        }
    }

    /// Start part `n` from its record: placed at the record's position (w included), turned about Y by the
    /// record's angle, scaled by its scale on all four, the record's program started (step 0x1000 a frame) and
    /// the part put in flight after the others.
    fn start_timeline_part(&mut self, t: &mut Timeline, h: &mut Host, n: usize) {
        let Some(layout) = t.layout else { return };
        let rec = layout.part_records + n as u32 * 0x1c;
        let f = |e: &Effect, k: u32| f32::from_bits(e.space.u32(rec + k));
        let scale = f(self, 0x10);
        let program = self.space.u32(rec + 0x18);
        let group = self.group;
        if t.records.len() <= n {
            t.records.resize(n + 1, TimelinePart::default());
        }
        let mut part = Part::default();
        self.run_part(h, false, &mut part, |r, part| {
            let list = r.m.u32(group + 0x10);
            let record = r.m.u32(list + program * 4);
            *part = Part::start(r, 0x1000, record);
        });
        part.place = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, scale, scale, scale, scale];
        t.records[n] = TimelinePart {
            part,
            turn: [0.0, f(self, 0x14), 0.0],
            offset: [f(self, 0), f(self, 4), f(self, 8), f(self, 0xc)],
            origin: [0.0, 0.0, 0.0, 1.0],
            colour: [1.0; 4],
            delay: 0,
            frames: 0,
            in_flight: true,
            held: false,
            fade_out: None,
            stopping: false,
        };
        t.in_flight.push(n);
    }

    /// The parts task: every part in flight (not held) whose delay has run out runs its callback, is placed (the
    /// frame: turned about Z, X then Y, scaled, at its offset plus its turned origin; the camera and view:
    /// world · frame) and stepped with the effect's own flag up; a finished or stopping part is freed and leaves
    /// the list. True when no part was in flight.
    fn update_parts(&mut self, t: &mut Timeline, h: &mut Host) -> bool {
        let list = t.in_flight.clone();
        for n in &list {
            let n = *n;
            let Some(mut p) = t.records.get(n).copied() else { continue };
            if !p.in_flight || p.held {
                continue;
            }
            if p.delay != 0 {
                p.delay -= 1;
                t.records[n] = p;
                continue;
            }
            if let Some(left) = p.fade_out.as_mut() {
                *left -= 1;
                if *left < 0x40 {
                    if *left > 0 {
                        p.part.colour = [((*left * 0x1000) / 64) as u16; 4];
                    } else {
                        p.stopping = true;
                    }
                }
            }
            let mut m: Mat = rot_y(&rot_x(&rot_z(&IDENTITY, p.turn[2]), p.turn[0]), p.turn[1]);
            let at = apply4(&m, p.origin);
            for (row, k) in [(0usize, 8usize), (4, 9), (8, 10)] {
                for c in 0..3 {
                    m[row + c] *= p.part.place[k];
                }
            }
            m[12] = p.offset[0] + at[0];
            m[13] = p.offset[1] + at[1];
            m[14] = p.offset[2] + at[2];
            p.part.place[0] = m[12];
            p.part.place[1] = m[13];
            p.part.place[2] = m[14];
            self.frame = m;
            self.camera = mul(&self.world, &self.frame);
            self.view = self.camera;
            let mut part = p.part;
            let done = std::mem::take(&mut p.stopping)
                || self.run_part(h, true, &mut part, |r, part| part.update(r, 0x30));
            if done {
                self.run_part(h, false, &mut part, |r, part| part.free(r));
                p.in_flight = false;
                t.in_flight.retain(|&k| k != n);
            }
            p.part = part;
            p.frames += 1;
            t.records[n] = p;
        }
        list.is_empty()
    }

    /// The spawner: a free sprite with the file's shape, frames playing from before the first, at a random
    /// point near one of five elements of the body (x within ±2000, y and z within ±1000 of the element, in its
    /// frame), rising at 3–8 a frame and drifting up to 5 sideways (the drift slowed by up to 2 % a frame either
    /// way), turned at random, a fifth of the size give or take a fifth, living until its frames stop. The random
    /// draws keep the original's order (the C runtime's pick of the element among the vector unit's draws).
    fn spawn_sprite(&mut self, t: &mut Timeline, h: &mut Host, sl: SpriteLayout) {
        let Some(pool) = t.sprites.as_mut() else { return };
        let Some(i) = pool.take() else { return };
        let shape = super::relocate::oef2_entry(&self.space, self.layout.oef2, SHAPE_KIND, sl.shape_slot);
        let mut dummy = Part::default();
        let (local, pick, motion) = self.run_part(h, false, &mut dummy, |r, _| {
            let offset = |r: &mut super::Run, p: f32| (r.vu_sym(p) as f32) * 1000.0;
            let local = [offset(r, 2.0), offset(r, 1.0), offset(r, 1.0), 1.0];
            let pick = r.crt.rand() % 5;
            let vx = r.vu_sym(5.0) as f32;
            let vy = -((r.vu_unit(5.0) as f32) + 3.0);
            let vz = r.vu_sym(5.0) as f32;
            let angle = r.vu_sym(std::f32::consts::PI) as f32;
            let ax = -(r.vu_sym(0.02) as f32) * vx;
            let az = -(r.vu_sym(0.02) as f32) * vz;
            let scale = ((r.vu_sym(0.2) as f32) + 1.0) * 0.2;
            (local, pick, [vx, vy, vz, angle, ax, az, scale])
        });
        let element = self.space.u32(sl.elements + 4 * pick as u32) as i32;
        let at = pool.body.and_then(|w| h.bodies.body_element_matrix(w, element));
        let s = &mut pool.sprites[i];
        let Some(m) = at else {
            log::error!(
                "GAP: effect {} sprite: actor {:#x} has no body to rise from (the original reads through a null body)",
                self.magic,
                sl.actor
            );
            s.live = false;
            return;
        };
        let [vx, vy, vz, angle, ax, az, scale] = motion;
        s.shape = shape;
        s.frame = -1;
        s.frame_time = 0x200;
        s.animating = true;
        s.position = apply4(&m, local);
        s.position[3] = 1.0;
        s.velocity = [vx, vy, vz];
        s.angle = angle;
        s.acceleration = [ax, 0.0, az];
        s.damping = 1.0;
        s.life = -1;
        s.fade_frames = 0;
        s.scale = scale;
    }

    /// The sprite task: every live sprite past its delay moves (the acceleration damped with flag bit 1, the
    /// velocity, the position), grows, turns, takes its colour, steps its frames and is drawn — turned about Z
    /// and scaled, at its position through the camera view — or, once its frames have stopped with no life
    /// count, goes; a life count runs down to its end.
    fn update_sprites(&mut self, t: &mut Timeline, h: &mut Host) {
        let Some(pool) = t.sprites.as_mut() else { return };
        let mut draws = Vec::new();
        for (i, s) in pool.sprites.iter_mut().enumerate() {
            if !s.live {
                continue;
            }
            if s.delay > 0 {
                s.delay -= 1;
                continue;
            }
            if s.flags & 2 != 0 {
                for a in &mut s.acceleration {
                    *a *= s.damping;
                }
            }
            for k in 0..3 {
                s.velocity[k] += s.acceleration[k];
                s.position[k] += s.velocity[k];
            }
            let size = if s.grow_frames > 0 {
                let f = s.grown as f32 / s.grow_frames as f32;
                s.grown += 1;
                let v = (s.grow_to * f + (1.0 - f)) * s.scale;
                if s.grow_frames < s.grown {
                    s.grow_frames = 0;
                }
                v
            } else {
                s.grow_to * s.scale
            };
            let mut angle = s.spin + s.angle;
            if angle > std::f32::consts::PI {
                angle -= std::f32::consts::TAU;
            } else if angle < -std::f32::consts::PI {
                angle += std::f32::consts::TAU;
            }
            s.angle = angle;
            let mut sized = IDENTITY;
            sized[0] = size;
            sized[5] = size;
            sized[10] = size;
            let mut matrix = rot_z(&sized, angle);
            matrix[12..16].copy_from_slice(&apply4(&h.view, s.position));
            let work_matrix = (s.flags & 1 != 0).then(|| {
                let mut w = matrix;
                w[14] += s.lift;
                w
            });
            let colour = if s.fade_frames > 0 {
                let k = s.faded.abs();
                let n = s.fade_frames;
                let c = std::array::from_fn(|c| {
                    ((i32::from(s.colour_from[c]) * (n - k) + i32::from(s.colour_to[c]) * k) / n) as u8
                });
                s.faded += 1;
                if s.fade_frames < s.faded {
                    s.fade_frames = 0;
                }
                c
            } else {
                s.colour_to
            };
            let frame_record = sprite_frame_step(&self.space, s);
            if s.life < 0 && !s.animating {
                s.life = 1;
            } else {
                let sprites = s.shape.wrapping_add(self.space.u16(frame_record) as i16 as i32 as u32);
                let blend = self.space.u8(sprites.wrapping_add(self.space.u32(sprites + 4)) + 4);
                let create = super::programs::blend_state(blend) | u32::from(work_matrix.is_some());
                draws.push(RenderCall::Sprite(Box::new(SpriteDraw {
                    index: i as u32,
                    shape: s.shape,
                    frame: s.frame as u16,
                    param_flags: pool.param_flags,
                    colour,
                    matrix,
                    work_matrix,
                    create,
                })));
            }
            if s.life > 0 {
                s.life -= 1;
                if s.life == 0 {
                    s.live = false;
                }
            }
        }
        self.render.extend(draws);
    }

    /// The end: every sprite's entry given back (the sprites' shapes now and as kept at each round of the pool).
    fn release_sprites(&mut self, t: &Timeline) {
        let Some(pool) = t.sprites.as_ref() else { return };
        let now = pool.sprites.iter().map(|s| s.shape).collect::<Vec<_>>();
        for shapes in std::iter::once(&now).chain(&pool.rounds) {
            for (i, &shape) in shapes.iter().enumerate() {
                if shape != 0 {
                    self.render.push(RenderCall::SpriteRelease { index: i as u32, shape });
                }
            }
        }
    }
}

/// A sprite's frame step (one 0x200 step of its frame's time; past the time, the next frame — past the last,
/// the first again when the frame loops, else the frames stop on it): the record of the frame to draw.
fn sprite_frame_step(m: &Space, s: &mut Sprite) -> u32 {
    let shape = s.shape;
    let record = |frame: i16| shape.wrapping_add(((i32::from(frame) + 2) * 8) as u32);
    let frame = s.frame;
    let mut at = record(frame);
    if s.animating {
        s.frame_time = s.frame_time.wrapping_sub(0x200);
        if s.frame_time < 1 {
            s.frame = frame.wrapping_add(1);
            if m.u16(shape + 6) as i16 <= s.frame {
                if m.u8(at + 4) & 0x80 == 0 {
                    s.frame = frame;
                    s.animating = false;
                    return at;
                }
                s.frame = 0;
            }
            at = record(s.frame);
            s.frame_time = m.u16(at + 2) as i16;
        }
    }
    at
}
