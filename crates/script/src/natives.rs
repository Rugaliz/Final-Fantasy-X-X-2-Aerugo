//! Native-call framework: a registry of native implementations over a game state `G`, a coverage tracker, and the
//! groups that need no game state (math, debug output, frame waits).
//!
//! Natives Aerugo does not implement yet take their arguments off the stack (counts from the format's table), log
//! once, and return zero, so scenes can run before everything exists.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::f32::consts::{PI, TAU};

use aerugo_formats::Game;
use aerugo_formats::script::{self, Ret};

use crate::vm::{Host, NativeCall, Value, WAIT_CONTINUE, WAIT_DONE};

pub type InitFn<G> = fn(&mut G, &mut NativeCall<'_>);
pub type WaitFn<G> = fn(&mut G, &mut NativeCall<'_>) -> u32;
pub type ResultFn<G> = fn(&mut G, &mut NativeCall<'_>) -> Value;

/// One native's steps. A missing wait finishes at once and keeps the worker running; a missing result pushes 0.
pub struct Native<G> {
    pub init: Option<InitFn<G>>,
    pub wait: Option<WaitFn<G>>,
    pub result: Option<ResultFn<G>>,
}

impl<G> Clone for Native<G> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<G> Copy for Native<G> {}

impl<G> std::fmt::Debug for Native<G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Native")
            .field("init", &self.init.is_some())
            .field("wait", &self.wait.is_some())
            .finish()
    }
}

impl<G> Native<G> {
    /// A native that only computes a result.
    pub const fn result(f: ResultFn<G>) -> Native<G> {
        Native { init: None, wait: None, result: Some(f) }
    }

    /// A native that starts, then is polled every frame.
    pub const fn waiting(init: InitFn<G>, wait: WaitFn<G>) -> Native<G> {
        Native { init: Some(init), wait: Some(wait), result: None }
    }
}

/// What the original game's natives reported (from a recording), handed out in order to natives Aerugo does not
/// implement, so a comparison isolates the machine and the implemented natives.
#[derive(Debug, Default, Clone)]
pub struct Replay {
    /// Poll flags per (worker, native).
    pub polls: HashMap<(u16, u16), std::collections::VecDeque<u8>>,
    /// Results (tag 1 int, 2 float; raw bits) per (worker, native).
    pub results: HashMap<(u16, u16), std::collections::VecDeque<(u8, u32)>>,
}

/// Which natives scripts called, and which of those Aerugo lacks.
#[derive(Debug, Default, Clone)]
pub struct Coverage {
    pub calls: BTreeMap<u16, u64>,
    pub missing: BTreeSet<u16>,
    /// Stand-ins already reported.
    pub stand_ins: BTreeSet<u16>,
}

impl Coverage {
    /// A report: calls to implemented and missing natives, weighted by call count.
    pub fn report(&self, game: Game) -> String {
        let total: u64 = self.calls.values().sum();
        let missing: u64 =
            self.calls.iter().filter(|(id, _)| self.missing.contains(id)).map(|(_, n)| n).sum();
        let mut out = format!(
            "{} natives called, {} missing; {} of {} calls ({:.1}%) reached an implementation\n",
            self.calls.len(),
            self.missing.len(),
            total - missing,
            total,
            100.0 * (total - missing) as f64 / total.max(1) as f64
        );
        let mut top: Vec<_> = self.calls.iter().filter(|(id, _)| self.missing.contains(id)).collect();
        top.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (id, n) in top.into_iter().take(20) {
            out.push_str(&format!("  missing {:<32} {n} calls\n", script::native(game, *id).display(*id)));
        }
        out
    }
}

/// The natives of one game over a game state `G`.
pub struct Natives<G> {
    pub game: Game,
    table: HashMap<u16, Native<G>>,
    /// Natives registered only to take their arguments and give a fixed or kept result: the engine part behind
    /// them is not ported yet.
    stubs: std::collections::HashSet<u16>,
    /// Every registration in order, with the source line that made it (the last one for an id is the one used).
    sites: Vec<(u16, &'static std::panic::Location<'static>)>,
    pub coverage: Coverage,
    /// Recorded native behaviour used for natives without an implementation.
    pub replay: Option<Replay>,
}

impl<G> std::fmt::Debug for Natives<G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Natives").field("game", &self.game).field("implemented", &self.table.len()).finish()
    }
}

impl<G> Natives<G> {
    /// An empty registry with the state-free groups (math, debug, waits) registered.
    pub fn new(game: Game) -> Natives<G> {
        let mut n = Natives {
            game,
            table: HashMap::new(),
            stubs: Default::default(),
            sites: Vec::new(),
            coverage: Coverage::default(),
            replay: None,
        };
        register_common(&mut n);
        n
    }

    #[track_caller]
    pub fn register(&mut self, id: u16, native: Native<G>) {
        self.sites.push((id, std::panic::Location::caller()));
        self.table.insert(id, native);
        self.stubs.remove(&id);
    }

    /// Register a stand-in: it keeps the stack and the results right, but the engine part behind the native is
    /// not ported yet.
    #[track_caller]
    pub fn register_stub(&mut self, id: u16, native: Native<G>) {
        self.sites.push((id, std::panic::Location::caller()));
        self.table.insert(id, native);
        self.stubs.insert(id);
    }

    /// Every registration in order: the id and the source line that made it.
    pub fn registration_sites(
        &self,
    ) -> impl Iterator<Item = (u16, &'static std::panic::Location<'static>)> + '_ {
        self.sites.iter().copied()
    }

    /// Mark a registered native as a stand-in.
    pub fn mark_stub(&mut self, id: u16) {
        self.stubs.insert(id);
    }

    /// The native is registered only as a stand-in.
    pub fn is_stub(&self, id: u16) -> bool {
        self.stubs.contains(&id)
    }

    /// Drop a native (comparisons replay the original's results for natives whose timing is the player's, such
    /// as menus).
    pub fn remove(&mut self, id: u16) {
        self.table.remove(&id);
        self.stubs.remove(&id);
    }

    pub fn implemented(&self, id: u16) -> bool {
        self.table.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Bind to the game state for one frame of the machine.
    pub fn host<'a>(&'a mut self, game: &'a mut G) -> Bound<'a, G> {
        Bound { natives: self, game }
    }
}

/// [`Natives`] bound to a game state: the [`Host`] the machine calls.
pub struct Bound<'a, G> {
    natives: &'a mut Natives<G>,
    pub game: &'a mut G,
}

impl<G> std::fmt::Debug for Bound<'_, G> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bound").finish_non_exhaustive()
    }
}

impl<G: World> Host for Bound<'_, G> {
    fn native_init(&mut self, id: u16, call: &mut NativeCall<'_>) {
        *self.natives.coverage.calls.entry(id).or_default() += 1;
        match self.natives.table.get(&id).copied() {
            Some(n) => {
                // A stand-in is reported the first time it runs.
                if self.natives.stubs.contains(&id) && self.natives.coverage.stand_ins.insert(id) {
                    log::error!(
                        "GAP: native {} runs as a stand-in (its engine part is not ported)",
                        script::native(self.natives.game, id).display(id)
                    );
                }
                if let Some(init) = n.init {
                    init(self.game, call);
                }
            }
            None => {
                if self.natives.coverage.missing.insert(id) {
                    log::error!(
                        "GAP: native {} not implemented",
                        script::native(self.natives.game, id).display(id)
                    );
                }
                // Keep the stack balanced: take the arguments the native would have taken.
                if let Some((ints, floats, _)) = script::arity(self.natives.game, id) {
                    for _ in 0..u32::from(ints) + u32::from(floats) {
                        call.stack.pop();
                    }
                }
            }
        }
    }

    fn native_wait(&mut self, id: u16, call: &mut NativeCall<'_>) -> u32 {
        if !self.natives.table.contains_key(&id)
            && let Some(r) = &mut self.natives.replay
            && let Some(f) = r.polls.get_mut(&(call.worker as u16, id)).and_then(|q| q.pop_front())
        {
            return u32::from(f);
        }
        match self.natives.table.get(&id).and_then(|n| n.wait) {
            Some(wait) => wait(self.game, call),
            None => WAIT_DONE | WAIT_CONTINUE,
        }
    }

    fn native_result(&mut self, id: u16, call: &mut NativeCall<'_>) -> Value {
        if !self.natives.table.contains_key(&id)
            && let Some(r) = &mut self.natives.replay
            && let Some((tag, v)) = r.results.get_mut(&(call.worker as u16, id)).and_then(|q| q.pop_front())
        {
            return if tag == 2 { Value::Float(f32::from_bits(v)) } else { Value::Int(v as i32) };
        }
        match self.natives.table.get(&id).and_then(|n| n.result) {
            Some(f) => f(self.game, call),
            None if !self.natives.table.contains_key(&id) => {
                match script::arity(self.natives.game, id).map(|a| a.2) {
                    Some(Ret::Float) => Value::Float(0.0),
                    _ => Value::Int(0),
                }
            }
            None => Value::Int(0),
        }
    }

    fn party_worker(&mut self, slot: i32) -> Option<usize> {
        self.game.party_worker(slot)
    }

    fn worker_ran(&mut self, w: usize, kind: u8, level: u8) -> Vec<u8> {
        self.game.worker_ran(w, kind, level)
    }

    fn after_workers(&mut self) -> Vec<(usize, u8)> {
        self.game.after_workers()
    }

    fn start_signals(&mut self, w: usize, kind: u8, level: u8) -> Vec<u8> {
        self.game.start_signals(w, kind, level)
    }

    fn worker_will_run(&mut self, w: usize, kind: u8, level: u8) {
        self.game.worker_will_run(w, kind, level)
    }

    fn level_started(&mut self, w: usize, level: u8) {
        self.game.level_started(w, level)
    }

    fn may_run(&mut self, w: usize) -> bool {
        self.game.may_run(w)
    }
}

/// What the machine asks of the game state beyond natives.
pub trait World {
    /// The worker standing for party slot 0–2.
    fn party_worker(&mut self, _slot: i32) -> Option<usize> {
        None
    }
    /// Worker `w` (of `kind`) finished its turn this frame on script level `level`: test its trigger against the
    /// player and return the signal types it raises, in order.
    fn worker_ran(&mut self, _w: usize, _kind: u8, _level: u8) -> Vec<u8> {
        Vec::new()
    }
    /// After every worker ran: engine signals (worker, type) from the whole frame, such as the worker the player
    /// talks to and characters bumped into.
    fn after_workers(&mut self) -> Vec<(usize, u8)> {
        Vec::new()
    }
    /// The event starts: worker `w`'s trigger tested with the player standing still.
    fn start_signals(&mut self, _w: usize, _kind: u8, _level: u8) -> Vec<u8> {
        Vec::new()
    }
    /// Worker `w` is about to take its turn (main or setup pass).
    fn worker_will_run(&mut self, _w: usize, _kind: u8, _level: u8) {}
    /// A request started on a worker's level, or the level started over.
    fn level_started(&mut self, _w: usize, _level: u8) {}
    /// The engine's part of the worker run gate (atel_run_worker 8641e0) past the worker's on bit.
    fn may_run(&mut self, _w: usize) -> bool {
        true
    }
}

impl World for () {}

/// Wrap an angle into [−π, π].
/// Heading on the ground plane from (x1, z1) to (x2, z2), the engine's way (exact axes taken apart).
pub fn heading(x1: f32, z1: f32, x2: f32, z2: f32) -> f32 {
    let (dx, dz) = (x2 - x1, z2 - z1);
    if dx == 0.0 {
        if dz < 0.0 { -PI / 2.0 } else { PI / 2.0 }
    } else if dz == 0.0 {
        if dx < 0.0 { PI } else { -0.0 }
    } else {
        f64::from(dz).atan2(f64::from(dx)) as f32
    }
}

/// angle_wrap (88bc20): ±2π (floats) until within [−π, π]; NaN comes back as it is. An infinite angle never gets
/// there (the original loops for ever): reported and returned.
pub fn wrap_angle(mut a: f32) -> f32 {
    if a.is_infinite() {
        log::error!("GAP: angle_wrap of an infinite angle (the original never returns)");
        return a;
    }
    while a < -PI {
        a += TAU;
    }
    while a > PI {
        a -= TAU;
    }
    a
}

/// TkMtRsin (88c690): the quarter-wave table read by quadrant; a turn is 0x8000, the result in 1/32768.
fn rsin(a: i32) -> i32 {
    let t = &crate::rsin::RSIN;
    let u = (a & 0x7FFF) as usize;
    let q = |v: usize| i32::from(if v < 0x2000 { t[v] } else { t[0x3FFF - v] });
    if u < 0x4000 { q(u) } else { -q(u - 0x4000) }
}

/// The x87 truncation of a float (ftol: out of range gives 0x80000000).
fn ftol(v: f32) -> i32 {
    if (-2147483648.0..2147483648.0).contains(&v) { v as i32 } else { i32::MIN }
}

/// 88c040 / 88bfe0: angle_wrap(v), plus 2π when below 0, times `n` over 2π (in extended), truncated.
fn turn_units(v: f32, n: f64) -> i32 {
    let mut w = wrap_angle(v);
    if w < 0.0 {
        w = (f64::from(w) + f64::from(TAU)) as f32;
    }
    let x = f64::from(w) * n / f64::from(TAU);
    if (-2147483648.0..2147483648.0).contains(&x) { x as i32 } else { i32::MIN }
}

fn pop_f<const N: usize>(c: &mut NativeCall<'_>) -> [f32; N] {
    // Arguments come off the stack last first; return them in push order.
    let mut v = [0.0; N];
    for i in (0..N).rev() {
        v[i] = c.stack.pop_float();
    }
    v
}

fn register_common<G>(n: &mut Natives<G>) {
    // Frame wait: the count is kept in the context and counted down once per frame.
    n.register(
        0x0000,
        Native::waiting(
            |_, c| c.scratch[0] = c.stack.pop_int(),
            |_, c| {
                c.scratch[0] -= 1;
                u32::from(c.scratch[0] < 1)
            },
        ),
    );
    // The calling worker's kind.
    n.register(0x0076, Native::result(|_, c| Value::Int(i32::from(c.kind))));
    // Halt: never finishes.
    n.register(0x005F, Native { init: None, wait: Some(|_, _| 0), result: None });

    // Math (8779xx–878060): the x87 works in extended precision and the result is stored as a float, so the
    // transcendental ones go through f64 with one rounding.
    fn ext(f: fn(f64) -> f64, x: f32) -> f32 {
        f(f64::from(x)) as f32
    }
    n.register(0x1001, Native::result(|_, c| Value::Float(ext(f64::sin, c.stack.pop_float()))));
    n.register(0x1002, Native::result(|_, c| Value::Float(ext(f64::cos, c.stack.pop_float()))));
    n.register(0x1003, Native::result(|_, c| Value::Float(ext(f64::tan, c.stack.pop_float()))));
    n.register(0x1004, Native::result(|_, c| Value::Float(ext(f64::atan, c.stack.pop_float()))));
    n.register(
        0x1005,
        Native::result(|_, c| {
            let [y, x] = pop_f::<2>(c);
            Value::Float(f64::from(y).atan2(f64::from(x)) as f32)
        }),
    );
    n.register(0x1006, Native::result(|_, c| Value::Float(ext(f64::sqrt, c.stack.pop_float()))));
    // 1007: 1 / the float of the square root.
    n.register(
        0x1007,
        Native::result(|_, c| Value::Float((1.0 / f64::from(ext(f64::sqrt, c.stack.pop_float()))) as f32)),
    );
    // 1008 / 1009: TkMtRsin of the angle (a turn is 0x8000) / of a quarter turn on: sine / cosine in 1/32768.
    n.register(0x1008, Native::result(|_, c| Value::Int(rsin(c.stack.pop_int()))));
    n.register(0x1009, Native::result(|_, c| Value::Int(rsin(c.stack.pop_int().wrapping_add(0x2000)))));
    n.register(0x100A, Native::result(|_, c| Value::Int(c.stack.pop_int())));
    n.register(0x100B, Native::result(|_, c| Value::Int(c.stack.pop_int().wrapping_shl(15))));
    n.register(0x100C, Native::result(|_, c| Value::Int(ftol(c.stack.pop_float() * 32768.0))));
    // 100d / 100e (88c040 / 88bfe0): the wrapped angle (2π added when below 0) as 1/32768 or 1/4096 of a turn.
    n.register(0x100D, Native::result(|_, c| Value::Int(turn_units(c.stack.pop_float(), 32768.0) & 0x7FFF)));
    n.register(0x100E, Native::result(|_, c| Value::Int(turn_units(c.stack.pop_float(), 4096.0) & 0xFFF)));
    n.register(
        0x100F,
        Native::result(|_, c| Value::Float(wrap_angle((c.stack.pop_int() & 0x7FFF) as f32 * TAU / 32768.0))),
    );
    n.register(
        0x1010,
        Native::result(|_, c| Value::Float(wrap_angle((c.stack.pop_int() & 0xFFF) as f32 * TAU / 4096.0))),
    );
    // Heading from (x1, z1) to (x2, z2).
    n.register(
        0x1011,
        Native::result(|_, c| {
            let [x1, z1, x2, z2] = pop_f::<4>(c);
            Value::Float(heading(x1, z1, x2, z2))
        }),
    );
    n.register(0x1013, Native::result(|_, c| Value::Float(wrap_angle(c.stack.pop_float()))));
    // 1014(a, b, step) (88b570): a turned toward b by at most step: d = wrap(b − a); when |d| reaches step,
    // a ± step (+ for d ≥ 0), else b; wrapped.
    n.register(
        0x1014,
        Native::result(|_, c| {
            let [a, b, step] = pop_f::<3>(c);
            let d = wrap_angle(b - a);
            let s = if d.abs() < step { d.abs() } else { step };
            let r = if step == s { if d >= 0.0 { a + s } else { a - s } } else { b };
            Value::Float(wrap_angle(r))
        }),
    );
    n.register(0x1019, Native::result(|_, c| Value::Float(c.stack.pop_float().abs())));
    // 101a / 101b: the distance on x/z / in 3D; the differences are floats, their squares summed in extended.
    n.register(
        0x101A,
        Native::result(|_, c| {
            let [x1, z1, x2, z2] = pop_f::<4>(c);
            let (dx, dz) = (f64::from(x1 - x2), f64::from(z1 - z2));
            Value::Float((dx * dx + dz * dz).sqrt() as f32)
        }),
    );
    n.register(
        0x101B,
        Native::result(|_, c| {
            let [x1, y1, z1, x2, y2, z2] = pop_f::<6>(c);
            let (dx, dy, dz) = (f64::from(x1 - x2), f64::from(y1 - y2), f64::from(z1 - z2));
            Value::Float((dx * dx + dy * dy + dz * dz).sqrt() as f32)
        }),
    );

    // Debug output.
    n.register(
        0x9000,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            log::debug!("script worker {} prints {v}", c.worker);
            Value::Int(0)
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles() {
        assert!((wrap_angle(3.0 * PI) - PI).abs() < 1e-5);
        assert_eq!(rsin(0x2000), 32768);
        assert_eq!(rsin(0), 0);
        assert_eq!(rsin(0x6000), -32768);
    }
}
