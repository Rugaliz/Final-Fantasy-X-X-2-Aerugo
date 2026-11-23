//! Field runner: an event's script running at 30 Hz on its map, with actors, message windows, the script camera,
//! player control (WASD, Enter to confirm), a worker inspector and native coverage.
//!
//! The same runner is the game screen ([`FieldView::game`]): the field fills the window through the scene's own
//! camera, with game message boxes and a pause menu, and no developer panels.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use aerugo_audio::AudioOutput;
use aerugo_debug_ui::egui;
use aerugo_formats::Game;
use aerugo_formats::motion::ps2_joint;
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::Renderer;
use aerugo_script::Wait;
use aerugo_sim::FixedTimestep;
use aerugo_sim::field::collision::Ground;
use aerugo_sim::field::{Event, Input};
use aerugo_sim::save::{SaveGame, write_atomically};
use aerugo_vfs::{Language, Vfs};
use glam::{Mat4, Vec3};

use super::filelist::FileList;
use super::maps::{best_camera, camera_view_proj, fit_layers};
use super::models::{OrbitCamera, game_to_view};
use super::movies::{MoviePlayer, movie_ticks};
use crate::assets::{ModelInfo, read_model_info};
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
    map: Option<ModelInfo>,
    layers: Option<ModelInfo>,
    /// Where a loaded save had the player, applied once the player is known.
    restore_position: Option<[f32; 3]>,
    /// Events a map change could lead to (when more than one).
    jump_choices: Vec<String>,
    /// Stored cameras of the map's 2D layers.
    map_cameras: Option<Vec<aerugo_formats::camera::Camera>>,
    /// Character models by model id (None when missing).
    actors: HashMap<u16, Option<ModelInfo>>,
    notes: Vec<String>,
    /// Drawing with the remaster's own shaders (the game screen).
    phyre: crate::phyre::field::FieldRenderer,
    /// The 2D layer (message windows) and the base font it lays text out with.
    ui: crate::phyre::ui::UiRenderer,
    font: Option<Option<aerugo_formats::ftc::Ftc>>,
}

pub struct FieldView {
    files: Option<FileList>,
    session: Option<Session>,
    error: Option<String>,
    lang: Language,
    /// The graphics clock (the choice cursor sways with it).
    started: std::time::Instant,
    clock: FixedTimestep,
    last: Option<Instant>,
    paused: bool,
    step_once: bool,
    confirm: bool,
    alt: bool,
    cancel: bool,
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
    /// The movie the scripts play, drawn over the field.
    movie: MoviePlayer,
    /// The game screen: no developer panels, the scene camera, game message boxes, a pause menu.
    game: bool,
    /// The pause menu is open (game screen).
    pause_menu: bool,
    /// The player asked to leave the game (game screen).
    quit: bool,
}

/// FFX event numbers (what map changes name) and their events, as recordings of the original showed them. The
/// original reads its table of event files at run time; until that table is read here, these cover the first
/// chapter, and other targets fall back to the map number in the script headers.
const FFX_EVENTS: &[(i32, &str)] = &[
    (23, "te/test20"),
    (48, "bj/bjyt0000"),
    (49, "bj/bjyt0200"),
    (50, "bj/bjyt0300"),
    (52, "bj/bjyt0600"),
    (63, "bj/bjyt0400"),
    (64, "cd/cdsp0700"),
    (71, "cd/cdsp0000"),
    (74, "bj/bjyt0700"),
    (132, "zn/znkd0600"),
    (165, "zn/znkd0500"),
    (288, "cd/cdsp0100"),
    (348, "me/memochek"),
    (349, "lo/loopdemo"),
    (366, "zn/znkd0800"),
    (367, "zn/znkd0900"),
    (368, "zn/znkd1000"),
    (370, "zn/znkd1200"),
    (371, "zn/znkd1300"),
    (376, "zn/znkd1400"),
    (384, "op/open0100"),
    (385, "op/open0200"),
    (389, "zn/znkd0801"),
];

/// The frame the remaster draws the field in (16:9; wider windows get bars or crop around it).
pub const FIELD_FRAME: f32 = 16.0 / 9.0;

/// The vertical field of view for a script's screen distance as the remaster draws it: half the frame height is
/// 240 at that distance (fitted to captures of the original on three maps), and the distance is 1024 until a
/// script sets one.
pub fn field_fov_y(screen_distance: f32) -> f32 {
    let d = if screen_distance > 1.0 { screen_distance } else { 1024.0 };
    2.0 * (240.0 / d).atan()
}

/// Where a map model goes in game space: map models are a tenth of game scale and turned half round the Z axis
/// (x and y negated) against it (fitted to the walk meshes of eight maps, and to captures of the original).
pub fn map_place() -> Mat4 {
    Mat4::from_scale(Vec3::new(-10.0, -10.0, 10.0))
}

/// Where a character model goes in game space, standing at `pos` and facing `yaw`. The remaster's world matrix
/// for a character is a turn of yaw − 90° about Y after negating Y and Z, at the position with x and y negated
/// (its frame is game space turned half round Z); brought into game space that is a turn of −yaw − 90° about Y.
pub fn character_place(pos: [f32; 3], yaw: f32) -> Mat4 {
    Mat4::from_translation(Vec3::from(pos)) * Mat4::from_rotation_y(-yaw - std::f32::consts::FRAC_PI_2)
}

/// The scene camera's view and projection in the field frame (before the aspect policy): eye and focus in game
/// space, the vertical field of view, the roll, and the map's extent in game units (which keeps the whole map
/// inside the far plane however close the focus is).
pub fn script_camera(eye: [f32; 3], focus: [f32; 3], fov_y: f32, roll: f32, extent: f32) -> Mat4 {
    let r = game_to_view();
    let (eye, focus) = (r.transform_point3(Vec3::from(eye)), r.transform_point3(Vec3::from(focus)));
    let d = (focus - eye).length().max(1.0);
    let far = (d * 200.0).max(extent * 2.0 + (eye - focus).length());
    let near = (d * 0.01).clamp(0.05, 1.0);
    Mat4::from_scale(Vec3::new(1.0, -1.0, 1.0))
        * Mat4::perspective_rh(fov_y, FIELD_FRAME, near, far)
        * Mat4::from_rotation_z(roll)
        * Mat4::look_at_rh(eye, focus, Vec3::Y)
        * r
}

/// A map model's bounds in view space (for framing the orbit camera on it).
fn map_view_bounds(m: &ModelInfo) -> (Vec3, Vec3) {
    let t = game_to_view() * map_place();
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for i in 0..8 {
        let c = Vec3::new(
            if i & 1 == 0 { m.min.x } else { m.max.x },
            if i & 2 == 0 { m.min.y } else { m.max.y },
            if i & 4 == 0 { m.min.z } else { m.max.z },
        );
        let p = t.transform_point3(c);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (lo, hi)
}

/// The event file of an FFX event number, when the table has it.
fn ffx_event_path(n: i32) -> Option<String> {
    let (_, e) = FFX_EVENTS.iter().find(|(k, _)| *k == n)?;
    let name = e.rsplit('/').next().unwrap_or(e);
    Some(format!("ffx_ps2/ffx/master/jppc/event/obj/{e}/{name}.ebp"))
}

/// A joint of the character engine's pose: rotations in turns, translations in 1/4096 file units (the engine
/// multiplies them by the model's scale and 4096), scales.
/// A model's part matrices from its PS2 bones (as the character engine composes them each frame: a bone's world
/// is its parent's × its own local matrix, the motion's values where a motion sets the joint and the `.chr`
/// defaults elsewhere): World for skinned parts, World × the part's bone for rigid ones.
fn rig_parts(
    rig: &aerugo_sim::field::ChrRig,
    pose: Option<&(Vec<Option<[f32; 9]>>, f32)>,
    world: Mat4,
) -> Vec<Mat4> {
    let mut bones: Vec<Mat4> = Vec::with_capacity(rig.bones.len());
    for (i, b) in rig.bones.iter().enumerate() {
        let local = match pose.and_then(|(j, scale)| j.get(i).copied().flatten().map(|v| (v, *scale))) {
            Some((v, scale)) => engine_joint(v).matrix(scale),
            None => aerugo_formats::motion::JointPose {
                rotation: b.rotation,
                translation: b.translation,
                scale: b.scale,
            }
            .matrix(1.0),
        };
        let parent = b.parent.and_then(|p| bones.get(p).copied()).unwrap_or(Mat4::IDENTITY);
        bones.push(parent * Mat4::from_cols_array(&local));
    }
    rig.part_bones
        .iter()
        .zip(&rig.skinned)
        .map(|(&b, &skinned)| {
            if skinned { world } else { world * bones.get(usize::from(b)).copied().unwrap_or(Mat4::IDENTITY) }
        })
        .collect()
}

fn engine_joint(v: [f32; 9]) -> aerugo_formats::motion::JointPose {
    let tau = std::f32::consts::TAU;
    aerugo_formats::motion::JointPose {
        rotation: [v[0] * tau, v[1] * tau, v[2] * tau],
        translation: [v[3] * 4096.0, v[4] * 4096.0, v[5] * 4096.0],
        scale: [v[6], v[7], v[8]],
    }
}

/// The menu the title asks for to load a game.
const LOAD_GAME_MENU: u32 = 0x4010_0000;

/// The first event of a new game: the title script (it checks the memory card, shows the title menu and starts
/// the opening).
const FFX_TITLE_EVENT: &str = "ffx_ps2/ffx/master/jppc/event/obj/te/test20/test20.ebp";

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
            started: std::time::Instant::now(),
            clock: FixedTimestep::new(30, 4),
            last: None,
            paused: false,
            step_once: false,
            confirm: false,
            alt: false,
            cancel: false,
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
            movie: MoviePlayer::new(),
            game: false,
            pause_menu: false,
            quit: false,
        }
    }

    /// The game screen, starting a new game of FFX at its title script.
    pub fn game(lang: Language) -> FieldView {
        let mut v = FieldView::new(lang);
        v.game = true;
        v.camera_mode = CameraMode::Script;
        v.follow_player = true;
        v.open_later(FFX_TITLE_EVENT.to_owned());
        v
    }

    /// The game screen continuing from a save file of the original release (read, never written): the event
    /// its save image names (word 0), with the image in place.
    pub fn game_from_steam(lang: Language, data: &[u8]) -> Result<FieldView, String> {
        let save = SaveGame::import_steam(Game::Ffx, data).map_err(|e| e.to_string())?;
        let n = i32::from(u16::from_le_bytes([save.image[0], save.image[1]]));
        let path = ffx_event_path(n).ok_or_else(|| format!("event {n} is not in the event table yet"))?;
        let mut v = FieldView::game(lang);
        v.open_later(path);
        v.pending_restore = Some(save);
        Ok(v)
    }

    /// The language of the game text.
    pub fn language(&self) -> Language {
        self.lang
    }

    /// Whether the player chose to leave the game (cleared by asking).
    pub fn take_quit(&mut self) -> bool {
        std::mem::take(&mut self.quit)
    }

    pub fn close(&mut self, renderer: &mut Renderer, audio: Option<&AudioOutput>) {
        self.movie.close(renderer, audio);
        if let Some(mut s) = self.session.take() {
            s.phyre.clear(renderer);
        }
    }

    fn open(&mut self, vfs: &Vfs, renderer: &mut Renderer, audio: Option<&AudioOutput>, path: &str) {
        // The character engine's motion files carry over: an event change drops all but the shared party file.
        // A game that starts (from the title) or loads reads the party file with its first motion file.
        let previous =
            self.session.as_ref().map(|s| (s.event.state.motion_groups.clone(), s.event.path.clone()));
        // The engine's globals outlive the event.
        let globals = self.session.as_ref().map(|s| s.event.state.globals.clone());
        self.close(renderer, audio);
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
        let mut load = |p: &Option<String>| -> Option<ModelInfo> {
            let p = p.as_ref()?;
            match read_model_info(vfs, p) {
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
            let (lo, hi) = map_view_bounds(m);
            self.orbit = OrbitCamera::framing(lo, hi);
        }
        let from_title = previous.as_ref().is_some_and(|(_, p)| p == FFX_TITLE_EVENT);
        if let Some((mut groups, _)) = previous {
            groups.remove_all_but_party();
            event.state.motion_groups = groups;
        }
        if let Some(g) = globals {
            event.state.globals = g;
        }
        if self.pending_restore.is_some() || from_title {
            event.state.motion_groups.party_pending = true;
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
        log::info!("field: {path} (map {})", event.map_model.as_deref().unwrap_or("none"));
        self.session = Some(Session {
            restore_position,
            jump_choices: Vec::new(),
            map_cameras,
            event,
            map,
            layers,
            actors: HashMap::new(),
            notes,
            phyre: Default::default(),
            ui: Default::default(),
            font: None,
        });
        self.last = None;
        self.clock.reset();
    }

    /// Open an event on the next frame.
    pub fn open_later(&mut self, event: String) {
        self.pending_open = Some(event);
    }

    fn tick(&mut self, input: Input, vfs: &Vfs) {
        let Some(s) = &mut self.session else { return };
        // The player is the first party member with a body, until the scripts say otherwise.
        if s.event.state.player.is_none() && s.event.initialized() {
            s.event.state.player =
                s.event.state.actors.iter().position(|a| a.model.is_some_and(|m| m >> 12 == 0));
            if let (Some(p), Some(pos)) = (s.event.state.player, s.restore_position.take()) {
                s.event.state.actors[p].pos = pos;
            }
        }
        s.event.load_motion_frames(vfs);
        if let Some(path) = s.event.movie_needing_length() {
            let ticks = movie_ticks(vfs, path).unwrap_or(0);
            s.event.set_movie_frames(ticks);
        }
        s.event.step(input);
    }

    /// The title's Load Game screen (game screen): the saved games; loading one goes on from where it was saved,
    /// Back returns to the title.
    fn load_menu(&mut self, ui: &mut egui::Ui) {
        let dir = aerugo_platform::user_dir(false).join("saves");
        let slots: Vec<(u32, SaveGame)> = (1..=9)
            .filter_map(|n| {
                let b = std::fs::read(dir.join(format!("slot{n}.aesv"))).ok()?;
                Some((n, SaveGame::from_bytes(&b).ok()?))
            })
            .collect();
        let mut chosen = None;
        let mut back = false;
        egui::Area::new(egui::Id::new("game-load"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                game_frame().show(ui, |ui| {
                    ui.set_min_width(420.0);
                    ui.label(egui::RichText::new("Load Game").size(26.0).color(egui::Color32::WHITE));
                    ui.add_space(8.0);
                    if slots.is_empty() {
                        ui.label(
                            egui::RichText::new("No saved games yet.")
                                .size(18.0)
                                .color(egui::Color32::from_gray(210)),
                        );
                    }
                    for (n, save) in &slots {
                        let place = save.event.as_deref().and_then(|e| e.rsplit('/').nth(1)).unwrap_or("?");
                        let time = save.ticks / 30;
                        let label = format!(
                            "Slot {n} · {place} · {}:{:02}:{:02}",
                            time / 3600,
                            time / 60 % 60,
                            time % 60
                        );
                        if ui.button(egui::RichText::new(label).size(18.0)).clicked() {
                            chosen = Some(save.clone());
                        }
                    }
                    ui.add_space(8.0);
                    if ui.button(egui::RichText::new("Back").size(18.0)).clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Backspace))
                    {
                        back = true;
                    }
                });
            });
        if let Some(save) = chosen {
            if let Some(ev) = save.event.clone() {
                self.pending_open = Some(ev);
            }
            self.pending_restore = Some(save);
        } else if back && let Some(s) = &mut self.session {
            s.event.close_menu();
        }
    }

    /// The menu the scripts asked for: saving (save spheres), shops; others just close.
    fn menu_window(&mut self, ui: &mut egui::Ui, mode: u32) {
        if self.game && mode == LOAD_GAME_MENU {
            self.load_menu(ui);
            return;
        }
        use aerugo_sim::field::menu_mode;
        use aerugo_sim::items;
        let slot = &mut self.slot;
        let Some(s) = &mut self.session else { return };
        let mut close = false;
        egui::Window::new("Menu").collapsible(false).resizable(true).show(ui.ctx(), |ui| {
            let ev = &mut s.event;
            let tables = ev.state.items.clone();
            let rate = ev.state.shop_rate.unwrap_or(100);
            let image = &mut ev.vm.memory.global[0];
            ui.label(format!("Gil: {}", items::gil(image)));
            match menu_mode::kind(mode) {
                menu_mode::SAVE => {
                    ui.label("Save sphere");
                    ui.horizontal(|ui| {
                        ui.label("Slot");
                        ui.add(egui::DragValue::new(slot).range(1..=9));
                        if ui.button("Save").clicked() {
                            let dir = aerugo_platform::user_dir(false).join("saves");
                            let path = dir.join(format!("slot{slot}.aesv"));
                            let msg = match write_atomically(&path, &ev.save().to_bytes()) {
                                Ok(()) => format!("saved to {}", path.display()),
                                Err(e) => format!("save failed: {e}"),
                            };
                            s.notes.push(msg);
                        }
                    });
                }
                menu_mode::ITEM_SHOP => {
                    let Some(t) = tables else { return };
                    let variant = items::text_variant(image);
                    ui.label("Buy");
                    for id in t.item_shop((mode & 0xFFFF) as u16) {
                        ui.horizontal(|ui| {
                            let price = t.item_buy_price(id, rate);
                            ui.label(format!(
                                "{} — {price} gil (have {})",
                                t.item_name(id, variant),
                                items::item_count(image, id)
                            ));
                            if ui.button("Buy").clicked() && !t.buy_item(image, id, 1, rate) {
                                s.notes.push("cannot buy".into());
                            }
                        });
                    }
                    ui.separator();
                    ui.label("Sell");
                    let held: Vec<u16> = (0..items::ITEM_SLOTS)
                        .map(|i| {
                            let at = items::offset::ITEM_IDS + i * 2;
                            u16::from_le_bytes([image[at], image[at + 1]])
                        })
                        .filter(|&id| id != 0xFF && id != 0 && items::item_count(image, id) > 0)
                        .collect();
                    for id in held {
                        ui.horizontal(|ui| {
                            let price = t.item_sell_price(id);
                            ui.label(format!(
                                "{} ×{} — {price} gil",
                                t.item_name(id, variant),
                                items::item_count(image, id)
                            ));
                            if ui.button("Sell").clicked() {
                                t.sell_item(image, id, 1);
                            }
                        });
                    }
                }
                menu_mode::WEAPON_SHOP => {
                    let Some(t) = tables else { return };
                    let variant = items::text_variant(image);
                    for e in t.weapon_shop((mode & 0xFFFF) as u16) {
                        ui.horizontal(|ui| {
                            let name = t.equipment_name(u16::from_le_bytes([e[0], e[1]]), e[4], variant);
                            let price = t.equipment_buy_price(&e, rate);
                            ui.label(format!("{name} — {price} gil"));
                            if ui.button("Buy").clicked() && !t.buy_equipment(image, &e, rate) {
                                s.notes.push("cannot buy".into());
                            }
                        });
                    }
                }
                menu_mode::NAME_ENTRY => {
                    ui.label("Name entry: the default name is kept.");
                }
                _ => {
                    ui.label(format!("Menu {mode:#010x} is not implemented yet."));
                }
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if close {
            s.event.close_menu();
        }
    }

    /// Start or stop the movie player to follow the scripts' movie.
    fn sync_movie(&mut self, vfs: &Arc<Vfs>, renderer: &mut Renderer, audio: Option<&AudioOutput>) {
        let want = self
            .session
            .as_ref()
            .and_then(|s| s.event.state.movie.as_ref().filter(|m| m.playing).and_then(|m| m.path.clone()));
        if want.as_deref() != self.movie.playing_path() {
            match want {
                Some(path) => self.movie.open(vfs, renderer, audio, path),
                None => self.movie.close(renderer, audio),
            }
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        vfs: &Arc<Vfs>,
        renderer: &mut Renderer,
        audio: Option<&AudioOutput>,
    ) {
        let mut picked = None;
        if !self.game {
            let files = self.files.get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".ebp")));
            egui::Panel::left("field-files").resizable(true).default_size(260.0).show(ui, |ui| {
                picked = files.ui(ui);
            });
        }
        if let Some(p) = picked.or_else(|| self.pending_open.take()) {
            self.open(vfs, renderer, audio, &p);
        }
        // Input and timing.
        // WASD or the arrow keys walk (Shift held: slower), Enter / Space confirms and talks, Backspace cancels, Q
        // talks with the second button, Escape skips a movie (on the game screen, otherwise pauses).
        let (stick, confirm_now, alt_now, cancel_now, walk) = ui.input(|i| {
            let down = |a: egui::Key, b: egui::Key| i.key_down(a) || i.key_down(b);
            let k = |pos: bool, neg: bool| f32::from(u8::from(pos)) - f32::from(u8::from(neg));
            (
                [
                    k(down(egui::Key::D, egui::Key::ArrowRight), down(egui::Key::A, egui::Key::ArrowLeft)),
                    k(down(egui::Key::W, egui::Key::ArrowUp), down(egui::Key::S, egui::Key::ArrowDown)),
                ],
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::Q),
                i.key_pressed(egui::Key::Backspace),
                i.modifiers.shift,
            )
        });
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        // The buttons held, as the scripts' word has them: confirm 0x20 (with Start 0x800 on the game screen, so
        // Enter also skips the logos to the title), cancel 0x40, the second talk button 0x80, the d-pad.
        let game = self.game;
        let held = ui.input(|i| {
            let mut w = 0u32;
            if i.key_down(egui::Key::Enter) || i.key_down(egui::Key::Space) {
                w |= 0x20 | if game { 0x800 } else { 0 };
            }
            if i.key_down(egui::Key::Backspace) {
                w |= 0x40;
            }
            if i.key_down(egui::Key::Q) {
                w |= 0x80;
            }
            for (keys, bit) in [
                ([egui::Key::W, egui::Key::ArrowUp], 0x1000),
                ([egui::Key::D, egui::Key::ArrowRight], 0x2000),
                ([egui::Key::S, egui::Key::ArrowDown], 0x4000),
                ([egui::Key::A, egui::Key::ArrowLeft], 0x8000),
            ] {
                if keys.iter().any(|&k| i.key_down(k)) {
                    w |= bit;
                }
            }
            w
        });
        let movie_playing =
            self.session.as_ref().is_some_and(|s| s.event.state.movie.as_ref().is_some_and(|m| m.playing));
        if self.game && escape && !movie_playing {
            self.pause_menu = !self.pause_menu;
        }
        let (stick, confirm_now, alt_now, cancel_now) = if self.pause_menu {
            ([0.0, 0.0], false, false, false)
        } else {
            (stick, confirm_now, alt_now, cancel_now)
        };
        self.confirm |= confirm_now;
        self.alt |= alt_now;
        self.cancel |= cancel_now;
        let now = Instant::now();
        let elapsed = self.last.map_or(std::time::Duration::ZERO, |t| now - t);
        self.last = Some(now);
        let steps = if self.paused || self.pause_menu {
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
                cancel: std::mem::take(&mut self.cancel),
                alt: std::mem::take(&mut self.alt),
                stick_pressed,
                walk,
                camera_heading: self.camera_heading,
                pad: 0,
                held: if self.pause_menu { 0 } else { held },
            };
            self.tick(input, vfs);
        }
        if escape
            && movie_playing
            && let Some(s) = &mut self.session
        {
            s.event.skip_movie();
        }
        self.sync_movie(vfs, renderer, audio);
        if let Some(mode) = self.session.as_ref().and_then(|s| s.event.open_menu()) {
            self.menu_window(ui, mode);
        }
        // Battles are not ported yet: a placeholder that ends them as won.
        if let Some(s) = &mut self.session
            && let Some(b) = s.event.running_battle()
        {
            let name = s.event.battle_name(&b);
            let mut won = false;
            egui::Window::new("Battle").collapsible(false).show(ui.ctx(), |ui| {
                ui.label(format!(
                    "{} battle {name} (group {}, formation {}).",
                    if b.scripted { "Scripted" } else { "Random" },
                    b.group,
                    b.formation
                ));
                won = ui.button("Win").clicked();
            });
            if won {
                s.event.finish_battle();
            }
        }
        self.handle_jump(vfs);
        if !self.game {
            egui::Panel::right("field-info").resizable(true).default_size(360.0).show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.info_ui(ui));
            });
        }
        let panel = if self.game {
            egui::CentralPanel::no_frame().frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
        } else {
            egui::CentralPanel::default_margins()
        };
        panel.show(ui, |ui| {
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            if self.session.is_none() {
                if !self.game {
                    ui.label("Pick an event on the left. WASD walks the player, Enter confirms messages.");
                }
                return;
            }
            let rect = ui.available_rect_before_wrap();
            if !self.movie.show(ui, renderer, rect) {
                self.viewport(ui, vfs, renderer);
            }
        });
        if self.pause_menu {
            self.pause_ui(ui);
        }
        ui.ctx().request_repaint();
    }

    /// Events whose header names map `n` (variants for other platforms and test events left out).
    fn jump_candidates(&mut self, vfs: &Vfs, game: Game, n: i16) -> Vec<String> {
        // FFX's event numbers name scripts through the engine's file table.
        if game == Game::Ffx
            && let Some(path) = usize::try_from(n)
                .ok()
                .and_then(|n| aerugo_sim::field::FileTable::load(vfs)?.event_script(n))
                .or_else(|| ffx_event_path(i32::from(n)))
        {
            return vec![path];
        }
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
        log::info!("field: map change to {target} entrance {entrance}: {candidates:?}");
        if (self.follow_jumps && candidates.len() == 1) || (self.game && !candidates.is_empty()) {
            s.notes.push(format!("jump to map {target} entrance {entrance}: {}", candidates[0]));
            self.pending_open = Some(candidates[0].clone());
            // The game goes on in the next event: the save image carries over as the map change left it (the
            // map and entrance moved to the previous ones, the new ones in their place).
            let mut save = s.event.save();
            save.position = None;
            if s.event.state.next_loading_screen {
                log::error!("GAP: the remaster's loading screen before the next scene is not shown");
            }
            self.pending_restore = Some(save);
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
        for (i, w) in
            ev.state.windows.iter().enumerate().filter(|(_, w)| w.atel_state != 0 || w.message.is_some())
        {
            let text =
                w.message.and_then(|m| ev.state.messages.get(m)).map(|m| m.raw.clone()).unwrap_or_default();
            ui.label(format!("{i} (state {}) #{:?}: {text}", w.atel_state, w.message));
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
                let m = path.as_ref().and_then(|p| read_model_info(vfs, p).ok());
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
        // The scene camera draws a 16:9 frame (the remaster's), the PS2 map cameras their layers' frame; both
        // follow the aspect policy around it.
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
        // The game screen shows the scene's camera; the orbit camera only stands in until a script sets one.
        if self.game {
            self.camera_mode = if cam_eye.is_some() && cam_focus.is_some() {
                CameraMode::Script
            } else {
                CameraMode::Follow
            };
        }
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
                let extent = s.map.as_ref().map_or(0.0, |m| (m.max - m.min).length() * 10.0);
                let fov = field_fov_y(st.camera.screen_distance);
                policy(script_camera(eye, focus, fov, st.camera.roll, extent), FIELD_FRAME)
            }
            (CameraMode::Map, _, _, Some(vp)) => vp,
            _ => self.orbit.view_proj(aspect),
        };
        // Everything is drawn with the remaster's own shaders: the game screen's script camera as the original's,
        // the viewer's other cameras through their view-projection.
        let settings = s.map.as_ref().and_then(|m| m.scene);
        let mut scene = match (self.game, self.camera_mode, cam_eye, cam_focus) {
            (true, CameraMode::Script, Some(eye), Some(focus)) => {
                crate::phyre::scene::SceneConstants::camera(
                    Vec3::from(eye),
                    Vec3::from(focus),
                    st.camera.screen_distance,
                    px.0 as f32,
                    px.1 as f32,
                )
            }
            _ => crate::phyre::scene::SceneConstants::from_view_proj(view_proj, px.0 as f32, px.1 as f32),
        };
        match settings.as_ref().filter(|_| self.game) {
            Some(set) => {
                // The map's fog with what the scripts set (its limit stays 0, no fog, until they set one) and its
                // background colour.
                let f = crate::assets::scene_fog(set);
                scene.fog_color = f.color;
                scene.fog_near = st.fog.near.unwrap_or(f.near);
                scene.fog_far = st.fog.far.unwrap_or(f.far);
                scene.fog_curve = st.fog.curve.unwrap_or(f.curve);
                scene.fog_limit = st.fog.limit;
                // The scene's background: the map's, until a script sets one (8045 → yiSetBackColor).
                scene.background =
                    st.background.unwrap_or([set.background[0], set.background[1], set.background[2]]);
            }
            None if self.game => {}
            None => scene.background = [0.05, 0.05, 0.08],
        }
        // Scene time: the event's ticks at 30 a second, plus the share of the next.
        scene.time = (s.event.tick as f32 + alpha) / 30.0;
        let cl = crate::assets::character_light(
            &st.lights,
            settings.as_ref().map(crate::assets::scene_glow),
            s.event.map_model.as_deref().unwrap_or(""),
        );
        let lighting =
            crate::phyre::field::Lighting { lights: cl.lights, ambient: cl.ambient, shade_coeff: cl.shade };
        // Alpha 0 (native 5033) hides a character as the original does; under 1 it draws blended.
        let mut paths = Vec::new();
        for (w, a) in st.actors.iter().enumerate().filter(|(_, a)| a.visible && a.color[3].value > 0.0) {
            let (Some(id), Some(path)) = (a.model, a.model_path(s.event.game)) else { continue };
            let (pos, yaw) = view.actors.get(w).copied().unwrap_or((a.pos, a.yaw));
            // The character engine's pose (each PS2 joint the motions set) and the model's translation scale.
            let scale = st.model_motion_scales.get(&id).copied().flatten().unwrap_or(0.001);
            let pose = a.player.as_ref().map(|p| (p.pose(), scale));
            let event_motion = a.player.as_ref().and_then(|p| p.motion_file).is_some_and(|k| k.0 == 1);
            let drawn = [pos[0] + a.draw_offset[0], pos[1] + a.draw_offset[1], pos[2] + a.draw_offset[2]];
            let place = character_place(drawn, yaw);
            // The walk-mesh shade is taken where the character is drawn: for an event motion that is where its
            // root (joint 1) has carried it, not the actor's own position.
            let shade = pose
                .as_ref()
                .filter(|_| event_motion && a.shade_from_ground)
                .and_then(|(joints, sc)| {
                    let root = Vec3::from(engine_joint(joints.get(1).copied().flatten()?).translation) * *sc;
                    st.ground.as_ref().and_then(|g| g.shade_at(place.transform_point3(root).to_array()))
                })
                .unwrap_or(a.shade.value);
            // The remaster's run-time material colour: the character's colour × (shade × 0.85 + 0.15); the
            // colour is also its shade colour.
            let rgb = [a.color[0].value, a.color[1].value, a.color[2].value];
            let k = shade * 0.85 + 0.15;
            let world = crate::phyre::field::character_world(drawn, yaw);
            let rig = st.model_rigs.get(&id).cloned().flatten();
            let (parts, all_skinned) = match &rig {
                Some(r) => (rig_parts(r, pose.as_ref(), world), r.skinned.iter().all(|&s| s)),
                None => (Vec::new(), false),
            };
            let caster = caster_source(st, w, id, path.as_str(), a.shade.value);
            paths.push((
                path,
                (world, parts, all_skinned),
                pose,
                rgb,
                k,
                a.color[3].value.min(1.0),
                (st.model_faces.get(&id).and_then(|f| f.1.as_deref()), a.face_layers.as_slice()),
                caster,
            ));
        }
        let poses: Vec<_> = paths
            .iter()
            .map(|(_, _, pose, ..)| {
                move |_: usize, sk: &aerugo_formats::skeleton::Skeleton| {
                    let mut local = sk.default_pose.clone();
                    if let Some((joints, scale)) = pose {
                        for (i, name) in sk.names.iter().enumerate() {
                            if let Some(v) = ps2_joint(name).and_then(|j| joints.get(j).copied().flatten()) {
                                local[i] = engine_joint(v).matrix(*scale);
                            }
                        }
                    }
                    local
                }
            })
            .collect();
        let actors: Vec<crate::phyre::field::ActorDraw<'_>> = paths
            .iter()
            .zip(&poses)
            .map(|((path, (world, parts, all_skinned), _, rgb, k, alpha, (face, layers), caster), pose)| {
                crate::phyre::field::ActorDraw {
                    path,
                    world: *world,
                    parts: parts.clone(),
                    all_skinned: *all_skinned,
                    pose,
                    material: [rgb[0] * k, rgb[1] * k, rgb[2] * k, *alpha],
                    shade: *rgb,
                    face: *face,
                    face_layers: layers,
                    caster: caster.clone(),
                }
            })
            .collect();
        let msaa = renderer.msaa();
        let (hdao, unsharp, brightness) = renderer.post_settings();
        let post = crate::phyre::post::PostSettings {
            hdao,
            glow: settings.as_ref().map(|set| {
                let g = crate::assets::scene_glow(set);
                [g.upper, g.lower, g.gain, g.spread]
            }),
            unsharp,
            // The remaster's SMAA4 setting (quality 4) and anisotropy (the original holds 4 at run time).
            smaa: 4,
            brightness,
            anisotropy: 4.0,
            fades: st
                .screen
                .passes
                .iter()
                .map(|p| ([p.rgb[0], p.rgb[1], p.rgb[2], p.alpha], p.flags))
                .collect(),
        };
        // The map's 2D layers sit where the map does (ten times their file size).
        let layers: Vec<(&str, Mat4)> = s
            .event
            .map_layers
            .as_deref()
            .filter(|_| self.show_layers && s.layers.is_some())
            .map(|p| (p, Mat4::from_scale(Vec3::splat(10.0))))
            .into_iter()
            .collect();
        let objects = crate::phyre::field::Objects {
            hidden_map_groups: st.hidden_map_groups,
            map: s.event.map_model.as_deref(),
            map_poses: &s.event.state.keyframes.poses,
            texture_animations: s.event.state.texture_animations.as_ref(),
            models: &layers,
            actors: &actors,
            vfx: &s.event.state.vfx,
            // The map table's story progress: the save image's u16 at 0xbec (global area 0 is the save image).
            shadow: Some(crate::phyre::field::ShadowSetup {
                map_id: st.map_id,
                event: u32::from(st.scene.unwrap_or(0)),
                progress: s.event.vm.memory.global[0]
                    .get(0xbec..0xbee)
                    .map_or(0, |b| u32::from(u16::from_le_bytes([b[0], b[1]]))),
                quality: renderer.shadow(),
            }),
        };
        let post = self.game.then_some(&post);
        // Windows the game's 2D layer drew (the rest keep the stand-in).
        let mut drawn_windows: Vec<usize> = Vec::new();
        if let Some(mut frame) =
            s.phyre.frame(vfs, renderer, &objects, &scene, &lighting, px, msaa, post, 4.0, false)
        {
            // The event's 2D shapes (the credits), from the event's texture folder.
            let shapes = st.shapes.draws(&s.event.vm.memory, st.scene.unwrap_or(0));
            if !shapes.is_empty() {
                let dir = s
                    .event
                    .path
                    .split_once("/event/")
                    .map(|(_, rest)| rest.rsplit_once('/').map_or(rest, |(d, _)| d))
                    .map(|d| format!("ffx_data/gamedata/ps3data/event/{d}/tex/d3d11"))
                    .unwrap_or_default();
                s.phyre.draw_2d_shapes(vfs, renderer, &shapes, &dir, px, &mut frame);
            }
            if self.game {
                let lang = self.lang;
                let font = s.font.get_or_insert_with(|| {
                    let code = lang.master_code();
                    [
                        format!("ffx_ps2/ffx/master/{code}pc/menu/base.ftc"),
                        format!("ffx_ps2/ffx/master/new_{code}pc/menu/base.ftc"),
                    ]
                    .iter()
                    .find_map(|p| vfs.read(p).ok().and_then(|d| aerugo_formats::ftc::Ftc::parse(&d).ok()))
                });
                if let Some(font) = font.as_ref() {
                    let (canvas, drawn) =
                        message_canvas(st, font, px, lang, self.started.elapsed().as_secs_f32());
                    s.ui.draw(vfs, renderer, &mut s.phyre.programs, &canvas, lang.code(), px, &mut frame);
                    drawn_windows = drawn;
                }
            }
            // AERUGO_FRAME_DUMP=<file>: the frame is written in the oracle's capture form whenever the file does not
            // exist (delete it to take another).
            if let Ok(path) = std::env::var("AERUGO_FRAME_DUMP")
                && !std::path::Path::new(&path).exists()
                && let Err(e) = crate::phyre::dump_frame(&frame, &s.phyre.programs.hashes(), &path)
            {
                log::warn!("frame dump: {e}");
            }
            renderer.submit_frame(frame);
        }
        // A window the game's 2D layer did not draw is a gap on the game screen; the debug viewer shows its text.
        let rect = response.rect;
        for (i, w) in
            st.windows.iter().enumerate().filter(|(i, w)| w.to.text.on && !drawn_windows.contains(i))
        {
            if self.game {
                log::error!(
                    "GAP: message window {i} (frame type {:?}) is not drawn by the 2D layer",
                    w.style
                );
                continue;
            }
            let Some(msg) = w.message.and_then(|m| st.messages.get(m)) else { continue };
            // Revealed so far; the last lines of a choice message are its choices.
            let shown: String = msg.text.chars().take(w.to.text.shown.max(0) as usize).collect();
            let lines: Vec<&str> = shown.lines().collect();
            let total = msg.text.lines().count();
            let first_choice = total.saturating_sub(usize::from(msg.choices));
            let text = lines
                .iter()
                .enumerate()
                .map(|(k, l)| {
                    if msg.choices > 0 && k >= first_choice {
                        format!("▸ {}", l.trim_end())
                    } else {
                        (*l).to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let pos =
                rect.min + egui::vec2(w.x as f32 / 512.0 * rect.width(), w.y as f32 / 448.0 * rect.height());
            let pivot = match w.align {
                4 => egui::Align2::CENTER_CENTER,
                2 => egui::Align2::RIGHT_CENTER,
                _ => egui::Align2::LEFT_TOP,
            };
            egui::Area::new(egui::Id::new(("field-window", i))).fixed_pos(pos).pivot(pivot).show(
                ui.ctx(),
                |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(rect.width() * 0.6);
                        ui.label(text);
                        ui.weak("Enter ▸");
                    });
                },
            );
        }
    }
}

impl FieldView {
    /// The game screen's pause menu: resume or leave to the launcher.
    fn pause_ui(&mut self, ui: &mut egui::Ui) {
        let screen = ui.ctx().content_rect();
        egui::Area::new(egui::Id::new("game-pause-dim"))
            .fixed_pos(screen.min)
            .order(egui::Order::Middle)
            .show(ui.ctx(), |ui| {
                ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
            });
        egui::Area::new(egui::Id::new("game-pause"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                game_frame().show(ui, |ui| {
                    ui.set_min_width(260.0);
                    ui.vertical_centered(|ui| {
                        ui.label(egui::RichText::new("Paused").size(28.0).color(egui::Color32::WHITE));
                        ui.add_space(12.0);
                        if ui.button(egui::RichText::new("Resume").size(20.0)).clicked() {
                            self.pause_menu = false;
                        }
                        ui.add_space(6.0);
                        if ui.button(egui::RichText::new("Quit to launcher").size(20.0)).clicked() {
                            self.pause_menu = false;
                            self.quit = true;
                        }
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(
                                "Arrows / WASD move · Enter talk, confirm · Backspace cancel",
                            )
                            .size(14.0)
                            .color(egui::Color32::from_gray(200)),
                        );
                    });
                });
            });
    }
}

/// A map change in the save image: word 0 is the event, word 2 the one before; byte 0xC the entrance, 0xD the one
/// before.
/// Ids that never cast a shadow.
const NO_SHADOW: [u16; 9] = [0x106a, 0x106d, 0x1077, 0x1088, 0x1083, 0x1089, 0x108c, 0x109f, 0x3016];

/// A character's shadow-manager values when it casts this frame: in use and shown (+0x198 bit 0x10), not in
/// motion modes 2 or 3, its shadow type (+0x183, after the hand-off's per-model rules for this map) 1 or 3,
/// and not one of the ids that never cast. The scale is the root's second column's length × 1000.
fn caster_source(
    st: &aerugo_sim::field::FieldState,
    w: usize,
    id: u16,
    path: &str,
    shade: f32,
) -> Option<crate::phyre::field::CasterSource> {
    let ch = st.chrs.get(w)?.as_ref()?;
    if ch.u8(2) == 0 || ch.u32(0x198) & 0x10 == 0 || matches!(ch.u8(0x186), 2 | 3) || NO_SHADOW.contains(&id)
    {
        return None;
    }
    let map = st.map_id;
    let mut kind = ch.u8(0x183);
    let model = ch.u16(0);
    if matches!(model, 0x5009 | 0x500a | 0x501d | 0x5034 | 0x5055 | 0x5056)
        || (model == 0x5033 && map != 0x200f)
        || (model == 0x4029 && map != 0xfc1 && map != 0x1c36)
    {
        kind = 1;
    }
    if kind != 1 && kind != 3 {
        return None;
    }
    let root = st.chr_frames.get(w).cloned().flatten()?.world;
    let scale = (((f64::from(root[4]) * f64::from(root[4]) + f64::from(root[5]) * f64::from(root[5]))
        + f64::from(root[6]) * f64::from(root[6]))
    .sqrt()
        * 1000.0) as f32;
    let name = path.rsplit('/').next()?.split('.').next()?.to_owned();
    Some(crate::phyre::field::CasterSource { key: w, name, id, scale, ground: ch.f32(0x16c), shade })
}

/// The frame of a game box: deep blue, half see-through, with a light edge (the look of FFX's windows).
/// The open message windows as the game's 2D layer draws them, for an output of `px` pixels, and which windows
/// those are. Frame types 0 (system windows), 1 and 2 (dialogue, no frame) are drawn; the shape-built frames of
/// the other types are not ported yet.
fn message_canvas(
    st: &aerugo_sim::field::FieldState,
    font: &aerugo_formats::ftc::Ftc,
    px: (u32, u32),
    lang: Language,
    time: f32,
) -> (aerugo_sim::ui2d::Canvas, Vec<usize>) {
    use aerugo_sim::msgwin;
    let mut canvas = aerugo_sim::ui2d::Canvas::new();
    let mut drawn = Vec::new();
    let metrics = msgwin::Metrics::new(px.0, px.1);
    // TODrawMessageWindow: each window's frame (while up), then its text (while on) — the current page.
    for (i, w) in st.windows.iter().enumerate() {
        let (f, t) = (&w.to.frame, &w.to.text);
        if t.hidden || !(f.on || t.on) {
            continue;
        }
        let Some(msg) = w.message.and_then(|m| st.messages.get(m)) else { continue };
        let frame_type = usize::from(w.style.unwrap_or(0));
        if frame_type > 2 {
            continue;
        }
        drawn.push(i);
        let end = t.end.unwrap_or(msg.bytes.len()).min(msg.bytes.len());
        let page = msg.bytes.get(t.start.min(end)..end).unwrap_or_default();
        let kind = msgwin::TextKind::from_flags(w.flags as u16);
        let (tw, th) = msgwin::measure(page, font, &metrics, kind, frame_type);
        let placement = msgwin::place(&metrics, frame_type, w.align, w.x, w.y, tw, th);
        let draw = msgwin::WindowDraw {
            bytes: page,
            placement,
            frame_type,
            kind,
            shown: if t.on { t.shown.max(0) as usize } else { 0 },
            choice: (t.choices && f.state == 2).then_some(w.to.choice.highlighted.max(0) as usize),
            time,
            frame_on: f.on,
            frame_t: if frame_type == 0 { f.t } else { 0x1000 },
        };
        msgwin::draw(&mut canvas, font, &metrics, &draw, lang == Language::Japanese);
    }
    (canvas, drawn)
}

fn game_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgba_unmultiplied(18, 32, 92, 215))
        .stroke(egui::Stroke::new(2.0, egui::Color32::from_rgb(196, 204, 230)))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(18, 12))
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
