//! The remaster's 2D layer: the PS2-side drawing calls (boxes, images and glyphs placed in the PS2's 512 × 416
//! frame) turned into HD quads and grouped into draws the way the remaster's menu manager groups them.
//!
//! HD space: 1920 × 1080 units centred on the screen, y down (the menu shaders' orthographic projection maps it
//! to the view). Colours are the PS2's 0–255 integers, where 0x80 is full intensity; the shaders double them.

/// PS2 frame of the 2D layer.
pub const PS2_W: f32 = 512.0;
pub const PS2_H: f32 = 416.0;
/// HD space.
pub const HD_W: f32 = 1920.0;
pub const HD_H: f32 = 1080.0;
/// HD units per PS2 unit.
pub const SCALE_X: f32 = 3.75;
pub const SCALE_Y: f32 = 2.596_153_7;

/// HD units to PS2 units (the remaster sizes its own window parts in HD units).
pub fn hd_to_ps2_x(v: f32) -> f32 {
    (f64::from(v) * f64::from(PS2_W) / f64::from(HD_W)) as f32
}

pub fn hd_to_ps2_y(v: f32) -> f32 {
    (f64::from(v) * f64::from(PS2_H) / f64::from(HD_H)) as f32
}

/// A PS2 colour (r, g, b, a; 0x80 = 1.0).
pub type Rgba = [u8; 4];

/// A colour word as the menu code writes it (0xAABBGGRR, bytes r, g, b, a in memory order).
pub fn rgba(word: u32) -> Rgba {
    word.to_le_bytes()
}

/// The images the 2D layer samples. Paths depend on the text language for the language-specific ones.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Image {
    /// Message window parts (`meswin`, per language).
    Meswin,
    /// Window background (`stonetexture`).
    Stone,
    /// A page of the base font, or of its shadow (`base_ftc/font_0_<page>`, `shadow_0_<page>`).
    Font { page: u8, shadow: bool },
}

impl Image {
    /// The image's path in the game data; `lang` is the menu language folder suffix (e.g. `us`).
    pub fn path(&self, lang: &str) -> String {
        let root = "ffx_data/gamedata/ps3data";
        match self {
            Image::Meswin => format!("{root}/menu_{lang}/d3d11/meswin.dds.phyre"),
            Image::Stone => format!("{root}/menu/d3d11/stonetexture.dds.phyre"),
            Image::Font { page, shadow } => {
                let kind = if *shadow { "shadow" } else { "font" };
                format!("{root}/menu_{lang}/base_ftc/d3d11/{kind}_0_{page}.dds.phyre")
            }
        }
    }
}

/// How a draw is shaded: the menu effect with a texture or without one, or the font effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shading {
    Plain,
    Textured,
    Font,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    /// HD space.
    pub pos: [f32; 2],
    /// Texture coordinates (v already flipped for the bottom-up images).
    pub uv: [f32; 2],
    /// Colour in the shaders' terms (channel / 255; glyph alpha doubled).
    pub color: [f32; 4],
}

/// One draw: quads (corners in index order 0, 1, 3, 3, 1, 2) sharing an image.
#[derive(Debug, Clone, PartialEq)]
pub struct Draw {
    pub shading: Shading,
    pub image: Option<Image>,
    pub quads: Vec<[Vertex; 4]>,
}

/// Quads a reused mesh instance takes before a new one starts (a first quad, then an upgraded instance
/// counted from 3 up to 12).
const RUN_LIMIT: usize = 11;
/// Textures one glyph group holds, and quads per texture.
const GROUP_TEXTURES: usize = 6;
const GROUP_QUADS: usize = 12;

/// An open glyph group: its draws (indices into the list) and each draw's kind (0 shadow pass, 1 font pass).
#[derive(Debug, Default, Clone)]
struct Group {
    members: Vec<(usize, u8)>,
}

/// The draw list of one frame.
#[derive(Debug, Default, Clone)]
pub struct Canvas {
    pub draws: Vec<Draw>,
    /// The draw the last ungrouped quad went to, while it may take more.
    run: Option<usize>,
    group: Option<Group>,
    /// Scissor rectangle in PS2 units (x0, y0, x1, y1); none = no clipping.
    scissor: Option<[f32; 4]>,
}

/// The corners of a box as the PS2 code passes them: top-left and bottom-right, each with texture coordinates
/// (in 0–1 of the image) and a colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Corner {
    pub x: f32,
    pub y: f32,
    pub u: f32,
    pub v: f32,
    pub rgba: Rgba,
}

/// How the quad bridge colours a box: per column (left = first colour) or per row (top = first colour).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Columns,
    Rows,
}

fn shade(c: Rgba) -> [f32; 4] {
    c.map(|x| f32::from(x) / 255.0)
}

impl Canvas {
    pub fn new() -> Canvas {
        Canvas::default()
    }

    /// Clip everything after this to the rectangle (PS2 units, x, y, width, height).
    pub fn set_scissor(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.scissor = Some([x, y, x + w, y + h]);
    }

    /// Close an open glyph group: its shadow draws move ahead of its font draws (each in first-use order).
    fn close_group(&mut self) {
        let Some(g) = self.group.take() else { return };
        let Some(first) = g.members.iter().map(|m| m.0).min() else { return };
        let mut taken: Vec<(Draw, u8)> = g
            .members
            .iter()
            .map(|&(i, k)| (std::mem::replace(&mut self.draws[i], empty_draw()), k))
            .collect();
        let mut order: Vec<Draw> = Vec::with_capacity(taken.len());
        for kind in [0, 1] {
            for (d, k) in &mut taken {
                if *k == kind {
                    order.push(std::mem::replace(d, empty_draw()));
                }
            }
        }
        for (i, d) in order.into_iter().enumerate() {
            self.draws[first + i] = d;
        }
    }

    /// The draw an ungrouped quad on `image` goes to.
    fn run_draw(&mut self, shading: Shading, image: Option<Image>) -> usize {
        self.close_group();
        if let Some(i) = self.run
            && self.draws[i].image == image
            && self.draws[i].shading == shading
            && self.draws[i].quads.len() < RUN_LIMIT
        {
            return i;
        }
        self.draws.push(Draw { shading, image, quads: Vec::new() });
        self.run = Some(self.draws.len() - 1);
        self.draws.len() - 1
    }

    /// The draw a glyph on `image` goes to (`kind` 0 for shadow passes, 1 for font passes).
    fn glyph_draw(&mut self, image: Image, kind: u8) -> usize {
        self.run = None;
        if let Some(g) = &self.group {
            if let Some(&(i, _)) =
                g.members.iter().find(|(i, _)| self.draws[*i].image.as_ref() == Some(&image))
            {
                if self.draws[i].quads.len() < GROUP_QUADS {
                    return i;
                }
                self.close_group();
            } else if g.members.len() >= GROUP_TEXTURES {
                self.close_group();
            }
        }
        self.draws.push(Draw { shading: Shading::Font, image: Some(image), quads: Vec::new() });
        let i = self.draws.len() - 1;
        self.group.get_or_insert_with(Group::default).members.push((i, kind));
        i
    }

    /// Clip a box against the scissor, adjusting its texture coordinates; false when nothing is left.
    fn clip(&self, a: &mut Corner, b: &mut Corner) -> bool {
        let Some([sx0, sy0, sx1, sy1]) = self.scissor else { return true };
        if b.x <= sx0 || a.x > sx1 || a.y > sy1 || b.y <= sy0 {
            return false;
        }
        if a.x < sx0 && sx0 < b.x {
            a.u += (b.u - a.u) * ((sx0 - a.x) / (b.x - a.x));
            a.x = sx0;
        }
        if a.x < sx1 && sx1 < b.x {
            b.u -= (b.u - a.u) * ((b.x - sx1) / (b.x - a.x));
            b.x = sx1;
        }
        if a.y < sy0 && sy0 < b.y {
            a.v += (b.v - a.v) * ((sy0 - a.y) / (b.y - a.y));
            a.y = sy0;
        }
        if a.y < sy1 && sy1 < b.y {
            b.v -= (b.v - a.v) * ((b.y - sy1) / (b.y - a.y));
            b.y = sy1;
        }
        true
    }

    /// The quad bridge for a box in PS2 units: corners (x0, y1) (x1, y1) (x1, y0) (x0, y0).
    pub fn quad(&mut self, mut a: Corner, mut b: Corner, image: Option<Image>, fill: Fill) {
        if !self.clip(&mut a, &mut b) {
            return;
        }
        let hx = |x: f32| x * SCALE_X - HD_W / 2.0;
        let hy = |y: f32| y * SCALE_Y - HD_H / 2.0;
        let (c0, c1, c2, c3) = match fill {
            Fill::Columns => (a.rgba, b.rgba, b.rgba, a.rgba),
            Fill::Rows => (b.rgba, b.rgba, a.rgba, a.rgba),
        };
        let quad = [
            Vertex { pos: [hx(a.x), hy(b.y)], uv: [a.u, 1.0 - b.v], color: shade(c0) },
            Vertex { pos: [hx(b.x), hy(b.y)], uv: [b.u, 1.0 - b.v], color: shade(c1) },
            Vertex { pos: [hx(b.x), hy(a.y)], uv: [b.u, 1.0 - a.v], color: shade(c2) },
            Vertex { pos: [hx(a.x), hy(a.y)], uv: [a.u, 1.0 - a.v], color: shade(c3) },
        ];
        let shading = if image.is_some() { Shading::Textured } else { Shading::Plain };
        let d = self.run_draw(shading, image);
        self.draws[d].quads.push(quad);
    }

    /// The glyph bridge: a glyph box in PS2 units, offset by (`sx`, `sy`) HD units and snapped to whole units;
    /// its width is scaled like heights so glyphs keep their shape. `tint` multiplies the bottom (`.0`) and top
    /// (`.1`) colours (0xRRGGBBAA words).
    pub fn glyph(
        &mut self,
        mut a: Corner,
        mut b: Corner,
        image: Image,
        offset: [f32; 2],
        tint: (u32, u32),
        kind: u8,
    ) {
        if !self.clip(&mut a, &mut b) {
            return;
        }
        let round = |v: f32| (f64::from(v) + 0.5).trunc() as f32;
        let x0 = round(a.x * SCALE_X + offset[0]);
        let x1 = round(x0 + (b.x - a.x) * SCALE_Y);
        let y0 = round(a.y * SCALE_Y + offset[1]);
        let y1 = round(b.y * SCALE_Y + offset[1]);
        let (xa, xb, ya, yb) = (x0 - HD_W / 2.0, x1 - HD_W / 2.0, y0 - HD_H / 2.0, y1 - HD_H / 2.0);
        let col = |c: Rgba, t: u32| {
            let [tr, tg, tb, ta] = t.to_be_bytes();
            let ch = |x: u8, m: u8| f32::from(x) / 255.0 * (f32::from(m) / 255.0);
            [
                ch(c[0], tr),
                ch(c[1], tg),
                ch(c[2], tb),
                f32::from(c[3]) * 2.0 / 255.0 * (f32::from(ta) * 2.0 / 255.0),
            ]
        };
        let quad = [
            Vertex { pos: [xa, yb], uv: [a.u, 1.0 - b.v], color: col(a.rgba, tint.0) },
            Vertex { pos: [xb, yb], uv: [b.u, 1.0 - b.v], color: col(b.rgba, tint.0) },
            Vertex { pos: [xb, ya], uv: [b.u, 1.0 - a.v], color: col(b.rgba, tint.1) },
            Vertex { pos: [xa, ya], uv: [a.u, 1.0 - a.v], color: col(a.rgba, tint.1) },
        ];
        let d = self.glyph_draw(image, kind);
        self.draws[d].quads.push(quad);
    }

    /// Finish the frame (closes an open glyph group).
    pub fn finish(&mut self) {
        self.close_group();
        self.run = None;
    }

    // ---- Menu primitives (the PS2 menu code's box, gradient and image helpers). ----

    /// A plain box of one colour.
    pub fn fill_box(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        let c = rgba(color);
        self.quad(
            Corner { x, y, u: 0.0, v: 0.0, rgba: c },
            Corner { x: x + w, y: y + h, u: 0.0, v: 0.0, rgba: c },
            None,
            Fill::Columns,
        );
    }

    /// A box shaded from `top` to `bottom`.
    pub fn gradient_box(&mut self, x: f32, y: f32, w: f32, h: f32, top: u32, bottom: u32) {
        self.quad(
            Corner { x, y, u: 0.0, v: 0.0, rgba: rgba(top) },
            Corner { x: x + w, y: y + h, u: 0.0, v: 0.0, rgba: rgba(bottom) },
            None,
            Fill::Rows,
        );
    }

    /// An image rectangle with texture coordinates in 0–1 and colours `left`/`right`.
    #[allow(clippy::too_many_arguments)]
    pub fn image_box(
        &mut self,
        image: Image,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        uv: [f32; 4],
        left: u32,
        right: u32,
    ) {
        self.quad(
            Corner { x, y, u: uv[0], v: uv[1], rgba: rgba(left) },
            Corner { x: x + w, y: y + h, u: uv[2], v: uv[3], rgba: rgba(right) },
            Some(image),
            Fill::Columns,
        );
    }

    /// The window plate in its style 10 (message windows): a drop shadow, the stone background, a purple
    /// gradient and a nine-slice border with 7-unit corners. Nothing is drawn narrower than 14 HD units.
    /// `alt_colors` picks the second gradient (a menu-side flag).
    pub fn plate_10(&mut self, x: f32, y: f32, w: f32, h: f32, alt_colors: bool) {
        if w < hd_to_ps2_x(14.0) {
            return;
        }
        self.fill_box(x + hd_to_ps2_x(6.0), y + hd_to_ps2_y(6.0), w, h, 0x401a_1a1a);
        let (hw, hh) = (w * HD_W / PS2_W, h * HD_H / PS2_H);
        self.image_box(
            Image::Stone,
            x,
            y,
            w,
            h,
            [0.0, 0.0, hw / 256.0, hh / 256.0],
            0x4080_8080,
            0x4080_8080,
        );
        if alt_colors {
            self.gradient_box(x, y, w, h, 0x6034_0614, 0x6030_0410);
        } else {
            self.gradient_box(x, y, w, h, 0x608f_2c45, 0x6050_1826);
        }
        // Border: corners 7 × 7 HD units, edges stretched between them; texture columns 385/393/441/450 and
        // rows 415/424/471/480 of the 1024-texel window image.
        let (cw, ch) = (hd_to_ps2_x(7.0), hd_to_ps2_y(7.0));
        let us = [385.0 / 1024.0, 393.0 / 1024.0, 441.0 / 1024.0, 450.0 / 1024.0];
        let vs = [415.0 / 1024.0, 424.0 / 1024.0, 471.0 / 1024.0, 480.0 / 1024.0];
        let xs = [x, x + cw, x + w - cw, x + w];
        let ys = [y, y + ch, y + h - ch, y + h];
        // Left column (top, middle, bottom), then the top and bottom middles, then the right column.
        let pieces = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 2), (2, 0), (2, 1), (2, 2)];
        for (c, r) in pieces {
            self.image_box(
                Image::Meswin,
                xs[c],
                ys[r],
                xs[c + 1] - xs[c],
                ys[r + 1] - ys[r],
                [us[c], vs[r], us[c + 1], vs[r + 1]],
                0x8080_8080,
                0x8080_8080,
            );
        }
    }
}

fn empty_draw() -> Draw {
    Draw { shading: Shading::Plain, image: None, quads: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn plate_geometry_and_batches() {
        let mut c = Canvas::new();
        c.plate_10(229.0, 196.0, 54.0, 54.0, false);
        c.finish();
        // Shadow, stone, gradient, one border draw of 8 pieces.
        let kinds: Vec<(Shading, usize)> = c.draws.iter().map(|d| (d.shading, d.quads.len())).collect();
        assert_eq!(
            kinds,
            [(Shading::Plain, 1), (Shading::Textured, 1), (Shading::Plain, 1), (Shading::Textured, 8)]
        );
        // Shadow offset by 6 HD units both ways; the box spans 54 PS2 units.
        let s = c.draws[0].quads[0];
        assert!(close(s[3].pos[0], (229.0 + 6.0 * 512.0 / 1920.0) * 3.75 - 960.0));
        assert!(close(s[3].pos[1], (196.0 + 6.0 * 416.0 / 1080.0) * SCALE_Y - 540.0));
        // Gradient: the first colour on top (corners 2 and 3), the second at the bottom.
        let g = c.draws[2].quads[0];
        assert_eq!(g[3].color, shade([0x45, 0x2c, 0x8f, 0x60]));
        assert_eq!(g[0].color, shade([0x26, 0x18, 0x50, 0x60]));
        // Border top-left corner: 7 × 7 HD units at the box corner, its texels flipped vertically.
        let tl = c.draws[3].quads[0];
        assert!(close(tl[3].pos[0], 229.0 * 3.75 - 960.0) && close(tl[1].pos[0] - tl[0].pos[0], 7.0));
        assert!(close(tl[3].uv[1], 1.0 - 415.0 / 1024.0));
    }

    #[test]
    fn glyphs_snap_and_group_shadows_first() {
        let mut c = Canvas::new();
        let corner = |x: f32, y: f32, u: f32| Corner { x, y, u, v: 0.0, rgba: [0x80; 4] };
        let page = |p: u8, shadow: bool| Image::Font { page: p, shadow };
        for (x, p) in [(245.0, 0), (249.0, 0), (253.0, 1)] {
            c.glyph(
                corner(x, 207.78, 0.0),
                corner(x + 6.825, 221.82, 0.1),
                page(p, true),
                [2.0, 2.0],
                (!0, !0),
                0,
            );
            c.glyph(
                corner(x, 207.78, 0.0),
                corner(x + 6.825, 221.82, 0.1),
                page(p, false),
                [0.0, 0.0],
                (!0, !0),
                1,
            );
        }
        c.finish();
        let order: Vec<(u8, bool, usize)> = c
            .draws
            .iter()
            .map(|d| match d.image {
                Some(Image::Font { page, shadow }) => (page, shadow, d.quads.len()),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(order, [(0, true, 2), (1, true, 1), (0, false, 2), (1, false, 1)]);
        // Snapped: x0 = round(245·3.75 + 2), x1 = round(x0 + 6.825·2.596), y = round(y·2.596 + 2).
        let q = c.draws[0].quads[0];
        assert_eq!((q[0].pos[0], q[1].pos[0]), (921.0 - 960.0, 939.0 - 960.0));
        assert_eq!((q[2].pos[1], q[0].pos[1]), (541.0 - 540.0, 578.0 - 540.0));
        // Alpha 0x80 × tint alpha 0xff, both doubled.
        assert!(close(q[0].color[3], 128.0 * 2.0 / 255.0 * (255.0 * 2.0 / 255.0)));
    }

    #[test]
    fn runs_split_by_image_and_size() {
        let mut c = Canvas::new();
        for _ in 0..13 {
            c.fill_box(0.0, 0.0, 1.0, 1.0, 0x8080_8080);
        }
        c.image_box(Image::Meswin, 0.0, 0.0, 1.0, 1.0, [0.0; 4], 0, 0);
        c.fill_box(0.0, 0.0, 1.0, 1.0, 0x8080_8080);
        c.finish();
        let sizes: Vec<usize> = c.draws.iter().map(|d| d.quads.len()).collect();
        assert_eq!(sizes, [11, 2, 1, 1]);
    }

    #[test]
    fn scissor_clips_texture_coordinates() {
        let mut c = Canvas::new();
        c.set_scissor(10.0, 0.0, 100.0, 100.0);
        c.image_box(Image::Meswin, 0.0, 0.0, 20.0, 10.0, [0.0, 0.0, 1.0, 1.0], 0x8080_8080, 0x8080_8080);
        c.image_box(Image::Meswin, 200.0, 0.0, 20.0, 10.0, [0.0, 0.0, 1.0, 1.0], 0x8080_8080, 0x8080_8080);
        c.finish();
        assert_eq!(c.draws.len(), 1);
        let q = c.draws[0].quads[0];
        assert!(close(q[0].pos[0], 10.0 * 3.75 - 960.0) && close(q[0].uv[0], 0.5));
    }
}
