//! PS2 menu shapes: the sprite pieces that message windows, choice cursors and menu frames are drawn
//! from. Each shape is a list of rectangles cut from one bank image (`meswin`, `battle` or `icon`).
//!
//! The shape banks exist only inside the game executables (the PC `FFX.exe` and the PS2 disc's boot
//! executable carry identical copies), not in any data file, so they are read from the user's
//! executable at run time and found by their layout. See `docs/formats/shape.md`.

/// One rectangle of a shape: `w`×`h` texels at (`u`, `v`) of the bank image, drawn at (`dx`, `dy`) from
/// the shape's position. A mirrored piece swaps its left/right (or top/bottom) edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Piece {
    pub u: u8,
    pub v: u8,
    pub dx: i8,
    pub dy: i8,
    pub w: u16,
    pub h: u16,
    pub mirror_x: bool,
    pub mirror_y: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Shape {
    pub pieces: Vec<Piece>,
}

/// One shape table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bank {
    pub shapes: Vec<Shape>,
}

/// The three menu banks, by the image their pieces are cut from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuShapes {
    /// `meswin` (256×256 on the PS2): window frames, cursors, message parts.
    pub meswin: Bank,
    /// `battle` (256×256).
    pub battle: Bank,
    /// `icon` (256×128).
    pub icon: Bank,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ShapeError {
    #[error("no menu shape banks in this executable")]
    NotFound,
}

/// Shape counts of the menu banks, in their order in the executables.
const MENU_BANK_SIZES: [usize; 3] = [85, 110, 25];
/// Largest gap (alignment padding) between consecutive banks.
const MAX_PADDING: usize = 16;

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A shape table at `at`, if the bytes there have its layout: a u32 shape count, that many u32 offsets
/// (from the table start; the first right after the offset list), and at each offset a u32 piece count
/// followed by 12-byte pieces, the shapes packed back to back. Returns the bank and its byte length.
fn bank_at(d: &[u8], at: usize) -> Option<(Bank, usize)> {
    let n = u32_at(d, at)? as usize;
    if !(2..=1000).contains(&n) {
        return None;
    }
    let mut next = 4 + 4 * n;
    if u32_at(d, at + 4)? as usize != next {
        return None;
    }
    let mut shapes = Vec::with_capacity(n);
    for i in 0..n {
        if u32_at(d, at + 4 + 4 * i)? as usize != next {
            return None;
        }
        let count = u32_at(d, at + next)? as usize;
        if !(1..=64).contains(&count) {
            return None;
        }
        let rec = d.get(at + next + 4..at + next + 4 + 12 * count)?;
        let pieces = rec
            .chunks_exact(12)
            .map(|p| Piece {
                u: p[0],
                v: p[1],
                dx: p[2] as i8,
                dy: p[3] as i8,
                w: u16::from_le_bytes([p[4], p[5]]),
                h: u16::from_le_bytes([p[6], p[7]]),
                mirror_x: u16::from_le_bytes([p[8], p[9]]) == 1,
                mirror_y: u16::from_le_bytes([p[10], p[11]]) == 1,
            })
            .collect();
        shapes.push(Shape { pieces });
        next += 4 + 12 * count;
    }
    Some((Bank { shapes }, next))
}

/// Every shape table in an executable's bytes, with its offset, in file order.
pub fn scan(d: &[u8]) -> Vec<(usize, Bank, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 8 <= d.len() {
        if let Some((bank, len)) = bank_at(d, at) {
            found.push((at, bank, len));
            at += len.next_multiple_of(4);
        } else {
            at += 4;
        }
    }
    found
}

impl MenuShapes {
    /// Find the menu banks in an executable (PC PE or PS2 ELF; raw file bytes): three consecutive shape
    /// tables of 85, 110 and 25 shapes.
    pub fn from_executable(d: &[u8]) -> Result<MenuShapes, ShapeError> {
        let banks = scan(d);
        for w in banks.windows(3) {
            let sizes_match = w.iter().zip(MENU_BANK_SIZES).all(|((_, b, _), n)| b.shapes.len() == n);
            let adjacent = w.windows(2).all(|p| {
                let end = p[0].0 + p[0].2;
                p[1].0 >= end && p[1].0 - end <= MAX_PADDING
            });
            if sizes_match && adjacent {
                return Ok(MenuShapes {
                    meswin: w[0].1.clone(),
                    battle: w[1].1.clone(),
                    icon: w[2].1.clone(),
                });
            }
        }
        Err(ShapeError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(shapes: &[&[[u8; 12]]]) -> Vec<u8> {
        let mut out = (shapes.len() as u32).to_le_bytes().to_vec();
        let mut off = 4 + 4 * shapes.len();
        for s in shapes {
            out.extend((off as u32).to_le_bytes());
            off += 4 + 12 * s.len();
        }
        for s in shapes {
            out.extend((s.len() as u32).to_le_bytes());
            for p in *s {
                out.extend(p);
            }
        }
        out
    }

    fn bank_of(n: usize) -> Vec<u8> {
        let piece = [1, 2, 0xfe, 3, 8, 0, 16, 0, 1, 0, 0, 0];
        let shapes: Vec<&[[u8; 12]]> = (0..n).map(|_| std::slice::from_ref(&piece)).collect();
        table(&shapes)
    }

    #[test]
    fn pieces_decode() {
        let t = table(&[&[[0xa0, 0x38, 0xff, 2, 16, 0, 24, 0, 1, 0, 0, 0]], &[[0; 12], [1; 12]]]);
        let (bank, len) = bank_at(&t, 0).unwrap();
        assert_eq!(len, t.len());
        assert_eq!(bank.shapes.len(), 2);
        let p = bank.shapes[0].pieces[0];
        assert_eq!(
            (p.u, p.v, p.dx, p.dy, p.w, p.h, p.mirror_x, p.mirror_y),
            (0xa0, 0x38, -1, 2, 16, 24, true, false)
        );
        assert_eq!(bank.shapes[1].pieces.len(), 2);
    }

    #[test]
    fn menu_banks_found_by_layout() {
        let mut exe = vec![0x55u8; 64];
        exe.extend(bank_of(75));
        exe.extend(bank_of(7));
        exe.extend([0; 4]);
        exe.extend(bank_of(85));
        exe.extend([0; 4]);
        exe.extend(bank_of(110));
        exe.extend(bank_of(25));
        exe.extend([0x11; 32]);
        let m = MenuShapes::from_executable(&exe).unwrap();
        assert_eq!((m.meswin.shapes.len(), m.battle.shapes.len(), m.icon.shapes.len()), (85, 110, 25));
        assert_eq!(MenuShapes::from_executable(&bank_of(85)), Err(ShapeError::NotFound));
    }

    #[test]
    fn junk_never_panics() {
        for seed in 0..64u32 {
            let d: Vec<u8> =
                (0..4096u32).map(|i| (i.wrapping_mul(2654435761).wrapping_add(seed) >> 13) as u8).collect();
            let _ = scan(&d);
        }
        assert!(scan(&[2, 0, 0, 0, 12, 0, 0, 0]).is_empty());
    }

    /// The PC executable and the PS2 disc executable hold the same banks.
    #[test]
    fn installed_executables() {
        let Some(install) = aerugo_vfs::install_from_env() else { return };
        let Ok(pc) = std::fs::read(install.root.join("FFX.exe")) else { return };
        let m = MenuShapes::from_executable(&pc).expect("menu shapes in FFX.exe");
        let fits = |b: &Bank, w: u32, h: u32| {
            b.shapes
                .iter()
                .flat_map(|s| &s.pieces)
                .all(|p| p.u as u32 + p.w as u32 <= w && p.v as u32 + p.h as u32 <= h)
        };
        assert!(fits(&m.meswin, 256, 256));
        if let Some(ps2) = std::env::var_os(aerugo_vfs::ps2::PS2_FFX_ENV) {
            let exe =
                aerugo_vfs::ps2::boot_executable_at(std::path::Path::new(&ps2)).expect("PS2 executable");
            assert_eq!(MenuShapes::from_executable(&exe.1).expect("menu shapes in the PS2 executable"), m);
        }
    }
}
