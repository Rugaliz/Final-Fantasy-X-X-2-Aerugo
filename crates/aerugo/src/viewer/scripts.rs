//! Script browser: every event and battle script of both games, disassembled per worker with our native names.

use std::collections::BTreeMap;
use std::sync::Arc;

use aerugo_debug_ui::egui;
use aerugo_formats::Game;
use aerugo_formats::script::{self, Operand, Script};
use aerugo_vfs::Vfs;

use super::filelist::FileList;

struct Loaded {
    path: String,
    game: Game,
    script: Script,
    worker: usize,
    lines: Vec<String>,
    /// Natives called anywhere in the script: id, name, signature, call count.
    natives: Vec<(u16, String, String, usize)>,
}

pub struct ScriptBrowser {
    files: Option<FileList>,
    loaded: Option<Loaded>,
    error: Option<String>,
    find: String,
}

impl std::fmt::Debug for ScriptBrowser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptBrowser").finish_non_exhaustive()
    }
}

fn game_of(path: &str) -> Game {
    if path.to_ascii_lowercase().contains("/ffx2/") { Game::Ffx2 } else { Game::Ffx }
}

impl ScriptBrowser {
    pub fn new() -> ScriptBrowser {
        ScriptBrowser { files: None, loaded: None, error: None, find: String::new() }
    }

    fn open(&mut self, vfs: &Vfs, path: String) {
        self.loaded = None;
        self.error = None;
        let data = match vfs.read(&path) {
            Ok(d) => d,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        if data.is_empty() {
            self.error = Some("This file is empty.".into());
            return;
        }
        match Script::parse(&data) {
            Ok(script) => {
                let game = game_of(&path);
                let mut counts: BTreeMap<u16, usize> = BTreeMap::new();
                for w in 0..script.workers.len() {
                    for i in script.walk(w) {
                        if i.info().operand == Operand::Native
                            && let Some(id) = i.operand
                        {
                            *counts.entry(id).or_default() += 1;
                        }
                    }
                }
                let mut natives: Vec<_> = counts
                    .into_iter()
                    .map(|(id, n)| {
                        let nat = script::native(game, id);
                        (id, nat.display(id), nat.signature(), n)
                    })
                    .collect();
                natives.sort_by(|a, b| b.3.cmp(&a.3).then(a.0.cmp(&b.0)));
                let mut l = Loaded { path, game, script, worker: 0, lines: Vec::new(), natives };
                l.lines = l.script.disassemble(0, game).lines().map(str::to_owned).collect();
                self.loaded = Some(l);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Arc<Vfs>) {
        let files = self.files.get_or_insert_with(|| FileList::new(vfs, script::is_script_path));
        let mut picked = None;
        egui::Panel::left("script-files").resizable(true).default_size(300.0).show(ui, |ui| {
            picked = files.ui(ui);
        });
        if let Some(path) = picked {
            self.open(vfs, path);
        }
        if let Some(l) = &mut self.loaded {
            egui::Panel::right("script-natives").resizable(true).default_size(320.0).show(ui, |ui| {
                ui.strong(format!("Natives called ({})", l.natives.len()));
                let named = l.natives.iter().filter(|n| n.2 != "unnamed").count();
                ui.label(format!("{named} named in our table"));
                ui.separator();
                egui::ScrollArea::vertical().id_salt("natives").show(ui, |ui| {
                    egui::Grid::new("natives-grid").striped(true).show(ui, |ui| {
                        for (id, name, sig, n) in &l.natives {
                            ui.monospace(format!("{id:04x}"));
                            ui.label(name).on_hover_text(sig);
                            ui.label(n.to_string());
                            ui.end_row();
                        }
                    });
                });
            });
        }
        egui::CentralPanel::default_margins().show(ui, |ui| {
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            let Some(l) = &mut self.loaded else {
                if self.error.is_none() {
                    ui.label("Pick a script on the left: event scripts (.ebp) and battle scripts (.bin).");
                }
                return;
            };
            ui.label(format!(
                "{} · {} · {} workers · {} bytes of code",
                l.path,
                l.game.name(),
                l.script.workers.len(),
                l.script.code_len()
            ));
            ui.horizontal(|ui| {
                let before = l.worker;
                egui::ComboBox::from_label("worker")
                    .selected_text(format!("worker {}", l.worker))
                    .height(400.0)
                    .show_ui(ui, |ui| {
                        for (i, w) in l.script.workers.iter().enumerate() {
                            ui.selectable_value(
                                &mut l.worker,
                                i,
                                format!("worker {i} (kind {}, {} entries)", w.kind, w.entries.len()),
                            );
                        }
                    });
                if before != l.worker {
                    l.lines = l.script.disassemble(l.worker, l.game).lines().map(str::to_owned).collect();
                }
                ui.label("find");
                ui.text_edit_singleline(&mut self.find);
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(l.lines.join("\n"));
                }
            });
            ui.separator();
            let find = self.find.to_ascii_lowercase();
            let shown: Vec<usize> = if find.is_empty() {
                (0..l.lines.len()).collect()
            } else {
                (0..l.lines.len()).filter(|&i| l.lines[i].to_ascii_lowercase().contains(&find)).collect()
            };
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
            egui::ScrollArea::both().auto_shrink(false).show_rows(ui, row_h, shown.len(), |ui, range| {
                for &i in &shown[range] {
                    let line = &l.lines[i];
                    let text = egui::RichText::new(line).monospace();
                    let text = if line.starts_with(';') {
                        text.color(ui.visuals().weak_text_color())
                    } else if line.ends_with(':') {
                        text.strong()
                    } else {
                        text
                    };
                    ui.add(egui::Label::new(text).extend());
                }
            });
        });
    }
}
