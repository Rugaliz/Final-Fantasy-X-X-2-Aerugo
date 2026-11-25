//! The sound driver state the music natives keep, and the remaster's sound commands they send. The remaster's
//! sound layer takes numbered commands with four arguments from a queue; the sim keeps that queue for the front
//! end, which plays them against the event projects and banks.
//!
//! Commands sent here: 0x1a read a music's data, 0x1c fade a music out (frames), 0x23 a music's volume, 0x25 and
//! 0x26 stop a music, 0x27 play a music, 0x28 read a music, 0x33 play a sequence sound (sound, volume, pan), 0x3c
//! a sound's volume over frames (sound, volume, frames), 0x3e a sound's pan over frames (sound, pan, frames).

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;

use super::FieldState;

/// One command for the remaster's sound layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundCmd {
    pub id: u32,
    pub args: [i32; 4],
}

/// The music side of the sound driver.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundState {
    /// Commands not yet taken by the front end.
    pub cmds: Vec<SoundCmd>,
    /// The music playing (−1 none; 0 before any).
    pub music: i32,
    /// A music to start when the current one fades (low byte the music, next byte its volume; −1 none).
    pub pending_music: i32,
    /// Each music's volume as last set.
    pub music_volume: [u8; 256],
    /// The music the driver keeps as its current one (−1 none).
    pub music_slot: i32,
    /// The music last faded out.
    pub faded_music: i32,
    /// Music 0x25 is playing (other parts of the engine look at it).
    pub music_0x25: bool,
    /// Music reads requested (the driver's request bits).
    pub music_requests: u32,
    pub music_standby: u32,
    /// The sound layer's next music and its switch (musics under 0xb6).
    pub layer_music: i32,
    pub layer_music_switch: i32,
    /// Sequence sounds are off.
    pub seq_off: bool,
    /// Sequence sounds started (never 0 once counting).
    pub seq_count: u32,
    pub seq_played: u32,
    /// Two sound switch masks (bit n: sound n on).
    pub mask_a: u32,
    pub mask_b: u32,
    /// The positional sounds' fade rate per frame (0144).
    pub pos_fade_rate: f32,
    /// The stream mode (1 keeps the stream on 00d7) and the stream request word.
    pub stream_mode: i32,
    pub stream_request: i32,
    /// The voice volume (018f; voices start at it, and the debug screen shows it as V.Vol), the sound mode and
    /// the pad's Dolby byte (+0x9d).
    pub voice_volume: u8,
    pub sound_mode: Option<i32>,
    pub dolby: u8,
    /// The sound data group 013d sent last.
    pub spk_group: Option<i32>,
    /// What the front end's sound layer answers to the synchronous queries: the sounds it is playing (command
    /// 0x3f, FmodSfx::isPlaying) and the music manager's status word (command 0x16: the current music's state
    /// flags | the manager's own).
    pub sfx_playing: Vec<i32>,
    pub music_status: u32,
}

impl Default for SoundState {
    fn default() -> Self {
        SoundState {
            cmds: Vec::new(),
            music: 0,
            pending_music: -1,
            music_volume: [0; 256],
            music_slot: 0,
            faded_music: 0,
            music_0x25: false,
            music_requests: 0,
            music_standby: 0,
            layer_music: 0,
            layer_music_switch: 0,
            seq_off: false,
            seq_count: 0,
            seq_played: 0,
            mask_a: 0,
            mask_b: 0,
            pos_fade_rate: 0.0,
            stream_mode: 0,
            stream_request: 0,
            voice_volume: 0,
            sound_mode: None,
            dolby: 0,
            spk_group: None,
            sfx_playing: Vec::new(),
            music_status: 0,
        }
    }
}

impl SoundState {
    fn cmd(&mut self, id: u32, args: [i32; 4]) {
        self.cmds.push(SoundCmd { id, args });
    }

    /// Command 0x3f (a synchronous command): the sound layer plays the sound, or a play of it (0x33) is still
    /// queued.
    fn is_playing(&self, sound: i32) -> bool {
        let sound = sound & 0x7FFF_FFFF;
        self.sfx_playing.contains(&sound) || self.cmds.iter().any(|c| c.id == 0x33 && c.args[0] == sound)
    }

    /// SndSepPlaySimple for other natives.
    pub fn play_simple_pub(&mut self, sound: i32, pan: i32, volume: i32) {
        self.play_simple(sound, pan, volume);
    }

    /// SndSepPlaySimple: command 0x33(sound, volume, pan) unless sequence sounds are off.
    fn play_simple(&mut self, sound: i32, pan: i32, volume: i32) {
        if !self.seq_off {
            self.seq_count = self.seq_count.wrapping_add(1).max(1);
            self.cmd(0x33, [sound & 0x7FFF_FFFF, volume & 0x7f, pan & 0x7f, 0]);
            self.seq_played |= 1;
        }
    }

    /// Read a music (the driver's read): only musics under 0x100; the sounds still playing finish first.
    fn read_music(&mut self, voice: &mut super::voice::VoiceState, n: i32) {
        if !(0..0x100).contains(&n) {
            return;
        }
        // The driver spins until its voice and music flags clear: game time stands still meanwhile.
        voice.playing = 0;
        self.cmd(0x28, [n, 0, 0, 0]);
        self.music_requests |= 1;
        self.music_standby |= self.music_requests;
    }

    /// A music's volume.
    fn set_volume(&mut self, n: i32, v: i32) {
        if (0..0x100).contains(&n) {
            self.cmd(0x23, [n, v, 1, 0]);
            self.music_volume[n as usize] = v as u8;
        }
    }

    /// Start a music: the driver keeps it, the sound layer switches to it and plays it at full volume.
    fn play_music(&mut self, n: i32) {
        if !(0..0x100).contains(&n) {
            return;
        }
        if self.music_slot != n {
            self.music_slot = n;
        }
        if (n as u32) < 0xb6 {
            self.layer_music = n;
            self.layer_music_switch = 1;
        }
        self.cmd(0x27, [n, 0, 0, 0]);
        self.music_volume[n as usize] = 0x7f;
        self.set_volume(n, 0x7f);
    }

    /// A music's volume over frames (cmd 0x22; the driver keeps the volume).
    fn volume_over(&mut self, n: i32, frames: i32, vol: i32) {
        if (0..0x100).contains(&n) {
            self.cmd(0x22, [n, frames, vol, 0]);
            self.music_volume[n as usize] = vol as u8;
        }
    }

    /// Stop a music (cmd 0x26) and let it go.
    fn stop(&mut self, n: i32) {
        if (0..0x100).contains(&n) {
            self.cmd(0x26, [n, 0, 0, 0]);
            self.release(n);
        }
    }

    /// Free a music (cmd 0x25) and let it go.
    fn free(&mut self, n: i32) {
        if (0..0x100).contains(&n) {
            self.cmd(0x25, [n, 0, 0, 0]);
            self.release(n);
        }
    }

    /// Fade a music out over frames: cmd 0x1c, its volume 0, the faded music, let go.
    fn fade_out(&mut self, n: i32, frames: i32) {
        if (0..0x100).contains(&n) {
            self.cmd(0x1c, [n, frames, 0, 0]);
            self.music_volume[n as usize] = 0;
            self.faded_music = n;
            self.release(n);
        }
    }

    /// Play music `m` at `v`: another music than the kept one: under 0x100 the driver keeps it, its volume 0
    /// when it is the one last faded and nothing is kept (else 0x7f), cmd 0x17 (m, v); otherwise the kept one fades
    /// over v.
    fn event_play(&mut self, m: i32, v: i32) {
        let old = self.music_slot;
        if m == old {
            return;
        }
        if (m as u32) < 0x100 {
            self.music_slot = m;
            self.music_volume[m as usize] = if self.faded_music == m && old == -1 { 0 } else { 0x7f };
            self.cmd(0x17, [m, v, 0, 0]);
        } else {
            self.fade_out(old, v);
        }
    }

    /// The driver lets go of a music it keeps.
    fn release(&mut self, n: i32) {
        if self.music_slot == n {
            self.music_slot = -1;
        }
    }
}

/// A sound's volume over frames: the sound without its top bit, the volume's low 7 bits.
fn sound_volume(s: &mut SoundState, sound: i32, frames: i32, volume: i32) {
    s.cmd(0x3c, [sound & 0x7FFF_FFFF, volume & 0x7f, frames, 0]);
}

/// A sound's flags in the event's background table, 0 when it is not listed.
fn background_flags(script: &[u8], sound: i32) -> u32 {
    let u32_at =
        |at: usize| script.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let Some(table) = u32_at(0x2c).filter(|&t| t != 0) else { return 0 };
    let Some(list) = u32_at(table + 0xc).filter(|&l| l != 0) else { return 0 };
    let n = u32_at(list).unwrap_or(0) as i32;
    (0..n.max(0) as usize)
        .find(|&i| u32_at(list + 4 + i * 8).unwrap_or(0) as i32 == sound)
        .map_or(0, |i| u32_at(list + 8 + i * 8).unwrap_or(0) as u32)
}

/// The event's background sounds (the script header's table at +0x2c, list at its +0xc: a count, then (sound,
/// flags) pairs) fade over `frames` toward `volume`: those whose flags have no 0x30 and have 2 (or 10 when `all`).
/// With the save's +0x28 byte set the volume is halved (flag 0x80 sounds keep it with flag 0x40); without it, flag
/// 0x80 sounds fade to 0.
fn fade_background(s: &mut SoundState, script: &[u8], save28: u8, frames: i32, volume: i32, all: bool) {
    let u32_at =
        |at: usize| script.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize);
    let Some(table) = u32_at(0x2c).filter(|&t| t != 0) else { return };
    let Some(list) = u32_at(table + 0xc).filter(|&l| l != 0) else { return };
    let n = u32_at(list).unwrap_or(0) as i32;
    for i in 0..n.max(0) as usize {
        let sound = u32_at(list + 4 + i * 8).unwrap_or(0) as i32;
        let flags = u32_at(list + 8 + i * 8).unwrap_or(0) as u32;
        let want = if all { flags & 10 } else { flags & 2 };
        if flags & 0x30 != 0 || want == 0 {
            continue;
        }
        let halved = |v: i32| (v * 0x80) / 0x100;
        let v = if flags & 0x80 != 0 {
            if save28 == 0 {
                0
            } else if flags & 0x40 == 0 {
                halved(volume)
            } else {
                volume
            }
        } else if save28 != 0 {
            halved(volume)
        } else {
            volume
        };
        sound_volume(s, sound, frames, v);
    }
}

/// 0102(n) reads a music (its wait is done once the frame is on time); 0104(n) plays it; 0188(frames) fades the
/// current music out over that many frames (below 1: stops it) and starts a pending one.
pub fn register(n: &mut Natives<FieldState>) {
    n.register(
        0x0102,
        Native {
            init: Some(|_, c| c.scratch[0] = c.stack.pop_int()),
            wait: Some(|_, _| 5),
            result: Some(|g, c| {
                let m = c.scratch[0];
                g.sound.read_music(&mut g.voice, m);
                Value::Int(m)
            }),
        },
    );
    // 0100(sound, frames, volume): a sound's volume over frames; returns the volume.
    n.register(
        0x0100,
        Native::result(|g, c| {
            let volume = c.stack.pop_int();
            let frames = c.stack.pop_int();
            let sound = c.stack.pop_int();
            sound_volume(&mut g.sound, sound, frames, volume);
            Value::Int(volume)
        }),
    );
    // 00fd(sound, pan, volume): play a sequence sound (unless they are off); returns the sound.
    n.register(
        0x00FD,
        Native::result(|g, c| {
            let volume = c.stack.pop_int();
            let pan = c.stack.pop_int();
            let sound = c.stack.pop_int();
            g.sound.play_simple(sound, pan, volume);
            Value::Int(sound)
        }),
    );
    // 0193(sound): play it (volume 0x7f, pan 0x3f) unless it plays; if it does, its volume goes over 12 frames to
    // 0x7f (0x3f with the save's +0x28 byte) unless the event's background table gives it flag 0x40. Returns the
    // sound.
    n.register(
        0x0193,
        Native::result(|g, c| {
            let sound = c.stack.pop_int();
            if !g.sound.is_playing(sound) {
                g.sound.play_simple(sound, 0x3f, 0x7f);
                return Value::Int(sound);
            }
            let save28 = c.memory.global[0].get(0x28).copied().unwrap_or(0);
            let flags = background_flags(&c.memory.script, sound);
            if flags & 0x40 == 0 {
                sound_volume(&mut g.sound, sound, 0xc, if save28 != 0 { 0x3f } else { 0x7f });
            }
            Value::Int(sound)
        }),
    );
    // 01a0(sound, pan, volume): play it unless it plays; if it does, volume (halved with the save's +0x28 byte)
    // and pan go there over 12 frames unless its background flags have 0x40. Returns the sound.
    n.register(
        0x01A0,
        Native::result(|g, c| {
            let mut volume = c.stack.pop_int();
            let pan = c.stack.pop_int();
            let sound = c.stack.pop_int();
            if !g.sound.is_playing(sound) {
                g.sound.play_simple(sound, pan, volume);
                return Value::Int(sound);
            }
            let save28 = c.memory.global[0].get(0x28).copied().unwrap_or(0);
            let flags = background_flags(&c.memory.script, sound);
            if save28 != 0 {
                volume = volume.wrapping_mul(0x80) / 0x100;
            }
            if flags & 0x40 == 0 {
                sound_volume(&mut g.sound, sound, 0xc, volume);
                // Command 0x3e(sound, pan, frames): the pan over 12 frames.
                g.sound.cmd(0x3e, [sound & 0x7FFF_FFFF, pan & 0x7f, 0xc, 0]);
            }
            Value::Int(sound)
        }),
    );
    // 01bb: the music manager's status word (command 0x16, answered by the sound layer).
    n.register(0x01BB, Native::result(|g, _| Value::Int(g.sound.music_status as i32)));
    // 0143(frames): fade the event's background sounds out; returns the frames.
    n.register(
        0x0143,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            // The positional sounds' fade step.
            g.pos_sounds.rate = if frames == 0 { -1.0 } else { -1.0 / (frames as f32 * 0.5) };
            let save28 = c.memory.global[0].get(0x28).copied().unwrap_or(0);
            fade_background(&mut g.sound, &c.memory.script, save28, frames, 0, false);
            Value::Int(frames)
        }),
    );
    n.register(
        0x0104,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            g.sound.music_0x25 = m == 0x25;
            g.sound.cmd(0x1a, [m, 0, 0, 0]);
            g.sound.play_music(m);
            g.sound.music = m;
            Value::Int(1)
        }),
    );
    // 017c(m): read the music (cmd 0x1a), then kept (cmd 0x21, volume 0x7f); current = m. 01e7(m, v): the same
    // with cmd 0x1b (m, v). Both return 1.
    n.register(
        0x017C,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            let s = &mut g.sound;
            s.cmd(0x1a, [m, 0, 0, 0]);
            if (0..0x100).contains(&m) {
                s.music_slot = m;
                s.cmd(0x21, [m, 0, 0, 0]);
                s.set_volume(m, 0x7f);
            }
            s.music = m;
            Value::Int(1)
        }),
    );
    n.register(
        0x01E7,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let m = c.stack.pop_int();
            let s = &mut g.sound;
            s.cmd(0x1a, [m, 0, 0, 0]);
            if (0..0x100).contains(&m) {
                s.music_slot = m;
                s.cmd(0x1b, [m, v, 0, 0]);
                s.set_volume(m, 0x7f);
            }
            s.music = m;
            Value::Int(1)
        }),
    );
    // 017e(m, v): nothing kept → read and play m (as 0104); else play m at v; current = m; returns 1.
    n.register(
        0x017E,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let m = c.stack.pop_int();
            let s = &mut g.sound;
            if s.music_slot < 0 {
                s.cmd(0x1a, [m, 0, 0, 0]);
                s.play_music(m);
            } else {
                s.event_play(m, v);
            }
            s.music = m;
            Value::Int(1)
        }),
    );
    // 01f8(m, frames): frames < 1 → the current music stopped and freed; else m (≥ 0) fades out
    // over frames. Returns frames.
    n.register(
        0x01F8,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            let m = c.stack.pop_int();
            let s = &mut g.sound;
            if frames < 1 {
                let cur = s.music;
                s.stop(cur);
                s.free(cur);
            } else if m >= 0 {
                s.fade_out(m, frames);
            }
            Value::Int(frames)
        }),
    );
    // 0184(v): the music tempo (cmd 0x1e, v clamped to −0x80..0x7f as a byte); 0185(m, v): a music's tempo
    // (cmd 0x1d). Both return 1.
    n.register(
        0x0184,
        Native::result(|g, c| {
            let v = c.stack.pop_int().clamp(-0x80, 0x7f);
            g.sound.cmd(0x1e, [i32::from(v as u8), 0, 0, 0]);
            Value::Int(1)
        }),
    );
    n.register(
        0x0185,
        Native::result(|g, c| {
            let v = c.stack.pop_int().clamp(-0x80, 0x7f);
            let m = c.stack.pop_int();
            g.sound.cmd(0x1d, [m, i32::from(v as u8), 0, 0]);
            Value::Int(1)
        }),
    );
    // 018f(a, b): the kept byte = b; cmd 0xd (a, b when settings bit 8 is set, else 0); returns 0.
    n.register(
        0x018F,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            g.sound.voice_volume = b as u8;
            let settings = c.memory.global[0]
                .get(0x3d0c..0x3d10)
                .map_or(0, |x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
            g.sound.cmd(0xd, [a, if settings >> 8 & 1 != 0 { b } else { 0 }, 0, 0]);
            Value::Int(0)
        }),
    );
    // 023a(on): settings bit 7; the sound mode 0 with it, else 2 − settings bit 0; the pad's Dolby byte
    // = settings bit 4; returns 1.
    n.register(
        0x023A,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let Some(b) = c.memory.global[0].get_mut(0x3d0c..0x3d10) else { return Value::Int(1) };
            let mut w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            w = if on { w | 0x80 } else { w & !0x80 };
            b.copy_from_slice(&w.to_le_bytes());
            let mode = if (w as u8 as i8) < 0 { 0 } else { 2 - i32::from(w & 1 != 0) };
            g.sound.sound_mode = Some(mode.clamp(0, 2));
            g.sound.dolby = ((w as u8 as i8 as i32 & 0x10) >> 4) as u8;
            Value::Int(1)
        }),
    );
    n.register(
        0x0188,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let s = &mut g.sound;
            let m = s.music;
            if v < 1 {
                if (0..0x100).contains(&m) {
                    s.cmd(0x26, [m, 0, 0, 0]);
                    s.release(m);
                    s.cmd(0x25, [m, 0, 0, 0]);
                    s.release(m);
                }
                s.music = -1;
                return Value::Int(v);
            }
            if m >= 0 {
                if m < 0x100 {
                    s.cmd(0x1c, [m, v, 0, 0]);
                    s.music_volume[m as usize] = 0;
                    s.faded_music = m;
                    s.release(m);
                }
                s.music = -1;
            }
            if s.pending_music != -1 {
                let id = s.pending_music & 0xff;
                let vol = (s.pending_music >> 8) & 0xff;
                g.sound.read_music(&mut g.voice, id);
                let s = &mut g.sound;
                s.cmd(0x1a, [id, 0, 0, 0]);
                s.play_music(id);
                s.set_volume(id, vol);
                s.music = id;
                s.pending_music = -1;
            }
            Value::Int(v)
        }),
    );
    // 0103(m): free a music (it stops being the current one); 0166(m): play it as the current one; 016c: stop the
    // current one; 016a(m, v): a music's volume (returns v).
    n.register(
        0x0103,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            let s = &mut g.sound;
            if s.music == m {
                s.music = -1;
            }
            s.free(m);
            Value::Int(1)
        }),
    );
    n.register(
        0x0166,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            g.sound.play_music(m);
            g.sound.music = m;
            Value::Int(1)
        }),
    );
    n.register(
        0x016C,
        Native::result(|g, _| {
            let s = &mut g.sound;
            if s.music >= 0 {
                let m = s.music;
                s.stop(m);
            }
            s.music = -1;
            Value::Int(1)
        }),
    );
    n.register(
        0x016A,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let m = c.stack.pop_int();
            g.sound.set_volume(m, v);
            Value::Int(v)
        }),
    );
    // 0155(id) / 0157(id): a bit of the first sound mask on / off; 0154 / 0156 the worker's own sound (none: 0).
    fn mask_a(g: &mut FieldState, id: i32, on: bool) {
        let b = 1u32 << (id as u32 & 31);
        let m = &mut g.sound.mask_a;
        *m = if on { *m | b } else { *m & !b };
    }
    n.register(
        0x0155,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            mask_a(g, id, true);
            Value::Int(1)
        }),
    );
    n.register(
        0x0157,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            mask_a(g, id, false);
            Value::Int(1)
        }),
    );
    for (id, on) in [(0x0154u16, true), (0x0156, false)] {
        n.register(
            id,
            Native::result(if on {
                |g, c| {
                    let s = g.workers.list[c.worker].i32(0x40);
                    if s == -1 {
                        return Value::Int(0);
                    }
                    mask_a(g, s, true);
                    Value::Int(1)
                }
            } else {
                |g, c| {
                    let s = g.workers.list[c.worker].i32(0x40);
                    if s == -1 {
                        return Value::Int(0);
                    }
                    mask_a(g, s, false);
                    Value::Int(1)
                }
            }),
        );
    }
    // 0201: both masks off for the worker's own sound.
    n.register(
        0x0201,
        Native::result(|g, c| {
            let s = g.workers.list[c.worker].i32(0x40);
            if s != -1 {
                let b = 1u32 << (s as u32 & 31);
                g.sound.mask_a &= !b;
                g.sound.mask_b &= !b;
            }
            Value::Int(1)
        }),
    );
    // 016d(m): stop a music (it stops being the current one). 017f(v): the current music's volume; returns it.
    n.register(
        0x016D,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            let s = &mut g.sound;
            if s.music == m {
                s.music = -1;
            }
            s.stop(m);
            Value::Int(1)
        }),
    );
    n.register(
        0x017F,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let m = g.sound.music;
            if -1 < m {
                g.sound.set_volume(m, v);
            }
            Value::Int(m)
        }),
    );
    // 0200(id) / 0202(id): both sound masks' bit on / off.
    n.register(
        0x0200,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            let b = 1u32 << (id as u32 & 31);
            g.sound.mask_a |= b;
            g.sound.mask_b |= b;
            Value::Int(1)
        }),
    );
    n.register(
        0x0202,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            mask_a(g, id, false);
            g.sound.mask_b &= !(1u32 << (id as u32 & 31));
            Value::Int(1)
        }),
    );
    // 015d(on) / 015e(on): two switches of the system's holds byte that nothing reads; return the switch before.
    n.register(
        0x015D,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let holds = &mut g.workers.sys.holds;
            let bit = crate::workers::holds::UNREAD_SWITCHES[0];
            let old = *holds & bit != 0;
            crate::workers::set_flag(holds, bit, on);
            Value::Int(i32::from(old))
        }),
    );
    n.register(
        0x015E,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let holds = &mut g.workers.sys.holds;
            let bit = crate::workers::holds::UNREAD_SWITCHES[1];
            let old = *holds & bit != 0;
            crate::workers::set_flag(holds, bit, on);
            Value::Int(i32::from(old))
        }),
    );
    // 0144(frames): the positional sounds' fade rate (1, or 1/(frames/2)) and the background sounds back up to
    // full volume over the frames; returns the frames.
    n.register(
        0x0144,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            g.sound.pos_fade_rate =
                if frames == 0 { 1.0 } else { (1.0 / (f64::from(frames as f32) * 0.5)) as f32 };
            let save28 = c.memory.global[0].get(0x28).copied().unwrap_or(0);
            fade_background(&mut g.sound, &c.memory.script, save28, frames, 0x7f, false);
            Value::Int(frames)
        }),
    );
    // 00d7: stop the sound stream (command 10) and clear the stream request (unless the stream mode is 1).
    n.register(
        0x00D7,
        Native::result(|g, _| {
            if g.sound.stream_mode != 1 {
                g.sound.stream_request = 0;
                g.sound.cmd(10, [0; 4]);
            }
            Value::Int(0)
        }),
    );
    // 016b(m, frames, volume): a music's volume over frames (cmd 0x22); 0180(frames, volume): the current
    // music's (returns the current music).
    n.register(
        0x016B,
        Native::result(|g, c| {
            let vol = c.stack.pop_int();
            let frames = c.stack.pop_int();
            let m = c.stack.pop_int();
            g.sound.volume_over(m, frames, vol);
            Value::Int(vol)
        }),
    );
    n.register(
        0x0180,
        Native::result(|g, c| {
            let vol = c.stack.pop_int();
            let frames = c.stack.pop_int();
            let m = g.sound.music;
            if -1 < m {
                g.sound.volume_over(m, frames, vol);
            }
            Value::Int(m)
        }),
    );
    // 01cc: the music the driver keeps (its music list's first place).
    n.register(0x01CC, Native::result(|g, _| Value::Int(g.sound.music_slot)));
    // 0167: sound command 0x19; returns 1.
    n.register(
        0x0167,
        Native::result(|g, _| {
            g.sound.cmd(0x19, [0; 4]);
            Value::Int(1)
        }),
    );
}
