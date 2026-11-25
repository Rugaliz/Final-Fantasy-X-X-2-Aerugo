//! The script machine: workers with prioritized levels, a shared operand stack per worker, requests between
//! workers, and native calls that may span frames. See `docs/script-vm.md`.

use std::sync::Arc;

use aerugo_formats::script::{Script, VarType};

use crate::memory::{Memory, encode_address};

/// Most instructions one worker may run in one frame before it is stopped as stuck.
const RUN_LIMIT: u32 = 0x1_0000;
const STACK_SLOTS: usize = 19;
/// Level the setup entry runs at.
const INIT_LEVEL: u8 = 7;
const CALL_DEPTH: usize = 3;
/// Native wait result bits.
pub const WAIT_DONE: u32 = 1;
pub const WAIT_CONTINUE: u32 = 4;

/// `!(l >= r)`: true when unordered (NaN), as in the original's less-than.
fn not_ge(l: f32, r: f32) -> bool {
    !matches!(l.partial_cmp(&r), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal))
}

/// `!(l > r)`: true when unordered.
fn not_gt(l: f32, r: f32) -> bool {
    !matches!(l.partial_cmp(&r), Some(std::cmp::Ordering::Greater))
}

/// A stack slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Int(i32),
    Float(f32),
}

impl Value {
    pub fn int(self) -> i32 {
        match self {
            Value::Int(v) => v,
            Value::Float(f) => f as i32,
        }
    }

    pub fn float(self) -> f32 {
        match self {
            Value::Int(v) => v as f32,
            Value::Float(f) => f,
        }
    }
}

/// Something went wrong in a script (the original sets an error flag and carries on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    StackUnderflow,
    StackOverflow,
    CallDepth,
    InvalidOpcode(u8),
    PcOutOfRange,
    DivideByZero,
    RunLimit,
}

/// A worker's operand stack (19 tagged slots).
#[derive(Debug, Clone, Default)]
pub struct Stack {
    slots: Vec<Value>,
    pub faults: Vec<Fault>,
}

impl Stack {
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn push(&mut self, v: Value) {
        if self.slots.len() < STACK_SLOTS {
            self.slots.push(v);
        } else {
            self.faults.push(Fault::StackOverflow);
        }
    }

    pub fn pop(&mut self) -> Value {
        self.slots.pop().unwrap_or_else(|| {
            self.faults.push(Fault::StackUnderflow);
            Value::Int(0)
        })
    }

    pub fn pop_int(&mut self) -> i32 {
        self.pop().int()
    }

    pub fn pop_float(&mut self) -> f32 {
        self.pop().float()
    }

    pub fn peek(&self) -> Option<Value> {
        self.slots.last().copied()
    }

    fn top_two_int(&self) -> bool {
        let n = self.slots.len();
        n >= 2 && matches!((self.slots[n - 1], self.slots[n - 2]), (Value::Int(_), Value::Int(_)))
    }
}

/// What a context is waiting for before it runs again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wait {
    #[default]
    None,
    /// A done signal from `target` for `entry` at `level`.
    Signal { target: u16, entry: u16, level: u8 },
    /// Until `target` no longer has a request for `entry`.
    Idle { target: u16, entry: u16 },
}

/// Execution state of one priority level.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// Offset from the start of the code.
    pub pc: u32,
    returns: [u32; CALL_DEPTH],
    callees: [u16; CALL_DEPTH],
    depth: usize,
    /// A native call has started and is being polled.
    calling: bool,
    pub wait: Wait,
    /// Scratch space a native keeps between its init and wait steps (frame counters and the like).
    pub scratch: [i32; 4],
    pub a: i32,
    pub x: i32,
    pub f: f32,
}

/// A request queued on a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    id: u32,
    /// Code from another script to run the entry in (a battle control's exec space: script, descriptor); the
    /// worker takes that script and descriptor when the request starts.
    pub code: Option<(u16, u16)>,
    /// Requesting worker (`None` for requests from the engine).
    pub from: Option<u16>,
    pub mode: RequestMode,
    pub level: u8,
    pub entry: u16,
    pub running: bool,
    /// Queued this frame.
    fresh: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestMode {
    /// Fire and forget.
    Plain,
    /// The requester waits until the entry starts.
    WaitStart,
    /// The requester waits until the entry ends.
    WaitEnd,
    /// A done signal sent back to a waiting requester.
    Signal,
}

/// One script entity.
#[derive(Debug, Clone)]
pub struct Worker {
    pub index: usize,
    /// The group's script the worker comes from, and its descriptor there.
    pub script: usize,
    pub descriptor: usize,
    pub kind: u8,
    pub active: bool,
    /// Runs at all (scripts turn other workers off and on); a worker turned off still takes requests.
    pub enabled: bool,
    /// Takes requests (set once the event is set up).
    pub accepts: bool,
    /// An entry has ended at least once (the init phase waits for this).
    pub ended_entry: bool,
    /// Entry being run.
    pub entry: u16,
    pub level: u8,
    pub contexts: Vec<Context>,
    pub stack: Stack,
    pub queue: Vec<Request>,
    active_request: Option<u32>,
    pub rate: f32,
    rate_acc: f32,
    /// Extra-pass marks: a two-bit count of extra passes this frame (bits 7–8), marks that become a count at
    /// the end of the pass (bits 9–10), and "requests from this worker mark their target" (bit 12).
    pub pass_marks: u16,
    /// The entries engine signals run, when a script changed them (0x79): one per signal type.
    pub signal_entries: Option<[i16; 8]>,
}

impl Worker {
    fn max_level(&self) -> u8 {
        if matches!(self.kind, 5 | 6) { 1 } else { 8 }
    }

    fn level_index(&self, level: u8) -> usize {
        usize::from(level.min(self.max_level()))
    }

    pub fn context(&self) -> &Context {
        &self.contexts[self.level_index(self.level)]
    }

    /// The running request, if any.
    pub fn active_request(&self) -> Option<&Request> {
        self.active_request.and_then(|id| self.queue.iter().find(|r| r.id == id))
    }
}

/// One traced instruction (for comparing runs with recordings of the original game).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceEvent {
    pub frame: u64,
    pub worker: u16,
    pub level: u8,
    pub pc: u32,
    pub op: u8,
    pub operand: Option<u16>,
}

/// What a native call can see and change.
pub struct NativeCall<'a> {
    pub worker: usize,
    /// The calling worker's kind.
    pub kind: u8,
    pub level: u8,
    pub scratch: &'a mut [i32; 4],
    pub stack: &'a mut Stack,
    pub memory: &'a mut Memory,
    pub frame: u64,
    /// Requests to the machine, applied when the call returns.
    pub effects: &'a mut Vec<Effect>,
    /// Per worker, the levels it has requests at (bit n: a non-signal request at level n).
    pub requests: &'a [u16],
}

impl std::fmt::Debug for NativeCall<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeCall").field("worker", &self.worker).field("level", &self.level).finish()
    }
}

/// The engine side of the machine: natives and game lookups.
pub trait Host {
    /// A native call starts (it pops its arguments here).
    fn native_init(&mut self, id: u16, call: &mut NativeCall<'_>);
    /// Poll a started call: [`WAIT_DONE`] when finished, [`WAIT_CONTINUE`] to keep running the worker this frame.
    fn native_wait(&mut self, id: u16, call: &mut NativeCall<'_>) -> u32;
    /// The value the finished call leaves on the stack.
    fn native_result(&mut self, id: u16, call: &mut NativeCall<'_>) -> Value;
    /// The worker standing for a party slot (0–2), for the party request instructions.
    fn party_worker(&mut self, _slot: i32) -> Option<usize> {
        None
    }
    /// Worker `w` finished its turn this frame: the engine signal types its trigger raises.
    fn worker_ran(&mut self, _w: usize, _kind: u8, _level: u8) -> Vec<u8> {
        Vec::new()
    }
    /// After every worker ran: engine signals (worker, type) from the whole frame (talking, bumping).
    fn after_workers(&mut self) -> Vec<(usize, u8)> {
        Vec::new()
    }
    /// The event starts (all init entries ended): the trigger signals of every worker with the player standing
    /// where it is, before the first requests start.
    fn start_signals(&mut self, _w: usize, _kind: u8, _level: u8) -> Vec<u8> {
        Vec::new()
    }
    /// Worker `w` is about to take its turn in the frame's main or setup pass (it takes its body's place).
    fn worker_will_run(&mut self, _w: usize, _kind: u8, _level: u8) {}
    /// A request started on level `level` of worker `w`, or the level started over.
    fn level_started(&mut self, _w: usize, _level: u8) {}
    /// The engine's part of the run gate at the top of atel_run_worker, past the worker's on bit.
    fn may_run(&mut self, _w: usize) -> bool {
        true
    }
}

/// Engine signal defaults per worker kind and signal type: the level, and the entry (255 / -1: none).
const SIGNAL_LEVELS: [[u8; 8]; 3] =
    [[7, 7, 6, 5, 255, 255, 255, 255], [7, 7, 255, 3, 5, 4, 2, 255], [1, 1, 255, 1, 1, 1, 1, 255]];
const SIGNAL_ENTRIES: [[i16; 8]; 4] = [
    [2, 3, 4, 5, -1, -1, -1, 1],
    [2, 3, -1, 5, 4, 6, 7, 1],
    [2, 2, -1, 2, 2, 2, 2, 1],
    [2, 2, -1, 2, 2, 2, 3, 1],
];

/// Phase of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Every worker runs its entry 0 until it ends.
    Init,
    Running,
}

/// A running script (one event), or a group of scripts run as one (a battle's control: workers numbered
/// across the scripts, every script's actors first).
#[derive(Debug, Clone)]
pub struct Vm {
    /// The group's first script (the event's).
    pub script: Arc<Script>,
    /// Every script of the group (the first is [`Vm::script`]).
    pub scripts: Vec<Arc<Script>>,
    /// Each worker's own script and descriptor (where it comes from; a request may run it in other code).
    order: Vec<(usize, usize)>,
    /// The first worker of each control (one control outside battles); a worker's requests and calls number
    /// the workers of its own control.
    controls: Vec<usize>,
    pub workers: Vec<Worker>,
    pub memory: Memory,
    pub phase: Phase,
    pub frame: u64,
    next_request: u32,
    /// The instruction just run ended the worker's turn (an END).
    turn_over: bool,
    /// Trace of executed instructions, when enabled.
    pub trace: Option<Vec<TraceEvent>>,
    /// Faults with the worker and pc they happened at.
    pub faults: Vec<(usize, u32, Fault)>,
    effects: Vec<Effect>,
    /// Levels a request started on or that started over (worker, level), not yet told to the host.
    level_starts: Vec<(usize, u8)>,
    /// The frame's main (or setup) pass is running, not an extra pass.
    main_pass: bool,
}

/// When a request is refused: the plain request opcodes refuse a second request at the same level; variants
/// refuse one for an entry already asked for, or never.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refuse {
    SameLevel,
    SameEntry,
    Never,
}

/// What a native asks of the machine itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Run this worker again in an extra pass after this one, and let its requests mark their targets the same
    /// way.
    ExtraPass(usize),
    /// Mark a worker to run again at the end of this frame (flags2 0x200).
    PassAgain(usize),
    /// Turn a worker on or off.
    Enable(usize, bool),
    /// The requests above `level` that have not started are dropped (a
    /// requester waiting for the start is answered first); running ones stay.
    SkipQueued(usize, u8),
}

impl Vm {
    /// Set up every worker of `script` at entry 0, level 0.
    pub fn new(script: Arc<Script>) -> Vm {
        Vm::group(vec![script], false)
    }

    /// Set up a group of scripts as one machine (AtelBattleSetUp): every script's actors in turn, then every
    /// script's other workers; with `own_work` each worker gets a zeroed work area of its descriptor's size.
    pub fn group(scripts: Vec<Arc<Script>>, own_work: bool) -> Vm {
        Vm::controls(vec![scripts], own_work)
    }

    /// Several controls in one machine (a battle's: the battle group and the monster group), their scripts in
    /// one list so a request can run any of them; each control numbers its own workers (every script's actors
    /// first, then the rest).
    pub fn controls(groups: Vec<Vec<Arc<Script>>>, own_work: bool) -> Vm {
        let mut order = Vec::new();
        let mut scripts = Vec::new();
        let mut controls = Vec::new();
        for group in groups {
            controls.push(order.len());
            let first = scripts.len();
            for (k, sc) in group.iter().enumerate() {
                order.extend((0..sc.actor_count()).map(|d| (first + k, d)));
            }
            for (k, sc) in group.iter().enumerate() {
                order.extend((sc.actor_count()..sc.workers.len()).map(|d| (first + k, d)));
            }
            scripts.extend(group);
        }
        let n = order.len();
        let workers = order
            .iter()
            .enumerate()
            .map(|(i, &(si, d))| {
                let w = &scripts[si].workers[d];
                let mut contexts = vec![Context::default(); 9];
                // Entry 0 (setup) runs at level 7 (the highest a two-level worker has is 1); level 0 holds entry 1,
                // which runs once setup ends.
                let start = if matches!(w.kind, 5 | 6) { 1 } else { INIT_LEVEL };
                contexts[usize::from(start)].pc = w.entries.first().copied().unwrap_or(0);
                contexts[0].pc = w.entries.get(1).copied().unwrap_or(0);
                // Kind 0 workers hold subroutines other workers call into; they never run on their own.
                let runs = w.kind != 0;
                Worker {
                    index: i,
                    script: si,
                    descriptor: d,
                    kind: w.kind,
                    active: runs,
                    enabled: true,
                    accepts: runs,
                    ended_entry: !runs,
                    entry: 0,
                    level: start,
                    contexts,
                    stack: Stack::default(),
                    queue: Vec::new(),
                    active_request: None,
                    rate: 1.0,
                    rate_acc: 0.0,
                    pass_marks: 0,
                    signal_entries: None,
                }
            })
            .collect();
        let works = own_work.then(|| {
            order.iter().map(|&(si, d)| scripts[si].workers[d].work_size as usize).collect::<Vec<_>>()
        });
        let memory = Memory::group(scripts.iter().map(|sc| sc.data().to_vec()).collect(), n, works);
        let script = scripts[0].clone();
        let order_kept = order.clone();
        let mut vm = Vm {
            script,
            scripts,
            order: order_kept,
            controls,
            workers,
            memory,
            phase: Phase::Init,
            frame: 0,
            next_request: 1,
            turn_over: false,
            trace: None,
            faults: Vec::new(),
            effects: Vec::new(),
            level_starts: Vec::new(),
            main_pass: false,
        };
        // Each runnable worker starts with its setup entry running and entry 1 (its main code) queued at level
        // 0; ending the setup entry starts entry 1.
        for w in 0..vm.workers.len() {
            if !vm.workers[w].active {
                continue;
            }
            let level = vm.workers[w].level;
            let id = vm.next_request;
            vm.next_request += 1;
            vm.workers[w].queue.push(Request {
                id,
                code: None,
                from: None,
                mode: RequestMode::Plain,
                level,
                entry: 0,
                running: true,
                fresh: false,
            });
            vm.workers[w].active_request = Some(id);
            if vm.descriptor(w).entries.len() > 1 {
                let id = vm.next_request;
                vm.next_request += 1;
                vm.workers[w].queue.push(Request {
                    id,
                    code: None,
                    from: None,
                    mode: RequestMode::Plain,
                    level: 0,
                    entry: 1,
                    running: false,
                    fresh: false,
                });
            }
        }
        vm
    }

    /// The descriptor of the code a worker runs now.
    pub fn descriptor(&self, w: usize) -> &aerugo_formats::script::Worker {
        let wk = &self.workers[w];
        &self.scripts[wk.script].workers[wk.descriptor]
    }

    /// The first worker of the control `w` belongs to.
    pub fn control_base(&self, w: usize) -> usize {
        self.controls.iter().copied().take_while(|&c| c <= w).last().unwrap_or(0)
    }

    /// Queue a request whose entry runs in other code (a battle exec space: `code` = script, descriptor).
    pub fn request_in(&mut self, target: usize, level: u8, entry: u16, code: (usize, usize)) -> bool {
        if !self.request(None, target, RequestMode::Plain, level, entry) {
            return false;
        }
        if let Some(r) = self.workers[target].queue.iter_mut().find(|r| r.fresh && r.entry == entry) {
            r.code = Some((code.0 as u16, code.1 as u16));
        }
        true
    }

    /// Arrive by `entrance`: the script's entrance worker is asked (at level 1) to run the entry the entrance
    /// names. Call right after creating the machine, before the first frame.
    pub fn enter(&mut self, entrance: usize) -> bool {
        match self.script.entrance_entry(entrance) {
            Some((worker, entry)) => self.request(None, usize::from(worker), RequestMode::Plain, 1, entry),
            None => false,
        }
    }

    /// Run one frame (1/30 s of game time).
    pub fn frame(&mut self, host: &mut dyn Host) {
        match self.phase {
            Phase::Init => {
                self.main_pass = true;
                for w in 0..self.workers.len() {
                    if !self.workers[w].ended_entry {
                        self.run_worker(w, host);
                        // The engine steps the worker after its setup turn too.
                        if self.workers[w].active && self.workers[w].enabled {
                            for t in host.worker_ran(w, self.workers[w].kind, self.workers[w].level) {
                                self.engine_signal(w, t);
                            }
                        }
                    }
                }
                self.main_pass = false;
                if self.workers.iter().all(|w| w.ended_entry || !w.active) {
                    for w in 0..self.workers.len() {
                        if self.workers[w].active && self.workers[w].enabled {
                            for t in host.start_signals(w, self.workers[w].kind, self.workers[w].level) {
                                self.engine_signal(w, t);
                            }
                        }
                    }
                    for w in 0..self.workers.len() {
                        self.decode(w, None);
                    }
                    self.tell_level_starts(host);
                    self.phase = Phase::Running;
                }
            }
            Phase::Running => {
                self.main_pass = true;
                for w in 0..self.workers.len() {
                    self.run_worker(w, host);
                    if self.workers[w].active && self.workers[w].enabled {
                        for t in host.worker_ran(w, self.workers[w].kind, self.workers[w].level) {
                            self.engine_signal(w, t);
                        }
                    }
                }
                self.main_pass = false;
                for (w, t) in host.after_workers() {
                    self.engine_signal(w, t);
                }
                for w in 0..self.workers.len() {
                    self.decode(w, None);
                    let m = &mut self.workers[w].pass_marks;
                    *m = ((*m >> 2) & 0x180 | *m & 0xF87F) & !0x1000;
                }
                self.tell_level_starts(host);
                // Extra passes: marked workers run again in the same frame (a worker asked by one that is marked
                // starts its new entry now rather than next frame).
                for _ in 0..4 {
                    if !self.workers.iter().any(|w| w.pass_marks & 0x180 != 0) {
                        break;
                    }
                    for w in 0..self.workers.len() {
                        if self.workers[w].pass_marks & 0x180 != 0 {
                            self.run_worker(w, host);
                        }
                    }
                    for w in 0..self.workers.len() {
                        let m = self.workers[w].pass_marks;
                        let m = if m & 0x600 != 0 { (m >> 2) & 0x180 | m & 0xF87F } else { m };
                        self.workers[w].pass_marks = m;
                        if m & 0x180 != 0 {
                            self.decode(w, None);
                            self.tell_level_starts(host);
                            let count = ((m >> 7) & 3).saturating_sub(1);
                            self.workers[w].pass_marks = m & !0x180 | count << 7;
                        }
                    }
                }
            }
        }
        self.frame += 1;
    }

    /// An engine signal (touch, talk, a line crossed, a zone entered or left) on worker `w`: a request for the
    /// entry the worker's kind runs for that signal type, at its level. Gating by the worker's signal bits is
    /// the caller's.
    pub fn engine_signal(&mut self, w: usize, signal: u8) -> bool {
        let Some(kind) = self.workers.get(w).map(|x| x.kind) else { return false };
        let (levels, entries) = match kind {
            1 => (0, 0),
            2 | 3 => (1, 1),
            5 => (2, 2),
            6 => (2, 3),
            _ => return false,
        };
        let t = usize::from(signal & 7);
        let entry = match self.workers[w].signal_entries {
            Some(table) => table[t],
            None => SIGNAL_ENTRIES[entries][t],
        };
        let level = SIGNAL_LEVELS[levels][t];
        if level == 255 || entry < 0 {
            return false;
        }
        self.request(None, w, RequestMode::Plain, level, entry as u16)
    }

    /// Queue a request on `target` from `from`. Refused (false) for entry 0, unknown targets, or when the target
    /// already has a request at that level.
    pub fn request(
        &mut self,
        from: Option<u16>,
        target: usize,
        mode: RequestMode,
        level: u8,
        entry: u16,
    ) -> bool {
        self.request_with(from, target, mode, level, entry, Refuse::SameLevel)
    }

    /// [`Vm::request`] with the request variant's refusal rule.
    pub fn request_with(
        &mut self,
        from: Option<u16>,
        target: usize,
        mode: RequestMode,
        level: u8,
        entry: u16,
        refuse: Refuse,
    ) -> bool {
        if entry == 0 || target >= self.workers.len() {
            return false;
        }
        // A worker marked for an extra pass marks the workers it asks too.
        if from.and_then(|f| self.workers.get(usize::from(f))).is_some_and(|f| f.pass_marks & 0x1000 != 0) {
            self.workers[target].pass_marks |= 0x200;
        }
        let t = &mut self.workers[target];
        let level = level.min(t.max_level());
        let taken = |r: &Request| match refuse {
            Refuse::SameLevel => r.level == level,
            Refuse::SameEntry => r.entry == entry,
            Refuse::Never => false,
        };
        if t.queue.iter().any(|r| r.mode != RequestMode::Signal && taken(r)) {
            return false;
        }
        let id = self.next_request;
        self.next_request += 1;
        let r = Request { id, code: None, from, mode, level, entry, running: false, fresh: true };
        let at = t
            .queue
            .iter()
            .position(|q| q.mode == RequestMode::Signal || level > q.level)
            .unwrap_or(t.queue.len());
        t.queue.insert(at, r);
        true
    }

    fn signal(&mut self, from: usize, to: Option<u16>, level: u8, entry: u16) {
        let Some(to) = to.map(usize::from).filter(|&t| t < self.workers.len()) else { return };
        let id = self.next_request;
        self.next_request += 1;
        self.workers[to].queue.push(Request {
            id,
            code: None,
            from: Some(from as u16),
            mode: RequestMode::Signal,
            level,
            entry,
            running: false,
            fresh: true,
        });
    }

    /// Start or resume the head request if it outranks the current level (`any`: any level).
    fn decode(&mut self, w: usize, any: Option<()>) {
        let worker = &self.workers[w];
        if !worker.active || !worker.accepts {
            return;
        }
        let threshold = if any.is_some() { -1 } else { i32::from(worker.level) };
        if let Some(head) = worker.queue.first().copied()
            && head.mode != RequestMode::Signal
            && threshold < i32::from(head.level)
        {
            if head.running {
                self.workers[w].level = head.level;
            } else {
                if let Some((sc, d)) = head.code {
                    self.workers[w].script = usize::from(sc);
                    self.workers[w].descriptor = usize::from(d);
                }
                let pc = self.descriptor(w).entries.get(usize::from(head.entry)).copied().unwrap_or(0);
                self.level_starts.push((w, head.level));
                let worker = &mut self.workers[w];
                worker.entry = head.entry;
                worker.level = head.level;
                let li = worker.level_index(head.level);
                let ctx = &mut worker.contexts[li];
                ctx.depth = 0;
                ctx.calling = false;
                ctx.pc = pc;
                if let Some(r) = worker.queue.first_mut() {
                    r.running = true;
                }
                if head.mode == RequestMode::WaitStart {
                    self.signal(w, head.from, head.level, head.entry);
                }
            }
            self.workers[w].active_request = Some(head.id);
        }
        for r in &mut self.workers[w].queue {
            r.fresh = false;
        }
    }

    /// The running entry ends: answer a waiting requester, cancel requests above `cancel_above`, take the next
    /// request; with `restart` the level starts over at entry 1.
    fn end_entry(&mut self, w: usize, cancel_above: Option<u8>, restart: bool) {
        if let Some(id) = self.workers[w].active_request.take()
            && let Some(at) = self.workers[w].queue.iter().position(|r| r.id == id)
        {
            let r = self.workers[w].queue.remove(at);
            if r.mode == RequestMode::WaitEnd {
                self.signal(w, r.from, r.level, r.entry);
            }
        }
        if let Some(n) = cancel_above {
            let cancelled: Vec<Request> = self.workers[w]
                .queue
                .iter()
                .filter(|r| r.mode != RequestMode::Signal && r.level > n && !r.fresh)
                .copied()
                .collect();
            self.workers[w].queue.retain(|r| !cancelled.iter().any(|c| c.id == r.id));
            for r in cancelled {
                if r.mode == RequestMode::WaitEnd || (r.mode == RequestMode::WaitStart && !r.running) {
                    self.signal(w, r.from, r.level, r.entry);
                }
            }
        }
        // The next request takes over (the main entry's, when it is still running); with none, the level stays and
        // the worker re-runs the END every frame.
        self.decode(w, Some(()));
        if restart {
            let pc = self.descriptor(w).entries.get(1).copied().unwrap_or(0);
            let level = self.workers[w].level;
            self.level_starts.push((w, level));
            let worker = &mut self.workers[w];
            let li = worker.level_index(worker.level);
            worker.contexts[li].depth = 0;
            worker.contexts[li].pc = pc;
        }
        self.workers[w].ended_entry = true;
    }

    /// Tell the host the levels that started since it last heard.
    fn tell_level_starts(&mut self, host: &mut dyn Host) {
        for (w, level) in std::mem::take(&mut self.level_starts) {
            host.level_started(w, level);
        }
    }

    fn fault(&mut self, w: usize, f: Fault) {
        let pc = self.workers[w].context().pc;
        if self.faults.len() < 1000 {
            self.faults.push((w, pc, f));
        }
    }

    /// Run worker `w` for this frame.
    pub fn run_worker(&mut self, w: usize, host: &mut dyn Host) {
        if !self.workers[w].active || !self.workers[w].enabled || !host.may_run(w) {
            return;
        }
        self.tell_level_starts(host);
        if self.main_pass {
            host.worker_will_run(w, self.workers[w].kind, self.workers[w].level);
        }
        // Speed: a rate below 1 skips frames.
        let worker = &mut self.workers[w];
        let step = worker.rate;
        worker.rate_acc += step;
        let mut run = step >= 1.0;
        if run {
            worker.rate_acc -= 1.0;
        }
        // Waits.
        let li = worker.level_index(worker.level);
        match worker.contexts[li].wait {
            Wait::None => {}
            Wait::Signal { target, entry, level } => {
                let found = worker.queue.iter().position(|r| {
                    r.mode == RequestMode::Signal
                        && r.from == Some(target)
                        && r.entry == entry
                        && r.level == level
                });
                match found {
                    Some(at) => {
                        worker.queue.remove(at);
                        worker.contexts[li].wait = Wait::None;
                    }
                    None => return,
                }
            }
            Wait::Idle { target, entry } => {
                let busy = self.workers.get(usize::from(target)).is_some_and(|t| {
                    t.queue.iter().any(|r| r.mode != RequestMode::Signal && r.entry == entry)
                });
                if busy {
                    return;
                }
                let worker = &mut self.workers[w];
                worker.contexts[li].wait = Wait::None;
            }
        }
        let mut count = 0u32;
        loop {
            count += 1;
            if count > RUN_LIMIT {
                self.fault(w, Fault::RunLimit);
                return;
            }
            if !run && self.workers[w].stack.is_empty() {
                return;
            }
            match self.step(w, host) {
                // An END finishes the worker's turn, whatever the stack still holds.
                Ok(_) if std::mem::take(&mut self.turn_over) => return,
                Ok(keep) => run = keep.unwrap_or(run),
                Err(f) => {
                    self.fault(w, f);
                    return;
                }
            }
            let faults = std::mem::take(&mut self.workers[w].stack.faults);
            for f in faults {
                self.fault(w, f);
            }
        }
    }

    /// Execute one instruction. `Some(run)` replaces the keep-running flag.
    fn step(&mut self, w: usize, host: &mut dyn Host) -> Result<Option<bool>, Fault> {
        let si = self.workers[w].script;
        let script = self.scripts[si].clone();
        let desc = &script.workers[self.workers[w].descriptor];
        let li = self.workers[w].level_index(self.workers[w].level);
        let pc = self.workers[w].contexts[li].pc;
        let insn = script.instruction(pc).ok_or(Fault::PcOutOfRange)?;
        if let Some(t) = &mut self.trace {
            t.push(TraceEvent {
                frame: self.frame,
                worker: w as u16,
                level: self.workers[w].level,
                pc,
                op: insn.op,
                operand: insn.operand,
            });
        }
        let arg = insn.operand.unwrap_or(0);
        let next = pc + insn.len();
        let level = self.workers[w].level;
        let requests: Vec<u16> = if matches!(insn.op, 0x35 | 0x58) {
            self.workers
                .iter()
                .map(|wk| {
                    wk.queue
                        .iter()
                        .filter(|r| r.mode != RequestMode::Signal)
                        .fold(0u16, |m, r| m | 1 << r.level.min(15))
                })
                .collect()
        } else {
            Vec::new()
        };
        let base = self.control_base(w);
        let worker = &mut self.workers[w];
        let s = &mut worker.stack;
        // Binary helpers: right operand on top.
        macro_rules! arith {
            ($i:expr, $f:expr) => {{
                if s.top_two_int() {
                    let r = s.pop_int();
                    let l = s.pop_int();
                    s.push(Value::Int($i(l, r)));
                } else {
                    let r = s.pop_float();
                    let l = s.pop_float();
                    s.push(Value::Float($f(l, r)));
                }
            }};
        }
        macro_rules! compare {
            ($i:expr, $f:expr) => {{
                let v = if s.top_two_int() {
                    let r = s.pop_int();
                    let l = s.pop_int();
                    $i(l, r)
                } else {
                    let r = s.pop_float();
                    let l = s.pop_float();
                    $f(l, r)
                };
                s.push(Value::Int(i32::from(v)));
            }};
        }
        macro_rules! int2 {
            ($e:expr) => {{
                let r = s.pop_int();
                let l = s.pop_int();
                s.push(Value::Int($e(l, r)));
            }};
        }
        let mut run = None;
        let mut jump: Option<u32> = None;
        match insn.op {
            0x00 | 0x1D | 0x1E | 0x76 => {}
            0x01 => int2!(|l: i32, r: i32| i32::from(l != 0 || r != 0)),
            0x02 => int2!(|l: i32, r: i32| i32::from(l != 0 && r != 0)),
            0x03 => int2!(|l: i32, r: i32| l | r),
            0x04 => int2!(|l: i32, r: i32| l ^ r),
            0x05 => int2!(|l: i32, r: i32| l & r),
            0x06 => compare!(|l, r| l == r, |l: f32, r: f32| l == r),
            0x07 => compare!(|l, r| l != r, |l: f32, r: f32| l != r),
            // Pairs share one handler in the original, the second negating the first: 08/0D, 0A/0F (gt, le) and
            // 0C/09, 0E/0B (ge, lt).
            0x08 => compare!(|l: i32, r: i32| (l as u32) > (r as u32), |l: f32, r: f32| l > r),
            0x09 => compare!(|l: i32, r: i32| (l as u32) < (r as u32), not_ge),
            0x0A => compare!(|l, r| l > r, |l: f32, r: f32| l > r),
            0x0B => compare!(|l, r| l < r, not_ge),
            0x0C => compare!(|l: i32, r: i32| (l as u32) >= (r as u32), |l: f32, r: f32| l >= r),
            0x0D => compare!(|l: i32, r: i32| (l as u32) <= (r as u32), not_gt),
            0x0E => compare!(|l, r| l >= r, |l: f32, r: f32| l >= r),
            0x0F => compare!(|l, r| l <= r, not_gt),
            0x10 => int2!(|l: i32, r: i32| (l >> (r & 31)) & 1),
            0x11 => int2!(|l: i32, r: i32| i32::from((l >> (r & 31)) & 1 == 0)),
            0x12 => int2!(|l: i32, r: i32| l.wrapping_shl(r as u32 & 31)),
            0x13 => int2!(|l: i32, r: i32| l >> (r & 31)),
            0x14 => arith!(|l: i32, r: i32| l.wrapping_add(r), |l, r| l + r),
            0x15 => arith!(|l: i32, r: i32| l.wrapping_sub(r), |l, r| l - r),
            0x16 => arith!(|l: i32, r: i32| l.wrapping_mul(r), |l, r| l * r),
            0x17 => {
                if s.top_two_int() {
                    let r = s.pop_int();
                    let l = s.pop_int();
                    if r == 0 {
                        s.push(Value::Int(0));
                        s.faults.push(Fault::DivideByZero);
                    } else {
                        s.push(Value::Int(l.wrapping_div(r)));
                    }
                } else {
                    let r = s.pop_float();
                    let l = s.pop_float();
                    s.push(Value::Float(l / r));
                }
            }
            0x18 => {
                let r = s.pop_int();
                let l = s.pop_int();
                if r == 0 {
                    s.push(Value::Int(0));
                    s.faults.push(Fault::DivideByZero);
                } else {
                    s.push(Value::Int(l.wrapping_rem(r)));
                }
            }
            0x19 => {
                let v = s.pop_int();
                s.push(Value::Int(i32::from(v == 0)));
            }
            0x1A => {
                let v = s.pop_float();
                s.push(Value::Float(-v));
            }
            0x1C => {
                let v = s.pop_int();
                s.push(Value::Int(!v));
            }
            0x1F | 0x22 | 0x27 => {
                let index = if insn.op == 0x1F { 0 } else { s.pop_int() };
                let Some(var) = desc.vars.get(usize::from(arg)).copied() else {
                    return Err(Fault::InvalidOpcode(insn.op));
                };
                let place = self.memory.place(&var, index, w, si, desc.data_areas, script.shared_data());
                let s = &mut self.workers[w].stack;
                if insn.op == 0x27 {
                    s.push(Value::Int(encode_address(place)));
                } else if var.ty == VarType::F32 {
                    s.push(Value::Float(self.memory.load_float(place, var.ty)));
                } else {
                    s.push(Value::Int(self.memory.load_int(place, var.ty)));
                }
            }
            0x20 | 0x21 | 0x23 | 0x24 => {
                let Some(var) = desc.vars.get(usize::from(arg)).copied() else {
                    return Err(Fault::InvalidOpcode(insn.op));
                };
                let saturate = matches!(insn.op, 0x21 | 0x24);
                let indexed = matches!(insn.op, 0x23 | 0x24);
                let value = s.pop();
                let index = if indexed { s.pop_int() } else { 0 };
                let place = self.memory.place(&var, index, w, si, desc.data_areas, script.shared_data());
                match value {
                    Value::Int(v) => self.memory.store_int(place, var.ty, v, saturate),
                    Value::Float(f) => self.memory.store_float(place, var.ty, f, saturate),
                }
            }
            0x25 => {
                let v = s.pop_float();
                worker.contexts[li].f = v;
            }
            0x26 => {
                let v = worker.contexts[li].f;
                worker.stack.push(Value::Float(v));
            }
            0x28 => {
                let v = worker.contexts[li].a;
                worker.stack.push(Value::Int(v));
            }
            0x29 => {
                let v = worker.contexts[li].x;
                worker.stack.push(Value::Int(v));
            }
            0x2A => worker.contexts[li].a = s.pop_int(),
            0x2C => worker.contexts[li].x = s.pop_int(),
            0x2B => {
                let v = s.peek().map_or(0, Value::int);
                s.push(Value::Int(v));
            }
            0x2D => s.push(Value::Int(desc.ints.get(usize::from(arg)).copied().unwrap_or(0))),
            0x2E => s.push(Value::Int(i32::from(arg as i16))),
            0x2F => s.push(Value::Float(desc.floats.get(usize::from(arg)).copied().unwrap_or(0.0))),
            0x30..=0x32 | 0x55..=0x57 => {
                if matches!(insn.op, 0x55..=0x57) {
                    worker.contexts[li].a = s.pop_int();
                }
                let a = worker.contexts[li].a;
                let take = match insn.op {
                    0x30 | 0x55 => true,
                    0x31 | 0x56 => a != 0,
                    _ => a == 0,
                };
                if take {
                    // Inside a call into another worker, labels are the callee's.
                    let ctx = &worker.contexts[li];
                    let owner = if ctx.depth > 0 { usize::from(ctx.callees[ctx.depth - 1]) } else { w };
                    let &(os, od) = self.order.get(owner).ok_or(Fault::PcOutOfRange)?;
                    let labels = &self.scripts[os].workers[od].labels;
                    jump = Some(*labels.get(usize::from(arg)).ok_or(Fault::PcOutOfRange)?);
                }
            }
            0x33 => {
                let ctx = &mut worker.contexts[li];
                if ctx.depth >= CALL_DEPTH {
                    // The original flags an error and steps over the call.
                    worker.stack.faults.push(Fault::CallDepth);
                    worker.contexts[li].pc = next;
                    return Ok(run);
                }
                // The callee is numbered across the group; its entry runs in the caller's code (as the original's
                // CALLSUB adds the callee's entry offset to the caller's script).
                let callee_index = usize::from(arg) + base;
                let &(cs, cd) = self.order.get(callee_index).ok_or(Fault::PcOutOfRange)?;
                let callee = &self.scripts[cs].workers[cd];
                ctx.returns[ctx.depth] = next;
                ctx.callees[ctx.depth] = callee_index as u16;
                ctx.depth += 1;
                ctx.calling = false;
                jump = Some(callee.entries.first().copied().unwrap_or(0));
            }
            0x34 => {
                let ctx = &mut worker.contexts[li];
                if ctx.depth == 0 {
                    // Return without a call (a subroutine worker running on its own): flagged, stepped over.
                    worker.stack.faults.push(Fault::CallDepth);
                    worker.contexts[li].pc = next;
                    return Ok(run);
                }
                ctx.depth -= 1;
                ctx.calling = false;
                jump = Some(ctx.returns[ctx.depth]);
            }
            0x35 | 0x58 => {
                run = Some(false);
                let ctx = &mut worker.contexts[li];
                let first = !ctx.calling;
                ctx.calling = true;
                let kind = worker.kind;
                let mut call = NativeCall {
                    worker: w,
                    kind,
                    level,
                    scratch: &mut ctx.scratch,
                    stack: &mut worker.stack,
                    memory: &mut self.memory,
                    frame: self.frame,
                    effects: &mut self.effects,
                    requests: &requests,
                };
                if first {
                    host.native_init(arg, &mut call);
                }
                let r = host.native_wait(arg, &mut call);
                run = Some(r & WAIT_CONTINUE != 0).or(run);
                if r & WAIT_DONE == 0 {
                    jump = Some(pc);
                } else {
                    let v = host.native_result(arg, &mut call);
                    worker.contexts[li].calling = false;
                    if insn.op == 0x58 {
                        worker.contexts[li].f = v.float();
                    } else {
                        worker.stack.push(v);
                    }
                }
                for e in std::mem::take(&mut self.effects) {
                    match e {
                        Effect::ExtraPass(t) => {
                            if let Some(t) = self.workers.get_mut(t) {
                                t.pass_marks = t.pass_marks & !0x400 | 0x1200;
                            }
                        }
                        Effect::PassAgain(t) => {
                            if let Some(t) = self.workers.get_mut(t) {
                                t.pass_marks = t.pass_marks & !0x400 | 0x200;
                            }
                        }
                        Effect::Enable(t, on) => {
                            if let Some(t) = self.workers.get_mut(t) {
                                t.enabled = on;
                            }
                        }
                        Effect::SkipQueued(t, level) => {
                            let Some(wk) = self.workers.get(t) else { continue };
                            let dropped: Vec<Request> =
                                wk.queue.iter().filter(|r| r.level > level && !r.running).copied().collect();
                            self.workers[t].queue.retain(|r| !dropped.iter().any(|d| d.id == r.id));
                            for r in dropped {
                                if r.mode == RequestMode::WaitStart && r.from.is_some() {
                                    self.signal(t, r.from, r.level, r.entry);
                                }
                            }
                        }
                    }
                }
            }
            0x36 | 0x45..=0x49 | 0x37 | 0x4A..=0x4E | 0x38 | 0x4F..=0x53 => {
                let entry = s.pop_int();
                let target = s.pop_int();
                let lvl = s.pop_int();
                let mode = match insn.op {
                    0x36 | 0x45..=0x49 => RequestMode::Plain,
                    0x37 | 0x4A..=0x4E => RequestMode::WaitStart,
                    _ => RequestMode::WaitEnd,
                };
                // Per family: the base opcode, then variants without a refusal, refusing the same entry, and
                // three gated ones (the gate is not ported; they follow the same rules).
                let refuse = match insn.op {
                    0x36..=0x38 => Refuse::SameLevel,
                    o => match (o - 0x45) % 5 {
                        0 | 3 => Refuse::Never,
                        1 | 4 => Refuse::SameEntry,
                        _ => Refuse::SameLevel,
                    },
                };
                let target = if target >= 0 { target + base as i32 } else { target };
                let ok = target >= 0
                    && self.request_with(
                        Some(w as u16),
                        target as usize,
                        mode,
                        lvl.clamp(0, 15) as u8,
                        entry as u16,
                        refuse,
                    );
                let worker = &mut self.workers[w];
                worker.stack.push(Value::Int(i32::from(ok)));
                if mode != RequestMode::Plain {
                    run = Some(false);
                    if ok {
                        worker.contexts[li].wait = Wait::Signal {
                            target: target as u16,
                            entry: entry as u16,
                            level: lvl.clamp(0, 15) as u8,
                        };
                    }
                }
            }
            0x39..=0x3B => {
                let entry = s.pop_int();
                let slot = s.pop_int();
                let lvl = s.pop_int().clamp(0, 15) as u8;
                let target = host.party_worker(slot);
                let mode = match insn.op {
                    0x39 => RequestMode::Plain,
                    0x3A => RequestMode::WaitStart,
                    _ => RequestMode::WaitEnd,
                };
                let ok = entry != 0
                    && target.is_some_and(|t| self.request(Some(w as u16), t, mode, lvl, entry as u16));
                let worker = &mut self.workers[w];
                worker.stack.push(Value::Int(i32::from(ok)));
                if mode != RequestMode::Plain {
                    run = Some(false);
                    if let (true, Some(t)) = (ok, target) {
                        worker.contexts[li].wait =
                            Wait::Signal { target: t as u16, entry: entry as u16, level: lvl };
                    }
                }
            }
            0x3C | 0x3D | 0x3E | 0x3F | 0x54 => {
                let cancel = match insn.op {
                    0x3D | 0x3F => Some(s.pop_int().clamp(0, 15) as u8),
                    0x54 => Some(0),
                    _ => None,
                };
                let restart = matches!(insn.op, 0x3E | 0x3F);
                // The pc stays on the instruction: an idle worker re-runs it every frame.
                self.end_entry(w, cancel, restart);
                self.turn_over = true;
                return Ok(Some(false));
            }
            0x40 => {}
            0x59..=0x5C => {
                let v = s.pop_int();
                let r = usize::from(insn.op - 0x59);
                self.memory.registers[w][r * 4..r * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            0x5D..=0x66 => {
                let v = s.pop_float();
                let r = usize::from(insn.op - 0x59);
                self.memory.registers[w][r * 4..r * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            0x67..=0x74 => {
                let r = usize::from(insn.op - 0x67);
                let b = &self.memory.registers[w][r * 4..r * 4 + 4];
                let raw = [b[0], b[1], b[2], b[3]];
                let s = &mut self.workers[w].stack;
                if r < 4 {
                    s.push(Value::Int(i32::from_le_bytes(raw)));
                } else {
                    s.push(Value::Float(f32::from_le_bytes(raw)));
                }
            }
            0x75 => s.push(Value::Int(0)),
            0x77 => {
                let entry = s.pop_int() as u16;
                let target = s.pop_int();
                let busy = usize::try_from(target).ok().and_then(|t| self.workers.get(t)).is_some_and(|t| {
                    t.queue.iter().any(|r| r.mode != RequestMode::Signal && r.entry == entry)
                });
                if busy {
                    let worker = &mut self.workers[w];
                    worker.contexts[li].wait = Wait::Idle { target: target as u16, entry };
                    run = Some(false);
                }
            }
            0x78 => {
                let entry = s.pop_int() as u16;
                let slot = s.pop_int();
                if let Some(t) = host.party_worker(slot) {
                    let busy = self.workers[t]
                        .queue
                        .iter()
                        .any(|r| r.mode != RequestMode::Signal && r.entry == entry);
                    if busy {
                        let worker = &mut self.workers[w];
                        worker.contexts[li].wait = Wait::Idle { target: t as u16, entry };
                        run = Some(false);
                    }
                }
            }
            // The entry a signal type runs (entry, type, and the variable the table lives in).
            0x79 => {
                let entry = s.pop_int() as i16;
                let t = s.pop_int().clamp(0, 7) as usize;
                s.pop();
                let kind = worker.kind;
                let row = match kind {
                    1 => 0,
                    2 | 3 => 1,
                    5 => 2,
                    _ => 3,
                };
                let table = worker.signal_entries.get_or_insert(SIGNAL_ENTRIES[row]);
                table[t] = entry;
            }
            0x7A => {
                s.pop();
                s.pop();
                s.push(Value::Int(0));
            }
            op => return Err(Fault::InvalidOpcode(op)),
        }
        let li = self.workers[w].level_index(self.workers[w].level);
        self.workers[w].contexts[li].pc = jump.unwrap_or(next);
        Ok(run)
    }
}
