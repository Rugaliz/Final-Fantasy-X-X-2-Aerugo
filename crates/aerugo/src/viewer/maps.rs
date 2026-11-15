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
    /// The background's extent through each camera, as view-space tangents (left, right, top, bottom).
    frusta: Vec<Option<[f32; 4]>>,
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
    culling: bool,
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
            culling: true,
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
        let frusta: Vec<Option<[f32; 4]>> = cameras.iter().map(|c| fit_layers(c, layers.as_ref())).collect();
        for (c, f) in cameras.iter_mut().zip(&frusta) {
            if let Some([_, _, top, bottom]) = f {
                c.projection = Projection::Perspective { fov_y: bottom.atan() - top.atan() };
            }
        }
        let fitted: Vec<Option<(f32, f32)>> =
            frusta.iter().map(|f| f.map(|[l, r, t, b]| (0.0, (r - l) / (b - t)))).collect();
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
        self.view = if layers.is_some() && !cameras.is_empty() {
            View::Scene(best_camera(&fitted))
        } else {
            View::Orbit
        };
        self.follow = [0.0, 0.0];
        self.loaded = Some(Loaded { entry, model, layers, cameras, design, frusta, errors });
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
        ui.checkbox(&mut self.culling, "Face culling");
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
                let base = camera_view_proj(cam, design, l.frusta.get(i).copied().flatten());
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
            culling: self.culling,
            mirrored: false,
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
/// The camera that frames the layers best: its fitted frame is closest to the PS2's 512 × 448. Maps keep
/// cameras for other shots too, which may not face the background at all.
fn best_camera(fitted: &[Option<(f32, f32)>]) -> usize {
    let score = |f: &Option<(f32, f32)>| f.map_or(f32::MAX, |(_, a)| (a - 512.0 / 448.0).abs());
    (0..fitted.len()).min_by(|&a, &b| score(&fitted[a]).total_cmp(&score(&fitted[b]))).unwrap_or(0)
}

/// A camera's view-projection. With a fitted frustum the frame is exactly the background (which may sit off
/// the camera's axis); otherwise the camera's own field of view at the `design` aspect.
fn camera_view_proj(cam: &Camera, design: f32, frustum: Option<[f32; 4]>) -> Mat4 {
    let world = Mat4::from_cols_array(&cam.world);
    let view = world.inverse();
    let (near, far) = (cam.near.max(0.001), cam.far.max(cam.near + 1.0));
    let proj = match (frustum, cam.projection) {
        (Some([l, r, t, b]), _) => {
            // Off-centre perspective, right-handed, depth 0–1: x/-z = l maps to -1, = r to +1 (y likewise, t to
            // the top of the screen).
            let zr = far / (near - far);
            Mat4::from_cols_array(&[
                2.0 / (r - l),
                0.0,
                0.0,
                0.0,
                0.0,
                2.0 / (b - t),
                0.0,
                0.0,
                (r + l) / (r - l),
                (b + t) / (b - t),
                zr,
                -1.0,
                0.0,
                0.0,
                zr * near,
                0.0,
            ])
        }
        (None, Projection::Perspective { fov_y }) => {
            Mat4::perspective_rh(fov_y, design, cam.near.max(0.001), cam.far.max(cam.near + 1.0))
        }
        (None, Projection::Orthographic { height }) => {
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
fn fit_layers(cam: &Camera, layers: Option<&GpuModel>) -> Option<[f32; 4]> {
    let layers = layers?;
    let view = Mat4::from_cols_array(&cam.world).inverse();
    // Half-height tangent of the camera's own view.
    let reach = match cam.projection {
        Projection::Perspective { fov_y } => (fov_y * 0.5).tan().clamp(0.05, 4.0),
        Projection::Orthographic { .. } => 4.0,
    };
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
            // Maps with several screens have a background per camera: keep layers whose centre lies inside
            // (a generous margin around) this camera's own view.
            let centre = view.transform_point3(world.transform_point3(min + Vec3::from(m.bounds_size) * 0.5));
            let in_view = centre.z < 0.0
                && (centre.x / -centre.z).abs() < 2.0 * reach
                && (centre.y / -centre.z).abs() < 1.5 * reach;
            (depth > 1e-3 && in_view && m.bounds_size[0] * m.bounds_size[1] > 0.0).then_some((depth, pts))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, pts)| pts)
        .unwrap_or_else(|| corners(layers.min, layers.max).map(|p| view.transform_point3(p)).collect());
    let (mut l, mut r, mut t, mut b) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for v in background {
        if v.z >= -1e-3 {
            continue;
        }
        let (x, y) = (v.x / -v.z, v.y / -v.z);
        (l, r, t, b) = (l.min(x), r.max(x), t.min(y), b.max(y));
    }
    (r - l > 1e-4 && b - t > 1e-4 && (r - l) / (b - t) < 8.0 && (b - t) / (r - l) < 8.0)
        .then_some([l, r, t, b])
}

#[cfg(test)]
mod tests {
    //! M1 gate for maps (local: needs `AERUGO_GAME_DIR` and a Vulkan device): pre-rendered maps open and their
    //! stored camera follows the aspect policy at 4:3, 16:9, 21:9 and 32:9.

    use super::*;
    use aerugo_gfx::ContextOptions;
    use aerugo_vfs::install_from_env;

    const CLEAR: [f32; 4] = [1.0, 0.0, 1.0, 1.0];
    /// Single-screen backgrounds that frame cleanly at all four ratios in crop and fit.
    const KNOWN_GOOD: [&str; 2] = ["X guad/guad02", "X azit/azit00"];
    /// How many layered maps framed cleanly when this test was written (a regression floor).
    const MIN_CLEAN: usize = 27;

    /// Share of the image's outermost columns (left and right) that show the clear colour.
    fn edge_clear_share(px: &[u8], w: u32, h: u32) -> f64 {
        let mut clear = 0;
        for y in 0..h {
            for x in [0, w - 1] {
                let i = ((y * w + x) * 4) as usize;
                if px[i] > 250 && px[i + 1] < 5 && px[i + 2] > 250 {
                    clear += 1;
                }
            }
        }
        f64::from(clear) / f64::from(2 * h)
    }

    #[test]
    fn layered_maps_follow_the_aspect_policy() {
        let Some(inst) = install_from_env() else {
            eprintln!("skipped: set AERUGO_GAME_DIR");
            return;
        };
        let mut renderer =
            match Renderer::new_headless(&ContextOptions { validation: true, preferred_gpu: None }) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("skipped: no usable Vulkan device ({e:#})");
                    return;
                }
            };
        let mut vfs = Vfs::new();
        vfs.mount_install(&inst).unwrap();
        let entries: Vec<MapEntry> =
            MapBrowser::list(&vfs).into_iter().filter(|e| e.layers.is_some()).collect();
        assert!(entries.len() > 150, "{} layered maps", entries.len());
        let mut browser = MapBrowser::new();
        let (mut checked, mut skipped, mut bad, mut clean) =
            (0, Vec::new(), Vec::<String>::new(), Vec::new());
        for entry in entries {
            let label = entry.label.clone();
            let problems_before = bad.len();
            browser.open(&vfs, &mut renderer, entry);
            let l = browser.loaded.as_ref().unwrap();
            assert!(l.errors.is_empty(), "{label}: {:?}", l.errors);
            if l.layers.is_none() {
                eprintln!("{label}: the 2d model has no layer geometry; skipped");
                skipped.push(label);
                continue;
            }
            let View::Scene(best) = browser.view else {
                panic!(
                    "{label}: no scene view (layers {}, cameras {:?})",
                    l.layers.as_ref().map_or(0, |m| m.meshes.len()),
                    l.cameras.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                )
            };
            let frustum = l.frusta.get(best).copied().flatten();
            let (Some(layers), Some(cam), Some(&design)) =
                (&l.layers, l.cameras.get(best), l.design.get(best))
            else {
                panic!("{label}: no layers or camera");
            };
            let draws: Vec<Draw> = layers
                .meshes
                .iter()
                .map(|m| Draw {
                    mesh: m.mesh,
                    model: m.world.to_cols_array(),
                    joint_base: None,
                    lit: false,
                    part: None,
                })
                .collect();
            let h = 180u32;
            for aspect in [4.0f32 / 3.0, 16.0 / 9.0, 21.0 / 9.0, 32.0 / 9.0] {
                let w = (h as f32 * aspect).round() as u32;
                for mode in [AspectMode::Crop, AspectMode::Fit] {
                    let c = clip_transform(design, w as f32 / h as f32, mode, [0.0, 0.0]);
                    let view_proj =
                        Mat4::from_cols_array(&c.matrix()) * camera_view_proj(cam, design, frustum);
                    let view = SceneView {
                        view_proj: view_proj.to_cols_array(),
                        clear: CLEAR,
                        draws: draws.clone(),
                        joints: Vec::new(),
                        culling: true,
                        mirrored: false,
                    };
                    let px = renderer.render_offscreen(view, (w, h)).unwrap();
                    let share = edge_clear_share(&px, w, h);
                    // Crop fills the frame at every ratio; fit leaves bars beside a frame narrower than the view.
                    match mode {
                        AspectMode::Crop => {
                            if share >= 0.05 {
                                bad.push(format!("{label} crop at {aspect:.2}: edges {share:.2} empty"));
                            }
                        }
                        AspectMode::Fit if aspect > design + 0.05 => {
                            // Foreground layers may reach into the bars; most of the edge must still be empty.
                            if share <= 0.25 {
                                bad.push(format!("{label} fit at {aspect:.2}: no bars ({share:.2})"));
                            }
                        }
                        AspectMode::Fit => {}
                    }
                }
            }
            checked += 1;
            if bad.len() == problems_before {
                clean.push(label);
            }
        }
        browser.close(&mut renderer);
        eprintln!(
            "{checked} layered maps rendered at 4:3, 16:9, 21:9 and 32:9 (crop and fit); {} skipped",
            skipped.len()
        );
        eprintln!("{} of {} maps frame their background cleanly at every ratio", clean.len(), checked);
        // Fitting the frame from the layers alone works for single-screen backgrounds; maps with several screens
        // or scrolling backgrounds need the game's own camera control (later milestones). These must keep working:
        for want in KNOWN_GOOD {
            assert!(clean.iter().any(|c| c == want), "{want} no longer frames cleanly:\n{}", bad.join("\n"));
        }
        assert!(clean.len() >= MIN_CLEAN, "only {} maps frame cleanly (was {MIN_CLEAN})", clean.len());
        assert_eq!(aerugo_gfx::validation_error_count(), 0, "Vulkan validation errors");
    }
}
