//! Natives on the save image, the party records, the event system's flags and small worker fields.

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::{NativeCall, Value};

use crate::workers;

use super::FieldState;

/// The party character records in the save image (18 of 0x94 bytes).
const PARTY_RECORDS: usize = 0x55cc;
const PARTY_RECORD: usize = 0x94;

fn save_u16(c: &NativeCall, at: usize) -> i32 {
    c.memory.global[0].get(at..at + 2).map_or(0, |b| i32::from(u16::from_le_bytes([b[0], b[1]])))
}

fn save_i32(c: &NativeCall, at: usize) -> i32 {
    c.memory.global[0].get(at..at + 4).map_or(0, |b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn set_save_u32(c: &mut NativeCall, at: usize, v: u32) {
    if let Some(b) = c.memory.global[0].get_mut(at..at + 4) {
        b.copy_from_slice(&v.to_le_bytes());
    }
}

/// The remaster's distortion switch (freshly on: +0x7e = 1, +0x78 = 0.25).
fn distortion_switch(d: &mut super::Distortion, on: bool) {
    let was = d.on;
    d.on = on;
    if !was && on {
        d.fresh = true;
        d.start = 0.25;
    }
}

/// Item slot i holds item 0x2000 + i, 99 of it (a slot already holding that item gets 99 first); the
/// menus then refresh.
fn full_items(g: &mut FieldState, c: &mut NativeCall) {
    let save = &mut c.memory.global[0];
    for i in 0..0x70usize {
        for k in 0..0x70usize {
            let id = save.get(0x3ecc + k * 2..0x3ecc + k * 2 + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
            if id == Some(0x2000 + i as u16) {
                if let Some(x) = save.get_mut(0x40cc + k) {
                    *x = 99;
                }
                break;
            }
            if g.language_type == Some(0xb) && id == Some(0x2065) {
                save[0x3ecc + k * 2..0x3ecc + k * 2 + 2].copy_from_slice(&0x2000u16.to_le_bytes());
                save[0x40cc + k] = 10;
                break;
            }
        }
        let (mut id, mut n) = (0x2000 + i as u16, 99u8);
        if g.language_type == Some(0xb) && id == 0x2065 {
            (id, n) = (0x2000, 10);
        }
        if let Some(b) = save.get_mut(0x3ecc + i * 2..0x3ecc + i * 2 + 2) {
            b.copy_from_slice(&id.to_le_bytes());
        }
        if let Some(x) = save.get_mut(0x40cc + i) {
            *x = n;
        }
    }
}

/// A party record's place (characters 0–17).
fn record(n: i32) -> Option<usize> {
    let n = (n & 0xff) as usize;
    (n < 0x12).then_some(PARTY_RECORDS + n * PARTY_RECORD)
}

fn wk<'a>(g: &'a mut FieldState, c: &NativeCall) -> &'a mut workers::Worker {
    let w = &mut g.workers.list[c.worker];
    w.set_u8(workers::LEVEL, c.level);
    w
}

/// The party list in the save image: three active slots, then 17 more.
const PARTY: usize = 0x3d58;
/// The equipment records (200 of 0x16 bytes).
const EQUIPMENT: usize = 0x449c;

fn img(c: &NativeCall, at: usize) -> u8 {
    c.memory.global[0].get(at).copied().unwrap_or(0)
}

fn set_img(c: &mut NativeCall, at: usize, v: u8) {
    if let Some(b) = c.memory.global[0].get_mut(at) {
        *b = v;
    }
}

/// A party record's flags byte (bit 0 joined, bit 1 kept from leaving, bit 2 kept in its slot, bit 4).
fn rec_flags(c: &NativeCall, chr: u32) -> u8 {
    img(c, PARTY_RECORDS + chr as usize * PARTY_RECORD + 0x2c)
}

/// Whether every joined member has its +0x4e bit 1 clear (the original keeps it as a global flag).
fn full_member(g: &mut FieldState, c: &NativeCall) {
    let mut full = 1u8;
    for n in 0..0x12u32 {
        if rec_flags(c, n) & 1 != 0 && img(c, PARTY_RECORDS + n as usize * PARTY_RECORD + 0x4e) & 2 != 0 {
            full = 0;
        }
    }
    g.workers.sys.full_member = full;
}

/// Put character `chr` (0xff: none) into party slot `slot`, swapping with whatever was there; returns −1 when done.
fn party_set(c: &mut NativeCall, chr: i32, slot: i32) -> i32 {
    let chr = if chr < 0 { 0xff } else { chr as u32 & 0xff };
    for i in 0..0x11 {
        let b = img(c, PARTY + 3 + i);
        if b != 0xff && b > 7 {
            set_img(c, PARTY + 3 + i, 0xff);
        }
    }
    let mut at: i32 = -1;
    if let Some(i) = (0..0x11).find(|&i| u32::from(img(c, PARTY + 3 + i)) == chr) {
        at = i as i32 + 3;
    } else {
        if let Some(i) = (0..3).find(|&i| u32::from(img(c, PARTY + i)) == chr) {
            at = i as i32;
        }
        if at < 0 {
            if rec_flags(c, chr) & 1 != 0 {
                if let Some(i) = (0..0x11).find(|&i| img(c, PARTY + 3 + i) == 0xff) {
                    at = i as i32 + 3;
                } else if let Some(i) = (0..3).find(|&i| img(c, PARTY + i) == 0xff) {
                    at = i as i32;
                }
            }
            if at < 0 {
                return 0;
            }
        }
    }
    if slot >= 0 {
        let mut b = img(c, PARTY + slot as usize);
        if b > 7 {
            b = 0xff;
        }
        let stays = |c: &NativeCall, n: u32| rec_flags(c, n) >> 2 & 1 != 0;
        if (chr == 0xff || !stays(c, chr)) && (b == 0xff || !stays(c, u32::from(b))) {
            set_img(c, PARTY + at as usize, b);
            set_img(c, PARTY + slot as usize, chr as u8);
            return -1;
        }
    }
    0
}

/// Mark (bit 1 of +3) the two equipment records a character (0–6) holds.
fn equip_mark(c: &mut NativeCall, chr: u32, on: bool) {
    let chr = chr & 0xff;
    if chr >= 7 {
        return;
    }
    let r = PARTY_RECORDS + chr as usize * PARTY_RECORD;
    for k in 0..2 {
        let id = img(c, r + 0x2d + k);
        if id != 0xff {
            let e = EQUIPMENT + usize::from(if id < 200 { id } else { 0 }) * 0x16 + 3;
            set_img(c, e, img(c, e) & !2 | u8::from(on) << 1);
        }
    }
}

/// 00cb: a character leaves; the active slots it held are refilled with the rest.
fn party_leave(g: &mut FieldState, c: &mut NativeCall, chr: i32) {
    party_join(g, c, chr as u32, 0);
    let mut s: [i32; 3] = std::array::from_fn(|i| i32::from(img(c, PARTY + i)));
    for v in &mut s {
        if *v == chr && *v != 0xff {
            *v = 0xff;
        }
    }
    for (slot, &v) in s.iter().enumerate() {
        party_set(c, v, slot as i32);
    }
}

/// A character joins (`join` 1) or leaves (0) the party.
fn party_join(g: &mut FieldState, c: &mut NativeCall, chr: u32, join: i32) {
    let chr = chr & 0xff;
    let r = PARTY_RECORDS + chr as usize * PARTY_RECORD;
    if join == 0 {
        if rec_flags(c, chr) >> 1 & 1 == 0 {
            set_img(c, r + 0x2c, img(c, r + 0x2c) & 0xfe);
            match (0..0x14).find(|&i| u32::from(img(c, PARTY + i)) == chr) {
                Some(i) => set_img(c, PARTY + i, 0xff),
                None => {
                    full_member(g, c);
                    return;
                }
            }
        }
    } else {
        equip_mark(c, chr, false);
        let b = (join & 1) as u8;
        set_img(c, r + 0x2c, img(c, r + 0x2c) & 0xee | b | b << 4);
        if chr < 8
            && !(0..0x14).any(|i| u32::from(img(c, PARTY + i)) == chr)
            && let Some(i) = (0..0x11).find(|&i| img(c, PARTY + 3 + i) == 0xff)
        {
            set_img(c, PARTY + 3 + i, chr as u8);
        }
    }
    full_member(g, c);
}

pub fn register(n: &mut Natives<FieldState>) {
    // 023d: a new game's save image, then the sound mode from its settings
    // and the battle data read again (which the sim keeps loaded); returns 1.
    n.register(
        0x023D,
        Native::result(|g, c| {
            if let (Some(t), Some(items), Some(st)) =
                (g.new_game_tables.clone(), g.items.clone(), g.stat_tables.clone())
            {
                let lt = g.language_type.unwrap_or(0) as u8;
                crate::new_game::save_ram_init(&t, &items, &st, &mut g.stats, &mut c.memory.global[0], lt);
                let w = save_i32(c, crate::items::offset::TEXT_VARIANT) as u32;
                let mode = if (w as u8 as i8) < 0 { 0 } else { 2 - i32::from(w & 1 != 0) };
                g.sound.sound_mode = Some(mode);
                g.sound.dolby = ((w as u8 as i8 as i32 & 0x10) >> 4) as u8;
            }
            Value::Int(1)
        }),
    );
    // 0235: the Al Bhed primer word of the save as loaded.
    n.register(
        0x0235,
        Native::result(|g, c| {
            Value::Int(g.primers_loaded.unwrap_or_else(|| save_i32(c, crate::items::offset::PRIMERS)))
        }),
    );
    // 0242: the HDD install state (2 after an install error, else the install result); on PC the install check finds no
    // HDD (its format check gives 3), so 0. 0243: the install mode (3 on PC).
    n.register(0x0242, Native::result(|_, _| Value::Int(0)));
    n.register(0x0243, Native::result(|_, _| Value::Int(3)));
    // 024a(on): settings word bit 13; the HDD mode it feeds stays off on PC (install mode 3). Returns
    // on != 0.
    n.register(
        0x024A,
        Native::result(|_, c| {
            let on = c.stack.pop_int() != 0;
            let at = crate::items::offset::TEXT_VARIANT;
            let w = save_i32(c, at) as u32 & !0x2000 | u32::from(on) << 13;
            set_save_u32(c, at, w);
            Value::Int(i32::from(on))
        }),
    );
    // 0265: a switch only the debug menu toggles.
    n.register(0x0265, Native::result(|g, _| Value::Int(g.debug_cccacc)));
    // 01d9: character 6's name becomes save text 0x2c (field 0, 4 with the text variant); returns 1.
    n.register(
        0x01D9,
        Native::result(|g, c| {
            let variant = crate::items::text_variant(&c.memory.global[0]);
            if let Some(t) = g.items.clone()
                && let Some(b) =
                    crate::items::ItemTables::text_bytes(&t.save_text, 0x202c, if variant { 4 } else { 0 })
            {
                let at = super::NAMES + 6 * super::NAME_SLOT;
                if let Some(dst) = c.memory.global[0].get_mut(at..at + b.len() + 1) {
                    dst[..b.len()].copy_from_slice(b);
                    dst[b.len()] = 0;
                }
            }
            Value::Int(1)
        }),
    );
    // 0216(window, chr, command): unless the character has the command, sound 0x26 plays and the window shows
    // battle-end text 0x1e with the character's and the command's names (values 0 and 1), placed at (0x100, 0xd0)
    // centred, holding the player's control (event byte +3 bit 2); the wait is the message wait, then one more
    // frame; at the end the character learns the command.
    n.register(
        0x0216,
        Native {
            init: Some(|g, c| {
                let cmd = c.stack.pop_int();
                let chr = c.stack.pop_int();
                let w = c.stack.pop_int();
                c.scratch[2] = chr;
                c.scratch[3] = cmd;
                c.scratch[0] = w.clamp(0, 7);
                if crate::stats::has_command(&c.memory.global[0], chr.max(0) as usize, cmd as u32) {
                    c.scratch[1] = 2;
                    return;
                }
                c.scratch[1] = 0;
                g.sound.play_simple_pub(0x8000_0026u32 as i32, 0x3f, 0x7f);
                g.workers.sys.mode3 |= 4;
                let variant = crate::items::text_variant(&c.memory.global[0]);
                let wi = c.scratch[0] as usize;
                let chr_name = if chr <= 0x13 {
                    (super::text_space::SAVE + (super::NAMES + chr.max(0) as usize * super::NAME_SLOT) as u32)
                        as i32
                } else {
                    super::text_space::EMPTY as i32
                };
                let cmd_name = g.items.clone().map_or(super::text_space::EMPTY, |t| {
                    g.texts.command_name(&t.commands, cmd as u32, variant)
                }) as i32;
                let win = &mut g.windows[wi];
                win.values.resize(win.values.len().max(0x20), (0xff, -1));
                win.values[0] = (0, chr_name);
                win.values[1] = (0, cmd_name);
                super::system_message(g, &c.memory.global[0], wi, 0x401e);
            }),
            wait: Some(|g, c| match c.scratch[1] {
                0 => {
                    let r = super::msg_wait(g, c.scratch[0] as usize);
                    if r & 1 != 0 {
                        c.scratch[1] = 1;
                    }
                    0
                }
                1 => {
                    c.scratch[1] = 2;
                    0
                }
                _ => 5,
            }),
            result: Some(|g, c| {
                g.workers.sys.mode3 &= !4;
                g.control_refresh();
                crate::stats::set_command(
                    &mut c.memory.global[0],
                    c.scratch[2].max(0) as usize,
                    c.scratch[3] as u32,
                    true,
                );
                Value::Int(0)
            }),
        },
    );
    // c03c: every character but 7 gets the commands of full-command record 0x12 (characters) or 0x13 (aeons) of
    // ply_save, and every shared command (0–0x82) is learned; returns 1.
    n.register(
        0xC03C,
        Native::result(|g, c| {
            let save = &mut c.memory.global[0];
            if let Some(t) = g.stat_tables.clone() {
                for chr in 0..0x12usize {
                    if chr == 7 {
                        continue;
                    }
                    let rec = t.ply_save.record(if chr > 7 { 0x13 } else { 0x12 }).map(|r| r.bytes.to_vec());
                    let rec = rec.unwrap_or_default();
                    for k in 0..6 {
                        let at = crate::stats::record(chr) + 0x3e + k * 2;
                        let add = rec
                            .get(0x3e + k * 2..0x40 + k * 2)
                            .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
                        if let Some(b) = save.get_mut(at..at + 2) {
                            let v = u16::from_le_bytes([b[0], b[1]]) | add;
                            b.copy_from_slice(&v.to_le_bytes());
                        }
                    }
                }
            }
            for k in 0..0x83u32 {
                crate::stats::set_command(save, 0, 0x3060 + k, true);
            }
            Value::Int(1)
        }),
    );
    // Battle labels (area id << 16 | formation) in the encounter table: 708a resets the area's weights,
    // 708b gives the formation's weight, 708c(label, w) sets it (0–15) and gives the old one.
    n.register(
        0x708A,
        Native::result(|g, c| {
            let label = c.stack.pop_int() as u32;
            if let Some(t) = g.encounters.as_mut()
                && let Some(b) = t.label(label)
            {
                std::sync::Arc::make_mut(t).reset_weights(b.area);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x708B,
        Native::result(|g, c| {
            let label = c.stack.pop_int() as u32;
            let w = g.encounters.as_ref().and_then(|t| {
                let b = t.label(label)?;
                Some(t.areas[b.area].groups[b.group].formations[b.formation].1)
            });
            Value::Int(i32::from(w.unwrap_or(0)))
        }),
    );
    n.register(
        0x708C,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let label = c.stack.pop_int() as u32;
            let Some(t) = g.encounters.as_mut() else { return Value::Int(0) };
            let Some(b) = t.label(label) else { return Value::Int(0) };
            let f = &mut std::sync::Arc::make_mut(t).areas[b.area].groups[b.group].formations[b.formation];
            let old = f.1;
            f.1 = v.clamp(0, 0xf) as u8;
            Value::Int(i32::from(old))
        }),
    );
    // 01b7(treasure): when a first weapon (flag 3, not a character's own) is held, the first one takes the
    // treasure's equipment (all but its first word; built as a pickup does); 1, else 0.
    n.register(
        0x01B7,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let save = &mut c.memory.global[0];
            let at = (0..crate::items::EQUIPMENT_SLOTS)
                .map(|i| crate::items::offset::EQUIPMENT + i * crate::items::EQUIPMENT_SIZE)
                .find(|&r| {
                    save.get(r + 2).is_some_and(|&b| b != 0)
                        && save.get(r + 3).is_some_and(|&f| f & 8 != 0 && f & 2 == 0)
                });
            let Some(r) = at else { return Value::Int(0) };
            if let Some(t) = g.items.clone()
                && let crate::items::Treasure::Equipment(e) = t.treasure(v as u16)
                && let Some(dst) = save.get_mut(r + 2..r + crate::items::EQUIPMENT_SIZE)
            {
                dst.copy_from_slice(&e[2..]);
            }
            Value::Int(1)
        }),
    );
    // 0215(chr, n): character chr's (0–0x11) legend weapon takes ability set clamp(n, 1, 2) and the party stats
    // are recomputed; 1, or 0 without one.
    n.register(
        0x0215,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            let chr = c.stack.pop_int();
            if !(0..=0x11).contains(&chr) {
                return Value::Int(0);
            }
            let (Some(t), table) = (g.stat_tables.clone(), &crate::traced_tables::LEGEND[..]) else {
                return Value::Int(0);
            };
            let n = n.clamp(1, 2) as usize;
            let save = &mut c.memory.global[0];
            Value::Int(i32::from(crate::stats::set_legend_abilities(
                &t,
                table,
                save,
                &mut g.stats,
                chr as u32,
                n,
            )))
        }),
    );
    // 021d: character 0 wears his first weapon (equipment flag bit 3) again, with the stats recomputed; 1 when
    // one was found.
    n.register(
        0x021D,
        Native::result(|g, c| {
            let Some(t) = g.stat_tables.clone() else { return Value::Int(0) };
            let save = &mut c.memory.global[0];
            let mut found = 0;
            for id in crate::stats::equipment_list(save, 0, 0, false) {
                let first = crate::stats::equipment_at(u32::from(id))
                    .is_some_and(|r| save.get(r + 3).is_some_and(|b| b & 8 != 0));
                if first {
                    crate::stats::equip(save, 0, 0, u32::from(id));
                    crate::stats::recompute(&t, save, &mut g.stats);
                    found = 1;
                }
            }
            Value::Int(found)
        }),
    );
    // 0217: the battle results buffer is reset (mode 7: cleared, its eight ids 0x7000 + i at
    // +0xee); returns 1.
    n.register(
        0x0217,
        Native::result(|g, _| {
            g.battle_get = vec![0; 0x1b0];
            for i in 0..8u16 {
                let at = 0xee + usize::from(i) * 2;
                g.battle_get[at..at + 2].copy_from_slice(&(0x7000 + i).to_le_bytes());
            }
            Value::Int(1)
        }),
    );
    register_party(n);
    // 0129(slot, r, b, g): a shape slot's colour, each 0–255; returns the slot.
    n.register(
        0x0129,
        Native::result(|g, c| {
            let a3 = c.stack.pop_int();
            let a2 = c.stack.pop_int();
            let a1 = c.stack.pop_int();
            let slot = c.stack.pop_int();
            let s = &mut g.shapes.slots[slot.clamp(0, 0x7f) as usize];
            s.color[0] = a1.clamp(0, 0xff) as u8;
            s.color[1] = a3.clamp(0, 0xff) as u8;
            s.color[2] = a2.clamp(0, 0xff) as u8;
            Value::Int(slot)
        }),
    );
    // 0171(character): HP back to the maximum (unless above it); returns the HP. 0172: MP likewise; returns the HP.
    n.register(
        0x0171,
        Native::result(|_, c| {
            let n = c.stack.pop_int();
            let Some(r) = record(n) else { return Value::Int(0) };
            let max = save_i32(c, r + 0x24) as u32;
            if let Some(b) = c.memory.global[0].get_mut(r + 0x3d) {
                *b = 0;
            }
            let cur = save_i32(c, r + 0x1c) as u32;
            if max < cur {
                return Value::Int(cur as i32);
            }
            set_save_u32(c, r + 0x1c, max);
            Value::Int(max as i32)
        }),
    );
    n.register(
        0x0172,
        Native::result(|_, c| {
            let n = c.stack.pop_int();
            let Some(r) = record(n) else { return Value::Int(0) };
            let (max, cur) = (save_i32(c, r + 0x28) as u32, save_i32(c, r + 0x20) as u32);
            if max < cur {
                return Value::Int(cur as i32);
            }
            set_save_u32(c, r + 0x20, max);
            Value::Int(save_i32(c, r + 0x1c))
        }),
    );
    n.register(0x019C, Native::result(|_, _| Value::Int(1)));
    // Save image values: 0087 the map number (+0), 0088 (+6), 0089 the entrance map (+2); 00ac sets +0xe.
    n.register(0x0087, Native::result(|_, c| Value::Int(save_u16(c, 0))));
    n.register(0x0088, Native::result(|_, c| Value::Int(save_u16(c, 6))));
    n.register(0x0089, Native::result(|_, c| Value::Int(save_u16(c, 2))));
    n.register(
        0x00AC,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            if let Some(b) = c.memory.global[0].get_mut(0xe..0x10) {
                b.copy_from_slice(&(v as u16).to_le_bytes());
            }
            Value::Int(v)
        }),
    );
    // 00e2(on): save +0x13 bit 1.
    n.register(
        0x00E2,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            if let Some(b) = c.memory.global[0].get_mut(0x13) {
                *b = *b & !2 | u8::from(v != 0) << 1;
            }
            Value::Int(v)
        }),
    );
    // 012d(i): the save's eight counters from +0x2c (below 0: the first, 8 and above: the last); 014d: the index
    // of the largest (the first of equals).
    n.register(
        0x012D,
        Native::result(|_, c| {
            let i = c.stack.pop_int();
            let at = if i < 0 {
                0x2c
            } else if i < 8 {
                0x2c + i as usize * 4
            } else {
                0x48
            };
            Value::Int(save_i32(c, at))
        }),
    );
    n.register(
        0x014D,
        Native::result(|_, c| {
            let v: Vec<i32> = (0..8).map(|k| save_i32(c, 0x2c + k * 4)).collect();
            let mut best = if -1 < v[0] { v[0] } else { -1 };
            let mut at = i32::from(best < v[1]);
            if best < v[1] {
                best = v[1];
            }
            for (k, &x) in v.iter().enumerate().take(7).skip(2) {
                if best < x {
                    at = k as i32;
                    best = x;
                }
            }
            Value::Int(if v[7] <= best { at } else { 7 })
        }),
    );
    // The event system's flags: 00aa sets bit 0 of its +0x210 word (00da reads it); 00be bit 7 of its first byte.
    n.register(
        0x00AA,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let s = &mut g.workers.sys;
            s.flags210 = s.flags210 & !1 | u32::from(v != 0);
            Value::Int(v)
        }),
    );
    n.register(0x00DA, Native::result(|g, _| Value::Int((g.workers.sys.flags210 & 1) as i32)));
    n.register(
        0x00BE,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let s = &mut g.workers.sys;
            s.flags0 = s.flags0 & 0x7f | u8::from(v != 0) << 7;
            Value::Int(i32::from(v != 0))
        }),
    );
    // Worker fields: 009a the kind 5/6 target, 00ce the mode 9 frame threshold, 00cf / 00d0 a kept value.
    n.register(
        0x009A,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let w = wk(g, c);
            if matches!(w.kind, 5 | 6) {
                let b = w.pb();
                w.set_i16(b + workers::p::TARGET, v as i16);
            }
            Value::Int(v)
        }),
    );
    n.register(
        0x00CE,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            wk(g, c).set_u16(workers::MODE9_FRAMES, v as u16);
            Value::Int(0)
        }),
    );
    n.register(
        0x00CF,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            wk(g, c).set_i32(0xaf0, v);
            Value::Int(0)
        }),
    );
    n.register(0x00D0, Native::result(|g, c| Value::Int(wk(g, c).i32(0xaf0))));
    // 00d1 / 00d2: every level's move / turn of the calling worker ends.
    n.register(
        0x00D1,
        Native::result(|g, c| {
            let w = wk(g, c);
            for l in 0..9 {
                w.move_reset(l);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x00D2,
        Native::result(|g, c| {
            let w = wk(g, c);
            for l in 0..9 {
                w.rot_reset(l);
            }
            Value::Int(0)
        }),
    );
}

/// A worker's character record (by index as the original looks it up).
fn chr_of(g: &mut FieldState, w: usize) -> Option<&mut crate::chr::Chr> {
    g.chrs.get_mut(w).and_then(Option::as_mut)
}

fn index(g: &FieldState, i: i32) -> usize {
    let n = g.workers.list.len();
    if i < 0 || i as usize >= n { 0 } else { i as usize }
}

pub(super) fn register_chr(n: &mut Natives<FieldState>) {
    // 5006(scale): the body's scale on all three axes.
    n.register(
        0x5006,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            if let Some(ch) = chr_of(g, c.worker) {
                for k in 0..3 {
                    ch.set_f32(crate::chr::SCALE + k * 4, v);
                }
            }
            Value::Int(0)
        }),
    );
    // 5009(weight): the push weight; returns the one before (0 without a body).
    n.register(
        0x5009,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let Some(ch) = chr_of(g, c.worker) else { return Value::Float(0.0) };
            let old = ch.f32(crate::chr::PUSH_WEIGHT);
            ch.set_f32(crate::chr::PUSH_WEIGHT, v);
            Value::Float(old)
        }),
    );
    // 507e(worker, v) / 5076(v): the body's +0x4d4 byte.
    n.register(
        0x507E,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            let w = index(g, i);
            if let Some(ch) = chr_of(g, w) {
                ch.set_u8(0x4d4, v as u8);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5076,
        Native::result(|g, c| {
            if let Some(ch) = chr_of(g, c.worker) {
                let v = c.stack.pop_int();
                ch.set_u8(0x4d4, v as u8);
            }
            Value::Int(0)
        }),
    );
    // 5051 / 5075(worker): the body's +0x740 value (0 without a body).
    n.register(0x5051, Native::result(|g, c| Value::Int(chr_of(g, c.worker).map_or(0, |ch| ch.i32(0x740)))));
    n.register(
        0x5075,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let w = index(g, i);
            Value::Int(chr_of(g, w).map_or(0, |ch| ch.i32(0x740)))
        }),
    );
    // 5050(worker, v): the body's +0x737 byte.
    n.register(
        0x5050,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            let w = index(g, i);
            if let Some(ch) = chr_of(g, w) {
                ch.set_u8(0x737, v as u8);
            }
            Value::Int(0)
        }),
    );
}

/// Let attachment slot `k` of worker `w` go: the attached body is freed; for bind id 7 the body's part 1
/// shows again when slot 0 goes.
fn detach(g: &mut FieldState, w: usize, k: usize) {
    if g.attachments.len() <= w {
        g.attachments.resize(w + 1, [None, None]);
    }
    g.attachments[w][k] = None;
    if let Some(wk) = g.workers.list.get_mut(w) {
        wk.set_u32(0xaf4 + k as u32 * 4, 0);
        let bind_id = wk.u16(workers::BIND_ID);
        wk.set_u16(0xb14, wk.u16(0xb14) & !(1 << k));
        if k == 0 && bind_id == 7 && chr_of(g, w).is_some() {
            let a = g.actor(w);
            if a.parts_hidden.len() > 1 {
                a.parts_hidden[1] = false;
            }
        }
    }
}

/// The neck switch: on/off, its weight, a restart when the target mode changed.
fn neck_on(ch: &mut crate::chr::Chr, v: i32) {
    ch.set_u8(0x40c, v as u8);
    if v == 0 {
        ch.set_f32(0x414, 0.0);
    } else {
        ch.set_f32(0x414, 1.0);
        if ch.u8(0x40e) != ch.u8(0x40d) {
            // The turn the neck had becomes the one the new target blends from.
            use crate::chr::elements as el;
            let q = el::quat_from(&el::mat_at(ch, 0x46c));
            for (k, v) in q.iter().enumerate() {
                ch.set_f32(0x4ac + 4 * k as u32, *v);
            }
            ch.set_f32(0x4bc, 1.0);
        }
    }
    let m = ch.u8(0x40d);
    ch.set_u8(0x40e, m);
}

/// Attach worker `w`'s body to another worker's body (`parent` −1: none) at a mount point, as the engine's attach
/// does: the parent's tag (its worker id), the mount, cleared offsets and a scale of 1.
fn attach(g: &mut FieldState, w: usize, parent: i32, mount: i32) {
    let p = (parent != -1).then(|| index(g, parent));
    let tag = p.and_then(|p| chr_of(g, p).map(|ch| ch.i32(0x190)));
    let Some(ch) = chr_of(g, w) else { return };
    match (p, tag) {
        (Some(p), Some(tag)) => {
            ch.set_u32(crate::chr::PARENT, p as u32 + 1);
            ch.set_i32(0x1b8, tag);
            ch.set_i32(0x1a0, mount);
        }
        _ => {
            ch.set_u32(crate::chr::PARENT, 0);
            ch.set_i32(0x1b8, -1);
            ch.set_i32(0x1a0, 0);
        }
    }
    ch.set_u32(0x1ac, 0);
    ch.set_u32(0x1a8, 0);
    ch.set_u32(0x1a4, 0);
    ch.set_f32(0x1b0, 1.0);
}

/// A character model's data is let go when no body uses it (the cache entries of its data).
fn chr_data_dispose(g: &mut FieldState, model: i32) -> i32 {
    let id = model as u16;
    if g.actors.iter().any(|a| a.model == Some(id)) {
        return -1;
    }
    g.chr_cache.entries.retain(|e| !(e.ty == 1 && e.id == u32::from(id)));
    0
}

/// Unbind a worker (019d): its body (attachments, the body, and with `dispose` the model's data), a camera or an
/// object binding.
fn unbind(g: &mut FieldState, w: usize, dispose: bool) {
    let (bind, id) = g.workers.list.get(w).map_or((0, 0), |wk| (wk.bind(), wk.u16(workers::BIND_ID)));
    if bind == 1 && g.chrs.get(w).is_some_and(Option::is_some) {
        for k in 0..2 {
            detach(g, w, k);
        }
        if let Some(wk) = g.workers.list.get_mut(w) {
            wk.set_u16(workers::PLACE, wk.u16(workers::PLACE) & 0xffef);
        }
        g.chrs[w] = None;
        let a = g.actor(w);
        a.model = None;
        a.player = None;
        if dispose {
            chr_data_dispose(g, i32::from(id));
        }
    } else if bind == 2 {
        // The worker's handle leaves the camera tables and its slots.
        let h = g.cam_slots.get(w).copied().unwrap_or(0);
        if h != 0 {
            g.cam.bind_off(h);
            g.cam_slots[w] = 0;
        }
    }
    let a = g.actor(w);
    a.bind_kind = 0;
    a.bind_id = 0;
    g.sync_bind(w);
}

/// A motion over a frame range (the motion range natives): `mode` 0 takes the frames from the record's frame table
/// and lengthens the end by one, 1 takes them as given, 2 lengthens the end; `other`: the worker comes first.
fn motion_range(g: &mut FieldState, c: &mut NativeCall, mode: u8, other: bool) {
    let end = c.stack.pop_int();
    let start = c.stack.pop_int();
    let loops = c.stack.pop_int();
    let _ = c.stack.pop_int();
    let motion = c.stack.pop_int();
    let w = if other {
        let i = c.stack.pop_int();
        index(g, i)
    } else {
        c.worker
    };
    let Some(model) = g.actors.get(w).and_then(|a| a.model) else { return };
    // Scene fixes the remaster makes on a few bodies (event number, story value): a body byte (+0x870).
    let scene = g.scene.unwrap_or(0);
    let story = save_u16(c, 0xbec);
    let fix = match (scene, story, model, motion, start, end) {
        (0xcd, 0x825, 0x79, 0x0079_1002, 0x76c, 0x8f2) | (0xcd, 0x825, 0x400b, 0x400b_1007, 0x76c, 0x8cd) => {
            Some(0)
        }
        (0xcd, 0x825, 0x79, 0x0079_1002, 0x8f2, -1) | (0xcd, 0x825, 0x400b, 0x400b_1007, 0x8cf, 0x8d7) => {
            Some(1)
        }
        (0x16c, 0x498, 7, 0x0007_1068, 0x2c, 0x42) | (0x16c, 0x498, 0x506a, 0x506a_1000, 0x2c, -1) => Some(0),
        (0x16c, 0x498, 7, 0x0007_1069, 0x44, 0x62) => Some(1),
        (0x11, 200, 5, 0x0005_1076, 0, -1) | (0x11, 200, 0x4001, 0x4001_1001, 0, -1) => Some(0),
        (0x11, 200, 5, 0x0005_1077, 0, -1) | (0x11, 200, 0x4001, 0x4001_1002, 0, -1) => Some(1),
        _ => None,
    };
    if let (Some(v), Some(ch)) = (fix, chr_of(g, w)) {
        ch.set_u8(0x870, v);
    }
    if model == 0x66 {
        g.chr_globals.motion_21092 = u8::from(motion == 0x21092);
    }
    let (table, extend) = match mode {
        0 => (true, true),
        1 => (false, false),
        _ => (false, true),
    };
    let groups = &g.motion_groups;
    let relations = g.model_relations.get(&model).map_or(&[][..], Vec::as_slice);
    let started = g.actors[w].player.as_mut().is_some_and(|p| {
        p.play_range(groups, model, relations, motion, loops as i16, start, end, table, extend)
    });
    if started {
        g.actors[w].motion = Some(motion);
        if let Some(ch) = chr_of(g, w) {
            ch.set_u8(0x736, 1);
            ch.set_u8(0x738, 1);
        }
    }
}

pub(super) fn register_chr2(n: &mut Natives<FieldState>) {
    // 0006(slot, model, mount): attach a body of a model to the worker's body at a mount point (slot 0/1; the old
    // one goes first); returns 1 (0 without a body or model).
    n.register(
        0x0006,
        Native::result(|g, c| {
            let mount = c.stack.pop_int();
            let model = c.stack.pop_int();
            let slot = c.stack.pop_int();
            let k = slot.clamp(0, 1) as usize;
            detach(g, c.worker, k);
            if chr_of(g, c.worker).is_none() || model == 0 {
                return Value::Int(0);
            }
            // The body (872190): its resident motions read (836870(model, 0)); model 3 hides part 1 and shows part
            // 2, model 7 shows part 1, models 0x10a0 and 0x1075 set every part's alpha to 1.0.
            let mut ch = g.allocate_body(model as u16);
            g.read_motion_file((0, 0, model as u16));
            let (mut parts_hidden, mut parts_alpha) = (Vec::new(), None);
            match model {
                3 => parts_hidden = vec![false, true, false],
                7 => parts_hidden = vec![false, false],
                0x10a0 | 0x1075 => parts_alpha = Some(1.0),
                _ => {}
            }
            ch.set_u8(0x184, 0xf);
            let wk = &g.workers.list[c.worker];
            let (bind_id, flags) = (wk.u16(workers::BIND_ID), wk.u32(0xb08 + k as u32 * 4));
            if bind_id == 7 && slot == 0 {
                let a = g.actor(c.worker);
                if a.parts_hidden.len() < 2 {
                    a.parts_hidden.resize(2, false);
                }
                a.parts_hidden[1] = true;
            }
            let parent_tag = chr_of(g, c.worker).map_or(-1, |p| p.i32(0x190));
            ch.set_u32(crate::chr::PARENT, c.worker as u32 + 1);
            ch.set_i32(0x1b8, parent_tag);
            ch.set_i32(0x1a0, mount);
            ch.set_u8(crate::chr::HEIGHT_MODE, 0);
            // 82aa70(body, 0): off the walk mesh (bit 0x80), ground normal (0, −1, 0), no walk-mesh shade.
            ch.set_f32(0x7c, 0.0);
            ch.set_f32(0x80, -1.0);
            ch.set_f32(0x84, 0.0);
            let f = ch.u32(crate::chr::FLAGS);
            ch.set_u32(crate::chr::FLAGS, (f | 0x80) & !0x40000);
            ch.set_u8(0x183, 0);
            let f = ch.u32(crate::chr::FLAGS);
            ch.set_u32(crate::chr::FLAGS, f & !(1 << 11) | (flags & 1) << 11);
            let f2 = ch.u32(crate::chr::FLAGS2);
            ch.set_u32(crate::chr::FLAGS2, f2 & !8 | (flags >> 1 & 1) << 3);
            if g.attachments.len() <= c.worker {
                g.attachments.resize(c.worker + 1, [None, None]);
            }
            g.attachments[c.worker][k] = Some(super::Attachment {
                model: model as u16,
                chr: ch,
                mount,
                hidden: false,
                parts_hidden,
                parts_alpha,
            });
            let wk = &mut g.workers.list[c.worker];
            wk.set_u32(0xaf4 + k as u32 * 4, 1);
            wk.set_i32(0xafc + k as u32 * 4, mount);
            wk.set_u16(0xb14, wk.u16(0xb14) | 1 << k);
            wk.set_i16(0xb04 + k as u32 * 2, model as i16);
            Value::Int(1)
        }),
    );
    // 024b(slot, hide): hide or show an attached body (a battle-only fix aside); returns the switch.
    n.register(
        0x024B,
        Native::result(|g, c| {
            let hide = c.stack.pop_int() != 0;
            let slot = c.stack.pop_int();
            let k = slot.clamp(0, 1) as usize;
            let wk = &g.workers.list[c.worker];
            if wk.u16(0xb14) & (1 << k) != 0
                && wk.i16(0xb04 + k as u32 * 2) != 0xff
                && let Some(Some(a)) = g.attachments.get_mut(c.worker).map(|s| &mut s[k])
            {
                a.hidden = hide;
            }
            Value::Int(i32::from(hide))
        }),
    );
    // 022f(slot, on): an attached body's +0x198 bit 3, and the slot's flag bit 1 (kept for the next attach).
    n.register(
        0x022F,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let slot = c.stack.pop_int();
            let k = slot.clamp(0, 1) as usize;
            let attached = g.workers.list[c.worker].u16(0xb14) & (1 << k) != 0;
            if attached && let Some(Some(a)) = g.attachments.get_mut(c.worker).map(|s| &mut s[k]) {
                let f = a.chr.u32(crate::chr::FLAGS2);
                a.chr.set_u32(crate::chr::FLAGS2, f & !8 | u32::from(on) << 3);
            }
            let wk = &mut g.workers.list[c.worker];
            let at = 0xb08 + k as u32 * 4;
            wk.set_u32(at, wk.u32(at) & !2 | u32::from(on) << 1);
            Value::Int(i32::from(on))
        }),
    );
    // 0007(slot): let an attached body go; returns 1.
    n.register(
        0x0007,
        Native::result(|g, c| {
            let slot = c.stack.pop_int();
            detach(g, c.worker, slot.clamp(0, 1) as usize);
            Value::Int(1)
        }),
    );
    // The motion range natives: 5019 (frame table), 5042 (frames), 5044 / 5046 (frames, another worker's body).
    n.register(
        0x5019,
        Native::result(|g, c| {
            motion_range(g, c, 0, false);
            Value::Int(0)
        }),
    );
    n.register(
        0x5042,
        Native::result(|g, c| {
            motion_range(g, c, 1, false);
            Value::Int(0)
        }),
    );
    for id in [0x5044u16, 0x5046] {
        n.register(
            id,
            Native::result(|g, c| {
                motion_range(g, c, 1, true);
                Value::Int(0)
            }),
        );
    }
    // 506d(degrees, tilt): the body's X rotation (+0x160 the wrapped angle; without a tilt +0x15c too and +0x164
    // 0, else +0x164 the tilt in radians).
    n.register(
        0x506D,
        Native::result(|g, c| {
            let tilt = c.stack.pop_float();
            let deg = c.stack.pop_float();
            if let Some(ch) = chr_of(g, c.worker) {
                let a = crate::chr::wrap(((f64::from(deg) * f64::from(crate::chr::BODY_PI)) / 180.0) as f32);
                let r = crate::chr::wrap(a);
                ch.set_f32(0x160, r);
                if tilt == 0.0 {
                    ch.set_f32(0x15c, r);
                    ch.set_f32(0x164, 0.0);
                } else {
                    ch.set_f32(0x164, ((f64::from(tilt) * f64::from(crate::chr::BODY_PI)) / 180.0) as f32);
                }
            }
            Value::Int(0)
        }),
    );
    // 506f(v): the body steps while held (+0x181).
    n.register(
        0x506F,
        Native::result(|g, c| {
            if chr_of(g, c.worker).is_some() {
                let v = c.stack.pop_int();
                if let Some(ch) = chr_of(g, c.worker) {
                    ch.set_u8(0x181, v as u8);
                }
            }
            Value::Int(0)
        }),
    );
    // 507a(worker): the body looks at another worker's body (neck mode 2, the target (Aerugo: worker + 1, 0
    // without a body)), neck on.
    n.register(
        0x507A,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let t = index(g, i);
            let target = if chr_of(g, t).is_some() { t as u32 + 1 } else { 0 };
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_u8(0x40d, 2);
                ch.set_u32(0x4c0, target);
                neck_on(ch, 1);
            }
            Value::Int(0)
        }),
    );
    // Body fields (the value is taken only when there is a body for those marked so):
    // 503a(v): +0x174 = v·100, returns the old value / 100.
    n.register(
        0x503A,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let Some(ch) = chr_of(g, c.worker) else { return Value::Float(0.0) };
            let old = ch.f32(0x174);
            ch.set_f32(0x174, (f64::from(v) * 100.0) as f32);
            Value::Float((f64::from(old) / 100.0) as f32)
        }),
    );
    fn flag_bit(ch: &mut crate::chr::Chr, at: u32, bit: u32, v: i32) {
        let f = ch.u32(at);
        ch.set_u32(at, f ^ (((v as u32) << bit) ^ f) & (1 << bit));
    }
    n.register(
        0x5067,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(ch) = chr_of(g, c.worker) {
                flag_bit(ch, crate::chr::FLAGS, 19, v);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x506A,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_u8(0x188, v as u8);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x506E,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_f32(0x1b4, v);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5080,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_f32(0x51c, v);
            }
            Value::Int(0)
        }),
    );
    // 5072: the body's +0x824 value (0 without a body).
    n.register(
        0x5072,
        Native::result(|g, c| Value::Int(chr_of(g, c.worker).map_or(0, |ch| i32::from(ch.i16(0x824))))),
    );
    // 507c: the body looks the default way (neck mode 1), neck on.
    n.register(
        0x507C,
        Native::result(|g, c| {
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_u8(0x40d, 1);
                neck_on(ch, 1);
            }
            Value::Int(0)
        }),
    );
    // 5082 / 5083 / 508e: flag bits (+0x194 bit 22, +0x198 bit 3, +0x820 bit 0); 5090: +0x871 byte.
    for (id, at, bit) in [(0x5082u16, crate::chr::FLAGS, 22u32), (0x5083, crate::chr::FLAGS2, 3)] {
        n.register(
            id,
            Native::result(if bit == 22 {
                |g, c| {
                    if chr_of(g, c.worker).is_some() {
                        let v = c.stack.pop_int();
                        if let Some(ch) = chr_of(g, c.worker) {
                            flag_bit(ch, crate::chr::FLAGS, 22, v);
                        }
                    }
                    Value::Int(0)
                }
            } else {
                |g, c| {
                    if chr_of(g, c.worker).is_some() {
                        let v = c.stack.pop_int();
                        if let Some(ch) = chr_of(g, c.worker) {
                            flag_bit(ch, crate::chr::FLAGS2, 3, v);
                        }
                    }
                    Value::Int(0)
                }
            }),
        );
        let _ = at;
    }
    n.register(
        0x508E,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(ch) = chr_of(g, c.worker) {
                flag_bit(ch, 0x820, 0, v);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5090,
        Native::result(|g, c| {
            if chr_of(g, c.worker).is_some() {
                let v = c.stack.pop_int();
                if let Some(ch) = chr_of(g, c.worker) {
                    ch.set_u8(0x871, v as u8);
                }
            }
            Value::Int(0)
        }),
    );
    // 5086: +0x194 bit 3 set unless bit 0 is.
    n.register(
        0x5086,
        Native::result(|g, c| {
            if let Some(ch) = chr_of(g, c.worker) {
                let f = ch.u32(crate::chr::FLAGS);
                if f & 1 == 0 {
                    ch.set_u32(crate::chr::FLAGS, f | 8);
                }
            }
            Value::Int(0)
        }),
    );
    // 5089 / 508b: a motion over a frame range, the end lengthened.
    for id in [0x5089u16, 0x508B] {
        n.register(
            id,
            Native::result(|g, c| {
                motion_range(g, c, 2, false);
                Value::Int(0)
            }),
        );
    }
    // 5014(value, frames): the body's eased value at +0x360 (target +0x364, frames +0x368; none: at once).
    n.register(
        0x5014,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            let v = c.stack.pop_float();
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_f32(0x364, v);
                ch.set_i32(0x368, frames);
                if frames == 0 {
                    ch.set_f32(0x360, v);
                }
            }
            Value::Int(0)
        }),
    );
    // 5049(worker): wait while the body's range motion plays (no body: the wait never ends).
    n.register(
        0x5049,
        Native {
            init: Some(|g, c| {
                let i = c.stack.pop_int();
                let w = index(g, i);
                c.scratch[0] = if chr_of(g, w).is_some() { w as i32 + 1 } else { 0 };
            }),
            wait: Some(|g, c| {
                let w = c.scratch[0];
                if w == 0 {
                    return 0;
                }
                let w = (w - 1) as usize;
                let playing = chr_of(g, w).is_some_and(|ch| ch.u8(0x736) != 0)
                    && g.actors.get(w).and_then(|a| a.player.as_ref()).is_some_and(|p| p.playing);
                if playing { 0 } else { 5 }
            }),
            result: None,
        },
    );
    // 5054(part, hide): hide or show a part of the body (the model's part count bounds it); returns 0.
    n.register(
        0x5054,
        Native::result(|g, c| {
            let hide = c.stack.pop_int();
            let part = c.stack.pop_int();
            if chr_of(g, c.worker).is_some()
                && let Ok(p) = usize::try_from(part)
            {
                let a = g.actor(c.worker);
                if a.parts_hidden.len() <= p {
                    a.parts_hidden.resize(p + 1, false);
                }
                a.parts_hidden[p] = hide & 1 != 0;
            }
            Value::Int(0)
        }),
    );
    // 5081(v): bit 2 of the body's second flags.
    n.register(
        0x5081,
        Native::result(|g, c| {
            if chr_of(g, c.worker).is_some() {
                let v = c.stack.pop_int() as u32;
                if let Some(ch) = chr_of(g, c.worker) {
                    let f = ch.u32(crate::chr::FLAGS2);
                    ch.set_u32(crate::chr::FLAGS2, f ^ ((v << 2) ^ f) & 4);
                }
            }
            Value::Int(0)
        }),
    );
    // 507b(x, y, z): the body looks at a point (neck mode 3; the fourth value the original copies is stack
    // leftover, kept 0), neck on.
    n.register(
        0x507B,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_u8(0x40d, 3);
                ch.set_f32(0x4c0, x);
                ch.set_f32(0x4c4, y);
                ch.set_f32(0x4c8, z);
                ch.set_u32(0x4cc, 0);
                neck_on(ch, 1);
            }
            Value::Int(0)
        }),
    );
    // 507d(yaw, pitch): the body looks along angles (degrees; neck mode 4), neck on.
    n.register(
        0x507D,
        Native::result(|g, c| {
            let pitch = c.stack.pop_float();
            let yaw = c.stack.pop_float();
            if let Some(ch) = chr_of(g, c.worker) {
                let pi = f64::from(crate::chr::BODY_PI);
                let b = crate::chr::wrap(((f64::from(pitch) * pi) / 180.0) as f32);
                let a = crate::chr::wrap(((f64::from(yaw) * pi) / 180.0) as f32);
                ch.set_f32(0x4c0, a);
                ch.set_u8(0x40d, 4);
                ch.set_f32(0x4c4, b);
                neck_on(ch, 1);
            }
            Value::Int(0)
        }),
    );
    // 5079(on): the body's neck (looking) on: +0x40c, its weight +0x414 (1 / 0); turning on with a new target
    // mode restarts the neck (+0x4bc = 1); +0x40e follows +0x40d.
    n.register(
        0x5079,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(ch) = chr_of(g, c.worker) {
                neck_on(ch, v);
            }
            Value::Int(0)
        }),
    );
    // 508f(v): the body's +0x870 byte; 5084(v): its +0x420 value.
    n.register(
        0x508F,
        Native::result(|g, c| {
            if chr_of(g, c.worker).is_some() {
                let v = c.stack.pop_int();
                if let Some(ch) = chr_of(g, c.worker) {
                    ch.set_u8(0x870, v as u8);
                }
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5084,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            // 835020: 0 stands for 6.0.
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_f32(0x420, if v == 0.0 { 6.0 } else { v });
            }
            Value::Int(0)
        }),
    );
    // 505d: the motion the body set last (+0x72c; 0 without a body).
    n.register(
        0x505D,
        Native::result(|g, c| {
            if chr_of(g, c.worker).is_none() {
                return Value::Int(0);
            }
            Value::Int(g.actors.get(c.worker).and_then(|a| a.player.as_ref()).map_or(0, |p| p.motion))
        }),
    );
    // 503f(motion) / 5048(worker, motion): set a body's motion.
    n.register(
        0x503F,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            if chr_of(g, c.worker).is_some() {
                g.set_motion(c.worker, m);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5048,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            let i = c.stack.pop_int();
            let w = index(g, i);
            if chr_of(g, w).is_some() {
                g.set_motion(w, m);
            }
            Value::Int(0)
        }),
    );
    // 0158(slot) (85cfa0): while the system's +0x1a word is 0: the body's +0x184 (bound to a body, 835b50), the
    // worker's +0xaf, and slots below 16 name the worker (885fd0); returns the slot.
    n.register(
        0x0158,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if g.workers.sys.word_1a != 0 {
                return Value::Int(v);
            }
            let bound = g.workers.list[c.worker].u8(workers::BIND) == 1;
            if bound && let Some(ch) = chr_of(g, c.worker) {
                ch.set_u8(0x184, v as u8);
            }
            let w = wk(g, c);
            w.set_u8(0xaf, v as u8);
            let id = i32::from(w.id());
            if (0..16).contains(&v) {
                g.workers.sys.body_slots[v as usize] = id;
            }
            Value::Int(v)
        }),
    );
    // 019d(dispose): unbind the worker; 0005: the same without letting the data go.
    n.register(
        0x019D,
        Native::result(|g, c| {
            let d = c.stack.pop_int();
            unbind(g, c.worker, d != 0);
            Value::Int(0)
        }),
    );
    n.register(
        0x0005,
        Native::result(|g, c| {
            unbind(g, c.worker, false);
            Value::Int(0)
        }),
    );
    // 503d(model) / 503e(model, warn): let a model's data go when no body uses it.
    n.register(
        0x503D,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            chr_data_dispose(g, m);
            Value::Int(0)
        }),
    );
    n.register(
        0x503E,
        Native::result(|g, c| {
            c.stack.pop_int();
            let m = c.stack.pop_int();
            chr_data_dispose(g, m);
            Value::Int(0)
        }),
    );
    // 5068: wait for every character read (reads complete at once here).
    n.register(0x5068, Native { init: None, wait: Some(|_, _| 1), result: None });
    // 400e(v) / 400f(v): the character engine's two flags (bit 0); 403a(v): its mode byte.
    n.register(
        0x400E,
        Native::result(|g, c| {
            g.chr_globals.flag_b1 = (c.stack.pop_int() & 1) as u8;
            Value::Int(0)
        }),
    );
    n.register(
        0x400F,
        Native::result(|g, c| {
            g.chr_globals.flag_b2 = (c.stack.pop_int() & 1) as u8;
            Value::Int(0)
        }),
    );
    n.register(
        0x403A,
        Native::result(|g, c| {
            g.chr_globals.byte_778 = c.stack.pop_int() as u8;
            Value::Int(0)
        }),
    );
    n.register(
        0x403C,
        Native::result(|g, c| {
            g.chr_globals.water2 = c.stack.pop_float();
            Value::Int(0)
        }),
    );
    n.register(
        0x4034,
        Native::result(|g, c| {
            g.chr_globals.map_alpha = c.stack.pop_int() as u8;
            Value::Float(0.0)
        }),
    );
    n.register(
        0x4045,
        Native::result(|g, c| {
            g.chr_globals.byte_7c5 = c.stack.pop_int() as u8;
            Value::Int(0)
        }),
    );
    n.register(
        0x4046,
        Native::result(|g, c| {
            g.chr_globals.byte_8fd = c.stack.pop_int() as u8;
            Value::Int(0)
        }),
    );
    // 4044(a, b, c): takes its arguments and does nothing.
    n.register(
        0x4044,
        Native::result(|_, c| {
            for _ in 0..3 {
                c.stack.pop_int();
            }
            Value::Int(0)
        }),
    );
    // 5007(axes, x, y, z): the body's scale on the axes in the mask.
    n.register(
        0x5007,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            let mask = c.stack.pop_int();
            if let Some(ch) = chr_of(g, c.worker) {
                for (k, v) in [x, y, z].into_iter().enumerate() {
                    if mask & (1 << k) != 0 {
                        ch.set_f32(crate::chr::SCALE + k as u32 * 4, v);
                    }
                }
            }
            Value::Int(0)
        }),
    );
    // 500b(height): the body's ground height.
    n.register(
        0x500B,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_f32(crate::chr::GROUND, v);
            }
            Value::Int(0)
        }),
    );
    // 500c(on): no longer does anything (gravity has its own native).
    n.register(
        0x500C,
        Native::result(|_, c| {
            c.stack.pop_int();
            Value::Int(0)
        }),
    );
    // 5018(v) / 504c(worker, v): the body's +0x750 value.
    n.register(
        0x5018,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(ch) = chr_of(g, c.worker) {
                ch.set_u16(0x750, v as u16);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x504C,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            let w = index(g, i);
            if let Some(ch) = chr_of(g, w) {
                ch.set_u16(0x750, v as u16);
            }
            Value::Int(0)
        }),
    );
    // 501f: the body's +0x736 byte is set.
    n.register(
        0x501F,
        Native::result(|g, c| Value::Int(i32::from(chr_of(g, c.worker).is_some_and(|ch| ch.u8(0x736) != 0)))),
    );
    // 5056(v): bit 0 of the body's flags; returns the bit before. 5058(v): every body's.
    n.register(
        0x5056,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as u32;
            let Some(ch) = chr_of(g, c.worker) else { return Value::Int(0) };
            let f = ch.u32(crate::chr::FLAGS);
            ch.set_u32(crate::chr::FLAGS, (f ^ v) & 1 ^ f);
            Value::Int((f & 1) as i32)
        }),
    );
    n.register(
        0x5058,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as u32;
            for ch in g.chrs.iter_mut().flatten().filter(|ch| ch.u8(2) != 0) {
                let f = ch.u32(crate::chr::FLAGS);
                ch.set_u32(crate::chr::FLAGS, (f ^ v) & 1 ^ f);
            }
            Value::Int(0)
        }),
    );
    // 502c(worker, mount): attach the body to another worker's body (−1: let go).
    n.register(
        0x502C,
        Native::result(|g, c| {
            let mount = c.stack.pop_int();
            let parent = c.stack.pop_int();
            attach(g, c.worker, parent, if parent == -1 { 0 } else { mount });
            Value::Int(0)
        }),
    );
}

pub(super) fn register_02(n: &mut Natives<FieldState>) {
    // 00f6(level, worker): wait while the worker has a request at the level or above (level at least 1; worker
    // below 0: no wait).
    n.register(
        0x00F6,
        Native {
            init: Some(|_, c| {
                let w = c.stack.pop_int();
                let level = c.stack.pop_int();
                c.scratch[0] = w;
                c.scratch[1] = level.max(1);
            }),
            wait: Some(|g, c| {
                let w = c.scratch[0];
                if w < 0 {
                    return 5;
                }
                let at = index(g, w);
                let mask = c.requests.get(at).copied().unwrap_or(0);
                if u32::from(mask) >> (c.scratch[1].clamp(0, 15) as u32) != 0 { 0 } else { 5 }
            }),
            result: None,
        },
    );
    // 008b: the status of the running request's command (kinds 5, 6 with a command list), else the side of the
    // last line crossing (second flags bits 1–2; 3 → −1).
    n.register(
        0x008B,
        Native::result(|g, c| {
            let w = wk(g, c);
            // The command list (+0x2e4) is not set up by any ported native yet: no command status.
            let status = -1;
            if status >= 0 {
                return Value::Int(status);
            }
            let side = i32::from(w.flags2() >> 1 & 3);
            Value::Int(if side == 3 { -1 } else { side })
        }),
    );
    // 00a2(window, a, b, c, d): the window's colour word d·2²⁴ | a·2¹⁶ | b·2⁸ | c (bytes); returns it.
    n.register(
        0x00A2,
        Native::result(|g, c| {
            let d = c.stack.pop_int() as u32;
            let cc = c.stack.pop_int() as u32;
            let b = c.stack.pop_int() as u32;
            let a = c.stack.pop_int() as u32;
            let v = (((a & 0xff) | d << 8) << 8 | b & 0xff) << 8 | cc & 0xff;
            let w = c.stack.pop_int();
            if let Some(win) = usize::try_from(w).ok().and_then(|w| g.windows.get_mut(w)) {
                win.color = v;
            }
            Value::Int(v as i32)
        }),
    );
    // 0256(window) / 0257(window): the window's +0x28 word 0x8000 / 0; return the window.
    for (id, v) in [(0x0256u16, 0x8000u32), (0x0257, 0)] {
        n.register(
            id,
            Native::result(if v != 0 {
                |g, c| {
                    let w = c.stack.pop_int();
                    if let Some(win) = usize::try_from(w).ok().and_then(|w| g.windows.get_mut(w)) {
                        win.word28 = 0x8000;
                    }
                    Value::Int(w)
                }
            } else {
                |g, c| {
                    let w = c.stack.pop_int();
                    if let Some(win) = usize::try_from(w).ok().and_then(|w| g.windows.get_mut(w)) {
                        win.word28 = 0;
                    }
                    Value::Int(w)
                }
            }),
        );
    }
    // 009e(window, slot, type, value): a value the window's text shows; returns the window.
    n.register(
        0x009E,
        Native::result(|g, c| {
            let value = c.stack.pop_int();
            let ty = c.stack.pop_int();
            let slot = c.stack.pop_int();
            let win = c.stack.pop_int();
            if let (Ok(w), Ok(s)) = (usize::try_from(win), usize::try_from(slot))
                && let Some(w) = g.windows.get_mut(w)
                && s < 0x20
            {
                if w.values.len() < 0x20 {
                    w.values.resize(0x20, (0xff, -1));
                }
                w.values[s] = (ty as u8, value);
            }
            Value::Int(win)
        }),
    );
    // Placement flags (+0x38): 020b sets 0x100 and asks for the facing from the body (4); 0229 sets 0x200;
    // 0231(worker) sets 0x800 on another worker.
    n.register(
        0x020B,
        Native::result(|g, c| {
            let w = wk(g, c);
            w.set_u16(workers::PLACE, w.u16(workers::PLACE) | 0x100 | 4);
            Value::Int(1)
        }),
    );
    n.register(
        0x0229,
        Native::result(|g, c| {
            let w = wk(g, c);
            w.set_u16(workers::PLACE, w.u16(workers::PLACE) | 0x200);
            Value::Int(1)
        }),
    );
    n.register(
        0x0231,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let at = index(g, i);
            let w = &mut g.workers.list[at];
            w.set_u16(workers::PLACE, w.u16(workers::PLACE) | 0x800);
            Value::Int(1)
        }),
    );
    // 0212(on, v): the system's +0x208 value and bit 0 of its +2 flags; returns the switch.
    n.register(
        0x0212,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let on = c.stack.pop_int() != 0;
            let s = &mut g.workers.sys;
            s.value208 = v;
            s.flags2 = s.flags2 & !1 | u8::from(on);
            Value::Int(i32::from(on))
        }),
    );
    // 020d(v): the save's +0xea value.
    n.register(
        0x020D,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            if let Some(b) = c.memory.global[0].get_mut(0xea..0xec) {
                b.copy_from_slice(&(v as u16).to_le_bytes());
            }
            Value::Int(v)
        }),
    );
    // 0220(bit): a bit (0–7) of the system's switch mask on.
    n.register(
        0x0220,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            if b < 8 {
                g.workers.sys.mask_b90 |= 1u32 << (b as u32 & 31);
            }
            Value::Int(b)
        }),
    );
    n.register(
        0x024C,
        Native::result(|g, c| {
            let w = wk(g, c);
            w.set_u16(workers::PLACE, w.u16(workers::PLACE) | 0x1000);
            Value::Int(1)
        }),
    );
    // 024d(on): bit 6 of the system's +0x210 word; returns the switch.
    n.register(
        0x024D,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let s = &mut g.workers.sys;
            s.flags210 = s.flags210 & !0x40 | u32::from(on) << 6;
            Value::Int(i32::from(on))
        }),
    );
    // 025d(v): [1325ac4] = v; the result is v (left in EAX, 859c20).
    n.register(
        0x025D,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.workers.sys.value_ac4 = v;
            Value::Int(v)
        }),
    );
    n.register(0x025C, Native::result(|_, _| Value::Int(0)));
    for id in [0x0260u16, 0x0261] {
        n.register(
            id,
            Native::result(|_, c| {
                c.stack.pop_int();
                Value::Int(1)
            }),
        );
    }
    for id in [0x0262u16, 0x0263] {
        n.register(id, Native::result(|_, _| Value::Int(1)));
    }
    // 0266: save +0x5ec bit 0; 0255: bits 14–15 of the save's settings word (+0x3d0c).
    n.register(0x0266, Native::result(|_, c| Value::Int(i32::from(img(c, 0x5ec) & 1))));
    n.register(0x0255, Native::result(|_, c| Value::Int(save_i32(c, 0x3d0c) >> 14 & 3)));
    // 0240(chr, on): the equipment a character holds marked (bit 1 of each record's +3).
    n.register(
        0x0240,
        Native::result(|_, c| {
            let on = c.stack.pop_int() != 0;
            let chr = c.stack.pop_int() as u32;
            equip_mark(c, chr, on);
            Value::Int(i32::from(on))
        }),
    );
    // 010e / 0135: save words +0x3d10 and +0x3d48.
    n.register(0x010E, Native::result(|_, c| Value::Int(save_i32(c, 0x3d10))));
    n.register(0x0135, Native::result(|_, c| Value::Int(save_i32(c, 0x3d48))));
    // 0115(on) / 012b(on): bits 3 and 6 of the system's +2 flags; return the switch.
    n.register(
        0x0115,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let s = &mut g.workers.sys;
            s.flags2 = s.flags2 & !8 | u8::from(on) << 3;
            Value::Int(i32::from(on))
        }),
    );
    n.register(
        0x012B,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let s = &mut g.workers.sys;
            s.flags2 = s.flags2 & !0x40 | u8::from(on) << 6;
            Value::Int(i32::from(on))
        }),
    );
    // 012a(slot, on): a shape slot's +3 byte (1 / 0); returns the slot.
    n.register(
        0x012A,
        Native::result(|g, c| {
            let on = c.stack.pop_int();
            let slot = c.stack.pop_int();
            g.shapes.slots[slot.clamp(0, 0x7f) as usize].byte3 = u8::from(on != 0);
            Value::Int(slot)
        }),
    );
    // 0142(mask, n): the bit number of the n-th set bit of the mask (0 when fewer).
    n.register(
        0x0142,
        Native::result(|_, c| {
            let nth = c.stack.pop_int();
            let mask = c.stack.pop_int() as u32;
            let mut seen = 0;
            for b in 0..32 {
                if mask & (1 << b) != 0 {
                    if seen == nth {
                        return Value::Int(b);
                    }
                    seen += 1;
                }
            }
            Value::Int(0)
        }),
    );
    // 0130(i, v): one of the save's eight counters (clamped index; below 0: 0); returns the index. 012e(i, d) adds
    // (below 0: the first); returns the new value.
    n.register(
        0x0130,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int().clamp(0, 7);
            set_save_u32(c, 0x2c + i as usize * 4, v.max(0) as u32);
            Value::Int(i)
        }),
    );
    n.register(
        0x012E,
        Native::result(|_, c| {
            let d = c.stack.pop_int();
            let i = c.stack.pop_int();
            let at = if i < 0 { 0x2c } else { 0x2c + i.min(7) as usize * 4 };
            let v = save_i32(c, at).wrapping_add(d);
            set_save_u32(c, at, v as u32);
            Value::Int(v)
        }),
    );
    // 01a8(chr): a party record's +0x54 word; 01ab: the save's +0x3d14 word.
    n.register(
        0x01A8,
        Native::result(|_, c| {
            let n = c.stack.pop_int();
            Value::Int(record(n).map_or(0, |r| save_i32(c, r + 0x54)))
        }),
    );
    n.register(0x01AB, Native::result(|_, c| Value::Int(save_i32(c, 0x3d14))));
    // 018a(bit) / 018b(bit): a bit of a switch mask the event system keeps on / off; return the bit number.
    n.register(
        0x018A,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            g.workers.sys.mask_b74 |= 1u32 << (b as u32 & 31);
            Value::Int(b)
        }),
    );
    n.register(
        0x018B,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            g.workers.sys.mask_b74 &= !(1u32 << (b as u32 & 31));
            Value::Int(b)
        }),
    );
    // 0194(on): save +0xd1.
    n.register(
        0x0194,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            set_img(c, 0xd1, u8::from(v != 0));
            Value::Int(v)
        }),
    );
    // Key item counts (save +0x420c, by the item's low 12 bits): 01ac reads one, 01ad adds 1, 01af(item, d) adds
    // d (kept within 0–10; item 0xff none); each returns the count.
    fn item_add(c: &mut NativeCall, item: i32, d: i32) -> i32 {
        let i = (item as u32 & 0xfff) as usize;
        if i == 0xff {
            return 0;
        }
        let v = (i32::from(img(c, 0x420c + i)) + d).clamp(0, 10);
        set_img(c, 0x420c + i, v as u8);
        v
    }
    n.register(
        0x01AC,
        Native::result(|_, c| {
            let i = c.stack.pop_int();
            Value::Int(i32::from(img(c, 0x420c + (i as u32 & 0xfff) as usize)))
        }),
    );
    n.register(
        0x01AD,
        Native::result(|_, c| {
            let i = c.stack.pop_int();
            Value::Int(item_add(c, i, 1))
        }),
    );
    n.register(
        0x01AF,
        Native::result(|_, c| {
            let d = c.stack.pop_int();
            let i = c.stack.pop_int();
            Value::Int(item_add(c, i, d))
        }),
    );
    // 0160(n) / 0161(n): a bit of the save's 0x80-bit field at +0x448c tested / cleared.
    n.register(
        0x0160,
        Native::result(|_, c| {
            let n = c.stack.pop_int() as u32 & 0xfff;
            if n >= 0x80 {
                return Value::Int(0);
            }
            let at = 0x448c + (n >> 4) as usize * 2;
            let w = u16::from_le_bytes([img(c, at), img(c, at + 1)]);
            Value::Int(i32::from(w & (1 << (n & 0xf)) != 0))
        }),
    );
    n.register(
        0x0161,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            let n = v as u32 & 0xfff;
            if n < 0x80 {
                let at = 0x448c + (n >> 4) as usize * 2;
                let w = u16::from_le_bytes([img(c, at), img(c, at + 1)]) & !(1 << (n & 0xf));
                set_img(c, at, w as u8);
                set_img(c, at + 1, (w >> 8) as u8);
            }
            Value::Int(v)
        }),
    );
    // 014a(a, b, c): three values the system keeps; returns a.
    n.register(
        0x014A,
        Native::result(|g, c| {
            let v3 = c.stack.pop_int();
            let v2 = c.stack.pop_int();
            let v1 = c.stack.pop_int();
            g.workers.sys.values_b48 = [v1, v2, v3];
            Value::Int(v1)
        }),
    );
    // 019e: talking is open (the system's mode flags 0x22).
    n.register(0x019E, Native::result(|g, _| Value::Int(i32::from(g.workers.sys.flags0 & 0x22 != 0))));
    // 0210(n): a help event flag (the save's 0x80-byte field at +0x3dcc); returns n.
    n.register(
        0x0210,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            let n = v.max(0);
            let at = 0x3dcc + (n >> 3).min(0x7f) as usize;
            set_img(c, at, img(c, at) | 1 << (n & 7));
            Value::Int(v)
        }),
    );
    // 0132(worker, on) and 0133 (the same): 0056 on another worker (lines, boxes, zones stepped or not); 00ed:
    // the worker's +0x40.
    for id in [0x0132u16, 0x0133] {
        n.register(
            id,
            Native::result(|g, c| {
                let on = c.stack.pop_int();
                let i = c.stack.pop_int();
                let at = index(g, i);
                let w = &mut g.workers.list[at];
                if !matches!(w.kind, 2 | 3 | 5 | 6) {
                    return Value::Int(0);
                }
                w.set_u16(workers::FLAGS, w.flags() & !0x80 | u16::from(on != 0) << 7);
                w.set_u16(workers::FLAGS2, w.flags2() | 1);
                Value::Int(1)
            }),
        );
    }
    n.register(0x00ED, Native::result(|g, c| Value::Int(wk(g, c).i32(0x40))));
    // 00eb: the running level; 00ba: the worker's own signal entry table cleared (returns 1); 00ee(worker): the
    // worker's attached flag (engine flags bit 5).
    n.register(0x00EB, Native::result(|_, c| Value::Int(i32::from(c.level))));
    n.register(
        0x00BA,
        Native::result(|g, c| {
            wk(g, c).set_u32(workers::SIGNAL_TABLE, 0);
            Value::Int(1)
        }),
    );
    n.register(
        0x00EE,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let w = index(g, i);
            Value::Int(i32::from(g.workers.list[w].flags() >> 5 & 1))
        }),
    );
    // 01b6: some equipment record is held (+2 set), marked (+3 bit 3) and not equipped (+3 bit 1).
    n.register(
        0x01B6,
        Native::result(|_, c| {
            let any = (0..200).any(|i| {
                let r = EQUIPMENT + i * 0x16;
                img(c, r + 2) != 0 && img(c, r + 3) & 8 != 0 && img(c, r + 3) & 2 == 0
            });
            Value::Int(i32::from(any))
        }),
    );
    // 01c6: a one-time flag (1 the first time, then 0); 01fa: a value a menu leaves (−1 until then).
    n.register(0x01C6, Native::result(|g, _| Value::Int(i32::from(std::mem::take(&mut g.globals.first_c6)))));
    n.register(0x01FA, Native::result(|g, _| Value::Int(g.globals.menu_value_fa)));
    // Save image values: 00fa(v) +0x2b, 01bc(v) +0xd4 (u16) and +0xd6 = 1, 01c3(v) +0xe4, 01c4(on) +0xe8 (u16).
    n.register(
        0x00FA,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            set_img(c, 0x2b, v as u8);
            Value::Int(v)
        }),
    );
    n.register(
        0x01BC,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            set_img(c, 0xd4, v as u8);
            set_img(c, 0xd5, (v >> 8) as u8);
            set_img(c, 0xd6, 1);
            Value::Int(1)
        }),
    );
    n.register(
        0x01C3,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            set_save_u32(c, 0xe4, v as u32);
            Value::Int(v)
        }),
    );
    n.register(
        0x01C4,
        Native::result(|_, c| {
            let on = c.stack.pop_int() != 0;
            set_img(c, 0xe8, u8::from(on));
            set_img(c, 0xe9, 0);
            Value::Int(i32::from(on))
        }),
    );
    // 01b2(item): how many of an item the party holds (the item list at save +0x3ecc, counts at +0x40cc).
    n.register(
        0x01B2,
        Native::result(|_, c| {
            let item = c.stack.pop_int() as u32;
            for i in 0..0x70 {
                let id = u32::from(u16::from_le_bytes([img(c, 0x3ecc + i * 2), img(c, 0x3ecd + i * 2)]));
                let n = img(c, 0x40cc + i);
                if id == item && n != 0 {
                    return Value::Int(i32::from(n));
                }
            }
            Value::Int(0)
        }),
    );
    // 01b8: the first marked equipment record (+2 set, +3 bit 3) loses its equipped mark (bit 1).
    n.register(
        0x01B8,
        Native::result(|_, c| {
            for i in 0..200 {
                let r = EQUIPMENT + i * 0x16;
                if img(c, r + 2) != 0 && img(c, r + 3) & 8 != 0 {
                    set_img(c, r + 3, img(c, r + 3) & 0xfd);
                    return Value::Int(1);
                }
            }
            Value::Int(0)
        }),
    );
    // 0195: wait while the event effect flag is up (00db raises it); 01bf: a system byte on (returns 1); 01c2:
    // save +0xe0 values 2–5 become 1 (returns 1).
    n.register(
        0x0195,
        Native {
            init: None,
            wait: Some(|g, _| if g.workers.sys.effect_b58 != 0 { 0 } else { 5 }),
            result: None,
        },
    );
    n.register(
        0x01BF,
        Native::result(|g, _| {
            g.workers.sys.byte_b81 = 1;
            Value::Int(1)
        }),
    );
    n.register(
        0x01C2,
        Native::result(|_, c| {
            let v = save_i32(c, 0xe0);
            if (v.wrapping_sub(2) as u32) <= 3 {
                set_save_u32(c, 0xe0, 1);
            }
            Value::Int(1)
        }),
    );
    // 013f(table, n): the address of dictionary text n·2 (+1 with settings bit 3) of the table.
    n.register(
        0x013F,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            let table = c.stack.pop_int();
            let alt = (save_i32(c, 0x3d0c) as u32 >> 3 & 1) as i32;
            Value::Int(g.texts.macro_text(table, n.wrapping_mul(2) + alt).unwrap_or(0) as i32)
        }),
    );
    // 0236(n): msg_text(n): the address of event message n (kept as the last asked).
    n.register(
        0x0236,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            g.last_text = n;
            Value::Int(g.texts.event_text(n) as i32)
        }),
    );
    // 0002(slot, record, _): the worker becomes a camera (019d(0) first; bind kind 2, bind id = slot; slot < 4:
    // A camera handle when it has none: handle(5, slot (1 for slot 0 on the header's camera
    // worker), record)); second flags bit 4 = slot is 1; kinds 2, 3, 5, 6: flags bits 13–14 = 2. Returns 0.
    n.register(
        0x0002,
        Native::result(|g, c| {
            c.stack.pop_int();
            let record = c.stack.pop_int();
            let slot = c.stack.pop_int();
            let w = c.worker;
            unbind(g, w, false);
            let a = g.actor(w);
            a.bind_kind = 2;
            a.bind_id = slot as u16;
            g.sync_bind(w);
            if (slot as u32) < 4 && g.cam_slots.get(w).copied() == Some(0) {
                let s = if slot == 0 && w as u16 == g.camera_worker { 1 } else { slot as u32 };
                let h = g.cam.get_handle(5, s, record as u32);
                g.cam_slots[w] = h;
            }
            let wk = &mut g.workers.list[w];
            let f2 = wk.u16(workers::FLAGS2);
            wk.set_u16(workers::FLAGS2, if slot == 1 { f2 | 0x10 } else { f2 & 0xffef });
            if matches!(wk.kind, 2 | 3 | 5 | 6) {
                let f = wk.u16(workers::FLAGS);
                wk.set_u16(workers::FLAGS, f ^ ((2 << 13) ^ f) & 0x6000);
            }
            Value::Int(0)
        }),
    );
    // 0138(v): the gil (save +0x3d48) = v clamped to 0–999999999; returns it.
    n.register(
        0x0138,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            let gil = if v < 0 { 0 } else { v.min(999_999_999) };
            set_save_u32(c, 0x3d48, gil as u32);
            Value::Int(gil)
        }),
    );
    // 00f8(var, value, size): clears script memory only in a debug build (not this one); returns size. 00f9:
    // 1 (the same test, else save +0x2b).
    n.register(
        0x00F8,
        Native::result(|_, c| {
            let size = c.stack.pop_int();
            c.stack.pop_int();
            c.stack.pop_int();
            Value::Int(size)
        }),
    );
    n.register(0x00F9, Native::result(|_, _| Value::Int(1)));
    // 00b2(on): a switch = on & 1, returns the old value; 00b3(v) / 00b4(v): two values = v, return v.
    n.register(
        0x00B2,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let old = g.byte_181e as i8;
            g.byte_181e = u8::from(on);
            Value::Int(i32::from(old))
        }),
    );
    n.register(
        0x00B3,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.values_194c[0] = v;
            Value::Int(v)
        }),
    );
    n.register(
        0x00B4,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.values_194c[1] = v;
            Value::Int(v)
        }),
    );
    // 00ef(on): second flags bit 11; returns on.
    n.register(
        0x00EF,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let w = &mut g.workers.list[c.worker];
            let f = w.u16(workers::FLAGS2);
            w.set_u16(workers::FLAGS2, f & !0x800 | u16::from(on) << 11);
            Value::Int(i32::from(on))
        }),
    );
    // 0149(on): the system block's +2 bit 7; returns on.
    n.register(
        0x0149,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            g.workers.sys.flags2 = g.workers.sys.flags2 & 0x7f | u8::from(on) << 7;
            Value::Int(i32::from(on))
        }),
    );
    // 01ca: the own worker's placement flag 4; returns 1.
    n.register(
        0x01CA,
        Native::result(|g, c| {
            let w = &mut g.workers.list[c.worker];
            w.set_u16(workers::PLACE, w.u16(workers::PLACE) | 4);
            Value::Int(1)
        }),
    );
    // 01e0(v): a character's body +0x408 = v; returns 1 (0 without a body).
    n.register(
        0x01E0,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if g.workers.list[c.worker].bind() != 1 {
                return Value::Int(0);
            }
            match chr_of(g, c.worker) {
                Some(ch) => {
                    ch.set_i32(0x408, v);
                    Value::Int(1)
                }
                None => Value::Int(0),
            }
        }),
    );
    // 01c0(slot, v): value[clamp(slot, 0, 3)] = v; returns v.
    n.register(
        0x01C0,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let slot = c.stack.pop_int().clamp(0, 3) as usize;
            g.workers.sys.values_ce0[slot] = v;
            Value::Int(v)
        }),
    );
    // 014f(limit, a, b): |angle_wrap(a − b)| below the limit (the smaller of the two).
    n.register(
        0x014F,
        Native::result(|_, c| {
            let b = c.stack.pop_float();
            let a = c.stack.pop_float();
            let limit = c.stack.pop_float();
            let w = workers::math::wrap(a - b);
            let m = if w.abs() < limit { w.abs() } else { limit };
            Value::Int(i32::from(m < limit))
        }),
    );
    // 0145(x, y) / 0146(size): the guide map's place and size (when there is one); return 1 / size.
    n.register(
        0x0145,
        Native::result(|g, c| {
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            if let Some(m) = g.guide_map.as_mut() {
                m.pos = (x, y);
            }
            Value::Int(1)
        }),
    );
    n.register(
        0x0146,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(m) = g.guide_map.as_mut() {
                m.size = v;
            }
            Value::Int(v)
        }),
    );
    // 010f(win, b22, b23, v): window bytes +0x22/+0x23 and word +0x24; returns the window.
    n.register(
        0x010F,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let b23 = c.stack.pop_int();
            let b22 = c.stack.pop_int();
            let w = c.stack.pop_int();
            let i = w.clamp(0, 7) as usize;
            if let Some(win) = g.windows.get_mut(i) {
                win.byte22 = b22 as u8;
                win.byte23 = b23 as u8;
                win.word24 = v as u32;
            }
            Value::Int(w)
        }),
    );
    // 0189(page): the walk mesh page switch without the guide map (kind 2, the page); done at once.
    n.register(
        0x0189,
        Native {
            init: Some(|g, c| {
                let page = c.stack.pop_int();
                g.page_switch.0 = 2;
                g.page_switch.2 = page as i8;
            }),
            wait: Some(|_, _| 1),
            result: None,
        },
    );
    // 0159(worker, slot): 0158 on another worker.
    n.register(
        0x0159,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            if g.workers.sys.word_1a != 0 {
                return Value::Int(v);
            }
            let w = index(g, i);
            if g.workers.list.get(w).is_some_and(|wk| wk.bind() == 1)
                && let Some(ch) = chr_of(g, w)
            {
                ch.set_u8(0x184, v as u8);
            }
            if let Some(wk) = g.workers.list.get_mut(w) {
                wk.set_u8(0xaf, v as u8);
                let id = i32::from(wk.id());
                if (0..16).contains(&v) {
                    g.workers.sys.body_slots[v as usize] = id;
                }
            }
            Value::Int(v)
        }),
    );
    // 0179(n): wait for event bit n (017a sets, 017b clears).
    n.register(
        0x0179,
        Native {
            init: Some(|_, c| c.scratch[0] = c.stack.pop_int()),
            wait: Some(|g, c| if g.engine.event_bits & 1 << (c.scratch[0] as u32 & 31) != 0 { 5 } else { 0 }),
            result: None,
        },
    );
    // 0119(v) / 01ba(worker, v): the own / another worker's character's (+0x40; 01ba only under 8) weapon (v 1:
    // party record +0x2e) or armour (+0x2d) equipment record's +0xc (0xff none → 0).
    fn equip_value(c: &NativeCall, chr: u32, weapon: bool) -> i32 {
        let rec = PARTY_RECORDS + chr as usize * PARTY_RECORD + if weapon { 0x2e } else { 0x2d };
        let Some(&e) = c.memory.global[0].get(rec) else { return 0 };
        if e == 0xff {
            return 0;
        }
        let i = usize::from(e) & 0xfff;
        let i = if i > 199 { 0 } else { i };
        save_u16(c, 0x449c + i * 0x16 + 0xc)
    }
    n.register(
        0x0119,
        Native::result(|g, c| {
            let v = c.stack.pop_int().clamp(0, 1);
            let chr = g.workers.list[c.worker].u32(0x40);
            Value::Int(equip_value(c, chr, v == 1))
        }),
    );
    n.register(
        0x01BA,
        Native::result(|g, c| {
            let v = c.stack.pop_int().clamp(0, 1);
            let i = c.stack.pop_int();
            let w = index(g, i);
            let chr = g.workers.list.get(w).map_or(u32::MAX, |wk| wk.u32(0x40));
            Value::Int(if chr < 8 { equip_value(c, chr, v == 1) } else { 0 })
        }),
    );
    // 021e: the distortion parameters' defaults; returns 1.
    n.register(
        0x021E,
        Native::result(|g, _| {
            g.distortion.reset();
            Value::Int(1)
        }),
    );
    // 022e(a, b, c, d): four values; returns 1.
    n.register(
        0x022E,
        Native::result(|g, c| {
            let d = c.stack.pop_int();
            let cc = c.stack.pop_int();
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            g.workers.sys.values_cf0 = [a, b, cc, d];
            Value::Int(1)
        }),
    );
    // 01b0(no, mode): the movie to preload (save +0xe0 2–5: another movie → it stops, the same with
    // another mode → state 3 and the mode; else the remaster's preload, state 1, +0xd8 = no, +0xdc = mode); returns no.
    // 01b1: state 6, +0xd8 = −1; returns 1.
    n.register(
        0x01B0,
        Native::result(|g, c| {
            let mode = c.stack.pop_int();
            let no = c.stack.pop_int();
            let state = save_i32(c, 0xe0);
            if (state.wrapping_sub(2) as u32) < 4 && no == save_i32(c, 0xd8) {
                if mode != save_i32(c, 0xdc) {
                    set_save_u32(c, 0xe0, 3);
                    set_save_u32(c, 0xdc, mode as u32);
                }
            } else {
                g.movie_preload = Some((no, mode & 0x80));
                set_save_u32(c, 0xe0, 1);
                set_save_u32(c, 0xd8, no as u32);
                set_save_u32(c, 0xdc, mode as u32);
            }
            Value::Int(no)
        }),
    );
    n.register(
        0x01B1,
        Native::result(|_, c| {
            set_save_u32(c, 0xe0, 6);
            set_save_u32(c, 0xd8, u32::MAX);
            Value::Int(1)
        }),
    );
    // 01d2(n): event message n's second text (offset at n·8 + 4) is the name at save +0xec.
    n.register(
        0x01D2,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            let text: &[u8] = g
                .texts
                .event
                .as_deref()
                .and_then(|d| {
                    let at = usize::try_from(n).ok()? * 8 + 4;
                    let off = usize::from(u16::from_le_bytes([*d.get(at)?, *d.get(at + 1)?]));
                    (off != 0).then(|| d.get(off..)).flatten()
                })
                .unwrap_or(&[0]);
            let save = c.memory.global[0].get(0xec..).unwrap_or(&[0]);
            let end = |b: &[u8]| b.iter().position(|&x| x == 0).unwrap_or(b.len());
            Value::Int(i32::from(text[..end(text)] == save[..end(save)]))
        }),
    );
    // 0259(i, v): value[i ≠ 0] = v; returns v.
    n.register(
        0x0259,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let i = c.stack.pop_int();
            g.values_5b98[usize::from(i != 0)] = v;
            Value::Float(v)
        }),
    );
    // 4043(a, b): two values; 4039(a, b, c, d): four bytes (returns 0.0). Return 0.
    n.register(
        0x4043,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            g.values_08c4 = [a, b];
            Value::Int(0)
        }),
    );
    n.register(
        0x4039,
        Native::result(|g, c| {
            let d = c.stack.pop_int();
            let cc = c.stack.pop_int();
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            g.bytes_0771 = [a as u8, b as u8, cc as u8, d as u8];
            Value::Float(0.0)
        }),
    );
    // 4023(n): the light table freed and n records made (+0x28 = 0, +0xc = +0x1c = 1.0); 4024: freed; 402a(i, v):
    // record i's +0x28 = v while i ≤ n (the original writes one past the table for i = n). All return 0.
    n.register(
        0x4023,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            let mut rec = [0u8; 0x2c];
            rec[0xc..0x10].copy_from_slice(&1.0f32.to_le_bytes());
            rec[0x1c..0x20].copy_from_slice(&1.0f32.to_le_bytes());
            g.lights_08cc = vec![rec; usize::try_from(n).unwrap_or(0)];
            Value::Int(0)
        }),
    );
    n.register(
        0x4024,
        Native::result(|g, _| {
            g.lights_08cc.clear();
            Value::Int(0)
        }),
    );
    n.register(
        0x402A,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            if i <= g.lights_08cc.len() as i32
                && let Some(r) = usize::try_from(i).ok().and_then(|i| g.lights_08cc.get_mut(i))
            {
                r[0x28] = v as u8;
            }
            Value::Int(0)
        }),
    );
    // 4025(i, x, y, z, range, r, g, b, k): light record i (while i ≤ the count): +0..+8 the place,
    // +0x10..+0x18 the colour / 255, +0x20 range, +0x29 k, +0x24 2.0, +0x28 1; returns 0.
    n.register(
        0x4025,
        Native::result(|g, c| {
            let k = c.stack.pop_int();
            let b = c.stack.pop_int();
            let gr = c.stack.pop_int();
            let r = c.stack.pop_int();
            let range = c.stack.pop_float();
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            let i = c.stack.pop_int();
            if i <= g.lights_08cc.len() as i32
                && let Some(rec) = usize::try_from(i).ok().and_then(|i| g.lights_08cc.get_mut(i))
            {
                let put = |rec: &mut [u8; 0x2c], at: usize, v: f32| {
                    rec[at..at + 4].copy_from_slice(&v.to_le_bytes())
                };
                put(rec, 0, x);
                put(rec, 4, y);
                put(rec, 8, z);
                for (at, v) in [(0x10, r), (0x14, gr), (0x18, b)] {
                    put(rec, at, (f64::from(v as f32) / 255.0) as f32);
                }
                put(rec, 0x20, range);
                rec[0x29] = k as u8;
                put(rec, 0x24, 2.0);
                rec[0x28] = 1;
            }
            Value::Int(0)
        }),
    );
    // 0254(v): settings bits 14–15 = v (then the ability data is set up again, eiAbmDataInit, battle side); returns
    // v.
    n.register(
        0x0254,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            let s = save_i32(c, 0x3d0c) as u32;
            set_save_u32(c, 0x3d0c, s ^ ((v as u32) << 14 ^ s) & 0xc000);
            Value::Int(v)
        }),
    );
    // Debug-build natives (off in the release game): c00b, c00c, c018 (its argument a variable), c022
    // return 1; c028 returns 1; c02a(v) returns v.
    for id in [0xC00Bu16, 0xC00C, 0xC018, 0xC022] {
        n.register(
            id,
            Native::result(|_, c| {
                c.stack.pop_int();
                Value::Int(1)
            }),
        );
    }
    n.register(0xC028, Native::result(|_, _| Value::Int(1)));
    n.register(0xC02A, Native::result(|_, c| Value::Int(c.stack.pop_int())));
    // c007(a, b): two values; returns a. 70ea(v): save +0x3da8 = v; returns v.
    n.register(
        0xC007,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            g.values_085c = [a, b];
            Value::Int(a)
        }),
    );
    n.register(
        0x70EA,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            set_save_u32(c, 0x3da8, v as u32);
            Value::Int(v)
        }),
    );
    // c03b: every item, 99 of each; returns 1.
    n.register(
        0xC03B,
        Native::result(|g, c| {
            full_items(g, c);
            Value::Int(1)
        }),
    );
    // 707d(i, v): debug switch i = v & 1 (0: every item; 10: every character's +0x3b = 0x62 and the most gil; 0x19
    // keeps the whole byte); returns v. 707e(i): the switch (2: both 1 and 0xc on).
    n.register(
        0x707D,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            let b = (v & 1) as u8;
            let at = |a: usize| a - 0x112a8f8;
            g.debug_flags.resize(0x2c, 0);
            match i {
                0 => full_items(g, c),
                1 => g.debug_flags[at(0x112a8f9)] = b,
                2 => {
                    g.debug_flags[at(0x112a8f8)] = b;
                    g.debug_flags[at(0x112a8f9)] = b;
                }
                3 => g.debug_flags[at(0x112a8fa)] = b,
                4 => g.debug_flags[at(0x112a901)] = b,
                5 => g.debug_flags[at(0x112a90c)] = b,
                6..=9 => g.debug_flags[at(0x112a908) + (i as usize - 6)] = b,
                10 => {
                    for k in 0..18 {
                        if let Some(x) = c.memory.global[0].get_mut(PARTY_RECORDS + k * PARTY_RECORD + 0x3b) {
                            *x = 0x62;
                        }
                    }
                    set_save_u32(c, 0x3d48, 999_999_999);
                }
                0xc => g.debug_flags[at(0x112a8f8)] = b,
                0xd..=0x14 => g.debug_flags[at(0x112a90d) + (i as usize - 0xd)] = b,
                0x17 => g.debug_flags[at(0x112a922)] = b,
                0x18 => g.debug_flags[at(0x112a915)] = b,
                0x19 => g.debug_flags[at(0x112a923)] = v as u8,
                _ => {}
            }
            Value::Int(v)
        }),
    );
    n.register(
        0x707E,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            g.debug_flags.resize(0x2c, 0);
            let f = |a: usize| i32::from(g.debug_flags[a - 0x112a8f8] as i8);
            Value::Int(match i {
                1 => f(0x112a8f9),
                2 => i32::from(f(0x112a8f9) != 0 && f(0x112a8f8) != 0),
                3 => f(0x112a8fa),
                4 => f(0x112a901),
                5 => f(0x112a90c),
                6..=9 => f(0x112a908 + (i as usize - 6)),
                0xc => f(0x112a8f8),
                0xd..=0x14 => f(0x112a90d + (i as usize - 0xd)),
                0x17 => f(0x112a922),
                0x18 => f(0x112a915),
                0x19 => i32::from(g.debug_flags[0x112a923 - 0x112a8f8]),
                _ => 0,
            })
        }),
    );
    // 806b(v): the refresh state = 2 when v ≠ 0, then the map refresh; returns 1.
    n.register(
        0x806B,
        Native::result(|g, c| {
            if c.stack.pop_int() != 0 {
                g.map_switch_5eb8 = 2;
            }
            Value::Int(1)
        }),
    );
    // 021a(area, n) / 021b(species, n): how many of the 0x8b capture records with that area (+2) /
    // species (+4) have a capture count (save +0x420c + record) of at least n.
    fn captured(c: &NativeCall, field: usize, id: i32, n: i32) -> i32 {
        crate::traced_tables::CAPTURE
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                i32::from(r[field]) == id
                    && i32::from(c.memory.global[0].get(0x420c + (i & 0xfff)).copied().unwrap_or(0)) >= n
            })
            .count() as i32
    }
    n.register(
        0x021A,
        Native::result(|_, c| {
            let n = c.stack.pop_int();
            let id = c.stack.pop_int();
            Value::Int(captured(c, 1, id, n))
        }),
    );
    n.register(
        0x021B,
        Native::result(|_, c| {
            let n = c.stack.pop_int();
            let id = c.stack.pop_int();
            Value::Int(captured(c, 2, id, n))
        }),
    );
    // 0221(i): word i of the engine's word table.
    n.register(
        0x0221,
        Native::result(|_, c| {
            let i = c.stack.pop_int();
            let w = usize::try_from(i).ok().and_then(|i| crate::traced_tables::WORDS_0221.get(i));
            Value::Int(w.copied().unwrap_or(0) as i32)
        }),
    );
    // 00bd(n): the own requests above level n − 1 (n at least 1) that have not started are dropped (the worker's
    // requests above level n − 1); returns n.
    n.register(
        0x00BD,
        Native::result(|_, c| {
            let n = c.stack.pop_int().max(1);
            c.effects.push(aerugo_script::vm::Effect::SkipQueued(c.worker, (n - 1).clamp(0, 255) as u8));
            Value::Int(n)
        }),
    );
    // 5032: the body's idle motion for its mode (for chr, mode +0x185, 0); returns 0. 504a(worker): the
    // body plays a motion a script set (+0x736).
    n.register(
        0x5032,
        Native::result(|g, c| {
            // 837180: 837d00(body, (i8) +0x185, 0).
            if let Some(mode) = g.chrs.get(c.worker).and_then(Option::as_ref).map(|ch| ch.u8(0x185)) {
                g.mode_motion(c.worker, mode, 0);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x504A,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let w = index(g, i);
            let on = g.chrs.get(w).is_some_and(Option::is_some)
                && g.actors.get(w).and_then(|a| a.player.as_ref()).is_some_and(|p| p.playing);
            Value::Int(i32::from(on))
        }),
    );
    // 504b(worker): chr_motion_stop (+0x730 = 1, +0x736 = 0, +0x728 = 0); returns 0. 505e: the body's speed
    // (+0x154; Aerugo keeps it as the actor's engine speed), 0.0 without a body.
    n.register(
        0x504B,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let w = index(g, i);
            if let Some(ch) = chr_of(g, w) {
                ch.set_u32(0x730, 1);
                ch.set_u8(0x736, 0);
                ch.set_u16(0x728, 0);
                if let Some(p) = g.actors.get_mut(w).and_then(|a| a.player.as_mut()) {
                    p.playing = false;
                }
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x505E,
        Native::result(|g, c| {
            let speed = if g.chrs.get(c.worker).is_some_and(Option::is_some) {
                g.actors.get(c.worker).map_or(0.0, |a| a.engine_speed)
            } else {
                0.0
            };
            Value::Float(speed)
        }),
    );
    // 5065(n): the event's motion file n starts reading; returns 0. 5066(n): waits
    // for it (Aerugo's reads are done at once); the result is its +0xc, which the init clears.
    n.register(
        0x5065,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            g.read_motion_file((1, n as u16, 0));
            Value::Int(0)
        }),
    );
    n.register(
        0x5066,
        Native {
            init: Some(|_, c| {
                c.scratch[3] = 0;
                c.scratch[0] = c.stack.pop_int();
            }),
            wait: Some(|_, _| 1),
            result: Some(|_, c| Value::Int(c.scratch[3])),
        },
    );
    // 4040(degrees): the player heads `degrees` off the camera from now; 403e(on, degrees): a fixed
    // heading (on, then fix(degrees·π/180)); 403f: the camera heading now, no offset
    //. All return 0.
    n.register(
        0x4040,
        Native::result(|g, c| {
            let d = c.stack.pop_float();
            let cam = g.cam.heading();
            g.sg.turn_to(cam, d);
            Value::Int(0)
        }),
    );
    n.register(
        0x403E,
        Native::result(|g, c| {
            let d = c.stack.pop_float();
            let on = c.stack.pop_int();
            g.sg.fixed_on = on as u8 != 0;
            g.sg.fixed =
                super::SgControl::dir_fix(((f64::from(d) * f64::from(crate::chr::BODY_PI)) / 180.0) as f32);
            Value::Int(0)
        }),
    );
    n.register(
        0x403F,
        Native::result(|g, _| {
            g.sg.camera = g.cam.heading();
            g.sg.offset = 0.0;
            Value::Int(0)
        }),
    );
    // 400b: the black/white fade's frames left cleared; returns 0.
    n.register(
        0x400B,
        Native::result(|g, _| {
            g.screen.channels[4].count = 0;
            Value::Int(0)
        }),
    );
    // 01fc(chr, command): the character learns the command ((chr, command, 1): 0x3000–0x305f
    // party record +0x3e bits, 0x3060.. save +0x3d6c bits); returns 1 (0 for other ids).
    n.register(
        0x01FC,
        Native::result(|_, c| {
            let cmd = c.stack.pop_int() as u32;
            let chr = c.stack.pop_int();
            if cmd & 0xffff_f000 != 0x3000 {
                return Value::Int(0);
            }
            let (at, bit) = if cmd < 0x3060 {
                let i = (cmd & 0xfff) >> 4;
                (
                    PARTY_RECORDS as i64 + i64::from(chr) * PARTY_RECORD as i64 + 0x3e + 2 * i64::from(i),
                    cmd & 0xf,
                )
            } else {
                let k = cmd - 0x60;
                (0x3d6c + 2 * i64::from((k & 0xfff) >> 4), k & 0xf)
            };
            if let Some(b) = usize::try_from(at).ok().and_then(|a| c.memory.global[0].get_mut(a..a + 2)) {
                let v = u16::from_le_bytes([b[0], b[1]]) | (1 << bit);
                b.copy_from_slice(&v.to_le_bytes());
            }
            Value::Int(1)
        }),
    );
    // 101c(slot, variable): the system block's slot (clamped 0–7) takes the variable's address (0: none) and +4 = 0;
    // returns 0.
    n.register(
        0x101C,
        Native::result(|g, c| {
            let var = c.stack.pop_int();
            let slot = c.stack.pop_int().clamp(0, 7) as usize;
            let addr = if var == 0 {
                0
            } else {
                aerugo_script::memory::encode_address(super::shapes::var_place(g, c, var))
            };
            g.sys_slots[slot] = (addr, 0);
            Value::Int(0)
        }),
    );
    // 0234: pending key items not yet held are given (0xa004 + bit, the key item, the
    // save +0x3d10 bit), then the pending state is cleared; returns 1.
    n.register(
        0x0234,
        Native::result(|g, c| {
            let (on, bits) = g.key_pending;
            if on != 0 {
                let held = save_i32(c, 0x3d10) as u32;
                for i in 0..0x1a {
                    if held & (1 << i) == 0 && bits & (1 << i) != 0 {
                        let k = 4 + i as usize;
                        if let Some(b) =
                            c.memory.global[0].get_mut(0x448c + (k >> 4) * 2..0x448c + (k >> 4) * 2 + 2)
                        {
                            let v = u16::from_le_bytes([b[0], b[1]]) | (1 << (k & 0xf));
                            b.copy_from_slice(&v.to_le_bytes());
                        }
                        let h = save_i32(c, 0x3d10) as u32 | 1 << i;
                        set_save_u32(c, 0x3d10, h);
                    }
                }
            }
            g.key_pending = (0, 0);
            Value::Int(1)
        }),
    );
    // 01a4(object, v): the display object's morph weight (when the object exists; the native pads the vector with
    // three zeros of its own); returns 0.
    n.register(
        0x01A4,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let i = c.stack.pop_int();
            let count = g.map_objects.as_ref().map_or(0, |m| m.objects.len());
            if let Some(i) = usize::try_from(i).ok().filter(|&i| i < count) {
                if g.mime_params.len() <= i {
                    g.mime_params.resize(count, None);
                }
                g.mime_params[i] = Some(v);
            }
            Value::Int(0)
        }),
    );
    // 00fe(sound, pan): the sound's pan (cmd 0x3d (sound & 0x7fffffff, pan & 0x7f)); returns pan.
    n.register(
        0x00FE,
        Native::result(|g, c| {
            let pan = c.stack.pop_int();
            let sound = c.stack.pop_int();
            g.sound
                .cmds
                .push(super::sound::SoundCmd { id: 0x3d, args: [sound & 0x7fff_ffff, pan & 0x7f, 0, 0] });
            Value::Int(pan)
        }),
    );
    // 0264(v): save +0x5ec = (v ≠ 0) and the remaster's music type follows it (iMuiscSetBgmType_Atel); returns 0.
    n.register(
        0x0264,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            if let Some(b) = c.memory.global[0].get_mut(0x5ec) {
                *b = u8::from(v != 0);
            }
            Value::Int(0)
        }),
    );
    // 025e(v): the remaster's shadow solution (returns 1). 025f(a, b): its loading screen state (
    // returns 0).
    n.register(
        0x025E,
        Native::result(|g, c| {
            g.shadow_solution = Some(c.stack.pop_int());
            Value::Int(1)
        }),
    );
    n.register(
        0x025F,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            g.loading_screen = Some((a, b));
            Value::Int(0)
        }),
    );
    // 01a6: the active party's empty slots are filled with the lowest characters 0–7 that have joined (record
    // +0x2c bit 0) and are not in a slot, then the three slots set again (each).
    n.register(
        0x01A6,
        Native::result(|_, c| {
            let mut slots = [img(c, PARTY), img(c, PARTY + 1), img(c, PARTY + 2)];
            let mut avail = 0u32;
            for i in 0..8 {
                if rec_flags(c, i) & 1 != 0 {
                    avail |= 1 << i;
                }
            }
            for &s in &slots {
                if s != 0xff {
                    avail &= !(1 << (s & 0x1f));
                }
            }
            let mut next = 0u32;
            for s in &mut slots {
                if *s == 0xff {
                    if avail == 0 {
                        break;
                    }
                    let mut bit = 1u32.rotate_left(next);
                    while avail & bit == 0 {
                        bit = bit.rotate_left(1);
                        next += 1;
                    }
                    *s = next as u8;
                    avail &= !bit;
                    next += 1;
                }
            }
            let mut r = 0;
            for (k, &s) in slots.iter().enumerate() {
                r = party_set(c, if s == 0xff { -1 } else { i32::from(s) }, k as i32);
            }
            // (The handler returns what the last slot set left in eax.)
            Value::Int(r)
        }),
    );
    // 013d(n): the system's sound data group n is sent, waiting
    // through the driver's states (done at once here) and two more frames: six frames in all.
    n.register(
        0x013D,
        Native {
            init: Some(|_, c| {
                let n = c.stack.pop_int();
                c.scratch[0] = 0;
                c.scratch[1] = n;
                c.scratch[2] = 2;
            }),
            wait: Some(|g, c| match c.scratch[0] {
                0 => {
                    g.sound.spk_group = Some(c.scratch[1]);
                    c.scratch[0] = 1;
                    0
                }
                1 => {
                    c.scratch[0] = 2;
                    0
                }
                2 => {
                    c.scratch[0] = 3;
                    0
                }
                _ => {
                    c.scratch[2] -= 1;
                    u32::from(c.scratch[2] < 0)
                }
            }),
            result: None,
        },
    );
    // c003(map) / c031(map): the map-change record for (map, 0) (save +6/+0xa from +4/+8, or +0xc6/+0xca with +0x2a; +4 =
    // map, +8 = 0, +0x2a = 0), then for map > 0 the map loads (field / battle map); return 0.
    fn map_load(g: &mut FieldState, c: &mut NativeCall, battle: bool) -> Value {
        let map = c.stack.pop_int();
        let save = &mut c.memory.global[0];
        let u16_at = |s: &[u8], a: usize| s.get(a..a + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        if save.len() >= 0xcc {
            let (p6, pa) = if save[0x2a] != 0 {
                (u16_at(save, 0xc6), u16_at(save, 0xca))
            } else {
                (u16_at(save, 4), u16_at(save, 8))
            };
            save[6..8].copy_from_slice(&p6.to_le_bytes());
            save[0xa..0xc].copy_from_slice(&pa.to_le_bytes());
            save[4..6].copy_from_slice(&(map as u16).to_le_bytes());
            save[0x2a] = 0;
            save[8..0xa].copy_from_slice(&0u16.to_le_bytes());
        }
        if map > 0 {
            g.map_load_request = Some((map, battle));
        }
        Value::Int(0)
    }
    n.register(0xC003, Native::result(|g, c| map_load(g, c, false)));
    n.register(0xC031, Native::result(|g, c| map_load(g, c, true)));
    // c058(variable): its address (the handler leaves the variable lookup's result); c05b(v): a debug print; returns v.
    n.register(
        0xC058,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            Value::Int(aerugo_script::memory::encode_address(super::shapes::var_place(g, c, v)))
        }),
    );
    n.register(0xC05B, Native::result(|_, c| Value::Int(c.stack.pop_int())));
    // Debug memory (test events): c054(size) a block (aligned to 0x40; returns its address),
    // c055(address) frees it (returns 1), c056(group, index, address) reads file table entry (group, index) into
    // it (returns 1), c057(address) copies 0x68c0 bytes from address + 0x40 into the save image (returns 1).
    const HEAP: u32 = 0x7000_0000;
    fn heap(g: &mut FieldState, addr: i32) -> Option<(&mut Vec<u8>, usize)> {
        let a = (addr as u32).checked_sub(HEAP)?;
        let (k, off) = ((a >> 20) as usize, (a & 0xf_ffff) as usize);
        g.debug_heap.get_mut(k).map(|b| (b, off))
    }
    n.register(
        0xC054,
        Native::result(|g, c| {
            let size = c.stack.pop_int().max(0) as usize;
            g.debug_heap.push(vec![0; size]);
            Value::Int((HEAP + ((g.debug_heap.len() as u32 - 1) << 20)) as i32)
        }),
    );
    n.register(
        0xC055,
        Native::result(|g, c| {
            let a = c.stack.pop_int();
            if let Some((b, _)) = heap(g, a) {
                b.clear();
            }
            Value::Int(1)
        }),
    );
    n.register(
        0xC056,
        Native::result(|g, c| {
            let addr = c.stack.pop_int();
            let index = c.stack.pop_int();
            let group = c.stack.pop_int();
            let data = g.vfs.clone().and_then(|v| {
                FieldState::file_table(&v)
                    .and_then(|t| t.path(group.max(0) as usize, index.max(0) as usize))
                    .and_then(|p| v.read(&p).ok())
            });
            if let (Some(d), Some((b, off))) = (data, heap(g, addr)) {
                let n = d.len().min(b.len().saturating_sub(off));
                b[off..off + n].copy_from_slice(&d[..n]);
            }
            Value::Int(1)
        }),
    );
    n.register(
        0xC057,
        Native::result(|g, c| {
            let addr = c.stack.pop_int();
            if let Some((b, off)) = heap(g, addr)
                && let Some(src) = b.get(off + 0x40..off + 0x40 + 0x68c0)
                && let Some(dst) = c.memory.global[0].get_mut(..0x68c0)
            {
                dst.copy_from_slice(src);
            }
            Value::Int(1)
        }),
    );
    // 01b5: the preloaded movie (01b0) starts when it is loaded (save +0xe0 in 2–4: started with the mode at +0xdc;
    // state 5) — returns 1, else 0. (The remaster's movie layer is switched on first.)
    n.register(
        0x01B5,
        Native::result(|g, c| {
            let state = save_i32(c, 0xe0);
            if (state.wrapping_sub(2) as u32) < 3 {
                let (no, mode) = (save_i32(c, 0xd8), save_i32(c, 0xdc));
                super::start_movie(g, no.max(0) as u32, mode, true);
                set_save_u32(c, 0xe0, 5);
                return Value::Int(1);
            }
            Value::Int(0)
        }),
    );
    // 025b(v): achievement v (0x16 stays, 0x17 unlocks 0x16, 0x19 unlocks 0x18); returns the id.
    n.register(
        0x025B,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let id = match v {
                0x16 => return Value::Int(0x16),
                0x17 => 0x16,
                0x19 => 0x18,
                v => v,
            };
            g.achievements.push(id);
            Value::Int(id)
        }),
    );
    // 0213(key): the own body's option position element for the key, 0 without a
    // body or entry.
    n.register(
        0x0213,
        Native::result(|g, c| {
            let key = c.stack.pop_int() as u32;
            let w = c.worker;
            if g.workers.list[w].bind() != 1 || !g.chrs.get(w).is_some_and(Option::is_some) {
                return Value::Int(0);
            }
            let model = g.actors.get(w).and_then(|a| a.model);
            let e =
                model.and_then(|m| g.model_optpos.get(&m).cloned().flatten()).and_then(|o| o.element(key));
            Value::Int(e.map_or(0, |e| e.0))
        }),
    );
    // 502d(worker, key): chr_attach: the body hangs on the other worker's body at its option position for the key
    // (+0x19c, +0x1b8 its tag, +0x1a0 the element, +0x1a4 the offset); −1: let go (+0x19c = 0, +0x1b8 = −1).
    n.register(
        0x502D,
        Native::result(|g, c| {
            let key = c.stack.pop_int() as u32;
            let other = c.stack.pop_int();
            let w = c.worker;
            if !g.chrs.get(w).is_some_and(Option::is_some) {
                return Value::Int(0);
            }
            if other == -1 {
                if let Some(ch) = chr_of(g, w) {
                    ch.set_u32(crate::chr::PARENT, 0);
                    ch.set_i32(0x1b8, -1);
                }
                return Value::Int(0);
            }
            let p = index(g, other);
            let tag = chr_of(g, p).map(|ch| ch.i32(0x190));
            let model = g.actors.get(p).and_then(|a| a.model);
            let e =
                model.and_then(|m| g.model_optpos.get(&m).cloned().flatten()).and_then(|o| o.element(key));
            let Some(ch) = chr_of(g, w) else { return Value::Int(0) };
            match tag {
                Some(tag) => {
                    ch.set_u32(crate::chr::PARENT, p as u32 + 1);
                    ch.set_i32(0x1b8, tag);
                    let (elem, off) = e.map_or((0, [0.0, 0.0, 0.0, 1.0]), |e| (e.0, e.1));
                    ch.set_i32(0x1a0, elem);
                    for (k, v) in off.iter().enumerate() {
                        ch.set_f32(0x1a4 + 4 * k as u32, *v);
                    }
                }
                None => {
                    ch.set_u32(crate::chr::PARENT, 0);
                    ch.set_i32(0x1b8, -1);
                }
            }
            Value::Int(0)
        }),
    );
    // 5052(worker, part, x, y, z): attached to the other body's part (−1: none) with an offset of
    // (x, y, z) · 100 · the own model block's +0x38, +0x1b0 = 1; returns 0.
    n.register(
        0x5052,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            let part = c.stack.pop_int();
            let other = c.stack.pop_int();
            let w = c.worker;
            if !g.chrs.get(w).is_some_and(Option::is_some) {
                return Value::Int(0);
            }
            let (p, part) = if other == -1 { (None, 0) } else { (Some(index(g, other)), part) };
            let tag = p.and_then(|p| chr_of(g, p).map(|ch| ch.i32(0x190)));
            let b38 = g
                .actors
                .get(w)
                .and_then(|a| a.model)
                .and_then(|m| g.model_optpos.get(&m).cloned().flatten())
                .map_or(1.0, |o| o.block_38);
            let Some(ch) = chr_of(g, w) else { return Value::Int(0) };
            match (p, tag) {
                (Some(p), Some(tag)) => {
                    ch.set_u32(crate::chr::PARENT, p as u32 + 1);
                    ch.set_i32(0x1b8, tag);
                }
                _ => {
                    ch.set_u32(crate::chr::PARENT, 0);
                    ch.set_i32(0x1b8, -1);
                }
            }
            ch.set_i32(0x1a0, part);
            let b = f64::from(b38);
            ch.set_f32(0x1a4, (b * f64::from(x) * 100.0) as f32);
            ch.set_f32(0x1a8, (f64::from(y) * 100.0 * b) as f32);
            ch.set_f32(0x1ac, (f64::from(z) * 100.0 * b) as f32);
            ch.set_f32(0x1b0, 1.0);
            Value::Int(0)
        }),
    );
    // 504d(v): every live body's +0x4e4 = v (0 or less: the model block's +0x1c, the info block's +4); returns
    // 0.0 (each).
    n.register(
        0x504D,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            for w in 0..g.chrs.len() {
                let model = g.actors.get(w).and_then(|a| a.model);
                let r = model.and_then(|m| g.model_radii.get(&m).copied().flatten()).unwrap_or(1.0);
                if let Some(ch) = g.chrs[w].as_mut().filter(|ch| ch.u8(2) != 0) {
                    ch.set_f32(0x4e4, if v.is_nan() || v > 0.0 { v } else { r });
                }
            }
            for k in 0..g.attachments.len() {
                for slot in 0..2 {
                    let r = g.attachments[k][slot]
                        .as_ref()
                        .and_then(|a| g.model_radii.get(&a.model).copied().flatten())
                        .unwrap_or(1.0);
                    if let Some(a) = g.attachments[k][slot].as_mut() {
                        a.chr.set_f32(0x4e4, if v.is_nan() || v > 0.0 { v } else { r });
                    }
                }
            }
            Value::Float(0.0)
        }),
    );
    // 00ec(chr): the active party slot (0–2) holding the character, −1 when none (or chr −1).
    n.register(
        0x00EC,
        Native::result(|_, c| {
            let chr = c.stack.pop_int();
            if chr != -1 {
                for k in 0..3 {
                    let s = i32::from(img(c, PARTY + k));
                    if s == chr && s != 0xff {
                        return Value::Int(k as i32);
                    }
                }
            }
            Value::Int(-1)
        }),
    );
    // 0253: 0 for the language type's +0 being 0, 3 or 4 (Japanese, Korean, Chinese), else 1.
    n.register(0x0253, Native::result(|g, _| Value::Int(i32::from(!matches!(g.language_region, 0 | 3 | 4)))));
    // 70a2(n): the shared battle motion file (regmot) read when it is not in (reads finish at once here),
    // then waited on: the first poll with it loaded clears the pending flag, the next one ends. 70a4: a battle flag = 0 and,
    // when loaded, the file let go; returns 0.
    n.register(
        0x70A2,
        Native {
            init: Some(|g, c| {
                g.regmot_flag = true;
                c.scratch[0] = c.stack.pop_int();
                if !g.motion_groups.has((2, 0, 0)) {
                    g.read_motion_file((2, 0, 0));
                    g.regmot_data = true;
                }
                g.regmot_ready = true;
            }),
            wait: Some(|g, _| {
                if g.regmot_ready {
                    if !g.regmot_flag {
                        return 5;
                    }
                    g.regmot_flag = false;
                }
                0
            }),
            result: None,
        },
    );
    n.register(
        0x70A4,
        Native::result(|g, _| {
            g.regmot_ready = false;
            if g.regmot_data && g.motion_groups.has((2, 0, 0)) {
                g.regmot_data = false;
                g.motion_groups.remove((2, 0, 0));
            }
            Value::Int(0)
        }),
    );
    // 01fb: the menu value the menus keep.
    n.register(0x01FB, Native::result(|g, _| Value::Int(g.value_a228)));
    // 01f2: a mask of the characters 8–15 whose party record +0x2c bit 0 is set.
    n.register(
        0x01F2,
        Native::result(|_, c| {
            let mut m = 0u32;
            for i in 8..16 {
                let at = PARTY_RECORDS + i * PARTY_RECORD + 0x2c;
                if c.memory.global[0].get(at).is_some_and(|b| b & 1 != 0) {
                    m |= 1 << i;
                }
            }
            Value::Int(m as i32)
        }),
    );
    // 01f4(n, mask): the n-th set bit among bits 8–15 of the mask, −1 when there are fewer.
    n.register(
        0x01F4,
        Native::result(|_, c| {
            let mask = c.stack.pop_int() as u32;
            let n = c.stack.pop_int();
            let mut k = 0;
            for i in 8..16 {
                if mask & 1 << i != 0 {
                    if n == k {
                        return Value::Int(i);
                    }
                    k += 1;
                }
            }
            Value::Int(-1)
        }),
    );
    // 01d0: the own worker's placement flag 8 when it is a character; returns 1.
    n.register(
        0x01D0,
        Native::result(|g, c| {
            let w = &mut g.workers.list[c.worker];
            if w.bind() == 1 {
                w.set_u16(workers::PLACE, w.u16(workers::PLACE) | 8);
            }
            Value::Int(1)
        }),
    );
    // 01d4(slot, on): an attached body's flag bit 11 and the slot's flag bit 0; returns on.
    n.register(
        0x01D4,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let slot = c.stack.pop_int();
            if (0..2).contains(&slot) {
                let k = slot as usize;
                let wk = &g.workers.list[c.worker];
                if wk.u32(0xaf4 + k as u32 * 4) != 0
                    && wk.u16(0xb14) & (1 << k) != 0
                    && let Some(Some(a)) = g.attachments.get_mut(c.worker).map(|s| &mut s[k])
                {
                    a.chr.set_flag(11, on);
                }
            }
            if let Some(at) =
                u32::try_from(slot).ok().map(|s| 0xb08 + s * 4).filter(|&a| a + 4 <= workers::SIZE)
            {
                let wk = &mut g.workers.list[c.worker];
                wk.set_u32(at, wk.u32(at) & !1 | u32::from(on));
            }
            Value::Int(i32::from(on))
        }),
    );
    // 01e2 / 01e3: the screen distortion on / off (display flags bit 24, its value = 0, 01e2 also its timer = 0; the
    // remaster's switch, freshly on: +0x7e = 1, +0x78 = 0.25); return 0.
    n.register(
        0x01E2,
        Native::result(|g, _| {
            g.map_display |= 0x0100_0000;
            g.distortion.value_ded28 = 0;
            g.distortion.value_ea8 = 0;
            distortion_switch(&mut g.distortion, true);
            Value::Int(0)
        }),
    );
    n.register(
        0x01E3,
        Native::result(|g, _| {
            g.map_display &= !0x0100_0000;
            g.distortion.value_ded28 = 0;
            distortion_switch(&mut g.distortion, false);
            Value::Int(0)
        }),
    );
    // 01e4(x, y, z, n), 01e5(r, g, b) (bytes), 01e6(n, a, b) (+4 = a, +0x24 = b, flags
    // |= 1, |= 3 with n; returns n). 01e4/01e5 return 0.
    n.register(
        0x01E4,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            g.distortion.point = [x, y, z];
            g.distortion.count_ea0 = n;
            Value::Int(0)
        }),
    );
    n.register(
        0x01E5,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let gr = c.stack.pop_int();
            let r = c.stack.pop_int();
            g.distortion.rgb = [r as u8, gr as u8, b as u8];
            Value::Int(0)
        }),
    );
    n.register(
        0x01E6,
        Native::result(|g, c| {
            let b = c.stack.pop_float();
            let a = c.stack.pop_float();
            let n = c.stack.pop_int();
            g.distortion.value_e7c = a;
            g.distortion.value_e9c = b;
            g.distortion.flags |= if n != 0 { 3 } else { 1 };
            Value::Int(n)
        }),
    );
    // c02f(on, v): the system block's +0x208 = v and +2 bit 0 = on; returns on.
    n.register(
        0xC02F,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let on = c.stack.pop_int() != 0;
            g.workers.sys.value208 = v;
            g.workers.sys.flags2 = g.workers.sys.flags2 & !1 | u8::from(on);
            Value::Int(i32::from(on))
        }),
    );
    // 7015(v): a battle debug print; nothing else.
    n.register(
        0x7015,
        Native::result(|_, c| {
            c.stack.pop_int();
            Value::Int(0)
        }),
    );
    // 7035: a byte the battle code leaves (none set in the field: 0).
    n.register(0x7035, Native::result(|g, _| Value::Int(i32::from(g.workers.sys.battle_9f1))));
    // c02c(v) / c053(a, b, c, d): take their arguments and do nothing.
    n.register(
        0xC02C,
        Native::result(|_, c| {
            c.stack.pop_int();
            Value::Int(0)
        }),
    );
    n.register(
        0xC053,
        Native::result(|_, c| {
            for _ in 0..4 {
                c.stack.pop_int();
            }
            Value::Int(0)
        }),
    );
    // c002(on) / c00d(on): settings bits 3 / 5 (c00d returns the switch); c014: bit 5 is set.
    fn setting_bit(c: &mut NativeCall, bit: u32, on: bool) {
        let w = save_i32(c, 0x3d0c) as u32;
        set_save_u32(c, 0x3d0c, w & !(1 << bit) | u32::from(on) << bit);
    }
    n.register(
        0xC002,
        Native::result(|_, c| {
            let on = c.stack.pop_int() != 0;
            setting_bit(c, 3, on);
            Value::Int(0)
        }),
    );
    n.register(
        0xC00D,
        Native::result(|_, c| {
            let on = c.stack.pop_int() != 0;
            setting_bit(c, 5, on);
            Value::Int(i32::from(on))
        }),
    );
    n.register(0xC014, Native::result(|_, c| Value::Int(save_i32(c, 0x3d0c) >> 5 & 1)));
    // c009(v): a value the event system keeps; c051(on) / c052: a switch set / read.
    n.register(
        0xC009,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.workers.sys.byte_11f = v as u8;
            Value::Int(v)
        }),
    );
    n.register(
        0xC051,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            g.workers.sys.switch_832 = u8::from(on);
            Value::Int(i32::from(on))
        }),
    );
    n.register(0xC052, Native::result(|g, _| Value::Int(i32::from(g.workers.sys.switch_832))));
    // c030: the event system's frame count (frames run since its setup ended); c013: settings bit 3 is set.
    n.register(0xC030, Native::result(|g, _| Value::Int(g.workers.sys.frames)));
    n.register(0xC013, Native::result(|_, c| Value::Int(save_i32(c, 0x3d0c) >> 3 & 1)));
    // 015f(v) / 010d(n < 0xff → 0xa004 + n): mark a found thing: the save's 0x80-bit field (by v & 0xfff), and
    // for 0xa004–0xa01d the 26-bit field at +0x3d10.
    fn mark_found(c: &mut NativeCall, v: i32) {
        let n = v as u32 & 0xfff;
        if n < 0x80 {
            let at = 0x448c + (n >> 4) as usize * 2;
            let w = u16::from_le_bytes([img(c, at), img(c, at + 1)]) | 1 << (n & 0xf);
            set_img(c, at, w as u8);
            set_img(c, at + 1, (w >> 8) as u8);
        }
        let k = (v as u32).wrapping_sub(0xa004);
        if k < 0x1a {
            let w = save_i32(c, 0x3d10) as u32 | 1 << k;
            set_save_u32(c, 0x3d10, w);
        }
    }
    n.register(
        0x015F,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            mark_found(c, v);
            Value::Int(v)
        }),
    );
    n.register(
        0x010D,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            if (v as u32) < 0xff {
                mark_found(c, v + 0xa004);
                return Value::Int(1);
            }
            Value::Int(0)
        }),
    );
    // 017b(bit): a bit of the event effect mask off; returns the bit.
    n.register(
        0x017B,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            g.workers.sys.mask_b70 &= !(1u32 << (b as u32 & 31));
            Value::Int(b)
        }),
    );
    // 01bd(on): save +0xd7; returns 1.
    n.register(
        0x01BD,
        Native::result(|_, c| {
            let v = c.stack.pop_int();
            set_img(c, 0xd7, u8::from(v != 0));
            Value::Int(1)
        }),
    );
    // 023b(n): a help event flag is set; 023c(first, count): every one of a run is.
    fn help_flag(c: &NativeCall, n: i32) -> bool {
        let n = n.max(0);
        img(c, 0x3dcc + (n >> 3).min(0x7f) as usize) & 1 << (n & 7) != 0
    }
    n.register(
        0x023B,
        Native::result(|_, c| {
            let n = c.stack.pop_int();
            Value::Int(i32::from(help_flag(c, n)))
        }),
    );
    n.register(
        0x023C,
        Native::result(|_, c| {
            let count = c.stack.pop_int();
            let first = c.stack.pop_int();
            let mut k = 0;
            while k < count && help_flag(c, k + first) {
                k += 1;
            }
            Value::Int(i32::from(k == count))
        }),
    );
    // 0239(bit): a bit of the save's 0x14-word field at +0x4c.
    n.register(
        0x0239,
        Native::result(|_, c| {
            let b = c.stack.pop_int() as u32;
            if b >> 5 < 0x14 {
                let at = 0x4c + (b >> 5) as usize * 4;
                let v = save_i32(c, at) as u32 | 1 << (b & 31);
                set_save_u32(c, at, v);
            }
            Value::Int(1)
        }),
    );
}

fn register_party(n: &mut Natives<FieldState>) {
    // 007f(v): bits 13–14 of the engine flags of a line, box or zone (kinds 2, 3, 5, 6).
    n.register(
        0x007F,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as i16 as u16;
            let w = wk(g, c);
            if !matches!(w.kind, 2 | 3 | 5 | 6) {
                return Value::Int(0);
            }
            let f = w.flags();
            w.set_u16(workers::FLAGS, f ^ ((v << 13) ^ f) & 0x6000);
            Value::Int(1)
        }),
    );
    // 013e(on): the worker runs while the system holds the others (second flags bit 15).
    n.register(
        0x013E,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let w = wk(g, c);
            w.set_u16(workers::FLAGS2, w.flags2() & 0x7fff | u16::from(v != 0) << 15);
            Value::Int(i32::from(v != 0))
        }),
    );
    // 00ca(chr): joins the party (Rikku at story 0x43d goes into the reserve's first place; 0xf sets a flag).
    n.register(
        0x00CA,
        Native::result(|g, c| {
            let chr = c.stack.pop_int();
            party_join(g, c, chr as u32, 1);
            if chr == 6 {
                if save_u16(c, 0xbec) == 0x43d {
                    party_set(c, 6, 3);
                    return Value::Int(6);
                }
            } else if chr == 0xf {
                g.workers.sys.party_ac8 = 3;
            }
            Value::Int(chr)
        }),
    );
    // 0111: keep the party (the active slots at save +0x21, which of characters 0–6 have joined at +0x24, +0x20
    // set); 0112: put it back (each of 0–6 joins or leaves, the slots refilled); returns the kept flag.
    n.register(
        0x0111,
        Native::result(|_, c| {
            for k in 0..3 {
                let v = img(c, PARTY + k);
                set_img(c, 0x21 + k, v);
            }
            let mut mask = 0u32;
            for chr in 0..7u32 {
                if rec_flags(c, chr) & 1 == 1 {
                    mask |= 1 << chr;
                }
            }
            set_save_u32(c, 0x24, mask);
            set_img(c, 0x20, 1);
            Value::Int(0)
        }),
    );
    n.register(
        0x0112,
        Native::result(|g, c| {
            let kept = img(c, 0x20);
            if kept != 0 {
                let mask = save_i32(c, 0x24) as u32;
                for chr in 0..7u32 {
                    party_join(g, c, chr, i32::from(mask & (1 << chr) != 0));
                }
                let slots: [i32; 3] = std::array::from_fn(|k| i32::from(img(c, 0x21 + k) as i8));
                for (k, v) in slots.into_iter().enumerate() {
                    party_set(c, v, k as i32);
                }
            }
            set_img(c, 0x20, 0);
            Value::Int(i32::from(kept as i8))
        }),
    );
    // 01f9(chr): 00cb, then the record's flag bit 4 off; returns 1.
    n.register(
        0x01F9,
        Native::result(|g, c| {
            let chr = c.stack.pop_int();
            party_leave(g, c, chr);
            let at = PARTY_RECORDS + (chr as u32 & 0xff) as usize * PARTY_RECORD + 0x2c;
            set_img(c, at, img(c, at) & !0x10);
            Value::Int(1)
        }),
    );
    // 00cb(chr): leaves; the active slots it held are refilled with the rest.
    n.register(
        0x00CB,
        Native::result(|g, c| {
            let chr = c.stack.pop_int();
            party_leave(g, c, chr);
            Value::Int(chr)
        }),
    );
    n.register(
        0x00CC,
        Native::result(|_, c| {
            let chr = c.stack.pop_int();
            Value::Int(i32::from(rec_flags(c, chr as u32 & 0xff) & 1 == 1))
        }),
    );
    // 00e7(slot, chr): put a character (−1: none) into a slot; returns the character.
    n.register(
        0x00E7,
        Native::result(|_, c| {
            let mut chr = c.stack.pop_int();
            let slot = c.stack.pop_int();
            if chr == -1 {
                chr = 0xff;
            }
            party_set(c, chr, slot);
            Value::Int(chr)
        }),
    );
    // 00e6(chr): looks the character up in the active slots and keeps nothing.
    n.register(
        0x00E6,
        Native::result(|_, c| {
            c.stack.pop_int();
            Value::Int(0)
        }),
    );
    // 00e8(a, b, c): the three active slots into variables (empty: −1).
    n.register(
        0x00E8,
        Native::result(|g, c| {
            let vars = [c.stack.pop_int(), c.stack.pop_int(), c.stack.pop_int()];
            for (k, desc) in vars.into_iter().enumerate() {
                let v = img(c, PARTY + 2 - k);
                let v = if v == 0xff { -1 } else { i32::from(v) };
                let place = super::shapes::var_place(g, c, desc);
                c.memory.write_u32(place, v as u32);
            }
            Value::Int(1)
        }),
    );
}
