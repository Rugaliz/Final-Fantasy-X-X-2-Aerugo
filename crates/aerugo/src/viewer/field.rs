//! Field runner: an event's script running at 30 Hz on its map, with actors, message windows, the script camera,
//! player control (WASD, Enter to confirm), a worker inspector and native coverage.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use aerugo_debug_ui::egui;
use aerugo_formats::Game;
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::{Draw, Renderer, SceneView};
use aerugo_script::Wait;
use aerugo_sim::FixedTimestep;
use aerugo_sim::field::collision::Ground;
use aerugo_sim::field::{Event, Input};
use aerugo_vfs::{Language, Vfs};
use glam::{Mat4, Vec3};

use super::filelist::FileList;
use super::models::{OrbitCamera, game_to_view};
use crate::assets::{GpuModel, load_model};

struct Session {
    event: Event,
    map: Option<GpuModel>,
    layers: Option<GpuModel>,
    /// Character models by model id (None when missing).
    actors: HashMap<u16, Option<GpuModel>>,
    notes: Vec<String>,
}

pub struct FieldView {
    files: Option<FileList>,
    session: Option<Session>,
    error: Option<String>,
    lang: Language,
    clock: FixedTimestep,
    last: Option<Instant>,
    paused: bool,
    step_once: bool,
    confirm: bool,
    orbit: OrbitCamera,
    follow_player: bool,
    script_camera: bool,
    show_layers: bool,
    request: (usize, u16, u8),
}

impl std::fmt::Debug for FieldView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldView").finish_non_exhaustive()
    }
}

fn game_of(path: &str) -> Game {
    if path.contains("/ffx2/") { Game::Ffx2 } else { Game::Ffx }
}

impl FieldView {
    pub fn new(lang: Language) -> FieldView {
        FieldView {
            files: None,
            session: None,
            error: None,
            lang,
            clock: FixedTimestep::new(30, 4),
            last: None,
            paused: false,
            step_once: false,
            confirm: false,
            orbit: OrbitCamera::framing(Vec3::splat(-100.0), Vec3::splat(100.0)),
            follow_player: true,
            script_camera: false,
            show_layers: false,
            request: (0, 2, 3),
        }
    }

    pub fn close(&mut self, renderer: &mut Renderer) {
        if let Some(s) = self.session.take() {
            for m in [s.map, s.layers].into_iter().flatten() {
                m.destroy(renderer);
            }
            for m in s.actors.into_values().flatten() {
                m.destroy(renderer);
            }
        }
    }

    fn open(&mut self, vfs: &Vfs, renderer: &mut Renderer, path: &str) {
        self.close(renderer);
        self.error = None;
        let game = game_of(path);
        let mut event = match Event::load(vfs, game, path, self.lang) {
            Ok(e) => e,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let mut notes = Vec::new();
        let mut load = |p: &Option<String>| -> Option<GpuModel> {
            let p = p.as_ref()?;
            match load_model(vfs, renderer, p) {
                Ok(m) => Some(m),
                Err(e) => {
                    notes.push(format!("{p}: {e}"));
                    None
                }
            }
        };
        let map = load(&event.map_model);
        let layers = load(&event.map_layers);
        if event.map_model.is_none() {
            notes.push("No map found for this event name.".into());
        }
        // Walkable ground from the map's triangles.
        if let Some(p) = &event.map_model
            && let Ok(c) = vfs
                .read(p)
                .map_err(|e| e.to_string())
                .and_then(|d| Cluster::parse(d).map_err(|e| e.to_string()))
            && let Ok(model) = aerugo_formats::mesh::read_model(&c)
        {
            event.state.ground = Some(Ground::from_model(&c, &model));
        }
        if let Some(m) = &map {
            self.orbit = OrbitCamera::framing(m.min, m.max);
        }
        self.session = Some(Session { event, map, layers, actors: HashMap::new(), notes });
        self.last = None;
        self.clock.reset();
    }

    fn tick(&mut self, input: Input) {
        let Some(s) = &mut self.session else { return };
        // The player is the first party member with a body, until the scripts say otherwise.
        if s.event.state.player.is_none() && s.event.initialized() {
            s.event.state.player =
                s.event.state.actors.iter().position(|a| a.model.is_some_and(|m| m >> 12 == 0));
        }
        s.event.step(input);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Arc<Vfs>, renderer: &mut Renderer) {
        let files = self.files.get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".ebp")));
        let mut picked = None;
        egui::Panel::left("field-files").resizable(true).default_size(260.0).show(ui, |ui| {
            picked = files.ui(ui);
        });
        if let Some(p) = picked {
            self.open(vfs, renderer, &p);
        }
        // Input and timing.
        let (stick, confirm_now) = ui.input(|i| {
            let k = |a: egui::Key, b: egui::Key| {
                f32::from(u8::from(i.key_down(a))) - f32::from(u8::from(i.key_down(b)))
            };
            (
                [k(egui::Key::D, egui::Key::A), k(egui::Key::W, egui::Key::S)],
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space),
            )
        });
        self.confirm |= confirm_now;
        let now = Instant::now();
        let elapsed = self.last.map_or(std::time::Duration::ZERO, |t| now - t);
        self.last = Some(now);
        let steps = if self.paused {
            u32::from(std::mem::take(&mut self.step_once))
        } else {
            self.clock.advance(elapsed).ticks
        };
        for _ in 0..steps {
            let input = Input { stick, confirm: std::mem::take(&mut self.confirm), cancel: false };
            self.tick(input);
        }
        egui::Panel::right("field-info").resizable(true).default_size(360.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.info_ui(ui));
        });
        egui::CentralPanel::default_margins().show(ui, |ui| {
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            if self.session.is_none() {
                ui.label("Pick an event on the left. WASD walks the player, Enter confirms messages.");
                return;
            }
            self.viewport(ui, vfs, renderer);
        });
        ui.ctx().request_repaint();
    }

    fn info_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.paused, "Pause");
            if ui.button("Step").clicked() {
                self.paused = true;
                self.step_once = true;
            }
            ui.checkbox(&mut self.follow_player, "Follow player");
            ui.checkbox(&mut self.script_camera, "Script camera");
            ui.checkbox(&mut self.show_layers, "2D layers");
        });
        let Some(s) = &mut self.session else { return };
        let ev = &mut s.event;
        ui.label(format!(
            "{} · tick {} · {} · {} workers · player {:?}",
            ev.name,
            ev.tick,
            if ev.initialized() { "running" } else { "init phase" },
            ev.vm.workers.len(),
            ev.state.player
        ));
        for n in &s.notes {
            ui.weak(n);
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Request: worker");
            ui.add(
                egui::DragValue::new(&mut self.request.0).range(0..=ev.vm.workers.len().saturating_sub(1)),
            );
            ui.label("entry");
            ui.add(egui::DragValue::new(&mut self.request.1).range(1..=64));
            ui.label("level");
            ui.add(egui::DragValue::new(&mut self.request.2).range(0..=8));
            if ui.button("Send").clicked() {
                let (w, e, l) = self.request;
                let ok = ev.request(w, l, e);
                s.notes.push(format!(
                    "request worker {w} entry {e} level {l}: {}",
                    if ok { "queued" } else { "refused" }
                ));
            }
        });
        ui.separator();
        ui.strong("Message windows");
        for (i, w) in ev.state.windows.iter().enumerate().filter(|(_, w)| w.open || w.message.is_some()) {
            let text = w.message.and_then(|m| ev.state.messages.get(m)).cloned().unwrap_or_default();
            ui.label(format!("{i}{} #{:?}: {text}", if w.open { " (open)" } else { "" }, w.message));
        }
        ui.separator();
        ui.strong("Workers");
        egui::Grid::new("workers").striped(true).show(ui, |ui| {
            for h in ["#", "kind", "lvl", "entry", "pc", "wait", "queue", "body"] {
                ui.strong(h);
            }
            ui.end_row();
            for w in &ev.vm.workers {
                if !w.active {
                    continue;
                }
                ui.label(w.index.to_string());
                ui.label(w.kind.to_string());
                ui.label(w.level.to_string());
                ui.label(w.entry.to_string());
                ui.monospace(format!("{:05x}", w.context().pc));
                ui.label(match w.context().wait {
                    Wait::None => String::new(),
                    Wait::Signal { target, entry, .. } => format!("w{target} e{entry}"),
                    Wait::Idle { target, entry } => format!("idle w{target} e{entry}"),
                });
                ui.label(w.queue.len().to_string());
                let a = ev.state.actors.get(w.index);
                ui.label(a.and_then(|a| a.model).map(|m| format!("{m:04x}")).unwrap_or_default());
                ui.end_row();
            }
        });
        ui.separator();
        ui.collapsing("Native coverage", |ui| {
            ui.monospace(ev.natives.coverage.report(ev.game));
        });
        if !ev.vm.faults.is_empty() {
            ui.collapsing(format!("Faults ({})", ev.vm.faults.len()), |ui| {
                for (w, pc, f) in ev.vm.faults.iter().take(50) {
                    ui.monospace(format!("w{w} {pc:05x} {f:?}"));
                }
            });
        }
    }

    fn viewport(&mut self, ui: &mut egui::Ui, vfs: &Vfs, renderer: &mut Renderer) {
        let Some(s) = &mut self.session else { return };
        // Load bodies as scripts bind them.
        for a in &s.event.state.actors {
            if let Some(id) = a.model
                && !s.actors.contains_key(&id)
            {
                let m = a.model_path(s.event.game).and_then(|p| load_model(vfs, renderer, &p).ok());
                s.actors.insert(id, m);
            }
        }
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
        self.orbit.handle(ui, &response);
        let st = &s.event.state;
        if self.follow_player
            && let Some(p) = st.player.and_then(|p| st.actors.get(p))
        {
            self.orbit.target = game_to_view().transform_point3(Vec3::from(p.pos));
        }
        let aspect = size.x / size.y.max(1.0);
        let view_proj = match (self.script_camera, st.camera.position, st.camera.focus) {
            (true, Some(eye), Some(focus)) => {
                let r = game_to_view();
                let (eye, focus) =
                    (r.transform_point3(Vec3::from(eye)), r.transform_point3(Vec3::from(focus)));
                let d = (focus - eye).length().max(1.0);
                Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0))
                    * Mat4::perspective_rh(40f32.to_radians(), aspect, d * 0.01, d * 200.0)
                    * Mat4::look_at_rh(eye, focus, Vec3::Y)
                    * r
            }
            _ => self.orbit.view_proj(aspect),
        };
        let mut draws = Vec::new();
        let mut joints = Vec::new();
        let mut add = |m: &GpuModel, place: Mat4, joints: &mut Vec<[f32; 16]>| {
            for mesh in &m.meshes {
                match (&mesh.skeleton, mesh.skinned) {
                    (Some(sk), true) => {
                        let world = sk.world(&sk.default_pose);
                        for (pi, &si) in mesh.part_segments.iter().enumerate() {
                            let seg = &m.model.meshes[mesh.source].segments[si];
                            let base = joints.len() as u32;
                            joints.extend(sk.palette(&world, seg));
                            draws.push(Draw {
                                mesh: mesh.mesh,
                                model: (place * mesh.world).to_cols_array(),
                                joint_base: Some(base),
                                lit: true,
                                part: Some(pi),
                            });
                        }
                    }
                    _ => draws.push(Draw {
                        mesh: mesh.mesh,
                        model: (place * mesh.world).to_cols_array(),
                        joint_base: None,
                        lit: false,
                        part: None,
                    }),
                }
            }
        };
        if let Some(m) = &s.map {
            add(m, Mat4::IDENTITY, &mut joints);
        }
        if self.show_layers
            && let Some(m) = &s.layers
        {
            add(m, Mat4::IDENTITY, &mut joints);
        }
        for a in st.actors.iter().filter(|a| a.visible) {
            if let Some(Some(m)) = a.model.and_then(|id| s.actors.get(&id)) {
                // Y points down in the map, so a turn about Y is negated to look right after the viewer's flip.
                let place = Mat4::from_translation(Vec3::from(a.pos)) * Mat4::from_rotation_y(-a.yaw);
                add(m, place, &mut joints);
            }
        }
        renderer.submit_scene(SceneView {
            view_proj: view_proj.to_cols_array(),
            clear: [0.05, 0.05, 0.08, 1.0],
            draws,
            joints,
            culling: true,
            mirrored: false,
        });
        // Message windows over the view, placed in the original's 512 × 448 frame.
        let rect = response.rect;
        for (i, w) in st.windows.iter().enumerate().filter(|(_, w)| w.open) {
            let Some(text) = w.message.and_then(|m| st.messages.get(m)) else { continue };
            let pos =
                rect.min + egui::vec2(w.x as f32 / 512.0 * rect.width(), w.y as f32 / 448.0 * rect.height());
            egui::Area::new(egui::Id::new(("field-window", i))).fixed_pos(pos).show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(rect.width() * 0.6);
                    ui.label(text);
                    ui.weak("Enter ▸");
                });
            });
        }
    }
}
