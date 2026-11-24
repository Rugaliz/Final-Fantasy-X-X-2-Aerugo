//! The save image of a new game (save_ram_init): cleared, then the settings, party data and starting
//! equipment, the characters and their names, the menu panel, the starting items, the equipment worn, the
//! system words and the sphere grid with each character's start, and the party stats.
//!
//! What it leaves out lives outside the save image: the battle random table (seeded from the clock), the
//! runtime name copies and the sphere grid's drawing data.

use std::sync::Arc;

use aerugo_formats::KernelTable;
use aerugo_vfs::{Language, Vfs};

use crate::items::{EQUIPMENT_SIZE, EQUIPMENT_SLOTS, ItemTables, offset};
use crate::stats::{self, StatTables, StatsState};

/// The save image's size.
pub const SAVE_SIZE: usize = 0x68c0;

/// The starting weapon records weapon.bin gives (more only under a debug switch).
const STARTING_EQUIPMENT: usize = 0x23;

/// Per sphere grid type (settings bits 14–15: 0 standard, 1 the second, 2 and 3 expert): each character's start
/// node (the grid's +0xf00 words) and the nodes activated for them (tables in the executable).
const GRID_STARTS: [[u16; 7]; 3] =
    [[0, 341, 540, 637, 56, 224, 448], [0, 341, 540, 637, 56, 224, 448], [43, 9, 38, 13, 24, 19, 80]];
const GRID_ACTIVE: [[&[u16]; 7]; 3] = [
    [&[], &[335, 336], &[541], &[636], &[57], &[220, 221, 222, 223], &[449, 450]],
    [&[], &[335, 336], &[541], &[636], &[57], &[220, 221, 222, 223], &[449, 450]],
    [&[], &[0, 8], &[39], &[13], &[31], &[14, 15, 16, 17], &[40, 41]],
];

/// The files a new game reads: battle set 0xd files 3 (menu), 4 (party), 5 (weapon), ply_save, 0x1f (menu
/// panel), and the sphere grid layouts (set 0x25 files 0x11–0x13).
#[derive(Debug)]
pub struct NewGameTables {
    pub menu: KernelTable,
    pub party: KernelTable,
    pub weapons: KernelTable,
    pub ply_save: KernelTable,
    pub menu_panel: KernelTable,
    pub grids: [Vec<u8>; 3],
}

impl NewGameTables {
    pub fn load(vfs: &Vfs, lang: Language) -> Option<Arc<NewGameTables>> {
        let table = |name: &str| {
            let path = format!("ffx_ps2/ffx/master/jppc/battle/kernel/{name}.bin");
            KernelTable::parse(vfs.read_localized(&path, lang).ok()?).ok()
        };
        let grid = |n: u32| vfs.read(&format!("ffx_ps2/ffx/master/jppc/menu/abmap/dat{n:02}.dat")).ok();
        Some(Arc::new(NewGameTables {
            menu: table("menu")?,
            party: table("party")?,
            weapons: table("weapon")?,
            ply_save: table("ply_save")?,
            menu_panel: table("menu_panel")?,
            grids: [grid(9)?, grid(10)?, grid(11)?],
        }))
    }
}

/// A kernel record by id, else the first segment's first record (kernel_rec), and whether it was found.
fn rec(t: &KernelTable, id: u32) -> (&[u8], bool) {
    if let Some(r) = t.record(id) {
        return (r.bytes, true);
    }
    let s = &t.segments()[0];
    (t.data().get(s.data_offset..s.data_offset + s.record_size).unwrap_or(&[]), false)
}

fn put(save: &mut [u8], at: usize, b: &[u8]) {
    if let Some(d) = save.get_mut(at..at + b.len()) {
        d.copy_from_slice(b);
    }
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |x| u16::from_le_bytes([x[0], x[1]]))
}

/// save_ram_init on `save` (`language_type` is the language type's +4, kept with each name).
pub fn save_ram_init(
    t: &NewGameTables,
    items: &ItemTables,
    st: &StatTables,
    stats_state: &mut StatsState,
    save: &mut [u8],
    language_type: u8,
) {
    let n = SAVE_SIZE.min(save.len());
    save[..n].fill(0);
    let first = |k: &KernelTable, n: usize| rec(k, 0).0.get(..n).map(<[u8]>::to_vec).unwrap_or_default();
    put(save, offset::TEXT_VARIANT, &first(&t.menu, 0x1c));
    put(save, 0x3d48, &first(&t.party, 0x84));
    // The starting equipment: weapon.bin's records until one is missing (its fallback is the first record).
    for i in 0..EQUIPMENT_SLOTS.min(STARTING_EQUIPMENT) {
        let (r, found) = rec(&t.weapons, i as u32);
        if i > 0 && !found {
            break;
        }
        let at = offset::EQUIPMENT + i * EQUIPMENT_SIZE;
        let mut e = [0u8; EQUIPMENT_SIZE];
        let n = r.len().min(EQUIPMENT_SIZE);
        e[..n].copy_from_slice(&r[..n]);
        if e[4] < 7 {
            let name = crate::items::equipment_name_id(&e);
            e[0..2].copy_from_slice(&name.to_le_bytes());
            let model = items.equipment_model(name, e[4]);
            e[0xc..0xe].copy_from_slice(&model.to_le_bytes());
        }
        put(save, at, &e);
    }
    // The characters: ply_save's records 0–0x11, each name (0x14 bytes from its text) with the language and a
    // valid byte after it.
    for chr in 0..0x12usize {
        let Some(r) = t.ply_save.record(chr as u32) else { continue };
        put(save, stats::record(chr), &r.bytes[..r.bytes.len().min(stats::RECORD_SIZE)]);
        let pool = t.ply_save.segments()[r.segment].text_pool.unwrap_or(0);
        let from = pool + usize::from(u16_at(r.bytes, 0));
        let mut name = [0u8; 0x14];
        for (k, b) in name.iter_mut().enumerate() {
            *b = t.ply_save.data().get(from + k).copied().unwrap_or(0);
        }
        let at = crate::field::NAMES + chr * crate::field::NAME_SLOT;
        put(save, at, &name);
        put(save, at + 0x12, &[language_type, 1]);
    }
    put(save, 0x3d28, &first(&t.menu_panel, 0x20));
    // The starting items: the party data's eight (id, count) pairs that are set, then empty slots.
    let mut k = 0;
    for i in 0..8 {
        let id = u16_at(save, 0x3d8c + i * 2);
        let count = save[0x3d9c + i];
        if id != 0xffff && count != 0 {
            put(save, offset::ITEM_IDS + k * 2, &id.to_le_bytes());
            save[offset::ITEM_COUNTS + k] = count;
            let bit = offset::ITEMS_GOT + usize::from((id & 0xfff) >> 4) * 2;
            let v = u16_at(save, bit) | 1 << (id & 0xf);
            put(save, bit, &v.to_le_bytes());
            k += 1;
        }
    }
    for i in k..0x100 {
        put(save, offset::ITEM_IDS + i * 2, &0xffu16.to_le_bytes());
        save[offset::ITEM_COUNTS + i] = 0;
    }
    // TOSwapInternationalEnvInit(0): settings bit 16 off.
    let w = u32::from_le_bytes(save[offset::TEXT_VARIANT..offset::TEXT_VARIANT + 4].try_into().unwrap());
    put(save, offset::TEXT_VARIANT, &(w & !0x1_0000).to_le_bytes());
    // Who wears each piece of equipment.
    for i in 0..EQUIPMENT_SLOTS {
        save[offset::EQUIPMENT + i * EQUIPMENT_SIZE + 6] = 0xff;
    }
    for chr in 0..0x12usize {
        for slot in [0x2d, 0x2e] {
            let e = save[stats::record(chr) + slot];
            if e != 0xff {
                save[offset::EQUIPMENT + usize::from(e) * EQUIPMENT_SIZE + 6] = chr as u8;
            }
        }
    }
    init_save_work(save);
    sphere_grid_init(t, save);
    stats::recompute(st, save, stats_state);
}

/// The system words at the start of the save image (and +0x1ec..+0x11ec cleared).
fn init_save_work(save: &mut [u8]) {
    let u32s: [(usize, u32); 12] = [
        (0, 0),
        (0xc, 0),
        (4, 0xffff_ffff),
        (0x14, 0),
        (0x18, 0),
        (0x1c, 0),
        (0xd2, 0),
        (0xb8, 0),
        (0xb4, 0),
        (0x20, 0xffff_ff00),
        (0x24, 0),
        (0xe4, 0),
    ];
    for (at, v) in u32s {
        put(save, at, &v.to_le_bytes());
    }
    save[0x13] = save[0x13] & 1 | 2;
    put(save, 0xd6, &0u16.to_le_bytes());
    save[0x10c..0x10c + 0x38 * 4].fill(0);
    save[0x2c..0x2c + 0x1c * 4].fill(0);
    for i in 0..8 {
        save[0xac + i] = i as u8;
    }
    put(save, 0x29, &0u16.to_le_bytes());
    save[0x28] = 0;
    save[0x2b] = 0;
    save[0xd0] = 2;
    put(save, 0xe8, &0u32.to_le_bytes());
    save[0xec] = 0;
    save[0x1ec..0x1ec + 0x1000].fill(0);
}

/// EiAbmDataInit on the save's sphere grid (+0x21ec): the layout of the type the settings pick (node kinds),
/// each character's starting nodes and start positions.
fn sphere_grid_init(t: &NewGameTables, save: &mut [u8]) {
    const GRID: usize = 0x21ec;
    let settings =
        u32::from_le_bytes(save[offset::TEXT_VARIANT..offset::TEXT_VARIANT + 4].try_into().unwrap());
    let ty = ((settings >> 14) & 3).min(2) as usize;
    save[GRID..GRID + 0x1320].fill(0);
    let d = &t.grids[ty];
    if u16_at(d, 0) == 0x31 {
        let n = usize::from(u16_at(d, 2));
        for i in 0..n {
            save[GRID + i * 2] = d.get(8 + i).copied().unwrap_or(0);
        }
    }
    for (chr, nodes) in GRID_ACTIVE[ty].iter().enumerate() {
        for &n in *nodes {
            save[GRID + usize::from(n) * 2 + 1] |= 1 << chr;
        }
    }
    for (chr, &s) in GRID_STARTS[ty].iter().enumerate() {
        put(save, GRID + 0xf00 + chr * 2, &s.to_le_bytes());
    }
}
