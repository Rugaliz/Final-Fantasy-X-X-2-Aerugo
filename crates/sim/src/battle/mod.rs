//! Battles as the original runs them.
//!
//! A battle starts from the field (a script's 7002 or a walking roll) as an encounter: an area, one of its groups
//! and a formation. MsBattleExe picks the battle's file (battle file set 0xe: the area's first file, then the
//! formations of the groups before, then the formation's place in its group) and names the battle "<area>_<nn>"
//! from the area's name and the formation's number; the read steps then load that file, the party's records from
//! the save, each monster's file and texts, and run the battle's scripts.
//!
//! Files (numbered through the battle file table, sets 0xd / 0xe / 0x10):
//! - a battle file (`battle/btl/<name>/<name>.bin`) and a monster file (`battle/mon/_mNNN/mNNN.bin`) are chunk
//!   containers: u32 chunk count, then each chunk's offset (0: none); a chunk ends where the next present one
//!   starts.
//! - Battle file chunks: 0 the battle's script, 1 (its first byte kept by the set-up), 2 the formation (+1 a flag
//!   the music set-up reads, +2 the effect texture set, +3 a flag several set-up steps read (its role is not
//!   traced yet), +0xc eight monster ids: 0x1000 | monster number, 0xffff none), 3 the positions.
//! - Monster file chunks: 0 the monster's script, 1 (its first byte kept at +0xdf6), 2 the stats, 4 the
//!   rewards (0x118 bytes, copied to the slot's reward record), 5 its sound data, 6 (kept for the display).
//! - Kernel tables the battle reads (`battle/kernel/`, localized): command, ply_rom, a_ability, item, monmagic1,
//!   monmagic2, and monster1–3 (the monsters' names and texts: numbers below 0x65 in monster1, 0x65–0xb4 in
//!   monster2, the rest in monster3).
//!
//! The records: [`records`].

pub mod records;
pub mod scripts;
pub mod setup;

use std::ops::Range;
use std::sync::Arc;

use aerugo_formats::KernelTable;
use aerugo_vfs::{Language, Vfs};

pub use records::Records;

/// The battle file sets of the file table.
pub const KERNEL_SET: usize = 0xd;
pub const BATTLE_SET: usize = 0xe;
pub const MONSTER_SET: usize = 0x10;

/// A chunk container (battle and monster files).
#[derive(Debug, Clone, Default)]
pub struct ChunkFile {
    pub bytes: Vec<u8>,
    chunks: Vec<Option<Range<usize>>>,
}

impl ChunkFile {
    pub fn parse(bytes: Vec<u8>) -> Option<ChunkFile> {
        let word =
            |at: usize| bytes.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
        let count = word(0)?;
        if !(1..=32).contains(&count) {
            return None;
        }
        let offsets = (0..count).map(|i| word(4 + 4 * i)).collect::<Option<Vec<_>>>()?;
        let chunks = offsets
            .iter()
            .enumerate()
            .map(|(i, &start)| {
                if start == 0 || start > bytes.len() {
                    return None;
                }
                let end = offsets[i + 1..].iter().copied().find(|&o| o != 0).unwrap_or(bytes.len());
                Some(start..end.clamp(start, bytes.len()))
            })
            .collect();
        Some(ChunkFile { bytes, chunks })
    }

    /// A chunk's bytes (empty when the file has none).
    pub fn chunk(&self, index: usize) -> &[u8] {
        self.chunks.get(index).cloned().flatten().map_or(&[], |r| &self.bytes[r])
    }

    pub fn has_chunk(&self, index: usize) -> bool {
        self.chunks.get(index).is_some_and(|c| c.is_some())
    }
}

/// The battle file's chunks.
pub mod battle_chunk {
    pub const SCRIPT: usize = 0;
    pub const HEAD: usize = 1;
    pub const FORMATION: usize = 2;
    pub const POSITIONS: usize = 3;
}

/// The monster file's chunks.
pub mod monster_chunk {
    pub const SCRIPT: usize = 0;
    pub const HEAD: usize = 1;
    pub const STATS: usize = 2;
    pub const REWARDS: usize = 4;
    pub const SOUNDS: usize = 5;
    pub const DISPLAY: usize = 6;
}

/// The formation chunk's fields.
pub mod formation {
    /// A flag the battle music set-up reads (0081ec20 gets whether it is 1).
    pub const MUSIC_FLAG: usize = 1;
    /// The battle effect texture set (MsEtEffectReadTex).
    pub const EFFECT_TEXTURES: usize = 2;
    /// A flag the record set-up reads (+0x20 of every record, and every character's command 0x3002).
    pub const SPECIAL: usize = 3;
    /// Eight monster ids (u16).
    pub const MONSTERS: usize = 0xc;
}

/// The kernel tables a battle reads.
#[derive(Debug)]
pub struct BattleTables {
    pub command: KernelTable,
    pub ply_rom: KernelTable,
    pub abilities: KernelTable,
    pub items: KernelTable,
    pub monster_magic1: KernelTable,
    pub monster_magic2: KernelTable,
    /// monster1–3.
    pub monsters: [KernelTable; 3],
    /// ctb_base (kernel file 0xb, not localized): by agility − 1, +0 the ticks of a turn, +1 the range of the
    /// first turn's random head start.
    pub ctb_base: KernelTable,
}

impl BattleTables {
    pub fn load(vfs: &Vfs, lang: Language) -> Option<Arc<BattleTables>> {
        let table = |name: &str| {
            let path = format!("ffx_ps2/ffx/master/jppc/battle/kernel/{name}.bin");
            KernelTable::parse(vfs.read_localized(&path, lang).ok()?).ok()
        };
        Some(Arc::new(BattleTables {
            command: table("command")?,
            ply_rom: table("ply_rom")?,
            abilities: table("a_ability")?,
            items: table("item")?,
            monster_magic1: table("monmagic1")?,
            monster_magic2: table("monmagic2")?,
            monsters: [table("monster1")?, table("monster2")?, table("monster3")?],
            ctb_base: KernelTable::parse(
                vfs.read("ffx_ps2/ffx/master/jppc/battle/kernel/ctb_base.bin").ok()?,
            )
            .ok()?,
        }))
    }
}

/// kernel_rec: a table's record by id; an id no segment holds reads the first segment's first record.
pub fn kernel_record(t: &KernelTable, id: u32) -> Option<aerugo_formats::kernel::Record<'_>> {
    t.record(id).or_else(|| t.record(t.segments().first()?.first_id))
}

/// Where a battle comes from and what it is called.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encounter {
    pub battle: crate::encounter::Battle,
    /// The battle file's number in set 0xe.
    pub file: usize,
    /// "<area>_<nn>".
    pub name: String,
}

impl Encounter {
    pub fn new(
        table: &crate::encounter::EncounterTable,
        battle: crate::encounter::Battle,
    ) -> Option<Encounter> {
        let area = table.areas.get(battle.area)?;
        Some(Encounter {
            battle,
            file: table.battle_file(&battle)?,
            name: format!("{}_{:02}", area.name, battle.formation_id),
        })
    }
}

/// One combatant as the host shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Combatant {
    pub slot: usize,
    pub name: String,
    pub hp: i32,
    pub max_hp: i32,
    pub mp: i32,
    pub max_mp: i32,
    /// Ticks before its turn.
    pub wait: u8,
}

/// A battle.
#[derive(Debug, Clone)]
pub struct Battle {
    pub encounter: Encounter,
    pub file: ChunkFile,
    /// The monster files read, in read order (a record's [`records::MONSTER_FILE`] is an index here + 1).
    pub monster_files: Vec<ChunkFile>,
    pub records: Records,
    /// The party as the battle starts (three front-line slots, then the four after them; 0xff none).
    pub front_line: [u8; 7],
    /// The save's reserve order as the battle copied it (save +0x3d5b, 20 bytes).
    pub reserve_order: [u8; 20],
    /// Who strikes first (MsGetFirstAttack): 0 neither, 1 the party, 2 the monsters.
    pub first_attack: u8,
}

impl Battle {
    /// The formation chunk's flag the record set-up reads.
    pub fn special(&self) -> bool {
        self.file.chunk(battle_chunk::FORMATION).get(formation::SPECIAL).is_some_and(|&b| b != 0)
    }

    /// A monster's file (by the record's [`records::MONSTER_FILE`]).
    pub fn monster_file(&self, slot: usize) -> Option<&ChunkFile> {
        use records::Fields;
        let index = self.records.get(slot).u32_at(records::MONSTER_FILE) as usize;
        index.checked_sub(1).and_then(|i| self.monster_files.get(i))
    }
}
