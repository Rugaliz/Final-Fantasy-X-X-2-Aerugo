//! Texture browser: every `*.dds.phyre` texture of both games, any level and cube face.

use aerugo_debug_ui::egui;
use aerugo_formats::phyre::Cluster;
use aerugo_formats::texture::{Texture, read_textures};
use aerugo_gfx::{Renderer, TextureHandle};
use aerugo_vfs::Vfs;

use super::filelist::FileList;

#[derive(Debug)]
pub struct TextureBrowser {
    files: Option<FileList>,
    shown: Option<Shown>,
    level: u32,
    face: u32,
    zoom: f32,
    nearest: bool,
    checker: bool,
}

#[derive(Debug)]
struct Shown {
    path: String,
    info: Result<(Texture, TextureHandle), String>,
}

impl TextureBrowser {
    pub fn new() -> TextureBrowser {
        TextureBrowser {
            files: None,
            shown: None,
            level: 0,
            face: 0,
            zoom: 1.0,
            nearest: false,
            checker: true,
        }
    }

    /// Free GPU memory.
    pub fn close(&mut self, renderer: &mut Renderer) {
        if let Some(Shown { info: Ok((_, h)), .. }) = self.shown.take() {
            renderer.destroy_texture(h);
        }
    }

    fn open(&mut self, vfs: &Vfs, renderer: &mut Renderer, path: String) {
        self.close(renderer);
        self.level = 0;
        self.face = 0;
        let info = (|| -> Result<(Texture, TextureHandle), String> {
            let cluster =
                Cluster::parse(vfs.read(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            let texture = read_textures(&cluster).into_iter().next().ok_or("no texture in this cluster")?;
            let texture = texture.map_err(|e| e.to_string())?;
            let desc = crate::assets::texture_desc(&texture, cluster.texture_data())
                .ok_or_else(|| format!("format {} is not supported yet", texture.format_name))?;
            let handle = renderer.create_texture(&desc).map_err(|e| format!("{e:#}"))?;
            Ok((texture, handle))
        })();
        self.shown = Some(Shown { path, info });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Vfs, renderer: &mut Renderer) {
        let files = self.files.get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".dds.phyre")));
        let mut picked = None;
        egui::Panel::left("texture-files").resizable(true).default_size(360.0).show(ui, |ui| {
            picked = files.ui(ui);
        });
        if let Some(path) = picked {
            self.open(vfs, renderer, path);
        }

        egui::CentralPanel::default_margins().show(ui, |ui| {
            let Some(shown) = &self.shown else {
                ui.label("Pick a texture on the left. Scroll to zoom with Ctrl held.");
                return;
            };
            ui.monospace(&shown.path);
            let (tex, handle) = match &shown.info {
                Ok((t, h)) => (t, *h),
                Err(e) => {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                    return;
                }
            };
            let levels = tex.levels.len() as u32 / tex.faces;
            ui.label(format!(
                "{} · {} {}×{} · {} levels{}",
                tex.name,
                tex.format_name,
                tex.width,
                tex.height,
                levels,
                if tex.faces == 6 { " · cube map" } else { "" }
            ));
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut self.level, 0..=levels - 1).text("level"));
                if tex.faces > 1 {
                    ui.add(egui::Slider::new(&mut self.face, 0..=tex.faces - 1).text("face"));
                }
                ui.add(egui::Slider::new(&mut self.zoom, 0.125..=8.0).logarithmic(true).text("zoom"));
                if ui.button("1:1").clicked() {
                    self.zoom = 1.0;
                }
                ui.checkbox(&mut self.nearest, "Nearest");
                ui.checkbox(&mut self.checker, "Checkerboard");
            });
            let w = (tex.width >> self.level).max(1) as f32;
            let h = (tex.height >> self.level).max(1) as f32;
            let id = match renderer.ui_texture(handle, self.face, self.level, self.nearest) {
                Ok(id) => id,
                Err(e) => {
                    ui.colored_label(ui.visuals().warn_fg_color, format!("{e:#}"));
                    return;
                }
            };
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                if ui.input(|i| i.modifiers.ctrl) {
                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll != 0.0 {
                        self.zoom = (self.zoom * (1.0 + scroll * 0.002)).clamp(0.125, 8.0);
                    }
                }
                // Show at least 256 points so small mips stay visible.
                let scale =
                    self.zoom * (256.0 / w.max(h)).max(1.0).min(if self.level == 0 { 1.0 } else { f32::MAX });
                let size = egui::vec2(w * scale, h * scale);
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                if self.checker {
                    paint_checker(ui.painter(), rect);
                }
                egui::Image::new((id, size)).paint_at(ui, rect);
            });
        });
    }
}

fn paint_checker(painter: &egui::Painter, rect: egui::Rect) {
    const CELL: f32 = 12.0;
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(90));
    let clip = painter.clip_rect().intersect(rect);
    let (x0, y0) = (
        ((clip.min.x - rect.min.x) / CELL).floor() as i32,
        ((clip.min.y - rect.min.y) / CELL).floor() as i32,
    );
    let (x1, y1) =
        (((clip.max.x - rect.min.x) / CELL).ceil() as i32, ((clip.max.y - rect.min.y) / CELL).ceil() as i32);
    for y in y0.max(0)..y1 {
        for x in x0.max(0)..x1 {
            if (x + y) % 2 == 0 {
                let min = rect.min + egui::vec2(x as f32 * CELL, y as f32 * CELL);
                let cell = egui::Rect::from_min_size(min, egui::vec2(CELL, CELL)).intersect(rect);
                painter.rect_filled(cell, 0.0, egui::Color32::from_gray(140));
            }
        }
    }
}
