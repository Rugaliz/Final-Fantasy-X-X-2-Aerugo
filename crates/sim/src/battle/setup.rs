//! The battle's set-up: the records from the save and the formation (MsSetRamChrData), each record's battle
//! parameters (btl_chr_init, with MsSetRamChrParam for characters and aeons), the monster files and texts
//! (MsBtlReadMonster), the monster records (MsSetRamMonData) and the copy letters of monsters that appear more
//! than once.
//!
//! The save image is the field's (save offsets as `stats` and `items` name them). Parts not ported yet report a
//! GAP once per battle.

use aerugo_vfs::Vfs;

use super::records::{self as r, Fields, Records, clamp};
use super::{
    Battle, BattleTables, ChunkFile, Encounter, MONSTER_SET, battle_chunk, formation, kernel_record,
    monster_chunk,
};
use crate::field::FileTable;
use crate::field::rng_streams::RngStreams;

/// Save offsets the set-up reads.
mod save {
    /// The party's slots: three in the front line, then the rest (save +0x3d58).
    pub const PARTY: usize = 0x3d58;
    /// The reserve order the battle copies (save +0x3d5b, 20 bytes).
    pub const RESERVE_ORDER: usize = 0x3d5b;
    /// The commands everyone shares (sixteen words).
    pub const SHARED_COMMANDS: usize = 0x3d6c;
    /// Monsters met (one bit each by monster number).
    pub const MONSTERS_MET: usize = 0x440c;
    /// Equipment records (200 of 0x16 bytes).
    pub const EQUIPMENT: usize = 0x449c;
    pub const EQUIPMENT_SIZE: usize = 0x16;
    /// Character records (0x94 bytes, characters then aeons).
    pub const RECORDS: usize = 0x55cc;
    pub const RECORD_SIZE: usize = 0x94;
    /// The names (0x14 bytes each).
    pub const NAMES: usize = 0x634c;
    pub const NAME_SIZE: usize = 0x14;
}

/// A character record's fields (from the record's start).
mod save_record {
    pub const ITEM_14: usize = 0x14;
    pub const HP: usize = 0x1c;
    pub const MP: usize = 0x20;
    pub const MAX_HP: usize = 0x24;
    pub const MAX_MP: usize = 0x28;
    pub const PARTY_FLAGS: usize = 0x2c;
    pub const WEAPON: usize = 0x2d;
    pub const ARMOUR: usize = 0x2e;
    pub const STATS: usize = 0x2f;
    pub const ITEM_37: usize = 0x37;
    pub const ITEM_38: usize = 0x38;
    pub const ITEM_3D: usize = 0x3d;
    pub const LEARNED: usize = 0x3e;
}

/// What the set-up reads beside the battle itself.
pub struct Inputs<'a> {
    pub vfs: &'a Vfs,
    pub files: &'a FileTable,
    pub tables: &'a BattleTables,
}

impl std::fmt::Debug for Inputs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inputs").finish_non_exhaustive()
    }
}

/// Read the battle's file and set up every record (the read steps up to the battle's scripts).
pub fn start(
    inputs: &Inputs<'_>,
    encounter: Encounter,
    save: &mut [u8],
    random: &mut RngStreams,
) -> Result<Battle, String> {
    let path = inputs
        .files
        .path(super::BATTLE_SET, encounter.file)
        .ok_or_else(|| format!("battle file {:#x} is not in the file table", encounter.file))?;
    let bytes = inputs.vfs.read(&path).map_err(|e| format!("{path}: {e}"))?;
    let file = ChunkFile::parse(bytes).ok_or_else(|| format!("{path}: not a chunk file"))?;
    let mut b = Battle {
        encounter,
        file,
        monster_files: Vec::new(),
        records: Records::default(),
        front_line: [0xff; 7],
        reserve_order: [0; 20],
        first_attack: 0,
        area_id: 0,
        worker_slots: Vec::new(),
        globals: Default::default(),
    };
    b.area_id = b.encounter.area_id;
    log::error!(
        "GAP: battle set-up: the damage image (MsSetChrImageDamageAttack), the command lists (MsSetRamChrAbility) and the weapons are not ported"
    );
    set_records(&mut b, inputs.tables, save);
    // Read step 10.
    b.first_attack = first_attack(&b.records, save, random, FIRST_ATTACK_CHANCE);
    count_battle(save);
    read_monsters(&mut b, inputs)?;
    set_monster_records(&mut b, inputs.tables, random);
    Ok(b)
}

/// MsSetRamChrData: every record's defaults, the party's slots, the characters' and aeons' records from the save,
/// then the formation's monsters.
fn set_records(b: &mut Battle, tables: &BattleTables, save: &mut [u8]) {
    let special = b.special();
    for slot in 0..r::SLOTS {
        let rec = b.records.get_mut(slot);
        rec.set_u8(r::SLOT, slot as u8);
        rec.set_u8(0x13, 0xff);
        rec.set_u8(r::FILE_SLOT, slot as u8);
        rec.set_u8(r::OWN_FILE, 1);
        rec.set_u8(r::PRESENT, 1);
        rec.set_u8(0x37, 1);
        rec.set_u8(0x20, if special && slot < r::PARTY_SLOTS { 3 } else { 1 });
        rec.set_u16(0x21, 0x101);
        rec.set_u8(0x25, u8::from(!special));
        rec.set_u8(0x26, u8::from(!special));
        rec.set_u16(0x27, 0xffff);
        rec.set_u16(0x2a, 0x100);
        rec.set_u8(0x2c, 3);
    }
    for slot in 0..r::FULL_SLOTS {
        let rec = b.records.get_mut(slot);
        full_record_defaults(rec, slot, special);
    }
    // The front line (save +0x3d58, three slots) and what follows it.
    for i in 0..3 {
        let who = save.u8_at(save::PARTY + i);
        if who != 0xff {
            let rec = b.records.get_mut(usize::from(who));
            rec.set_u8(r::IN_BATTLE, 1);
            rec.set_u8(r::ACTIVE, 1);
            rec.set_u8(r::ORDER, i as u8);
            if i == 1 {
                rec.set_u8(0x3f8, 1);
            }
        }
        b.front_line[i] = who;
    }
    b.front_line[3..].fill(0xff);
    b.reserve_order.copy_from_slice(&save[save::RESERVE_ORDER..save::RESERVE_ORDER + 20]);
    for slot in 0..r::PARTY_SLOTS {
        let joined = save_joined(save, slot);
        let rec = b.records.get_mut(slot);
        if joined || rec.u8_at(r::IN_BATTLE) != 0 {
            rec.set_u8(r::IN_BATTLE, 1);
            rec.set_u16(0xdd6, 0x101);
            rec.set_u16(0xdd9, 0x101);
            rec.set_u8(0xdd8, 1);
        }
    }
    for slot in 0..r::PARTY_SLOTS {
        let rec = b.records.get_mut(slot);
        rec.set_u8(0xdd5, 1);
        rec.set_u16(r::CHARACTER, slot as u16);
        set_models(rec, save);
        init_character(&mut b.records, slot, tables, save, special);
        log::trace!("battle slot {slot:#x}: the weapon models are not set (MsGetSaveWeaponModel)");
    }
    for slot in 0..r::FULL_SLOTS {
        let aeon = !r::is_monster_slot(slot) && is_aeon(&b.records, slot);
        let rec = b.records.get_mut(slot);
        if !r::is_monster_slot(slot) {
            rec.set_u16(r::DEATH_LOOK, 0);
            rec.set_u8(r::MODEL_KIND, 8);
            if !aeon {
                rec.set_u8(r::KIND, r::KIND_CHARACTER);
                rec.set_u8(0x437, 1);
            } else {
                rec.set_u8(r::KIND, r::KIND_AEON);
                rec.set_u8(r::ORDER, (slot as u8).wrapping_sub(8));
                rec.set_u8(0x1e, 1);
                rec.set_u8(0x3f9, 1);
                rec.set_u8(0xe15, 0);
                if slot == 0x10 {
                    rec.set_u8(0x3f8, 1);
                }
            }
        } else {
            rec.set_u16(r::DEATH_LOOK, 0x303);
            rec.set_u8(r::MODEL_KIND, 3);
            rec.set_u8(r::KIND, r::KIND_MONSTER);
        }
    }
    // The formation's monsters.
    let ids: Vec<u16> = {
        let f = b.file.chunk(battle_chunk::FORMATION);
        (0..r::MONSTERS).map(|i| f.u16_at(formation::MONSTERS + 2 * i)).collect()
    };
    let mut order = 0u8;
    for (i, id) in ids.into_iter().enumerate() {
        let rec = b.records.get_mut(r::FIRST_MONSTER + i);
        rec.set_u16(r::CHARACTER, id);
        set_models(rec, save);
        if id != 0xffff {
            mark_met(save, id);
            rec.set_u8(r::ORDER, order);
            order = order.wrapping_add(1);
            rec.set_u8(0xdf3, 1);
            rec.set_u8(r::IN_BATTLE, 1);
            rec.set_u8(0x39, 1);
            rec.set_u16(0xdd6, 0x101);
            rec.set_u16(0xdd9, 0x101);
            rec.set_u8(0xdd8, 1);
            rec.set_u8(r::ACTIVE, 1);
            rec.set_u8(0x447, 3);
        }
    }
}

/// The defaults MsSetRamChrData gives every full record (roles traced as their readers are ported).
fn full_record_defaults(rec: &mut [u8], slot: usize, special: bool) {
    // MsClearDamageBuffer.
    rec.set_u8(0x776, 0xff);
    rec.set_u8(0xa4e, 0xff);
    rec.set_f32(0x40, 1.0);
    rec.set_f32(0x44, 1.0);
    rec.set_u16(0xdd2, 0x101);
    rec.set_u8(0xdd4, 1);
    rec.set_u16(0xdde, 0xff);
    rec.set_u8(0x15, 1);
    rec.set_u16(0x1b, 1);
    rec.set_u8(0x1d, 1);
    rec.set_u32(0x16, 0x0101_0101);
    rec.set_u8(0x2d, 1);
    rec.set_u32(0x2e, 0x0101_0101);
    rec.set_u8(0x23, 1);
    rec.set_u8(0x422, slot as u8);
    rec.set_u8(r::CURSOR_POINT, 1);
    rec.set_u16(0xde4, 0xffff);
    rec.set_u8(0x3f1, 0x10);
    rec.set_u16(0xe0e, 0x108);
    rec.set_u32(0xe2c, 1);
    rec.set_u8(0xe15, 1);
    // 0079e890: the hit sound pair (characters 0x1f1e, monsters 0x3837).
    rec.set_u16(0xe10, if r::is_monster_slot(slot) { 0x3837 } else { 0x1f1e });
    rec.set_u8(0x43b, 1);
    rec.set_u16(0x3fc, 0x100);
    rec.set_u16(0x400, 0x100);
    rec.set_u8(0x406, 8);
    rec.set_u8(r::DIRECTION_RETURN, 1);
    rec.set_u8(0x43f, 0xff);
    rec.set_u8(0x442, 0xff);
    rec.set_u8(0x3f2, 0xff);
    rec.set_u8(0x44f, 0);
    rec.set_u16(0x408, 0);
    rec.set_u8(0x40f, 1);
    rec.set_u8(0x438, 0xff);
    rec.set_u16(0xf74, 0xffff);
    // The scale and the float after it.
    for k in 0..4 {
        rec.set_f32(r::SCALE + 4 * k, 1.0);
    }
    rec.set_u8(0x3f0, 0xff);
    // Round frame and round return frame 6; idle-2 chance 4, attack increase speed 0x20.
    rec.set_u32(r::ROUND_FRAME, 0x0006_0006);
    rec.set_u16(r::IDLE2_CHANCE, 0x2004);
    rec.set_f32(r::ATTACK_START_DISTANCE, 40.0);
    rec.set_u8(r::ATTACK_DEC_SPEED, 0x20);
    rec.set_u8(r::MODEL_SWITCH, 1);
    rec.set_u32(r::ATTACK_OFFSET, 0);
    rec.set_u8(r::HEIGHT_MODE, 1);
    rec.set_u32(r::ATTACK_HEIGHT, 0);
    rec.set_u8(0x6d4, 0xff);
    rec.set_f32(r::RUN_SPEED, 3.0);
    rec.set_f32(r::RUN_RETURN_SPEED, 3.0);
    rec.set_f32(r::RUN_ACCELERATION, f32::from_bits(0x3ecc_cccd));
    rec.set_f32(r::WEIGHT, 50.0);
    rec.set_f32(0x4f0, f32::from_bits(0x3e4c_cccd));
    rec.set_u32(r::HEIGHT, 0);
    rec.set_u8(r::GRAVITY_MODE, u8::from(!special));
}

/// MsGetSavePlyJoin: whether a character or aeon has joined (the save record's party bits, bit 0).
fn save_joined(save: &[u8], slot: usize) -> bool {
    save.u8_at(save::RECORDS + slot * save::RECORD_SIZE + save_record::PARTY_FLAGS) & 1 != 0
}

/// MsGetRamChrSummon: an aeon by its ply_rom flags (+0x590 bit 2).
fn is_aeon(records: &Records, slot: usize) -> bool {
    records.get(slot).u8_at(r::ROM_FLAGS) >> 2 & 1 != 0
}

/// 0079a440: the models of the slot's holder: a character or aeon (id >> 12 = 0) the traced battle model
/// table's (character 6 takes model 0x29 for +4 while save +0xd1 is set, MsGetChrID); a monster (1) 0x1000 +
/// its number. Returns the file number (the holder's number; 0 for other kinds).
fn set_models(rec: &mut [u8], save: &[u8]) -> usize {
    let id = rec.u16_at(r::CHARACTER);
    let kind = id >> 12;
    let number = usize::from(id & 0xfff);
    let (model, file) = match kind {
        0 => (crate::traced_tables::BATTLE_MODELS.get(number).copied().unwrap_or(0), number),
        1 => (number as u32 + 0x1000, number),
        _ => (0, 0),
    };
    rec.set_u32(r::BATTLE_MODEL, model);
    if kind == 0 && number == 6 && save.u8_at(ALTERNATIVE_MODEL) != 0 {
        rec.set_u8(0x1d, 0);
        rec.set_u32(r::MODEL, 0x29);
        return 6;
    }
    rec.set_u32(r::MODEL, model);
    file
}

/// The save byte that gives character 6 its alternative model (MsGetChrID).
const ALTERNATIVE_MODEL: usize = 0xd1;

/// 00790be0: the monster's bit in the save's monsters-met bitmap.
fn mark_met(save: &mut [u8], id: u16) {
    let at = save::MONSTERS_MET + usize::from((id & 0xfff) >> 4) * 2;
    let v = save.u16_at(at) | 1 << (id & 0xf);
    save.set_u16(at, v);
}

/// btl_chr_init for a character or aeon: the parameter block from the ply_rom record and the save record.
fn init_character(records: &mut Records, slot: usize, tables: &BattleTables, save: &[u8], special: bool) {
    let rom = kernel_record(&tables.ply_rom, slot as u32).map(|k| k.bytes.to_vec()).unwrap_or_default();
    let base = save::RECORDS + slot * save::RECORD_SIZE;
    let s = |o: usize| save.u8_at(base + o);
    let w = |o: usize| save.u32_at(base + o);
    let rec = records.get_mut(slot);
    rec[r::PARAMS..r::PARAMS + r::PARAMS_SIZE].fill(0);
    let name = save::NAMES + slot * save::NAME_SIZE;
    rec[r::NAME..r::NAME + save::NAME_SIZE].copy_from_slice(&save[name..name + save::NAME_SIZE]);
    rec.set_u8(r::ROM_FLAGS, rom.u8_at(0x10));
    rec.set_u8(r::PARTY_FLAGS, s(save_record::PARTY_FLAGS));
    rec.set_u8(0x5ba, clamp(i32::from(s(save_record::ITEM_37)), 0, 0xff) as u8);
    rec.set_u16(r::FLAGS, 0);
    rec.set_u16(0x5c5, 0xffff);
    for i in 0..3 {
        rec.set_u8(0x5bb + i, s(save_record::ITEM_38 + i));
    }
    rec.set_u8(0x5c8, 0);
    rec.set_u8(0x5c9, rom.u8_at(0x2a));
    rec.set_u16(0x6d6, 0);
    rec.set_u8(0x6d8, s(save_record::ITEM_3D));
    rec.set_u8(0x6d9, rom.u8_at(0x2b));
    rec.set_u32(0x5cc, w(save_record::ITEM_14));
    rec.set_u32(r::HP, w(save_record::HP));
    rec.set_u32(r::MP, w(save_record::MP));
    rec.set_u32(r::OVERKILL_OR_BASE, w(save_record::MAX_HP));
    let (hp, mp) = (rec.u32_at(r::HP), rec.u32_at(r::MP));
    rec.set_u32(r::START_HP, hp);
    rec.set_u32(r::START_MP, mp);
    rec.set_u32(0x6fc, 0xffff_ffff);
    rec.set_u8(0x6d1, 0xff);
    rec.set_u8(0x6e1, 0x32);
    rec.set_u8(0x719, 0);
    rec.set_u32(0xf60, w(save_record::MAX_HP));
    rec.set_f32(0x70c, 1.0);
    rec.set_u8(r::KERNEL_SLOT, slot as u8);
    rec.set_u8(r::COPY_NUMBER, 0);
    rec.set_u16(0x712, 0xff);
    character_params(rec, save, base, tables);
    rec.set_u16(r::STATUS, 0);
    rec.set_u16(0x616, 0);
    rec.set_u16(r::FIRST_COMMAND, 0x3000);
    let aeon = rec.u8_at(r::ROM_FLAGS) >> 2 & 1 != 0;
    rec.set_u16(r::SECOND_COMMAND, if aeon { 0 } else { 0x3021 });
    let flags = rec.u16_at(r::FLAGS) | 0x200;
    rec.set_u16(r::FLAGS, flags);
    rec.set_u16(0x6d3, 0xffff);
    for i in 0..11 {
        rec.set_u32(0x690 + 4 * i, 0);
    }
    // MsSetRamChrAbility: the learned commands.
    for i in 0..6 {
        rec.set_u16(r::LEARNED + 2 * i, save.u16_at(base + save_record::LEARNED + 2 * i));
    }
    for i in 0..16 {
        rec.set_u16(r::SHARED_LEARNED + 2 * i, save.u16_at(save::SHARED_COMMANDS + 2 * i));
    }
    if special {
        log::error!("GAP: btl_set_cmd_flag(slot, 0x3002) for the formation's flag is not ported");
    }
    finish_record(rec, special);
}

/// The end every btl_chr_init shares: the eternal-life bit, the weak state, the auto-statuses and the
/// knocked-out start.
fn finish_record(rec: &mut [u8], special: bool) {
    // MsSetEternalRelife.
    if rec.u8_at(0x3b) != 0 {
        let v = rec.u16_at(0x616) | 0x200;
        rec.set_u16(0x616, v);
    }
    weak_state(rec, special);
    set_auto_status(rec);
    if rec.i32_at(r::HP) < 1 || rec.u16_at(r::STATUS) & r::STATUS_KO != 0 {
        let v = rec.u16_at(r::STATUS) | r::STATUS_KO;
        rec.set_u16(r::STATUS, v);
        rec.set_u8(r::DEATH_STATE, 2);
    }
}

/// MsSetRamChrParam: maximum HP / MP, the stats with the battle's bonuses, the equipment and what its
/// auto-abilities add, the limit flags, then HP / MP kept within the maximums.
fn character_params(rec: &mut [u8], save: &[u8], base: usize, tables: &BattleTables) {
    let s = |o: usize| save.u8_at(base + o);
    let max_hp = save.u32_at(base + save_record::MAX_HP);
    let max_mp = save.u32_at(base + save_record::MAX_MP);
    rec.set_u32(r::MAX_HP, max_hp);
    rec.set_u32(r::MAX_MP, max_mp);
    rec.set_u32(r::BASE_MAX_HP, max_hp);
    rec.set_u32(r::BASE_MAX_MP, max_mp);
    for i in 0..r::STAT_COUNT {
        // Evasion (6) may be 0; the others are at least 1.
        let low = if i == 6 { 0 } else { 1 };
        let v = i32::from(rec.u8_at(r::STAT_BONUSES + i)) + i32::from(s(save_record::STATS + i));
        rec.set_u8(r::STATS + i, clamp(v, low, 0xff) as u8);
    }
    rec.set_u8(r::WEAPON, s(save_record::WEAPON));
    rec.set_u8(r::ARMOUR, s(save_record::ARMOUR));
    let worn = [rec.u8_at(r::WEAPON), rec.u8_at(r::ARMOUR)].map(|e| equipment(save, u32::from(e)));
    rec.set_u8(r::WEAPON_FORMULA, 1);
    rec.set_u8(r::WEAPON_CRITICAL, 0x10);
    rec[0x5d8..0x604].fill(0);
    rec.set_u16(0x604, 0);
    rec.set_u8(r::AUTO_STATUS2_ON, 0);
    rec[0x62a..0x63c].fill(0);
    rec[r::RESISTANCES..r::RESISTANCES + r::RESISTANCE_COUNT].fill(0);
    rec.set_u16(r::RESIST_WORD, 0);
    rec[r::ABILITY_WORDS..r::ABILITY_WORDS + 6].fill(0);
    for (i, e) in worn.iter().enumerate() {
        let Some(e) = e else { continue };
        if i == 0 {
            rec.set_u8(r::WEAPON_FORMULA, e.u8_at(8));
            rec.set_u8(r::WEAPON_CRITICAL, e.u8_at(9));
        }
        let power = rec.u8_at(r::EQUIPMENT_POWER).wrapping_add(e.u8_at(10));
        rec.set_u8(r::EQUIPMENT_POWER, power);
        for k in 0..4 {
            let ability = u32::from(e.u16_at(0xe + 2 * k));
            if ability == 0 || ability == 0xff {
                continue;
            }
            let Some(a) = kernel_record(&tables.abilities, ability & 0xfff) else { continue };
            add_ability(rec, a.bytes);
        }
    }
    limit_status(rec, -1);
    let hp = clamp(rec.i32_at(r::HP), 0, rec.i32_at(r::MAX_HP));
    rec.set_i32(r::HP, hp);
    let mp = clamp(rec.i32_at(r::MP), 0, rec.i32_at(r::MAX_MP));
    rec.set_i32(r::MP, mp);
}

/// 007abbd0 for a save slot: the equipment record (0xff none; slots past 199 read slot 0). The battle's own
/// equipment sets (ids 0x7xxx and 0xbxxx) are not ported.
fn equipment(save: &[u8], id: u32) -> Option<Vec<u8>> {
    if id == 0xff {
        return None;
    }
    let mut number = (id & 0xfff) as usize;
    if id >> 12 == 7 || id >> 12 == 0xb {
        log::error!("GAP: battle equipment set {:#x} is not ported", id >> 12);
        return None;
    }
    if number > 199 {
        number = 0;
    }
    let at = save::EQUIPMENT + number * save::EQUIPMENT_SIZE;
    save.get(at..at + save::EQUIPMENT_SIZE).map(<[u8]>::to_vec)
}

/// What one auto-ability adds to the parameter block (MsSetRamChrParam's inner loop).
fn add_ability(rec: &mut [u8], a: &[u8]) {
    for i in 0..5 {
        let v = rec.u8_at(r::ABILITY_BITS + i) | a.u8_at(0x11 + i);
        rec.set_u8(r::ABILITY_BITS + i, v);
    }
    for i in 0..r::ABILITY_SUMS_COUNT {
        let v = i32::from(a.u8_at(0x16 + i)) + i32::from(rec.u8_at(r::ABILITY_SUMS + i));
        rec.set_u8(r::ABILITY_SUMS + i, clamp(v, 0, 0xfe) as u8);
    }
    for i in 0..r::ABILITY_SUMS2_COUNT {
        let v = i32::from(a.u8_at(0x2f + i)) + i32::from(rec.u8_at(r::ABILITY_SUMS2 + i));
        rec.set_u8(r::ABILITY_SUMS2 + i, clamp(v, 0, 0xfe) as u8);
    }
    let word = rec.u16_at(r::ABILITY_WORD) | a.u16_at(0x5e);
    rec.set_u16(r::ABILITY_WORD, word);
    let target = if a.u8_at(0x10) & 1 == 0 {
        r::AUTO_STATUS
    } else {
        rec.set_u8(r::AUTO_STATUS2_ON, 1);
        r::AUTO_STATUS2
    };
    for k in 0..3 {
        let v = rec.u16_at(target + 2 * k) | a.u16_at(0x58 + 2 * k);
        rec.set_u16(target + 2 * k, v);
    }
    for i in 0..r::RESISTANCE_COUNT {
        let v = i32::from(a.u8_at(0x3c + i)) + i32::from(rec.u8_at(r::RESISTANCES + i));
        rec.set_u8(r::RESISTANCES + i, clamp(v, 0, 0xff) as u8);
    }
    let v = rec.u16_at(r::RESIST_WORD) | a.u16_at(0x60);
    rec.set_u16(r::RESIST_WORD, v);
    for k in 0..3 {
        let v = rec.u16_at(r::ABILITY_WORDS + 2 * k) | a.u16_at(0x62 + 2 * k);
        rec.set_u16(r::ABILITY_WORDS + 2 * k, v);
    }
}

/// The limit flags (+0x640): bit 0 doubles the maximum HP (up to 9999, or 99999 with +0x6be bit 9), bit 1 the
/// maximum MP (999 / 9999 with bit 10). `flags` < 0 re-applies the flags already set (MsLimitStatusProcess).
pub fn limit_status(rec: &mut [u8], flags: i32) {
    let before = rec.u8_at(0x640);
    let setting = flags >= 0;
    let flags = if setting {
        rec.set_u8(0x640, flags as u8);
        flags as u32
    } else {
        u32::from(before)
    };
    let words = rec.u16_at(0x6be);
    for (bit, max, base, now, cap, more) in
        [(0, r::MAX_HP, r::BASE_MAX_HP, r::HP, 9999, 90000), (1, r::MAX_MP, r::BASE_MAX_MP, r::MP, 999, 9000)]
    {
        let want = flags >> bit & 1 != 0;
        let had = before >> bit & 1 != 0;
        let value = if want && !(had && setting) {
            let extra = if words & (0x200 << bit) != 0 { more } else { 0 };
            clamp(rec.i32_at(base).wrapping_mul(2), 0, cap + extra)
        } else if !(!had && setting) && !want {
            rec.i32_at(base)
        } else {
            continue;
        };
        rec.set_i32(max, value);
        let v = clamp(rec.i32_at(now), 0, value);
        rec.set_i32(now, v);
    }
}

/// MsCalcWeakLevel: 3 knocked out, 2 below a quarter, 1 below half, 0 otherwise.
pub fn weak_level(hp: i32, max: i32) -> u8 {
    let quarters = if max != 0 { hp.wrapping_mul(4) / max } else { 0 };
    if hp < 1 {
        3
    } else if quarters < 1 {
        2
    } else if quarters < 2 {
        1
    } else {
        0
    }
}

/// 0078d7f0 with no level given: the record's weak level (+0x63e) and the weak state shown (+0x63d).
pub(crate) fn weak_state(rec: &mut [u8], special: bool) {
    let level = weak_level(rec.i32_at(r::HP), rec.i32_at(r::MAX_HP));
    let state = 'state: {
        if rec.u8_at(0x450) != 0 {
            break 'state 0;
        }
        let mut cannot_weaken = false;
        let mut state = level;
        match level {
            0 => {
                if rec.u8_at(0x608) != 0 || rec.u8_at(0x60a) != 0 || rec.u8_at(0x609) != 0 {
                    break 'state 1;
                }
            }
            3 => {
                cannot_weaken = !may_weaken(rec);
                if cannot_weaken {
                    state = 2;
                }
                if rec.u8_at(r::DEATH_STATE) == 0 {
                    state = 2;
                }
            }
            _ => {}
        }
        if state != 2 {
            break 'state state;
        }
        if special {
            state = 1;
        }
        if rec.u8_at(r::ROM_FLAGS) >> 2 & 1 != 0 {
            state = u8::from(!cannot_weaken);
        }
        state
    };
    rec.set_u8(0x63e, level);
    rec.set_u8(0x63d, state);
}

/// MsSetChrWeak, with the command queues empty (as they are while the battle is set up): neither bit 1 nor bit
/// 2 of +0xf5e, and not an aeon (+0x3f9) with +0x508 set.
fn may_weaken(rec: &[u8]) -> bool {
    let flags = rec.u8_at(0xf5e);
    let ok = flags & 4 == 0 && flags & 2 == 0;
    ok && !(rec.u8_at(0x3f9) != 0 && rec.u8_at(0x508) != 0)
}

/// MsSetAutoStatus: the auto-status masks (the second set too while weak), the statuses they hold set and what
/// they replaced kept, the status counters they hold kept aside.
pub fn set_auto_status(rec: &mut [u8]) {
    let (had_612, weak, had_613) =
        (rec.u8_at(r::STATUS_COUNTERS + 0xa), rec.u8_at(0x63e), rec.u8_at(r::HALVES_WAIT));
    let mut masks = [rec.u16_at(0x630), rec.u16_at(0x632), rec.u16_at(0x634)];
    if weak == 1 || weak == 2 {
        for (k, m) in masks.iter_mut().enumerate() {
            *m |= rec.u16_at(r::AUTO_STATUS2 + 2 * k);
        }
    }
    for (k, m) in masks.iter().enumerate() {
        rec.set_u16(0x62a + 2 * k, *m);
    }
    rec.set_u8(0x63f, weak);
    let (mut kept, mut status) = (rec.u16_at(0x618), rec.u16_at(r::STATUS));
    for bit in 0..12 {
        if masks[0] >> bit & 1 != 0 {
            let b = 1u16 << bit;
            kept = !b & kept | b & status;
            status |= b;
        }
    }
    rec.set_u16(r::STATUS, status);
    rec.set_u16(0x618, kept);
    for i in 0..13 {
        if masks[1] >> i & 1 != 0 {
            let v = rec.u8_at(0x608 + i);
            rec.set_u8(0x61a + i, v);
            rec.set_u8(0x608 + i, 0xff);
        }
    }
    if had_613 == 0 && rec.u8_at(r::HALVES_WAIT) != 0 && rec.u8_at(r::DOUBLES_WAIT) != 0xff {
        rec.set_u8(0x614, 0);
    }
    let (mut kept, mut status) = (rec.u16_at(0x628), rec.u16_at(0x616));
    for bit in 0..16 {
        if masks[2] >> bit & 1 != 0 {
            let b = 1u16 << bit;
            kept = !b & kept | b & status;
            status |= b;
        }
    }
    rec.set_u16(0x616, status);
    rec.set_u16(0x628, kept);
    if rec.u8_at(r::STATUS_COUNTERS + 0xa) != 0 && had_612 == 0 {
        rec.set_u8(0x6d2, 0);
    }
}

/// MsBtlReadMonster: each formation monster's file (or the earlier slot's it shares) and its texts from the
/// monster tables, then the file's chunks (00783b00).
fn read_monsters(b: &mut Battle, inputs: &Inputs<'_>) -> Result<(), String> {
    // DAT ca80: the monster file reads so far (each record's file order).
    let mut reads = 0u8;
    // 0x102–0x104 keep the monsters' own names (DAT ca78 = 0); other battles rename three monsters that are
    // characters.
    let renames = !(0x102..=0x104).contains(&b.encounter.file);
    for i in 0..r::MONSTERS {
        let slot = r::FIRST_MONSTER + i;
        let id = b.records.get(slot).u16_at(r::CHARACTER);
        if id == 0xffff {
            continue;
        }
        let number = usize::from(id & 0xfff);
        let shared = (0..i).find(|&k| {
            let other = b.records.monster(k);
            other.u16_at(r::CHARACTER) == id && other.u32_at(r::MONSTER_FILE) != 0
        });
        if let Some(k) = shared {
            let file = b.records.monster(k).u32_at(r::MONSTER_FILE);
            let rec = b.records.get_mut(slot);
            rec.set_u8(r::OWN_FILE, 0);
            rec.set_u8(r::FILE_SLOT, (k + r::FIRST_MONSTER) as u8);
            rec.set_u32(r::MONSTER_FILE, file);
        }
        monster_texts(b.records.get_mut(slot), number, renames, inputs.tables);
        if shared.is_none() {
            let path = inputs
                .files
                .path(MONSTER_SET, number)
                .ok_or_else(|| format!("monster file {number:#x} is not in the file table"))?;
            let bytes = inputs.vfs.read(&path).map_err(|e| format!("{path}: {e}"))?;
            let file = ChunkFile::parse(bytes).ok_or_else(|| format!("{path}: not a chunk file"))?;
            b.monster_files.push(file);
            let index = b.monster_files.len() as u32;
            b.records.get_mut(slot).set_u32(r::MONSTER_FILE, index);
        }
        monster_file_read(b, slot, reads);
        reads = reads.wrapping_add(1);
    }
    Ok(())
}

/// The monster's name, Sensor and Scan texts (monster1 below 0x65, monster2 to 0xb4, monster3 after; three
/// monsters that are characters take their names in battles that rename them).
fn monster_texts(rec: &mut [u8], number: usize, renames: bool, tables: &BattleTables) {
    let renamed = match number {
        0xfe if renames => Some(8),
        0x11c if renames => Some(9),
        0x11f if renames => Some(0xc),
        _ => None,
    };
    let table = if renamed.is_some() || number > 0xb4 {
        &tables.monsters[2]
    } else if number < 0x65 {
        &tables.monsters[0]
    } else {
        &tables.monsters[1]
    };
    if let Some(k) = kernel_record(table, number as u32) {
        for (field, at, size) in [
            (0, r::MONSTER_NAME, r::MONSTER_NAME_SIZE),
            (4, r::SENSOR_TEXT, r::SENSOR_TEXT_SIZE),
            (0xc, r::SCAN_TEXT, r::SCAN_TEXT_SIZE),
        ] {
            rec[at..at + size].fill(0);
            if let Some(text) = table.text(&k, field) {
                let n = text.len().min(size - 1);
                rec[at..at + n].copy_from_slice(&text[..n]);
            }
        }
    }
    if let Some(who) = renamed {
        log::error!(
            "GAP: the monster named for character {who:#x} (getDefaultChrName) keeps the table's name"
        );
    }
}

/// 00783b00: the read file's head byte, the record's file order, its sounds and rewards.
fn monster_file_read(b: &mut Battle, slot: usize, order: u8) {
    let Some(file) = b.monster_file(slot) else { return };
    let head = file.chunk(monster_chunk::HEAD).first().copied().unwrap_or(0);
    let has_sounds = file.has_chunk(monster_chunk::SOUNDS);
    let own = b.records.get(slot).u8_at(r::OWN_FILE) != 0;
    let rec = b.records.get_mut(slot);
    rec.set_u8(r::MONSTER_HEAD, head);
    rec.set_u8(r::FILE_ORDER, order);
    if own && has_sounds {
        log::trace!("battle slot {slot:#x}: the monster's sound data (MsRegistSEdata) is not registered");
    }
}

/// MsSetRamMonData's record part: btl_chr_init for every formation monster, then the copy letters.
fn set_monster_records(b: &mut Battle, tables: &BattleTables, random: &mut RngStreams) {
    let special = b.special();
    for i in 0..r::MONSTERS {
        let slot = r::FIRST_MONSTER + i;
        if b.records.get(slot).u8_at(r::IN_BATTLE) != 0 {
            init_monster(b, slot, special);
        }
    }
    copy_letters(&mut b.records, random);
    first_turn_order(&mut b.records, tables, random, b.first_attack);
    // 0079a0e0: HP and MP within the maximums.
    for slot in 0..r::FULL_SLOTS {
        let rec = b.records.get_mut(slot);
        if rec.u8_at(r::IN_BATTLE) != 0 {
            let hp = clamp(rec.i32_at(r::HP), 0, rec.i32_at(r::MAX_HP));
            rec.set_i32(r::HP, hp);
            let mp = clamp(rec.i32_at(r::MP), 0, rec.i32_at(r::MAX_MP));
            rec.set_i32(r::MP, mp);
        }
    }
}

/// The chance MsGetFirstAttack is asked with (of 256) at read step 10.
const FIRST_ATTACK_CHANCE: i32 = 0x20;

/// The save's battle count (save +0x3d14), raised at read step 10 below 0xfffffff (DAT c9e5 = 2 keeps it; that
/// mode is not set up yet).
fn count_battle(save: &mut [u8]) {
    const BATTLES: usize = 0x3d14;
    let n = save.u32_at(BATTLES);
    if (n as i32) < 0x0fff_ffff {
        save.set_u32(BATTLES, n.wrapping_add(1));
    }
}

/// MsGetFirstAttack: a roll on stream 1 (0–255): below `chance` the party strikes first (1), at 255 − chance or
/// more the monsters do (2), else neither; a front-line character with the auto-ability flag at save record
/// +0x4a bit 2 lowers the roll by chance + 1.
fn first_attack(records: &Records, save: &[u8], random: &mut RngStreams, chance: i32) -> u8 {
    let mut roll = (random.next(1) & 0xff) as i32;
    let first_strike = (0..r::PARTY_SLOTS).any(|slot| {
        records.get(slot).u8_at(r::ACTIVE) != 0
            && save.u8_at(save::RECORDS + slot * save::RECORD_SIZE + 0x4a) & 4 != 0
    });
    if first_strike {
        roll += -1 - chance;
    }
    if roll < 0xff - chance { u8::from(roll < chance) } else { 2 }
}

/// MsGetRomCTBcount and the ctb_base record (agility clamped to 1–255, record agility − 1).
fn ctb_record(tables: &BattleTables, agility: u8) -> &[u8] {
    let index = clamp(i32::from(agility), 1, 0xff) - 1;
    kernel_record(&tables.ctb_base, index as u32).map_or(&[], |k| k.bytes)
}

/// MsGetRomCTBbase: a wait halved and doubled by the record's status counters, kept within 0–255.
fn wait_of(rec: &[u8], wait: i32) -> u8 {
    let mut wait = wait;
    if rec.u8_at(r::HALVES_WAIT) != 0 {
        wait /= 2;
    }
    if rec.u8_at(r::DOUBLES_WAIT) != 0 {
        wait *= 2;
    }
    clamp(wait, 0, 0xff) as u8
}

/// MsSetFirstCTB: every record's turn length (ticks ×3), then the first waits: the party none when it strikes
/// first, a full turn when the monsters do, else a turn less a random head start; the monsters a full turn
/// when the party strikes first, none when they do, else a turn stretched by up to 10%. With ability word
/// bit 1 (+0x6bc) the wait is 0. The shortest wait on the field then comes off every record on the field.
fn first_turn_order(records: &mut Records, tables: &BattleTables, random: &mut RngStreams, first: u8) {
    for slot in 0..r::FULL_SLOTS {
        let agility = records.get(slot).u8_at(r::AGILITY);
        let ticks = ctb_record(tables, agility).u8_at(0);
        records.get_mut(slot).set_u8(r::CTB_TURN, ticks.wrapping_mul(3));
    }
    let mut shortest: i32 = -1;
    let place = |records: &mut Records, slot: usize, wait: i32, shortest: &mut i32| {
        let rec = records.get_mut(slot);
        let wait = if rec.u8_at(r::ABILITY_WORDS) & 2 != 0 { 0 } else { wait };
        let w = wait_of(rec, wait);
        rec.set_u8(r::CTB, w);
        if rec.u8_at(r::ACTIVE) != 0 && (*shortest < 0 || i32::from(w) < *shortest) {
            *shortest = i32::from(w);
        }
    };
    for slot in 0..r::PARTY_SLOTS {
        let turn = i32::from(records.get(slot).u8_at(r::CTB_TURN));
        let wait = match first {
            1 => 0,
            2 => turn,
            _ => {
                let range = i32::from(ctb_record(tables, records.get(slot).u8_at(r::AGILITY)).u8_at(1));
                let roll = random.next(damage_stream(records, slot)) as i32;
                turn - roll % (range + 1)
            }
        };
        place(records, slot, wait, &mut shortest);
    }
    for i in 0..r::MONSTERS {
        let slot = r::FIRST_MONSTER + i;
        let turn = i64::from(records.get(slot).u8_at(r::CTB_TURN));
        let wait = match first {
            1 => turn as i32,
            2 => 0,
            _ => {
                let roll = random.next(damage_stream(records, slot)) as i32;
                let w = (turn * 100 / i64::from(100 - roll % 11)) as i32;
                w.max(0)
            }
        };
        place(records, slot, wait, &mut shortest);
    }
    for slot in 0..r::FULL_SLOTS {
        let rec = records.get_mut(slot);
        if rec.u8_at(r::ACTIVE) != 0 {
            let v = rec.u8_at(r::CTB).wrapping_sub(shortest as u8);
            rec.set_u8(r::CTB, v);
        }
    }
}

/// btl_chr_init for a monster: the parameter block from the monster file's stats.
fn init_monster(b: &mut Battle, slot: usize, special: bool) {
    b.records.get_mut(slot)[r::PARAMS..r::PARAMS + r::PARAMS_SIZE].fill(0);
    if let Some(file) = b.monster_file(slot) {
        let stats = file.chunk(monster_chunk::STATS).to_vec();
        let rewards = file.chunk(monster_chunk::REWARDS).to_vec();
        monster_params(b.records.get_mut(slot), &stats);
        if rewards.len() >= r::REWARD_SIZE {
            b.records.reward_mut(slot).copy_from_slice(&rewards[..r::REWARD_SIZE]);
        }
    }
    let rec = b.records.get_mut(slot);
    rec.set_f32(0x70c, 1.0);
    rec.set_u16(0x712, 0xff);
    log::trace!(
        "battle slot {slot:#x}: the monster's command list (stats +0x50, sixteen words) is not set up"
    );
    finish_record(rec, special);
}

/// btl_chr_init's monster block: the parameters from the monster file's stats chunk.
fn monster_params(rec: &mut [u8], stats: &[u8]) {
    let st = |o: usize| stats.u8_at(o);
    rec.set_u32(r::BATTLE_MODEL, u32::from(stats.u16_at(0x74)));
    let name: Vec<u8> = rec[r::MONSTER_NAME..r::MONSTER_NAME + 0x28].to_vec();
    rec[r::NAME..r::NAME + 0x28].copy_from_slice(&name);
    rec.set_u16(r::WEAPON, 0xffff);
    let (hp, mp) = (stats.u32_at(0x14), stats.u32_at(0x18));
    rec.set_u32(r::MAX_HP, hp);
    rec.set_u32(r::MAX_MP, mp);
    rec.set_u32(r::BASE_MAX_HP, hp);
    rec.set_u32(r::BASE_MAX_MP, mp);
    for i in 0..r::STAT_COUNT {
        let low = if i == 6 { 0 } else { 1 };
        rec.set_u8(r::STATS + i, clamp(i32::from(st(0x20 + i)), low, 0xff) as u8);
    }
    rec.set_u8(0x5ba, clamp(i32::from(st(0x2a)), 0, 0xff) as u8);
    rec.set_u16(r::FLAGS, stats.u16_at(0x28));
    rec.set_u8(r::WEAPON_FORMULA, 1);
    rec.set_u16(0x5c6, 0x10ff);
    rec.set_u8(0x5c5, 0xff);
    rec.set_u16(0x5bb, 0);
    rec.set_u8(0x5bd, 100);
    rec.set_u8(0x5c8, 0);
    rec.set_u8(0x5c9, st(0x77));
    rec.set_u16(0x6d6, stats.u16_at(0x78));
    rec.set_u16(0x6d8, 0);
    rec.set_u32(r::HP, hp);
    rec.set_u32(r::MP, mp);
    let overkill = stats.u32_at(0x1c);
    rec.set_u32(r::OVERKILL_OR_BASE, overkill);
    rec.set_u32(0xf60, overkill);
    rec.set_u32(r::START_HP, hp);
    rec.set_u32(r::START_MP, mp);
    rec.set_u32(0x6fc, 0xffff_ffff);
    rec.set_u8(0x6d1, 0xff);
    rec.set_u8(0x6e1, 0);
    rec.set_u8(0x719, 1);
    rec.set_u8(r::ABILITY_BITS, 0);
    for i in 1..5 {
        rec.set_u8(r::ABILITY_BITS + i, st(0x2a + i));
    }
    rec.set_u16(0x62a, 0);
    rec.set_u16(0x62e, 0);
    rec.set_u8(r::AUTO_STATUS2_ON, 0);
    for k in 0..3 {
        rec.set_u16(r::AUTO_STATUS + 2 * k, stats.u16_at(0x48 + 2 * k));
    }
    rec.set_u32(0x636, 0);
    rec.set_u16(0x63a, 0);
    for i in 0..r::RESISTANCE_COUNT {
        let v = i32::from(st(0x2f + i)) + i32::from(rec.u8_at(r::RESISTANCES + i));
        rec.set_u8(r::RESISTANCES + i, clamp(v, 0, 0xff) as u8);
    }
    rec.set_u16(r::RESIST_WORD, stats.u16_at(0x4e));
    rec.set_u16(r::FIRST_COMMAND, stats.u16_at(0x70));
    rec.set_u16(r::SECOND_COMMAND, 0xff);
    rec.set_u16(0x6d3, 0xffff);
    rec.set_u8(r::KERNEL_SLOT, st(0x76));
    rec.set_u8(r::COPY_NUMBER, 0);
}

/// 007842f0: monsters that appear more than once get a copy letter each (+0x424: a random 0–3 plus 8 per
/// copy, dealt out at random), drawn from each record's damage stream.
fn copy_letters(records: &mut Records, random: &mut RngStreams) {
    for first in r::FIRST_MONSTER..r::FIRST_MONSTER + r::MONSTERS {
        let rec = records.get(first);
        if rec.u8_at(r::IN_BATTLE) == 0 || rec.u8_at(r::OWN_FILE) == 0 {
            continue;
        }
        let end = r::FIRST_MONSTER + r::MONSTERS;
        let copies = 1
            + (first + 1..end).filter(|&s| usize::from(records.get(s).u8_at(r::FILE_SLOT)) == first).count();
        if copies < 2 {
            continue;
        }
        let mut letters: Vec<u8> = (0..copies)
            .map(|k| (random.next(damage_stream(records, first)) as u8 & 3).wrapping_add(k as u8 * 8))
            .collect();
        let mut left = copies;
        for s in first..end {
            if usize::from(records.get(s).u8_at(r::FILE_SLOT)) != first {
                continue;
            }
            let pick = (random.next(damage_stream(records, s)) as i32 % left as i32) as usize;
            left -= 1;
            records.get_mut(s).set_u8(0x424, letters[pick]);
            letters.remove(pick);
        }
    }
}

/// MsCalcDamageCommand(slot, 0): the random stream of a slot's damage rolls (monsters slot + 8, aeons 0x1b,
/// characters slot + 0x14).
pub fn damage_stream(records: &Records, slot: usize) -> usize {
    let slot = slot & 0xff;
    if r::is_monster_slot(slot) {
        slot + 8
    } else if is_aeon(records, slot) {
        0x1b
    } else {
        slot + 0x14
    }
}
