//! Model browser: any `*.dae.phyre` model of both games in a 3D viewport with an orbit camera.

use aerugo_debug_ui::egui;
use aerugo_gfx::{Draw, Renderer, SceneView};
use aerugo_vfs::Vfs;
use glam::{Mat4, Vec3};

use super::filelist::FileList;
use crate::assets::{GpuModel, load_model};

/// The games keep the PS2 convention of Y pointing down; viewers turn the world half a turn around X so up
/// is up (a rotation, so triangle winding is unchanged).
pub fn game_to_view() -> Mat4 {
    Mat4::from_rotation_x(std::f32::consts::PI)
}

/// Orbit camera around a target point in view space (Y up).
#[derive(Debug, Clone, Copy)]
pub struct OrbitCamera {
    pub target: Vec3,
    pub distance: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub fov_y: f32,
}

impl OrbitCamera {
    /// Frame a box given in game coordinates.
    pub fn framing(min: Vec3, max: Vec3) -> OrbitCamera {
        let (a, b) = (game_to_view().transform_point3(min), game_to_view().transform_point3(max));
        let (min, max) = (a.min(b), a.max(b));
        let radius = ((max - min).length() * 0.5).max(0.01);
        OrbitCamera {
            target: (min + max) * 0.5,
            distance: radius * 2.6,
            yaw: 0.6,
            pitch: 0.25,
            fov_y: 45f32.to_radians(),
        }
    }

    pub fn eye(&self) -> Vec3 {
        let dir =
            Vec3::new(self.pitch.cos() * self.yaw.sin(), self.pitch.sin(), self.pitch.cos() * self.yaw.cos());
        self.target + dir * self.distance
    }

    /// View-projection for Vulkan clip space (y down, depth 0–1).
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let near = (self.distance * 0.01).max(0.001);
        let far = self.distance * 100.0 + 1000.0;
        let proj = Mat4::perspective_rh(self.fov_y, aspect.max(0.01), near, far);
        let flip = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0));
        flip * proj * Mat4::look_at_rh(self.eye(), self.target, Vec3::Y) * game_to_view()
    }

    /// Apply mouse input from a viewport response.
    pub fn handle(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.yaw -= d.x * 0.008;
            self.pitch = (self.pitch + d.y * 0.008).clamp(-1.55, 1.55);
        }
        if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            let d = response.drag_delta();
            let forward = (self.target - self.eye()).normalize_or_zero();
            let right = forward.cross(Vec3::Y).normalize_or_zero();
            let up = right.cross(forward);
            let scale = self.distance * 0.0015;
            self.target += (-right * d.x + up * d.y) * scale;
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.distance = (self.distance * (1.0 - scroll * 0.0015)).max(0.001);
            }
        }
    }
}

#[derive(Debug)]
pub struct ModelBrowser {
    files: Option<FileList>,
    loaded: Option<(String, Result<GpuModel, String>)>,
    camera: OrbitCamera,
    lit: bool,
    background: [f32; 3],
}

impl ModelBrowser {
    pub fn new() -> ModelBrowser {
        ModelBrowser {
            files: None,
            loaded: None,
            camera: OrbitCamera::framing(Vec3::splat(-1.0), Vec3::splat(1.0)),
            lit: true,
            background: [0.18, 0.19, 0.22],
        }
    }

    pub fn close(&mut self, renderer: &mut Renderer) {
        if let Some((_, Ok(m))) = self.loaded.take() {
            m.destroy(renderer);
        }
        renderer.release_viewport();
    }

    fn open(&mut self, vfs: &Vfs, renderer: &mut Renderer, path: String) {
        if let Some((_, Ok(m))) = self.loaded.take() {
            m.destroy(renderer);
        }
        let result = load_model(vfs, renderer, &path);
        if let Ok(m) = &result {
            self.camera = OrbitCamera::framing(m.min, m.max);
        }
        self.loaded = Some((path, result));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Vfs, renderer: &mut Renderer) {
        let files = self.files.get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".dae.phyre")));
        let mut picked = None;
        egui::Panel::left("model-files").resizable(true).default_size(340.0).show(ui, |ui| {
            picked = files.ui(ui);
        });
        if let Some(path) = picked {
            self.open(vfs, renderer, path);
        }

        egui::Panel::right("model-info").resizable(true).default_size(300.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.info_ui(ui));
        });

        egui::CentralPanel::default_margins().show(ui, |ui| {
            let Some((_, Ok(model))) = &self.loaded else {
                ui.label(match &self.loaded {
                    Some((_, Err(e))) => e.clone(),
                    _ => "Pick a model on the left. Drag to orbit, right-drag to pan, scroll to zoom.".into(),
                });
                return;
            };
            let size = ui.available_size();
            let ppp = ui.ctx().pixels_per_point();
            let px = ((size.x * ppp).round().max(1.0) as u32, (size.y * ppp).round().max(1.0) as u32);
            let id = match renderer.viewport_texture(px) {
                Ok(id) => id,
                Err(e) => {
                    ui.label(format!("{e:#}"));
                    return;
                }
            };
            let response = ui.add(egui::Image::new((id, size)).sense(egui::Sense::click_and_drag()));
            self.camera.handle(ui, &response);
            let view_proj = self.camera.view_proj(size.x / size.y.max(1.0));
            let draws = model
                .meshes
                .iter()
                .map(|m| Draw {
                    mesh: m.mesh,
                    model: m.world.to_cols_array(),
                    joint_base: None,
                    lit: self.lit,
                    part: None,
                })
                .collect();
            let [r, g, b] = self.background;
            renderer.submit_scene(SceneView {
                view_proj: view_proj.to_cols_array(),
                clear: [r, g, b, 1.0],
                draws,
                joints: Vec::new(),
            });
        });
    }

    fn info_ui(&mut self, ui: &mut egui::Ui) {
        ui.checkbox(&mut self.lit, "Lighting");
        ui.horizontal(|ui| {
            ui.label("Background");
            ui.color_edit_button_rgb(&mut self.background);
        });
        if ui.button("Reset camera").clicked()
            && let Some((_, Ok(m))) = &self.loaded
        {
            self.camera = OrbitCamera::framing(m.min, m.max);
        }
        ui.separator();
        let Some((path, result)) = &self.loaded else { return };
        ui.monospace(path);
        match result {
            Err(e) => {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            Ok(m) => {
                ui.label(format!(
                    "{} meshes · {} vertices · {} triangles · {} textures",
                    m.meshes.len(),
                    m.vertices,
                    m.triangles,
                    m.textures.len()
                ));
                let size = m.max - m.min;
                ui.label(format!("size {:.2} × {:.2} × {:.2}", size.x, size.y, size.z));
                for w in &m.warnings {
                    ui.colored_label(ui.visuals().warn_fg_color, w);
                }
                for mesh in &m.meshes {
                    egui::CollapsingHeader::new(format!(
                        "{}{}",
                        mesh.name,
                        if mesh.skinned { " (skinned)" } else { "" }
                    ))
                    .id_salt(mesh.mesh)
                    .show(ui, |ui| {
                        for p in &mesh.parts {
                            ui.label(p);
                        }
                    });
                }
            }
        }
    }
}
