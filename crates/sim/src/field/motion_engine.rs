//! The character engine's motion player, as the original runs it.
//!
//! A motion id is a character id (high 16 bits) and an entry number (low 16 bits). The engine keeps a list of
//! loaded motion groups; setting a motion finds the first entry with that number in the groups of that character
//! id (in list order) and points the character's sequence at the entry's bytecode. Each frame the sequence runs
//! until it waits:
//!
//! - 0: end (the character stops playing once its range is done);
//! - 1 record, plays, from, to: play a frame range of a motion record (`from`/`to` index the record's frame
//!   table; the range ends a frame later unless either `to` or `plays` is 1), `plays` times (0: for ever);
//! - 2: wait until the range is done;
//! - 3 n: wait n frames;
//! - 4 i: jump through the entry's jump table;
//! - 5: stop;
//! - 6 n: wait until the play count (native 5022) reaches n.
//!
//! The frame clock counts 1/256 frames and advances each frame by speed × motion speed × rate / 0x1E00. A range
//! loops by its length until its plays run out; the last play stops on its last whole frame. Channels with frame
//! values are decoded up to the frame after the clock (wrapping back to the start decodes from the start again,
//! keeping the old value as the previous one) and interpolated by the clock's fraction (rotations the short way
//! round within a turn). The character's joints then move towards those values by a 1/n share of the way, n
//! counting down from the motion's blend length (4 frames unless a script set another) to 1.

use std::sync::Arc;

use aerugo_formats::motion::{Channel, MotionEvent, MotionGroup};

/// A motion file the engine has loaded: kind (0 a character's resident file, 1 an event's, 2 the shared party
/// file), resident number and character id.
pub type FileKey = (u8, u16, u16);

/// The engine's motion group slots (100). Adding a file puts each of its groups in the first free slot; motion
/// lookups go through the slots in order. The host carries the slots from event to event: every event change
/// removes all files but the shared party file, which keeps its slots.
#[derive(Debug, Clone, Default)]
pub struct Groups {
    slots: Vec<Option<(FileKey, Arc<MotionGroup>)>>,
    /// Files read before (in the game's file cache: they load without waiting).
    read_before: Vec<FileKey>,
    /// The shared party motion file was asked for (a game started) and lands with the next read that waits.
    pub party_pending: bool,
}

/// How many motion groups the engine holds at once.
const SLOTS: usize = 100;

impl Groups {
    /// Whether a file is loaded.
    pub fn has(&self, key: FileKey) -> bool {
        self.slots.iter().flatten().any(|(k, _)| *k == key)
    }

    /// Whether a file was read before (it loads without waiting).
    pub fn cached(&self, key: FileKey) -> bool {
        self.read_before.contains(&key)
    }

    /// Add a file's groups (in file order) to the first free slots.
    pub fn add(&mut self, key: FileKey, groups: Vec<MotionGroup>) {
        if !self.read_before.contains(&key) {
            self.read_before.push(key);
        }
        for g in groups {
            let g = Some((key, Arc::new(g)));
            match self.slots.iter().position(Option::is_none) {
                Some(i) => self.slots[i] = g,
                None if self.slots.len() < SLOTS => self.slots.push(g),
                None => log::warn!("motion groups: all {SLOTS} slots are taken"),
            }
        }
    }

    /// Remove a file's groups.
    pub fn remove(&mut self, key: FileKey) {
        for s in &mut self.slots {
            if s.as_ref().is_some_and(|(k, _)| *k == key) {
                *s = None;
            }
        }
    }

    /// Remove every file but the shared party file.
    pub fn remove_all_but_party(&mut self) {
        for s in &mut self.slots {
            if s.as_ref().is_some_and(|(k, _)| k.0 != 2) {
                *s = None;
            }
        }
    }

    /// The loaded files in slot order (each once).
    pub fn files(&self) -> Vec<FileKey> {
        let mut out = Vec::new();
        for (k, _) in self.slots.iter().flatten() {
            if !out.contains(k) {
                out.push(*k);
            }
        }
        out
    }

    /// The file, group and entry motion `id` names: the first entry with its number in the groups of its
    /// character id.
    pub fn find(&self, id: i32) -> Option<(FileKey, Arc<MotionGroup>, usize)> {
        let chr = ((id >> 16) & 0xFFFF) as u16;
        let number = (id & 0xFFFF) as u16;
        self.slots
            .iter()
            .flatten()
            .filter(|(_, g)| g.chr_id == chr)
            .find_map(|(k, g)| g.entries.iter().position(|&(n, _)| n == number).map(|e| (*k, g.clone(), e)))
    }
}

/// One joint channel: what the motion asks for (`target`) and where the joint is (`current`). Rotations are in
/// turns, translations and scales in 1/4096 of the file's units (×4096 makes them file values).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ChannelState {
    pub target: f32,
    pub current: f32,
    rotation: bool,
    prev: i16,
    cur: i16,
}

/// Where a character's sequence is.
#[derive(Debug, Clone, PartialEq)]
enum Pc {
    /// In a group's data.
    Data(usize),
    /// In the engine's own sequence for frame ranges: wait for the range, then end.
    Range(usize),
}

/// The engine's own sequence for frame ranges.
const RANGE_SEQUENCE: [u8; 2] = [2, 0];

/// A model's joint relation to another character's motions (`.chr` section 4): the motion's joint k drives the
/// model's joint `map[k]` (0xFFFF: none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub owner: u16,
    pub map: Vec<u16>,
}

/// The joint relations of a character's `.chr` data: section 4 (offset at 0x30, count at 0x34) lists records of
/// {owner character id, joint count, …, the joint map from +8}.
pub fn chr_relations(d: &[u8]) -> Vec<Relation> {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let (Some(at), Some(n)) = (u32_at(0x30), u32_at(0x34)) else { return Vec::new() };
    if at == 0 {
        return Vec::new();
    }
    let n = i32::from(n as i16).max(0) as usize;
    (0..n)
        .filter_map(|i| {
            let r = u32_at(at as usize + 4 * i)? as usize;
            let owner = u16_at(r)?;
            let count = usize::from(u16_at(r + 2)?);
            let map = (0..count).map(|k| u16_at(r + 8 + 2 * k)).collect::<Option<Vec<u16>>>()?;
            Some(Relation { owner, map })
        })
        .collect()
}

/// How setting a motion went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetMotion {
    Set,
    /// The motion is already set (and not stopped).
    Same,
    /// No loaded group has it.
    Missing,
}

/// A character's motion player.
#[derive(Debug, Clone, PartialEq)]
pub struct Player {
    /// The motion set last.
    pub motion: i32,
    stop_flag: bool,
    /// The entry the sequence belongs to, and the file it came from.
    entry: Option<(Arc<MotionGroup>, usize)>,
    pub motion_file: Option<FileKey>,
    /// The joint relation a motion of another character's plays through.
    remap: Option<Arc<Relation>>,
    /// Joints any motion has set (the others keep the model's own pose).
    touched: usize,
    pc: Option<Pc>,
    /// The motion record playing.
    record: Option<(Arc<MotionGroup>, usize)>,
    start: i32,
    end1: i32,
    len: i32,
    loops: i16,
    /// Plays for the next range (−1: the sequence's own).
    pub loops_next: i16,
    /// The range is still playing.
    pub in_range: i16,
    wait: i16,
    /// The sequence stopped (code 5).
    pub stopped: bool,
    /// A motion is playing (what the motion waits watch).
    pub playing: bool,
    /// The play count codes 6 wait for (−1 until a script sets it).
    pub plays: i8,
    /// Frames a new motion blends over (0: none).
    pub blend: i16,
    /// The blend length for the next motion set (0: the default 4).
    pub next_blend: i16,
    /// Speed (0x100 normal).
    pub speed: i16,
    speed2: i16,
    step_size: i16,
    blend_count: i16,
    clock: i32,
    /// The frame the pose shows (1/256 frames).
    pub shown: i32,
    decoded: i32,
    rate: i32,
    blended: bool,
    /// The channels the last frame moved (the motion step sets their dirty bytes: all of them while the blend runs, then
    /// the ones with frame values); the element pass writes only those into the elements.
    pub dirty: Vec<bool>,
    /// Model joints × 9 channels: rotation X/Y/Z, translation X/Y/Z, scale X/Y/Z.
    pub channels: Vec<ChannelState>,
    streams: Vec<usize>,
    last_event: i16,
}

/// The sequences' numbers: two bytes, the low one sign-extended into the high bits.
fn seq_value(d: &[u8], at: usize) -> i16 {
    let lo = i32::from(d.get(at).copied().unwrap_or(0) as i8);
    let hi = i32::from(d.get(at + 1).copied().unwrap_or(0) as i8);
    ((hi << 8) | lo) as i16
}

fn i16_at(d: &[u8], at: usize) -> i16 {
    d.get(at..at + 2).map_or(0, |b| i16::from_le_bytes([b[0], b[1]]))
}

/// A channel value in turns (rotations) or 1/4096 units.
fn unit(v: i16) -> f32 {
    f32::from(v) * (1.0 / 4096.0)
}

/// The interpolation share of a clock fraction (1/256).
fn share(frac: i32) -> f32 {
    frac as f32 * (1.0 / 256.0)
}

/// A rotation between two frame values the short way round within a turn.
fn angle_lerp(prev: i16, cur: i16, frac: i32) -> f32 {
    let mut a = unit((i32::from(prev) & 0xFFF) as i16);
    let b = unit((i32::from(cur) & 0xFFF) as i16);
    let t = f64::from(share(frac));
    let mut d = b - a;
    if d >= 0.5 {
        a += 1.0;
        d = b - a;
    }
    if d <= -0.5 {
        return (f64::from(a) + t * (f64::from(b + 1.0) - f64::from(a))) as f32;
    }
    (f64::from(a) + t * f64::from(d)) as f32
}

/// A rotation's next blended value: within a turn, a 1/n share of the shorter way towards the target.
fn angle_approach(current: f32, target: f32, n: i32) -> f32 {
    let frac = |x: f64| -> f32 {
        let f = (x - x.trunc()) as f32;
        if f < 0.0 { f + 1.0 } else { f }
    };
    let a = frac(f64::from(current));
    let e = frac(f64::from(f64::from(a) as f32 - target) as f32 as f64);
    if e == 0.0 {
        return a;
    }
    let n = f64::from(n);
    if e < 0.5 {
        (f64::from(a) - f64::from(e) / n) as f32
    } else {
        (f64::from(a) + (1.0 - f64::from(e)) / n) as f32
    }
}

impl Player {
    /// A player for a model with `joints` joints, as a new character starts: speed 0x100, nothing playing, and
    /// the first motion set blends in one frame.
    pub fn new(joints: usize) -> Player {
        Player {
            motion: 0,
            stop_flag: false,
            entry: None,
            motion_file: None,
            remap: None,
            touched: 0,
            pc: None,
            record: None,
            start: 0,
            end1: 0,
            len: 0,
            loops: 0,
            loops_next: -1,
            in_range: 0,
            wait: 0,
            stopped: false,
            playing: false,
            plays: 0,
            blend: 0,
            next_blend: 1,
            speed: 0x100,
            speed2: 0,
            step_size: 0,
            blend_count: 0,
            clock: 0,
            shown: 0,
            decoded: 0,
            rate: 0,
            blended: false,
            dirty: vec![false; joints * 9],
            channels: vec![ChannelState::default(); joints * 9],
            streams: Vec::new(),
            last_event: 0,
        }
    }

    /// Stop the motion: it no longer plays and the next set starts it again.
    pub fn stop(&mut self) {
        self.stop_flag = true;
        self.playing = false;
        self.in_range = 0;
    }

    /// Set a motion: point the sequence at its entry (the sequence starts it on the next frame). `chr` is the
    /// character's own id; a motion of another character's plays through the model's joint relation to that
    /// character (without one it does not play).
    pub fn set_motion(
        &mut self,
        groups: &Groups,
        chr: u16,
        relations: &[Arc<Relation>],
        id: i32,
    ) -> SetMotion {
        let owner = ((id >> 16) & 0xFFFF) as u16;
        self.remap = None;
        if owner != chr {
            self.remap = relations.iter().find(|r| r.owner == owner).cloned();
            if self.remap.is_none() {
                log::debug!("motion {id:#x} of character {owner:#x} set on {chr:#x}: no joint relation");
                self.playing = false;
                self.in_range = 1;
                return SetMotion::Missing;
            }
        }
        if self.motion == id && !self.stop_flag {
            return SetMotion::Same;
        }
        self.stop_flag = false;
        if chr & 0xF000 == 0 && ((self.motion ^ id) as u32 & 0xFFFF_0000) != 0 {
            self.next_blend = 1;
        }
        self.motion = id;
        let Some((file, group, entry)) = groups.find(id) else {
            log::debug!("motion {id:#x} is in no loaded group (files {:?})", groups.files());
            self.playing = false;
            self.in_range = 1;
            return SetMotion::Missing;
        };
        self.pc = Some(Pc::Data(group.entries[entry].1 as usize));
        self.entry = Some((group, entry));
        self.motion_file = Some(file);
        self.in_range = 0;
        self.wait = 0;
        self.loops_next = -1;
        self.plays = -1;
        self.playing = true;
        if self.next_blend == 0 {
            self.blend = 4;
        } else {
            self.blend = self.next_blend;
            self.next_blend = 0;
        }
        SetMotion::Set
    }

    /// The first motion record an entry's sequence plays.
    fn first_record(&self) -> Option<usize> {
        let (group, entry) = self.entry.as_ref()?;
        let d = &group.data;
        let mut at = group.entries[*entry].1 as usize;
        for _ in 0..64 {
            match *d.get(at)? {
                1 => return usize::try_from(seq_value(d, at + 1)).ok(),
                2 => at += 1,
                3 | 6 => at += 3,
                4 => {
                    let i = seq_value(d, at + 1);
                    let table = group.entry_jumps[*entry] as usize;
                    at = (group.entries[*entry].1 as i64 + i64::from(i16_at(d, table + 2 * i as usize)))
                        as usize;
                }
                _ => return None,
            }
        }
        None
    }

    /// Play a frame range of a motion (native 501a and its kin): `start` and `end` are frames (−1: the motion's
    /// end), or with `table` indices into the record's frame table; with `extend` the end takes one frame more
    /// unless `loops` is 1 or the end is not after frame 0.
    #[allow(clippy::too_many_arguments)]
    pub fn play_range(
        &mut self,
        groups: &Groups,
        chr: u16,
        relations: &[Arc<Relation>],
        id: i32,
        loops: i16,
        start: i32,
        end: i32,
        table: bool,
        extend: bool,
    ) -> bool {
        self.stop();
        if self.set_motion(groups, chr, relations, id) != SetMotion::Set {
            return false;
        }
        let Some(record) = self.first_record() else { return false };
        let Some((group, _)) = self.entry.clone() else { return false };
        let (mut start, mut end) = (start, end);
        if table {
            let frames = group.record_frames.get(record).copied().unwrap_or(0) as usize;
            start = i32::from(i16_at(&group.data, frames + 2 * start.max(0) as usize));
            end = i32::from(i16_at(&group.data, frames + 2 * end.max(0) as usize));
        }
        if extend && loops != 1 && end > 0 {
            end += 1;
        }
        self.start_range(&group, record, loops, start << 8, end << 8);
        self.playing = true;
        self.pc = Some(Pc::Range(0));
        true
    }

    /// Start a frame range (1/256 frames; a negative end is the motion's end).
    fn start_range(&mut self, group: &Arc<MotionGroup>, record: usize, loops: i16, start: i32, end: i32) {
        self.rate = 0x1E00;
        self.setup(group, record);
        self.start = start;
        let frames = group.motions.get(record).map_or(0, |m| i32::from(m.frames as i16));
        let end = if end < 0 { frames << 8 } else { end };
        self.len = end - start;
        self.end1 = end - 1;
        self.loops = if self.loops_next == -1 { loops } else { self.loops_next };
        self.loops_next = -1;
        self.speed2 = 0x100;
        self.clock = self.start;
        self.blend_count = if self.blend == 0 { 1 } else { self.blend };
        self.blended = false;
        self.in_range = 1;
    }

    /// Take a motion record: its rate and events, each channel's kind (values for still ones), the frame decoder
    /// at its first frame.
    fn setup(&mut self, group: &Arc<MotionGroup>, record: usize) {
        let Some(m) = group.motions.get(record) else { return };
        self.rate = i32::from(m.rate as i16);
        self.record = Some((group.clone(), record));
        let joints = (self.channels.len() / 9).min(m.joints as usize);
        self.touched = self.touched.max(joints);
        self.streams.clear();
        for i in 0..joints * 9 {
            let c = &mut self.channels[i];
            match m.channels.get(i) {
                Some(Channel::Zero) | None => c.target = 0.0,
                Some(Channel::One) => c.target = 1.0,
                Some(Channel::Constant(v)) => c.target = unit(*v),
                Some(Channel::Frames(_)) => {
                    c.rotation = i % 9 < 3;
                    c.cur = 0;
                    self.streams.push(i);
                }
            }
        }
        self.decoded = 0;
        self.decode(1);
        for &i in &self.streams {
            self.channels[i].prev = self.channels[i].cur;
        }
        self.decoded = 1;
    }

    /// Decode `n` more frames of the channels with frame values.
    fn decode(&mut self, n: i32) {
        let Some((group, record)) = &self.record else { return };
        let Some(m) = group.motions.get(*record) else { return };
        for &i in &self.streams {
            let Some(Channel::Frames(v)) = m.channels.get(i) else { continue };
            let c = &mut self.channels[i];
            for k in 0..n.max(0) {
                c.prev = c.cur;
                let at = (self.decoded + k).max(0) as usize;
                c.cur = v.get(at.min(v.len().saturating_sub(1))).copied().unwrap_or(0);
            }
        }
    }

    /// Decode from the first frame again, keeping each value as the previous one.
    fn rewind(&mut self) {
        let saved: Vec<i16> = self.streams.iter().map(|&i| self.channels[i].cur).collect();
        for &i in &self.streams {
            self.channels[i].cur = 0;
        }
        self.decoded = 0;
        self.decode(1);
        for (k, &i) in self.streams.iter().enumerate() {
            self.channels[i].prev = saved[k];
        }
        self.decoded = 1;
    }

    /// One frame of the sequence.
    fn run_sequence(&mut self) -> i32 {
        let (data, at, range) = match (&self.pc, &self.entry) {
            (Some(Pc::Range(at)), _) => (Arc::<[u8]>::from(&RANGE_SEQUENCE[..]), *at, true),
            (Some(Pc::Data(at)), Some((g, _))) => (g.data.clone(), *at, false),
            _ => return -1,
        };
        let set_pc =
            |p: &mut Player, at: usize| p.pc = Some(if range { Pc::Range(at) } else { Pc::Data(at) });
        match data.get(at).copied().unwrap_or(0xFF) as i8 {
            0 => 2,
            1 => {
                let Some((group, _)) = self.entry.clone() else { return -1 };
                let record = seq_value(&data, at + 1);
                let frames = group.record_frames.get(record.max(0) as usize).copied().unwrap_or(0) as usize;
                let start = i32::from(i16_at(&data, frames + 2 * seq_value(&data, at + 5) as isize as usize));
                let to = seq_value(&data, at + 7);
                let mut end = i32::from(i16_at(&data, frames + 2 * to as isize as usize));
                let loops = seq_value(&data, at + 3);
                if to != 1 && loops != 1 {
                    end += 1;
                }
                self.start_range(&group, record.max(0) as usize, loops, start << 8, end << 8);
                set_pc(self, at + 9);
                0
            }
            2 => {
                if self.in_range == 0 {
                    set_pc(self, at + 1);
                    0
                } else {
                    1
                }
            }
            3 => {
                if self.wait == 0 {
                    self.wait = seq_value(&data, at + 1);
                }
                self.wait = self.wait.wrapping_sub(1);
                if self.wait == 0 {
                    set_pc(self, at + 3);
                    0
                } else {
                    1
                }
            }
            4 => {
                let Some((group, entry)) = &self.entry else { return -1 };
                let i = seq_value(&data, at + 1);
                let table = group.entry_jumps[*entry] as usize;
                let base = i64::from(group.entries[*entry].1);
                let to = base + i64::from(i16_at(&group.data, table.wrapping_add(2 * i as isize as usize)));
                self.pc = Some(Pc::Data(to.max(0) as usize));
                0
            }
            5 => 3,
            6 => {
                if seq_value(&data, at + 1) <= i16::from(self.plays) {
                    set_pc(self, at + 3);
                    0
                } else {
                    1
                }
            }
            _ => {
                log::warn!("motion {:#x}: unknown sequence code at {at:#x}", self.motion);
                -1
            }
        }
    }

    /// The sequence for this frame: run it to its wait, then keep the range going.
    fn sequence_frame(&mut self) {
        let mut r = 0;
        for _ in 0..256 {
            r = self.run_sequence();
            if r != 0 {
                break;
            }
        }
        match r {
            1 => {
                self.range_frame();
            }
            2 => {
                self.range_frame();
                if self.in_range == 0 {
                    self.playing = false;
                }
            }
            3 => {
                self.stopped = true;
                self.playing = false;
            }
            _ => {}
        }
    }

    /// Loop the clock within the range, or end the range on its last play.
    fn range_frame(&mut self) -> i32 {
        if self.clock < 0 {
            self.clock = 0;
        }
        let end1 = self.end1;
        if ((end1 + 1) & !0xFF) < ((i32::from(self.step_size) + 0x1FF + self.clock) & !0xFF)
            && self.loops == 1
        {
            self.clock = end1 & !0xFF;
            self.in_range = 0;
            return 1;
        }
        loop {
            if self.clock <= end1 {
                return 0;
            }
            if self.loops == 0 {
                if self.len == 0 {
                    self.clock = self.start;
                    return 1;
                }
            } else {
                self.loops -= 1;
                if self.loops == 0 {
                    self.clock = end1 & !0xFF;
                    self.in_range = 0;
                    self.speed2 = 0;
                    return 1;
                }
            }
            self.clock -= self.len;
        }
    }

    /// The channel targets at the clock: decoded up to the next frame, interpolated by the fraction.
    fn interpolate(&mut self) {
        let clock = self.clock;
        self.shown = clock;
        let mut want = (clock + 0x1FF) >> 8;
        let last = (self.end1 + 1) >> 8;
        if last < want {
            if self.loops == 1 {
                want = last;
            } else {
                want -= self.len >> 8;
            }
        }
        let mut more = want - self.decoded;
        if more < 0 {
            self.rewind();
            more = want - 1;
        }
        if self.decoded < want {
            self.decode(more);
            self.decoded += more;
        }
        let frac = clock & 0xFF;
        for &i in &self.streams {
            let c = &mut self.channels[i];
            c.target = if frac == 0 {
                unit(c.cur)
            } else if c.rotation {
                angle_lerp(c.prev, c.cur, frac)
            } else {
                let (a, b) = (unit(c.prev), unit(c.cur));
                (f64::from(share(frac)) * (f64::from(b) - f64::from(a)) + f64::from(a)) as f32
            };
        }
    }

    /// Move the joints a 1/n share of the way to the targets (all channels while a blend runs, then only those
    /// with frame values).
    fn blend_to_targets(&mut self, n: i16) {
        let n = i32::from(n.max(1));
        let towards = |c: &mut ChannelState| {
            if c.rotation {
                c.current = angle_approach(c.current, c.target, n);
            } else if c.target - c.current != 0.0 {
                c.current = ((f64::from(c.target) - f64::from(c.current)) / f64::from(n)
                    + f64::from(c.current)) as f32;
            }
        };
        self.dirty.resize(self.channels.len(), false);
        if !self.blended {
            for (i, c) in self.channels.iter_mut().enumerate() {
                c.rotation = i % 9 < 3;
                towards(c);
            }
            self.dirty.iter_mut().for_each(|d| *d = true);
            if n == 1 {
                self.blended = true;
            }
        } else {
            for &i in &self.streams {
                towards(&mut self.channels[i]);
                if let Some(d) = self.dirty.get_mut(i) {
                    *d = true;
                }
            }
        }
    }

    /// One frame: the sequence, the pose at the clock, then the clock moves on. `alt_clock` is the frame-time
    /// scale (0x100 normal) when the alternative clock (native 4022) is on.
    /// The motion clock (the body's +0x740: 1/256 frames, from the range start).
    pub fn clock(&self) -> i32 {
        self.clock
    }

    pub fn frame(&mut self, alt_clock: Option<i32>) {
        self.dirty.iter_mut().for_each(|d| *d = false);
        if self.pc.is_none() {
            return;
        }
        let mut step = ((i32::from(self.speed2) * i32::from(self.speed)) >> 8) * self.rate / 0x1E00;
        if let Some(scale) = alt_clock {
            step = ((step as u32).wrapping_mul(scale as u32) >> 8) as i32;
        }
        self.step_size = step as i16;
        self.sequence_frame();
        self.interpolate();
        self.blend_to_targets(self.blend_count);
        if self.blend_count > 1 {
            self.blend_count -= 1;
            if self.blend_count < 1 {
                self.blend_count = 1;
            }
        }
        self.clock = self.clock.wrapping_add(step);
    }

    /// The motion's events the shown frame reached since the last call.
    pub fn events(&mut self, out: &mut Vec<MotionEvent>) {
        let Some((group, record)) = &self.record else { return };
        let Some(m) = group.motions.get(*record) else { return };
        if m.events.is_empty() {
            return;
        }
        let frame = self.shown >> 8;
        let last = i32::from(self.last_event);
        if last < frame {
            for e in &m.events {
                if last < i32::from(e.end) && i32::from(e.start) <= frame {
                    out.push(*e);
                }
            }
        }
        self.last_event = ((self.shown as u32) >> 8) as i16;
    }

    /// The player's state in the oracle's log form (for comparisons).
    pub fn trace(&self) -> String {
        let j1: Vec<String> =
            (0..9).map(|k| format!("{:.4}", self.channels.get(9 + k).map_or(0.0, |c| c.current))).collect();
        format!(
            "m{:x} r{:x}/{:x}/{:x} l{} in{} p{} n{} b{} v{:x}/{:x} st{:x} a{:x} s{:x} rate{:x} j1[{}]",
            self.motion,
            self.start,
            self.end1,
            self.len,
            self.loops,
            self.in_range,
            u8::from(self.playing),
            self.plays,
            self.blend_count,
            self.speed,
            self.speed2,
            self.step_size,
            self.clock,
            self.shown,
            self.rate,
            j1.join(",")
        )
    }

    /// The frames the sequence has left from where it is: each play of a range adds its length times its
    /// plays, each wait its frames; −1 without a sequence, for a range record that is not there or plays 0
    /// times, or an unknown code.
    pub fn sequence_length(&self) -> i32 {
        let (data, mut at) = match (&self.pc, &self.entry) {
            (Some(Pc::Range(at)), _) => (Arc::<[u8]>::from(&RANGE_SEQUENCE[..]), *at),
            (Some(Pc::Data(at)), Some((g, _))) => (g.data.clone(), *at),
            _ => return -1,
        };
        let mut total = 0i32;
        // The original walks until the sequence ends (a sequence that jumps back for ever would hang it).
        for _ in 0..0x10000 {
            match data.get(at).copied().unwrap_or(0xFF) as i8 {
                0 | 5 => return total,
                1 => {
                    let Some((group, _)) = &self.entry else { return -1 };
                    let record = seq_value(&data, at + 1);
                    let Some(&frames) = usize::try_from(record).ok().and_then(|r| group.record_frames.get(r))
                    else {
                        return -1;
                    };
                    let plays = i32::from(seq_value(&data, at + 3));
                    if plays == 0 {
                        return -1;
                    }
                    let frame = |k: i16| {
                        i32::from(i16_at(&data, (frames as usize).wrapping_add(2 * k as isize as usize)))
                    };
                    let (to, from) = (frame(seq_value(&data, at + 7)), frame(seq_value(&data, at + 5)));
                    total = total.wrapping_add((to - from).wrapping_mul(plays));
                    at += 9;
                }
                2 => at += 1,
                3 => {
                    total = total.wrapping_add(i32::from(seq_value(&data, at + 1)));
                    at += 3;
                }
                6 => at += 3,
                4 => {
                    let Some((group, entry)) = &self.entry else { return -1 };
                    let i = seq_value(&data, at + 1);
                    let table = group.entry_jumps[*entry] as usize;
                    let base = i64::from(group.entries[*entry].1);
                    at = (base + i64::from(i16_at(&group.data, table.wrapping_add(2 * i as isize as usize))))
                        .max(0) as usize;
                }
                _ => {
                    log::warn!("motion {:#x}: unknown sequence code at {at:#x}", self.motion);
                    return -1;
                }
            }
        }
        total
    }

    /// The pose with each channel's dirty mark, for the model joints (through the relation, as pose).
    pub fn pose_dirty(&self) -> Vec<Option<([f32; 9], [bool; 9])>> {
        let joints = self.channels.len() / 9;
        let mut out = vec![None; joints];
        for c in 0..self.touched.min(joints) {
            let j = match &self.remap {
                Some(r) => match r.map.get(c) {
                    Some(&0xFFFF) => continue,
                    Some(&j) => usize::from(j),
                    None => break,
                },
                None => c,
            };
            if let Some(slot) = out.get_mut(j) {
                let v: [f32; 9] = std::array::from_fn(|k| self.channels[c * 9 + k].current);
                let d: [bool; 9] =
                    std::array::from_fn(|k| self.dirty.get(c * 9 + k).copied().unwrap_or(false));
                *slot = Some((v, d));
            }
        }
        out
    }

    /// The joint relation the motion plays through (the character's +0x758), if any.
    pub fn relation(&self) -> Option<&Relation> {
        self.remap.as_deref()
    }

    /// Blend all channels again from the next frame (a script set the blend length).
    pub fn unblend(&mut self) {
        self.blended = false;
    }

    /// The pose: for each model joint a motion has set, its nine current values (rotation X/Y/Z in turns,
    /// translation X/Y/Z and scale X/Y/Z in 1/4096 units). A motion of another character's sets the joints its
    /// relation maps to.
    pub fn pose(&self) -> Vec<Option<[f32; 9]>> {
        let joints = self.channels.len() / 9;
        let mut out = vec![None; joints];
        for c in 0..self.touched.min(joints) {
            let j = match &self.remap {
                Some(r) => match r.map.get(c) {
                    Some(&0xFFFF) => continue,
                    Some(&j) => usize::from(j),
                    None => break,
                },
                None => c,
            };
            if let Some(slot) = out.get_mut(j) {
                let v: [f32; 9] = std::array::from_fn(|k| self.channels[c * 9 + k].current);
                *slot = Some(v);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_numbers_sign_extend_the_low_byte() {
        assert_eq!(seq_value(&[0x05, 0x01], 0), 0x105);
        assert_eq!(seq_value(&[0x80, 0x01], 0), -128);
        assert_eq!(seq_value(&[0xFF, 0xFF], 0), -1);
    }

    #[test]
    fn rotations_take_the_short_way() {
        // 0.9 turn to 0.1 turn: half way is 1.0 (a whole turn).
        assert!((angle_lerp(3686, 410, 128) - 1.0).abs() < 1e-3);
        // Approach 0.1 from 0.9 over 2 frames: 1.0.
        assert!((angle_approach(0.9, 0.1, 2) - 1.0).abs() < 1e-6);
        assert!((angle_approach(0.2, 0.1, 2) - 0.15).abs() < 1e-6);
        assert_eq!(angle_approach(0.25, 0.25, 4), 0.25);
    }

    /// Tidus's idle (resident0): the clock runs at the motion's rate, loops for ever, and the blend reaches the
    /// targets after four frames.
    #[test]
    fn plays_tidus_idle() {
        let Some(inst) = aerugo_vfs::install_from_env() else {
            eprintln!("skipped: set AERUGO_GAME_DIR");
            return;
        };
        let mut vfs = aerugo_vfs::Vfs::new();
        vfs.mount_install(&inst).unwrap();
        let d = vfs.read("ffx_ps2/ffx/master/jppc/chr/pc/c001/mot/resident0.mgrp").unwrap();
        let mut groups = Groups::default();
        groups.add((0, 0, 1), MotionGroup::parse_all(&d).unwrap());
        let mut p = Player::new(136);
        assert_eq!(p.set_motion(&groups, 1, &[], 0x11040), SetMotion::Set);
        assert_eq!(p.set_motion(&groups, 1, &[], 0x11040), SetMotion::Same);
        for _ in 0..8 {
            p.frame(None);
        }
        assert!(p.playing && p.in_range == 1 && p.loops == 0);
        assert_eq!(p.rate, 0x1E00);
        // Eight frames: the first one's step is taken before its sequence starts the motion (no rate yet), so
        // the clock showed 6.0 last.
        assert_eq!(p.shown, 6 << 8);
        assert!(p.blended);
        for &i in &p.streams {
            let c = p.channels[i];
            if c.rotation {
                // Rotations settle on the target within a turn.
                let d = (c.current - c.target).rem_euclid(1.0);
                assert!(!(1e-5..=1.0 - 1e-5).contains(&d), "channel {i}: {} vs {}", c.current, c.target);
            } else {
                assert_eq!(c.current.to_bits(), c.target.to_bits(), "channel {i}");
            }
        }
        let frames = i32::from(p.record.as_ref().map(|(g, r)| g.motions[*r].frames).unwrap());
        for _ in 0..frames * 2 {
            p.frame(None);
        }
        assert!(p.shown >> 8 < frames, "loops within the motion");
    }
}
