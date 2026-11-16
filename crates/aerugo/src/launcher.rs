//! Game discovery and the launcher screen.

use std::path::PathBuf;

use aerugo_debug_ui::egui;
use aerugo_vfs::{ArchiveId, ArchiveStatus, GameId, Install};

/// Where the install in use came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallSource {
    CommandLine,
    Environment,
    Settings,
    Steam,
    Manual,
}

impl InstallSource {
    fn label(self) -> &'static str {
        match self {
            InstallSource::CommandLine => "from --game-dir",
            InstallSource::Environment => "from AERUGO_GAME_DIR",
            InstallSource::Settings => "from settings",
            InstallSource::Steam => "found in a Steam library",
            InstallSource::Manual => "chosen manually",
        }
    }
}

/// Why a launcher entry can or cannot start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryStatus {
    Ready,
    NotImplemented,
    NoInstall,
    MissingData(Vec<ArchiveId>),
}

impl EntryStatus {
    pub fn describe(&self) -> String {
        match self {
            EntryStatus::Ready => "ready".into(),
            EntryStatus::NotImplemented => "game data found; not implemented yet".into(),
            EntryStatus::NoInstall => "no game folder selected".into(),
            EntryStatus::MissingData(list) => {
                let names: Vec<&str> = list.iter().map(|a| archive_file_name(*a)).collect();
                format!("missing or invalid: {}", names.join(", "))
            }
        }
    }
}

fn archive_file_name(id: ArchiveId) -> &'static str {
    let p = id.relative_path();
    p.rsplit('/').next().unwrap_or(p)
}

/// Entries whose game module exists: FFX (its field; battles are a placeholder).
fn implemented(game: GameId) -> bool {
    game == GameId::Ffx
}

pub fn entry_status(install: Option<&Install>, game: GameId) -> EntryStatus {
    let Some(install) = install else { return EntryStatus::NoInstall };
    let missing: Vec<ArchiveId> = game
        .required_archives()
        .iter()
        .copied()
        .filter(|&a| !matches!(install.archive_status(a), ArchiveStatus::Ok { .. }))
        .collect();
    if !missing.is_empty() {
        EntryStatus::MissingData(missing)
    } else if implemented(game) {
        EntryStatus::Ready
    } else {
        EntryStatus::NotImplemented
    }
}

/// Pick the install to use, in priority order: command line, environment, settings, Steam libraries.
pub fn choose_install(
    cli_dir: Option<PathBuf>,
    settings_dir: Option<PathBuf>,
    steam: impl FnOnce() -> Vec<Install>,
) -> Option<(Install, InstallSource)> {
    if let Some(dir) = cli_dir {
        return Some((Install::inspect(dir), InstallSource::CommandLine));
    }
    if let Some(dir) = std::env::var_os(aerugo_vfs::GAME_DIR_ENV) {
        return Some((Install::inspect(PathBuf::from(dir)), InstallSource::Environment));
    }
    if let Some(dir) = settings_dir {
        let install = Install::inspect(dir);
        if install.is_usable() {
            return Some((install, InstallSource::Settings));
        }
        log::warn!("game folder from settings is not usable: {}", install.root.display());
    }
    let found = steam();
    let best = found.iter().position(Install::is_usable).or(if found.is_empty() { None } else { Some(0) });
    best.map(|i| (found.into_iter().nth(i).expect("index in range"), InstallSource::Steam))
}

/// Things the launcher asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum LauncherAction {
    StartGame(GameId),
    OpenTestScene,
    OpenViewer,
    OpenSettings,
    UseFolder(PathBuf),
    Rescan,
    Quit,
}

#[derive(Debug, Default)]
pub struct Launcher {
    pub install: Option<(Install, InstallSource)>,
    /// One-line message shown at the top (errors, CLI notes).
    pub notice: Option<String>,
    manual_path: String,
}

impl Launcher {
    pub fn new(install: Option<(Install, InstallSource)>) -> Self {
        let manual_path = install.as_ref().map(|(i, _)| i.root.display().to_string()).unwrap_or_default();
        Self { install, notice: None, manual_path }
    }

    pub fn set_install(&mut self, install: Install, source: InstallSource) {
        self.manual_path = install.root.display().to_string();
        self.install = Some((install, source));
    }

    pub fn current(&self) -> Option<&Install> {
        self.install.as_ref().map(|(i, _)| i)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<LauncherAction> {
        let mut action = None;
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);
                ui.heading(egui::RichText::new("Aerugo").size(40.0).strong());
                ui.label("Final Fantasy X / X-2 source port");
                ui.add_space(16.0);
            });
            if let Some(notice) = &self.notice {
                ui.colored_label(ui.visuals().warn_fg_color, notice);
                ui.add_space(8.0);
            }

            let width = ui.available_width().min(720.0);
            ui.vertical_centered(|ui| {
                ui.set_max_width(width);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.strong("Games");
                    ui.add_space(4.0);
                    for game in GameId::ALL {
                        let status = entry_status(self.current(), game);
                        ui.horizontal(|ui| {
                            let enabled = status == EntryStatus::Ready;
                            let button = egui::Button::new(game.title()).min_size(egui::vec2(260.0, 28.0));
                            if ui.add_enabled(enabled, button).clicked() {
                                action = Some(LauncherAction::StartGame(game));
                            }
                            let color = match status {
                                EntryStatus::Ready => ui.visuals().text_color(),
                                EntryStatus::NotImplemented => ui.visuals().weak_text_color(),
                                _ => ui.visuals().warn_fg_color,
                            };
                            ui.colored_label(color, status.describe());
                        });
                    }
                });
                ui.add_space(8.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.strong("Game folder");
                    match &self.install {
                        Some((install, source)) => {
                            ui.label(format!("{} ({})", install.root.display(), source.label()));
                            for (id, status) in &install.archives {
                                let text = match status {
                                    ArchiveStatus::Ok { size } => {
                                        format!(
                                            "{}: ok, {:.1} GiB",
                                            archive_file_name(*id),
                                            *size as f64 / 1073741824.0
                                        )
                                    }
                                    ArchiveStatus::Missing => format!("{}: missing", archive_file_name(*id)),
                                    ArchiveStatus::Invalid(why) => {
                                        format!("{}: invalid ({why})", archive_file_name(*id))
                                    }
                                };
                                ui.label(egui::RichText::new(text).monospace().small());
                            }
                        }
                        None => {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "No install found. Enter the folder that contains data/FFX_Data.vbf.",
                            );
                        }
                    }
                    ui.horizontal(|ui| {
                        let edit = egui::TextEdit::singleline(&mut self.manual_path)
                            .hint_text("path to FINAL FANTASY FFX&FFX-2 HD Remaster")
                            .desired_width(width - 190.0);
                        let response = ui.add(edit);
                        let submitted =
                            response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button("Use folder").clicked() || submitted {
                            action = Some(LauncherAction::UseFolder(PathBuf::from(self.manual_path.trim())));
                        }
                        if ui.button("Rescan").clicked() {
                            action = Some(LauncherAction::Rescan);
                        }
                    });
                });
                ui.add_space(8.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    ui.strong("Tools");
                    ui.horizontal(|ui| {
                        if ui.button("Timing test scene").clicked() {
                            action = Some(LauncherAction::OpenTestScene);
                        }
                        let has_data = self.current().is_some_and(Install::is_usable);
                        if ui
                            .add_enabled(has_data, egui::Button::new("Asset viewer"))
                            .on_disabled_hover_text("Needs the game data")
                            .clicked()
                        {
                            action = Some(LauncherAction::OpenViewer);
                        }
                        if ui.button("Settings").clicked() {
                            action = Some(LauncherAction::OpenSettings);
                        }
                        if ui.button("Quit").clicked() {
                            action = Some(LauncherAction::Quit);
                        }
                    });
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("F3: debug panel   F11 / Alt+Enter: fullscreen   Esc: back")
                        .small()
                        .weak(),
                );
            });
        });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_install(ok: &[ArchiveId]) -> Install {
        Install {
            root: PathBuf::from("/fake"),
            archives: ArchiveId::ALL
                .iter()
                .map(|&a| {
                    (a, if ok.contains(&a) { ArchiveStatus::Ok { size: 1 } } else { ArchiveStatus::Missing })
                })
                .collect(),
        }
    }

    #[test]
    fn entry_status_reports_missing_archives() {
        let install = fake_install(&[ArchiveId::Ffx, ArchiveId::MetaMenu]);
        assert_eq!(entry_status(Some(&install), GameId::Ffx), EntryStatus::Ready);
        assert_eq!(entry_status(Some(&install), GameId::FfxEternalCalm), EntryStatus::NotImplemented);
        assert_eq!(
            entry_status(Some(&install), GameId::Ffx2),
            EntryStatus::MissingData(vec![ArchiveId::Ffx2])
        );
        assert_eq!(entry_status(None, GameId::Ffx), EntryStatus::NoInstall);
        assert!(EntryStatus::MissingData(vec![ArchiveId::Ffx2]).describe().contains(".vbf"));
    }

    #[test]
    fn command_line_wins_over_discovery() {
        let chosen = choose_install(Some(PathBuf::from("/nowhere")), None, || panic!("must not scan"));
        let (install, source) = chosen.unwrap();
        assert_eq!(source, InstallSource::CommandLine);
        assert!(!install.is_usable());
    }
}
