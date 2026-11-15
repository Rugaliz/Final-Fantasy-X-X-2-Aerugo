//! Items in the save image: the inventory, key items, gil and equipment, and what treasure gives.
//!
//! Treasure (`takara.bin`) records are four bytes: a kind, a count and an id. Kind 2 is an item (count of it),
//! 5 a piece of equipment (the id indexes `buki_get.bin`), 10 a key item; anything else is gil (count × 100).
//! Equipment from treasure is built into the 22-byte record the save image keeps, named by its abilities.

use std::sync::Arc;

use aerugo_formats::KernelTable;
use aerugo_formats::text::{self, Charset, Token};
use aerugo_vfs::{Language, Vfs};

/// Offsets in the FFX save image.
pub mod offset {
    /// Text variant flag (bit 3): picks the second name of items and equipment.
    pub const TEXT_VARIANT: usize = 0x3D0C;
    /// Al Bhed primers read (one bit each, 26).
    pub const PRIMERS: usize = 0x3D10;
    pub const GIL: usize = 0x3D48;
    /// Item ids (u16, 0xff when empty) and counts (u8), 112 slots.
    pub const ITEM_IDS: usize = 0x3ECC;
    pub const ITEM_COUNTS: usize = 0x40CC;
    /// Bitmaps of items ever obtained and ever used up.
    pub const ITEMS_GOT: usize = 0x41CC;
    pub const ITEMS_LOST: usize = 0x41EC;
    /// Key items held (one bit each).
    pub const KEY_ITEMS: usize = 0x448C;
    /// Equipment records, 200 of 22 bytes.
    pub const EQUIPMENT: usize = 0x449C;
}

pub const ITEM_SLOTS: usize = 0x70;
pub const EQUIPMENT_SLOTS: usize = 200;
pub const EQUIPMENT_SIZE: usize = 0x16;
pub const MAX_GIL: i32 = 999_999_999;
/// The first key item that is an Al Bhed primer, and how many there are.
const FIRST_PRIMER: u16 = 0xA004;
const PRIMERS: u16 = 26;

/// The tables treasure and its messages need.
#[derive(Debug)]
pub struct ItemTables {
    pub treasure: KernelTable,
    pub equipment_gets: KernelTable,
    pub items: KernelTable,
    pub key_items: KernelTable,
    pub equipment_names: KernelTable,
    /// Battle-end texts, which include the "Obtained ..." lines.
    pub messages: KernelTable,
    /// Shops: item shops (item ids), weapon shops (indexes into the shop equipment), the shop equipment, item
    /// prices and the value of each auto-ability.
    pub item_shops: KernelTable,
    pub weapon_shops: KernelTable,
    pub shop_equipment: KernelTable,
    pub item_prices: KernelTable,
    pub ability_prices: KernelTable,
    pub charset: Charset,
}

impl ItemTables {
    pub fn load(vfs: &Vfs, lang: Language) -> Option<Arc<ItemTables>> {
        let table = |name: &str| {
            let path = format!("ffx_ps2/ffx/master/jppc/battle/kernel/{name}.bin");
            KernelTable::parse(vfs.read_localized(&path, lang).ok()?).ok()
        };
        let charset =
            Charset::parse(&vfs.read(&text::charset_path(aerugo_formats::Game::Ffx, lang)).ok()?).ok()?;
        Some(Arc::new(ItemTables {
            treasure: table("takara")?,
            equipment_gets: table("buki_get")?,
            items: table("item")?,
            key_items: table("important")?,
            equipment_names: table("w_name")?,
            messages: table("btlend_txt")?,
            item_shops: table("item_shop")?,
            weapon_shops: table("arms_shop")?,
            shop_equipment: table("shop_arms")?,
            item_prices: table("item_rate")?,
            ability_prices: table("arms_rate")?,
            charset,
        }))
    }

    /// A text field of a record, decoded; `None` for an empty one ("-" marks a missing variant).
    fn text(&self, table: &KernelTable, id: u16, field: usize) -> Option<Vec<Token>> {
        let r = table.record(u32::from(id & 0xFFF))?;
        let tokens = text::decode(table.text(&r, field)?, &self.charset);
        (!tokens.is_empty() && tokens != [Token::Char('-')]).then_some(tokens)
    }

    /// A name in one of two variants (the second falls back to the first).
    fn name(
        &self,
        table: &KernelTable,
        id: u16,
        field: usize,
        variant_field: usize,
        variant: bool,
    ) -> String {
        let tokens = variant
            .then(|| self.text(table, id, variant_field))
            .flatten()
            .or_else(|| self.text(table, id, field));
        tokens.map(|t| plain(&t)).unwrap_or_default()
    }

    pub fn item_name(&self, id: u16, variant: bool) -> String {
        self.name(&self.items, id, 0, 4, variant)
    }

    pub fn key_item_name(&self, id: u16, variant: bool) -> String {
        self.name(&self.key_items, id, 0, 4, variant)
    }

    /// An equipment name: per name id, one per character, with the second variant 0x1C further on.
    pub fn equipment_name(&self, name: u16, owner: u8, variant: bool) -> String {
        let field = usize::from(owner.min(6)) * 4;
        self.name(&self.equipment_names, name, field, 0x1C + field, variant)
    }

    /// The model an equipment name shows for its owner.
    pub fn equipment_model(&self, name: u16, owner: u8) -> u16 {
        self.equipment_names
            .record(u32::from(name & 0xFFF))
            .and_then(|r| r.u16_at(0x38 + usize::from(owner.min(6)) * 2))
            .unwrap_or(0)
    }

    /// What treasure `index` holds.
    pub fn treasure(&self, index: u16) -> Treasure {
        let Some(r) = self.treasure.record(u32::from(index)) else { return Treasure::Gil(0) };
        let (kind, count, id) = (r.bytes[0], r.bytes[1], u16::from_le_bytes([r.bytes[2], r.bytes[3]]));
        match kind {
            2 => Treasure::Item(id, count),
            5 => match self.equipment_gets.record(u32::from(id)) {
                Some(e) => Treasure::Equipment(self.equipment_from_get(e.bytes)),
                None => Treasure::Gil(0),
            },
            10 => Treasure::KeyItem(id),
            _ => Treasure::Gil(i32::from(count) * 100),
        }
    }

    /// The save image's record for a piece of equipment described by a `buki_get` record: owner, kind and look,
    /// slot count (at least the abilities it has) and up to four abilities, named by them.
    fn equipment_from_get(&self, g: &[u8]) -> [u8; EQUIPMENT_SIZE] {
        let mut e = [0u8; EQUIPMENT_SIZE];
        e[2] = 1;
        e[3] = g[0];
        e[4] = g[1];
        e[5] = g[2];
        e[6] = 0xFF;
        e[8] = g[4];
        e[9] = g[5];
        e[10] = g[6];
        let mut used = 0u8;
        for i in 0..4 {
            let a = u16::from_le_bytes([g[8 + i * 2], g[9 + i * 2]]);
            let a = if a == 0 { 0xFF } else { a };
            used += u8::from(a != 0xFF);
            e[0xE + i * 2..0x10 + i * 2].copy_from_slice(&a.to_le_bytes());
        }
        e[0xB] = g[7].max(used);
        let name = equipment_name_id(&e);
        e[0..2].copy_from_slice(&name.to_le_bytes());
        let model = self.equipment_model(name, e[4]);
        e[0xC..0xE].copy_from_slice(&model.to_le_bytes());
        e
    }

    /// The items an item shop sells.
    pub fn item_shop(&self, shop: u16) -> Vec<u16> {
        self.shop_entries(&self.item_shops, shop)
    }

    /// The equipment a weapon shop sells, as save image records (named, with their owner's model).
    pub fn weapon_shop(&self, shop: u16) -> Vec<[u8; EQUIPMENT_SIZE]> {
        self.shop_entries(&self.weapon_shops, shop)
            .into_iter()
            .filter_map(|i| {
                let r = self.shop_equipment.record(u32::from(i))?;
                let mut e = [0u8; EQUIPMENT_SIZE];
                e.copy_from_slice(r.bytes.get(..EQUIPMENT_SIZE)?);
                let name = equipment_name_id(&e);
                e[0..2].copy_from_slice(&name.to_le_bytes());
                let model = self.equipment_model(name, e[4]);
                e[0xC..0xE].copy_from_slice(&model.to_le_bytes());
                Some(e)
            })
            .collect()
    }

    /// A shop record: a word, then up to 16 entries (0 ends the list).
    fn shop_entries(&self, table: &KernelTable, shop: u16) -> Vec<u16> {
        let Some(r) = table.record(u32::from(shop)) else { return Vec::new() };
        (0..16).map_while(|i| r.u16_at(2 + i * 2).filter(|&v| v != 0)).collect()
    }

    fn item_price(&self, id: u16) -> i32 {
        self.item_prices
            .record(u32::from(id & 0xFFF))
            .map_or(0, |r| i32::from_le_bytes([r.bytes[0], r.bytes[1], r.bytes[2], r.bytes[3]]))
    }

    /// What an item costs at the given price rate (percent; scripts set it, 100 by default), at least 1.
    pub fn item_buy_price(&self, id: u16, rate: i32) -> i32 {
        (rate * self.item_price(id) / 100).max(1)
    }

    /// What an item sells for: a quarter of its price, at least 1.
    pub fn item_sell_price(&self, id: u16) -> i32 {
        (self.item_price(id) / 4).max(1)
    }

    /// An equipment's value: its abilities' values plus 50, scaled by how many slots it has and how many are
    /// empty (1, 1, 1.5, 3, 5 for 0..4 each); 100000 with four empty slots.
    pub fn equipment_value(&self, e: &[u8; EQUIPMENT_SIZE]) -> i32 {
        const SCALE: [f32; 5] = [1.0, 1.0, 1.5, 3.0, 5.0];
        let slots = usize::from(e[0xB]);
        let mut empty = slots as i32;
        let mut sum = 0;
        for i in 0..slots.min(4) {
            let a = u16::from_le_bytes([e[0xE + i * 2], e[0xF + i * 2]]);
            if a != 0xFF && a != 0 {
                sum += self
                    .ability_prices
                    .record(u32::from(a & 0xFFF))
                    .map_or(0, |r| i32::from_le_bytes([r.bytes[0], r.bytes[1], r.bytes[2], r.bytes[3]]));
                empty -= 1;
            }
        }
        let empty = empty.max(0) as usize;
        if empty == 4 {
            return 100_000;
        }
        (SCALE[slots.min(4)] * (sum + 50) as f32 * SCALE[empty.min(4)]) as i32
    }

    pub fn equipment_buy_price(&self, e: &[u8; EQUIPMENT_SIZE], rate: i32) -> i32 {
        (rate * self.equipment_value(e) / 100).max(1)
    }

    /// A quarter of the value; nothing for equipment that cannot be sold (flags 0x01, 0x04, 0x08).
    pub fn equipment_sell_price(&self, e: &[u8; EQUIPMENT_SIZE]) -> i32 {
        if e[3] & 0xD != 0 { 0 } else { (self.equipment_value(e) / 4).max(1) }
    }

    /// Buy `n` of an item: false (nothing changes) without the gil or the room.
    pub fn buy_item(&self, save: &mut [u8], id: u16, n: i32, rate: i32) -> bool {
        let cost = self.item_buy_price(id, rate) * n;
        if n <= 0 || gil(save) < cost || i32::from(item_count(save, id)) + n > 99 {
            return false;
        }
        if !add_item(save, id, n) {
            return false;
        }
        add_gil(save, -cost)
    }

    /// Sell `n` of an item.
    pub fn sell_item(&self, save: &mut [u8], id: u16, n: i32) -> bool {
        if n <= 0 || i32::from(item_count(save, id)) < n || !add_item(save, id, -n) {
            return false;
        }
        add_gil(save, self.item_sell_price(id) * n)
    }

    /// Buy a piece of equipment: false without the gil or a free record.
    pub fn buy_equipment(&self, save: &mut [u8], e: &[u8; EQUIPMENT_SIZE], rate: i32) -> bool {
        let cost = self.equipment_buy_price(e, rate);
        if gil(save) < cost || add_equipment(save, e).is_none() {
            return false;
        }
        add_gil(save, -cost)
    }

    /// The pickup message for what was gained, as text (the original's "Obtained ..." lines).
    pub fn pickup_message(&self, got: &Treasure, variant: bool) -> String {
        let (line, name, number) = match got {
            Treasure::Item(id, n) if *id != 0 && *n > 0 => {
                (if *n == 1 { 0x18 } else { 0x19 }, self.item_name(*id, variant), i64::from(*n))
            }
            Treasure::KeyItem(id) => (0x17, self.key_item_name(*id, variant), 0),
            Treasure::Equipment(e) => {
                let name = u16::from_le_bytes([e[0], e[1]]);
                (0x16, self.equipment_name(name, e[4], variant), 0)
            }
            Treasure::Gil(g) if *g != 0 => (0x1A, String::new(), i64::from(*g)),
            _ => (0x1D, String::new(), 0),
        };
        let tokens = variant
            .then(|| self.text(&self.messages, line, 4))
            .flatten()
            .or_else(|| self.text(&self.messages, line, 0))
            .unwrap_or_default();
        let mut out = String::new();
        for t in tokens {
            match t {
                Token::Char(c) => out.push(c),
                Token::LineBreak => out.push('\n'),
                // Message variables: 0x30 the first (a name), 0x31 the second (a number).
                Token::Control { code: 0x12, arg: Some(0x30) } => out.push_str(&name),
                Token::Control { code: 0x12, arg: Some(0x31) } => out.push_str(&number.to_string()),
                _ => {}
            }
        }
        out
    }
}

fn plain(tokens: &[Token]) -> String {
    tokens
        .iter()
        .filter_map(|t| match *t {
            Token::Char(c) => Some(c),
            Token::LineBreak => Some('\n'),
            _ => None,
        })
        .collect()
}

/// What a treasure gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Treasure {
    Gil(i32),
    Item(u16, u8),
    KeyItem(u16),
    Equipment([u8; EQUIPMENT_SIZE]),
}

/// Whether the save image's text variant flag is set.
pub fn text_variant(save: &[u8]) -> bool {
    save.get(offset::TEXT_VARIANT).is_some_and(|b| b & 8 != 0)
}

fn u16_at(save: &[u8], at: usize) -> u16 {
    save.get(at..at + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

fn set_u16(save: &mut [u8], at: usize, v: u16) {
    if let Some(b) = save.get_mut(at..at + 2) {
        b.copy_from_slice(&v.to_le_bytes());
    }
}

fn set_bit(save: &mut [u8], words: usize, id: u16) {
    let at = words + usize::from((id & 0xFFF) >> 4) * 2;
    set_u16(save, at, u16_at(save, at) | 1 << (id & 0xF));
}

pub fn gil(save: &[u8]) -> i32 {
    save.get(offset::GIL..offset::GIL + 4).map_or(0, |b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Add (or take, when negative) gil; false when there is not enough to take.
pub fn add_gil(save: &mut [u8], amount: i32) -> bool {
    let have = gil(save);
    let Some(next) = have.checked_add(amount).filter(|n| *n >= 0) else { return false };
    let next = next.min(MAX_GIL);
    if let Some(b) = save.get_mut(offset::GIL..offset::GIL + 4) {
        b.copy_from_slice(&next.to_le_bytes());
    }
    true
}

/// How many of an item the inventory holds.
pub fn item_count(save: &[u8], id: u16) -> u8 {
    (0..ITEM_SLOTS)
        .find(|&i| u16_at(save, offset::ITEM_IDS + i * 2) == id)
        .map_or(0, |i| save[offset::ITEM_COUNTS + i])
}

/// Add (or remove, when negative) items: into the slot that holds the item, else the first empty slot. Counts
/// stay within 0..=99, an emptied slot is freed, and a held stack that changes marks the item as obtained (or
/// used up). False when nothing changed (no such item to remove, no free slot, or not an item id).
pub fn add_item(save: &mut [u8], id: u16, n: i32) -> bool {
    if id & 0xF000 != 0x2000 || n == 0 || save.len() < offset::KEY_ITEMS {
        return false;
    }
    let mut free = None;
    for i in 0..ITEM_SLOTS {
        let (at_id, at_n) = (offset::ITEM_IDS + i * 2, offset::ITEM_COUNTS + i);
        let (slot_id, count) = (u16_at(save, at_id), save[at_n]);
        if slot_id == id && count != 0 && (n >= 0 || i32::from(count) + n >= 0) {
            let next = (i32::from(count) + n).clamp(0, 99) as u8;
            save[at_n] = next;
            if next != count {
                set_bit(save, if n > 0 { offset::ITEMS_GOT } else { offset::ITEMS_LOST }, id);
            }
            if next == 0 {
                set_u16(save, at_id, 0xFF);
            }
            return true;
        }
        if (slot_id == 0 || count == 0) && free.is_none() {
            free = Some(i);
        }
    }
    match free {
        Some(i) if n > 0 => {
            set_u16(save, offset::ITEM_IDS + i * 2, id);
            save[offset::ITEM_COUNTS + i] = n.clamp(0, 99) as u8;
            true
        }
        _ => false,
    }
}

/// Mark a key item as held (and an Al Bhed primer as read).
pub fn add_key_item(save: &mut [u8], id: u16) {
    if id & 0xFFF < 0x80 {
        set_bit(save, offset::KEY_ITEMS, id);
    }
    if id.wrapping_sub(FIRST_PRIMER) < PRIMERS {
        let at = offset::PRIMERS;
        if let Some(b) = save.get_mut(at..at + 4) {
            let bits = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) | 1 << (id - FIRST_PRIMER);
            b.copy_from_slice(&bits.to_le_bytes());
        }
    }
}

pub fn has_key_item(save: &[u8], id: u16) -> bool {
    id & 0xFFF < 0x80
        && u16_at(save, offset::KEY_ITEMS + usize::from((id & 0xFFF) >> 4) * 2) & 1 << (id & 0xF) != 0
}

/// Put a piece of equipment in the first free record; its equipment id (0x5000 + slot), or `None` when all
/// 200 are taken.
pub fn add_equipment(save: &mut [u8], record: &[u8; EQUIPMENT_SIZE]) -> Option<u16> {
    (0..EQUIPMENT_SLOTS).find_map(|i| {
        let at = offset::EQUIPMENT + i * EQUIPMENT_SIZE;
        let slot = save.get_mut(at..at + EQUIPMENT_SIZE)?;
        if slot[2] != 0 {
            return None;
        }
        slot.copy_from_slice(record);
        slot[2] = 1;
        slot[6] = 0xFF;
        Some(0x5000 + i as u16)
    })
}

/// Give a treasure: add it to the save image. False when equipment did not fit.
pub fn give(save: &mut [u8], got: &Treasure) -> bool {
    match got {
        Treasure::Gil(g) => {
            if *g != 0 {
                add_gil(save, *g);
            }
            true
        }
        Treasure::Item(id, n) => {
            add_item(save, *id, i32::from(*n));
            true
        }
        Treasure::KeyItem(id) => {
            add_key_item(save, *id);
            true
        }
        Treasure::Equipment(e) => add_equipment(save, e).is_some(),
    }
}

/// The name an equipment record gets from its abilities (weapons when byte 5 is 0, else armour), slot count
/// and flags: the first rule that matches.
pub fn equipment_name_id(e: &[u8; EQUIPMENT_SIZE]) -> u16 {
    let abilities: [u16; 4] = std::array::from_fn(|i| u16::from_le_bytes([e[0xE + i * 2], e[0xF + i * 2]]));
    let has = |a: u16| abilities.contains(&a);
    let count = |list: &[u16]| list.iter().filter(|&&a| has(a)).count();
    let either = |a: u16, b: u16| has(a) || has(b);
    let slots = e[0xB];
    if e[5] == 0 {
        weapon_name(e[3], slots, &has, &count, &either)
    } else {
        armor_name(slots, &has, &count, &either)
    }
}

fn weapon_name(
    flags: u8,
    slots: u8,
    has: &dyn Fn(u16) -> bool,
    count: &dyn Fn(&[u16]) -> usize,
    either: &dyn Fn(u16, u16) -> bool,
) -> u16 {
    let elemental_strikes = count(&[0x8062, 0x8063, 0x8064, 0x8065]);
    let elemental_wards = count(&[0x8066, 0x8067, 0x8068, 0x8069]);
    let touches = count(&[0x801E, 0x8022, 0x8026, 0x802A]);
    let strikes = count(&[0x802E, 0x8036, 0x803A, 0x803E, 0x8042, 0x8046, 0x804A]);
    if flags & 4 != 0 {
        return 0x5000;
    }
    if flags & 8 != 0 {
        return 0x5001;
    }
    let rules: &[(bool, u16)] = &[
        (has(0x807A), 0x5002),
        (touches > 3, 0x5003),
        (has(0x8019), 0x5004),
        (has(0x800F) && has(0x8013) && has(0x8011), 0x5005),
        (has(0x800F) && has(0x8011), 0x5006),
        (has(0x800E) && has(0x8012), 0x5007),
        (has(0x800F), 0x5008),
        (has(0x800E), 0x5009),
        (has(0x8013), 0x500A),
        (has(0x8012), 0x500B),
        (has(0x8011), 0x500C),
        (has(0x8010), 0x500D),
        (has(0x800D), 0x500F),
        (strikes > 3, 0x5010),
        (elemental_strikes > 3, 0x5013),
        (elemental_wards > 3, 0x5014),
        (elemental_wards > 2 && has(0x8006), 0x5015),
        (has(0x800C), 0x5016),
        (has(0x801A), 0x5017),
        (touches > 2, 0x5018),
        (strikes > 2, 0x5019),
        (either(0x8003, 0x8004) && has(0x8005), 0x501A),
        (either(0x8003, 0x8004), 0x501B),
        (has(0x8005), 0x5020),
        (has(0x8006), 0x5021),
        (has(0x8007), 0x5022),
        (has(0x8001), 0x5023),
        (has(0x8002), 0x5024),
        (has(0x802E), 0x5025),
        (has(0x804A), 0x5026),
        (has(0x8036), 0x5027),
        (has(0x803A), 0x5028),
        (has(0x803E), 0x5029),
        (has(0x8042), 0x502A),
        (has(0x8046), 0x502B),
        (elemental_strikes > 2, 0x502C),
        (elemental_wards > 2, 0x502D),
        (touches > 1, 0x502E),
    ];
    if let Some(&(_, name)) = rules.iter().find(|r| r.0) {
        return name;
    }
    let pairs = [
        (0x802E, 0x802F),
        (0x8036, 0x8037),
        (0x803A, 0x803B),
        (0x803E, 0x803F),
        (0x8042, 0x8043),
        (0x8046, 0x8047),
        (0x804A, 0x804B),
    ];
    let rules: &[(bool, u16)] = &[
        (pairs.iter().filter(|&&(a, b)| either(a, b)).count() > 1, 0x502F),
        (has(0x802F), 0x5030),
        (has(0x804B), 0x5031),
        (has(0x8037), 0x5032),
        (has(0x803B), 0x5033),
        (has(0x803F), 0x5034),
        (has(0x8043), 0x5035),
        (has(0x8047), 0x5036),
        (has(0x8000), 0x5037),
        (has(0x801E), 0x5038),
        (has(0x8022), 0x5039),
        (has(0x8026), 0x503A),
        (has(0x802A), 0x503B),
        (has(0x807C), 0x501C),
        (has(0x807D), 0x501D),
        (has(0x807E), 0x501E),
        (has(0x807F), 0x501F),
        (slots > 3, 0x503C),
        (elemental_wards > 0 && elemental_strikes > 0, 0x503D),
        (slots > 1, 0x503E),
        (either(0x8068, 0x8069), 0x503F),
        (either(0x8064, 0x8065), 0x5040),
        (has(0x8067), 0x5041),
        (has(0x8066), 0x5042),
        (has(0x8063), 0x5043),
        (has(0x8062), 0x5044),
        (has(0x800B), 0x5045),
    ];
    rules.iter().find(|r| r.0).map_or(0x5046, |r| r.1)
}

fn armor_name(
    slots: u8,
    has: &dyn Fn(u16) -> bool,
    count: &dyn Fn(&[u16]) -> usize,
    either: &dyn Fn(u16, u16) -> bool,
) -> u16 {
    let guards = count(&[0x8008, 0x8009, 0x800A]);
    let wards_a = count(&[0x8021, 0x8025, 0x8029, 0x802D]);
    let wards_b = count(&[0x8020, 0x8024, 0x8028, 0x802C]);
    let status = count(&[0x8054, 0x8055, 0x8057, 0x8058]);
    let protects =
        count(&[0x8030, 0x8034, 0x8038, 0x803C, 0x8040, 0x8044, 0x8048, 0x804C, 0x804E, 0x8050, 0x8052]);
    let eat_a = count(&[0x806A, 0x806B, 0x806C, 0x806D]);
    let eat_b = count(&[0x806E, 0x806F, 0x8070, 0x8071]);
    let eat_c = count(&[0x8072, 0x8073, 0x8074, 0x8075]);
    let eat_d = count(&[0x8076, 0x8077, 0x8078, 0x8079]);
    let stat_ups = count(&[0x805E, 0x805F, 0x8060, 0x8061]);
    let all_ups = stat_ups + count(&[0x8059, 0x805A, 0x805B, 0x805C, 0x805D]);
    let with_8056 = usize::from(has(0x8056)) + status;
    let pairs = [(0x8021, 0x8020), (0x8025, 0x8024), (0x8029, 0x8028), (0x802D, 0x802C)];
    let rules: &[(bool, u16)] = &[
        (has(0x8017) && has(0x8018), 0x504A),
        (has(0x8080), 0x509E),
        (has(0x8017), 0x504B),
        (has(0x8018), 0x504C),
        (wards_a > 3, 0x504D),
        (wards_b > 3, 0x504E),
        (status > 3, 0x504F),
        (guards > 2, 0x5050),
        (has(0x8008) && has(0x8009), 0x5051),
        (protects > 3, 0x5052),
        (eat_a > 3, 0x5053),
        (eat_b > 3, 0x5054),
        (eat_c > 3, 0x5055),
        (eat_d > 3, 0x5056),
        (has(0x8016), 0x5057),
        (has(0x8015), 0x5058),
        (has(0x801B) && has(0x801C), 0x5059),
        (with_8056 > 2, 0x505A),
        (wards_a > 2, 0x505B),
        (has(0x801B), 0x505C),
        (has(0x801C), 0x505D),
        (has(0x800A), 0x505E),
        (has(0x8009), 0x505F),
        (stat_ups > 3, 0x5060),
        (all_ups > 3, 0x5061),
        (protects > 2, 0x5062),
        (has(0x801D), 0x5063),
        (has(0x8008), 0x5064),
        (wards_b > 2, 0x5065),
        (all_ups > 2, 0x5066),
        (with_8056 > 1, 0x5067),
        (all_ups > 1, 0x5068),
        (either(0x8057, 0x805C), 0x5069),
        (either(0x8056, 0x805B), 0x506A),
        (either(0x8058, 0x805D), 0x506B),
        (either(0x8054, 0x8059), 0x506C),
        (either(0x8055, 0x805A), 0x506D),
        (has(0x8007), 0x506E),
        (eat_a > 2, 0x506F),
        (eat_b > 2, 0x5070),
        (eat_c > 2, 0x5071),
        (eat_d > 2, 0x5072),
        (pairs.iter().filter(|&&(a, b)| either(a, b)).count() > 1, 0x5073),
        (protects > 1, 0x5074),
        (has(0x8021), 0x5075),
        (has(0x8025), 0x5076),
        (has(0x8029), 0x5077),
        (has(0x802D), 0x5078),
        (either(0x8052, 0x8053), 0x5079),
        (either(0x804E, 0x804F), 0x507A),
        (either(0x8050, 0x8051), 0x507B),
        (either(0x804C, 0x804D), 0x507C),
        (either(0x8030, 0x8031), 0x507D),
        (either(0x8034, 0x8035), 0x507E),
        (either(0x8038, 0x8039), 0x507F),
        (either(0x803C, 0x803D), 0x5080),
        (either(0x8040, 0x8041), 0x5081),
        (either(0x8044, 0x8045), 0x5082),
        (either(0x8048, 0x8049), 0x5083),
        (either(0x8020, 0x801F), 0x5084),
        (either(0x8024, 0x8023), 0x5085),
        (either(0x8028, 0x8027), 0x5086),
        (either(0x802C, 0x802B), 0x5087),
        (has(0x805E), 0x5088),
        (has(0x8061), 0x5089),
        (has(0x8060), 0x508A),
        (has(0x805F), 0x508B),
        (eat_c + eat_d > 3, 0x508C),
        (slots > 3, 0x508D),
        (eat_a > 0 && eat_b > 0, 0x508E),
        (eat_a > 1, 0x508F),
        (eat_b > 1, 0x5090),
        (eat_c > 1, 0x5091),
        (eat_d > 1, 0x5092),
        (either(0x806C, 0x806D), 0x5093),
        (either(0x8070, 0x8071), 0x5094),
        (either(0x8078, 0x8079), 0x5095),
        (either(0x8074, 0x8075), 0x5096),
        (slots > 2, 0x5097),
        (either(0x806A, 0x806B), 0x5098),
        (either(0x806E, 0x806F), 0x5099),
        (either(0x8076, 0x8077), 0x509A),
        (either(0x8072, 0x8073), 0x509B),
    ];
    rules.iter().find(|r| r.0).map_or(0x509C + u16::from(slots < 2), |r| r.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Vec<u8> {
        let mut s = vec![0u8; crate::save::FFX_IMAGE_SIZE];
        for i in 0..ITEM_SLOTS {
            set_u16(&mut s, offset::ITEM_IDS + i * 2, 0xFF);
        }
        s
    }

    #[test]
    fn items_stack_cap_at_99_and_free_their_slot_when_used_up() {
        let mut s = image();
        assert!(add_item(&mut s, 0x2000, 8));
        assert!(add_item(&mut s, 0x2006, 3));
        assert!(add_item(&mut s, 0x2000, 95));
        assert_eq!(item_count(&s, 0x2000), 99);
        assert_eq!(u16_at(&s, offset::ITEM_IDS + 2), 0x2006);
        assert!(add_item(&mut s, 0x2006, -3));
        assert_eq!(u16_at(&s, offset::ITEM_IDS + 2), 0xFF);
        // Only a change to a held stack marks the item (a new slot does not).
        assert_eq!(u16_at(&s, offset::ITEMS_GOT), 0x01);
        assert_eq!(u16_at(&s, offset::ITEMS_LOST), 0x40);
        // A freed slot is reused.
        assert!(add_item(&mut s, 0x2010, 1));
        assert_eq!(u16_at(&s, offset::ITEM_IDS + 2), 0x2010);
        assert!(!add_item(&mut s, 0x2011, -1));
    }

    #[test]
    fn gil_stays_in_range() {
        let mut s = image();
        assert!(add_gil(&mut s, 300));
        assert!(!add_gil(&mut s, -301));
        assert_eq!(gil(&s), 300);
        assert!(add_gil(&mut s, MAX_GIL));
        assert_eq!(gil(&s), MAX_GIL);
    }

    #[test]
    fn key_items_and_primers() {
        let mut s = image();
        add_key_item(&mut s, 0xA001);
        add_key_item(&mut s, 0xA005);
        assert!(has_key_item(&s, 0xA001));
        assert!(has_key_item(&s, 0xA005));
        assert!(!has_key_item(&s, 0xA002));
        assert_eq!(s[offset::PRIMERS], 2);
    }

    #[test]
    fn equipment_names_follow_abilities_and_slots() {
        // The starting sword: no abilities, no slots; the starting armour: one slot.
        let mut e = [0u8; EQUIPMENT_SIZE];
        for i in 0..4 {
            e[0xE + i * 2] = 0xFF;
        }
        assert_eq!(equipment_name_id(&e), 0x5046);
        e[5] = 1;
        e[0xB] = 1;
        assert_eq!(equipment_name_id(&e), 0x509D);
        e[0xB] = 4;
        assert_eq!(equipment_name_id(&e), 0x508D);
        // A weapon with Firestrike in it.
        e[5] = 0;
        e[0xE..0x10].copy_from_slice(&0x801Eu16.to_le_bytes());
        assert_eq!(equipment_name_id(&e), 0x5038);
    }

    #[test]
    fn equipment_takes_the_first_free_record() {
        let mut s = image();
        let mut e = [0u8; EQUIPMENT_SIZE];
        e[0] = 0x46;
        e[1] = 0x50;
        assert_eq!(add_equipment(&mut s, &e), Some(0x5000));
        assert_eq!(add_equipment(&mut s, &e), Some(0x5001));
        assert_eq!(s[offset::EQUIPMENT + 2], 1);
        assert_eq!(s[offset::EQUIPMENT + 6], 0xFF);
    }
}
