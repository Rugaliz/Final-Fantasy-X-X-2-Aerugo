//! The script machine: workers with prioritized levels, a shared operand stack per worker, requests between
//! workers, and native calls that may span frames. See `docs/script-vm.md`.

use std::sync::Arc;

use aerugo_formats::script::{Script, VarType};

use crate::memory::{Memory, encode_address};

/// Most instructions one worker may run in one frame before it is stopped as stuck.
const RUN_LIMIT: u32 = 0x1_0000;
const STACK_SLOTS: usize = 19;
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
    pub kind: u8,
    pub active: bool,
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
    pub level: u8,
    pub scratch: &'a mut [i32; 4],
    pub stack: &'a mut Stack,
    pub memory: &'a mut Memory,
    pub frame: u64,
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
}

/// Phase of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Every worker runs its entry 0 until it ends.
    Init,
    Running,
}

/// A running script (one event or battle).
#[derive(Debug)]
pub struct Vm {
    pub script: Arc<Script>,
    pub workers: Vec<Worker>,
    pub memory: Memory,
    pub phase: Phase,
    pub frame: u64,
    next_request: u32,
    /// Trace of executed instructions, when enabled.
    pub trace: Option<Vec<TraceEvent>>,
    /// Faults with the worker and pc they happened at.
    pub faults: Vec<(usize, u32, Fault)>,
}

impl Vm {
    /// Set up every worker of `script` at entry 0, level 0.
    pub fn new(script: Arc<Script>) -> Vm {
        let n = script.workers.len();
        let workers = script
            .workers
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let mut contexts = vec![Context::default(); 9];
                contexts[0].pc = w.entries.first().copied().unwrap_or(0);
                // Kind 0 workers hold subroutines other workers call into; they never run on their own.
                let runs = w.kind != 0;
                Worker {
                    index: i,
                    kind: w.kind,
                    active: runs,
                    accepts: runs,
                    ended_entry: !runs,
                    entry: 0,
                    level: 0,
                    contexts,
                    stack: Stack::default(),
                    queue: Vec::new(),
                    active_request: None,
                    rate: 1.0,
                    rate_acc: 0.0,
                }
            })
            .collect();
        let memory = Memory::new(script.data().to_vec(), n);
        Vm {
            script,
            workers,
            memory,
            phase: Phase::Init,
            frame: 0,
            next_request: 1,
            trace: None,
            faults: Vec::new(),
        }
    }

    /// Run one frame (1/30 s of game time).
    pub fn frame(&mut self, host: &mut dyn Host) {
        match self.phase {
            Phase::Init => {
                for w in 0..self.workers.len() {
                    if !self.workers[w].ended_entry {
                        self.run_worker(w, host);
                    }
                }
                if self.workers.iter().all(|w| w.ended_entry || !w.active) {
                    for w in 0..self.workers.len() {
                        self.decode(w, None);
                    }
                    self.phase = Phase::Running;
                }
            }
            Phase::Running => {
                for w in 0..self.workers.len() {
                    self.run_worker(w, host);
                }
                for w in 0..self.workers.len() {
                    self.decode(w, None);
                }
            }
        }
        self.frame += 1;
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
        if entry == 0 || target >= self.workers.len() {
            return false;
        }
        let t = &mut self.workers[target];
        let level = level.min(t.max_level());
        if t.queue.iter().any(|r| r.mode != RequestMode::Signal && r.level == level) {
            return false;
        }
        let id = self.next_request;
        self.next_request += 1;
        let r = Request { id, from, mode, level, entry, running: false, fresh: true };
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
                let entries = &self.script.workers[w].entries;
                let pc = entries.get(usize::from(head.entry)).copied().unwrap_or(0);
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
        self.decode(w, Some(()));
        if restart {
            let pc = self.script.workers[w].entries.get(1).copied().unwrap_or(0);
            let worker = &mut self.workers[w];
            let li = worker.level_index(worker.level);
            worker.contexts[li].depth = 0;
            worker.contexts[li].pc = pc;
        }
        self.workers[w].ended_entry = true;
    }

    fn fault(&mut self, w: usize, f: Fault) {
        let pc = self.workers[w].context().pc;
        if self.faults.len() < 1000 {
            self.faults.push((w, pc, f));
        }
    }

    /// Run worker `w` for this frame.
    pub fn run_worker(&mut self, w: usize, host: &mut dyn Host) {
        if !self.workers[w].active {
            return;
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
        let script = self.script.clone();
        let desc = &script.workers[w];
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
            0x08 => compare!(|l: i32, r: i32| (l as u32) > (r as u32), |l: f32, r: f32| l > r),
            0x09 => compare!(|l: i32, r: i32| (l as u32) >= (r as u32), |l: f32, r: f32| l >= r),
            0x0A => compare!(|l, r| l > r, |l: f32, r: f32| l > r),
            0x0B => compare!(|l, r| l >= r, |l: f32, r: f32| l >= r),
            0x0C => compare!(|l: i32, r: i32| (l as u32) < (r as u32), not_ge),
            0x0D => compare!(|l: i32, r: i32| (l as u32) <= (r as u32), not_gt),
            0x0E => compare!(|l, r| l < r, not_ge),
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
                let place = self.memory.place(&var, index, w, desc.data_areas, script.shared_data());
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
                let place = self.memory.place(&var, index, w, desc.data_areas, script.shared_data());
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
                    let labels = &script.workers.get(owner).ok_or(Fault::PcOutOfRange)?.labels;
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
                let callee = script.workers.get(usize::from(arg)).ok_or(Fault::PcOutOfRange)?;
                ctx.returns[ctx.depth] = next;
                ctx.callees[ctx.depth] = arg;
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
                let mut call = NativeCall {
                    worker: w,
                    level,
                    scratch: &mut ctx.scratch,
                    stack: &mut worker.stack,
                    memory: &mut self.memory,
                    frame: self.frame,
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
                let ok = target >= 0
                    && self.request(
                        Some(w as u16),
                        target as usize,
                        mode,
                        lvl.clamp(0, 15) as u8,
                        entry as u16,
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
            0x79 => {
                s.pop();
                s.pop();
                s.pop();
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
