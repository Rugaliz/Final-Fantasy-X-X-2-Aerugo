//! Asset viewer: browses the game data. Tabs are added as readers arrive.

pub(crate) mod field;
mod filelist;
mod maps;
mod models;
mod movies;
mod scripts;
mod sounds;
mod tables;
mod textures;

use std::sync::Arc;

use aerugo_debug_ui::egui;
use aerugo_gfx::Renderer;
use aerugo_vfs::{Install, Language, Vfs};

use field::FieldView;
use maps::MapBrowser;
use models::ModelBrowser;
#[cfg(test)]
pub(crate) use models::OrbitCamera;
use movies::MoviePlayer;
use scripts::ScriptBrowser;
use sounds::SoundBrowser;
use tables::TableBrowser;
use textures::TextureBrowser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Tables,
    Textures,
    Models,
    Maps,
    Sounds,
    Movies,
    Scripts,
    Field,
}

impl Tab {
    const ALL: [Tab; 8] = [
        Tab::Tables,
        Tab::Textures,
        Tab::Models,
        Tab::Maps,
        Tab::Sounds,
        Tab::Movies,
        Tab::Scripts,
        Tab::Field,
    ];

    fn name(self) -> &'static str {
        match self {
            Tab::Tables => "tables",
            Tab::Textures => "textures",
            Tab::Models => "models",
            Tab::Maps => "maps",
            Tab::Sounds => "sounds",
            Tab::Movies => "movies",
            Tab::Scripts => "scripts",
            Tab::Field => "field",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Tab::Tables => "Tables",
            Tab::Textures => "Textures",
            Tab::Models => "Models",
            Tab::Maps => "Maps",
            Tab::Sounds => "Sounds",
            Tab::Movies => "Movies",
            Tab::Scripts => "Scripts",
            Tab::Field => "Field",
        }
    }
}

#[derive(Debug)]
pub struct Viewer {
    vfs: Option<Arc<Vfs>>,
    error: Option<String>,
    tab: Tab,
    tables: TableBrowser,
    textures: TextureBrowser,
    models: ModelBrowser,
    maps: MapBrowser,
    sounds: SoundBrowser,
    movies: MoviePlayer,
    scripts: ScriptBrowser,
    field: FieldView,
    audio: Option<aerugo_audio::AudioOutput>,
    /// The game screen: only the field, without the viewer's tabs.
    game: bool,
}

/// What the viewer asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerAction {
    Back,
}

impl Viewer {
    /// Mounts the archives of `install`. Errors are shown in the viewer instead of failing.
    pub fn open(
        install: Option<&Install>,
        text_language: Language,
        sdl: &aerugo_platform::sdl3::Sdl,
        japanese_voices: bool,
        original_music: bool,
        tab: &str,
    ) -> Viewer {
        let audio = match aerugo_audio::AudioOutput::open(sdl) {
            Ok(a) => Some(a),
            Err(e) => {
                log::warn!("{e:#}");
                None
            }
        };
        let (vfs, error) = match install {
            None => (None, Some("No game installation selected.".to_owned())),
            Some(install) => {
                let mut vfs = Vfs::new();
                match vfs.mount_install(install) {
                    Ok(()) => (Some(Arc::new(vfs)), None),
                    Err(e) => (None, Some(format!("Could not open the archives: {e}"))),
                }
            }
        };
        let mut tables = TableBrowser::new(text_language);
        if let Some(vfs) = &vfs {
            tables.refresh(vfs);
        }
        Viewer {
            vfs,
            error,
            tab: Tab::ALL.into_iter().find(|t| t.name() == tab).unwrap_or(Tab::Tables),
            tables,
            textures: TextureBrowser::new(),
            models: ModelBrowser::new(),
            maps: MapBrowser::new(),
            sounds: SoundBrowser::with_languages(japanese_voices, original_music),
            movies: MoviePlayer::new(),
            scripts: ScriptBrowser::new(),
            field: FieldView::new(text_language),
            audio,
            game: false,
        }
    }

    /// The game screen: a new game of FFX from its title script, the field filling the window.
    pub fn open_game(
        install: Option<&Install>,
        text_language: Language,
        sdl: &aerugo_platform::sdl3::Sdl,
        japanese_voices: bool,
        original_music: bool,
    ) -> Viewer {
        let mut v = Viewer::open(install, text_language, sdl, japanese_voices, original_music, "field");
        v.tab = Tab::Field;
        v.field = FieldView::game(text_language);
        v.game = true;
        v
    }

    /// On the game screen, continue from a save file of the original release instead of the title.
    pub fn continue_from_steam(&mut self, data: &[u8]) -> Result<(), String> {
        self.field = FieldView::game_from_steam(self.field.language(), data)?;
        Ok(())
    }

    /// Show the field runner on an event.
    pub fn open_field(&mut self, event: String) {
        self.tab = Tab::Field;
        self.field.open_later(event);
    }

    /// The shown tab's settings name.
    pub fn tab_name(&self) -> &'static str {
        self.tab.name()
    }

    /// Release GPU resources before the viewer is dropped.
    pub fn close(&mut self, renderer: &mut Renderer) {
        self.textures.close(renderer);
        self.models.close(renderer);
        self.maps.close(renderer);
        self.sounds.close(self.audio.as_ref());
        self.movies.close(renderer, self.audio.as_ref());
        self.field.close(renderer, self.audio.as_ref());
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) -> Option<ViewerAction> {
        if self.game {
            match &self.vfs {
                Some(vfs) => self.field.ui(ui, vfs, renderer, self.audio.as_ref()),
                None => {
                    egui::CentralPanel::default_margins().show(ui, |ui| {
                        ui.label(self.error.as_deref().unwrap_or("No data."));
                    });
                }
            }
            return self.field.take_quit().then_some(ViewerAction::Back);
        }
        let mut action = None;
        egui::Panel::top("viewer-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("< Launcher").clicked() {
                    action = Some(ViewerAction::Back);
                }
                ui.separator();
                for (i, tab) in Tab::ALL.into_iter().enumerate() {
                    ui.selectable_value(&mut self.tab, tab, tab.label())
                        .on_hover_text(format!("Ctrl+{}", i + 1));
                }
                // Ctrl+1 … Ctrl+7 switch tabs.
                let keys = [
                    egui::Key::Num1,
                    egui::Key::Num2,
                    egui::Key::Num3,
                    egui::Key::Num4,
                    egui::Key::Num5,
                    egui::Key::Num6,
                    egui::Key::Num7,
                    egui::Key::Num8,
                ];
                for (tab, key) in Tab::ALL.into_iter().zip(keys) {
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, key)) {
                        self.tab = tab;
                    }
                }
                if let Some(vfs) = &self.vfs {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let files: usize = vfs.archives().map(|(_, a)| a.entries().len()).sum();
                        ui.weak(format!("{} archives · {files} files", vfs.archives().count()));
                    });
                }
            });
        });
        match (&self.vfs, &self.error) {
            (Some(vfs), _) => match self.tab {
                Tab::Tables => self.tables.ui(ui, vfs),
                Tab::Textures => self.textures.ui(ui, vfs, renderer),
                Tab::Models => self.models.ui(ui, vfs, renderer),
                Tab::Maps => self.maps.ui(ui, vfs, renderer),
                Tab::Sounds => self.sounds.ui(ui, vfs, self.audio.as_ref()),
                Tab::Movies => self.movies.ui(ui, vfs, renderer, self.audio.as_ref()),
                Tab::Scripts => self.scripts.ui(ui, vfs),
                Tab::Field => self.field.ui(ui, vfs, renderer, self.audio.as_ref()),
            },
            (None, error) => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    ui.label(error.as_deref().unwrap_or("No data."));
                });
            }
        }
        action
    }
}
