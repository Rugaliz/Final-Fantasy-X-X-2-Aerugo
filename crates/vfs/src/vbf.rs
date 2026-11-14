//! Reader for the VBF archives of the Steam release (`FFX_Data.vbf`, `FFX2_Data.vbf`, `metamenu.vbf`).
//!
//! The format is documented in `docs/formats/vbf.md`. In short: a header with one record per file,
//! a name table, and a table of stored sizes for 64 KiB blocks. A block whose stored size equals its
//! uncompressed size is raw; any other block is a zlib stream.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Uncompressed size of one archive block.
pub const BLOCK_SIZE: u64 = 0x1_0000;
const MAGIC: &[u8; 4] = b"SRYK";
const RECORD_SIZE: usize = 32;
const HASH_SIZE: usize = 16;

#[derive(Debug, Error)]
pub enum VbfError {
    #[error("I/O error on {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}: not a VBF archive")]
    BadMagic { path: PathBuf },
    #[error("{path}: corrupt header ({reason})")]
    BadHeader { path: PathBuf, reason: &'static str },
    #[error("{path}: entry '{entry}' is corrupt ({reason})")]
    BadEntry { path: PathBuf, entry: String, reason: &'static str },
}

/// One file inside an archive.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Path as stored in the archive (forward slashes).
    pub path: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Absolute file offset of the first block.
    pub offset: u64,
    /// Index of the entry's first block in the block table.
    pub first_block: u32,
}

impl Entry {
    pub fn block_count(&self) -> u64 {
        self.size.div_ceil(BLOCK_SIZE)
    }
}

/// An opened VBF archive. Reads are positional, so `&self` methods can be used from several threads.
#[derive(Debug)]
pub struct VbfArchive {
    path: PathBuf,
    file: File,
    file_len: u64,
    entries: Vec<Entry>,
    blocks: Vec<u16>,
    by_path: HashMap<String, usize>,
}

/// Normalize a path for lookups: forward slashes, lower case, no leading slash.
pub fn normalize_path(path: &str) -> String {
    path.trim_start_matches(['/', '\\']).replace('\\', "/").to_ascii_lowercase()
}

impl VbfArchive {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, VbfError> {
        let path = path.as_ref().to_path_buf();
        let io_err = |source| VbfError::Io { path: path.clone(), source };
        let file = File::open(&path).map_err(io_err)?;
        let file_len = file.metadata().map_err(io_err)?.len();

        let mut head = [0u8; 16];
        read_exact_at(&file, &mut head, 0).map_err(io_err)?;
        if &head[0..4] != MAGIC {
            return Err(VbfError::BadMagic { path });
        }
        let header_len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as u64;
        let count = u64::from_le_bytes(head[8..16].try_into().unwrap());
        if header_len < 16 || header_len > file_len {
            return Err(VbfError::BadHeader { path, reason: "header length out of range" });
        }
        let mut header = vec![0u8; (header_len - 16) as usize];
        read_exact_at(&file, &mut header, 16).map_err(io_err)?;
        let (entries, blocks) = parse_header(&header, count)
            .map_err(|reason| VbfError::BadHeader { path: path.clone(), reason })?;

        let mut by_path = HashMap::with_capacity(entries.len());
        for (i, e) in entries.iter().enumerate() {
            by_path.entry(normalize_path(&e.path)).or_insert(i);
        }
        let archive = Self { path, file, file_len, entries, blocks, by_path };
        archive.check_entries()?;
        Ok(archive)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Case-insensitive lookup by archive path.
    pub fn find(&self, path: &str) -> Option<&Entry> {
        self.by_path.get(&normalize_path(path)).map(|&i| &self.entries[i])
    }

    /// Read and decompress a whole entry.
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>, VbfError> {
        let mut out = Vec::with_capacity(entry.size as usize);
        let mut pos = entry.offset;
        let mut stored_buf = Vec::with_capacity(BLOCK_SIZE as usize);
        for i in 0..entry.block_count() {
            let want = (entry.size - i * BLOCK_SIZE).min(BLOCK_SIZE) as usize;
            let stored = self.stored_size(entry, i);
            stored_buf.resize(stored, 0);
            read_exact_at(&self.file, &mut stored_buf, pos)
                .map_err(|source| VbfError::Io { path: self.path.clone(), source })?;
            pos += stored as u64;
            if stored == want {
                out.extend_from_slice(&stored_buf);
            } else {
                inflate_block(&stored_buf, want, &mut out).map_err(|reason| VbfError::BadEntry {
                    path: self.path.clone(),
                    entry: entry.path.clone(),
                    reason,
                })?;
            }
        }
        Ok(out)
    }

    fn stored_size(&self, entry: &Entry, block: u64) -> usize {
        match self.blocks[entry.first_block as usize + block as usize] {
            0 => BLOCK_SIZE as usize,
            n => n as usize,
        }
    }

    /// Structural checks that do not need decompression: block indices and stored data in range.
    fn check_entries(&self) -> Result<(), VbfError> {
        for e in &self.entries {
            // Empty entries own no blocks; their offset field is meaningless in shipped archives.
            if e.size == 0 {
                continue;
            }
            let bad = |reason| VbfError::BadEntry { path: self.path.clone(), entry: e.path.clone(), reason };
            let first = e.first_block as u64;
            if first + e.block_count() > self.blocks.len() as u64 {
                return Err(bad("block index out of range"));
            }
            let stored: u64 = (0..e.block_count()).map(|i| self.stored_size(e, i) as u64).sum();
            if e.offset.checked_add(stored).is_none_or(|end| end > self.file_len) {
                return Err(bad("data extends past end of archive"));
            }
        }
        Ok(())
    }
}

/// Parse everything after the 16-byte fixed header.
fn parse_header(h: &[u8], count: u64) -> Result<(Vec<Entry>, Vec<u16>), &'static str> {
    let n = usize::try_from(count).map_err(|_| "entry count too large")?;
    let records_at = n.checked_mul(HASH_SIZE).ok_or("entry count too large")?;
    let names_at = records_at.checked_add(n.checked_mul(RECORD_SIZE).ok_or("entry count too large")?);
    let names_at = names_at.filter(|&p| p + 4 <= h.len()).ok_or("entry table truncated")?;

    let name_len = u32::from_le_bytes(h[names_at..names_at + 4].try_into().unwrap()) as usize;
    let names_end = names_at.checked_add(name_len).filter(|&e| e <= h.len() && name_len >= 4);
    let names_end = names_end.ok_or("name table truncated")?;
    let names = &h[names_at + 4..names_end];

    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let r = &h[records_at + i * RECORD_SIZE..records_at + (i + 1) * RECORD_SIZE];
        let first_block = u32::from_le_bytes(r[0..4].try_into().unwrap());
        let size = u64::from_le_bytes(r[8..16].try_into().unwrap());
        let offset = u64::from_le_bytes(r[16..24].try_into().unwrap());
        let name_off = usize::try_from(u64::from_le_bytes(r[24..32].try_into().unwrap()))
            .map_err(|_| "name offset out of range")?;
        let tail = names.get(name_off..).ok_or("name offset out of range")?;
        let end = tail.iter().position(|&b| b == 0).ok_or("unterminated name")?;
        let path = String::from_utf8_lossy(&tail[..end]).into_owned();
        entries.push(Entry { path, size, offset, first_block });
    }

    let block_bytes = &h[names_end..];
    let blocks = block_bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    Ok((entries, blocks))
}

fn inflate_block(stored: &[u8], want: usize, out: &mut Vec<u8>) -> Result<(), &'static str> {
    use flate2::{Decompress, FlushDecompress, Status};
    let start = out.len();
    out.reserve(want);
    let mut z = Decompress::new(true);
    let status = z.decompress_vec(stored, out, FlushDecompress::Finish).map_err(|_| "invalid zlib stream")?;
    if status != Status::StreamEnd || out.len() - start != want {
        return Err("block decompressed to the wrong size");
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_archive {
    //! Builds small synthetic archives so the reader can be tested without game data.
    use super::*;
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write;

    /// Write an archive containing `files` (path, contents). Blocks that shrink under zlib are stored
    /// compressed, others raw.
    pub fn build(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let n = files.len();
        let mut names = Vec::new();
        let mut name_offs = Vec::new();
        for (p, _) in files {
            name_offs.push(names.len() as u64);
            names.extend_from_slice(p.as_bytes());
            names.push(0);
        }
        let mut blocks: Vec<u16> = Vec::new();
        let mut payload: Vec<Vec<u8>> = Vec::new();
        let mut first_blocks = Vec::new();
        for (_, data) in files {
            first_blocks.push(blocks.len() as u32);
            let mut stored = Vec::new();
            for chunk in data.chunks(BLOCK_SIZE as usize) {
                let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
                enc.write_all(chunk).unwrap();
                let z = enc.finish().unwrap();
                let block = if z.len() < chunk.len() { z } else { chunk.to_vec() };
                blocks.push(if block.len() == BLOCK_SIZE as usize { 0 } else { block.len() as u16 });
                stored.extend_from_slice(&block);
            }
            payload.push(stored);
        }
        let header_len = 16 + n * (HASH_SIZE + RECORD_SIZE) + 4 + names.len() + blocks.len() * 2;
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&(header_len as u32).to_le_bytes());
        out.extend_from_slice(&(n as u64).to_le_bytes());
        out.extend(std::iter::repeat_n(0u8, n * HASH_SIZE));
        let mut offset = header_len as u64;
        for i in 0..n {
            out.extend_from_slice(&first_blocks[i].to_le_bytes());
            out.extend_from_slice(&first_blocks[i].to_le_bytes());
            out.extend_from_slice(&(files[i].1.len() as u64).to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(&name_offs[i].to_le_bytes());
            offset += payload[i].len() as u64;
        }
        out.extend_from_slice(&((names.len() + 4) as u32).to_le_bytes());
        out.extend_from_slice(&names);
        for b in &blocks {
            out.extend_from_slice(&b.to_le_bytes());
        }
        assert_eq!(out.len(), header_len);
        for p in payload {
            out.extend_from_slice(&p);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_archive(bytes: &[u8]) -> (PathBuf, VbfArchive) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("aerugo-vbf-test-{}-{id}.vbf", std::process::id()));
        std::fs::File::create(&path).unwrap().write_all(bytes).unwrap();
        let archive = VbfArchive::open(&path).unwrap();
        (path, archive)
    }

    fn pseudo_random(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed.wrapping_mul(2654435761) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect()
    }

    #[test]
    fn reads_raw_compressed_and_empty_files() {
        let files = vec![
            ("dir/Empty.bin", Vec::new()),
            ("dir/text.txt", b"hello archive ".repeat(10_000)),
            ("noise.bin", pseudo_random(200_000, 7)),
            ("exact_block.bin", vec![0xAB; BLOCK_SIZE as usize]),
        ];
        let (path, a) = temp_archive(&test_archive::build(&files));
        assert_eq!(a.entries().len(), files.len());
        for (name, data) in &files {
            let e = a.find(name).unwrap_or_else(|| panic!("{name} not found"));
            assert_eq!(&a.read(e).unwrap(), data, "{name}");
        }
        assert!(a.find("/DIR/EMPTY.BIN").is_some(), "lookup is case-insensitive and ignores a leading slash");
        assert!(a.find("missing").is_none());
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = test_archive::build(&[("a", vec![1, 2, 3])]);
        bytes[0] = b'X';
        let path = std::env::temp_dir().join(format!("aerugo-vbf-magic-{}.vbf", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(VbfArchive::open(&path), Err(VbfError::BadMagic { .. })));
        std::fs::remove_file(path).ok();
    }

    proptest::proptest! {
        #[test]
        fn header_parser_never_panics(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512),
                                      count in 0u64..64) {
            let _ = parse_header(&bytes, count);
        }

        #[test]
        fn corrupted_archives_error_instead_of_panicking(flip in 0usize..400, value: u8) {
            let mut bytes = test_archive::build(&[("a/b.bin", b"0123456789".repeat(50)), ("c", vec![9; 70_000])]);
            let i = flip % bytes.len();
            bytes[i] = value;
            let path = std::env::temp_dir().join(format!("aerugo-vbf-fuzz-{}.vbf", std::process::id()));
            std::fs::write(&path, &bytes).unwrap();
            if let Ok(a) = VbfArchive::open(&path) {
                for e in a.entries().to_vec() {
                    let _ = a.read(&e);
                }
            }
            std::fs::remove_file(path).ok();
        }
    }
}
