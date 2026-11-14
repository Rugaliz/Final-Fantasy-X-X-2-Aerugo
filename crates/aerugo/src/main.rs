//! Aerugo: a source port of Final Fantasy X and X-2. Needs the player's own copy of the HD Remaster.

mod app;
mod cli;
mod launcher;
mod scene;
mod settings;
mod viewer;

use std::process::ExitCode;

use aerugo_gfx::PresentMode;
use aerugo_vfs::GameId;
use clap::Parser;

use crate::cli::Cli;
use crate::launcher::{choose_install, entry_status};
use crate::settings::{SETTINGS_FILE, Settings};

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            log::error!("{e:#}");
            eprintln!("aerugo: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let user_dir = aerugo_platform::user_dir(cli.portable);
    let settings_path = user_dir.join(SETTINGS_FILE);
    let settings = Settings::load(&settings_path);
    log::info!("settings: {}", settings_path.display());

    let game = match cli.game.as_deref() {
        Some(name) => match GameId::from_cli_name(name) {
            Some(g) => Some(g),
            None => {
                let names: Vec<&str> = GameId::ALL.iter().map(|g| g.cli_name()).collect();
                eprintln!("unknown game '{name}'; expected one of: {}", names.join(", "));
                return Ok(ExitCode::from(2));
            }
        },
        None => None,
    };

    let install =
        choose_install(cli.game_dir.clone(), settings.game.dir.clone(), aerugo_vfs::install::discover);

    if cli.list_games {
        list_games(install.as_ref().map(|(i, _)| i));
        return Ok(ExitCode::SUCCESS);
    }

    let mut notice = None;
    if let Some(game) = game {
        let status = entry_status(install.as_ref().map(|(i, _)| i), game);
        notice = Some(format!("{}: {}", game.title(), status.describe()));
    }

    // Command-line options override the stored settings for this run only.
    let mut effective = settings.clone();
    effective.window.fullscreen |= cli.fullscreen;
    effective.graphics.validation |= cli.validation;
    if let Some(hz) = cli.fps_limit {
        effective.graphics.frame_limit = hz;
    }
    if let Some(mode) = &cli.present_mode {
        match PresentMode::from_name(mode) {
            Some(m) => effective.graphics.present_mode = m.name().into(),
            None => log::warn!("unknown present mode '{mode}'; keeping {}", effective.graphics.present_mode),
        }
    }
    if let Some(gpu) = &cli.gpu {
        effective.graphics.gpu = gpu.clone();
    }
    let opts = app::StartOptions {
        settings_path,
        file_settings: settings,
        settings: effective,
        install,
        notice,
        test_scene: cli.test_scene,
        viewer: cli.viewer,
    };
    let mut app = app::App::new(opts)?;
    app.run()?;
    Ok(ExitCode::SUCCESS)
}

fn list_games(chosen: Option<&aerugo_vfs::Install>) {
    let found = aerugo_vfs::install::discover();
    if found.is_empty() {
        println!("No install found in Steam libraries.");
    }
    for install in &found {
        println!("Steam install: {}", install.root.display());
    }
    match chosen {
        Some(install) => {
            println!("Using: {}", install.root.display());
            for game in GameId::ALL {
                println!(
                    "  {:<14} {:<34} {}",
                    game.cli_name(),
                    game.title(),
                    entry_status(Some(install), game).describe()
                );
            }
        }
        None => println!("No usable install. Pass --game-dir or set {}.", aerugo_vfs::GAME_DIR_ENV),
    }
}
