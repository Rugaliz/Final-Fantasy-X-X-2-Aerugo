//! The fog natives over the two fog states the original keeps: the PS2 fog block (colour bytes, limit, near,
//! far; the background colour bytes; a distance scale that every field frame sets to 10.0; a colour transform of
//! multipliers and offsets applied while the display flags' bit 2 is set) and the remaster's scene settings (the map cluster's `GlobalCamera`: the getters read
//! its fog colour, near, far, limit; +0x50 is read with the transform on; the background colour is written).
//!
//! The PS2 block is filled from the map object data's chunk 4 at map load (body +0 background bytes, +0xc colour
//! bytes, +0x10 limit, +0x14 near, +0x18 far). The remaster's fog setters (near, far, limit, curve, colour) are
//! what Aerugo's renderer takes (`FogOverride`).

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::{NativeCall, Value};

use super::FieldState;

/// The PS2 fog block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsFog {
    pub rgba: [u8; 4],
    pub limit: f32,
    pub near: f32,
    pub far: f32,
    pub back: [u8; 4],
    /// The distance scale (32.0 at boot; each field frame sets 10.0).
    pub scale: f32,
}

impl Default for PsFog {
    fn default() -> Self {
        PsFog { rgba: [0; 4], limit: 0.0, near: 0.0, far: 0.0, back: [0; 4], scale: 10.0 }
    }
}

impl PsFog {
    /// From a `mapout.vpa`: the map object data (MAP1 +0x14, magic 0x214365), chunk 4.
    pub fn from_map_package(vpa: &[u8]) -> Option<PsFog> {
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
                let b = |o: usize| -> Option<[u8; 4]> {
                    vpa.get(d + o..d + o + 4).map(|x| [x[0], x[1], x[2], x[3]])
                };
                return Some(PsFog {
                    back: b(0)?,
                    rgba: b(0xc)?,
                    limit: f32_at(d + 0x10)?,
                    near: f32_at(d + 0x14)?,
                    far: f32_at(d + 0x18)?,
                    ..PsFog::default()
                });
            }
            if size == 0 {
                return None;
            }
            at += 0x40 + size;
        }
        None
    }

    /// The distance factor (scale · 0.03125 · 32).
    fn factor(&self) -> f64 {
        f64::from(self.scale) * 0.03125 * 32.0
    }

    /// Near, far and limit (near and far divided by the factor).
    fn set(&mut self, near: f32, far: f32, limit: f32) {
        let k = self.factor() as f32;
        self.far = (f64::from(far) / f64::from(k)) as f32;
        self.near = (f64::from(near) / f64::from(k)) as f32;
        self.limit = limit;
    }
}

/// The scene settings values the fog natives read (the remaster's scene settings object).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HdScene {
    /// What the fog getters take for the fog colour while the colour transform is on: the scene settings'
    /// +0x50..+0x58, which fall one component into the ambient colour (its green, blue and w).
    pub transformed_fog_colour: [f32; 3],
    /// +0x60 fog colour, +0x70 near, +0x74 far, +0x7c limit, +0x80 background.
    pub fog_colour: [f32; 3],
    pub near: f32,
    pub far: f32,
    pub limit: f32,
    pub back: [f32; 3],
}

impl HdScene {
    pub fn from_settings(s: &aerugo_formats::mesh::SceneSettings) -> HdScene {
        HdScene {
            transformed_fog_colour: [s.ambient[1], s.ambient[2], s.ambient_w],
            fog_colour: s.fog_color,
            near: s.fog_near,
            far: s.fog_far,
            limit: s.fog_limit,
            back: s.background,
        }
    }
}

/// x87 __ftol2 of a value (out of range: 0).
fn ftol(x: f64) -> i32 {
    if x.is_finite() && x.abs() < 9.2e18 { (x.trunc() as i64) as i32 } else { 0 }
}

/// (near, far, limit) — the PS2 block's, or the remaster's on event 0xe0.
fn values(g: &FieldState) -> (f32, f32, f32) {
    let f = &g.ps_fog;
    if g.scene == Some(0xe0) {
        let h = g.hd_scene.unwrap_or_default();
        return (h.near, h.far, (f64::from(h.limit) * 255.0) as f32);
    }
    let k = f.factor();
    ((k * f64::from(f.near)) as f32, (f64::from(f.far) * k) as f32, f.limit)
}

/// The fog colour as the getters see it: alpha from the block (transformed with the display flags' bit 2), the
/// colour from the remaster's (+0x50 with the transform, else its fog colour) × 255.
fn colour(g: &FieldState) -> [i32; 4] {
    let f = &g.ps_fog;
    let (mul, add) = (g.globals.colour_mul, g.globals.colour_add);
    let transform = g.map_display & 4 != 0;
    let a = if transform {
        ftol(f64::from(
            (f64::from(add[3]) + f64::from(mul[3]) * f64::from(f.rgba[3])).clamp(0.0, 255.0) as f32
        ))
    } else {
        i32::from(f.rgba[3])
    };
    let h = g.hd_scene.unwrap_or_default();
    let c = if transform { h.transformed_fog_colour } else { h.fog_colour };
    [ftol(f64::from(c[0]) * 255.0), ftol(f64::from(c[1]) * 255.0), ftol(f64::from(c[2]) * 255.0), a]
}

/// The background colour bytes (transformed with the display flags' bits 0 and 2).
fn back(g: &FieldState) -> [i32; 4] {
    let f = &g.ps_fog;
    let (mul, add) = (g.globals.colour_mul, g.globals.colour_add);
    if g.map_display & 1 == 0 || g.map_display & 4 == 0 {
        return f.back.map(i32::from);
    }
    std::array::from_fn(|i| {
        let v = (f64::from(mul[i]) * f64::from(f.back[i]) + f64::from(add[i])) as f32;
        ftol(f64::from(v.clamp(0.0, 255.0)))
    })
}

/// The PS2 block and, on event 0x140, the remaster's colour setter with the 0–255 values: the fog colour (alpha 0 from the natives).
fn set_colour(g: &mut FieldState, c: [i32; 4]) {
    g.ps_fog.rgba = c.map(|v| v as u8);
    if g.scene == Some(0xe0) {
        g.fog.colour = Some([c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0]);
    }
    if g.scene == Some(0x140) {
        g.fog.colour = Some([c[0] as f32, c[1] as f32, c[2] as f32]);
    }
}

/// The three maps (by the remaster's map id) where 8038/803a/803c change nothing.
fn skipped(g: &FieldState) -> bool {
    matches!(g.map_id, 0xca2 | 0x1e38 | 0x699)
}

pub fn register(n: &mut Natives<FieldState>) {
    // 8037(near, far, limit): the PS2 block (on event 0xe0 also the remaster's near ×0.001, far ×0.001,
    // limit (1 − l/255)·0.4, curve 1); on event 0x140 the remaster's near, far, limit / 255 and curve 1. Returns 1.
    n.register(
        0x8037,
        Native::result(|g, c| {
            let limit = c.stack.pop_float();
            let far = c.stack.pop_float();
            let near = c.stack.pop_float();
            ps_set(g, near, far, limit);
            if g.scene == Some(0x140) {
                g.fog.near = Some(near);
                g.fog.far = Some(far);
                g.fog.limit = (f64::from(limit) / 255.0) as f32;
                g.fog.curve = Some(1.0);
            }
            Value::Int(1)
        }),
    );
    // 8038(near) / 803a(far) / 803c(limit): the PS2 block with the other two as they read, and the
    // remaster's value (limit / 255) — except on three maps. Return 1.
    n.register(
        0x8038,
        Native::result(|g, c| {
            let (_, far, limit) = values(g);
            let v = c.stack.pop_float();
            if !skipped(g) {
                ps_set(g, v, far, limit);
                g.fog.near = Some(v);
            }
            Value::Int(1)
        }),
    );
    n.register(
        0x803A,
        Native::result(|g, c| {
            let (near, _, limit) = values(g);
            let v = c.stack.pop_float();
            if !skipped(g) {
                ps_set(g, near, v, limit);
                g.fog.far = Some(v);
            }
            Value::Int(1)
        }),
    );
    n.register(
        0x803C,
        Native::result(|g, c| {
            let (near, far, _) = values(g);
            let v = c.stack.pop_float();
            if !skipped(g) {
                ps_set(g, near, far, v);
                g.fog.limit_raw = v;
                g.fog.limit = (f64::from(v) / 255.0) as f32;
            }
            Value::Int(1)
        }),
    );
    // 8039 / 803b / 803d: near, far, limit as the block gives them.
    n.register(0x8039, Native::result(|g, _| Value::Float(values(g).0)));
    n.register(0x803B, Native::result(|g, _| Value::Float(values(g).1)));
    n.register(0x803D, Native::result(|g, _| Value::Float(values(g).2)));
    // 803e(r, g, b): the fog colour (alpha 0); 803f(r) / 8040(g) / 8041(b): one channel over the colour as the block
    // holds it. Their result is undefined in the original (the frame's cookie check); 0 here.
    n.register(
        0x803E,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let gr = c.stack.pop_int();
            let r = c.stack.pop_int();
            set_colour(g, [r, gr, b, 0]);
            Value::Int(0)
        }),
    );
    for (id, k) in [(0x803Fu16, 0usize), (0x8040, 1), (0x8041, 2)] {
        n.register(
            id,
            Native::result(match k {
                0 => |g: &mut FieldState, c: &mut NativeCall<'_>| one_channel(g, c, 0),
                1 => |g: &mut FieldState, c: &mut NativeCall<'_>| one_channel(g, c, 1),
                _ => |g: &mut FieldState, c: &mut NativeCall<'_>| one_channel(g, c, 2),
            }),
        );
    }
    // 8042 / 8043 / 8044: the fog colour's r / g / b.
    n.register(0x8042, Native::result(|g, _| Value::Int(colour(g)[0])));
    n.register(0x8043, Native::result(|g, _| Value::Int(colour(g)[1])));
    n.register(0x8044, Native::result(|g, _| Value::Int(colour(g)[2])));
    // 8045(r, g, b): the background colour (the bytes, alpha 0; the remaster's background r/255, g/255,
    // b/255 unless magic 0x2c2 is loaded). 8049 / 804a / 804b: its r / g / b as the block holds them.
    n.register(
        0x8045,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let gr = c.stack.pop_int();
            let r = c.stack.pop_int();
            g.ps_fog.back = [r as u8, gr as u8, b as u8, 0];
            if g.magic.file != 0x2c2 {
                let h = g.hd_scene.get_or_insert_with(HdScene::default);
                h.back = [r as f32 / 255.0, gr as f32 / 255.0, b as f32 / 255.0];
                g.background = Some(h.back);
            }
            Value::Int(0)
        }),
    );
    n.register(0x8049, Native::result(|g, _| Value::Int(back(g)[0])));
    n.register(0x804A, Native::result(|g, _| Value::Int(back(g)[1])));
    n.register(0x804B, Native::result(|g, _| Value::Int(back(g)[2])));
}

fn ps_set(g: &mut FieldState, near: f32, far: f32, limit: f32) {
    g.ps_fog.set(near, far, limit);
    if g.scene == Some(0xe0) {
        let f = g.ps_fog;
        g.fog.near = Some((f64::from(f.near) * 0.001) as f32);
        g.fog.far = Some((f64::from(f.far) * 0.001) as f32);
        g.fog.limit = ((1.0 - f64::from(limit) / 255.0) * 0.4) as f32;
        g.fog.curve = Some(1.0);
    }
}

fn one_channel(g: &mut FieldState, c: &mut NativeCall<'_>, k: usize) -> Value {
    let mut col = colour(g);
    col[k] = c.stack.pop_int();
    set_colour(g, col);
    Value::Int(0)
}
