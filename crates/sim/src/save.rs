//! Saves. The game state scripts read and write by offset is the **save image** (global block 0 of the script
//! machine). Aerugo keeps it in its own container with the place to resume and a checksum, and can import the
//! image from a save of the Steam release.

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

    /// The save image of a Steam release save file (FFX): a 0x40-byte header, then the image.
    pub fn import_steam(game: Game, d: &[u8]) -> Result<SaveGame, SaveError> {
        let need = STEAM_HEADER_SIZE + FFX_IMAGE_SIZE;
        if d.len() < need {
            return Err(SaveError::SteamSize(d.len(), need));
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
