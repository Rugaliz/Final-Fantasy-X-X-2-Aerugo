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
    culling: bool,
    background: [f32; 3],
    /// Draw skinned meshes through their skeleton (default pose) instead of the stored bind pose.
    skinning: bool,
    show_bones: bool,
    /// (mesh, joint) being rotated by the test sliders.
    selected: Option<(usize, usize)>,
    test_rotation: [f32; 3],
    motions: Vec<(String, aerugo_formats::motion::Motion)>,
    motion: Option<usize>,
    /// Current frame (fractional).
    time: f32,
    playing: bool,
    speed: f32,
    /// Interpolate between frames (off shows the 30 Hz steps).
    interpolate: bool,
    translation_scale: f32,
}

impl ModelBrowser {
    pub fn new() -> ModelBrowser {
        ModelBrowser {
            files: None,
            loaded: None,
            camera: OrbitCamera::framing(Vec3::splat(-1.0), Vec3::splat(1.0)),
            lit: true,
            culling: true,
            background: [0.18, 0.19, 0.22],
            skinning: true,
            show_bones: false,
            selected: None,
            test_rotation: [0.0; 3],
            motions: Vec::new(),
            motion: None,
            time: 0.0,
            playing: true,
            speed: 1.0,
            interpolate: true,
            translation_scale: 0.01,
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
        self.selected = None;
        self.test_rotation = [0.0; 3];
        self.motions = load_motions(vfs, &path);
        self.motion = None;
        self.time = 0.0;
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
            self.advance_motion(ui);
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
            let mut draws = Vec::new();
            let mut joints = Vec::new();
            let mut overlay = Vec::new();
            for (mi, m) in model.meshes.iter().enumerate() {
                let posed = self.posed_world(mi, m);
                match (&m.skeleton, &posed) {
                    (Some(sk), Some(world)) if self.skinning && m.skinned => {
                        for (pi, &si) in m.part_segments.iter().enumerate() {
                            let seg = &model.model.meshes[m.source].segments[si];
                            let base = joints.len() as u32;
                            joints.extend(sk.palette(world, seg));
                            draws.push(Draw {
                                mesh: m.mesh,
                                model: m.world.to_cols_array(),
                                joint_base: Some(base),
                                lit: self.lit,
                                part: Some(pi),
                            });
                        }
                    }
                    _ => draws.push(Draw {
                        mesh: m.mesh,
                        model: m.world.to_cols_array(),
                        joint_base: None,
                        lit: self.lit,
                        part: None,
                    }),
                }
                if let (Some(sk), Some(world)) = (&m.skeleton, posed) {
                    overlay.push((
                        mi,
                        sk.parents.clone(),
                        sk.names.clone(),
                        world
                            .iter()
                            .map(|w| m.world.transform_point3(Vec3::new(w[12], w[13], w[14])))
                            .collect::<Vec<_>>(),
                    ));
                }
            }
            let [r, g, b] = self.background;
            renderer.submit_scene(SceneView {
                view_proj: view_proj.to_cols_array(),
                clear: [r, g, b, 1.0],
                draws,
                joints,
                culling: self.culling,
                mirrored: false,
                fog: None,
                time: 0.0,
                color_correction: None,
            });
            if self.show_bones {
                self.draw_bones(ui, &response, view_proj, &overlay);
            }
        });
    }

    /// World matrices of a mesh's joints in the current pose (default pose plus the test rotation).
    fn posed_world(&self, mi: usize, m: &crate::assets::GpuMeshInstance) -> Option<Vec<[f32; 16]>> {
        let sk = m.skeleton.as_ref()?;
        let mut local = sk.default_pose.clone();
        if let Some((_, motion)) = self.motion.and_then(|i| self.motions.get(i)) {
            let frame = if self.interpolate { self.time } else { self.time.floor() };
            for (i, name) in sk.names.iter().enumerate() {
                if let Some(j) =
                    aerugo_formats::motion::ps2_joint(name).filter(|&j| j < motion.joints as usize)
                {
                    local[i] = motion.joint_at(j, frame).matrix(self.translation_scale);
                }
            }
        }
        if let Some((sm, j)) = self.selected
            && sm == mi
            && let Some(l) = local.get_mut(j)
        {
            let [x, y, z] = self.test_rotation.map(f32::to_radians);
            let rot = Mat4::from_euler(glam::EulerRot::XYZ, x, y, z);
            *l = (Mat4::from_cols_array(l) * rot).to_cols_array();
        }
        Some(sk.world(&local))
    }

    #[allow(clippy::type_complexity)]
    fn draw_bones(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        view_proj: Mat4,
        overlay: &[(usize, Vec<i32>, Vec<String>, Vec<Vec3>)],
    ) {
        let rect = response.rect;
        let painter = ui.painter_at(rect);
        let project = |p: Vec3| -> Option<egui::Pos2> {
            let c = view_proj * p.extend(1.0);
            (c.w > 0.0).then(|| {
                egui::pos2(
                    rect.min.x + (c.x / c.w + 1.0) * 0.5 * rect.width(),
                    rect.min.y + (c.y / c.w + 1.0) * 0.5 * rect.height(),
                )
            })
        };
        let hover = response.hover_pos();
        let mut hovered: Option<(usize, usize, egui::Pos2)> = None;
        for (mi, parents, names, points) in overlay {
            let screen: Vec<Option<egui::Pos2>> = points.iter().map(|&p| project(p)).collect();
            for (j, p) in screen.iter().enumerate() {
                let Some(p) = *p else { continue };
                if let Some(&parent) = parents.get(j)
                    && parent >= 0
                    && let Some(Some(q)) = screen.get(parent as usize)
                {
                    painter
                        .line_segment([p, *q], egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 210, 60)));
                }
                let selected = self.selected == Some((*mi, j));
                let color = if selected {
                    egui::Color32::from_rgb(255, 80, 80)
                } else {
                    egui::Color32::from_rgb(90, 220, 255)
                };
                painter.circle_filled(p, if selected { 4.5 } else { 2.5 }, color);
                if let Some(h) = hover
                    && h.distance(p) < 12.0
                    && hovered.is_none_or(|(_, _, best)| h.distance(p) < h.distance(best))
                {
                    hovered = Some((*mi, j, p));
                }
            }
            if let Some((hm, j, p)) = hovered
                && hm == *mi
            {
                let name = names.get(j).cloned().unwrap_or_default();
                painter.text(
                    p + egui::vec2(8.0, -8.0),
                    egui::Align2::LEFT_BOTTOM,
                    format!("{j}: {name}"),
                    egui::FontId::proportional(13.0),
                    egui::Color32::WHITE,
                );
            }
        }
        if response.clicked()
            && let Some((mi, j, _)) = hovered
        {
            self.selected = Some((mi, j));
            self.test_rotation = [0.0; 3];
        }
    }

    fn advance_motion(&mut self, ui: &egui::Ui) {
        let Some((_, motion)) = self.motion.and_then(|i| self.motions.get(i)) else { return };
        let frames = motion.frames.max(1) as f32;
        if self.playing {
            let dt = ui.input(|i| i.stable_dt).min(0.1);
            let rate = f32::from(motion.rate.max(1)) / f32::from(0x1E00u16);
            self.time += dt * 30.0 * rate * self.speed;
        }
        let last = (frames - 1.0).max(0.0);
        if self.time > last {
            self.time = if last > 0.0 { self.time % last } else { 0.0 };
        }
        if self.time < 0.0 {
            self.time = 0.0;
        }
    }

    fn select_motion(&mut self, index: usize) {
        self.motion = Some(index);
        self.time = 0.0;
        if let (Some((_, Ok(m))), Some((_, motion))) = (&self.loaded, self.motions.get(index))
            && let Some(sk) = m.meshes.iter().filter_map(|x| x.skeleton.as_ref()).max_by_key(|s| s.len())
        {
            self.translation_scale =
                aerugo_formats::motion::fit_translation_scale(&sk.names, &sk.default_pose, motion);
        }
    }

    fn motion_ui(&mut self, ui: &mut egui::Ui) {
        if self.motions.is_empty() {
            ui.weak("No PS2 motions found for this model.");
            return;
        }
        let current = self
            .motion
            .and_then(|i| self.motions.get(i))
            .map(|(l, _)| l.clone())
            .unwrap_or_else(|| "bind pose".into());
        let mut pick = None;
        egui::ComboBox::from_label("Motion").selected_text(current).height(500.0).show_ui(ui, |ui| {
            if ui.selectable_label(self.motion.is_none(), "bind pose").clicked() {
                self.motion = None;
            }
            for (i, (label, _)) in self.motions.iter().enumerate() {
                if ui.selectable_label(self.motion == Some(i), label).clicked() {
                    pick = Some(i);
                }
            }
        });
        if let Some(i) = pick {
            self.select_motion(i);
        }
        if let Some((_, motion)) = self.motion.and_then(|i| self.motions.get(i)) {
            let last = (motion.frames.max(1) - 1) as f32;
            ui.horizontal(|ui| {
                if ui.button(if self.playing { "Pause" } else { "Play" }).clicked() {
                    self.playing = !self.playing;
                }
                ui.add(egui::Slider::new(&mut self.speed, 0.05..=4.0).logarithmic(true).text("speed"));
            });
            ui.add(egui::Slider::new(&mut self.time, 0.0..=last).text("frame"));
            ui.checkbox(&mut self.interpolate, "Interpolate between frames");
            ui.weak(format!("{} joints · translation scale {:.5}", motion.joints, self.translation_scale));
        }
    }

    fn info_ui(&mut self, ui: &mut egui::Ui) {
        self.motion_ui(ui);
        ui.separator();
        ui.checkbox(&mut self.lit, "Lighting");
        ui.checkbox(&mut self.culling, "Face culling");
        ui.checkbox(&mut self.skinning, "Skinning (default pose)").on_hover_text(
            "Off: the stored bind-pose vertices. On: vertices moved by the skeleton in its default pose.",
        );
        ui.checkbox(&mut self.show_bones, "Bones")
            .on_hover_text("Click a joint to select it for the rotation test.");
        if let Some((_, Ok(m))) = &self.loaded
            && let Some((mi, sk)) =
                m.meshes.iter().enumerate().find_map(|(i, x)| x.skeleton.as_ref().map(|s| (i, s)))
        {
            let current = self.selected.filter(|&(s, _)| s == mi).map(|(_, j)| j);
            let label = current
                .map(|j| format!("{j}: {}", sk.names.get(j).cloned().unwrap_or_default()))
                .unwrap_or_else(|| "pick a joint".into());
            egui::ComboBox::from_label("Joint").selected_text(label).height(400.0).show_ui(ui, |ui| {
                for (j, name) in sk.names.iter().enumerate() {
                    if ui.selectable_label(current == Some(j), format!("{j}: {name}")).clicked() {
                        self.selected = Some((mi, j));
                        self.test_rotation = [0.0; 3];
                    }
                }
            });
        }
        if let Some((mi, j)) = self.selected {
            ui.label(format!("Joint {j} of mesh {mi}: rotate to check the skin weights"));
            for (axis, v) in ["X", "Y", "Z"].iter().zip(self.test_rotation.iter_mut()) {
                ui.add(egui::Slider::new(v, -180.0..=180.0).text(*axis).suffix("°"));
            }
            if ui.button("Clear joint").clicked() {
                self.selected = None;
            }
        }
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
                        for (p, fx) in mesh.parts.iter().zip(&mesh.part_effects) {
                            ui.label(p).on_hover_text(format!("effect {fx}"));
                        }
                    });
                }
            }
        }
    }
}

/// Every motion in the PS2 motion groups that belong to a character model.
pub(super) fn load_motions(vfs: &Vfs, model_path: &str) -> Vec<(String, aerugo_formats::motion::Motion)> {
    use aerugo_formats::motion::{MotionGroup, motion_dir};
    let Some(dir) = motion_dir(model_path) else { return Vec::new() };
    let mut paths: Vec<String> = vfs
        .archives()
        .flat_map(|(_, a)| {
            a.entries()
                .iter()
                .filter(|e| e.path.starts_with(&dir) && e.path.ends_with(".mgrp") && e.size > 16)
                .map(|e| e.path.clone())
        })
        .collect();
    paths.sort();
    paths.dedup();
    let mut out = Vec::new();
    for path in paths {
        let Ok(bytes) = vfs.read(&path) else { continue };
        let Ok(group) = MotionGroup::parse(&bytes) else { continue };
        let file = path.rsplit('/').next().unwrap_or(&path).trim_end_matches(".mgrp").to_owned();
        for (i, m) in group.motions.into_iter().enumerate() {
            out.push((format!("{file} #{i} ({} frames)", m.frames), m));
        }
    }
    out
}
