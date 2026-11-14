//! Readers for the games' data formats. Pure parsing: each reader takes bytes and never panics on bad input.
//! Functions that name archive paths take a [`Game`].

pub mod camera;
pub mod fev;
pub mod fsb;
pub mod kernel;
pub mod mesh;
pub mod motion;
pub mod phyre;
pub mod skeleton;
pub mod text;
pub mod texture;

pub use kernel::{KernelError, KernelTable};
pub use phyre::{Cluster, ObjectId, PhyreError, Pointer};
pub use text::{Charset, Token};

use aerugo_vfs::GameId;

/// The two data sets. Eternal Calm uses FFX's data and Last Mission uses FFX-2's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Game {
    Ffx,
    Ffx2,
}

impl Game {
    pub const ALL: [Game; 2] = [Game::Ffx, Game::Ffx2];

    pub fn from_id(id: GameId) -> Option<Game> {
        match id {
            GameId::Ffx | GameId::FfxEternalCalm => Some(Game::Ffx),
            GameId::Ffx2 | GameId::Ffx2LastMission => Some(Game::Ffx2),
            GameId::CreditsBonusAudio => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Game::Ffx => "FFX",
            Game::Ffx2 => "FFX-2",
        }
    }
}
