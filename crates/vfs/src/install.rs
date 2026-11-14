//! Finding and validating an installation of FINAL FANTASY X/X-2 HD Remaster (Steam release).

use std::path::{Path, PathBuf};

use crate::vdf;

/// Steam application id of the Steam release.
pub const STEAM_APP_ID: u32 = 359870;
/// Default install folder name inside `steamapps/common`.
pub const DEFAULT_INSTALL_DIR: &str = "FINAL FANTASY FFX&FFX-2 HD Remaster";

/// The data archives of the Steam release, relative to the install folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveId {
    Ffx,
    Ffx2,
    MetaMenu,
}

impl ArchiveId {
    pub const ALL: [ArchiveId; 3] = [ArchiveId::Ffx, ArchiveId::Ffx2, ArchiveId::MetaMenu];

    pub fn relative_path(self) -> &'static str {
        match self {
            ArchiveId::Ffx => "data/FFX_Data.vbf",
            ArchiveId::Ffx2 => "data/FFX2_Data.vbf",
            ArchiveId::MetaMenu => "data/metamenu.vbf",
        }
    }
}

/// Programs offered by the launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameId {
    Ffx,
    FfxEternalCalm,
    Ffx2,
    Ffx2LastMission,
    CreditsBonusAudio,
}

impl GameId {
    pub const ALL: [GameId; 5] = [
        GameId::Ffx,
        GameId::FfxEternalCalm,
        GameId::Ffx2,
        GameId::Ffx2LastMission,
        GameId::CreditsBonusAudio,
    ];

    pub fn title(self) -> &'static str {
        match self {
            GameId::Ffx => "FINAL FANTASY X",
            GameId::FfxEternalCalm => "FINAL FANTASY X - Eternal Calm",
            GameId::Ffx2 => "FINAL FANTASY X-2",
            GameId::Ffx2LastMission => "FINAL FANTASY X-2 - Last Mission",
            GameId::CreditsBonusAudio => "Credits & Bonus Audio",
        }
    }

    /// Short id used on the command line (`--game <id>`).
    pub fn cli_name(self) -> &'static str {
        match self {
            GameId::Ffx => "ffx",
            GameId::FfxEternalCalm => "eternal-calm",
            GameId::Ffx2 => "ffx2",
            GameId::Ffx2LastMission => "last-mission",
            GameId::CreditsBonusAudio => "credits",
        }
    }

    pub fn from_cli_name(name: &str) -> Option<GameId> {
        GameId::ALL.into_iter().find(|g| g.cli_name().eq_ignore_ascii_case(name))
    }

    /// Archives the program needs.
    pub fn required_archives(self) -> &'static [ArchiveId] {
        match self {
            GameId::Ffx | GameId::CreditsBonusAudio => &[ArchiveId::Ffx],
            GameId::FfxEternalCalm => &[ArchiveId::Ffx, ArchiveId::MetaMenu],
            GameId::Ffx2 | GameId::Ffx2LastMission => &[ArchiveId::Ffx2],
        }
    }
}

/// State of one archive in an installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveStatus {
    Ok { size: u64 },
    Missing,
    Invalid(String),
}

/// Result of checking a candidate install folder.
#[derive(Debug, Clone)]
pub struct Install {
    pub root: PathBuf,
    pub archives: Vec<(ArchiveId, ArchiveStatus)>,
}

impl Install {
    /// Check a folder. Never fails: problems are reported per archive.
    pub fn inspect(root: impl Into<PathBuf>) -> Install {
        let root = root.into();
        let archives =
            ArchiveId::ALL.iter().map(|&a| (a, check_archive(&root.join(a.relative_path())))).collect();
        Install { root, archives }
    }

    pub fn archive_status(&self, id: ArchiveId) -> &ArchiveStatus {
        &self.archives.iter().find(|(a, _)| *a == id).expect("all archives are inspected").1
    }

    pub fn archive_path(&self, id: ArchiveId) -> PathBuf {
        self.root.join(id.relative_path())
    }

    /// True if at least one launcher entry can use this folder.
    pub fn is_usable(&self) -> bool {
        GameId::ALL.iter().any(|&g| self.has_data_for(g))
    }

    pub fn has_data_for(&self, game: GameId) -> bool {
        game.required_archives().iter().all(|&a| matches!(self.archive_status(a), ArchiveStatus::Ok { .. }))
    }
}

fn check_archive(path: &Path) -> ArchiveStatus {
    use std::io::Read;
    let meta = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return ArchiveStatus::Invalid("not a file".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ArchiveStatus::Missing,
        Err(e) => return ArchiveStatus::Invalid(e.to_string()),
    };
    let mut magic = [0u8; 4];
    match std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)) {
        Ok(()) if &magic == b"SRYK" => ArchiveStatus::Ok { size: meta.len() },
        Ok(()) => ArchiveStatus::Invalid("not a VBF archive".into()),
        Err(e) => ArchiveStatus::Invalid(e.to_string()),
    }
}

/// Steam installation folders to look in on this machine.
pub fn steam_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    {
        use winreg::RegKey;
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        let candidates = [
            (HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath"),
            (HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\Valve\Steam", "InstallPath"),
            (HKEY_LOCAL_MACHINE, r"SOFTWARE\Valve\Steam", "InstallPath"),
        ];
        for (hive, key, value) in candidates {
            if let Ok(path) =
                RegKey::predef(hive).open_subkey(key).and_then(|k| k.get_value::<String, _>(value))
            {
                roots.push(PathBuf::from(path));
            }
        }
        roots.push(PathBuf::from(r"C:\Program Files (x86)\Steam"));
    }
    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for rel in [
            ".local/share/Steam",
            ".steam/steam",
            ".steam/root",
            ".var/app/com.valvesoftware.Steam/.local/share/Steam",
            "snap/steam/common/.local/share/Steam",
        ] {
            roots.push(home.join(rel));
        }
    }
    roots
}

/// Library folders listed by a Steam root's `libraryfolders.vdf` (the root itself included).
pub fn steam_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let mut libs = vec![steam_root.to_path_buf()];
    let file = steam_root.join("steamapps").join("libraryfolders.vdf");
    if let Ok(text) = std::fs::read_to_string(&file) {
        match vdf::parse(&text) {
            Ok(doc) => {
                if let Some(folders) = doc.get("libraryfolders") {
                    for (_, lib) in folders.entries() {
                        // Old format: "1" "path"; new format: "1" { "path" "..." }
                        let path = lib.as_str().or_else(|| lib.get("path").and_then(vdf::Value::as_str));
                        if let Some(p) = path {
                            libs.push(PathBuf::from(p));
                        }
                    }
                }
            }
            Err(e) => log::warn!("{}: {e}", file.display()),
        }
    }
    libs
}

/// Install folder of the game inside a library, from its app manifest (or the default folder name).
pub fn install_dir_in_library(library: &Path) -> Option<PathBuf> {
    let steamapps = library.join("steamapps");
    let manifest = steamapps.join(format!("appmanifest_{STEAM_APP_ID}.acf"));
    let dir_name = std::fs::read_to_string(manifest)
        .ok()
        .and_then(|t| vdf::parse(&t).ok())
        .and_then(|doc| doc.get("AppState")?.get("installdir")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| DEFAULT_INSTALL_DIR.to_owned());
    let dir = steamapps.join("common").join(dir_name);
    dir.is_dir().then_some(dir)
}

/// Search all Steam libraries on this machine for usable installations.
pub fn discover() -> Vec<Install> {
    discover_in(&steam_roots())
}

/// Search the given Steam roots (testable form of [`discover`]).
pub fn discover_in(roots: &[PathBuf]) -> Vec<Install> {
    let mut seen = Vec::new();
    let mut found = Vec::new();
    for root in roots {
        for lib in steam_libraries(root) {
            let Some(dir) = install_dir_in_library(&lib) else { continue };
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if seen.contains(&canonical) {
                continue;
            }
            seen.push(canonical);
            let install = Install::inspect(dir);
            if install.is_usable() {
                found.push(install);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aerugo-install-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_install(dir: &Path, archives: &[ArchiveId]) {
        std::fs::create_dir_all(dir.join("data")).unwrap();
        for a in archives {
            let bytes = crate::vbf::test_archive::build(&[("x", vec![1])]);
            std::fs::write(dir.join(a.relative_path()), bytes).unwrap();
        }
    }

    #[test]
    fn reports_per_game_availability() {
        let dir = temp_dir("avail");
        fake_install(&dir, &[ArchiveId::Ffx2]);
        std::fs::write(dir.join(ArchiveId::MetaMenu.relative_path()), b"nope").unwrap();
        let inst = Install::inspect(&dir);
        assert!(matches!(inst.archive_status(ArchiveId::Ffx), ArchiveStatus::Missing));
        assert!(matches!(inst.archive_status(ArchiveId::MetaMenu), ArchiveStatus::Invalid(_)));
        assert!(inst.has_data_for(GameId::Ffx2) && inst.has_data_for(GameId::Ffx2LastMission));
        assert!(!inst.has_data_for(GameId::Ffx) && !inst.has_data_for(GameId::FfxEternalCalm));
        assert!(inst.is_usable());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn discovers_install_through_library_folders() {
        let base = temp_dir("steam");
        let steam = base.join("Steam");
        let library = base.join("Other Library");
        let game = library.join("steamapps/common/My Folder");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::create_dir_all(library.join("steamapps")).unwrap();
        fake_install(&game, &ArchiveId::ALL);
        let lib_escaped = library.display().to_string().replace('\\', "\\\\");
        std::fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!("\"libraryfolders\" {{ \"1\" {{ \"path\" \"{lib_escaped}\" }} }}"),
        )
        .unwrap();
        std::fs::write(
            library.join(format!("steamapps/appmanifest_{STEAM_APP_ID}.acf")),
            "\"AppState\" { \"appid\" \"359870\" \"installdir\" \"My Folder\" }",
        )
        .unwrap();
        let found = discover_in(&[steam.clone(), steam]);
        assert_eq!(found.len(), 1, "found exactly once even when a root is listed twice");
        assert!(found[0].root.ends_with("My Folder"));
        assert!(GameId::ALL.iter().all(|&g| found[0].has_data_for(g)));
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn cli_names_round_trip() {
        for g in GameId::ALL {
            assert_eq!(GameId::from_cli_name(g.cli_name()), Some(g));
        }
        assert_eq!(GameId::from_cli_name("FFX2"), Some(GameId::Ffx2));
        assert_eq!(GameId::from_cli_name("unknown"), None);
    }
}
