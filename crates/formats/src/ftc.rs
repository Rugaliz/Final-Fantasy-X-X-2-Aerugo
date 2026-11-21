//! PS2 font descriptions (`*.ftc`, magic `FTCX`): the font slot a file fills, the PS2 size of its glyph image,
//! and one width per glyph. The HD remaster keeps the glyph images as textures (`base_ftc/d3d11/font_0_<page>`
//! and `shadow_0_<page>`) and reads only these descriptions. See `docs/formats/text.md`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ftc {
    /// Font slot (0 the Japanese base font, 4 the base font of the other languages, others for extra pages).
    pub slot: u16,
    /// Size of the glyph image on the PS2 (texture coordinates are texel positions divided by it).
    pub image_w: u16,
    pub image_h: u16,
    /// Glyph widths in quarter texels, by glyph index.
    pub widths: Vec<u8>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FtcError {
    #[error("not a font description")]
    NotFtc,
    #[error("font description version {0} predates the width table")]
    OldVersion(u16),
    #[error("file ends early")]
    Truncated,
}

/// Glyph cells: 14 × 18 texels, glyphs in pairs across nine columns per row; even glyphs on page 0, odd on page 1.
pub const CELL_W: u32 = 14;
pub const CELL_H: u32 = 18;
const PAIRS_PER_ROW: u32 = 9;

/// Where glyph `index` sits: page, and the texel position of its cell in that page.
pub fn cell(index: u32) -> (u8, u32, u32) {
    let row = index / (2 * PAIRS_PER_ROW);
    let col = (index % (2 * PAIRS_PER_ROW)) / 2;
    ((index & 1) as u8, col * CELL_W, row * CELL_H)
}

impl Ftc {
    pub fn parse(d: &[u8]) -> Result<Ftc, FtcError> {
        if d.get(0..4) != Some(b"FTCX") {
            return Err(FtcError::NotFtc);
        }
        let u16_at =
            |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(FtcError::Truncated);
        let u32_at = |o: usize| {
            d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(FtcError::Truncated)
        };
        let version = u16_at(4)?;
        if version < 200 {
            return Err(FtcError::OldVersion(version));
        }
        let count = u32_at(0x10)? as usize;
        let at = u32_at(0x30)? as usize;
        let widths = d.get(at..at + count).ok_or(FtcError::Truncated)?.to_vec();
        Ok(Ftc { slot: u16_at(8)?, image_w: u16_at(0x28)?, image_h: u16_at(0x2a)?, widths })
    }

    /// A glyph's width in texels.
    pub fn width(&self, index: u32) -> Option<f32> {
        self.widths.get(index as usize).map(|&w| f32::from(w) * 0.25)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(version: u16, widths: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 0x40];
        d[0..4].copy_from_slice(b"FTCX");
        d[4..6].copy_from_slice(&version.to_le_bytes());
        d[8..10].copy_from_slice(&4u16.to_le_bytes());
        d[0x10..0x14].copy_from_slice(&(widths.len() as u32).to_le_bytes());
        d[0x28..0x2a].copy_from_slice(&128u16.to_le_bytes());
        d[0x2a..0x2c].copy_from_slice(&234u16.to_le_bytes());
        d[0x30..0x34].copy_from_slice(&0x40u32.to_le_bytes());
        d.extend_from_slice(widths);
        d
    }

    #[test]
    fn parses_header_and_widths() {
        let f = Ftc::parse(&file(200, &[0x24, 0x23])).unwrap();
        assert_eq!((f.slot, f.image_w, f.image_h), (4, 128, 234));
        assert_eq!((f.width(1), f.width(2)), (Some(8.75), None));
        assert_eq!(Ftc::parse(&file(100, &[1])), Err(FtcError::OldVersion(100)));
        assert_eq!(Ftc::parse(b"FTC"), Err(FtcError::NotFtc));
        let mut cut = file(200, &[1, 2, 3]);
        cut.truncate(0x41);
        assert_eq!(Ftc::parse(&cut), Err(FtcError::Truncated));
    }

    #[test]
    fn cells() {
        assert_eq!(cell(0), (0, 0, 0));
        assert_eq!(cell(1), (1, 0, 0));
        assert_eq!(cell(50), (0, 7 * 14, 2 * 18));
        assert_eq!(cell(85), (1, 6 * 14, 4 * 18));
    }

    /// Every base font description of the installed game parses.
    #[test]
    fn installed_fonts() {
        let Some(install) = aerugo_vfs::install_from_env() else { return };
        let mut vfs = aerugo_vfs::Vfs::new();
        if vfs.mount_install(&install).is_err() {
            return;
        }
        for lang in ["us", "jp"] {
            let Ok(d) = vfs.read(&format!("ffx_ps2/ffx/master/{lang}pc/menu/base.ftc")) else { continue };
            let f = Ftc::parse(&d).expect("base font");
            assert!(!f.widths.is_empty() && f.image_w > 0 && f.image_h > 0);
        }
    }
}
