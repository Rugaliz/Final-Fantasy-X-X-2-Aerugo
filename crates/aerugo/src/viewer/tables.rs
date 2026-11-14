//! Data table browser: every table of both games, with decoded text, in any text language.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use aerugo_debug_ui::egui;
use aerugo_formats::kernel::{self, Layout, TableFile};
use aerugo_formats::text::{self, Charset};
use aerugo_formats::{Game, KernelTable};
use aerugo_vfs::{Language, Vfs};

const ID_WIDTH: f32 = 56.0;
const TEXT_WIDTH: f32 = 260.0;

#[derive(Debug)]
pub struct TableBrowser {
    game: Game,
    lang: Language,
    files: Vec<TableFile>,
    selected: Option<usize>,
    loaded: Option<Loaded>,
    filter: String,
    show_hex: bool,
    charsets: HashMap<(Game, Language), Option<Arc<Charset>>>,
}

/// A table read and decoded for display.
#[derive(Debug)]
struct Loaded {
    resolved_path: String,
    file_lang: Language,
    summary: String,
    fields: Vec<usize>,
    rows: Vec<Row>,
    error: Option<String>,
}

#[derive(Debug)]
struct Row {
    id: u32,
    texts: Vec<String>,
    hex: String,
    /// Lower-cased text and id for filtering.
    search: String,
}

impl TableBrowser {
    pub fn new(lang: Language) -> TableBrowser {
        TableBrowser {
            game: Game::Ffx,
            lang,
            files: Vec::new(),
            selected: None,
            loaded: None,
            filter: String::new(),
            show_hex: false,
            charsets: HashMap::new(),
        }
    }

    /// Re-list the tables of the current game.
    pub fn refresh(&mut self, vfs: &Vfs) {
        self.files = kernel::list_tables(vfs, self.game);
        self.selected = None;
        self.loaded = None;
    }

    fn charset(&mut self, vfs: &Vfs, lang: Language) -> Option<Arc<Charset>> {
        self.charsets
            .entry((self.game, lang))
            .or_insert_with(|| {
                let path = text::charset_path(self.game, lang);
                let parsed = vfs
                    .read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| Charset::parse(&b).map_err(|e| e.to_string()));
                match parsed {
                    Ok(cs) => Some(Arc::new(cs)),
                    Err(e) => {
                        log::warn!("{path}: {e}");
                        None
                    }
                }
            })
            .clone()
    }

    fn load(&mut self, vfs: &Vfs) {
        let Some(file) = self.selected.and_then(|i| self.files.get(i)).cloned() else {
            self.loaded = None;
            return;
        };
        self.loaded = Some(self.decode(vfs, &file));
    }

    fn decode(&mut self, vfs: &Vfs, file: &TableFile) -> Loaded {
        let mut out = Loaded {
            resolved_path: file.base_path.clone(),
            file_lang: self.lang,
            summary: String::new(),
            fields: Vec::new(),
            rows: Vec::new(),
            error: None,
        };
        let Some((path, _)) = vfs.locate_localized(&file.base_path, self.lang) else {
            out.error = Some("Not present for this language.".into());
            return out;
        };
        out.file_lang = text::folder_language(&path).unwrap_or(self.lang);
        out.resolved_path = path.clone();
        let bytes = match vfs.read(&path) {
            Ok(b) => b,
            Err(e) => {
                out.error = Some(e.to_string());
                return out;
            }
        };
        let size = bytes.len();
        let table = match KernelTable::parse(bytes) {
            Ok(t) => t,
            Err(e) => {
                out.summary = format!("{size} bytes");
                out.error = Some(format!("{e} (other format; viewer support comes later)"));
                return out;
            }
        };
        let charset = self.charset(vfs, out.file_lang);
        out.fields = table.text_fields();
        let layout = match table.layout() {
            Layout::Ffx => "FFX",
            Layout::Ffx2 => "FFX-2",
        };
        let ranges: Vec<String> =
            table.segments().iter().map(|s| format!("{:#x}–{:#x}", s.first_id, s.last_id)).collect();
        out.summary = format!(
            "{layout} layout · {} records × {:#x} bytes · ids {} · {} text fields · {size} bytes",
            table.len(),
            table.record_size(),
            ranges.join(", "),
            out.fields.len()
        );
        for r in table.records() {
            let texts: Vec<String> = out
                .fields
                .iter()
                .map(|&f| match (table.text(&r, f), &charset) {
                    (Some(s), Some(cs)) => text::decode_display(s, cs),
                    (Some(_), None) => "(no charset)".into(),
                    (None, _) => String::new(),
                })
                .collect();
            let mut hex = String::with_capacity(r.bytes.len() * 3);
            for (i, b) in r.bytes.iter().enumerate() {
                if i > 0 {
                    hex.push(if i % 4 == 0 { ' ' } else { '\u{2009}' });
                }
                let _ = write!(hex, "{b:02x}");
            }
            let search = format!("{:x} {}", r.id, texts.join(" ")).to_lowercase();
            out.rows.push(Row { id: r.id, texts, hex, search });
        }
        out
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Arc<Vfs>) {
        let mut reload = false;
        let mut relist = false;
        egui::Panel::top("tables-options").show(ui, |ui| {
            ui.horizontal(|ui| {
                for game in Game::ALL {
                    if ui.selectable_value(&mut self.game, game, game.name()).changed() {
                        relist = true;
                    }
                }
                ui.separator();
                egui::ComboBox::from_id_salt("tables-lang").selected_text(self.lang.name()).show_ui(
                    ui,
                    |ui| {
                        for lang in Language::ALL {
                            if ui.selectable_value(&mut self.lang, lang, lang.name()).changed() {
                                reload = true;
                            }
                        }
                    },
                );
                ui.separator();
                ui.label("Filter");
                ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(180.0));
                ui.checkbox(&mut self.show_hex, "Raw bytes");
            });
        });
        if relist {
            self.refresh(vfs);
        }

        egui::Panel::left("tables-list").resizable(true).default_size(200.0).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let mut group = "";
                for (i, f) in self.files.iter().enumerate() {
                    if f.group != group {
                        group = &f.group;
                        ui.add_space(4.0);
                        ui.weak(group);
                    }
                    if ui.selectable_label(self.selected == Some(i), &f.name).clicked() {
                        self.selected = Some(i);
                        reload = true;
                    }
                }
            });
        });
        if reload {
            self.load(vfs);
        }

        egui::CentralPanel::default_margins().show(ui, |ui| {
            let Some(loaded) = &self.loaded else {
                ui.label(format!("{} tables. Pick one on the left.", self.files.len()));
                return;
            };
            ui.monospace(&loaded.resolved_path);
            if loaded.file_lang != self.lang {
                ui.label(format!("No {} version; showing {}.", self.lang.name(), loaded.file_lang.name()));
            }
            if !loaded.summary.is_empty() {
                ui.label(&loaded.summary);
            }
            if let Some(e) = &loaded.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
                return;
            }
            ui.separator();
            let filter = self.filter.to_lowercase();
            let rows: Vec<&Row> =
                loaded.rows.iter().filter(|r| filter.is_empty() || r.search.contains(&filter)).collect();
            let row_height =
                ui.text_style_height(&egui::TextStyle::Monospace).max(ui.spacing().interact_size.y);
            let show_hex = self.show_hex;
            ui.horizontal(|ui| {
                cell(ui, ID_WIDTH, row_height, egui::Label::new(egui::RichText::new("id").strong()));
                for f in &loaded.fields {
                    let head = egui::RichText::new(format!("text @{f:#x}")).strong();
                    cell(ui, TEXT_WIDTH, row_height, egui::Label::new(head));
                }
                if show_hex {
                    ui.strong("record bytes");
                }
            });
            egui::ScrollArea::both().auto_shrink([false, false]).show_rows(
                ui,
                row_height,
                rows.len(),
                |ui, range| {
                    for row in &rows[range] {
                        ui.horizontal(|ui| {
                            let id = egui::RichText::new(format!("{:04x}", row.id)).monospace();
                            cell(ui, ID_WIDTH, row_height, egui::Label::new(id));
                            for t in &row.texts {
                                let one_line = t.replace('\n', " ⏎ ");
                                let label =
                                    cell(ui, TEXT_WIDTH, row_height, egui::Label::new(one_line).truncate());
                                if t.chars().count() > 30 || t.contains('\n') {
                                    label.on_hover_text(t);
                                }
                            }
                            if show_hex {
                                ui.label(egui::RichText::new(&row.hex).monospace().weak());
                            }
                        });
                    }
                },
            );
        });
    }
}

/// A fixed-width, left-aligned table cell.
fn cell(ui: &mut egui::Ui, width: f32, height: f32, label: egui::Label) -> egui::Response {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.add(label)
        },
    )
    .inner
}
