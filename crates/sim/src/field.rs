//! A field event: the event's script running on the script machine, with the state its natives act on (actors,
//! message windows, camera, player control). The renderer only reads this state.

use std::sync::Arc;

use aerugo_formats::Game;
use aerugo_formats::script::Script;
use aerugo_formats::text::{self, Charset};
use aerugo_script::natives::{Native, PartyLookup, wrap_angle};
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
    /// Flag bits scripts set and clear per worker.
    pub flags: u32,
    move_target: [f32; 3],
    move_speed: [f32; 2],
    moving: Option<Movement>,
    turn_target: f32,
    turn_speed: [f32; 2],
    turning: bool,
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
            flags: 0,
            move_target: [0.0; 3],
            move_speed: [0.0; 2],
            moving: None,
            turn_target: 0.0,
            turn_speed: [0.0; 2],
            turning: false,
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
}

/// Characters revealed per tick.
const TEXT_SPEED: usize = 2;

/// One event message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Message {
    /// What the player reads: characters and line breaks (control codes left out).
    pub text: String,
    /// With control codes shown as `{xx}` / `{xx:yy}`.
    pub raw: String,
    /// How many of the last lines are choices.
    pub choices: u8,
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
    /// The window's state as scripts see it: 0 closed, 2 showing.
    pub fn state(&self) -> i32 {
        if self.open { 2 } else { 0 }
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
}

impl CameraState {
    /// Vertical field of view from the screen distance (the frame is 448 high).
    pub fn fov_y(&self) -> f32 {
        let d = if self.screen_distance > 1.0 { self.screen_distance } else { 400.0 };
        2.0 * (224.0 / d).atan()
    }

    pub fn is_moving(&self) -> bool {
        self.moving.is_some()
    }

    fn update(&mut self) {
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
    /// A map change the scripts asked for (event jump), for the session to carry out.
    pub jump: Option<i32>,
    pub rng: Rng,
}

impl PartyLookup for FieldState {
    fn party_worker(&mut self, slot: i32) -> Option<usize> {
        // Party slot 0 is the player character; the others are not tracked yet.
        (slot == 0).then_some(self.player).flatten()
    }
}

impl FieldState {
    fn actor(&mut self, w: usize) -> &mut Actor {
        if self.actors.len() <= w {
            self.actors.resize(w + 1, Actor::default());
        }
        &mut self.actors[w]
    }

    /// Advance movements and turns by one tick, then player control.
    fn update(&mut self) {
        self.camera.update();
        let messages = &self.messages;
        for w in self.windows.iter_mut().filter(|w| w.open) {
            let len = w.message.and_then(|m| messages.get(m)).map_or(0, Message::len);
            w.shown = (w.shown + TEXT_SPEED).min(len);
        }
        for a in &mut self.actors {
            if let Some(m) = &mut a.moving {
                m.done += 1;
                let t = (m.done as f32 / m.frames.max(1) as f32).min(1.0);
                for k in 0..3 {
                    a.pos[k] = m.from[k] + (m.to[k] - m.from[k]) * t;
                }
                if m.done >= m.frames {
                    a.moving = None;
                }
            }
            if a.turning {
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
            Message { text, raw: text::to_display(&tokens), choices: (word(i * 8 + 2) >> 8) as u8 }
        })
        .collect()
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
        let mut state = FieldState::default();
        state.actors.resize(script.workers.len(), Actor::default());
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
        self.state.input = input;
        self.vm.frame(&mut self.natives.host(&mut self.state));
        self.state.update();
        self.tick += 1;
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
                return WAIT_DONE;
            }
            if msg.choices > 0 && g.input.stick[1] != 0.0 && g.input.stick_pressed {
                let n = usize::from(msg.choices);
                win.choice =
                    if g.input.stick[1] > 0.0 { (win.choice + n - 1) % n } else { (win.choice + 1) % n };
            }
            if g.input.confirm {
                if win.shown < msg.len() {
                    win.shown = msg.len();
                } else {
                    win.open = false;
                    return WAIT_DONE;
                }
            }
            0
        }),
        result: Some(|g, c| Value::Int(g.windows[c.scratch[0] as usize].choice as i32)),
    }
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
            let bits = c.stack.pop_int() as u32;
            g.actor(c.worker).flags |= bits;
            Value::Int(0)
        }),
    );
    n.register(
        0x0035,
        Native::result(|g, c| {
            let bits = c.stack.pop_int() as u32;
            g.actor(c.worker).flags &= !bits;
            Value::Int(0)
        }),
    );
    // Random numbers from the script stream.
    n.register(0x00A9, Native::result(|g, _| Value::Int((g.rng.next_value() & 0xFFFF) as i32)));
    n.register(
        0x00A6,
        Native::result(|g, c| {
            let n = c.stack.pop_int().max(1);
            Value::Int((((g.rng.next_value() & 0xFFFF) as i64 * i64::from(n)) >> 16) as i32)
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
            c.stack.pop_int();
            c.stack.pop_int();
            g.actor(c.worker).turning = true;
            Value::Int(0)
        }),
    );
    n.register(
        0x001B,
        Native { init: None, wait: Some(|g, c| if g.actor(c.worker).turning { 0 } else { 5 }), result: None },
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
            let win = &mut g.windows[w];
            win.open = true;
            win.shown = 0;
            win.choice = 0;
            Value::Int(w as i32)
        }),
    );
    n.register(0x0084, message_wait());
    n.register(0x007C, message_wait());
    // Window state queries: done when closed or fully shown; the raw state.
    n.register(
        0x008F,
        Native::result(|g, c| {
            let w = window(c);
            let win = &g.windows[w];
            let len = win.message.and_then(|m| g.messages.get(m)).map_or(0, Message::len);
            Value::Int(i32::from(!win.open || win.shown >= len))
        }),
    );
    n.register(0x0097, Native::result(|g, c| Value::Int(g.windows[window(c)].state())));
    n.register(
        0x006B,
        Native::result(|g, c| {
            let w = window(c);
            g.windows[w].open = false;
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
    n.register(
        0x6014,
        Native::result(|g, c| {
            // Four frame counts (start and end of two phases); the longest is the move time.
            let frames = (0..4).map(|_| c.stack.pop_int()).max().unwrap_or(1);
            g.camera.move_frames = frames.max(1) as u32;
            g.camera.start_move();
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
            messages: vec![Message { text: "Hello there".into(), raw: String::new(), choices: 0 }],
            ..FieldState::default()
        };
        let done = |vm: &Vm| vm.memory.global[0][0] == 1;
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
        tick(&mut vm, &mut state, false);
        assert!(done(&vm));
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
