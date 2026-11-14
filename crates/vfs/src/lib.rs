//! Game data access for Aerugo: finding the installation, reading its VBF archives and resolving
//! game paths through a virtual file system.

pub mod fs;
pub mod install;
pub mod lang;
pub mod vbf;
pub mod vdf;

pub use fs::{Origin, Vfs, VfsError};
pub use install::{ArchiveId, ArchiveStatus, GameId, Install};
pub use lang::Language;
pub use vbf::{Entry, VbfArchive, VbfError};

/// Name of the environment variable that points tests and tools at a game installation.
pub const GAME_DIR_ENV: &str = "AERUGO_GAME_DIR";

/// Installation named by [`GAME_DIR_ENV`], if set.
pub fn install_from_env() -> Option<Install> {
    std::env::var_os(GAME_DIR_ENV).map(Install::inspect)
}
