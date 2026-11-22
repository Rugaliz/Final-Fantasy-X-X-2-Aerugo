//! The PS2 map's pre-rendered background layers, kept with the original's record layouts, and the remaster's
//! 2D layer switches they drive.
//!
//! A PS2 map package (`mapout.vpa`, `MAP1` +0x10) carries a chunk stream: 0x10-byte headers (a four-letter tag, the
//! body size in 16-byte units, +0xa a u16) each followed by its body. `YNDT` opens it and `YNED` closes it. `YNPR`
//! holds the scene count; each `YNSC` fills the next scene with its layers; `YNGM` (in the stream at +0x3c) adds a
//! guide map page to the scene its header names. A scene is 0x110 bytes: +4 layer count, +0x18 page count, +0x1a
//! page, +0x64, +0x68/+0x78/+0x88 camera vectors, +0xa8 the field of view, +0xb8/+0xbc screen offsets, +0xc0–+0xd8
//! a screen shake, +0xdc–+0xf0 a roll shake, +0xf4 a scale (10.0), +0xf8/+0xfc load flags, +0x104. A layer is 0xc0
//! bytes: +0 flags (bits 0–1 hidden, bit 4 a texture placed in memory), +4/+8 size, +0xc/+0x10 position, +0x14,
//! +0x34/+0x36 memory places, +0x40 colour (+0x43 alpha), +0x44/+0x48 scale (+0x54…+0x60 a scale move over +0x74
//! frames, +0x78 its count), +0x4c/+0x50 centre (+0x64…+0x70 a centre move over +0x7c frames, +0x80 its count),
//! then its frame animation (+0x90 flags: bit 0 paused, bit 1 backwards, bits 8–11 the end mode; +0x94 hold count,
//! +0x96 frame, +0x98 frame count, +0xa4 speed (0x100 one hold step a frame), +0xa8 its accumulator), its texture
//! (+0xa0, whose frame the animation sets) and a palette cycle (+0xb0…+0xbb, its texture at +0xbc).
//!
//! The remaster draws each layer as node `layer<i>` of the map's 2D model: natives and the per-frame step show and
//! hide it, set its alpha, move it, scale it and pick its texture animation frame. Those calls are kept here per
//! layer for the renderer.

use aerugo_script::natives::{Native, Natives};
use aerugo_script::vm::Value;

use super::FieldState;

pub const LAYER: usize = 0xc0;
pub const SCENE: usize = 0x110;

macro_rules! access {
    ($get:ident, $set:ident, $t:ty, $n:expr) => {
        pub fn $get(&self, a: usize) -> $t {
            <$t>::from_le_bytes(self.b[a..a + $n].try_into().unwrap())
        }
        pub fn $set(&mut self, a: usize, v: $t) {
            self.b[a..a + $n].copy_from_slice(&v.to_le_bytes());
        }
    };
}

/// One layer record.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    b: Vec<u8>,
    /// The frame animation's entries (+0x9c: 4 bytes each, byte 0 the texture frame, byte 1 the hold).
    pub frames: Vec<[u8; 4]>,
    /// The frame of the layer's texture (+0xa0 → +0x3c), when it has one.
    pub texture_frame: Option<u32>,
    /// The palette texture's width (+0xbc → +0x30) and frame (+0x3c).
    pub palette_width: u32,
    pub palette_frame: u32,
}

impl Layer {
    access!(u8, set_u8, u8, 1);
    access!(i16, set_i16, i16, 2);
    access!(u32, set_u32, u32, 4);
    access!(i32, set_i32, i32, 4);
    access!(f32, set_f32, f32, 4);

    /// The defaults.
    fn new() -> Layer {
        let mut l = Layer {
            b: vec![0; LAYER],
            frames: Vec::new(),
            texture_frame: None,
            palette_width: 0,
            palette_frame: 0,
        };
        l.set_u32(0x90 + 0x14, 0x100);
        l.set_f32(0x48, 1.0);
        l.set_f32(0x44, 1.0);
        l.set_u32(0x30, 0x100);
        l.set_u32(0x2c, 0x100);
        l.set_u32(0x38, 0x44);
        l.set_u32(0x3c, 0);
        l.set_u32(0x40, 0x8080_8080);
        l
    }

    pub fn hidden(&self) -> bool {
        self.u32(0) & 3 != 0
    }

    /// One frame of the frame animation; the entry it stood on (none without entries).
    fn animate(&mut self) -> Option<[u8; 4]> {
        let count = self.i32(0x98);
        if count == 0 {
            return None;
        }
        let at = self.i16(0x96) as i32;
        let entry = usize::try_from(at).ok().and_then(|i| self.frames.get(i)).copied().unwrap_or([0; 4]);
        let flags = self.u32(0x90);
        if flags & 1 == 0 {
            let acc = self.i32(0xa8).wrapping_add(self.i32(0xa4));
            self.set_i32(0xa8, acc);
            if acc > 0xff {
                let steps = acc.wrapping_add(acc >> 31 & 0xff) >> 8;
                let mut hold = self.i16(0x94).wrapping_add(steps as i16);
                self.set_i16(0x94, hold);
                self.set_i32(0xa8, acc.wrapping_sub(steps << 8));
                let limit = i16::from(entry[1]);
                while limit < hold {
                    hold = hold.wrapping_add(-1 - limit);
                    self.set_i16(0x94, hold);
                    let mut frame = self.i16(0x96);
                    if flags & 2 == 0 {
                        frame = frame.wrapping_add(1);
                        if count <= i32::from(frame) {
                            match flags & 0xf00 {
                                0 => frame = 0,
                                0x100 => frame = (count as i16).wrapping_sub(1),
                                _ => {}
                            }
                        }
                    } else {
                        frame = frame.wrapping_sub(1);
                        if frame < 0 {
                            match flags & 0xf00 {
                                0 => frame = (count as i16).wrapping_sub(1),
                                0x100 => frame = 0,
                                _ => {}
                            }
                        }
                    }
                    self.set_i16(0x96, frame);
                }
            }
        }
        Some(entry)
    }

    /// The palette cycle's counters (the palette copies themselves are PS2 memory and not kept) and the
    /// palette texture's frame (its width − 1).
    fn cycle_palette(&mut self) {
        let mode = self.u8(0xb0 + 10);
        if self.u8(0xb0) & 1 == 0 && (mode == 1 || mode == 2) {
            let n = self.i16(0xb4).wrapping_add(1);
            self.set_i16(0xb4, n);
            if i16::from(self.u8(0xbb)) < n {
                let mut p = self.i16(0xb6).wrapping_add(1);
                if p > 0xff {
                    p -= 0x100;
                }
                self.set_i16(0xb6, p);
                self.set_i16(0xb4, 0);
            }
        }
        self.palette_frame = self.palette_width.wrapping_sub(1);
    }

    /// The scale and centre moves; the scale is passed on to the remaster (layer, width, height, x, y).
    fn moves(&mut self) -> Option<(i32, i32, f32, f32)> {
        let mut scale = None;
        let n = self.i32(0x74);
        if n != 0 {
            let i = self.i32(0x78).wrapping_add(1);
            self.set_i32(0x78, i);
            let t = f64::from((f64::from(i as f32) / f64::from(n as f32)) as f32);
            let lerp = |a: f32, b: f32| ((f64::from(b) - f64::from(a)) * t + f64::from(a)) as f32;
            let x = lerp(self.f32(0x54), self.f32(0x58));
            self.set_f32(0x44, x);
            let y = lerp(self.f32(0x5c), self.f32(0x60));
            self.set_f32(0x48, y);
            if n <= i {
                self.set_i32(0x74, 0);
            }
            scale = Some((self.i32(4), self.i32(8), x, y));
        }
        let n = self.i32(0x7c);
        if n != 0 {
            let i = self.i32(0x80).wrapping_add(1);
            self.set_i32(0x80, i);
            let t = f64::from((f64::from(i as f32) / f64::from(n as f32)) as f32);
            let lerp = |a: f32, b: f32| ((f64::from(b) - f64::from(a)) * t + f64::from(a)) as f32;
            let x = lerp(self.f32(0x64), self.f32(0x68));
            self.set_f32(0x4c, x);
            let y = lerp(self.f32(0x6c), self.f32(0x70));
            self.set_f32(0x50, y);
            if n <= i {
                self.set_i32(0x7c, 0);
            }
        }
        scale
    }
}

/// One scene record and its layers.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    b: Vec<u8>,
    pub layers: Vec<Layer>,
}

impl Scene {
    access!(i16, set_i16, i16, 2);
    access!(u32, set_u32, u32, 4);
    access!(i32, set_i32, i32, 4);
    access!(f32, set_f32, f32, 4);

    /// Cleared, +0xf4 = 10.0, +0 = 1.
    fn new() -> Scene {
        let mut s = Scene { b: vec![0; SCENE], layers: Vec::new() };
        s.set_u32(0xf4, 0x4120_0000);
        s.set_u32(0, 1);
        s
    }

    /// The layer count (+4).
    pub fn count(&self) -> i32 {
        self.i32(4)
    }
}

/// What the remaster's 2D layer `layer<i>` was told.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer2d {
    pub hidden: bool,
    /// The material visibility switch (graphicTextureAnimationSetMaterialVisiability).
    pub material_visible: bool,
    /// The texture animation plays by itself.
    pub auto: bool,
    /// The texture animation frame set this frame.
    pub frame: Option<u32>,
    /// The alpha (×1/128 of the material colour).
    pub alpha: Option<i32>,
    /// The moves passed (added to the node's translation).
    pub moved: [i32; 2],
    /// The last scale (width, height, x, y).
    pub scale: Option<(i32, i32, f32, f32)>,
}

impl Default for Layer2d {
    fn default() -> Self {
        Layer2d {
            hidden: false,
            material_visible: true,
            auto: true,
            frame: None,
            alpha: None,
            moved: [0; 2],
            scale: None,
        }
    }
}

/// The background: its scenes (none when the map has no stream), the current one and the remaster's layers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PreBg {
    /// The main record exists (its first word).
    pub main: bool,
    pub scenes: Vec<Scene>,
    pub current: usize,
    pub remaster: Vec<Layer2d>,
    /// The last switch of all the remaster's layers hid them.
    pub all_hidden: bool,
    /// The map's name (its folder, e.g. `genk12`) for the per-map cases.
    pub map: String,
}

/// A chunk stream reader (rcdt parse: a cursor that copies and skips).
struct Parse<'a> {
    d: &'a [u8],
    at: usize,
}

impl Parse<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.d.get(self.at..self.at + n)?;
        self.at += n;
        Some(s)
    }
    fn skip(&mut self, n: usize) {
        self.at += n;
    }
    fn i32(&mut self) -> Option<i32> {
        self.take(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

const TAGS: [&[u8; 4]; 13] = [
    b"YNDT", b"YNPR", b"YNSC", b"YNTX", b"YNTM", b"YNSM", b"YNST", b"YNSP", b"YNTT", b"YNSE", b"YNFT",
    b"YNGM", b"YNED",
];

impl PreBg {
    pub fn scene(&self) -> Option<&Scene> {
        self.main.then(|| self.scenes.get(self.current)).flatten()
    }

    fn scene_mut(&mut self) -> Option<&mut Scene> {
        if !self.main {
            return None;
        }
        self.scenes.get_mut(self.current)
    }

    /// The current scene's layer `i` when `i` is below its count.
    fn layer(&mut self, i: i32) -> Option<&mut Layer> {
        let s = self.scene_mut()?;
        if i >= s.count() {
            return None;
        }
        s.layers.get_mut(usize::try_from(i).ok()?)
    }

    pub fn count(&self) -> i32 {
        self.scene().map_or(0, Scene::count)
    }

    fn layer2d(&mut self, i: i32) -> Option<&mut Layer2d> {
        let i = usize::try_from(i).ok()?;
        if self.remaster.len() <= i {
            let d = Layer2d { hidden: self.all_hidden, ..Layer2d::default() };
            self.remaster.resize(i + 1, d);
        }
        self.remaster.get_mut(i)
    }

    /// The streams of a `MAP1` package (map load: +0x10, then +0x40 and +0x3c).
    pub fn from_map_package(vpa: &[u8], map: &str) -> PreBg {
        let mut p = PreBg { map: map.to_owned(), ..PreBg::default() };
        let u32_at = |at: usize| vpa.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
        if !vpa.starts_with(b"MAP1") {
            return p;
        }
        for off in [0x10, 0x40, 0x3c] {
            if let Some(o) = u32_at(off).filter(|&o| o != 0) {
                p.load(vpa, o as usize);
            }
        }
        p
    }

    /// The background load: one chunk stream.
    fn load(&mut self, d: &[u8], at: usize) {
        let tag = |at: usize| d.get(at..at + 4).and_then(|t| TAGS.iter().position(|k| &k[..] == t));
        if tag(at) != Some(0) {
            return;
        }
        let mut at = at + 0x10;
        let mut next_scene = 0usize;
        loop {
            let Some(size) =
                d.get(at + 4..at + 8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            else {
                return;
            };
            let body = at + 0x10;
            let mut r = Parse { d, at: body };
            match tag(at) {
                Some(1) => {
                    // A new main record and its scenes.
                    let n = r.i32().unwrap_or(0).max(0) as usize;
                    self.main = true;
                    self.scenes = (0..n).map(|_| Scene::new()).collect();
                    self.current = 0;
                    next_scene = 0;
                }
                Some(2) => {
                    if let Some(s) = self.scenes.get_mut(next_scene) {
                        load_layers(s, &mut r);
                    }
                    next_scene += 1;
                }
                Some(11) => {
                    // A guide map page: the load makes a main record with one scene when there is none.
                    if !self.main {
                        self.main = true;
                        self.scenes = vec![Scene::new()];
                        self.current = 0;
                    }
                    let k = d.get(at + 10..at + 12).map_or(0, |b| u16::from_le_bytes([b[0], b[1]])) as usize;
                    if let Some(s) = self.scenes.get_mut(k) {
                        let n = s.i16(0x18);
                        s.set_i16(0x18, n.wrapping_add(1));
                    }
                }
                Some(12) => return,
                // 5/6 scene values, 8/10 effect models, the rest skipped (unknown tags report an error first).
                _ => {}
            }
            at = body + size * 0x10;
        }
    }

    /// The scene step (once a frame after the scripts): the current scene's shakes and layers.
    pub fn frame(
        &mut self,
        rand: &mut super::face::CrtRand,
        sine: &[f32],
        map_id: u32,
        language: Option<u32>,
    ) {
        if !self.main {
            return;
        }
        let map = self.map.clone();
        let Some(s) = self.scenes.get_mut(self.current) else { return };
        shake(s, rand, sine);
        roll(s, sine);
        // The layers.
        let n = usize::try_from(s.count()).unwrap_or(0);
        let mut out = Vec::new();
        for i in 0..n {
            out.push((i, Op::Auto(false)));
        }
        for (i, l) in s.layers.iter_mut().enumerate().take(n) {
            if let Some(e) = l.animate() {
                l.texture_frame = l.texture_frame.map(|_| u32::from(e[0]));
                l.set_i16(0xb0 + 8, i16::from(e[0]));
            }
            l.cycle_palette();
            if let Some(sc) = l.moves() {
                out.push((i, Op::Scale(sc)));
            }
            let auto_on = match i {
                2 => {
                    if map.contains("genk12") {
                        l.set_u32(0xa4, 0x80);
                    }
                    map.contains("genk05")
                }
                3 => map.contains("genk12"),
                _ => false,
            };
            if auto_on {
                out.push((i, Op::Auto(true)));
            } else {
                if i == 0xd && map_id == 0xcbc && language == Some(9) {
                    l.texture_frame = l.texture_frame.map(|_| 1);
                }
                if let Some(f) = l.texture_frame {
                    out.push((i, Op::Frame(f)));
                }
            }
            if l.hidden() {
                out.push((i, Op::Hide));
            }
        }
        for r in &mut self.remaster {
            r.frame = None;
        }
        for (i, op) in out {
            self.apply(i as i32, op);
        }
    }

    fn apply(&mut self, i: i32, op: Op) {
        let Some(r) = self.layer2d(i) else { return };
        match op {
            Op::Auto(on) => r.auto = on,
            Op::Frame(f) => {
                r.auto = false;
                r.frame = Some(f);
            }
            Op::Hide => {
                r.hidden = true;
                r.material_visible = false;
            }
            Op::Show => {
                r.hidden = false;
                r.material_visible = true;
            }
            Op::Scale(s) => r.scale = Some(s),
        }
    }

    /// Layer 0's flag, then all of the remaster's 2D layers the other way.
    fn layer0(&mut self, on: bool) {
        let Some(l) = self.scene_mut().and_then(|s| s.layers.first_mut()) else { return };
        let f = l.u32(0);
        l.set_u32(0, if on { f & !1 } else { f | 1 });
        self.all_hidden = on;
        for r in &mut self.remaster {
            r.hidden = on;
        }
    }

    /// Hidden; 800f: shown. The remaster's range is the layer count, 0x1c
    /// in scenes 0x15b and 0xd4.
    fn display(&mut self, i: i32, on: bool, scene: Option<u16>) {
        let mut range = self.count();
        if let Some(l) = self.layer(i) {
            let f = l.u32(0);
            l.set_u32(0, if on { f & !1 } else { f | 1 });
        }
        if matches!(scene, Some(0x15b | 0xd4)) {
            range = 0x1c;
        }
        if i < range {
            self.apply(i, if on { Op::Show } else { Op::Hide });
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Auto(bool),
    Frame(u32),
    Hide,
    Show,
    Scale((i32, i32, f32, f32)),
}

/// The layer load: the layers, the camera values and the load flags.
fn load_layers(s: &mut Scene, r: &mut Parse<'_>) {
    let n = r.i32().unwrap_or(0);
    s.set_i32(4, n);
    r.skip(0xc);
    s.layers.clear();
    for _ in 0..n.max(0) {
        let mut l = Layer::new();
        if let Some(raw) = r.take(0x90) {
            l.b[..0x90].copy_from_slice(raw);
        }
        let frames = r.i32().unwrap_or(0);
        r.skip(0xc);
        if frames > 0 {
            l.set_i32(0x98, frames);
            l.frames = (0..frames).map(|_| r.take(4).map_or([0; 4], |b| [b[0], b[1], b[2], b[3]])).collect();
            if frames & 3 != 0 {
                r.skip((0x10 - (frames & 3) * 4) as usize);
            }
            // The texture (frame −1), its header and image.
            l.texture_frame = Some(u32::MAX);
            texture(r);
        }
        if let Some(b) = r.take(2) {
            l.b[0xba] = b[0];
            l.b[0xbb] = b[1];
        }
        r.skip(0xe);
        l.palette_width = texture(r);
        l.palette_frame = u32::MAX;
        // The centres.
        let w = (f64::from(l.i32(4) as f32) * 0.5) as f32;
        let h = (f64::from(l.i32(8) as f32) * 0.5) as f32;
        for a in [0x68, 0x64, 0x4c] {
            l.set_f32(a, w);
        }
        for a in [0x70, 0x6c, 0x50] {
            l.set_f32(a, h);
        }
        s.layers.push(l);
    }
    for (a, n) in [(0x68, 0x10), (0x78, 0x10), (0x88, 0x10), (0xa8, 4), (0x64, 4)] {
        if let Some(b) = r.take(n) {
            s.b[a..a + n].copy_from_slice(b);
        }
    }
    // The load flags, then the memory places of the layers with bit 4.
    if s.layers.iter().any(|l| l.i32(8) > 0x1bf) {
        s.set_u32(0xf8, 1);
    }
    if s.layers.iter().any(|l| l.u32(0) & 0x10 != 0) {
        s.set_u32(0xfc, 1);
    }
    place(s);
}

/// A texture's header (three 8-byte words, width, height) and its image; the width.
fn texture(r: &mut Parse<'_>) -> u32 {
    r.skip(0x18);
    let w = r.i32().unwrap_or(0);
    let h = r.i32().unwrap_or(0);
    r.skip(w.wrapping_mul(h).max(0) as usize);
    w as u32
}

/// Where the bit-4 layers' textures go (+0x34 image, +0x36 palette, in 64-byte units).
fn place(s: &mut Scene) {
    let mut total: i32 = 0;
    let mut half = false;
    for l in s.layers.iter_mut() {
        if l.u32(0) & 0x10 != 0 {
            l.set_i16(0x34, (total.wrapping_add(total >> 31 & 0x3f) >> 6) as i16);
            let mut size =
                (l.i32(4).wrapping_add(0x7f) & !0x7f).wrapping_mul(l.i32(8).wrapping_add(0x3f) & !0x3f);
            if half {
                size = size.wrapping_add(size >> 31 & 3) >> 2;
            }
            total = total.wrapping_add(size);
            if !half && total > 0x67fff {
                half = true;
                l.b[0x34] = 0;
                l.b[0x35] = 0x2f;
                total = 0xbc000;
            }
        }
    }
    if !half {
        total = 0xbc000;
    }
    for l in s.layers.iter_mut() {
        if l.u32(0) & 0x10 != 0 {
            l.set_i16(0x36, (total.wrapping_add(total >> 31 & 0x3f) >> 6) as i16);
            total = total.wrapping_add(0x100);
            l.texture_frame = l.texture_frame.map(|_| 0);
            l.set_i16(0xb0 + 8, 0);
        }
    }
}

/// x87 `__ftol2`: truncation toward zero, the low 32 bits.
fn ftol(x: f64) -> i32 {
    if x.is_finite() && x.abs() < 9.2e18 { (x.trunc() as i64) as i32 } else { 0 }
}

/// The screen shake (+0xc0 mode: odd → +0xb8, even → +0xbc; modes 3–4 fade over +0xc8 frames).
fn shake(s: &mut Scene, rand: &mut super::face::CrtRand, sine: &[f32]) {
    let mode = s.i32(0xc0);
    if mode == 0 {
        return;
    }
    let tau = f64::from_bits(0x4019_21fb_6000_0000);
    let k = ftol(f64::from(s.f32(0xd4)) * -65536.0 / tau);
    let mut v = sine.get(((0x4000 - k) >> 4 & 0xfff) as usize).copied().unwrap_or(0.0);
    let total = s.i32(0xc8);
    if (mode - 1) / 2 == 1 && total > 0 {
        let left = s.i32(0xd8);
        v = (f64::from(total.wrapping_sub(left) as f32) / f64::from(total as f32) * f64::from(v)) as f32;
    }
    let base = s.i32(0xcc);
    let r = if s.i32(0xd0) > 0 { rand.rand() % s.i32(0xd0) + base } else { base };
    let off = (f64::from(r as f32) * f64::from(v)) as f32;
    s.set_f32(if mode & 1 != 0 { 0xb8 } else { 0xbc }, off);
    let n = s.i32(0xd8).wrapping_add(1);
    s.set_i32(0xd8, n);
    s.set_f32(0xd4, (f64::from(s.f32(0xc4)) + f64::from(s.f32(0xd4))) as f32);
    if total > 0 && total <= n {
        s.set_u32(0xbc, 0);
        s.set_u32(0xb8, 0);
        s.set_u32(0xc0, 0);
    }
}

/// The roll shake (+0xdc mode; mode 2 fades over +0xe4 frames) into +0xac.
fn roll(s: &mut Scene, sine: &[f32]) {
    let mode = s.i32(0xdc);
    if mode == 0 {
        return;
    }
    let tau = f64::from_bits(0x4019_21fb_6000_0000);
    let k = ftol(f64::from(s.f32(0xec)) * 65536.0 / tau);
    let mut v = sine.get((k >> 4 & 0xfff) as usize).copied().unwrap_or(0.0);
    let total = s.i32(0xe4);
    if mode == 2 && total > 0 {
        v = (f64::from(total.wrapping_sub(s.i32(0xf0)) as f32) / f64::from(total as f32) * f64::from(v))
            as f32;
    }
    let n = s.i32(0xf0).wrapping_add(1);
    s.set_i32(0xf0, n);
    s.set_f32(0xac, (f64::from(s.f32(0xe8)) * f64::from(v)) as f32);
    s.set_f32(0xec, (f64::from(s.f32(0xe0)) + f64::from(s.f32(0xec))) as f32);
    if total > 0 && total <= n {
        s.set_u32(0xac, 0);
        s.set_u32(0xdc, 0);
    }
}

/// MenuManager's localized title layers: map id, then the layer per language type 0–5, 9, 10.
const LOCALIZED: [(u32, [i32; 9]); 2] =
    [(0xcbc, [0, 0xd, 7, 0xb, 9, 5, 0xd, 3, 0xd]), (0x199f, [2, 0, 6, 8, 7, 5, 2, 2, 8])];

/// getLocMenuPageLayer: the layer of map `map` for language type `lang`, −1 when it has none.
fn localized_layer(lang: Option<u32>, map: u32) -> i32 {
    let Some(lang) = lang else { return -1 };
    let Some((_, t)) = LOCALIZED.iter().find(|(m, _)| *m == map) else { return -1 };
    match lang {
        0..=5 => t[lang as usize],
        9 | 10 => t[lang as usize - 3],
        _ => -1,
    }
}

pub fn register(n: &mut Natives<FieldState>) {
    // 8010(i): the layer hidden (+0 |= 1) and the remaster's layer hidden; 800f(i): shown (its localized layer on
    // the title maps). Both return 1.
    n.register(
        0x8010,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            g.pre_bg.display(i, false, g.scene);
            Value::Int(1)
        }),
    );
    n.register(
        0x800F,
        Native::result(|g, c| {
            let mut i = c.stack.pop_int();
            let l = localized_layer(g.language_type, g.map_id);
            if l != -1 {
                i = l;
            }
            g.pre_bg.display(i, true, g.scene);
            Value::Int(1)
        }),
    );
    // 802c: layer 0 hidden (+0 |= 1), then every remaster 2D layer shown; 802d: layer 0 shown, then
    // every remaster 2D layer hidden. Both return 1.
    n.register(
        0x802C,
        Native::result(|g, _| {
            g.pre_bg.layer0(false);
            Value::Int(1)
        }),
    );
    n.register(
        0x802D,
        Native::result(|g, _| {
            g.pre_bg.layer0(true);
            Value::Int(1)
        }),
    );
    // Animation switches of layer i (return 1): 801e paused, 801f running, 8020 backwards, 8021 forwards; 8022(i,
    // mode) the end mode (+0x90 bits 8–11 = mode, unmasked); 801d(i, frame); 802f(i) the last frame; 802e(i, v)
    // the speed.
    fn flags(g: &mut FieldState, i: i32, f: impl Fn(u32) -> u32) {
        if let Some(l) = g.pre_bg.layer(i) {
            let v = l.u32(0x90);
            l.set_u32(0x90, f(v));
        }
    }
    n.register(
        0x801E,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            flags(g, i, |v| v | 1);
            Value::Int(1)
        }),
    );
    n.register(
        0x801F,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            flags(g, i, |v| v & !1);
            Value::Int(1)
        }),
    );
    n.register(
        0x8020,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            flags(g, i, |v| v | 2);
            Value::Int(1)
        }),
    );
    n.register(
        0x8021,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            flags(g, i, |v| v & !2);
            Value::Int(1)
        }),
    );
    n.register(
        0x8022,
        Native::result(|g, c| {
            let mode = c.stack.pop_int() as u32;
            let i = c.stack.pop_int();
            flags(g, i, |v| v & 0xffff_f0ff | mode);
            Value::Int(1)
        }),
    );
    n.register(
        0x801D,
        Native::result(|g, c| {
            let f = c.stack.pop_int();
            let i = c.stack.pop_int();
            if let Some(l) = g.pre_bg.layer(i) {
                l.set_i16(0x96, f as i16);
            }
            Value::Int(1)
        }),
    );
    n.register(
        0x802F,
        Native::result(|g, c| {
            let i = c.stack.pop_int();
            if let Some(l) = g.pre_bg.layer(i) {
                let last = l.i16(0x98).wrapping_sub(1);
                l.set_i16(0x96, last);
            }
            Value::Int(1)
        }),
    );
    n.register(
        0x802E,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            let i = c.stack.pop_int();
            if let Some(l) = g.pre_bg.layer(i) {
                l.set_i32(0xa4, v);
            }
            Value::Int(1)
        }),
    );
    // 805d: every map effect's state 0, then every layer's frame 0; returns 1.
    n.register(
        0x805D,
        Native::result(|g, _| {
            g.map_effects.0.iter_mut().for_each(|e| e.state = 0);
            for i in 0..g.pre_bg.count() {
                if let Some(l) = g.pre_bg.layer(i) {
                    l.set_i16(0x96, 0);
                }
            }
            Value::Int(1)
        }),
    );
    // 8011(i, x, y): the layer's place (+0xc, +0x10); the remaster's layer moves by the difference. Layer 0x1a of
    // event 0x17e: y one less on the 0th row of 18 from 0x54, one more on the 14th.
    n.register(
        0x8011,
        Native::result(|g, c| {
            let mut y = c.stack.pop_int();
            let x = c.stack.pop_int();
            let i = c.stack.pop_int();
            let scene = g.scene;
            if let Some(l) = g.pre_bg.layer(i) {
                if i == 0x1a && scene == Some(0x17e) {
                    let d = y.wrapping_sub(0x54);
                    let s = d >> 31;
                    let row = d / 0x12 + s;
                    if row == s {
                        y -= 1;
                    } else if row - s == 0xe {
                        y += 1;
                    }
                }
                let moved = [x.wrapping_sub(l.i32(0xc)), y.wrapping_sub(l.i32(0x10))];
                l.set_i32(0x10, y);
                l.set_i32(0xc, x);
                if let Some(r) = g.pre_bg.layer2d(i) {
                    r.moved[0] = r.moved[0].wrapping_add(moved[0]);
                    r.moved[1] = r.moved[1].wrapping_add(moved[1]);
                }
            }
            Value::Int(1)
        }),
    );
    // 8014(i, v): layer +0x14 = v (a float); returns 1.
    n.register(
        0x8014,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            let i = c.stack.pop_int();
            if let Some(l) = g.pre_bg.layer(i) {
                l.set_f32(0x14, v);
            }
            Value::Int(1)
        }),
    );
    // 8030(i, a): the remaster's layer alpha and the layer's colour alpha byte (+0x43); returns 1.
    n.register(
        0x8030,
        Native::result(|g, c| {
            let a = c.stack.pop_int();
            let i = c.stack.pop_int();
            if let Some(l) = g.pre_bg.layer(i) {
                l.b[0x43] = a as u8;
                if let Some(r) = g.pre_bg.layer2d(i) {
                    r.alpha = Some(a);
                }
            }
            Value::Int(1)
        }),
    );
    // 804f(i, scale, frames): the scale for both axes: a scale move from the current scale over
    // `frames` frames (scale 0 keeps the current one); returns 1.
    n.register(
        0x804F,
        Native::result(|g, c| {
            let frames = c.stack.pop_int();
            let v = c.stack.pop_float();
            let i = c.stack.pop_int();
            if let Some(l) = g.pre_bg.layer(i) {
                let (sx, sy) = (l.u32(0x44), l.u32(0x48));
                l.set_u32(0x54, sx);
                if v == 0.0 {
                    l.set_u32(0x58, sx);
                } else {
                    l.set_f32(0x58, v);
                }
                l.set_u32(0x5c, sy);
                if v == 0.0 {
                    l.set_u32(0x60, sy);
                } else {
                    l.set_f32(0x60, v);
                }
                l.set_i32(0x74, frames);
                l.set_i32(0x78, 0);
            }
            Value::Int(1)
        }),
    );
    // 8026(mode, step, frames, base, spread): the screen shake (+0xc0 …, +0xd4 and +0xd8 cleared); returns 1.
    n.register(
        0x8026,
        Native::result(|g, c| {
            let spread = c.stack.pop_int();
            let base = c.stack.pop_int();
            let frames = c.stack.pop_int();
            let step = c.stack.pop_float();
            let mode = c.stack.pop_int();
            if let Some(s) = g.pre_bg.scene_mut() {
                s.set_i32(0xc0, mode);
                s.set_f32(0xc4, step);
                s.set_i32(0xc8, frames);
                s.set_u32(0xd4, 0);
                s.set_i32(0xcc, base);
                s.set_i32(0xd0, spread);
                s.set_u32(0xd8, 0);
            }
            Value::Int(1)
        }),
    );
    // 806a(v): scene +0x104 = v; returns 1.
    n.register(
        0x806A,
        Native::result(|g, c| {
            let v = c.stack.pop_int();
            if let Some(s) = g.pre_bg.scene_mut() {
                s.set_i32(0x104, v);
            }
            Value::Int(1)
        }),
    );
    // 8006(page): the walk mesh page switch for the next frame (switch on, the page, no second page)
    // and the map-change record (room, page: save +6/+0xa = +4/+8, or +0xc6/+0xca when +0x2a is
    // set; +4 = the room (unchanged), +0x2a = 0, +8 = page). On map 0x159c at story 0x7e4, page 2 sets the background's alternate flag.
    // The wait is done at once.
    n.register(
        0x8006,
        Native {
            init: Some(|g, c| {
                let page = c.stack.pop_int();
                let save = &mut c.memory.global[0];
                let u16_at =
                    |s: &[u8], a: usize| s.get(a..a + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
                if g.map_id == 0x159c && u16_at(save, 0xbec) == 0x7e4 && page == 2 {
                    g.page_flag_bf68 = true;
                }
                g.page_switch = (1, page as i8, -1);
                if save.len() >= 0xcc {
                    let room = u16_at(save, 4);
                    let (p6, pa) = if save[0x2a] != 0 {
                        (u16_at(save, 0xc6), u16_at(save, 0xca))
                    } else {
                        (u16_at(save, 4), u16_at(save, 8))
                    };
                    save[6..8].copy_from_slice(&p6.to_le_bytes());
                    save[0xa..0xc].copy_from_slice(&pa.to_le_bytes());
                    save[4..6].copy_from_slice(&room.to_le_bytes());
                    save[0x2a] = 0;
                    save[8..0xa].copy_from_slice(&(page as u16).to_le_bytes());
                }
            }),
            wait: Some(|_, _| 1),
            result: None,
        },
    );
    // 8032(v): the guide map's farthest camera distance (+0x6c) when the guide map is set up; returns 1.
    n.register(
        0x8032,
        Native::result(|g, c| {
            let v = c.stack.pop_float();
            if let Some(m) = g.guide_map.as_mut() {
                m.max_camera_dist = Some(v);
            }
            Value::Int(1)
        }),
    );
    // 8001(on): map display bit 0x10 on or off and the remaster's render switch (on ≠ 0); returns on.
    n.register(
        0x8001,
        Native::result(|g, c| {
            let on = c.stack.pop_int();
            if on != 0 {
                g.map_display |= 0x10;
            } else {
                g.map_display &= !0x10;
            }
            g.map_render_off = Some(on != 0);
            Value::Int(on)
        }),
    );
    // 805e(page): the guide map page (scene +0x1a) when the guide map is set up and the page exists; returns 1.
    n.register(
        0x805E,
        Native::result(|g, c| {
            let page = c.stack.pop_int();
            let guide = g.guide_map.is_some();
            if guide
                && let Some(s) = g.pre_bg.scene_mut()
                && page >= 0
                && page < i32::from(s.i16(0x18))
            {
                s.set_i16(0x1a, page as i16);
            }
            Value::Int(1)
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer_with(frames: &[[u8; 4]]) -> Layer {
        let mut l = Layer::new();
        l.frames = frames.to_vec();
        l.set_i32(0x98, frames.len() as i32);
        l.texture_frame = Some(u32::MAX);
        l
    }

    #[test]
    fn animation_holds_then_steps() {
        // Hold 1: each entry shows for two frames at speed 0x100.
        let mut l = layer_with(&[[5, 1, 0, 0], [6, 1, 0, 0]]);
        let seen: Vec<u8> = (0..5).map(|_| l.animate().unwrap()[0]).collect();
        assert_eq!(seen, [5, 5, 6, 6, 5]);
        // The end mode 0x100 stays on the last frame.
        let mut l = layer_with(&[[1, 0, 0, 0], [2, 0, 0, 0]]);
        l.set_u32(0x90, 0x100);
        let seen: Vec<u8> = (0..4).map(|_| l.animate().unwrap()[0]).collect();
        assert_eq!(seen, [1, 2, 2, 2]);
    }

    #[test]
    fn localized_title_layers() {
        assert_eq!(localized_layer(Some(1), 0xcbc), 0xd);
        assert_eq!(localized_layer(Some(10), 0x199f), 2);
        assert_eq!(localized_layer(Some(7), 0xcbc), -1);
        assert_eq!(localized_layer(Some(1), 5), -1);
    }
}
