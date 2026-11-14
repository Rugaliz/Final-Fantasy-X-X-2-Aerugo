//! FMOD Designer event projects (`*.fev`, RIFF `FEV `). The games use only simple events: each event plays
//! one sound definition with one waveform, named like the event, from the project's banks
//! (`<project>_bankNN.fsb` beside the `.fev`).

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FevError {
    #[error("not an FMOD event project")]
    NotFev,
    #[error("file ends early ({0})")]
    Truncated(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Project {
    pub name: String,
    /// Event groups (e.g. `voice`, `music`, `loop`, `nonloop`).
    pub groups: Vec<String>,
    /// Event names in project order (index = sound definition number).
    pub events: Vec<String>,
}

fn u32_at(d: &[u8], at: usize) -> Result<u32, FevError> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(FevError::Truncated("u32"))
}

/// Find a chunk by id inside RIFF data (descending into LIST chunks).
fn find_chunk<'a>(d: &'a [u8], mut p: usize, end: usize, id: &[u8; 4]) -> Option<&'a [u8]> {
    while p + 8 <= end {
        let cid = &d[p..p + 4];
        let size = u32_at(d, p + 4).ok()? as usize;
        if cid == b"LIST" {
            if let Some(found) = find_chunk(d, p + 12, (p + 8 + size).min(end), id) {
                return Some(found);
            }
        } else if cid == id {
            return d.get(p + 8..p + 8 + size);
        }
        p += 8 + size + (size & 1);
    }
    None
}

impl Project {
    pub fn parse(d: &[u8]) -> Result<Project, FevError> {
        if d.len() < 12 || &d[0..4] != b"RIFF" || &d[8..12] != b"FEV " {
            return Err(FevError::NotFev);
        }
        let end = d.len().min(u32_at(d, 4)? as usize + 8);
        let name = find_chunk(d, 12, end, b"PROP")
            .and_then(|p| {
                let len = u32_at(p, 0).ok()? as usize;
                let s = p.get(4..4 + len)?;
                Some(String::from_utf8_lossy(s.split(|&b| b == 0).next()?).into_owned())
            })
            .unwrap_or_default();
        let strr = find_chunk(d, 12, end, b"STRR").ok_or(FevError::Truncated("string table"))?;
        let count = u32_at(strr, 0)? as usize;
        if count > strr.len() / 4 {
            return Err(FevError::Truncated("string table"));
        }
        let base = 4 + 4 * count;
        let mut strings = Vec::with_capacity(count);
        for i in 0..count {
            let off = base + u32_at(strr, 4 + 4 * i)? as usize;
            let s = strr.get(off..).ok_or(FevError::Truncated("string"))?;
            let n = s.iter().position(|&b| b == 0).unwrap_or(s.len());
            strings.push(String::from_utf8_lossy(&s[..n]).into_owned());
        }
        // Sound definitions name their event and number: /__simpleevent_sounddef__/<event>_<n>.
        let mut defs: Vec<(usize, String)> = strings
            .iter()
            .filter_map(|s| {
                let rest = s.strip_prefix("/__simpleevent_sounddef__/")?;
                let (event, n) = rest.rsplit_once('_')?;
                Some((n.parse().ok()?, event.to_owned()))
            })
            .collect();
        defs.sort_by_key(|(n, _)| *n);
        let events: Vec<String> = defs.into_iter().map(|(_, e)| e).collect();
        let first_event = strings.iter().position(|s| events.first() == Some(s)).unwrap_or(strings.len());
        let groups = strings[..first_event].iter().filter(|s| !s.is_empty()).cloned().collect();
        Ok(Project { name, groups, events })
    }
}

/// Folder of a project's banks and the bank file prefix (`…/voice/us/ffx_us_voice01.fev` →
/// `…/voice/us/ffx_us_voice01_bank`).
pub fn bank_prefix(fev_path: &str) -> String {
    format!("{}_bank", fev_path.trim_end_matches(".fev"))
}

/// The same project in the other voice language or music set: swaps `/voice/us/` ↔ `/voice/jp/` (and the
/// `_us_`/`_jp_` in the file name) or `/music/` ↔ `/music_ps2/`.
pub fn variant_path(fev_path: &str, japanese_voices: bool, original_music: bool) -> String {
    let mut p = fev_path.to_owned();
    if p.contains("/voice/") || p.contains("/sfx/") {
        let (from, to) = if japanese_voices { ("us", "jp") } else { ("jp", "us") };
        p = p
            .replace(&format!("/{from}/"), &format!("/{to}/"))
            .replace(&format!("_{from}_"), &format!("_{to}_"));
    }
    if original_music {
        if p.contains("/music/") {
            p = p.replace("/music/", "/music_ps2/").replace("_music.fev", "_music_ps2.fev");
        }
    } else if p.contains("/music_ps2/") {
        p = p.replace("/music_ps2/", "/music/").replace("_music_ps2.fev", "_music.fev");
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fev(strings: &[&str]) -> Vec<u8> {
        let mut strr = Vec::new();
        strr.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        let mut pool = Vec::new();
        for s in strings {
            strr.extend_from_slice(&(pool.len() as u32).to_le_bytes());
            pool.extend_from_slice(s.as_bytes());
            pool.push(0);
        }
        strr.extend(pool);
        let mut body = Vec::new();
        body.extend_from_slice(b"STRR");
        body.extend_from_slice(&(strr.len() as u32).to_le_bytes());
        body.extend(&strr);
        if strr.len() % 2 == 1 {
            body.push(0);
        }
        let mut d = Vec::new();
        d.extend_from_slice(b"RIFF");
        d.extend_from_slice(&(4 + body.len() as u32).to_le_bytes());
        d.extend_from_slice(b"FEV ");
        d.extend(body);
        d
    }

    #[test]
    fn events_follow_their_sound_definitions() {
        let d = fev(&[
            "",
            "loop",
            "nonloop",
            "b",
            "a",
            "/__simpleevent_sounddef__/b_0",
            "/__simpleevent_sounddef__/a_1",
        ]);
        let p = Project::parse(&d).unwrap();
        assert_eq!(p.events, vec!["b", "a"]);
        assert_eq!(p.groups, vec!["loop", "nonloop"]);
        assert_eq!(Project::parse(b"RIFF\0\0\0\0WAVE").unwrap_err(), FevError::NotFev);
    }

    #[test]
    fn variants() {
        let v = "ffx_data/gamedata/ps3data/sound_pc/voice/us/ffx_us_voice01.fev";
        assert_eq!(
            variant_path(v, true, false),
            "ffx_data/gamedata/ps3data/sound_pc/voice/jp/ffx_jp_voice01.fev"
        );
        let m = "ffx_data/gamedata/ps3data/sound_pc/music/ffx_music.fev";
        assert_eq!(
            variant_path(m, false, true),
            "ffx_data/gamedata/ps3data/sound_pc/music_ps2/ffx_music_ps2.fev"
        );
        assert_eq!(variant_path(&variant_path(m, false, true), false, false), m);
        assert_eq!(bank_prefix(v), "ffx_data/gamedata/ps3data/sound_pc/voice/us/ffx_us_voice01_bank");
    }
}
