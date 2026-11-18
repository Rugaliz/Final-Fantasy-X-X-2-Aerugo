//! A field event: the event's script running on the script machine, with the state its natives act on (actors,
//! message windows, camera, player control). The renderer only reads this state.

use std::sync::Arc;

use aerugo_formats::Game;
use aerugo_formats::script::Script;
use aerugo_formats::text::{self, Charset};
use aerugo_script::natives::{Native, World, heading, wrap_angle};

use crate::control::PlayerControl;
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
    /// Collision radius set by a script (else the model's).
    pub radius: Option<f32>,
    /// The model's own collision radius (from its PS2 character data, once loaded).
    pub model_radius: Option<f32>,
    /// The heading a move goes along (scripts set and read it).
    pub move_heading: f32,
    /// The worker's first pass of this tick is over (its moves and turns have had their update).
    first_pass_done: bool,
    /// A move or turn started after that: it first advances next tick.
    started_late: bool,
    /// The player touched this one walking (cleared once they are apart): a bump is this going on.
    bumped: bool,
    /// Body hit turned off (500f): neither blocks nor bumps.
    pub no_body_hit: bool,
    /// Collisions with this body test heights too (on by default; 5073 switches it): two bodies touch across a
    /// height difference unless both test it.
    pub height_test: bool,
    /// The motion playing: motion id, first frame, last frame (−1: its end), ticks played, loops left (0xFF:
    /// for ever).
    pub motion_play: Option<(i32, i32, i32, u32, u8)>,
    /// Motion speed (0x100 normal).
    pub motion_speed: u16,
    /// The script level whose move advances (the worker's running level).
    pub level: u8,
    /// Move state per script level: a level's move waits while a higher level runs.
    moves: [MoveState; MOVE_LEVELS],
    turn_target: f32,
    turn_speed: [f32; 2],
    turning: bool,
    /// A turn over a set number of frames: the step per frame and the frames left.
    turn_steps: Option<(f32, u32)>,
    /// The level that started the turn: it advances only while that level runs, and only that level's
    /// set_rotation ends it (turns are per level in the original).
    turn_level: u8,
    /// The running turn's mode word (rotate.start), its eased progress and the progress per tick.
    turn_mode: u32,
    turn_t: f32,
    turn_inc: f32,
    /// Facing pitch, and the pitch moves in space go along.
    pub pitch: f32,
    pub move_pitch: f32,
    /// How lit the character is (its material colour is × shade × 0.85 + 0.15): the walk mesh's shade where it
    /// stands, or what native 5088 set while that is off.
    pub shade: Eased,
    /// The shade follows the walk mesh (on for every character; native 5060 switches it).
    pub shade_from_ground: bool,
    /// The character's colour and alpha (natives 5015 and 5033): its material colour and its shade colour; alpha 0
    /// hides it.
    pub color: [Eased; 4],
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
            radius: None,
            model_radius: None,
            move_heading: 0.0,
            first_pass_done: false,
            started_late: false,
            bumped: false,
            no_body_hit: false,
            height_test: true,
            motion_play: None,
            motion_speed: 0x100,
            level: 0,
            moves: [MoveState::default(); MOVE_LEVELS],
            turn_target: 0.0,
            turn_speed: [0.0; 2],
            turning: false,
            turn_steps: None,
            turn_level: 0,
            turn_mode: 0,
            turn_t: 0.0,
            turn_inc: 0.0,
            pitch: 0.0,
            move_pitch: 0.0,
            shade: Eased::new(1.0),
            shade_from_ground: true,
            color: [Eased::new(1.0); 4],
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

/// Collision radius of a character whose model size is not known (the party leader's: 4).
const DEFAULT_RADIUS: f32 = 4.0;

/// How far past the sum of the radii two characters still touch.
const CONTACT_SLACK: f32 = 1e-3;

/// Script levels with their own move state.
const MOVE_LEVELS: usize = 9;

/// One script level's move settings and the move it runs.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
struct MoveState {
    target: [f32; 3],
    /// Frame count of a timed move.
    frames: f32,
    /// Walking speed (a walk covers speed / 20 per tick) and arrival radius (0: none).
    speed: f32,
    radius: f32,
    /// The heading and pitch the next move starts along (0021, 0022).
    start_heading: Option<f32>,
    start_pitch: Option<f32>,
    active: Option<ActorMove>,
}

/// A move under way.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ActorMove {
    /// From `from` to `to` over `frames` ticks; `count` ticks done.
    Timed { from: [f32; 3], to: [f32; 3], count: f32, frames: f32 },
    /// Towards `to` at a speed, on the ground plane only or in space; `mode` is move.start's mode word (0x8000:
    /// steered, the heading turning toward the target at a limited rate; 0x4000: the turn ends with it).
    Walk { to: [f32; 3], speed: f32, ground_only: bool, radius: f32, mode: u32 },
}

/// One tick of a walk from `pos` to `to`: steps of speed / 20, ending (true) once within speed / 10 with a last
/// step of half the rest.
fn walk_step(pos: [f32; 3], to: [f32; 3], speed: f32, ground_only: bool, radius: f32) -> ([f32; 3], bool) {
    let arrived = (to[0] - pos[0]).powi(2) + (to[2] - pos[2]).powi(2) < radius * radius;
    let to = if ground_only { [to[0], pos[1], to[2]] } else { to };
    let d = [to[0] - pos[0], to[1] - pos[1], to[2] - pos[2]];
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let (step, done) = if len <= speed / 10.0 { (len / 2.0, true) } else { (speed / 20.0, arrived) };
    if len <= 0.0 {
        return (pos, done);
    }
    (std::array::from_fn(|k| pos[k] + d[k] / len * step), done)
}
/// The settings key of a per-level value of a worker.
fn level_key(worker: usize, level: u8) -> usize {
    worker | (usize::from(level) + 1) << 16
}

/// A turn block's rate before a script sets one (π/10 a tick).
const DEFAULT_TURN_RATE: f32 = std::f32::consts::PI / 10.0;

/// Turn `cur` toward `target` by at most `max` (exactly onto it when closer).
fn turn_toward(cur: f32, target: f32, max: f32) -> f32 {
    let d = wrap_angle(target - cur);
    if d.abs() < max { target } else { wrap_angle(cur + max.copysign(d)) }
}

/// The share of an eased turn's rate at progress `t`: 0.4 at the ends, 1 halfway.
fn turn_ease(t: f32) -> f32 {
    ((1.0 - (t - 0.5) * 4.0 * (t - 0.5)) * 0.6 + 0.4).clamp(0.0, 1.0)
}

/// What a steered walk does this tick, once its heading is known: the step length, whether the move ends, and
/// whether it keeps to the ground.
#[derive(Debug, Clone, Copy)]
struct SteerStep {
    len: f32,
    done: bool,
    ground_only: bool,
    mode: u32,
}

impl Actor {
    /// A steered walk (move mode 0x8000), first half of the tick: the arrival tests on the position before the
    /// step, then the heading (and pitch, in space) turned toward the target by at most the steering rates.
    fn steer_walk_begin(&mut self, yaw_rate: f32, pitch_rate: f32) -> Option<SteerStep> {
        let pos = self.pos;
        let level = self.level;
        let Some(ActorMove::Walk { to, speed, ground_only, radius, mode }) = self.move_at(level).active
        else {
            return None;
        };
        if mode & 0x8000 == 0 {
            return None;
        }
        let arrived = (to[0] - pos[0]).powi(2) + (to[2] - pos[2]).powi(2) < radius * radius;
        let aim = if ground_only { [to[0], pos[1], to[2]] } else { to };
        let d = ((aim[0] - pos[0]).powi(2) + (aim[1] - pos[1]).powi(2) + (aim[2] - pos[2]).powi(2)).sqrt();
        let near = d <= speed / 10.0;
        if pos[0] != to[0] || pos[2] != to[2] {
            self.move_heading =
                turn_toward(self.move_heading, heading(pos[0], pos[2], to[0], to[2]), yaw_rate);
            if !ground_only {
                let ground = ((to[0] - pos[0]).powi(2) + (to[2] - pos[2]).powi(2)).sqrt();
                self.move_pitch =
                    turn_toward(self.move_pitch, heading(0.0, pos[1], ground, to[1]), pitch_rate);
            }
        }
        Some(SteerStep {
            len: if near { d / 2.0 } else { speed / 20.0 },
            done: near || arrived,
            ground_only,
            mode,
        })
    }

    /// Second half: the step along the heading (after the turn, which a linked turn sets it from).
    fn steer_walk_end(&mut self, st: SteerStep, ground: Option<&Ground>) {
        let (h, p) = (self.move_heading, if st.ground_only { 0.0 } else { self.move_pitch });
        let mut next = [
            self.pos[0] + st.len * p.cos() * h.cos(),
            self.pos[1] + st.len * p.sin(),
            self.pos[2] + st.len * p.cos() * h.sin(),
        ];
        if st.ground_only
            && let Some(y) = ground.and_then(|g| g.floor(next))
        {
            next[1] = y;
        }
        // A hidden body walks in the character engine only (see advance_move).
        if self.visible || self.model.is_none() {
            self.pos = next;
        }
        if st.done {
            let level = self.level;
            self.move_at(level).active = None;
            // A move linked to the turn (0x4000) ends it too.
            if st.mode & 0x4000 != 0 {
                self.turning = false;
                self.turn_steps = None;
            }
        }
    }

    /// One tick of a steered turn (kinds 1 and 2 with mode 0x8000): the facing turns toward the current level's
    /// move target, an eased turn (kind 2) at the rate times `turn_ease` of its progress. With 0x1000 the walk
    /// goes along the facing.
    fn steer_turn(&mut self, rate: f32) {
        let kind = self.turn_mode & 0x1F;
        let mut share = 1.0;
        if kind == 2 {
            self.turn_t = (self.turn_t + self.turn_inc).clamp(0.0, 1.0);
            share = turn_ease(self.turn_t);
        }
        let level = self.level;
        let to = self.move_at(level).target;
        let pos = self.pos;
        let want = heading(pos[0], pos[2], to[0], to[2]);
        let ground = ((to[0] - pos[0]).powi(2) + (to[2] - pos[2]).powi(2)).sqrt();
        self.yaw = turn_toward(self.yaw, want, rate * share);
        self.pitch = turn_toward(self.pitch, heading(0.0, pos[1], ground, to[1]), rate * share);
        if kind == 2 && self.turn_t >= 1.0 && self.yaw == want {
            self.turn_mode = self.turn_mode & !0x1F | 1;
        }
        if self.turn_mode & 0x1000 != 0 {
            self.move_heading = self.yaw;
            self.move_pitch = self.pitch;
        }
    }

    /// Whether the current level runs a steered walk.
    fn steered_walk(&self) -> bool {
        matches!(self.move_state().active, Some(ActorMove::Walk { mode, .. }) if mode & 0x8000 != 0)
    }

    /// Radius for collisions between characters.
    pub fn collision_radius(&self) -> f32 {
        self.radius.or(self.model_radius).unwrap_or(DEFAULT_RADIUS)
    }

    /// Whether the running level's move is a walk under way.
    pub fn is_walking(&self) -> bool {
        matches!(self.move_state().active, Some(ActorMove::Walk { .. }))
    }

    /// Whether the running level's move is under way.
    pub fn is_moving(&self) -> bool {
        self.move_state().active.is_some()
    }

    fn move_state(&self) -> &MoveState {
        &self.moves[usize::from(self.level).min(MOVE_LEVELS - 1)]
    }

    fn move_at(&mut self, level: u8) -> &mut MoveState {
        &mut self.moves[usize::from(level).min(MOVE_LEVELS - 1)]
    }

    /// One tick of the running level's move.
    fn advance_move(&mut self, ground: Option<&Ground>) {
        let pos = self.pos;
        let body_hidden = !self.visible && self.model.is_some();
        let level = self.level;
        let m = self.move_at(level);
        let Some(mv) = &mut m.active else { return };
        let (next, done) = match mv {
            ActorMove::Walk { to, speed, ground_only, radius, .. } => {
                let (mut next, done) = walk_step(pos, *to, *speed, *ground_only, *radius);
                // A hidden body walks in the character engine only: the worker's position stays, so the walk
                // never arrives (znkd1400 w6).
                if body_hidden {
                    next = pos;
                }
                if *ground_only && let Some(y) = ground.and_then(|g| g.floor(next)) {
                    next[1] = y;
                }
                (next, done)
            }
            // The share done is the count over the frames (before counting this tick); the move ends, at its
            // target, once the count passes the frames.
            ActorMove::Timed { from, to, count, frames } => {
                let t =
                    if *count == 0.0 && *frames == 0.0 { 1.0 } else { (*count / *frames).clamp(0.0, 1.0) };
                *count += 1.0;
                if *frames < *count {
                    (*to, true)
                } else {
                    (std::array::from_fn(|k| from[k] + (to[k] - from[k]) * t), false)
                }
            }
        };
        if done {
            m.active = None;
        }
        self.pos = next;
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
    /// Screen position in the original's 512 × 448 frame, and how the window sits on it (4: centred there, 2: its
    /// right edge there; otherwise its top left corner).
    pub x: i32,
    pub y: i32,
    pub align: i32,
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
    /// Flags a script set for the window (0x2000: a label left on screen).
    pub flags: u16,
    /// Opened to show its text at once (choice windows).
    pub instant: bool,
    /// The choice highlighted when the window opens, and the one cancel picks.
    pub first_choice: u8,
    pub cancel_choice: u8,
    /// Ticks the taken choice stays readable after a choice window starts closing.
    pub choice_live: u8,
    /// The style a script set (0066); 0 is the system window (slower to close).
    pub style: Option<u8>,
}

/// Characters further apart than this vertically do not touch (cdsp0100: one falling past another 16 apart).
const BUMP_HEIGHT: f32 = 10.0;

/// Ticks a window takes to open (the original reports "opening" for the frame it opens and the next).
const OPEN_TICKS: u8 = 2;

/// Ticks a window takes to close (a wait on it polls as not done on the frame it closes and the next).
const CLOSE_TICKS: u8 = 2;

/// Extra ticks a style-0 (system) window takes to close: 013b choices and 015b pickups end 9 ticks after the
/// press, style-1 dialogue windows 4.
const SYSTEM_CLOSE_EXTRA: u8 = 5;

/// Characters revealed per tick.
const TEXT_SPEED: usize = 2;

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
    /// The text starts with {09:30}: it shows at once.
    pub instant: bool,
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
            self.closing = CLOSE_TICKS + if self.style == Some(0) { SYSTEM_CLOSE_EXTRA } else { 0 };
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
#[derive(Debug, Clone, Default, PartialEq)]
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
    /// Ticks left of the move channels: 0–2 the eye's three values, 3 and 4 the roll and the screen distance, 5–7 the
    /// focus's three values. A move of n frames keeps its channels busy for n + 1 polls from the tick it is set.
    timers: [u32; 8],
    /// Eye channels (bits 0–4) the next eye move time applies to; all until a script picks, and only for one
    /// move.
    parts: Option<u8>,
    /// Ticks left of the moves of the eye and the focus along a path.
    path_timers: [u32; 2],
    /// A timed move whose destination may still be set this tick.
    pending: bool,
    /// The field camera following the player (when a script turns it on).
    pub follow: crate::follow_camera::FollowCamera,
}

impl CameraState {
    /// The eye's channels or its path move (what a camera wait waits for) are under way.
    pub fn is_moving(&self) -> bool {
        self.timers[0..5].iter().any(|&t| t > 0) || self.path_timers[0] > 0
    }

    /// The focus's channels or its path move are under way.
    pub fn focus_moving(&self) -> bool {
        self.timers[5..8].iter().any(|&t| t > 0) || self.path_timers[1] > 0
    }

    /// Time the eye's channels (those the part mask leaves, all by default) over `frames` (0: stops them).
    pub fn time_eye(&mut self, frames: u32) {
        let parts = self.parts.take().unwrap_or(0x1F);
        for (bit, t) in self.timers[0..5].iter_mut().enumerate() {
            if parts & (1 << bit) != 0 {
                *t = if frames == 0 { 0 } else { frames + 1 };
            }
        }
    }

    /// Time the focus's channels over `frames`.
    pub fn time_focus(&mut self, frames: u32) {
        self.timers[5..8].fill(if frames == 0 { 0 } else { frames + 1 });
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
    /// The pad word the scripts read (pressed this tick: 0x20 confirm, 0x40 cancel, 0x80 the second talk button,
    /// 0x1000 up, 0x4000 down, ...). Zero lets the fields above stand for it.
    pub pad: u32,
    /// The pad word of buttons held (the scripts' other word). Zero lets `pad` stand for it.
    pub held: u32,
    /// Cancel held: the player walks instead of running.
    pub walk: bool,
    /// Heading of the camera the player sees (towards where it looks on the ground plane, as `atan2(dz, dx)`);
    /// the stick is relative to it. None: the camera looks along +Z.
    pub camera_heading: Option<f32>,
}

/// A value moving linearly to a target over a number of ticks (the original's interpolated character values).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Eased {
    pub value: f32,
    target: f32,
    step: f32,
}

impl Eased {
    pub fn new(value: f32) -> Eased {
        Eased { value, target: value, step: 0.0 }
    }

    /// Move to `target` over `frames` ticks (at once when 0 or less).
    pub fn set(&mut self, target: f32, frames: i32) {
        self.target = target;
        if frames <= 0 {
            self.value = target;
            self.step = 0.0;
        } else {
            self.step = (target - self.value) / frames as f32;
        }
    }

    fn tick(&mut self) {
        if self.step != 0.0 {
            self.value += self.step;
            if (self.step > 0.0 && self.value >= self.target)
                || (self.step < 0.0 && self.value <= self.target)
            {
                self.value = self.target;
                self.step = 0.0;
            }
        }
    }
}

/// One light of the field light set: a colour (0–1) and the direction it shines in as yaw and pitch (degrees;
/// the ambient, index 3, has none).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldLight {
    pub color: [f32; 3],
    pub yaw: f32,
    pub pitch: f32,
}

impl FieldLight {
    /// A light from the 0–255 colour and the angles scripts pass.
    pub fn from_script(r: i32, g: i32, b: i32, yaw: i32, pitch: i32) -> FieldLight {
        FieldLight { color: [r, g, b].map(|c| c as f32 / 255.0), yaw: yaw as f32, pitch: pitch as f32 }
    }

    /// The direction the light shines in (game space): (sin yaw · cos pitch, sin pitch, cos yaw · cos pitch).
    pub fn direction(&self) -> [f32; 3] {
        let (y, p) = (self.yaw.to_radians(), self.pitch.to_radians());
        [y.sin() * p.cos(), p.sin(), y.cos() * p.cos()]
    }
}

/// Fog values scripts set (natives 8038 near, 803A far, 803C limit; 8037 all three and a curve of 1) over the
/// map's scene fog. The remaster's shaders see a limit of 0 (no fog) until a script sets one.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FogOverride {
    pub near: Option<f32>,
    pub far: Option<f32>,
    pub curve: Option<f32>,
    /// Share of fog colour at most (0–1).
    pub limit: f32,
}

/// The field's three directional lights and its ambient (index 3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldLights(pub [FieldLight; 4]);

impl Default for FieldLights {
    /// What every event start sets before its scripts run.
    fn default() -> Self {
        FieldLights([
            FieldLight::from_script(0x53, 0x53, 0x53, 0x82, 0x4C),
            FieldLight::from_script(0x37, 0x37, 0x37, 0x136, 0xE),
            FieldLight::from_script(0x12, 0x12, 0xD, 0x82, 0x11C),
            FieldLight::from_script(0x50, 0x5A, 0x69, 0, 0),
        ])
    }
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
    /// Item, treasure and equipment tables (FFX).
    pub items: Option<Arc<crate::items::ItemTables>>,
    pub input: Input,
    /// Worker the player controls.
    pub player: Option<usize>,
    pub ground: Option<Ground>,
    /// The map's fog from its PS2 map package (the scripts may change it).
    pub map_fog: Option<MapFog>,
    /// The field light characters are lit by (reset at every event start, set by native 4013).
    pub lights: FieldLights,
    /// What the fog natives set over the map's scene fog.
    pub fog: FogOverride,
    /// A map change the scripts asked for: target and entrance, for the session to carry out.
    pub jump: Option<(i32, i32)>,
    pub rng: Rng,
    /// Character ids in the three party slots (from the save image, refreshed every tick).
    pub party: [u8; 3],
    /// Random values to hand out instead of the generator's (replaying a recording of the original).
    pub rng_replay: std::collections::VecDeque<u32>,
    /// Where the player stood when the scripts of the last tick were done (before its own walk), and the
    /// step from there to where it stands is what triggers test. A script placing it late in a tick (in a later
    /// pass) is then not seen as a step the tick after (bjyt0400 f1435), as in the original, which takes the
    /// step from the player's own update.
    pub player_prev: [f32; 3],
    player_start: Option<[f32; 3]>,
    /// The player has control (scripts turn it on and off); talking needs it.
    pub user_control: bool,
    /// The player was walking on the last tick it had control.
    player_was_walking: bool,
    /// Added to every talk radius (a script sets it; 0 by default).
    pub talk_reach: f32,
    /// The talk started this tick, while workers offer themselves.
    pub talk: Option<Talk>,
    pub control: PlayerControl,
    /// Frame counts of each model's motions by motion number, as the host loads them.
    pub motion_frames: std::collections::HashMap<u16, std::collections::HashMap<u16, u32>>,
    /// Collision radii of models by id (None: no character data).
    pub model_radii: std::collections::HashMap<u16, Option<f32>>,
    /// Movie files by number (the game's video list).
    pub videos: std::collections::HashMap<u32, String>,
    /// The movie playing, if any.
    pub movie: Option<Movie>,
    /// A menu the scripts asked for (save, shop, name entry, ... by mode) and whether it is open; scripts stop
    /// while it is open. The result scripts read afterwards.
    pub menu: Option<MenuRequest>,
    pub menu_result: i32,
    /// The screen fade: 0 clear, 1 black, moving towards `fade_target` by `fade_step` a tick.
    pub fade: f32,
    pub fade_target: f32,
    pub fade_step: f32,
    /// Encounter areas (btl.bin), the walking counters, battle random streams, an area scripts set in place
    /// of the save image's, and encounters switched off by the host.
    pub encounters: Option<Arc<crate::encounter::EncounterTable>>,
    pub encounter_counters: crate::encounter::Counters,
    pub battle_random: crate::encounter::BattleRandom,
    pub encounter_area: i32,
    pub encounters_off: bool,
    /// A battle about to start or running (scripts hold while it runs), and whether it has started.
    pub battle: Option<(crate::encounter::Battle, bool)>,
    /// Values natives set for parts of the engine not ported yet, by (native, worker or actor).
    pub settings: std::collections::HashMap<(u16, usize), Vec<Value>>,
    /// The shop price rate in percent, when scripts changed it from 100.
    pub shop_rate: Option<i32>,
    /// A script warped the player this tick (cleared at the start of each tick).
    pub player_placed: bool,
    /// A switch scripts turn off around menus they open (0241).
    pub menu_switch_off: bool,
    /// Each worker's three data areas in the script (for natives that take tables there).
    pub data_areas: Vec<[u32; 3]>,
    /// Each worker's kind.
    pub kinds: Vec<u8>,
    /// The event's entrance records (the save image says which one the party arrived by).
    pub entrances: Vec<aerugo_formats::script::Entrance>,
    /// The player's step for next tick (from this tick's push), on the ground plane.
    player_step: Option<[f32; 2]>,
}

impl FieldState {
    /// The next script random number: a replayed one, else the generator's.
    pub fn random(&mut self) -> u32 {
        self.rng_replay.pop_front().unwrap_or_else(|| self.rng.next_value())
    }
}

impl World for FieldState {
    fn worker_ran(&mut self, w: usize, kind: u8, level: u8) -> Vec<u8> {
        // The level the worker's turn ended on is the one whose move advances this tick. (The original advances
        // moves and turns here, at the end of the worker's first pass: one started in a later pass of the same
        // tick waits for the next.)
        let a = self.actor(w);
        a.level = level;
        a.first_pass_done = true;
        let Some(p) = self.player.filter(|&p| p < self.actors.len()) else { return Vec::new() };
        // A player warped this tick did not walk: nothing is crossed.
        let cur = self.actors[p].pos;
        let prev = if self.player_placed { cur } else { self.player_prev };
        let step = PlayerStep { prev, cur };
        let Some(a) = self.actors.get_mut(w).filter(|a| a.trigger.active) else { return Vec::new() };
        let mut out = Vec::new();
        match kind {
            1 if a.model.is_some() => trigger::test_character(&mut a.trigger, a.pos, step, &mut out),
            2 => trigger::test_line_zone(&mut a.trigger, a.pos, step, &mut out),
            3 | 6 => trigger::test_box(&mut a.trigger, a.pos, a.yaw, step, &mut out),
            5 => trigger::test_line(&mut a.trigger, a.pos, step, &mut out),
            _ => {}
        }
        let bits = a.flags;
        // Kind 6 zones raise enter and leave, not "inside" every tick.
        out.retain(|&t| bits & (1 << t) != 0 && !(kind == 6 && t == trigger::INSIDE));
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

    fn start_signals(&mut self, w: usize, kind: u8, level: u8) -> Vec<u8> {
        let Some(p) = self.player.and_then(|p| self.actors.get(p)).map(|a| a.pos) else { return Vec::new() };
        let prev = std::mem::replace(&mut self.player_prev, p);
        let talk = self.talk.take();
        let out = self.worker_ran(w, kind, level);
        self.player_prev = prev;
        self.talk = talk;
        out
    }

    fn after_workers(&mut self) -> Vec<(usize, u8)> {
        let mut out = Vec::new();
        if let Some(talk) = self.talk.take()
            && let Some((w, _)) = talk.best.filter(|_| self.user_control)
        {
            out.push((w, talk.signal));
        }
        out.extend(self.bumps().into_iter().map(|w| (w, trigger::BUMP)));
        out
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
    /// The player pushes others back out (it does not walk through them), and walking into one bumps it: the
    /// one touched gets a flag, and a bump is the flag going on. The flag goes off only once the two are apart
    /// (further than the sum of their radii) while the player is not walking into it. Returns the workers bumped
    /// this tick.
    ///
    /// The player's walking is the original controller's speed, which stays as it was while control is off: a
    /// player that walked up to something and stood there a hair apart (pushed out, rounding to just past the
    /// contact distance) bumps it again once control comes back (cdsp0000 f2719, bjyt0400 f1509); one left just
    /// within it does not (bjyt0200 f113, bjyt0400 f252). Characters well above or below each other do not
    /// touch. Only the player bumps (pairs of others, and others walking into the player, do not).
    fn bumps(&mut self) -> Vec<usize> {
        let Some(p) = self.player.filter(|&p| p < self.actors.len()) else { return Vec::new() };
        let player_moved = {
            let a = &self.actors[p];
            a.pos[0] != self.player_prev[0] || a.pos[2] != self.player_prev[2]
        };
        let walking = self.user_control && (player_moved || self.player_was_walking);
        if self.user_control {
            self.player_was_walking = player_moved;
        }
        let body = |a: &Actor| a.model.is_some() && a.visible && !a.no_body_hit;
        let mut out = Vec::new();
        for o in 0..self.actors.len() {
            if o == p {
                continue;
            }
            let (a, b) = (&self.actors[p], &self.actors[o]);
            if !body(a) || !body(b) {
                self.actors[o].bumped = false;
                continue;
            }
            let r = a.collision_radius() + b.collision_radius();
            let (dx, dz) = (a.pos[0] - b.pos[0], a.pos[2] - b.pos[2]);
            let d = (dx * dx + dz * dz).sqrt();
            let level = !(a.height_test && b.height_test) || (a.pos[1] - b.pos[1]).abs() <= BUMP_HEIGHT;
            // Pushed back out, the player stands at the contact distance give or take rounding: walking on into
            // the other still touches it.
            let touching = level && d <= r + CONTACT_SLACK;
            let apart = !level || d > r;
            let was = self.actors[o].bumped;
            if walking && touching {
                if !was {
                    out.push(o);
                }
                self.actors[o].bumped = true;
            } else if apart || walking {
                self.actors[o].bumped = false;
            }
            if touching && d > 0.0 {
                let push = (r - d) / d;
                self.actors[p].pos[0] += dx * push;
                self.actors[p].pos[2] += dz * push;
            }
        }
        out
    }

    /// Start a talk when a talk button is pressed while the player has control and no message window is open.
    fn begin_talk(&mut self) {
        let pressed = self.input.confirm || self.input.alt;
        // Windows flagged 0x2000 (labels left on screen) do not hold talking up.
        self.talk =
            (pressed && self.user_control && !self.windows.iter().any(|w| w.open && w.flags & 0x2000 == 0))
                .then(|| Talk::new(if self.input.confirm { trigger::TALK } else { trigger::TALK_ALT }));
    }

    fn actor(&mut self, w: usize) -> &mut Actor {
        if self.actors.len() <= w {
            self.actors.resize(w + 1, Actor::default());
        }
        &mut self.actors[w]
    }

    /// Whether an actor's motion still plays (a looping one always does; one with an unknown length does not).
    fn motion_playing(&self, a: &Actor) -> bool {
        let Some((motion, start, end, done, loops)) = a.motion_play else { return false };
        if loops != 0 {
            return true;
        }
        let frames = if end >= 0 {
            (end - start).max(0) as u32
        } else {
            let len = a
                .model
                .and_then(|m| self.motion_frames.get(&m))
                .and_then(|f| f.get(&(motion as u32 as u16)))
                .copied()
                .unwrap_or(0);
            len.saturating_sub(start.max(0) as u32)
        };
        let ticks = frames * 0x100 / u32::from(a.motion_speed.max(1));
        done < ticks
    }

    /// Advance movements and turns by one tick, then player control.
    fn update(&mut self) {
        for a in &mut self.actors {
            if let Some(p) = &mut a.motion_play {
                p.3 += 1;
            }
            a.shade.tick();
            for c in &mut a.color {
                c.tick();
            }
            // Characters take their shade from the walk mesh where they stand, unless a script switched it off,
            // closing a third of the gap each tick (the original eases to it over three frames, re-aimed every
            // frame).
            if a.shade_from_ground
                && a.model.is_some()
                && let Some(s) = self.ground.as_ref().and_then(|g| g.shade_at(a.pos))
            {
                a.shade = Eased::new(a.shade.value + (s - a.shade.value) / 3.0);
            }
        }
        if self.fade != self.fade_target {
            let d = self.fade_target - self.fade;
            self.fade = if d.abs() <= self.fade_step {
                self.fade_target
            } else {
                self.fade + self.fade_step * d.signum()
            };
        }
        if let Some(m) = self.movie.as_mut().filter(|m| m.playing) {
            m.elapsed += 1;
            if let Some(f) = m.frames.filter(|&f| !m.looping && m.elapsed > f) {
                m.elapsed = f;
                m.playing = false;
            }
        }
        self.camera.update();
        let actors = &self.actors;
        if let Some((focus, eye)) = self.camera.follow.update(|w| actors.get(w).map(|a| a.pos)) {
            self.camera.focus = Some(focus);
            if eye.is_some() {
                self.camera.position = eye;
            }
        } else if let Some(eye) = self.camera.focus.and_then(|f| self.camera.follow.polar_eye(f)) {
            self.camera.position = Some(eye);
        }
        for w in &mut self.windows {
            w.closing = w.closing.saturating_sub(1);
            w.choice_live = w.choice_live.saturating_sub(1);
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
            w.shown = if w.instant { stop } else { (w.shown + TEXT_SPEED).min(stop) };
        }
        for (i, a) in self.actors.iter_mut().enumerate() {
            // Bodies not moving settle onto the nearest floor (cdsp0100: one placed under the floor comes up).
            if Some(i) != self.player
                && a.model.is_some()
                && a.move_state().active.is_none()
                && let Some(g) = self.ground.as_ref()
                && let Some(h) = g
                    .heights(a.pos[0], a.pos[2])
                    .min_by(|x, y| (x - a.pos[1]).abs().total_cmp(&(y - a.pos[1]).abs()))
            {
                a.pos[1] = h;
            }
            a.first_pass_done = false;
            if std::mem::take(&mut a.started_late) {
                continue;
            }
            let setting_at = |id: u16, key: usize| {
                self.settings.get(&(id, key)).and_then(|v| v.first()).map(|v| match v {
                    Value::Float(f) => *f,
                    Value::Int(x) => *x as f32,
                })
            };
            let setting = |id: u16| setting_at(id, i);
            let lvl = a.level;
            let level_setting = |id: u16| setting_at(id, level_key(i, lvl));
            let turn_runs = a.turn_level == a.level;
            let steering_turn = a.turning
                && turn_runs
                && a.turn_steps.is_none()
                && matches!(a.turn_mode & 0x1F, 1 | 2)
                && a.turn_mode & 0x8000 != 0
                && a.steered_walk();
            let steer = a.steer_walk_begin(
                level_setting(0x002B).or(setting(0x006D)).unwrap_or(0.0),
                setting(0x002C).or(setting(0x006E)).unwrap_or(0.0),
            );
            if steer.is_none() {
                a.advance_move(self.ground.as_ref());
            }
            if steering_turn {
                let rate = level_setting(if a.turn_mode & 0x200 != 0 { 0x00E3 } else { 0x002E });
                a.steer_turn(rate.unwrap_or(DEFAULT_TURN_RATE));
            } else if a.turning
                && turn_runs
                && let Some((step, left)) = &mut a.turn_steps
            {
                a.yaw = wrap_angle(a.yaw + *step);
                *left = left.saturating_sub(1);
                if *left == 0 {
                    a.yaw = a.turn_target;
                    a.turning = false;
                    a.turn_steps = None;
                }
            } else if a.turning && turn_runs {
                let step = if a.turn_speed[0] > 0.0 { a.turn_speed[0] } else { 0.2 };
                let d = wrap_angle(a.turn_target - a.yaw);
                if d.abs() <= step {
                    a.yaw = a.turn_target;
                    a.turning = false;
                } else {
                    a.yaw = wrap_angle(a.yaw + step.copysign(d));
                }
            }
            if let Some(st) = steer {
                a.steer_walk_end(st, self.ground.as_ref());
            }
        }
        // The player covers last tick's push, then this tick's input makes the next one (only while the
        // player has control).
        if let Some(p) = self.player.filter(|&p| p < self.actors.len()) {
            // With no floor under the player (in water), the ground does not hold the step back.
            let swimming =
                self.ground.as_ref().is_some_and(|g| !g.is_empty() && !g.standing(self.actors[p].pos));
            if let Some([dx, dz]) = self.player_step.take() {
                let a = &self.actors[p];
                let next = [a.pos[0] + dx, a.pos[1], a.pos[2] + dz];
                let next = match &self.ground {
                    // Blocked: slide along the wall on one axis.
                    Some(g) if !swimming => g
                        .slide(a.pos, next)
                        .or_else(|| g.slide(a.pos, [next[0], a.pos[1], a.pos[2]]))
                        .or_else(|| g.slide(a.pos, [a.pos[0], a.pos[1], next[2]])),
                    _ => Some(next),
                };
                if let Some(n) = next {
                    let a = &mut self.actors[p];
                    a.pos = n;
                    a.yaw = dz.atan2(dx);
                }
            }
            let push = self.control.tick(self.input.stick, self.input.walk, swimming);
            if let Some(push) = push.filter(|_| self.user_control) {
                // Forward is where the camera looks; right is a quarter turn clockwise from it seen from above.
                let h = self.input.camera_heading.unwrap_or(std::f32::consts::FRAC_PI_2);
                let (fwd, right) = ([h.cos(), h.sin()], [h.sin(), -h.cos()]);
                let [x, y] = push.dir;
                self.player_step =
                    Some([(right[0] * x + fwd[0] * y) * push.step, (right[1] * x + fwd[1] * y) * push.step]);
            }
        }
    }
}

/// Offset of the entrance the party arrived by in the save image.
const ENTRANCE_INDEX: usize = 0xC;

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
            let instant = starts_instant(&tokens);
            Message {
                text,
                raw: text::to_display(&tokens),
                choices: (word(i * 8 + 2) >> 8) as u8,
                reveal,
                instant,
            }
        })
        .collect()
}

/// The PS2 character data of a model id (`.chr`: its header carries the collision radius among other sizes).
pub fn chr_path(game: Game, id: u16) -> Option<String> {
    let p = model_path(game, id)?;
    let (_, rest) = p.split_once("/chr/")?;
    let (dir, _) = rest.split_once("/mdl/")?;
    let name = dir.rsplit('/').next()?;
    let root = match game {
        Game::Ffx => "ffx_ps2/ffx/master/jppc",
        Game::Ffx2 => "ffx_ps2/ffx2/master/jppc",
    };
    Some(format!("{root}/chr/{dir}/mdl/{name}.chr"))
}

/// A character's collision radius from its `.chr` data: header version (u32 at 4) 10 or later has an info block
/// (offset at 0x58) whose second float is the radius; older data takes 1.
pub fn chr_radius(d: &[u8]) -> Option<f32> {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if u32_at(4)? < 10 {
        return Some(1.0);
    }
    let info = u32_at(0x58)? as usize;
    u32_at(info + 4).map(f32::from_bits).filter(|r| r.is_finite() && *r > 0.0)
}

/// Frame counts of a model's motions by motion number (each group entry's first motion record).
pub fn motion_frames(
    vfs: &Vfs,
    model_path: &str,
    model: u16,
    event_path: Option<&str>,
) -> std::collections::HashMap<u16, u32> {
    let mut out = std::collections::HashMap::new();
    for group in motion_groups(vfs, model_path, model, event_path) {
        for (&(number, _), record) in group.entries.iter().zip(&group.entry_motion) {
            if let Some(m) = record.and_then(|r| group.motions.get(r)) {
                out.entry(number).or_insert(u32::from(m.frames));
            }
        }
    }
    out
}

/// The motion groups a character's motions come from: its own (`chr/…/mot/`, in file order) and then those of
/// the event's folder that belong to it (an event's motion files hold one group per character it animates).
pub fn motion_groups(
    vfs: &Vfs,
    model_path: &str,
    model: u16,
    event_path: Option<&str>,
) -> Vec<aerugo_formats::motion::MotionGroup> {
    use aerugo_formats::motion::{MotionGroup, motion_dir};
    let files = |dir: &str| {
        let mut paths: Vec<String> = vfs
            .archives()
            .flat_map(|(_, a)| {
                a.entries()
                    .iter()
                    .filter(|e| {
                        e.path.starts_with(dir)
                            && !e.path[dir.len()..].contains('/')
                            && e.path.ends_with(".mgrp")
                            && e.size > 16
                    })
                    .map(|e| e.path.clone())
                    .collect::<Vec<_>>()
            })
            .collect();
        paths.sort();
        paths.dedup();
        paths
    };
    let mut out = Vec::new();
    if let Some(dir) = motion_dir(model_path) {
        for path in files(&dir) {
            if let Some(groups) = vfs.read(&path).ok().and_then(|b| MotionGroup::parse_all(&b).ok()) {
                out.extend(groups);
            }
        }
    }
    if let Some((dir, _)) = event_path.and_then(|p| p.rsplit_once('/')) {
        for path in files(&format!("{dir}/")) {
            if let Some(groups) = vfs.read(&path).ok().and_then(|b| MotionGroup::parse_all(&b).ok()) {
                out.extend(groups.into_iter().filter(|g| g.chr_id == model));
            }
        }
    }
    out
}

/// A map's fog as its PS2 map package states it (the scene chunk of the map object data): colour, the limit
/// (0–255) and the near and far distances.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapFog {
    pub color: [u8; 3],
    pub limit: f32,
    pub near: f32,
    pub far: f32,
}

impl MapFog {
    /// From a `mapout.vpa` package: the section the `MAP1` header names at +0x14 is the map object data (a
    /// 0x40-byte header, then chunks of a 0x40-byte header — type, size — and their data); chunk 4 holds the
    /// scene values, with the fog colour bytes at +0xC and the limit, near and far as floats from +0x10.
    pub fn from_map_package(vpa: &[u8]) -> Option<MapFog> {
        let u32_at = |at: usize| vpa.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        let f32_at = |at: usize| u32_at(at).map(f32::from_bits);
        if !vpa.starts_with(b"MAP1") {
            return None;
        }
        let section = u32_at(0x14)? as usize;
        if u32_at(section)? != 0x0021_4365 {
            return None;
        }
        let mut at = section + 0x40;
        for _ in 0..4096 {
            let (kind, size) = (u32_at(at)?, u32_at(at + 4)? as usize);
            if kind == 4 {
                let d = at + 0x40;
                let c = vpa.get(d + 0xC..d + 0xF)?;
                return Some(MapFog {
                    color: [c[0], c[1], c[2]],
                    limit: f32_at(d + 0x10)?,
                    near: f32_at(d + 0x14)?,
                    far: f32_at(d + 0x18)?,
                });
            }
            if size == 0 {
                return None;
            }
            at += 0x40 + size;
        }
        None
    }
}

/// The PS2 map package holding the walk mesh for a drawn map (`…/map/<area>/<map>/…`).
pub fn walk_mesh_path(map_model: &str) -> Option<String> {
    let rest = map_model.split("/map/").nth(1)?;
    let mut parts = rest.split('/');
    let (area, map) = (parts.next()?, parts.next()?);
    Some(format!("ffx_ps2/ffx/master/jppc/map/{area}/{map}/bin/mapout.vpa"))
}

/// A menu request: the mode (0x40200000 save, 0x40100000 load, 0x4002_00nn item shop nn, 0x4004_00nn
/// weapon shop nn, 0x4001_xxxx name entry, ...) and whether the host has opened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuRequest {
    pub mode: u32,
    pub open: bool,
}

pub mod menu_mode {
    pub const SAVE: u32 = 0x4020_0000;
    pub const LOAD: u32 = 0x4010_0000;
    pub const ITEM_SHOP: u32 = 0x4002_0000;
    pub const WEAPON_SHOP: u32 = 0x4004_0000;
    pub const NAME_ENTRY: u32 = 0x4001_0000;
    /// The kind of a mode (its high half).
    pub fn kind(mode: u32) -> u32 {
        mode & 0xFFFF_0000
    }
}

/// A movie the scripts opened: its number and file, whether it plays (and loops), video frames shown so far
/// (one per tick while playing), and its length in frames once the host has read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Movie {
    pub number: u32,
    pub path: Option<String>,
    pub playing: bool,
    pub looping: bool,
    pub elapsed: u32,
    pub frames: Option<u32>,
}

/// Open a movie (pops flags, then the number): paused unless `playing`; flag 0x2000 loops it.
fn open_movie(g: &mut FieldState, c: &mut NativeCall<'_>, playing: bool) {
    let flags = c.stack.pop_int();
    let number = c.stack.pop_int().max(0) as u32;
    let path = g.videos.get(&number).cloned();
    let looping = flags & 0x2000 != 0;
    // A movie missing from the list counts as already over.
    let frames = if path.is_none() { Some(0) } else { None };
    g.movie = Some(Movie { number, path, playing, looping, elapsed: u32::from(playing), frames });
}

/// The game's video list (`N:,file,file` lines) as movie number → file name.
pub fn parse_video_list(text: &str) -> std::collections::HashMap<u32, String> {
    text.lines()
        .filter_map(|l| {
            let (n, rest) = l.split_once(':')?;
            let file = rest.trim_start_matches(',').split(',').next()?.trim();
            Some((n.trim().parse().ok()?, file.to_owned()))
        })
        .collect()
}

/// Name code: the character's name (argument 0x30 + character id).
const NAME_CODE: u8 = 0x13;

/// Pause code: the text waits for confirm before going on.
const PAUSE_CODE: u8 = 0x01;

/// What the text reveal goes through in a message: characters, names spelled out, and pauses.
/// Whether a text opens with the {09:30} code, which shows it at once.
fn starts_instant(tokens: &[text::Token]) -> bool {
    matches!(tokens.first(), Some(text::Token::Control { code: 0x09, arg: Some(0x30) }))
}

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
        let mut state = FieldState {
            actors,
            entrances: script.entrances(),
            data_areas: script.workers.iter().map(|w| w.data_areas).collect(),
            kinds: script.workers.iter().map(|w| w.kind).collect(),
            ..FieldState::default()
        };
        if let Some(tp) = event_text_path(game, path, lang)
            && let Ok(bytes) = vfs.read(&tp)
            && let Ok(cs) = vfs.read(&text::charset_path(game, lang)).map(|b| Charset::parse(&b))
            && let Ok(cs) = cs
        {
            state.messages = decode_event_text(&bytes, &cs);
        }
        let (map_model, map_layers) = map_paths(vfs, game, &name);
        if game == Game::Ffx {
            state.items = crate::items::ItemTables::load(vfs, lang);
            state.encounters = vfs
                .read("ffx_ps2/ffx/master/jppc/battle/kernel/btl.bin")
                .ok()
                .and_then(|d| crate::encounter::EncounterTable::parse(&d))
                .map(Arc::new);
        }
        // The video list (movie numbers to files).
        let region = if lang == Language::Japanese { "jp" } else { "us" };
        if let Ok(list) = vfs.read(&format!("ffx_data/gamedata/ps3data/video/{region}/ffx_videolist.txt")) {
            state.videos = parse_video_list(&String::from_utf8_lossy(&list))
                .into_iter()
                .map(|(n, f)| (n, format!("ffx_data/gamedata/ps3data/video/{}", f.to_lowercase())))
                .collect();
        }
        // The walk mesh from the PS2 map package next to the drawn map.
        // (Maps drawn as pre-rendered backgrounds have no 3D model but still have a walk mesh.)
        let vpa = map_model.as_deref().and_then(walk_mesh_path).or_else(|| {
            (game == Game::Ffx && name.len() >= 6 && name.is_ascii())
                .then(|| format!("ffx_ps2/ffx/master/jppc/map/{}/{}/bin/mapout.vpa", &name[..4], &name[..6]))
        });
        if let Some(d) = vpa.and_then(|p| vfs.read(&p).ok()) {
            state.ground = Ground::from_walk_mesh(&d);
            state.map_fog = MapFog::from_map_package(&d);
        }
        let mut natives = Natives::new(game);
        register(&mut natives);
        Ok(Event {
            game,
            name,
            path: path.to_owned(),
            vm: Vm::new(Arc::new(script)),
            natives,
            state,
            map_model,
            map_layers,
            tick: 0,
        })
    }

    /// One 30 Hz tick: scripts first, then movement and player control.
    pub fn step(&mut self, input: Input) {
        // Arriving: the entrance the save image names picks the entrance worker's entry (read on the first tick,
        // once a loaded or restored image is in place).
        if self.tick == 0 {
            let entrance = self.vm.memory.global[0].get(ENTRANCE_INDEX).copied().unwrap_or(0);
            self.vm.enter(usize::from(entrance));
        }
        self.state.input = input;
        let image = &self.vm.memory.global[0];
        if let Some(p) = image.get(PARTY_SLOTS..PARTY_SLOTS + 3) {
            self.state.party = [p[0], p[1], p[2]];
        }
        if let Some(pos) = self.state.player.and_then(|p| self.state.actors.get(p)).map(|a| a.pos) {
            self.state.player_prev = self.state.player_start.unwrap_or(pos);
        }
        // A battle starts at the start of the next frame and holds the field's scripts until the host ends it
        // (the original runs the battle's own script meanwhile; one field worker keeps going for the first four
        // frames, which is not modelled).
        if let Some((_, started)) = &mut self.state.battle {
            *started = true;
            return;
        }
        // A requested menu opens at the start of the next frame and holds the scripts until the host closes it.
        if let Some(m) = &mut self.state.menu {
            if !m.open {
                m.open = true;
                self.state.menu_result = 0;
            }
            return;
        }
        self.state.player_placed = false;
        self.state.begin_talk();
        let before = self.state.player.and_then(|p| self.state.actors.get(p)).map(|a| a.pos);
        self.vm.frame(&mut self.natives.host(&mut self.state));
        self.state.player_start = self.state.player.and_then(|p| self.state.actors.get(p)).map(|a| a.pos);
        self.state.update();
        self.walk_encounters(before);
        self.tick += 1;
    }

    /// Stop the playing movie (the player skipped it); scripts waiting for its end go on.
    pub fn skip_movie(&mut self) {
        if let Some(m) = &mut self.state.movie {
            m.playing = false;
        }
    }

    /// Random encounters: the player's walk this tick, when the area allows them (save image byte 0x13 bits 1–2,
    /// the area id at 4 or the one scripts set, the group at 0x10).
    fn walk_encounters(&mut self, before: Option<[f32; 3]>) {
        let s = &mut self.state;
        let image = &self.vm.memory.global[0];
        if s.battle.is_some()
            || s.encounters_off
            || !s.user_control
            || image.len() < 0x14
            || image[0x13] & 6 == 0
        {
            return;
        }
        let (Some(table), Some(before), Some(after)) =
            (s.encounters.clone(), before, s.player.and_then(|p| s.actors.get(p)).map(|a| a.pos))
        else {
            return;
        };
        let id = if s.encounter_area > 0 {
            s.encounter_area
        } else {
            i32::from(u16::from_le_bytes([image[4], image[5]]))
        };
        let Some(area) = table.area(id) else { return };
        let distance = before.iter().zip(after).map(|(a, b)| (b - a) * (b - a)).sum::<f32>().sqrt();
        let random = &mut s.battle_random;
        if let Some(b) =
            s.encounter_counters.walk(&table, area, usize::from(image[0x10]), distance, |k| random.next(k))
        {
            s.battle = Some((b, false));
        }
    }

    /// The battle the scripts are waiting on, once started.
    pub fn running_battle(&self) -> Option<crate::encounter::Battle> {
        self.state.battle.filter(|b| b.1).map(|b| b.0)
    }

    /// The name of a battle's scene ("area name" and formation, as the battle files are named).
    pub fn battle_name(&self, b: &crate::encounter::Battle) -> String {
        let area =
            self.state.encounters.as_ref().and_then(|t| t.areas.get(b.area)).map_or("?", |a| a.name.as_str());
        format!("{area}_{:02}", b.formation_id)
    }

    /// End the running battle (won); scripts go on with the next tick.
    pub fn finish_battle(&mut self) {
        self.state.battle = None;
        self.state.encounter_counters = Default::default();
    }

    /// The menu the scripts are waiting on (its mode), once open.
    pub fn open_menu(&self) -> Option<u32> {
        self.state.menu.filter(|m| m.open).map(|m| m.mode)
    }

    /// Close the open menu; scripts go on with the next tick.
    pub fn close_menu(&mut self) {
        self.state.menu = None;
    }

    /// The file of a started movie whose length the host still has to give (with `set_movie_frames`).
    pub fn movie_needing_length(&self) -> Option<&str> {
        self.state.movie.as_ref().filter(|m| m.frames.is_none()).and_then(|m| m.path.as_deref())
    }

    pub fn set_movie_frames(&mut self, frames: u32) {
        if let Some(m) = &mut self.state.movie {
            m.frames = Some(frames);
        }
    }

    /// Load the frame counts of the motions of models that appeared since the last call (hosts call it before each
    /// tick).
    pub fn load_motion_frames(&mut self, vfs: &Vfs) {
        let models: Vec<u16> = self.state.actors.iter().filter_map(|a| a.model).collect();
        for m in models {
            if self.state.motion_frames.contains_key(&m) {
                continue;
            }
            let frames = model_path(self.game, m)
                .map(|p| motion_frames(vfs, &p, m, Some(&self.path)))
                .unwrap_or_default();
            self.state.motion_frames.insert(m, frames);
            let radius = chr_path(self.game, m).and_then(|p| vfs.read(&p).ok()).and_then(|d| chr_radius(&d));
            self.state.model_radii.insert(m, radius);
        }
        for a in &mut self.state.actors {
            a.model_radius = a.model.and_then(|m| self.state.model_radii.get(&m).copied().flatten());
        }
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

/// Natives that set a value for later (an actor's, a worker's or the field's) and give back what the original
/// returns; the values are kept by (native, worker) for the parts of the engine not ported yet.
fn register_settings(n: &mut Natives<FieldState>) {
    fn keep(g: &mut FieldState, id: u16, w: usize, values: Vec<Value>) {
        g.settings.insert((id, w), values);
    }
    // One float, returned (worker settings: 002d, 006d–0071, 00e4).
    macro_rules! float_back {
        ($($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                let v = c.stack.pop_float();
                keep(g, $id, c.worker, vec![Value::Float(v)]);
                Value::Float(v)
            }));
        )*};
    }
    float_back!(0x006D, 0x006E, 0x006F, 0x0070, 0x0071, 0x00E4);
    // One int, returned (0074 worker setting, 00bb/00bc/00f3 field settings, 0116 actor flag).
    macro_rules! int_back {
        ($($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                let v = c.stack.pop_int();
                keep(g, $id, c.worker, vec![Value::Int(v)]);
                Value::Int(v)
            }));
        )*};
    }
    int_back!(0x0074, 0x00BB, 0x00BC, 0x00F3, 0x017A);
    // One float kept, nothing useful returned (per-level move settings: 002e, 002f, 0030).
    macro_rules! float_set {
        ($($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                let v = c.stack.pop_float();
                keep(g, $id, c.worker, vec![Value::Float(v)]);
                Value::Int(0)
            }));
        )*};
    }
    float_set!(0x002F, 0x0030, 0x002C);
    // Per-level turn and steering rates (002e the turn rate, 00e3 the other one, 002b the steering rate): kept
    // by worker and level.
    macro_rules! level_float_set {
        ($($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                let v = c.stack.pop_float();
                keep(g, $id, level_key(c.worker, c.level), vec![Value::Float(v)]);
                Value::Int(0)
            }));
        )*};
    }
    level_float_set!(0x002E, 0x00E3, 0x002B);
    // The calling worker's facing (wrapped).
    n.register(
        0x0025,
        Native::result(|g, c| {
            let v = wrap_angle(c.stack.pop_float());
            g.actor(c.worker).yaw = v;
            Value::Int(0)
        }),
    );
    // One int kept, 1 returned (0007, 0151).
    macro_rules! int_one {
        ($($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                let v = c.stack.pop_int();
                keep(g, $id, c.worker, vec![Value::Int(v)]);
                Value::Int(1)
            }));
        )*};
    }
    int_one!(0x0007, 0x0151);
    // The entrance and the area id in the save image.
    n.register(
        0x0010,
        Native::result(|_, c| Value::Int(i32::from(c.memory.global[0].get(0xC).copied().unwrap_or(0)))),
    );
    n.register(
        0x0086,
        Native::result(|_, c| {
            let b = c.memory.global[0].get(4..6).map_or([0, 0], |b| [b[0], b[1]]);
            Value::Int(i32::from(u16::from_le_bytes(b)))
        }),
    );
    // Two words into the save image at 0xb4 and 0xb6.
    n.register(
        0x016E,
        Native::result(|_, c| {
            let b = c.stack.pop_int() as u16;
            let a = c.stack.pop_int() as u16;
            if let Some(d) = c.memory.global[0].get_mut(0xB4..0xB8) {
                d[0..2].copy_from_slice(&a.to_le_bytes());
                d[2..4].copy_from_slice(&b.to_le_bytes());
            }
            Value::Int(1)
        }),
    );
    // Two field switches; returns the first.
    n.register(
        0x0114,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            keep(g, 0x0114, 0, vec![Value::Int(a), Value::Int(b)]);
            Value::Int(i32::from(a != 0))
        }),
    );
    // An actor's value (as on|0x80000000, or 0), returned.
    n.register(
        0x0148,
        Native::result(|g, c| {
            let chr = c.stack.pop_int();
            let v = c.stack.pop_int() as u32;
            let stored = if v != 0 { v | 0x8000_0000 } else { 0 } as i32;
            keep(g, 0x0148, chr.max(0) as usize, vec![Value::Int(stored)]);
            Value::Int(stored)
        }),
    );
    // A window's style byte; returns the window.
    n.register(
        0x0066,
        Native::result(|g, c| {
            let style = c.stack.pop_int();
            let w = window(c);
            g.windows[w].style = Some(style as u8);
            Value::Int(w as i32)
        }),
    );
}

/// Natives that only hand values to parts of the engine Aerugo does not draw or simulate yet (actor drawing and
/// motion blending, screen effects, map layer animation): they take their arguments (by the native's arity),
/// keep them, and return what the original returns.
fn register_engine_settings(n: &mut Natives<FieldState>) {
    fn take(g: &mut FieldState, c: &mut NativeCall<'_>, id: u16) -> Vec<Value> {
        let (ints, floats) = aerugo_formats::script::arity(Game::Ffx, id).map_or((0, 0), |a| (a.0, a.1));
        let mut v: Vec<Value> = (0..u32::from(ints) + u32::from(floats)).map(|_| c.stack.pop()).collect();
        v.reverse();
        g.settings.insert((id, c.worker), v.clone());
        v
    }
    // Returning 0.
    macro_rules! zero {
        ($($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                take(g, c, $id);
                Value::Int(0)
            }));
        )*};
    }
    zero!(
        0x500B, 0x500E, 0x5013, 0x5014, 0x5017, 0x5020, 0x5034, 0x5057, 0x505C, 0x400A, 0x4017, 0x4019,
        0x401A, 0x403B, 0x6003, 0x6021, 0x5001, 0x5010
    );
    // Field light: index (0–2 directional, 3 ambient), colour 0–255, yaw and pitch in degrees.
    n.register(
        0x4013,
        Native::result(|g, c| {
            let pitch = c.stack.pop_int();
            let yaw = c.stack.pop_int();
            let b = c.stack.pop_int();
            let gr = c.stack.pop_int();
            let r = c.stack.pop_int();
            if let Some(l) = usize::try_from(c.stack.pop_int()).ok().and_then(|i| g.lights.0.get_mut(i)) {
                *l = FieldLight::from_script(r, gr, b, yaw, pitch);
            }
            Value::Int(0)
        }),
    );
    // Fog near, far and limit (0–255) for the remaster's shaders. (The original skips them on three maps,
    // ids 0xCA2, 0x1E38 and 0x699, not checked here.)
    n.register(
        0x8037,
        Native::result(|g, c| {
            let limit = c.stack.pop_float();
            g.fog.far = Some(c.stack.pop_float());
            g.fog.near = Some(c.stack.pop_float());
            g.fog.limit = (limit / 255.0).clamp(0.0, 1.0);
            g.fog.curve = Some(1.0);
            Value::Int(1)
        }),
    );
    n.register(
        0x8038,
        Native::result(|g, c| {
            g.fog.near = Some(c.stack.pop_float());
            Value::Int(1)
        }),
    );
    n.register(
        0x803A,
        Native::result(|g, c| {
            g.fog.far = Some(c.stack.pop_float());
            Value::Int(1)
        }),
    );
    n.register(
        0x803C,
        Native::result(|g, c| {
            g.fog.limit = (c.stack.pop_float() / 255.0).clamp(0.0, 1.0);
            Value::Int(1)
        }),
    );
    // A character's shade (float; the original eases to it over the given frames, not modelled).
    n.register(
        0x5088,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            let shade = c.stack.pop_float();
            let w = c.worker;
            g.actor(w).shade.set(shade, frames);
            Value::Int(0)
        }),
    );
    // Whether a character's shade follows the walk mesh.
    n.register(
        0x5060,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let w = c.worker;
            g.actor(w).shade_from_ground = on;
            Value::Int(0)
        }),
    );
    // A character's colour (r, g, b) and alpha, each eased over the given frames.
    n.register(
        0x5015,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            let b = c.stack.pop_float();
            let gr = c.stack.pop_float();
            let r = c.stack.pop_float();
            let w = c.worker;
            let a = g.actor(w);
            for (k, v) in [r, gr, b].into_iter().enumerate() {
                a.color[k].set(v, frames);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5033,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            let alpha = c.stack.pop_float();
            let w = c.worker;
            g.actor(w).color[3].set(alpha, frames);
            Value::Int(0)
        }),
    );
    // Field light colour only (0–1 floats); no FFX event calls it.
    n.register(
        0x4012,
        Native::result(|g, c| {
            let b = c.stack.pop_float();
            let gr = c.stack.pop_float();
            let r = c.stack.pop_float();
            if let Some(l) = usize::try_from(c.stack.pop_int()).ok().and_then(|i| g.lights.0.get_mut(i)) {
                l.color = [r, gr, b];
            }
            Value::Int(0)
        }),
    );
    // Body hit off (1) or on (0): characters without it neither block nor bump.
    n.register(
        0x500F,
        Native::result(|g, c| {
            let off = c.stack.pop_int() != 0;
            let w = c.worker;
            g.actor(w).no_body_hit = off;
            Value::Int(0)
        }),
    );
    n.register(
        0x4034,
        Native::result(|g, c| {
            take(g, c, 0x4034);
            Value::Float(0.0)
        }),
    );
    n.register(
        0x8009,
        Native::result(|g, c| {
            take(g, c, 0x8009);
            Value::Int(1)
        }),
    );
    // Map layer animations (on/off by the second argument): return it.
    macro_rules! second {
        ($($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                let v = take(g, c, $id);
                v.get(1).copied().unwrap_or(Value::Int(0))
            }));
        )*};
    }
    second!(0x8002, 0x800D);
    // Map switch: returns the first argument.
    n.register(
        0x8000,
        Native::result(|g, c| {
            let v = take(g, c, 0x8000);
            v.first().copied().unwrap_or(Value::Int(0))
        }),
    );
}

/// Natives whose effects are on systems not ported yet (sound banks, effects, model parts, screen) and whose
/// results are fixed or follow from the arguments (third sweep).
fn register_fixed_results(n: &mut Natives<FieldState>) {
    fn take(g: &mut FieldState, c: &mut NativeCall<'_>, id: u16) -> Vec<Value> {
        let (ints, floats) = aerugo_formats::script::arity(Game::Ffx, id).map_or((0, 0), |a| (a.0, a.1));
        let mut v: Vec<Value> = (0..u32::from(ints) + u32::from(floats)).map(|_| c.stack.pop()).collect();
        v.reverse();
        g.settings.insert((id, c.worker), v.clone());
        v
    }
    fn int(v: &[Value], i: usize) -> i32 {
        match v.get(i) {
            Some(Value::Int(x)) => *x,
            Some(Value::Float(f)) => *f as i32,
            _ => 0,
        }
    }
    macro_rules! fixed {
        ($ret:expr; $($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                take(g, c, $id);
                Value::Int($ret)
            }));
        )*};
    }
    fixed!(0; 0x6076, 0x603F, 0x6041, 0x6044, 0x6045, 0x6060, 0x01E2, 0x01E3, 0x01E4, 0x01E5, 0x0119, 0x0004);
    // Camera shakes, offsets and moves the follow camera does not take yet; map layer switches; 00a3 (effects).
    fixed!(0; 0x606B, 0x6071, 0x6081, 0x6082, 0x6083, 0x6084, 0x605C, 0x605D);
    fixed!(1; 0x801D, 0x801E, 0x801F);
    // Character data disposal, binding and motion switches whose results scripts do not branch on (the recordings
    // show these constants).
    fixed!(0; 0x0002, 0x0005, 0x00D1, 0x0197, 0x5087, 0x5016, 0x504E);
    fixed!(1; 0x0196, 0x0198, 0x01C9);
    // The memory card check and the title (memochek, test20): save data present, the answers the recordings show
    // (01c6 1, 0243 3, 01c5 0, 0253 1), so a new game goes on to the title.
    fixed!(1; 0x01C6, 0x0253);
    fixed!(3; 0x0243);
    fixed!(0; 0x400E);
    // Reading the save dictionary (01c5): done at once but the worker's turn ends there; nothing to report (0).
    n.register(
        0x01C5,
        Native {
            init: Some(|_, c| {
                c.stack.pop_int();
                c.stack.pop_int();
            }),
            wait: Some(|_, _| WAIT_DONE),
            result: Some(|_, _| Value::Int(0)),
        },
    );
    // Results that echo an argument: 00cb its value, 00e7 its second as a byte.
    n.register(0x00CB, Native::result(|g, c| Value::Int(int(&take(g, c, 0x00CB), 0))));
    n.register(0x00E7, Native::result(|g, c| Value::Int(int(&take(g, c, 0x00E7), 1) & 0xFF)));
    // Character switches that return the value they replace (1 at first): 5009 the push weight (float), 502a,
    // 5029.
    macro_rules! swap {
        ($($id:expr),*) => {$(
            n.register($id, Native::result(|g, c| {
                let old = g.settings.get(&($id, c.worker)).and_then(|v| v.first().copied());
                take(g, c, $id);
                match old {
                    Some(v) => v,
                    None if $id == 0x5009 => Value::Float(1.0),
                    None => Value::Int(1),
                }
            }));
        )*};
    }
    swap!(0x5009, 0x502A, 0x5029);
    fixed!(1; 0x8010, 0x8030);
    // A message variable (window, slot, kind, value) for the window's text; returns the window.
    n.register(0x009E, Native::result(|g, c| Value::Int(int(&take(g, c, 0x009E), 0))));
    // Play a sound (returns the first argument).
    n.register(0x00FD, Native::result(|g, c| Value::Int(int(&take(g, c, 0x00FD), 0))));
    // The calling worker's facing.
    n.register(0x0091, Native::result(|g, c| Value::Float(g.actor(c.worker).yaw)));
    // A zone or line worker's mode bits (other kinds: nothing, 0).
    n.register(
        0x007F,
        Native::result(|g, c| {
            take(g, c, 0x007F);
            Value::Int(i32::from(matches!(c.kind, 2 | 3 | 5 | 6)))
        }),
    );
    // The highlighted choice of a choice window whose text is all shown (or that is closing), else -1.
    n.register(
        0x00FB,
        Native::result(|g, c| {
            let w = window(c);
            let win = &g.windows[w];
            let choices = win.message.and_then(|m| g.messages.get(m)).map_or(0, |m| m.choices);
            let ready = (win.open && win.opening == 0 && win.shown >= win.reveal) || win.choice_live > 0;
            Value::Int(if ready && choices > 0 { win.choice as i32 } else { -1 })
        }),
    );
    // A worker's position into three float variables (worker, x, y, z descriptors); a line's midpoint for
    // line workers (kinds 2 and 5).
    n.register(
        0x00A3,
        Native::result(|g, c| {
            let z = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let w = target(c);
            let kind = g.kinds.get(w).copied().unwrap_or(0);
            let a = g.actors.get(w).cloned().unwrap_or_default();
            let p = if matches!(kind, 2 | 5) {
                std::array::from_fn(|k| (a.pos[k] + a.trigger.b[k]) * 0.5)
            } else {
                a.pos
            };
            let areas = g.data_areas.get(c.worker).copied().unwrap_or_default();
            for (desc, v) in [(x, p[0]), (y, p[1]), (z, p[2])] {
                store_float_var(c, areas, desc, v);
            }
            Value::Int(0)
        }),
    );
    // Height test of the calling worker's body in collisions on or off.
    n.register(
        0x5073,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.actor(c.worker).height_test = v != 0;
            Value::Int(0)
        }),
    );
    // A body's position (worker, then three variable descriptors for x, y, z); nothing for a worker with none.
    n.register(
        0x5037,
        Native::result(|g, c| {
            let z = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let w = c.stack.pop_int().max(0) as usize;
            if let Some(a) = g.actors.get(w).filter(|a| a.model.is_some()).cloned() {
                let areas = g.data_areas.get(c.worker).copied().unwrap_or_default();
                for (desc, v) in [(x, a.pos[0]), (y, a.pos[1]), (z, a.pos[2])] {
                    store_float_var(c, areas, desc, v);
                }
            }
            Value::Int(0)
        }),
    );
    // The ground distance (x, z) between two workers.
    n.register(
        0x00A5,
        Native::result(|g, c| {
            let b = target(c);
            let a = target(c);
            let pos = |w: usize| g.actors.get(w).map_or([0.0; 3], |x| x.pos);
            let (p, q) = (pos(a), pos(b));
            let (dx, dz) = (f64::from(q[0] - p[0]), f64::from(q[2] - p[2]));
            Value::Float((dx * dx + dz * dz).sqrt() as f32)
        }),
    );
    fixed!(1; 0x019C, 0x01EB, 0x0103, 0x0104, 0x01CE, 0x01CF, 0x0205, 0x0209, 0x01D0, 0x022E);
    // Switches that return whether they were turned on: the only argument (013e), the second (01d4), the int of
    // (int, float) (c02f).
    n.register(0x013E, Native::result(|g, c| Value::Int(i32::from(int(&take(g, c, 0x013E), 0) != 0))));
    n.register(0x01D4, Native::result(|g, c| Value::Int(i32::from(int(&take(g, c, 0x01D4), 1) != 0))));
    n.register(0xC02F, Native::result(|g, c| Value::Int(i32::from(int(&take(g, c, 0xC02F), 0) != 0))));
    // Returns its int argument (01e6: int, float, float).
    n.register(0x01E6, Native::result(|g, c| Value::Int(int(&take(g, c, 0x01E6), 0))));
    // (0102/0105 load sound banks and wait for the loader: replayed until sound loading is ported.)
    // Wait n ticks (the call ends the worker's turn).
    n.register(
        0x01D1,
        Native {
            init: Some(|g, c| {
                c.scratch[0] = int(&take(g, c, 0x01D1), 0);
            }),
            wait: Some(|_, c| {
                c.scratch[0] -= 1;
                if c.scratch[0] < 1 { WAIT_DONE } else { 0 }
            }),
            result: None,
        },
    );
}

/// Worker switches and the player.
fn register_worker_switches(n: &mut Natives<FieldState>) {
    // A worker switch (bit 0x200 of its flags): on/off by the argument (0031: on); returns the new state.
    n.register(
        0x0060,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            g.settings.insert((0x0060, c.worker), vec![Value::Int(i32::from(on))]);
            Value::Int(i32::from(on))
        }),
    );
    n.register(
        0x0031,
        Native::result(|g, c| {
            g.settings.insert((0x0060, c.worker), vec![Value::Int(1)]);
            Value::Int(1)
        }),
    );
    // The calling worker becomes the one the player controls; returns its number.
    n.register(
        0x0043,
        Native::result(|g, c| {
            g.player = Some(c.worker);
            Value::Int(c.worker as i32)
        }),
    );
}

/// Store a float into the variable a descriptor names (type bits 28–31, area 25–27, offset 0–23), as the calling
/// worker sees it (the shared area is not reachable from here and is left alone).
fn store_float_var(c: &mut NativeCall<'_>, areas: [u32; 3], desc: i32, v: f32) {
    use aerugo_formats::script::{Var, VarType};
    let d = desc as u32;
    let var = Var {
        ty: VarType::from_nibble((d >> 28) as u8),
        area: ((d >> 25) & 7) as u8,
        offset: d & 0xFF_FFFF,
        count: 1,
    };
    if var.area == 6 {
        return;
    }
    let place = c.memory.place(&var, 0, c.worker, areas, 0);
    c.memory.store_float(place, var.ty, v, false);
}

/// Open a window on its message: the text reveal starts over.
fn open_window(g: &mut FieldState, image: &[u8], w: usize, instant: bool) {
    let text_instant = g.windows[w].message.and_then(|m| g.messages.get(m)).is_some_and(|m| m.instant);
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
    win.choice = usize::from(win.first_choice);
    win.opening = OPEN_TICKS;
    win.closing = 0;
    win.instant = instant || text_instant;
}

/// A message made at run time (not from the event's text).
fn runtime_message(text: &str) -> Message {
    let tokens: Vec<text::Token> =
        text.chars().map(|c| if c == '\n' { text::Token::LineBreak } else { text::Token::Char(c) }).collect();
    Message {
        text: text.to_owned(),
        raw: text.to_owned(),
        choices: 0,
        reveal: reveal_parts(&tokens),
        instant: true,
    }
}

/// Treasure: give treasure `index` (the item, key item, equipment or gil) into the save image. Returns whether
/// the equipment it held did not fit.
fn give_treasure(g: &FieldState, image: &mut [u8], index: i32) -> (crate::items::Treasure, bool) {
    use crate::items::{Treasure, give};
    let got = g.items.as_ref().map_or(Treasure::Gil(0), |t| t.treasure(index.max(0) as u16));
    let full = !give(image, &got);
    (got, full)
}

/// Menus, the save sphere's record of where the player saved, screen fades, choice windows.
fn register_menus(n: &mut Natives<FieldState>) {
    // Ask for a menu by mode; the call returns two ticks later (after the menu, which holds the scripts) with
    // the menu's result.
    n.register(
        0x401D,
        Native {
            init: Some(|g, c| {
                let mode = c.stack.pop_int() as u32;
                g.menu = Some(MenuRequest { mode, open: false });
                g.menu_result = -1;
                c.scratch[0] = 1;
            }),
            wait: Some(|_, c| {
                if c.scratch[0] == 0 {
                    return WAIT_DONE;
                }
                c.scratch[0] -= 1;
                0
            }),
            result: Some(|g, _| Value::Int(g.menu_result)),
        },
    );
    // Save sphere: note its number and the place (the first 14 bytes of the save image: map, entrance, ...)
    // that loading a save made here returns to.
    n.register(
        0x016F,
        Native::result(|_, c| {
            let number = c.stack.pop_int();
            let image = &mut c.memory.global[0];
            if image.len() >= 0xD0 {
                let room = [image[0], image[1]];
                image[0xB8..0xBA].copy_from_slice(&(number as u16).to_le_bytes());
                image[0xBA..0xBC].copy_from_slice(&room);
                let (words, b12, b13) = (image[0..12].to_vec(), image[12], image[13]);
                image[0xC0..0xCC].copy_from_slice(&words);
                image[0xCC..0xCE].copy_from_slice(&u16::from(b12).to_le_bytes());
                image[0xCE..0xD0].copy_from_slice(&u16::from(b13).to_le_bytes());
            }
            Value::Int(number)
        }),
    );
    // The switch scripts turn off around their menus.
    n.register(
        0x0241,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            g.menu_switch_off = !on;
            Value::Int(i32::from(on))
        }),
    );
    // Fade the screen in (4004) or out to black (4005) over n ticks.
    fn fade(g: &mut FieldState, c: &mut NativeCall<'_>, target: f32) -> Value {
        let ticks = c.stack.pop_int().max(1) as f32;
        g.fade_target = target;
        g.fade_step = 1.0 / ticks;
        Value::Int(0)
    }
    n.register(0x4004, Native::result(|g, c| fade(g, c, 0.0)));
    n.register(0x4005, Native::result(|g, c| fade(g, c, 1.0)));
    // A choice window in one call (window, message, ?, ?, x, y, alignment): placed, opened, and waited on; the
    // result is the choice.
    for id in [0x013B, 0x0267] {
        n.register(
            id,
            Native {
                init: Some(|g, c| {
                    let _align = c.stack.pop_int();
                    let y = c.stack.pop_int();
                    let x = c.stack.pop_int();
                    let cancel = c.stack.pop_int();
                    let first = c.stack.pop_int();
                    let msg = c.stack.pop_int();
                    let w = window(c);
                    g.windows[w].first_choice = first as u8;
                    g.windows[w].cancel_choice = cancel as u8;
                    g.windows[w].style = Some(0);
                    g.windows[w].message = usize::try_from(msg).ok();
                    g.windows[w].x = x;
                    g.windows[w].y = y;
                    g.windows[w].flags = 0;
                    open_window(g, &c.memory.global[0], w, true);
                    c.scratch[0] = w as i32;
                    c.scratch[1] = 0;
                    c.scratch[2] = 0;
                    c.scratch[3] = 1;
                }),
                wait: Some(choice_wait),
                ..message_wait()
            },
        );
    }
    n.register(
        0x007D,
        Native {
            init: Some(|_, c| {
                c.scratch[0] = window(c) as i32;
                c.scratch[1] = 0;
                c.scratch[2] = 0;
                c.scratch[3] = 0;
            }),
            wait: Some(choice_wait),
            ..message_wait()
        },
    );
    // Battles: a scripted battle by label (area id << 16 | formation), starting when the frame ends; the
    // encounter area scripts set in place of the save image's (the walking counters start over); whether the
    // save image allows encounters.
    // (The call ends the worker's turn.)
    n.register(
        0x7002,
        Native {
            init: Some(|g, c| {
                let _flag = c.stack.pop_int();
                let label = c.stack.pop_int() as u32;
                if let Some(b) = g.encounters.as_ref().and_then(|t| t.label(label)) {
                    g.battle = Some((b, false));
                }
            }),
            wait: Some(|_, _| WAIT_DONE),
            result: None,
        },
    );
    n.register(
        0x020F,
        Native::result(|g, c| {
            g.encounter_area = c.stack.pop_int();
            g.encounter_counters = Default::default();
            let image = &c.memory.global[0];
            let saved = image.get(4..6).map_or(0, |b| i32::from(u16::from_le_bytes([b[0], b[1]])));
            Value::Int(if g.encounter_area > 0 { g.encounter_area } else { saved })
        }),
    );
    n.register(
        0x00E1,
        Native::result(|_, c| {
            Value::Int(i32::from(c.memory.global[0].get(0x13).copied().unwrap_or(0) >> 1 & 1))
        }),
    );
    // Gil: receive, pay (false when there is not enough), the shops' price rate.
    n.register(
        0x0136,
        Native::result(|_, c| {
            let n = c.stack.pop_int();
            Value::Int(i32::from(crate::items::add_gil(&mut c.memory.global[0], n)))
        }),
    );
    n.register(
        0x0137,
        Native::result(|_, c| {
            let n = c.stack.pop_int();
            Value::Int(i32::from(crate::items::add_gil(&mut c.memory.global[0], -n)))
        }),
    );
    n.register(
        0x0225,
        Native::result(|g, c| {
            let rate = c.stack.pop_int();
            g.shop_rate = Some(rate);
            Value::Int(rate)
        }),
    );
}

/// The pad word the scripts read this tick.
fn pad_word(g: &FieldState) -> u32 {
    if g.input.pad != 0 {
        return g.input.pad;
    }
    let i = &g.input;
    let mut w = 0;
    w |= if i.confirm { 0x20 } else { 0 };
    w |= if i.cancel { 0x40 } else { 0 };
    w |= if i.alt { 0x80 } else { 0 };
    if i.stick_pressed {
        w |= if i.stick[1] > 0.0 {
            0x1000
        } else if i.stick[1] < 0.0 {
            0x4000
        } else {
            0
        };
    }
    w
}

/// The held-buttons word the scripts read this tick.
fn held_word(g: &FieldState) -> u32 {
    if g.input.held != 0 || g.input.pad != 0 { g.input.held } else { pad_word(g) }
}

/// Queries and settings about a worker's move, angles and the pad.
fn register_queries(n: &mut Natives<FieldState>) {
    // Close a window; with the flag, wait until it has closed. The result is 1.
    n.register(
        0x01E9,
        Native {
            init: Some(|g, c| {
                c.scratch[1] = c.stack.pop_int();
                let w = c.stack.pop_int().clamp(0, 7);
                c.scratch[0] = w;
                g.windows[w as usize].close();
            }),
            wait: Some(|g, c| {
                let w = &g.windows[c.scratch[0] as usize];
                if c.scratch[1] == 0 || (!w.open && w.closing == 0) { WAIT_DONE } else { 0 }
            }),
            result: Some(|_, _| Value::Int(1)),
        },
    );
    use aerugo_script::natives::heading;
    // Heading from the worker to its move target / to a point (x, y, z), on the ground plane.
    n.register(
        0x001F,
        Native::result(|g, c| {
            let level = c.level;
            let a = g.actor(c.worker);
            let t = a.move_at(level).target;
            Value::Float(heading(a.pos[0], a.pos[2], t[0], t[2]))
        }),
    );
    n.register(
        0x00F4,
        Native::result(|g, c| {
            let [x, _, z] = pop3(c);
            let a = g.actor(c.worker);
            Value::Float(heading(a.pos[0], a.pos[2], x, z))
        }),
    );
    // Pitch towards the move target: the heading of (ground distance, height) from (0, own height).
    n.register(
        0x0020,
        Native::result(|g, c| {
            let level = c.level;
            let a = g.actor(c.worker);
            let t = a.move_at(level).target;
            let d = ((t[0] - a.pos[0]).powi(2) + (t[2] - a.pos[2]).powi(2)).sqrt();
            Value::Float(heading(0.0, a.pos[1], d, t[1]))
        }),
    );
    // The heading moves go along: set, read.
    n.register(
        0x0023,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            g.actor(c.worker).move_heading = v;
            Value::Float(v)
        }),
    );
    n.register(0x0090, Native::result(|g, c| Value::Float(g.actor(c.worker).move_heading)));
    // Target pitch of a turn (wrapped; turns in pitch are not modelled).
    n.register(0x0029, Native::result(|_, c| Value::Float(wrap_angle(c.stack.pop_float()))));
    // Whether the current level's move is under way.
    n.register(
        0x00C4,
        Native::result(|g, c| {
            let level = c.level;
            Value::Int(i32::from(g.actor(c.worker).move_at(level).active.is_some()))
        }),
    );
    // Whether worker w is within r of this one (in space).
    n.register(
        0x0128,
        Native::result(|g, c| {
            let r = c.stack.pop_float();
            let w = c.stack.pop_int().max(0) as usize;
            let b = g.actor(w).pos;
            let a = g.actor(c.worker).pos;
            let d2 = (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2);
            Value::Int(i32::from(d2 <= r * r))
        }),
    );
    // Facing test: whether (x1, z1) lies within `half` of `yaw` seen from (x0, z0).
    n.register(
        0x1015,
        Native::result(|_, c| {
            let z1 = c.stack.pop_float();
            let x1 = c.stack.pop_float();
            let z0 = c.stack.pop_float();
            let x0 = c.stack.pop_float();
            let half = c.stack.pop_float();
            let yaw = c.stack.pop_float();
            Value::Int(i32::from(wrap_angle(heading(x0, z0, x1, z1) - yaw).abs() < half))
        }),
    );
    // Stop worker w's turn where it is.
    n.register(
        0x0078,
        Native::result(|g, c| {
            let w = c.stack.pop_int().max(0) as usize;
            let a = g.actor(w);
            a.turning = false;
            a.turn_steps = None;
            Value::Int(0)
        }),
    );
    // The pad: the word, one bit of it, and a wait until the given bits are let go.
    n.register(0x0046, Native::result(|g, _| Value::Int((pad_word(g) & 0xFFFF) as i32)));
    n.register(
        0x004D,
        Native::result(|g, c| {
            let bit = c.stack.pop_int() as u32 & 31;
            Value::Int(i32::from(pad_word(g) & 0xFFFF & (1 << bit) != 0))
        }),
    );
    n.register(
        0x022D,
        Native {
            init: Some(|_, c| c.scratch[0] = c.stack.pop_int()),
            wait: Some(|g, c| if pad_word(g) & (c.scratch[0] as u32) & 0xFFFF == 0 { WAIT_DONE } else { 0 }),
            result: None,
        },
    );
    // Buttons held: the word and one bit of it; one bit of the pressed word as the pad gives it.
    n.register(0x0044, Native::result(|g, _| Value::Int((held_word(g) & 0xFFFF) as i32)));
    n.register(
        0x004C,
        Native::result(|g, c| {
            let bit = c.stack.pop_int() as u32 & 31;
            Value::Int(i32::from(held_word(g) & 0xFFFF & (1 << bit) != 0))
        }),
    );
    n.register(
        0x0051,
        Native::result(|g, c| {
            let bit = c.stack.pop_int() as u32 & 31;
            Value::Int(i32::from(pad_word(g) & 0xFFFF & (1 << bit) != 0))
        }),
    );
    // The stick's angle from its two bytes (centre 0x80), 0 inside the dead zone (and 1e-6 for an angle of 0).
    n.register(
        0x014E,
        Native::result(|_, c| {
            let y = c.stack.pop_int() - 0x80;
            let x = c.stack.pop_int() - 0x80;
            let dead = c.stack.pop_int();
            if x * x + y * y < dead * dead {
                return Value::Float(0.0);
            }
            let a = (y as f32).atan2(x as f32);
            Value::Float(if a == 0.0 { 1e-6 } else { a })
        }),
    );
    // Whether the player has control (or the scripts steer another worker).
    n.register(
        0x019F,
        Native::result(|g, c| {
            c.stack.pop_int();
            Value::Int(i32::from(g.user_control))
        }),
    );
    // Distance in space between two workers; a worker's facing.
    n.register(
        0x00A4,
        Native::result(|g, c| {
            let b = c.stack.pop_int().max(0) as usize;
            let a = c.stack.pop_int().max(0) as usize;
            let (p, q) = (g.actor(a).pos, g.actor(b).pos);
            Value::Float(((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt())
        }),
    );
    n.register(
        0x0093,
        Native::result(|g, c| {
            let w = c.stack.pop_int().max(0) as usize;
            Value::Float(g.actor(w).yaw)
        }),
    );
    // End every level's move of worker w.
    n.register(
        0x0036,
        Native::result(|g, c| {
            let w = c.stack.pop_int().max(0) as usize;
            for m in &mut g.actor(w).moves {
                m.active = None;
            }
            Value::Int(0)
        }),
    );
    // Target roll of a turn (wrapped; not modelled).
    n.register(0x002A, Native::result(|_, c| Value::Float(wrap_angle(c.stack.pop_float()))));
    // A map part's animation set back (returns 1).
    n.register(
        0x8003,
        Native::result(|_, c| {
            c.stack.pop_int();
            Value::Int(1)
        }),
    );
    // Sound effects (not played yet): the result is the sound.
    for id in [0x00DFu16, 0x00E0] {
        n.register(id, Native::result(|_, c| Value::Int(c.stack.pop_int())));
    }
    // Switches whose result is whether they were turned on.
    for id in [0x00E9u16, 0x0177] {
        n.register(id, Native::result(|_, c| Value::Int(i32::from(c.stack.pop_int() != 0))));
    }
    // Map change with a fade (as 0x0011).
    n.register(
        0x010B,
        Native::result(|g, c| {
            let entrance = c.stack.pop_int();
            let target = c.stack.pop_int();
            g.jump = Some((target, entrance));
            Value::Int(0)
        }),
    );
}

fn register_treasure(n: &mut Natives<FieldState>) {
    // Open a chest (window, treasure): give it, then show "Obtained ..." in the window and wait for the player
    // to close it. Scratch: window, step, equipment did not fit.
    n.register(
        0x015B,
        Native {
            init: Some(|g, c| {
                let index = c.stack.pop_int();
                let w = window(c);
                let image = &mut c.memory.global[0];
                let (got, full) = give_treasure(g, image, index);
                let variant = crate::items::text_variant(image);
                let text = g.items.as_ref().map(|t| t.pickup_message(&got, variant)).unwrap_or_default();
                g.messages.push(runtime_message(&text));
                g.windows[w].message = Some(g.messages.len() - 1);
                g.windows[w].x = 0x100;
                g.windows[w].y = 0xD0;
                g.windows[w].style = Some(0);
                open_window(g, image, w, false);
                c.scratch[0] = w as i32;
                c.scratch[1] = 0;
                c.scratch[2] = i32::from(full);
            }),
            wait: Some(|g, c| match c.scratch[1] {
                // The message, then a tick (with the "full" sound when the equipment did not fit), then done.
                0 => {
                    if message_wait().wait.is_some_and(|w| w(g, c) & WAIT_DONE != 0) {
                        c.scratch[1] = 1;
                    }
                    0
                }
                1 => {
                    c.scratch[1] = 2;
                    0
                }
                _ => WAIT_DONE,
            }),
            result: Some(|_, _| Value::Int(1)),
        },
    );
    // The same without a message (a tick's wait when the equipment did not fit).
    n.register(
        0x01A7,
        Native {
            init: Some(|g, c| {
                let index = c.stack.pop_int();
                let (_, full) = give_treasure(g, &mut c.memory.global[0], index);
                c.scratch[1] = 0;
                c.scratch[2] = i32::from(full);
            }),
            wait: Some(|_, c| {
                if c.scratch[2] == 0 || c.scratch[1] == 1 {
                    return WAIT_DONE;
                }
                c.scratch[1] = 1;
                0
            }),
            result: Some(|_, _| Value::Int(1)),
        },
    );
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
                win.choice = if g.input.stick[1] > 0.0 {
                    win.choice.saturating_sub(1)
                } else {
                    (win.choice + 1).min(n - 1)
                };
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

/// The choice window wait (007D, 013B): like the message wait, but the window takes the choice on the tick
/// after the press and closes then. Scratch 1: 1 once chosen.
fn choice_wait(g: &mut FieldState, c: &mut NativeCall<'_>) -> u32 {
    let w = c.scratch[0] as usize;
    if c.scratch[1] == 1 {
        c.scratch[1] = 2;
        g.windows[w].close();
        // Scripts still read the choice until the wait ends.
        g.windows[w].choice_live = g.windows[w].closing + 2;
        return 0;
    }
    // Cursor moves take effect on the tick after the press.
    let n = g.windows[w].message.and_then(|m| g.messages.get(m)).map_or(0, |m| usize::from(m.choices));
    let win = &mut g.windows[w];
    if c.scratch[2] != 0 && n > 0 {
        // The cursor stops at the first and the last choice (cdsp0000: up on the first stays there).
        win.choice =
            if c.scratch[2] > 0 { win.choice.saturating_sub(1) } else { (win.choice + 1).min(n - 1) };
    }
    c.scratch[2] = 0;
    if c.scratch[1] == 0 && win.open && win.shown >= win.reveal {
        if g.input.stick_pressed && g.input.stick[1] != 0.0 {
            c.scratch[2] = if g.input.stick[1] > 0.0 { 1 } else { -1 };
        }
        if g.input.confirm || g.input.cancel {
            if !g.input.confirm {
                win.choice = usize::from(win.cancel_choice);
            }
            c.scratch[1] = 1;
        }
        return 0;
    }
    if c.scratch[1] == 0 && win.open {
        // Still revealing: confirm shows the rest, as in message waits (no cursor moves yet).
        let stick = std::mem::take(&mut g.input.stick_pressed);
        let r = message_wait().wait.map_or(0, |f| f(g, c));
        g.input.stick_pressed = stick;
        return r;
    }
    let win = &g.windows[w];
    if !win.open && win.closing == 0 { WAIT_DONE } else { 0 }
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

/// A move time with speeding up, steady and slowing down frame counts (then a fourth value): the channels
/// run for the three together. `eye`: the eye's channels (else the focus's).
fn camera_move_time(g: &mut FieldState, c: &mut NativeCall, eye: bool) -> Value {
    let _d = c.stack.pop_int();
    let frames = (0..3).map(|_| c.stack.pop_int() as i16).fold(0i16, |a, b| a.wrapping_add(b));
    let frames = frames.max(0) as u32;
    let cam = &mut g.camera;
    if eye {
        let parts = cam.parts.unwrap_or(0x1F);
        cam.time_eye(frames);
        if parts & 7 != 0 {
            cam.move_frames = frames.max(1);
            cam.moving = None;
            cam.pending = true;
        }
    } else {
        cam.time_focus(frames);
    }
    Value::Int(0)
}

/// A linear move time in frames for the eye or the focus (the same channels as above).
fn camera_move_frames(g: &mut FieldState, c: &mut NativeCall, eye: bool) -> u32 {
    let frames = (c.stack.pop_int() as i16).max(0) as u32;
    if eye {
        g.camera.time_eye(frames)
    } else {
        g.camera.time_focus(frames)
    }
    frames
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
/// Register every field native Aerugo implements.
pub fn register(n: &mut Natives<FieldState>) {
    register_treasure(n);
    register_queries(n);
    register_menus(n);
    register_settings(n);
    register_engine_settings(n);
    register_worker_switches(n);
    register_fixed_results(n);
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
    // The entrance the party arrived by (the save image's byte 0xC) or a given one (below 0: that one too):
    // position, height, depth, heading, entry.
    fn entrance(g: &FieldState, c: &NativeCall, index: Option<i32>) -> aerugo_formats::script::Entrance {
        let current = i32::from(c.memory.global[0].get(ENTRANCE_INDEX).copied().unwrap_or(0));
        let i = index.filter(|&i| i >= 0).unwrap_or(current);
        usize::try_from(i)
            .ok()
            .and_then(|i| g.entrances.get(i))
            .or(g.entrances.first())
            .copied()
            .unwrap_or_default()
    }
    n.register(0x0080, Native::result(|g, c| Value::Float(entrance(g, c, None).pos[0])));
    n.register(0x0081, Native::result(|g, c| Value::Float(entrance(g, c, None).pos[1])));
    n.register(0x0082, Native::result(|g, c| Value::Float(entrance(g, c, None).pos[2])));
    n.register(0x0083, Native::result(|g, c| Value::Float(entrance(g, c, None).yaw)));
    fn indexed(g: &mut FieldState, c: &mut NativeCall) -> aerugo_formats::script::Entrance {
        let i = c.stack.pop_int();
        entrance(g, c, Some(i))
    }
    n.register(0x00BF, Native::result(|g, c| Value::Float(indexed(g, c).pos[0])));
    n.register(0x00C0, Native::result(|g, c| Value::Float(indexed(g, c).pos[1])));
    n.register(0x00C1, Native::result(|g, c| Value::Float(indexed(g, c).pos[2])));
    n.register(0x00C2, Native::result(|g, c| Value::Float(indexed(g, c).yaw)));
    n.register(0x00C9, Native::result(|g, c| Value::Int(i32::from(indexed(g, c).entry as i16))));
    // Player speeds (1 walking, 2 running, 0x12 in water; others not used here).
    n.register(
        0x4016,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            match c.stack.pop_int() {
                1 => g.control.walk_speed = v,
                2 => g.control.run_speed = v,
                0x12 => g.control.swim_speed = v,
                _ => {}
            }
            Value::Int(0)
        }),
    );
    // Collision radius (0 or below: back to the default); the result is the previous one.
    n.register(
        0x5039,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let a = g.actor(c.worker);
            let old = a.collision_radius();
            a.radius = (v > 0.0).then_some(v);
            Value::Float(old)
        }),
    );
    // Place the worker. 0x126 is a warp: the player placed with it did not walk, so triggers see no movement
    // that tick; a 0x13 placement counts as a move.
    fn place(g: &mut FieldState, c: &mut NativeCall<'_>, warp: bool) -> Value {
        let p = pop3(c);
        if warp && g.player == Some(c.worker) {
            g.player_placed = true;
        }
        let a = g.actor(c.worker);
        a.pos = p;
        a.move_at(c.level).active = None;
        Value::Int(0)
    }
    n.register(0x0013, Native::result(|g, c| place(g, c, false)));
    n.register(0x0126, Native::result(|g, c| place(g, c, true)));
    n.register(
        0x0095,
        Native::result(|g, c| {
            let yaw = wrap_angle(c.stack.pop_float());
            let a = g.actor(c.worker);
            a.yaw = yaw;
            if a.turn_level == c.level {
                a.turning = false;
                a.turn_steps = None;
            }
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
    // A motion over a frame range (motion, two values not used here, first frame, last frame or −1), and the
    // waits for it.
    n.register(
        0x501A,
        Native::result(|g, c| {
            let end = c.stack.pop_int();
            let start = c.stack.pop_int();
            let _b = c.stack.pop_int();
            let _a = c.stack.pop_int();
            let motion = c.stack.pop_int();
            let a = g.actor(c.worker);
            a.motion = Some(motion);
            a.motion_play = Some((motion, start, end, 0, 0));
            Value::Int(0)
        }),
    );
    // Movies. Open (number, flags): loaded paused, flag 0x2000 loops it; open and play at once (b00a).
    n.register(
        0xB000,
        Native { init: Some(|g, c| open_movie(g, c, false)), ..Native::result(|_, _| Value::Int(0)) },
    );
    n.register(
        0xB00A,
        Native { init: Some(|g, c| open_movie(g, c, true)), ..Native::result(|_, _| Value::Int(0)) },
    );
    // Play the opened movie; its first frame shows this tick.
    n.register(
        0xB009,
        Native::result(|g, _| {
            if let Some(m) = &mut g.movie {
                m.playing = true;
                m.elapsed = 1;
            }
            Value::Int(0)
        }),
    );
    // Wait for the end, then close.
    n.register(
        0xB001,
        Native {
            init: None,
            wait: Some(|g, _| if g.movie.as_ref().is_some_and(|m| m.playing) { 0 } else { 5 }),
            result: Some(|g, _| {
                g.movie = None;
                Value::Int(0)
            }),
        },
    );
    // Stop and close; force close.
    for id in [0xB004, 0xB00B] {
        n.register(
            id,
            Native::result(|g, _| {
                g.movie = None;
                Value::Int(0)
            }),
        );
    }
    // Frames shown so far; the movie's length.
    n.register(0xB003, Native::result(|g, _| Value::Int(g.movie.as_ref().map_or(0, |m| m.elapsed as i32))));
    n.register(
        0xB00C,
        Native::result(|g, _| Value::Int(g.movie.as_ref().and_then(|m| m.frames).map_or(0, |f| f as i32))),
    );
    // Calls the original leaves empty (status, pause, channel, volume, debug start, overlay, palette) or that
    // only switch drawing off.
    for id in [0xB002, 0xB005, 0xB006, 0xB007, 0xB008, 0xB00D, 0xB00E, 0xB00F] {
        n.register(id, Native::result(|_, _| Value::Int(0)));
    }
    // How many more times the motion plays (0: once; 0xFF: for ever), and its speed (0x100 normal).
    n.register(
        0x5022,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            if let Some(p) = &mut g.actor(c.worker).motion_play {
                p.4 = n as u8;
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5021,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.actor(c.worker).motion_speed = v as u16;
            Value::Int(0)
        }),
    );
    // The waits for a motion (0x5003, 0x501E): until a motion played once has run its frames (a looping one, or
    // one of unknown length, does not hold the script up). The original also waits out the rest of a motion the
    // character engine was already playing (after 0x5022(0)), which needs the engine's own motion state: the
    // comparisons keep these waits with the recordings.
    for id in [0x5003u16, 0x501E] {
        n.register(
            id,
            Native {
                init: None,
                wait: Some(|g, c| {
                    let a = g.actor(c.worker).clone();
                    let once = a.motion_play.is_some_and(|p| p.4 == 0);
                    if once && g.motion_playing(&a) { 0 } else { WAIT_DONE }
                }),
                result: None,
            },
        );
    }
    n.register(
        0x5000,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            g.actor(c.worker).motion_play = Some((m, 0, -1, 0, 0xFF));
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
                g.actor(c.worker).motion_play = Some((m, 0, -1, 0, 0xFF));
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
    // Movement (per script level): target, walking speed (0x6C sets every level's) and arrival radius, duration,
    // start, wait.
    n.register(
        0x0015,
        Native::result(|g, c| {
            let p = pop3(c);
            g.actor(c.worker).move_at(c.level).target = p;
            Value::Int(0)
        }),
    );
    n.register(
        0x0016,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            g.actor(c.worker).move_at(c.level).speed = v;
            Value::Float(v)
        }),
    );
    n.register(
        0x006C,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            for m in &mut g.actor(c.worker).moves {
                m.speed = v;
            }
            Value::Float(v)
        }),
    );
    // The heading and pitch the current level's next move starts along (wrapped).
    n.register(
        0x0021,
        Native::result(|g, c| {
            let v = wrap_angle(c.stack.pop_float());
            g.actor(c.worker).move_at(c.level).start_heading = Some(v);
            Value::Float(v)
        }),
    );
    n.register(
        0x0022,
        Native::result(|g, c| {
            let v = wrap_angle(c.stack.pop_float());
            g.actor(c.worker).move_at(c.level).start_pitch = Some(v);
            Value::Float(v)
        }),
    );
    // Arrival radius of every level's move (0x0017: the current level's).
    n.register(
        0x002D,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            for m in &mut g.actor(c.worker).moves {
                m.radius = v;
            }
            Value::Float(v)
        }),
    );
    n.register(
        0x0017,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            g.actor(c.worker).move_at(c.level).radius = v;
            Value::Float(v)
        }),
    );
    // A starting count (the start of a timed move sets it back to 0) and the frame count.
    n.register(
        0x001C,
        Native::result(|g, c| {
            let frames = c.stack.pop_float();
            let _count = c.stack.pop_float();
            g.actor(c.worker).move_at(c.level).frames = frames;
            Value::Float(0.0)
        }),
    );
    n.register(
        0x0018,
        Native::result(|g, c| {
            let _b = c.stack.pop_int();
            let mode = c.stack.pop_int();
            let _a = c.stack.pop_int();
            let a = g.actor(c.worker);
            let from = a.pos;
            a.started_late |= a.first_pass_done;
            let (h, p) = (a.move_at(c.level).start_heading, a.move_at(c.level).start_pitch);
            if let Some(h) = h {
                a.move_heading = h;
            }
            if let Some(p) = p {
                a.move_pitch = p;
            }
            let m = a.move_at(c.level);
            // Modes 1 and 2 walk at the set speed (1 on the ground plane, 2 in space); the others run over the
            // frame count.
            m.active = Some(match mode & 0x7F {
                1 | 2 if m.speed > 0.0 => ActorMove::Walk {
                    to: m.target,
                    speed: m.speed,
                    ground_only: mode & 0x7F == 1,
                    radius: m.radius,
                    mode: mode as u32,
                },
                _ => ActorMove::Timed { from, to: m.target, count: 0.0, frames: m.frames },
            });
            Value::Int(0)
        }),
    );
    n.register(
        0x001A,
        Native {
            init: None,
            wait: Some(|g, c| if g.actor(c.worker).move_at(c.level).active.is_some() { 0 } else { 5 }),
            result: None,
        },
    );
    n.register(
        0x0077,
        Native::result(|g, c| {
            let w = c.stack.pop_int();
            // Ends the move of the worker's current level (other levels keep theirs).
            if let Ok(w) = usize::try_from(w) {
                let level = if w == c.worker { c.level } else { g.actor(w).level };
                g.actor(w).move_at(level).active = None;
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
            let rate = g
                .settings
                .get(&(0x002E, level_key(c.worker, c.level)))
                .and_then(|v| v.first())
                .map_or(DEFAULT_TURN_RATE, |v| match v {
                    Value::Float(f) => *f,
                    Value::Int(x) => *x as f32,
                });
            let a = g.actor(c.worker);
            a.turning = true;
            a.started_late |= a.first_pass_done;
            a.turn_mode = mode;
            a.turn_level = c.level;
            // An eased turn (kind 2) goes through its progress in steps of 5/6 of the rate over the whole turns
            // of the angle left (truncated; none: it stays at the start).
            if mode & 0x1F == 2 {
                let n = wrap_angle(a.yaw - a.turn_target) as i32;
                a.turn_t = 0.0;
                a.turn_inc = if n == 0 { 0.0 } else { (rate * 0.833_333_3 / n as f32).abs() };
            }
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
            let align = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let w = window(c);
            g.windows[w].x = x;
            g.windows[w].y = y;
            g.windows[w].align = align;
            Value::Int(0)
        }),
    );
    // A choice window's first highlighted choice and the choice cancel picks.
    n.register(
        0x0069,
        Native::result(|g, c| {
            let cancel = c.stack.pop_int();
            let first = c.stack.pop_int();
            let w = window(c);
            g.windows[w].first_choice = first as u8;
            g.windows[w].cancel_choice = cancel as u8;
            Value::Int(w as i32)
        }),
    );
    n.register(
        0x006A,
        Native::result(|g, c| {
            // Mode bit 2 shows the text at once (choice windows).
            let mode = c.stack.pop_int();
            let w = window(c);
            open_window(g, &c.memory.global[0], w, mode & 2 != 0);
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
            let to = pop3(c);
            let cam = &mut g.camera;
            cam.follow.clear_eye();
            // While the eye's channels are timed, it travels there over the frames left; else it is placed.
            cam.moving = match (cam.position, cam.timers[0]) {
                (Some(from), left) if left > 1 => Some(Movement { from, to, frames: left - 1, done: 0 }),
                _ => {
                    cam.position = Some(to);
                    None
                }
            };
            Value::Int(0)
        }),
    );
    // The eye at a heading and pitch (degrees) and a distance from the focus, kept there as the focus moves
    // (moving there over the frames a move time sets); 0x6005 adds to the heading, pitch and distance it has (an
    // eye placed otherwise is first measured that way).
    n.register(
        0x6004,
        Native::result(|g, c| {
            let [yaw, pitch, dist] = pop3(c);
            g.camera.follow.clear_eye();
            g.camera.follow.polar = Some([yaw.to_radians(), pitch.to_radians(), dist]);
            Value::Int(0)
        }),
    );
    n.register(
        0x6005,
        Native::result(|g, c| {
            let [yaw, pitch, dist] = pop3(c);
            let cam = &mut g.camera;
            let now = cam.follow.polar.or_else(|| {
                let (eye, focus) = (cam.position?, cam.focus?);
                Some(crate::follow_camera::to_polar(focus, eye))
            });
            if let Some([y, p, d]) = now {
                cam.follow.clear_eye();
                cam.follow.polar = Some([y + yaw.to_radians(), p + pitch.to_radians(), d + dist]);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x6010,
        Native::result(|g, c| {
            let frames = camera_move_frames(g, c, true).max(1);
            g.camera.move_frames = frames;
            g.camera.start_move();
            // The following camera's eye moves there from where it is over the frames.
            if let Some(from) = g.camera.position {
                g.camera.follow.eye_move = Some((from, frames, 0));
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x602E,
        Native::result(|g, c| {
            let frames = camera_move_frames(g, c, false).max(1);
            if let Some(from) = g.camera.focus {
                g.camera.follow.focus_move = Some((from, frames, 0));
            }
            Value::Int(0)
        }),
    );
    // Speeding up, steady and slowing down frame counts, then a fourth value: 0x6014 the eye, 0x6032 the focus;
    // 0x6015 / 0x6033 the same moving by angles and distance around the other.
    for id in [0x6014u16, 0x6015] {
        n.register(id, Native::result(|g, c| camera_move_time(g, c, true)));
    }
    for id in [0x6032u16, 0x6033] {
        n.register(id, Native::result(|g, c| camera_move_time(g, c, false)));
    }
    // Linear move times: 0x6011 the eye around the focus, 0x602F the focus around the eye.
    n.register(
        0x6011,
        Native::result(|g, c| {
            camera_move_frames(g, c, true);
            Value::Int(0)
        }),
    );
    n.register(
        0x602F,
        Native::result(|g, c| {
            camera_move_frames(g, c, false);
            Value::Int(0)
        }),
    );
    // Eye and focus along a path: (path, start, step per tick, mode); the position on the path goes from the
    // start to 1 by the step each tick.
    n.register(0x6046, Native::result(|g, c| path_move(g, c, 0)));
    n.register(0x6047, Native::result(|g, c| path_move(g, c, 1)));
    register_triggers(n);
    register_control(n);
    register_workers(n);
    // Halt: the worker stops here for good (until a request takes it elsewhere).
    n.register(0x005F, Native { init: None, wait: Some(|_, _| 0), result: None });
    // Have a worker run again at the end of this frame (its new requests start then); returns the worker.
    n.register(
        0x0116,
        Native::result(|_, c| {
            let w = c.stack.pop_int();
            let t = if w < 0 { c.worker } else { w as usize };
            c.effects.push(aerugo_script::vm::Effect::PassAgain(t));
            Value::Int(w)
        }),
    );
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
    // (6062/6063 wait on the eye's / focus's move channels; which Aerugo timers they map to is not settled:
    // replayed for now.)
    n.register(
        0x601A,
        Native { init: None, wait: Some(|g, _| if g.camera.is_moving() { 0 } else { 5 }), result: None },
    );
    // Stop the eye's / the focus's moves where they are; wait for the focus's.
    n.register(
        0x6016,
        Native::result(|g, _| {
            g.camera.moving = None;
            g.camera.timers[0..5].fill(0);
            Value::Int(0)
        }),
    );
    n.register(
        0x6034,
        Native::result(|g, _| {
            g.camera.timers[5..8].fill(0);
            Value::Int(0)
        }),
    );
    n.register(
        0x6038,
        Native { init: None, wait: Some(|g, _| if g.camera.focus_moving() { 0 } else { 5 }), result: None },
    );
    n.register(
        0x603B,
        Native::result(|g, c| {
            g.camera.screen_distance = c.stack.pop_float();
            Value::Int(0)
        }),
    );
    // The follow camera: follow workers (−1: none) with a height offset; limits on the focus; easing; the eye
    // around the focus.
    n.register(
        0x6027,
        Native::result(|g, c| {
            let f = c.stack.pop_float();
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            let w = |v: i32| usize::try_from(v).ok();
            g.camera.follow.follow = Some(([w(a), w(b)], [0.0, f, 0.0]));
            Value::Int(0)
        }),
    );
    n.register(
        0x602D,
        Native::result(|g, _| {
            g.camera.follow.clear_limits();
            g.camera.follow.rail = None;
            Value::Int(0)
        }),
    );
    n.register(
        0x6029,
        Native::result(|g, c| {
            // (x0, z0) to (x1, z1), then the share kept past it.
            let k = c.stack.pop_float();
            let z1 = c.stack.pop_float();
            let x1 = c.stack.pop_float();
            let z0 = c.stack.pop_float();
            let x0 = c.stack.pop_float();
            let limit = crate::follow_camera::Limit::Line { start: [x0, z0], dir: [x1 - x0, z1 - z0], k };
            g.camera.follow.add_limit(limit);
            Value::Int(0)
        }),
    );
    n.register(
        0x602B,
        Native::result(|g, c| {
            let k = c.stack.pop_float();
            let max = c.stack.pop_float();
            let min = c.stack.pop_float();
            g.camera.follow.add_limit(crate::follow_camera::Limit::Height { min, max, k });
            Value::Int(0)
        }),
    );
    n.register(
        0x6035,
        Native::result(|g, c| {
            let _ = c.stack.pop_float();
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            g.camera.follow.ease = [x, y, z];
            Value::Int(0)
        }),
    );
    n.register(
        0x6006,
        Native::result(|g, c| {
            let dist_div = c.stack.pop_float();
            let dist = c.stack.pop_float();
            let pitch_div = c.stack.pop_float();
            let pitch = c.stack.pop_float().to_radians();
            let b = c.stack.pop_float();
            let a = c.stack.pop_float();
            g.camera.follow.clear_eye();
            g.camera.follow.orbit =
                Some(crate::follow_camera::Orbit { anchor: [a, b], pitch, pitch_div, dist, dist_div });
            Value::Int(0)
        }),
    );
    // The eye from a table in the worker's data (a variable naming it), a heading change in degrees and the
    // anchor point.
    n.register(
        0x601F,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let x = c.stack.pop_float();
            let yaw = c.stack.pop_float().to_radians();
            let var = c.stack.pop_int() as u32;
            let areas = g.data_areas.get(c.worker).copied().unwrap_or_default();
            let area = ((var >> 25) & 7) as usize;
            if (2..=4).contains(&area) {
                let at = areas[area - 2] as usize + (var & 0xFF_FFFF) as usize;
                let table = c.memory.script.get(at..).unwrap_or_default();
                g.camera.follow.clear_eye();
                g.camera.follow.path = crate::follow_camera::EyePath::read(table, [x, z], yaw);
            }
            Value::Int(0)
        }),
    );
    // Rails: the focus held near a path (605A: table, share kept, 3 for the cubic path) and the eye on a path at
    // the focus's place along it (605B: table, 3 for cubic).
    fn table<'a>(g: &FieldState, c: &'a NativeCall, var: u32) -> Option<&'a [u8]> {
        let areas = g.data_areas.get(c.worker).copied().unwrap_or_default();
        let area = ((var >> 25) & 7) as usize;
        if !(2..=4).contains(&area) {
            return None;
        }
        c.memory.script.get(areas[area - 2] as usize + (var & 0xFF_FFFF) as usize..)
    }
    n.register(
        0x605A,
        Native::result(|g, c| {
            let mode = c.stack.pop_int();
            let k = c.stack.pop_float();
            let var = c.stack.pop_int() as u32;
            let rail = table(g, c, var).and_then(|t| crate::follow_camera::Path::read(t, mode == 3));
            if let Some(r) = rail {
                g.camera.follow.rail = Some((r, k));
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x605B,
        Native::result(|g, c| {
            let mode = c.stack.pop_int();
            let var = c.stack.pop_int() as u32;
            if let Some(r) = table(g, c, var).and_then(|t| crate::follow_camera::Path::read(t, mode == 3)) {
                g.camera.follow.clear_eye();
                g.camera.follow.eye_rail = Some(r);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x6020,
        Native::result(|g, c| {
            g.camera.follow.stop();
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
    #[test]
    fn a_walk_steps_a_twentieth_of_its_speed_and_ends_with_half_the_rest() {
        let (mut pos, to) = ([0.0, 0.0, 0.0], [10.0, 5.0, 0.0]);
        let mut ticks = 0;
        loop {
            let (next, done) = walk_step(pos, to, 34.0, true, 0.0);
            ticks += 1;
            if done {
                // Within 3.4 of the target: half the remaining distance, on the ground plane.
                assert!((next[0] - (pos[0] + (10.0 - pos[0]) / 2.0)).abs() < 1e-5);
                assert_eq!(next[1], 0.0);
                break;
            }
            assert!((next[0] - pos[0] - 1.7).abs() < 1e-5);
            pos = next;
        }
        assert_eq!(ticks, 5);
    }

    #[test]
    fn a_levels_move_waits_while_a_higher_level_runs() {
        let mut a = Actor::default();
        a.moves[1].active =
            Some(ActorMove::Timed { from: [0.0; 3], to: [10.0, 0.0, 0.0], count: 0.0, frames: 10.0 });
        a.level = 1;
        a.advance_move(None);
        a.advance_move(None);
        assert_eq!(a.pos[0], 1.0);
        a.level = 2;
        a.advance_move(None);
        assert_eq!(a.pos[0], 1.0);
        a.level = 1;
        a.advance_move(None);
        assert_eq!(a.pos[0], 2.0);
    }

    #[test]
    fn walking_into_a_character_bumps_it_once_and_stops_at_contact() {
        let body = |x: f32| Actor { model: Some(0x2001), pos: [x, 0.0, 0.0], ..Actor::default() };
        let mut state =
            FieldState { actors: vec![body(0.0), body(7.0)], player: Some(0), ..FieldState::default() };
        state.user_control = true;
        state.player_prev = [-1.0, 0.0, 0.0];
        // Radii 4 + 4: the player is pushed back to 8 from the other.
        assert_eq!(state.bumps(), vec![1]);
        assert!((state.actors[0].pos[0] + 1.0).abs() < 1e-5);
        state.player_prev = state.actors[0].pos;
        state.actors[0].pos[0] += 0.5;
        assert!(state.bumps().is_empty());
        // Placed onto a character by a script (no player control): no bump.
        state.user_control = false;
        state.actors[1].bumped = false;
        state.player_prev = [-1.0, 0.0, 0.0];
        assert!(state.bumps().is_empty());
    }

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
                instant: false,
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
    fn a_movie_plays_one_frame_a_tick_and_its_wait_ends_with_it() {
        use aerugo_script::asm::{Op, WorkerSrc, assemble};
        // open(84, 0x500); play; wait for the end; global[0] = 1
        let w = WorkerSrc {
            kind: 1,
            entries: vec![vec![
                Op(0x2E, Some(84)),
                Op(0x2E, Some(0x500)),
                Op(0x58, Some(0xB000)),
                Op(0x58, Some(0xB009)),
                Op(0x58, Some(0xB001)),
                Op(0x2E, Some(1)),
                Op(0x20, Some(0)),
                Op(0x3C, None),
            ]],
            vars: vec![(5 << 28, 1)],
            ..Default::default()
        };
        let mut vm = Vm::new(Arc::new(Script::parse(&assemble(&[w])).unwrap()));
        let mut natives = Natives::new(Game::Ffx);
        register(&mut natives);
        let mut state = FieldState {
            videos: [(84, "movie.webm".to_owned())].into_iter().collect(),
            ..FieldState::default()
        };
        let done = |vm: &Vm| vm.memory.global[0][aerugo_script::memory::AREA0_OFFSET] == 1;
        let mut ticks = 0;
        while !done(&vm) && ticks < 20 {
            vm.frame(&mut natives.host(&mut state));
            // The host reads the length once the movie is open.
            if let Some(m) = state.movie.as_mut().filter(|m| m.frames.is_none()) {
                assert_eq!(m.path.as_deref(), Some("movie.webm"));
                m.frames = Some(4);
            }
            state.update();
            ticks += 1;
        }
        assert!(done(&vm));
        assert!(state.movie.is_none());
        assert!((5..=8).contains(&ticks), "{ticks}");
    }

    #[test]
    fn video_list_lines() {
        let list = parse_video_list("0:,OPN_us.webm,OPN_us.webm\r\n84:,opp_us.webm,opp_us.webm\r\n");
        assert_eq!(list.get(&84).map(String::as_str), Some("opp_us.webm"));
        assert_eq!(list.len(), 2);
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
            user_control: true,
            ..FieldState::default()
        };
        state.input.stick = [0.0, 1.0];
        // The push ramps up over eight ticks to the run speed (1.9 a tick).
        for _ in 0..20 {
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
        assert_eq!(
            chr_path(Game::Ffx, 0x2051).as_deref(),
            Some("ffx_ps2/ffx/master/jppc/chr/npc/n081/mdl/n081.chr")
        );
    }

    #[test]
    fn eased_turns_and_turning_toward() {
        // 0.4 of the rate at the ends of an eased turn, all of it halfway.
        assert!((turn_ease(0.0) - 0.4).abs() < 1e-6);
        assert!((turn_ease(0.5) - 1.0).abs() < 1e-6);
        assert!((turn_ease(1.0) - 0.4).abs() < 1e-6);
        // At most the step, exactly onto the target when closer, the short way round.
        assert_eq!(turn_toward(0.0, 1.0, 0.25), 0.25);
        assert_eq!(turn_toward(0.0, 0.1, 0.25), 0.1);
        assert!((turn_toward(3.0, -3.0, 0.1) - 3.1).abs() < 1e-6);
    }

    #[test]
    fn a_steered_turn_leads_the_walk() {
        // cdsp0700 w2: facing 0, target at heading 2.4857; the turn's rate 0.1745 eased, its progress 0.1309 a
        // tick: the first step is 0.1175, and the walk (0x1000) goes along the facing.
        let mut a = Actor { model: Some(1), ..Actor::default() };
        let to = [2.4857f32.cos() * 100.0, 0.0, 2.4857f32.sin() * 100.0];
        a.move_at(0).target = to;
        a.move_at(0).active =
            Some(ActorMove::Walk { to, speed: 20.0, ground_only: true, radius: 0.0, mode: 0xC001 });
        a.turning = true;
        a.turn_mode = 0xD202;
        a.turn_inc = 0.1309;
        a.steer_turn(0.17453294);
        assert!((a.yaw - 0.1175).abs() < 1e-3, "{}", a.yaw);
        assert_eq!(a.move_heading, a.yaw);
        let st = a.steer_walk_begin(0.0, 0.0).expect("a steered walk");
        a.steer_walk_end(st, None);
        assert!((a.pos[0] - a.yaw.cos()).abs() < 1e-4 && (a.pos[2] - a.yaw.sin()).abs() < 1e-4);
    }

    #[test]
    fn chr_radius_from_the_info_block() {
        let mut d = vec![0u8; 0x100];
        d[4] = 11;
        d[0x58] = 0x80;
        d[0x84..0x88].copy_from_slice(&3.5f32.to_le_bytes());
        assert_eq!(chr_radius(&d), Some(3.5));
        d[4] = 9;
        assert_eq!(chr_radius(&d), Some(1.0));
    }
}
