//! Asset viewer: browses the game data. Tabs are added as readers arrive (tables first).

mod tables;

use std::sync::Arc;

use aerugo_debug_ui::egui;
use aerugo_vfs::{Install, Language, Vfs};

use tables::TableBrowser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Tables,
}

#[derive(Debug)]
pub struct Viewer {
    vfs: Option<Arc<Vfs>>,
    error: Option<String>,
    tab: Tab,
    tables: TableBrowser,
}

/// What the viewer asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerAction {
    Back,
}

impl Viewer {
    /// Mounts the archives of `install`. Errors are shown in the viewer instead of failing.
    pub fn open(install: Option<&Install>, text_language: Language) -> Viewer {
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
        Viewer { vfs, error, tab: Tab::Tables, tables }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<ViewerAction> {
        let mut action = None;
        egui::Panel::top("viewer-bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("< Launcher").clicked() {
                    action = Some(ViewerAction::Back);
                }
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Tables, "Tables");
            });
        });
        match (&self.vfs, &self.error) {
            (Some(vfs), _) => match self.tab {
                Tab::Tables => self.tables.ui(ui, vfs),
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
