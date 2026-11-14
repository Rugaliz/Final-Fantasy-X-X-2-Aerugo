//! Sound browser: every FSB5 bank of both games, its samples, playback through the mixer and a waveform.

use std::time::Instant;

use aerugo_audio::{AudioOutput, Sound, VoiceId, VoiceParams};
use aerugo_debug_ui::egui;
use aerugo_formats::fev::{Project, bank_prefix, variant_path};
use aerugo_formats::fsb::{Bank, Sample};
use aerugo_vfs::Vfs;

use super::filelist::FileList;

#[derive(Debug)]
struct OpenBank {
    path: String,
    bank: Result<Bank, String>,
}

#[derive(Debug)]
struct Playing {
    sample: usize,
    voice: VoiceId,
    started: Instant,
    sound: Sound,
    /// Min/max per column for the waveform.
    envelope: Vec<(f32, f32)>,
}

/// An event project resolved to the chosen voice language and music set, with its banks loaded.
#[derive(Debug)]
struct OpenProject {
    resolved: String,
    project: Result<Project, String>,
    /// (path, bank header) — sample data is read when played.
    banks: Vec<(String, Bank)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Banks,
    Events,
}

#[derive(Debug)]
pub struct SoundBrowser {
    mode: Mode,
    projects: Option<FileList>,
    project: Option<OpenProject>,
    japanese_voices: bool,
    original_music: bool,
    event_status: String,
    files: Option<FileList>,
    bank: Option<OpenBank>,
    playing: Option<Playing>,
    looping: bool,
    volume: f32,
    filter: String,
}

impl SoundBrowser {
    pub fn new() -> SoundBrowser {
        SoundBrowser {
            mode: Mode::Events,
            projects: None,
            project: None,
            japanese_voices: false,
            original_music: false,
            event_status: String::new(),
            files: None,
            bank: None,
            playing: None,
            looping: false,
            volume: 0.8,
            filter: String::new(),
        }
    }

    pub fn close(&mut self, audio: Option<&AudioOutput>) {
        if let (Some(p), Some(a)) = (self.playing.take(), audio) {
            a.stop(p.voice);
        }
    }

    fn play(&mut self, vfs: &Vfs, audio: &AudioOutput, index: usize) {
        self.close(Some(audio));
        let Some(OpenBank { path, bank: Ok(bank), .. }) = &self.bank else { return };
        let Some(s) = bank.samples.get(index) else { return };
        let Ok(pcm) = read_sample(vfs, path, bank, s) else { return };
        let sound = Sound::from_i16(&pcm, s.channels, s.rate, s.looping);
        let voice = audio.play(
            sound.clone(),
            VoiceParams { gain: self.volume, looping: self.looping, ..VoiceParams::default() },
        );
        let envelope = envelope(&sound, 800);
        self.playing = Some(Playing { sample: index, voice, started: Instant::now(), sound, envelope });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Vfs, audio: Option<&AudioOutput>) {
        egui::Panel::top("sound-mode").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Events, "Events");
                ui.selectable_value(&mut self.mode, Mode::Banks, "Banks");
                ui.separator();
                let mut changed = ui.checkbox(&mut self.japanese_voices, "Japanese voices").changed();
                changed |= ui.checkbox(&mut self.original_music, "Original FFX music").changed();
                if changed && let Some(p) = &self.project {
                    let canonical = canonical(&p.resolved);
                    self.open_project(vfs, &canonical);
                }
            });
        });
        if self.mode == Mode::Events {
            self.events_ui(ui, vfs, audio);
            return;
        }
        let files = self.files.get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".fsb")));
        let mut picked = None;
        egui::Panel::left("sound-banks").resizable(true).default_size(340.0).show(ui, |ui| {
            picked = files.ui(ui);
        });
        if let Some(path) = picked {
            self.close(audio);
            let bank = load_bank(vfs, &path);
            self.bank = Some(OpenBank { path, bank });
        }
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let Some(audio) = audio else {
                ui.colored_label(ui.visuals().warn_fg_color, "No audio output could be opened.");
                return;
            };
            ui.horizontal(|ui| {
                if ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).text("volume")).changed()
                    && let Some(p) = &self.playing
                {
                    audio.set_params(
                        p.voice,
                        VoiceParams { gain: self.volume, looping: self.looping, ..VoiceParams::default() },
                    );
                }
                ui.checkbox(&mut self.looping, "Loop (sample loop points)");
                if ui.button("Stop").clicked() {
                    self.close(Some(audio));
                }
                let peak = audio.peak();
                ui.add(
                    egui::ProgressBar::new(peak.min(1.0))
                        .desired_width(120.0)
                        .text(format!("peak {:.2}", peak)),
                );
            });
            if let Some(p) = &self.playing {
                self.waveform(ui, p);
            }
            ui.separator();
            let Some(open) = &self.bank else {
                ui.label("Pick a sound bank on the left.");
                return;
            };
            ui.monospace(&open.path);
            let bank = match &open.bank {
                Ok(b) => b,
                Err(e) => {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                    return;
                }
            };
            ui.horizontal(|ui| {
                ui.label(format!("{} samples, codec {:?}", bank.samples.len(), bank.codec));
                ui.label("Filter");
                ui.text_edit_singleline(&mut self.filter);
            });
            let filter = self.filter.to_lowercase();
            let rows: Vec<(usize, &Sample)> = bank
                .samples
                .iter()
                .enumerate()
                .filter(|(_, s)| filter.is_empty() || s.name.to_lowercase().contains(&filter))
                .collect();
            let mut play = None;
            let row_h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
                ui,
                row_h,
                rows.len(),
                |ui, range| {
                    for &(i, s) in &rows[range] {
                        ui.horizontal(|ui| {
                            let current = self.playing.as_ref().is_some_and(|p| p.sample == i);
                            if ui.selectable_label(current, "▶").clicked() {
                                play = Some(i);
                            }
                            let secs = s.frames as f32 / s.rate as f32;
                            let lp = s.looping.map(|(a, b)| format!(" · loop {a}–{b}")).unwrap_or_default();
                            ui.monospace(format!(
                                "{i:4}  {:<24} {:>5} Hz {}ch {:7.2} s{lp}",
                                s.name, s.rate, s.channels, secs
                            ));
                        });
                    }
                },
            );
            if let Some(i) = play {
                self.play(vfs, audio, i);
            }
        });
    }

    fn waveform(&self, ui: &mut egui::Ui, p: &Playing) {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 90.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
        let mid = rect.center().y;
        let h = rect.height() * 0.45;
        let n = p.envelope.len().max(1) as f32;
        for (i, &(lo, hi)) in p.envelope.iter().enumerate() {
            let x = rect.left() + rect.width() * i as f32 / n;
            painter.line_segment(
                [egui::pos2(x, mid - hi * h), egui::pos2(x, mid - lo * h)],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 170, 230)),
            );
        }
        // Playhead from wall time (loops wrap).
        let secs = p.sound.frames() as f64 / f64::from(p.sound.rate);
        let mut t = p.started.elapsed().as_secs_f64();
        if self.looping && secs > 0.0 {
            t %= secs;
        }
        if t <= secs {
            let x = rect.left() + rect.width() * (t / secs) as f32;
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(1.5, egui::Color32::WHITE),
            );
        }
        ui.ctx().request_repaint();
    }
}

fn envelope(sound: &Sound, columns: usize) -> Vec<(f32, f32)> {
    let ch = sound.channels as usize;
    let frames = sound.frames();
    let per = frames.div_ceil(columns.max(1)).max(1);
    (0..frames.div_ceil(per))
        .map(|c| {
            let start = c * per;
            let end = ((c + 1) * per).min(frames);
            let mut lo = 0.0f32;
            let mut hi = 0.0f32;
            for f in start..end {
                let v = sound.samples[f * ch];
                lo = lo.min(v);
                hi = hi.max(v);
            }
            (lo, hi)
        })
        .collect()
}

/// The project's English / remastered path, used to list each project once.
fn canonical(path: &str) -> String {
    variant_path(path, false, false)
}

impl SoundBrowser {
    pub fn with_languages(japanese_voices: bool, original_music: bool) -> SoundBrowser {
        SoundBrowser { japanese_voices, original_music, ..SoundBrowser::new() }
    }

    fn open_project(&mut self, vfs: &Vfs, canonical_path: &str) {
        let resolved = variant_path(canonical_path, self.japanese_voices, self.original_music);
        let resolved = if vfs.exists(&resolved) { resolved } else { canonical_path.to_owned() };
        let project = vfs
            .read(&resolved)
            .map_err(|e| e.to_string())
            .and_then(|d| Project::parse(&d).map_err(|e| e.to_string()));
        let prefix = bank_prefix(&resolved);
        let mut bank_paths: Vec<String> = vfs
            .archives()
            .flat_map(|(_, a)| {
                a.entries()
                    .iter()
                    .filter(|e| e.path.starts_with(&prefix) && e.path.ends_with(".fsb"))
                    .map(|e| e.path.clone())
            })
            .collect();
        bank_paths.sort();
        let banks = bank_paths.iter().filter_map(|p| Some((p.clone(), load_bank(vfs, p).ok()?))).collect();
        self.project = Some(OpenProject { resolved, project, banks });
    }

    /// Play an event by name: the sample of the same name in the project's banks.
    fn play_event(&mut self, vfs: &Vfs, audio: &AudioOutput, event: &str) {
        self.close(Some(audio));
        let Some(p) = &self.project else { return };
        let found = p
            .banks
            .iter()
            .find_map(|(path, b)| b.samples.iter().find(|s| s.name == event).map(|s| (path, b, s)));
        let Some((path, bank, sample)) = found else {
            self.event_status = format!("{event}: no sample");
            return;
        };
        let pcm = match read_sample(vfs, path, bank, sample) {
            Ok(p) => p,
            Err(e) => {
                self.event_status = e;
                return;
            }
        };
        // Music loops on its loop points; voices and one-shots play once unless looping is ticked.
        let looping = self.looping || (p.resolved.contains("/music") && !event.ends_with("oneshot"));
        let sound = Sound::from_i16(&pcm, sample.channels, sample.rate, sample.looping);
        let voice =
            audio.play(sound.clone(), VoiceParams { gain: self.volume, looping, ..VoiceParams::default() });
        self.event_status = format!(
            "{}/{event}: {} Hz, {} ch, {:.1} s{}",
            p.project.as_ref().map(|x| x.name.as_str()).unwrap_or("?"),
            sample.rate,
            sample.channels,
            sample.frames as f32 / sample.rate as f32,
            if looping { ", looping" } else { "" }
        );
        let envelope = envelope(&sound, 800);
        self.playing = Some(Playing { sample: usize::MAX, voice, started: Instant::now(), sound, envelope });
    }

    fn events_ui(&mut self, ui: &mut egui::Ui, vfs: &Vfs, audio: Option<&AudioOutput>) {
        let projects = self
            .projects
            .get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".fev") && canonical(p) == p));
        let mut picked = None;
        egui::Panel::left("sound-projects").resizable(true).default_size(340.0).show(ui, |ui| {
            picked = projects.ui(ui);
        });
        if let Some(path) = picked {
            self.close(audio);
            self.open_project(vfs, &path);
        }
        egui::CentralPanel::default_margins().show(ui, |ui| {
            let Some(audio) = audio else {
                ui.colored_label(ui.visuals().warn_fg_color, "No audio output could be opened.");
                return;
            };
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).text("volume"));
                ui.checkbox(&mut self.looping, "Loop");
                if ui.button("Stop").clicked() {
                    self.close(Some(audio));
                }
                let peak = audio.peak();
                ui.add(
                    egui::ProgressBar::new(peak.min(1.0))
                        .desired_width(120.0)
                        .text(format!("peak {peak:.2}")),
                );
            });
            ui.label(&self.event_status);
            if let Some(p) = &self.playing {
                self.waveform(ui, p);
            }
            ui.separator();
            let Some(open) = &self.project else {
                ui.label("Pick an event project on the left (sound effects, voices, music).");
                return;
            };
            ui.monospace(&open.resolved);
            let project = match &open.project {
                Ok(p) => p,
                Err(e) => {
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                    return;
                }
            };
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} events · groups {} · {} banks",
                    project.events.len(),
                    project.groups.join(", "),
                    open.banks.len()
                ));
                ui.label("Filter");
                ui.text_edit_singleline(&mut self.filter);
            });
            let filter = self.filter.to_lowercase();
            let events: Vec<String> = project
                .events
                .iter()
                .filter(|e| filter.is_empty() || e.to_lowercase().contains(&filter))
                .cloned()
                .collect();
            let mut play = None;
            let row_h = ui.spacing().interact_size.y;
            egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
                ui,
                row_h,
                events.len(),
                |ui, range| {
                    for e in &events[range] {
                        ui.horizontal(|ui| {
                            if ui.button("▶").clicked() {
                                play = Some(e.clone());
                            }
                            ui.monospace(e);
                        });
                    }
                },
            );
            if let Some(e) = play {
                self.play_event(vfs, audio, &e);
            }
        });
    }
}

/// A bank's header only (sample data stays in the archive until played).
fn load_bank(vfs: &Vfs, path: &str) -> Result<Bank, String> {
    let size = vfs.size(path).ok_or("missing")? as usize;
    let first = vfs.read_range(path, 0, 0x40).map_err(|e| e.to_string())?;
    let len = Bank::header_len(&first).map_err(|e| e.to_string())?;
    let head = vfs.read_range(path, 0, len as u64).map_err(|e| e.to_string())?;
    Bank::parse_header(&head, size).map_err(|e| e.to_string())
}

/// Read and decode one sample with a ranged read.
fn read_sample(vfs: &Vfs, path: &str, bank: &Bank, s: &Sample) -> Result<Vec<i16>, String> {
    let data = vfs.read_range(path, s.data.start as u64, s.data.len() as u64).map_err(|e| e.to_string())?;
    bank.decode_data(s, &data).map_err(|e| e.to_string())
}
