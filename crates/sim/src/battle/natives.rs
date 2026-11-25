//! The battle natives (group 7) and what they share: the combatant a worker stands for (worker +0xc0, set by
//! MsAtelChrInit), the target expressions (btl_select_chrs), and the stat accessors (btl_get_chr_stat /
//! btl_set_chr_stat).
//!
//! Stat ids not ported yet report a GAP once each; the accessors' other branches follow the original.

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::{NativeCall, Value};

use super::Battle;
use super::records::{self as r, Fields, clamp};

/// Battle state the natives read and write beside the records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleGlobals {
    /// The combatant acting now (DAT c9e0, 0xff none).
    pub actor: u8,
    /// The chosen command's targets (DAT c8c8, one bit per slot).
    pub command_targets: u32,
    /// A combatant chosen for the battle's display (DAT c9df, 0xff none).
    pub chosen: u8,
    /// Use each record's second command (DAT ca00, 1 at the start; 70b8 sets it).
    pub second_command_on: bool,
    /// A bit native 7001 keeps (DAT bded; nothing in the battle code reads it: unread).
    pub unread_script_flag: bool,
    /// znkd09_00: once slot 0x14's stat 0x57 is set to 4, its HP is set to 100 whenever a script sets it (DAT
    /// 1136ac0, the remaster's own rule).
    pub tanker_hp_rule: bool,
    /// Stat ids already reported as not ported.
    pub reported: std::collections::BTreeSet<(bool, i32)>,
}

impl Default for BattleGlobals {
    fn default() -> Self {
        BattleGlobals {
            actor: 0xff,
            command_targets: 0,
            chosen: 0xff,
            second_command_on: true,
            unread_script_flag: false,
            tanker_hp_rule: false,
            reported: Default::default(),
        }
    }
}

/// Target expressions the scripts pass (the original's numbering) used by name here.
pub mod target {
    /// The worker's own combatant.
    pub const SELF: i32 = -0xd;
}

/// Stat ids the scripts pass to the stat natives (the original's numbering), where their meaning is traced.
pub mod stat {
    pub const HP: i32 = 0;
    pub const MP: i32 = 1;
    pub const MAX_HP: i32 = 2;
    pub const MAX_MP: i32 = 3;
    /// Counts the targets not fallen.
    pub const NOT_FALLEN: i32 = 4;
    pub const WEAK_STATE: i32 = 8;
    /// Strength, then defence, magic, magic defence, agility, luck, evasion, accuracy (9–0x10).
    pub const STRENGTH: i32 = 9;
    pub const EVASION: i32 = 0xf;
    pub const ACCURACY: i32 = 0x10;
    pub const POISON_PERCENT: i32 = 0x11;
    pub const OVERDRIVE_MODE: i32 = 0x12;
    pub const OVERDRIVE: i32 = 0x13;
    pub const OVERDRIVE_MAX: i32 = 0x14;
    pub const ON_FIELD: i32 = 0x15;
    /// The ply_rom flag bits 0–2 (0x16–0x18; bit 2 an aeon).
    pub const ROM_FLAG0: i32 = 0x16;
    pub const IS_MONSTER: i32 = 0x19;
    pub const CENTER_SET: i32 = 0x1a;
    pub const SCRIPT_VALUE: i32 = 0x1c;
    pub const ORDER: i32 = 0x1d;
    pub const DEATH_LOOK: i32 = 0x4f;
    pub const ATTACK_NORMAL_FRAME: i32 = 0x7c;
    pub const MODEL_KIND: i32 = 0x86;
    pub const CURSOR_POINT: i32 = 0x87;
    pub const MOVE_FLAGS: i32 = 0x9e;
    pub const DIRECTION_RETURN: i32 = 0xa3;
    pub const DIRECTION_FIXED: i32 = 0xa4;
    pub const ROUND_FRAME: i32 = 0xda;
    pub const ROUND_RETURN_FRAME: i32 = 0xdb;
    /// Counts the targets neither fallen nor falling.
    pub const STANDING: i32 = 0xdf;
    pub const IDLE2_CHANCE: i32 = 0xe4;
    pub const ATTACK_INC_SPEED: i32 = 0xe6;
    pub const ATTACK_DEC_SPEED: i32 = 0xe7;
    pub const CTB: i32 = 0xe8;
    pub const BLEND_TYPE: i32 = 0xea;
    pub const SCRIPT_BIT: i32 = 0xfd;
    pub const ATTACK_NEAR_FRAME: i32 = 0x116;
    /// Counts the targets below half their maximum HP.
    pub const BELOW_HALF_HP: i32 = 0x119;
    pub const ATTACK_MOTION_FRAME: i32 = 0x123;
}

/// The combatant slot a worker stands for (0xff none).
fn own_slot(b: &Battle, c: &NativeCall<'_>) -> u8 {
    b.worker_slots.get(c.worker).copied().unwrap_or(0xff)
}

/// MsBtlChrNumCheck.
fn has_record(slot: usize) -> bool {
    slot < r::FULL_SLOTS
}

/// On the battlefield and not taken out of it (+0xdc8 set, +0xdcd clear).
fn present(b: &Battle, slot: usize) -> bool {
    let rec = b.records.get(slot);
    rec.u8_at(r::ACTIVE) != 0 && rec.u8_at(r::ESCAPED) == 0
}

fn is_aeon(b: &Battle, slot: usize) -> bool {
    b.records.get(slot).u8_at(r::ROM_FLAGS) >> 2 & 1 != 0
}

/// 00795780: knocked out at the start state (+0xdcc).
fn out_of_play(b: &Battle, slot: usize) -> bool {
    has_record(slot) && b.records.get(slot).u8_at(r::DEATH_STATE) != 0
}

/// 00795800: +0xdce.
fn hidden(b: &Battle, slot: usize) -> bool {
    has_record(slot) && b.records.get(slot).u8_at(r::FALLING) != 0
}

fn mask_where(f: impl Fn(usize) -> bool) -> u32 {
    (0..r::FULL_SLOTS).filter(|&s| f(s)).fold(0, |m, s| m | 1 << s)
}

/// btl_select_chrs: the slots (one bit each) a target expression names for `slot`. `with_absent` keeps
/// combatants that are not present for the command target lists.
pub fn select(b: &Battle, slot: u8, target: i32, with_absent_all: bool, with_absent_lists: bool) -> u32 {
    let slot = usize::from(slot);
    let one = |s: usize| if has_record(s) { 1u32 << s } else { 0 };
    if target as u32 & 0xffff_f000 == 0x1000 {
        return mask_where(|s| {
            present(b, s) && u32::from(b.records.get(s).u16_at(r::CHARACTER)) == target as u32
        });
    }
    match target {
        -0x1a => b.records.get(slot).u32_at(r::OTHER_TARGETS),
        -0x19 => {
            mask_where(|s| present(b, s) && !r::is_monster_slot(s) && !out_of_play(b, s) && !hidden(b, s))
        }
        -0x18 => mask_where(|s| !r::is_monster_slot(s) && !is_aeon(b, s)),
        -0x17 => mask_where(|s| !r::is_monster_slot(s)),
        -0x16 => one(usize::from(b.records.get(slot).u8_at(r::FILE_SLOT))),
        -0x15 => mask_where(|_| true),
        -0x14 => mask_where(|s| present(b, s) && is_aeon(b, s)),
        -0x13 => mask_where(|s| present(b, s) && !r::is_monster_slot(s) && !is_aeon(b, s)),
        -0x12 => one(usize::from(b.globals.chosen)),
        -0x11 => 1 << (b.records.get(slot).u8_at(r::LAST_ATTACKER) & 0x1f),
        -0x10 => b.records.get(slot).u32_at(r::TARGETS),
        -0xf => mask_where(|s| present(b, s) && r::is_monster_slot(s)),
        -0xe => mask_where(|s| present(b, s) && !r::is_monster_slot(s)),
        -0xd => one(slot),
        -0xc..=-6 => one(usize::from(b.front_line.get((-target - 6) as usize).copied().unwrap_or(0xff))),
        -5 => mask_where(|s| present(b, s) || with_absent_all),
        -4 => mask_where(|s| b.globals.command_targets >> s & 1 != 0 && (present(b, s) || with_absent_lists)),
        -3 | -2 => {
            let acting = b.globals.actor != 0xff
                && b.records.get(usize::from(b.globals.actor)).u8_at(r::COMMAND_RUNNING) != 0;
            if acting {
                log::error!(
                    "GAP: target expression {target} (the acting command's record, 7b09b0) is not ported"
                );
            }
            0
        }
        t => one(t as usize),
    }
}

/// btl_get_chr_stat over the slots `target` names: averages for values, counts for yes/no stats. Returns the
/// value the scripts get.
pub fn get_stat(b: &mut Battle, slot: u8, stat: i32, target: i32) -> i32 {
    let slots = select(b, slot, target, false, true);
    let (mut sum, mut averaged) = (0i32, 0i32);
    for s in (0..r::FULL_SLOTS).filter(|&s| slots >> s & 1 != 0) {
        let rec = b.records.get(s);
        let value = |at: usize| i32::from(rec.u8_at(at));
        match stat {
            stat::HP..=stat::MAX_MP => {
                averaged += 1;
                sum = sum.wrapping_add(rec.i32_at([r::HP, r::MP, r::MAX_HP, r::MAX_MP][stat as usize]));
            }
            stat::NOT_FALLEN => sum += i32::from(!out_of_play(b, s)),
            5 => sum += i32::from(has_record(s) && rec.u16_at(r::STATUS) >> 3 & 1 != 0),
            6 => sum += i32::from(hidden(b, s)),
            7 => sum += i32::from(has_record(s) && rec.u16_at(r::STATUS) >> 1 & 1 != 0),
            stat::WEAK_STATE => {
                averaged += 1;
                sum += if has_record(s) { value(r::WEAK_STATE) } else { 0 };
            }
            stat::STRENGTH..=stat::ACCURACY => {
                averaged += 1;
                sum += value(r::STATS + (stat - stat::STRENGTH) as usize);
            }
            stat::POISON_PERCENT..=stat::OVERDRIVE_MAX | 0x118 => {
                averaged += 1;
                let at = match stat {
                    stat::POISON_PERCENT => r::POISON_PERCENT,
                    stat::OVERDRIVE_MODE => r::OVERDRIVE_MODE,
                    // 0x118 reads the charge too.
                    stat::OVERDRIVE | 0x118 => r::OVERDRIVE,
                    _ => r::OVERDRIVE_MAX,
                };
                sum += value(at);
            }
            stat::ON_FIELD => sum += i32::from(rec.u8_at(r::ACTIVE) != 0),
            0x16..=0x18 => sum += i32::from(rec.u8_at(r::ROM_FLAGS) >> (stat - stat::ROM_FLAG0) & 1 != 0),
            stat::IS_MONSTER => sum += i32::from(r::is_monster_slot(s)),
            stat::CENTER_SET => sum += i32::from(rec.u8_at(r::CENTER_SET) != 0),
            stat::SCRIPT_VALUE => {
                averaged += 1;
                sum += i32::from(rec.u8_at(r::SCRIPT_VALUE) as i8);
            }
            stat::ORDER => {
                averaged += 1;
                sum += i32::from(rec.u8_at(r::ORDER) as i8);
            }
            // Status word bits 4–7, 8 (byte +0x607 bit 0), 9 and 10.
            0x25..=0x28 => sum += i32::from(rec.u16_at(r::STATUS) >> (stat - 0x21) & 1),
            0x29 => sum += i32::from(rec.u8_at(0x607) & 1),
            0x2a | 0x2b => sum += i32::from(rec.u16_at(r::STATUS) >> (stat - 0x21) & 1),
            // The status counters (+0x608..+0x614): set or not.
            0x2d..=0x39 => sum += i32::from(rec.u8_at(r::STATUS_COUNTERS + (stat - 0x2d) as usize) != 0),
            0x5b => sum += i32::from(rec.u8_at(0x420) as i8),
            0x85 => sum += i32::from(rec.u8_at(0x451) as i8),
            0xa6 => sum = sum.wrapping_add(rec.i32_at(0xd34)),
            0xdc => sum += i32::from(rec.u8_at(0x43b) as i8),
            stat::STANDING => sum += i32::from(!out_of_play(b, s) && !hidden(b, s)),
            stat::CTB => sum += i32::from(rec.u8_at(r::CTB)),
            0xf7 => sum += i32::from(rec.u8_at(0x44a) as i8),
            0xf8 => {
                sum += i32::from(
                    rec.u8_at(r::ACTIVE) != 0
                        && !out_of_play(b, s)
                        && !hidden(b, s)
                        && rec.u8_at(r::STATUS_COUNTERS) == 0
                        && rec.u8_at(r::STATUS_COUNTERS + 1) == 0,
                )
            }
            0xf9 => {
                sum += i32::from(
                    rec.u8_at(r::ACTIVE) != 0
                        && !out_of_play(b, s)
                        && !hidden(b, s)
                        && rec.u8_at(r::STATUS_COUNTERS) == 0
                        && rec.u8_at(r::STATUS_COUNTERS + 1) == 0
                        && rec.u16_at(r::STATUS) & 0x300 == 0,
                )
            }
            0x101 => sum += i32::from(rec.u8_at(0x5c4)),
            stat::BELOW_HALF_HP => sum += i32::from(rec.u32_at(r::HP) < rec.u32_at(r::MAX_HP) >> 1),
            0x155 => {
                sum += i32::from(
                    rec.u8_at(r::ACTIVE) != 0 && !hidden(b, s) && rec.u16_at(r::STATUS2) >> 8 & 1 == 0,
                )
            }
            _ => {
                if b.globals.reported.insert((false, stat)) {
                    log::error!("GAP: battle stat {stat:#x} (read) is not ported");
                }
            }
        }
    }
    if averaged > 0 { sum / averaged } else { sum }
}

/// btl_set_chr_stat on the slots `target` names; returns how many records took it.
pub fn set_stat(b: &mut Battle, slot: u8, stat: i32, target: i32, value: i32) -> i32 {
    if stat == 0x88 {
        log::error!("GAP: battle stat 0x88's sins07_07 target rule (7a8970) is not ported");
    }
    let mut value = value;
    if !b.encounter.name.contains("znkd09_00") {
        b.globals.tanker_hp_rule = false;
    } else if slot == 0x14 && stat == 0x57 && value == 4 {
        b.globals.tanker_hp_rule = true;
    } else if b.globals.tanker_hp_rule && slot == 0x14 && stat == 0 {
        value = 100;
    }
    let special = b.special();
    let at_least_one = clamp(value, 1, 0xff) as u8;
    let byte = clamp(value, 0, 0xff) as u8;
    let percent = clamp(value, 1, 100) as u8;
    let slots = select(b, slot, target, false, true);
    let mut taken = 0;
    for s in (0..r::FULL_SLOTS).filter(|&s| slots >> s & 1 != 0) {
        let rec = b.records.get_mut(s);
        let status = rec.u16_at(r::STATUS);
        match stat {
            stat::HP => {
                rec.set_i32(r::HP, value);
                if status & r::STATUS_KO != 0 && value > 0 {
                    rec.set_u16(r::STATUS, status & !r::STATUS_KO);
                }
                // The tail: the weak state again (7af260, the display's part, is not ported).
                super::setup::weak_state(rec, special);
            }
            stat::MP => rec.set_i32(r::MP, value),
            stat::MAX_HP => rec.set_i32(r::MAX_HP, value),
            stat::MAX_MP => rec.set_i32(r::MAX_MP, value),
            5..=7 => {
                let bit = [8u16, 4, 2][(stat - 5) as usize];
                let on = (value as u16).wrapping_mul(bit) & bit;
                rec.set_u16(r::STATUS, status & !bit | on);
                log::trace!("battle slot {s:#x}: the display update (79eff0) is not run");
            }
            stat::WEAK_STATE => rec.set_u8(r::WEAK_STATE, value as u8),
            // Defence to agility, and accuracy (strength also redoes the damage image, not ported).
            0xa..=0xe | stat::ACCURACY => {
                rec.set_u8(r::STATS + (stat - stat::STRENGTH) as usize, at_least_one)
            }
            stat::EVASION => rec.set_u8(r::STATS + 6, byte),
            stat::POISON_PERCENT => rec.set_u8(r::POISON_PERCENT, byte),
            stat::OVERDRIVE_MODE => rec.set_u8(r::OVERDRIVE_MODE, percent),
            stat::OVERDRIVE => rec.set_u8(r::OVERDRIVE, percent),
            stat::OVERDRIVE_MAX => rec.set_u8(r::OVERDRIVE_MAX, percent),
            stat::ROM_FLAG0 => {
                let v = rec.u8_at(r::ROM_FLAGS) & !1 | value as u8 & 1;
                rec.set_u8(r::ROM_FLAGS, v);
            }
            0x1f => rec.set_u8(0x504, value as u8 & 7),
            0x3b => {
                let v = rec.u16_at(r::ABILITY_WORDS);
                rec.set_u16(r::ABILITY_WORDS, v & !2 | (value as u16 & 1) << 1);
            }
            stat::DEATH_LOOK => {
                rec.set_u8(r::DEATH_LOOK, value as u8);
                rec.set_u8(r::DEATH_LOOK_KEPT, value as u8);
            }
            0x51 => rec.set_u8(0xdd6, value as u8 & 1),
            0x52 => rec.set_u8(0xdd9, value as u8 & 1),
            0x53 => rec.set_u8(0xdd7, value as u8 & 1),
            stat::ATTACK_NORMAL_FRAME => rec.set_u8(r::ATTACK_NORMAL_FRAME, value as u8),
            0x7d => rec.set_u8(0x41e, value as u8 & 1),
            0x7e => rec.set_u8(0x41f, value as u8 & 1),
            stat::MODEL_KIND => rec.set_u8(r::MODEL_KIND, value as u8),
            stat::CURSOR_POINT => rec.set_u8(r::CURSOR_POINT, value as u8),
            stat::MOVE_FLAGS => rec.set_i32(r::MOVE_FLAGS, value),
            stat::DIRECTION_RETURN => rec.set_u8(r::DIRECTION_RETURN, value as u8),
            stat::DIRECTION_FIXED => rec.set_u8(r::DIRECTION_FIXED, value as u8),
            stat::ROUND_FRAME => rec.set_u16(r::ROUND_FRAME, value as u16),
            stat::ROUND_RETURN_FRAME => rec.set_u16(r::ROUND_RETURN_FRAME, value as u16),
            stat::IDLE2_CHANCE => rec.set_u8(r::IDLE2_CHANCE, value as u8),
            stat::ATTACK_INC_SPEED => rec.set_u8(r::ATTACK_INC_SPEED, value as u8),
            stat::ATTACK_DEC_SPEED => rec.set_u8(r::ATTACK_DEC_SPEED, value as u8),
            stat::CTB => rec.set_u8(r::CTB, value as u8),
            stat::BLEND_TYPE => rec.set_u8(r::BLEND_TYPE, value as u8),
            stat::SCRIPT_BIT => rec.set_u8(r::SCRIPT_BIT, value as u8 & 1),
            stat::ATTACK_NEAR_FRAME => rec.set_u8(r::ATTACK_NEAR_FRAME, value as u8),
            stat::ATTACK_MOTION_FRAME => rec.set_u8(r::ATTACK_MOTION_FRAME, value as u8),
            _ => {
                if b.globals.reported.insert((true, stat)) {
                    log::error!("GAP: battle stat {stat:#x} (write) is not ported");
                }
                continue;
            }
        }
        taken += 1;
    }
    taken
}

/// btl_set_chr_fstat (7b4a90): the record's motion distances and speeds; 9 the model parameter (the model's
/// own copy is not set, 0082b880).
fn set_float_stat(b: &mut Battle, slot: u8, stat: i32, v: f32) {
    const AT: [usize; 9] = [
        r::ATTACK_START_DISTANCE,
        r::ATTACK_OFFSET,
        r::BACK_JUMP_DISTANCE,
        r::RUN_SPEED,
        r::RUN_RETURN_SPEED,
        r::RUN_FIRST_SPEED,
        r::RUN_ACCELERATION,
        r::WEIGHT,
        r::ATTACK_HEIGHT,
    ];
    let rec = b.records.get_mut(usize::from(slot));
    match stat {
        0..=8 => rec.set_f32(AT[stat as usize], v),
        9 => {
            rec.set_u8(r::MODEL_PARAMETER_SET, 1);
            rec.set_f32(r::MODEL_PARAMETER, v);
            log::trace!("battle slot {slot:#x}: model float 9 not applied (0082b880)");
        }
        _ => {}
    }
}

/// btl_set_cmd_flag (79b470): a character's use flag (+0xdd4, command id 0x0nnn) or a command's bit in the
/// record's command flags (+0x690, ids 0x3nnn).
pub fn set_command_flag(b: &mut Battle, slot: usize, command: u32, on: bool) {
    match command >> 12 {
        0 => b.records.get_mut((command & 0xfff) as usize).set_u8(r::COMMANDS_ON, u8::from(on)),
        3 => {
            let rec = b.records.get_mut(slot);
            let at = r::COMMAND_FLAGS + ((command & 0xfff) as usize >> 4) * 2;
            let bit = 1u16 << (command & 0xf);
            let v = rec.u16_at(at);
            rec.set_u16(at, if on { v | bit } else { v & !bit });
        }
        _ => {}
    }
}

/// MsNearRag: `to` as the angle nearest `from` (the difference brought into [0, 2π) with the float 2π, then
/// above π taken the other way round), with the original's float steps.
pub fn near_angle(from: f32, to: f32) -> f32 {
    const TURN: f64 = 6.283_185_482_025_146_5;
    const HALF_TURN: f64 = 3.141_592_741_012_573_2;
    let difference = f64::from(to - from) as f32;
    let turns = (f64::from(difference) / TURN).trunc() as i32 as f32;
    let mut rest = (f64::from(difference) - f64::from(turns) * TURN) as f32;
    if rest < 0.0 {
        rest = (f64::from(rest) + TURN) as f32;
    }
    if f64::from(rest) < HALF_TURN {
        (f64::from(from) + f64::from(rest)) as f32
    } else {
        (f64::from(from) - TURN + f64::from(rest)) as f32
    }
}

fn call(f: fn(&mut Battle, &mut NativeCall<'_>) -> Value) -> Native<Battle> {
    Native { init: None, wait: None, result: Some(f) }
}

/// Register the battle natives ported so far.
pub fn register(n: &mut Natives<Battle>) {
    // 7001(on): a bit the battle keeps (unread).
    n.register(
        0x7001,
        call(|b, c| {
            b.globals.unread_script_flag = c.stack.pop_int() & 1 != 0;
            Value::Int(0)
        }),
    );
    // 7005: the formation's special flag.
    n.register(0x7005, call(|b, _| Value::Int(i32::from(b.special()))));
    // 700a(mode, offset): how the record's battle height is found.
    n.register(
        0x700A,
        call(|b, c| {
            let distance = c.stack.pop_float();
            let kind = c.stack.pop_int();
            let slot = own_slot(b, c);
            if slot != 0xff {
                let rec = b.records.get_mut(usize::from(slot));
                rec.set_f32(r::HEIGHT_OFFSET, distance);
                rec.set_u8(r::HEIGHT_MODE, kind as u8);
                // 7ab300: mode 1 has no height; mode 0 needs the model's height (MsSetChrHeight).
                match kind as u8 {
                    1 => rec.set_u32(r::HEIGHT, 0),
                    0 => log::error!("GAP: battle height from the model (MsSetChrHeight) is not ported"),
                    _ => {}
                }
            }
            Value::Int(0)
        }),
    );
    // 7009(on): the model's gravity mode (off when on is set; the model's own switch, Ch_SetGravMode, is not
    // ported).
    n.register(
        0x7009,
        call(|b, c| {
            let on = c.stack.pop_int() & 1 != 0;
            let slot = own_slot(b, c);
            if has_record(usize::from(slot)) {
                b.records.get_mut(usize::from(slot)).set_u8(r::GRAVITY_MODE, u8::from(!on));
                log::trace!("battle 7009: the model's gravity mode is not set (Ch_SetGravMode)");
            }
            Value::Int(0)
        }),
    );
    // 700f(target, stat): the stat over the target's slots.
    n.register(
        0x700F,
        call(|b, c| {
            let stat = c.stack.pop_int();
            let target = c.stack.pop_int();
            let slot = own_slot(b, c);
            Value::Int(get_stat(b, slot, stat, target))
        }),
    );
    // 7018(target, stat, value).
    n.register(
        0x7018,
        call(|b, c| {
            let value = c.stack.pop_int();
            let stat = c.stack.pop_int();
            let target = c.stack.pop_int();
            let slot = own_slot(b, c);
            Value::Int(set_stat(b, slot, stat, target, value))
        }),
    );
    // 7022(who): who strikes first.
    n.register(
        0x7022,
        call(|b, c| {
            b.first_attack = c.stack.pop_int() as u8;
            Value::Int(0)
        }),
    );
    // 7024: the battle's label (area id << 16 | formation).
    n.register(
        0x7024,
        call(|b, _| {
            let area = b.area_id;
            Value::Int(i32::from(area) << 16 | i32::from(b.encounter.battle.formation_id))
        }),
    );
    // 7029(on): the record's +0x505.
    n.register(
        0x7029,
        call(|b, c| {
            let on = c.stack.pop_int() & 1;
            let slot = own_slot(b, c);
            if slot != 0xff {
                b.records.get_mut(usize::from(slot)).set_u8(r::CENTER_SET, on as u8);
            }
            Value::Int(0)
        }),
    );
    // 7028(x, y, z): the record's scale (the model's, 0082b560, is not set).
    n.register(
        0x7028,
        call(|b, c| {
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            let slot = own_slot(b, c);
            if slot != 0xff {
                let rec = b.records.get_mut(usize::from(slot));
                for (k, v) in [x, y, z].into_iter().enumerate() {
                    rec.set_f32(r::SCALE + 4 * k, v);
                }
                log::trace!("battle 7028: the model's scale is not set (0082b560)");
            }
            Value::Int(0)
        }),
    );
    // 7032(target, degrees): the targets face that way and return to it.
    n.register(
        0x7032,
        call(|b, c| {
            let degrees = c.stack.pop_float();
            let radians = (f64::from(degrees) * 3.141_592_741_012_573_2 / 180.0) as f32;
            let angle = near_angle(0.0, radians);
            let target = c.stack.pop_int();
            let slot = own_slot(b, c);
            let slots = select(b, slot, target, false, true);
            for s in (0..r::FULL_SLOTS).filter(|&s| slots >> s & 1 != 0) {
                let rec = b.records.get_mut(s);
                rec.set_u8(r::DIRECTION_SET, 1);
                rec.set_f32(r::DIRECTION, angle);
                rec.set_f32(r::HOME_DIRECTION, angle);
            }
            Value::Int(0)
        }),
    );
    // 7033(on): the model switch the weapons copy.
    n.register(
        0x7033,
        call(|b, c| {
            let on = c.stack.pop_int() & 1;
            let slot = own_slot(b, c);
            if slot != 0xff {
                b.records.get_mut(usize::from(slot)).set_u8(r::MODEL_SWITCH, on as u8);
            }
            Value::Int(0)
        }),
    );
    // 7055(on): the model's body collision (msSetChrBodyHit, on the model: not ported).
    n.register(
        0x7055,
        call(|b, c| {
            let _on = c.stack.pop_int() & 1;
            if own_slot(b, c) != 0xff {
                log::error!("GAP: battle 7055 (the model's body collision, msSetChrBodyHit) is not ported");
            }
            Value::Int(0)
        }),
    );
    // 703b(target, command, on): the command flag on every slot the target names; returns how many.
    n.register(
        0x703B,
        call(|b, c| {
            let on = c.stack.pop_int() != 0;
            let command = c.stack.pop_int() as u32;
            let target = c.stack.pop_int();
            let slot = own_slot(b, c);
            let slots = select(b, slot, target, false, true);
            let mut count = 0;
            for s in (0..r::FULL_SLOTS).filter(|&s| slots >> s & 1 != 0) {
                set_command_flag(b, s, command, on);
                count += 1;
            }
            Value::Int(count)
        }),
    );
    // 70ab(stat, value): a stat of the worker's own record.
    n.register(
        0x70AB,
        call(|b, c| {
            let value = c.stack.pop_int();
            let stat = c.stack.pop_int();
            let slot = own_slot(b, c);
            set_stat(b, slot, stat, target::SELF, value);
            Value::Int(0)
        }),
    );
    // 70b2(stat, value): a float parameter of the worker's own record.
    n.register(
        0x70B2,
        call(|b, c| {
            let value = c.stack.pop_float();
            let stat = c.stack.pop_int();
            let slot = own_slot(b, c);
            if slot != 0xff {
                set_float_stat(b, slot, stat, value);
            }
            Value::Int(0)
        }),
    );
    // 70b8(on): use the records' second commands.
    n.register(
        0x70B8,
        call(|b, c| {
            b.globals.second_command_on = c.stack.pop_int() != 0;
            Value::Int(0)
        }),
    );
}
