//! Asset viewer: browses the game data. Tabs are added as readers arrive.

mod filelist;
mod maps;
mod models;
mod sounds;
mod tables;
mod textures;

use std::sync::Arc;

use aerugo_debug_ui::egui;
use aerugo_gfx::Renderer;
use aerugo_vfs::{Install, Language, Vfs};

use maps::MapBrowser;
use models::ModelBrowser;
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
    audio: Option<aerugo_audio::AudioOutput>,
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
            tab: Tab::Tables,
            tables,
            textures: TextureBrowser::new(),
            models: ModelBrowser::new(),
            maps: MapBrowser::new(),
            sounds: SoundBrowser::with_languages(japanese_voices, original_music),
            audio,
        }
    }

    /// Release GPU resources before the viewer is dropped.
    pub fn close(&mut self, renderer: &mut Renderer) {
        self.textures.close(renderer);
        self.models.close(renderer);
        self.maps.close(renderer);
        self.sounds.close(self.audio.as_ref());
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) -> Option<ViewerAction> {
        let mut action = None;
        egui::Panel::top("viewer-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("< Launcher").clicked() {
                    action = Some(ViewerAction::Back);
                }
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Tables, "Tables");
                ui.selectable_value(&mut self.tab, Tab::Textures, "Textures");
                ui.selectable_value(&mut self.tab, Tab::Models, "Models");
                ui.selectable_value(&mut self.tab, Tab::Maps, "Maps");
                ui.selectable_value(&mut self.tab, Tab::Sounds, "Sounds");
            });
        });
        match (&self.vfs, &self.error) {
            (Some(vfs), _) => match self.tab {
                Tab::Tables => self.tables.ui(ui, vfs),
                Tab::Textures => self.textures.ui(ui, vfs, renderer),
                Tab::Models => self.models.ui(ui, vfs, renderer),
                Tab::Maps => self.maps.ui(ui, vfs, renderer),
                Tab::Sounds => self.sounds.ui(ui, vfs, self.audio.as_ref()),
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
