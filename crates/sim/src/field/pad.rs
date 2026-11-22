//! The pad block the event natives read beyond the button words: the analog bytes (sticks, button pressures, the
//! d-pad axes) and the second button word, whose stick directions count as d-pad presses, with its key repeat.
//!
//! Analog bytes: 0 / 1 the right stick, 2 / 3 the left (0x80 at rest, 0x80 + trunc(127 · axis), y down), 4–11
//! the pressures (none on PC: 0), 12 / 13 the d-pad's x / y (0xbf right or down, 0x40 left or up, else 0x80).
//! The second word is the held buttons with a stick far over (byte < 0x10 or > 0xf0) setting its direction in
//! place of the opposite one. Its repeat, per group of buttons: a press counts on the frame it starts (unless a
//! button of the group was let go), then after 12 frames held and every 5 frames after.

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;

use super::FieldState;

/// The button groups the repeat counts by.
const GROUPS: [u32; 4] = [0x5000, 0xa000, 0x00f0, 0x060f];

/// The frames before the first repeat and between repeats.
const FIRST_DELAY: i32 = 12;
const REPEAT_DELAY: i32 = 5;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PadBlock {
    pub analog: [u8; 14],
    /// The second word this frame and last, and its repeat word.
    pub second: u32,
    pub prev_second: u32,
    pub second_repeat: u32,
    /// Frames left per group until the next repeat.
    timers: [i32; 4],
}

/// A stick axis (−1..1) as a byte.
fn axis_byte(v: f32) -> u8 {
    (0x80 + (127.0 * f64::from(v.clamp(-1.0, 1.0))) as i32) as u8
}

impl PadBlock {
    /// One pad refresh: `held` the buttons held, `stick` the left stick (x right, y forward).
    pub fn refresh(&mut self, mut held: u32, stick: [f32; 2]) {
        // Opposite directions both held cancel.
        if held & 0xa000 == 0xa000 {
            held &= !0xa000;
        }
        if held & 0x5000 == 0x5000 {
            held &= !0x5000;
        }
        let (lx, ly) = (axis_byte(stick[0]), axis_byte(-stick[1]));
        self.analog = [0x80, 0x80, lx, ly, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x80];
        if held & 0x2000 != 0 {
            self.analog[12] = 0xbf;
        }
        if held & 0x8000 != 0 {
            self.analog[12] = 0x40;
        }
        if held & 0x1000 != 0 {
            self.analog[13] = 0x40;
        }
        if held & 0x4000 != 0 {
            self.analog[13] = 0xbf;
        }
        let mut second = held & 0xffff;
        if lx < 0x10 {
            second = second & 0xdfff | 0x8000;
        } else if lx >= 0xf1 {
            second = second & 0x5fff | 0x2000;
        }
        if ly > 0xf0 {
            second = second & 0xefff | 0x4000;
        } else if ly < 0x10 {
            second = second & 0xbfff | 0x1000;
        }
        self.prev_second = self.second;
        self.second = second;
        let mut out = 0;
        for (g, &mask) in GROUPS.iter().enumerate() {
            let (cur, prev) = (second & mask, self.prev_second & mask);
            if cur == 0 {
                continue;
            }
            if cur == prev {
                self.timers[g] -= 1;
                if self.timers[g] <= 0 {
                    self.timers[g] = REPEAT_DELAY;
                    out |= cur;
                }
            } else {
                self.timers[g] = FIRST_DELAY;
                if (cur ^ prev) & prev == 0 {
                    out |= cur;
                }
            }
        }
        self.second_repeat = out;
    }
}

pub fn register(n: &mut Natives<FieldState>) {
    // 0121(i): analog byte i (0–13) through the key config's analog binding (the default one keeps i).
    n.register(
        0x0121,
        Native::result(|g, c| {
            let i = c.stack.pop_int().clamp(0, 0xd) as usize;
            Value::Int(i32::from(g.pad.analog[i]))
        }),
    );
    // 0122(i): analog byte i (below 0: byte 0).
    n.register(
        0x0122,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            Value::Int(i32::from(g.pad.analog[if i < 0 { 0 } else { i.min(0xd) as usize }]))
        }),
    );
    // 0251(bit): whether the second word's repeat (key config remap) has the bit.
    n.register(
        0x0251,
        Native::result(|g, c| {
            let bit = c.stack.pop_int() as u32 & 31;
            Value::Int(i32::from(g.pad.second_repeat & 0xffff & (1 << bit) != 0))
        }),
    );
}
