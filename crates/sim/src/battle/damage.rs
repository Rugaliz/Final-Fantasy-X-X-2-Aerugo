//! Damage: the formula table (789bf0) the per-hit calculation (78e630) applies.
//!
//! Integer steps follow the original's types: `quarter_signed` and friends are its truncating divisions of
//! signed values (x + (x >> 31 & mask)) >> n.

use super::records::{self as r, Fields};

/// What a formula computes against (789bf0's seventh argument): HP, MP or the CTB wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Against {
    Hp,
    Mp,
    Wait,
}

/// A signed division by 2^n truncating toward zero.
fn div_pow2(v: i32, n: u32) -> i32 {
    (v + (v >> 31 & ((1 << n) - 1))) >> n
}

/// The defence factor (0x2da − (d·51 − d²/11)/10) the physical and magic formulas scale by.
fn defence_factor(defence: i32) -> i32 {
    0x2da - (defence * 0x33 - defence * defence / 0xb) / 10
}

/// The damage formula (789bf0): `formula` 1–0x17 of `attacker` against `target` with `power`; `random`
/// applies the 240–271/256 variance from the attacker's damage stream (`draw`); `piercing` bit 0x40 drops the
/// defence and 0x80 the magic defence when the command is physical (+0x23 bit 0); a healing command (+0x20
/// bit 0x10) turns the result round unless `piercing` bit 1. Returns the damage and the defence and magic
/// defence it used (none for formulas 0xe and 0x14, which return before setting them).
#[allow(clippy::too_many_arguments)]
pub fn formula(
    attacker: &[u8],
    target: &[u8],
    command: Option<&[u8]>,
    formula: u8,
    power: i32,
    piercing: u8,
    against: Against,
    random: bool,
    draw: &mut dyn FnMut() -> u32,
    paid_gil: i32,
    target_save_value: Option<i32>,
) -> (i32, Option<(i32, i32)>) {
    let mut magic_defence = i32::from(target.u8_at(r::STATS + 3));
    let mut defence = i32::from(target.u8_at(r::STATS + 1));
    let (now, max) = match against {
        Against::Hp => (target.i32_at(r::START_HP), target.u32_at(r::MAX_HP) as i32),
        Against::Mp => (target.i32_at(r::START_MP), target.u32_at(r::MAX_MP) as i32),
        Against::Wait => (target.i32_at(r::START_WAIT), i32::from(target.u8_at(r::CTB_TURN))),
    };
    if let Some(c) = command
        && c.u8_at(0x23) & 1 != 0
    {
        if piercing & 0x40 != 0 {
            defence = 0;
        }
        if piercing & 0x80 != 0 {
            magic_defence = 0;
        }
    }
    let variance = if random { (draw() & 0x1f) as i32 + 0xf0 } else { 0x100 };
    let strength = i32::from(attacker.u8_at(r::BOOST_PHYSICAL)) + i32::from(attacker.u8_at(r::STATS));
    let magic = i32::from(attacker.u8_at(r::BOOST_MAGIC)) + i32::from(attacker.u8_at(r::STATS + 2));
    let cube = |v: i32| (v.wrapping_mul(v).wrapping_mul(v) >> 5) + 0x1e;
    // The common tail: (base · power / 16) · variance / 256.
    let scaled = |base: i32| div_pow2(div_pow2(base.wrapping_mul(power), 4).wrapping_mul(variance), 8);
    let mut result = match formula {
        1 => {
            let base = (defence_factor(defence) * cube(strength) / 0x2da)
                * (0xf - i32::from(target.u8_at(r::BOOST_PHYSICAL)))
                / 0xf;
            scaled(base)
        }
        2 => scaled(cube(strength)),
        3 => {
            let p = ((magic * magic) as u32 / 6) as i32;
            let p = (p + power).wrapping_mul(power);
            let v = (defence_factor(magic_defence) * div_pow2(p, 2) / 0x2da)
                * (0xf - i32::from(target.u8_at(r::BOOST_MAGIC)))
                / 0xf;
            div_pow2(v.wrapping_mul(variance), 8)
        }
        4 => {
            let p = ((magic * magic) as u32 / 6) as i32;
            let p = (p + power).wrapping_mul(power);
            div_pow2(div_pow2(p, 2).wrapping_mul(variance), 8)
        }
        5 => div_pow2(now.wrapping_mul(power), 4),
        6 => power * 0x32,
        7 => {
            let v = ((i32::from(attacker.u8_at(r::STATS + 2))
                + i32::from(attacker.u8_at(r::BOOST_MAGIC))
                + power)
                / 2)
            .wrapping_mul(variance)
            .wrapping_mul(power);
            div_pow2(v, 8)
        }
        8 => div_pow2(max.wrapping_mul(power), 4),
        9 => div_pow2(variance.wrapping_mul(power).wrapping_mul(0x32), 8),
        10 => ((target.u32_at(r::MAX_MP)).wrapping_mul(power as u32) >> 4) as i32,
        0xb => div_pow2(i32::from(target.u8_at(r::CTB_TURN)).wrapping_mul(power), 4),
        0xc => div_pow2(target.i32_at(r::START_MP).wrapping_mul(power), 4),
        0xd => div_pow2(target.i32_at(r::START_WAIT).wrapping_mul(power), 4),
        0xe => {
            let s = i32::from(attacker.u8_at(r::STATS));
            return (div_pow2(cube(s).wrapping_mul(power), 4), None);
        }
        0xf => scaled(cube(magic)),
        0x10 => (attacker.u32_at(r::MAX_HP).wrapping_mul(power as u32) / 10) as i32,
        0x11..=0x13 => {
            let (now, max) = if formula == 0x12 {
                (attacker.u32_at(r::MP), attacker.u32_at(r::MAX_MP))
            } else {
                (attacker.u32_at(r::HP), attacker.u32_at(r::MAX_HP))
            };
            // (The original divides by a zero maximum unchecked.)
            let percent = now.wrapping_mul(100).checked_div(max).unwrap_or(0) as i32;
            let (scale, divisor) =
                if formula == 0x13 { (0x82 - percent, 0x3c) } else { (percent + 10, 0x6e) };
            ((scale.wrapping_mul(cube(strength)) / divisor).wrapping_mul(power) >> 4).wrapping_mul(variance)
                >> 8
        }
        0x14 => {
            let m = i32::from(attacker.u8_at(r::STATS + 2));
            return (div_pow2(cube(m).wrapping_mul(power), 4), None);
        }
        0x15 => paid_gil / 10,
        // A character target's save record +0x54 times the power.
        0x16 => target_save_value.map_or(0, |v| v.wrapping_mul(power)),
        0x17 => power * 9999,
        _ => 0,
    };
    if let Some(c) = command
        && c.u8_at(0x20) & 0x10 != 0
        && piercing & 2 == 0
    {
        result = -result;
    }
    (result, Some((defence, magic_defence)))
}

/// The battle debug switches the damage steps test (BattleConfig: a91f, a90a, a90d; all off in play).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DebugSwitches {
    pub always_miss: bool,
    pub never_miss: bool,
    pub always_critical: bool,
}

/// The kernel command record's fields the damage steps read (command.bin and the monsters' magic tables).
pub mod command {
    /// +0x1c flags: bits 3–5 the hit kind, 0x40 blindness lowers the hit, 0x800000 always hits a target with
    /// no status bits 0–1.
    pub const FLAGS: usize = 0x1c;
    /// +0x20: bits 0–1 the damage class (1 physical, 2 magical), 4 may be critical, 8 its critical bonus from
    /// the attacker's weapon, 0x10 heals.
    pub const CLASS: usize = 0x20;
    pub const CRITICAL_BONUS: usize = 0x27;
    pub const ACCURACY: usize = 0x29;
}

/// The hit kind's accuracy (78a890's first switch): 1–2 the command's, 3–4 the attacker's, 5 ×2.5, 6 ×1.5, 7
/// half.
fn hit_accuracy(kind: u32, attacker: &[u8], cmd: &[u8]) -> u32 {
    let accuracy = u32::from(attacker.u8_at(r::STATS + 7));
    match kind {
        1 | 2 => u32::from(cmd.u8_at(command::ACCURACY)),
        3 | 4 => accuracy,
        5 => accuracy * 5 / 2,
        6 => accuracy * 3 / 2,
        7 => accuracy >> 1,
        _ => 0,
    }
}

/// The hit roll (78a890): 0 hit, 1 miss, 2 a sure hit on a target with no status bits 0–1. `target_hit` is
/// the hit record (+7 a reflected or guarded hit, +0x14 bit 2: no roll); `guard` 2 skips the miss.
pub fn hit_roll(
    attacker: &[u8],
    target: &[u8],
    cmd: &[u8],
    target_hit: &[u8],
    guard: i32,
    debug: &DebugSwitches,
    draw: &mut dyn FnMut() -> u32,
) -> u8 {
    let flags = cmd.u32_at(command::FLAGS);
    if flags & 0x80_0000 != 0 && target.u16_at(r::STATUS) & 3 == 0 {
        return 2;
    }
    let kind = flags >> 3 & 7;
    if kind == 0 {
        return 0;
    }
    let accuracy = hit_accuracy(kind, attacker, cmd);
    if target_hit.u8_at(7) != 0 || target_hit.u8_at(0x14) & 4 != 0 {
        return 0;
    }
    if debug.always_miss {
        return 1;
    }
    let roll = draw() as i32;
    let blind = cmd.u8_at(command::FLAGS) & 0x40 != 0 && attacker.u8_at(r::STATUS_COUNTERS + 2) != 0;
    let boosts = (i32::from(attacker.u8_at(r::ACCURACY_BOOST)) - i32::from(target.u8_at(r::EVASION_BOOST)))
        * 10
        - i32::from(target.u8_at(r::STATS + 5))
        + i32::from(attacker.u8_at(r::LUCK_BOOST));
    let mut chance = match kind {
        1 | 3 | 5 | 6 | 7 => {
            let step = (accuracy * 2 / 5) as i32 - i32::from(target.u8_at(r::STATS + 6)) + 10;
            i32::from(crate::traced_tables::HIT_CHANCES[step.clamp(0, 8) as usize])
        }
        _ => accuracy as i32 - i32::from(target.u8_at(r::STATS + 6)),
    };
    if blind {
        chance /= 10;
    }
    chance = i32::from(attacker.u8_at(r::STATS + 5))
        + i32::from(target.u8_at(r::HIT_BONUS_AGAINST))
        + chance
        + boosts;
    if (roll % 0x65 < chance && guard != 2) || debug.never_miss { 0 } else { 1 }
}

/// The critical roll (789690): with the command's bit 4 (+0x20), a roll below the attacker's luck and luck
/// boost less the target's luck, plus the target's hit bonus and the command's (or, with bit 8, the weapon's)
/// critical bonus, doubles the damage and marks the hit (0x100); limit flag 0x10 always does.
pub fn critical(
    attacker: &[u8],
    target: &[u8],
    cmd: &[u8],
    marks: &mut u32,
    damage: i32,
    always: bool,
    draw: &mut dyn FnMut() -> u32,
) -> i32 {
    let class = cmd.u8_at(command::CLASS);
    if class & 4 == 0 {
        return damage;
    }
    let roll = draw() as i32;
    let bonus =
        if class & 8 == 0 { cmd.u8_at(command::CRITICAL_BONUS) } else { attacker.u8_at(r::EQUIPMENT_POWER) };
    let chance = i32::from(attacker.u8_at(r::LUCK_BOOST)) - i32::from(target.u8_at(r::STATS + 5))
        + i32::from(target.u8_at(r::HIT_BONUS_AGAINST))
        + i32::from(attacker.u8_at(r::STATS + 5))
        + i32::from(bonus);
    if roll % 0x65 < chance || attacker.u8_at(0x640) & 0x10 != 0 || always {
        *marks |= 0x100;
        damage * 2
    } else {
        damage
    }
}

/// 0078c530: status word 2 bit 6 quarters the damage (and marks it 0x8000, +0x6da), bit 7 then takes it ×1.5.
pub fn shielded(target: &mut [u8], marks: &mut u32, damage: i32) -> i32 {
    let mut damage = damage;
    if target.u8_at(r::STATUS2) & 0x40 != 0 {
        target.set_u8(0x6da, 1);
        damage = div_pow2(damage, 2);
        *marks |= 0x8000;
    }
    if target.u8_at(r::STATUS2) & 0x80 != 0 { damage * 3 / 2 } else { damage }
}

/// 0078adc0 / 0078ad40: a magical hit on a record with hit flag +10, or a physical one with +0xb, is halved
/// (marks 0x20 / 0x40, the hit kind 2).
pub fn halved_by_hit(cmd: &[u8], marks: &mut u32, kind: &mut u32, hit: &[u8], damage: i32) -> i32 {
    let class = cmd.u8_at(command::CLASS) & 3;
    if class == 2 && hit.u8_at(10) != 0 {
        *kind = 2;
        *marks |= 0x20;
        return damage / 2;
    }
    if class == 1 && hit.u8_at(0xb) != 0 {
        *kind = 2;
        *marks |= 0x40;
        return damage / 2;
    }
    damage
}

/// 0078c0b0: status bit 9 on an attacker using its first command takes the damage ×1.5.
pub fn first_command_boost(attacker: &[u8], damage: i32) -> i32 {
    if attacker.u16_at(r::STATUS) & 0x200 != 0 && attacker.u16_at(0xf5c) == attacker.u16_at(r::FIRST_COMMAND)
    {
        damage * 3 / 2
    } else {
        damage
    }
}

/// 0078c5f0: ability word bit 9 doubles a healing item's (0x2nnn) formula 6 or 8 result.
pub fn item_healing_boost(attacker: &[u8], cmd: &[u8], command_id: u32, formula: u8, damage: i32) -> i32 {
    if attacker.u16_at(r::ABILITY_WORDS) & 0x200 != 0
        && command_id & 0xffff_f000 == 0x2000
        && cmd.u8_at(command::CLASS) & 0x10 != 0
        && (formula == 6 || formula == 8)
    {
        damage * 2
    } else {
        damage
    }
}

/// The record flags (+0x5b8) the damage steps test.
pub mod record_flags {
    /// Armoured: physical damage is a third (78a830).
    pub const ARMOURED: u8 = 1;
    /// Unaffected by the percentage formulas 5 and 8 (78ad80).
    pub const NO_PERCENT_DAMAGE: u8 = 2;
    /// Takes no physical (0x20), magical (0x40) or any (0x80) damage (78aac0).
    pub const NO_PHYSICAL: u8 = 0x20;
    pub const NO_MAGICAL: u8 = 0x40;
    pub const NO_DAMAGE: u8 = 0x80;
}

/// The damage class of a command (+0x20 bits 0–1): 1 physical, 2 magical.
fn class(cmd: &[u8]) -> u8 {
    cmd.u8_at(command::CLASS) & 3
}

/// 007891e0: the auto-abilities' percents (stats' per-character table: attack physical / magical, defence
/// physical / magical): the attacker's raise the damage, the target's take it down.
pub fn ability_percents(attacker: &[u8; 4], target: &[u8; 4], cmd: &[u8], damage: i32) -> i32 {
    let (raise, lower) = match class(cmd) {
        1 => (attacker[0], target[2]),
        2 => (attacker[1], target[3]),
        _ => return damage,
    };
    let mut damage = damage;
    if raise != 0 {
        damage += (u32::from(raise) * damage as u32) as i32 / 100;
    }
    if lower == 0 {
        return damage;
    }
    // (The original divides u·d by −100 through its reciprocal: d − u·d/100 truncated toward zero.)
    let product = (u32::from(lower) * damage as u32) as i32;
    damage - product / 100
}

/// 0078ad80: formulas 5 and 8 do nothing to a record with [`record_flags::NO_PERCENT_DAMAGE`] (the kind bit is
/// cleared and the hit counted as nullified).
pub fn percent_immunity(
    target: &[u8],
    formula: u8,
    kinds: &mut u32,
    kind: u32,
    nullified: &mut i32,
    damage: i32,
) -> i32 {
    if (formula == 8 || formula == 5) && target.u8_at(r::FLAGS) & record_flags::NO_PERCENT_DAMAGE != 0 {
        *kinds &= !kind;
        *nullified += 1;
        return 0;
    }
    damage
}

/// 0078a200: a draining command (+0x1c bit 0x100) turns round on exactly one of the attacker's status bit 1 and
/// the hit's +0x14 bit 1.
pub fn drain_turn(attacker: &[u8], cmd: &[u8], hit: &[u8], damage: i32) -> i32 {
    let turned = (attacker.u16_at(r::STATUS) >> 1 & 1) + (hit.u16_at(0x14) >> 1 & 1) == 1;
    if cmd.u32_at(command::FLAGS) & 0x100 != 0 && turned { -damage } else { damage }
}

/// 0078a830: an armoured target takes a third, unless the command (+0x1e bit 0) or the attacker (ability word
/// bit 13) pierces or the hit (+0x14 bit 6) does; `armoured` is set when it applies.
pub fn armour(
    attacker: &[u8],
    target: &[u8],
    cmd: &[u8],
    hit: &[u8],
    armoured: &mut bool,
    damage: i32,
) -> i32 {
    if cmd.u8_at(0x1e) & 1 == 0
        && target.u8_at(r::FLAGS) & record_flags::ARMOURED != 0
        && attacker.u16_at(r::ABILITY_WORDS) & 0x2000 == 0
        && hit.u8_at(0x14) & 0x40 == 0
    {
        *armoured = true;
        return damage / 3;
    }
    damage
}

/// 0078a7d0: a physical hit the hit's +0x16 bits 0x800 / 0x2000 guard halves (+0x6da set; marks 8 or 0x10).
pub fn guarded(target: &mut [u8], cmd: &[u8], marks: &mut u32, hit: &[u8], damage: i32) -> i32 {
    if class(cmd) != 1 {
        return damage;
    }
    let guard = hit.u16_at(0x16);
    if guard & 0x2800 == 0 {
        return damage;
    }
    target.set_u8(0x6da, 1);
    *marks |= if guard & 0x800 == 0 { 0x10 } else { 8 };
    damage / 2
}

/// 00789290: the attacker's status bit 4 halves the physical damage it deals, bit 5 the magical.
pub fn attacker_halved(attacker: &[u8], cmd: &[u8], damage: i32) -> i32 {
    let status = attacker.u16_at(r::STATUS);
    match class(cmd) {
        1 if status & 0x10 != 0 => damage / 2,
        2 if status & 0x20 != 0 => damage / 2,
        _ => damage,
    }
}

/// 0078aac0: a target that takes none of this class (or none at all) nullifies the hit.
pub fn damage_immunity(
    target: &[u8],
    cmd: &[u8],
    kinds: &mut u32,
    kind: u32,
    nullified: &mut i32,
    damage: i32,
) -> i32 {
    let flags = target.u8_at(r::FLAGS);
    let by_class = match class(cmd) {
        1 => flags & record_flags::NO_PHYSICAL != 0,
        2 => flags & record_flags::NO_MAGICAL != 0,
        _ => false,
    };
    if flags & record_flags::NO_DAMAGE == 0 && !by_class {
        return damage;
    }
    *kinds &= !kind;
    *nullified += 1;
    0
}

/// 0078bd90: with the attacker's timed bonus on (+0xd26), damage × (gained + 2·base) / (2·base) (+0xd2c,
/// +0xd30), in the original's float steps.
pub fn timed_bonus(attacker: &[u8], damage: i32) -> i32 {
    if attacker.u8_at(r::TIMED_BONUS_ON) == 0 {
        return damage;
    }
    let base = f64::from(f32::from_bits(attacker.u32_at(r::TIMED_BONUS_BASE)));
    let gained = f64::from(f32::from_bits(attacker.u32_at(r::TIMED_BONUS_GAINED)));
    let twice = base + base;
    ((gained + twice) * f64::from(damage as f32) / twice) as i32
}

/// 0078e0f0: a delaying command (+0x1c bit 0x2000 a turn and a half, 0x4000 three) adds its ticks to the
/// target's CTB result and marks the CTB kind (4).
pub fn delay(
    tables: &super::BattleTables,
    target: &[u8],
    cmd: &[u8],
    kinds: u32,
    ctb_result: &mut i32,
) -> u32 {
    let flags = cmd.u32_at(command::FLAGS);
    let factor = if flags & 0x4000 != 0 {
        2
    } else if flags & 0x2000 != 0 {
        1
    } else {
        return kinds;
    };
    let index = r::clamp(i32::from(target.u8_at(r::AGILITY)), 1, 0xff) - 1;
    let ticks = super::kernel_record(&tables.ctb_base, index as u32).map_or(0, |k| k.bytes.u8_at(0));
    *ctb_result += (u32::from(ticks) * 3 * factor / 2) as i32;
    kinds | 4
}

/// 0078bd50: a hit with +0x14 bit 0x800 cancels the CTB part (the kind bit cleared, counted as nullified).
pub fn delay_cancelled(
    hit: &[u8],
    marks: &mut u32,
    kind: u32,
    kinds: u32,
    nullified: &mut i32,
    ctb_result: &mut i32,
) -> u32 {
    if kinds & 4 == 0 || hit.u16_at(0x14) & 0x800 == 0 {
        return kinds;
    }
    *ctb_result = 0;
    *marks &= !kind;
    *nullified += 1;
    kinds
}
