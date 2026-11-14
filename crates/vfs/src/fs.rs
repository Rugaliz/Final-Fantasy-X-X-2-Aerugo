//! The virtual file system: an ordered stack of sources (loose override folder, then archives),
//! looked up with case-insensitive, slash-normalized paths.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use crate::install::{ArchiveId, Install};
use crate::lang::{Language, localized_candidates};
use crate::vbf::{VbfArchive, VbfError, normalize_path};

#[derive(Debug, Error)]
pub enum VfsError {
    #[error(transparent)]
    Archive(#[from] VbfError),
    #[error("I/O error on {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("file not found: {0}")]
    NotFound(String),
}

#[derive(Debug)]
enum Source {
    Loose(PathBuf),
    Archive { id: Option<ArchiveId>, archive: Arc<VbfArchive> },
}

/// Where a file was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Loose(PathBuf),
    Archive(Option<ArchiveId>),
}

/// Ordered set of file sources. Earlier sources win.
#[derive(Debug, Default)]
pub struct Vfs {
    sources: Vec<Source>,
}

impl Vfs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount the archives of an installation that are present and valid.
    pub fn mount_install(&mut self, install: &Install) -> Result<(), VfsError> {
        for id in ArchiveId::ALL {
            if matches!(install.archive_status(id), crate::install::ArchiveStatus::Ok { .. }) {
                let archive = VbfArchive::open(install.archive_path(id))?;
                self.sources.push(Source::Archive { id: Some(id), archive: Arc::new(archive) });
            }
        }
        Ok(())
    }

    pub fn mount_archive(&mut self, archive: VbfArchive) {
        self.sources.push(Source::Archive { id: None, archive: Arc::new(archive) });
    }

    /// Put a loose-file folder in front of everything mounted so far (developer overrides).
    pub fn mount_loose_front(&mut self, dir: impl Into<PathBuf>) {
        self.sources.insert(0, Source::Loose(dir.into()));
    }

    pub fn archives(&self) -> impl Iterator<Item = (Option<ArchiveId>, &Arc<VbfArchive>)> {
        self.sources.iter().filter_map(|s| match s {
            Source::Archive { id, archive } => Some((*id, archive)),
            Source::Loose(_) => None,
        })
    }

    /// Locate a file without reading it.
    pub fn locate(&self, path: &str) -> Option<Origin> {
        let norm = normalize_path(path);
        for s in &self.sources {
            match s {
                Source::Loose(dir) => {
                    if let Some(p) = find_loose(dir, &norm) {
                        return Some(Origin::Loose(p));
                    }
                }
                Source::Archive { id, archive } => {
                    if archive.find(&norm).is_some() {
                        return Some(Origin::Archive(*id));
                    }
                }
            }
        }
        None
    }

    pub fn exists(&self, path: &str) -> bool {
        self.locate(path).is_some()
    }

    /// Locate the best localized variant of `path` (see [`crate::lang`]); returns the path that matched.
    pub fn locate_localized(&self, path: &str, lang: Language) -> Option<(String, Origin)> {
        localized_candidates(path, lang).into_iter().find_map(|p| self.locate(&p).map(|o| (p, o)))
    }

    /// Read the best localized variant of `path`.
    pub fn read_localized(&self, path: &str, lang: Language) -> Result<Vec<u8>, VfsError> {
        match self.locate_localized(path, lang) {
            Some((found, _)) => self.read(&found),
            None => Err(VfsError::NotFound(path.to_owned())),
        }
    }

    /// Read a whole file.
    /// Size of a file in bytes.
    pub fn size(&self, path: &str) -> Option<u64> {
        let norm = normalize_path(path);
        for s in &self.sources {
            match s {
                Source::Loose(dir) => {
                    if let Some(p) = find_loose(dir, &norm) {
                        return std::fs::metadata(&p).ok().map(|m| m.len());
                    }
                }
                Source::Archive { archive, .. } => {
                    if let Some(e) = archive.find(&norm) {
                        return Some(e.size);
                    }
                }
            }
        }
        None
    }

    /// Read part of a file (from archives, only the blocks that cover it are inflated).
    pub fn read_range(&self, path: &str, start: u64, len: u64) -> Result<Vec<u8>, VfsError> {
        let norm = normalize_path(path);
        for s in &self.sources {
            match s {
                Source::Loose(dir) => {
                    if let Some(p) = find_loose(dir, &norm) {
                        let all = std::fs::read(&p).map_err(|source| VfsError::Io { path: p, source })?;
                        let a = (start as usize).min(all.len());
                        let b = (start.saturating_add(len) as usize).min(all.len());
                        return Ok(all[a..b].to_vec());
                    }
                }
                Source::Archive { archive, .. } => {
                    if let Some(e) = archive.find(&norm) {
                        return Ok(archive.read_range(e, start, len)?);
                    }
                }
            }
        }
        self.read(path)
    }

    pub fn read(&self, path: &str) -> Result<Vec<u8>, VfsError> {
        let norm = normalize_path(path);
        for s in &self.sources {
            match s {
                Source::Loose(dir) => {
                    if let Some(p) = find_loose(dir, &norm) {
                        return std::fs::read(&p).map_err(|source| VfsError::Io { path: p, source });
                    }
                }
                Source::Archive { archive, .. } => {
                    if let Some(e) = archive.find(&norm) {
                        return Ok(archive.read(e)?);
                    }
                }
            }
        }
        Err(VfsError::NotFound(path.to_owned()))
    }
}

/// Case-insensitive lookup inside a loose folder.
fn find_loose(dir: &Path, norm: &str) -> Option<PathBuf> {
    let mut current = dir.to_path_buf();
    for part in norm.split('/').filter(|p| !p.is_empty()) {
        let exact = current.join(part);
        if exact.exists() {
            current = exact;
            continue;
        }
        let entry = std::fs::read_dir(&current)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        current = entry.path();
    }
    current.is_file().then_some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loose_files_override_archives() {
        let dir = std::env::temp_dir().join(format!("aerugo-vfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("loose/Data/Sub")).unwrap();
        std::fs::write(dir.join("loose/Data/Sub/File.TXT"), b"loose").unwrap();
        let archive_path = dir.join("a.vbf");
        std::fs::write(
            &archive_path,
            crate::vbf::test_archive::build(&[
                ("data/sub/file.txt", b"archived".to_vec()),
                ("only", b"x".to_vec()),
            ]),
        )
        .unwrap();

        let mut vfs = Vfs::new();
        vfs.mount_archive(VbfArchive::open(&archive_path).unwrap());
        assert_eq!(vfs.read("/DATA/sub/file.txt").unwrap(), b"archived");
        vfs.mount_loose_front(dir.join("loose"));
        assert_eq!(vfs.read("data/SUB/file.txt").unwrap(), b"loose");
        assert!(matches!(vfs.locate("data/sub/file.txt"), Some(Origin::Loose(_))));
        assert_eq!(vfs.read("only").unwrap(), b"x");
        assert!(matches!(vfs.read("nothing"), Err(VfsError::NotFound(_))));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn localized_reads_prefer_the_language_overlay() {
        let dir = std::env::temp_dir().join(format!("aerugo-vfs-lang-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let archive_path = dir.join("l.vbf");
        std::fs::write(
            &archive_path,
            crate::vbf::test_archive::build(&[
                ("g/master/jppc/menu/a.bin", b"base".to_vec()),
                ("g/master/new_frpc/menu/a.bin", b"french".to_vec()),
                ("g/master/jppc/menu/b.bin", b"base only".to_vec()),
                ("g/ui_us/t.dds", b"english ui".to_vec()),
            ]),
        )
        .unwrap();
        let mut vfs = Vfs::new();
        vfs.mount_archive(VbfArchive::open(&archive_path).unwrap());
        assert_eq!(vfs.read_localized("g/master/jppc/menu/a.bin", Language::French).unwrap(), b"french");
        assert_eq!(vfs.read_localized("g/master/jppc/menu/a.bin", Language::German).unwrap(), b"base");
        assert_eq!(vfs.read_localized("g/master/jppc/menu/b.bin", Language::French).unwrap(), b"base only");
        assert_eq!(vfs.read_localized("g/ui_{lang}/t.dds", Language::Korean).unwrap(), b"english ui");
        assert!(vfs.read_localized("g/ui_{lang}/none.dds", Language::Korean).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
