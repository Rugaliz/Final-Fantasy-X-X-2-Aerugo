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
        0x11 | 0x12 | 0x13 => {
            let (now, max) = if formula == 0x12 {
                (attacker.u32_at(r::MP), attacker.u32_at(r::MAX_MP))
            } else {
                (attacker.u32_at(r::HP), attacker.u32_at(r::MAX_HP))
            };
            let percent = if max != 0 { now.wrapping_mul(100) / max } else { 0 } as i32;
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
