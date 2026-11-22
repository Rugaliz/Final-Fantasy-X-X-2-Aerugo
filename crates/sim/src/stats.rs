//! Party stats in the save image: the recompute that runs whenever equipment or abilities change (every
//! character's maximum HP / MP and stats from the base values, aeons from Yuna's, and the auto-abilities of the
//! equipment worn), the equipment list it and its callers walk, and the legend weapons' ability sets.
//!
//! Character record (0x94 bytes at save +0x55cc, characters 0–7 then the aeons 8–0x11): +4 / +8 base HP / MP
//! (u32), +0xc..+0x13 base strength, defence, magic, magic defence, agility, luck, evasion, accuracy; +0x1c / +0x20
//! HP / MP now, +0x24 / +0x28 their maximums, +0x2c bit 0 in the party, +0x2d / +0x2e the weapon and armour
//! (equipment slots, 0xff none), +0x2f..+0x36 the stats, +0x3e.. the commands learned (one bit each), +0x4a..+0x4e
//! the auto-ability flag words.
//!
//! Equipment record (0x16 bytes at save +0x449c, 200): +2 in use, +3 flags (bit 1 a character's own, 2 a legend
//! weapon, 3 the first weapon), +4 the owner, +5 0 weapon / 1 armour, +6 who wears it (0xff nobody), +0xe four
//! abilities (0xff none).

use aerugo_formats::KernelTable;

use crate::items::{EQUIPMENT_SIZE, EQUIPMENT_SLOTS, offset};

pub const RECORDS: usize = 0x55cc;
pub const RECORD_SIZE: usize = 0x94;
/// The commands after 0x305f are kept for everyone at save +0x3d6c.
const SHARED_COMMANDS: usize = 0x3d6c;
/// Battles Yuna has fought (aeons' minimum stats grow every 30).
const BATTLES: usize = 0x3d14;

/// The kernel tables the recompute reads: `ply_rom` (aeon growth), `a_ability` (auto-abilities), `sum_assure`
/// (aeon minimum stats).
#[derive(Debug)]
pub struct StatTables {
    pub ply_rom: KernelTable,
    pub abilities: KernelTable,
    pub sum_assure: KernelTable,
    /// `ply_save` (the starting party records; 0x12 / 0x13 the full command sets).
    pub ply_save: KernelTable,
    /// `panel` (the sphere grid's node kinds: +0x10 what they raise, +0x12 a command, +0x14 by how much).
    pub panel: KernelTable,
}

impl StatTables {
    pub fn load(vfs: &aerugo_vfs::Vfs, lang: aerugo_vfs::Language) -> Option<std::sync::Arc<StatTables>> {
        let table = |name: &str| {
            let path = format!("ffx_ps2/ffx/master/jppc/battle/kernel/{name}.bin");
            KernelTable::parse(vfs.read_localized(&path, lang).ok()?).ok()
        };
        Some(std::sync::Arc::new(StatTables {
            ply_rom: table("ply_rom")?,
            abilities: table("a_ability")?,
            sum_assure: table("sum_assure")?,
            ply_save: table("ply_save")?,
            panel: table("panel")?,
        }))
    }
}

/// A record of a kernel table by id, else the first segment's first record.
fn kernel_rec(t: &KernelTable, id: u32) -> &[u8] {
    if let Some(r) = t.record(id) {
        return r.bytes;
    }
    let s = &t.segments()[0];
    t.data().get(s.data_offset..s.data_offset + s.record_size).unwrap_or(&[])
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |x| u16::from_le_bytes([x[0], x[1]]))
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
}

fn byte(b: &[u8], o: usize) -> u8 {
    b.get(o).copied().unwrap_or(0)
}

fn set_u16(b: &mut [u8], o: usize, v: u16) {
    if let Some(x) = b.get_mut(o..o + 2) {
        x.copy_from_slice(&v.to_le_bytes());
    }
}

fn set_u32(b: &mut [u8], o: usize, v: u32) {
    if let Some(x) = b.get_mut(o..o + 4) {
        x.copy_from_slice(&v.to_le_bytes());
    }
}

fn set_byte(b: &mut [u8], o: usize, v: u8) {
    if let Some(x) = b.get_mut(o) {
        *x = v;
    }
}

pub fn record(chr: usize) -> usize {
    RECORDS + chr * RECORD_SIZE
}

/// The save offset of an equipment id's record: ids 0x7xxx / 0xbxxx are battle buffers (none here),
/// others the slot (0 past the last); 0xff none.
pub fn equipment_at(id: u32) -> Option<usize> {
    if id == 0xff {
        return None;
    }
    let slot = (id & 0xfff) as usize;
    match (id as i32) >> 12 {
        7 | 0xb => None,
        _ => Some(offset::EQUIPMENT + if slot >= EQUIPMENT_SLOTS { 0 } else { slot } * EQUIPMENT_SIZE),
    }
}

/// The equipment ids (0x5000 + slot) of the records that pass: a free slot always does; a character's
/// own piece only for `owner` ≥ 8; otherwise the owner matches (or `owner` < 0), with `unworn` nobody wears it,
/// and `kind` 0 / 1 asks for a weapon / armour.
pub fn equipment_list(save: &[u8], owner: i32, kind: i32, unworn: bool) -> Vec<u16> {
    let mut out = Vec::new();
    for i in 0..EQUIPMENT_SLOTS {
        let r = offset::EQUIPMENT + i * EQUIPMENT_SIZE;
        let id = 0x5000 + i as u16;
        if byte(save, r + 2) == 0 {
            out.push(id);
            continue;
        }
        if byte(save, r + 3) & 2 != 0 && owner < 8 {
            continue;
        }
        let mut ok = i32::from(byte(save, r + 4)) == owner || owner < 0;
        if unworn {
            ok &= byte(save, r + 6) == 0xff;
        }
        let kind_ok = match kind {
            0 => byte(save, r + 5) == 0,
            1 => byte(save, r + 5) == 1,
            _ => true,
        };
        if kind_ok && ok {
            out.push(id);
        }
    }
    out
}

/// Character `chr` wears equipment `id` as weapon (slot 0) or armour: what it wore is worn by nobody.
pub fn equip(save: &mut [u8], chr: u8, slot: i32, id: u32) {
    let at = record(usize::from(chr)) + if slot == 0 { 0x2d } else { 0x2e };
    let id = id & 0xfff;
    let old = byte(save, at) as i8;
    if old != -1
        && let Some(r) = equipment_at(i32::from(old) as u32)
    {
        set_byte(save, r + 6, 0xff);
    }
    if id != 0xff
        && let Some(r) = equipment_at(id)
    {
        set_byte(save, r + 6, chr);
    }
    set_byte(save, at, id as u8);
}

/// Whether character `chr` has learned command `cmd` (0x3000..).
pub fn has_command(save: &[u8], chr: usize, cmd: u32) -> bool {
    if cmd & 0xffff_f000 != 0x3000 {
        return false;
    }
    let (at, bit) = if cmd < 0x3060 {
        (record(chr) + 0x3e + ((cmd & 0xfff) >> 4) as usize * 2, cmd & 0xf)
    } else {
        let c = (cmd - 0x60) & 0xfff;
        (SHARED_COMMANDS + (c >> 4) as usize * 2, (cmd - 0x60) & 0xf)
    };
    u16_at(save, at) & (1 << bit) != 0
}

/// Character `chr` learns (or forgets) command `cmd` (the battle side is not here).
pub fn set_command(save: &mut [u8], chr: usize, cmd: u32, on: bool) -> bool {
    if cmd & 0xffff_f000 != 0x3000 {
        return false;
    }
    let (at, bit) = if cmd < 0x3060 {
        (record(chr) + 0x3e + ((cmd & 0xfff) >> 4) as usize * 2, cmd & 0xf)
    } else {
        let c = (cmd - 0x60) & 0xfff;
        (SHARED_COMMANDS + (c >> 4) as usize * 2, (cmd - 0x60) & 0xf)
    };
    let m = 1u16 << bit;
    let v = u16_at(save, at) & !m | if on { m } else { 0 };
    set_u16(save, at, v);
    true
}

/// The sphere grid in the save image: 0x400 nodes of two bytes at +0x21ec (the node's `panel` record, then
/// the characters who activated it, one bit each).
const SPHERE_GRID: usize = 0x21ec;

/// A character's sphere grid totals (the ability map, as the sphere grid code builds it for
/// characters 0–6 from the activated nodes; 7 keeps none): HP and MP node amounts, the eight stats (to 255),
/// and the commands the nodes teach (six words of bits).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AbilityMap {
    pub hp: i32,
    pub mp: i32,
    pub stats: [u8; 8],
    pub commands: [u16; 6],
}

pub fn ability_map(t: &StatTables, save: &[u8], chr: usize) -> AbilityMap {
    let mut m = AbilityMap::default();
    if chr >= 7 {
        return m;
    }
    let mut sums = [0i32; 8];
    for i in 0..0x400 {
        let at = SPHERE_GRID + i * 2;
        if byte(save, at + 1) & (1 << chr) == 0 {
            continue;
        }
        let n = kernel_rec(&t.panel, u32::from(byte(save, at)));
        let (flags, amount) = (u16_at(n, 0x10), i32::from(byte(n, 0x14)));
        for (k, sum) in sums.iter_mut().enumerate() {
            if flags & (1 << k) != 0 {
                *sum += amount;
            }
        }
        if flags & 0x100 != 0 {
            m.hp += amount;
        }
        if flags & 0x200 != 0 {
            m.mp += amount;
        }
        let cmd = u32::from(u16_at(n, 0x12));
        if cmd & 0xffff_f000 == 0x3000
            && let Some(word) = m.commands.get_mut(((cmd & 0xfff) >> 4) as usize)
        {
            *word |= 1 << (cmd & 0xf);
        }
    }
    m.stats = sums.map(|v| v.min(0xff) as u8);
    m
}

/// Character `chr`'s base values: strength … accuracy, HP, MP, with its sphere grid totals (HP × 50,
/// MP × 5).
fn base(t: &StatTables, save: &[u8], chr: usize) -> [i32; 10] {
    let r = record(chr);
    let mut s = [0i32; 10];
    for (i, v) in s.iter_mut().take(8).enumerate() {
        *v = i32::from(byte(save, r + 0xc + i));
    }
    s[8] = u32_at(save, r + 4) as i32;
    s[9] = u32_at(save, r + 8) as i32;
    if chr < 8 {
        let m = ability_map(t, save, chr);
        s[8] = s[8].wrapping_add(m.hp.wrapping_mul(0x32));
        s[9] = s[9].wrapping_add(m.mp.wrapping_mul(5));
        for (v, a) in s.iter_mut().zip(m.stats) {
            *v += i32::from(a);
        }
    }
    s
}

/// What the recompute keeps besides the save image: per character the auto-abilities' percents for the four
/// values past MP (per character), and the flag it clears when a party member's auto-abilities have bit 1 of +0x4e.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsState {
    pub bonus: [[u8; 4]; 0x12],
    pub flag: u8,
}

/// The save record's values for character `chr`.
pub fn set_save_param(t: &StatTables, save: &mut [u8], st: &mut StatsState, chr: usize) {
    let r = record(chr);
    let mut pct = [0i32; 14];
    // Strength, defence, magic, magic defence, agility, luck, evasion, accuracy, HP, MP, then four more.
    let mut s = [0i32; 14];
    if chr < 8 {
        s[..10].copy_from_slice(&base(t, save, chr));
        // The sphere grid's commands are learned too.
        let m = ability_map(t, save, chr);
        for (k, w) in m.commands.iter().enumerate() {
            let at = r + 0x3e + k * 2;
            let v = u16_at(save, at) | w;
            set_u16(save, at, v);
        }
    } else {
        let k = kernel_rec(&t.ply_rom, chr as u32);
        let kb = |o: usize| i32::from(byte(k, o));
        let y = base(t, save, 1);
        let hp = y[8].clamp(0, 9999);
        let mp = y[9].clamp(0, 999);
        let st_ = y[0].clamp(1, 0xff);
        let def = y[1].clamp(1, 0xff);
        let mag = y[2].clamp(1, 0xff);
        let mdef = y[3].clamp(1, 0xff);
        let agi = y[4].clamp(1, 0xff);
        let luck = y[5].clamp(1, 0xff);
        let eva = y[6].clamp(0, 0xff);
        let acc = y[7].clamp(1, 0xff);
        let sum = mp / 10 + hp / 100 + acc + eva + agi + mdef + mag + def + st_;
        let mut hp2 = kb(0x19) * hp / 100 + kb(0x18) * sum;
        let mut mp2 = kb(0x1b) * mp / 100 + kb(0x1a) * sum / 10;
        let mut st2 = sum / kb(0x1c) + kb(0x1d) * st_ / 10;
        let mut def2 = sum / kb(0x1e) + kb(0x1f) * def / 10;
        let mut mag2 = sum / kb(0x20) + kb(0x21) * mag / 10;
        let mut mdef2 = sum / kb(0x22) + kb(0x23) * mdef / 10;
        let mut agi2 = sum / kb(0x24) + kb(0x25) * agi / 10;
        let mut eva2 = sum / kb(0x26) + kb(0x27) * eva / 10;
        let mut acc2 = sum / kb(0x28) + kb(0x29) * acc / 10;
        let n = u32_at(save, BATTLES) / 30;
        if n != 0 {
            let n = n.min(20) as i32;
            let a = kernel_rec(&t.sum_assure, (chr as i32 + (n * 5 - 9) * 2) as u32);
            let ab = |o: usize| i32::from(byte(a, o));
            hp2 = hp2.max(i32::from(u16_at(a, 0)));
            mp2 = mp2.max(i32::from(u16_at(a, 2)));
            st2 = st2.max(ab(4));
            def2 = def2.max(ab(5));
            mag2 = mag2.max(ab(6));
            mdef2 = mdef2.max(ab(7));
            acc2 = acc2.max(ab(8));
            eva2 = eva2.max(ab(9));
            agi2 = agi2.max(ab(10));
        }
        let rb = |o: usize| i32::from(byte(save, r + o));
        s[8] = hp2.wrapping_add(u32_at(save, r + 4) as i32);
        s[9] = mp2.wrapping_add(u32_at(save, r + 8) as i32);
        s[0] = st2 + rb(0xc);
        s[1] = def2 + rb(0xd);
        s[2] = mag2 + rb(0xe);
        s[3] = mdef2 + rb(0xf);
        s[4] = agi2 + rb(0x10);
        s[5] = rb(0x11) + luck;
        s[6] = eva2 + rb(0x12);
        s[7] = acc2 + rb(0x13);
    }
    if chr == 5 {
        for cmd in 0x3041..0x3054 {
            if has_command(save, 5, cmd) {
                set_command(save, 5, cmd + 0x37, true);
            }
        }
    }
    set_u32(save, r + 0x4a, 0);
    set_u16(save, r + 0x4e, 0);
    for slot in 0..2 {
        let id = u32::from(byte(save, r + 0x2d + slot));
        let Some(e) = equipment_at(id) else { continue };
        for k in 0..4 {
            let a = u16_at(save, e + 0xe + k * 2);
            if a == 0 || a == 0xff {
                continue;
            }
            let ab = kernel_rec(&t.abilities, u32::from(a & 0xfff));
            for o in [0x4a, 0x4c, 0x4e] {
                let v = u16_at(save, r + o) | u16_at(ab, o + 0x18);
                set_u16(save, r + o, v);
            }
            let (mask, amount) = (u16_at(ab, 0x56), i32::from(byte(ab, 0x55)));
            for (i, p) in pct.iter_mut().enumerate() {
                if mask & (1 << i) != 0 {
                    *p += amount;
                }
            }
        }
    }
    for (v, p) in s.iter_mut().zip(pct) {
        if p != 0 {
            *v = (v.wrapping_mul(p) / 100).wrapping_add(*v);
        }
    }
    st.bonus[chr] = [pct[10] as u8, pct[11] as u8, pct[12] as u8, pct[13] as u8];
    let flags = u16_at(save, r + 0x4c);
    let hp_max = s[8].clamp(0, if flags & 0x200 != 0 { 99999 } else { 9999 });
    let mp_max = s[9].clamp(0, if flags & 0x400 != 0 { 9999 } else { 999 });
    set_u32(save, r + 0x24, hp_max as u32);
    set_u32(save, r + 0x28, mp_max as u32);
    for (i, v) in s.iter().take(8).enumerate() {
        set_byte(save, r + 0x2f + i, (*v).clamp(if i == 6 { 0 } else { 1 }, 0xff) as u8);
    }
    let hp = (u32_at(save, r + 0x1c) as i32).clamp(0, hp_max);
    let mp = (u32_at(save, r + 0x20) as i32).clamp(0, mp_max);
    set_u32(save, r + 0x1c, hp as u32);
    set_u32(save, r + 0x20, mp as u32);
    if byte(save, r + 0x2c) & 1 != 0 && byte(save, r + 0x4e) & 2 != 0 {
        st.flag = 0;
    }
}

/// Every character's stats again.
pub fn recompute(t: &StatTables, save: &mut [u8], st: &mut StatsState) {
    st.flag = 1;
    for chr in 0..0x12 {
        set_save_param(t, save, st, chr);
    }
}

/// The legend weapon's record of character `chr`: one of the list with flag bit 2 owned by it.
pub fn legend_weapon(save: &[u8], chr: u32) -> Option<usize> {
    equipment_list(save, chr as i32, -1, false).into_iter().find_map(|id| {
        let r = equipment_at(u32::from(id))?;
        (byte(save, r + 3) & 4 != 0 && u32::from(byte(save, r + 4)) == chr).then_some(r)
    })
}

/// Where character `chr`'s legend weapon ability sets start in the table (set n ∈ {1, 2} is read at
/// base + 8 · n; the table read starts at character 0's set 1).
const LEGEND_BASES: [usize; 18] = [
    0x00, 0x10, 0x60, 0x20, 0x30, 0x40, 0x50, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xe0, 0xd0, 0xf0, 0x100,
    0x110,
];

/// Character `chr`'s legend weapon takes ability set `n` (1 or 2) and the stats are recomputed; false
/// when it has none.
pub fn set_legend_abilities(
    t: &StatTables,
    table: &[u16],
    save: &mut [u8],
    st: &mut StatsState,
    chr: u32,
    n: usize,
) -> bool {
    let Some(base) = LEGEND_BASES.get(chr as usize) else { return false };
    let Some(r) = legend_weapon(save, chr) else { return false };
    let at = (base + 8 * n - 8) / 2;
    for k in 0..4 {
        set_u16(save, r + 0xe + k * 2, table.get(at + k).copied().unwrap_or(0));
    }
    recompute(t, save, st);
    true
}
