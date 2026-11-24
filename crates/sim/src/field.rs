//! A field event: the event's script running on the script machine, with the state its natives act on (actors,
//! message windows, camera, player control). The renderer only reads this state.

use std::sync::Arc;

use aerugo_formats::Game;
use aerugo_formats::script::Script;
use aerugo_formats::text::{self, Charset};
use aerugo_script::natives::{Native, World, wrap_angle};

use crate::control::PlayerControl;
use crate::trigger::{self, Talk};
use aerugo_script::vm::{NativeCall, Value, WAIT_DONE};
use aerugo_script::{Natives, Phase, RequestMode, Vm};
use aerugo_vfs::{Language, Vfs};

pub mod cam_natives;
pub mod chr_cache;
pub mod collision;
pub mod effects;
pub mod face;
pub mod fog;
pub mod map_change;
pub mod map_fx;
pub mod map_objects;
pub mod motion_engine;
pub mod msg_to;
pub mod pad;
pub mod pos_sound;
pub mod pre_bg;
pub mod rng_streams;
pub mod screen_fx;
pub mod shapes;
pub mod sound;
pub mod sys_natives;
pub mod text_build;
pub mod text_space;
pub mod texture_animation;
pub mod vibration;
pub mod voice;
pub mod walk_mesh;
pub mod worker_bridge;
pub mod worker_natives;

use collision::Ground;

/// A character's texture records as the PS2 engine keeps them (0x40 each at the character's +0x6ec): the image
/// base to load (+0x38) and the one loaded (+0x3a), −1 for none. The remaster draws its own textures; this is
/// the state scripts set.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChrTextures {
    /// Image bases by record (set for all records: `None` as the key).
    pub bases: Vec<(Option<i32>, i32)>,
    /// The bases were loaded (5071) this many times.
    pub loads: u32,
    /// The texture animation's timer (per slot).
    pub anim_timer: Option<u8>,
}

/// A body's matrices as the character engine's batch left them: the world matrix (+0x1d0), the element matrices
/// (model space) and the option points (+0x524 + key · 0x10).
#[derive(Debug, Clone, PartialEq)]
pub struct ChrFrame {
    pub world: crate::chr::elements::Mat,
    pub elems: Vec<crate::chr::elements::Mat>,
    pub points: [[f32; 4]; 0x16],
}

/// The body's X turn one step toward the target (by at most `speed`), wrapped.
fn tilt_step(a: f32, target: f32, speed: f32) -> f32 {
    let f = crate::chr::wrap(a - target);
    if f == 0.0 {
        return target;
    }
    let a = if f <= 0.0 {
        let s = if -speed < f { -f } else { speed };
        s + a
    } else if speed <= f {
        a - speed
    } else {
        a - f
    };
    crate::chr::wrap(a)
}

/// A worker's body in the world.
#[derive(Debug, Clone, PartialEq)]
pub struct Actor {
    /// Model id (`type << 12 | number`): 0 party, 1 monster, 2 npc, 3 summon, 4 weapon, 5 object.
    pub model: Option<u16>,
    /// Position in the map's space (Y points down).
    pub pos: [f32; 3],
    /// Added to the position where the model is drawn (native 5016; the character's +0x6c).
    pub draw_offset: [f32; 3],
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
    /// Body hit turned off (500f): neither blocks nor bumps.
    pub no_body_hit: bool,
    /// The map display object this worker is bound to (0197), which its 0198/0199 keyframe requests act for.
    pub display_object: Option<u16>,
    /// What the worker is bound to (the original's worker +0xaa and +0xa8): 0 nothing, 1 a character model
    /// (0001), 2 the camera (0002), 3 (0003), 4 (0004) and 5 (0197) display objects; and the bound number.
    pub bind_kind: u8,
    pub bind_id: u16,
    /// The character's motion player (made when a model is bound).
    pub player: Option<motion_engine::Player>,
    /// The character's texture records' image bases as scripts set them (505a / 5071) and its texture
    /// animation timer (508d).
    pub chr_textures: ChrTextures,
    /// The character engine's motion slots (5025): 0 standing, 1 walking, 2 running, 16–19 in water; a value
    /// under 0x1000 picks from the model's own list, others are motion ids.
    pub motion_slots: [i32; 20],
    /// Above this speed a moving character runs (5028; 18 by default).
    pub run_threshold: f32,
    /// The speed the character engine moves the body at this tick (the player's push or the running move's).
    pub engine_speed: f32,
    /// The character engine's mode (5013): 0 on the ground, 2 in water.
    pub chr_mode: u8,
    /// The body's rise this tick (the engine's vertical step, ×0.05).
    pub chr_rise: f32,
    /// The body's parts hidden by script (5054), by part number.
    pub parts_hidden: Vec<bool>,
    /// The character's face animation player (for the model it was made for) and the remaster's face layer stack.
    pub face: Option<(u16, face::Face)>,
    pub face_layers: Vec<face::FaceLayer>,
    /// The script level whose move advances (the worker's running level).
    pub level: u8,
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

/// The motion slots a character starts with (a new character: 0, 1, 2 for standing, walking, running and the
/// defaults 0, 1, 2, 3 for slots 16–19; the rest start at 0).
const DEFAULT_MOTION_SLOTS: [i32; 20] = [0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3];
/// The value a slot set to −1 takes (by slot).
const MOTION_SLOT_DEFAULTS: [i32; 20] = [0, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3];
const DEFAULT_RUN_THRESHOLD: f32 = 18.0;
/// The water level an event starts with.
const WATER_LEVEL_AT_START: f32 = 1_000_000.0;

/// A character's own motion lists from its `.chr` data (header entries 5, 6 and 7: on the ground, the second
/// list, in water), motion ids indexed by a slot value under 0x1000.
pub fn chr_motion_lists(d: &[u8]) -> Option<[Vec<u32>; 4]> {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let list = |k: usize| -> Option<Vec<u32>> {
        let (at, n) = (u32_at(0x10 + k * 8)? as usize, u32_at(0x14 + k * 8)? as usize);
        if at == 0 {
            return Some(Vec::new());
        }
        (0..n.min(0x400)).map(|i| u32_at(at + 4 * i)).collect()
    };
    Some([list(5)?, list(6)?, list(7)?, list(8)?])
}

impl Default for Actor {
    fn default() -> Actor {
        Actor {
            model: None,
            pos: [0.0; 3],
            draw_offset: [0.0; 3],
            yaw: 0.0,
            visible: true,
            motion: None,
            params: [0; 4],
            character: None,
            flags: 0,
            radius: None,
            model_radius: None,
            move_heading: 0.0,
            first_pass_done: false,
            started_late: false,
            no_body_hit: false,
            display_object: None,
            bind_kind: 0,
            bind_id: 0,
            chr_textures: ChrTextures::default(),
            player: None,
            motion_slots: DEFAULT_MOTION_SLOTS,
            run_threshold: DEFAULT_RUN_THRESHOLD,
            engine_speed: 0.0,
            chr_mode: 0,
            chr_rise: 0.0,
            parts_hidden: Vec::new(),
            face: None,
            face_layers: Vec::new(),
            level: 0,
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

/// A camera part following a script path: the path, the place along it and the step a tick.
#[derive(Debug, Clone, PartialEq)]
struct PathMove {
    path: crate::follow_camera::Path,
    t: f32,
    step: f32,
}

/// Collision radius of a character whose model size is not known (the party leader's: 4).
const DEFAULT_RADIUS: f32 = 4.0;

/// The frame time the character engine scales its steps by.
const FRAME_TIME: f32 = 1.0 / 30.0;

impl Actor {
    /// Radius for collisions between characters.
    pub fn collision_radius(&self) -> f32 {
        self.radius.or(self.model_radius).unwrap_or(DEFAULT_RADIUS)
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

/// Engine globals that outlive an event (the host carries them to the next one).
#[derive(Debug, Clone, PartialEq)]
pub struct Globals {
    /// The wide-turn limit in degrees (179 at start; 01f0).
    pub turn_limit_deg: f32,
    /// A one-time flag 01c6 returns and clears (1 at start).
    pub first_c6: u8,
    /// A value a menu sets (−1 at start; 01fa reads it).
    pub menu_value_fa: i32,
    /// The save read's argument 01c5 keeps ([1330230]).
    pub save_read_1330230: i32,
    /// The scene's colour transform: multipliers and offsets (zero at start; an event effect's end, or a
    /// battle's start, sets the multipliers to 1). The map effects take their colour from the multipliers, and
    /// the fog and background colours use both while the display flags' bit 2 is set.
    pub colour_mul: [f32; 4],
    pub colour_add: [f32; 4],
}

impl Default for Globals {
    fn default() -> Self {
        Globals {
            turn_limit_deg: 179.0,
            first_c6: 1,
            menu_value_fa: -1,
            save_read_1330230: 0,
            colour_mul: [0.0; 4],
            colour_add: [0.0; 4],
        }
    }
}

/// A body attached to a worker's body (a weapon): its model, its character record, the mount point.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub model: u16,
    pub chr: crate::chr::Chr,
    pub mount: i32,
    /// Hidden (024b).
    pub hidden: bool,
    /// Parts hidden by number (Ch_ModelSetHide), and every part's alpha when one was set for all.
    pub parts_hidden: Vec<bool>,
    pub parts_alpha: Option<f32>,
}

/// One of the eight message windows: the event side's record (msg_window) and the message module's part.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Window {
    pub message: Option<usize>,
    /// Screen position in the original's 512 × 448 frame, and how the window sits on it (4: centred there, 2: its
    /// right edge there; otherwise its top left corner).
    pub x: i32,
    pub y: i32,
    pub align: i32,
    /// +0x14: 0 closed, 1 asked to open, 2 shown, 3 closing (the frame's state each frame after the workers).
    pub atel_state: i16,
    /// +0x1d: the open mode (bit 1 choices, bit 3 an explicit size, bit 4 a message wait, bit 5 a choice wait, bit
    /// 6 the number entry).
    pub mode: u8,
    /// +0x18: the text flags (009d; low nibble the text type, 0x400 pages go on by themselves, 0x2000/0x8000 the
    /// size type).
    pub flags: u32,
    /// The choice highlighted when the window opens, and the one cancel picks.
    pub first_choice: u8,
    pub cancel_choice: u8,
    /// The frame type a script set (0066).
    pub style: Option<u8>,
    /// The message module's frame, text and choice records.
    pub to: msg_to::To,
    /// The values the text's placeholders show (1869ab0's 32 slots: 009e / 8b8960 the staged type +0, 8b8980 the
    /// value +4); a slot not listed is (0xff, −1), as 8b81d0 resets them.
    pub values: Vec<(u8, i32)>,
    /// The slots' active types (+1), copied from the staged ones when the window opens (8b8210); the {12:3n}
    /// macros read these. A slot not listed is 0xff (none).
    pub value_types: Vec<u8>,
    /// The window's colour word (00a2: four bytes) and its +0x28 word (0256 / 0257).
    pub color: u32,
    pub word28: u32,
    /// The +0x16 word (a text's choices in its high byte; 0141).
    pub word16: u16,
    /// Bytes +0x22 / +0x23 and the word +0x24 (010f).
    pub byte22: u8,
    pub byte23: u8,
    pub word24: u32,
    /// The number entry a window opened with flag 0x40 takes (its input record).
    pub number: Option<NumberEntry>,
}

/// A number entry window's input: digits picked by a cursor, each stepped up or down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NumberEntry {
    /// +1: 2 once confirmed.
    pub state: u8,
    /// +2 the digit under the cursor (0: the leftmost), +3 the one it starts on, +4 / +5 the first and the
    /// count.
    pub cursor: i8,
    pub start_cursor: i8,
    pub first: i8,
    pub digits: i8,
    /// +0xc the number, +0x10 the one it starts with.
    pub value: i32,
    pub start: i32,
}

/// Powers of ten by digit place + 1 (its first entry is not one).
const DIGIT_PLACES: [i32; 10] =
    [0x003b_3b3b, 1, 10, 100, 1000, 10000, 100_000, 1_000_000, 10_000_000, 100_000_000];

impl NumberEntry {
    /// The window opens on cursor `cursor` (and goes back to `start`) with `value`, for `digits` digits.
    pub fn new(cursor: i32, start: i32, value: i32, digits: i8) -> Self {
        let clamp = |v: i32| (v.clamp(0, 7) as i8).min(digits - 1);
        NumberEntry {
            state: 0,
            cursor: clamp(cursor),
            start_cursor: clamp(start),
            first: 0,
            digits,
            value,
            start: value,
        }
    }

    /// One frame of input: `repeat` the pressed-with-repeat word, `pressed` the pressed word.
    pub fn step(&mut self, repeat: u32, pressed: u32) {
        if repeat & 0x2000 != 0 {
            self.cursor = self.cursor.wrapping_add(1);
        }
        if repeat & 0x8000 != 0 {
            self.cursor = self.cursor.wrapping_sub(1);
        }
        let place = |e: &Self| {
            let k = (i32::from(e.digits) - i32::from(e.cursor)).clamp(0, 9) as usize;
            let p = DIGIT_PLACES[k];
            (p, if k == 0 { -1 } else { e.value / p % 10 })
        };
        if repeat & 0x1000 != 0 {
            let (p, d) = place(self);
            self.value = self.value.wrapping_add(if d == 9 { p.wrapping_mul(-9) } else { p });
        }
        if repeat & 0x4000 != 0 {
            let (p, d) = place(self);
            self.value = self.value.wrapping_add(if d == 0 { p.wrapping_mul(9) } else { -p });
        }
        let (n, c) = (self.digits, self.cursor);
        self.cursor = if n - 1 < c {
            self.first - n + c
        } else if c < self.first {
            c - self.first + n
        } else {
            c
        };
        if pressed & 0x40 != 0 {
            self.cursor = self.start_cursor;
            self.value = self.start;
        }
        if pressed & 0x20 != 0 {
            self.state = 2;
        }
    }
}

/// Character names in the save image: one 0x14-byte slot per character, NUL-terminated.
pub(crate) const NAMES: usize = 0x634C;
pub(crate) const NAME_SLOT: usize = 0x14;

/// One event message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    /// What the player reads: characters and line breaks (control codes left out).
    pub text: String,
    /// With control codes shown as `{xx}` / `{xx:yy}`.
    pub raw: String,
    /// How many of the last lines are choices.
    pub choices: u8,
    /// The text table's word for it (the window's +0x16: the choices in the high byte, a number entry's digits in
    /// the low nibble).
    pub word: u16,
    /// The message as stored (up to its NUL): what the 2D layer measures and draws. Empty for messages made at
    /// run time.
    pub bytes: Vec<u8>,
}

impl Message {
    pub fn len(&self) -> usize {
        self.text.chars().count()
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
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
    /// The eye's and the focus's fixed places (6002 / 6020) while nothing else places them.
    eye_fixed: Option<[f32; 3]>,
    focus_fixed: Option<[f32; 3]>,
    /// Timed moves of the eye and the focus, the roll and the screen distance (whose targets follow).
    eye_move: crate::camera_move::PartMove,
    focus_move: crate::camera_move::PartMove,
    roll_move: crate::camera_move::ValueMove,
    distance_move: crate::camera_move::ValueMove,
    roll_target: f32,
    distance_target: f32,
    /// Ticks left of the move channels: 0–2 the eye's three values, 3 and 4 the roll and the screen distance, 5–7 the
    /// focus's three values. A move of n frames keeps its channels busy for n + 1 polls from the tick it is set.
    timers: [u32; 8],
    /// Eye channels (bits 0–4) the next eye move time applies to; all until a script picks, and only for one
    /// move.
    parts: Option<u8>,
    /// Points recorded for script paths (604b / 6049, up to 32 each): x, y, z and the roll.
    focus_record: Vec<[f32; 4]>,
    eye_record: Vec<[f32; 4]>,
    /// Script paths the focus and the eye follow (6047 / 6046).
    focus_path: Option<PathMove>,
    eye_path: Option<PathMove>,
    /// The field camera following the player (when a script turns it on).
    pub follow: crate::follow_camera::FollowCamera,
}

impl CameraState {
    /// The eye's channels or its path move (what a camera wait waits for) are under way.
    pub fn is_moving(&self) -> bool {
        self.timers[0..5].iter().any(|&t| t > 0) || self.eye_path.is_some()
    }

    /// The focus's channels or its path move are under way.
    pub fn focus_moving(&self) -> bool {
        self.timers[5..8].iter().any(|&t| t > 0) || self.focus_path.is_some()
    }

    /// Set the roll (radians); it takes effect at once unless its move runs.
    fn set_roll(&mut self, roll: f32) {
        self.roll_target = roll;
        self.roll_move.value = roll;
        if !self.roll_move.channel.active() {
            self.roll = roll;
        }
    }

    fn update(&mut self) {
        for t in &mut self.timers {
            *t = t.saturating_sub(1);
        }
    }

    /// One tick of the script camera after the follow camera's (`follow`: its focus and eye while it follows):
    /// paths step, then each part's timed moves take it from where they started towards its target (the eye
    /// around the focus's target), and the roll and the screen distance move.
    fn process(&mut self, follow: Option<([f32; 3], Option<[f32; 3]>)>) {
        let focus_t = match follow {
            Some((f, _)) => Some(f),
            None => self.path_tick(false).or(self.focus_fixed).or(self.focus),
        };
        let follow_eye = follow.and_then(|f| f.1);
        let mut polar = None;
        let eye_t = match self.path_tick(true) {
            Some(p) => Some(p),
            None => match (self.follow.polar, focus_t) {
                (Some(p), Some(f)) if self.eye_path.is_none() => {
                    polar = Some(p);
                    Some(crate::camera_move::polar_point(f, p))
                }
                _ => self.eye_fixed.or(self.position),
            },
        };
        match follow {
            Some((f, _)) => self.focus = Some(f),
            None => {
                if let Some(t) = focus_t {
                    self.focus = Some(self.focus_move.apply(t, eye_t.unwrap_or(t), None));
                }
            }
        }
        if let Some(e) = follow_eye {
            self.position = Some(e);
        } else if let Some(t) = eye_t {
            self.position = Some(self.eye_move.apply(t, focus_t.unwrap_or(t), polar));
        }
        self.roll = self.roll_move.apply(self.roll_target, true);
        self.screen_distance = self.distance_move.apply(self.distance_target, false);
    }

    /// One tick along a script path: the point at the place (clamped to 0..1), with its roll set; the place
    /// moves on by the step, and the path ends (the part fixed at its point) once the place was clamped or the
    /// step is zero.
    fn path_tick(&mut self, eye: bool) -> Option<[f32; 3]> {
        let slot = if eye { &mut self.eye_path } else { &mut self.focus_path };
        let m = slot.as_mut()?;
        let clamped = !(0.0..=1.0).contains(&m.t);
        m.t = m.t.clamp(0.0, 1.0);
        let at = m.path.eval(m.t);
        m.t += m.step;
        let end = clamped || m.step == 0.0;
        if end {
            *slot = None;
        }
        let [x, y, z, roll] = at?;
        self.set_roll(roll);
        if end {
            if eye {
                self.eye_fixed = Some([x, y, z]);
            } else {
                self.focus_fixed = Some([x, y, z]);
            }
        }
        Some([x, y, z])
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
    /// The limit as scripts set it (0–255).
    pub limit_raw: f32,
    /// The fog colour the natives hand the remaster (events 0xe0 / 0x140 only).
    pub colour: Option<[f32; 3]>,
}

/// Values natives set for parts of the engine: event flag bits, sound switches, the character ID effect
/// tables, the cloth wind, screen and shape switches.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineValues {
    /// The event flags byte (bit 5: 00f2).
    pub event_flags: u8,
    /// Sound switches (0151 sets bit n).
    pub sound_switches: u32,
    /// Event bits (017a sets bit n; an event effect load clears them).
    pub event_bits: u32,
    /// Set by 017a(9) and cleared by 017a(10) on scene 0x5e, room 0x186.
    pub event_switch_9: bool,
    /// The screen copy switch (4014).
    pub screen_copy: bool,
    /// Character ID effect variables (4017) and the current set's effect bytes (403b).
    pub id_vars: Vec<i32>,
    pub id_effects: [u8; 8],
    pub id_set: usize,
    /// The cloth wind (4036).
    pub cloth_wind: [f32; 3],
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

/// A model's face animation data (`.chr`) and its face sprites (the remaster's).
pub type ModelFace = (Option<std::sync::Arc<face::FaceData>>, Option<std::sync::Arc<face::CharFace>>);

/// The guide map's placement: its mode (0–3 a corner at 128, 4 the centre at 256), size and centre in the PS2
/// frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GuideMap {
    pub mode: i32,
    pub size: i32,
    pub pos: (i32, i32),
    /// The farthest camera distance (+0x6c; 8032).
    pub max_camera_dist: Option<f32>,
}

impl GuideMap {
    pub fn set_mode(&mut self, mode: i32) {
        self.mode = mode;
        let (size, pos) = match mode {
            0 => (0x80, (0x50, 0x40)),
            1 => (0x80, (0x1B0, 0x40)),
            2 => (0x80, (0x50, 0x160)),
            3 => (0x80, (0x1B0, 0x160)),
            4 => (0x100, (0x100, 0xD0)),
            _ => return,
        };
        self.size = size;
        self.pos = pos;
    }
}

/// The magic effect file loaded (−1 none) and its state value (−1 none), which the effect natives keep; a count
/// one magic effect starts (−1 off) and the coin it sets for 6080.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagicState {
    pub file: i32,
    pub slot: i32,
    pub stop_count: i32,
    pub coin: bool,
}

impl Default for MagicState {
    fn default() -> Self {
        MagicState { file: -1, slot: -1, stop_count: -1, coin: false }
    }
}

/// Everything natives act on.
#[derive(Debug, Clone, Default)]
pub struct FieldState {
    /// Values natives set for engine parts, kept as the original keeps them.
    pub engine: EngineValues,
    /// The engine's scene number (the event number) when known.
    pub scene: Option<u16>,
    /// Map groups not drawn (bit per mesh instance `m_groupID`): group 3 from the map's load; native 8000 turns
    /// groups on and off.
    pub hidden_map_groups: u32,
    pub actors: Vec<Actor>,
    pub windows: [Window; 8],
    pub camera: CameraState,
    /// The event camera system's memory.
    pub cam: crate::event_camera::CamMem,
    /// The shown camera's view matrix as the original builds it (row-major, points as rows).
    /// The camera's screen matrix (screen slot 0), published with the view.
    pub cam_screen: Option<[f32; 16]>,
    /// The effect geometry of the frame's draw phase (the event effect's, then the map effects'), with the
    /// folder its textures are in.
    pub vfx: Vec<(crate::ppp::vfx::Mesh, String)>,
    pub cam_view: Option<[f32; 16]>,
    /// Each worker's camera handle (the original's worker +0x3c), 0 until a camera native first needs one.
    pub cam_slots: Vec<u32>,
    /// The worker the script header names (+0x18; its camera handles are of the event camera's kind).
    pub camera_worker: u16,
    /// The event's character worker count as the machine keeps it (header +0x36 less +0x1c, +0x16 and +0x14).
    pub chr_worker_count: i32,
    /// The game's random streams (the camera's shakes use stream 9).
    pub rng_streams: rng_streams::RngStreams,
    /// The magic effect state other natives look at.
    pub magic: MagicState,
    /// The character data cache's entries.
    pub chr_cache: chr_cache::ChrCache,
    /// Character reads (0203) running: the machine's 512-bit mask, OR'd (voice standbys wait for it).
    pub chr_read_mask: u32,
    /// The voice natives' and the sound driver's voice state.
    pub voice: voice::VoiceState,
    /// The event's 2D shapes.
    pub shapes: shapes::Shapes,
    /// The offset of the script's data shared by all workers.
    pub shared_data: u32,
    /// The worker records the engine steps (the original's layout).
    pub workers: crate::workers::Workers,
    /// Each worker's character record (the original's layout), once it binds a body.
    pub chrs: Vec<Option<crate::chr::Chr>>,
    /// The event effect running in the sandbox (00db–00de).
    pub effects: effects::Effects,
    /// Each body's matrices from the last batch.
    pub chr_frames: Vec<Option<ChrFrame>>,
    /// Each body's element values (+0xe4..) and the model they belong to.
    pub chr_joints: Vec<Option<(u16, Vec<crate::chr::elements::Joint>)>>,
    pub chr_globals: crate::chr::Globals,
    /// The positional sound slots.
    pub pos_sounds: pos_sound::PosSounds,
    /// The engine globals that outlive the event.
    pub globals: Globals,
    /// Each worker's two attached bodies (0006): the model, its record, the mount point on the worker's body.
    pub attachments: Vec<[Option<Attachment>; 2]>,
    /// The music side of the sound driver and the commands for the remaster's sound layer.
    pub sound: sound::SoundState,
    /// The guide map (the field's small map), when the field has one.
    pub guide_map: Option<GuideMap>,
    /// The event machine's 32-entry table that native 1000 fills (8 bytes each: x / 8 with bit 7, y / 4, the
    /// worker, a value). Nothing in the remaster reads it.
    pub point_table: [[u8; 8]; 32],
    /// The event's messages, decoded.
    pub messages: Vec<Message>,
    /// Item, treasure and equipment tables (FFX).
    pub items: Option<Arc<crate::items::ItemTables>>,
    /// The party stat recompute's tables and what it keeps outside the save image.
    pub stat_tables: Option<Arc<crate::stats::StatTables>>,
    pub stats: crate::stats::StatsState,
    /// What a new game's save image is made from (023d).
    pub new_game_tables: Option<Arc<crate::new_game::NewGameTables>>,
    /// The battle results buffer 0217 resets (0x1b0 bytes).
    pub battle_get: Vec<u8>,
    /// The Al Bhed primer word of the save as it was loaded (save +0x3d10 when the save was read), for
    /// the session to set; without it the save image's word now stands for it.
    pub primers_loaded: Option<i32>,
    /// A switch only the debug menu toggles (0265).
    pub debug_cccacc: i32,
    pub input: Input,
    /// Worker the player controls.
    pub player: Option<usize>,
    pub ground: Option<Ground>,
    /// The character engine's walk mesh (the map package's, page by page).
    pub walk_mesh: Option<walk_mesh::WalkMesh>,
    /// The map's fog from its PS2 map package (the scripts may change it).
    pub map_fog: Option<MapFog>,
    /// The map's particle effects as natives switch them.
    pub map_effects: map_fx::MapEffects,
    pub map_effects_on: map_fx::EffectsOn,
    /// The map effects' particle runtime (from the map package's table).
    pub map_runtime: Option<Box<map_fx::MapRuntime>>,
    /// A map display switch (805c), and the particle runtime's pause (8066 sets it; an effect may too).
    pub map_switch_5eb8: u8,
    pub map_effect_230fd34: i32,
    /// The PS2 map's pre-rendered background layers.
    pub pre_bg: pre_bg::PreBg,
    /// The remaster's map id (the hash of the map path) and language type (jp 0, us 1, fr 2, es 3, de 4, it 5,
    /// kr 9, ch 10) and its +0 (0 Japanese, 1 western, 3 Korean / Chinese).
    pub map_id: u32,
    pub language_type: Option<u32>,
    pub language_region: u32,
    /// The event's character set (for texts natives build at run time).
    pub charset: Option<std::sync::Arc<Charset>>,
    /// The texts natives hand out addresses of, and the last message asked for.
    pub texts: text_space::TextSpace,
    pub last_text: i32,
    /// The PS2 map package (for its walk mesh pages) and the page switch the event step applies next frame
    /// (kind: 1 with the guide map, 2 without; the two pages, −1 none).
    pub map_package: Option<std::sync::Arc<[u8]>>,
    pub page_switch: (u8, i8, i8),
    /// Set by 8006 on map 0x159c at story 0x7e4 with page 2 (the remaster reads it).
    pub page_flag_bf68: bool,
    /// The map display switches (map load sets 0x11, 0x4000 with a background stream; 8001 bit 0x10)
    /// and the remaster's render switch 8001 passes.
    pub map_display: u32,
    pub map_render_off: Option<bool>,
    /// The screen distortion (01e2–01e6).
    pub distortion: Distortion,
    /// The pad vibration channels.
    pub vibration: vibration::Vibration,
    /// The PS2 fog block, the remaster's scene settings the fog natives read, and the background colour 8045
    /// gives the remaster.
    pub ps_fog: fog::PsFog,
    pub hd_scene: Option<fog::HdScene>,
    pub background: Option<[f32; 3]>,
    /// Achievements the scripts unlocked (025b).
    pub achievements: Vec<i32>,
    /// The movie 01b0 asked the remaster to preload (number, mode & 0x80).
    pub movie_preload: Option<(i32, i32)>,
    /// The two values 0259 sets; 4043's pair; 4039's bytes
    /// ; the character light table 4023 allocates (0x2c bytes each: +0xc and +0x1c
    /// 1.0, +0x28 the byte 402a sets).
    pub values_5b98: [f32; 2],
    /// The message module's menu pad (pressed and repeating buttons).
    pub menu_pad: msg_to::MenuPad,
    /// The player control's heading state.
    pub sg: SgControl,
    /// Remaster switches natives hand over: the shadow solution (025e → graphicSwitchShadowSolution) and the
    /// loading screen request (025f(a, b)).
    pub shadow_solution: Option<i32>,
    pub loading_screen: Option<(i32, i32)>,
    /// A map the scripts asked to load directly (c003: a map, c031: a battle map; the map number and
    /// whether it is the battle map) for the host.
    pub map_load_request: Option<(i32, bool)>,
    /// Debug-build memory blocks (c054 allocates; their addresses are 0x7000_0000 + index << 20).
    pub debug_heap: Vec<Vec<u8>>,
    /// The shared battle motions (loaded, and the flag 70a2's wait clears first).
    pub regmot_ready: bool,
    pub regmot_flag: bool,
    /// 70a2 read the file itself (holding its data).
    pub regmot_data: bool,
    /// The system block's eight variable slots (+0x1a4 + slot·8: the address, +4; 101c sets).
    pub sys_slots: [(i32, u32); 8],
    /// Pending key items (on, and the bits; 0234 grants them).
    pub key_pending: (u32, u32),
    /// The morph weight 01a4 gives each display object.
    pub mime_params: Vec<Option<f32>>,
    /// The pad's held word last frame and the buttons let go this frame (the pad history's +8, kept).
    pub pad_prev_held: u32,
    /// A menu value (01fb reads it; the menus set it).
    pub value_a228: i32,
    pub pad_released: u32,
    /// The pad block's analog bytes and second word (0121 / 0122 / 0251).
    pub pad: pad::PadBlock,
    /// c007's pair and the debug switch bytes 707d / 707e set and read.
    pub values_085c: [i32; 2],
    pub debug_flags: Vec<u8>,
    pub values_08c4: [i32; 2],
    pub bytes_0771: [u8; 4],
    pub lights_08cc: Vec<[u8; 0x2c]>,
    /// A switch byte 00b2 sets and the three values 00b3/00b4 set.
    pub byte_181e: u8,
    pub values_194c: [i32; 3],
    /// The PS2 map's display objects and keyframe tracks, and the keyframe slots playing them.
    pub map_objects: Option<std::sync::Arc<map_objects::MapObjects>>,
    pub keyframes: map_objects::Keyframes,
    /// The remaster's texture animations of the map (its materials' swap textures).
    pub texture_animations: Option<texture_animation::TextureAnimations>,
    /// The field light characters are lit by (reset at every event start, set by native 4013).
    pub lights: FieldLights,
    /// What the fog natives set over the map's scene fog.
    pub fog: FogOverride,
    /// The next scene a map change set up (map, entrance from the save image), for the session to load, and the
    /// map change under way.
    pub jump: Option<(i32, i32)>,
    pub map_change: Option<map_change::MapChange>,
    /// The remaster's loading screen comes up for the next scene.
    pub next_loading_screen: bool,
    /// A map change asked for the software reset (not ported: reported).
    pub soft_reset: bool,
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
    /// The body the player's control moves (the control system's choice).
    pub controlled: Option<usize>,
    /// The workers whose bodies a push bumped this frame (the bump signal, type 2).
    pub bump_signals: Vec<usize>,
    /// Added to every talk radius (a script sets it; 0 by default).
    pub talk_reach: f32,
    /// The talk started this tick, while workers offer themselves.
    pub talk: Option<Talk>,
    pub control: PlayerControl,
    /// Frame counts of each model's motions by motion number, as the host loads them.
    /// The water level (4001; far below everything at an event's start): a swimming character deeper than one
    /// below it takes its diving slot.
    pub water_level: f32,
    /// Each model's face animation data (`.chr`) and the remaster's face sprites.
    pub model_faces: std::collections::HashMap<u16, ModelFace>,
    /// The C runtime's random numbers (the face animation's blink waits and holds).
    pub crt_rand: face::CrtRand,
    /// The game files, for what scripts load as they run (motion files).
    pub vfs: Option<Vfs>,
    /// The game and the event script's path.
    pub game: Option<Game>,
    pub event_path: String,
    /// The character engine's motion groups (carried from event to event by the host).
    pub motion_groups: motion_engine::Groups,
    /// Each model's PS2 joint count and joint relations to other characters' motions (`.chr` data).
    pub model_joints: std::collections::HashMap<u16, usize>,
    pub model_relations: std::collections::HashMap<u16, Vec<std::sync::Arc<motion_engine::Relation>>>,
    /// The alternative motion clock (native 4022).
    pub alt_clock: bool,
    /// Each model's own motion lists (`.chr` data).
    pub model_motion_lists: std::collections::HashMap<u16, Option<[Vec<u32>; 4]>>,
    /// Each model's option positions.
    pub model_optpos: std::collections::HashMap<u16, Option<std::sync::Arc<OptPos>>>,
    /// Each model's motion translation scale (`.chr` data).
    pub model_motion_scales: std::collections::HashMap<u16, Option<f32>>,
    /// Models' PS2 bones and parts (from their `.chr` data).
    pub model_rigs: std::collections::HashMap<u16, Option<std::sync::Arc<ChrRig>>>,
    /// Collision radii of models by id (None: no character data).
    pub model_radii: std::collections::HashMap<u16, Option<f32>>,
    /// Each model's block values (height defaults, scale).
    pub model_blocks: std::collections::HashMap<u16, Option<crate::chr::Block>>,
    /// Movie files by number (the game's video list).
    pub videos: std::collections::HashMap<u32, String>,
    /// The movie playing, if any.
    pub movie: Option<Movie>,
    /// A menu the scripts asked for (save, shop, name entry, ... by mode) and whether it is open; scripts stop
    /// while it is open. The result scripts read afterwards.
    pub menu: Option<MenuRequest>,
    pub menu_result: i32,
    /// The screen effect channels (fades, colour fade and filter, motion blur, accumulation).
    pub screen: screen_fx::ScreenFx,
    /// The screen copy entries (4015) and their switch bits.
    pub screen_copies: [i32; 4],
    pub screen_copy_bits: u32,
    /// The accumulation's two script bytes (401b, 401c).
    pub accumulation_bytes: [u8; 2],
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
    /// The next script random number (rng_next(2), 7988f0): a replayed one, else stream 2's.
    pub fn random(&mut self) -> u32 {
        self.rng_replay.pop_front().unwrap_or_else(|| self.rng_streams.next(2))
    }
}

impl World for FieldState {
    fn worker_ran(&mut self, w: usize, _kind: u8, level: u8) -> Vec<u8> {
        self.worker_engine_step(w, level)
    }

    fn worker_will_run(&mut self, w: usize, _kind: u8, _level: u8) {
        self.worker_pre_run(w);
    }

    // 8641e0: a worker runs with +0x34 bit 2 or +0x36 bit 6 set, or while no battle is requested or running
    // (780c90: [112a8e0] / [112a8e2]; the field frame does not run during Aerugo's battles, so the request);
    // never with the system's +3 & 3 == 1, and with 2 only with +0x36 bit 15. (+0x34 bit 1 is the VM's on bit.)
    fn may_run(&mut self, w: usize) -> bool {
        let Some(wk) = self.workers.list.get(w) else { return true };
        let (f, f2) = (wk.flags(), wk.flags2());
        let mode = self.workers.sys.mode3 & 3;
        (f & 4 != 0 || f2 & 0x40 != 0 || self.battle.is_none())
            && mode != 1
            && (mode != 2 || f2 & 0x8000 != 0)
    }

    fn level_started(&mut self, w: usize, level: u8) {
        if let Some(wk) = self.workers.list.get_mut(w) {
            wk.reset_level(i32::from(level));
        }
    }

    fn after_workers(&mut self) -> Vec<(usize, u8)> {
        let mut out = Vec::new();
        if let Some(t) = self.talk_end().filter(|_| self.user_control) {
            out.push(t);
        }
        self.talk = None;
        out.extend(std::mem::take(&mut self.bump_signals).into_iter().map(|w| (w, trigger::BUMP)));
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
    /// Start a talk when a talk button is pressed while the player has control and no message window is open.
    fn begin_talk(&mut self) {
        let pressed = self.input.confirm || self.input.alt;
        // Windows flagged 0x2000 (labels left on screen) do not hold talking up.
        self.talk = (pressed && self.user_control && msg_to::waiting(self) == 0)
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
        for a in &mut self.actors {
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
        self.screen.frame();
        self.voice.tick();
        if let Some(m) = self.movie.as_mut().filter(|m| m.playing) {
            m.elapsed += 1;
            if let Some(f) = m.frames.filter(|&f| !m.looping && m.elapsed > f) {
                m.elapsed = f;
                m.playing = false;
            }
        }
        self.camera.update();
        self.cam_frame();
        let actors = &self.actors;
        let follow = self.camera.follow.update(|w| actors.get(w).map(|a| a.pos));
        self.camera.process(follow);
        self.cam_publish();
        // The map's frame push: group 3 is always off, scene 0x8a keeps group 8 on.
        if self.scene == Some(0x8A) {
            self.hidden_map_groups &= !0x100;
        }
        self.hidden_map_groups |= 1 << 3;
        // The character engine's step of each body: the velocity this frame (from the speed its worker's engine
        // handed over, along the heading of travel: speed · 0.05 · 30 · the frame time; the player's from last
        // tick's push unless a script move runs on it), then the body step: a body that follows the ground
        // (flag 0x80 of +0x194 clear) takes the ground step on the walk mesh and its rise, else it moves by the
        // whole velocity; the velocity is cleared. A walking body (height mode 0) stays above its ground height.
        let player_push = self.player_step.take();
        let event = self.scene.unwrap_or(0);
        let mut remap = [0u32; 64];
        for (k, v) in self.engine.id_vars.iter().take(64).enumerate() {
            remap[k] = *v as u32;
        }
        let players: Vec<bool> = (0..self.actors.len()).map(|i| Some(i) == self.controlled).collect();
        for i in 0..self.actors.len() {
            if self.actors[i].model.is_none() {
                continue;
            }
            let scripted = self.workers.list.get(i).is_some_and(|wk| {
                let m = wk.move_of(i32::from(wk.level()));
                wk.u16(m + crate::workers::mv::MODE) & 0x7f != 0
            });
            let is_player = Some(i) == self.controlled;
            let a = &mut self.actors[i];
            let (vx, vz) = if is_player && !scripted {
                match player_push {
                    Some([dx, dz]) => {
                        a.yaw = dz.atan2(dx);
                        (dx, dz)
                    }
                    None => (0.0, 0.0),
                }
            } else if a.engine_speed == 0.0 {
                (0.0, 0.0)
            } else {
                let v = (f64::from(a.engine_speed) * 0.05 * 30.0 * f64::from(FRAME_TIME)) as f32;
                let h = f64::from(a.move_heading);
                ((h.cos() as f32) * v, (h.sin() as f32) * v)
            };
            let rise = if is_player && !scripted { 0.0 } else { a.chr_rise };
            let Some(ch) = self.chrs.get_mut(i).and_then(Option::as_mut) else {
                a.pos[0] += vx;
                a.pos[1] += rise;
                a.pos[2] += vz;
                continue;
            };
            ch.set_pos_keep_previous(a.pos);
            // The body record takes what the actor holds for it: speed (+0x154), facing (+0x158, Ch_SetRot),
            // heading of travel (+0x168, Ch_SetDirF).
            ch.set_f32(crate::chr::SPEED, a.engine_speed);
            ch.set_f32(crate::chr::FACING, a.yaw);
            ch.set_f32(crate::chr::HEADING, a.move_heading);
            ch.set_f32(0x4c, vx);
            ch.set_f32(0x50, rise);
            ch.set_f32(0x54, vz);
        }
        // The body step and the collision push (83d3b0) over the worker bodies.
        let walk_mesh = self.walk_mesh.take();
        self.bump_signals = body_pass(&mut self.chrs, walk_mesh.as_ref(), event, &players, &remap);
        self.walk_mesh = walk_mesh;
        for i in 0..self.actors.len() {
            let Some(ch) = self.chrs.get(i).and_then(Option::as_ref) else { continue };
            if self.actors[i].model.is_none() {
                continue;
            }
            let mut p = ch.pos();
            if self.actors[i].chr_mode == 0
                && ch.u32(crate::chr::FLAGS) & 0x80 == 0
                && self.walk_mesh.is_some()
            {
                let ground = ch.f32(crate::chr::GROUND);
                if ground < p[1] {
                    p[1] = ground;
                }
            }
            self.actors[i].pos = p;
        }
        let mut player_speed = 0.0;
        // This tick's input makes the player's next push (only while the player has control).
        if let Some(p) = self.player.filter(|&p| p < self.actors.len()) {
            // The water speed while the character engine is in water (mode 2, as the player control reads it).
            let in_water = self.actors[p].chr_mode == 2;
            let push = self.control.tick(self.input.stick, self.input.walk, in_water);
            player_speed = push.filter(|_| self.user_control).map_or(0.0, |p| p.speed);
            // The heading: the player control's camera heading kept while the stick holds its direction.
            let cam = self.input.camera_heading.unwrap_or(std::f32::consts::FRAC_PI_2);
            let vector = self.control.vector;
            let heading = self.sg.frame(cam, self.user_control, self.control.pushing(), vector, self.map_id);
            if let (Some(push), Some(h)) = (push.filter(|_| self.user_control), heading) {
                self.player_step = Some([h.cos() * push.step, h.sin() * push.step]);
            }
        }
        self.engine_motions(player_speed);
        // The motion players (after the engine's picks), then the face players, then the motions' events.
        let alt = self.alt_clock.then_some(0x100);
        for a in &mut self.actors {
            if let Some(p) = &mut a.player {
                p.frame(alt);
            }
        }
        self.chr_elements();
        self.motion_events();
        // AERUGO_MOTION_TRACE: each character's motion player state, in the oracle's log form, every tick.
        if std::env::var_os("AERUGO_MOTION_TRACE").is_some() {
            let mut out = Vec::new();
            for a in &self.actors {
                if let (Some(m), Some(p)) = (a.model, &a.player) {
                    out.push(format!("c{m:x} {}", p.trace()));
                }
            }
            if !out.is_empty() {
                eprintln!("MT {}", out.join(" | "));
            }
        }
    }

    /// The batch step for every body: first the bodies standing on their own, then the hung ones (so these
    /// take this frame's matrices of what they hang on); a body held (+0x180 set and +0x181 clear) is skipped.
    /// Per body: the motion's values into the elements (only the channels it moved this frame; the element values
    /// persist), the blink, the neck, and with +0x194 & 0x3000000 the element matrices, the tilt step, the world
    /// matrix and the option points; then its face player. (Mode 1 of the neck looks at the camera's focus side,
    /// taken as the sim camera's focus.)
    fn chr_elements(&mut self) {
        let n = self.actors.len();
        self.chr_frames.resize(n, None);
        self.chr_joints.resize(n, None);
        for pass in 0..2 {
            for w in 0..n {
                let hung = self.chrs.get(w).and_then(Option::as_ref).map(|c| c.u32(crate::chr::PARENT) != 0);
                match hung {
                    Some(h) if h == (pass == 1) => {}
                    Some(_) => continue,
                    None => {
                        if pass == 0 {
                            self.chr_frames[w] = None;
                            self.chr_joints[w] = None;
                            self.face_tick(w);
                        }
                        continue;
                    }
                }
                let held = self.chrs[w].as_ref().is_some_and(|c| c.u8(0x180) != 0 && c.u8(0x181) == 0);
                if !held {
                    self.chr_batch(w);
                }
                self.face_tick(w);
            }
        }
    }

    /// One body's part of the batch (see chr_elements).
    fn chr_batch(&mut self, w: usize) {
        use crate::chr::elements as el;
        let Some(model) = self.actors[w].model else {
            self.chr_frames[w] = None;
            return;
        };
        let rig = self.model_rigs.get(&model).cloned().flatten();
        let opt = self.model_optpos.get(&model).cloned().flatten();
        let (b30, b38) = opt.as_ref().map_or((1.0, 1.0), |o| (o.block_30, o.block_38));
        let mscale = self.model_motion_scales.get(&model).copied().flatten().unwrap_or(0.001);
        // The element values: the model's own when the body (or its model) is new.
        let fresh = !matches!(&self.chr_joints[w], Some((m, _)) if *m == model);
        if fresh {
            let joints = rig.as_ref().map_or_else(Vec::new, |r| {
                r.bones
                    .iter()
                    .map(|b| el::Joint { rotation: b.rotation, translation: b.translation, scale: b.scale })
                    .collect()
            });
            // At allocation (the model at rest).
            let parents: Vec<Option<usize>> =
                rig.as_ref().map_or_else(Vec::new, |r| r.bones.iter().map(|b| b.parent).collect());
            let elem9 = opt.as_ref().and_then(|o| o.element(9)).map(|e| e.0).filter(|&e| e != 0);
            if let Some(ch) = self.chrs[w].as_mut() {
                el::neck_init(ch, &parents, &joints, elem9.and_then(|e| usize::try_from(e).ok()));
            }
            self.chr_joints[w] = Some((model, joints));
        }
        // The blink (its rand calls whether or not the model has an eyelid element).
        let eye = opt.as_ref().and_then(|o| o.element(0xd)).map(|e| e.0).filter(|&e| e != 0);
        let eye_block = opt.as_ref().map_or((0, 2, 1, [0.0; 3]), |o| o.eye_block(model));
        let mut face_frame = None;
        if let Some(ch) = self.chrs[w].as_mut()
            && let Some((_, joints)) = self.chr_joints[w].as_mut()
        {
            // The element values for the channels the motion moved this frame (it runs before the batch).
            if let Some(p) = &self.actors[w].player {
                for (j, v) in p.pose_dirty().into_iter().enumerate() {
                    if let (Some((v, d)), Some(joint)) = (v, joints.get_mut(j)) {
                        el::apply_channels(joint, v, d, mscale);
                    }
                }
            }
            face_frame = el::blink(ch, &mut self.crt_rand, joints, eye, eye_block);
        }
        if let (Some(f), Some((_, face))) = (face_frame, self.actors[w].face.as_mut()) {
            face.blink_frame(f);
        }
        // The neck, from the option points of the last batch.
        let elem9 = opt.as_ref().and_then(|o| o.element(9)).map(|e| e.0).filter(|&e| e != 0);
        let mut neck = None;
        if let Some(e9) = elem9.and_then(|e| usize::try_from(e).ok()) {
            let last = self.chr_frames[w].clone();
            let pts = last.as_ref().map_or([[0.0; 4]; 0x16], |f| f.points);
            let body = last.as_ref().map_or([0.0; 3], |f| [f.world[12], f.world[13], f.world[14]]);
            let p = |k: usize| (pts[k][3] != 0.0).then(|| el::point(&pts, k, body));
            let ch = self.chrs[w].as_ref();
            let mode = ch.map_or(0, |c| c.i8(0x40d));
            let target = match mode {
                1 => self.camera.focus.map(|f| [f[0], f[1], f[2], 1.0]),
                2 => ch.map(|c| c.u32(0x4c0)).filter(|&t| t != 0).and_then(|t| {
                    let t = t as usize - 1;
                    self.chrs.get(t).and_then(Option::as_ref)?;
                    let f = self.chr_frames.get(t).cloned().flatten()?;
                    Some(el::point(&f.points, 9, [f.world[12], f.world[13], f.world[14]]))
                }),
                3 => ch.map(|c| [c.f32(0x4c0), c.f32(0x4c4), c.f32(0x4c8), c.f32(0x4cc)]),
                _ => None,
            };
            let inp = el::NeckInput {
                p13: p(0x13),
                p14: p(0x14),
                p9: el::point(&pts, 9, body),
                target,
                side: opt.as_ref().map_or(0, |o| o.eye_block(model).0),
            };
            if let Some(ch) = self.chrs[w].as_mut() {
                neck = el::neck(ch, &inp).map(|(m, wt)| (e9, m, wt));
            }
        }
        let Some(ch) = self.chrs[w].as_ref() else { return };
        if ch.u32(crate::chr::FLAGS) & 0x0300_0000 == 0 {
            return;
        }
        let parents: Vec<Option<usize>> =
            rig.as_ref().map_or_else(Vec::new, |r| r.bones.iter().map(|b| b.parent).collect());
        let joints = self.chr_joints[w].as_ref().map_or(&[][..], |(_, j)| &j[..]);
        let elems = el::element_matrices_with(&parents, joints, neck);
        let (scale, facing) = ([ch.f32(0x5c), ch.f32(0x60), ch.f32(0x64)], ch.f32(crate::chr::FACING));
        let (hung, elem, off, k1b4) = (
            ch.u32(crate::chr::PARENT),
            ch.i32(0x1a0),
            [ch.f32(0x1a4), ch.f32(0x1a8), ch.f32(0x1ac), ch.f32(0x1b0)],
            ch.f32(0x1b4),
        );
        let pos = ch.pos();
        let mut tilt = None;
        if ch.f32(0x160) != 0.0 {
            let t = tilt_step(ch.f32(0x15c), ch.f32(0x160), ch.f32(0x164));
            if let Some(ch) = self.chrs[w].as_mut() {
                ch.set_f32(0x15c, t);
            }
            tilt = Some(t);
        }
        let dr = self.actors[w].draw_offset;
        let place = [dr[0] + pos[0], dr[1] + pos[1], dr[2] + pos[2]];
        let parent = (hung != 0).then(|| hung as usize - 1).and_then(|p| {
            let f = self.chr_frames.get(p).cloned().flatten()?;
            let pm = self.actors.get(p).and_then(|a| a.model)?;
            let pb30 = self.model_optpos.get(&pm).cloned().flatten().map_or(1.0, |o| o.block_30);
            let pe = usize::try_from(elem).ok().and_then(|e| f.elems.get(e)).copied().unwrap_or(el::IDENTITY);
            Some(el::hung_world(&f.world, &pe, pb30, k1b4, off))
        });
        let world = parent.unwrap_or_else(|| el::body_world(b30, b38, scale, tilt, facing, place));
        let entries = opt.as_ref().map_or(&[][..], |o| &o.entries[..]);
        let points = el::option_points(&world, &elems, entries, b30, b38);
        // The shadow centre (+0x694..+0x6a0, the old one kept at +0x684): between the feet (points 7 and 8: their
        // mean x and z, the higher y) and point 1, in x and z; the feet's height.
        let body = [world[12], world[13], world[14]];
        let p1 = el::point(&points, 1, body);
        let p7 = el::point(&points, 7, body);
        let p8 = el::point(&points, 8, body);
        let feet = [(p7[0] + p8[0]) * 0.5, if p7[1] <= p8[1] { p8[1] } else { p7[1] }, (p7[2] + p8[2]) * 0.5];
        let centre = [(feet[0] + p1[0]) * 0.5, feet[1], (feet[2] + p1[2]) * 0.5, p7[3]];
        let p0 = el::point(&points, 0, body);
        if let Some(ch) = self.chrs[w].as_mut() {
            for k in 0..4u32 {
                let old = ch.f32(0x694 + k * 4);
                ch.set_f32(0x684 + k * 4, old);
                ch.set_f32(0x694 + k * 4, centre[k as usize]);
            }
            // Point 0 (its height less +0x4f4) at +0x6b4; the centre at point 1's height at +0x6a4; the head-to-feet
            // span +0x510 and the body's height above the feet +0x514; the centre lowered by 0.8 of the span at
            // +0x6c4.
            for (k, v) in [p0[0], p0[1] - ch.f32(0x4f4), p0[2], p0[3]].into_iter().enumerate() {
                ch.set_f32(0x6b4 + k as u32 * 4, v);
            }
            for (k, v) in [centre[0], p1[1], centre[2], p1[3]].into_iter().enumerate() {
                ch.set_f32(0x6a4 + k as u32 * 4, v);
            }
            let span = (ch.f32(0x6a8) - ch.f32(0x698)).abs();
            ch.set_f32(0x510, span);
            ch.set_f32(0x514, (ch.f32(0x10) - ch.f32(0x698)).abs());
            for (k, v) in [centre[0], feet[1] - span * 0.8, centre[2], p7[3]].into_iter().enumerate() {
                ch.set_f32(0x6c4 + k as u32 * 4, v);
            }
        }
        self.chr_frames[w] = Some(ChrFrame { world, elems, points });
    }

    /// The events the motions reached this frame: face patterns (kind 3: the id's high byte is the request kind,
    /// 4 sets the pattern to the low byte).
    fn motion_events(&mut self) {
        let mut events = Vec::new();
        for w in 0..self.actors.len() {
            events.clear();
            let Some(p) = &mut self.actors[w].player else { continue };
            p.events(&mut events);
            for e in &events {
                if e.kind == 3 && e.id >> 8 == 4 {
                    self.set_face_pattern(w, i32::from(e.id & 0xFF));
                }
            }
        }
    }

    /// Set a character's face pattern (the value's low byte) and the pattern's flags (next byte).
    fn set_face_pattern(&mut self, w: usize, value: i32) {
        let sprites = self
            .actors
            .get(w)
            .and_then(|a| a.model)
            .and_then(|m| self.model_faces.get(&m))
            .and_then(|f| f.1.clone());
        let actor = self.actor(w);
        if let Some((_, f)) = &mut actor.face {
            let mut reports = Vec::new();
            f.set_pattern((value & 0xFF) as u8, ((value >> 8) & 0xFF) as u8, &mut reports);
            if let Some(s) = sprites {
                face::apply_reports(&mut actor.face_layers, &s, &reports);
            }
        }
    }

    /// A new body record for model `m` (Ch_Allocate's setter chain): its collision radius base (+0x4e4) is the
    /// model block's +0x1c, the character data's info block +4 (82b880 with −1).
    pub(crate) fn allocate_body(&mut self, m: u16) -> crate::chr::Chr {
        self.model_data(m);
        let block = self.model_blocks.get(&m).copied().flatten();
        if block.is_none() {
            log::error!("GAP: model {m:#x} has no character data block (its block values are unset)");
        }
        let mut ch = crate::chr::Chr::allocate(m, block.unwrap_or_default());
        match self.model_radii.get(&m).copied().flatten() {
            Some(r) => ch.set_f32(0x4e4, r),
            None => log::error!("GAP: model {m:#x} has no character data block (its radius base is unset)"),
        }
        ch
    }

    /// A model's character data (`.chr`), read once: collision radius, motion lists and translation scale, face
    /// data and sprites, PS2 joint count.
    fn model_data(&mut self, m: u16) {
        if self.model_radii.contains_key(&m) {
            return;
        }
        let (Some(vfs), Some(game)) = (self.vfs.clone(), self.game) else { return };
        let chr = chr_path(game, m).and_then(|p| vfs.read(&p).ok());
        self.model_radii.insert(m, chr.as_deref().and_then(chr_radius));
        self.model_blocks.insert(m, chr.as_deref().and_then(chr_block));
        self.model_motion_lists.insert(m, chr.as_deref().and_then(chr_motion_lists));
        self.model_optpos.insert(m, chr.as_deref().and_then(chr_optpos).map(std::sync::Arc::new));
        self.model_motion_scales.insert(m, chr.as_deref().and_then(chr_motion_scale));
        self.model_rigs.insert(m, chr.as_deref().and_then(chr_rig).map(std::sync::Arc::new));
        self.model_joints.insert(m, chr.as_deref().and_then(chr_joints).unwrap_or(0));
        self.model_relations.insert(
            m,
            chr.as_deref()
                .map(motion_engine::chr_relations)
                .unwrap_or_default()
                .into_iter()
                .map(std::sync::Arc::new)
                .collect(),
        );
        let face = chr.as_deref().and_then(face::FaceData::from_chr).map(std::sync::Arc::new);
        let sprites =
            model_path(game, m).and_then(|p| face::CharFace::load(&vfs, &p)).map(std::sync::Arc::new);
        self.model_faces.insert(m, (face, sprites));
    }

    /// Read a motion file into the engine's groups (unless it is in): a character's resident file (kind 0), the
    /// event's (kind 1), the shared party file (kind 2). A file not read before waits for the read, and the shared
    /// party file read at a game's start lands during that wait — before a resident file, after an event's.
    pub fn read_motion_file(&mut self, key: motion_engine::FileKey) {
        if self.motion_groups.has(key) {
            return;
        }
        let (Some(vfs), Some(game)) = (self.vfs.clone(), self.game) else { return };
        let root = match game {
            Game::Ffx => "ffx_ps2/ffx/master/jppc",
            Game::Ffx2 => "ffx_ps2/ffx2/master/jppc",
        };
        let path = match key.0 {
            0 => ps2_chr_dir(game, key.2).map(|d| format!("{d}/mot/resident{}.mgrp", key.1)),
            1 => self.event_path.rsplit_once('/').map(|(dir, file)| {
                let name = file.trim_end_matches(".ebp");
                format!("{dir}/{name}{:02}.mgrp", key.1)
            }),
            _ => Some(format!("{root}/battle/mot/regmot.mgrp")),
        };
        // A resident file of a character the engine does not list, or an empty one, is not read at all.
        let data = path.and_then(|p| vfs.read(&p).ok());
        if key.0 == 0 && (!chr_listed(&vfs, game, key.2) || data.as_ref().is_none_or(|d| d.len() <= 16)) {
            return;
        }
        let groups =
            data.and_then(|d| aerugo_formats::motion::MotionGroup::parse_all(&d).ok()).unwrap_or_default();
        let waited = !self.motion_groups.cached(key);
        if waited && key.0 == 0 {
            self.party_motions_land();
        }
        self.motion_groups.add(key, groups);
        if waited && key.0 == 1 {
            self.party_motions_land();
        }
    }

    /// The shared party motion file, if a game start asked for it.
    fn party_motions_land(&mut self) {
        if std::mem::take(&mut self.motion_groups.party_pending) {
            self.motion_groups.party_pending = false;
            let key = (2, 0, 0);
            if !self.motion_groups.has(key) {
                self.read_motion_file(key);
            }
        }
    }

    /// Bind a model's motions to a worker: its character data, its resident motion file 0 and a new player.
    fn bind_motions(&mut self, w: usize, model: u16) {
        self.model_data(model);
        self.read_motion_file((0, 0, model));
        let joints = self.model_joints.get(&model).copied().unwrap_or(0);
        self.actor(w).player = Some(motion_engine::Player::new(joints));
    }

    /// Set a worker's motion (scripts and the character engine).
    /// The idle motion (chr, mode, key) for the models with the motion tables (block +0x6c; mode → list 5, 6, 7,
    /// 8): the list's motion for the key, else (mode ≠ 0) the ground list's first; set by chr_set_motion.
    pub(crate) fn mode_motion(&mut self, w: usize, mode: u8, key: usize) {
        let Some(model) = self.actors.get(w).and_then(|a| a.model) else { return };
        let lists = self.model_motion_lists.get(&model).and_then(|l| l.clone());
        let Some(lists) = lists else { return };
        let pick = |m: usize, k: usize| lists.get(m).and_then(|l| l.get(k)).copied().filter(|&id| id != 0);
        let id = pick(usize::from(mode).min(3), key).or_else(|| if mode != 0 { pick(0, 0) } else { None });
        if let Some(id) = id {
            self.set_motion(w, id as i32);
        }
    }

    fn set_motion(&mut self, w: usize, id: i32) -> Option<motion_engine::SetMotion> {
        let model = self.actors.get(w)?.model?;
        let groups = &self.motion_groups;
        let relations = self.model_relations.get(&model).map_or(&[][..], Vec::as_slice);
        let a = self.actors.get_mut(w)?;
        let r = a.player.as_mut()?.set_motion(groups, model, relations, id);
        a.motion = Some(id);
        Some(r)
    }

    /// The face animation players (after the character engine's motions): a character gets one when its model has face
    /// data; each frame it plays and reports to the face layer stack.
    /// One body's face player (at the end of its batch step).
    fn face_tick(&mut self, w: usize) {
        let mut reports = Vec::new();
        let a = &mut self.actors[w];
        let Some(model) = a.model else { return };
        let Some((data, sprites)) = self.model_faces.get(&model) else { return };
        if a.face.as_ref().map(|f| f.0) != Some(model) {
            a.face = data.clone().map(|d| (model, face::Face::new(d)));
            a.face_layers.clear();
        }
        let Some((_, f)) = &mut a.face else { return };
        f.tick(&mut self.crt_rand, &mut reports);
        if let Some(s) = sprites {
            face::apply_reports(&mut a.face_layers, s, &reports);
        }
    }

    /// The character engine's own motions: a body bound to a character with no script
    /// motion playing stands, walks or runs by the speed the engine moves it at (the running move's, or the
    /// player's push), picking its standing, walking or running slot; a slot under 0x1000 picks from the model's
    /// own list.
    fn engine_motions(&mut self, player_speed: f32) {
        for w in 0..self.actors.len() {
            let a = &self.actors[w];
            let Some(model) = a.model else { continue };
            // The speed the body moves at: what its worker's engine handed over, or the player's push.
            let scripted = self.workers.list.get(w).is_some_and(|wk| {
                let m = wk.move_of(i32::from(wk.level()));
                wk.u16(m + crate::workers::mv::MODE) & 0x7f != 0
            });
            let speed = if Some(w) == self.player && !scripted { player_speed } else { a.engine_speed };
            self.actors[w].engine_speed = speed;
            if self.actors[w].player.as_ref().is_none_or(|p| p.playing) {
                continue;
            }
            let a = &self.actors[w];
            // On the ground: standing, walking or running; in water: still, or diving below one under
            // the water level, else swimming at the surface.
            let (group, slot) = match a.chr_mode {
                0 if speed == 0.0 => (0, 0),
                0 if speed <= a.run_threshold => (0, 1),
                0 => (0, 2),
                2 if speed == 0.0 => (2, 16),
                2 if self.water_level + 1.0 <= a.pos[1] => (2, 19),
                2 => (2, 18),
                _ => continue,
            };
            let value = a.motion_slots[slot];
            let id = if value & 0xFFFF < 0x1000 {
                // The model's list for the group, else the first of its ground list.
                let lists = self.model_motion_lists.get(&model).and_then(|l| l.as_ref());
                let pick =
                    |g: usize, i: usize| lists.and_then(|l| l[g].get(i)).copied().filter(|&id| id != 0);
                match pick(group, value as usize).or_else(|| if group != 0 { pick(0, 0) } else { None }) {
                    Some(id) => id as i32,
                    None => continue,
                }
            } else {
                value
            };
            // The engine's own motions do not count as playing.
            self.set_motion(w, id);
            if let Some(p) = &mut self.actors[w].player {
                p.playing = false;
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
/// A message from its stored bytes (up to the NUL) and its choice count.
pub fn message_from_bytes(bytes: Vec<u8>, word: u16, charset: &Charset) -> Message {
    let tokens = text::decode(&bytes, charset);
    let text = tokens
        .iter()
        .filter_map(|t| match *t {
            text::Token::Char(c) => Some(c),
            text::Token::LineBreak => Some('\n'),
            _ => None,
        })
        .collect();
    Message { text, raw: text::to_display(&tokens), choices: (word >> 8) as u8, word, bytes }
}

pub fn decode_event_text(data: &[u8], charset: &Charset) -> Vec<Message> {
    let Some(first) = data.get(0..2).map(|b| usize::from(u16::from_le_bytes([b[0], b[1]]))) else {
        return Vec::new();
    };
    (0..first / 8)
        .map(|i| {
            let word = |k: usize| data.get(k..k + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            let off = usize::from(word(i * 8));
            let bytes = text::string_at(data, off).unwrap_or_default().to_vec();
            message_from_bytes(bytes, word(i * 8 + 2), charset)
        })
        .collect()
}

/// A character id's PS2 folder (`chr/<kind>/<letter><number>`): kinds 0 pc, 1 mon, 2 npc, 3 sum, 4 wep, 5 obj,
/// 6 skl (motion skeletons other characters' motions play through).
pub fn ps2_chr_dir(game: Game, id: u16) -> Option<String> {
    let (dir, prefix) = match id >> 12 {
        0 => ("pc", 'c'),
        1 => ("mon", 'm'),
        2 => ("npc", 'n'),
        3 => ("sum", 's'),
        4 => ("wep", 'w'),
        5 => ("obj", 'f'),
        6 => ("skl", 'k'),
        _ => return None,
    };
    let root = match game {
        Game::Ffx => "ffx_ps2/ffx/master/jppc",
        Game::Ffx2 => "ffx_ps2/ffx2/master/jppc",
    };
    Some(format!("{root}/chr/{dir}/{prefix}{:03}", id & 0xFFF))
}

/// The engine's numbered files (`proj/battle/jp/cddata`): `cdrom.fid` gives each set's first number (i16, −1
/// none), `cdrom.fnd` each number's name (u32 count, u32 offsets, `host0:/ffx/master/jppc/…` paths).
#[derive(Debug, Clone)]
pub struct FileTable {
    sets: Vec<i16>,
    names: Vec<u8>,
}

impl FileTable {
    /// FFX's file table.
    pub fn load(vfs: &Vfs) -> Option<FileTable> {
        let dir = "ffx_ps2/ffx/proj/battle/jp/cddata";
        let fid = vfs.read(&format!("{dir}/cdrom.fid")).ok()?;
        let names = vfs.read(&format!("{dir}/cdrom.fnd")).ok()?;
        let sets = fid.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        Some(FileTable { sets, names })
    }

    /// The archive path of file `index` of `set` (`host0:/ffx/…` → `ffx_ps2/ffx/…`).
    pub fn path(&self, set: usize, index: usize) -> Option<String> {
        let base = usize::try_from(*self.sets.get(set)?).ok()?;
        let n = base + index;
        let u32_at =
            |o: usize| self.names.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if n >= u32_at(0)? as usize {
            return None;
        }
        let (a, b) = (u32_at(4 + 4 * n)? as usize, u32_at(8 + 4 * n)? as usize);
        let name = self.names.get(a..b.max(a))?;
        let name = &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())];
        let name = std::str::from_utf8(name).ok()?.strip_prefix("host0:/")?;
        (!name.is_empty()).then(|| format!("ffx_ps2/{}", name.to_ascii_lowercase()))
    }

    /// The script of FFX event number `n` (what map changes and the save image name): set 0xC holds 18 files per
    /// event, the script first.
    pub fn event_script(&self, n: usize) -> Option<String> {
        self.path(0xC, n * 18).filter(|p| p.ends_with(".ebp"))
    }

    /// The event number of a script (the engine's scene number), by its path.
    pub fn event_number(&self, script: &str) -> Option<u16> {
        (0..0x400usize)
            .find(|&n| self.event_script(n).is_some_and(|p| p.eq_ignore_ascii_case(script)))
            .map(|n| n as u16)
    }
}

/// Whether the engine can read a character's resident motion files: its number is in its type's list
/// (`chr/<kind>/<kind>.tbl`: i16 count, i16 numbers).
pub fn chr_listed(vfs: &Vfs, game: Game, chr: u16) -> bool {
    let Some(dir) = ps2_chr_dir(game, chr) else { return false };
    let Some((kind_dir, _)) = dir.rsplit_once('/') else { return false };
    let kind = kind_dir.rsplit('/').next().unwrap_or("");
    let Ok(d) = vfs.read(&format!("{kind_dir}/{kind}.tbl")) else { return false };
    let n = d.get(0..2).map_or(0, |b| i16::from_le_bytes([b[0], b[1]])).max(0) as usize;
    let number = (chr & 0xFFF) as i16;
    (0..n).any(|i| d.get(2 + 2 * i..4 + 2 * i).is_some_and(|b| i16::from_le_bytes([b[0], b[1]]) == number))
}

/// The PS2 character data of a model id (`.chr`: its header carries the collision radius among other sizes).
pub fn chr_path(game: Game, id: u16) -> Option<String> {
    let dir = ps2_chr_dir(game, id)?;
    let name = dir.rsplit('/').next()?.to_owned();
    Some(format!("{dir}/mdl/{name}.chr"))
}

/// The PS2 character data of an HD character model (`…/ps3data/chr/<kind>/<id>/mdl/d3d11/<id>.dae.phyre`).
pub fn chr_path_of_model(model_path: &str) -> Option<String> {
    let (_, rest) = model_path.split_once("/chr/")?;
    let (dir, _) = rest.split_once("/mdl/")?;
    let name = dir.rsplit('/').next()?;
    let root = if model_path.starts_with("ffx-2_data/") {
        "ffx_ps2/ffx2/master/jppc"
    } else {
        "ffx_ps2/ffx/master/jppc"
    };
    Some(format!("{root}/chr/{dir}/mdl/{name}.chr"))
}

/// A character's motion translation scale: its character record's size × 0.001, where the size is the `.chr` info
/// block's (offset at 0x58) float at +0x20 from data version 10 on, and 1 for older data.
/// A character's option positions (`.chr` list 2 at 0x20: 16-byte entries: u16 key (low 14 bits; bits 14–15 the
/// kind), i16 element, three floats) and the model block's +0x30 / +0x38 (info block +0xc / +0x20; 1.0 for data
/// before version 10).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OptPos {
    pub entries: Vec<(u16, i16, [f32; 3])>,
    pub block_30: f32,
    pub block_38: f32,
    /// The block's +0x40 (neck side), +0x42 (the eyelid's axis), +0x43 (its sign) and +0x44..+0x4c (its turn at
    /// rest), from the info block (+0x24, bytes +0x26 / +0x27, +0x28..+0x30); none before version 10.
    pub eye: Option<(u8, u8, u8, [f32; 3])>,
}

impl OptPos {
    /// The block's eye and neck values for model `id` (the block's defaults: side 0, axis 2, sign 1; model kind
    /// 0x2000: side 1, sign 0).
    pub fn eye_block(&self, id: u16) -> (u8, u8, u8, [f32; 3]) {
        self.eye.unwrap_or(if id & 0xf000 == 0x2000 { (1, 2, 0, [0.0; 3]) } else { (0, 2, 1, [0.0; 3]) })
    }
}

pub fn chr_optpos(d: &[u8]) -> Option<OptPos> {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let f = |o: usize| u32_at(o).map(f32::from_bits);
    let (at, n) = (u32_at(0x20)? as usize, u32_at(0x24)? as usize);
    let entries = if at == 0 {
        Vec::new()
    } else {
        (0..n.min(0x400))
            .map_while(|i| {
                let e = at + i * 16;
                let key = d.get(e..e + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))?;
                let elem = d.get(e + 2..e + 4).map(|b| i16::from_le_bytes([b[0], b[1]]))?;
                Some((key, elem, [f(e + 4)?, f(e + 8)?, f(e + 12)?]))
            })
            .collect()
    };
    let (block_30, block_38, eye) = if u32_at(4)? < 10 {
        (1.0, 1.0, None)
    } else {
        let info = u32_at(0x58)? as usize;
        let b = |o: usize| d.get(info + o).copied();
        // Block +0x30 = info +0xc, +0x38 = info +0x20.
        let eye = (b(0x24)?, b(0x26)?, b(0x27)?, [f(info + 0x28)?, f(info + 0x2c)?, f(info + 0x30)?]);
        (f(info + 0xc)?, f(info + 0x20)?, Some(eye))
    };
    Some(OptPos { entries, block_30, block_38, eye })
}

impl OptPos {
    /// The element of the key and its offset (kind 0x4000: the floats × (10000 / b30 / b30)
    /// · b38; kind 0: none), with the kind code it returns (0, 1, 2 for 0x8000, else 0).
    pub fn element(&self, key: u32) -> Option<(i32, [f32; 4], u8)> {
        let &(k, elem, v) =
            self.entries.iter().find(|e| u32::from(e.0 as i16 as i32 as u32 as u16 & 0x3fff) == key)?;
        let kind = k & 0xc000;
        let mut off = [0.0, 0.0, 0.0, 1.0];
        let code = match kind {
            0 => 0,
            0x4000 => {
                let s = ((10000.0f64 / f64::from(self.block_30) / f64::from(self.block_30))
                    * f64::from(self.block_38)) as f32;
                off = [
                    (f64::from(s) * f64::from(v[0])) as f32,
                    (f64::from(v[1]) * f64::from(s)) as f32,
                    (f64::from(s) * f64::from(v[2])) as f32,
                    1.0,
                ];
                1
            }
            0x8000 => 2,
            _ => 0,
        };
        Some((i32::from(elem), off, code))
    }
}

pub fn chr_motion_scale(d: &[u8]) -> Option<f32> {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let block = if u32_at(4)? < 10 { 1.0 } else { f32::from_bits(u32_at(u32_at(0x58)? as usize + 0x20)?) };
    Some(block * 0.001)
}

/// A character's collision radius from its `.chr` data: header version (u32 at 4) 10 or later has an info block
/// (offset at 0x58) whose second float is the radius; older data takes 1.
/// A model's block values (825ee0): with a version above 9 from the info block (file +0x58): +0x24 ← info +0x18,
/// +0x28 ← +0x14, +0x2c ← +8, the scale +0x34 ← +0x1c; else 1, 0, 8 and 1.
pub fn chr_block(d: &[u8]) -> Option<crate::chr::Block> {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if u32_at(4)? < 10 {
        return Some(crate::chr::Block::default());
    }
    let info = u32_at(0x58)? as usize;
    let f = |o: usize| u32_at(info + o).map(f32::from_bits);
    Some(crate::chr::Block { h24: f(0x18)?, h28: f(0x14)?, h2c: f(8)?, scale: f(0x1c)? })
}

pub fn chr_radius(d: &[u8]) -> Option<f32> {
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if u32_at(4)? < 10 {
        return Some(1.0);
    }
    let info = u32_at(0x58)? as usize;
    u32_at(info + 4).map(f32::from_bits).filter(|r| r.is_finite() && *r > 0.0)
}

/// One bone of a character's PS2 model: its parent (None: hangs on the model's root) and the rotation (radians),
/// translation and scale it has until a motion sets it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigBone {
    pub parent: Option<usize>,
    pub rotation: [f32; 3],
    pub translation: [f32; 3],
    pub scale: [f32; 3],
}

/// A character's PS2 model bones and parts, as the engine sets them up from the `.chr` model block (offset at
/// 0x10): the bone each part hangs on, and the parts drawn skinned (the others are rigid and move with their
/// bone).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChrRig {
    pub bones: Vec<RigBone>,
    pub part_bones: Vec<u16>,
    pub skinned: Vec<bool>,
}

/// The character engine's pi (3.141592, short of the nearest f32 to pi) and twice it, as its angle code uses them.
#[allow(clippy::approx_constant)]
const ENGINE_PI: f32 = 3.141592;
const ENGINE_TAU: f32 = 6.283184;
/// The same pi as a double, as the model loader's degree conversion uses it.
#[allow(clippy::approx_constant)]
const ENGINE_PI_F64: f64 = 3.141592;

/// An angle brought within half a turn with the engine's constants.
fn ps2_wrap_angle(mut a: f32) -> f32 {
    while a > ENGINE_PI {
        a -= ENGINE_TAU;
    }
    while a < -ENGINE_PI {
        a += ENGINE_TAU;
    }
    a
}

/// A character's bones and parts from its `.chr` data. The model block holds the part count (+6), skinned part
/// count (+8), bone count (+10), the part list (+0x10, 0x28-byte records, 0x18 for model ids 0x831..0x1313; the
/// part's bone is the first u16), the skinned part list (+0x14, 12-byte records, part number at +2) and the bone
/// table (+0x1c, 20 bytes: u16 parent (its own index: the root), i16 rotation X/Y/Z in hundredths of a degree,
/// i16 translation in thousandths, i16 scale in 1/4096 kept as whole numbers); offsets are from the block.
pub fn chr_rig(d: &[u8]) -> Option<ChrRig> {
    let u16_at = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let i16_at = |o: usize| u16_at(o).map(|v| v as i16);
    let u32_at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let model = u32_at(0x10)?;
    let ident = u16_at(model + 4)?;
    let (parts, skinned_count, bone_count) = (u16_at(model + 6)?, u16_at(model + 8)?, u16_at(model + 10)?);
    let stride = if ident > 0x830 && ident < 0x1314 { 0x18 } else { 0x28 };
    let list = model + u32_at(model + 0x10)?;
    let part_bones =
        (0..usize::from(parts)).map(|k| u16_at(list + k * stride)).collect::<Option<Vec<_>>>()?;
    let skin_list = model + u32_at(model + 0x14)?;
    let mut skinned = vec![false; usize::from(parts)];
    for k in 0..usize::from(skinned_count) {
        if let Some(flag) = skinned.get_mut(usize::from(u16_at(skin_list + k * 12 + 2)?)) {
            *flag = true;
        }
    }
    let table = model + u32_at(model + 0x1c)?;
    let bones = (0..usize::from(bone_count))
        .map(|i| {
            let at = table + i * 20;
            let parent = usize::from(u16_at(at)?);
            let v = |k: usize| i16_at(at + 2 + k * 2);
            let angle = |k: usize| {
                v(k).map(|x| ps2_wrap_angle((f64::from(x) / 100.0 * ENGINE_PI_F64 / 180.0) as f32))
            };
            let scale =
                |k: usize| v(k).map(|x| ((i32::from(x) + ((i32::from(x) >> 31) & 0xFFF)) >> 12) as f32);
            Some(RigBone {
                parent: (parent != i).then_some(parent),
                rotation: [angle(0)?, angle(1)?, angle(2)?],
                translation: [v(3)? as f32 / 1000.0, v(4)? as f32 / 1000.0, v(5)? as f32 / 1000.0],
                scale: [scale(6)?, scale(7)?, scale(8)?],
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(ChrRig { bones, part_bones, skinned })
}

/// A character's PS2 model joint count from its `.chr` data (the model block at the offset at 0x10, count at +10).
pub fn chr_joints(d: &[u8]) -> Option<usize> {
    let model = d.get(0x10..0x14).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))? as usize;
    d.get(model + 10..model + 12).map(|b| usize::from(u16::from_le_bytes([b[0], b[1]])))
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
    start_movie(g, number, flags, playing);
}

/// A movie by number with its flags.
pub(crate) fn start_movie(g: &mut FieldState, number: u32, flags: i32, playing: bool) {
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

/// An event's map number: the script header's word at +4 is the offset (from the script base, header word +4
/// of the file) of a table whose first i16 is the map the event loads.
pub fn event_map_number(script: &[u8]) -> Option<u16> {
    let u32_at =
        |o: usize| script.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let base = u32_at(4)?;
    let table = u32_at(base + 4)?;
    let m = script.get(base + table..base + table + 2).map(|b| i16::from_le_bytes([b[0], b[1]]))?;
    u16::try_from(m).ok()
}

/// The HD map of FFX map number `m`: the engine reads files 2m (textures) and 2m + 1 (the map package) of set
/// 0x13, whose folder (`map/<area>/<id>/`) names the map.
pub fn map_paths_by_number(vfs: &Vfs, table: &FileTable, m: u16) -> (Option<String>, Option<String>) {
    let Some(pkg) = table.path(0x13, usize::from(m) * 2 + 1) else { return (None, None) };
    let Some(rest) = pkg.split("/map/").nth(1) else { return (None, None) };
    let mut parts = rest.split('/');
    let (Some(area), Some(id)) = (parts.next(), parts.next()) else { return (None, None) };
    map_folder_paths(vfs, Game::Ffx, area, id)
}

fn map_folder_paths(vfs: &Vfs, game: Game, area: &str, id: &str) -> (Option<String>, Option<String>) {
    let root = match game {
        Game::Ffx => "ffx_data",
        Game::Ffx2 => "ffx-2_data",
    };
    let model = format!("{root}/gamedata/ps3data/map/{area}/{id}/mdl/d3d11/{id}.dae.phyre");
    let layers = format!("{root}/gamedata/ps3data/map/{area}/{id}/2d/mdl/d3d11/{id}.dae.phyre");
    let exists = |p: String| vfs.size(&p).is_some().then_some(p);
    (exists(model), exists(layers))
}

/// FFX-2's maps by the event name (its file table is not read yet): area from the first four letters, map from
/// the first six (`bjyt0000` → `bjyt/bjyt00`).
pub fn map_paths(vfs: &Vfs, game: Game, event: &str) -> (Option<String>, Option<String>) {
    if event.len() < 6 || !event.is_ascii() {
        return (None, None);
    }
    map_folder_paths(vfs, game, &event[..4], &event[..6])
}

/// The player control's heading state (the camera heading it goes by, an offset, the stick's angle
/// atan2(right − left, up − down), the angle the heading was last taken at, a fixed heading switch and value).
/// With the control mode at 3 (its value from the start): while the stick is
/// pushed the camera heading is taken again only when the stick turns more than 0.349066 (and always on maps 0x19c,
/// 0x220); let go, it is taken every frame. The body heads (fixed or camera) − (stick − offset).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SgControl {
    pub camera: f32,
    pub offset: f32,
    pub stick: f32,
    pub last: f32,
    pub fixed_on: bool,
    pub fixed: f32,
}

impl SgControl {
    /// The heading fix: into ±3.141592.
    pub fn dir_fix(a: f32) -> f32 {
        crate::chr::wrap(a)
    }

    /// One frame (the player control's heading part): the heading the body takes, if pushed.
    pub fn frame(
        &mut self,
        cam: f32,
        control: bool,
        pushing: bool,
        vector: [f32; 2],
        map_id: u32,
    ) -> Option<f32> {
        // Without control: the offset is cleared and nothing else changes.
        if !control {
            self.offset = 0.0;
            return None;
        }
        let moving = vector[0] != 0.0 || vector[1] != 0.0;
        if pushing {
            self.stick = (f64::from(vector[0])).atan2(f64::from(vector[1])) as f32;
        }
        if !moving {
            self.camera = cam;
            self.last = -1000.0;
            self.offset = 0.0;
            return None;
        }
        let turn = (f64::from((f64::from(self.stick) - f64::from(self.last)) as f32)).abs();
        if f64::from(SgControl::dir_fix(f32::from_bits(0x3eb2_b8c0))) < turn {
            self.camera = cam;
            self.last = self.stick;
            self.offset = 0.0;
        }
        if matches!(map_id, 0x19c | 0x220) {
            self.camera = cam;
        }
        let a = (f64::from(self.stick) - f64::from(self.offset)) as f32;
        let base = if self.fixed_on { self.fixed } else { self.camera };
        Some(-((f64::from(a) - f64::from(base)) as f32))
    }

    /// The camera heading now, the offset fix(degrees · π / 180) + the stick angle.
    pub fn turn_to(&mut self, cam: f32, degrees: f32) {
        self.camera = cam;
        let fix = SgControl::dir_fix(degree_rad(degrees));
        self.offset = (f64::from(fix) + f64::from(self.stick)) as f32;
    }
}

/// (degrees · 3.141592) / 180, as the engine computes it.
fn degree_rad(d: f32) -> f32 {
    ((f64::from(d) * f64::from(crate::chr::BODY_PI)) / 180.0) as f32
}

/// The screen distortion: the PS2 block natives set (flags, +4, +0x18..+0x28, the colour bytes +0x2c, a value
/// and a timer) and the remaster's Distortion switch (+0x7c on, +0x7e fresh, +0x78 its start value).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Distortion {
    pub flags: u32,
    pub value_e7c: f32,
    /// +8..+0x14 (the reset clears them).
    pub values_e80: [u32; 4],
    pub point: [f32; 3],
    pub value_e9c: f32,
    pub count_ea0: i32,
    pub rgb: [u8; 3],
    pub value_ea8: u32,
    pub value_ded28: u32,
    pub on: bool,
    pub fresh: bool,
    pub start: f32,
}

impl Distortion {
    /// The parameters' defaults (the flags stay).
    pub fn reset(&mut self) {
        self.value_e7c = 0.0;
        self.values_e80 = [0; 4];
        self.point = [f32::from_bits(0x3cf5_c28f), f32::from_bits(0x3ca3_d70a), f32::from_bits(0x3ba3_d70a)];
        self.value_e9c = f32::from_bits(0x4280_0000);
        self.count_ea0 = 0x60;
        self.rgb = [0x80; 3];
    }
}

impl FieldState {
    /// The game's file table.
    pub fn file_table(vfs: &Vfs) -> Option<FileTable> {
        FileTable::load(vfs)
    }

    /// A worker's switch (flag 0x200 of +0x34); returns the new state.
    pub fn worker_switch(&mut self, w: usize, on: bool) -> bool {
        let Some(wk) = self.workers.list.get_mut(w) else { return false };
        let f = wk.u16(0x34);
        wk.set_u16(0x34, if on { f | 0x200 } else { f & !0x200 });
        on
    }

    /// Whether control is blocked (system block byte 1 bit 4, byte 3 bits 0x84).
    fn control_blocked(&self) -> bool {
        self.workers.sys.flags1 & 4 != 0 || self.workers.sys.mode3 & 0x84 != 0
    }

    /// Control on: unless blocked, every kind-1 worker with a body, a party character and its switch on
    /// (flag 0x200 of +0x34, or with byte 0 bit 0x20 flag 8 of +0x36) is taken over (+0x34 |= 0x20), and the
    /// player's one becomes the controlled body; byte 0 bit 1 set.
    pub fn control_on(&mut self) {
        let mut controlled = None;
        if !self.control_blocked() {
            let alt = self.workers.sys.flags0 & 0x20 != 0;
            let player = self.workers.sys.player;
            for w in 0..self.workers.list.len() {
                let wk = &mut self.workers.list[w];
                if wk.kind != 1 {
                    continue;
                }
                let switch = if alt { wk.u16(0x36) >> 3 & 1 } else { wk.u16(0x34) >> 9 & 1 };
                if wk.u8(crate::workers::BIND) == 1 && wk.i32(0x40) != -1 && switch != 0 {
                    wk.set_u16(0x34, wk.u16(0x34) | 0x20);
                    if wk.id() as i16 == player && self.chrs.get(w).is_some_and(Option::is_some) {
                        controlled = Some(w);
                    }
                }
            }
        }
        self.controlled = controlled;
        self.workers.sys.flags0 |= 2;
        self.user_control = true;
    }

    /// Control off: every taken-over kind-1 worker with a body is let go (its record's +0x564 and its body's
    /// speed 0); no body controlled; byte 0 bit 1 cleared.
    pub fn control_off(&mut self) {
        for w in 0..self.workers.list.len() {
            let wk = &mut self.workers.list[w];
            if wk.kind == 1 && wk.u8(crate::workers::BIND) == 1 && wk.u16(0x34) & 0x20 != 0 {
                wk.set_u16(0x34, wk.u16(0x34) & !0x20);
                wk.set_u32(0x564, 0);
                if let Some(ch) = self.chrs.get_mut(w).and_then(Option::as_mut) {
                    ch.set_f32(crate::chr::SPEED, 0.0);
                }
            }
        }
        self.controlled = None;
        self.workers.sys.flags0 &= !2;
        self.user_control = false;
    }

    /// 86a130: control off, then on again unless byte 1 bit 2 or byte 3 bits 0x84 block it, when byte 0 had 0x22
    /// before.
    pub fn control_refresh(&mut self) {
        let kept = self.workers.sys.flags0;
        self.control_off();
        if self.workers.sys.flags1 & 4 == 0 && self.workers.sys.mode3 & 0x84 == 0 && kept & 0x22 != 0 {
            self.control_on();
        }
    }

    /// Control checked again: off, then on when not blocked and the kept byte 0 had 0x22; bit 1 of byte 0
    /// restored from the kept byte.
    pub fn control_recheck(&mut self) {
        let kept = self.workers.sys.flags0;
        self.control_off();
        if !self.control_blocked() && kept & 0x22 != 0 {
            self.control_on();
        }
        self.workers.sys.flags0 = self.workers.sys.flags0 & !2 | kept & 2;
        self.user_control = self.workers.sys.flags0 & 2 != 0;
    }

    /// A new walk mesh: every body in use looks its triangle up again.
    fn forget_triangles(&mut self) {
        for c in self.chrs.iter_mut().flatten() {
            if c.u8(2) != 0 {
                c.set_u16(0x824, 0xffff);
            }
        }
    }

    /// Kind 1 loads the first page's walk mesh (and sets the guide map model), kind 2 the second page's
    ///; the kind is cleared.
    pub fn apply_page_switch(&mut self) {
        let (kind, a, b) = self.page_switch;
        let page = match kind {
            1 if a >= 0 => Some(a),
            2 if b >= 0 => Some(b),
            _ => None,
        };
        if let (Some(p), Some(d)) = (page, &self.map_package)
            && let Some(g) = Ground::from_walk_mesh_page(d, p as usize)
        {
            self.ground = Some(g);
            self.walk_mesh = walk_mesh::WalkMesh::from_map_package(d, p as usize);
            self.forget_triangles();
        }
        self.page_switch.0 = 0;
    }
}

/// The remaster's map id: a hash of the map package path between `jppc` and `bin/` (`/map/bjyt/bjyt00/`).
/// The body step and the collision push (83d3b0) over the bodies in use and shown: up to two passes, each
/// stepping every body (the ground step for bodies on the walk mesh, else the whole velocity; the velocity is then
/// cleared) and pushing overlapping pairs apart (their push goes into the velocity the next pass applies); then the
/// bodies a walker touched for the first time get the bump signal. Returns the bumped bodies' workers.
fn body_pass(
    chrs: &mut [Option<crate::chr::Chr>],
    walk_mesh: Option<&walk_mesh::WalkMesh>,
    event: u16,
    players: &[bool],
    remap: &[u32; 64],
) -> Vec<usize> {
    use crate::chr::FLAGS;
    let eligible = |ch: &crate::chr::Chr| ch.u8(2) != 0 && (ch.u8(0x180) == 0 || ch.u8(0x181) != 0);
    for ch in chrs.iter_mut().flatten().filter(|ch| eligible(ch)) {
        ch.set_u32(FLAGS, ch.u32(FLAGS) & !0x4000);
    }
    let mut pass = 0;
    loop {
        let mut list = Vec::new();
        for (i, slot) in chrs.iter_mut().enumerate() {
            let Some(ch) = slot.as_mut().filter(|ch| eligible(ch)) else { continue };
            if ch.u32(FLAGS) & 0x100 == 0 {
                list.push(i);
            }
            let id = ch.u16(0);
            if ch.u32(FLAGS) & 0x80 == 0 {
                // The engine's re-find flag is only set by its debug pad handling.
                let player = players.get(i).copied().unwrap_or(false);
                walk_mesh::ground_step(walk_mesh, ch, id, event, player, remap, false);
                ch.set_f32(0x10, ch.f32(0x50) + ch.f32(0x10));
            } else {
                for k in 0..3u32 {
                    ch.set_f32(0xc + k * 4, ch.f32(0x4c + k * 4) + ch.f32(0xc + k * 4));
                }
            }
            for k in 0..3u32 {
                ch.set_f32(0x4c + k * 4, 0.0);
            }
            ch.set_f32(0x4e8, (ch.f32(0x64) + ch.f32(0x5c)) * 0.5 * ch.f32(0x4e4));
            let bottom = if ch.f32(0x51c) == 0.0 { ch.f32(0x6a8) } else { ch.f32(0x10) - ch.f32(0x51c) };
            ch.set_f32(0x520, bottom);
        }
        // By depth (selection, the first of equals stays).
        for a in 0..list.len() {
            let mut m = a;
            for b in a..list.len() {
                let z = |k: usize| chrs[list[k]].as_ref().map_or(0.0, |c| c.f32(0x14));
                if z(b) < z(m) {
                    m = b;
                }
            }
            list.swap(a, m);
        }
        let mut pushes = 0;
        for a in 0..list.len() {
            for b in a + 1..list.len() {
                let (ia, ib) = (list[a], list[b]);
                let (Some(ca), Some(cb)) = (chrs[ia].clone(), chrs[ib].clone()) else { continue };
                let (ra2, rb2) = (ca.f32(0x4e8) * 2.0, cb.f32(0x4e8) * 2.0);
                if ca.f32(0x14) + ra2 < cb.f32(0x14) - rb2 {
                    break;
                }
                let both_height = ca.u32(FLAGS) & 0x4000_0000 != 0 && cb.u32(FLAGS) & 0x4000_0000 != 0;
                if cb.f32(0xc) - rb2 > ca.f32(0xc) + ra2
                    || ca.f32(0xc) - ra2 > rb2 + cb.f32(0xc)
                    || (both_height && !(ca.f32(0x520) <= cb.f32(0x698) && cb.f32(0x520) <= ca.f32(0x698)))
                {
                    continue;
                }
                let r = ca.f32(0x4e8) + cb.f32(0x4e8);
                let d = xz_distance(ca.pos(), cb.pos());
                if r < d {
                    continue;
                }
                let (mut ca, mut cb) = (ca, cb);
                pushes += push_apart(&mut ca, &mut cb, r, d);
                if ca.u32(FLAGS) & 0x200 != 0 && ca.f32(crate::chr::SPEED) != 0.0 {
                    cb.set_u32(FLAGS, cb.u32(FLAGS) | 0x4000);
                }
                if cb.u32(FLAGS) & 0x200 != 0 && cb.f32(crate::chr::SPEED) != 0.0 {
                    ca.set_u32(FLAGS, ca.u32(FLAGS) | 0x4000);
                }
                chrs[ia] = Some(ca);
                chrs[ib] = Some(cb);
            }
        }
        pass += 1;
        if pushes == 0 || pass > 1 {
            let mut bumped = Vec::new();
            for (i, ch) in chrs.iter_mut().enumerate() {
                let Some(ch) = ch.as_mut().filter(|ch| eligible(ch)) else { continue };
                let f = ch.u32(FLAGS);
                if f & 0x4000 != 0 && f & 0x8000 == 0 {
                    bumped.push(i);
                }
                ch.set_u32(FLAGS, f & !0x8000 | (f & 0x4000) << 1);
            }
            return bumped;
        }
    }
}

/// The distance on the ground plane (83e3e0).
fn xz_distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dz) = (b[0] - a[0], b[2] - a[2]);
    (f64::from(dx * dx + dz * dz)).sqrt() as f32
}

/// chr_push_apart (83ecd0): the overlap `r − d` shared out by the push weights (+0x500; both negative: by how far
/// each moved this frame) along the unit direction from b to a, into the velocities; a body given a push gets
/// flag 0x20. Returns whether there was a push.
fn push_apart(a: &mut crate::chr::Chr, b: &mut crate::chr::Chr, r: f32, d: f32) -> i32 {
    use crate::chr::{FLAGS, PUSH_WEIGHT};
    let (mut wa, mut wb) = (1.0f32, 1.0f32);
    let mut moved = false;
    if b.f32(PUSH_WEIGHT) >= 0.0 {
        if a.f32(PUSH_WEIGHT) >= 0.0 {
            wa = a.f32(PUSH_WEIGHT);
            wb = b.f32(PUSH_WEIGHT);
            if wa == 0.0 && wb == 0.0 {
                (wa, wb) = (1.0, 1.0);
            }
        } else {
            wb = 0.0;
        }
    } else {
        wa = 0.0;
        if a.f32(PUSH_WEIGHT) < 0.0 {
            moved = true;
            let prev = |c: &crate::chr::Chr| [c.f32(0x1c), c.f32(0x20), c.f32(0x24)];
            let ma = xz_distance(prev(a), a.pos());
            let mb = xz_distance(prev(b), b.pos());
            if mb == ma {
                return 0;
            }
            let m = if mb <= ma { ma } else { mb };
            wa = ma / m;
            wb = mb / m;
        }
    }
    let (dx, dz) = (a.f32(0xc) - b.f32(0xc), a.f32(0x14) - b.f32(0x14));
    let len = (f64::from(dx * dx + dz * dz)).sqrt() as f32;
    let (ux, uz) = if len != 0.0 { (dx / len, dz / len) } else { (dx, dz) };
    let fa = (wa * (r - d)) / (wa + wb);
    if fa < 0.0 {
        return 0;
    }
    let fb = (r - d) - fa;
    // The moving case gives a the share measured for b and b the one for a.
    let (sa, sb) = if moved { (fa, fb) } else { (fb, fa) };
    let give = |c: &mut crate::chr::Chr, k: f32| {
        let vx = c.f32(0x4c) + ux * k;
        let vz = c.f32(0x54) + uz * k;
        c.set_f32(0x4c, vx);
        c.set_f32(0x54, vz);
        if vx != 0.0 || vz != 0.0 {
            c.set_u32(FLAGS, c.u32(FLAGS) | 0x20);
        }
    };
    if sa != 0.0 {
        give(a, sa);
    }
    if sb != 0.0 {
        give(b, -sb);
    }
    1
}

pub fn map_id(path: &str) -> u32 {
    let (Some(i), Some(j)) = (path.find("jppc/"), path.find("bin/")) else { return 0 };
    let Some(part) = path.get(i + 4..j) else { return 0 };
    let (mut h, mut k) = (0i32, 0x7ab7i32);
    for c in part.bytes() {
        h = (i32::from(c as i8) + h * k) % 0x2717;
        k = k * 0x6af5 % 0x2716;
    }
    if h < 0 { (h + 0x2717) as u32 } else { h as u32 }
}

/// The remaster's language type (its current language's +4), from the configured language as the remaster sets it
/// (config index 1 us 1, 2 fr 2, 3 de 4, 4 it 5, 5 es 3, 6 kr 9, 7 ch 10; anything else, as simplified Chinese
/// would be, all zero like Japanese).
pub fn language_type(lang: Language) -> Option<u32> {
    Some(match lang {
        Language::Japanese | Language::ChineseSimplified => 0,
        Language::English => 1,
        Language::French => 2,
        Language::Spanish => 3,
        Language::German => 4,
        Language::Italian => 5,
        Language::Korean => 9,
        Language::ChineseTraditional => 10,
    })
}

/// The language type's +0: 0 Japanese (and the unlisted), 1 the western languages, 3 Korean and
/// Chinese.
pub fn language_region(lang: Language) -> u32 {
    match lang {
        Language::Japanese | Language::ChineseSimplified => 0,
        Language::Korean | Language::ChineseTraditional => 3,
        _ => 1,
    }
}

impl Event {
    /// Load an event script (`.ebp`), its text in `lang`, and set up the machine.
    pub fn load(vfs: &Vfs, game: Game, path: &str, lang: Language) -> Result<Event, String> {
        let data = vfs.read(path).map_err(|e| e.to_string())?;
        let script = Script::parse(&data).map_err(|e| e.to_string())?;
        let file_base = if data.starts_with(b"EV01") {
            data.get(4..8).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        } else {
            0
        };
        let name = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".ebp").to_owned();
        let actors = script
            .workers
            .iter()
            .map(|w| Actor { flags: trigger::default_signal_bits(w.kind), ..Actor::default() })
            .collect();
        let mut state = FieldState {
            water_level: WATER_LEVEL_AT_START,
            actors,
            entrances: script.entrances(),
            data_areas: script.workers.iter().map(|w| w.data_areas).collect(),
            shared_data: script.shared_data(),
            kinds: script.workers.iter().map(|w| w.kind).collect(),
            cam_slots: vec![0; script.workers.len()],
            camera_worker: script.header_u16(0x18).unwrap_or(0xFFFF),
            chr_worker_count: {
                let h = |at| i32::from(script.header_u16(at).unwrap_or(0));
                h(0x36) - h(0x1c) - h(0x16) - h(0x14)
            },
            vfs: Some(vfs.clone()),
            game: Some(game),
            event_path: path.to_owned(),
            ..FieldState::default()
        };
        let kinds = state.kinds.clone();
        state.setup_workers(&kinds);
        if let Some(tp) = event_text_path(game, path, lang)
            && let Ok(bytes) = vfs.read(&tp)
            && let Ok(cs) = vfs.read(&text::charset_path(game, lang)).map(|b| Charset::parse(&b))
            && let Ok(cs) = cs
        {
            state.messages = decode_event_text(&bytes, &cs);
            state.charset = Some(std::sync::Arc::new(cs.clone()));
            state.texts.event = Some(bytes.clone().into());
        }
        state.texts.macrodic =
            vfs.read_localized("ffx_ps2/ffx/master/jppc/menu/macrodic.dcp", lang).ok().map(Into::into);
        // FFX events load the map their header names; FFX-2 still goes by the event name.
        let by_number = (game == Game::Ffx)
            .then(|| FileTable::load(vfs).zip(event_map_number(&data)))
            .flatten()
            .map(|(table, m)| map_paths_by_number(vfs, &table, m));
        let (map_model, map_layers) = by_number.unwrap_or_else(|| map_paths(vfs, game, &name));
        // The remaster's scene settings (the map cluster's GlobalCamera), which the fog getters read.
        state.hd_scene = map_model
            .as_deref()
            .and_then(|p| vfs.read(p).ok())
            .and_then(|d| aerugo_formats::phyre::Cluster::parse(d).ok())
            .and_then(|c| aerugo_formats::mesh::scene_settings(&c))
            .map(|s| fog::HdScene::from_settings(&s));
        if game == Game::Ffx {
            state.scene = FileTable::load(vfs).and_then(|t| t.event_number(path));
            // The vibration data (set 0x18, file 8).
            state.vibration.data = FileTable::load(vfs)
                .and_then(|t| t.path(0x18, 8))
                .and_then(|p| vfs.read(&p).ok())
                .map(Into::into);
        }
        state.screen.mode_0x102 = state.scene == Some(0x102);
        // The map's group mask at load: every group but 3 (all of them only under a debug option).
        state.hidden_map_groups = 1 << 3;
        state.texture_animations =
            map_model.as_deref().and_then(|m| texture_animation::TextureAnimations::load(vfs, m));
        if game == Game::Ffx {
            state.items = crate::items::ItemTables::load(vfs, lang);
            state.stat_tables = crate::stats::StatTables::load(vfs, lang);
            state.new_game_tables = crate::new_game::NewGameTables::load(vfs, lang);
            state.texts.commands = state.items.as_ref().map(|t| t.commands.data().into());
            state.texts.items = state.items.as_ref().map(|t| t.items.data().into());
            state.texts.key_items = state.items.as_ref().map(|t| t.key_items.data().into());
            state.texts.equipment_names = state.items.as_ref().map(|t| t.equipment_names.data().into());
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
        state.shapes.file_base = file_base;
        // A map drawn only as pre-rendered layers names its package through its 2D model's folder.
        let vpa = map_model.as_deref().or(map_layers.as_deref()).and_then(walk_mesh_path).or_else(|| {
            (game == Game::Ffx && name.len() >= 6 && name.is_ascii())
                .then(|| format!("ffx_ps2/ffx/master/jppc/map/{}/{}/bin/mapout.vpa", &name[..4], &name[..6]))
        });
        if let Some(p) = &vpa {
            state.map_id = map_id(p);
        }
        state.language_type = language_type(lang);
        state.language_region = language_region(lang);
        state.page_switch = (0, -1, -1);
        state.distortion.reset();
        if let Some(d) = vpa.and_then(|p| vfs.read(&p).ok()) {
            state.ground = Ground::from_walk_mesh(&d);
            state.walk_mesh = walk_mesh::WalkMesh::from_map_package(&d, 0);
            state.forget_triangles();
            state.map_fog = MapFog::from_map_package(&d);
            state.ps_fog = fog::PsFog::from_map_package(&d).unwrap_or_default();
            state.map_effects = map_fx::MapEffects::from_map_package(&d);
            state.map_runtime = map_fx::MapRuntime::new(&d).map(Box::new);
            state.pre_bg = pre_bg::PreBg::from_map_package(&d, name.get(..6).unwrap_or(&name));
            state.map_display = 0x11 | if state.pre_bg.main { 0x4000 } else { 0 };
            state.map_package = Some(d.clone().into());
            // Map setup attaches the objects' and texture slots' own tracks.
            state.map_objects = map_objects::MapObjects::from_map_package(&d).map(std::sync::Arc::new);
            if let Some(m) = &state.map_objects {
                state.keyframes = map_objects::Keyframes::new(m);
                state.keyframes.setup(m, 0);
            }
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
        // The effect loader's tick comes early in the game's frame, before the event step.
        effects::frame_start(&mut self.state);
        // The event step's page switch, before the shapes and the scripts; the pad words: the
        // buttons let go since last frame.
        self.state.apply_page_switch();
        {
            let held = held_word(&self.state);
            self.state.pad_released = (self.state.pad_prev_held ^ held) & self.state.pad_prev_held;
            self.state.pad_prev_held = held;
            let stick = self.state.input.stick;
            self.state.pad.refresh(held, stick);
        }
        self.state.begin_talk();
        self.state.talk_begin();
        // The shapes step twice before the scripts run, then the vibration channels.
        for _ in 0..2 {
            self.state.shapes.step(&self.vm.memory);
        }
        self.state.vibration.frame();
        // The positional sounds: the listeners, the fade level, every slot.
        {
            let st = &mut self.state;
            let player = st.player.and_then(|p| st.workers.list.get(p)).map(crate::workers::Worker::position);
            st.pos_sounds.listen(st.cam_view, player);
            let scene = st.scene.unwrap_or(0);
            let image = &self.vm.memory.global[0];
            let story = image.get(0xbec..0xbee).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            st.pos_sounds.frame(&self.vm.memory, &st.workers, scene, story, &mut st.sound);
        }
        let before = self.state.player.and_then(|p| self.state.actors.get(p)).map(|a| a.pos);
        // The block's press check before its workers (a press while a window waits sets the advance bit).
        let pressed = pad_word(&self.state);
        msg_to::press(&mut self.state, pressed);
        self.vm.frame(&mut self.natives.host(&mut self.state));
        // After the workers the windows' states follow their frames; then the message module's frame.
        msg_to::status(&mut self.state);
        let held = held_word(&self.state);
        msg_to::step(&mut self.state, held, &self.vm.memory.global[0]);
        // The event system counts the frames it runs once its setup is over.
        if self.vm.phase == aerugo_script::vm::Phase::Running {
            self.state.workers.sys.frames = self.state.workers.sys.frames.wrapping_add(1);
        }
        self.state.player_start = self.state.player.and_then(|p| self.state.actors.get(p)).map(|a| a.pos);
        // The background layers step after the scripts, before the characters.
        {
            let st = &mut self.state;
            st.pre_bg.frame(&mut st.crt_rand, crate::traced_tables::sine(), st.map_id, st.language_type);
        }
        self.state.update();
        // The running effect's update (the main loop's effect step after the field's process).
        effects::frame_after_scripts(&mut self.state);
        // The map's keyframes play once a frame after the scripts and characters, on a clock of two ticks a frame.
        if let Some(m) = self.state.map_objects.clone() {
            self.state.keyframes.update(&m, (self.tick * 2) as u32);
            // Texture slots whose keyframes picked a frame stop the remaster's animation of the frame's name.
            if let Some(t) = &mut self.state.texture_animations {
                for (slot, set) in self.state.keyframes.texture_frames_set.iter().enumerate() {
                    if let (true, Some(frame)) = (*set, self.state.keyframes.texture_frames[slot])
                        && let Some(name) = m.texture_slots[slot].frames.get(frame as usize)
                    {
                        t.set_frame(name, frame);
                    }
                }
            }
        }
        // The draw phase: the map's effects (while they are on and the map is shown), then the running effect's
        // draw and its frame callback.
        self.state.vfx.clear();
        map_fx::frame_draw(&mut self.state);
        effects::frame_draw(&mut self.state);
        // The remaster's own animations step by the frame time (a 30 Hz frame here).
        if let Some(t) = &mut self.state.texture_animations {
            t.update(1.0 / 30.0);
        }
        // The message draw's effects on the reveal (late in the frame).
        msg_to::draw_pass(&mut self.state, &self.vm.memory.global[0]);
        self.walk_encounters(before);
        self.tick += 1;
        // The next frame's map change check (it comes before that frame's scripts; nothing runs in between).
        if let Some(next) = map_change::frame(&mut self.state, &self.vm.memory.global[0]) {
            self.state.next_loading_screen = next.loading_screen;
            self.state.jump = Some((next.map, next.entrance));
        }
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

    /// Read the character data of models that appeared since the last call (hosts call it before each tick).
    pub fn load_motion_frames(&mut self, vfs: &Vfs) {
        if self.state.vfs.is_none() {
            self.state.vfs = Some(vfs.clone());
        }
        let models: Vec<u16> = self.state.actors.iter().filter_map(|a| a.model).collect();
        for m in models {
            self.state.model_data(m);
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

/// A worker by index as atel_worker_by_index takes it: negative or past the last is worker 0.
fn target(g: &FieldState, c: &mut NativeCall<'_>) -> usize {
    let i = c.stack.pop_int();
    usize::try_from(i).ok().filter(|&i| i < g.workers.list.len()).unwrap_or(0)
}

fn window(c: &mut NativeCall<'_>) -> usize {
    c.stack.pop_int().clamp(0, 7) as usize
}

/// Natives that set a value for later (an actor's, a worker's or the field's) and give back what the original
/// returns; the values are kept by (native, worker) for the parts of the engine not ported yet.
fn register_settings(n: &mut Natives<FieldState>) {
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
    // 0114(a, b): system byte 2 bit 2 = a != 0, bit 1 = b != 0; returns a != 0.
    n.register(
        0x0114,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            let s = &mut g.workers.sys;
            s.flags2 = s.flags2 & 0xf9 | u8::from(a != 0) << 2 | u8::from(b != 0) << 1;
            Value::Int(i32::from(a != 0))
        }),
    );
    // 0148(v, worker): the worker's +0xbc = v | 0x80000000 (0 stays 0) and +0xb4 = −10000.0.
    n.register(
        0x0148,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let v = c.stack.pop_int() as u32;
            let w = usize::try_from(i).ok().filter(|&w| w < g.workers.list.len()).unwrap_or(0);
            let stored = if v != 0 { v | 0x8000_0000 } else { 0 };
            if let Some(wk) = g.workers.list.get_mut(w) {
                wk.set_u32(0xbc, stored);
                wk.set_f32(0xb4, -10000.0);
            }
            Value::Int(stored as i32)
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
    // 500e(v): the body's +0x194 bit 0x80 = v & 1 (set: it leaves the walk mesh, its ground normal (0, −1, 0)), and
    // the walk-mesh shade bit 0x40000 the opposite (82aa70). 5060(v): the shade bit = v & 1 (8340f0).
    n.register(
        0x500E,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as u32 & 1;
            let w = c.worker;
            if let Some(ch) = g.chrs.get_mut(w).and_then(Option::as_mut) {
                let f = ch.u32(crate::chr::FLAGS) & !0x80 | v << 7;
                if v != 0 {
                    ch.set_f32(0x7c, 0.0);
                    ch.set_f32(0x80, -1.0);
                    ch.set_f32(0x84, 0.0);
                }
                ch.set_u32(crate::chr::FLAGS, f & !0x40000 | (v ^ 1) << 18);
                g.actor(w).shade_from_ground = v == 0;
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5060,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as u32 & 1;
            let w = c.worker;
            if let Some(ch) = g.chrs.get_mut(w).and_then(Option::as_mut) {
                ch.set_u32(crate::chr::FLAGS, ch.u32(crate::chr::FLAGS) & !0x40000 | v << 18);
                g.actor(w).shade_from_ground = v != 0;
            }
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
            if g.chrs.get(w).is_none_or(Option::is_none) {
                return Value::Int(0);
            }
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
            if g.chrs.get(w).is_some_and(Option::is_some) {
                g.actor(w).color[3].set(alpha, frames);
            }
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
    // 500f(v): body hit off (+0x194 bit 0x100 = v & 1, Ch_DisableBodyHit): it neither blocks nor bumps.
    n.register(
        0x500F,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as u32 & 1;
            let w = c.worker;
            if let Some(ch) = g.chrs.get_mut(w).and_then(Option::as_mut) {
                ch.set_u32(crate::chr::FLAGS, ch.u32(crate::chr::FLAGS) & !0x100 | v << 8);
                g.actor(w).no_body_hit = v != 0;
            }
            Value::Int(0)
        }),
    );
    // Map group on (non-zero) or off; returns the group. Scene 0x188 keeps group 14 on.
    n.register(
        0x8000,
        Native::result(|g, c| {
            let mut on = c.stack.pop_int();
            let group = c.stack.pop_int();
            if g.scene == Some(0x188) && group == 14 {
                on = 1;
            }
            let bit = 1u32 << (group as u32 & 31);
            if on == 0 {
                g.hidden_map_groups |= bit;
            } else {
                g.hidden_map_groups &= !bit;
            }
            Value::Int(group)
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
    // Camera shakes, offsets and moves the follow camera does not take yet; map layer switches; 00a3 (effects).
    // Character data disposal, binding and motion switches whose results scripts do not branch on (the recordings
    // show these constants).
    // The model's drawing offset from its position (x, y, z), for a worker with a character.
    n.register(
        0x5016,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            if g.actors.get(c.worker).is_some_and(|a| a.model.is_some()) {
                g.actor(c.worker).draw_offset = [x, y, z];
            }
            Value::Int(0)
        }),
    );
    // Map display objects: 0197 binds the worker to one; 0198(object, track, once) and 0199(track, once; the bound
    // object) attach a keyframe track, for bound workers only (result 1, else 0).
    n.register(
        0x0197,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            let a = g.actor(c.worker);
            a.display_object = Some(id as u16);
            a.bind_kind = 5;
            a.bind_id = id as u16;
            g.sync_bind(c.worker);
            Value::Int(0)
        }),
    );
    // 01a5(slot, frame): a texture slot's frame; returns 1.
    n.register(
        0x01A5,
        Native::result(|g, c| {
            let frame = c.stack.pop_int() as u32;
            let slot = c.stack.pop_int() as u32;
            if let Some(m) = g.map_objects.clone() {
                g.keyframes.set_texture_frame(&m, slot, frame);
            }
            Value::Int(1)
        }),
    );
    // 01c9(target, kind): stop the keyframe slots on the target of that kind (any kind when negative);
    // returns 1.
    n.register(
        0x01C9,
        Native::result(|g, c| {
            let kind = c.stack.pop_int();
            let target = c.stack.pop_int();
            g.keyframes.stop(target as u16, u16::try_from(kind).ok().filter(|_| kind >= 0));
            Value::Int(1)
        }),
    );
    fn attach(g: &mut FieldState, c: &NativeCall<'_>, object: Option<i32>, track: i32, once: i32) -> Value {
        let Some(own) = g.actors.get(c.worker).filter(|a| a.bind_kind == 5).and_then(|a| a.display_object)
        else {
            return Value::Int(0);
        };
        if let Some(m) = g.map_objects.clone() {
            let object = object.unwrap_or(i32::from(own));
            g.keyframes.attach_object(&m, object as usize, track as usize, once as u16, (c.frame * 2) as u32);
        }
        Value::Int(1)
    }
    n.register(
        0x0198,
        Native::result(|g, c| {
            let once = c.stack.pop_int();
            let track = c.stack.pop_int();
            let object = c.stack.pop_int();
            attach(g, c, Some(object), track, once)
        }),
    );
    n.register(
        0x0199,
        Native::result(|g, c| {
            let once = c.stack.pop_int();
            let track = c.stack.pop_int();
            attach(g, c, None, track, once)
        }),
    );
    // The memory card check and the title (memochek, test20): save data present, the answers the recordings show
    // (01c6 1, 0243 3, 01c5 0, 0253 1), so a new game goes on to the title.
    // 01c5(a, mode): the Al Bhed dictionary read from the save (85b1a0 / 85b590 / 85d8f0): +0x210 bit 4 off,
    // [1330230] = a; the read's state starts at 2, so the result is 0 with mode 1, else 2; bit 4 on again. The
    // remaster's save screen path of worker 0x5045 is not ported (reported).
    n.register(
        0x01C5,
        Native {
            init: Some(|g, c| {
                let mode = c.stack.pop_int();
                let a = c.stack.pop_int();
                g.workers.sys.flags210 &= !0x10;
                g.globals.save_read_1330230 = a;
                c.scratch[1] = mode as i8 as i32;
                if g.workers.list.get(c.worker).is_some_and(|w| w.u16(crate::workers::BIND_ID) == 0x5045) {
                    log::error!("GAP: 01c5 on worker 0x5045 (the remaster's save screen) is not ported");
                }
            }),
            wait: Some(|_, _| WAIT_DONE),
            result: Some(|g, c| {
                g.workers.sys.flags210 |= 0x10;
                Value::Int(if c.scratch[1] == 1 { 0 } else { 2 })
            }),
        },
    );
    // 00fb(window): the highlighted choice of a window whose text has choices and whose choice record runs
    // (8b8010), else −1.
    n.register(
        0x00FB,
        Native::result(|g, c| {
            let w = window(c);
            let to = &g.windows[w].to;
            Value::Int(if to.text.choices && to.choice.state > 0 {
                i32::from(to.choice.highlighted)
            } else {
                -1
            })
        }),
    );
    // 00a3(worker, x, y, z): a worker's position into three float variables (86a2f0): lines (kinds 2, 5) their two
    // points' midpoint (pos +0 and +0x30); kind 4 workers have no position (the original reads through a null
    // pointer: reported).
    n.register(
        0x00A3,
        Native::result(|g, c| {
            let z = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let w = target(g, c);
            let Some(wk) = g.workers.list.get(w) else { return Value::Int(0) };
            if wk.kind == 4 {
                log::error!("GAP: 00a3 on a kind-4 worker (no position record)");
                return Value::Int(0);
            }
            let p = if matches!(wk.kind, 2 | 5) {
                std::array::from_fn(|k| (wk.pf(0x30 + k as u32 * 4) + wk.pf(k as u32 * 4)) * 0.5)
            } else {
                [wk.pf(0), wk.pf(4), wk.pf(8)]
            };
            let areas = g.data_areas.get(c.worker).copied().unwrap_or_default();
            for (desc, v) in [(x, p[0]), (y, p[1]), (z, p[2])] {
                store_float_var(c, areas, desc, v);
            }
            Value::Int(0)
        }),
    );
    // 5073(v): the body's collision height test (+0x194 bit 30 = v & 1, 82b420); without a body the value stays
    // on the stack.
    n.register(
        0x5073,
        Native::result(|g, c| {
            if let Some(ch) = g.chrs.get_mut(c.worker).and_then(Option::as_mut) {
                let v = c.stack.pop_int() as u32 & 1;
                ch.set_u32(crate::chr::FLAGS, ch.u32(crate::chr::FLAGS) & !0x4000_0000 | v << 30);
            }
            Value::Int(0)
        }),
    );
    // 5035: the frames the calling worker's motion sequence has left; 0 without a character.
    n.register(
        0x5035,
        Native::result(|g, c| {
            let a = g.actor(c.worker);
            if a.model.is_none() {
                return Value::Int(0);
            }
            Value::Int(a.player.as_ref().map_or(-1, |p| p.sequence_length()))
        }),
    );
    // 505a(record, base): texture record `record` (−1: all of them) takes image base `base` (
    // its result, the base after the record, is discarded by every script).
    n.register(
        0x505A,
        Native::result(|g, c| {
            let base = c.stack.pop_int();
            let record = c.stack.pop_int();
            let a = g.actor(c.worker);
            if a.model.is_none() {
                return Value::Int(0);
            }
            let key = (record != -1).then_some(record);
            if key.is_none() {
                a.chr_textures.bases.clear();
            }
            a.chr_textures.bases.retain(|(k, _)| *k != key);
            a.chr_textures.bases.push((key, base));
            Value::Int(base)
        }),
    );
    // 5071(_): the texture records load their image bases; returns 0.
    n.register(
        0x5071,
        Native::result(|g, c| {
            c.stack.pop_int();
            let a = g.actor(c.worker);
            if a.model.is_some() {
                a.chr_textures.loads += 1;
            }
            Value::Int(0)
        }),
    );
    // 508d(t): the character's texture animation timer; returns 0.
    n.register(
        0x508D,
        Native::result(|g, c| {
            let t = c.stack.pop_int();
            let a = g.actor(c.worker);
            if a.model.is_some() {
                a.chr_textures.anim_timer = Some(t as u8);
            }
            Value::Int(0)
        }),
    );
    // 5036(worker, part, x, y, z) / 5037(worker, x, y, z): option point `part` (0) of the worker's body (
    // the point when set, else the first one, else the body's place) into three float variables; nothing for a
    // worker without one.
    fn body_point(g: &mut FieldState, c: &mut NativeCall<'_>, with_part: bool) {
        let z = c.stack.pop_int();
        let y = c.stack.pop_int();
        let x = c.stack.pop_int();
        let part = if with_part { c.stack.pop_int() } else { 0 };
        let w = c.stack.pop_int().max(0) as usize;
        if g.actors.get(w).is_some_and(|a| a.model.is_some())
            && let Some(f) = g.chr_frames.get(w).cloned().flatten()
        {
            let body = [f.world[12], f.world[13], f.world[14]];
            let p = crate::chr::elements::point(&f.points, part.max(0) as usize, body);
            let areas = g.data_areas.get(c.worker).copied().unwrap_or_default();
            for (desc, v) in [(x, p[0]), (y, p[1]), (z, p[2])] {
                store_float_var(c, areas, desc, v);
            }
        }
    }
    n.register(
        0x5036,
        Native::result(|g, c| {
            body_point(g, c, true);
            Value::Int(0)
        }),
    );
    n.register(
        0x5037,
        Native::result(|g, c| {
            body_point(g, c, false);
            Value::Int(0)
        }),
    );
    // 5026(worker, element, x, y, z): the world position of an element of the worker's body (an element
    // the motion relation lists, or any without one, takes its translation × 1000).
    n.register(
        0x5026,
        Native::result(|g, c| {
            let z = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let e = c.stack.pop_int();
            let w = c.stack.pop_int().max(0) as usize;
            if g.actors.get(w).is_some_and(|a| a.model.is_some())
                && let Some(f) = g.chr_frames.get(w).cloned().flatten()
            {
                let n = f.elems.len();
                let listed = match g.actors[w].player.as_ref().and_then(|p| p.relation()) {
                    Some(r) => r.map.iter().take(n).any(|&m| i32::from(m) == e),
                    None => usize::try_from(e).is_ok_and(|e| e < n),
                };
                let p = crate::chr::elements::element_position(&f.world, &f.elems, e.max(0) as usize, listed);
                let areas = g.data_areas.get(c.worker).copied().unwrap_or_default();
                for (desc, v) in [(x, p[0]), (y, p[1]), (z, p[2])] {
                    store_float_var(c, areas, desc, v);
                }
            }
            Value::Int(0)
        }),
    );
    // Results that only follow from the arguments: the engine parts behind them are not ported yet.
    for id in [0x4034, 0x007F, 0x00CB, 0x00E7, 0x009E, 0x00FD, 0x013E] {
        n.mark_stub(id);
    }
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
            let state = g.worker_switch(c.worker, on);
            g.control_recheck();
            Value::Int(i32::from(state))
        }),
    );
    n.register(
        0x0031,
        Native::result(|g, c| {
            g.worker_switch(c.worker, true);
            g.control_recheck();
            Value::Int(1)
        }),
    );
    // The calling worker becomes the one the player controls; returns its number.
    // 0043: the system's player becomes the calling worker (its id, and its position as the player's current and
    // previous positions; none for kind 4); returns its id.
    n.register(
        0x0043,
        Native::result(|g, c| {
            g.player = Some(c.worker);
            let Some(wk) = g.workers.list.get(c.worker) else { return Value::Int(c.worker as i32) };
            let id = wk.id();
            g.workers.sys.player = id as i16;
            if wk.kind != 4 {
                let p = wk.position();
                let v = [p[0], p[1], p[2], 1.0];
                g.workers.sys.player_pos = v;
                g.workers.sys.player_prev = v;
            }
            Value::Int(i32::from(id))
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

/// A window's place as 0065 sets it: three windows move by event and language type.
fn window_place(g: &FieldState, x: i32, y: i32) -> (i32, i32) {
    let lang = g.language_type;
    match g.scene {
        Some(0x17) if (x, y) == (0x1e6, 0x104) => (0x1c7, 199),
        Some(0xab) if (x, y) == (0x100, 0x14f) && matches!(lang, Some(2 | 3)) => (x, 0x140),
        Some(0xd4) if (x, y) == (0x16, 0xdc) && lang == Some(4) => (x, 0x82),
        _ => (x, y),
    }
}

/// Native 006a's open (858b50) on the window record: state 1; with mode bit 1 the choice counts of +0x16 clamp
/// the record's choice bytes (871210: no digits → +0x22 = 0 and mode bit 6 off; no choices → mode bit 1 off and
/// +0x1e/+0x1f = 0); the mode is kept (+0x1d); the number entry (mode bit 6); the message module opens it.
pub(crate) fn msg_open(g: &mut FieldState, image: &[u8], w: usize, mode: u32) {
    let mut mode = mode;
    let win = &mut g.windows[w];
    win.atel_state = 1;
    if mode & 2 != 0 {
        let digits = (win.word16 & 0xf) as u8;
        let choices = (win.word16 >> 8) as u8;
        if digits == 0 {
            win.byte22 = 0;
            mode &= !0x40;
        } else {
            if digits <= win.byte22 {
                win.byte22 = digits - 1;
            }
            if digits <= win.byte23 {
                win.byte23 = digits - 1;
            }
        }
        if choices == 0 {
            mode &= !2;
            win.first_choice = 0;
            win.cancel_choice = 0;
        } else {
            if i32::from(choices) <= i32::from(win.first_choice as i8) {
                win.first_choice = choices - 1;
            }
            if i32::from(choices) <= i32::from(win.cancel_choice as i8) {
                win.cancel_choice = choices - 1;
            }
        }
    }
    win.mode = mode as u8;
    if mode & 0x40 != 0 {
        open_number(g, w, mode as i32);
    } else {
        g.windows[w].number = None;
    }
    msg_to::open(g, w, image);
}

/// 0084's init (85abd0): the window (0–7), the message wait's mode bit 4.
pub(crate) fn msg_wait_init(g: &mut FieldState, w: usize) {
    g.windows[w].mode |= 0x10;
}

/// 0084's wait (85ae90): closed (or closed state) → done; shown with its text all shown and the advance bit →
/// close it (the bit goes off) and wait on.
pub(crate) fn msg_wait(g: &mut FieldState, w: usize) -> u32 {
    match g.windows[w].atel_state {
        0 | 4 => WAIT_DONE,
        2 if g.windows[w].text_state() == 3 && g.workers.sys.flags0 & 4 != 0 => {
            msg_to::close(g, w);
            g.workers.sys.flags0 &= !4;
            0
        }
        _ => 0,
    }
}

/// 007d's wait (859c50): once shown with its text all shown and its choice taken, the pick is kept and the window
/// closes; done when closed. Scratch 1: the pick (−1 none).
fn choice_wait(g: &mut FieldState, c: &mut NativeCall<'_>) -> u32 {
    let w = c.scratch[0] as usize;
    let pick = |win: &Window| -> i32 {
        if let Some(e) = win.number {
            return if e.state == 2 { 0 } else { -1 };
        }
        win.to.choice.picks.first().map_or(-1, |&p| i32::from(p))
    };
    match g.windows[w].atel_state {
        0 | 4 => {
            c.scratch[1] = pick(&g.windows[w]);
            WAIT_DONE
        }
        2 => {
            let win = &g.windows[w];
            let chosen = win.number.map_or(win.to.choice.state, |e| e.state) == 2;
            if win.text_state() == 3 && chosen {
                c.scratch[1] = pick(win);
                msg_to::close(g, w);
            }
            0
        }
        _ => 0,
    }
}

/// With mode flag 0x40 a window whose text has digits (+0x16 low nibble) takes a number entry: the cursor from
/// +0x22, where cancel puts it back from +0x23, the number from +0x24.
fn open_number(g: &mut FieldState, w: usize, mode: i32) {
    let word = g.windows[w].message.and_then(|m| g.messages.get(m)).map_or(0, |m| m.word);
    let win = &mut g.windows[w];
    win.word16 = word;
    let digits = (word & 0xf) as i8;
    win.number = (mode & 0x40 != 0 && digits != 0)
        .then(|| NumberEntry::new(i32::from(win.byte22), i32::from(win.byte23), win.word24 as i32, digits));
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
    // 0241(on): bit 3 of the system block's +0x210 word; returns on != 0.
    n.register(
        0x0241,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let s = &mut g.workers.sys;
            s.flags210 = s.flags210 & !8 | u32::from(on) << 3;
            Value::Int(i32::from(on))
        }),
    );
    // Screen fades over n frames: 4004 in from black, 4005 out to black, 4006 in from white, 4007 out to white.
    fn fade(g: &mut FieldState, c: &mut NativeCall<'_>, fade_in: bool, white: bool) -> Value {
        let frames = c.stack.pop_int() as i16;
        g.screen.fade(frames, fade_in, white);
        Value::Int(0)
    }
    // 400d: wait until the black/white fade channel's frames run out.
    n.register(
        0x400D,
        Native { init: None, wait: Some(|g, _| u32::from(g.screen.channels[4].count == 0)), result: None },
    );
    n.register(0x4004, Native::result(|g, c| fade(g, c, true, false)));
    n.register(0x4005, Native::result(|g, c| fade(g, c, false, false)));
    n.register(0x4006, Native::result(|g, c| fade(g, c, true, true)));
    n.register(0x4007, Native::result(|g, c| fade(g, c, false, true)));
    // A colour fade (frames, r, g, b): from the colour to clear.
    n.register(
        0x4003,
        Native::result(|g, c| {
            let b = c.stack.pop_int() as u8;
            let gr = c.stack.pop_int() as u8;
            let r = c.stack.pop_int() as u8;
            let frames = c.stack.pop_int() as i16;
            g.screen.colour_fade(frames, [r, gr, b]);
            Value::Int(0)
        }),
    );
    // A colour filter (r, g, b, strength; 0 off): 4008 normal, 4009 the other blend; 400c turns it off.
    fn filter(g: &mut FieldState, c: &mut NativeCall<'_>, mode: u8) -> Value {
        let v = c.stack.pop_int() as i16;
        let b = c.stack.pop_int() as u8;
        let gr = c.stack.pop_int() as u8;
        let r = c.stack.pop_int() as u8;
        g.screen.filter([r, gr, b], v, mode);
        Value::Int(0)
    }
    n.register(0x4008, Native::result(|g, c| filter(g, c, 7)));
    n.register(0x4009, Native::result(|g, c| filter(g, c, 8)));
    n.register(
        0x400C,
        Native::result(|g, _| {
            g.screen.filter_off();
            Value::Int(0)
        }),
    );
    // Motion blur at v / 128 (0 off).
    n.register(
        0x400A,
        Native::result(|g, c| {
            let v = c.stack.pop_int() as i16;
            g.screen.blur(v);
            Value::Int(0)
        }),
    );
    // The accumulation trail: 4019 takes the current frame, 401a moves its alpha (alpha, frames), 401b/401c set
    // its two bytes.
    n.register(
        0x4019,
        Native::result(|g, c| {
            c.stack.pop_int();
            g.screen.accumulation_current = true;
            Value::Int(0)
        }),
    );
    n.register(
        0x401A,
        Native::result(|g, c| {
            let frames = c.stack.pop_int() as i16;
            let alpha = c.stack.pop_int() as i16;
            g.screen.accumulation(alpha, frames);
            Value::Int(0)
        }),
    );
    n.register(
        0x401B,
        Native::result(|g, c| {
            g.accumulation_bytes[0] = c.stack.pop_int() as u8;
            Value::Int(0)
        }),
    );
    n.register(
        0x401C,
        Native::result(|g, c| {
            g.accumulation_bytes[1] = c.stack.pop_int() as u8;
            Value::Int(0)
        }),
    );
    // A screen copy entry (index, value): 0 clears switch bit 0, else sets bit `value` and the screen copy
    // switch.
    n.register(
        0x4015,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            if let Some(e) = usize::try_from(i).ok().and_then(|i| g.screen_copies.get_mut(i)) {
                *e = v;
            }
            if v == 0 {
                g.screen_copy_bits &= !1;
            } else {
                g.screen_copy_bits |= 1 << (v as u32 & 31);
                g.engine.screen_copy = true;
            }
            Value::Int(0)
        }),
    );
    // 013b / 0267(window, message, first, cancel, x, y, alignment): a choice window in one call (8607a0): 0065
    // placement, frame type 0 (0066), the message (0064), text flags 0 (009d), first and cancel (0069), opened
    // with mode 2 (006a), then the choice wait (007d).
    for id in [0x013B, 0x0267] {
        n.register(
            id,
            Native {
                init: Some(|g, c| {
                    let align = c.stack.pop_int();
                    let y = c.stack.pop_int();
                    let x = c.stack.pop_int();
                    let cancel = c.stack.pop_int();
                    let first = c.stack.pop_int();
                    let msg = c.stack.pop_int();
                    let w = window(c);
                    let (x, y) = window_place(g, x, y);
                    g.last_text = msg;
                    let word =
                        usize::try_from(msg).ok().and_then(|m| g.messages.get(m)).map_or(0, |m| m.word);
                    let win = &mut g.windows[w];
                    win.x = x;
                    win.y = y;
                    win.align = align;
                    win.style = Some(0);
                    win.message = usize::try_from(msg).ok();
                    win.word16 = word;
                    win.flags = 0;
                    win.first_choice = first as u8;
                    win.cancel_choice = cancel as u8;
                    msg_open(g, &c.memory.global[0], w, 2);
                    c.scratch[0] = w as i32;
                    c.scratch[1] = -1;
                    g.windows[w].mode |= 0x20;
                }),
                ..choice_native()
            },
        );
    }
    // 0139(window, message, number, cursor, cancel cursor, x, y, alignment): a number entry window placed (0065),
    // given the message (+0x20 = 0, +0x18 flags = 0) and opened with mode 0x42, then the choice wait (007d); the
    // result is the number entered.
    n.register(
        0x0139,
        Native {
            init: Some(|g, c| {
                let align = c.stack.pop_int();
                let y = c.stack.pop_int();
                let x = c.stack.pop_int();
                let start = c.stack.pop_int();
                let cursor = c.stack.pop_int();
                let value = c.stack.pop_int();
                let msg = c.stack.pop_int();
                let w = window(c);
                g.last_text = msg;
                let win = &mut g.windows[w];
                win.x = x;
                win.y = y;
                win.align = align;
                win.message = usize::try_from(msg).ok();
                win.flags = 0;
                win.byte22 = cursor as u8;
                win.byte23 = start as u8;
                win.word24 = value as u32;
                msg_open(g, &c.memory.global[0], w, 0x42);
                c.scratch[0] = w as i32;
                c.scratch[1] = -1;
                g.windows[w].mode |= 0x20;
            }),
            wait: Some(choice_wait),
            result: Some(|g, c| Value::Int(g.windows[c.scratch[0] as usize].number.map_or(0, |e| e.value))),
        },
    );
    // 0110(window): the number its entry holds.
    n.register(
        0x0110,
        Native::result(|g, c| {
            let w = window(c);
            Value::Int(g.windows[w].number.map_or(0, |e| e.value))
        }),
    );
    n.register(0x007D, choice_native());
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
        w |= if i.stick[0] > 0.0 {
            0x2000
        } else if i.stick[0] < 0.0 {
            0x8000
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
                msg_to::close(g, w as usize);
            }),
            wait: Some(|g, c| {
                let w = &g.windows[c.scratch[0] as usize];
                if c.scratch[1] == 0 || w.atel_state == 0 { WAIT_DONE } else { 0 }
            }),
            result: Some(|_, _| Value::Int(1)),
        },
    );
    use aerugo_script::natives::heading;
    // The heading moves go along: set, read.
    n.register(
        0x0023,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            g.actor(c.worker).move_heading = v;
            Value::Float(v)
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
    // The pad: the word, one bit of it, and a wait until the given bits are let go.
    n.register(0x0046, Native::result(|g, _| Value::Int((pad_word(g) & 0xFFFF) as i32)));
    // 004d: event 0x31 reads 0 for worker 0xb while the save's byte at offset 0xc39 has bit 0x20.
    n.register(
        0x004D,
        Native::result(|g, c| {
            let bit = c.stack.pop_int() as u32 & 31;
            let id = g.workers.list.get(c.worker).map_or(0, crate::workers::Worker::id);
            let flag = c.memory.global[0].get(0xc39).copied().unwrap_or(0);
            if g.scene == Some(0x31) && flag & 0x20 != 0 && id == 0xb {
                return Value::Int(0);
            }
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
    // 0050(bit): a bit of the held word as the pad gives it (no key remap); 0047: the buttons let go this frame
    // (the remapped released word).
    n.register(
        0x0050,
        Native::result(|g, c| {
            let bit = c.stack.pop_int() as u32 & 31;
            Value::Int(i32::from(held_word(g) & 0xFFFF & (1 << bit) != 0))
        }),
    );
    n.register(0x0047, Native::result(|g, _| Value::Int((g.pad_released & 0xFFFF) as i32)));
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
    // Sound effects (not played yet): the result is the sound.
    for id in [0x00DFu16, 0x00E0] {
        n.register(id, Native::result(|_, c| Value::Int(c.stack.pop_int())));
    }
    // 00e9(on): the system's byte 1 bit 4 (86aff0); 0177(on): the caller's +0x36 bit 6, and bit 15 when on
    // (both run gates of 8641e0). The result is on ≠ 0.
    n.register(
        0x00E9,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            let s = &mut g.workers.sys;
            s.flags1 = s.flags1 & !0x10 | u8::from(on) << 4;
            Value::Int(i32::from(on))
        }),
    );
    n.register(
        0x0177,
        Native::result(|g, c| {
            let on = c.stack.pop_int() != 0;
            if let Some(w) = g.workers.list.get_mut(c.worker) {
                let f2 = w.flags2() & !0x40 | u16::from(on) << 6 | if on { 0x8000 } else { 0 };
                w.set_u16(crate::workers::FLAGS2, f2);
            }
            Value::Int(i32::from(on))
        }),
    );
}

fn register_treasure(n: &mut Natives<FieldState>) {
    // 015b(window, treasure) (85a8a0): the treasure given (MsFieldItemGet), then the system message window with
    // battle-end text 0x4016 equipment / 0x4017 key item / 0x4018 or 0x4019 one or more of an item / 0x401a gil
    // (else 0x401d); value slot 0 (type 0) the name, slot 1 (type 1) the count or the gil. Wait (85af70): the
    // message wait each frame; once it reports done, one frame (the menu 0x10001 asked for when the equipment did
    // not fit), then done. The end (85b4c0): the system's +3 bit 2 off, control checked again; 1.
    // Scratch: window, step, the equipment did not fit.
    n.register(
        0x015B,
        Native {
            init: Some(|g, c| {
                use crate::items::Treasure;
                g.sound.play_simple_pub(0x8000_0026u32 as i32, 0x3f, 0x7f);
                g.workers.sys.mode3 |= 4;
                let index = c.stack.pop_int();
                let w = window(c);
                let (got, full) = give_treasure(g, &mut c.memory.global[0], index);
                let image = &c.memory.global[0];
                let variant = crate::items::text_variant(image);
                let (text, name, number) = match (&got, g.items.clone()) {
                    (Treasure::Item(id, n), Some(t)) if *id != 0 && *n > 0 => (
                        if *n == 1 { 0x4018 } else { 0x4019 },
                        Some(g.texts.item_name(&t.items, u32::from(*id), variant)),
                        Some(i32::from(*n)),
                    ),
                    (Treasure::KeyItem(id), Some(t)) => {
                        (0x4017, Some(g.texts.key_item_name(&t.key_items, u32::from(*id), variant)), None)
                    }
                    (Treasure::Equipment(e), Some(t)) => {
                        let name = u32::from(u16::from_le_bytes([e[0], e[1]]));
                        (0x4016, Some(g.texts.equipment_name(&t.equipment_names, name, e[4], variant)), None)
                    }
                    (Treasure::Gil(gil), _) if *gil != 0 => (0x401a, None, Some(*gil)),
                    _ => (0x401d, None, None),
                };
                let win = &mut g.windows[w];
                if win.values.len() < 0x20 {
                    win.values.resize(0x20, (0xff, -1));
                }
                win.values[0].0 = 0;
                win.values[1].0 = 1;
                if let Some(v) = number {
                    win.values[1].1 = v;
                }
                if let Some(v) = name {
                    win.values[0].1 = v as i32;
                }
                system_message(g, image, w, text);
                c.scratch[0] = w as i32;
                c.scratch[1] = 0;
                c.scratch[2] = i32::from(full);
            }),
            wait: Some(|g, c| {
                let r = msg_wait(g, c.scratch[0] as usize);
                match c.scratch[1] {
                    0 => {
                        if r & WAIT_DONE != 0 {
                            c.scratch[1] = 1;
                        }
                        0
                    }
                    1 => {
                        if c.scratch[2] != 0 {
                            g.menu = Some(MenuRequest { mode: 0x10001, open: false });
                        }
                        c.scratch[1] = 2;
                        0
                    }
                    _ => WAIT_DONE,
                }
            }),
            result: Some(|g, _| {
                g.workers.sys.mode3 &= !4;
                g.control_refresh();
                Value::Int(1)
            }),
        },
    );
    // 01a7(treasure) (857b70): the same without a message. Wait (857ef0): done at once (and on this frame) when
    // everything fit; else the menu 0x10001 is asked for, then done the frame after. The result is 1.
    n.register(
        0x01A7,
        Native {
            init: Some(|g, c| {
                let index = c.stack.pop_int();
                let (_, full) = give_treasure(g, &mut c.memory.global[0], index);
                c.scratch[1] = 0;
                c.scratch[2] = i32::from(full);
            }),
            wait: Some(|g, c| {
                if c.scratch[2] == 0 {
                    return WAIT_DONE | aerugo_script::vm::WAIT_CONTINUE;
                }
                if c.scratch[1] == 0 {
                    g.menu = Some(MenuRequest { mode: 0x10001, open: false });
                    c.scratch[1] = 1;
                    return 0;
                }
                WAIT_DONE
            }),
            result: Some(|_, _| Value::Int(1)),
        },
    );
}

/// The system message window of 015b and 0216 (85a8a0, 85b280), after their value slots: battle-end text `text`
/// (TkBtlEndGetText: field variant · 4) placed by 0065 at (0x100, 0xd0) aligned 4, frame style 0 (+0x20), the
/// text with +0x16 = 0 and flags +0x18 = 0, opened (006a mode 0); control checked again (86a130) and the message
/// wait's mode bit (+0x1d |= 0x10).
pub(crate) fn system_message(g: &mut FieldState, image: &[u8], w: usize, text: u32) {
    let variant = crate::items::text_variant(image);
    let bytes = g
        .items
        .clone()
        .and_then(|t| {
            crate::items::ItemTables::text_bytes(&t.messages, text as u16, if variant { 4 } else { 0 })
                .map(<[u8]>::to_vec)
        })
        .unwrap_or_default();
    if let Some(cs) = g.charset.clone() {
        g.messages.push(message_from_bytes(bytes, 0, &cs));
    }
    let m = g.messages.len().checked_sub(1);
    let (x, y) = window_place(g, 0x100, 0xd0);
    let win = &mut g.windows[w];
    win.x = x;
    win.y = y;
    win.align = 4;
    win.style = Some(0);
    win.message = m;
    win.word16 = 0;
    win.flags = 0;
    msg_open(g, image, w, 0);
    g.control_refresh();
    msg_wait_init(g, w);
}

/// The message wait (0084): the window and a value; done once the window has closed (msg_wait); the result
/// closes it (again) and is 0.
pub(crate) fn message_wait() -> Native<FieldState> {
    Native {
        init: Some(|g, c| {
            let _b = c.stack.pop_int();
            let w = window(c);
            c.scratch[0] = w as i32;
            msg_wait_init(g, w);
        }),
        wait: Some(|g, c| msg_wait(g, c.scratch[0] as usize)),
        result: Some(|g, c| {
            msg_to::close(g, c.scratch[0] as usize);
            Value::Int(0)
        }),
    }
}

/// The choice wait (007d): the window (mode bit 5); done once it has closed; the result closes it and is the pick
/// (−1 none).
fn choice_native() -> Native<FieldState> {
    Native {
        init: Some(|g, c| {
            let w = window(c);
            c.scratch[0] = w as i32;
            c.scratch[1] = -1;
            g.windows[w].mode |= 0x20;
        }),
        wait: Some(choice_wait),
        result: Some(|g, c| {
            msg_to::close(g, c.scratch[0] as usize);
            Value::Int(c.scratch[1] as i16 as i32)
        }),
    }
}

/// Workers turned off (they stop running, but keep their requests) and on again.
fn register_workers(n: &mut Natives<FieldState>) {
    // 00a7 / 00a8(w): worker by index (86a830: negative or past the last → 0), +0x34 bit 1 cleared / set.
    fn set(g: &FieldState, c: &mut NativeCall, on: bool) -> Value {
        let w = target(g, c);
        c.effects.push(aerugo_script::vm::Effect::Enable(w, on));
        Value::Int(0)
    }
    n.register(0x00A7, Native::result(|g, c| set(g, c, false)));
    n.register(0x00A8, Native::result(|g, c| set(g, c, true)));
}

/// Player control on and off, and the player's talk reach.
fn register_control(n: &mut Natives<FieldState>) {
    n.register(
        0x005D,
        Native::result(|g, _| {
            g.control_on();
            Value::Int(1)
        }),
    );
    n.register(
        0x005E,
        Native::result(|g, _| {
            g.control_off();
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

/// Natives that set values for engine parts: save bytes, event and sound switches, the character ID effect
/// tables, the cloth wind, character values and flag bits, the fog read-back.
fn register_engine_values(n: &mut Natives<FieldState>) {
    // Save image bytes 0x12 (00bb) and 0x11 (00bc); the value comes back.
    fn save_byte(c: &mut NativeCall, at: usize) -> Value {
        let v = c.stack.pop_int();
        if let Some(b) = c.memory.global[0].get_mut(at) {
            *b = v as u8;
        }
        Value::Int(v)
    }
    n.register(0x00BB, Native::result(|_, c| save_byte(c, 0x12)));
    n.register(0x00BC, Native::result(|_, c| save_byte(c, 0x11)));
    // The event flags byte's bit 5: set (non-zero) or clear; the old bit comes back.
    n.register(
        0x00F2,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let old = (g.engine.event_flags >> 5) & 1;
            g.engine.event_flags = (g.engine.event_flags & !0x20) | (u8::from(v != 0) << 5);
            Value::Int(i32::from(old))
        }),
    );
    // Sound switch n on; returns 1.
    n.register(
        0x0151,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.engine.sound_switches |= 1 << (v as u32 & 31);
            Value::Int(1)
        }),
    );
    // Event bit n on; on scene 0x5e in room 0x186 (the save image's u16 at 0xbec), 9 also sets a switch and 10
    // clears it.
    n.register(
        0x017A,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let room = c.memory.global[0].get(0xBEC..0xBEE).map(|b| u16::from_le_bytes([b[0], b[1]]));
            if g.scene == Some(0x5E) && room == Some(0x186) {
                if v == 10 {
                    g.engine.event_switch_9 = false;
                } else if v == 9 {
                    g.engine.event_switch_9 = true;
                }
            }
            g.engine.event_bits |= 1 << (v as u32 & 31);
            Value::Int(v)
        }),
    );
    // The screen copy switch (bit 0); the old one comes back.
    n.register(
        0x4014,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let old = g.engine.screen_copy;
            g.engine.screen_copy = v & 1 != 0;
            Value::Int(i32::from(old))
        }),
    );
    // Character ID effect variable i = v (4017); the current set's effect byte i = v (403b).
    n.register(
        0x4017,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int().max(0) as usize;
            if g.engine.id_vars.len() <= i {
                g.engine.id_vars.resize(i + 1, 0);
            }
            g.engine.id_vars[i] = v;
            Value::Int(0)
        }),
    );
    n.register(
        0x403B,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            if let Some(b) = usize::try_from(g.engine.id_set as i32 * 4 + i)
                .ok()
                .and_then(|k| g.engine.id_effects.get_mut(k))
            {
                *b = v as u8;
            }
            Value::Int(0)
        }),
    );
    // The cloth wind (x, y, z).
    n.register(
        0x4036,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let y = c.stack.pop_float();
            let x = c.stack.pop_float();
            g.engine.cloth_wind = [x, y, z];
            Value::Int(0)
        }),
    );
    // Character values on the worker's body record (none: nothing): 5017 +0x182 byte, 505f +0x726 short, 5085
    // +0x17c; 5029 swaps +0x183 (the old value back, as a signed byte), 502a / 5069 bits 11 / 20 of +0x194 (the old
    // bit back); 5057(worker, v) bit 0 of another worker's body (by index); 5087: every body with bit 0 clear gets
    // bit 3.
    fn body(g: &mut FieldState, w: usize) -> Option<&mut crate::chr::Chr> {
        g.chrs.get_mut(w).and_then(Option::as_mut)
    }
    n.register(
        0x5017,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(ch) = body(g, c.worker) {
                ch.set_u8(crate::chr::HEIGHT_MODE, v as u8);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x505F,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(ch) = body(g, c.worker) {
                ch.set_u16(0x726, v as u16);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5085,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            if let Some(ch) = body(g, c.worker) {
                ch.set_f32(0x17c, v);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5029,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let Some(ch) = body(g, c.worker) else { return Value::Int(0) };
            let old = ch.u8(0x183) as i8;
            ch.set_u8(0x183, v as u8);
            Value::Int(i32::from(old))
        }),
    );
    fn swap_bit(ch: &mut crate::chr::Chr, bit: u32, v: i32) -> Value {
        let f = ch.u32(crate::chr::FLAGS);
        ch.set_u32(crate::chr::FLAGS, f & !(1 << bit) | ((v as u32) & 1) << bit);
        Value::Int((f >> bit & 1) as i32)
    }
    n.register(
        0x502A,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            body(g, c.worker).map_or(Value::Int(0), |ch| swap_bit(ch, 11, v))
        }),
    );
    n.register(
        0x5069,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            body(g, c.worker).map_or(Value::Int(0), |ch| swap_bit(ch, 20, v))
        }),
    );
    n.register(
        0x5057,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            // The worker by index: negative or past the last is worker 0.
            let w = usize::try_from(i).ok().filter(|&w| w < g.workers.list.len()).unwrap_or(0);
            body(g, w).map_or(Value::Int(0), |ch| swap_bit(ch, 0, v))
        }),
    );
    n.register(
        0x5087,
        Native::result(|g, _| {
            let attached = g.attachments.iter_mut().flatten().flatten().map(|a| &mut a.chr);
            for ch in g.chrs.iter_mut().flatten().chain(attached).filter(|ch| ch.u8(2) != 0) {
                let f = ch.u32(crate::chr::FLAGS);
                if f & 1 == 0 {
                    ch.set_u32(crate::chr::FLAGS, f | 8);
                }
            }
            Value::Int(0)
        }),
    );
    // The fog limit as last set (0–255).
    // A wait that ends at once; returns 1.
    n.register(
        0x0105,
        Native { init: Some(|_, _| {}), wait: Some(|_, _| 5), result: Some(|_, _| Value::Int(1)) },
    );
}

/// Natives of the field slice: actors, movement, turning, messages, camera, worker queries.
/// Register every field native Aerugo implements.
pub fn register(n: &mut Natives<FieldState>) {
    register_engine_values(n);
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
    // Actors: bind a character model (0134 is the same native).
    fn bind_model(g: &mut FieldState, c: &mut NativeCall) -> Value {
        let id = c.stack.pop_int();
        let a = g.actor(c.worker);
        a.model = Some(id as u16);
        a.display_object = None;
        a.bind_kind = 1;
        a.bind_id = id as u16;
        g.bind_motions(c.worker, id as u16);
        // A new character record; the body faces the worker's heading of travel and stands where the worker is.
        if g.chrs.len() <= c.worker {
            g.chrs.resize(c.worker + 1, None);
        }
        let mut ch = g.allocate_body(id as u16);
        ch.set_i32(0x190, i32::from(g.workers.list.get(c.worker).map_or(0, crate::workers::Worker::id)));
        // The body takes the worker's slot (+0xaf), and a slot below 16 names the worker.
        if let Some(wk) = g.workers.list.get(c.worker) {
            let slot = wk.u8(0xaf);
            ch.set_u8(0x184, slot);
            if slot < 16 {
                g.workers.sys.body_slots[usize::from(slot)] = i32::from(wk.id());
            }
        }
        g.chrs[c.worker] = Some(ch);
        g.sync_bind(c.worker);
        if let Some(wk) = g.workers.list.get(c.worker) {
            let (pos, dir) = (wk.position(), wk.pf(crate::workers::p::DIR_YAW));
            let a = g.actor(c.worker);
            a.yaw = dir;
            a.move_heading = dir;
            worker_natives::place_worker(g, c.worker, pos, false);
        }
        Value::Int(0)
    }
    n.register(0x0001, Native::result(bind_model));
    n.register(0x0134, Native::result(bind_model));
    // 0203(model): the model's data read (resident motions, data; done at once here), then 0001 with it and the
    // worker's reading bit cleared; returns 0001's result.
    n.register(
        0x0203,
        Native {
            init: Some(|_, c| c.scratch[0] = c.stack.pop_int()),
            wait: Some(|_, _| 1),
            result: Some(|g, c| {
                c.stack.push(Value::Int(c.scratch[0]));
                bind_model(g, c)
            }),
        },
    );
    // Bind the worker to a thing of kind 3 (0003) or 4 (0004), by number.
    fn bind(g: &mut FieldState, c: &mut NativeCall, kind: u8) -> Value {
        let v = c.stack.pop_int();
        let a = g.actor(c.worker);
        a.bind_kind = kind;
        a.bind_id = v as u16;
        g.sync_bind(c.worker);
        Value::Int(0)
    }
    n.register(0x0003, Native::result(|g, c| bind(g, c, 3)));
    n.register(0x0004, Native::result(|g, c| bind(g, c, 4)));
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
    // 5039(v): the body's collision radius base +0x4e4 = v (0 or below: the model block's +0x1c); the old one
    // back (82b880); without a body 0.
    n.register(
        0x5039,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let w = c.worker;
            let model = g.actors.get(w).and_then(|a| a.model);
            let base = model.and_then(|m| g.model_radii.get(&m).copied().flatten());
            let Some(ch) = g.chrs.get_mut(w).and_then(Option::as_mut) else { return Value::Float(0.0) };
            let old = ch.f32(0x4e4);
            let v = if v.is_nan() || v > 0.0 { v } else { base.unwrap_or(old) };
            ch.set_f32(0x4e4, v);
            let a = g.actor(w);
            a.radius = Some(v);
            Value::Float(old)
        }),
    );
    // 0126(x, y, z): the worker placed as 0013 does, with the set's flag 1 (870b20); on event 0x9f a z between
    // −35.6 and −35.5 also raises the music flag [1325b40].
    n.register(
        0x0126,
        Native::result(|g, c| {
            let [x, y, z] = pop3(c);
            if g.scene == Some(0x9f) && -35.6 < z && z < -35.5 {
                g.sound.music_0x25 = true;
            }
            if g.player == Some(c.worker) {
                g.player_placed = true;
            }
            worker_natives::place_worker(g, c.worker, [x, y, z], true);
            Value::Int(0)
        }),
    );
    // 5008 / 500d(v): the body's hide bit (+0x180 bit 0 = v; showing it marks +0x820 bit 2). In a battle on map
    // 0x21e8 only body 0x108a stays shown (the battle side is not ported: reported).
    for id in [0x5008u16, 0x500D] {
        n.register(
            id,
            Native::result(|g, c| {
                let v = c.stack.pop_int();
                if g.battle.as_ref().is_some_and(|(_, started)| *started) {
                    log::error!(
                        "GAP: 5008/500d during a battle (the battle id test of 82b2c0 is not ported)"
                    );
                }
                let w = c.worker;
                if let Some(ch) = g.chrs.get_mut(w).and_then(Option::as_mut) {
                    let b = ch.u8(0x180);
                    if v == 0 {
                        ch.set_u32(0x820, ch.u32(0x820) | 4);
                        ch.set_u8(0x180, b & !1);
                    } else {
                        ch.set_u8(0x180, b | 1);
                    }
                    g.actor(w).visible = v == 0;
                }
                Value::Int(0)
            }),
        );
    }
    // A motion over a frame range (motion, a value not used here, plays, first frame, last frame or −1).
    n.register(
        0x501A,
        Native::result(|g, c| {
            let end = c.stack.pop_int();
            let start = c.stack.pop_int();
            let loops = c.stack.pop_int();
            let _ = c.stack.pop_int();
            let motion = c.stack.pop_int();
            let w = c.worker;
            if let Some(model) = g.actors.get(w).and_then(|a| a.model) {
                let groups = &g.motion_groups;
                let relations = g.model_relations.get(&model).map_or(&[][..], Vec::as_slice);
                if let Some(p) = g.actors[w].player.as_mut() {
                    p.play_range(groups, model, relations, motion, loops as i16, start, end, false, false);
                }
                g.actors[w].motion = Some(motion);
            }
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
            if let Some(p) = &mut g.actor(c.worker).player {
                p.plays = n as i8;
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x5021,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(p) = &mut g.actor(c.worker).player {
                p.speed = v as i16;
            }
            Value::Int(0)
        }),
    );
    // The waits for a motion (0x5003, 0x501E): while the character's motion plays.
    for id in [0x5003u16, 0x501E] {
        n.register(
            id,
            Native {
                init: None,
                wait: Some(|g, c| {
                    let playing =
                        g.actors.get(c.worker).and_then(|a| a.player.as_ref()).is_some_and(|p| p.playing);
                    if playing { 0 } else { WAIT_DONE }
                }),
                result: None,
            },
        );
    }
    n.register(
        0x5000,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            g.set_motion(c.worker, m);
            Value::Int(0)
        }),
    );
    // Motions: the motion a body plays (the renderer picks it from the body's motion groups).
    for id in [0x501Bu16, 0x5040] {
        n.register(
            id,
            Native::result(|g, c| {
                let m = c.stack.pop_int();
                g.set_motion(c.worker, m);
                Value::Int(0)
            }),
        );
    }
    n.register(
        0x5047,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            let w = target(g, c);
            g.set_motion(w, m);
            Value::Int(0)
        }),
    );
    // A character's motion slot (5025(slot, motion); −1: the slot's default).
    n.register(
        0x5025,
        Native::result(|g, c| {
            let motion = c.stack.pop_int();
            let slot = c.stack.pop_int();
            if let Ok(slot) = usize::try_from(slot)
                && slot < MOTION_SLOT_DEFAULTS.len()
            {
                g.actor(c.worker).motion_slots[slot] =
                    if motion == -1 { MOTION_SLOT_DEFAULTS[slot] } else { motion };
            }
            Value::Int(0)
        }),
    );
    // The speed above which a moving character runs (5028; 0 or less: 18); the result is the old one.
    n.register(
        0x5028,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let a = g.actor(c.worker);
            let old = a.run_threshold;
            a.run_threshold = if v <= 0.0 { DEFAULT_RUN_THRESHOLD } else { v };
            Value::Float(old)
        }),
    );
    // A character's texture animation (5078(kind, value)): kind 4 sets the face pattern (value's low byte) and its
    // flags (next byte).
    n.register(
        0x5078,
        Native::result(|g, c| {
            let value = c.stack.pop_int();
            let kind = c.stack.pop_int();
            if kind == 4 {
                let a = g.actors.get(c.worker).and_then(|a| a.model);
                let sprites = a.and_then(|m| g.model_faces.get(&m)).and_then(|f| f.1.clone());
                let actor = g.actor(c.worker);
                if let Some((_, f)) = &mut actor.face {
                    let mut reports = Vec::new();
                    f.set_pattern((value & 0xFF) as u8, ((value >> 8) & 0xFF) as u8, &mut reports);
                    if let Some(s) = sprites {
                        face::apply_reports(&mut actor.face_layers, &s, &reports);
                    }
                }
            }
            Value::Int(0)
        }),
    );
    // The character engine's mode (5013: 0 on the ground, 2 in water) and the water level (4001).
    n.register(
        0x5013,
        Native::result(|g, c| {
            let m = c.stack.pop_int();
            g.actor(c.worker).chr_mode = m as u8;
            Value::Int(0)
        }),
    );
    n.register(
        0x4001,
        Native::result(|g, c| {
            g.water_level = c.stack.pop_float();
            Value::Int(0)
        }),
    );
    // Stopping the motion: the character engine picks its own again.
    for id in [0x5005u16, 0x5020] {
        n.register(
            id,
            Native::result(|g, c| {
                if let Some(p) = &mut g.actor(c.worker).player {
                    p.stop();
                }
                Value::Int(0)
            }),
        );
    }
    // Blending: the frames the next motion start blends over (5034; 0: none), and the length the next motion
    // set takes (505c; −1: the default 4, 0: none).
    n.register(
        0x5034,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(p) = &mut g.actor(c.worker).player {
                p.blend = v as i16;
                p.unblend();
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x505C,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(p) = &mut g.actor(c.worker).player {
                p.next_blend = match v {
                    -1 => 0,
                    0 => 1,
                    v => v as i16,
                };
            }
            Value::Int(0)
        }),
    );
    // Motion files: the event's (5001(n)), a character's resident file (5010(character, n)); dropping a resident
    // file of the worker's character (5011(n)) or of any (503b(character, n)), an event's (503c(n)).
    n.register(
        0x5001,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            g.read_motion_file((1, n as u16, 0));
            Value::Int(0)
        }),
    );
    n.register(
        0x5010,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            let chr = c.stack.pop_int();
            g.read_motion_file((0, n as u16, chr as u16));
            Value::Int(0)
        }),
    );
    n.register(
        0x5011,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            if let Some(m) = g.actors.get(c.worker).filter(|a| a.player.is_some()).and_then(|a| a.model) {
                g.motion_groups.remove((0, n as u16, m));
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x503B,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            let chr = c.stack.pop_int();
            if g.actors.get(c.worker).is_some_and(|a| a.player.is_some()) {
                g.motion_groups.remove((0, n as u16, chr as u16));
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x503C,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            g.motion_groups.remove((1, n as u16, 0));
            Value::Int(0)
        }),
    );
    // The alternative motion clock.
    n.register(
        0x4022,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.alt_clock = v != 0;
            Value::Int(0)
        }),
    );
    // The party character a worker plays (actors only); character 0 is taken as the player until the
    // native that names the player is known.
    // 0042(n): the worker's party character (kind 1 workers: +0x40 = n and the body's battle character
    // number), its switch on when n is 0, control rechecked; the slot (n below 8, else 15) goes to the body and
    // the party slot table unless the script mode is set.
    n.register(
        0x0042,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            if c.kind == 1 {
                if let Some(wk) = g.workers.list.get_mut(c.worker) {
                    wk.set_i32(0x40, n);
                }
                g.actor(c.worker).character = u8::try_from(n).ok();
            }
            g.worker_switch(c.worker, n == 0);
            g.control_recheck();
            let slot = if (n as u32) < 8 { n as u8 } else { 0xf };
            if g.workers.sys.mode1a == 0 {
                if let Some(wk) = g.workers.list.get_mut(c.worker) {
                    wk.set_u8(0xaf, slot);
                }
                if let Some(ch) = g.chrs.get_mut(c.worker).and_then(Option::as_mut) {
                    ch.set_u8(0x184, slot);
                }
                if slot < 16 {
                    let id = g.workers.list.get(c.worker).map_or(0, crate::workers::Worker::id);
                    g.workers.sys.body_slots[usize::from(slot)] = i32::from(id);
                }
            }
            Value::Int(n)
        }),
    );
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
    // Random numbers from stream 2: 00a9 the low 16 bits, 00a6(n) (bits · max(n, 1), unsigned) >> 16.
    n.register(0x00A9, Native::result(|g, _| Value::Int((g.random() & 0xFFFF) as i32)));
    n.register(
        0x00A6,
        Native::result(|g, c| {
            let n = c.stack.pop_int().max(1);
            Value::Int(((g.random() & 0xFFFF).wrapping_mul(n as u32) >> 16) as i32)
        }),
    );
    // Map change: target and entrance; the session carries it out between ticks.
    // (00ab is the same native: (target, entrance, 1).)
    // 0011 / 00ab(target, entrance): a map change with the save's fade; 010b / 010c without (map_change.rs).
    for id in [0x0011u16, 0x00AB] {
        n.register(
            id,
            Native::result(|g, c| {
                let entrance = c.stack.pop_int() as i8;
                let target = c.stack.pop_int();
                map_change::request(g, c, target, entrance, true);
                Value::Int(0)
            }),
        );
    }
    for id in [0x010Bu16, 0x010C] {
        n.register(
            id,
            Native::result(|g, c| {
                let entrance = c.stack.pop_int() as i8;
                let target = c.stack.pop_int();
                map_change::request(g, c, target, entrance, false);
                Value::Int(0)
            }),
        );
    }
    // Messages: text, position, open, wait for the player, close.
    // 0064(window, message): the text ([1325ac0] keeps the message), +0x16 its choice and digit word (86bf10).
    n.register(
        0x0064,
        Native::result(|g, c| {
            let msg = c.stack.pop_int();
            let w = window(c);
            g.last_text = msg;
            let word = usize::try_from(msg).ok().and_then(|m| g.messages.get(m)).map_or(0, |m| m.word);
            g.windows[w].message = usize::try_from(msg).ok();
            g.windows[w].word16 = word;
            Value::Int(0)
        }),
    );
    n.register(
        0x0065,
        Native::result(|g, c| {
            let align = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let (x, y) = window_place(g, x, y);
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
    // 006a(window, mode): open (msg_open).
    n.register(
        0x006A,
        Native::result(|g, c| {
            let mode = c.stack.pop_int();
            let w = window(c);
            msg_open(g, &c.memory.global[0], w, mode as u32);
            Value::Int(w as i32)
        }),
    );
    n.register(0x0084, message_wait());
    // The other wait takes only the window.
    n.register(
        0x007C,
        Native {
            init: Some(|g, c| {
                let w = window(c);
                c.scratch[0] = w as i32;
                msg_wait_init(g, w);
            }),
            ..message_wait()
        },
    );
    // 008f(window): closed, or shown with its text all shown.
    n.register(
        0x008F,
        Native::result(|g, c| {
            let w = window(c);
            let win = &g.windows[w];
            Value::Int(i32::from(win.atel_state == 0 || (win.atel_state == 2 && win.text_state() == 3)))
        }),
    );
    // Window flags.
    n.register(
        0x009D,
        Native::result(|g, c| {
            let flags = c.stack.pop_int();
            let w = window(c);
            g.windows[w].flags = flags as u32;
            Value::Int(w as i32)
        }),
    );
    // 0097(window): the window's state (+0x14).
    n.register(0x0097, Native::result(|g, c| Value::Int(i32::from(g.windows[window(c)].atel_state))));
    // 006b(window): msg_close.
    n.register(
        0x006B,
        Native::result(|g, c| {
            let w = window(c);
            msg_to::close(g, w);
            Value::Int(w as i32)
        }),
    );
    register_control(n);
    register_workers(n);
    // Halt: the worker stops here for good (until a request takes it elsewhere).
    n.register(0x005F, Native { init: None, wait: Some(|_, _| 0), result: None });
    // Have a worker run again at the end of this frame (its new requests start then); returns the worker.
    n.register(
        0x0116,
        Native::result(|g, c| {
            let w = c.stack.pop_int();
            // The worker by index: negative or past the last is worker 0.
            let t = usize::try_from(w).ok().filter(|&i| i < g.workers.list.len()).unwrap_or(0);
            c.effects.push(aerugo_script::vm::Effect::PassAgain(t));
            Value::Int(w)
        }),
    );
    // 0117(w): a worker (by index: negative or past the last is worker 0) marked for an extra pass this frame
    // (+0x36 & ~0x400 | 0x1200); returns w as popped.
    n.register(
        0x0117,
        Native::result(|g, c| {
            let w = c.stack.pop_int();
            let t = usize::try_from(w).ok().filter(|&i| i < g.workers.list.len()).unwrap_or(0);
            c.effects.push(aerugo_script::vm::Effect::ExtraPass(t));
            Value::Int(w)
        }),
    );
    // The camera natives on the camera system (they replace the ones above).
    cam_natives::register(n);
    chr_cache::register(n);
    voice::register(n);
    sound::register(n);
    shapes::register(n);
    map_fx::register(n);
    pre_bg::register(n);
    vibration::register(n);
    pad::register(n);
    effects::register(n);
    fog::register(n);
    text_build::register(n);
    worker_natives::register(n);
    worker_natives::register_shapes(n);
    worker_natives::register_turn_status(n);
    sys_natives::register(n);
    sys_natives::register_chr(n);
    sys_natives::register_chr2(n);
    pos_sound::register(n);
    sys_natives::register_02(n);
    // 00f3(mode): the guide map's placement (when there is one); returns the mode.
    n.register(
        0x00F3,
        Native::result(|g, c| {
            let mode = c.stack.pop_int();
            if let Some(m) = g.guide_map.as_mut() {
                m.set_mode(mode);
            }
            Value::Int(mode)
        }),
    );
    // 1000(slot, x, y, value): a point table entry (x or y negative: the entry cleared); returns 1.
    n.register(
        0x1000,
        Native::result(|g, c| {
            let value = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let slot = c.stack.pop_int().clamp(0, 31) as usize;
            let e = &mut g.point_table[slot];
            if x < 0 || y < 0 {
                e[0] = 0;
                e[1] = 0;
                return Value::Int(1);
            }
            e[0] = ((x / 8) as u8 & 0x3F) | 0x80;
            e[1] = (y / 4) as u8 & 0x7F;
            e[2..4].copy_from_slice(&(c.worker as u16).to_le_bytes());
            e[4..8].copy_from_slice(&value.to_le_bytes());
            Value::Int(1)
        }),
    );
}

#[cfg(test)]
mod tests {

    use super::*;

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
    fn chr_motion_scale_from_the_info_block() {
        let mut d = vec![0u8; 0x100];
        d[4] = 11;
        d[0x58] = 0x80;
        d[0xA0..0xA4].copy_from_slice(&10.0f32.to_le_bytes());
        // The original multiplies by 0.001f widened to a double, which rounds as an f32 product does.
        assert_eq!(chr_motion_scale(&d), Some(10.0f32 * 0.001));
        d[4] = 9;
        assert_eq!(chr_motion_scale(&d), Some(0.001));
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

    /// The engine's file table names event scripts by number, and the character lists say whose resident motion
    /// files it reads.
    #[test]
    fn map_number_from_header() {
        // Script base 0x10; its +4 points 0x20 further on, where the map number sits.
        let mut d = vec![0u8; 0x40];
        d[4..8].copy_from_slice(&0x10u32.to_le_bytes());
        d[0x14..0x18].copy_from_slice(&0x20u32.to_le_bytes());
        d[0x30..0x32].copy_from_slice(&16i16.to_le_bytes());
        assert_eq!(event_map_number(&d), Some(16));
        assert_eq!(event_map_number(&d[..0x20]), None);
    }

    /// Events load the map their header names: the attract demo and the opening share znkd06.
    /// The attract scene's weapons: rigid parts on bones, the sword's blade bone turned a quarter turn about Z.
    #[test]
    fn weapon_rigs_from_their_chr_data() {
        let Some(inst) = aerugo_vfs::install_from_env() else { return };
        let mut vfs = Vfs::new();
        if vfs.mount_install(&inst).is_err() {
            return;
        }
        let rig = |n: &str| {
            let d = vfs.read(&format!("ffx_ps2/ffx/master/jppc/chr/wep/{n}/mdl/{n}.chr")).unwrap();
            chr_rig(&d).expect("rig")
        };
        let w001 = rig("w001");
        assert_eq!(w001.part_bones, [4, 2, 3]);
        assert_eq!(w001.skinned, [false, false, false]);
        assert_eq!(w001.bones.len(), 5);
        assert_eq!(w001.bones[0].parent, None);
        assert_eq!(
            w001.bones.iter().map(|b| b.parent).collect::<Vec<_>>()[1..],
            [Some(0), Some(1), Some(2), Some(2)]
        );
        assert!((w001.bones[1].rotation[2] - 89.99f32.to_radians()).abs() < 1e-4);
        assert_eq!(w001.bones[1].scale, [1.0; 3]);
        assert_eq!(rig("w041").part_bones, [3]);
        let w011 = rig("w011");
        assert_eq!(
            (w011.part_bones.as_slice(), w011.skinned.as_slice()),
            ([3, 2, 1].as_slice(), [true; 3].as_slice())
        );
        assert!((w011.bones[6].translation[0] - 6.268).abs() < 1e-6);
    }

    #[test]
    fn events_find_their_maps() {
        let Some(inst) = aerugo_vfs::install_from_env() else { return };
        let mut vfs = Vfs::new();
        if vfs.mount_install(&inst).is_err() {
            return;
        }
        let table = FileTable::load(&vfs).expect("file table");
        for (event, map) in [
            ("lo/loopdemo/loopdemo", "znkd06"),
            ("zn/znkd0600/znkd0600", "znkd06"),
            ("bj/bjyt0400/bjyt0400", "bjyt04"),
        ] {
            let data = vfs.read(&format!("ffx_ps2/ffx/master/jppc/event/obj/{event}.ebp")).unwrap();
            let m = event_map_number(&data).expect("map number");
            let (model, _) = map_paths_by_number(&vfs, &table, m);
            assert!(model.is_some_and(|p| p.ends_with(&format!("/{map}.dae.phyre"))), "{event}");
        }
    }

    #[test]
    fn file_table_and_character_lists() {
        let Some(inst) = aerugo_vfs::install_from_env() else {
            eprintln!("skipped: set AERUGO_GAME_DIR");
            return;
        };
        let mut vfs = Vfs::new();
        vfs.mount_install(&inst).unwrap();
        let t = FileTable::load(&vfs).unwrap();
        assert_eq!(
            t.event_script(50).as_deref(),
            Some("ffx_ps2/ffx/master/jppc/event/obj/bj/bjyt0300/bjyt0300.ebp")
        );
        assert_eq!(
            t.event_script(64).as_deref(),
            Some("ffx_ps2/ffx/master/jppc/event/obj/cd/cdsp0700/cdsp0700.ebp")
        );
        assert!(vfs.read(&t.event_script(371).unwrap()).is_ok());
        assert!(chr_listed(&vfs, Game::Ffx, 0x0001));
        assert!(chr_listed(&vfs, Game::Ffx, 0x6004));
    }
}
