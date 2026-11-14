//! A filterable, virtualized list of archive paths.

use aerugo_debug_ui::egui;
use aerugo_vfs::Vfs;

#[derive(Debug, Default)]
pub struct FileList {
    paths: Vec<String>,
    filter: String,
    shown: Vec<usize>,
    pub selected: Option<String>,
}

impl FileList {
    /// All archive paths for which `keep` returns true, sorted.
    pub fn new(vfs: &Vfs, keep: impl Fn(&str) -> bool) -> FileList {
        let mut paths: Vec<String> = vfs
            .archives()
            .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
            .filter(|p| keep(p))
            .collect();
        paths.sort();
        paths.dedup();
        let shown = (0..paths.len()).collect();
        FileList { paths, filter: String::new(), shown, selected: None }
    }

    /// Draws the filter box and the list; returns a path the user just picked.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut picked = None;
        ui.horizontal(|ui| {
            ui.label("Filter");
            if ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(f32::INFINITY)).changed() {
                let words: Vec<String> =
                    self.filter.to_lowercase().split_whitespace().map(str::to_owned).collect();
                self.shown = (0..self.paths.len())
                    .filter(|&i| words.iter().all(|w| self.paths[i].contains(w.as_str())))
                    .collect();
            }
        });
        ui.weak(format!("{} of {} files", self.shown.len(), self.paths.len()));
        let row = ui.text_style_height(&egui::TextStyle::Body) + 2.0;
        egui::ScrollArea::both().auto_shrink([false, false]).show_rows(
            ui,
            row,
            self.shown.len(),
            |ui, range| {
                for &i in &self.shown[range] {
                    let path = &self.paths[i];
                    // Show the interesting tail; the full path is in the tooltip.
                    let short = path
                        .strip_prefix("ffx_data/gamedata/ps3data/")
                        .or_else(|| path.strip_prefix("ffx-2_data/gamedata/ps3data/"))
                        .unwrap_or(path);
                    let game = if path.starts_with("ffx-2") { "X-2 " } else { "X " };
                    let selected = self.selected.as_deref() == Some(path.as_str());
                    let r = ui.selectable_label(selected, format!("{game}{short}")).on_hover_text(path);
                    if r.clicked() {
                        self.selected = Some(path.clone());
                        picked = Some(path.clone());
                    }
                }
            },
        );
        picked
    }
}
