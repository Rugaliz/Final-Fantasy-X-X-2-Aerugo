//! The event's 2D shapes (the credits): 128 slots, each showing an animated shape from data in the script's own
//! memory. 011a turns the shapes on (and names the event's texture list), 011b puts shape data in a slot, 011c
//! takes a slot off, 011d sets its alpha, 011e its brightness, 011f its position, 0120 its scale (×1/256); 023f
//! switches the drawing. Slot numbers are clamped to 0–127.
//!
//! Shape data: an i16 frame count at +4, then 8-byte frames from +0x10 (i16 shape offset, i16 duration in 1/256
//! steps, a flags byte: 0x80 loops). The step runs twice a frame before the scripts: a slot's clock gains 0x100 a
//! step until it reaches the frame's duration, then the next frame starts; past the last frame the slot starts
//! over, and turns off unless that last frame loops.

use aerugo_formats::script::{Var, VarType};
use aerugo_script::memory::{Memory, Place};
use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::{NativeCall, Value};

use super::FieldState;

pub const SLOTS: usize = 128;

/// One shape slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapeSlot {
    /// Shown and stepping.
    pub on: bool,
    /// Brightness (the colour's rgb times this / 255).
    pub bright: u8,
    pub byte2: u8,
    pub byte3: u8,
    pub x: i16,
    pub y: i16,
    /// Scale ×1/256 on x and y.
    pub scale: [u16; 2],
    pub frame: u16,
    /// r, g, b, alpha.
    pub color: [u8; 4],
    /// Where the shape data is.
    pub data: Option<Place>,
    /// The current frame's clock (0x100 a step).
    pub clock: i32,
}

impl Default for ShapeSlot {
    /// A slot as the event start leaves it.
    fn default() -> Self {
        ShapeSlot {
            on: false,
            bright: 0x80,
            byte2: 0,
            byte3: 0,
            x: 0,
            y: 0,
            scale: [0x100, 0x100],
            frame: 0,
            color: [0x80; 4],
            data: None,
            clock: 0,
        }
    }
}

/// The shape slots and the switches around them.
#[derive(Debug, Clone, PartialEq)]
pub struct Shapes {
    pub slots: [ShapeSlot; SLOTS],
    /// 011a's first table (shapes step and draw only once it is set).
    pub base: Option<Place>,
    /// 011a's count less 0xa0, the offset from `base` of a second table.
    pub base_plus: i32,
    /// 011a's texture list table: once set, the first 011b loads the event's texture list.
    pub tex_table: Option<Place>,
    /// The event's texture list is loaded.
    pub tex_loaded: bool,
    /// The drawing switch (023f; on at the event start).
    pub drawn: i32,
}

impl Default for Shapes {
    fn default() -> Self {
        Shapes {
            slots: [ShapeSlot::default(); SLOTS],
            base: None,
            base_plus: -0xa0,
            tex_table: None,
            tex_loaded: false,
            drawn: 1,
        }
    }
}

fn bytes(mem: &Memory, p: Place, off: usize, n: usize) -> Option<&[u8]> {
    match p {
        Place::Global(b, o) => mem.global[b].get(o + off..o + off + n),
        Place::Script(o) => mem.script.get(o + off..o + off + n),
        Place::Registers(w, o) => mem.registers.get(w)?.get(o + off..o + off + n),
    }
}

fn i16_at(mem: &Memory, p: Option<Place>, off: usize) -> i16 {
    p.and_then(|p| bytes(mem, p, off, 2)).map_or(0, |b| i16::from_le_bytes([b[0], b[1]]))
}

fn u8_at(mem: &Memory, p: Option<Place>, off: usize) -> u8 {
    p.and_then(|p| bytes(mem, p, off, 1)).map_or(0, |b| b[0])
}

impl Shapes {
    /// One step of every slot that is on (nothing before 011a or while the drawing is off).
    pub fn step(&mut self, mem: &Memory) {
        if self.base.is_none() || self.drawn == 0 {
            return;
        }
        for s in self.slots.iter_mut().filter(|s| s.on) {
            let frame = usize::from(s.frame);
            if s.clock + 0x100 < i32::from(i16_at(mem, s.data, 0x12 + frame * 8)) {
                s.clock += 0x100;
                continue;
            }
            let mut next = frame as i32 + 1;
            if i32::from(i16_at(mem, s.data, 4)) <= next {
                next = 0;
                if u8_at(mem, s.data, frame * 8 + 0x14) & 0x80 == 0 {
                    s.on = false;
                }
            }
            s.frame = next as u16;
            s.clock = 0;
        }
    }

    fn slot(&mut self, n: i32) -> &mut ShapeSlot {
        &mut self.slots[n.clamp(0, SLOTS as i32 - 1) as usize]
    }
}

/// Where a variable descriptor (type bits 28–31, area 25–27, offset 0–23) points for the calling worker.
pub(super) fn var_place(g: &FieldState, c: &NativeCall, desc: i32) -> Place {
    let d = desc as u32;
    let var = Var {
        ty: VarType::from_nibble((d >> 28) as u8),
        area: ((d >> 25) & 7) as u8,
        offset: d & 0xFF_FFFF,
        count: 1,
    };
    let areas = g.data_areas.get(c.worker).copied().unwrap_or_default();
    c.memory.place(&var, 0, c.worker, areas, g.shared_data)
}

/// A float truncated as the original converts it (out of range: 0), kept to 16 bits.
fn ftol16(f: f32) -> u16 {
    let v = f64::from(f).trunc();
    if v.is_finite() && v.abs() < 9.223_372_036_854_776e18 { v as i64 as u16 } else { 0 }
}

pub fn register(n: &mut Natives<FieldState>) {
    // 011a(table, count, texture table): the shapes on; returns 1.
    n.register(
        0x011A,
        Native::result(|g, c| {
            let b = c.stack.pop_int();
            let count = c.stack.pop_int();
            let a = c.stack.pop_int();
            let tex = var_place(g, c, b);
            let base = var_place(g, c, a);
            let s = &mut g.shapes;
            s.base_plus = count.wrapping_sub(0xa0);
            s.base = Some(base);
            s.tex_table = Some(tex);
            s.drawn = 1;
            s.tex_loaded = true;
            Value::Int(1)
        }),
    );
    // 011b(slot, data): shape data in a slot, on at full brightness and the default colour, from its first frame.
    n.register(
        0x011B,
        Native::result(|g, c| {
            let desc = c.stack.pop_int();
            let n = c.stack.pop_int();
            let data = var_place(g, c, desc);
            let s = &mut g.shapes;
            let slot = s.slot(n);
            slot.data = Some(data);
            slot.on = true;
            slot.bright = 0xff;
            slot.color = [0x80; 4];
            slot.byte3 = 1;
            slot.clock = 0;
            slot.frame = 0;
            if s.tex_table.is_some() && !s.tex_loaded {
                s.tex_loaded = true;
            }
            Value::Int(n)
        }),
    );
    n.register(
        0x011C,
        Native::result(|g, c| {
            let n = c.stack.pop_int();
            g.shapes.slot(n).on = false;
            Value::Int(n)
        }),
    );
    n.register(
        0x011D,
        Native::result(|g, c| {
            let a = c.stack.pop_int();
            let n = c.stack.pop_int();
            g.shapes.slot(n).color[3] = a as u8;
            Value::Int(n)
        }),
    );
    n.register(
        0x011E,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let n = c.stack.pop_int();
            g.shapes.slot(n).bright = v.clamp(0, 0xff) as u8;
            Value::Int(n)
        }),
    );
    // 011f(slot, x, y). (The original moves (0x10e, 0xd5) to (0x122, 0xd5) in one language on one map, not
    // checked here.)
    n.register(
        0x011F,
        Native::result(|g, c| {
            let y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let n = c.stack.pop_int();
            let slot = g.shapes.slot(n);
            slot.x = x as i16;
            slot.y = y as i16;
            Value::Int(n)
        }),
    );
    n.register(
        0x0120,
        Native::result(|g, c| {
            let sy = c.stack.pop_float();
            let sx = c.stack.pop_float();
            let n = c.stack.pop_int();
            g.shapes.slot(n).scale = [ftol16(sx), ftol16(sy)];
            Value::Int(n)
        }),
    );
    // The drawing switch; returns the value.
    n.register(
        0x023F,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.shapes.drawn = v;
            Value::Int(v)
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_steps_through_frames_and_stops_without_loop() {
        // Two frames of 0x200 each, the last without the loop flag.
        let mut script = vec![0u8; 0x40];
        script[4..6].copy_from_slice(&2i16.to_le_bytes());
        for f in 0..2 {
            script[0x12 + f * 8..0x14 + f * 8].copy_from_slice(&0x200i16.to_le_bytes());
        }
        let mem = Memory::new(script, 0);
        let mut s = Shapes { base: Some(Place::Script(0)), ..Shapes::default() };
        s.slots[0] = ShapeSlot { on: true, data: Some(Place::Script(0)), ..ShapeSlot::default() };
        s.step(&mem);
        assert_eq!((s.slots[0].frame, s.slots[0].clock), (0, 0x100));
        s.step(&mem);
        assert_eq!((s.slots[0].frame, s.slots[0].clock), (1, 0));
        s.step(&mem);
        s.step(&mem);
        assert!(!s.slots[0].on);
        assert_eq!(s.slots[0].frame, 0);
    }

    #[test]
    fn ftol16_truncates() {
        assert_eq!(ftol16(128.9), 128);
        assert_eq!(ftol16(-1.5), 0xffff);
        assert_eq!(ftol16(f32::NAN), 0);
    }
}
