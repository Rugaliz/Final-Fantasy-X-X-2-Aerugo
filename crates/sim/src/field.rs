//! A field event: the event's script running on the script machine, with the state its natives act on (actors,
//! message windows, camera, player control). The renderer only reads this state.

use std::sync::Arc;

use aerugo_formats::Game;
use aerugo_formats::script::Script;
use aerugo_formats::text::{self, Charset};
use aerugo_script::natives::{Native, World, wrap_angle};

use crate::trigger::{self, PlayerStep, Talk, Talker, Trigger};
use aerugo_script::vm::{NativeCall, Value, WAIT_DONE};
use aerugo_script::{Natives, Phase, RequestMode, Vm};
use aerugo_vfs::{Language, Vfs};

pub mod collision;

use collision::Ground;

/// A worker's body in the world.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    /// Model id (`type << 12 | number`): 0 party, 1 monster, 2 npc, 3 summon, 4 weapon, 5 object.
    pub model: Option<u16>,
    /// Position in the map's space (Y points down).
    pub pos: [f32; 3],
    pub yaw: f32,
    pub visible: bool,
    /// Motion id requested by the script (played by the renderer when it has the motion).
    pub motion: Option<i32>,
    /// Four values scripts keep per worker.
    pub params: [i32; 4],
    /// The party character this worker plays (set by scripts), which party requests resolve to.
    pub character: Option<u8>,
    /// Signal bits scripts set and clear per worker: an engine signal of type n reaches the worker only while
    /// bit n is set.
    pub flags: u32,
    pub trigger: Trigger,
    move_target: [f32; 3],
    move_speed: [f32; 2],
    moving: Option<Movement>,
    turn_target: f32,
    turn_speed: [f32; 2],
    turning: bool,
    /// A turn over a set number of frames: the step per frame and the frames left.
    turn_steps: Option<(f32, u32)>,
}

impl Default for Actor {
    fn default() -> Actor {
        Actor {
            model: None,
            pos: [0.0; 3],
            yaw: 0.0,
            visible: true,
            motion: None,
            params: [0; 4],
            character: None,
            flags: 0,
            trigger: Trigger::default(),
            move_target: [0.0; 3],
            move_speed: [0.0; 2],
            moving: None,
            turn_target: 0.0,
            turn_speed: [0.0; 2],
            turning: false,
            turn_steps: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Movement {
    from: [f32; 3],
    to: [f32; 3],
    frames: u32,
    done: u32,
}

impl Actor {
    pub fn is_moving(&self) -> bool {
        self.moving.is_some()
    }

    /// The model's archive path.
    pub fn model_path(&self, game: Game) -> Option<String> {
        model_path(game, self.model?)
    }
}

/// The archive path of a character model id.
pub fn model_path(game: Game, id: u16) -> Option<String> {
    let (dir, prefix) = match id >> 12 {
        0 => ("pc", 'c'),
        1 => ("mon", 'm'),
        2 => ("npc", 'n'),
        3 => ("sum", 's'),
        4 => ("wep", 'w'),
        5 => ("obj", 'f'),
        _ => return None,
    };
    let root = match game {
        Game::Ffx => "ffx_data",
        Game::Ffx2 => "ffx-2_data",
    };
    let n = id & 0xFFF;
    Some(format!("{root}/gamedata/ps3data/chr/{dir}/{prefix}{n:03}/mdl/d3d11/{prefix}{n:03}.dae.phyre"))
}

/// One of the eight message windows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    pub message: Option<usize>,
    /// Screen position in the original's 512 × 448 frame.
    pub x: i32,
    pub y: i32,
    pub open: bool,
    /// Characters revealed so far (text appears over a few ticks).
    pub shown: usize,
    /// Highlighted choice for messages that offer choices.
    pub choice: usize,
    /// Ticks left of the opening animation (text appears once it is over).
    pub opening: u8,
    /// Ticks left of the closing animation (a message wait ends once it is over).
    pub closing: u8,
    /// How many characters the text reveal counts for the open message (names spelled out).
    pub reveal: usize,
    /// Where the reveal stops until confirm is pressed, in the same count.
    pub stops: Vec<usize>,
    /// Flags a script set for the window; 0x1000 shows the text at once.
    pub flags: u16,
}

/// Ticks a window takes to open (the original reports "opening" for the frame it opens and the next).
const OPEN_TICKS: u8 = 2;

/// Ticks a window takes to close (a wait on it polls as not done on the frame it closes and the next).
const CLOSE_TICKS: u8 = 2;

/// Characters revealed per tick.
const TEXT_SPEED: usize = 2;

/// Window flag: the text appears at once instead of a few characters per tick.
const WINDOW_INSTANT: u16 = 0x1000;

/// Character names in the save image: one 0x14-byte slot per character, NUL-terminated.
const NAMES: usize = 0x634C;
const NAME_SLOT: usize = 0x14;

/// Characters in a character's name as the save image spells it.
fn name_chars(image: &[u8], chr: u8) -> usize {
    let at = NAMES + usize::from(chr) * NAME_SLOT;
    let slot = image.get(at..at + NAME_SLOT).unwrap_or_default();
    // Page leads and the extension prefix start two-byte characters.
    slot.iter().take_while(|&&b| b != 0).filter(|&&b| !matches!(b, 0x04 | 0x2C..=0x2F)).count()
}

/// One event message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    /// What the player reads: characters and line breaks (control codes left out).
    pub text: String,
    /// With control codes shown as `{xx}` / `{xx:yy}`.
    pub raw: String,
    /// How many of the last lines are choices.
    pub choices: u8,
    /// What the text reveal goes through: everything but line breaks, after a first line that is only control
    /// codes (the speaker's name, shown at once).
    pub reveal: Vec<RevealPart>,
}

/// A piece of a message's text reveal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevealPart {
    Chars(usize),
    /// A character's name, counted by its length in the save image.
    Name(u8),
    /// The reveal waits here for confirm.
    Pause,
}

impl Message {
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

impl Window {
    /// The window's state as scripts see it: 0 closed, 1 opening, 2 showing.
    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            self.closing = CLOSE_TICKS;
        }
    }

    pub fn state(&self) -> i32 {
        match (self.open, self.opening) {
            (false, _) => 0,
            (true, 0) => 2,
            (true, _) => 1,
        }
    }
}

/// The camera the scripts set (map space).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CameraState {
    pub position: Option<[f32; 3]>,
    pub focus: Option<[f32; 3]>,
    pub roll: f32,
    /// Distance from the eye to the screen plane in the original's 512 × 448 frame (sets the field of view);
    /// zero until a script sets it.
    pub screen_distance: f32,
    destination: Option<[f32; 3]>,
    move_frames: u32,
    moving: Option<Movement>,
    /// Ticks left of the timed moves of the camera's parts (bits of a part mask); a camera wait lasts until
    /// all have run out.
    timers: [u32; 8],
    /// Parts the next move time applies to (all three of the usual ones until a script picks).
    parts: Option<u8>,
    /// Ticks left of the moves of the eye and the focus along a path.
    path_timers: [u32; 2],
    /// A timed move whose destination may still be set this tick.
    pending: bool,
}

impl CameraState {
    /// Vertical field of view from the screen distance (the frame is 448 high).
    pub fn fov_y(&self) -> f32 {
        let d = if self.screen_distance > 1.0 { self.screen_distance } else { 400.0 };
        2.0 * (224.0 / d).atan()
    }

    pub fn is_moving(&self) -> bool {
        self.moving.is_some() || self.timers.iter().chain(&self.path_timers).any(|&t| t > 0)
    }

    fn update(&mut self) {
        if std::mem::take(&mut self.pending) && self.destination.is_some() {
            self.start_move();
        }
        for t in self.timers.iter_mut().chain(&mut self.path_timers) {
            *t = t.saturating_sub(1);
        }
        if let Some(m) = &mut self.moving {
            m.done += 1;
            let t = (m.done as f32 / m.frames.max(1) as f32).min(1.0);
            self.position = Some(std::array::from_fn(|k| m.from[k] + (m.to[k] - m.from[k]) * t));
            if m.done >= m.frames {
                self.moving = None;
            }
        }
    }

    /// Start moving to the destination over the set number of frames.
    fn start_move(&mut self) {
        if let (Some(to), Some(from)) = (self.destination, self.position.or(self.destination)) {
            self.moving = Some(Movement { from, to, frames: self.move_frames.max(1), done: 0 });
        }
    }
}

/// Player input for one tick.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Input {
    /// Left stick or d-pad, −1..1 (x right, y forward).
    pub stick: [f32; 2],
    /// Confirm pressed this tick (advances messages, talks).
    pub confirm: bool,
    pub cancel: bool,
    /// The second talk button pressed this tick (square on a PlayStation pad): talks with another signal type.
    pub alt: bool,
    /// The stick moved from rest this tick (for menus and choices).
    pub stick_pressed: bool,
}

/// The script random number stream: a multiply / xor generator with a 16-bit rotate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rng {
    pub state: u32,
}

impl Rng {
    const MUL: u32 = 0x0EBB_6BD7;
    const XOR: u32 = 0x2BA9;

    pub fn next_value(&mut self) -> u32 {
        let s = Self::MUL.wrapping_mul(self.state) ^ Self::XOR;
        let s = (((s as i32) >> 16) as u32).wrapping_add(s << 16);
        self.state = s;
        s & 0x7FFF_FFFF
    }
}

/// Everything natives act on.
#[derive(Debug, Clone, Default)]
pub struct FieldState {
    pub actors: Vec<Actor>,
    pub windows: [Window; 8],
    pub camera: CameraState,
    /// The event's messages, decoded.
    pub messages: Vec<Message>,
    pub input: Input,
    /// Worker the player controls.
    pub player: Option<usize>,
    pub ground: Option<Ground>,
    /// A map change the scripts asked for: target and entrance, for the session to carry out.
    pub jump: Option<(i32, i32)>,
    pub rng: Rng,
    /// Character ids in the three party slots (from the save image, refreshed every tick).
    pub party: [u8; 3],
    /// Random values to hand out instead of the generator's (replaying a recording of the original).
    pub rng_replay: std::collections::VecDeque<u32>,
    /// Where the player stood at the start of the last tick and of this one (what triggers test the step
    /// between).
    pub player_prev: [f32; 3],
    player_start: Option<[f32; 3]>,
    /// The player has control (scripts turn it on and off); talking needs it.
    pub user_control: bool,
    /// Added to every talk radius (a script sets it; 0 by default).
    pub talk_reach: f32,
    /// The talk started this tick, while workers offer themselves.
    pub talk: Option<Talk>,
}

impl FieldState {
    /// The next script random number: a replayed one, else the generator's.
    pub fn random(&mut self) -> u32 {
        self.rng_replay.pop_front().unwrap_or_else(|| self.rng.next_value())
    }
}

impl World for FieldState {
    fn worker_ran(&mut self, w: usize, kind: u8) -> Vec<u8> {
        let Some(p) = self.player.filter(|&p| p < self.actors.len()) else { return Vec::new() };
        let step = PlayerStep { prev: self.player_prev, cur: self.actors[p].pos };
        let Some(a) = self.actors.get_mut(w).filter(|a| a.trigger.active) else { return Vec::new() };
        let mut out = Vec::new();
        match kind {
            1 if a.model.is_some() => trigger::test_character(&mut a.trigger, a.pos, step, &mut out),
            2 => trigger::test_line_zone(&mut a.trigger, a.pos, step, &mut out),
            3 => trigger::test_box(&mut a.trigger, a.pos, a.yaw, step, &mut out),
            5 => trigger::test_line(&mut a.trigger, a.pos, step, &mut out),
            _ => {}
        }
        let bits = a.flags;
        out.retain(|&t| bits & (1 << t) != 0);
        if let Some(talk) = self.talk.as_mut().filter(|_| bits & 1 != 0 && w != p) {
            let talker = Talker { pos: step.cur, yaw: self.actors[p].yaw, reach: self.talk_reach };
            let a = &self.actors[w];
            match kind {
                1 if a.model.is_some() => trigger::talk_character(&a.trigger, w, a.pos, &talker, talk),
                2 => trigger::talk_line_zone(&a.trigger, w, a.pos, &talker, talk),
                3 => trigger::talk_box(&a.trigger, w, a.pos, a.yaw, &talker, talk),
                _ => {}
            }
        }
        out
    }

    fn talk(&mut self) -> Option<(usize, u8)> {
        let talk = self.talk.take()?;
        let (w, _) = talk.best.filter(|_| self.user_control)?;
        Some((w, talk.signal))
    }

    fn party_worker(&mut self, slot: i32) -> Option<usize> {
        // The party's three slots hold character ids (0xFF empty); the worker playing that character answers.
        let chr = *self.party.get(usize::try_from(slot).ok()?)?;
        if chr == 0xFF {
            return None;
        }
        self.actors.iter().position(|a| a.character == Some(chr))
    }
}

impl FieldState {
    /// Start a talk when a talk button is pressed while the player has control and no message window is open.
    fn begin_talk(&mut self) {
        let pressed = self.input.confirm || self.input.alt;
        self.talk = (pressed && self.user_control && !self.windows.iter().any(|w| w.open))
            .then(|| Talk::new(if self.input.confirm { trigger::TALK } else { trigger::TALK_ALT }));
    }

    fn actor(&mut self, w: usize) -> &mut Actor {
        if self.actors.len() <= w {
            self.actors.resize(w + 1, Actor::default());
        }
        &mut self.actors[w]
    }

    /// Advance movements and turns by one tick, then player control.
    fn update(&mut self) {
        self.camera.update();
        for w in &mut self.windows {
            w.closing = w.closing.saturating_sub(1);
        }
        for w in self.windows.iter_mut().filter(|w| w.open) {
            if w.opening > 0 {
                w.opening -= 1;
                continue;
            }
            // Up to the next pause; confirm there lets the reveal go on from the next tick.
            let stop = w.stops.first().copied().unwrap_or(w.reveal).min(w.reveal);
            if w.shown >= stop && stop < w.reveal {
                if self.input.confirm {
                    w.stops.remove(0);
                }
                continue;
            }
            w.shown = if w.flags & WINDOW_INSTANT != 0 { stop } else { (w.shown + TEXT_SPEED).min(stop) };
        }
        for a in &mut self.actors {
            if let Some(m) = &mut a.moving {
                m.done += 1;
                let t = (m.done as f32 / m.frames.max(1) as f32).min(1.0);
                for k in 0..3 {
                    a.pos[k] = m.from[k] + (m.to[k] - m.from[k]) * t;
                }
                // The original reports the move as going on for one tick at the destination.
                if m.done > m.frames {
                    a.moving = None;
                }
            }
            if a.turning
                && let Some((step, left)) = &mut a.turn_steps
            {
                a.yaw = wrap_angle(a.yaw + *step);
                *left = left.saturating_sub(1);
                if *left == 0 {
                    a.yaw = a.turn_target;
                    a.turning = false;
                    a.turn_steps = None;
                }
            } else if a.turning {
                let step = if a.turn_speed[0] > 0.0 { a.turn_speed[0] } else { 0.2 };
                let d = wrap_angle(a.turn_target - a.yaw);
                if d.abs() <= step {
                    a.yaw = a.turn_target;
                    a.turning = false;
                } else {
                    a.yaw = wrap_angle(a.yaw + step.copysign(d));
                }
            }
        }
        if let Some(p) = self.player {
            let stick = self.input.stick;
            if stick[0] != 0.0 || stick[1] != 0.0 {
                let ground = self.ground.clone();
                let a = self.actor(p);
                // Screen-relative movement on the ground plane (the field camera looks along +Z).
                let speed = 2.4;
                let next = [a.pos[0] + stick[0] * speed, a.pos[1], a.pos[2] + stick[1] * speed];
                let next = match &ground {
                    Some(g) => g.slide(a.pos, next),
                    None => Some(next),
                };
                if let Some(n) = next {
                    a.pos = n;
                    a.yaw = stick[0].atan2(stick[1]);
                }
            }
        }
    }
}

/// Offset of the three party slots in the save image.
const PARTY_SLOTS: usize = 0x3D58;

/// A running field event.
pub struct Event {
    pub game: Game,
    pub name: String,
    pub path: String,
    pub vm: Vm,
    pub natives: Natives<FieldState>,
    pub state: FieldState,
    /// The map model the event most likely belongs to, and its pre-rendered layers.
    pub map_model: Option<String>,
    pub map_layers: Option<String>,
    pub tick: u64,
}

impl std::fmt::Debug for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Event").field("name", &self.name).field("tick", &self.tick).finish_non_exhaustive()
    }
}

/// The text file of an event in a language (`new_<xx>pc/event/obj_ps3/...`).
pub fn event_text_path(game: Game, ebp_path: &str, lang: Language) -> Option<String> {
    let (_, rest) = ebp_path.split_once("/event/obj/")?;
    let rest = rest.strip_suffix(".ebp")?;
    let root = match game {
        Game::Ffx => "ffx_ps2/ffx/master",
        Game::Ffx2 => "ffx_ps2/ffx2/master",
    };
    Some(format!("{root}/new_{}pc/event/obj_ps3/{rest}.bin", lang.master_code()))
}

/// Decode an event text table: 8-byte entries (u16 offset, u16 flags with the choice count in the high byte,
/// twice), strings after the table.
pub fn decode_event_text(data: &[u8], charset: &Charset) -> Vec<Message> {
    let Some(first) = data.get(0..2).map(|b| usize::from(u16::from_le_bytes([b[0], b[1]]))) else {
        return Vec::new();
    };
    (0..first / 8)
        .map(|i| {
            let word = |k: usize| data.get(k..k + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            let off = usize::from(word(i * 8));
            let tokens = text::string_at(data, off).map(|s| text::decode(s, charset)).unwrap_or_default();
            let text = tokens
                .iter()
                .filter_map(|t| match *t {
                    text::Token::Char(c) => Some(c),
                    text::Token::LineBreak => Some('\n'),
                    _ => None,
                })
                .collect();
            let reveal = reveal_parts(&tokens);
            Message { text, raw: text::to_display(&tokens), choices: (word(i * 8 + 2) >> 8) as u8, reveal }
        })
        .collect()
}

/// Name code: the character's name (argument 0x30 + character id).
const NAME_CODE: u8 = 0x13;

/// Pause code: the text waits for confirm before going on.
const PAUSE_CODE: u8 = 0x01;

/// What the text reveal goes through in a message: characters, names spelled out, and pauses.
fn reveal_parts(tokens: &[text::Token]) -> Vec<RevealPart> {
    let first_break = tokens.iter().position(|t| matches!(t, text::Token::LineBreak));
    let speaker_line =
        first_break.filter(|&n| !tokens[..n].iter().any(|t| matches!(t, text::Token::Char(_))));
    let body = speaker_line.map_or(tokens, |n| &tokens[n + 1..]);
    let mut parts = Vec::new();
    for t in body {
        match *t {
            text::Token::Char(_) | text::Token::Unmapped(_) => match parts.last_mut() {
                Some(RevealPart::Chars(n)) => *n += 1,
                _ => parts.push(RevealPart::Chars(1)),
            },
            text::Token::Control { code: NAME_CODE, arg: Some(a) } if a >= 0x30 => {
                parts.push(RevealPart::Name(a - 0x30))
            }
            text::Token::Control { code: PAUSE_CODE, arg: None } => parts.push(RevealPart::Pause),
            _ => {}
        }
    }
    parts
}

/// The map the event most likely uses: area from the first four letters, map from the first six
/// (`bjyt0000` → `bjyt/bjyt00`).
pub fn map_paths(vfs: &Vfs, game: Game, event: &str) -> (Option<String>, Option<String>) {
    if event.len() < 6 || !event.is_ascii() {
        return (None, None);
    }
    let (area, id) = (&event[..4], &event[..6]);
    let root = match game {
        Game::Ffx => "ffx_data",
        Game::Ffx2 => "ffx-2_data",
    };
    let model = format!("{root}/gamedata/ps3data/map/{area}/{id}/mdl/d3d11/{id}.dae.phyre");
    let layers = format!("{root}/gamedata/ps3data/map/{area}/{id}/2d/mdl/d3d11/{id}.dae.phyre");
    let exists = |p: String| vfs.size(&p).is_some().then_some(p);
    (exists(model), exists(layers))
}

impl Event {
    /// Load an event script (`.ebp`), its text in `lang`, and set up the machine.
    pub fn load(vfs: &Vfs, game: Game, path: &str, lang: Language) -> Result<Event, String> {
        let data = vfs.read(path).map_err(|e| e.to_string())?;
        let script = Script::parse(&data).map_err(|e| e.to_string())?;
        let name = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".ebp").to_owned();
        let actors = script
            .workers
            .iter()
            .map(|w| Actor {
                flags: trigger::default_signal_bits(w.kind),
                trigger: Trigger::for_kind(w.kind),
                ..Actor::default()
            })
            .collect();
        let mut state = FieldState { actors, ..FieldState::default() };
        if let Some(tp) = event_text_path(game, path, lang)
            && let Ok(bytes) = vfs.read(&tp)
            && let Ok(cs) = vfs.read(&text::charset_path(game, lang)).map(|b| Charset::parse(&b))
            && let Ok(cs) = cs
        {
            state.messages = decode_event_text(&bytes, &cs);
        }
        let (map_model, map_layers) = map_paths(vfs, game, &name);
        let mut natives = Natives::new(game);
        register(&mut natives);
        Ok(Event {
            game,
            name,
            path: path.to_owned(),
            vm: {
                let mut vm = Vm::new(Arc::new(script));
                vm.enter(0);
                vm
            },
            natives,
            state,
            map_model,
            map_layers,
            tick: 0,
        })
    }

    /// One 30 Hz tick: scripts first, then movement and player control.
    pub fn step(&mut self, input: Input) {
        self.state.input = input;
        let image = &self.vm.memory.global[0];
        if let Some(p) = image.get(PARTY_SLOTS..PARTY_SLOTS + 3) {
            self.state.party = [p[0], p[1], p[2]];
        }
        if let Some(pos) = self.state.player.and_then(|p| self.state.actors.get(p)).map(|a| a.pos) {
            self.state.player_prev = self.state.player_start.replace(pos).unwrap_or(pos);
        }
        self.state.begin_talk();
        self.vm.frame(&mut self.natives.host(&mut self.state));
        self.state.update();
        self.tick += 1;
    }

    /// Capture a save: the save image (global block 0), this event and the player's position.
    pub fn save(&self) -> crate::save::SaveGame {
        let size = crate::save::FFX_IMAGE_SIZE.min(self.vm.memory.global[0].len());
        crate::save::SaveGame {
            game: self.game,
            event: Some(self.path.clone()),
            position: self.state.player.and_then(|p| self.state.actors.get(p)).map(|a| a.pos),
            ticks: self.tick,
            image: self.vm.memory.global[0][..size].to_vec(),
        }
    }

    /// Put a save's image in place (and the player where it stood) before the event runs.
    pub fn restore(&mut self, save: &crate::save::SaveGame) {
        let g = &mut self.vm.memory.global[0];
        let n = save.image.len().min(g.len());
        g[..n].copy_from_slice(&save.image[..n]);
        self.tick = save.ticks;
        if let (Some(p), Some(pos)) = (self.state.player, save.position)
            && let Some(a) = self.state.actors.get_mut(p)
        {
            a.pos = pos;
        }
    }

    /// The map number in the script's header.
    pub fn map_number(&self) -> Option<i16> {
        self.vm.script.map_number()
    }

    pub fn initialized(&self) -> bool {
        self.vm.phase == Phase::Running
    }

    /// Ask a worker to run an entry (what talking to or touching an entity does).
    pub fn request(&mut self, worker: usize, level: u8, entry: u16) -> bool {
        self.vm.request(None, worker, RequestMode::Plain, level, entry)
    }
}

fn pop3(c: &mut NativeCall<'_>) -> [f32; 3] {
    let z = c.stack.pop_float();
    let y = c.stack.pop_float();
    let x = c.stack.pop_float();
    [x, y, z]
}

fn target(c: &mut NativeCall<'_>) -> usize {
    let i = c.stack.pop_int();
    if i < 0 { c.worker } else { i as usize }
}

fn window(c: &mut NativeCall<'_>) -> usize {
    c.stack.pop_int().clamp(0, 7) as usize
}

/// The message wait: the init takes the window (and a flag), the wait reveals the text and closes the window on
/// confirm (moving between choices with the stick), the result is the chosen choice.
fn message_wait() -> Native<FieldState> {
    Native {
        init: Some(|_, c| {
            let _b = c.stack.pop_int();
            c.scratch[0] = window(c) as i32;
        }),
        wait: Some(|g, c| {
            let w = c.scratch[0] as usize;
            let msg = g.windows[w].message.and_then(|m| g.messages.get(m)).cloned().unwrap_or_default();
            let win = &mut g.windows[w];
            if !win.open {
                return if win.closing == 0 { WAIT_DONE } else { 0 };
            }
            if msg.choices > 0 && g.input.stick[1] != 0.0 && g.input.stick_pressed {
                let n = usize::from(msg.choices);
                win.choice =
                    if g.input.stick[1] > 0.0 { (win.choice + n - 1) % n } else { (win.choice + 1) % n };
            }
            if g.input.confirm {
                if win.shown < win.reveal {
                    // The rest appears at once (up to a pause, which the tick's update lets go of).
                    win.shown =
                        win.stops.first().copied().unwrap_or(win.reveal).min(win.reveal).max(win.shown);
                } else {
                    win.close();
                    return 0;
                }
            }
            0
        }),
        result: Some(|g, c| Value::Int(g.windows[c.scratch[0] as usize].choice as i32)),
    }
}

/// Workers turned off (they stop running, but keep their requests) and on again.
fn register_workers(n: &mut Natives<FieldState>) {
    fn set(c: &mut NativeCall, on: bool) -> Value {
        let w = c.stack.pop_int().max(0) as usize;
        c.effects.push(aerugo_script::vm::Effect::Enable(w, on));
        Value::Int(0)
    }
    n.register(0x00A7, Native::result(|_, c| set(c, false)));
    n.register(0x00A8, Native::result(|_, c| set(c, true)));
}

/// Player control on and off, and the player's talk reach.
fn register_control(n: &mut Natives<FieldState>) {
    n.register(
        0x005D,
        Native::result(|g, _| {
            g.user_control = true;
            Value::Int(1)
        }),
    );
    n.register(
        0x005E,
        Native::result(|g, _| {
            g.user_control = false;
            Value::Int(0)
        }),
    );
    n.register(
        0x0192,
        Native::result(|g, c| {
            g.talk_reach = c.stack.pop_float();
            Value::Int(0)
        }),
    );
}

/// Trigger shapes: lines (kinds 2 and 5), boxes (3 and 6), and the touch and talk sizes of characters.
fn register_triggers(n: &mut Natives<FieldState>) {
    // A line: two points; no height limit.
    n.register(
        0x0054,
        Native::result(|g, c| {
            let b = pop3(c);
            let a = pop3(c);
            let kind = c.kind;
            let actor = g.actor(c.worker);
            if !matches!(kind, 2 | 5) {
                return Value::Int(0);
            }
            actor.pos = a;
            actor.trigger.b = b;
            actor.trigger.height = -1.0;
            actor.trigger.active = true;
            actor.trigger.dirty = true;
            Value::Int(1)
        }),
    );
    // Height limit of a line or box.
    n.register(
        0x0055,
        Native::result(|g, c| {
            let h = c.stack.pop_float();
            let kind = c.kind;
            let t = &mut g.actor(c.worker).trigger;
            if !matches!(kind, 2 | 3 | 5 | 6) {
                return Value::Int(0);
            }
            t.height = h;
            t.dirty = true;
            Value::Int(1)
        }),
    );
    // Trigger on or off (two natives do the same).
    for id in [0x0056u16, 0x005C] {
        n.register(
            id,
            Native::result(|g, c| {
                let on = c.stack.pop_int() != 0;
                let kind = c.kind;
                let t = &mut g.actor(c.worker).trigger;
                if !matches!(kind, 2 | 3 | 5 | 6) {
                    return Value::Int(0);
                }
                t.active = on;
                t.dirty = true;
                Value::Int(1)
            }),
        );
    }
    // A box's centre.
    n.register(
        0x0057,
        Native::result(|g, c| {
            let p = pop3(c);
            let kind = c.kind;
            let a = g.actor(c.worker);
            if !matches!(kind, 3 | 6) {
                return Value::Int(0);
            }
            a.pos = p;
            a.trigger.active = true;
            a.trigger.dirty = true;
            Value::Int(1)
        }),
    );
    // A box's half width and depth, with (0x59) or without (0x58) a half height.
    n.register(0x0058, Native::result(|g, c| box_size(g, c, -1.0)));
    n.register(
        0x0059,
        Native::result(|g, c| {
            let h = c.stack.pop_float();
            box_size(g, c, h)
        }),
    );
    // Lengthen a line at both ends.
    n.register(
        0x007A,
        Native::result(|g, c| {
            let after = c.stack.pop_float();
            let before = c.stack.pop_float();
            let kind = c.kind;
            let a = g.actor(c.worker);
            if !matches!(kind, 2 | 5) {
                return Value::Int(0);
            }
            a.trigger.dirty = true;
            let d: [f32; 3] = std::array::from_fn(|k| a.trigger.b[k] - a.pos[k]);
            let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            if len <= 0.0 {
                return Value::Int(0);
            }
            for (k, dk) in d.iter().enumerate() {
                a.pos[k] -= dk * before / len;
                a.trigger.b[k] += dk * after / len;
            }
            Value::Int(1)
        }),
    );
    // Talk radius (whole units), talk facing half angle, touch radius; two values not used here yet.
    n.register(
        0x0061,
        Native::result(|g, c| {
            let r = c.stack.pop_float().trunc();
            let t = &mut g.actor(c.worker).trigger;
            t.talk_radius = r;
            t.dirty = true;
            Value::Float(r)
        }),
    );
    n.register(
        0x0063,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let t = &mut g.actor(c.worker).trigger;
            t.talk_angle = v;
            t.dirty = true;
            Value::Float(v)
        }),
    );
    n.register(
        0x0085,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let t = &mut g.actor(c.worker).trigger;
            t.radius = v;
            t.dirty = true;
            Value::Int(0)
        }),
    );
    for id in [0x0062u16, 0x008D] {
        n.register(
            id,
            Native::result(|g, c| {
                let v = c.stack.pop_float();
                g.actor(c.worker).trigger.dirty = true;
                Value::Float(v)
            }),
        );
    }
}

fn box_size(g: &mut FieldState, c: &mut NativeCall, h: f32) -> Value {
    let hd = c.stack.pop_float();
    let hw = c.stack.pop_float();
    let kind = c.kind;
    let t = &mut g.actor(c.worker).trigger;
    if !matches!(kind, 3 | 6) {
        return Value::Int(0);
    }
    t.b = [hw, 0.0, hd];
    t.height = h;
    t.dirty = true;
    Value::Int(1)
}

fn camera_move_time(g: &mut FieldState, c: &mut NativeCall) -> Value {
    let _d = c.stack.pop_int();
    let frames: i32 = (0..3).map(|_| c.stack.pop_int().max(0)).sum();
    let cam = &mut g.camera;
    let parts = cam.parts.unwrap_or(7);
    for (bit, t) in cam.timers.iter_mut().enumerate() {
        if parts & (1 << bit) != 0 {
            *t = frames as u32 + 1;
        }
    }
    if parts & 7 != 0 {
        cam.move_frames = frames.max(1) as u32;
        cam.moving = None;
        cam.pending = true;
    }
    Value::Int(0)
}

fn path_move(g: &mut FieldState, c: &mut NativeCall, which: usize) -> Value {
    let _mode = c.stack.pop_int();
    let step = c.stack.pop_float();
    let start = c.stack.pop_float();
    let _path = c.stack.pop_int();
    let mut ticks = 0u32;
    if step > 0.0 {
        let mut t = start;
        while t < 1.0 && ticks < 100_000 {
            t += step;
            ticks += 1;
        }
    }
    g.camera.path_timers[which] = ticks;
    Value::Int(0)
}

/// Natives of the field slice: actors, movement, turning, messages, camera, worker queries.
fn register(n: &mut Natives<FieldState>) {
    // Worker ids: the own id for a negative argument.
    n.register(
        0x0033,
        Native::result(|_, c| {
            let i = c.stack.pop_int();
            Value::Int(if i < 0 { c.worker as i32 } else { i })
        }),
    );
    // Actors.
    n.register(
        0x0001,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            g.actor(c.worker).model = Some(id as u16);
            Value::Int(0)
        }),
    );
    n.register(
        0x0013,
        Native::result(|g, c| {
            let p = pop3(c);
            let a = g.actor(c.worker);
            a.pos = p;
            a.moving = None;
            Value::Int(0)
        }),
    );
    n.register(
        0x0095,
        Native::result(|g, c| {
            let yaw = wrap_angle(c.stack.pop_float());
            let a = g.actor(c.worker);
            a.yaw = yaw;
            a.turning = false;
            a.turn_steps = None;
            Value::Float(yaw)
        }),
    );
    for id in [0x5008u16, 0x500D] {
        n.register(
            id,
            Native::result(|g, c| {
                let hidden = c.stack.pop_int() != 0;
                g.actor(c.worker).visible = !hidden;
                Value::Int(0)
            }),
        );
    }
    n.register(
        0x5000,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            g.actor(c.worker).motion = Some(m);
            Value::Int(0)
        }),
    );
    // Position of a worker (negative: the caller).
    n.register(0x0038, Native::result(|g, c| Value::Float(g.actor(target(c)).pos[0])));
    n.register(0x0039, Native::result(|g, c| Value::Float(g.actor(target(c)).pos[1])));
    n.register(0x003A, Native::result(|g, c| Value::Float(g.actor(target(c)).pos[2])));
    // Motions: the motion a body plays (the renderer picks it from the body's motion groups).
    for id in [0x501Bu16, 0x5040] {
        n.register(
            id,
            Native::result(|g, c| {
                let m = c.stack.pop_int();
                g.actor(c.worker).motion = Some(m);
                Value::Int(0)
            }),
        );
    }
    n.register(
        0x5047,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            let w = target(c);
            g.actor(w).motion = Some(m);
            Value::Int(0)
        }),
    );
    n.register(
        0x5025,
        Native::result(|g, c| {
            let _b = c.stack.pop_int();
            let m = c.stack.pop_int();
            g.actor(c.worker).motion = Some(m);
            Value::Int(0)
        }),
    );
    // The party character a worker plays (actors only); character 0 is taken as the player until the
    // native that names the player is known.
    n.register(
        0x0042,
        Native::result(|g, c| {
            let chr = c.stack.pop_int();
            if c.kind == 1 {
                g.actor(c.worker).character = u8::try_from(chr).ok();
                if chr == 0 && g.player.is_none() {
                    g.player = Some(c.worker);
                }
            }
            Value::Int(chr)
        }),
    );
    // Per-worker values and flags.
    n.register(
        0x00C5,
        Native::result(|g, c| {
            let mut v = [0; 4];
            for k in (0..4).rev() {
                v[k] = c.stack.pop_int();
            }
            g.actor(c.worker).params = v;
            Value::Int(0)
        }),
    );
    n.register(
        0x00C7,
        Native::result(|g, c| {
            let i = c.stack.pop_int().clamp(0, 3) as usize;
            Value::Int(g.actor(c.worker).params[i])
        }),
    );
    n.register(
        0x0034,
        Native::result(|g, c| {
            let bit = c.stack.pop_int() as u32 & 31;
            g.actor(c.worker).flags |= 1 << bit;
            Value::Int(0)
        }),
    );
    n.register(
        0x0035,
        Native::result(|g, c| {
            let bit = c.stack.pop_int() as u32 & 31;
            g.actor(c.worker).flags &= !(1 << bit);
            Value::Int(0)
        }),
    );
    // Random numbers from the script stream.
    n.register(0x00A9, Native::result(|g, _| Value::Int((g.random() & 0xFFFF) as i32)));
    n.register(
        0x00A6,
        Native::result(|g, c| {
            let n = c.stack.pop_int().max(1);
            Value::Int((((g.random() & 0xFFFF) as i64 * i64::from(n)) >> 16) as i32)
        }),
    );
    // Movement: target, speed (distance per tick, or a duration), start, wait.
    n.register(
        0x0015,
        Native::result(|g, c| {
            let p = pop3(c);
            g.actor(c.worker).move_target = p;
            Value::Int(0)
        }),
    );
    n.register(
        0x001C,
        Native::result(|g, c| {
            let b = c.stack.pop_float();
            let a = c.stack.pop_float();
            g.actor(c.worker).move_speed = [a, b];
            Value::Float(0.0)
        }),
    );
    n.register(
        0x0018,
        Native::result(|g, c| {
            let _b = c.stack.pop_int();
            let _mode = c.stack.pop_int();
            let _a = c.stack.pop_int();
            let a = g.actor(c.worker);
            let d = (0..3).map(|k| (a.move_target[k] - a.pos[k]).powi(2)).sum::<f32>().sqrt();
            let frames = if a.move_speed[0] > 0.0 {
                (d / a.move_speed[0]).ceil() as u32
            } else {
                a.move_speed[1].max(1.0) as u32
            };
            a.moving = Some(Movement { from: a.pos, to: a.move_target, frames: frames.max(1), done: 0 });
            Value::Int(0)
        }),
    );
    n.register(
        0x001A,
        Native {
            init: None,
            wait: Some(|g, c| if g.actor(c.worker).is_moving() { 0 } else { 5 }),
            result: None,
        },
    );
    n.register(
        0x0077,
        Native::result(|g, c| {
            let w = c.stack.pop_int();
            if let Ok(w) = usize::try_from(w) {
                g.actor(w).moving = None;
            }
            Value::Int(0)
        }),
    );
    // Turning.
    n.register(
        0x0028,
        Native::result(|g, c| {
            let yaw = wrap_angle(c.stack.pop_float());
            g.actor(c.worker).turn_target = yaw;
            Value::Float(yaw)
        }),
    );
    n.register(
        0x001D,
        Native::result(|g, c| {
            let b = c.stack.pop_float();
            let a = c.stack.pop_float();
            g.actor(c.worker).turn_speed = [a, if b <= 0.0 { 1.0 } else { b }];
            Value::Float(0.0)
        }),
    );
    n.register(
        0x0019,
        Native::result(|g, c| {
            c.stack.pop_int();
            let mode = c.stack.pop_int() as u32;
            c.stack.pop_int();
            let a = g.actor(c.worker);
            a.turning = true;
            // Modes 3 and 4 without a speed turn at an even rate over the set number of frames, the short way.
            a.turn_steps = (matches!(mode & 0xffff_001f, 3 | 4) && a.turn_speed[0] == 0.0).then(|| {
                let frames = a.turn_speed[1].max(1.0) as u32;
                (wrap_angle(a.turn_target - a.yaw) / frames as f32, frames)
            });
            Value::Int(0)
        }),
    );
    n.register(
        0x001B,
        Native { init: None, wait: Some(|g, c| if g.actor(c.worker).turning { 0 } else { 5 }), result: None },
    );
    // Map change: target and entrance; the session carries it out between ticks.
    n.register(
        0x0011,
        Native::result(|g, c| {
            let entrance = c.stack.pop_int();
            let target = c.stack.pop_int();
            g.jump = Some((target, entrance));
            Value::Int(0)
        }),
    );
    // Messages: text, position, open, wait for the player, close.
    n.register(
        0x0064,
        Native::result(|g, c| {
            let msg = c.stack.pop_int();
            let w = window(c);
            g.windows[w].message = usize::try_from(msg).ok();
            Value::Int(0)
        }),
    );
    n.register(
        0x0065,
        Native::result(|g, c| {
            let _align = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let w = window(c);
            g.windows[w].x = x;
            g.windows[w].y = y;
            Value::Int(0)
        }),
    );
    n.register(
        0x006A,
        Native::result(|g, c| {
            let _mode = c.stack.pop_int();
            let w = window(c);
            let image = &c.memory.global[0];
            let (mut reveal, mut stops) = (0, Vec::new());
            for part in g.windows[w].message.and_then(|m| g.messages.get(m)).map_or(&[][..], |m| &m.reveal) {
                match *part {
                    RevealPart::Chars(n) => reveal += n,
                    RevealPart::Name(chr) => reveal += name_chars(image, chr),
                    RevealPart::Pause => stops.push(reveal),
                }
            }
            let win = &mut g.windows[w];
            win.reveal = reveal;
            win.stops = stops;
            win.open = true;
            win.shown = 0;
            win.choice = 0;
            win.opening = OPEN_TICKS;
            win.closing = 0;
            Value::Int(w as i32)
        }),
    );
    n.register(0x0084, message_wait());
    // The other wait takes only the window.
    n.register(0x007C, Native { init: Some(|_, c| c.scratch[0] = window(c) as i32), ..message_wait() });
    // Window state queries: done when closed or fully shown; the raw state.
    n.register(
        0x008F,
        Native::result(|g, c| {
            let w = window(c);
            let win = &g.windows[w];
            Value::Int(i32::from(!win.open || (win.opening == 0 && win.shown >= win.reveal)))
        }),
    );
    // Window flags.
    n.register(
        0x009D,
        Native::result(|g, c| {
            let flags = c.stack.pop_int();
            let w = window(c);
            g.windows[w].flags = flags as u16;
            Value::Int(0)
        }),
    );
    n.register(0x0097, Native::result(|g, c| Value::Int(g.windows[window(c)].state())));
    n.register(
        0x006B,
        Native::result(|g, c| {
            let w = window(c);
            g.windows[w].close();
            Value::Int(w as i32)
        }),
    );
    // Camera: position and focus.
    n.register(
        0x6002,
        Native::result(|g, c| {
            g.camera.position = Some(pop3(c));
            g.camera.moving = None;
            Value::Int(0)
        }),
    );
    // Destination, then a move time in frames starts the move; wait until it arrives; stop.
    n.register(
        0x6004,
        Native::result(|g, c| {
            g.camera.destination = Some(pop3(c));
            Value::Int(0)
        }),
    );
    n.register(
        0x6010,
        Native::result(|g, c| {
            g.camera.move_frames = c.stack.pop_int().max(1) as u32;
            g.camera.start_move();
            Value::Int(0)
        }),
    );
    // Speeding up, steady and slowing down frame counts, then a fourth value; the move takes the first three
    // together, and a camera wait polls as not done for one tick more. 0x6015 sets another mode with the same
    // timing.
    for id in [0x6014u16, 0x6015] {
        n.register(id, Native::result(camera_move_time));
    }
    // Eye and focus along a path: (path, start, step per tick, mode); the position on the path goes from the
    // start to 1 by the step each tick.
    n.register(0x6046, Native::result(|g, c| path_move(g, c, 0)));
    n.register(0x6047, Native::result(|g, c| path_move(g, c, 1)));
    register_triggers(n);
    register_control(n);
    register_workers(n);
    // Mark a worker for an extra pass this frame.
    n.register(
        0x0117,
        Native::result(|_, c| {
            let w = c.stack.pop_int();
            if let Ok(w) = usize::try_from(w) {
                c.effects.push(aerugo_script::vm::Effect::ExtraPass(w));
            }
            Value::Int(0)
        }),
    );
    // The camera parts the following move times apply to.
    n.register(
        0x6043,
        Native::result(|g, c| {
            g.camera.parts = Some(c.stack.pop_int() as u8);
            Value::Int(0)
        }),
    );
    n.register(
        0x601A,
        Native { init: None, wait: Some(|g, _| if g.camera.is_moving() { 0 } else { 5 }), result: None },
    );
    n.register(
        0x6016,
        Native::result(|g, _| {
            g.camera.moving = None;
            Value::Int(0)
        }),
    );
    n.register(
        0x603B,
        Native::result(|g, c| {
            g.camera.screen_distance = c.stack.pop_float();
            Value::Int(0)
        }),
    );
    n.register(
        0x6020,
        Native::result(|g, c| {
            g.camera.focus = Some(pop3(c));
            Value::Int(0)
        }),
    );
    n.register(
        0x603A,
        Native::result(|g, c| {
            g.camera.roll = c.stack.pop_float();
            Value::Int(0)
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_positive() {
        let mut a = Rng { state: 1 };
        let mut b = Rng { state: 1 };
        let xs: Vec<u32> = (0..100).map(|_| a.next_value()).collect();
        assert!(xs.iter().all(|&x| x <= 0x7FFF_FFFF));
        assert_eq!(xs, (0..100).map(|_| b.next_value()).collect::<Vec<_>>());
        assert!(xs.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn message_window_reveals_and_closes_on_confirm() {
        use aerugo_script::asm::{Op, WorkerSrc, assemble};
        let imm = |v: i16| Op(0x2E, Some(v as u16));
        // set_text(0, 0); open(0, 0); wait(0, 0); global[0] = 1
        let w = WorkerSrc {
            kind: 1,
            entries: vec![vec![
                imm(0),
                imm(0),
                Op(0x58, Some(0x0064)),
                imm(0),
                imm(0),
                Op(0x58, Some(0x006A)),
                imm(0),
                imm(0),
                Op(0x58, Some(0x0084)),
                imm(1),
                Op(0x20, Some(0)),
                Op(0x3C, None),
            ]],
            vars: vec![(5 << 28, 1)],
            ..Default::default()
        };
        let script = Script::parse(&assemble(&[w])).unwrap();
        let mut vm = Vm::new(Arc::new(script));
        let mut natives = Natives::new(Game::Ffx);
        register(&mut natives);
        let mut state = FieldState {
            messages: vec![Message {
                text: "Hello there".into(),
                raw: String::new(),
                choices: 0,
                reveal: vec![RevealPart::Chars(11)],
            }],
            ..FieldState::default()
        };
        let done = |vm: &Vm| vm.memory.global[0][aerugo_script::memory::AREA0_OFFSET] == 1;
        let mut tick = |vm: &mut Vm, state: &mut FieldState, confirm: bool| {
            state.input.confirm = confirm;
            vm.frame(&mut natives.host(state));
            state.update();
        };
        tick(&mut vm, &mut state, false);
        assert!(state.windows[0].open);
        assert!(state.windows[0].shown < 11);
        // The first confirm reveals the rest, the second closes the window and resumes the script.
        tick(&mut vm, &mut state, true);
        assert!(state.windows[0].open);
        assert_eq!(state.windows[0].shown, 11);
        tick(&mut vm, &mut state, true);
        assert!(!state.windows[0].open);
        // The wait ends once the window has finished closing, and the script goes on the tick after.
        tick(&mut vm, &mut state, false);
        tick(&mut vm, &mut state, false);
        assert!(!done(&vm));
        tick(&mut vm, &mut state, false);
        assert!(done(&vm));
    }

    #[test]
    fn map_change_is_recorded() {
        use aerugo_script::asm::{Op, WorkerSrc, assemble};
        let w = WorkerSrc {
            kind: 1,
            entries: vec![vec![
                Op(0x2E, Some(165)),
                Op(0x2E, Some(2)),
                Op(0x58, Some(0x0011)),
                Op(0x3C, None),
            ]],
            ..Default::default()
        };
        let mut vm = Vm::new(Arc::new(Script::parse(&assemble(&[w])).unwrap()));
        let mut natives = Natives::new(Game::Ffx);
        register(&mut natives);
        let mut state = FieldState::default();
        vm.frame(&mut natives.host(&mut state));
        assert_eq!(state.jump, Some((165, 2)));
    }

    #[test]
    fn player_walks_on_ground() {
        let mut state = FieldState {
            actors: vec![Actor::default()],
            player: Some(0),
            ground: Some(Ground::new(vec![
                [[-100.0, 0.0, -100.0], [100.0, 0.0, -100.0], [100.0, 0.0, 100.0]],
                [[-100.0, 0.0, -100.0], [100.0, 0.0, 100.0], [-100.0, 0.0, 100.0]],
            ])),
            ..FieldState::default()
        };
        state.input.stick = [0.0, 1.0];
        for _ in 0..10 {
            state.update();
        }
        assert!(state.actors[0].pos[2] > 20.0, "{:?}", state.actors[0].pos);
        // The edge of the ground stops the player.
        for _ in 0..200 {
            state.update();
        }
        assert!(state.actors[0].pos[2] <= 100.0);
    }

    #[test]
    fn model_paths() {
        assert_eq!(
            model_path(Game::Ffx, 0x2023).as_deref(),
            Some("ffx_data/gamedata/ps3data/chr/npc/n035/mdl/d3d11/n035.dae.phyre")
        );
        assert_eq!(model_path(Game::Ffx, 0x9000), None);
    }
}
