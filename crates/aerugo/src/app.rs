//! The main loop: events, simulation, egui and rendering.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use aerugo_debug_ui::widgets::FrameTimeGraph;
use aerugo_debug_ui::{EguiSdl, egui};
use aerugo_gfx::{ContextOptions, EguiFrame, FrameInput, PresentMode, Quad, Renderer};
use aerugo_platform::sdl3::event::{Event, WindowEvent};
use aerugo_platform::sdl3::keyboard::{Keycode, Mod};
use aerugo_platform::{FrameLimiter, Platform, WindowConfig};
use aerugo_vfs::{Install, Language};
use anyhow::Result;

use crate::launcher::{InstallSource, Launcher, LauncherAction, entry_status};
use crate::scene::SceneState;
use crate::settings::{MusicSet, Settings, VoiceLanguage};
use crate::viewer::{Viewer, ViewerAction};

const FRAME_LIMIT_PRESETS: [u32; 8] = [0, 30, 60, 120, 144, 165, 240, 360];

/// Start-up choices resolved from the command line and settings.
#[derive(Debug, Clone)]
pub struct StartOptions {
    pub settings_path: PathBuf,
    /// Settings as stored in the file.
    pub file_settings: Settings,
    /// Settings with command-line overrides applied; these are what the session uses.
    pub settings: Settings,
    pub install: Option<(Install, InstallSource)>,
    pub notice: Option<String>,
    pub test_scene: bool,
    /// Open the asset viewer instead of the launcher.
    pub viewer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Launcher,
    TestScene,
    Viewer,
}

/// Renderer facts shown in the UI, copied once per frame.
#[derive(Debug, Clone)]
struct RenderInfo {
    device: String,
    extent: (u32, u32),
    format: String,
    present_mode: PresentMode,
    available: Vec<PresentMode>,
}

/// State the egui code reads and edits.
#[derive(Debug)]
struct UiState {
    screen: Screen,
    launcher: Launcher,
    scene: SceneState,
    viewer: Option<Viewer>,
    settings: Settings,
    show_debug: bool,
    show_settings: bool,
    graph: FrameTimeGraph,
    last_frame_ms: f32,
}

#[derive(Debug, Clone, PartialEq)]
enum Action {
    Launcher(LauncherAction),
    BackToLauncher,
}

/// Fields drop in declaration order: GPU objects (including the window surface) first, then egui (which owns an
/// SDL cursor), and the SDL platform last.
pub struct App {
    renderer: Renderer,
    egui: EguiSdl,
    limiter: FrameLimiter,
    quads: Vec<Quad>,
    ui: UiState,
    /// Settings as last applied, to detect edits.
    applied: Settings,
    /// Effective settings at start (file plus command-line overrides).
    launch: Settings,
    /// The settings file as it was at start; untouched command-line overrides keep these values.
    launch_file: Settings,
    /// Settings as last written to the file.
    saved: Settings,
    settings_path: PathBuf,
    last_save: Instant,
    last_frame: Instant,
    quit: bool,
    cjk_fonts: bool,
    platform: Platform,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App").field("renderer", &self.renderer).finish_non_exhaustive()
    }
}

impl App {
    pub fn new(opts: StartOptions) -> Result<Self> {
        let start_in_viewer = opts.viewer;
        let settings = opts.settings;
        let platform = Platform::new(&WindowConfig {
            title: "Aerugo".into(),
            width: settings.window.width.max(640),
            height: settings.window.height.max(360),
            fullscreen: settings.window.fullscreen,
        })?;
        let present_mode =
            PresentMode::from_name(&settings.graphics.present_mode).unwrap_or(PresentMode::Fifo);
        let gpu = Some(settings.graphics.gpu.clone()).filter(|g| !g.is_empty());
        let extensions = platform.vulkan_instance_extensions()?;
        let renderer = Renderer::new(
            &platform.window,
            &extensions,
            platform.drawable_size(),
            present_mode,
            &ContextOptions { validation: settings.graphics.validation, preferred_gpu: gpu },
        )?;
        log::info!("renderer ready on {}", renderer.device_name());

        let mut egui = EguiSdl::new();
        egui.ui_scale = settings.ui.scale;
        let mut launcher = Launcher::new(opts.install);
        launcher.notice = opts.notice;
        let ui = UiState {
            screen: if opts.test_scene { Screen::TestScene } else { Screen::Launcher },
            launcher,
            scene: SceneState::new(),
            viewer: None,
            settings: settings.clone(),
            show_debug: opts.test_scene,
            show_settings: false,
            graph: FrameTimeGraph::default(),
            last_frame_ms: 0.0,
        };
        let mut app = Self {
            platform,
            egui,
            limiter: FrameLimiter::new(Some(settings.graphics.frame_limit)),
            quads: Vec::new(),
            ui,
            applied: settings.clone(),
            launch: settings,
            launch_file: opts.file_settings.clone(),
            saved: opts.file_settings,
            settings_path: opts.settings_path,
            last_save: Instant::now(),
            last_frame: Instant::now(),
            quit: false,
            cjk_fonts: false,
            renderer,
        };
        if start_in_viewer {
            app.open_viewer();
        }
        Ok(app)
    }

    pub fn run(&mut self) -> Result<()> {
        while !self.quit {
            self.limiter.wait();
            let now = Instant::now();
            let elapsed = now - self.last_frame;
            self.last_frame = now;
            self.ui.last_frame_ms = elapsed.as_secs_f32() * 1000.0;
            self.ui.graph.push(self.ui.last_frame_ms);

            self.handle_events();
            self.renderer.resize(self.platform.drawable_size());

            self.quads.clear();
            if self.ui.screen == Screen::TestScene {
                // The square the scene lives in, so letterboxing is visible.
                self.quads.push(Quad {
                    center: [0.0, 0.0],
                    half_size: [1.0, 1.0],
                    rotation: 0.0,
                    color: [0.07, 0.07, 0.09, 1.0],
                });
                let mut items = Vec::new();
                self.ui.scene.update(elapsed, &mut items);
                self.quads.extend(items);
            }

            let info = self.render_info();
            let clipboard = self.platform.video.clipboard();
            let mut actions = Vec::new();
            let ui_state = &mut self.ui;
            let renderer = &mut self.renderer;
            let mut frame = self.egui.run(&self.platform.window, &clipboard, |ui| {
                draw_ui(ui, ui_state, renderer, &info, &mut actions);
            });
            for action in actions {
                self.handle_action(action);
            }

            let presented = self.renderer.render(&FrameInput {
                clear_color: [0.03, 0.03, 0.04, 1.0],
                quads: &self.quads,
                egui: Some(EguiFrame {
                    textures: &frame.textures,
                    primitives: &frame.primitives,
                    pixels_per_point: frame.pixels_per_point,
                }),
            });
            // Texture changes were applied by the renderer (or must be dropped with an error); egui requires an
            // explicit acknowledgement either way.
            frame.textures.clear();
            let presented = presented?;
            if !presented && self.platform.drawable_size().0 == 0 {
                std::thread::sleep(Duration::from_millis(16)); // minimized
            }
            self.apply_settings();
        }
        self.remember_window_size();
        self.save_settings();
        Ok(())
    }

    fn render_info(&self) -> RenderInfo {
        let sc = self.renderer.swapchain();
        RenderInfo {
            device: self.renderer.device_name().to_owned(),
            extent: (sc.extent.width, sc.extent.height),
            format: format!("{:?}", sc.format),
            present_mode: sc.present_mode,
            available: sc.available_modes.clone(),
        }
    }

    fn handle_events(&mut self) {
        let events: Vec<Event> = self.platform.events.poll_iter().collect();
        for event in events {
            if EguiSdl::is_paste(&event)
                && let Ok(text) = self.platform.video.clipboard().clipboard_text()
            {
                self.egui.paste(text);
            }
            let egui_wants = self.egui.handle_event(&event);
            match event {
                Event::Quit { .. } | Event::Window { win_event: WindowEvent::CloseRequested, .. } => {
                    self.quit = true;
                }
                Event::KeyDown { keycode: Some(key), keymod, repeat: false, .. } => {
                    let alt = keymod.intersects(Mod::LALTMOD | Mod::RALTMOD);
                    match key {
                        Keycode::F3 => self.ui.show_debug = !self.ui.show_debug,
                        Keycode::F11 => {
                            self.ui.settings.window.fullscreen = !self.ui.settings.window.fullscreen
                        }
                        Keycode::Return if alt => {
                            self.ui.settings.window.fullscreen = !self.ui.settings.window.fullscreen;
                        }
                        Keycode::Escape if !egui_wants => {
                            if self.ui.show_settings {
                                self.ui.show_settings = false;
                            } else if self.ui.screen != Screen::Launcher {
                                self.handle_action(Action::BackToLauncher);
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::BackToLauncher => {
                self.ui.screen = Screen::Launcher;
                if let Some(mut v) = self.ui.viewer.take() {
                    v.close(&mut self.renderer);
                }
            }
            Action::Launcher(a) => match a {
                LauncherAction::StartGame(game) => {
                    let status = entry_status(self.ui.launcher.current(), game);
                    self.ui.launcher.notice = Some(format!("{}: {}", game.title(), status.describe()));
                }
                LauncherAction::OpenTestScene => {
                    self.ui.scene = SceneState::new();
                    self.ui.screen = Screen::TestScene;
                    self.ui.show_debug = true;
                }
                LauncherAction::OpenViewer => self.open_viewer(),
                LauncherAction::OpenSettings => self.ui.show_settings = true,
                LauncherAction::UseFolder(dir) => {
                    let install = Install::inspect(dir);
                    if install.is_usable() {
                        self.ui.settings.game.dir = Some(install.root.clone());
                        self.ui.launcher.notice = None;
                    } else {
                        self.ui.launcher.notice =
                            Some(format!("No game data found in {}", install.root.display()));
                    }
                    self.ui.launcher.set_install(install, InstallSource::Manual);
                }
                LauncherAction::Rescan => {
                    let found = crate::launcher::choose_install(
                        None,
                        self.ui.settings.game.dir.clone(),
                        aerugo_vfs::install::discover,
                    );
                    match found {
                        Some((install, source)) => {
                            self.ui.launcher.notice = None;
                            self.ui.launcher.set_install(install, source);
                        }
                        None => {
                            self.ui.launcher.notice = Some("No install found in any Steam library.".into())
                        }
                    }
                }
                LauncherAction::Quit => self.quit = true,
            },
        }
    }

    fn open_viewer(&mut self) {
        if !self.cjk_fonts {
            aerugo_debug_ui::fonts::add_cjk_fallbacks(&self.egui.ctx);
            self.cjk_fonts = true;
        }
        let lang = Language::from_code(&self.ui.settings.game.text_language).unwrap_or(Language::English);
        let japanese = self.ui.settings.game.voice == VoiceLanguage::Japanese;
        let original = self.ui.settings.game.music == MusicSet::Original;
        self.ui.viewer = Some(Viewer::open(
            self.ui.launcher.current(),
            lang,
            &self.platform.sdl,
            japanese,
            original,
            &self.ui.settings.ui.viewer_tab,
        ));
        self.ui.screen = Screen::Viewer;
    }

    /// Apply settings the user edited this frame.
    fn apply_settings(&mut self) {
        let new = self.ui.settings.clone();
        let old = &self.applied;
        if new.window.fullscreen != old.window.fullscreen
            && let Err(e) = self.platform.set_fullscreen(new.window.fullscreen)
        {
            log::warn!("{e:#}");
        }
        if new.graphics.present_mode != old.graphics.present_mode
            && let Some(mode) = PresentMode::from_name(&new.graphics.present_mode)
        {
            self.renderer.set_present_mode(mode);
        }
        if new.graphics.frame_limit != old.graphics.frame_limit {
            self.limiter.set_limit(Some(new.graphics.frame_limit));
        }
        if new.ui.scale != old.ui.scale {
            self.egui.ui_scale = new.ui.scale;
        }
        self.applied = new;
        if self.last_save.elapsed() > Duration::from_secs(1) {
            self.save_settings();
        }
    }

    fn remember_window_size(&mut self) {
        if !self.ui.settings.window.fullscreen {
            let (w, h) = self.platform.window.size();
            self.ui.settings.window.width = w;
            self.ui.settings.window.height = h;
        }
    }

    fn save_settings(&mut self) {
        self.last_save = Instant::now();
        let to_save = without_untouched_overrides(&self.ui.settings, &self.launch, &self.launch_file);
        if to_save == self.saved {
            return;
        }
        match to_save.save(&self.settings_path) {
            Ok(()) => self.saved = to_save,
            Err(e) => log::warn!("saving settings: {e:#}"),
        }
    }
}

/// Settings to write: command-line overrides the user did not change in the UI keep their file value.
fn without_untouched_overrides(current: &Settings, launch: &Settings, file: &Settings) -> Settings {
    fn keep<T: Clone + PartialEq>(cur: &T, launch: &T, file: &T) -> T {
        if cur == launch { file.clone() } else { cur.clone() }
    }
    let mut out = current.clone();
    out.window.fullscreen =
        keep(&current.window.fullscreen, &launch.window.fullscreen, &file.window.fullscreen);
    out.graphics = crate::settings::GraphicsSettings {
        present_mode: keep(
            &current.graphics.present_mode,
            &launch.graphics.present_mode,
            &file.graphics.present_mode,
        ),
        frame_limit: keep(
            &current.graphics.frame_limit,
            &launch.graphics.frame_limit,
            &file.graphics.frame_limit,
        ),
        gpu: keep(&current.graphics.gpu, &launch.graphics.gpu, &file.graphics.gpu),
        validation: keep(
            &current.graphics.validation,
            &launch.graphics.validation,
            &file.graphics.validation,
        ),
    };
    out.game.dir = keep(&current.game.dir, &launch.game.dir, &file.game.dir);
    out
}

fn draw_ui(
    ui: &mut egui::Ui,
    state: &mut UiState,
    renderer: &mut Renderer,
    info: &RenderInfo,
    actions: &mut Vec<Action>,
) {
    let ctx = ui.ctx().clone();
    match state.screen {
        Screen::Launcher => {
            if let Some(a) = state.launcher.ui(ui) {
                actions.push(Action::Launcher(a));
            }
        }
        Screen::Viewer => {
            if let Some(viewer) = &mut state.viewer {
                if viewer.ui(ui, renderer) == Some(ViewerAction::Back) {
                    actions.push(Action::BackToLauncher);
                }
                if state.settings.ui.viewer_tab != viewer.tab_name() {
                    state.settings.ui.viewer_tab = viewer.tab_name().to_owned();
                }
            }
        }
        Screen::TestScene => {
            egui::Panel::top("scene-bar").show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("< Launcher").clicked() {
                        actions.push(Action::BackToLauncher);
                    }
                    ui.label(
                        "Timing test scene: 30 Hz simulation, drawn at display rate. F3 toggles the panel.",
                    );
                });
            });
        }
    }

    if state.show_debug {
        let mut open = true;
        egui::Window::new("Debug").open(&mut open).default_pos([16.0, 64.0]).default_width(340.0).show(
            &ctx,
            |ui| {
                let avg = state.graph.average_ms();
                let fps = if avg > 0.0 { 1000.0 / avg } else { 0.0 };
                ui.monospace(format!("{fps:6.1} fps  {avg:5.2} ms  max {:5.2} ms", state.graph.max_ms()));
                state.graph.ui(ui);
                ui.separator();
                present_mode_combo(ui, &mut state.settings, info);
                frame_limit_combo(ui, &mut state.settings);
                if state.screen == Screen::TestScene {
                    ui.separator();
                    state.scene.controls_ui(ui);
                }
                ui.separator();
                egui::CollapsingHeader::new("Renderer").default_open(false).show(ui, |ui| {
                    ui.label(format!("GPU: {}", info.device));
                    ui.label(format!("Swapchain: {}×{} {}", info.extent.0, info.extent.1, info.format));
                    ui.label(format!("Present mode: {}", info.present_mode.label()));
                });
            },
        );
        state.show_debug = open;
    }

    if state.show_settings {
        let mut open = true;
        egui::Window::new("Settings").open(&mut open).collapsible(false).default_width(360.0).show(
            &ctx,
            |ui| {
                settings_ui(ui, &mut state.settings, info);
            },
        );
        state.show_settings = open;
    }
}

fn present_mode_combo(ui: &mut egui::Ui, settings: &mut Settings, info: &RenderInfo) {
    let current = PresentMode::from_name(&settings.graphics.present_mode).unwrap_or(PresentMode::Fifo);
    egui::ComboBox::from_label("Presentation").selected_text(current.label()).show_ui(ui, |ui| {
        for mode in PresentMode::ALL {
            let available = info.available.contains(&mode);
            let mut text = mode.label().to_owned();
            if !available {
                text.push_str(" (unsupported)");
            }
            let response = ui.add_enabled(available, egui::Button::selectable(mode == current, text));
            if response.clicked() {
                settings.graphics.present_mode = mode.name().into();
            }
        }
    });
}

fn frame_limit_combo(ui: &mut egui::Ui, settings: &mut Settings) {
    let label = |hz: u32| if hz == 0 { "Uncapped".to_owned() } else { format!("{hz} Hz") };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("frame-limit")
            .selected_text(label(settings.graphics.frame_limit))
            .show_ui(ui, |ui| {
                for hz in FRAME_LIMIT_PRESETS {
                    ui.selectable_value(&mut settings.graphics.frame_limit, hz, label(hz));
                }
            });
        ui.add(egui::DragValue::new(&mut settings.graphics.frame_limit).range(0..=1000).suffix(" Hz"));
        ui.label("Frame limit");
    });
}

fn settings_ui(ui: &mut egui::Ui, settings: &mut Settings, info: &RenderInfo) {
    ui.heading("Display");
    ui.checkbox(&mut settings.window.fullscreen, "Fullscreen (F11)");
    present_mode_combo(ui, settings, info);
    frame_limit_combo(ui, settings);
    ui.add(egui::Slider::new(&mut settings.ui.scale, 0.5..=3.0).text("Interface scale"));

    ui.separator();
    ui.heading("Language");
    ui.horizontal(|ui| {
        ui.label("Voices:");
        for v in [VoiceLanguage::English, VoiceLanguage::Japanese] {
            ui.radio_value(&mut settings.game.voice, v, v.label());
        }
    });
    ui.horizontal(|ui| {
        ui.label("FFX music:");
        for m in [MusicSet::Arranged, MusicSet::Original] {
            ui.radio_value(&mut settings.game.music, m, m.label());
        }
    });
    let current = Language::from_code(&settings.game.text_language).map_or("?", Language::name);
    egui::ComboBox::from_label("Text").selected_text(current).show_ui(ui, |ui| {
        for lang in Language::ALL {
            ui.selectable_value(&mut settings.game.text_language, lang.code().to_owned(), lang.name());
        }
    });

    ui.separator();
    ui.heading("Advanced (restart required)");
    ui.horizontal(|ui| {
        ui.label("Preferred GPU:");
        ui.add(egui::TextEdit::singleline(&mut settings.graphics.gpu).hint_text(info.device.as_str()));
    });
    ui.checkbox(&mut settings.graphics.validation, "Vulkan validation layer");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_are_not_persisted_unless_changed() {
        let file = Settings::default();
        let mut launch = file.clone();
        launch.graphics.frame_limit = 144; // from --fps-limit
        launch.window.fullscreen = true; // from --fullscreen
        let mut current = launch.clone();
        current.ui.scale = 1.5; // edited in the UI
        current.window.fullscreen = false; // toggled back in the UI
        let saved = without_untouched_overrides(&current, &launch, &file);
        assert_eq!(saved.graphics.frame_limit, 0);
        assert!(!saved.window.fullscreen);
        assert_eq!(saved.ui.scale, 1.5);
        current.graphics.frame_limit = 240;
        assert_eq!(without_untouched_overrides(&current, &launch, &file).graphics.frame_limit, 240);
    }

    #[test]
    fn toggling_back_restores_the_launch_value() {
        // No overrides: fullscreen on, saved, then off again must save off.
        let file = Settings::default();
        let launch = file.clone();
        let mut current = launch.clone();
        current.window.fullscreen = true;
        assert!(without_untouched_overrides(&current, &launch, &file).window.fullscreen);
        current.window.fullscreen = false;
        assert!(!without_untouched_overrides(&current, &launch, &file).window.fullscreen);
    }
}
