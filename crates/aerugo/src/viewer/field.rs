//! Field runner: an event's script running at 30 Hz on its map, with actors, message windows, the script camera,
//! player control (WASD, Enter to confirm), a worker inspector and native coverage.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use aerugo_debug_ui::egui;
use aerugo_formats::Game;
use aerugo_formats::motion::{Motion, fit_translation_scale, ps2_joint};
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::{Draw, Renderer, SceneView};
use aerugo_script::Wait;
use aerugo_sim::FixedTimestep;
use aerugo_sim::field::collision::Ground;
use aerugo_sim::field::{Event, Input};
use aerugo_sim::save::{SaveGame, write_atomically};
use aerugo_vfs::{Language, Vfs};
use glam::{Mat4, Vec3};

use super::filelist::FileList;
use super::maps::{best_camera, camera_view_proj, fit_layers};
use super::models::{OrbitCamera, game_to_view, load_motions};
use crate::assets::{GpuModel, load_model};
use aerugo_gfx::aspect::{AspectMode, clip_transform};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CameraMode {
    /// Orbit around the player (or freely).
    Follow,
    /// The camera the scripts set.
    Script,
    /// The map's stored camera, framing its pre-rendered background.
    Map,
}

struct Session {
    event: Event,
    map: Option<GpuModel>,
    layers: Option<GpuModel>,
    /// Where a loaded save had the player, applied once the player is known.
    restore_position: Option<[f32; 3]>,
    /// Events a map change could lead to (when more than one).
    jump_choices: Vec<String>,
    /// Stored cameras of the map's 2D layers.
    map_cameras: Option<Vec<aerugo_formats::camera::Camera>>,
    /// Character models by model id (None when missing).
    actors: HashMap<u16, Option<GpuModel>>,
    /// PS2 motions of each character model.
    motions: HashMap<u16, Vec<Motion>>,
    /// Per worker: motion index, time in frames, translation scale.
    anim: HashMap<usize, (usize, f32, f32)>,
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
    alt: bool,
    last_stick: [f32; 2],
    /// Heading of the camera drawn last (the stick is relative to it).
    camera_heading: Option<f32>,
    /// Draw between the last two ticks (smooth above 30 Hz); the share of the next tick elapsed.
    interpolate: bool,
    alpha: f32,
    /// What was on screen at the tick before the current one.
    prev_view: Option<View>,
    orbit: OrbitCamera,
    follow_player: bool,
    camera_mode: CameraMode,
    aspect: AspectMode,
    show_layers: bool,
    request: (usize, u16, u8),
    /// Event scripts by the map number in their header, per game (built when first needed).
    map_index: HashMap<Game, HashMap<i16, Vec<String>>>,
    follow_jumps: bool,
    pending_open: Option<String>,
    /// A save to put in place once its event is open.
    pending_restore: Option<SaveGame>,
    slot: u32,
    import_path: String,
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
            alt: false,
            last_stick: [0.0, 0.0],
            camera_heading: None,
            interpolate: true,
            alpha: 1.0,
            prev_view: None,
            orbit: OrbitCamera::framing(Vec3::splat(-100.0), Vec3::splat(100.0)),
            follow_player: true,
            camera_mode: CameraMode::Follow,
            aspect: AspectMode::Crop,
            show_layers: false,
            request: (0, 2, 3),
            map_index: HashMap::new(),
            follow_jumps: true,
            pending_open: None,
            pending_restore: None,
            slot: 1,
            import_path: String::new(),
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
        // Walkable ground: the walk mesh (read with the event), else the map's triangles.
        if event.state.ground.is_none()
            && let Some(p) = &event.map_model
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
        let mut restore_position = None;
        if let Some(save) = self.pending_restore.take() {
            event.restore(&save);
            restore_position = save.position;
            notes.push("save image restored".into());
        }
        let map_cameras = event
            .map_layers
            .as_ref()
            .and_then(|p| vfs.read(p).ok())
            .and_then(|d| Cluster::parse(d).ok())
            .map(|c| aerugo_formats::camera::read_cameras(&c))
            .filter(|c| !c.is_empty());
        self.session = Some(Session {
            restore_position,
            jump_choices: Vec::new(),
            map_cameras,
            event,
            map,
            layers,
            actors: HashMap::new(),
            motions: HashMap::new(),
            anim: HashMap::new(),
            notes,
        });
        self.last = None;
        self.clock.reset();
    }

    /// Open an event on the next frame.
    pub fn open_later(&mut self, event: String) {
        self.pending_open = Some(event);
    }

    fn tick(&mut self, input: Input) {
        let Some(s) = &mut self.session else { return };
        // The player is the first party member with a body, until the scripts say otherwise.
        if s.event.state.player.is_none() && s.event.initialized() {
            s.event.state.player =
                s.event.state.actors.iter().position(|a| a.model.is_some_and(|m| m >> 12 == 0));
            if let (Some(p), Some(pos)) = (s.event.state.player, s.restore_position.take()) {
                s.event.state.actors[p].pos = pos;
            }
        }
        s.event.step(input);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Arc<Vfs>, renderer: &mut Renderer) {
        let files = self.files.get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".ebp")));
        let mut picked = None;
        egui::Panel::left("field-files").resizable(true).default_size(260.0).show(ui, |ui| {
            picked = files.ui(ui);
        });
        if let Some(p) = picked.or_else(|| self.pending_open.take()) {
            self.open(vfs, renderer, &p);
        }
        // Input and timing.
        // WASD walks (Shift held: slower), Enter / Space confirms and talks, Q talks with the second button.
        let (stick, confirm_now, alt_now, walk) = ui.input(|i| {
            let k = |a: egui::Key, b: egui::Key| {
                f32::from(u8::from(i.key_down(a))) - f32::from(u8::from(i.key_down(b)))
            };
            (
                [k(egui::Key::D, egui::Key::A), k(egui::Key::W, egui::Key::S)],
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Q),
                i.modifiers.shift,
            )
        });
        self.confirm |= confirm_now;
        self.alt |= alt_now;
        let now = Instant::now();
        let elapsed = self.last.map_or(std::time::Duration::ZERO, |t| now - t);
        self.last = Some(now);
        let steps = if self.paused {
            self.alpha = 1.0;
            u32::from(std::mem::take(&mut self.step_once))
        } else {
            let s = self.clock.advance(elapsed);
            self.alpha = s.alpha;
            s.ticks
        };
        for _ in 0..steps {
            if let Some(s) = &self.session {
                self.prev_view = Some(View::of(&s.event.state));
            }
            let stick_pressed = stick != [0.0, 0.0] && self.last_stick == [0.0, 0.0];
            self.last_stick = stick;
            let input = Input {
                stick,
                confirm: std::mem::take(&mut self.confirm),
                cancel: false,
                alt: std::mem::take(&mut self.alt),
                stick_pressed,
                walk,
                camera_heading: self.camera_heading,
            };
            self.tick(input);
        }
        self.handle_jump(vfs);
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

    /// Events whose header names map `n` (variants for other platforms and test events left out).
    fn jump_candidates(&mut self, vfs: &Vfs, game: Game, n: i16) -> Vec<String> {
        let index = self.map_index.entry(game).or_insert_with(|| {
            let mut index: HashMap<i16, Vec<String>> = HashMap::new();
            let root = if game == Game::Ffx { "ffx_ps2/ffx/" } else { "ffx_ps2/ffx2/" };
            let paths: Vec<String> = vfs
                .archives()
                .flat_map(|(_, a)| a.entries().iter().map(|e| e.path.clone()))
                .filter(|p| p.starts_with(root) && p.ends_with(".ebp"))
                .collect();
            for p in paths {
                let Ok(d) = vfs.read(&p) else { continue };
                if let Some(m) = aerugo_formats::script::Script::parse(&d).ok().and_then(|s| s.map_number()) {
                    index.entry(m).or_default().push(p);
                }
            }
            index
        });
        let name = |p: &String| p.rsplit('/').next().unwrap_or(p).to_owned();
        let mut v: Vec<String> = index
            .get(&n)
            .into_iter()
            .flatten()
            .filter(|p| {
                let n = name(p);
                !n.starts_with("psv") && !n.starts_with("cn_") && !n.starts_with("test")
            })
            .cloned()
            .collect();
        v.sort();
        v
    }

    /// Carry out a map change the scripts asked for when it leads to one event.
    fn handle_jump(&mut self, vfs: &Vfs) {
        let Some(s) = &self.session else { return };
        let Some((target, entrance)) = s.event.state.jump else { return };
        let game = s.event.game;
        let candidates = self.jump_candidates(vfs, game, target as i16);
        let Some(s) = &mut self.session else { return };
        if self.follow_jumps && candidates.len() == 1 {
            s.notes.push(format!("jump to map {target} entrance {entrance}: {}", candidates[0]));
            self.pending_open = Some(candidates[0].clone());
        } else {
            s.notes.push(format!(
                "jump to map {target} entrance {entrance}: {} candidate events",
                candidates.len()
            ));
            s.jump_choices = candidates;
        }
        s.event.state.jump = None;
    }

    fn info_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.paused, "Pause");
            ui.checkbox(&mut self.interpolate, "Smooth")
                .on_hover_text("Draw between the last two 30 Hz ticks (the simulation is unchanged)");
            if ui.button("Step").clicked() {
                self.paused = true;
                self.step_once = true;
            }
            ui.checkbox(&mut self.show_layers, "2D layers");
        });
        ui.horizontal(|ui| {
            ui.label("Camera");
            ui.radio_value(&mut self.camera_mode, CameraMode::Follow, "follow");
            ui.radio_value(&mut self.camera_mode, CameraMode::Script, "script");
            ui.radio_value(&mut self.camera_mode, CameraMode::Map, "map");
            ui.checkbox(&mut self.follow_player, "track player");
        });
        ui.horizontal(|ui| {
            ui.label("Aspect");
            ui.radio_value(&mut self.aspect, AspectMode::Crop, "crop");
            ui.radio_value(&mut self.aspect, AspectMode::Fit, "fit");
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
        for n in s.notes.iter().rev().take(6) {
            ui.weak(n);
        }
        ui.checkbox(&mut self.follow_jumps, "Follow map changes");
        // Saves: own container per slot; import of a Steam release save's image.
        let dir = aerugo_platform::user_dir(false).join("saves");
        let path = dir.join(format!("slot{}.aesv", self.slot));
        ui.horizontal(|ui| {
            ui.label("Slot");
            ui.add(egui::DragValue::new(&mut self.slot).range(1..=9));
            if ui.button("Save").clicked() {
                let bytes = ev.save().to_bytes();
                let msg = match write_atomically(&path, &bytes) {
                    Ok(()) => format!("saved to {}", path.display()),
                    Err(e) => format!("save failed: {e}"),
                };
                s.notes.push(msg);
            }
            if ui.button("Load").clicked() {
                let loaded = std::fs::read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| SaveGame::from_bytes(&b).map_err(|e| e.to_string()));
                match loaded {
                    Ok(save) => {
                        if let Some(ev) = save.event.clone() {
                            self.pending_open = Some(ev);
                        }
                        self.pending_restore = Some(save);
                    }
                    Err(e) => s.notes.push(format!("load failed: {e}")),
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("Steam save");
            ui.text_edit_singleline(&mut self.import_path);
            if ui.button("Import").clicked() {
                let imported = std::fs::read(&self.import_path)
                    .map_err(|e| e.to_string())
                    .and_then(|b| SaveGame::import_steam(ev.game, &b).map_err(|e| e.to_string()));
                match imported {
                    Ok(save) => {
                        ev.restore(&save);
                        s.notes.push("Steam save image put in place".into());
                    }
                    Err(e) => s.notes.push(format!("import failed: {e}")),
                }
            }
        });
        if !s.jump_choices.is_empty() {
            ui.label("Map change to one of:");
            for p in &s.jump_choices {
                if ui.button(p.rsplit('/').next().unwrap_or(p)).clicked() {
                    self.pending_open = Some(p.clone());
                }
            }
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
            let text =
                w.message.and_then(|m| ev.state.messages.get(m)).map(|m| m.raw.clone()).unwrap_or_default();
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
                let path = a.model_path(s.event.game);
                let m = path.as_ref().and_then(|p| load_model(vfs, renderer, p).ok());
                s.actors.insert(id, m);
                s.motions.insert(
                    id,
                    path.map(|p| load_motions(vfs, &p).into_iter().map(|(_, m)| m).collect())
                        .unwrap_or_default(),
                );
            }
        }
        // Advance each body's motion: the one the script asked for when it exists, else the first (idle).
        let dt = if self.paused { 0.0 } else { ui.input(|i| i.stable_dt).min(0.1) };
        for (w, a) in s.event.state.actors.iter().enumerate() {
            let (Some(id), Some(Some(model))) = (a.model, a.model.and_then(|id| s.actors.get(&id))) else {
                continue;
            };
            let Some(motions) = s.motions.get(&id).filter(|m| !m.is_empty()) else { continue };
            let want = a
                .motion
                .and_then(|m| usize::try_from(m & 0xFFF).ok())
                .filter(|&m| m < motions.len())
                .unwrap_or(0);
            let entry = s.anim.entry(w).or_insert((usize::MAX, 0.0, 1.0));
            if entry.0 != want {
                let scale = model
                    .meshes
                    .iter()
                    .filter_map(|m| m.skeleton.as_ref())
                    .max_by_key(|sk| sk.len())
                    .map_or(1.0, |sk| fit_translation_scale(&sk.names, &sk.default_pose, &motions[want]));
                *entry = (want, 0.0, scale);
            }
            let motion = &motions[want];
            let rate = f32::from(motion.rate.max(1)) / f32::from(0x1E00u16);
            let last = (motion.frames.max(1) as f32 - 1.0).max(1.0);
            entry.1 = (entry.1 + dt * 30.0 * rate) % last;
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
        // The original frame is 512 × 448; the script and map cameras follow the aspect policy around it.
        let design = 512.0 / 448.0;
        let mode = self.aspect;
        let policy = |base: Mat4, design: f32| {
            Mat4::from_cols_array(&clip_transform(design, aspect, mode, [0.0, 0.0]).matrix()) * base
        };
        let map_camera = s.layers.as_ref().and_then(|layers| {
            let cams = s.map_cameras.as_ref()?;
            let fitted: Vec<_> = cams.iter().map(|c| fit_layers(c, Some(layers))).collect();
            let shapes: Vec<_> =
                fitted.iter().map(|f| f.map(|[l, r, t, b]| (0.0, (r - l) / (b - t)))).collect();
            let i = best_camera(&shapes);
            let f = fitted.get(i).copied().flatten()?;
            let d = (f[1] - f[0]) / (f[3] - f[2]);
            // The camera looks down its −Z axis; view space is game space turned half way round X.
            let w = &cams[i].world;
            Some((policy(camera_view_proj(&cams[i], d, Some(f)), d), w[10].atan2(-w[8])))
        });
        // The heading the stick is relative to: where the drawn camera looks, on the ground plane.
        // Between the last two ticks: positions, headings and the camera blend by the share of the next tick
        // elapsed, except across jumps (teleports, camera cuts, new actors).
        let alpha = if self.interpolate { self.alpha } else { 1.0 };
        let view = View::of(st).blended_from(self.prev_view.as_ref(), alpha);
        let (cam_eye, cam_focus) = (view.eye, view.focus);
        self.camera_heading = match (self.camera_mode, cam_eye, cam_focus, map_camera) {
            (CameraMode::Script, Some(eye), Some(focus), _) => {
                Some((focus[2] - eye[2]).atan2(focus[0] - eye[0]))
            }
            (CameraMode::Map, _, _, Some((_, h))) => Some(h),
            _ => {
                let d = self.orbit.target - self.orbit.eye();
                Some((-d.z).atan2(d.x))
            }
        };
        let map_camera = map_camera.map(|(vp, _)| vp);
        let view_proj = match (self.camera_mode, cam_eye, cam_focus, map_camera) {
            (CameraMode::Script, Some(eye), Some(focus), _) => {
                let r = game_to_view();
                let (eye, focus) =
                    (r.transform_point3(Vec3::from(eye)), r.transform_point3(Vec3::from(focus)));
                let d = (focus - eye).length().max(1.0);
                let base = Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0))
                    * Mat4::perspective_rh(st.camera.fov_y(), design, d * 0.01, d * 200.0)
                    * Mat4::from_rotation_z(st.camera.roll)
                    * Mat4::look_at_rh(eye, focus, Vec3::Y)
                    * r;
                policy(base, design)
            }
            (CameraMode::Map, _, _, Some(vp)) => vp,
            _ => self.orbit.view_proj(aspect),
        };
        let mut draws = Vec::new();
        let mut joints = Vec::new();
        let mut add =
            |m: &GpuModel, place: Mat4, pose: Option<(&Motion, f32, f32)>, joints: &mut Vec<[f32; 16]>| {
                for mesh in &m.meshes {
                    match (&mesh.skeleton, mesh.skinned) {
                        (Some(sk), true) => {
                            let mut local = sk.default_pose.clone();
                            if let Some((motion, time, scale)) = pose {
                                for (i, name) in sk.names.iter().enumerate() {
                                    if let Some(j) = ps2_joint(name).filter(|&j| j < motion.joints as usize) {
                                        local[i] = motion.joint_at(j, time).matrix(scale);
                                    }
                                }
                            }
                            let world = sk.world(&local);
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
            add(m, Mat4::IDENTITY, None, &mut joints);
        }
        if self.show_layers
            && let Some(m) = &s.layers
        {
            add(m, Mat4::IDENTITY, None, &mut joints);
        }
        for (w, a) in st.actors.iter().enumerate().filter(|(_, a)| a.visible) {
            if let Some(Some(m)) = a.model.and_then(|id| s.actors.get(&id)) {
                // Y points down in the map, so a turn about Y is negated to look right after the viewer's flip.
                let (pos, yaw) = view.actors.get(w).copied().unwrap_or((a.pos, a.yaw));
                let place = Mat4::from_translation(Vec3::from(pos)) * Mat4::from_rotation_y(-yaw);
                let pose = a
                    .model
                    .and_then(|id| s.motions.get(&id))
                    .zip(s.anim.get(&w))
                    .and_then(|(ms, &(i, t, sc))| ms.get(i).map(|m| (m, t, sc)));
                add(m, place, pose, &mut joints);
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
            let Some(msg) = w.message.and_then(|m| st.messages.get(m)) else { continue };
            // Revealed so far; the last lines of a choice message are its choices.
            let shown: String = msg.text.chars().take(w.shown).collect();
            let lines: Vec<&str> = shown.lines().collect();
            let total = msg.text.lines().count();
            let first_choice = total.saturating_sub(usize::from(msg.choices));
            let text = lines
                .iter()
                .enumerate()
                .map(|(k, l)| {
                    if msg.choices > 0 && k >= first_choice {
                        format!("{} {l}", if k - first_choice == w.choice { "▸" } else { "  " })
                    } else {
                        (*l).to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
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

/// What the field shows at one tick: each actor's position and heading, and the camera.
#[derive(Debug, Clone, Default)]
struct View {
    actors: Vec<([f32; 3], f32)>,
    eye: Option<[f32; 3]>,
    focus: Option<[f32; 3]>,
}

/// Moves longer than these in one tick are jumps (teleports, camera cuts) and are not blended.
const ACTOR_JUMP: f32 = 30.0;
const CAMERA_JUMP: f32 = 60.0;

impl View {
    fn of(st: &aerugo_sim::field::FieldState) -> View {
        View {
            actors: st.actors.iter().map(|a| (a.pos, a.yaw)).collect(),
            eye: st.camera.position,
            focus: st.camera.focus,
        }
    }

    /// This view drawn `alpha` of the way from `prev` (1: as it is).
    fn blended_from(mut self, prev: Option<&View>, alpha: f32) -> View {
        let Some(prev) = prev.filter(|_| alpha < 1.0) else { return self };
        let mix = |a: [f32; 3], b: [f32; 3], jump: f32| {
            let d = (0..3).map(|k| (b[k] - a[k]).powi(2)).sum::<f32>().sqrt();
            if d > jump { b } else { std::array::from_fn(|k| a[k] + (b[k] - a[k]) * alpha) }
        };
        for (cur, old) in self.actors.iter_mut().zip(&prev.actors) {
            let turn = aerugo_script::natives::wrap_angle(cur.1 - old.1);
            *cur = (mix(old.0, cur.0, ACTOR_JUMP), old.1 + turn * alpha);
        }
        if let (Some(a), Some(b)) = (prev.eye, self.eye) {
            self.eye = Some(mix(a, b, CAMERA_JUMP));
        }
        if let (Some(a), Some(b)) = (prev.focus, self.focus) {
            self.focus = Some(mix(a, b, CAMERA_JUMP));
        }
        self
    }
}

#[cfg(test)]
mod view_tests {
    use super::*;

    #[test]
    fn views_blend_but_not_across_jumps() {
        let prev = View { actors: vec![([0.0; 3], 0.0), ([0.0; 3], 0.0)], eye: Some([0.0; 3]), focus: None };
        let cur = View {
            actors: vec![([2.0, 0.0, 0.0], 1.0), ([100.0, 0.0, 0.0], 0.0)],
            eye: Some([10.0, 0.0, 0.0]),
            focus: None,
        };
        let v = cur.blended_from(Some(&prev), 0.25);
        assert_eq!(v.actors[0], ([0.5, 0.0, 0.0], 0.25));
        assert_eq!(v.actors[1].0, [100.0, 0.0, 0.0]);
        assert_eq!(v.eye, Some([2.5, 0.0, 0.0]));
    }
}
