//! The event camera system as the original keeps it: a layout-exact island. The camera globals — three camera
//! records, their two sides (focus and eye) with per-sub-camera filters and move channels, the roll and screen
//! distance, the sub-camera handles, the recorded path buffers, the handle tables and the screen parameters — live
//! in one byte region with the original's layout, and each camera function is a translation of the original's
//! code on that region. The layout matters: sides keep the address of the other side's target, filters are
//! copied whole between sub-cameras, and some writes reach across a record's parts.
//!
//! Positions are byte offsets from the island's start; an address kept inside the island is such an offset.
//!
//! Record r (0xda0 bytes at +0x9e0 + r·0xda0):
//! - +0x30 + side·0x61c: a side. +0 index, +1 moves by angles, +2 changed this frame, +3 acceleration-limited,
//!   +4/+5 heading/pitch wrap, +0x10 view point, +0x20 output, +0x30 the other side's target (copied), +0x40
//!   accumulated move, +0x50 target, +0x60 heading/pitch/distance, +0x6c height add, +0x70 move start, +0x80
//!   polar move start, +0x8c the other side's target (an address), +0x90 shake offset, +0xc0 + sub·0x130 the
//!   sub-camera's filter, +0x580/+0x590/+0x5a0 move channels, +0x5b0 shake channels.
//! - +0xc68 + sub·8: sub-camera handle (u32), +4 suspended, +5 held, +6 priority adjust.
//! - +0xc89 processed this frame, +0xc8a view scale on, +0xc8b roll wraps, +0xc8c active sub (0xff none), +0xc90
//!   roll output, +0xc94 roll, +0xc98 roll move start, +0xc9c screen distance output, +0xca0 screen distance,
//!   +0xca4 its move start, +0xcb8 + sub·0x30 per-sub roll target (+0) and screen distance target (+8), +0xd78
//!   roll channel, +0xd88 screen distance channel.

pub mod btl;
pub mod handles;
pub mod natives;
pub mod polar;
pub mod process;
pub mod rail;
pub mod screen;
pub mod set;
pub mod spline;

pub use process::CamEnv;

/// The island's start and its end (one past the last byte).
pub const BASE: u32 = 0;
pub const END: u32 = 0x3300;
/// Camera records.
pub const RECORDS: u32 = 0x9e0;
pub const RECORD: u32 = 0xda0;
/// A record's first side.
pub const SIDES: u32 = 0xa10;
pub const SIDE: u32 = 0x61c;
pub const FILTER: u32 = 0x130;
/// Recorded path buffers (counts, then 33 vec4 per side: header then points).
pub const PATH_COUNTS: u32 = 0;
pub const PATH_BUFFERS: u32 = 8;
/// Handle tables of the worker kinds (3 cameras each): kind 1, 3, 5, 7.
pub const HANDLES_K1: u32 = 0x428;
pub const HANDLES_K3: u32 = 0x434;
pub const HANDLES_K5: u32 = 0x440;
pub const HANDLES_K7: u32 = 0x44c;
/// Screen slots (3 × 0x1c0): projection values from +0, +0x38/+0x3c the PS2 screen size, +0xc0 on, +0xc1 the
/// camera record shown, +0xd0 −1.
pub const SCREENS: u32 = 0x4a0;
pub const SCREEN: u32 = 0x1c0;
/// The screen distance a battle override sets, and the battle's two override targets.
pub const BATTLE_DISTANCE: u32 = 0x32c0;
pub const BATTLE_TARGETS: u32 = 0x32c4;
/// Ids of script tables (paths, eye tables) a filter names: the tag plus the table's place in its script.
pub const TABLE: u32 = 0x8000_0000;

/// The camera island's memory.
#[derive(Clone, PartialEq)]
pub struct CamMem {
    bytes: Vec<u8>,
    /// Script tables the filters name (paths, eye tables), by the id kept at filter +8: copies of the script's
    /// static data, which the eye-table filter writes its weights into as the original does.
    pub tables: std::collections::BTreeMap<u32, Vec<u8>>,
    /// The fourth lane of the emulated VU result register as the camera's own operations last left it (the
    /// look-ahead path step reads it back as a roll).
    pub vu_w: f32,
    /// The rail search's place and the measured path length (globals every rail shares).
    pub rail_place: f32,
    pub rail_len: f32,
    /// The record the last stepped screen slot shows (record 0 at the start).
    pub screen_record: u32,
}

impl std::fmt::Debug for CamMem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CamMem").finish_non_exhaustive()
    }
}

impl Default for CamMem {
    fn default() -> Self {
        let mut m = CamMem {
            bytes: vec![0; (END - BASE) as usize],
            tables: Default::default(),
            vu_w: 0.0,
            rail_place: 0.0,
            rail_len: 0.0,
            screen_record: RECORDS,
        };
        m.init();
        m
    }
}

macro_rules! access {
    ($get:ident, $set:ident, $t:ty, $n:expr) => {
        pub fn $get(&self, a: u32) -> $t {
            let o = (a - BASE) as usize;
            <$t>::from_le_bytes(self.bytes[o..o + $n].try_into().unwrap())
        }
        pub fn $set(&mut self, a: u32, v: $t) {
            let o = (a - BASE) as usize;
            self.bytes[o..o + $n].copy_from_slice(&v.to_le_bytes());
        }
    };
}

impl CamMem {
    access!(u8, set_u8, u8, 1);
    access!(i8, set_i8, i8, 1);
    access!(u16, set_u16, u16, 2);
    access!(i16, set_i16, i16, 2);
    access!(u32, set_u32, u32, 4);
    access!(i32, set_i32, i32, 4);
    access!(f32, set_f32, f32, 4);

    pub fn vec4(&self, a: u32) -> [f32; 4] {
        std::array::from_fn(|k| self.f32(a + 4 * k as u32))
    }
    pub fn set_vec4(&mut self, a: u32, v: [f32; 4]) {
        for (k, x) in v.into_iter().enumerate() {
            self.set_f32(a + 4 * k as u32, x);
        }
    }
    pub fn vec3(&self, a: u32) -> [f32; 3] {
        std::array::from_fn(|k| self.f32(a + 4 * k as u32))
    }
    pub fn set_vec3(&mut self, a: u32, v: [f32; 3]) {
        for (k, x) in v.into_iter().enumerate() {
            self.set_f32(a + 4 * k as u32, x);
        }
    }
    pub fn fill(&mut self, a: u32, len: u32, v: u8) {
        let o = (a - BASE) as usize;
        self.bytes[o..o + len as usize].fill(v);
    }
    pub fn copy(&mut self, dst: u32, src: u32, len: u32) {
        let (d, s) = ((dst - BASE) as usize, (src - BASE) as usize);
        self.bytes.copy_within(s..s + len as usize, d);
    }

    /// Camera record of a handle (its low byte).
    pub fn record(h: u32) -> u32 {
        RECORDS + (h & 0xff) * RECORD
    }
    /// A side of a handle's record.
    pub fn side(h: u32, side: u32) -> u32 {
        SIDES + (h & 0xff) * RECORD + (side & 1) * SIDE
    }
    /// The handle's sub-camera filter on a side.
    pub fn filter(h: u32, side: u32) -> u32 {
        Self::side(h, side) + 0xc0 + ((h >> 8) & 0xff) * FILTER
    }

    /// The camera system's start: records cleared with their defaults, then the screen slots.
    pub fn init(&mut self) {
        self.set_u32(BATTLE_DISTANCE, 1024f32.to_bits());
        for r in 0..3u32 {
            let rec = RECORDS + r * RECORD;
            self.fill(rec, RECORD, 0);
            self.set_f32(rec + 4, -1.0);
            self.set_u8(rec + 0xc8c, 0xff);
            for side in 0..2u32 {
                let s = rec + 0x30 + side * SIDE;
                self.set_u8(s, side as u8);
                // Each side's anchor is the other side's target.
                let other = rec + 0x30 + (1 - side) * SIDE + 0x50;
                self.set_u32(s + 0x8c, other);
            }
            for sub in 0..4u32 {
                self.set_f32(rec + 0xcc0 + sub * 0x30, 1024.0);
                for side in 0..2u32 {
                    let f = rec + 0x30 + side * SIDE + 0xc0 + sub * FILTER;
                    self.set_f32(f + 0x5c, 1.0);
                    self.set_f32(f + 0x60, 1.0);
                }
            }
        }
        self.set_u32(PATH_COUNTS, 0);
        self.set_u32(PATH_COUNTS + 4, 0);
        for a in (HANDLES_K1..HANDLES_K7 + 12).step_by(4) {
            self.set_u32(a, 0);
        }
        self.init_screens();
    }

    /// The screen slots: cleared, numbered, the PS2 screen size, then the projection values;
    /// slot 0 on.
    fn init_screens(&mut self) {
        for i in 0..3u32 {
            let s = SCREENS + i * SCREEN;
            self.fill(s, SCREEN, 0);
            self.set_u8(s + 0xc1, i as u8);
            self.set_i32(s + 0xd0, -1);
            for off in [0x38, 0x48, 0x58, 0x88, 0x98] {
                self.set_f32(s + off, 512.0);
            }
            for off in [0x3c, 0x4c, 0x5c, 0x8c, 0x9c] {
                self.set_f32(s + off, 416.0);
            }
            self.set_screen_projection(
                i,
                [512.0, 0.8, f32::from_bits(0x3f6e_eeef), 2048.0, 2048.0],
                [1.0, f32::from_bits(0x4b7f_e3d0), 1.0, 65536.0],
            );
        }
        self.set_u8(SCREENS + 0xc0, 1);
    }

    /// A screen's distance, two factors, the screen centre and the depth range values.
    pub fn set_screen_projection(
        &mut self,
        i: u32,
        [dist, a, b, cx, cy]: [f32; 5],
        [n0, f0, n1, f1]: [f32; 4],
    ) {
        if i >= 3 {
            return;
        }
        let s = SCREENS + i * SCREEN;
        self.set_f32(s + 0x1c, dist);
        self.set_f32(s + 0x10, a);
        self.set_f32(s + 0x14, b);
        self.set_f32(s + 0x18, (n1 * f1 * (f0 - n0)) / (f1 - n1));
        self.set_f32(s + 0x20, cx);
        self.set_f32(s + 0x24, cy);
        self.set_f32(s + 0x28, (n0 * f1 - f0 * n1) / (f1 - n1));
        self.set_f32(s, n0);
        self.set_f32(s + 4, f0);
        self.set_f32(s + 8, n1);
        self.set_f32(s + 0xc, f1);
    }
}
