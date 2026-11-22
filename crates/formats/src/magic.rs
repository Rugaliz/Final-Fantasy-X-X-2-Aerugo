//! Magic files: the effects' data. The remaster ships each effect as a Windows DLL (`magicFiles/FFX/
//! magic_NNNN.dll`) whose initialised data section carries the effect's data in the PS2 layout; Aerugo reads that
//! section as data only (the DLL's code is never run; each effect's small driver is ported to Rust). See
//! `docs/formats/magic.md`.
//!
//! - [`DllData`]: a DLL's image base and its `.data` section (virtual address and bytes).
//! - [`Oef2`]: the effect file container: a directory header with typed tables (images, palettes, slots, and the
//!   particle groups), and a second header at `+0x3c` holding the same counts with offsets into the data.
//! - [`Ppp`]: a particle group: its parts (each a timed list of sub-part definitions that spawn objects and run
//!   particle programs over them).
//!
//! Offsets in the files are relative: [`Oef2`]'s from its second header, a particle group's from its header, a
//! part's sub-part definitions and their program references from the part record. The readers keep them as
//! offsets into the section bytes and never panic on bad input.

/// A DLL's initialised data section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DllData {
    pub base: u32,
    /// The section's virtual address (absolute, with the base) and its bytes.
    pub va: u32,
    pub bytes: Vec<u8>,
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

impl DllData {
    /// The `.data` section of a 32-bit PE image.
    pub fn read(file: &[u8]) -> Option<DllData> {
        if file.get(0..2)? != b"MZ" {
            return None;
        }
        let pe = u32_at(file, 0x3c)? as usize;
        if file.get(pe..pe + 4)? != b"PE\0\0" || u16_at(file, pe + 4)? != 0x14c {
            return None;
        }
        let sections = usize::from(u16_at(file, pe + 6)?);
        let opt = pe + 0x18;
        let opt_size = usize::from(u16_at(file, pe + 0x14)?);
        let base = u32_at(file, opt + 0x1c)?;
        for i in 0..sections {
            let s = opt + opt_size + i * 0x28;
            let name = file.get(s..s + 8)?;
            if !name.starts_with(b".data\0") {
                continue;
            }
            let (va, raw_size, raw) =
                (u32_at(file, s + 0xc)?, u32_at(file, s + 0x10)?, u32_at(file, s + 0x14)?);
            let bytes = file.get(raw as usize..(raw as usize).checked_add(raw_size as usize)?)?.to_vec();
            return Some(DllData { base, va: base.wrapping_add(va), bytes });
        }
        None
    }

    /// The section offset of absolute address `va`.
    pub fn offset(&self, va: u32) -> Option<usize> {
        let o = va.checked_sub(self.va)? as usize;
        (o < self.bytes.len()).then_some(o)
    }

    /// The candidates for an OEF2 header by its layout (16-byte aligned; a second header at `+0x3c` with the
    /// same counts; every table inside the section). An effect's driver names its own; this is a check.
    pub fn oef2_candidates(&self) -> Vec<usize> {
        let d = &self.bytes;
        (0..d.len().saturating_sub(0x60)).step_by(16).filter(|&f| Oef2::at(d, f).is_some()).collect()
    }
}

/// An OEF2 effect file at a section offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Oef2 {
    /// The directory header and the second header (where the tables' offsets count from).
    pub head: usize,
    pub second: usize,
}

/// The OEF2 tables: (offset field, count field) in both headers.
const TABLES: [(usize, usize); 8] =
    [(4, 0x42), (8, 0x44), (0xc, 0x46), (0x10, 0x48), (0x14, 0x4a), (0x18, 0x4c), (0x20, 0x50), (0x24, 0x52)];

/// Table kinds by their offset field: image records (0x20 bytes), palette records (0x10), the particle groups.
pub const KIND_IMAGE: usize = 8;
pub const KIND_PALETTE: usize = 0xc;
pub const KIND_PARTICLE: usize = 0x20;

impl Oef2 {
    /// The file at `head` when its layout holds.
    pub fn at(d: &[u8], head: usize) -> Option<Oef2> {
        let sb = u32_at(d, head + 0x3c)? as usize;
        if sb == 0 || sb & 0xf != 0 {
            return None;
        }
        let second = head.checked_add(sb)?;
        u32_at(d, second + 0x5c)?;
        let mut any = false;
        for (off, cnt) in TABLES {
            let (c1, c2) = (u16_at(d, head + cnt)?, u16_at(d, second + cnt)?);
            if c1 != c2 {
                return None;
            }
            if c2 != 0 {
                any = true;
                let t = u32_at(d, second + off)? as usize;
                if t == 0
                    || second + t + 4 * usize::from(c2) > d.len()
                    || head + u32_at(d, head + off)? as usize > d.len()
                {
                    return None;
                }
            }
        }
        (any && u16_at(d, head + 0x40)? == 0 && u16_at(d, second + 0x40)? == 0)
            .then_some(Oef2 { head, second })
    }

    /// How many entries table `kind` has.
    pub fn count(&self, d: &[u8], kind: usize) -> usize {
        TABLES.iter().find(|t| t.0 == kind).and_then(|t| u16_at(d, self.second + t.1)).map_or(0, usize::from)
    }

    /// Entry `n` of table `kind` as a section offset (None when the entry is empty).
    pub fn entry(&self, d: &[u8], kind: usize, n: usize) -> Option<usize> {
        if n >= self.count(d, kind) {
            return None;
        }
        let list = self.second + u32_at(d, self.second + kind)? as usize;
        let o = u32_at(d, list + 4 * n)? as usize;
        (o != 0).then(|| self.second + o)
    }

    /// Particle group `n`.
    pub fn particle(&self, d: &[u8], n: usize) -> Option<Ppp> {
        Ppp::at(d, self.entry(d, KIND_PARTICLE, n)?)
    }
}

/// A particle group (the ppp header) at a section offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ppp {
    pub head: usize,
}

/// A part record: its length (frames · 0x1000), whether it runs until its objects are gone, and its lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub record: usize,
    pub length: u32,
    pub until_empty: bool,
    /// The part's program-table indexes (+8 list) and its second list (+0xc): section offsets of the counts.
    pub programs: usize,
    pub second: usize,
    /// The sub-part definitions, in their chain order (section offsets).
    pub defs: Vec<usize>,
}

impl Ppp {
    pub fn at(d: &[u8], head: usize) -> Option<Ppp> {
        u32_at(d, head + 0x1c)?;
        Some(Ppp { head })
    }

    /// The counts: parts (+6), the 0x20-byte packet records (+8), the u32 list (+0xa), the pairs (+0xc).
    pub fn part_count(&self, d: &[u8]) -> usize {
        u16_at(d, self.head + 6).map_or(0, usize::from)
    }

    /// Part `n`'s record (the parts list at +0x10, offsets from the header).
    pub fn part(&self, d: &[u8], n: usize) -> Option<Part> {
        if n >= self.part_count(d) {
            return None;
        }
        let list = self.head + u32_at(d, self.head + 0x10)? as usize;
        let record = self.head + u32_at(d, list + 4 * n)? as usize;
        let length = u32_at(d, record)?;
        let until_empty = *d.get(record + 4)? != 0;
        let programs = record + u32_at(d, record + 8)? as usize;
        let second = record + u32_at(d, record + 0xc)? as usize;
        // The chain from +0x10 (each definition's +0 is the next one's offset from the record); its last element
        // is a terminator the binding cuts off.
        let mut defs = Vec::new();
        let mut def = record + 0x10;
        loop {
            if defs.len() > 0x1000 || def + 0x28 > d.len() {
                return None;
            }
            defs.push(def);
            let next = u32_at(d, def)? as usize;
            if next == 0 {
                break;
            }
            def = record + next;
        }
        defs.pop();
        Some(Part { record, length, until_empty, programs, second, defs })
    }
}
