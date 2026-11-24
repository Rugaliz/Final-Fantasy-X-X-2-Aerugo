//! The character records as the original's character engine keeps them: one 0x880-byte record per body, with the
//! original's layout, set up the way the engine's allocation does it.
//!
//! Record (offsets from its start):
//! - +2 in use, +0xc position (+0x1c the previous one), +0x4c/+0x50/+0x54 velocity this frame (+0x50 the rise),
//!   +0x5c/+0x60/+0x64 scale, +0xbc/+0xc0/+0xc4 colour, +0x110, +0x154 speed, +0x158 facing, +0x168 heading of
//!   travel, +0x16c ground height, +0x170 run threshold, +0x174 (×100), +0x17c, +0x182 height mode, +0x183, +0x185
//!   mode, +0x194 / +0x198 flags, +0x19c the body it is attached to, +0x1b4, +0x1b8 −1 or the parent's tag,
//!   +0x1c4 the model block, +0x4d4, +0x4d8, +0x4e4/+0x4ec/+0x4f0/+0x4f4 water and height values, +0x500 push
//!   weight, +0x72c the motion, +0x736/+0x737, +0x73c, +0x740, +0x750, +0x824, +0x828 ground flags.

/// A record's size.
pub const SIZE: u32 = 0x880;
pub const POS: u32 = 0xc;
pub const PREV: u32 = 0x1c;
pub const VEL: u32 = 0x4c;
pub const SCALE: u32 = 0x5c;
pub const SPEED: u32 = 0x154;
pub const FACING: u32 = 0x158;
pub const HEADING: u32 = 0x168;
pub const GROUND: u32 = 0x16c;
pub const RUN_AT: u32 = 0x170;
pub const HEIGHT_MODE: u32 = 0x182;
pub const MODE: u32 = 0x185;
pub const FLAGS: u32 = 0x194;
pub const FLAGS2: u32 = 0x198;
pub const PARENT: u32 = 0x19c;
pub const PUSH_WEIGHT: u32 = 0x500;
pub const MOTION: u32 = 0x72c;

/// The model block values a new record takes defaults from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Block {
    /// +0x24, +0x28, +0x2c: the defaults of the height values set to −1; +0x34 the scale.
    pub h24: f32,
    pub h28: f32,
    pub h2c: f32,
    pub scale: f32,
}

/// The values a block has without an info block (the model block set-up's defaults).
impl Default for Block {
    fn default() -> Self {
        Block { h24: 1.0, h28: 0.0, h2c: 8.0, scale: 1.0 }
    }
}

/// Character engine switches natives set (two flags, a map mode byte).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Globals {
    pub switch_400e: u8,
    pub switch_400f: u8,
    pub mode_403a: u8,
    /// Body 0x66 plays motion 0x21092 (the remaster checks it).
    pub motion_21092: u8,
    /// The second water height (403c; the bodies' step takes the higher of it and the water level), the map
    /// alpha mode (4034) and two more switches (4045, 4046).
    pub water2: f32,
    pub map_alpha: u8,
    pub switch_4045: u8,
    pub switch_4046: u8,
}

/// One character record.
#[derive(Clone, PartialEq)]
pub struct Chr {
    bytes: Vec<u8>,
}

impl std::fmt::Debug for Chr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chr").finish_non_exhaustive()
    }
}

macro_rules! access {
    ($get:ident, $set:ident, $t:ty, $n:expr) => {
        pub fn $get(&self, a: u32) -> $t {
            let o = a as usize;
            <$t>::from_le_bytes(self.bytes[o..o + $n].try_into().unwrap())
        }
        pub fn $set(&mut self, a: u32, v: $t) {
            let o = a as usize;
            self.bytes[o..o + $n].copy_from_slice(&v.to_le_bytes());
        }
    };
}

impl Chr {
    access!(u8, set_u8, u8, 1);
    access!(i8, set_i8, i8, 1);
    access!(u16, set_u16, u16, 2);
    access!(i16, set_i16, i16, 2);
    access!(u32, set_u32, u32, 4);
    access!(i32, set_i32, i32, 4);
    access!(f32, set_f32, f32, 4);

    /// A flag bit of the first flags word set to `on`.
    pub fn set_flag(&mut self, bit: u32, on: bool) {
        let f = self.u32(FLAGS);
        self.set_u32(FLAGS, f & !(1 << bit) | u32::from(on) << bit);
    }

    /// Place the body (its previous place too), as the engine's position set does.
    /// The position the body step starts from (the actor's, which the scripts and the engine bridge move),
    /// without the placement's side effects.
    pub fn set_pos_keep_previous(&mut self, p: [f32; 3]) {
        for k in 0..3 {
            self.set_f32(POS + k * 4, p[k as usize]);
        }
    }

    pub fn set_pos(&mut self, p: [f32; 3]) {
        for k in 0..3 {
            self.set_f32(POS + k * 4, p[k as usize]);
            self.set_f32(PREV + k * 4, p[k as usize]);
        }
        let w = self.u32(POS + 0xc);
        self.set_u32(PREV + 0xc, w);
        self.set_u32(FLAGS, self.u32(FLAGS) | 0x0200_0000);
        self.set_u16(0x824, 0xffff);
    }

    /// The record's bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn pos(&self) -> [f32; 3] {
        [self.f32(POS), self.f32(POS + 4), self.f32(POS + 8)]
    }

    /// A new record for model `id` (the allocation's setter chain, as far as traced).
    pub fn allocate(id: u16, block: Block) -> Chr {
        let mut c = Chr { bytes: vec![0; SIZE as usize] };
        c.set_u16(0, id);
        c.set_u8(2, 1);
        for a in [0x7e4, 0x7e6, 0x190, 0x192, 0x1b8, 0x1ba] {
            c.set_u16(a, 0xffff);
        }
        c.set_pos([0.0; 3]);
        c.set_u8(HEIGHT_MODE, 1);
        c.set_f32(0xbc, 1.0);
        c.set_f32(0xc0, 1.0);
        c.set_f32(0xc4, 1.0);
        c.set_f32(0x334, 1.0);
        c.set_f32(0x340, 1.0);
        c.set_f32(0x34c, 1.0);
        c.set_u16(0x750, 0x100);
        c.set_f32(0x174, (50.0f64 * 100.0) as f32);
        c.set_f32(PUSH_WEIGHT, 1.0);
        for k in 0..3 {
            c.set_f32(SCALE + k * 4, block.scale);
        }
        c.set_f32(0x4f4, block.h2c);
        c.set_f32(0x4f0, block.h28);
        c.set_f32(0x4e4, -1.0);
        c.set_f32(0x4ec, block.h24);
        c.set_f32(0x17c, f32::from_bits(0x3e4c_cccd));
        c.set_flag(20, true);
        c.set_i16(0x73c, 1);
        c.set_flag(11, true);
        c.set_f32(RUN_AT, -1.0);
        c.set_flag(10, true);
        c.set_flag(18, true);
        c.set_u32(0x828, 0xfffe_0000);
        c.set_flag(19, true);
        c.set_f32(0x1b4, 1.0);
        c.set_flag(30, true);
        c.set_flag(22, true);
        c.set_f32(0x420, 0.0);
        let f2 = c.u32(FLAGS2);
        c.set_u32(FLAGS2, f2 | 0x100);
        c.set_f32(0x110, 1.0);
        c.set_u32(0x408, 1);
        c.set_u8(
            0x183,
            match id >> 12 {
                0..=3 => 1,
                0xf => 3,
                _ => 0,
            },
        );
        if id >> 12 == 0xf {
            c.set_u32(FLAGS, c.u32(FLAGS) | 0x40);
        }
        c.set_f32(0x4d8, f32::from_bits(0x449a_4000));
        c
    }
}

pub mod elements;

/// The character engine's π (3.141592 as a float).
pub const BODY_PI: f32 = f32::from_bits(0x4049_0fd8);

/// The character engine's own angle wrap (into ±3.141592 by steps of 6.283184, each step stored as a float).
pub fn wrap(mut x: f32) -> f32 {
    let (pi, tau) = (f64::from(BODY_PI), f64::from(f32::from_bits(0x40c9_0fd8)));
    while pi < f64::from(x) {
        x = (f64::from(x) - tau) as f32;
    }
    while f64::from(x) < -pi {
        x = (f64::from(x) + tau) as f32;
    }
    x
}
