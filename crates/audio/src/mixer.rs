//! The mixer: voices playing sounds at any rate, mixed in 32-bit float to stereo at the device rate, then
//! through a peak limiter.

use std::sync::Arc;

use crate::resample::SincTable;

/// Decoded audio: interleaved samples in −1…1.
#[derive(Debug, Clone, PartialEq)]
pub struct Sound {
    pub samples: Arc<[f32]>,
    pub channels: u16,
    pub rate: u32,
    /// Loop start and end (inclusive), in frames.
    pub looping: Option<(u32, u32)>,
}

impl Sound {
    pub fn from_i16(samples: &[i16], channels: u16, rate: u32, looping: Option<(u32, u32)>) -> Sound {
        let samples: Arc<[f32]> = samples.iter().map(|&s| f32::from(s) / 32768.0).collect();
        Sound { samples, channels: channels.max(1), rate: rate.max(1), looping }
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VoiceParams {
    /// Linear gain.
    pub gain: f32,
    /// −1 left … 1 right (constant-power).
    pub pan: f32,
    /// Playback speed (1 = original pitch).
    pub pitch: f32,
    /// Follow the sound's loop points.
    pub looping: bool,
}

impl Default for VoiceParams {
    fn default() -> Self {
        VoiceParams { gain: 1.0, pan: 0.0, pitch: 1.0, looping: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VoiceId(pub u64);

/// Audio fed while it plays (movies, streamed music): interleaved samples appended by the producer.
#[derive(Debug, Default)]
pub struct Stream {
    pub samples: std::collections::VecDeque<f32>,
    /// No more samples will come; the voice ends when the queue runs dry.
    pub finished: bool,
    /// Frames the mixer has consumed so far (a clock for synchronising pictures).
    pub played_frames: u64,
}

pub type SharedStream = Arc<std::sync::Mutex<Stream>>;

#[derive(Debug)]
struct StreamVoice {
    id: VoiceId,
    shared: SharedStream,
    channels: usize,
    rate: u32,
    gain: f32,
    /// Local window of interleaved samples; `base` is the frame index of its first frame.
    buf: Vec<f32>,
    base: i64,
    pos: f64,
}

#[derive(Debug)]
struct Voice {
    id: VoiceId,
    sound: Sound,
    params: VoiceParams,
    /// Position in source frames.
    pos: f64,
    /// Kernel for this voice's rate ratio (shared table unless it downsamples).
    table: Option<Arc<SincTable>>,
}

/// Peak limiter: instant attack, smooth release, stereo-linked.
#[derive(Debug, Clone)]
pub struct Limiter {
    pub threshold: f32,
    gain: f32,
    release: f32,
}

impl Limiter {
    pub fn new(rate: u32) -> Limiter {
        // About 80 ms to recover.
        Limiter { threshold: 0.98, gain: 1.0, release: 1.0 - (-1.0 / (0.08 * rate as f32)).exp() }
    }

    pub fn process(&mut self, frame: &mut [f32; 2]) {
        let peak = frame[0].abs().max(frame[1].abs());
        let target = if peak > self.threshold { self.threshold / peak } else { 1.0 };
        if target < self.gain {
            self.gain = target;
        } else {
            self.gain += (target - self.gain) * self.release;
        }
        frame[0] *= self.gain;
        frame[1] *= self.gain;
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }
}

#[derive(Debug)]
pub struct Mixer {
    rate: u32,
    voices: Vec<Voice>,
    streams: Vec<StreamVoice>,
    table: Arc<SincTable>,
    pub master: f32,
    limiter: Limiter,
    /// Peak of the last block before the limiter, for meters.
    pub last_peak: f32,
}

impl Mixer {
    pub fn new(rate: u32) -> Mixer {
        Mixer {
            rate: rate.max(1),
            voices: Vec::new(),
            streams: Vec::new(),
            table: Arc::new(SincTable::new(1.0)),
            master: 1.0,
            limiter: Limiter::new(rate.max(1)),
            last_peak: 0.0,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn play(&mut self, id: VoiceId, sound: Sound, params: VoiceParams) {
        let step = f64::from(sound.rate) / f64::from(self.rate) * f64::from(params.pitch.max(0.01));
        // Downsampling needs a lower cutoff to avoid aliasing.
        let table = (step > 1.0).then(|| Arc::new(SincTable::new((1.0 / step) as f32 * 0.95)));
        self.voices.push(Voice { id, sound, params, pos: 0.0, table });
    }

    /// Play a stream; samples are pulled from `shared` as needed.
    pub fn play_stream(&mut self, id: VoiceId, shared: SharedStream, channels: u16, rate: u32, gain: f32) {
        self.streams.push(StreamVoice {
            id,
            shared,
            channels: channels.max(1) as usize,
            rate: rate.max(1),
            gain,
            buf: Vec::new(),
            base: 0,
            pos: 0.0,
        });
    }

    pub fn stop(&mut self, id: VoiceId) {
        self.voices.retain(|v| v.id != id);
        self.streams.retain(|v| v.id != id);
    }

    pub fn stop_all(&mut self) {
        self.voices.clear();
        self.streams.clear();
    }

    pub fn set_params(&mut self, id: VoiceId, params: VoiceParams) {
        if let Some(v) = self.voices.iter_mut().find(|v| v.id == id) {
            v.params = params;
        }
    }

    pub fn is_playing(&self, id: VoiceId) -> bool {
        self.voices.iter().any(|v| v.id == id)
    }

    /// Position of a voice in seconds.
    pub fn position(&self, id: VoiceId) -> Option<f64> {
        self.voices.iter().find(|v| v.id == id).map(|v| v.pos / f64::from(v.sound.rate))
    }

    pub fn voice_count(&self) -> usize {
        self.voices.len() + self.streams.len()
    }

    /// Mix into interleaved stereo `out` (overwrites it).
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        let rate = f64::from(self.rate);
        let mut finished = Vec::new();
        for v in &mut self.voices {
            let ch = v.sound.channels as usize;
            let n = v.sound.frames() as i64;
            let step = f64::from(v.sound.rate) / rate * f64::from(v.params.pitch.max(0.01));
            let table = v.table.as_deref().unwrap_or(&self.table);
            let loop_range = if v.params.looping {
                v.sound.looping.map(|(a, b)| (i64::from(a), i64::from(b).min(n - 1))).filter(|&(a, b)| b > a)
            } else {
                None
            };
            let samples = &v.sound.samples;
            // Frames past a loop end wrap to its start; outside the sound they are silent.
            let fetch = |i: i64, c: usize| -> f32 {
                let i = match loop_range {
                    Some((a, b)) if i > b => a + (i - a) % (b - a + 1),
                    _ => i,
                };
                if i < 0 || i >= n { 0.0 } else { samples[i as usize * ch + c] }
            };
            let theta = (v.params.pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
            let (gl, gr) = (theta.cos() * v.params.gain, theta.sin() * v.params.gain);
            for f in 0..frames {
                if loop_range.is_none() && v.pos >= n as f64 + crate::resample::HALF_TAPS as f64 {
                    finished.push(v.id);
                    break;
                }
                let (l, r) = if ch == 1 {
                    let s = table.interpolate(v.pos, |i| fetch(i, 0));
                    (s, s)
                } else {
                    (table.interpolate(v.pos, |i| fetch(i, 0)), table.interpolate(v.pos, |i| fetch(i, 1)))
                };
                // Mono sources are spread at −3 dB per side by the pan law; stereo keeps its channels.
                if ch == 1 {
                    out[2 * f] += l * gl;
                    out[2 * f + 1] += r * gr;
                } else {
                    out[2 * f] += l * gl * std::f32::consts::SQRT_2;
                    out[2 * f + 1] += r * gr * std::f32::consts::SQRT_2;
                }
                v.pos += step;
                if let Some((a, b)) = loop_range
                    && v.pos > b as f64 + 1.0
                {
                    v.pos -= (b - a + 1) as f64;
                }
            }
        }
        self.voices.retain(|v| !finished.contains(&v.id));
        self.render_streams(out);
        let mut peak = 0.0f32;
        for frame in out.chunks_exact_mut(2) {
            let mut fr = [frame[0] * self.master, frame[1] * self.master];
            peak = peak.max(fr[0].abs()).max(fr[1].abs());
            self.limiter.process(&mut fr);
            frame.copy_from_slice(&fr);
        }
        self.last_peak = peak;
    }
}

impl Mixer {
    fn render_streams(&mut self, out: &mut [f32]) {
        let frames = out.len() / 2;
        let rate = f64::from(self.rate);
        let half = crate::resample::HALF_TAPS as i64;
        let table = &self.table;
        let mut ended = Vec::new();
        for s in &mut self.streams {
            let ch = s.channels;
            let step = f64::from(s.rate) / rate;
            let need_frames = (s.pos + step * frames as f64).ceil() as i64 + half + 1 - s.base;
            let finished = {
                let Ok(mut shared) = s.shared.lock() else { continue };
                let have = (s.buf.len() / ch) as i64;
                let want = ((need_frames - have).max(0) as usize * ch).min(shared.samples.len());
                s.buf.extend(shared.samples.drain(..want));
                shared.finished
            };
            let available = (s.buf.len() / ch) as i64;
            if finished && s.pos as i64 >= s.base + available {
                ended.push(s.id);
                continue;
            }
            let (buf, base) = (&s.buf, s.base);
            let fetch = |i: i64, c: usize| -> f32 {
                let k = i - base;
                if k < 0 || k >= available { 0.0 } else { buf[k as usize * ch + c] }
            };
            let start_pos = s.pos;
            for f in 0..frames {
                // Without data yet, wait (silence) rather than run ahead of the producer.
                if (s.pos as i64) + half >= s.base + available && !finished {
                    break;
                }
                let l = table.interpolate(s.pos, |i| fetch(i, 0));
                let r = if ch > 1 { table.interpolate(s.pos, |i| fetch(i, 1)) } else { l };
                out[2 * f] += l * s.gain;
                out[2 * f + 1] += r * s.gain;
                s.pos += step;
            }
            let played = (s.pos.floor() - start_pos.floor()).max(0.0) as u64;
            if let Ok(mut shared) = s.shared.lock() {
                shared.played_frames += played;
            }
            // Drop frames well behind the read position.
            let keep_from = (s.pos as i64 - half - 2).max(s.base);
            let drop = ((keep_from - s.base) as usize * ch).min(s.buf.len());
            s.buf.drain(..drop);
            s.base = keep_from;
        }
        self.streams.retain(|s| !ended.contains(&s.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hz: f32, secs: f32, amp: f32) -> Sound {
        let n = (rate as f32 * secs) as usize;
        let s: Vec<i16> = (0..n)
            .map(|i| ((i as f32 * hz / rate as f32 * std::f32::consts::TAU).sin() * amp * 32767.0) as i16)
            .collect();
        Sound::from_i16(&s, 1, rate, None)
    }

    #[test]
    fn mono_voice_reaches_both_sides_and_ends() {
        let mut m = Mixer::new(48000);
        m.play(VoiceId(1), tone(24000, 440.0, 0.1, 0.5), VoiceParams::default());
        let mut out = vec![0.0; 2 * 4800];
        m.render(&mut out);
        let peak_l = out.iter().step_by(2).fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!((peak_l - 0.5 * std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02, "{peak_l}");
        assert_eq!(
            out[0..200].iter().step_by(2).zip(out[1..200].iter().step_by(2)).filter(|(a, b)| a != b).count(),
            0
        );
        let mut more = vec![0.0; 2 * 4800];
        m.render(&mut more);
        assert!(!m.is_playing(VoiceId(1)));
    }

    #[test]
    fn limiter_keeps_the_output_in_range() {
        let mut m = Mixer::new(48000);
        for i in 0..8 {
            m.play(
                VoiceId(i),
                tone(48000, 200.0, 0.2, 0.9),
                VoiceParams { pan: 0.0, ..VoiceParams::default() },
            );
        }
        let mut out = vec![0.0; 2 * 9600];
        m.render(&mut out);
        assert!(
            out.iter().all(|s| s.abs() <= 0.981),
            "max {}",
            out.iter().fold(0.0f32, |a, &b| a.max(b.abs()))
        );
        assert!(m.last_peak > 1.0);
    }

    #[test]
    fn looping_voice_keeps_playing_and_stop_removes_it() {
        let mut m = Mixer::new(48000);
        let mut s = tone(48000, 100.0, 0.05, 0.3);
        s.looping = Some((0, s.frames() as u32 - 1));
        m.play(VoiceId(7), s, VoiceParams { looping: true, ..VoiceParams::default() });
        let mut out = vec![0.0; 2 * 48000];
        m.render(&mut out);
        assert!(m.is_playing(VoiceId(7)));
        assert!(out[2 * 47000..].iter().any(|s| s.abs() > 0.1));
        m.stop(VoiceId(7));
        assert_eq!(m.voice_count(), 0);
    }

    #[test]
    fn streams_play_what_is_fed_and_end() {
        let mut m = Mixer::new(48000);
        let shared: SharedStream = Default::default();
        m.play_stream(VoiceId(3), shared.clone(), 2, 48000, 1.0);
        let mut out = vec![0.0; 2 * 480];
        m.render(&mut out);
        assert!(out.iter().all(|s| *s == 0.0), "no data yet: silence");
        {
            let mut s = shared.lock().unwrap();
            s.samples.extend(std::iter::repeat_n([0.5f32, -0.25], 4800).flatten());
            s.finished = true;
        }
        m.render(&mut out);
        assert!((out[200] - 0.5).abs() < 0.01 && (out[201] + 0.25).abs() < 0.01);
        for _ in 0..12 {
            m.render(&mut out);
        }
        assert!(shared.lock().unwrap().played_frames >= 4800 - 40);
        assert_eq!(m.voice_count(), 0);
    }

    #[test]
    fn pan_is_constant_power() {
        let mut m = Mixer::new(48000);
        m.play(
            VoiceId(1),
            tone(48000, 300.0, 0.05, 0.5),
            VoiceParams { pan: -1.0, ..VoiceParams::default() },
        );
        let mut out = vec![0.0; 2 * 1200];
        m.render(&mut out);
        let right = out.iter().skip(1).step_by(2).fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(right < 1e-6);
    }
}
