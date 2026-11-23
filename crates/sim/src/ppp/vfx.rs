//! The remaster's effect geometry: what its renderer builds from a model draw request. Each packet of the PS2
//! effect model becomes a segment of vertex streams (positions, normals, colours as four floats, texture
//! coordinates as two floats, 16-bit indices) with its texture (the PS2 texture page, format and size, by which
//! the remaster names its HD texture), and the entry takes the shader properties (blend, texture, depth
//! write, z matrix) the remaster picks its VFX effect by. The world matrix is the inverse view times the
//! object's draw matrix.
//!
//! Packet kinds (header 0x10 bytes: +1 kind, i16 +2 count, +8 the texture word): 0–3 triangles of 0x14,
//! 0x1c, 0x20, 0x28 bytes; 4–7 quads of 0x18, 0x20, 0x28, 0x30 bytes. Textured: 2, 3, 6, 7. Kinds 2 and 6 are
//! built so far (the others' packets are skipped, their segments empty).

use super::{ModelDraw, Space};
use crate::chr::elements::{Mat, apply4, mul};

/// A PS2 texture as the remaster names its HD replacement: `<page>_<format>_0_0_<width>_<height>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Texture {
    pub page: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

impl Texture {
    /// The texture of a packet's texture words (`lo`, `hi`).
    pub fn from_words(lo: u32, hi: u32) -> Texture {
        let b = (lo >> 24) as u8;
        Texture {
            page: lo & 0x3fff,
            format: lo >> 20 & 0x3f,
            width: 1 << (b >> 2 & 0xf),
            height: 1 << ((b >> 6 | (hi << 2) as u8) & 0xf),
        }
    }

    /// The remaster's file name (`.dds.phyre`).
    pub fn file_name(&self) -> String {
        format!("{}_{}_0_0_{}_{}.dds.phyre", self.page, self.format, self.width, self.height)
    }

    /// Pages that name the frame buffer, not a texture.
    pub fn is_frame_buffer(&self) -> bool {
        self.page == 0x1a00 || self.page == 0xd00
    }
}

/// One packet's geometry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Segment {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colours: Vec<[f32; 4]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u16>,
    pub texture: Option<Texture>,
}

/// One render entry's geometry for this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Mesh {
    /// The entry's key (the program reference index and the object).
    pub ref_index: u32,
    pub object: u32,
    /// The shader properties the effect is chosen by (`aerugo_formats::vfx_shader` bits).
    pub properties: u32,
    pub world: Mat,
    pub z_matrix: Option<Mat>,
    pub semi: bool,
    pub segments: Vec<Segment>,
    /// Every colour component at most 0.5.
    pub dim: bool,
}

/// What the builder reads from the engine.
#[derive(Debug, Clone, Copy)]
pub struct Scene {
    /// The PS2 screen matrix and the inverse of the camera's view.
    pub screen: Mat,
    pub inverse_view: Mat,
    pub magic: u32,
    pub map_id: u32,
    pub event: u32,
    /// The map effect part being run (none for the others).
    pub map_part: Option<u32>,
}

/// The shader properties of a textured piece by its blend (modulate, additive, the three subtracts, none).
pub fn blend_properties(blend: u8) -> u32 {
    8 | match blend {
        0x41 => 0x100,
        0x42 => 0x10,
        0x46 => 0x20,
        0x48 => 2,
        0x88 => 0x200,
        _ => 0,
    }
}

/// The packet record sizes by kind, and whether the kind is textured.
fn record(kind: u8) -> Option<(u32, bool)> {
    Some(match kind {
        0 => (0x14, false),
        1 => (0x1c, false),
        2 => (0x20, true),
        3 => (0x28, true),
        4 => (0x18, false),
        5 => (0x20, false),
        6 => (0x28, true),
        7 => (0x30, true),
        _ => return None,
    })
}

/// The scroll offset in texture units (the draw's 16.16 offset modulo the size, in sixteenths, over the size).
fn scroll(offset: u32, size: u32) -> f32 {
    (((offset % (size << 16)) >> 4) / size) as f32
}

/// The entry's shader properties from the draw's state, the packets' textures and the scene (as the
/// remaster's buffer creation adjusts them).
fn properties(d: &ModelDraw, textured: bool, first_page: u32, s: &Scene) -> u32 {
    let mut p = d.create;
    if textured {
        p |= 8;
    }
    if s.map_part.is_some() && s.event == 0x188 {
        p |= 0x8_0000;
    } else if s.event == 0x46 && (first_page == 0x3800 || first_page == 0) {
        p &= !1;
    }
    if p & 0x1000_0000 != 0 {
        p = 0x1000_0000;
    }
    if (p as i32) < 0 {
        p = 0x8000_0000;
    }
    if (s.magic == 0xbc || s.magic == 0x15f) && first_page.wrapping_sub(0x3ac0) < 0x81 {
        p |= 0x40;
    }
    p
}

/// The geometry of a model draw.
pub fn model(m: &Space, d: &ModelDraw, s: &Scene) -> Mesh {
    let model = d.model;
    let count = m.u16(model + 0x10) as i16 as i32;
    let verts = if d.vertices != 0 { d.vertices } else { model.wrapping_add(m.u32(model + 8)) };
    // The colour the vertex colours are multiplied by: the draw's bytes · 2⁻¹⁵ (alpha by 1/128/the alpha
    // scale: 256, or 512 for magic 0x66 / 0x67 on page 0x3400).
    let first = model.wrapping_add(m.u32(model + 4));
    let first_page = m.u32(first + 8) & 0x3fff;
    let mut c = d.colour;
    if s.magic == 0x176 && matches!(first_page, 0x3780 | 0x3800 | 0x3880) {
        c[0] = 0x30;
        c[1] = 0x30;
        c[2] = 0x30;
    }
    let alpha_scale =
        if (s.magic == 0x66 || s.magic == 0x67) && first_page == 0x3400 { 512.0 } else { 256.0 };
    let k = [
        f32::from(c[0]) * f32::from_bits(0x3800_0000),
        f32::from(c[1]) * f32::from_bits(0x3800_0000),
        f32::from(c[2]) * f32::from_bits(0x3800_0000),
        f32::from(c[3]) * (0.0078125 / alpha_scale) as f32,
    ];
    let cull = d.flags & 0x40 == 0 && d.flags & 0x10 != 0 && s.map_id != 0x52b;
    let projection = d.work_matrix.map_or_else(|| mul(&s.screen, &d.matrix), |w| mul(&s.screen, &w));
    let projected = |i: u32| -> [f32; 2] {
        let p = verts + 6 * i;
        let v =
            [f32::from(m.u16(p) as i16), f32::from(m.u16(p + 2) as i16), f32::from(m.u16(p + 4) as i16), 1.0];
        let q = apply4(&projection, v);
        let f = if q[3] == 0.0 { 0.0 } else { 1.0 / q[3] };
        [f * q[0], q[1] * f]
    };
    let position = |i: u32| -> [f32; 3] {
        let p = verts + 6 * i;
        [f32::from(m.u16(p) as i16), f32::from(m.u16(p + 2) as i16), f32::from(m.u16(p + 4) as i16)]
    };
    let back = |a: u32, b: u32, c2: u32| -> bool {
        let (pa, pb, pc) = (projected(a), projected(b), projected(c2));
        let (x1, y1) = (pa[0] - pb[0], pa[1] - pb[1]);
        let (x2, y2) = (pc[0] - pb[0], pc[1] - pb[1]);
        let f = y2 * x1 - x2 * y1;
        0.0 < f
    };
    let mut segments = Vec::new();
    let mut textured = false;
    let mut dim = true;
    let mut left = count;
    let mut p = first;
    while left > 0 && segments.len() < 0x1d {
        let kind = m.u8(p + 1);
        let n = m.u16(p + 2) as i16;
        let Some((size, has_texture)) = record(kind) else { break };
        let tex = Texture::from_words(m.u32(p + 8), m.u32(p + 0xc));
        let mut seg = Segment { texture: has_texture.then_some(tex), ..Segment::default() };
        textured |= has_texture && !tex.is_frame_buffer();
        let (uo, vo) = (scroll(d.offsets.0, tex.width), scroll(d.offsets.1, tex.height));
        let colour = |seg: &mut Segment, at: u32, dim: &mut bool| {
            let v = [m.u8(at), m.u8(at + 1), m.u8(at + 2), m.u8(at + 3)];
            let c: [f32; 4] = std::array::from_fn(|i| f32::from(v[i]) * k[i]);
            if c.iter().any(|&x| 0.5 < x) {
                *dim = false;
            }
            seg.colours.push(c);
        };
        let mut r = p + 0x10;
        for i in 0..n.max(0) as u32 {
            match kind {
                2 => {
                    let idx = [m.u16(r + 0xc), m.u16(r + 0xe), m.u16(r + 0x10)];
                    let base = seg.positions.len() as u16;
                    let wide =
                        s.magic == 0x9a && matches!(tex.page, 0x37c0 | 0x3780 | 0x3800 | 0x3840 | 0x3880);
                    for &v in &idx {
                        let mut q = position(u32::from(v));
                        if wide {
                            q[0] *= 1.333_333_4;
                        }
                        seg.positions.push(q);
                        seg.normals.push([0.0; 3]);
                    }
                    for j in 0..3 {
                        colour(&mut seg, r + 4 * j, &mut dim);
                        let u = f32::from(m.u16(r + 0x14 + 4 * j));
                        let v = f32::from(m.u16(r + 0x16 + 4 * j));
                        seg.uvs.push([(u + uo) * 0.000_244_140_62, 1.0 - (v + vo) * 0.000_244_140_62]);
                    }
                    let culled = cull && back(u32::from(idx[0]), u32::from(idx[1]), u32::from(idx[2]));
                    seg.indices.extend(if culled { [0, 0, 0] } else { [base, base + 2, base + 1] });
                }
                6 => {
                    let idx = [m.u16(r + 0x10), m.u16(r + 0x12), m.u16(r + 0x14), m.u16(r + 0x16)];
                    let base = seg.positions.len() as u16;
                    let wide = s.magic == 0x241 && matches!(tex.page, 0x3900 | 0x39c0 | 0x3940 | 0x3980);
                    for &v in &idx {
                        let mut q = position(u32::from(v));
                        if wide {
                            q[0] *= 1.333_333_4;
                        }
                        seg.positions.push(q);
                        seg.normals.push([0.0, 0.0, 1.0]);
                    }
                    let special = matches!(s.magic, 0x66 | 0x67 | 0x1ba) && tex.page == 0x3400;
                    let bias = if special { -0x1000 } else { 0 };
                    for j in 0..4 {
                        colour(&mut seg, r + 4 * j, &mut dim);
                        let u = (i32::from(m.u16(r + 0x18 + 4 * j) as i16) + bias) as f32;
                        let v = (i32::from(m.u16(r + 0x1a + 4 * j) as i16) + bias) as f32;
                        seg.uvs.push([(u + uo) * 0.000_244_140_62, 1.0 - (v + vo) * 0.000_244_140_62]);
                    }
                    let culled = cull && back(u32::from(idx[0]), u32::from(idx[1]), u32::from(idx[2]));
                    seg.indices.extend(if culled {
                        [0; 6]
                    } else {
                        [base, base + 2, base + 1, base + 2, base + 3, base + 1]
                    });
                }
                _ => {}
            }
            let _ = i;
            r += size;
        }
        segments.push(seg);
        left -= i32::from(n);
        p = r;
    }
    let mut world = mul(&s.inverse_view, &d.matrix);
    world[3] = 0.0;
    world[7] = 0.0;
    world[11] = 0.0;
    world[15] = 1.0;
    let z_matrix = d.work_matrix.map(|w| mul(&s.inverse_view, &w));
    Mesh {
        ref_index: d.ref_index,
        object: d.object,
        properties: properties(d, textured, first_page, s),
        world,
        z_matrix,
        semi: d.semi,
        segments,
        dim,
    }
}
