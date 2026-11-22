//! The PC voice tables: "ML" maps (voice id → event name in `voice/voiceidmapper.txt`; event number → voice
//! project in `voice/<lang>/voicefevmapper.txt`) and the "SL" sync list (`syncdata/syncvoicelist.txt`: frames a
//! voice's standby, start or sync wait takes instead of following playback). The files are binary despite their
//! names.

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// An "ML" map: a count, then (key, name offset) pairs; names are NUL-terminated.
pub fn parse_ml(d: &[u8]) -> Option<Vec<(u32, String)>> {
    if d.get(0..4)? != b"ML\0\0" {
        return None;
    }
    let n = u32_at(d, 4)? as usize;
    (0..n)
        .map(|i| {
            let key = u32_at(d, 8 + 8 * i)?;
            let s = d.get(u32_at(d, 12 + 8 * i)? as usize..)?;
            let end = s.iter().position(|&b| b == 0)?;
            Some((key, String::from_utf8_lossy(&s[..end]).into_owned()))
        })
        .collect()
}

/// One sync list entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncEntry {
    /// 1 standby, 2 start, 3 sync (4 also appears).
    pub stage: u32,
    pub event: u32,
    pub voice: u32,
    pub frames: i32,
}

/// The "SL" sync list: a count, then 16-byte records (stage, event, voice, frames).
pub fn parse_sync(d: &[u8]) -> Option<Vec<SyncEntry>> {
    if d.get(0..4)? != b"SL\0\0" {
        return None;
    }
    let n = u32_at(d, 4)? as usize;
    (0..n)
        .map(|i| {
            let at = 8 + 16 * i;
            Some(SyncEntry {
                stage: u32_at(d, at)?,
                event: u32_at(d, at + 4)?,
                voice: u32_at(d, at + 8)?,
                frames: u32_at(d, at + 12)? as i32,
            })
        })
        .collect()
}

/// The frames the sync list gives a voice's stage (the last matching entry; 0 when none).
pub fn sync_frames(list: &[SyncEntry], voice: u32, stage: u32) -> i32 {
    list.iter().rev().find(|e| e.voice == voice && e.stage == stage).map_or(0, |e| e.frames)
}

/// A project slot from two name characters as the game computes it: `b + (a·5 − 0x108)·2` (the two decimal
/// digits "ab" as a number).
pub fn slot(a: u8, b: u8) -> i32 {
    i32::from(b as i8) + (i32::from(a as i8) * 5 - 0x108) * 2
}

/// A voice's slot: its event name's first two characters.
pub fn voice_slot(name: &str) -> Option<i32> {
    let b = name.as_bytes();
    Some(slot(*b.first()?, *b.get(1)?))
}

/// A project's slot: its name's last two characters.
pub fn project_slot(name: &str) -> Option<i32> {
    let b = name.as_bytes();
    let n = b.len();
    (n >= 2).then(|| slot(b[n - 2], b[n - 1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_the_two_digits() {
        assert_eq!(voice_slot("210101td"), Some(21));
        assert_eq!(project_slot("ffx_us_voice01"), Some(1));
        assert_eq!(project_slot("ffx_us_voice270"), Some(70));
    }

    #[test]
    fn sync_list_takes_the_last_match() {
        let mut d = b"SL\0\0".to_vec();
        d.extend_from_slice(&2u32.to_le_bytes());
        for frames in [5u32, 9] {
            for v in [3u32, 0x50, 0x2315_5504, frames] {
                d.extend_from_slice(&v.to_le_bytes());
            }
        }
        let l = parse_sync(&d).unwrap();
        assert_eq!(sync_frames(&l, 0x2315_5504, 3), 9);
        assert_eq!(sync_frames(&l, 0x2315_5504, 2), 0);
    }
}
