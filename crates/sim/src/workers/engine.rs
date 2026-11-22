//! The per-frame engine step of one worker, run after its script's turn: a character's move and turn records
//! advance and drive its body; lines, boxes and zones test the player and raise their signals.

use super::{CTX_MOVE, CTX_ROT, FLAGS, FLAGS2, MODE9_FRAMES, SIGNAL_MASK, Worker, Workers, math, mv, p, rt};

fn d(x: f32) -> f64 {
    f64::from(x)
}

const PI: f32 = std::f32::consts::PI;
const TAU: f32 = 2.0 * std::f32::consts::PI;

/// What the engine reaches outside the worker records: the bound body, cameras and objects, signals, paths.
pub trait Env {
    /// The bound character's position (it starts over there), its mode (0 walking, 2 swimming), its speed this
    /// frame and its vertical step (×0.05), its heading of travel and its facing.
    fn chr_set_pos(&mut self, w: usize, pos: [f32; 3]);
    fn chr_mode(&self, w: usize) -> i8;
    fn chr_set_speed(&mut self, w: usize, v: f32);
    fn chr_set_rise(&mut self, w: usize, v: f32);
    fn chr_set_dir(&mut self, w: usize, v: f32);
    fn chr_dir(&self, w: usize) -> f32;
    fn chr_yaw(&self, w: usize) -> f32;
    fn chr_set_yaw(&mut self, w: usize, v: f32);
    /// A camera bound worker's point (its camera handle, the point with w 1).
    fn camera_point(&mut self, handle: u32, v: [f32; 4]);
    /// A bound object's or camera id's place (facing, position).
    fn bind_place(&mut self, kind: u8, id: u16, rot: [f32; 4], pos: [f32; 4]);
    /// An engine signal of a type on a worker (a request for the entry its kind or table names).
    fn signal(&mut self, w: usize, t: u8);
    /// The same without queuing it twice (the zone test of kind 6).
    fn signal_once(&mut self, w: usize, t: u8);
    /// A point along a move path (mode 5 the quadratic path, 6 the cubic one) at `t`.
    fn path_point(&self, table: u32, cubic: bool, t: f32) -> [f32; 3];
}

impl Workers {
    /// The step of worker `w` after its turn (the original's per-worker engine call). `ctx` is the level the turn
    /// ended on.
    pub fn step(&mut self, w: usize, env: &mut dyn Env) {
        let Some(wk) = self.list.get(w) else { return };
        if wk.flags() & 0x80 == 0 {
            return;
        }
        let player = self.sys.player;
        let ctx = wk.cur_ctx();
        match wk.kind {
            1 => {
                self.move_step(w, ctx, env);
                self.rot_step(w, ctx);
                let wk = &mut self.list[w];
                if wk.flags() & 0x20 == 0 {
                    apply_move(wk, w, ctx, env);
                    if wk.chr_bound() {
                        env.chr_set_dir(w, wk.pf(p::DIR_YAW));
                    }
                    if let Some(yaw) = wk.clear_move_mode_mask(ctx) {
                        env.chr_set_yaw(w, yaw);
                    }
                    drift(wk, ctx, env);
                } else if wk.chr_bound() {
                    let dir = env.chr_dir(w);
                    wk.set_pf(p::DIR_YAW, dir);
                    let yaw = math::wrap(env.chr_yaw(w));
                    wk.set_pf(p::YAW, yaw);
                }
                if player != -1 {
                    self.touch_character(w, env);
                }
            }
            2 if player != -1 => self.test_line(w, env),
            3 if player != -1 => self.test_box(w, env),
            6 => self.test_zone(w, env),
            _ => {}
        }
    }

    fn other_pos(&self, id: i16) -> [f32; 3] {
        self.list.get(id as usize).map_or([0.0; 3], Worker::position)
    }

    /// The move record's advance (the speed this frame, a timed or path position).
    fn move_step(&mut self, w: usize, ctx: u32, env: &mut dyn Env) {
        let wk = &self.list[w];
        let m = wk.u32(ctx + CTX_MOVE);
        let mode = wk.u16(m + mv::MODE);
        if mode & 0x8000 != 0 && mode & 0x2000 != 0 {
            let id = wk.i16(m + mv::ID);
            let to = self.other_pos(id);
            let wk = &mut self.list[w];
            let c = wk.u32(ctx + CTX_MOVE);
            wk.set_i16(c + mv::ID, id);
            wk.move_set_target(to[0], to[1], to[2]);
        }
        let pos_track = |wk: &Worker| wk.position();
        let wk = &mut self.list[w];
        let ease = wk.i16(m + mv::EASE);
        let md = mode & 0x7f;
        let mut f = 1.0f32;
        if 0 < ease || md == 9 || md == 7 {
            let count = wk.i16(m + mv::COUNT);
            if count < 0x7ffe {
                wk.set_i16(m + mv::COUNT, count + 1);
            }
            let count = wk.i16(m + mv::COUNT);
            if ease < count {
                f = (f64::from(count) / f64::from(ease)) as f32;
                if f > 8.0 {
                    f = 8.0;
                }
            }
        }
        let pos = pos_track(wk);
        let tgt = [wk.f32(m + mv::TARGET), wk.f32(m + mv::TARGET + 4), wk.f32(m + mv::TARGET + 8)];
        match md {
            1 | 2 => {
                if md == 1 {
                    wk.set_f32(m + mv::TARGET + 4, pos[1]);
                }
                let tgt = [wk.f32(m + mv::TARGET), wk.f32(m + mv::TARGET + 4), wk.f32(m + mv::TARGET + 8)];
                if wk.u16(m + mv::MODE) & 0x8000 != 0 {
                    let pitch = if pos[0] == tgt[0] && pos[2] == tgt[2] {
                        if pos[1] <= tgt[1] { PI } else { -PI }
                    } else {
                        let (yaw_to, pitch_to) = math::angles_to(pos, tgt);
                        let (mut sy, mut sp) = (wk.f32(m + mv::TURN_YAW), wk.f32(m + mv::TURN_PITCH));
                        if wk.u16(m + mv::MODE) & 0x200 != 0 {
                            sy = TAU;
                            sp = TAU;
                        }
                        let yaw = math::turn_toward(wk.pf(p::DIR_YAW), yaw_to, sy * f);
                        wk.set_pf(p::DIR_YAW, yaw);
                        math::turn_toward(wk.pf(p::DIR_PITCH), pitch_to, sp * f)
                    };
                    wk.set_pf(p::DIR_PITCH, pitch);
                }
                let speed = wk.f32(m + mv::SPEED);
                let dist = math::sqrt(math::len2(tgt[0] - pos[0], tgt[1] - pos[1], tgt[2] - pos[2]));
                let scale = if matches!(wk.bind(), 4 | 5) { 1.0f32 } else { 10.0 };
                if d(dist) <= d(speed) / d(scale) {
                    wk.set_u16(m + mv::MODE, wk.u16(m + mv::MODE) | 0x1000);
                    wk.set_pf(p::SPEED, scale * dist);
                } else {
                    wk.set_pf(p::SPEED, speed);
                }
            }
            3 | 4 => {
                if md == 3 {
                    wk.set_f32(m + mv::TARGET + 4, pos[1]);
                }
                let tgt = [wk.f32(m + mv::TARGET), wk.f32(m + mv::TARGET + 4), wk.f32(m + mv::TARGET + 8)];
                let dist = math::sqrt(math::len2(tgt[0] - pos[0], tgt[1] - pos[1], tgt[2] - pos[2]));
                let acc = wk.f32(m + mv::F34);
                let cur = wk.pf(p::SPEED);
                let max = wk.f32(m + mv::SPEED);
                let lim = math::sqrt((acc + acc) * dist);
                let v = cur + acc;
                let clamp = |s: f32| if max < s { max } else { s };
                let floor = |s: f32| if 0.001 <= s { clamp(s) } else { 0.001 };
                let s = if max <= lim {
                    floor(v)
                } else if 0.001 <= v {
                    floor(if v <= lim { v } else { lim })
                } else {
                    clamp(0.001)
                };
                wk.set_pf(p::SPEED, s);
                if d(dist) <= d(s) / 10.0 {
                    wk.set_u16(m + mv::MODE, wk.u16(m + mv::MODE) | 0x1000);
                }
            }
            5 | 6 => {
                let t = wk.f32(m + mv::T) / wk.f32(m + mv::TOTAL);
                wk.set_f32(m + mv::T, wk.f32(m + mv::T) + 1.0);
                let t = math::unit(t);
                let table = wk.u32(m + mv::F14);
                if table == 0 {
                    wk.set_f32(m + mv::T, wk.f32(m + mv::TOTAL) + 1.0);
                    wk.set_pf(p::SPEED, 0.0);
                    return;
                }
                let at = env.path_point(table, md == 6, t);
                let (yaw, pitch) = math::angles_to(pos, at);
                wk.set_pf(p::DIR_YAW, yaw);
                wk.set_pf(p::DIR_PITCH, pitch);
                let dist = math::sqrt(math::len2(at[0] - pos[0], at[1] - pos[1], at[2] - pos[2]));
                wk.set_pf(p::SPEED, dist);
            }
            7 => {
                let (tt, total) = (wk.f32(m + mv::T), wk.f32(m + mv::TOTAL));
                let mut t = tt / total;
                if tt == 0.0 && total == 0.0 {
                    t = 1.0;
                }
                wk.set_f32(m + mv::T, tt + 1.0);
                let t = math::unit(t);
                let u = 1.0 - t;
                let s = [wk.f32(m + mv::START), wk.f32(m + mv::START + 4), wk.f32(m + mv::START + 8)];
                wk.set_pf(p::X, (d(tgt[0]) * d(t) + d(u) * d(s[0])) as f32);
                wk.set_pf(p::Y, (d(tgt[1]) * d(t) + d(s[1]) * d(u)) as f32);
                wk.set_pf(p::Z, (d(u) * d(s[2]) + d(tgt[2]) * d(t)) as f32);
                wk.set_pf(p::SPEED, 0.0);
            }
            9 => {
                let count = wk.i16(m + mv::COUNT);
                let t = if i32::from(count) < i32::from(wk.u16(MODE9_FRAMES)) {
                    0.0
                } else {
                    let tt = wk.f32(m + mv::T) + 1.0;
                    wk.set_f32(m + mv::T, tt);
                    let total = wk.f32(m + mv::TOTAL);
                    let t = tt / total;
                    if total <= tt {
                        wk.set_u16(m + mv::MODE, wk.u16(m + mv::MODE) | 0x1000);
                        1.0
                    } else {
                        t
                    }
                };
                let s = [wk.f32(m + mv::START), wk.f32(m + mv::START + 4), wk.f32(m + mv::START + 8)];
                let z = (d(s[2]) * d(1.0 - t) + d(tgt[2]) * d(t)) as f32;
                let rise = wk.f32(m + mv::F34);
                let y = ((d(rise) * 0.5 * d(t) + d(wk.f32(m + mv::F18))) * d(t) + d(s[1])) as f32;
                let x = (d(t) * d(tgt[0]) + d(s[0]) * d(1.0 - t)) as f32;
                wk.set_pf(p::X, x);
                wk.set_pf(p::Y, y);
                wk.set_pf(p::Z, z);
                wk.set_pf(p::SPEED, 0.0);
            }
            _ => wk.set_pf(p::SPEED, 0.0),
        }
    }

    /// The turn record's advance: the facing (yaw, pitch, roll) toward its target, eased or timed.
    fn rot_step(&mut self, w: usize, ctx: u32) {
        let wk = &self.list[w];
        let r = wk.u32(ctx + CTX_ROT);
        let m = wk.u32(ctx + CTX_MOVE);
        let mode = wk.u16(r + rt::MODE);
        let ease = wk.i16(r + rt::EASE);
        let mut fe = 1.0f32;
        // A turn following another worker faces where that worker stands.
        let tracked = (mode & 0xa000 == 0xa000).then(|| {
            let id = wk.i16(r + rt::ID);
            (id, self.other_pos(id))
        });
        let wk = &mut self.list[w];
        if 0 < ease {
            let c = wk.i16(r + rt::COUNT);
            if c < 0x7ffe {
                wk.set_i16(r + rt::COUNT, c + 1);
            }
            let c = wk.i16(r + rt::COUNT);
            if ease < c {
                fe = (f64::from(c) / f64::from(ease)) as f32;
                if fe > 8.0 {
                    fe = 8.0;
                }
            }
        }
        let speed = if mode & 0x200 == 0 { wk.f32(r + rt::SPEED) } else { wk.f32(r + rt::SPEED2) };
        let mut shape = 1.0f32;
        let target_of =
            |wk: &Worker| [wk.f32(m + mv::TARGET), wk.f32(m + mv::TARGET + 4), wk.f32(m + mv::TARGET + 8)];
        match mode & 0x1f {
            1 | 5 => {}
            2 => {
                let mut t = wk.f32(r + rt::TOTAL) + wk.f32(r + rt::T);
                if d(t) < 0.0 {
                    t = 0.0;
                } else if 1.0 <= t {
                    if wk.pf(p::YAW) == wk.f32(r + rt::TARGET) {
                        wk.set_u16(r + rt::MODE, mode & 0xffe1 | 1);
                    }
                    t = 1.0;
                }
                wk.set_f32(r + rt::T, t);
                shape = ((1.0 - (d(t) - 0.5) * 4.0 * (d(t) - 0.5)) * 0.6 + 0.4) as f32;
                if d(shape) < 0.0 {
                    shape = 0.0;
                } else if shape > 1.0 {
                    shape = 1.0;
                }
            }
            k @ (3 | 4) => {
                let t0 = wk.f32(r + rt::T);
                wk.set_f32(r + rt::T, t0 + 1.0);
                let total = wk.f32(r + rt::TOTAL);
                let mut t = if k == 3 && total == 0.0 { 0.0 } else { (d(t0 + 1.0) / d(total)) as f32 };
                t = math::unit(t);
                if mode & 0x8000 != 0 {
                    if let Some((id, to)) = tracked {
                        track_apply(wk, id, to);
                    }
                    let (a, b) = math::angles_to(wk.position(), target_of(wk));
                    let dy = math::wrap((wk.f32(r + rt::DELTA) + a) - wk.f32(r + rt::TARGET));
                    wk.set_f32(r + rt::DELTA, dy);
                    let dp = math::wrap((wk.f32(r + rt::DELTA + 4) + b) - wk.f32(r + rt::TARGET + 4));
                    wk.set_f32(r + rt::DELTA + 4, dp);
                }
                let e = if k == 3 {
                    t
                } else if t < 0.5 {
                    (t + t) * t
                } else {
                    let u = 1.0 - t;
                    1.0 - (u + u) * u
                };
                let done = if k == 3 { !(t < 1.0) } else { !(e < 1.0) };
                if done {
                    wk.set_pf(p::YAW, wk.f32(r + rt::TARGET));
                    wk.set_pf(p::PITCH, wk.f32(r + rt::TARGET + 4));
                    wk.set_pf(p::ROLL, wk.f32(r + rt::TARGET + 8));
                } else if (k == 3 && t == 0.0) || (k == 4 && e <= 0.0) {
                    for (o, f) in [(0u32, p::YAW), (4, p::PITCH), (8, p::ROLL)] {
                        let v = math::wrap(wk.f32(r + rt::DELTA + o) + wk.f32(r + rt::TARGET + o));
                        wk.set_pf(f, v);
                    }
                } else {
                    let mut adj = [0.0f32; 3];
                    for (i, o) in [0u32, 4, 8].into_iter().enumerate() {
                        let delta = wk.f32(r + rt::DELTA + o);
                        if mode & 0x800 != 0 && 0.0 < delta {
                            adj[i] = -TAU;
                        }
                        if mode & 0x400 != 0 && delta < 0.0 {
                            adj[i] = TAU;
                        }
                    }
                    let u = 1.0 - e;
                    let yaw = math::wrap(
                        (d(wk.f32(r + rt::TARGET)) + d(u) * (d(wk.f32(r + rt::DELTA)) + d(adj[0]))) as f32,
                    );
                    wk.set_pf(p::YAW, yaw);
                    let pitch = math::wrap(
                        ((d(wk.f32(r + rt::DELTA + 4)) + d(adj[1])) * d(u) + d(wk.f32(r + rt::TARGET + 4)))
                            as f32,
                    );
                    wk.set_pf(p::PITCH, pitch);
                    let roll = math::wrap(
                        ((d(wk.f32(r + rt::DELTA + 8)) + d(adj[2])) * d(u) + d(wk.f32(r + rt::TARGET + 8)))
                            as f32,
                    );
                    wk.set_pf(p::ROLL, roll);
                }
                if mode & 0x1000 != 0 {
                    wk.set_pf(p::DIR_YAW, wk.pf(p::YAW));
                    wk.set_pf(p::DIR_PITCH, wk.pf(p::PITCH));
                }
                return;
            }
            _ => {
                if mode & 0x1000 != 0 {
                    wk.set_pf(p::DIR_YAW, wk.pf(p::YAW));
                    wk.set_pf(p::DIR_PITCH, wk.pf(p::PITCH));
                }
                return;
            }
        }
        // Modes 1, 2 and 5: turn at the speed (eased by the shape and the ease factor), or spin by it.
        if mode & 0x8000 == 0 {
            let yaw = math::wrap(wk.f32(r + rt::SPEED) + wk.pf(p::YAW));
            wk.set_pf(p::YAW, yaw);
            let pitch = math::wrap(wk.f32(r + rt::PITCH_SPEED) + wk.pf(p::PITCH));
            wk.set_pf(p::PITCH, pitch);
            let roll = math::wrap(wk.f32(r + rt::ROLL_SPEED) + wk.pf(p::ROLL));
            wk.set_pf(p::ROLL, roll);
        } else {
            let way = if mode & 0x400 != 0 { 2 } else { u8::from(mode & 0x800 != 0) };
            if let Some((id, to)) = tracked {
                track_apply(wk, id, to);
            }
            let (a, b) = math::angles_to(wk.position(), target_of(wk));
            let dy = math::wrap((wk.f32(r + rt::DELTA) + a) - wk.f32(r + rt::TARGET));
            wk.set_f32(r + rt::DELTA, dy);
            let dp = math::wrap((wk.f32(r + rt::DELTA + 4) + b) - wk.f32(r + rt::TARGET + 4));
            wk.set_f32(r + rt::DELTA + 4, dp);
            wk.set_f32(r + rt::TARGET, a);
            wk.set_f32(r + rt::TARGET + 4, b);
            let step = (d(speed) * d(shape) * d(fe)) as f32;
            let yaw = math::turn_by(wk.pf(p::YAW), a, step, way);
            wk.set_pf(p::YAW, yaw);
            let ps = (d(wk.f32(r + rt::PITCH_SPEED)) * d(shape) * d(fe)) as f32;
            let pitch = math::turn_by(wk.pf(p::PITCH), wk.f32(r + rt::TARGET + 4), ps, way);
            wk.set_pf(p::PITCH, pitch);
        }
        if mode & 0x1000 != 0 {
            wk.set_pf(p::DIR_YAW, wk.pf(p::YAW));
            wk.set_pf(p::DIR_PITCH, wk.pf(p::PITCH));
        }
    }

    /// A character worker against the player: touching (within the touch radius and the height range) raises
    /// signal 3; the talk target is the nearest worker the player faces within talk reach.
    fn touch_character(&mut self, w: usize, env: &mut dyn Env) {
        let sys = self.sys.clone();
        let player_yaw = self.list.get(sys.player as usize).map_or(0.0, |p| p.pf(p::YAW));
        let wk = &mut self.list[w];
        if wk.bind() != 1 {
            return;
        }
        let pos = wk.position();
        let [px, py, pz, _] = sys.player_pos;
        let height = wk.pf(p::HEIGHT);
        wk.set_u16(FLAGS2, wk.flags2() & 0xfffe);
        let mut touching = false;
        if (py - pos[1]).abs() < height {
            let dist = math::len2(px - pos[0], 0.0, pz - pos[2]);
            let r = wk.pf(p::RADIUS);
            touching = dist <= r * r;
            if touching {
                env.signal(w, 3);
            }
        }
        let wk = &mut self.list[w];
        wk.set_u16(FLAGS, wk.flags() & !0x40 | u16::from(touching) << 6);
        if wk.u8(SIGNAL_MASK) & 1 == 0 || sys.talk_mode != 1 {
            return;
        }
        let talk_h = wk.pf(p::TALK_HEIGHT);
        let dy = if d(talk_h) >= 0.0 { 0.0 } else { py - pos[1] };
        let dist = math::len2(px - pos[0], dy, pz - pos[2]);
        let reach = sys.talk_reach + wk.pf(p::TALK_RADIUS);
        let reach = reach * reach;
        if reach <= dist || wk.flags() & 0x20 != 0 || wk.flags() & 0x200 != 0 {
            return;
        }
        let ok = if d(talk_h) >= 0.0 {
            wk.u8(FLAGS2) & 8 == 0 && (py - pos[1]).abs() < talk_h
        } else {
            wk.u8(FLAGS2) & 8 == 0
        };
        if ok
            && (sys.talk_target == -1 || dist < sys.talk_dist)
            && math::faces(player_yaw, wk.pf(p::TALK_ANGLE), px, pz, pos[0], pos[2])
        {
            self.sys.talk_dist = dist;
            self.sys.talk_target = wk.id() as i16;
        }
    }

    /// A line (kind 2): crossing it raises signal 4, coming within its zone 5 (entering), 3 (inside), 6 (leaving).
    fn test_line(&mut self, w: usize, env: &mut dyn Env) {
        let sys = self.sys.clone();
        let player_yaw = self.list.get(sys.player as usize).map_or(0.0, |p| p.pf(p::YAW));
        let wk = &mut self.list[w];
        let a = wk.position();
        let b = [wk.pf(p::PITCH), wk.pf(p::YAW), wk.pf(p::ROLL)];
        let prev = [sys.player_prev[0], sys.player_prev[1], sys.player_prev[2]];
        let cur = [sys.player_pos[0], sys.player_pos[1], sys.player_pos[2]];
        let height = wk.pf(p::HEIGHT);
        let mut crossed = false;
        if cur[0] != prev[0] || cur[2] != prev[2] || wk.flags2() & 1 != 0 {
            wk.set_u16(FLAGS2, wk.flags2() & 0xfffe);
            let side = math::crossing(prev, cur, a, b);
            if side != 0 {
                let gap = math::crossing_height(prev, cur, a, b);
                if height > 0.0 && gap > height {
                    crossed = false;
                } else {
                    let f2 = wk.flags2();
                    wk.set_u16(FLAGS2, if side < 1 { f2 & 0xfff9 } else { f2 & 0xfffb | 2 });
                    crossed = true;
                    env.signal(w, 4);
                }
            }
        }
        let wk = &mut self.list[w];
        let t = segment_param(cur, a, b);
        let near = [
            (d(t) * d(b[0] - a[0]) + d(a[0])) as f32,
            (d(a[1]) + d(t) * d(b[1] - a[1])) as f32,
            (d(a[2]) + d(b[2] - a[2]) * d(t)) as f32,
        ];
        let dist = math::len2(near[0] - cur[0], 0.0, near[2] - cur[2]);
        let r = wk.pf(p::RADIUS);
        let flags = wk.flags();
        let out_of_height = height > 0.0 && (near[1] + height < cur[1] || cur[1] < near[1] - height);
        if r * r < dist || out_of_height {
            wk.set_u16(FLAGS, flags & 0xffbf);
            if crossed {
                env.signal(w, 5);
            }
            if (flags >> 6) & 1 == 1 || crossed {
                env.signal(w, 6);
            }
        } else {
            wk.set_u16(FLAGS, flags | 0x40);
            env.signal(w, 3);
            if (flags >> 6) & 1 == 0 {
                env.signal(w, 5);
            }
        }
        let wk = &self.list[w];
        if wk.u8(SIGNAL_MASK) & 1 != 0 && sys.talk_mode == 1 {
            let reach = wk.pf(p::TALK_RADIUS) + sys.talk_reach;
            let reach = reach * reach;
            let in_height = !(height > 0.0) || (cur[1] <= near[1] + height && near[1] - height <= cur[1]);
            if dist <= reach
                && in_height
                && (sys.talk_target == -1 || reach < sys.talk_dist)
                && math::faces(player_yaw, wk.pf(p::TALK_ANGLE), cur[0], cur[2], near[0], near[2])
            {
                self.sys.talk_dist = reach;
                self.sys.talk_target = wk.id() as i16;
            }
        }
    }

    /// A box (kind 3) around the worker, turned by its facing: half extents x (+0x30) and z (+0x38), height
    /// (+0x1c). Crossing into it raises 4, inside 3, entering 5, leaving 6.
    fn test_box(&mut self, w: usize, env: &mut dyn Env) {
        let sys = self.sys.clone();
        let player_yaw = self.list.get(sys.player as usize).map_or(0.0, |p| p.pf(p::YAW));
        let (prev, cur) = (sys.player_prev, sys.player_pos);
        let (inside, near, crossed, out_flags) =
            self.box_flags(w, [prev[0], prev[1], prev[2]], [cur[0], cur[1], cur[2]]);
        let wk = &mut self.list[w];
        if crossed && wk.u8(FLAGS + 1) & 1 == 0 {
            env.signal(w, 4);
        }
        let wk = &mut self.list[w];
        let was = (wk.flags() >> 6) & 1;
        let hit = inside;
        wk.set_u16(FLAGS, wk.flags() & !0x40 | u16::from(hit) << 6);
        if hit {
            env.signal(w, 3);
            if was == 0 {
                env.signal(w, 5);
            }
        } else if (crossed && was == 0) || was == 1 {
            if crossed && was == 0 {
                env.signal(w, 5);
            }
            env.signal(w, 6);
        }
        let wk = &self.list[w];
        if wk.u8(SIGNAL_MASK) & 1 != 0 && sys.talk_mode == 1 {
            let reach = wk.pf(p::TALK_RADIUS) + sys.talk_reach;
            let reach = reach * reach;
            let height = wk.pf(p::HEIGHT);
            let (lx, lz, dy) = near;
            if out_flags != 0 {
                if 0.0 < height && !(dy <= height && -height <= dy) {
                    return;
                }
                let gap = math::len2(lx.0 - lx.1, 0.0, lz.0 - lz.1);
                if reach < gap {
                    return;
                }
            }
            let pos = wk.position();
            if (sys.talk_target == -1 || reach < sys.talk_dist)
                && math::faces(player_yaw, wk.pf(p::TALK_ANGLE), cur[0], cur[2], pos[0], pos[2])
            {
                self.sys.talk_dist = reach;
                self.sys.talk_target = wk.id() as i16;
            }
        }
    }

    /// The box test's outcode work: the player's previous and current points in the box's frame; returns whether
    /// the current point is inside (or within the touch radius of the box), the clamped point (x, z pairs, the
    /// height difference), whether the path crossed into it, and the current outcode.
    #[allow(clippy::type_complexity)]
    fn box_flags(
        &mut self,
        w: usize,
        prev: [f32; 3],
        cur: [f32; 3],
    ) -> (bool, ((f32, f32), (f32, f32), f32), bool, u8) {
        let wk = &mut self.list[w];
        let pos = wk.position();
        let (hz, hx, height) = (wk.pf(p::ROLL), wk.pf(p::PITCH), wk.pf(p::HEIGHT));
        let yaw = wk.pf(p::YAW);
        let (c, s) = (d(yaw).cos() as f32, d(yaw).sin() as f32);
        let (ax, ay, az) = (prev[0] - pos[0], prev[1] - pos[1], prev[2] - pos[2]);
        let (bx, by, bz) = (cur[0] - pos[0], cur[1] - pos[1], cur[2] - pos[2]);
        let az_r = (d(ax) * d(s) + d(az) * d(c)) as f32;
        let ax_r = (d(c) * d(ax) - d(s) * d(az)) as f32;
        let bz_r = (d(bx) * d(s) + d(bz) * d(c)) as f32;
        wk.set_u16(FLAGS2, wk.flags2() & 0xfffe);
        let bx_r = (d(bx) * d(c) - d(bz) * d(s)) as f32;
        let code = |x: f32, z: f32, y: f32| {
            let mut f = 0u8;
            if hx <= x {
                f |= 1;
            }
            if x <= -hx {
                f |= 2;
            }
            if hz <= z {
                f |= 4;
            }
            if z <= -hz {
                f |= 8;
            }
            if 0.0 < height {
                if height <= y {
                    f |= 0x10;
                }
                if y <= -height {
                    f |= 0x20;
                }
            }
            f
        };
        let fa = code(ax_r, az_r, ay);
        let fb = code(bx_r, bz_r, by);
        let mut cx = if hx <= bx_r { hx } else { bx_r };
        if bx_r <= -hx {
            cx = -hx;
        }
        let mut cz = if bz_r < hz { bz_r } else { hz };
        if bz_r <= -hz {
            cz = -hz;
        }
        let crossed = fa & fb == 0 && (fb != 0 || fa != 0);
        let inside = if fb == 0 {
            wk.set_u16(FLAGS, wk.flags() | 0x100);
            true
        } else {
            wk.set_u16(FLAGS, wk.flags() & 0xfeff);
            // The height test takes the previous point's height.
            let in_height = !(0.0 < height) || (ay <= height && -height <= ay);
            in_height && {
                let r = wk.pf(p::RADIUS);
                !(r * r < math::len2(bx_r - cx, 0.0, bz_r - cz))
            }
        };
        (inside, ((bx_r, cx), (bz_r, cz), ay), crossed, fb)
    }

    /// A zone (kind 6): a box like kind 3's against the player (target −1) or another worker, raising enter (5)
    /// and leave (6) once each.
    fn test_zone(&mut self, w: usize, env: &mut dyn Env) {
        let wk = &self.list[w];
        let target = wk.i16(wk.pb() + p::TARGET);
        let (prev, cur) = if target == -1 {
            let s = &self.sys;
            (
                [s.player_prev[0], s.player_prev[1], s.player_prev[2]],
                [s.player_pos[0], s.player_pos[1], s.player_pos[2]],
            )
        } else {
            let o = self.list.get(target as usize);
            let prev = o.map_or([0.0; 3], |o| [o.pf(p::PREV), o.pf(p::PREV + 4), o.pf(p::PREV + 8)]);
            (prev, o.map_or([0.0; 3], Worker::position))
        };
        let (inside, _, crossed, _) = self.box_flags(w, prev, cur);
        let wk = &mut self.list[w];
        let was = (wk.flags() >> 6) & 1;
        wk.set_u16(FLAGS, wk.flags() & !0x40 | u16::from(inside) << 6);
        if inside {
            if was == 0 {
                env.signal_once(w, 5);
            }
        } else if (crossed && was == 0) || was == 1 {
            if crossed && was == 0 {
                env.signal_once(w, 5);
            }
            env.signal_once(w, 6);
        }
    }
}

fn track_apply(wk: &mut Worker, id: i16, to: [f32; 3]) {
    let c = wk.rot_of(i32::from(wk.level()));
    wk.set_i16(c + rt::ID, id);
    wk.rot_set_target_point(to[0], to[1], to[2]);
}

/// Where along the line a→b the point lies (its projection's parameter, clamped by the original's VU code to the
/// segment).
fn segment_param(pt: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
    let ab = [d(b[0]) - d(a[0]), d(b[1]) - d(a[1]), d(b[2]) - d(a[2])];
    let ap = [d(pt[0]) - d(a[0]), d(pt[1]) - d(a[1]), d(pt[2]) - d(a[2])];
    let len2 = (ab[0] * ab[0] + ab[1] * ab[1] + ab[2] * ab[2]) as f32;
    if len2 == 0.0 {
        return 0.0;
    }
    ((ap[0] * ab[0] + ap[1] * ab[1] + ap[2] * ab[2]) as f32 / len2).clamp(0.0, 1.0)
}

/// The move's effect on the bound body (its speed and rise this frame) and the end of the move: arriving within
/// the radius, the done flag, or a timed move's end.
fn apply_move(wk: &mut Worker, w: usize, ctx: u32, env: &mut dyn Env) {
    let m = wk.u32(ctx + CTX_MOVE);
    let mut speed = if wk.u16(m + mv::MODE) != 0 { wk.pf(p::SPEED) } else { 0.0 };
    if wk.chr_bound() {
        let pitch = wk.pf(p::DIR_PITCH);
        let (s, c) = (d(pitch).sin() as f32, d(pitch).cos() as f32);
        match env.chr_mode(w) {
            0 => match wk.u16(m + mv::MODE) & 0x7f {
                2 | 4 => {
                    env.chr_set_rise(w, s * speed);
                    speed *= c;
                }
                5 | 6 => {
                    let flat = c * speed;
                    env.chr_set_rise(w, (d(s) * d(flat) * 10.0) as f32);
                    speed = (d(flat) * 10.0) as f32;
                }
                7 | 9 => {
                    speed = 0.0;
                    env.chr_set_pos(w, wk.position());
                }
                _ => {}
            },
            2 => {
                env.chr_set_rise(w, s * speed);
                speed *= c;
            }
            _ => {}
        }
        env.chr_set_speed(w, speed);
    }
    let mode = wk.u16(m + mv::MODE);
    match mode & 0x7f {
        0 => {
            if wk.bind() == 1 {
                if wk.u32(super::CHR) != 0 && mode & 0x800 != 0 {
                    let t = [wk.f32(m + mv::TARGET), wk.f32(m + mv::TARGET + 4), wk.f32(m + mv::TARGET + 8)];
                    env.chr_set_pos(w, t);
                    wk.set_u16(m + mv::MODE, wk.u16(m + mv::MODE) & 0xf7ff);
                }
            } else if matches!(wk.bind(), 4 | 5) {
                bind_update(wk, env);
            }
        }
        5 | 6 => {
            if wk.f32(m + mv::TOTAL) < wk.f32(m + mv::T) {
                wk.move_reset(-1);
                wk.set_u16(m + mv::MODE, 0);
                wk.set_u32(m + mv::F14, 0);
            }
        }
        7 | 9 => {
            if wk.f32(m + mv::TOTAL) < wk.f32(m + mv::T) {
                wk.move_reset(-1);
                wk.set_u16(m + mv::MODE, 0);
                let t = [wk.f32(m + mv::TARGET), wk.f32(m + mv::TARGET + 4), wk.f32(m + mv::TARGET + 8)];
                wk.set_position(t);
            }
        }
        _ => {
            let pos = wk.position();
            let t = [wk.f32(m + mv::TARGET), wk.f32(m + mv::TARGET + 4), wk.f32(m + mv::TARGET + 8)];
            let r = wk.f32(m + mv::RADIUS);
            let (dx, dz) = (t[0] - pos[0], t[2] - pos[2]);
            let dy = if mode & 0x400 == 0 { 0.0 } else { t[1] - pos[1] };
            let arrived = d(dx) * d(dx) + d(dy) * d(dy) + d(dz) * d(dz) <= d(r) * d(r);
            if arrived {
                if wk.u16(m + mv::MODE) & 0x7f != 0 {
                    wk.move_reset(-1);
                }
                wk.set_u16(m + mv::MODE, 0);
            }
            let mode = wk.u16(m + mv::MODE);
            if mode & 0x1000 != 0 && mode & 0x7f != 0 {
                wk.move_reset(-1);
                wk.set_u16(m + mv::MODE, wk.u16(m + mv::MODE) & 0x800);
            }
        }
    }
}

/// A bound object or camera id takes the worker's facing and position.
fn bind_update(wk: &Worker, env: &mut dyn Env) {
    let rot = [wk.pf(p::PITCH), wk.pf(p::YAW), wk.pf(p::ROLL), 0.0];
    let pos = wk.position();
    env.bind_place(wk.bind(), wk.u16(super::BIND_ID), rot, [pos[0], pos[1], pos[2], 1.0]);
}

/// Workers bound to a camera or an object (bind kinds 2–5) drift by their speed along their direction.
fn drift(wk: &mut Worker, ctx: u32, env: &mut dyn Env) {
    let bind = wk.bind();
    if !(2..6).contains(&bind) {
        return;
    }
    let mut speed = wk.pf(p::SPEED);
    if bind == 4 || bind == 5 {
        speed = (d(speed) / 10.0) as f32;
    }
    let v = math::dir_vec(speed, wk.pf(p::DIR_YAW), wk.pf(p::DIR_PITCH));
    let pos = wk.position();
    let np = [pos[0] + v[0], pos[1] + v[1], pos[2] + v[2]];
    wk.set_position(np);
    if bind == 2 {
        if wk.u16(wk.u32(ctx + CTX_MOVE) + mv::MODE) != 0 || wk.u16(wk.u32(ctx + CTX_ROT) + rt::MODE) != 0 {
            env.camera_point(wk.u32(super::CAMERA), [np[0], np[1], np[2], 1.0]);
        }
    } else if bind == 4 || bind == 5 {
        bind_update(wk, env);
    }
}
