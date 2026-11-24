//! Natives on the worker records: moves (target, speed, radius, timing, start, wait), turns (targets, speeds,
//! start, wait), positions and facings, the signal mask, ending moves and turns.

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::{NativeCall, Value};

use crate::workers::{self, Worker, math, mv, p, rt};

use super::FieldState;

/// The calling worker's record, at its running level.
fn wk<'a>(g: &'a mut FieldState, c: &NativeCall) -> &'a mut Worker {
    let w = &mut g.workers.list[c.worker];
    w.set_u8(workers::LEVEL, c.level);
    w
}

/// A worker by index as the original looks it up: out of range is worker 0.
fn by_index(g: &FieldState, i: i32) -> usize {
    let n = g.workers.list.len();
    if i < 0 || i as usize >= n { 0 } else { i as usize }
}

fn pos_of(g: &FieldState, w: usize) -> [f32; 3] {
    g.workers.list.get(w).map_or([0.0; 3], Worker::position)
}

/// The auto-face switch off: a worker bound to a body (+0xaa = 1, +0x9c set) clears the body's flag 0x400 (+0x194).
fn body_flag_400_off(g: &mut FieldState, w: usize) {
    let bound = g.workers.list.get(w).is_some_and(Worker::chr_bound);
    if let Some(ch) = g.chrs.get_mut(w).and_then(Option::as_mut).filter(|_| bound) {
        ch.set_u32(crate::chr::FLAGS, ch.u32(crate::chr::FLAGS) & !0x400);
    }
}

fn pop3(c: &mut NativeCall) -> [f32; 3] {
    let z = c.stack.pop_float();
    let y = c.stack.pop_float();
    let x = c.stack.pop_float();
    [x, y, z]
}

/// Truncation of a float to an integer as the original's conversion does it.
fn ftol(x: f32) -> i32 {
    f64::from(x).trunc() as i64 as i32
}

/// A float set on the running level's own move or turn record; returns it.
fn level_move_f(g: &mut FieldState, c: &mut NativeCall, field: u32) -> Value {
    let v = c.stack.pop_float();
    let w = wk(g, c);
    let m = w.cur_move();
    w.set_f32(m + field, v);
    Value::Float(v)
}

fn level_rot_f(g: &mut FieldState, c: &mut NativeCall, field: u32) -> Value {
    let v = c.stack.pop_float();
    let w = wk(g, c);
    let r = w.cur_rot();
    w.set_f32(r + field, v);
    Value::Float(v)
}

/// A float set on every level's own move or turn record; returns it.
fn all_move_f(g: &mut FieldState, c: &mut NativeCall, field: u32) -> Value {
    let v = c.stack.pop_float();
    let w = wk(g, c);
    for l in 0..9 {
        let m = w.own_move(l);
        w.set_f32(m + field, v);
    }
    Value::Float(v)
}

fn all_rot_f(g: &mut FieldState, c: &mut NativeCall, field: u32) -> Value {
    let v = c.stack.pop_float();
    let w = wk(g, c);
    for l in 0..9 {
        let r = w.own_rot(l);
        w.set_f32(r + field, v);
    }
    Value::Float(v)
}

/// A turn target (yaw, pitch or roll) of the running level's own turn record, and the turn left to it from the
/// current facing.
fn rot_target(g: &mut FieldState, c: &mut NativeCall, k: u32, facing: u32) -> Value {
    let v = math::wrap(c.stack.pop_float());
    let w = wk(g, c);
    let r = w.cur_rot();
    w.set_f32(r + rt::TARGET + k * 4, v);
    let left = math::wrap(w.pf(facing) - v);
    w.set_f32(r + rt::DELTA + k * 4, left);
    Value::Float(v)
}

/// Place a worker (the original's position set): its body goes there too, the player's tracked place moves, and
/// unless placements keep the previous place (engine flag 0x10, `keep_prev` off) the previous place is the new one.
pub(super) fn place_worker(
    g: &mut FieldState,
    image: &mut [u8],
    w: usize,
    pos: [f32; 3],
    keep_prev_off: bool,
) {
    let Some(wk) = g.workers.list.get_mut(w) else { return };
    if wk.pos().is_none() {
        return;
    }
    wk.set_position(pos);
    let bound = wk.chr_bound();
    let bind = wk.bind();
    let cam = wk.u32(workers::CAMERA);
    let keep = wk.flags() & 0x10 == 0 || keep_prev_off;
    if bound {
        // The body's position set (position and previous position, moved flag 0x2000000, the triangle looked up again), then
        // MsSetChrPosReset flag 0x10 unless flag 1 and Ch_ClothReset (no cloth in Aerugo).
        g.actor(w).pos = pos;
        if let Some(ch) = g.chrs.get_mut(w).and_then(Option::as_mut) {
            ch.set_pos(pos);
            let f = ch.u32(crate::chr::FLAGS);
            if f & 1 == 0 {
                ch.set_u32(crate::chr::FLAGS, f | 0x10);
            }
        }
    } else if bind == 2 {
        // cam_set_vec(handle +0x3c, side 1, (pos, 1)).
        g.cam.set_vec(cam, 1, [pos[0], pos[1], pos[2], 1.0]);
    }
    if g.workers.sys.player == w as i16 {
        let s = &mut g.workers.sys;
        s.player_prev = s.player_pos;
        s.player_pos = [pos[0], pos[1], pos[2], 1.0];
        if keep {
            s.player_prev = s.player_pos;
        }
        // The ground flags of the player's body: save +0x10 = +0x828 bits 7–8 (0 without a body), save +0xe =
        // the event's area for bits 11–12.
        let flags = g.workers.list[w]
            .chr_bound()
            .then(|| g.chrs.get(w).and_then(Option::as_ref).map(|ch| ch.u32(0x828)))
            .flatten();
        let (ground, area) = flags.map_or((0, 0), |f| (f >> 7 & 3, f >> 11 & 3));
        if let Some(b) = image.get_mut(0x10) {
            *b = ground as u8;
        }
        let a = g.area_word(area);
        if let Some(b) = image.get_mut(0xe..0x10) {
            b.copy_from_slice(&a.to_le_bytes());
        }
    }
    let wk = &mut g.workers.list[w];
    if keep {
        for k in 0..3 {
            wk.set_pf(p::PREV + k * 4, pos[k as usize]);
        }
    }
    g.workers.sys.flags2 |= 0x10;
    g.player_placed = true;
    g.show_worker(w);
}

pub fn register(n: &mut Natives<FieldState>) {
    // 0012(x, z): place on the ground plane (height 0).
    n.register(
        0x0012,
        Native::result(|g, c| {
            let z = c.stack.pop_float();
            let x = c.stack.pop_float();
            place_worker(g, &mut c.memory.global[0], c.worker, [x, 0.0, z], false);
            Value::Int(0)
        }),
    );
    n.register(
        0x0013,
        Native::result(|g, c| {
            let pos = pop3(c);
            place_worker(g, &mut c.memory.global[0], c.worker, pos, false);
            Value::Int(0)
        }),
    );
    n.register(
        0x0015,
        Native::result(|g, c| {
            let [x, y, z] = pop3(c);
            wk(g, c).move_set_target(x, y, z);
            Value::Int(0)
        }),
    );
    n.register(0x0016, Native::result(|g, c| level_move_f(g, c, mv::SPEED)));
    n.register(0x0017, Native::result(|g, c| level_move_f(g, c, mv::RADIUS)));
    n.register(0x002D, Native::result(|g, c| all_move_f(g, c, mv::RADIUS)));
    n.register(0x0094, Native::result(|g, c| level_move_f(g, c, mv::F34)));
    n.register(
        0x00B1,
        Native::result(|g, c| {
            level_move_f(g, c, mv::F18);
            Value::Float(0.0)
        }),
    );
    // 0018(mask, mode, id): start the running level's move (the body's auto-face switch off first).
    n.register(
        0x0018,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            let mut mode = c.stack.pop_int() as u32;
            let mask = c.stack.pop_int() as u32;
            body_flag_400_off(g, c.worker);
            let w = wk(g, c);
            let m = w.cur_move();
            match mode & 0xffff_007f {
                5 | 6 => w.set_f32(m + mv::T, 0.0),
                7 => {
                    let pos = w.position();
                    for k in 0..3 {
                        w.set_f32(m + mv::START + k * 4, pos[k as usize]);
                    }
                    w.set_f32(m + mv::F18, 0.0);
                    w.set_f32(m + mv::T, 0.0);
                }
                9 => {
                    let rise = w.f32(m + mv::TARGET + 4) - w.f32(m + mv::START + 4);
                    let up = if rise <= 0.0 { w.f32(m + mv::F18) } else { -w.f32(m + mv::F18) };
                    let a = (rise - up) + (rise - up);
                    if 0.0 <= a {
                        w.set_f32(m + mv::F34, a);
                        w.set_f32(m + mv::F18, up);
                    } else {
                        w.set_f32(m + mv::F34, -a);
                        w.set_f32(m + mv::F18, (f64::from(rise) - f64::from(-a) * 0.5) as f32);
                    }
                    w.set_f32(m + mv::T, 0.0);
                }
                _ => {
                    let total = w.f32(m + mv::TOTAL);
                    if total != 0.0 {
                        w.set_f32(m + mv::TOTAL, 1.0 / total);
                    }
                }
            }
            if w.f32(m + mv::RADIUS) == 0.0 {
                mode |= 0x800;
            }
            w.move_start(mask, mode, id as u16);
            Value::Int(0)
        }),
    );
    // 0019(mask, mode, id): start the running level's turn (the body's auto-face switch off first).
    n.register(
        0x0019,
        Native::result(|g, c| {
            let id = c.stack.pop_int();
            let mode = c.stack.pop_int() as u32;
            let mask = c.stack.pop_int() as u32;
            body_flag_400_off(g, c.worker);
            let w = wk(g, c);
            let r = w.rot_of(i32::from(c.level));
            let m = mode & 0xffff_001f;
            if m == 2 {
                const TAU: f32 = 2.0 * std::f32::consts::PI;
                let mut n = ftol(w.f32(r + rt::DELTA));
                if mode & 0x800 != 0 && 0.0 < n as f32 {
                    n = ftol(n as f32 - TAU);
                }
                if mode & 0x400 != 0 && (n as f32) < 0.0 {
                    n = ftol(n as f32 + TAU);
                }
                w.set_f32(r + rt::T, 0.0);
                let x = n as f32;
                let total = if x == 0.0 {
                    0.0
                } else {
                    ((f64::from(w.f32(r + rt::SPEED)) * f64::from(5.0f32 / 6.0)) / f64::from(x)).abs() as f32
                };
                w.set_f32(r + rt::TOTAL, total);
            } else if !(2 < m && m < 5) && w.f32(r + rt::TOTAL) != 0.0 {
                let t = w.f32(r + rt::TOTAL);
                w.set_f32(r + rt::TOTAL, 1.0 / t);
            }
            w.rot_start(mask, mode, id as u16);
            Value::Int(0)
        }),
    );
    // 001a / 001b: wait for the running level's move / turn.
    n.register(
        0x001A,
        Native {
            init: None,
            wait: Some(|g, c| {
                let w = wk(g, c);
                if w.u16(w.cur_move() + mv::MODE) & 0x7f != 0 { 0 } else { 5 }
            }),
            result: None,
        },
    );
    n.register(
        0x001B,
        Native {
            init: None,
            wait: Some(|g, c| {
                let w = wk(g, c);
                let r = w.rot_of(i32::from(c.level));
                if w.u16(r + rt::MODE) & 0x1f != 0 { 0 } else { 5 }
            }),
            result: None,
        },
    );
    // 001c(count, frames): a timed move's frames and its count.
    n.register(
        0x001C,
        Native::result(|g, c| {
            let frames = c.stack.pop_float();
            let count = c.stack.pop_float();
            let w = wk(g, c);
            let m = w.cur_move();
            w.set_f32(m + mv::TOTAL, frames);
            w.set_f32(m + mv::T, count);
            Value::Float(0.0)
        }),
    );
    // 001d(count, frames): a timed turn's (frames below 1: 1).
    n.register(
        0x001D,
        Native::result(|g, c| {
            let mut frames = c.stack.pop_float();
            let count = c.stack.pop_float();
            if frames <= 0.0 {
                frames = 1.0;
            }
            let w = wk(g, c);
            let r = w.rot_of(i32::from(c.level));
            w.set_f32(r + rt::TOTAL, frames);
            w.set_f32(r + rt::T, count);
            Value::Float(0.0)
        }),
    );
    // 001f / 0020: the heading and the pitch toward the running level's move target.
    n.register(
        0x001F,
        Native::result(|g, c| {
            let w = wk(g, c);
            let m = w.cur_move();
            let pos = w.position();
            Value::Float(math::heading(pos[0], pos[2], w.f32(m + mv::TARGET), w.f32(m + mv::TARGET + 8)))
        }),
    );
    n.register(
        0x0020,
        Native::result(|g, c| {
            let w = wk(g, c);
            let m = w.cur_move();
            let t = [w.f32(m + mv::TARGET), w.f32(m + mv::TARGET + 4), w.f32(m + mv::TARGET + 8)];
            Value::Float(math::angles_to(w.position(), t).1)
        }),
    );
    // 0021 / 0022: the heading and pitch a move starts along.
    n.register(
        0x0021,
        Native::result(|g, c| {
            level_move_wrapped(g, c, mv::F44);
            Value::Float(0.0)
        }),
    );
    n.register(
        0x0022,
        Native::result(|g, c| {
            level_move_wrapped(g, c, mv::F48);
            Value::Float(0.0)
        }),
    );
    // 0025: facing.
    n.register(
        0x0025,
        Native::result(|g, c| {
            let v = math::wrap(c.stack.pop_float());
            wk(g, c).set_pf(p::YAW, v);
            g.show_worker(c.worker);
            Value::Float(0.0)
        }),
    );
    n.register(0x0028, Native::result(|g, c| rot_target(g, c, 0, p::YAW)));
    n.register(0x0029, Native::result(|g, c| rot_target(g, c, 1, p::PITCH)));
    n.register(0x002A, Native::result(|g, c| rot_target(g, c, 2, p::ROLL)));
    n.register(
        0x002B,
        Native::result(|g, c| {
            level_move_f(g, c, mv::TURN_YAW);
            Value::Float(0.0)
        }),
    );
    n.register(
        0x002C,
        Native::result(|g, c| {
            level_move_f(g, c, mv::TURN_PITCH);
            Value::Float(0.0)
        }),
    );
    n.register(
        0x002E,
        Native::result(|g, c| {
            level_rot_f(g, c, rt::SPEED);
            Value::Float(0.0)
        }),
    );
    n.register(
        0x002F,
        Native::result(|g, c| {
            level_rot_f(g, c, rt::PITCH_SPEED);
            Value::Float(0.0)
        }),
    );
    n.register(
        0x0030,
        Native::result(|g, c| {
            level_rot_f(g, c, rt::ROLL_SPEED);
            Value::Float(0.0)
        }),
    );
    n.register(
        0x00E3,
        Native::result(|g, c| {
            level_rot_f(g, c, rt::SPEED2);
            Value::Float(0.0)
        }),
    );
    // 00c4: whether the running level's move is under way (its mode's low 7 bits).
    n.register(
        0x00C4,
        Native::result(|g, c| {
            let w = wk(g, c);
            let m = w.own_move(i32::from(c.level));
            Value::Int(i32::from(w.u16(m + mv::MODE) & 0x7f != 0))
        }),
    );
    // 006d / 006e: every level's move turn yaw / pitch.
    n.register(0x006D, Native::result(|g, c| all_move_f(g, c, mv::TURN_YAW)));
    n.register(0x006E, Native::result(|g, c| all_move_f(g, c, mv::TURN_PITCH)));
    n.register(0x00E4, Native::result(|g, c| all_rot_f(g, c, rt::SPEED2)));
    n.register(0x0070, Native::result(|g, c| all_rot_f(g, c, rt::PITCH_SPEED)));
    n.register(0x0071, Native::result(|g, c| all_rot_f(g, c, rt::ROLL_SPEED)));
    // 0072 / 0074: the move's ease frames (16 bits), the running level's / every level's.
    n.register(
        0x0072,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let w = wk(g, c);
            let m = w.cur_move();
            w.set_i16(m + mv::EASE, v as i16);
            Value::Int(v)
        }),
    );
    n.register(
        0x0074,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let w = wk(g, c);
            for l in 0..9 {
                let m = w.own_move(l);
                w.set_i16(m + mv::EASE, v as i16);
            }
            Value::Int(v)
        }),
    );
    // 0034 / 0035: a bit of the signal mask on / off.
    n.register(
        0x0034,
        Native::result(|g, c| {
            let b = c.stack.pop_int() as u32 & 31;
            let w = wk(g, c);
            w.set_u8(workers::SIGNAL_MASK, w.u8(workers::SIGNAL_MASK) | (1u32 << b) as u8);
            Value::Int(0)
        }),
    );
    n.register(
        0x0035,
        Native::result(|g, c| {
            let b = c.stack.pop_int() as u32 & 31;
            let w = wk(g, c);
            w.set_u8(workers::SIGNAL_MASK, w.u8(workers::SIGNAL_MASK) & !((1u32 << b) as u8));
            Value::Int(0)
        }),
    );
    // 0036 / 0037: every level's move / turn of a worker ends.
    n.register(
        0x0036,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let w = by_index(g, i);
            for l in 0..9 {
                g.workers.list[w].move_reset(l);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x0037,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let w = by_index(g, i);
            for l in 0..9 {
                g.workers.list[w].rot_reset(l);
            }
            Value::Int(0)
        }),
    );
    // 0038–003a: a worker's position.
    for (id, k) in [(0x0038u16, 0usize), (0x0039, 1), (0x003A, 2)] {
        n.register(
            id,
            Native::result(match k {
                0 => |g, c| {
                    let i = c.stack.pop_int();
                    Value::Float(pos_of(g, by_index(g, i))[0])
                },
                1 => |g, c| {
                    let i = c.stack.pop_int();
                    Value::Float(pos_of(g, by_index(g, i))[1])
                },
                _ => |g, c| {
                    let i = c.stack.pop_int();
                    Value::Float(pos_of(g, by_index(g, i))[2])
                },
            }),
        );
    }
    // 003d: keep the current facing in the running level's turn record; 003f reads the kept yaw.
    n.register(
        0x003D,
        Native::result(|g, c| {
            let w = wk(g, c);
            let r = w.rot_of(i32::from(c.level));
            let (y, pi, ro) = (w.pf(p::YAW), w.pf(p::PITCH), w.pf(p::ROLL));
            w.set_f32(r + 0x38, y);
            w.set_f32(r + 0x3c, pi);
            w.set_f32(r + 0x40, ro);
            Value::Int(0)
        }),
    );
    n.register(
        0x003F,
        Native::result(|g, c| {
            let w = wk(g, c);
            let r = w.rot_of(i32::from(c.level));
            Value::Float(w.f32(r + 0x38))
        }),
    );
    // 0098(path, frames): a path move's table and frame count.
    n.register(
        0x0098,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            let table = c.stack.pop_int();
            let w = wk(g, c);
            let m = w.cur_move();
            w.set_u32(m + mv::F14, table as u32);
            w.set_f32(m + mv::TOTAL, frames as f32);
            w.set_f32(m + mv::T, 0.0);
            Value::Int(1)
        }),
    );
}

/// An angle (wrapped) set on the running level's own move record.
fn level_move_wrapped(g: &mut FieldState, c: &mut NativeCall, field: u32) {
    let v = math::wrap(c.stack.pop_float());
    let w = wk(g, c);
    let m = w.cur_move();
    w.set_f32(m + field, v);
}

/// A position block float a native sets on kinds that have it (`kinds`); the block is tested again next step.
fn shape_f(g: &mut FieldState, c: &mut NativeCall, field: u32, kinds: &[u8], ok: i32) -> Value {
    let v = c.stack.pop_float();
    let w = wk(g, c);
    if !kinds.is_empty() && !kinds.contains(&w.kind) {
        return Value::Int(0);
    }
    w.set_pf(field, v);
    w.set_u16(workers::FLAGS2, w.flags2() | 1);
    Value::Int(ok)
}

/// 0054(x, y, z, x2, y2, z2): a line (kinds 2, 5) from the point to the second point, tested, no height.
fn line_shape(g: &mut FieldState, c: &mut aerugo_script::vm::NativeCall<'_>) -> Value {
    let b = pop3(c);
    let a = pop3(c);
    let w = wk(g, c);
    if !matches!(w.kind, 2 | 5) {
        return Value::Int(0);
    }
    w.set_position(a);
    w.set_pf(p::PITCH, b[0]);
    w.set_pf(p::YAW, b[1]);
    w.set_pf(p::ROLL, b[2]);
    w.set_pf(p::HEIGHT, -1.0);
    w.set_u16(workers::FLAGS, w.flags() | 0x80);
    w.set_u16(workers::FLAGS2, w.flags2() | 1);
    g.show_worker(c.worker);
    Value::Int(1)
}

pub(super) fn register_shapes(n: &mut Natives<FieldState>) {
    // 0054(x, y, z, x2, y2, z2): a line (kinds 2, 5) from the point to the second point, tested, no height.
    n.register(0x0054, Native::result(line_shape));
    // 009b(a, b): 0054 with the two points read from script variables (three floats each).
    n.register(
        0x009B,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            let (pa, pb) = (super::shapes::var_place(g, c, a), super::shapes::var_place(g, c, b));
            let at = |p: aerugo_script::memory::Place, k: usize| match p {
                aerugo_script::memory::Place::Global(i, o) => {
                    aerugo_script::memory::Place::Global(i, o + 4 * k)
                }
                aerugo_script::memory::Place::Script(o) => aerugo_script::memory::Place::Script(o + 4 * k),
                aerugo_script::memory::Place::Registers(w, o) => {
                    aerugo_script::memory::Place::Registers(w, o + 4 * k)
                }
            };
            let mut v = [0.0f32; 6];
            for k in 0..3 {
                v[k] = f32::from_bits(c.memory.read_u32(at(pa, k)));
                v[3 + k] = f32::from_bits(c.memory.read_u32(at(pb, k)));
            }
            for x in v {
                c.stack.push(Value::Float(x));
            }
            line_shape(g, c)
        }),
    );
    n.register(0x0055, Native::result(|g, c| shape_f(g, c, p::HEIGHT, &[2, 5, 3, 6], 1)));
    // 0056 / 005c (the same handler)(on): the engine steps a line, box or zone (kinds 2, 3, 5, 6).
    for id in [0x0056u16, 0x005C] {
        n.register(
            id,
            Native::result(|g, c| {
                let on = c.stack.pop_int();
                let w = wk(g, c);
                if !matches!(w.kind, 2 | 3 | 5 | 6) {
                    return Value::Int(0);
                }
                w.set_u16(workers::FLAGS, w.flags() & !0x80 | u16::from(on != 0) << 7);
                w.set_u16(workers::FLAGS2, w.flags2() | 1);
                Value::Int(1)
            }),
        );
    }
    // 0057(x, y, z): a box's or zone's centre (kinds 3, 6), stepped from now on.
    n.register(
        0x0057,
        Native::result(|g, c| {
            let a = pop3(c);
            let w = wk(g, c);
            if !matches!(w.kind, 3 | 6) {
                return Value::Int(0);
            }
            w.set_position(a);
            w.set_u16(workers::FLAGS, w.flags() | 0x80);
            w.set_u16(workers::FLAGS2, w.flags2() | 1);
            g.show_worker(c.worker);
            Value::Int(1)
        }),
    );
    // 0059(x extent, z extent, height): a box's half extents and height (kinds 3, 6); 0058(x, z) pushes −1.0
    // and runs 0059.
    fn box_extents(g: &mut FieldState, c: &mut NativeCall, h: f32) -> Value {
        let z = c.stack.pop_float();
        let x = c.stack.pop_float();
        let w = wk(g, c);
        if !matches!(w.kind, 3 | 6) {
            return Value::Int(0);
        }
        w.set_pf(p::PITCH, x);
        w.set_pf(p::ROLL, z);
        w.set_pf(p::HEIGHT, h);
        w.set_u16(workers::FLAGS2, w.flags2() | 1);
        Value::Int(1)
    }
    n.register(
        0x0059,
        Native::result(|g, c| {
            let h = c.stack.pop_float();
            box_extents(g, c, h)
        }),
    );
    n.register(0x0058, Native::result(|g, c| box_extents(g, c, -1.0)));
    // 005a(yaw): a box's turn (kinds 3, 6).
    n.register(
        0x005A,
        Native::result(|g, c| {
            shape_f(g, c, p::YAW, &[3, 6], 0);
            Value::Int(0)
        }),
    );
    // 0061 / 0062: the talk radius and the next value, as whole numbers.
    for (id, field) in [(0x0061u16, p::TALK_RADIUS), (0x0062, p::R44)] {
        n.register(
            id,
            Native::result(if field == p::TALK_RADIUS {
                |g, c| {
                    let v = c.stack.pop_float();
                    let w = wk(g, c);
                    w.set_pf(p::TALK_RADIUS, ftol(v) as f32);
                    w.set_u16(workers::FLAGS2, w.flags2() | 1);
                    Value::Float(0.0)
                }
            } else {
                |g, c| {
                    let v = c.stack.pop_float();
                    let w = wk(g, c);
                    w.set_pf(p::R44, ftol(v) as f32);
                    w.set_u16(workers::FLAGS2, w.flags2() | 1);
                    Value::Float(0.0)
                }
            }),
        );
    }
    n.register(
        0x0063,
        Native::result(|g, c| {
            shape_f(g, c, p::TALK_ANGLE, &[], 0);
            Value::Float(0.0)
        }),
    );
    n.register(0x0085, Native::result(|g, c| shape_f(g, c, p::RADIUS, &[], 1)));
    n.register(
        0x008D,
        Native::result(|g, c| {
            shape_f(g, c, p::R4C, &[], 0);
            Value::Float(0.0)
        }),
    );
    n.register(
        0x0204,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            wk(g, c).set_pf(p::TALK_HEIGHT, v);
            Value::Float(0.0)
        }),
    );
    // 007a(back, ahead): a line (kinds 2, 5) lengthened at both ends.
    n.register(
        0x007A,
        Native::result(|g, c| {
            let ahead = c.stack.pop_float();
            let back = c.stack.pop_float();
            let w = wk(g, c);
            if !matches!(w.kind, 2 | 5) {
                return Value::Int(0);
            }
            let a = w.position();
            let b = [w.pf(p::PITCH), w.pf(p::YAW), w.pf(p::ROLL)];
            let dv = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
            let len = math::sqrt(math::len2(dv[0], dv[1], dv[2]));
            if 0.0 < len {
                let (fb, fa) = (back / len, ahead / len);
                w.set_position([a[0] - dv[0] * fb, a[1] - dv[1] * fb, a[2] - dv[2] * fb]);
                w.set_pf(p::PITCH, b[0] + fa * dv[0]);
                w.set_pf(p::YAW, dv[1] * fa + b[1]);
                w.set_pf(p::ROLL, dv[2] * fa + b[2]);
            }
            w.set_u16(workers::FLAGS2, w.flags2() | 1);
            g.show_worker(c.worker);
            Value::Int(1)
        }),
    );
    n.register(0x006C, Native::result(|g, c| all_move_f(g, c, mv::SPEED)));
    n.register(0x006F, Native::result(|g, c| all_rot_f(g, c, rt::SPEED)));
    // 0077 / 0078: a worker's running move / turn ends.
    n.register(
        0x0077,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let w = by_index(g, i);
            g.workers.list[w].move_reset(-1);
            Value::Int(0)
        }),
    );
    n.register(
        0x0078,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            let w = by_index(g, i);
            g.workers.list[w].rot_reset(-1);
            Value::Int(0)
        }),
    );
    // 0090 / 0091: the heading of travel and the facing; 0092 / 0093 another worker's; 0174 / 0181 the pitch
    // and roll.
    n.register(0x0090, Native::result(|g, c| Value::Float(wk(g, c).pf(p::DIR_YAW))));
    n.register(0x0091, Native::result(|g, c| Value::Float(wk(g, c).pf(p::YAW))));
    n.register(0x0174, Native::result(|g, c| Value::Float(wk(g, c).pf(p::PITCH))));
    n.register(0x0181, Native::result(|g, c| Value::Float(wk(g, c).pf(p::ROLL))));
    n.register(
        0x0092,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            Value::Float(g.workers.list[by_index(g, i)].pf(p::DIR_YAW))
        }),
    );
    n.register(
        0x0093,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            Value::Float(g.workers.list[by_index(g, i)].pf(p::YAW))
        }),
    );
    // 0095 / 0096: face (and head) a way at once; the turn ends.
    n.register(0x0095, Native::result(set_facing));
    // 0096: the pitch (and the pitch of travel) at once.
    n.register(
        0x0096,
        Native::result(|g, c| {
            wk(g, c).rot_reset(-1);
            let v = math::wrap(c.stack.pop_float());
            let w = wk(g, c);
            w.set_pf(p::DIR_PITCH, v);
            w.set_pf(p::PITCH, v);
            g.show_worker(c.worker);
            Value::Float(v)
        }),
    );
    // 00a4 / 00a5: the distance between two workers, in space / on the ground plane.
    n.register(
        0x00A4,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            let (pa, pb) = (pos_of(g, by_index(g, a)), pos_of(g, by_index(g, b)));
            let (dx, dy, dz) = (pa[0] - pb[0], pa[1] - pb[1], pa[2] - pb[2]);
            let d = |x: f32| f64::from(x);
            Value::Float((d(dy) * d(dy) + d(dz) * d(dz) + d(dx) * d(dx)).sqrt() as f32)
        }),
    );
    n.register(
        0x00A5,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            let (pa, pb) = (pos_of(g, by_index(g, a)), pos_of(g, by_index(g, b)));
            let (dx, dz) = (pa[0] - pb[0], pa[2] - pb[2]);
            let d = |x: f32| f64::from(x);
            Value::Float((d(dz) * d(dz) + d(dx) * d(dx)).sqrt() as f32)
        }),
    );
    // 00f4(x, y, z) / 00f5(x, y, z): the heading / pitch toward a point.
    n.register(
        0x00F4,
        Native::result(|g, c| {
            let [x, _, z] = pop3(c);
            let pos = wk(g, c).position();
            Value::Float(math::heading(pos[0], pos[2], x, z))
        }),
    );
    n.register(
        0x00F5,
        Native::result(|g, c| {
            let t = pop3(c);
            let pos = wk(g, c).position();
            Value::Float(math::angles_to(pos, t).1)
        }),
    );
    // 0127(worker): within the touch radius of a worker; 0128(worker, radius): within a radius.
    n.register(
        0x0127,
        Native::result(|g, c| {
            let r = wk(g, c).pf(p::RADIUS);
            within(g, c, r)
        }),
    );
    n.register(
        0x0128,
        Native::result(|g, c| {
            let r = c.stack.pop_float();
            within(g, c, r)
        }),
    );
}

/// The share (0–1) of a turn still to make: 1 − (1 − left/π)·1.2 (`wide`) or 1 − (1 − left/(π/2)), where left is
/// the running level's turn target minus the facing (wrapped, absolute).
fn turn_share(left: f32, wide: bool) -> f32 {
    let d = |x: f32| f64::from(x);
    let pi = d(std::f32::consts::PI);
    let x = if wide { (d(left) / pi) as f32 } else { (d(left) / d(std::f32::consts::FRAC_PI_2)) as f32 };
    let u = (1.0 - d(x)) as f32;
    let v = if wide { (d(u) * d(1.2f32)) as f32 } else { u };
    ((1.0 - d(v)) as f32).clamp(0.0, 1.0)
}

pub(super) fn register_turn_status(n: &mut Natives<FieldState>) {
    // 00c6: the running level's turn: bit 0 the turn left is positive, bit 1 it is at least the limit (degrees).
    n.register(
        0x00C6,
        Native::result(|g, c| {
            let limit = g.globals.turn_limit_deg;
            let w = wk(g, c);
            let r = w.cur_rot();
            let left = w.f32(r + rt::DELTA);
            let mut b = i32::from(0.0 < left);
            let lim = f64::from(limit) * f64::from(std::f32::consts::PI) / 180.0;
            if lim <= f64::from(left.abs()) {
                b |= 2;
            }
            Value::Int(b)
        }),
    );
    // 00c8(flags, a, b): a (or b with flag 2) scaled by the share of the turn to its target still to make,
    // truncated.
    n.register(
        0x00C8,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            let flags = c.stack.pop_int();
            let w = wk(g, c);
            let r = w.cur_rot();
            let left = math::wrap(w.f32(r + rt::TARGET) - w.pf(p::YAW)).abs();
            let wide = flags & 2 != 0;
            let f = turn_share(left, wide);
            let k = if wide { b } else { a } as f32;
            Value::Int(ftol((f64::from(f) * f64::from(k)) as f32))
        }),
    );
    // 01f0(degrees): the wide-turn limit; returns it.
    n.register(
        0x01F0,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            g.globals.turn_limit_deg = v;
            Value::Float(v)
        }),
    );
}

fn within(g: &mut FieldState, c: &mut NativeCall, r: f32) -> Value {
    let i = c.stack.pop_int();
    let o = pos_of(g, by_index(g, i));
    let me = pos_of(g, c.worker);
    let (dx, dy, dz) = (me[0] - o[0], me[1] - o[1], me[2] - o[2]);
    let d = |x: f32| f64::from(x);
    Value::Int(i32::from(d(dx) * d(dx) + d(dy) * d(dy) + d(dz) * d(dz) <= d(r) * d(r)))
}

/// Face a way at once: the running turn ends, the facing and the heading of travel take the wrapped value; the
/// body turns and heads that way; a bound object or camera id follows.
fn set_facing(g: &mut FieldState, c: &mut NativeCall) -> Value {
    let w = wk(g, c);
    w.rot_reset(-1);
    let v = math::wrap(c.stack.pop_float());
    let w = wk(g, c);
    w.set_pf(p::DIR_YAW, v);
    w.set_pf(p::YAW, v);
    let (bound, bind) = (w.chr_bound(), w.bind());
    // The body's auto-face switch off, then the bound body's facing and heading.
    body_flag_400_off(g, c.worker);
    if bound {
        let a = g.actor(c.worker);
        a.yaw = v;
        a.move_heading = v;
    } else if matches!(bind, 4 | 5) {
        g.show_worker(c.worker);
    }
    g.show_worker(c.worker);
    Value::Float(v)
}
