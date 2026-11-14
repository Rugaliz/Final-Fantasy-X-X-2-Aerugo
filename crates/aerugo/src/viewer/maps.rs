//! Map browser: a map's 3D model and its pre-rendered 2D layers, with orbit, fly and stored-camera views.
//! Stored cameras show the aspect policy: the frame the layers were made for is cropped (following a point)
//! or fitted at any viewport ratio.

use aerugo_debug_ui::egui;
use aerugo_formats::camera::{Camera, Projection, read_cameras};
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::aspect::{AspectMode, clip_transform};
use aerugo_gfx::{Draw, Renderer, SceneView};
use aerugo_vfs::Vfs;
use glam::{Mat4, Vec3};

use super::models::{OrbitCamera, game_to_view};
use crate::assets::{GpuModel, load_model};

#[derive(Debug, Clone, PartialEq, Eq)]
struct MapEntry {
    label: String,
    model: Option<String>,
    layers: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum View {
    Orbit,
    Fly,
    Scene(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Ratio {
    Window,
    Fixed(f32, &'static str),
}

const RATIOS: [Ratio; 5] = [
    Ratio::Window,
    Ratio::Fixed(4.0 / 3.0, "4:3"),
    Ratio::Fixed(16.0 / 9.0, "16:9"),
    Ratio::Fixed(21.0 / 9.0, "21:9"),
    Ratio::Fixed(32.0 / 9.0, "32:9"),
];

#[derive(Debug)]
struct Loaded {
    entry: MapEntry,
    model: Option<GpuModel>,
    layers: Option<GpuModel>,
    cameras: Vec<Camera>,
    /// Design aspect of the layers through each camera.
    design: Vec<f32>,
    errors: Vec<String>,
}

#[derive(Debug)]
pub struct MapBrowser {
    maps: Option<Vec<MapEntry>>,
    filter: String,
    loaded: Option<Loaded>,
    orbit: OrbitCamera,
    fly_pos: Vec3,
    fly_yaw: f32,
    fly_pitch: f32,
    fly_speed: f32,
    view: View,
    show_model: bool,
    show_layers: bool,
    mode: AspectMode,
    ratio: Ratio,
    follow: [f32; 2],
    lit: bool,
}

impl MapBrowser {
    pub fn new() -> MapBrowser {
        MapBrowser {
            maps: None,
            filter: String::new(),
            loaded: None,
            orbit: OrbitCamera::framing(Vec3::splat(-1.0), Vec3::splat(1.0)),
            fly_pos: Vec3::ZERO,
            fly_yaw: 0.0,
            fly_pitch: 0.0,
            fly_speed: 50.0,
            view: View::Orbit,
            show_model: true,
            show_layers: true,
            mode: AspectMode::Crop,
            ratio: Ratio::Window,
            follow: [0.0, 0.0],
            lit: false,
        }
    }

    pub fn close(&mut self, renderer: &mut Renderer) {
        if let Some(l) = self.loaded.take() {
            for m in [l.model, l.layers].into_iter().flatten() {
                m.destroy(renderer);
            }
        }
        renderer.release_viewport();
    }

    fn list(vfs: &Vfs) -> Vec<MapEntry> {
        use std::collections::BTreeMap;
        let mut maps: BTreeMap<String, MapEntry> = BTreeMap::new();
        for (_, a) in vfs.archives() {
            for e in a.entries() {
                let Some((root, rest)) = e.path.split_once("/gamedata/ps3data/map/") else { continue };
                let parts: Vec<&str> = rest.split('/').collect();
                // <area>/<map>/mdl/d3d11/<map>.dae.phyre or <area>/<map>/2d/mdl/d3d11/<map>.dae.phyre
                let (area, id, layers) = match parts.as_slice() {
                    [area, id, "mdl", "d3d11", file] if *file == format!("{id}.dae.phyre") => {
                        (*area, *id, false)
                    }
                    [area, id, "2d", "mdl", "d3d11", file] if *file == format!("{id}.dae.phyre") => {
                        (*area, *id, true)
                    }
                    _ => continue,
                };
                let game = if root.starts_with("ffx-2") { "X-2" } else { "X" };
                let key = format!("{game} {area}/{id}");
                let entry =
                    maps.entry(key.clone()).or_insert(MapEntry { label: key, model: None, layers: None });
                if layers {
                    entry.layers = Some(e.path.clone());
                } else {
                    entry.model = Some(e.path.clone());
                }
            }
        }
        maps.into_values().collect()
    }

    fn open(&mut self, vfs: &Vfs, renderer: &mut Renderer, entry: MapEntry) {
        self.close(renderer);
        let mut errors = Vec::new();
        let mut load = |p: &Option<String>| -> Option<GpuModel> {
            let p = p.as_ref()?;
            match load_model(vfs, renderer, p) {
                Ok(m) if !m.meshes.is_empty() => Some(m),
                Ok(m) => {
                    m.destroy(renderer);
                    None
                }
                Err(e) => {
                    errors.push(format!("{p}: {e}"));
                    None
                }
            }
        };
        let model = load(&entry.model);
        let layers = load(&entry.layers);
        let mut cameras = Vec::new();
        for p in [&entry.layers, &entry.model].into_iter().flatten() {
            if let Ok(c) = vfs
                .read(p)
                .map_err(|e| e.to_string())
                .and_then(|b| Cluster::parse(b).map_err(|e| e.to_string()))
            {
                // Leave out the authoring tool's default cameras.
                cameras.extend(
                    read_cameras(&c)
                        .into_iter()
                        .filter(|c| !["persp", "top", "front", "side"].contains(&c.name.as_str())),
                );
            }
        }
        // With layers, each camera is fitted to frame them exactly (field of view and frame shape).
        let fitted: Vec<Option<(f32, f32)>> =
            cameras.iter().map(|c| fit_layers(c, layers.as_ref())).collect();
        for (c, f) in cameras.iter_mut().zip(&fitted) {
            if let Some((fov_y, _)) = f {
                c.projection = Projection::Perspective { fov_y: *fov_y };
            }
        }
        let design = fitted.iter().map(|f| f.map_or(16.0 / 9.0, |(_, a)| a)).collect();
        // Frame whatever is there.
        let bounds = [&model, &layers].into_iter().flatten().fold(None, |acc: Option<(Vec3, Vec3)>, m| {
            Some(acc.map_or((m.min, m.max), |(a, b)| (a.min(m.min), b.max(m.max))))
        });
        if let Some((min, max)) = bounds {
            self.orbit = OrbitCamera::framing(min, max);
            self.fly_pos = self.orbit.eye();
            let dir = (self.orbit.target - self.orbit.eye()).normalize_or_zero();
            self.fly_yaw = dir.x.atan2(-dir.z);
            self.fly_pitch = dir.y.asin();
            self.fly_speed = ((max - min).length() * 0.2).max(1.0);
        }
        self.view = if layers.is_some() && !cameras.is_empty() { View::Scene(0) } else { View::Orbit };
        self.follow = [0.0, 0.0];
        self.loaded = Some(Loaded { entry, model, layers, cameras, design, errors });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Vfs, renderer: &mut Renderer) {
        let maps = self.maps.get_or_insert_with(|| Self::list(vfs));
        let mut picked = None;
        egui::Panel::left("map-list").resizable(true).default_size(240.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Filter");
                ui.text_edit_singleline(&mut self.filter);
            });
            let words: Vec<String> =
                self.filter.to_lowercase().split_whitespace().map(str::to_owned).collect();
            let shown: Vec<&MapEntry> =
                maps.iter().filter(|m| words.iter().all(|w| m.label.to_lowercase().contains(w))).collect();
            ui.weak(format!("{} of {} maps", shown.len(), maps.len()));
            let row = ui.text_style_height(&egui::TextStyle::Body) + 2.0;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
                ui,
                row,
                shown.len(),
                |ui, range| {
                    for m in &shown[range] {
                        let tags = format!(
                            "{}{}",
                            if m.model.is_some() { " 3D" } else { "" },
                            if m.layers.is_some() { " 2D" } else { "" }
                        );
                        let selected = self.loaded.as_ref().is_some_and(|l| l.entry.label == m.label);
                        if ui.selectable_label(selected, format!("{}{tags}", m.label)).clicked() {
                            picked = Some((*m).clone());
                        }
                    }
                },
            );
        });
        if let Some(entry) = picked {
            self.open(vfs, renderer, entry);
        }
        egui::Panel::right("map-info").resizable(true).default_size(280.0).show(ui, |ui| self.info_ui(ui));
        egui::CentralPanel::default_margins().show(ui, |ui| self.viewport_ui(ui, renderer));
    }

    fn info_ui(&mut self, ui: &mut egui::Ui) {
        let Some(l) = &self.loaded else {
            ui.label("Pick a map. Maps with 2D layers open through their stored camera.");
            return;
        };
        ui.strong(&l.entry.label);
        for e in &l.errors {
            ui.colored_label(ui.visuals().warn_fg_color, e);
        }
        ui.checkbox(&mut self.show_model, "3D model").on_disabled_hover_text("none");
        ui.checkbox(&mut self.show_layers, "2D layers");
        ui.checkbox(&mut self.lit, "Viewer lighting");
        ui.separator();
        ui.label("View");
        ui.radio_value(&mut self.view, View::Orbit, "Orbit (drag, right-drag, scroll)");
        ui.radio_value(&mut self.view, View::Fly, "Fly (WASD, Q/E, right-drag to look)");
        for (i, c) in l.cameras.iter().enumerate() {
            let fov = match c.projection {
                Projection::Perspective { fov_y } => fov_y.to_degrees(),
                Projection::Orthographic { .. } => 0.0,
            };
            ui.radio_value(
                &mut self.view,
                View::Scene(i),
                format!("Camera {} ({fov:.0}°, frame {:.2}:1)", c.name, l.design[i]),
            );
        }
        if let View::Fly = self.view {
            ui.add(egui::Slider::new(&mut self.fly_speed, 0.5..=2000.0).logarithmic(true).text("speed"));
        }
        if let View::Scene(_) = self.view {
            ui.separator();
            ui.label("Aspect policy");
            ui.radio_value(&mut self.mode, AspectMode::Crop, "Crop (fill, follow a point)");
            ui.radio_value(&mut self.mode, AspectMode::Fit, "Fit (whole frame, bars)");
            ui.horizontal_wrapped(|ui| {
                for r in RATIOS {
                    let label = match r {
                        Ratio::Window => "window",
                        Ratio::Fixed(_, l) => l,
                    };
                    ui.selectable_value(&mut self.ratio, r, label);
                }
            });
            ui.add(egui::Slider::new(&mut self.follow[0], -1.0..=1.0).text("follow x"));
            ui.add(egui::Slider::new(&mut self.follow[1], -1.0..=1.0).text("follow y"));
            ui.weak("Right-drag in the view to move the follow point.");
        }
    }

    fn viewport_ui(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer) {
        // The loaded map is moved out while the camera state (other fields) is updated.
        let loaded = self.loaded.take();
        if let Some(l) = &loaded {
            self.viewport_body(ui, renderer, l);
        }
        self.loaded = loaded;
    }

    fn viewport_body(&mut self, ui: &mut egui::Ui, renderer: &mut Renderer, l: &Loaded) {
        let avail = ui.available_size();
        // Letterbox the viewport to the chosen ratio so crop and fit can be seen at any shape.
        let size = match self.ratio {
            Ratio::Window => avail,
            Ratio::Fixed(r, _) => {
                if avail.x / avail.y.max(1.0) > r {
                    egui::vec2(avail.y * r, avail.y)
                } else {
                    egui::vec2(avail.x, avail.x / r)
                }
            }
        };
        let ppp = ui.ctx().pixels_per_point();
        let px = ((size.x * ppp).round().max(1.0) as u32, (size.y * ppp).round().max(1.0) as u32);
        let id = match renderer.viewport_texture(px) {
            Ok(id) => id,
            Err(e) => {
                ui.label(format!("{e:#}"));
                return;
            }
        };
        let response = ui
            .centered_and_justified(|ui| {
                ui.add(egui::Image::new((id, size)).sense(egui::Sense::click_and_drag()))
            })
            .inner;
        let aspect = size.x / size.y.max(1.0);
        let view_proj = match self.view {
            View::Orbit => {
                self.orbit.handle(ui, &response);
                self.orbit.view_proj(aspect)
            }
            View::Fly => {
                self.fly(ui, &response);
                let dir = Vec3::new(
                    self.fly_pitch.cos() * self.fly_yaw.sin(),
                    self.fly_pitch.sin(),
                    -self.fly_pitch.cos() * self.fly_yaw.cos(),
                );
                let proj = Mat4::perspective_rh(60f32.to_radians(), aspect, 0.1, 100_000.0);
                Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0))
                    * proj
                    * Mat4::look_at_rh(self.fly_pos, self.fly_pos + dir, Vec3::Y)
                    * game_to_view()
            }
            View::Scene(i) => {
                if response.dragged_by(egui::PointerButton::Secondary) {
                    let d = response.drag_delta();
                    self.follow[0] = (self.follow[0] + d.x / size.x * 2.0).clamp(-1.0, 1.0);
                    self.follow[1] = (self.follow[1] - d.y / size.y * 2.0).clamp(-1.0, 1.0);
                }
                let (Some(cam), Some(&design)) = (l.cameras.get(i), l.design.get(i)) else { return };
                let base = camera_view_proj(cam, design);
                let crop = clip_transform(design, aspect, self.mode, self.follow);
                Mat4::from_cols_array(&crop.matrix()) * base
            }
        };
        let mut draws = Vec::new();
        let mut add = |m: &GpuModel| {
            for mesh in &m.meshes {
                draws.push(Draw {
                    mesh: mesh.mesh,
                    model: mesh.world.to_cols_array(),
                    joint_base: None,
                    lit: self.lit,
                    part: None,
                });
            }
        };
        if self.show_model
            && let Some(m) = &l.model
        {
            add(m);
        }
        if self.show_layers
            && let Some(m) = &l.layers
        {
            add(m);
        }
        renderer.submit_scene(SceneView {
            view_proj: view_proj.to_cols_array(),
            clear: [0.0, 0.0, 0.0, 1.0],
            draws,
            joints: Vec::new(),
        });
    }

    fn fly(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Primary)
        {
            let d = response.drag_delta();
            self.fly_yaw += d.x * 0.005;
            self.fly_pitch = (self.fly_pitch - d.y * 0.005).clamp(-1.5, 1.5);
        }
        if !response.hovered() {
            return;
        }
        let dt = ui.input(|i| i.stable_dt).min(0.1);
        let forward = Vec3::new(
            self.fly_pitch.cos() * self.fly_yaw.sin(),
            self.fly_pitch.sin(),
            -self.fly_pitch.cos() * self.fly_yaw.cos(),
        );
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let mut v = Vec3::ZERO;
        ui.input(|i| {
            let k = |key| if i.key_down(key) { 1.0 } else { 0.0 };
            v += forward * (k(egui::Key::W) - k(egui::Key::S));
            v += right * (k(egui::Key::D) - k(egui::Key::A));
            v += Vec3::Y * (k(egui::Key::E) - k(egui::Key::Q));
        });
        self.fly_pos += v * self.fly_speed * dt;
    }
}

/// View-projection of a stored camera for its design aspect (Vulkan clip space). Cameras look down −Z.
fn camera_view_proj(cam: &Camera, design: f32) -> Mat4 {
    let world = Mat4::from_cols_array(&cam.world);
    let view = world.inverse();
    let proj = match cam.projection {
        Projection::Perspective { fov_y } => {
            Mat4::perspective_rh(fov_y, design, cam.near.max(0.001), cam.far.max(cam.near + 1.0))
        }
        Projection::Orthographic { height } => {
            let h = height * 0.5;
            Mat4::orthographic_rh(-h * design, h * design, -h, h, cam.near, cam.far)
        }
    };
    // The games' Y axis already points down the screen, which is Vulkan's clip-space convention.
    proj * view
}

/// Field of view and frame shape that make the 2D background exactly fill a camera's frame. The background is
/// the layer farthest from the camera (foreground layers may reach past the frame); the fit is a perspective
/// from the camera's position, looking down its −Z axis.
fn fit_layers(cam: &Camera, layers: Option<&GpuModel>) -> Option<(f32, f32)> {
    let layers = layers?;
    let view = Mat4::from_cols_array(&cam.world).inverse();
    let corners = |min: Vec3, max: Vec3| {
        (0..8).map(move |c| {
            Vec3::new(
                if c & 1 == 0 { min.x } else { max.x },
                if c & 2 == 0 { min.y } else { max.y },
                if c & 4 == 0 { min.z } else { max.z },
            )
        })
    };
    // Bounds of each layer in the camera's space; pick the farthest one with some area.
    let background = layers
        .model
        .meshes
        .iter()
        .filter_map(|m| {
            let min = Vec3::from(m.bounds_min);
            let max = min + Vec3::from(m.bounds_size);
            let world = Mat4::from_cols_array(&m.world);
            let pts: Vec<Vec3> =
                corners(min, max).map(|p| view.transform_point3(world.transform_point3(p))).collect();
            let depth = pts.iter().map(|p| -p.z).fold(f32::MAX, f32::min);
            (depth > 1e-3 && m.bounds_size[0] * m.bounds_size[1] > 0.0).then_some((depth, pts))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, pts)| pts)
        .unwrap_or_else(|| corners(layers.min, layers.max).map(|p| view.transform_point3(p)).collect());
    let (mut tx, mut ty) = (0.0f32, 0.0f32);
    for v in background {
        if v.z >= -1e-3 {
            continue;
        }
        tx = tx.max((v.x / -v.z).abs());
        ty = ty.max((v.y / -v.z).abs());
    }
    (tx > 0.0 && ty > 0.0).then(|| (2.0 * ty.atan(), (tx / ty).clamp(0.5, 4.0)))
}
