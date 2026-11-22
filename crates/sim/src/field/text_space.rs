//! Text addresses: what natives hand scripts as the address of a text (013f, 0236) and message values take back.
//!
//! The original returns real pointers into its loaded text files. Here each file has its own base in a private
//! address range, so an address is the base plus the offset in the file, as the original's pointer arithmetic
//! makes it:
//! - the macro dictionary (`menu/macrodic.dcp`: 16 table offsets, each table u16 offsets to its strings),
//! - the event's text (u16 offsets every 8 bytes),
//! - an empty text (the original's static one, when the event has no text),
//! - the command table (`command.bin`: its names).

use std::sync::Arc;

pub const MACRODIC: u32 = 0x4000_0000;
pub const EVENT_TEXT: u32 = 0x5000_0000;
pub const EMPTY: u32 = 0x5fff_0000;
pub const COMMANDS: u32 = 0x6000_0000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextSpace {
    pub macrodic: Option<Arc<[u8]>>,
    pub event: Option<Arc<[u8]>>,
    pub commands: Option<Arc<[u8]>>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

impl TextSpace {
    /// 013f: the address of string `n` of dictionary table `table` (tables 19–34 are the
    /// file's 16; others read outside the table list and give none here).
    pub fn macro_text(&self, table: i32, n: i32) -> Option<u32> {
        let d = self.macrodic.as_deref()?;
        let k = usize::try_from(table.checked_sub(19)?).ok().filter(|&k| k < 16)?;
        let off = d.get(k * 4..k * 4 + 4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))?;
        if off < 1 {
            return None;
        }
        let base = off as usize;
        let entry = u16_at(d, base + usize::try_from(n).ok()? * 2)?;
        Some(MACRODIC + (base + usize::from(entry)) as u32)
    }

    /// msg_text: the address of event message `n` (its offset at n·8; offset 0 falls back to another text, which
    /// is not kept, so it gives the empty text).
    pub fn event_text(&self, n: i32) -> u32 {
        let Some(d) = self.event.as_deref() else { return EMPTY };
        match usize::try_from(n).ok().and_then(|n| u16_at(d, n * 8)) {
            Some(off) if off != 0 => EVENT_TEXT + u32::from(off),
            _ => EMPTY,
        }
    }

    /// The address of command `cmd`'s name (its record's text field 0, or 4 with the text variant).
    pub fn command_name(&self, table: &aerugo_formats::KernelTable, cmd: u32, variant: bool) -> u32 {
        let Some(r) = table.record(cmd & 0xfff) else { return EMPTY };
        let Some(pool) = table.segments()[r.segment].text_pool else { return EMPTY };
        let field = if variant { 4 } else { 0 };
        let at = r.bytes.get(field..field + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        COMMANDS + (pool + usize::from(at)) as u32
    }

    /// The bytes at an address, up to the end of its file (a text runs to its NUL).
    pub fn bytes_at(&self, addr: u32) -> Option<&[u8]> {
        let (file, at) = if (MACRODIC..EVENT_TEXT).contains(&addr) {
            (self.macrodic.as_deref()?, addr - MACRODIC)
        } else if (EVENT_TEXT..EMPTY).contains(&addr) {
            (self.event.as_deref()?, addr - EVENT_TEXT)
        } else if addr == EMPTY {
            return Some(&[0]);
        } else if (COMMANDS..COMMANDS + 0x0100_0000).contains(&addr) {
            (self.commands.as_deref()?, addr - COMMANDS)
        } else {
            return None;
        };
        file.get(at as usize..)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionary_addresses() {
        let mut d = vec![0u8; 0x60];
        d[7 * 4..7 * 4 + 4].copy_from_slice(&0x40i32.to_le_bytes());
        d[0x40..0x42].copy_from_slice(&4u16.to_le_bytes());
        d[0x42..0x44].copy_from_slice(&6u16.to_le_bytes());
        d[0x44] = b'A';
        let t = TextSpace { macrodic: Some(d.into()), ..Default::default() };
        assert_eq!(t.macro_text(26, 1), Some(MACRODIC + 0x46));
        assert_eq!(t.bytes_at(MACRODIC + 0x44).map(|b| b[0]), Some(b'A'));
        assert_eq!(t.macro_text(18, 0), None);
        assert_eq!(t.event_text(3), EMPTY);
    }
}
