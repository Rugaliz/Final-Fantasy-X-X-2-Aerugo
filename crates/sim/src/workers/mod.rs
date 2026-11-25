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

/// Bits of the system block's state byte (+0).
pub mod state {
    /// The block runs.
    pub const ON: u8 = 1;
    /// The player has control.
    pub const CONTROL_ON: u8 = 2;
    /// A confirm press reached a window waiting on it (it lasts the frame).
    pub const ADVANCE_PRESSED: u8 = 4;
    /// The frame count runs (the event's background sounds step only then).
    pub const COUNT_FRAMES: u8 = 8;
    /// Cleared before the workers every frame; nothing found sets or reads it.
    pub const CLEARED_EACH_FRAME: u8 = 0x10;
    /// Control takes the workers' alternate switch (+0x36 bit 3) rather than their switch (+0x34 bit 9).
    pub const ALTERNATE_SWITCH: u8 = 0x20;
    /// A map change may be asked for (00be opens it; a change closes it).
    pub const MAP_CHANGE_OPEN: u8 = 0x80;
}

/// Bits of the system block's message byte (+1).
pub mod message {
    /// A window with choices is up: control is held off (and the main menu and the soft reset).
    pub const MESSAGE_UP: u8 = 4;
    /// Message-up follows the windows (00e5); without it the windows clear it.
    pub const WINDOWS_HOLD_CONTROL: u8 = 8;
    /// A switch the scripts keep (00e9 sets it, 00ea reads it; nothing else does).
    pub const SCRIPT_SWITCH: u8 = 0x10;
}

/// Bits of the system block's scene byte (+2).
pub mod scene {
    /// The camera water check runs (0212 / c02f): under water when the camera is below the surface height.
    pub const CAMERA_WATER_CHECK: u8 = 1;
    /// With [`FORCE_UNDERWATER`], whether the scene counts as under water (0114).
    pub const FORCED_UNDERWATER_VALUE: u8 = 2;
    /// The under-water state is forced rather than the camera water check's (0114).
    pub const FORCE_UNDERWATER: u8 = 4;
    /// Random encounters may start (0115; and only after the block's first frames).
    pub const ENCOUNTERS_ON: u8 = 8;
    /// A worker was placed this frame (cleared after the workers ran).
    pub const PLAYER_PLACED: u8 = 0x10;
    /// Party characters earn the per-event points when an event starts (012b).
    pub const EVENT_CREDIT_ON: u8 = 0x40;
    /// The switch 0149 sets: nothing reads it.
    pub const UNREAD_SWITCH: u8 = 0x80;
}

/// Bits of the system block's holds byte (+3).
pub mod holds {
    /// What the battle end left (the low two bits: 1 a random battle, 2 a scripted one); workers are held.
    pub const BATTLE_END: u8 = 3;
    /// A system message window (015b's treasure window) is open.
    pub const SYSTEM_WINDOW: u8 = 4;
    /// The player has no control (message up, or control off): the windows' step keeps it.
    pub const NO_CONTROL: u8 = 0x10;
    /// The switches 015d and 015e set, in that order: nothing reads them.
    pub const UNREAD_SWITCHES: [u8; 2] = [8, 0x40];
    /// The battle music set-up holds control.
    pub const BATTLE_MUSIC: u8 = 0x80;
}

/// Bits of the system block's play word (+0x210): what the player may do.
pub mod play {
    /// The player's moved distance counts (00aa; with the save's walk flags, unless the menu just opened).
    pub const COUNT_DISTANCE: u32 = 1;
    /// A map change has been asked for (no soft reset, the battle status steps it).
    pub const MAP_CHANGE_ASKED: u32 = 2;
    /// The main menu opened this frame (AtelCtrl clears it each frame).
    pub const MENU_OPENED: u32 = 4;
    /// The main menu may open (0241).
    pub const MENU_ALLOWED: u32 = 8;
    /// The save data has been checked (01c5 clears it while it reads): the menu may open, the title's prompt
    /// blinks.
    pub const SAVE_CHECKED: u32 = 0x10;
    /// The soft reset is allowed (024d).
    pub const SOFT_RESET_ALLOWED: u32 = 0x40;
}

/// Set or clear `bit` of `flags`.
pub fn set_flag<T>(flags: &mut T, bit: T, on: bool)
where
    T: Copy + std::ops::BitOr<Output = T> + std::ops::BitAnd<Output = T> + std::ops::Not<Output = T>,
{
    *flags = if on { *flags | bit } else { *flags & !bit };
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
    /// The scene flags (byte 2; [`scene`] names the bits).
    pub scene: u8,
    /// What holds the workers and control (byte 3; [`holds`] names the bits).
    pub holds: u8,
    /// The state flags (byte 0; [`state`]) and what the player may do (+0x210; [`play`]).
    pub state: u8,
    /// The message flags (byte 1; [`message`]) and the script mode (+0x1a: 0 lets party changes reach the
    /// bodies and the slot table, and the windows decide message-up).
    pub message: u8,
    pub script_mode: u16,
    pub play: u32,
    /// Every joined member is free (the party code's global flag).
    pub full_member: u8,
    /// The names still to enter after character 0xf joins (00ca sets 3; the name entry asks for characters
    /// 0xf–0x11 one after another, counting down).
    pub sister_names_left: u8,
    /// The water surface height the camera water check compares the camera with (0212 / c02f).
    pub water_surface_height: f32,
    /// Party characters (bit per character below 8; 0220 sets and clears them) left out of the points an event
    /// adds once to each party character's save tally when it starts (AtelGetNowEventJumpId).
    pub members_without_event_credit: u32,
    /// The Al Bhed letters known (025d sets the mask the game calls g_albhed; the text shows those letters
    /// translated).
    pub al_bhed_letters: i32,
    /// Main-menu commands held off (018a sets a bit, 018b clears it); the main command window offers command
    /// 0x40 only while bit 6 is clear. And the three values 014a keeps.
    pub menu_commands_off: u32,
    pub battle_return_params: [i32; 3],
    /// The worker each body slot names (0158).
    pub body_slots: [i32; 16],
    /// Frames run since the setup ended (+500).
    pub frames: i32,
    /// The development environment (c009: 0 map, 1 event, 2 operation check, 3 sound, 4 motion; the debug
    /// screen shows it and the event files' path follows it) and the development switch (c051: while on, the
    /// main menu cannot open and the development host's queries answer instead of the PC's stubs).
    pub development_environment: u8,
    pub development_mode: u8,
    /// The event effect mask (00db clears it, 017b clears a bit).
    pub effect_mask: u32,
    /// The battle voice's standby state (MsVoiceStanby keeps it; 7035 reads it).
    pub voice_standby: u8,
    /// The event effect flag (00db raises it, the effect's end lowers it).
    pub effect_up: i32,
    /// After a battle, character 3's body keeps its model parts as they are (01bf; otherwise part 1 is hidden
    /// and part 2 shown).
    pub keep_parts_after_battle: u8,
    /// The system block's +0x1a (0158/0159 do nothing while it is set).
    pub slot_lock: i16,
    /// The four effect parameters 01c0 sets (only the debug screen shows them, as EffPm 0–3) and the four sounds
    /// 022e names (−1 none) whose volume the event drops under water (AtelEventExecBGVolumeInWater).
    pub effect_params: [i32; 4],
    pub underwater_sounds: [i32; 4],
}

/// The block as the original's setup leaves it, turned on: byte 0 0x80 | 1, byte 1 0x98, byte 2
/// 0xc8, byte 3 0x30, the +0x210 word 0x49.
impl Default for System {
    fn default() -> Self {
        System {
            player: -1,
            message: 0x98,
            script_mode: 0,
            player_pos: [0.0, 0.0, 0.0, 1.0],
            player_prev: [0.0, 0.0, 0.0, 1.0],
            talk_mode: 0,
            talk_signal: 0,
            talk_target: -1,
            talk_dist: 0.0,
            talk_reach: 0.0,
            scene: 0xc8,
            holds: 0x30,
            state: 0x81,
            play: 0x49,
            full_member: 0,
            sister_names_left: 0,
            water_surface_height: 0.0,
            members_without_event_credit: 0,
            al_bhed_letters: 0,
            menu_commands_off: 0,
            battle_return_params: [0; 3],
            body_slots: [0; 16],
            frames: 0,
            development_environment: 0,
            development_mode: 0,
            effect_mask: 0,
            voice_standby: 0,
            effect_up: 0,
            keep_parts_after_battle: 0,
            slot_lock: 0,
            effect_params: [0; 4],
            underwater_sounds: [0; 4],
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
