//! Saves. The game state scripts read and write by offset is the **save image** (global block 0 of the script
//! machine). Aerugo keeps it in its own container with the place to resume and a checksum, and reads and writes
//! the remaster's own save files (`FINAL FANTASY X/ffx_NNN`: a 0x40-byte header, then the image), as
//! TkMenuPreSave writes them and the load checks them.

use aerugo_formats::Game;

/// Size of FFX's save image.
pub const FFX_IMAGE_SIZE: usize = 0x68C0;
/// Bytes before the image in a Steam save file.
pub const STEAM_HEADER_SIZE: usize = 0x40;

const MAGIC: &[u8; 8] = b"AERUGOSV";
const VERSION: u16 = 1;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SaveError {
    #[error("not an Aerugo save")]
    NotASave,
    #[error("save version {0} is newer than this build understands")]
    Version(u16),
    #[error("the save is damaged (checksum mismatch)")]
    Checksum,
    #[error("the save is cut short")]
    Truncated,
    #[error("a Steam save is {0} bytes; at least {1} expected")]
    SteamSize(usize, usize),
    #[error("the Steam save's checksum does not match (header {0:#06x}, image {1:#x}, computed {2:#06x})")]
    SteamChecksum(u16, u32, u16),
}

/// The remaster's save checksum: CRC-16, polynomial 0x1021 MSB first, start 0xffff, inverted at the end;
/// the table is built for entries 0..=0xfe only (entry 0xff stays 0, as the original's loop bound leaves it).
pub fn steam_crc(data: &[u8]) -> u16 {
    let mut t = [0u16; 256];
    for (i, e) in t.iter_mut().enumerate().take(0xff) {
        let mut v = (i as u32) << 8;
        for _ in 0..8 {
            v = if v & 0x8000 != 0 { (v << 1) ^ 0x1021 } else { v << 1 };
        }
        *e = v as u16;
    }
    let mut c: u16 = 0xffff;
    for &b in data {
        c = (c << 8) ^ t[usize::from((c >> 8) as u8 ^ b)];
    }
    !c
}

/// The checksum word in the image (+0x64b4, a u32 holding the CRC) and the span the CRC covers (the image up to
/// and including that word, which counts as 0), as the save writes and the load checks it.
const STEAM_CRC_AT: usize = 0x64B4;
const STEAM_CRC_LEN: usize = 0x64B8;

fn steam_image_crc(image: &[u8]) -> u16 {
    let mut img = image[..STEAM_CRC_LEN].to_vec();
    img[STEAM_CRC_AT..STEAM_CRC_LEN].fill(0);
    steam_crc(&img)
}

/// The header of a remaster save for `image` (already with its +0x2a set): the save counter (the
/// session's, capped at 0x80000000), the menu's party list (the three active slots, then the 17 reserve
/// slots, neither 0xff nor 7, then characters 0–6 that joined (+0x55f8 + c·0x94 bit 4) not listed yet; only the
/// first before story 0x10; 0xff after it up to the seventh), +4 = 1, the language type, +5..+7 = the active
/// slots (from story 0x10; 6 shows as 7 before story 300), the u32 at +0xbc, the play time word +0x3d48, the
/// place name (+0xd4 when +0xd6 is set, else +0xe), 0x100, the leader (+0xd1), 0 (the text variant on PC), the
/// first character's name, and the checksum.
pub fn steam_header(image: &[u8], counter: u32, language_type: u8) -> [u8; STEAM_HEADER_SIZE] {
    let u16_at = |a: usize| u16::from_le_bytes([image[a], image[a + 1]]);
    let mut h = [0u8; STEAM_HEADER_SIZE];
    h[0..4].copy_from_slice(&counter.min(0x8000_0000).to_le_bytes());
    let story = u16_at(0xbec);
    let mut list = Vec::new();
    let mut seen = 0u32;
    for &c in &image[0x3d58..0x3d6c] {
        if c != 0xff && c != 7 {
            list.push(c);
            seen |= 1 << (c & 31);
        }
    }
    for i in 0..8u8 {
        if i != 7 && image[0x55f8 + usize::from(i) * 0x94] >> 4 & 1 != 0 && seen >> i & 1 == 0 {
            list.push(i);
        }
    }
    let mut n = list.len();
    if story <= 0xf && n > 1 {
        n = 1;
    }
    for (i, &c) in list.iter().take(n).enumerate() {
        if let Some(b) = h.get_mut(5 + i) {
            *b = c;
        }
    }
    for k in n..7 {
        h[5 + k] = 0xff;
    }
    h[0xc] = language_type;
    h[5] = image[0x3d58];
    if story > 0xf {
        h[6] = image[0x3d59];
        h[7] = image[0x3d5a];
        if story < 300 {
            for k in [5, 6] {
                if h[k] == 6 {
                    h[k] = 7;
                }
            }
        }
    }
    h[4] = 1;
    h[0x10..0x14].copy_from_slice(&image[0xbc..0xc0]);
    h[0x14..0x18].copy_from_slice(&image[0x3d48..0x3d4c]);
    let place = if image[0xd6] != 0 { 0xd4 } else { 0xe };
    h[0x18..0x1a].copy_from_slice(&image[place..place + 2]);
    h[0x1c..0x1e].copy_from_slice(&0x100u16.to_le_bytes());
    h[0x1e] = image[0xd1];
    h[0x1f] = 0;
    let name = &image[crate::field::NAMES..crate::field::NAMES + crate::field::NAME_SLOT];
    let len = name.iter().position(|&b| b == 0).map_or(name.len(), |p| p + 1).min(0x20);
    h[0x20..0x20 + len].copy_from_slice(&name[..len]);
    h[0x1a..0x1c].copy_from_slice(&steam_image_crc(image).to_le_bytes());
    h
}

/// One saved game.
#[derive(Debug, Clone, PartialEq)]
pub struct SaveGame {
    pub game: Game,
    /// Event script to resume in (archive path), when known.
    pub event: Option<String>,
    /// Where the player stood.
    pub position: Option<[f32; 3]>,
    /// Game time in ticks.
    pub ticks: u64,
    /// The save image.
    pub image: Vec<u8>,
}

/// CRC-32 (IEEE), bitwise.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

struct Reader<'a> {
    d: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], SaveError> {
        let s = self.d.get(self.at..self.at + n).ok_or(SaveError::Truncated)?;
        self.at += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, SaveError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, SaveError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, SaveError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, SaveError> {
        Ok(u64::from(self.u32()?) | (u64::from(self.u32()?) << 32))
    }
    fn f32(&mut self) -> Result<f32, SaveError> {
        Ok(f32::from_bits(self.u32()?))
    }
}

impl SaveGame {
    /// The container: magic, version, game, flags, ticks, position, event path, image, CRC-32 of all before it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.image.len() + 128);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.push(match self.game {
            Game::Ffx => 0,
            Game::Ffx2 => 1,
        });
        out.push(u8::from(self.position.is_some()));
        out.extend_from_slice(&self.ticks.to_le_bytes());
        for v in self.position.unwrap_or([0.0; 3]) {
            out.extend_from_slice(&v.to_le_bytes());
        }
        let event = self.event.as_deref().unwrap_or("").as_bytes();
        out.extend_from_slice(&(event.len().min(0xFFFF) as u16).to_le_bytes());
        out.extend_from_slice(&event[..event.len().min(0xFFFF)]);
        out.extend_from_slice(&(self.image.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.image);
        let crc = crc32(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn from_bytes(d: &[u8]) -> Result<SaveGame, SaveError> {
        if !d.starts_with(MAGIC) {
            return Err(SaveError::NotASave);
        }
        if d.len() < MAGIC.len() + 4 {
            return Err(SaveError::Truncated);
        }
        let (body, tail) = d.split_at(d.len() - 4);
        if crc32(body) != u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) {
            return Err(SaveError::Checksum);
        }
        let mut r = Reader { d: body, at: MAGIC.len() };
        let version = r.u16()?;
        if version > VERSION {
            return Err(SaveError::Version(version));
        }
        let game = if r.u8()? == 1 { Game::Ffx2 } else { Game::Ffx };
        let has_position = r.u8()? != 0;
        let ticks = r.u64()?;
        let position = [r.f32()?, r.f32()?, r.f32()?];
        let n = usize::from(r.u16()?);
        let event = String::from_utf8_lossy(r.take(n)?).into_owned();
        let n = r.u32()? as usize;
        let image = r.take(n)?.to_vec();
        Ok(SaveGame {
            game,
            event: (!event.is_empty()).then_some(event),
            position: has_position.then_some(position),
            ticks,
            image,
        })
    }

    /// The save image of a Steam release save file (FFX): a 0x40-byte header, then the image. The checksum is
    /// checked as the load does (the header word and the image's +0x64b4 equal, and the CRC of the image up to it).
    pub fn import_steam(game: Game, d: &[u8]) -> Result<SaveGame, SaveError> {
        let need = STEAM_HEADER_SIZE + FFX_IMAGE_SIZE;
        if d.len() < need {
            return Err(SaveError::SteamSize(d.len(), need));
        }
        let image = &d[STEAM_HEADER_SIZE..need];
        let head = u16::from_le_bytes([d[0x1a], d[0x1b]]);
        let word = u32::from_le_bytes(image[STEAM_CRC_AT..STEAM_CRC_LEN].try_into().unwrap_or_default());
        let computed = steam_image_crc(image);
        if u32::from(head) != word || computed != head {
            return Err(SaveError::SteamChecksum(head, word, computed));
        }
        Ok(SaveGame {
            game,
            event: None,
            position: None,
            ticks: 0,
            image: d[STEAM_HEADER_SIZE..need].to_vec(),
        })
    }
}

impl SaveGame {
    /// The remaster's save file for this save: the image's +0x2a set (the load
    /// resumes at the saved place), its checksum word, and the header before it.
    pub fn export_steam(&self, counter: u32, language_type: u8) -> Vec<u8> {
        let mut image = self.image.clone();
        image.resize(FFX_IMAGE_SIZE, 0);
        image[0x2a] = 1;
        image[STEAM_CRC_AT..STEAM_CRC_LEN].fill(0);
        let crc = steam_image_crc(&image);
        image[STEAM_CRC_AT..STEAM_CRC_LEN].copy_from_slice(&u32::from(crc).to_le_bytes());
        let mut out = steam_header(&image, counter, language_type).to_vec();
        out.extend_from_slice(&image);
        out
    }
}

/// Write a file through a temporary name so a crash never leaves half a save.
pub fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SaveGame {
        SaveGame {
            game: Game::Ffx,
            event: Some("ffx_ps2/ffx/master/jppc/event/obj/bs/bsyt0000/bsyt0000.ebp".into()),
            position: Some([1.0, -2.5, 300.0]),
            ticks: 123_456,
            image: (0..FFX_IMAGE_SIZE).map(|i| (i * 7) as u8).collect(),
        }
    }

    #[test]
    fn round_trip() {
        let s = sample();
        assert_eq!(SaveGame::from_bytes(&s.to_bytes()), Ok(s));
        let empty = SaveGame { event: None, position: None, ..sample() };
        assert_eq!(SaveGame::from_bytes(&empty.to_bytes()), Ok(empty));
    }

    #[test]
    fn damage_is_detected() {
        let mut b = sample().to_bytes();
        b[200] ^= 1;
        assert_eq!(SaveGame::from_bytes(&b), Err(SaveError::Checksum));
        assert_eq!(SaveGame::from_bytes(b"nonsense"), Err(SaveError::NotASave));
        let b = sample().to_bytes();
        assert!(SaveGame::from_bytes(&b[..b.len() / 2]).is_err());
    }

    #[test]
    fn steam_import_takes_the_image() {
        let mut file = vec![0xAAu8; STEAM_HEADER_SIZE];
        file.extend((0..FFX_IMAGE_SIZE).map(|i| i as u8));
        let s = SaveGame::import_steam(Game::Ffx, &file).unwrap();
        assert_eq!(s.image.len(), FFX_IMAGE_SIZE);
        assert_eq!(s.image[1], 1);
        assert!(matches!(SaveGame::import_steam(Game::Ffx, &file[..100]), Err(SaveError::SteamSize(..))));
    }

    #[test]
    fn crc_matches_the_standard() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
