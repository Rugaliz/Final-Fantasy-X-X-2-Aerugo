//! Voices: 00d5 stands a voice by, 00d6 starts it, 00d8 asks whether it plays, 00d9 waits for its end. Each stage
//! first asks the sync list for a frame count; a voice listed for the stage counts those frames instead of
//! following the sound. Otherwise the standby is ready unless a 0203 character read runs, the start is ready at
//! once (the PC build has no sound processor status to wait on), and the voice plays for its sample's length.
//!
//! The voice plays from its start for its sample's length in ticks (rounded up): the remaster's sound thread
//! follows the device clock, which a deterministic run replaces by the sample's own length.

use std::collections::HashMap;

use aerugo_formats::voice_map::{self, SyncEntry};
use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;
use aerugo_vfs::Vfs;

use super::FieldState;

const ROOT: &str = "ffx_data/gamedata/ps3data";

/// A stage's frame count from the sync list, and whether it is counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Countdown {
    pub frames: i32,
    pub on: bool,
}

impl Countdown {
    fn set(frames: i32) -> Countdown {
        Countdown { frames, on: frames != 0 }
    }
}

/// The voice state the natives keep (the script side's and the sound driver's flags).
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceState {
    /// The voice the last standby named.
    pub voice: i32,
    pub standby: Countdown,
    pub start: Countdown,
    pub status: Countdown,
    pub sync: Countdown,
    /// A standby was sent and no start followed.
    pub standby_pending: bool,
    /// Ticks the started voice still plays.
    pub playing: u32,
    /// The start delay a script left for the next start (−1 none).
    pub start_delay: i32,
    /// Japanese voices (else English).
    pub japanese: bool,
    /// Events 0x189–0x18e put every voice in one project from then on.
    pub single_project: bool,
    sync_list: Option<Vec<SyncEntry>>,
    names: Option<HashMap<u32, String>>,
    projects: Option<Vec<(u32, String)>>,
    lengths: HashMap<(i32, u16), u32>,
}

impl Default for VoiceState {
    fn default() -> Self {
        VoiceState {
            voice: 0,
            standby: Countdown::default(),
            start: Countdown::default(),
            status: Countdown::default(),
            sync: Countdown::default(),
            standby_pending: false,
            playing: 0,
            start_delay: -1,
            japanese: false,
            single_project: false,
            sync_list: None,
            names: None,
            projects: None,
            lengths: HashMap::new(),
        }
    }
}

impl VoiceState {
    fn lang(&self) -> &'static str {
        if self.japanese { "jp" } else { "us" }
    }

    /// The sync list's frames for a voice's stage.
    fn sync_frames(&mut self, vfs: Option<&Vfs>, voice: i32, stage: u32) -> i32 {
        if self.sync_list.is_none() {
            let path = format!("{ROOT}/syncdata/syncvoicelist.txt");
            self.sync_list = Some(
                vfs.and_then(|v| v.read(&path).ok())
                    .and_then(|d| voice_map::parse_sync(&d))
                    .unwrap_or_default(),
            );
        }
        voice_map::sync_frames(self.sync_list.as_deref().unwrap_or(&[]), voice as u32, stage)
    }

    /// A voice's length in ticks in `event`: its name, the event's project in the voice's slot, the project's
    /// bank sample of that name.
    pub fn length_ticks(&mut self, vfs: Option<&Vfs>, event: u16, voice: i32) -> u32 {
        if let Some(&t) = self.lengths.get(&(voice, event)) {
            return t;
        }
        let Some(vfs) = vfs else { return 0 };
        let lang = self.lang();
        if self.names.is_none() {
            let d = vfs.read(&format!("{ROOT}/sound_pc/voice/voiceidmapper.txt")).ok();
            self.names =
                Some(d.and_then(|d| voice_map::parse_ml(&d)).unwrap_or_default().into_iter().collect());
        }
        if self.projects.is_none() {
            let d = vfs.read(&format!("{ROOT}/sound_pc/voice/{lang}/voicefevmapper.txt")).ok();
            self.projects = Some(d.and_then(|d| voice_map::parse_ml(&d)).unwrap_or_default());
        }
        if (0x189..=0x18e).contains(&event) {
            self.single_project = true;
        }
        let ticks = (|| {
            let name = self.names.as_ref()?.get(&(voice as u32))?.clone();
            let slot = if self.single_project || event == 0x10e { 0 } else { voice_map::voice_slot(&name)? };
            let project = self.projects.as_ref()?.iter().find(|(e, p)| {
                u32::from(event) == *e
                    && if self.single_project || event == 0x10e {
                        true
                    } else {
                        voice_map::project_slot(p) == Some(slot)
                    }
            })?;
            let fev = format!("{ROOT}/sound_pc/voice/{lang}/{}.fev", project.1);
            let bank = format!("{}00.fsb", aerugo_formats::fev::bank_prefix(&fev));
            let first = vfs.read_range(&bank, 0, 0x40).ok()?;
            let len = aerugo_formats::fsb::Bank::header_len(&first).ok()?;
            let head = vfs.read_range(&bank, 0, len as u64).ok()?;
            let total = vfs.size(&bank)? as usize;
            let b = aerugo_formats::fsb::Bank::parse_header(&head, total).ok()?;
            let s = b.samples.iter().find(|s| s.name == name)?;
            let ticks = (u64::from(s.frames) * u64::from(crate::tick::GAME_TICK_HZ))
                .div_ceil(u64::from(s.rate.max(1)));
            Some(ticks as u32)
        })()
        .unwrap_or(0);
        self.lengths.insert((voice, event), ticks);
        ticks
    }

    /// The driver's playing flags (00d8 / 00d9's poll).
    fn playing(&self) -> bool {
        self.playing > 0
    }

    /// A tick of the sound driver.
    pub fn tick(&mut self) {
        self.playing = self.playing.saturating_sub(1);
    }
}

pub fn register(n: &mut Natives<FieldState>) {
    // 00d5(voice): stand a voice by; the result sends the standby unless one is pending.
    n.register(
        0x00D5,
        Native {
            init: Some(|g, c| {
                let v = c.stack.pop_int();
                c.scratch[0] = v;
                g.voice.voice = v;
                let f = g.voice.sync_frames(g.vfs.as_ref(), v, 1);
                g.voice.standby = Countdown::set(f);
            }),
            wait: Some(|g, _| {
                let s = &mut g.voice.standby;
                if s.on {
                    if s.frames > 0 {
                        s.frames -= 1;
                        return 0;
                    }
                    *s = Countdown::default();
                    return 5;
                }
                if g.chr_read_mask != 0 { 0 } else { 5 }
            }),
            result: Some(|g, _| {
                if !g.voice.standby_pending {
                    g.voice.standby_pending = true;
                }
                Value::Int(0)
            }),
        },
    );
    // 00d6: start the standing-by voice (after the delay a script left, none by default).
    n.register(
        0x00D6,
        Native {
            init: Some(|g, c| {
                let v = g.voice.voice;
                let f = g.voice.sync_frames(g.vfs.as_ref(), v, 2);
                g.voice.start = Countdown::set(f);
                c.scratch[1] = 0;
                // The kept delay stands only in the sound driver's alternative mode (a debug option), so the
                // start waits none.
                c.scratch[0] = 0;
                g.voice.start_delay = -1;
            }),
            wait: Some(|g, c| {
                let s = &mut g.voice.start;
                if s.on {
                    if s.frames > 0 {
                        s.frames -= 1;
                        return 0;
                    }
                    *s = Countdown::default();
                    return 5;
                }
                match c.scratch[1] {
                    0 => c.scratch[1] = 1,
                    1 => {}
                    _ => return 0,
                }
                if c.scratch[0] > 0 {
                    c.scratch[0] -= 1;
                    return 0;
                }
                5
            }),
            result: Some(|g, _| {
                let v = g.voice.voice;
                let event = g.scene.unwrap_or(0);
                let vfs = g.vfs.clone();
                g.voice.standby_pending = false;
                g.voice.playing = g.voice.length_ticks(vfs.as_ref(), event, v).max(1);
                g.voice.status.on = g.voice.status.frames != 0;
                let f = g.voice.sync_frames(vfs.as_ref(), v, 3);
                g.voice.sync = Countdown::set(f);
                Value::Int(0)
            }),
        },
    );
    // 00d8: 1 while the voice plays.
    n.register(
        0x00D8,
        Native::result(|g, _| {
            let s = &mut g.voice.status;
            if s.on {
                if s.frames < 1 {
                    *s = Countdown::default();
                    return Value::Int(0);
                }
                s.frames -= 1;
                return Value::Int(1);
            }
            Value::Int(i32::from(g.voice.playing()))
        }),
    );
    // 00d9: wait for the voice's end.
    n.register(
        0x00D9,
        Native {
            init: None,
            wait: Some(|g, _| {
                let s = &mut g.voice.sync;
                if s.on {
                    if s.frames < 1 {
                        *s = Countdown::default();
                        return 1;
                    }
                    s.frames -= 1;
                    return 0;
                }
                u32::from(!g.voice.playing())
            }),
            result: None,
        },
    );
}
