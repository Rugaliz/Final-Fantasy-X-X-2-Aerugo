//! The camera natives (0x6000–0x6089) on the camera system's island. Most act on the calling worker's camera
//! handle: the one it keeps, or a new one of the event camera's kind (1) for the worker the script header names,
//! else of kind 3, on camera record 0. Side 0 is the focus, side 1 the eye. Angles come in degrees.
//!
//! A handler's result is what the original's returns: the handle for some, 0 for others.

use aerugo_script::memory::{Place, decode_address};
use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::{NativeCall, Value};

use super::{Actor, FieldState};
use crate::event_camera::{CamEnv, CamMem, PATH_BUFFERS, PATH_COUNTS, TABLE};

/// Degrees to radians as the handlers convert (in double, rounded to float).
fn rad(deg: f32) -> f32 {
    (deg as f64 * 3.141_592_741_012_573_2 / 180.0) as f32
}

/// The camera system's view of the field.
#[derive(Debug)]
pub struct CamWorld<'a> {
    pub actors: &'a [Actor],
    pub streams: &'a mut super::rng_streams::RngStreams,
    pub current: i32,
    pub chr_count: i32,
}

impl CamEnv for CamWorld<'_> {
    fn worker_pos(&self, w: i32) -> [f32; 3] {
        let w = if w < 0 || w as usize >= self.actors.len() { self.current } else { w };
        self.actors.get(w.max(0) as usize).map_or([0.0; 3], |a| a.pos)
    }
    fn worker_yaw(&self, w: i32) -> f32 {
        self.actors.get(w.max(0) as usize).map_or(0.0, |a| a.yaw)
    }
    fn worker_count(&self) -> i32 {
        self.chr_count
    }
    fn worker_shown(&self, i: i32) -> bool {
        self.actors.get(i.max(0) as usize).is_some_and(|a| a.bind_kind == 1 && a.model.is_some())
    }
    fn current_worker(&self) -> i32 {
        self.current
    }
    fn rng9(&mut self) -> i32 {
        self.streams.next(9) as i32
    }
}

impl FieldState {
    /// The camera system's view of this field (`current`: the running worker, −1 none).
    pub fn cam_world(&mut self, current: i32) -> (&mut CamMem, CamWorld<'_>) {
        let world = CamWorld {
            actors: &self.actors,
            streams: &mut self.rng_streams,
            current,
            chr_count: self.chr_worker_count,
        };
        (&mut self.cam, world)
    }

    /// What the renderer draws from: screen slot 0's record — the eye's and the focus's view points (with their
    /// shakes), the roll and the screen distance — once a script has opened a camera on it.
    pub fn cam_publish(&mut self) {
        use crate::event_camera::{RECORD, RECORDS, SCREENS};
        let r = u32::from(self.cam.u8(SCREENS + 0xc1)).min(2);
        let rec = RECORDS + r * RECORD;
        let open = (0..4).any(|sub| self.cam.u32(rec + 0xc68 + sub * 8) != 0);
        if !open {
            return;
        }
        self.camera.position = Some(self.cam.vec3(CamMem::side(r, 1) + 0x10));
        self.camera.focus = Some(self.cam.vec3(CamMem::side(r, 0) + 0x10));
        self.camera.roll = self.cam.f32(rec + 0xc90);
        self.camera.screen_distance = self.cam.f32(rec + 0xc9c);
        self.cam_view = Some(crate::event_camera::view::view_matrix(
            self.cam.vec4(CamMem::side(r, 1) + 0x10),
            self.cam.vec4(CamMem::side(r, 0) + 0x10),
            self.cam.vec4(rec),
            self.cam.f32(rec + 0xc90),
        ));
    }

    /// The frame's camera step (after the workers ran), then each screen slot's step.
    pub fn cam_frame(&mut self) {
        let (cam, mut world) = self.cam_world(-1);
        cam.process(&mut world);
        for i in 0..3 {
            cam.screen_step(&mut world, i);
        }
    }
}

/// Worker `w`'s camera handle (kept, or taken now: kind 1 for the header's camera worker, else 3; record 0).
fn worker_handle(g: &mut FieldState, w: usize) -> u32 {
    let kept = g.cam_slots.get(w).copied().unwrap_or(0);
    if kept != 0 {
        return kept;
    }
    let which = u32::from(w as u16 == g.camera_worker);
    let h = g.cam.get_handle(5, which, 0);
    if let Some(s) = g.cam_slots.get_mut(w) {
        *s = h;
    }
    h
}

/// The calling worker's handle.
fn handle(g: &mut FieldState, c: &NativeCall) -> u32 {
    worker_handle(g, c.worker)
}

/// The calling worker's handle for character cameras: kept, or a new one of kind 5.
fn chr_handle(g: &mut FieldState, c: &NativeCall) -> u32 {
    let kept = g.cam_slots.get(c.worker).copied().unwrap_or(0);
    if kept != 0 {
        return kept;
    }
    let h = g.cam.get_handle(5, 2, 0);
    if let Some(s) = g.cam_slots.get_mut(c.worker) {
        *s = h;
    }
    h
}

fn int(h: u32) -> Value {
    Value::Int(h as i32)
}

/// A float written to the variable at a native's address argument.
fn store(c: &mut NativeCall, addr: i32, v: f32) {
    c.memory.write_u32(decode_address(addr), v.to_bits());
}

/// The id of the script table at a variable address, copied into the island's table map (a count, then up to
/// 0x30-byte records from +0x10).
fn table(g: &mut FieldState, c: &NativeCall, addr: i32) -> u32 {
    let place = decode_address(addr);
    let bytes: Option<&[u8]> = match place {
        Place::Script(off) => c.memory.script.get(off..),
        Place::Global(b, off) => c.memory.global[b].get(off..),
        Place::Registers(..) => None,
    };
    let id = TABLE | (addr as u32 & 0x07FF_FFFF);
    if let Some(b) = bytes {
        let n =
            b.get(0..4).map_or(0, |v| i32::from_le_bytes(v.try_into().unwrap())).clamp(0, 0x1000) as usize;
        let len = (0x10 + 0x30 * n).min(b.len());
        g.cam.tables.insert(id, b[..len].to_vec());
    }
    id
}

/// A path argument: 0 and 1 the recorded buffers of the focus and the eye (their header count set; none with
/// fewer than three points), any other value a script table's address.
fn path(g: &mut FieldState, c: &NativeCall, v: i32) -> Option<u32> {
    if v == 0 || v == 1 {
        let n = g.cam.i32(PATH_COUNTS + v as u32 * 4);
        if n < 3 {
            return None;
        }
        let at = PATH_BUFFERS + v as u32 * 0x210;
        g.cam.set_i32(at, n);
        Some(at)
    } else {
        Some(table(g, c, v))
    }
}

/// Move frames (`by_angles`, `mode`) for a side.
fn move_frames(g: &mut FieldState, c: &mut NativeCall, side: u32, by_angles: u8, mode: i16) -> Value {
    let h = handle(g, c);
    let frames = c.stack.pop_int() as i16;
    g.cam.set_move_frames(h, side, by_angles, frames, mode);
    Value::Int(0)
}

/// Move time profile for a side.
fn move_time(g: &mut FieldState, c: &mut NativeCall, side: u32, by_angles: u8) -> Value {
    let h = handle(g, c);
    let lead = c.stack.pop_int() as i16;
    let decel = c.stack.pop_int() as i16;
    let steady = c.stack.pop_int() as i16;
    let accel = c.stack.pop_int() as i16;
    g.cam.set_move_time(h, side, by_angles, [accel, steady, decel, lead]);
    Value::Int(0)
}

/// A side's place: x, y, z.
fn set_place(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
    let h = handle(g, c);
    let z = c.stack.pop_float();
    let y = c.stack.pop_float();
    let x = c.stack.pop_float();
    // The handler's scene fixes (map, story value and battle conditions) need values only a battle or particular
    // maps set; outside them they do nothing.
    g.cam.set_vec(h, side, [x, y, z, 0.0]);
    Value::Int(0)
}

/// A side's place read into three variables (`real`: the view point with its shake, else the output).
fn get_place(g: &mut FieldState, c: &mut NativeCall, side: u32, real: bool) -> Value {
    let za = c.stack.pop_int();
    let ya = c.stack.pop_int();
    let xa = c.stack.pop_int();
    let h = handle(g, c);
    let at = CamMem::side(h, side) + if real { 0x10 } else { 0x20 };
    let v = g.cam.vec3(at);
    store(c, xa, v[0]);
    store(c, ya, v[1]);
    store(c, za, v[2]);
    Value::Int(0)
}

/// A side at a heading, pitch (degrees) and distance around the other (`add`: added to what it has).
fn set_polar(g: &mut FieldState, c: &mut NativeCall, side: u32, add: bool) -> Value {
    let h = handle(g, c);
    let dist = c.stack.pop_float();
    let pitch = rad(c.stack.pop_float());
    let yaw = rad(c.stack.pop_float());
    if add {
        g.cam.add_polar(h, side, [yaw, pitch, dist]);
    } else {
        g.cam.set_polar(h, side, [yaw, pitch, dist]);
    }
    Value::Int(0)
}

/// Measured placements: `variant` < 0 a heading from the anchor to a ground point (x, z) at a pitch; 0 / 1 to a
/// point (x, y, z) at the measured pitch, the distance set (0) or less the measured one (1). Each adds the
/// measured distance over its divisor.
fn set_hypot(g: &mut FieldState, c: &mut NativeCall, side: u32, variant: i32) -> Value {
    let h = handle(g, c);
    let f1 = c.stack.pop_float();
    let f2 = c.stack.pop_float();
    let f3 = c.stack.pop_float();
    let f4 = c.stack.pop_float();
    let f5 = c.stack.pop_float();
    let f6 = c.stack.pop_float();
    if variant < 0 {
        g.cam.set_hypot_ground(h, side, [f6, f5], rad(f4), f3, f2, f1);
    } else {
        g.cam.set_hypot_point(h, side, variant != 0, [f6, f5, f4], f3, f2, f1);
    }
    Value::Int(0)
}

/// Limit lines from (x0, z0) to (x1, z1) keeping `k` past them (`flag` 0: soft, 1: linear).
fn limit_line(g: &mut FieldState, c: &mut NativeCall, side: u32, flag: i32) -> Value {
    let h = handle(g, c);
    let k = c.stack.pop_float();
    let z1 = c.stack.pop_float();
    let x1 = c.stack.pop_float();
    let z0 = c.stack.pop_float();
    let x0 = c.stack.pop_float();
    g.cam.add_limit_line(h, side, flag, [x0, z0, x1, z1], k);
    Value::Int(0)
}

/// Height limits (`linear` 0: soft, 1: linear).
fn limit_height(g: &mut FieldState, c: &mut NativeCall, side: u32, linear: i32) -> Value {
    let h = handle(g, c);
    let k = c.stack.pop_float();
    let max = c.stack.pop_float();
    let min = c.stack.pop_float();
    g.cam.add_limit_height(h, side, linear != 0, min, max, k);
    Value::Int(0)
}

/// Shake a side (`mode` 1–8).
fn shake(g: &mut FieldState, c: &mut NativeCall, side: u32, mode: u8) -> Value {
    let h = handle(g, c);
    let jitter = c.stack.pop_int() as u8;
    let amp = c.stack.pop_int() as u16;
    let frames = c.stack.pop_int() as i16;
    let step = c.stack.pop_float();
    let mask = c.stack.pop_int() as u32;
    g.cam.shake(h, side, mask, mode, step, frames, amp, jitter);
    int(h)
}

/// Shake a screen slot (`mode` 1–6).
fn screen_shake(g: &mut FieldState, c: &mut NativeCall, mode: u8) -> Value {
    let jitter = c.stack.pop_int() as u8;
    let amp = c.stack.pop_int() as u16;
    let frames = c.stack.pop_int() as i16;
    let step = c.stack.pop_float();
    let mask = c.stack.pop_int() as u32;
    let screen = c.stack.pop_int() as u32;
    g.cam.screen_shake(screen, mask, mode, step, frames, amp, jitter);
    Value::Int(0)
}

/// Follow a path: (path, start, step, mode).
fn follow_path(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
    let h = handle(g, c);
    let mode = c.stack.pop_int() as u8;
    let step = c.stack.pop_float();
    let start = c.stack.pop_float();
    let p = c.stack.pop_int();
    if let Some(id) = path(g, c, p) {
        g.cam.follow_path(h, side, id, start, step, mode);
    }
    int(h)
}

/// Attach a side to characters (`variant` 0–4; 0 with a height share only).
fn attach(g: &mut FieldState, c: &mut NativeCall, side: u32, variant: u32) -> Value {
    let h = chr_handle(g, c);
    let (share, yaw, pitch, size);
    if variant == 0 {
        share = (c.stack.pop_float() as f64 / 100.0) as f32;
        (yaw, pitch, size) = (0.0, 0.0, 0.0);
    } else {
        size = (c.stack.pop_float() as f64 / 100.0) as f32;
        pitch = c.stack.pop_float();
        yaw = c.stack.pop_float();
        share = (c.stack.pop_float() as f64 / 100.0) as f32;
    }
    let b = c.stack.pop_int();
    let a = c.stack.pop_int();
    let (mut share, mut size) = (share, size);
    if variant != 0 && g.magic.slot != -1 {
        // The remaster's fixes for two magic effects' cameras (others need a battle's script).
        let off = |v: f32, k: f64| ((v as f64 - k) as i32).wrapping_abs() as f32 as f64;
        const NEAR: f64 = 9.999_999_747_378_752e-5;
        match g.magic.file {
            0x2C1 if off(size, 30.489_999_771_118_164) < NEAR || off(size, 29.489_999_771_118_164) < NEAR => {
                size = (size as f64 - 3.5) as f32;
            }
            0x2C0 if off(size, 10.090_000_152_587_89) < NEAR => {
                share = (share as f64 + 0.100_000_001_490_116_12) as f32
            }
            _ => {}
        }
    }
    let env = CamWorld {
        actors: &g.actors,
        streams: &mut g.rng_streams,
        current: c.worker as i32,
        chr_count: g.chr_worker_count,
    };
    g.cam.attach_chr(&env, h, side, variant, a, b, share, rad(yaw), rad(pitch), size);
    Value::Int(0)
}

pub fn register(n: &mut Natives<FieldState>) {
    // Suspend / resume the camera of the worker given.
    n.register(
        0x6000,
        Native::result(|g, c| {
            let w = c.stack.pop_int().max(0) as usize;
            let h = worker_handle(g, w);
            g.cam.set_suspended(h, 1);
            int(h)
        }),
    );
    n.register(
        0x6001,
        Native::result(|g, c| {
            let w = c.stack.pop_int().max(0) as usize;
            let h = worker_handle(g, w);
            g.cam.set_suspended(h, 0);
            int(h)
        }),
    );
    n.register(0x6002, Native::result(|g, c| set_place(g, c, 1)));
    n.register(0x6020, Native::result(|g, c| set_place(g, c, 0)));
    n.register(0x6003, Native::result(|g, c| get_place(g, c, 1, false)));
    n.register(0x6021, Native::result(|g, c| get_place(g, c, 0, false)));
    n.register(0x6085, Native::result(|g, c| get_place(g, c, 1, true)));
    n.register(0x6086, Native::result(|g, c| get_place(g, c, 0, true)));
    n.register(0x6004, Native::result(|g, c| set_polar(g, c, 1, false)));
    n.register(0x6005, Native::result(|g, c| set_polar(g, c, 1, true)));
    n.register(0x6022, Native::result(|g, c| set_polar(g, c, 0, false)));
    n.register(0x6023, Native::result(|g, c| set_polar(g, c, 0, true)));
    n.register(0x6006, Native::result(|g, c| set_hypot(g, c, 1, -1)));
    n.register(0x6007, Native::result(|g, c| set_hypot(g, c, 1, 0)));
    n.register(0x6008, Native::result(|g, c| set_hypot(g, c, 1, 1)));
    n.register(0x6024, Native::result(|g, c| set_hypot(g, c, 0, -1)));
    n.register(0x6025, Native::result(|g, c| set_hypot(g, c, 0, 0)));
    n.register(0x6026, Native::result(|g, c| set_hypot(g, c, 0, 1)));
    // Follow up to two workers: (worker a, worker b, height offset).
    fn act(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
        let h = handle(g, c);
        let y = c.stack.pop_float();
        let b = c.stack.pop_int() as i16;
        let a = c.stack.pop_int() as i16;
        let env = CamWorld {
            actors: &g.actors,
            streams: &mut g.rng_streams,
            current: c.worker as i32,
            chr_count: g.chr_worker_count,
        };
        g.cam.set_act_workers(&env, h, side, a, b, [0.0, y, 0.0, 0.0]);
        Value::Int(0)
    }
    n.register(0x6009, Native::result(|g, c| act(g, c, 1)));
    n.register(0x6027, Native::result(|g, c| act(g, c, 0)));
    n.register(0x600A, Native::result(|g, c| limit_line(g, c, 1, 0)));
    n.register(0x600B, Native::result(|g, c| limit_line(g, c, 1, 1)));
    n.register(0x6028, Native::result(|g, c| limit_line(g, c, 0, 0)));
    n.register(0x6029, Native::result(|g, c| limit_line(g, c, 0, 1)));
    n.register(0x600C, Native::result(|g, c| limit_height(g, c, 1, 0)));
    n.register(0x600D, Native::result(|g, c| limit_height(g, c, 1, 1)));
    n.register(0x602A, Native::result(|g, c| limit_height(g, c, 0, 0)));
    n.register(0x602B, Native::result(|g, c| limit_height(g, c, 0, 1)));
    fn held(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
        let h = handle(g, c);
        let v = c.stack.pop_int();
        g.cam.set_limits_held(h, side, (v & 1) as u8);
        int(h)
    }
    n.register(0x600E, Native::result(|g, c| held(g, c, 1)));
    n.register(0x602C, Native::result(|g, c| held(g, c, 0)));
    n.register(
        0x600F,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.clear_limits(h, 1);
            int(h)
        }),
    );
    n.register(
        0x602D,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.clear_limits(h, 0);
            int(h)
        }),
    );
    n.register(0x6010, Native::result(|g, c| move_frames(g, c, 1, 0, 1)));
    n.register(0x6011, Native::result(|g, c| move_frames(g, c, 1, 1, 1)));
    n.register(0x6012, Native::result(|g, c| move_frames(g, c, 1, 0, 3)));
    n.register(0x6013, Native::result(|g, c| move_frames(g, c, 1, 1, 3)));
    n.register(0x602E, Native::result(|g, c| move_frames(g, c, 0, 0, 1)));
    n.register(0x602F, Native::result(|g, c| move_frames(g, c, 0, 1, 1)));
    n.register(0x6030, Native::result(|g, c| move_frames(g, c, 0, 0, 3)));
    n.register(0x6031, Native::result(|g, c| move_frames(g, c, 0, 1, 3)));
    n.register(0x6014, Native::result(|g, c| move_time(g, c, 1, 0)));
    n.register(0x6015, Native::result(|g, c| move_time(g, c, 1, 1)));
    n.register(0x6032, Native::result(|g, c| move_time(g, c, 0, 0)));
    n.register(0x6033, Native::result(|g, c| move_time(g, c, 0, 1)));
    // Stop a side's moves. The eye's also counts for magic effect 0x92 once its effect starts the count: the
    // second stop sets the coin 6080 then forces.
    n.register(
        0x6016,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.move_stop(h, 1);
            if g.magic.file == 0x92 && g.magic.stop_count >= 0 {
                g.magic.stop_count += 1;
                if g.magic.stop_count == 2 {
                    g.magic.coin = true;
                    g.magic.stop_count = -1;
                }
            }
            int(h)
        }),
    );
    n.register(
        0x6034,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.move_stop(h, 0);
            int(h)
        }),
    );
    // Easing divisors (x, y, z, roll).
    fn ease(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
        let h = handle(g, c);
        let r = c.stack.pop_float();
        let z = c.stack.pop_float();
        let y = c.stack.pop_float();
        let x = c.stack.pop_float();
        g.cam.set_easing(h, side, [x, y, z, r]);
        Value::Int(0)
    }
    n.register(0x6017, Native::result(|g, c| ease(g, c, 1)));
    n.register(0x6035, Native::result(|g, c| ease(g, c, 0)));
    // Acceleration limit: (start speed, cap, speed).
    fn accel(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
        let h = handle(g, c);
        let speed = c.stack.pop_float();
        let cap = c.stack.pop_float();
        let start = c.stack.pop_float();
        g.cam.set_accel(h, side, start, cap, speed);
        Value::Int(0)
    }
    n.register(0x6018, Native::result(|g, c| accel(g, c, 1)));
    n.register(0x6036, Native::result(|g, c| accel(g, c, 0)));
    n.register(
        0x6019,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.clear_accel(h, 1);
            int(h)
        }),
    );
    n.register(
        0x6037,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.clear_accel(h, 0);
            int(h)
        }),
    );
    // Waits for a side's moves; the plain calls return the moving bits.
    n.register(
        0x601A,
        Native {
            init: None,
            wait: Some(|g, c| {
                let h = handle(g, c);
                if g.cam.is_moving(h, 1) != 0 { 0 } else { 5 }
            }),
            result: None,
        },
    );
    n.register(
        0x6038,
        Native {
            init: None,
            wait: Some(|g, c| {
                let h = handle(g, c);
                if g.cam.is_moving(h, 0) != 0 { 0 } else { 5 }
            }),
            result: None,
        },
    );
    n.register(
        0x601B,
        Native::result(|g, c| {
            let h = handle(g, c);
            Value::Int(g.cam.is_moving(h, 1) as i32)
        }),
    );
    n.register(
        0x6039,
        Native::result(|g, c| {
            let h = handle(g, c);
            Value::Int(g.cam.is_moving(h, 0) as i32)
        }),
    );
    // Eye tables: (table, heading) for 601c / 601e; (table, x, z, heading offset) for 601d / 601f (heading
    // measured to the point). 601c / 601d square the distances.
    fn point_table(g: &mut FieldState, c: &mut NativeCall, side: u32, squared: u8, around: bool) -> Value {
        let h = handle(g, c);
        let (yaw, x, z);
        if around {
            z = c.stack.pop_float();
            x = c.stack.pop_float();
            yaw = rad(c.stack.pop_float());
        } else {
            yaw = rad(c.stack.pop_float());
            (x, z) = (0.0, 0.0);
        }
        let t = c.stack.pop_int();
        let id = table(g, c, t);
        if around {
            g.cam.set_point_table_around(h, side, squared, id, yaw, x, z);
        } else {
            g.cam.set_point_table(h, side, squared, id, yaw);
        }
        int(h)
    }
    n.register(0x601C, Native::result(|g, c| point_table(g, c, 1, 1, false)));
    n.register(0x601E, Native::result(|g, c| point_table(g, c, 1, 0, false)));
    n.register(0x601D, Native::result(|g, c| point_table(g, c, 1, 1, true)));
    n.register(0x601F, Native::result(|g, c| point_table(g, c, 1, 0, true)));
    n.register(
        0x603A,
        Native::result(|g, c| {
            let h = handle(g, c);
            let r = rad(c.stack.pop_float());
            g.cam.set_roll(h, r);
            int(h)
        }),
    );
    n.register(
        0x603B,
        Native::result(|g, c| {
            let h = handle(g, c);
            let d = c.stack.pop_float();
            g.cam.set_screen_distance(h, d);
            int(h)
        }),
    );
    // The eye's or focus's heading from the workers' facing: (heading, pitch, distance, unused).
    fn act2(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
        let h = handle(g, c);
        let _ = c.stack.pop_float();
        let dist = c.stack.pop_float();
        let pitch = rad(c.stack.pop_float());
        let yaw = rad(c.stack.pop_float());
        let env = CamWorld {
            actors: &g.actors,
            streams: &mut g.rng_streams,
            current: c.worker as i32,
            chr_count: g.chr_worker_count,
        };
        g.cam.set_act_facing(&env, h, side, yaw, pitch, dist);
        Value::Int(0)
    }
    n.register(0x603C, Native::result(|g, c| act2(g, c, 1)));
    n.register(0x603D, Native::result(|g, c| act2(g, c, 0)));
    n.register(0x603E, Native::result(|g, c| attach(g, c, 1, 0)));
    n.register(0x603F, Native::result(|g, c| attach(g, c, 0, 0)));
    n.register(0x6040, Native::result(|g, c| attach(g, c, 1, 1)));
    n.register(0x6041, Native::result(|g, c| attach(g, c, 0, 1)));
    n.register(0x6044, Native::result(|g, c| attach(g, c, 1, 2)));
    n.register(0x6045, Native::result(|g, c| attach(g, c, 0, 2)));
    n.register(0x604C, Native::result(|g, c| attach(g, c, 1, 3)));
    n.register(0x604D, Native::result(|g, c| attach(g, c, 1, 4)));
    // The moves a set skips (the mask complemented).
    fn skip(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
        let h = handle(g, c);
        let v = c.stack.pop_int();
        g.cam.set_skip_mask(h, side, (v ^ 0xFFFF) as u8);
        int(h)
    }
    n.register(0x6042, Native::result(|g, c| skip(g, c, 0)));
    n.register(0x6043, Native::result(|g, c| skip(g, c, 1)));
    n.register(0x6046, Native::result(|g, c| follow_path(g, c, 1)));
    n.register(0x6047, Native::result(|g, c| follow_path(g, c, 0)));
    n.register(
        0x6048,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.spline_clear(h, 1);
            int(h)
        }),
    );
    n.register(
        0x604A,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.spline_clear(h, 0);
            int(h)
        }),
    );
    n.register(
        0x6049,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.spline_record(h, 1);
            int(h)
        }),
    );
    n.register(
        0x604B,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.spline_record(h, 0);
            int(h)
        }),
    );
    // Screen slots.
    n.register(
        0x604E,
        Native::result(|g, c| {
            let extra = c.stack.pop_int();
            let hgt = c.stack.pop_int();
            let w = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let s = c.stack.pop_int() as u32;
            g.cam.screen_set_on(s, 1);
            g.cam.screen_set_rect_a(s, [x, y, w, hgt], extra as u32);
            Value::Int(0)
        }),
    );
    n.register(
        0x604F,
        Native::result(|g, c| {
            let s = c.stack.pop_int() as u32;
            g.cam.screen_set_on_record(s, 0, s as u8);
            Value::Int(0)
        }),
    );
    n.register(
        0x6050,
        Native::result(|g, c| {
            let hgt = c.stack.pop_int();
            let w = c.stack.pop_int();
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let s = c.stack.pop_int() as u32;
            g.cam.screen_set_rect_b(s, [x, y, w, hgt]);
            Value::Int(0)
        }),
    );
    fn follow(g: &mut FieldState, c: &mut NativeCall, k: i32) -> Value {
        let v = c.stack.pop_int() as u8;
        let s = c.stack.pop_int() as u32;
        g.cam.screen_set_follow(s, k, v);
        Value::Int(0)
    }
    n.register(0x6051, Native::result(|g, c| follow(g, c, 1)));
    n.register(0x6052, Native::result(|g, c| follow(g, c, 0)));
    fn rect_frames(g: &mut FieldState, c: &mut NativeCall, k: u32, mode: i16) -> Value {
        let frames = c.stack.pop_int() as i16;
        let s = c.stack.pop_int() as u32;
        g.cam.screen_move_frames(s, k, frames, mode);
        Value::Int(0)
    }
    n.register(0x6053, Native::result(|g, c| rect_frames(g, c, 0, 1)));
    n.register(0x6054, Native::result(|g, c| rect_frames(g, c, 0, 3)));
    n.register(0x6056, Native::result(|g, c| rect_frames(g, c, 1, 1)));
    n.register(0x6057, Native::result(|g, c| rect_frames(g, c, 1, 3)));
    fn rect_profile(g: &mut FieldState, c: &mut NativeCall, k: u32) -> Value {
        let d = c.stack.pop_int() as i16;
        let st = c.stack.pop_int() as i16;
        let a = c.stack.pop_int() as i16;
        let s = c.stack.pop_int() as u32;
        g.cam.screen_move_profile(s, k, a, st, d);
        Value::Int(0)
    }
    n.register(0x6055, Native::result(|g, c| rect_profile(g, c, 0)));
    n.register(0x6058, Native::result(|g, c| rect_profile(g, c, 1)));
    n.register(
        0x6075,
        Native::result(|g, c| {
            let r = c.stack.pop_int() as u8;
            let s = c.stack.pop_int() as u32;
            g.cam.screen_set_record(s, r);
            Value::Int(0)
        }),
    );
    n.register(
        0x6079,
        Native::result(|g, c| {
            let s = c.stack.pop_int() as u32;
            g.cam.screen_move_stop(s, 0);
            Value::Int(0)
        }),
    );
    n.register(
        0x607A,
        Native::result(|g, c| {
            let s = c.stack.pop_int() as u32;
            g.cam.screen_move_stop(s, 1);
            Value::Int(0)
        }),
    );
    n.register(
        0x607B,
        Native::waiting(
            |_, c| c.scratch[0] = c.stack.pop_int(),
            |g, c| if g.cam.screen_moving(c.scratch[0] as u32, 0) != 0 { 0 } else { 5 },
        ),
    );
    n.register(
        0x607C,
        Native::waiting(
            |_, c| c.scratch[0] = c.stack.pop_int(),
            |g, c| if g.cam.screen_moving(c.scratch[0] as u32, 1) != 0 { 0 } else { 5 },
        ),
    );
    // Rails for the focus: (path, mode) at k 0; (path, k, mode); the eye on a path at the focus's rail place.
    n.register(
        0x6059,
        Native::result(|g, c| {
            let mode = c.stack.pop_int();
            let p = c.stack.pop_int();
            if let Some(id) = path(g, c, p) {
                let h = handle(g, c);
                g.cam.add_rail(h, 0, id, 0.0, mode);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x605A,
        Native::result(|g, c| {
            let mode = c.stack.pop_int();
            let k = c.stack.pop_float();
            let p = c.stack.pop_int();
            if let Some(id) = path(g, c, p) {
                let h = handle(g, c);
                g.cam.add_rail(h, 0, id, k, mode);
            }
            Value::Int(0)
        }),
    );
    n.register(
        0x605B,
        Native::result(|g, c| {
            let mode = c.stack.pop_int() as u8;
            let p = c.stack.pop_int();
            if let Some(id) = path(g, c, p) {
                let h = handle(g, c);
                g.cam.set_eye_rail_path(h, 1, id, mode);
            }
            Value::Int(0)
        }),
    );
    // Side shakes: focus / eye by mode.
    for (id, side, mode) in [
        (0x605C, 0, 1),
        (0x605D, 1, 1),
        (0x6066, 0, 2),
        (0x6067, 1, 2),
        (0x6069, 0, 3),
        (0x606A, 1, 3),
        (0x606C, 0, 4),
        (0x606D, 1, 4),
        (0x606F, 0, 5),
        (0x6070, 1, 5),
        (0x6072, 0, 6),
        (0x6073, 1, 6),
        (0x6081, 0, 7),
        (0x6082, 1, 7),
        (0x6083, 0, 8),
        (0x6084, 1, 8),
    ] {
        n.register(id, shake_native(side, mode));
    }
    for (id, mode) in [(0x605E, 1), (0x6068, 2), (0x606B, 3), (0x606E, 4), (0x6071, 5), (0x6074, 6)] {
        n.register(id, screen_shake_native(mode));
    }
    fn shake_stop(g: &mut FieldState, c: &mut NativeCall, side: u32) -> Value {
        let h = handle(g, c);
        g.cam.shake_clear(h, side);
        Value::Int(0)
    }
    n.register(0x605F, Native::result(|g, c| shake_stop(g, c, 0)));
    n.register(0x6060, Native::result(|g, c| shake_stop(g, c, 1)));
    n.register(
        0x6061,
        Native::result(|g, c| {
            let s = c.stack.pop_int() as u32;
            g.cam.screen_shake_clear(s);
            Value::Int(0)
        }),
    );
    n.register(
        0x6062,
        Native {
            init: None,
            wait: Some(|g, c| {
                let h = handle(g, c);
                if g.cam.shaking(h, 0) { 0 } else { 5 }
            }),
            result: None,
        },
    );
    n.register(
        0x6063,
        Native {
            init: None,
            wait: Some(|g, c| {
                let h = handle(g, c);
                if g.cam.shaking(h, 1) { 0 } else { 5 }
            }),
            result: None,
        },
    );
    n.register(
        0x6064,
        Native::waiting(
            |_, c| c.scratch[0] = c.stack.pop_int(),
            |g, c| if g.cam.screen_shaking(c.scratch[0] as u32) { 0 } else { 5 },
        ),
    );
    n.register(
        0x6089,
        Native::result(|g, c| {
            let fade = c.stack.pop_int() as u8;
            let pull = c.stack.pop_float();
            let spread = c.stack.pop_int();
            let speed = c.stack.pop_float();
            let frames = c.stack.pop_int();
            let steps = c.stack.pop_int() as u8;
            let mask = c.stack.pop_int() as u32;
            let s = c.stack.pop_int() as u32;
            let (cam, mut world) = g.cam_world(c.worker as i32);
            cam.screen_shake_spring(&mut world, s, mask, 9, steps, frames, speed, spread, pull, fade);
            Value::Int(0)
        }),
    );
    // Priority adjust of the handle's sub-camera (not while the battle camera holds the system).
    n.register(
        0x6065,
        Native::result(|g, c| {
            let h = handle(g, c);
            let v = c.stack.pop_int();
            g.cam.set_priority_adjust(h, v as i8);
            int(h)
        }),
    );
    // Free the character and battle cameras of every record; the screens' kept value back to −1.
    n.register(
        0x6076,
        Native::result(|g, _| {
            g.cam.free_chr_cameras();
            g.cam.screens_reset_d0();
            Value::Int(0)
        }),
    );
    // The shown record's roll (degrees, within half a turn of 0) and screen distance.
    n.register(
        0x6077,
        Native::result(|g, _| {
            let r = g.cam.f32(g.cam.screen_record + 0xc90);
            let r = crate::camera_move::angle_near(0.0, r);
            Value::Float((r as f64 * 180.0 / 3.141_592_741_012_573_2) as f32)
        }),
    );
    n.register(0x6078, Native::result(|g, _| Value::Float(g.cam.f32(g.cam.screen_record + 0xc9c))));
    n.register(
        0x6087,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.reset(h, 0);
            int(h)
        }),
    );
    n.register(
        0x6088,
        Native::result(|g, c| {
            let h = handle(g, c);
            g.cam.reset(h, 1);
            int(h)
        }),
    );
    // A draw from random stream 14 or 15; with magic effect 0x92's coin set, the coin instead (2 for an odd
    // draw, else 0), once.
    n.register(
        0x6080,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let stream = ((v & 1) + 0xe) as usize;
            if g.magic.file != 0x92 || !g.magic.coin {
                return Value::Int(g.rng_streams.next(stream) as i32);
            }
            g.magic.coin = false;
            let r = g.rng_streams.next(stream);
            Value::Int(if r & 1 != 0 { 2 } else { 0 })
        }),
    );
    // Depth-of-field style blur and the record's per-sub-camera values (607e: value or, below 0, its bits
    // with 2; 607f: one of the two side values).
    n.register(
        0x607D,
        Native::result(|g, c| {
            let h = handle(g, c);
            let v = c.stack.pop_int();
            if g.cam.is_active_handle(h) {
                g.screen.blur(v as i16);
            }
            int(h)
        }),
    );
    n.register(
        0x607E,
        Native::result(|g, c| {
            let h = handle(g, c);
            let v = c.stack.pop_int();
            g.cam.set_sub_value(h, v);
            int(h)
        }),
    );
    n.register(
        0x607F,
        Native::result(|g, c| {
            let h = handle(g, c);
            let v = c.stack.pop_int();
            let which = c.stack.pop_int() as u32;
            g.cam.set_sub_side_value(h, which, v as u32);
            int(h)
        }),
    );
}

fn shake_native(side: u32, mode: u8) -> Native<FieldState> {
    macro_rules! s {
        ($side:expr, $mode:expr) => {
            Native::result(|g, c| shake(g, c, $side, $mode))
        };
    }
    match (side, mode) {
        (0, 1) => s!(0, 1),
        (1, 1) => s!(1, 1),
        (0, 2) => s!(0, 2),
        (1, 2) => s!(1, 2),
        (0, 3) => s!(0, 3),
        (1, 3) => s!(1, 3),
        (0, 4) => s!(0, 4),
        (1, 4) => s!(1, 4),
        (0, 5) => s!(0, 5),
        (1, 5) => s!(1, 5),
        (0, 6) => s!(0, 6),
        (1, 6) => s!(1, 6),
        (0, 7) => s!(0, 7),
        (1, 7) => s!(1, 7),
        (0, 8) => s!(0, 8),
        _ => s!(1, 8),
    }
}

fn screen_shake_native(mode: u8) -> Native<FieldState> {
    match mode {
        1 => Native::result(|g, c| screen_shake(g, c, 1)),
        2 => Native::result(|g, c| screen_shake(g, c, 2)),
        3 => Native::result(|g, c| screen_shake(g, c, 3)),
        4 => Native::result(|g, c| screen_shake(g, c, 4)),
        5 => Native::result(|g, c| screen_shake(g, c, 5)),
        _ => Native::result(|g, c| screen_shake(g, c, 6)),
    }
}
