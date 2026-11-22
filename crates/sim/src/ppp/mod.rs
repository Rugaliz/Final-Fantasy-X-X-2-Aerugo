//! The particle program runtime the effects drive (the PS2 "ppp" engine), ported with the original's memory
//! layout: the effect's data and its work buffer are byte regions at the addresses the effect's driver gives
//! them, the work buffer is a block pool with the original's headers, and objects are pool blocks whose fields
//! and program slots sit at the original's offsets. Allocation order, reuse and failure therefore match.
//!
//! - [`Space`]: the effect's address space (its data section and its work buffer).
//! - [`Env`]: the environment (the pool and the group's lists) and [`Part`]: a running part (a timed list of
//!   sub-part definitions that spawn objects, each running its definition's programs).
//! - The programs are in [`programs`], by name; an effect's driver lists the ones its table holds.

pub mod programs;
pub mod relocate;

/// A byte region at an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub base: u32,
    pub bytes: Vec<u8>,
}

/// The effect's address space. Reads and writes outside every region read 0 and are dropped, and are counted
/// (a port error, never a game behaviour).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Space {
    pub regions: Vec<Region>,
    pub faults: std::cell::Cell<u32>,
}

impl Space {
    fn find(&self, at: u32, n: u32) -> Option<(usize, usize)> {
        self.regions.iter().enumerate().find_map(|(i, r)| {
            let o = at.checked_sub(r.base)? as usize;
            (o + n as usize <= r.bytes.len()).then_some((i, o))
        })
    }

    pub fn u8(&self, at: u32) -> u8 {
        match self.find(at, 1) {
            Some((i, o)) => self.regions[i].bytes[o],
            None => {
                self.faults.set(self.faults.get() + 1);
                0
            }
        }
    }

    pub fn u16(&self, at: u32) -> u16 {
        match self.find(at, 2) {
            Some((i, o)) => {
                let b = &self.regions[i].bytes;
                u16::from_le_bytes([b[o], b[o + 1]])
            }
            None => {
                self.faults.set(self.faults.get() + 1);
                0
            }
        }
    }

    pub fn u32(&self, at: u32) -> u32 {
        match self.find(at, 4) {
            Some((i, o)) => {
                let b = &self.regions[i].bytes;
                u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
            }
            None => {
                self.faults.set(self.faults.get() + 1);
                0
            }
        }
    }

    pub fn f32(&self, at: u32) -> f32 {
        f32::from_bits(self.u32(at))
    }

    pub fn set_u8(&mut self, at: u32, v: u8) {
        match self.find(at, 1) {
            Some((i, o)) => self.regions[i].bytes[o] = v,
            None => self.faults.set(self.faults.get() + 1),
        }
    }

    pub fn set_u16(&mut self, at: u32, v: u16) {
        match self.find(at, 2) {
            Some((i, o)) => self.regions[i].bytes[o..o + 2].copy_from_slice(&v.to_le_bytes()),
            None => self.faults.set(self.faults.get() + 1),
        }
    }

    pub fn set_u32(&mut self, at: u32, v: u32) {
        match self.find(at, 4) {
            Some((i, o)) => self.regions[i].bytes[o..o + 4].copy_from_slice(&v.to_le_bytes()),
            None => self.faults.set(self.faults.get() + 1),
        }
    }

    pub fn set_f32(&mut self, at: u32, v: f32) {
        self.set_u32(at, v.to_bits())
    }
}

/// A block pool (pppInitEnv / the pool's alloc and free): four words {cursor, first block, end marker, used}.
/// Blocks are {next, prev, in use, _} headers of 0x10 bytes before their space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pool {
    pub cursor: u32,
    pub first: u32,
    pub end: u32,
    pub used: u32,
}

impl Pool {
    /// A pool over `size` bytes at `work`: one free block up to the end marker.
    pub fn init(m: &mut Space, work: u32, size: u32) -> Pool {
        let end = work.wrapping_add(size).wrapping_sub(0x10);
        m.set_u32(work + 4, 0);
        m.set_u32(work, end);
        m.set_u32(work + 8, 0);
        m.set_u32(end + 4, work);
        m.set_u32(end, 0);
        m.set_u32(end + 8, 1);
        Pool { cursor: 0, first: work, end, used: 0 }
    }

    /// A block of `n` bytes (its space's address), carved from the top of the first free block that fits,
    /// searched backwards from the block after the cursor; 0 when none fits.
    pub fn alloc(&mut self, m: &mut Space, n: u32) -> u32 {
        let size = (n.wrapping_add(0xf) & !0xf).wrapping_add(0x10);
        let mut from = if self.cursor == 0 {
            self.cursor = self.first;
            m.u32(self.first)
        } else {
            m.u32(self.cursor)
        };
        loop {
            let mut b = m.u32(from + 4);
            while b != 0 {
                let next = m.u32(b);
                if m.u32(b + 8) == 0 && (size as i32) <= next.wrapping_sub(b) as i32 {
                    self.used = self.used.wrapping_add(size);
                    let nb = next.wrapping_sub(size);
                    if nb == b || nb.wrapping_sub(b) < 0x10 {
                        self.cursor = 0;
                        m.set_u32(b + 8, 1);
                        return b + 0x10;
                    }
                    self.cursor = b;
                    m.set_u32(nb + 4, b);
                    m.set_u32(nb, next);
                    m.set_u32(next + 4, nb);
                    m.set_u32(b, nb);
                    m.set_u32(nb + 8, 1);
                    return nb + 0x10;
                }
                b = m.u32(b + 4);
            }
            if self.cursor == 0 {
                return 0;
            }
            from = self.end;
            self.cursor = 0;
        }
    }

    /// Give block `p` back, merging it with free neighbours.
    pub fn free(&mut self, m: &mut Space, p: u32) {
        let mut b = p.wrapping_sub(0x10);
        m.set_u32(p.wrapping_sub(8), 0);
        self.cursor = b;
        let next = m.u32(b);
        self.used = b.wrapping_add(self.used.wrapping_sub(next));
        let prev = m.u32(p.wrapping_sub(0xc));
        if prev != 0 && m.u32(prev + 8) == 0 {
            m.set_u32(prev, next);
            m.set_u32(next + 4, prev);
            b = prev;
        }
        let n = m.u32(b);
        if m.u32(n + 8) == 0 {
            let nn = m.u32(n);
            m.set_u32(b, nn);
            m.set_u32(nn + 4, b);
        }
    }
}

/// The environment (the effect's state block): its pools and the group's three lists (header +0x14 / +0x18 /
/// +0x1c as addresses).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Env {
    pub pool: Pool,
    /// The second pool, used when the first is full (none is set up for the event effects seen).
    pub pool2: Option<Pool>,
    pub lists: [u32; 3],
    /// The objects the pools could not hold.
    pub alloc_failed: bool,
    /// The engine's scratch offset (the draw stage sets it for a moment; the i16 inits add it, then clear it).
    pub scratch_offset: u32,
}

impl Env {
    /// The instance set-up (7169d0): the group's lists, the pool over the work buffer.
    pub fn new(m: &mut Space, group: u32, group_lists: [u32; 3], work: u32, size: u32) -> Env {
        let _ = group;
        Env {
            pool: Pool::init(m, work, size),
            pool2: None,
            lists: group_lists,
            alloc_failed: false,
            scratch_offset: 0,
        }
    }

    /// pppMemAlloc: the first pool, then the second; 0 (and the failure flag) when neither holds it.
    pub fn alloc(&mut self, m: &mut Space, n: u32) -> u32 {
        let p = self.pool.alloc(m, n);
        self.alloc_failed = false;
        if p != 0 {
            return p;
        }
        if let Some(p2) = &mut self.pool2 {
            let q = p2.alloc(m, n);
            if q != 0 {
                return q;
            }
        }
        self.alloc_failed = true;
        0
    }

    /// Free a block of whichever pool holds it (the second pool's blocks lie past the first's end).
    pub fn free(&mut self, m: &mut Space, p: u32) {
        match &mut self.pool2 {
            Some(p2) if p > self.pool.end => p2.free(m, p),
            _ => self.pool.free(m, p),
        }
    }
}

/// A program's part-level entry (slot, def, ref, index).
pub type GroupFn = fn(&mut Run, u32, u32, u32, u32);
/// A per-object entry (object, key, ref).
pub type ObjectFn = fn(&mut Run, u32, u32, u32);
/// A per-part entry.
pub type PartFn = fn(&mut Run, &mut Part);
/// An object set-up entry (object, ref).
pub type SetupFn = fn(&mut Run, u32, u32);

/// A program's entry points (the table's +4 .. +0x24); `None` where the table has none.
#[derive(Clone, Copy, Default)]
pub struct Program {
    pub name: &'static str,
    /// +4: run once for the slot instead of per object: (rt, slot, def, ref, index).
    pub group: Option<GroupFn>,
    /// +8 / +0xc: per object (rt, object, key, ref); +0xc is the draw stage.
    pub update: Option<ObjectFn>,
    pub second: Option<ObjectFn>,
    /// +0x14 / +0x18: per part, before / after (update flags 0x10 / 0x20).
    pub pre: Option<PartFn>,
    pub post: Option<PartFn>,
    /// +0x1c at creation, +0x20 at a restart, +0x24 at the free: (rt, object, ref).
    pub init: Option<SetupFn>,
    pub restart: Option<SetupFn>,
    pub free: Option<SetupFn>,
}

impl std::fmt::Debug for Program {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name)
    }
}

/// The engine's vector-unit random generator: its seed word (shared by every effect and the engine's other
/// random users; 1.0's bits at boot) and its switch (on from the engine's start-up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VuRand {
    pub seed: u32,
    pub on: bool,
}

impl Default for VuRand {
    fn default() -> VuRand {
        VuRand { seed: 0x3f80_0000, on: true }
    }
}

/// The address the programs' shared target vector has in an effect's space (the engine's scratch vector, for
/// keys that name no work offset). Aerugo's own place, outside every effect's data.
pub const SCRATCH_VECTOR: u32 = 0xf000_0000;

/// What the programs run against: the address space, the environment, the effect's program table (its
/// address in the effect's data, and its entries), the engine's pause switch and its random generators (the
/// vector-unit one, and the C runtime's, which each vector draw re-seeds).
#[derive(Debug)]
pub struct Run<'a> {
    pub m: &'a mut Space,
    pub env: &'a mut Env,
    pub table_va: u32,
    pub table: &'a [Program],
    pub paused: bool,
    pub vu: &'a mut VuRand,
    pub crt: &'a mut crate::field::face::CrtRand,
    /// The running part's object list head and its sub-part slots (what programs that spawn objects use).
    pub objects: u32,
    pub slots: u32,
    /// The remaster's slower vertex spawning in one scene (map 0xe6c, event 0xcc6, magic 0x207 loaded, one of
    /// workers 5–7 running): pppVertexAp's interval × 1.8. The host sets it.
    pub vertex_ap_slow: bool,
    /// The remaster's lifetime link for local vertex spawns (map 0x18cd or event 0x10e, with no magic loaded
    /// and the engine's link value set): a parent ending before its new child lives a frame longer. The host
    /// sets it.
    pub vertex_lc_extend: bool,
    /// The engine's current magic number (some programs leave effect 0x2a9 alone).
    pub magic: u32,
    /// What the programs ask of the renderer this step, in order (the host's renderer acts on them).
    pub render: Vec<RenderCall>,
}

/// A program's request to the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderCall {
    /// The scene's accumulation setting (pppKeAcmCic: (0, 1)).
    Accumulation(u32, u32),
}

impl Run<'_> {
    /// One vector-unit draw in [0, 1): the seed flipped (its low 23 bits) and handed to the C runtime's
    /// srand, then stepped as a float in [1, 2) when the generator is on (1.0 when off), less 1.
    pub fn vu_draw(&mut self) -> f32 {
        let s = self.vu.seed ^ 0x7f_ffff;
        self.vu.seed = s;
        *self.crt = crate::field::face::CrtRand(s & 0x7f_ffff);
        let v = if self.vu.on {
            let n = ((s >> 18 ^ s) >> 4 & 1) | ((s & 0x3f_ffff) | 0x1fc0_0000) << 1;
            self.vu.seed = n;
            f32::from_bits(n)
        } else {
            1.0
        };
        v + -1.0
    }

    /// The vector-unit integer draw (two draws; the second's value · 32768, truncated).
    pub fn vu_int(&mut self) -> i32 {
        self.vu_draw();
        (f64::from(self.vu_draw()) * 32768.0).trunc() as i32
    }

    /// The program a bound reference (or list entry) at `at` points to (table entries are 0x28 bytes).
    pub fn program(&mut self, at: u32) -> Option<Program> {
        let p = self.m.u32(at).checked_sub(self.table_va)?;
        (p % 0x28 == 0).then(|| self.table.get((p / 0x28) as usize).copied()).flatten()
    }
}

/// A running part (the part state _pppStartPart fills).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Part {
    /// 0, or 0xfffff000 when its sub-part slots could not be allocated.
    pub status: u32,
    pub length: u32,
    /// The part's time, the time before the last step, and the step (0x1000 a frame).
    pub time: i32,
    pub prev: i32,
    pub step: i32,
    /// The program list (count, address of the list) and the sub-part list (count, address of the list).
    pub programs: (u32, u32),
    pub subs: (u32, u32),
    pub until_empty: bool,
    pub drained: bool,
    /// The objects of the part (its list head: an object address or 0), and its sub-part slots (16 bytes each:
    /// +0 definition, +4 object list, +8 start time, +0xc u16 count, +0xe index).
    pub objects: u32,
    pub slots: u32,
}

impl Part {
    /// _pppStartPart(part, step, record): the record's length, flags and lists; one slot per definition.
    pub fn start(r: &mut Run, step: i32, record: u32) -> Part {
        let m = &mut *r.m;
        let mut p =
            Part { length: m.u32(record), until_empty: m.u8(record + 4) != 0, step, ..Part::default() };
        let (l1, l2) = (record.wrapping_add(m.u32(record + 8)), record.wrapping_add(m.u32(record + 0xc)));
        p.programs = (m.u32(l1), l1 + 4);
        p.subs = (m.u32(l2), l2 + 4);
        if p.subs.0 != 0 {
            p.slots = r.env.alloc(m, p.subs.0 << 4);
            if p.slots == 0 {
                p.status = 0xffff_f000;
                return p;
            }
        }
        // One slot per definition of the (bound) chain from +0x10.
        let mut def = record + 0x10;
        let mut i = 0u32;
        while def != 0 && i < p.subs.0 {
            let s = p.slots + i * 0x10;
            m.set_u32(s, def);
            m.set_u32(s + 8, m.u32(def + 0x10));
            m.set_u32(s + 4, 0);
            m.set_u16(s + 0xc, 0);
            m.set_u8(s + 0xe, i as u8);
            i += 1;
            def = m.u32(def);
        }
        p
    }

    /// One step (the update with `flags`): spawns, the programs over every object, the time; true when done.
    pub fn update(&mut self, r: &mut Run, flags: u8) -> bool {
        r.objects = self.objects;
        r.slots = self.slots;
        let done = self.step(r, flags);
        self.objects = r.objects;
        done
    }

    fn step(&mut self, r: &mut Run, flags: u8) -> bool {
        if flags & 0x10 != 0 {
            self.each_program(r, |pg| pg.pre);
        }
        self.spawn(r);
        self.run(r);
        if flags & 0x20 != 0 {
            self.each_program(r, |pg| pg.post);
        }
        if r.paused {
            return false;
        }
        self.prev = self.time;
        self.time = self.time.wrapping_add(self.step);
        if !self.until_empty {
            return self.length as i32 <= self.time;
        }
        if self.drained {
            return r.objects == 0;
        }
        if self.time > 0x6fff_ffff {
            self.prev = 0x7000_0000 - self.step;
            self.time = 0x7000_0000;
        }
        false
    }

    fn each_program(&mut self, r: &mut Run, pick: fn(&Program) -> Option<PartFn>) {
        for i in 0..self.programs.0 {
            if let Some(f) = r.program(self.programs.1 + 4 * i).as_ref().and_then(pick) {
                f(r, self);
            }
        }
    }

    /// 712b80: each slot whose start time has come gets one object (and its start time is spent).
    fn spawn(&mut self, r: &mut Run) {
        for i in 0..self.subs.0 {
            let s = self.slots + i * 0x10;
            if self.slots != 0 && (r.m.u32(s + 8) as i32) <= self.time {
                r.m.set_u32(s + 8, 0x7fff_ffff);
                if create(r, s) == 0 {
                    return;
                }
            }
        }
    }
}

/// pppCreatePObject: a pool block for the slot's definition, linked into the slot's list and into the running
/// part's list by priority (def u16 +0x24), then each program slot set and each program's +0x1c run; 0 when
/// the pools are full.
pub fn create(r: &mut Run, slot: u32) -> u32 {
    let o = {
        let m = &mut *r.m;
        let def = m.u32(slot);
        let n = m.u32(def + 0x20).wrapping_add((m.u16(def + 0x26) as i16 as i32 as u32).wrapping_mul(4));
        let o = r.env.alloc(m, n);
        if o == 0 {
            return 0;
        }
        m.set_u32(o + 0xc, 0);
        m.set_u32(o + 0x90, 0);
        m.set_u32(o + 0x94, 0);
        m.set_u32(o + 8, slot);
        let first = m.u32(slot + 4);
        if r.objects != 0 && first != 0 {
            // After the slot's newest object.
            m.set_u32(o, m.u32(first));
            m.set_u32(first, o);
        } else {
            // Into the part list before the first object of a priority at least this one's.
            let pri = m.u16(def + 0x24) as i16;
            let mut link: Option<u32> = None;
            let mut cur = r.objects;
            let mut placed = false;
            while cur != 0 {
                let cdef = m.u32(m.u32(cur + 8));
                if pri <= m.u16(cdef + 0x24) as i16 {
                    m.set_u32(slot + 4, o);
                    match link {
                        Some(l) => m.set_u32(l, o),
                        None => r.objects = o,
                    }
                    m.set_u32(o, cur);
                    placed = true;
                    break;
                }
                link = Some(cur);
                cur = m.u32(cur);
            }
            if !placed {
                m.set_u32(slot + 4, o);
                match link {
                    Some(l) => m.set_u32(l, o),
                    None => r.objects = o,
                }
                m.set_u32(o, 0);
            }
        }
        let c = m.u16(slot + 0xc);
        m.set_u16(slot + 0xc, c.wrapping_add(1));
        o
    };
    init_slots(r, o, |pg| pg.init);
    o
}

/// 712d90 / 712d20: each program slot of the object set to its reference's track, then the init (or restart)
/// entry of its program.
fn init_slots(r: &mut Run, o: u32, pick: fn(&Program) -> Option<SetupFn>) {
    {
        let def = r.m.u32(r.m.u32(o + 8));
        let mut at = o.wrapping_add(r.m.u32(def + 0x20));
        let count = r.m.u16(def + 0x26) as i16;
        for k in 0..count.max(0) as u32 {
            let rf = def + 0x28 + k * 0x10;
            let track = r.m.u32(rf + 8);
            r.m.set_u32(at, track);
            at += 4;
            if let Some(f) = r.program(rf).as_ref().and_then(pick) {
                f(r, o, rf);
            }
        }
    }
}

/// _pppAllFreePObject: each program's +0x24, the slot's count down (its list moves on when this was its head),
/// the block back to its pool. (The caller unlinks it from the part's list.)
pub fn free_object(r: &mut Run, o: u32) {
    let slot = r.m.u32(o + 8);
    let def = r.m.u32(slot);
    let refs = r.m.u16(def + 0x26) as i16;
    for k in 0..refs.max(0) as u32 {
        let rf = def + 0x28 + k * 0x10;
        if let Some(f) = r.program(rf).and_then(|p| p.free) {
            f(r, o, rf);
        }
    }
    let c = r.m.u16(slot + 0xc).wrapping_sub(1);
    r.m.set_u16(slot + 0xc, c);
    if c == 0 {
        r.m.set_u32(slot + 4, 0);
    } else if r.m.u32(slot + 4) == o {
        let n = r.m.u32(o);
        r.m.set_u32(slot + 4, n);
    }
    r.env.free(r.m, o);
}

impl Part {
    /// pppRunPartStd: each slot's objects through its definition's programs (keys advanced on their time),
    /// their ages stepped; then objects past their definition's times restart or go.
    fn run(&mut self, r: &mut Run) {
        for i in 0..self.subs.0 {
            let s = self.slots + i * 0x10;
            if self.slots == 0 {
                break;
            }
            let def = r.m.u32(s);
            let count = r.m.u16(s + 0xc);
            if def == 0 || count == 0 {
                continue;
            }
            let refs = r.m.u16(def + 0x26) as i16;
            for k in 0..refs.max(0) as u32 {
                let rf = def + 0x28 + k * 0x10;
                let Some(pg) = r.program(rf) else { continue };
                if let Some(g) = pg.group {
                    g(r, s, def, rf, k);
                    continue;
                }
                let skip_second = r.m.u32(def + 0xc) & 1 != 0;
                let stride = u32::from(r.m.u16(rf + 4));
                let mut o = r.m.u32(s + 4);
                for _ in 0..count {
                    let slot_at = o.wrapping_add(r.m.u32(def + 0x20)).wrapping_add(k * 4);
                    let mut key = r.m.u32(slot_at);
                    let next = key.wrapping_add(stride);
                    if r.m.u32(next) == r.m.u32(o + 0xc) {
                        r.m.set_u32(slot_at, next);
                        key = next;
                    }
                    let after = r.m.u32(o);
                    if let Some(f) = pg.update {
                        f(r, o, key, rf);
                    }
                    if !skip_second && let Some(f) = pg.second {
                        f(r, o, key, rf);
                    }
                    o = after;
                }
            }
            if !r.paused {
                let mut o = r.m.u32(s + 4);
                for _ in 0..count {
                    let age = r.m.u32(o + 0xc);
                    r.m.set_u32(o + 0xc, age.wrapping_add(0x1000));
                    o = r.m.u32(o);
                }
            }
        }
        if r.paused {
            return;
        }
        let mut ends: u32 = 0;
        let mut link: Option<u32> = None;
        let mut o = r.objects;
        while o != 0 {
            let def = r.m.u32(r.m.u32(o + 8));
            let next = r.m.u32(o);
            let age = r.m.u32(o + 0xc);
            if !self.drained && (r.m.u32(def + 0x1c) as i32) <= age as i32 {
                r.m.set_u32(o + 0xc, r.m.u32(def + 0x18));
                init_slots(r, o, |pg| pg.restart);
                link = Some(o);
            } else {
                let end = r.m.u32(def + 0x14);
                ends |= end;
                if age < end {
                    link = Some(o);
                } else {
                    match link {
                        Some(l) => r.m.set_u32(l, next),
                        None => r.objects = next,
                    }
                    free_object(r, o);
                }
            }
            o = next;
        }
        if self.drained && ends == 0x7000_0000 {
            let mut o = r.objects;
            while o != 0 {
                let next = r.m.u32(o);
                free_object(r, o);
                o = next;
            }
            r.objects = 0;
        }
    }

    /// 711f20: every object freed, the slots back to the pool.
    pub fn free(&mut self, r: &mut Run) {
        r.objects = self.objects;
        let mut o = r.objects;
        while o != 0 {
            let next = r.m.u32(o);
            free_object(r, o);
            o = next;
        }
        r.objects = 0;
        self.objects = 0;
        if self.slots != 0 {
            r.env.free(r.m, self.slots);
            self.slots = 0;
        }
    }
}
