//! Command-line interface.

use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(
    name = "aerugo",
    version,
    about = "Final Fantasy X / X-2 source port (needs your own copy of the game)"
)]
pub struct Cli {
    /// Game install folder (the one containing `data/FFX_Data.vbf`). Overrides discovery and settings.
    #[arg(long, value_name = "DIR")]
    pub game_dir: Option<PathBuf>,

    /// Keep settings and saves in `aerugo-portable/` next to the executable.
    #[arg(long)]
    pub portable: bool,

    /// Start an entry directly, skipping the launcher (ffx, ffx-eternal-calm, ffx2, ffx2-last-mission, credits).
    #[arg(long, value_name = "ID")]
    pub game: Option<String>,

    /// Print detected installs and launcher entries, then exit.
    #[arg(long)]
    pub list_games: bool,

    /// Open the asset viewer.
    #[arg(long)]
    pub viewer: bool,

    /// Open the viewer's field runner on this event script (an archive path ending in `.ebp`).
    #[arg(long, value_name = "EVENT")]
    pub field: Option<String>,

    /// Start a new game of FFX right away (the game screen, from the title).
    #[arg(long)]
    pub new_game: bool,

    /// Open the timing test scene.
    #[arg(long)]
    pub test_scene: bool,

    /// Start in fullscreen.
    #[arg(long)]
    pub fullscreen: bool,

    /// Enable the Vulkan validation layer (if installed).
    #[arg(long)]
    pub validation: bool,

    /// Prefer the GPU whose name contains this text.
    #[arg(long, value_name = "NAME")]
    pub gpu: Option<String>,

    /// Presentation mode: fifo, fifo-relaxed, mailbox or immediate.
    #[arg(long, value_name = "MODE")]
    pub present_mode: Option<String>,

    /// Frame rate cap in Hz (0 = uncapped).
    #[arg(long, value_name = "HZ")]
    pub fps_limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags() {
        let cli = Cli::try_parse_from(["aerugo", "--game-dir", "/x", "--game", "ffx", "--fps-limit", "240"])
            .unwrap();
        assert_eq!(cli.game_dir, Some(PathBuf::from("/x")));
        assert_eq!(cli.game.as_deref(), Some("ffx"));
        assert_eq!(cli.fps_limit, Some(240));
        assert!(!cli.portable);
    }

    #[test]
    fn verify_cli() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
