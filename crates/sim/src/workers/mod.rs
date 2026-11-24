//! The field workers as the original's engine keeps them: one record per worker with the original's layout, holding
//! each level's move and turn records, the position block, the engine flags and the bindings, and the engine
//! functions translated on those records. The script machine keeps its own state (code, stack, requests); this
//! is the part the natives and the per-frame engine step read and write.
//!
//! Offsets are bytes from a record's start; a record address kept inside a record (a level's current move or turn
//! record) is such an offset.
//!
//! Record:
//! - +0x2e id, +0x32 running level, +0x34 engine flags (0x80 the engine steps it, 0x20 attached: its direction and
//!   facing follow the character), +0x36 second flags (bit 0 cleared each step, bits 1–2 the side of a line
//!   crossing, 8 no talk), +0x38 placement flags (bit 0: put the character on the worker's position before the
//!   next run, bit 2: take the facing from the character).
//! - +0x3c camera handle, +0x40 −1, +0x44 the worker's own signal entry table (0 the kind's), +0x94 run rate.
//! - +0x9c the bound character (non-zero when bound), +0xa8 bound id, +0xaa bind kind (1 character, 2 camera,
//!   3 none, 4 bound object, 5 camera id), +0xab signal mask (bit n lets signal n through).
//! - +0x12c + level·0x4c the level contexts: +0x44 the level's move record, +0x48 its turn record.
//! - +0x284 (kinds 5, 6) or +0x558 (others) the position block, see [`P`].
//! - +0x5b8 + level·0x4c move records, +0x864 + level·0x44 turn records (kind 1).
//! - +0xaf0 a value (00cf/00d0), +0xaf4.. attachments, +0xb16 the frame threshold of move mode 9.

// Negated comparisons are kept as the original writes them: they differ from the plain ones for NaN.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub mod engine;
pub mod math;
pub mod moves;
pub mod setup;

/// The size every record gets (the character kind's, the largest).
pub const SIZE: u32 = 0xb58;
pub const ID: u32 = 0x2e;
pub const LEVEL: u32 = 0x32;
pub const FLAGS: u32 = 0x34;
pub const FLAGS2: u32 = 0x36;
pub const PLACE: u32 = 0x38;
pub const CAMERA: u32 = 0x3c;
pub const SIGNAL_TABLE: u32 = 0x44;
pub const CHR: u32 = 0x9c;
pub const BIND_ID: u32 = 0xa8;
pub const BIND: u32 = 0xaa;
pub const SIGNAL_MASK: u32 = 0xab;
pub const CONTEXTS: u32 = 0x12c;
pub const CONTEXT: u32 = 0x4c;
/// A context's move and turn record (offsets in the record).
pub const CTX_MOVE: u32 = 0x44;
pub const CTX_ROT: u32 = 0x48;
pub const POS_SMALL: u32 = 0x284;
pub const POS_FULL: u32 = 0x558;
pub const MOVES: u32 = 0x5b8;
pub const MOVE: u32 = 0x4c;
pub const ROTS: u32 = 0x864;
pub const ROT: u32 = 0x44;
pub const MODE9_FRAMES: u32 = 0xb16;

/// Position block fields (offsets from the block).
pub mod p {
    pub const X: u32 = 0;
    pub const Y: u32 = 4;
    pub const Z: u32 = 8;
    /// The distance covered this frame.
    pub const SPEED: u32 = 0xc;
    /// The position before the last placement.
    pub const PREV: u32 = 0x10;
    /// Height range of touching (characters 5, others −1: none).
    pub const HEIGHT: u32 = 0x1c;
    /// The heading and pitch the worker moves along.
    pub const DIR_YAW: u32 = 0x20;
    pub const DIR_PITCH: u32 = 0x24;
    /// Kind 5: the command; kind 6: the worker tested against (−1 the player).
    pub const TARGET: u32 = 0x28;
    /// Height range of talking (−1 none).
    pub const TALK_HEIGHT: u32 = 0x2c;
    pub const PITCH: u32 = 0x30;
    pub const YAW: u32 = 0x34;
    pub const ROLL: u32 = 0x38;
    /// Half angle of talking.
    pub const TALK_ANGLE: u32 = 0x3c;
    pub const TALK_RADIUS: u32 = 0x40;
    pub const R44: u32 = 0x44;
    /// Touch radius (boxes: half width), and the box's second half extent.
    pub const RADIUS: u32 = 0x48;
    pub const R4C: u32 = 0x4c;
}

/// Move record fields.
pub mod mv {
    pub const ID: u32 = 0;
    pub const MODE: u32 = 2;
    pub const EASE: u32 = 4;
    pub const COUNT: u32 = 6;
    pub const T: u32 = 8;
    pub const TOTAL: u32 = 0xc;
    pub const SPEED: u32 = 0x10;
    pub const F14: u32 = 0x14;
    pub const F18: u32 = 0x18;
    pub const START: u32 = 0x1c;
    pub const TARGET: u32 = 0x28;
    pub const F34: u32 = 0x34;
    pub const RADIUS: u32 = 0x38;
    pub const TURN_YAW: u32 = 0x3c;
    pub const TURN_PITCH: u32 = 0x40;
    pub const F44: u32 = 0x44;
    pub const F48: u32 = 0x48;
}

/// Turn record fields.
pub mod rt {
    pub const ID: u32 = 0;
    pub const MODE: u32 = 2;
    pub const EASE: u32 = 4;
    pub const COUNT: u32 = 6;
    pub const T: u32 = 8;
    pub const TOTAL: u32 = 0xc;
    pub const SPEED: u32 = 0x10;
    pub const SPEED2: u32 = 0x14;
    pub const PITCH_SPEED: u32 = 0x18;
    pub const ROLL_SPEED: u32 = 0x1c;
    pub const TARGET: u32 = 0x20;
    pub const DELTA: u32 = 0x2c;
}

/// One worker's record.
#[derive(Clone, PartialEq)]
pub struct Worker {
    bytes: Vec<u8>,
    /// The worker's kind (its descriptor's first byte).
    pub kind: u8,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker").field("kind", &self.kind).finish_non_exhaustive()
    }
}

macro_rules! access {
    ($get:ident, $set:ident, $t:ty, $n:expr) => {
        pub fn $get(&self, a: u32) -> $t {
            let o = a as usize;
            <$t>::from_le_bytes(self.bytes[o..o + $n].try_into().unwrap())
        }
        pub fn $set(&mut self, a: u32, v: $t) {
            let o = a as usize;
            self.bytes[o..o + $n].copy_from_slice(&v.to_le_bytes());
        }
    };
}

impl Worker {
    pub fn new(kind: u8, id: u16) -> Worker {
        let mut w = Worker { bytes: vec![0; SIZE as usize], kind };
        w.set_u16(ID, id);
        w
    }

    access!(u8, set_u8, u8, 1);
    access!(i8, set_i8, i8, 1);
    access!(u16, set_u16, u16, 2);
    access!(i16, set_i16, i16, 2);
    access!(u32, set_u32, u32, 4);
    access!(i32, set_i32, i32, 4);
    access!(f32, set_f32, f32, 4);

    pub fn id(&self) -> u16 {
        self.u16(ID)
    }

    pub fn level(&self) -> u8 {
        self.u8(LEVEL)
    }

    /// Kinds 5 and 6 have two levels, the others nine.
    pub fn two_levels(&self) -> bool {
        matches!(self.kind, 5 | 6)
    }

    /// A level clamped to the kind's levels (below 0: 0).
    pub fn slot(&self, level: i32) -> u32 {
        let l = level.max(0) as u32;
        l.min(if self.two_levels() { 1 } else { 8 })
    }

    /// A level's context.
    pub fn ctx(&self, level: i32) -> u32 {
        CONTEXTS + self.slot(level) * CONTEXT
    }

    /// The running level's context.
    pub fn cur_ctx(&self) -> u32 {
        self.ctx(i32::from(self.level()))
    }

    /// A level's current move and turn record.
    pub fn move_of(&self, level: i32) -> u32 {
        self.u32(self.ctx(level) + CTX_MOVE)
    }

    pub fn rot_of(&self, level: i32) -> u32 {
        self.u32(self.ctx(level) + CTX_ROT)
    }

    /// The position block (kind 4 has none).
    pub fn pos(&self) -> Option<u32> {
        match self.kind {
            4 => None,
            5 | 6 => Some(POS_SMALL),
            _ => Some(POS_FULL),
        }
    }

    /// The position block's base, for code that reads it unconditionally (kind 4 reads the full kind's place).
    pub fn pb(&self) -> u32 {
        self.pos().unwrap_or(POS_FULL)
    }

    pub fn position(&self) -> [f32; 3] {
        let b = self.pb();
        [self.f32(b), self.f32(b + 4), self.f32(b + 8)]
    }

    pub fn set_position(&mut self, v: [f32; 3]) {
        let b = self.pb();
        self.set_f32(b, v[0]);
        self.set_f32(b + 4, v[1]);
        self.set_f32(b + 8, v[2]);
    }

    pub fn pf(&self, field: u32) -> f32 {
        self.f32(self.pb() + field)
    }

    pub fn set_pf(&mut self, field: u32, v: f32) {
        let b = self.pb();
        self.set_f32(b + field, v);
    }

    pub fn flags(&self) -> u16 {
        self.u16(FLAGS)
    }

    pub fn flags2(&self) -> u16 {
        self.u16(FLAGS2)
    }

    pub fn bind(&self) -> u8 {
        self.u8(BIND)
    }

    /// Bound to a character (bind kind 1 with a character).
    pub fn chr_bound(&self) -> bool {
        self.bind() == 1 && self.u32(CHR) != 0
    }

    /// Copy `n` bytes inside the record.
    pub fn copy(&mut self, to: u32, from: u32, n: u32) {
        let (t, f) = (to as usize, from as usize);
        self.bytes.copy_within(f..f + n as usize, t);
    }
}

/// The event system's own block the engine reads (the first of the original's system blocks): the player and
/// the talk target the frame picks.
#[derive(Debug, Clone, PartialEq)]
pub struct System {
    /// The player's worker (−1 none).
    pub player: i16,
    /// The player's position and where it was before the last placement (w 1).
    pub player_pos: [f32; 4],
    pub player_prev: [f32; 4],
    /// Talking this frame (1), with the talk signal type and the nearest worker found (−1 none) at a distance².
    pub talk_mode: u8,
    pub talk_signal: u8,
    pub talk_target: i16,
    pub talk_dist: f32,
    /// The player's talk reach.
    pub talk_reach: f32,
    /// Flags: bit 4 a worker was placed this frame (cleared after the workers ran).
    pub flags2: u8,
    /// The mode bits (1, 2) that hold workers.
    pub mode3: u8,
    /// The first flags byte (bit 0 on; bit 7 set by 00be) and the +0x210 word (bit 0: 00aa).
    pub flags0: u8,
    /// Byte 1 (bit 4 blocks control) and the script mode word (+0x1a of the current block).
    pub flags1: u8,
    pub mode1a: u16,
    pub flags210: u32,
    /// Every joined member is free (the party code's global flag) and the flag 00ca(0xf) sets.
    pub full_member: u8,
    pub party_flag_00ca: u8,
    /// The value 0212 sets (+0x208).
    pub value208: f32,
    /// The switch mask 0220 sets bits of.
    pub mask_0220: u32,
    /// The value 025d keeps.
    pub value_025d: i32,
    /// The mask 018a/018b set bits of, and the three values 014a keeps.
    pub mask_018a: u32,
    pub battle_return_params: [i32; 3],
    /// The worker each body slot names (0158).
    pub body_slots: [i32; 16],
    /// Frames run since the setup ended (+500).
    pub frames: i32,
    /// c009's value and c051's switch.
    pub value_c009: u8,
    pub switch_c051: u8,
    /// The event effect mask (00db clears it, 017b clears a bit).
    pub effect_mask: u32,
    /// A byte the battle code sets (7035 reads it).
    pub battle_byte_7035: u8,
    /// The event effect flag (00db raises it, the effect's end lowers it) and a byte 01bf sets.
    pub effect_up: i32,
    pub byte_01bf: u8,
    /// The system block's +0x1a (0158/0159 do nothing while it is set).
    pub slot_lock: i16,
    /// The four values 01c0 sets and the four 022e sets.
    pub values_01c0: [i32; 4],
    pub values_022e: [i32; 4],
}

/// The block as the original's setup leaves it, turned on: byte 0 0x80 | 1, byte 1 0x98, byte 2
/// 0xc8, byte 3 0x30, the +0x210 word 0x49.
impl Default for System {
    fn default() -> Self {
        System {
            player: -1,
            flags1: 0x98,
            mode1a: 0,
            player_pos: [0.0, 0.0, 0.0, 1.0],
            player_prev: [0.0, 0.0, 0.0, 1.0],
            talk_mode: 0,
            talk_signal: 0,
            talk_target: -1,
            talk_dist: 0.0,
            talk_reach: 0.0,
            flags2: 0xc8,
            mode3: 0x30,
            flags0: 0x81,
            flags210: 0x49,
            full_member: 0,
            party_flag_00ca: 0,
            value208: 0.0,
            mask_0220: 0,
            value_025d: 0,
            mask_018a: 0,
            battle_return_params: [0; 3],
            body_slots: [0; 16],
            frames: 0,
            value_c009: 0,
            switch_c051: 0,
            effect_mask: 0,
            battle_byte_7035: 0,
            effect_up: 0,
            byte_01bf: 0,
            slot_lock: 0,
            values_01c0: [0; 4],
            values_022e: [0; 4],
        }
    }
}

/// The field's workers and the system block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workers {
    pub list: Vec<Worker>,
    pub sys: System,
}

impl Workers {
    pub fn get(&self, w: usize) -> Option<&Worker> {
        self.list.get(w)
    }

    pub fn get_mut(&mut self, w: usize) -> Option<&mut Worker> {
        self.list.get_mut(w)
    }
}
