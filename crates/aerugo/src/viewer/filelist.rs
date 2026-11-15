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

    /// Draws the filter box and the list; returns a path the user just picked. Up and Down step through the
    /// shown files and open each one.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut picked = None;
        let step = ui.input_mut(|i| {
            i32::from(i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
                - i32::from(i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp))
        });
        let mut scroll_to = None;
        if step != 0 && !self.shown.is_empty() {
            let current =
                self.selected.as_ref().and_then(|s| self.shown.iter().position(|&i| &self.paths[i] == s));
            let next = match current {
                Some(c) => (c as i64 + i64::from(step)).clamp(0, self.shown.len() as i64 - 1) as usize,
                None => 0,
            };
            if current != Some(next) {
                let path = self.paths[self.shown[next]].clone();
                self.selected = Some(path.clone());
                picked = Some(path);
                scroll_to = Some(next);
            }
        }
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
        let mut area = egui::ScrollArea::both().auto_shrink([false, false]);
        if let Some(n) = scroll_to {
            let pitch = row + ui.spacing().item_spacing.y;
            area = area.vertical_scroll_offset((n as f32 * pitch - 4.0 * pitch).max(0.0));
        }
        area.show_rows(ui, row, self.shown.len(), |ui, range| {
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
        });
        picked
    }
}
