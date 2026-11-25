//! The battle character records as the original keeps them (btl_get_chr): 31 full records of 0xf90 bytes (slots
//! 0–0x11 the characters and aeons in save order, 0x14–0x1b the formation's monsters, the rest spare), 62 short
//! records of 0x390 bytes (slots 0x1f–0x5c, which only the record head of the battle set-up touches) and one
//! spare record every other slot reads.
//!
//! Offsets are bytes from a record's start. Fields whose role is traced have names here; the set-up's other
//! defaults are kept as the original writes them (see [`crate::battle::setup`]) until their readers are ported.

/// Full records.
pub const FULL_SLOTS: usize = 0x1f;
pub const FULL_SIZE: usize = 0xf90;
/// Short records (slots 0x1f–0x5c).
pub const SHORT_SLOTS: usize = 0x5d - FULL_SLOTS;
pub const SHORT_SIZE: usize = 0x390;
/// Every slot below this has a record of its own.
pub const SLOTS: usize = 0x5d;
/// The characters and aeons (save order: 0–7 characters, 8–0x11 aeons).
pub const PARTY_SLOTS: usize = 0x12;
/// The first monster slot, and how many there are.
pub const FIRST_MONSTER: usize = 0x14;
pub const MONSTERS: usize = 8;

/// The slot's number (+0xc).
pub const SLOT: usize = 0xc;
/// Who holds the slot (+0xe): a character or aeon number, 0x1000 | monster number, 0xffff nobody.
pub const CHARACTER: usize = 0xe;
/// Taking part in this battle (+0x10).
pub const IN_BATTLE: usize = 0x10;
/// Whether the monster reads its own file (+0x11, 1) or shares an earlier slot's (0; +0x14 then names that
/// slot).
pub const OWN_FILE: usize = 0x11;
pub const FILE_SLOT: usize = 0x14;
/// Present (+0x24): the monster read checks it before reading a file.
pub const PRESENT: usize = 0x24;
/// The record is set up for the battle's display (+0x38, MsBtlReadMonsterProcess).
pub const DISPLAY_READY: usize = 0x38;
/// The models (+4: the model number, +8 the battle model; a monster's from its stats, +0x74).
pub const MODEL: usize = 0x4;
pub const BATTLE_MODEL: usize = 0x8;
/// The monster file the slot reads (+0x48 in the original a pointer; here the index of the read file + 1, 0
/// none).
pub const MONSTER_FILE: usize = 0x48;
/// A monster's texts from the monster tables: name (0x40 bytes), the Sensor text (0x100), the Scan text
/// (0x200).
pub const MONSTER_NAME: usize = 0x50;
pub const MONSTER_NAME_SIZE: usize = 0x40;
pub const SENSOR_TEXT: usize = 0x90;
pub const SENSOR_TEXT_SIZE: usize = 0x100;
pub const SCAN_TEXT: usize = 0x190;
pub const SCAN_TEXT_SIZE: usize = 0x200;
/// The kind of combatant (+0x4fd): 3 character, 4 aeon, 5 monster.
pub const KIND: usize = 0x4fd;
pub const KIND_CHARACTER: u8 = 3;
pub const KIND_AEON: u8 = 4;
pub const KIND_MONSTER: u8 = 5;
/// The order number (+0x4fe): a character's place in the front line, an aeon's number, a monster's place in
/// the formation.
pub const ORDER: usize = 0x4fe;

/// The battle parameter block (+0x540, 0x1ec bytes): what the stat natives read and the formulas use.
pub const PARAMS: usize = 0x540;
pub const PARAMS_SIZE: usize = 0x1ec;
/// The name in battle (+0x540: a character's 0x14 bytes from the save, a monster's 0x28 from +0x50).
pub const NAME: usize = 0x540;
/// The ply_rom record's flags (+0x590; bit 2 an aeon).
pub const ROM_FLAGS: usize = 0x590;
/// The save record's party bits (+0x591).
pub const PARTY_FLAGS: usize = 0x591;
/// The equipment worn (+0x592 weapon, +0x593 armour: equipment slots, 0xff none; a monster 0xffff).
pub const WEAPON: usize = 0x592;
pub const ARMOUR: usize = 0x593;
/// Maximum HP and MP (+0x594 / +0x598), and the maximums before the battle's changes (+0x59c / +0x5a0).
pub const MAX_HP: usize = 0x594;
pub const MAX_MP: usize = 0x598;
pub const BASE_MAX_HP: usize = 0x59c;
pub const BASE_MAX_MP: usize = 0x5a0;
/// +0x5a4: the save record's +0x24 for a character, a monster's stats +0x1c (also kept at +0xf60).
pub const OVERKILL_OR_BASE: usize = 0x5a4;
/// The stats (+0x5a8..+0x5af: strength, defence, magic, magic defence, agility, luck, evasion, accuracy) and
/// what the battle adds to them (+0x5b0..+0x5b7).
pub const STATS: usize = 0x5a8;
pub const STAT_BONUSES: usize = 0x5b0;
pub const STAT_COUNT: usize = 8;
/// +0x5b8 the character flags (a monster's from its stats +0x28; a character gets bit 9).
pub const FLAGS: usize = 0x5b8;
/// The weapon's damage kind (+0x5c1) and critical bonus (+0x5c7).
pub const WEAPON_FORMULA: usize = 0x5c1;
pub const WEAPON_CRITICAL: usize = 0x5c7;
/// The record's own slot as the kernel sees it (+0x5c2; a monster's from its stats +0x76) and its copy number
/// (+0x5c3).
pub const KERNEL_SLOT: usize = 0x5c2;
pub const COPY_NUMBER: usize = 0x5c3;
/// HP and MP now (+0x5d0 / +0x5d4), and at the battle's start (+0x6e4 / +0x6e8).
pub const HP: usize = 0x5d0;
pub const MP: usize = 0x5d4;
pub const START_HP: usize = 0x6e4;
pub const START_MP: usize = 0x6e8;
/// What the equipment's auto-abilities add up to (+0x5d8: the weapon's power bonus; +0x5d9..+0x5dd the
/// abilities' bits; +0x5de 0x19 and +0x5f7 0xd sums; +0x604 a word).
pub const EQUIPMENT_POWER: usize = 0x5d8;
pub const ABILITY_BITS: usize = 0x5d9;
pub const ABILITY_SUMS: usize = 0x5de;
pub const ABILITY_SUMS_COUNT: usize = 0x19;
pub const ABILITY_SUMS2: usize = 0x5f7;
pub const ABILITY_SUMS2_COUNT: usize = 0xd;
pub const ABILITY_WORD: usize = 0x604;
/// The status bits (+0x606; bit 0 knocked out).
pub const STATUS: usize = 0x606;
pub const STATUS_KO: u16 = 1;
/// Auto-statuses from the equipment (+0x630..+0x634), or the second set (+0x636..+0x63a, with +0x63c set) when
/// the ability's flags +0x10 bit 0 are set; a monster's from its stats +0x48..+0x4c.
pub const AUTO_STATUS: usize = 0x630;
pub const AUTO_STATUS2: usize = 0x636;
pub const AUTO_STATUS2_ON: usize = 0x63c;
/// The resistances (+0x641, 0x19 bytes).
pub const RESISTANCES: usize = 0x641;
pub const RESISTANCE_COUNT: usize = 0x19;
/// +0x65a a word the abilities OR in (a monster's stats +0x4e).
pub const RESIST_WORD: usize = 0x65a;
/// The commands a character has learned (+0x664, six words from the save record +0x3e) and the shared ones
/// (+0x670, sixteen words from save +0x3d6c).
pub const LEARNED: usize = 0x664;
pub const SHARED_LEARNED: usize = 0x670;
/// +0x6bc..+0x6c0 words the abilities OR in.
pub const ABILITY_WORDS: usize = 0x6bc;
/// The first commands (+0x6c6, +0x6c8).
pub const FIRST_COMMAND: usize = 0x6c6;
pub const SECOND_COMMAND: usize = 0x6c8;
/// The turn order: the ticks left before the record's turn (+0x65c) and the ticks of a turn ×3 (+0x65d, from
/// ctb_base by agility).
pub const CTB: usize = 0x65c;
pub const CTB_TURN: usize = 0x65d;
/// Status counters that halve (+0x613) or double (+0x614) the first wait.
pub const HALVES_WAIT: usize = 0x613;
pub const DOUBLES_WAIT: usize = 0x614;
/// Agility, one of [`STATS`].
pub const AGILITY: usize = STATS + 4;
/// The motion parameters the debug editors (CHR data 1 / 2) label: +0x4ac the move flags (bits: 1 ATTACK Run
/// 00+01, 2 Run 01, 4 Miss Motion, 8 Return, 0x10 Pos Check, 0x20 AVOID Move, 0x100 MOVE Back Jump, 0x200 MOVE
/// Avoid, 0x400 TURN Stay, 0x800 TURN Run, 0x1000 ATTACK Stay Attack, 0x2000 RETURN Wait, 0x4000 AVOID
/// Disable, 0x8000 MOVE Disable Move Chest, 0x10000 ATTACK Weight Inf).
pub const MOVE_FLAGS: usize = 0x4ac;
pub const ROUND_FRAME: usize = 0x4b0;
pub const ROUND_RETURN_FRAME: usize = 0x4b2;
pub const IDLE2_CHANCE: usize = 0x4b8;
pub const ATTACK_INC_SPEED: usize = 0x4b9;
pub const ATTACK_DEC_SPEED: usize = 0x4ba;
/// "HOKAN : Type" (the motion interpolation type).
pub const BLEND_TYPE: usize = 0x4bb;
pub const ATTACK_START_DISTANCE: usize = 0x4bc;
pub const ATTACK_OFFSET: usize = 0x4c0;
pub const ATTACK_HEIGHT: usize = 0x4c4;
pub const BACK_JUMP_DISTANCE: usize = 0x4c8;
pub const RUN_FIRST_SPEED: usize = 0x4cc;
pub const RUN_ACCELERATION: usize = 0x4d0;
pub const RUN_SPEED: usize = 0x4d4;
pub const RUN_RETURN_SPEED: usize = 0x4d8;
pub const WEIGHT: usize = 0x4dc;
pub const ATTACK_NORMAL_FRAME: usize = 0x40c;
pub const ATTACK_NEAR_FRAME: usize = 0x40d;
pub const ATTACK_MOTION_FRAME: usize = 0x40e;
/// "DIR : Return" and "DIR : Fix".
pub const DIRECTION_RETURN: usize = 0x41d;
pub const DIRECTION_FIXED: usize = 0x41b;
/// What the scripts set for the record's position centre (7029; MsGetChr*Pos take the model's own centre
/// while it is clear, outside special battles).
pub const CENTER_SET: usize = 0x505;
/// The percentage of maximum HP poison takes (MsPoisonProcess; save record +0x37).
pub const POISON_PERCENT: usize = 0x5ba;
/// The overdrive mode (save +0x38), charge (+0x39) and its maximum (+0x3a) (MsLimitTypeStatusCheck,
/// MsLimitUp).
pub const OVERDRIVE_MODE: usize = 0x5bb;
pub const OVERDRIVE: usize = 0x5bc;
pub const OVERDRIVE_MAX: usize = 0x5bd;
/// The look of the record when it falls (MsSetDeath → MsStoneProcess; characters 0, monsters 0x303) and the
/// one revival restores it from (MsRelife).
pub const DEATH_LOOK: usize = 0x3f5;
pub const DEATH_LOOK_KEPT: usize = 0x3f6;
/// The model kind (8 a character or aeon, 3 a monster, 4 one exchanged in).
pub const MODEL_KIND: usize = 0x43d;
/// The second status word (+0x616: bit 9 eternal life, bit 8 tested at +0x617 bit 0).
pub const STATUS2: usize = 0x616;
/// The status counters (+0x608..+0x614, 13).
pub const STATUS_COUNTERS: usize = 0x608;
/// The weak state shown (+0x63d) and the weak level (+0x63e, MsCalcWeakLevel).
pub const WEAK_STATE: usize = 0x63d;
pub const WEAK_LEVEL: usize = 0x63e;
/// The record's command flags (one bit per command 0x3nnn, btl_set_cmd_flag).
pub const COMMAND_FLAGS: usize = 0x690;
/// The targets the record's command chose (btl_set_targets) and a second target set the scripts name (-0x1a).
pub const TARGETS: usize = 0xdc0;
pub const OTHER_TARGETS: usize = 0xdc4;
/// Escaped from the battle (MsSetEscapeFlag).
pub const ESCAPED: usize = 0xdcd;
/// In its falling sequence (MsSetDeath, MsCheckDeathMotion).
pub const FALLING: usize = 0xdce;
/// The slot of whoever damaged the record last (MsSetDamage).
pub const LAST_ATTACKER: usize = 0xded;
/// Running a command now (the acting record's; target expressions -3 / -2).
pub const COMMAND_RUNNING: usize = 0xdf0;
/// The model point the target cursor sits on (MsCalcCursorPos: Ch_OptposGet).
pub const CURSOR_POINT: usize = 0xdf9;
/// A character slot's commands on or off (btl_set_cmd_flag with a character id).
pub const COMMANDS_ON: usize = 0xdd4;
/// The record's height in battle (+0x534) and how it is found (+0x50c: 0 the base height less half the
/// model's height plus the offset +0x538, 1 none; 7ab300).
pub const HEIGHT: usize = 0x534;
pub const HEIGHT_MODE: usize = 0x50c;
pub const HEIGHT_OFFSET: usize = 0x538;
/// A model float the scripts set (+0x4e4, given to the model's +0x4e4: 0 or less takes the model's own; +0x44c
/// marks it set).
pub const MODEL_PARAMETER: usize = 0x4e4;
pub const MODEL_PARAMETER_SET: usize = 0x44c;
/// Values only the scripts read and write (no engine reader found): a signed byte (+0x4fc) and a bit (+0x4b4).
pub const SCRIPT_VALUE: usize = 0x4fc;
pub const SCRIPT_BIT: usize = 0x4b4;
/// The record's facing (+0x514), the facing it returns to (+0x518) and whether a script set them (+0x512).
pub const DIRECTION: usize = 0x514;
pub const HOME_DIRECTION: usize = 0x518;
pub const DIRECTION_SET: usize = 0x512;
/// The model's gravity mode (+0x34, given to Ch_SetGravMode).
pub const GRAVITY_MODE: usize = 0x34;
/// A model switch the record's weapons copy with +0x2b, +0x2e and +0x34 (MsBtlWeaponRegist).
pub const MODEL_SWITCH: usize = 0x32;
/// The model's scale (+0x3a0..+0x3a8, 1.0 at the start; 7028 also gives it to the model, 0082b560 mode 7).
pub const SCALE: usize = 0x3a0;
/// On the battlefield (+0xdc8).
pub const ACTIVE: usize = 0xdc8;
/// The record's death state (+0xdcc: 2 fallen; MsCheckDeathMotion).
pub const DEATH_STATE: usize = 0xdcc;
/// A monster's file read order (+0xdf5) and the first byte of its file's chunk 1 (+0xdf6).
pub const FILE_ORDER: usize = 0xdf5;
pub const MONSTER_HEAD: usize = 0xdf6;

/// The reward records (0x118 bytes per slot; a monster's copy of its file's chunk 4, a character's untouched).
pub const REWARD_SIZE: usize = 0x118;

/// All the records.
#[derive(Clone)]
pub struct Records {
    full: Vec<u8>,
    short: Vec<u8>,
    spare: Vec<u8>,
    /// The reward records (DAT 1136068 + slot · 0x118).
    pub rewards: Vec<u8>,
}

impl Default for Records {
    fn default() -> Self {
        Records {
            full: vec![0; FULL_SLOTS * FULL_SIZE],
            short: vec![0; SHORT_SLOTS * SHORT_SIZE],
            spare: vec![0; FULL_SIZE],
            rewards: vec![0; FULL_SLOTS * REWARD_SIZE],
        }
    }
}

impl std::fmt::Debug for Records {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Records").finish_non_exhaustive()
    }
}

impl Records {
    /// A slot's record (btl_get_chr: the slot's low byte; slots past the short records read the spare one).
    pub fn get(&self, slot: usize) -> &[u8] {
        let slot = slot & 0xff;
        if slot < FULL_SLOTS {
            &self.full[slot * FULL_SIZE..(slot + 1) * FULL_SIZE]
        } else if slot < SLOTS {
            let i = slot - FULL_SLOTS;
            &self.short[i * SHORT_SIZE..(i + 1) * SHORT_SIZE]
        } else {
            &self.spare
        }
    }

    pub fn get_mut(&mut self, slot: usize) -> &mut [u8] {
        let slot = slot & 0xff;
        if slot < FULL_SLOTS {
            &mut self.full[slot * FULL_SIZE..(slot + 1) * FULL_SIZE]
        } else if slot < SLOTS {
            let i = slot - FULL_SLOTS;
            &mut self.short[i * SHORT_SIZE..(i + 1) * SHORT_SIZE]
        } else {
            &mut self.spare
        }
    }

    /// A monster's record by its place in the formation (MsGetMon).
    pub fn monster(&self, index: usize) -> &[u8] {
        self.get(FIRST_MONSTER + index)
    }

    pub fn reward_mut(&mut self, slot: usize) -> &mut [u8] {
        &mut self.rewards[slot * REWARD_SIZE..(slot + 1) * REWARD_SIZE]
    }
}

/// Whether a slot holds a monster (MsGetRamChrMonster: 0x14–0x1b).
pub fn is_monster_slot(slot: usize) -> bool {
    (slot & 0xff).wrapping_sub(FIRST_MONSTER) < MONSTERS
}

/// Little-endian reads and writes on a record; a field past the record's end reads 0 and is not written.
pub trait Fields {
    fn u8_at(&self, at: usize) -> u8;
    fn u16_at(&self, at: usize) -> u16;
    fn u32_at(&self, at: usize) -> u32;
    fn i32_at(&self, at: usize) -> i32 {
        self.u32_at(at) as i32
    }
    fn set_u8(&mut self, at: usize, v: u8);
    fn set_u16(&mut self, at: usize, v: u16);
    fn set_u32(&mut self, at: usize, v: u32);
    fn set_i32(&mut self, at: usize, v: i32) {
        self.set_u32(at, v as u32);
    }
    fn set_f32(&mut self, at: usize, v: f32) {
        self.set_u32(at, v.to_bits());
    }
}

impl Fields for [u8] {
    fn u8_at(&self, at: usize) -> u8 {
        self.get(at).copied().unwrap_or(0)
    }
    fn u16_at(&self, at: usize) -> u16 {
        self.get(at..at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32_at(&self, at: usize) -> u32 {
        self.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn set_u8(&mut self, at: usize, v: u8) {
        if let Some(b) = self.get_mut(at) {
            *b = v;
        }
    }
    fn set_u16(&mut self, at: usize, v: u16) {
        if let Some(b) = self.get_mut(at..at + 2) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }
    fn set_u32(&mut self, at: usize, v: u32) {
        if let Some(b) = self.get_mut(at..at + 4) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }
}

/// clamp_i32: raised to `low`, then lowered to `high` (so `high` wins when they cross).
pub fn clamp(v: i32, low: i32, high: i32) -> i32 {
    let v = if v < low { low } else { v };
    if v > high { high } else { v }
}
