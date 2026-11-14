//! Data tables ("kernel" files) under `battle/kernel/` (see `docs/formats/kernel.md`).
//!
//! A table is an array of fixed-size records, optionally followed by a text pool. Text fields in a record are
//! `u16` offsets into that pool. FFX and FFX-2 use different headers.

use std::collections::BTreeMap;

use aerugo_vfs::Vfs;

use crate::Game;

/// Upper bound on FFX segment counts seen in the data; anything larger is not a table.
const MAX_SEGMENTS: i16 = 8;
const FFX2_HEADER_LEN: usize = 0x20;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KernelError {
    #[error("not a data table")]
    NotATable,
    #[error("segment {0} lies outside the file")]
    BadSegment(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// FFX: segment list at 0x08, each with its own id range.
    Ffx,
    /// FFX-2: one segment described by a 0x20-byte header.
    Ffx2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub first_id: u32,
    pub last_id: u32,
    pub record_size: usize,
    pub data_offset: usize,
    /// Absolute offset of the text pool, when the table has one.
    pub text_pool: Option<usize>,
}

impl Segment {
    pub fn count(&self) -> usize {
        (self.last_id - self.first_id) as usize + 1
    }

    fn data_end(&self) -> usize {
        self.data_offset + self.count() * self.record_size
    }
}

/// One record of a table.
#[derive(Debug, Clone, Copy)]
pub struct Record<'a> {
    pub id: u32,
    pub segment: usize,
    pub bytes: &'a [u8],
}

impl Record<'_> {
    pub fn u16_at(&self, offset: usize) -> Option<u16> {
        let b = self.bytes.get(offset..offset + 2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }
}

#[derive(Debug, Clone)]
pub struct KernelTable {
    data: Vec<u8>,
    layout: Layout,
    segments: Vec<Segment>,
}

fn u16_le(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn u32_le(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

impl KernelTable {
    pub fn parse(data: Vec<u8>) -> Result<KernelTable, KernelError> {
        if data.len() >= FFX2_HEADER_LEN
            && data[8..0x10].iter().all(|&b| b == 0)
            && u32_le(&data, 0x1C) as usize == FFX2_HEADER_LEN
        {
            return Self::parse_ffx2(data);
        }
        Self::parse_ffx(data)
    }

    fn parse_ffx2(data: Vec<u8>) -> Result<KernelTable, KernelError> {
        let last_id = u32_le(&data, 0x10);
        let record_size = u32_le(&data, 0x14) as usize;
        let data_size = u32_le(&data, 0x18) as usize;
        let data_offset = FFX2_HEADER_LEN;
        let expected = (last_id as usize).checked_add(1).and_then(|n| n.checked_mul(record_size));
        if record_size == 0 || expected != Some(data_size) || data_offset + data_size > data.len() {
            return Err(KernelError::NotATable);
        }
        let pool = data_offset + data_size;
        let segment = Segment {
            first_id: 0,
            last_id,
            record_size,
            data_offset,
            text_pool: (pool < data.len()).then_some(pool),
        };
        Ok(KernelTable { data, layout: Layout::Ffx2, segments: vec![segment] })
    }

    fn parse_ffx(data: Vec<u8>) -> Result<KernelTable, KernelError> {
        if data.len() < 8 {
            return Err(KernelError::NotATable);
        }
        let n = i16::from_le_bytes([data[0], data[1]]);
        if !(1..=MAX_SEGMENTS).contains(&n) {
            return Err(KernelError::NotATable);
        }
        let mut segments = Vec::with_capacity(n as usize);
        for i in 0..n as usize {
            let at = 8 + 12 * i;
            if at + 12 > data.len() {
                return Err(KernelError::NotATable);
            }
            let first_id = u32::from(u16_le(&data, at));
            let last_id = u32::from(u16_le(&data, at + 2));
            let record_size = usize::from(u16_le(&data, at + 4));
            let text_rel = usize::from(u16_le(&data, at + 6));
            let data_offset = u32_le(&data, at + 8) as usize;
            if last_id < first_id || record_size == 0 {
                return Err(KernelError::NotATable);
            }
            let mut segment = Segment { first_id, last_id, record_size, data_offset, text_pool: None };
            if segment.data_end() > data.len() {
                return Err(KernelError::BadSegment(i));
            }
            // The pool follows the records; a smaller offset means the table has no text.
            let pool = data_offset + text_rel;
            if text_rel >= segment.count() * record_size && pool < data.len() {
                segment.text_pool = Some(pool);
            }
            segments.push(segment);
        }
        Ok(KernelTable { data, layout: Layout::Ffx, segments })
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Record size of the first segment (all segments seen so far share it).
    pub fn record_size(&self) -> usize {
        self.segments[0].record_size
    }

    pub fn len(&self) -> usize {
        self.segments.iter().map(Segment::count).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn has_text(&self) -> bool {
        self.segments.iter().any(|s| s.text_pool.is_some())
    }

    pub fn records(&self) -> impl Iterator<Item = Record<'_>> {
        self.segments.iter().enumerate().flat_map(move |(si, s)| {
            (0..s.count()).map(move |k| {
                let start = s.data_offset + k * s.record_size;
                Record {
                    id: s.first_id + k as u32,
                    segment: si,
                    bytes: &self.data[start..start + s.record_size],
                }
            })
        })
    }

    pub fn record(&self, id: u32) -> Option<Record<'_>> {
        let (si, s) =
            self.segments.iter().enumerate().find(|(_, s)| (s.first_id..=s.last_id).contains(&id))?;
        let start = s.data_offset + (id - s.first_id) as usize * s.record_size;
        Some(Record { id, segment: si, bytes: &self.data[start..start + s.record_size] })
    }

    /// Bytes of the string a text field points at (without its NUL).
    pub fn text(&self, record: &Record<'_>, field_offset: usize) -> Option<&[u8]> {
        let pool = self.segments[record.segment].text_pool?;
        let rel = usize::from(record.u16_at(field_offset)?);
        crate::text::string_at(&self.data, pool + rel)
    }

    /// Offsets of `u16` fields that point at string starts for (nearly) every record. The files carry no field
    /// descriptions, so this is inferred from the data.
    pub fn text_fields(&self) -> Vec<usize> {
        if !self.has_text() {
            return Vec::new();
        }
        let records: Vec<Record<'_>> = self.records().collect();
        let size = self.records().map(|r| r.bytes.len()).min().unwrap_or(0);
        let is_string_start = |r: &Record<'_>, v: u16| {
            let Some(pool) = self.segments[r.segment].text_pool else { return false };
            let at = pool + usize::from(v);
            at < self.data.len() && (v == 0 || self.data[at - 1] == 0)
        };
        let mut out = Vec::new();
        for offset in (0..size.saturating_sub(1)).step_by(2) {
            let mut ok = 0;
            let mut distinct = std::collections::HashSet::new();
            for r in &records {
                let v = r.u16_at(offset).unwrap_or(0);
                distinct.insert(v);
                if is_string_start(r, v) {
                    ok += 1;
                }
            }
            let n = records.len();
            if ok * 100 >= n * 95 && distinct.len() > (n / 4).max(2) {
                out.push(offset);
            }
        }
        out
    }
}

/// Folders that hold tables, as base (Japanese) paths. Language folders override files by name.
pub fn table_dirs(game: Game) -> &'static [&'static str] {
    match game {
        Game::Ffx => &["ffx_ps2/ffx/master/jppc/battle/kernel/"],
        Game::Ffx2 => {
            &["ffx_ps2/ffx2/master/jppc/battle/kernel/", "ffx_ps2/ffx2/master/jppc/lastmiss/kernel/"]
        }
    }
}

/// A table file, named by its base path; read it with [`Vfs::read_localized`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TableFile {
    pub base_path: String,
    pub name: String,
    /// Folder label for display ("battle", "lastmiss").
    pub group: String,
}

/// Every table file (`*.bin`) of a game, from the base folder and all language folders. The folders also hold
/// development leftovers (headers, lists), which are skipped.
pub fn list_tables(vfs: &Vfs, game: Game) -> Vec<TableFile> {
    let mut found = BTreeMap::new();
    for dir in table_dirs(game) {
        // "…/master/jppc/<group>/kernel/" → prefix "…/master/" and suffix "/<group>/kernel/".
        let Some(split) = dir.find("/jppc/") else { continue };
        let (master, suffix) = (&dir[..split + 1], &dir[split + 5..]);
        let group = suffix.trim_matches('/').split('/').next().unwrap_or("").to_owned();
        for (_, archive) in vfs.archives() {
            for e in archive.entries() {
                let Some(rest) = e.path.strip_prefix(master) else { continue };
                let Some((folder, file)) = rest.split_once('/') else { continue };
                let Some(name) = file.strip_prefix(&suffix[1..]) else { continue };
                if !folder.ends_with("pc") || name.contains('/') || !name.ends_with(".bin") {
                    continue;
                }
                found.entry(format!("{dir}{name}")).or_insert_with(|| TableFile {
                    base_path: format!("{dir}{name}"),
                    name: name.to_owned(),
                    group: group.clone(),
                });
            }
        }
    }
    found.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// An FFX-style table: one segment of `records` (4 bytes each: u16 text offset + u16 value) and a pool.
    fn ffx_table(first: u16, strings: &[&[u8]]) -> Vec<u8> {
        let size = 4u16;
        let n = strings.len() as u16;
        let data_off = 0x14u32;
        let text_rel = n * size;
        let mut d = vec![0u8; data_off as usize];
        d[0..2].copy_from_slice(&1i16.to_le_bytes());
        d[8..10].copy_from_slice(&first.to_le_bytes());
        d[10..12].copy_from_slice(&(first + n - 1).to_le_bytes());
        d[12..14].copy_from_slice(&size.to_le_bytes());
        d[14..16].copy_from_slice(&text_rel.to_le_bytes());
        d[16..20].copy_from_slice(&data_off.to_le_bytes());
        let mut pool = Vec::new();
        for (i, s) in strings.iter().enumerate() {
            d.extend_from_slice(&(pool.len() as u16).to_le_bytes());
            d.extend_from_slice(&(i as u16 * 7).to_le_bytes());
            pool.extend_from_slice(s);
            pool.push(0);
        }
        d.extend(pool);
        d
    }

    fn ffx2_table(strings: &[&[u8]]) -> Vec<u8> {
        let size = 8u32;
        let n = strings.len() as u32;
        let mut d = vec![0u8; FFX2_HEADER_LEN];
        d[0x10..0x14].copy_from_slice(&(n - 1).to_le_bytes());
        d[0x14..0x18].copy_from_slice(&size.to_le_bytes());
        d[0x18..0x1C].copy_from_slice(&(n * size).to_le_bytes());
        d[0x1C..0x20].copy_from_slice(&(FFX2_HEADER_LEN as u32).to_le_bytes());
        let mut pool = Vec::new();
        for s in strings {
            d.extend_from_slice(&(pool.len() as u16).to_le_bytes());
            d.extend_from_slice(&[0xAA; 6]);
            pool.extend_from_slice(s);
            pool.push(0);
        }
        d.extend(pool);
        d
    }

    const WORDS: [&[u8]; 6] = [b"\x31\x32", b"\x33", b"\x34\x35\x36", b"\x37", b"\x38\x39", b"\x3A"];

    #[test]
    fn ffx_layout_records_and_text() {
        let t = KernelTable::parse(ffx_table(0x65, &WORDS)).unwrap();
        assert_eq!(t.layout(), Layout::Ffx);
        assert_eq!(t.len(), 6);
        assert_eq!(t.record_size(), 4);
        let ids: Vec<u32> = t.records().map(|r| r.id).collect();
        assert_eq!(ids, (0x65..0x6B).collect::<Vec<_>>());
        let r = t.record(0x67).unwrap();
        assert_eq!(t.text(&r, 0).unwrap(), b"\x34\x35\x36");
        assert!(t.record(0x64).is_none());
        assert_eq!(t.text_fields(), vec![0]);
    }

    #[test]
    fn ffx2_layout_records_and_text() {
        let t = KernelTable::parse(ffx2_table(&WORDS)).unwrap();
        assert_eq!(t.layout(), Layout::Ffx2);
        assert_eq!(t.len(), 6);
        assert_eq!(t.text(&t.record(5).unwrap(), 0).unwrap(), b"\x3A");
        assert_eq!(t.text_fields(), vec![0]);
    }

    #[test]
    fn rejects_non_tables() {
        assert_eq!(KernelTable::parse(vec![]).unwrap_err(), KernelError::NotATable);
        assert_eq!(KernelTable::parse(vec![0x40; 64]).unwrap_err(), KernelError::NotATable);
        let mut d = ffx_table(0, &WORDS);
        d.truncate(0x18);
        assert_eq!(KernelTable::parse(d).unwrap_err(), KernelError::BadSegment(0));
    }

    proptest! {
        #[test]
        fn parse_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            if let Ok(t) = KernelTable::parse(bytes) {
                let fields = t.text_fields();
                for r in t.records() {
                    for &f in &fields {
                        let _ = t.text(&r, f);
                    }
                }
            }
        }

        #[test]
        fn mutated_tables_never_panic(flips in proptest::collection::vec((0usize..64, any::<u8>()), 1..6)) {
            let mut d = ffx_table(3, &WORDS);
            for (at, v) in flips {
                let n = d.len();
                d[at % n] = v;
            }
            if let Ok(t) = KernelTable::parse(d) {
                let fields = t.text_fields();
                for r in t.records() {
                    for &f in &fields {
                        let _ = t.text(&r, f);
                    }
                }
            }
        }
    }
}
