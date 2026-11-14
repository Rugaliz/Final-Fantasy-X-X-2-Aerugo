//! Sound browser: every FSB5 bank of both games, its samples, playback through the mixer and a waveform.

use std::time::Instant;

use aerugo_audio::{AudioOutput, Sound, VoiceId, VoiceParams};
use aerugo_debug_ui::egui;
use aerugo_formats::fsb::{Bank, Sample};
use aerugo_vfs::Vfs;

use super::filelist::FileList;

#[derive(Debug)]
struct OpenBank {
    path: String,
    data: Vec<u8>,
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

#[derive(Debug)]
pub struct SoundBrowser {
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

    fn play(&mut self, audio: &AudioOutput, index: usize) {
        self.close(Some(audio));
        let Some(OpenBank { data, bank: Ok(bank), .. }) = &self.bank else { return };
        let Some(s) = bank.samples.get(index) else { return };
        let Ok(pcm) = bank.decode(data, s) else { return };
        let sound = Sound::from_i16(&pcm, s.channels, s.rate, s.looping);
        let voice = audio.play(
            sound.clone(),
            VoiceParams { gain: self.volume, looping: self.looping, ..VoiceParams::default() },
        );
        let envelope = envelope(&sound, 800);
        self.playing = Some(Playing { sample: index, voice, started: Instant::now(), sound, envelope });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, vfs: &Vfs, audio: Option<&AudioOutput>) {
        let files = self.files.get_or_insert_with(|| FileList::new(vfs, |p| p.ends_with(".fsb")));
        let mut picked = None;
        egui::Panel::left("sound-banks").resizable(true).default_size(340.0).show(ui, |ui| {
            picked = files.ui(ui);
        });
        if let Some(path) = picked {
            self.close(audio);
            let data = vfs.read(&path).map_err(|e| e.to_string());
            let (data, bank) = match data {
                Ok(d) => {
                    let b = Bank::parse(&d).map_err(|e| e.to_string());
                    (d, b)
                }
                Err(e) => (Vec::new(), Err(e)),
            };
            self.bank = Some(OpenBank { path, data, bank });
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
                self.play(audio, i);
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
