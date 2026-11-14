//! SDL3 playback: a float stereo stream whose callback runs the mixer. The rest of the program sends
//! commands; the mixer's state (positions, peak) comes back through a shared snapshot.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use aerugo_platform::sdl3::Sdl;
use aerugo_platform::sdl3::audio::{
    AudioCallback, AudioFormat, AudioSpec, AudioStream, AudioStreamWithCallback,
};
use anyhow::{Result, anyhow};

use crate::mixer::{Mixer, Sound, VoiceId, VoiceParams};

/// Output rate the mixer runs at; SDL converts to the device.
pub const RATE: u32 = 48_000;

enum Command {
    Play(VoiceId, Sound, VoiceParams),
    Params(VoiceId, VoiceParams),
    Stop(VoiceId),
    StopAll,
    Master(f32),
}

/// What the audio thread reports back.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub voices: Vec<(VoiceId, f64)>,
    pub peak: f32,
}

struct Callback {
    mixer: Mixer,
    commands: Receiver<Command>,
    status: Arc<Mutex<Status>>,
    buffer: Vec<f32>,
}

impl AudioCallback<f32> for Callback {
    fn callback(&mut self, stream: &mut AudioStream, requested: i32) {
        while let Ok(c) = self.commands.try_recv() {
            match c {
                Command::Play(id, s, p) => self.mixer.play(id, s, p),
                Command::Params(id, p) => self.mixer.set_params(id, p),
                Command::Stop(id) => self.mixer.stop(id),
                Command::StopAll => self.mixer.stop_all(),
                Command::Master(g) => self.mixer.master = g,
            }
        }
        // `requested` counts samples of both channels; mix whole frames.
        let frames = (requested.max(0) as usize).div_ceil(2);
        self.buffer.resize(frames * 2, 0.0);
        self.mixer.render(&mut self.buffer);
        let _ = stream.put_data_f32(&self.buffer);
        if let Ok(mut s) = self.status.try_lock() {
            s.peak = self.mixer.last_peak;
            s.voices.clear();
        }
    }
}

/// An open playback device.
pub struct AudioOutput {
    _stream: AudioStreamWithCallback<Callback>,
    commands: Sender<Command>,
    next: AtomicU64,
    status: Arc<Mutex<Status>>,
}

impl std::fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOutput").finish_non_exhaustive()
    }
}

impl AudioOutput {
    pub fn open(sdl: &Sdl) -> Result<AudioOutput> {
        let audio = sdl.audio().map_err(|e| anyhow!("SDL audio: {e}"))?;
        let spec =
            AudioSpec { freq: Some(RATE as i32), channels: Some(2), format: Some(AudioFormat::f32_sys()) };
        let (tx, rx) = channel();
        let status = Arc::new(Mutex::new(Status::default()));
        let callback =
            Callback { mixer: Mixer::new(RATE), commands: rx, status: status.clone(), buffer: Vec::new() };
        let stream =
            audio.open_playback_stream(&spec, callback).map_err(|e| anyhow!("opening audio output: {e}"))?;
        stream.resume().map_err(|e| anyhow!("starting audio output: {e}"))?;
        log::info!("audio output open: {RATE} Hz stereo float");
        Ok(AudioOutput { _stream: stream, commands: tx, next: AtomicU64::new(1), status })
    }

    pub fn play(&self, sound: Sound, params: VoiceParams) -> VoiceId {
        let id = VoiceId(self.next.fetch_add(1, Ordering::Relaxed));
        let _ = self.commands.send(Command::Play(id, sound, params));
        id
    }

    pub fn set_params(&self, id: VoiceId, params: VoiceParams) {
        let _ = self.commands.send(Command::Params(id, params));
    }

    pub fn stop(&self, id: VoiceId) {
        let _ = self.commands.send(Command::Stop(id));
    }

    pub fn stop_all(&self) {
        let _ = self.commands.send(Command::StopAll);
    }

    pub fn set_master(&self, gain: f32) {
        let _ = self.commands.send(Command::Master(gain));
    }

    /// Peak level of the last mixed block (before the limiter).
    pub fn peak(&self) -> f32 {
        self.status.lock().map(|s| s.peak).unwrap_or(0.0)
    }
}
