//! Movie player: every `*.webm` of both games, VP8 pictures shown by presentation time, Vorbis audio
//! streamed to the mixer (which also serves as the clock), fit or crop to the view.

use std::sync::Arc;
use std::time::Instant;

use aerugo_audio::{AudioOutput, SharedStream, VoiceId};
use aerugo_debug_ui::egui;
use aerugo_gfx::aspect::{AspectMode, clip_transform};
use aerugo_gfx::{Renderer, TexFormat, TextureDesc, TextureHandle};
use aerugo_vfs::Vfs;
use aerugo_video::{Chunked, Movie};

use super::filelist::FileList;

type VfsSource = Chunked<Box<dyn FnMut(u64, usize) -> Result<Vec<u8>, String>>>;

struct Playing {
    path: String,
    movie: Movie<VfsSource>,
    texture: TextureHandle,
    stream: Option<(SharedStream, VoiceId)>,
    started: Instant,
    paused_at: Option<f64>,
    rgba: Vec<u8>,
    shown: usize,
}

pub struct MoviePlayer {
    files: Option<FileList>,
    playing: Option<Playing>,
    error: Option<String>,
    mode: AspectMode,
    volume: f32,
}

impl std::fmt::Debug for MoviePlayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MoviePlayer").finish_non_exhaustive()
    }
}

impl MoviePlayer {
    pub fn new() -> MoviePlayer {
        MoviePlayer { files: None, playing: None, error: None, mode: AspectMode::Fit, volume: 0.8 }
    }

    pub fn close(&mut self, renderer: &mut Renderer, audio: Option<&AudioOutput>) {
        if let Some(p) = self.playing.take() {
            if let (Some((_, id)), Some(a)) = (&p.stream, audio) {
                a.stop(*id);
            }
            renderer.destroy_texture(p.texture);
        }
    }

    fn open(&mut self, vfs: &Arc<Vfs>, renderer: &mut Renderer, audio: Option<&AudioOutput>, path: String) {
        self.close(renderer, audio);
        self.error = None;
        let result = (|| -> Result<Playing, String> {
            let len = vfs.size(&path).ok_or("missing")?;
            let (v, p) = (vfs.clone(), path.clone());
            let read: Box<dyn FnMut(u64, usize) -> Result<Vec<u8>, String>> =
                Box::new(move |at, n| v.read_range(&p, at, n as u64).map_err(|e| e.to_string()));
            let movie = Movie::open(Chunked::new(len, 4 << 20, read)).map_err(|e| e.to_string())?;
            let black = vec![0u8; movie.width as usize * movie.height as usize * 4];
            let texture = renderer
                .create_texture(&TextureDesc {
                    format: TexFormat::Rgba8,
                    width: movie.width,
                    height: movie.height,
                    levels: 1,
                    layers: 1,
                    cube: false,
                    data: vec![&black],
                    name: &path,
                })
                .map_err(|e| format!("{e:#}"))?;
            let stream = match (movie.has_audio(), audio) {
                (true, Some(a)) => {
                    let s: SharedStream = Default::default();
                    let id = a.play_stream(s.clone(), movie.audio_channels, movie.audio_rate, self.volume);
                    Some((s, id))
                }
                _ => None,
            };
            Ok(Playing {
                path,
                movie,
                texture,
                stream,
                started: Instant::now(),
                paused_at: None,
                rgba: Vec::new(),
                shown: 0,
            })
        })();
        match result {
            Ok(p) => self.playing = Some(p),
            Err(e) => self.error = Some(e),
        }
    }

    /// Playback position in seconds: the audio clock when there is audio, else wall time.
    fn clock(p: &Playing) -> f64 {
        if let Some(t) = p.paused_at {
            return t;
        }
        match &p.stream {
            Some((s, _)) if p.movie.audio_rate > 0 => {
                s.lock().map_or(0.0, |s| s.played_frames as f64 / f64::from(p.movie.audio_rate))
            }
            _ => p.started.elapsed().as_secs_f64(),
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        vfs: &Arc<Vfs>,
        renderer: &mut Renderer,
        audio: Option<&AudioOutput>,
    ) {
        let files = self.files.get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".webm")));
        let mut picked = None;
        egui::Panel::left("movie-files").resizable(true).default_size(300.0).show(ui, |ui| {
            picked = files.ui(ui);
        });
        if let Some(path) = picked {
            self.open(vfs, renderer, audio, path);
        }
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.mode, AspectMode::Fit, "Fit");
                ui.radio_value(&mut self.mode, AspectMode::Crop, "Crop");
                ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).text("volume"));
                if ui.button("Stop").clicked() {
                    self.close(renderer, audio);
                }
            });
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            let Some(p) = &mut self.playing else {
                ui.label(
                    "Pick a movie on the left. Story movies have Vorbis audio; libvpx decodes the pictures.",
                );
                return;
            };
            let t = Self::clock(p);
            let t_ns = (t * 1e9) as i64;
            // Decode ahead so audio never runs dry.
            if let Err(e) = p.movie.fill(t_ns + 300_000_000) {
                self.error = Some(e.to_string());
            }
            let pcm = p.movie.take_audio();
            if let Some((s, _)) = &p.stream
                && let Ok(mut s) = s.lock()
            {
                s.samples.extend(pcm);
                s.finished = p.movie.ended();
            }
            if let Some(pic) = p.movie.picture_at(t_ns) {
                pic.to_rgba(&mut p.rgba);
                if let Err(e) = renderer.update_texture(p.texture, &p.rgba) {
                    self.error = Some(format!("{e:#}"));
                }
                p.shown += 1;
            }
            ui.label(format!(
                "{} · {}×{} · {:.1} / {:.1} s · {} pictures shown{}",
                p.path,
                p.movie.width,
                p.movie.height,
                t,
                p.movie.duration_s,
                p.shown,
                if p.movie.has_audio() {
                    format!(" · audio {} Hz", p.movie.audio_rate)
                } else {
                    String::new()
                }
            ));
            let id = match renderer.ui_texture(p.texture, 0, 0, false) {
                Ok(id) => id,
                Err(e) => {
                    ui.label(format!("{e:#}"));
                    return;
                }
            };
            let avail = ui.available_size();
            let (rect, _) = ui.allocate_exact_size(avail, egui::Sense::hover());
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            // Fit or crop the movie frame to the panel with the shared aspect policy.
            let design = p.movie.width as f32 / p.movie.height.max(1) as f32;
            let view = rect.width() / rect.height().max(1.0);
            let c = clip_transform(design, view, self.mode, [0.0, 0.0]);
            let [x0, y0, x1, y1] = c.visible_frame();
            let size =
                egui::vec2(rect.width() * (x1 - x0) / 2.0 * c.sx, rect.height() * (y1 - y0) / 2.0 * c.sy);
            let shown = egui::Rect::from_center_size(rect.center(), size);
            let uv = egui::Rect::from_min_max(
                egui::pos2((x0 + 1.0) / 2.0, (y0 + 1.0) / 2.0),
                egui::pos2((x1 + 1.0) / 2.0, (y1 + 1.0) / 2.0),
            );
            ui.painter().image(id, shown, uv, egui::Color32::WHITE);
            ui.ctx().request_repaint();
        });
    }
}
