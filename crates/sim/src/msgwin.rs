//! Message windows on the 2D layer: text measured and placed as the PS2 text module does, the window frame,
//! the text with its shadow, and the choice cursor, drawn into a [`Canvas`].
//!
//! Text metrics snap to the output resolution (the original measures in render pixels), so a layout depends on
//! the screen size it is made for.

use aerugo_formats::ftc::{self, Ftc};

use crate::ui2d::{Canvas, Corner, Image, hd_to_ps2_x, hd_to_ps2_y};

/// Frame insets (left, top, right, bottom) by frame type (the script's window type, native 0066).
const INSETS: [[i32; 4]; 8] = [
    [20, 10, 20, 10],
    [32, 12, 32, 12],
    [16, 8, 16, 8],
    [0, 0, 0, 0],
    [16, 8, 16, 8],
    [16, 8, 16, 8],
    [16, 8, 16, 8],
    [16, 8, 16, 8],
];

/// Text colours (font tint bottom/top, shadow tint bottom/top; 0xRRGGBBAA) by colour index.
const FONT_TINT: [(u32, u32); 8] = [
    (0x9d9d_9dff, 0xffff_ffff),
    (0x6666_66ff, 0x8c8c_8cff),
    (0xff7e_00ff, 0xffd8_00ff),
    (0x9d14_0dff, 0xff43_24ff),
    (0x9d9d_9dff, 0xffff_ffff),
    (0x0000_00ff, 0x3333_33ff),
    (0xdc3d_b1ff, 0xff8b_e0ff),
    (0x00c1_d8ff, 0x39ec_f7ff),
];
const SHADOW_TINT: [(u32, u32); 8] = [
    (0x1a1a_1aff, 0x1a1a_1aff),
    (0x1a1a_1aff, 0x1a1a_1aff),
    (0xcccc_ccff, 0xcccc_ccff),
    (0x0041_0fff, 0xf200_00ff),
    (0x162b_79ff, 0x162b_79ff),
    (0x3908_08ff, 0x3908_08ff),
    (0x2108_7dff, 0x2108_7dff),
    (0x0071_88ff, 0x0071_88ff),
];

/// Line height of the text layout.
const LINE: f32 = 16.0;
/// Glyph scale of ordinary windows (text types other than 1 and 3).
const GLYPH_SCALE: f32 = 0.78;

/// Text metrics for one output size.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    /// Render pixels per PS2 unit across and down.
    k: f32,
    ky: f32,
    /// Space after every glyph.
    spacing: f32,
    /// Glyph rows sit this far below the pen (1 for the Western languages).
    y_offset: f32,
}

impl Metrics {
    /// Metrics for a `width` × `height` output, for the Western languages.
    pub fn new(width: u32, height: u32) -> Metrics {
        let k = (width as f32) * (1.0 / 512.0);
        let ky = height as f32 / 416.0;
        let spacing = round(k * 0.7) / k;
        Metrics { k, ky, spacing, y_offset: 1.0 }
    }

    /// The pen snapped to whole render pixels.
    fn snap(&self, x: f32) -> f32 {
        round(x * self.k) / self.k
    }

    /// How far a glyph of `width` texels moves the pen.
    fn advance(&self, width: f32) -> f32 {
        round(width * GLYPH_SCALE * self.ky) / self.k + self.spacing
    }
}

/// trunc(v + 0.5), as the original converts to whole pixels.
fn round(v: f32) -> f32 {
    (f64::from(v) + 0.5).trunc() as f32
}

/// A glyph of the base font: its index in the font, and how many bytes it took.
fn glyph(bytes: &[u8]) -> Option<(u32, usize)> {
    match *bytes.first()? {
        b @ 0x2c..=0x2f => {
            Some((u32::from(b - 0x2b) * 0xd0 + u32::from(bytes.get(1)?.checked_sub(0x30)?), 2))
        }
        b @ 0x30.. => Some((u32::from(b - 0x30), 1)),
        _ => None,
    }
}

/// Bytes a text element takes (control codes and macros two, page-prefixed glyphs two, others one).
fn step(bytes: &[u8]) -> usize {
    match bytes.first() {
        None | Some(0) => 0,
        Some(4) => {
            if bytes.get(1).is_some_and(|&b| b < 0x30) {
                3
            } else {
                2
            }
        }
        Some(6..=0x0b | 0x0d | 0x0e | 0x10 | 0x12..=0x2f) => 2,
        Some(_) => 1,
    }
}

/// The text's size: the widest line (whole units) and the height (16 per line, plus 2).
pub fn measure(bytes: &[u8], font: &Ftc, m: &Metrics) -> (i32, i32) {
    let (mut widest, mut line, mut height, mut max_height) = (0f32, 0f32, LINE, 0f32);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            0 => break,
            1..=3 => {
                widest = widest.max(line);
                line = 0.0;
                if b == 3 {
                    height += LINE;
                } else {
                    max_height = max_height.max(height);
                    height = LINE;
                }
            }
            0x07 => {
                if let Some(&p) = bytes.get(i + 1)
                    && (0xe1..0xf0).contains(&p)
                {
                    line += f32::from(p - 0xe0);
                }
            }
            _ => {
                if let Some((index, _)) = glyph(&bytes[i..]) {
                    line += m.advance(font.width(index).unwrap_or(0.0));
                }
            }
        }
        i += step(&bytes[i..]).max(1);
    }
    widest = widest.max(line);
    max_height = max_height.max(height);
    (widest as i32, max_height as i32 + 2)
}

/// Where a window sits: its frame and text rectangles (x, y, width, height in PS2 units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub frame: [f32; 4],
    pub text: [f32; 4],
}

/// Place a window whose text measures `text_w` × `text_h` at the script's position: `align` 1 bottom, 2 right,
/// 3 bottom right, 4 centre, else top left; kept on the 512 × 416 screen.
pub fn place(frame_type: usize, align: i32, x: i32, y: i32, text_w: i32, text_h: i32) -> Placement {
    let [l, t, r, b] = INSETS[frame_type.min(7)];
    let mut w = text_w + l + r;
    let h = (text_h + t + b).clamp(4, 416);
    let (mut x, mut y) = (x, y);
    match align {
        1 => y -= h,
        2 => x -= w,
        3 => {
            x -= w;
            y -= h;
        }
        4 => {
            x -= w / 2;
            y -= h / 2;
        }
        _ => {}
    }
    w = w.min(512);
    x = x.clamp(0, 512 - w);
    y = y.min(412 - h).max(4);
    let (fx, fy, fw, fh) = (x as f32, y as f32, w as f32, h as f32);
    Placement {
        frame: [fx, fy, fw, fh],
        text: [fx + l as f32, fy + t as f32, fw - (l + r) as f32, fh - (t + b) as f32],
    }
}

/// What a window draws this frame.
#[derive(Debug)]
pub struct WindowDraw<'a> {
    pub bytes: &'a [u8],
    pub placement: Placement,
    pub frame_type: usize,
    /// Glyphs revealed so far.
    pub shown: usize,
    /// The highlighted choice, for choice windows.
    pub choice: Option<usize>,
    /// Seconds of the graphics clock (the cursor sways with it).
    pub time: f32,
}

/// Draw a window: its frame (frame type 0), its revealed text and the choice cursor.
pub fn draw(c: &mut Canvas, font: &Ftc, m: &Metrics, w: &WindowDraw<'_>, language_jp: bool) {
    let [fx, fy, fw, fh] = w.placement.frame;
    c.set_scissor(0.0, 0.0, 512.0, 416.0);
    if w.frame_type == 0 {
        let y = if language_jp { fy } else { fy - 1.0 };
        c.plate_10(fx + 4.0, y, fw - 8.0, fh, false);
    }
    let [tx, ty, _, _] = w.placement.text;
    let (img_w, img_h) = (f32::from(font.image_w), f32::from(font.image_h));
    let (mut x, mut y) = (tx, ty);
    let mut color = 0usize;
    let mut choices: Vec<(f32, f32)> = Vec::new();
    let mut drawn = 0;
    let mut i = 0;
    while i < w.bytes.len() {
        let b = w.bytes[i];
        match b {
            0 => break,
            1..=3 => {
                x = tx;
                y += LINE;
            }
            0x07 => {
                if let Some(&p) = w.bytes.get(i + 1)
                    && (0xe1..0xf0).contains(&p)
                {
                    x += f32::from(p - 0xe0);
                }
            }
            0x0a => {
                if let Some(&p) = w.bytes.get(i + 1)
                    && p & 0xf != 0
                {
                    color = usize::from((p & 0xf) - 1) % FONT_TINT.len();
                }
            }
            0x10 => {
                if let Some(&p) = w.bytes.get(i + 1)
                    && (0x30..0x50).contains(&p)
                {
                    let k = usize::from(p - 0x30);
                    if choices.len() <= k {
                        choices.resize(k + 1, (0.0, 0.0));
                    }
                    choices[k] = (x - tx, y - ty + LINE);
                }
            }
            _ => {
                if let Some((index, _)) = glyph(&w.bytes[i..])
                    && drawn < w.shown
                {
                    drawn += 1;
                    let width = font.width(index).unwrap_or(0.0);
                    let (page, cx, cy) = ftc::cell(index);
                    let (cx, cy) = (cx as f32, cy as f32);
                    let corner =
                        |px: f32, py: f32, u: f32, v: f32| Corner { x: px, y: py, u, v, rgba: [0x80; 4] };
                    let a = corner(x, y + GLYPH_SCALE * m.y_offset, cx / img_w, cy / img_h);
                    let z = corner(
                        x + width * GLYPH_SCALE,
                        y + (18.0 + m.y_offset) * GLYPH_SCALE,
                        (cx + width) / img_w,
                        (cy + 18.0) / img_h,
                    );
                    let shadow = Image::Font { page, shadow: true };
                    c.glyph(a, z, shadow, [2.0, 2.0], SHADOW_TINT[color], 0);
                    c.glyph(a, z, Image::Font { page, shadow: false }, [0.0, 0.0], FONT_TINT[color], 1);
                    x = m.snap(x) + m.advance(width);
                }
            }
        }
        i += step(&w.bytes[i..]).max(1);
    }
    c.finish();
    if let Some(k) = w.choice
        && let Some(&(cx, cy)) = choices.get(k)
    {
        let (x, y) = (tx + cx, ty + cy - 2.0 - 12.0);
        let x = x - hd_to_ps2_x(68.0) + 3.0 * (w.time * 8.0).cos();
        let y = y - hd_to_ps2_y(5.0);
        c.image_box(
            Image::Meswin,
            x,
            y,
            hd_to_ps2_x(68.0),
            hd_to_ps2_y(45.0),
            [1.0 / 1024.0, 779.0 / 1024.0, 69.0 / 1024.0, 824.0 / 1024.0],
            0x8080_8080,
            0x8080_8080,
        );
        c.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A font whose glyph `i` is `w(i)` quarter texels wide.
    fn font() -> Ftc {
        Ftc { slot: 4, image_w: 128, image_h: 234, widths: (0..230u32).map(|i| 28 + (i % 9) as u8).collect() }
    }

    #[test]
    fn save_choice_layout() {
        // "Save" / "Cancel" as choices 0 and 1, shown at once.
        let text = [
            0x09, 0x30, 0x10, 0x30, 0x62, 0x70, 0x85, 0x74, 0x03, 0x10, 0x31, 0x52, 0x70, 0x7d, 0x72, 0x74,
            0x7b,
        ];
        let f = font();
        let m = Metrics::new(1920, 1080);
        let (w, h) = measure(&text, &f, &m);
        assert_eq!(h, 34);
        let expect: f32 = [0x52u8, 0x70, 0x7d, 0x72, 0x74, 0x7b]
            .iter()
            .map(|&b| m.advance(f.width(u32::from(b - 0x30)).unwrap()))
            .sum();
        assert_eq!(w, expect as i32);
        // Centred on (256, 224) in a frame-type-0 window: insets 20/10 each side.
        let p = place(0, 4, 256, 224, 22, 34);
        assert_eq!(p.frame, [225.0, 197.0, 62.0, 54.0]);
        assert_eq!(p.text, [245.0, 207.0, 22.0, 34.0]);
        let mut c = Canvas::new();
        draw(
            &mut c,
            &f,
            &m,
            &WindowDraw { bytes: &text, placement: p, frame_type: 0, shown: 99, choice: Some(1), time: 0.0 },
            false,
        );
        // Plate (4 draws), glyphs grouped by pass and page, then the cursor.
        assert_eq!(c.draws.len(), 4 + 4 + 1);
        let glyphs: usize = c.draws[4..8].iter().map(|d| d.quads.len()).sum();
        assert_eq!(glyphs, 2 * 10);
        // Cursor at the second choice: its line, 68 HD units left of the text, swaying by 3 at time 0.
        let q = c.draws[8].quads[0];
        let x = 245.0 - hd_to_ps2_x(68.0) + 3.0;
        assert!((q[3].pos[0] - (x * 3.75 - 960.0)).abs() < 1e-3);
        let y = 207.0 + 32.0 - 14.0 - hd_to_ps2_y(5.0);
        assert!((q[3].pos[1] - (y * crate::ui2d::SCALE_Y - 540.0)).abs() < 1e-3);
    }

    #[test]
    fn placement_alignments_and_clamps() {
        assert_eq!(place(2, 2, 300, 100, 40, 18).frame, [228.0, 100.0, 72.0, 34.0]);
        assert_eq!(place(2, 3, 300, 100, 40, 18).frame, [228.0, 66.0, 72.0, 34.0]);
        assert_eq!(place(2, 0, 500, 410, 40, 18).frame, [440.0, 378.0, 72.0, 34.0]);
        assert_eq!(place(3, 0, -5, 0, 40, 18).frame, [0.0, 4.0, 40.0, 18.0]);
    }

    #[test]
    fn reveal_limits_glyphs() {
        let text = [0x62, 0x70, 0x85, 0x74];
        let f = font();
        let m = Metrics::new(1920, 1080);
        let p = place(0, 0, 100, 100, 20, 18);
        let mut c = Canvas::new();
        draw(
            &mut c,
            &f,
            &m,
            &WindowDraw { bytes: &text, placement: p, frame_type: 3, shown: 2, choice: None, time: 0.0 },
            false,
        );
        let glyphs: usize = c.draws.iter().map(|d| d.quads.len()).sum();
        assert_eq!(glyphs, 2 * 2);
    }
}
