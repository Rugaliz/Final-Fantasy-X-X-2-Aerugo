//! PS2 discs: reading files from an ISO 9660 image (2048-byte sectors, as dumped from a DVD) and finding
//! the disc's boot executable through `SYSTEM.CNF`. Some data (the menu shape banks) exists only in the
//! executables, so a PS2 disc can stand in for the PC executable.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Environment variable naming a PS2 FFX disc image (`.iso`) or its boot executable, for tests and tools.
pub const PS2_FFX_ENV: &str = "AERUGO_PS2_FFX";

const SECTOR: u64 = 2048;

#[derive(Debug, thiserror::Error)]
pub enum DiscError {
    #[error("not an ISO 9660 image")]
    NotIso,
    #[error("{0} is not on the disc")]
    NotFound(String),
    #[error("SYSTEM.CNF names no boot executable")]
    NoBoot,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A directory entry: extent sector, length in bytes, directory flag.
#[derive(Debug, Clone, Copy)]
struct Extent {
    sector: u32,
    len: u32,
    dir: bool,
}

#[derive(Debug)]
pub struct Disc<R> {
    image: R,
    root: Extent,
}

impl<R: Read + Seek> Disc<R> {
    /// Open an image by its primary volume descriptor (sector 16).
    pub fn new(mut image: R) -> Result<Disc<R>, DiscError> {
        let mut pvd = [0u8; 2048];
        image.seek(SeekFrom::Start(16 * SECTOR))?;
        image.read_exact(&mut pvd).map_err(|_| DiscError::NotIso)?;
        if pvd[0] != 1 || &pvd[1..6] != b"CD001" {
            return Err(DiscError::NotIso);
        }
        let root = record(&pvd[156..190]).ok_or(DiscError::NotIso)?.1;
        Ok(Disc { image, root })
    }

    fn read_extent(&mut self, e: Extent) -> Result<Vec<u8>, DiscError> {
        let mut out = vec![0; e.len as usize];
        self.image.seek(SeekFrom::Start(e.sector as u64 * SECTOR))?;
        self.image.read_exact(&mut out)?;
        Ok(out)
    }

    /// Read a file by its path (`/`- or `\`-separated, any case, with or without the `;1` version).
    pub fn read(&mut self, path: &str) -> Result<Vec<u8>, DiscError> {
        let mut at = self.root;
        for part in path.split(['/', '\\']).filter(|p| !p.is_empty()) {
            let want = part.split(';').next().unwrap_or(part);
            let dir = self.read_extent(at)?;
            let mut found = None;
            let mut p = 0;
            while p < dir.len() {
                let len = dir[p] as usize;
                if len == 0 {
                    // Records never cross a sector: continue at the next one.
                    p = (p / SECTOR as usize + 1) * SECTOR as usize;
                    continue;
                }
                if let Some((name, e)) = dir.get(p..p + len).and_then(record)
                    && name.split(';').next().is_some_and(|n| n.eq_ignore_ascii_case(want))
                {
                    found = Some(e);
                    break;
                }
                p += len;
            }
            at = found.ok_or_else(|| DiscError::NotFound(path.to_string()))?;
        }
        if at.dir {
            return Err(DiscError::NotFound(path.to_string()));
        }
        self.read_extent(at)
    }

    /// The boot executable named by `SYSTEM.CNF` (`BOOT2 = cdrom0:\NAME;1`): its name and bytes.
    pub fn boot_executable(&mut self) -> Result<(String, Vec<u8>), DiscError> {
        let cnf = self.read("SYSTEM.CNF")?;
        let cnf = String::from_utf8_lossy(&cnf);
        let name = cnf
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once('=')?;
                (k.trim() == "BOOT2").then(|| v.trim().trim_start_matches("cdrom0:").trim_start_matches('\\'))
            })
            .and_then(|v| v.split(';').next())
            .filter(|v| !v.is_empty())
            .ok_or(DiscError::NoBoot)?
            .to_string();
        let exe = self.read(&name)?;
        Ok((name, exe))
    }
}

/// Name and extent of a directory record.
fn record(r: &[u8]) -> Option<(String, Extent)> {
    let name_len = *r.get(32)? as usize;
    let sector = u32::from_le_bytes(r.get(2..6)?.try_into().ok()?);
    let len = u32::from_le_bytes(r.get(10..14)?.try_into().ok()?);
    let dir = r.get(25)? & 2 != 0;
    let name = String::from_utf8_lossy(r.get(33..33 + name_len)?).into_owned();
    Some((name, Extent { sector, len, dir }))
}

/// The boot executable of a disc image, or the file itself when `path` is an executable (ELF) already.
pub fn boot_executable_at(path: &Path) -> Result<(String, Vec<u8>), DiscError> {
    let mut f = std::fs::File::open(path)?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic == b"\x7fELF" {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok((name, std::fs::read(path)?));
    }
    Disc::new(std::io::BufReader::new(f))?.boot_executable()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_record(name: &[u8], sector: u32, len: u32, dir: bool) -> Vec<u8> {
        let size = 33 + name.len() + (name.len() + 1) % 2;
        let mut r = vec![0u8; size];
        r[0] = size as u8;
        r[2..6].copy_from_slice(&sector.to_le_bytes());
        r[10..14].copy_from_slice(&len.to_le_bytes());
        r[25] = if dir { 2 } else { 0 };
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    /// A tiny image: root (sector 18) with SYSTEM.CNF and a subdirectory DATA (sector 19) holding the
    /// executable.
    fn image() -> Vec<u8> {
        let mut img = vec![0u8; 24 * SECTOR as usize];
        let s = |n: usize| n * SECTOR as usize;
        img[s(16)] = 1;
        img[s(16) + 1..s(16) + 6].copy_from_slice(b"CD001");
        let root = dir_record(&[0], 18, SECTOR as u32, true);
        img[s(16) + 156..s(16) + 156 + root.len()].copy_from_slice(&root);
        img[s(17)] = 255;
        let cnf = b"BOOT2 = cdrom0:\\DATA\\SLPS_123.45;1\r\nVER = 1.00\r\n";
        let exe = b"\x7fELF test executable";
        let mut root_dir = Vec::new();
        root_dir.extend(dir_record(&[0], 18, SECTOR as u32, true));
        root_dir.extend(dir_record(&[1], 18, SECTOR as u32, true));
        root_dir.extend(dir_record(b"DATA", 19, SECTOR as u32, true));
        root_dir.extend(dir_record(b"SYSTEM.CNF;1", 20, cnf.len() as u32, false));
        img[s(18)..s(18) + root_dir.len()].copy_from_slice(&root_dir);
        let mut data_dir = Vec::new();
        data_dir.extend(dir_record(&[0], 19, SECTOR as u32, true));
        data_dir.extend(dir_record(b"SLPS_123.45;1", 21, exe.len() as u32, false));
        img[s(19)..s(19) + data_dir.len()].copy_from_slice(&data_dir);
        img[s(20)..s(20) + cnf.len()].copy_from_slice(cnf);
        img[s(21)..s(21) + exe.len()].copy_from_slice(exe);
        img
    }

    #[test]
    fn reads_files_and_boot_executable() {
        let mut disc = Disc::new(std::io::Cursor::new(image())).unwrap();
        assert!(disc.read("system.cnf;1").unwrap().starts_with(b"BOOT2"));
        let (name, exe) = disc.boot_executable().unwrap();
        assert_eq!(name, "DATA\\SLPS_123.45");
        assert_eq!(exe, b"\x7fELF test executable");
        assert!(matches!(disc.read("DATA/MISSING"), Err(DiscError::NotFound(_))));
        assert!(matches!(disc.read("DATA"), Err(DiscError::NotFound(_))));
    }

    #[test]
    fn rejects_other_files() {
        assert!(matches!(Disc::new(std::io::Cursor::new(vec![0u8; 40000])), Err(DiscError::NotIso)));
        assert!(matches!(Disc::new(std::io::Cursor::new(vec![0u8; 100])), Err(DiscError::NotIso)));
    }
}
