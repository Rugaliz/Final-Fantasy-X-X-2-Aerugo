//! The pad vibration channels as the event system keeps them: eight channels of two streams (one per motor), each
//! stepping through level segments, and the motor levels the strongest channels give each frame.
//!
//! Channel (0x24 bytes): two streams of 0x10 (+0 the data, +4 position, +6 end, +8 from level, +9 to level, +0xa
//! the level now), +0x20 a counter, +0x22 the state (0 idle, 1 playing data, 2 a single ramp). A data stream is a
//! list of (level, hold) byte pairs: each segment ramps from its level to the next one's over (hold + 1) · 2
//! frames; a hold of 0xff (an end of 0x200) ends it. The data comes from the file table's set 0x18, file 8: per
//! entry a u32 offset, the entry starting with the second motor's data length and a flag, then both streams.

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;

use super::FieldState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stream {
    /// Where the next segment starts in the data file (none for a ramp).
    pub data: Option<usize>,
    pub pos: i16,
    pub end: i16,
    pub from: u8,
    pub to: u8,
    pub level: u8,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Channel {
    pub streams: [Stream; 2],
    pub counter: u16,
    pub state: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vibration {
    pub channels: [Channel; 8],
    /// The stop switch (stops the step) and the channel mask.
    pub off: u8,
    pub mask: u32,
    /// The motor levels this frame.
    pub levels: [u8; 2],
    /// The vibration data file.
    pub data: Option<std::sync::Arc<[u8]>>,
}

impl Default for Vibration {
    fn default() -> Self {
        Vibration { channels: [Channel::default(); 8], off: 0, mask: 0xff, levels: [0; 2], data: None }
    }
}

fn ch(i: i32) -> usize {
    i.clamp(0, 7) as usize
}

impl Vibration {
    /// A single ramp on one motor of channel `c` from level `a` (−1: where it is) to `b` over `frames`
    /// (−1: hold `a`).
    pub fn ramp(&mut self, c: i32, slot: i32, a: i32, b: i32, frames: i32) {
        let c = &mut self.channels[ch(c)];
        c.counter = c.counter.wrapping_add(1);
        c.state = 0;
        let s = &mut c.streams[slot.clamp(0, 1) as usize];
        s.data = None;
        c.state = 2;
        s.pos = 0;
        s.end = -2;
        s.from = 0;
        s.to = 0;
        let a = if a < 0 { i32::from(s.level) } else { a }.clamp(0, 0x7f);
        let b = b.max(0).clamp(0, 0x7f);
        let frames = if frames < 0 { -1 } else { frames };
        s.pos = 0;
        s.end = frames as i16;
        s.from = a as u8;
        s.to = if (frames as i16) < 0 { a as u8 } else { b as u8 };
    }

    /// Channel `c` stops (both streams cleared).
    pub fn stop(&mut self, c: i32) {
        let c = &mut self.channels[ch(c)];
        c.state = 0;
        for s in &mut c.streams {
            s.data = None;
            s.pos = 0;
            s.end = 0;
            s.from = 0;
            s.to = 0;
            s.level = 0;
        }
    }

    /// A segment from the data at `at`: level, hold, and the next level.
    fn segment(&self, s: &mut Stream, at: usize) {
        let d = self.data.as_deref().unwrap_or(&[]);
        let b = |i: usize| d.get(at + i).copied().unwrap_or(0);
        s.pos = 0;
        s.end = ((u16::from(b(1)) + 1) * 2) as i16;
        s.from = b(0);
        s.to = b(2);
        s.data = Some(at + 2);
    }

    /// Channel `c` plays data entry `n`.
    pub fn play(&mut self, c: i32, n: u32) {
        let ci = ch(c);
        {
            let c = &mut self.channels[ci];
            c.counter = c.counter.wrapping_add(1);
            c.state = 0;
        }
        let d = self.data.clone();
        let d = d.as_deref().unwrap_or(&[]);
        let u16_at = |a: usize| d.get(a..a + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
        let entry = (n < 0x40)
            .then(|| d.get(n as usize * 4..n as usize * 4 + 4))
            .flatten()
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .filter(|&o| o != 0);
        let second = entry.filter(|&o| u16_at(o) != 0).map(|o| o + 4);
        let first = entry.filter(|&o| u16_at(o + 2) != 0).map(|o| o + 4 + usize::from(u16_at(o)));
        let mut streams = self.channels[ci].streams;
        for (s, at) in streams.iter_mut().zip([first, second]) {
            match at {
                None => {
                    s.pos = 0;
                    s.end = 0x200;
                    s.from = 0;
                    s.to = 0;
                    s.data = None;
                }
                Some(at) => self.segment(s, at),
            }
        }
        let c = &mut self.channels[ci];
        c.streams = streams;
        c.state = 1;
    }

    /// One frame of a stream; its level.
    fn stream_level(&self, state: u8, s: &mut Stream) -> u8 {
        let end = i32::from(s.end);
        if end < 0 {
            return s.from;
        }
        s.pos = s.pos.wrapping_add(1);
        if end <= i32::from(s.pos) {
            if state == 1 {
                if let Some(at) = s.data {
                    self.segment(s, at);
                }
            } else if state == 2 {
                s.from = s.to;
                s.end = -2;
            }
        }
        if s.from == s.to || s.end < 1 {
            return s.from;
        }
        let (e, p) = (i32::from(s.end), i32::from(s.pos));
        (((e - p) * i32::from(s.from) + i32::from(s.to) * p) / e) as u8
    }

    fn level(&self, state: u8, s: &mut Stream) -> u8 {
        let end = i32::from(s.end);
        let l = if !(1..=2).contains(&state) {
            0
        } else if end < 0 {
            s.from
        } else if state == 1 && end > 0x1ff {
            0
        } else {
            self.stream_level(state, s)
        };
        s.level = l;
        l
    }

    /// Once a frame in the event step: finished channels go idle, the streams step, the strongest levels
    /// drive the motors.
    pub fn frame(&mut self) {
        if self.off != 0 {
            return;
        }
        let (mut a, mut b) = (0u8, 0u8);
        for i in 0..8 {
            if self.mask & (1 << i) == 0 {
                continue;
            }
            let c = self.channels[i];
            let ends = (c.streams[0].end, c.streams[1].end);
            if (c.state == 1 && ends == (0x200, 0x200)) || (c.state == 2 && ends == (-2, -2)) {
                self.channels[i].state = 0;
            }
            let state = self.channels[i].state;
            let mut s = self.channels[i].streams;
            let l0 = self.level(state, &mut s[0]);
            let l1 = self.level(state, &mut s[1]);
            self.channels[i].streams = s;
            a = a.max(l0);
            b = b.max(l1);
        }
        self.levels = [a, b];
    }
}

pub fn register(n: &mut Natives<FieldState>) {
    // 01da(slot, a, b, frames): a ramp on channel 3; returns 1.
    n.register(
        0x01DA,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            let b = c.stack.pop_int();
            let a = c.stack.pop_int();
            let slot = c.stack.pop_int();
            g.vibration.ramp(3, slot, a, b, frames);
            Value::Int(1)
        }),
    );
    // 01dc / 01ee: channel 3 / 1 stops; returns the channel.
    n.register(
        0x01DC,
        Native::result(|g, _| {
            g.vibration.stop(3);
            Value::Int(3)
        }),
    );
    n.register(
        0x01EE,
        Native::result(|g, _| {
            g.vibration.stop(1);
            Value::Int(1)
        }),
    );
    // 01de: wait while channel 3 plays.
    n.register(
        0x01DE,
        Native {
            init: Some(|_, c| c.scratch[0] = 3),
            wait: Some(|g, c| if g.vibration.channels[ch(c.scratch[0])].state == 0 { 5 } else { 0 }),
            result: None,
        },
    );
    // 01eb(n): channel 1 plays data entry n; returns 1.
    n.register(
        0x01EB,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            g.vibration.play(1, v as u32);
            Value::Int(1)
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_ends_and_goes_idle() {
        let mut v = Vibration::default();
        v.ramp(3, 0, 0, 100, 4);
        v.ramp(3, 1, 0, 0, 4);
        let mut seen = Vec::new();
        for _ in 0..6 {
            v.frame();
            seen.push(v.channels[3].streams[0].level);
        }
        assert_eq!(seen[..4], [25, 50, 75, 100]);
        assert_eq!(v.channels[3].state, 0);
    }
}
