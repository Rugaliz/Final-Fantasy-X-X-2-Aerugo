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

/// The page set (shadow pages or not) and tint of colour `index` for a font (`shadow` false) or shadow pass.
fn tint(index: u8, shadow: bool) -> (bool, (u32, u32)) {
    let wrapped = usize::from(index & 7);
    if shadow || index > 7 { (true, SHADOW_TINT[wrapped]) } else { (false, FONT_TINT[wrapped]) }
}

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
    /// The screen size the layout is made for.
    width: f32,
    height: f32,
}

impl Metrics {
    /// Metrics for a `width` × `height` output, for the Western languages.
    pub fn new(width: u32, height: u32) -> Metrics {
        let k = (width as f32) * (1.0 / 512.0);
        let ky = height as f32 / 416.0;
        let spacing = round(k * 0.7) / k;
        Metrics { k, ky, spacing, y_offset: 1.0, width: width as f32, height: height as f32 }
    }

    /// The pen snapped to whole render pixels.
    fn snap(&self, x: f32) -> f32 {
        round(x * self.k) / self.k
    }

    /// How far a glyph of `width` texels moves the pen.
    fn advance(&self, width: f32, scale: f32) -> f32 {
        round(width * scale * self.ky) / self.k + self.spacing
    }

    /// Frame type 1 windows are laid out for the PS2's 4:3 screen and drawn into the widescreen: their measured
    /// advances grow by the screen's aspect over the PS2 frame's, and their text starts at a mapped x.
    fn widescreen_scale(&self) -> f32 {
        (self.width / self.height) / 1.230_769_3
    }

    fn widescreen_x(&self, x: f32) -> f32 {
        let fit = self.height / 416.0;
        (self.width - fit * 512.0) / (self.k + self.k) + x * (fit / self.k)
    }
}

/// How a window's text is laid out, from its flags (native 009D): the text type (low nibble; 2 and 3 are the
/// dialogue layouts) and the size type (0x2000, with 0x8000: 1 or 3 draw glyphs full size with an outline).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextKind {
    pub text_type: u16,
    pub size_type: u16,
}

impl TextKind {
    pub fn from_flags(flags: u16) -> TextKind {
        let text_type = flags & 0xf;
        let wide = if flags & 0x8000 != 0 { 3 } else { 1 };
        let size_type = if text_type == 3 || flags & 0x2000 != 0 { wide } else { 0 };
        TextKind { text_type, size_type }
    }

    fn full_size(self) -> bool {
        matches!(self.size_type, 1 | 3)
    }

    fn scale(self) -> f32 {
        if self.full_size() { 1.0 } else { GLYPH_SCALE }
    }

    /// Line advance: 16, or 12.48 in the dialogue layouts at the small size.
    fn line(self) -> f32 {
        if self.text_type >= 2 && !self.full_size() { 12.48 } else { LINE }
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
pub fn measure(bytes: &[u8], font: &Ftc, m: &Metrics, kind: TextKind, frame_type: usize) -> (i32, i32) {
    let wide = if frame_type == 1 { m.widescreen_scale() } else { 1.0 };
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
            0x07 => match bytes.get(i + 1) {
                Some(&p @ 0x40..0xc0) => line = f32::from(p) * 4.0 - 256.0,
                Some(&p @ 0xe1..0xf0) => line += f32::from(p - 0xe0),
                _ => {}
            },
            _ => {
                if let Some((index, _)) = glyph(&bytes[i..]) {
                    line += m.advance(font.width(index).unwrap_or(0.0), kind.scale()) * wide;
                }
            }
        }
        i += step(&bytes[i..]).max(1);
    }
    widest = widest.max(line);
    max_height = max_height.max(height);
    (widest as i32, max_height as i32 + 2)
}

/// The width of the text from `bytes` to the end of its line (how centred and right-aligned lines are placed).
fn line_width(bytes: &[u8], font: &Ftc, m: &Metrics, kind: TextKind, wide: f32) -> f32 {
    let end = bytes.iter().position(|&b| b <= 3).unwrap_or(bytes.len());
    let mut width = 0.0;
    let mut i = 0;
    while i < end {
        match bytes[i] {
            0x07 => match bytes.get(i + 1) {
                Some(&p @ 0x40..0xc0) => width = f32::from(p) * 4.0 - 256.0,
                Some(&p @ 0xe1..0xf0) => width += f32::from(p - 0xe0),
                _ => {}
            },
            _ => {
                if let Some((index, _)) = glyph(&bytes[i..]) {
                    width += m.advance(font.width(index).unwrap_or(0.0), kind.scale()) * wide;
                }
            }
        }
        i += step(&bytes[i..]).max(1);
    }
    width
}

/// Where a window sits: its frame and text rectangles (x, y, width, height in PS2 units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub frame: [f32; 4],
    pub text: [f32; 4],
}

/// Place a window whose text measures `text_w` × `text_h` at the script's position: `align` 1 bottom, 2 right,
/// 3 bottom right, 4 centre, else top left; kept on the 512 × 416 screen.
pub fn place(
    m: &Metrics,
    frame_type: usize,
    align: i32,
    x: i32,
    y: i32,
    text_w: i32,
    text_h: i32,
) -> Placement {
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
    // On screens 720 tall and more the window also stays inside a safe area (frame type 1 measures it from its
    // text, the others from the frame). Frame type 1's horizontal check works in screen space and is not ported.
    if m.height >= 720.0 {
        let (ins_l, ins_t) = if frame_type == 1 { (l, t) } else { (0, 0) };
        if ((y + ins_t) as f32) < 31.199_995 {
            y = (31.199_995 - ins_t as f32) as i32;
        } else if ((y + h - ins_t) as f32) > 384.800_02 {
            y = (384.800_02 - h as f32 + ins_t as f32) as i32;
        }
        if frame_type != 1 {
            if ((x + ins_l) as f32) < 38.399_994 {
                x = (38.399_994 - ins_l as f32) as i32;
            } else if ((x + w - ins_l) as f32) > 473.6 {
                x = (473.6 - w as f32 + ins_l as f32) as i32;
            }
        }
    }
    let (fx, fy, fw, fh) = (x as f32, y as f32, w as f32, h as f32);
    let text_x = fx + l as f32;
    let text_x = if frame_type == 1 { m.widescreen_x(text_x) } else { text_x };
    Placement {
        frame: [fx, fy, fw, fh],
        text: [text_x, fy + t as f32, fw - (l + r) as f32, fh - (t + b) as f32],
    }
}

/// What a window draws this frame.
#[derive(Debug)]
pub struct WindowDraw<'a> {
    pub bytes: &'a [u8],
    pub placement: Placement,
    pub frame_type: usize,
    pub kind: TextKind,
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
    let [tx, ty, tw, _] = w.placement.text;
    let wide = if w.frame_type == 1 { m.widescreen_scale() } else { 1.0 };
    let scale = w.kind.scale();
    // The outline of full-size text: the glyph on the shadow pages at ten offsets.
    let outline_div = (m.width / 1920.0).max(1.0);
    const OUTLINE: [(f32, f32); 10] = [
        (-2.0, -2.0),
        (0.0, -2.0),
        (2.0, -2.0),
        (-1.0, 0.0),
        (-2.0, 0.0),
        (1.0, 0.0),
        (2.0, 0.0),
        (-2.0, 2.0),
        (0.0, 2.0),
        (2.0, 2.0),
    ];
    let (img_w, img_h) = (f32::from(font.image_w), f32::from(font.image_h));
    let (mut x, mut y) = (tx, ty);
    // Text colour byte: low nibble the font colour, high nibble the shadow colour.
    let mut color = 0u8;
    // {07:38}: every line is centred.
    let mut centre_all = false;
    let mut choices: Vec<(f32, f32)> = Vec::new();
    let mut drawn = 0;
    let mut i = 0;
    while i < w.bytes.len() {
        let b = w.bytes[i];
        match b {
            0 => break,
            1..=3 => {
                x = tx;
                y += w.kind.line();
                if centre_all {
                    x = tx + (tw - line_width(&w.bytes[i + 1..], font, m, w.kind, wide)) * 0.5;
                }
            }
            0x07 => {
                let rest = w.bytes.get(i + 2..).unwrap_or_default();
                match w.bytes.get(i + 1) {
                    Some(&p @ 0xe0..0xf0) => x += f32::from(p - 0xe0),
                    Some(b'1') => x = tx + (tw - line_width(rest, font, m, w.kind, wide)) * 0.5,
                    Some(b'8') => {
                        centre_all = true;
                        x = tx + (tw - line_width(rest, font, m, w.kind, wide)) * 0.5;
                    }
                    Some(b'2') => x = tx + tw - line_width(rest, font, m, w.kind, wide),
                    Some(&p @ 0x40..0xc0) => x = tx + f32::from(p) * 4.0 - 256.0,
                    _ => {}
                }
            }
            0x0a => {
                if let Some(&p) = w.bytes.get(i + 1) {
                    if p & 0xf != 0 {
                        color = color & 0xf0 | ((p & 0xf) - 1);
                    }
                    if p & 0xf0 > 0x3f {
                        color = color & 0xf | ((p & 0xf0) - 0x40);
                    }
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
                    let a = corner(x, y + scale * m.y_offset, cx / img_w, cy / img_h);
                    let z = corner(
                        x + width * scale,
                        y + (18.0 + m.y_offset) * scale,
                        (cx + width) / img_w,
                        (cy + 18.0) / img_h,
                    );
                    // Colours 8 and up draw from the shadow pages with the shadow tints.
                    let (font_shadowed, font_tint) = tint(color & 0xf, false);
                    let (_, shadow_tint) = tint(color >> 4, true);
                    let shadow = Image::Font { page, shadow: true };
                    if w.kind.full_size() {
                        for (dx, dy) in OUTLINE {
                            c.glyph(
                                a,
                                z,
                                shadow.clone(),
                                [dx / outline_div, dy / outline_div],
                                shadow_tint,
                                0,
                            );
                        }
                    } else {
                        c.glyph(a, z, shadow, [2.0, 2.0], shadow_tint, 0);
                    }
                    let face = Image::Font { page, shadow: font_shadowed };
                    c.glyph(a, z, face, [0.0, 0.0], font_tint, 1);
                    x = m.snap(x) + m.advance(width, scale);
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
        let (w, h) = measure(&text, &f, &m, TextKind::from_flags(0), 0);
        assert_eq!(h, 34);
        let expect: f32 = [0x52u8, 0x70, 0x7d, 0x72, 0x74, 0x7b]
            .iter()
            .map(|&b| m.advance(f.width(u32::from(b - 0x30)).unwrap(), GLYPH_SCALE))
            .sum();
        assert_eq!(w, expect as i32);
        // Centred on (256, 224) in a frame-type-0 window: insets 20/10 each side.
        let p = place(&m, 0, 4, 256, 224, 22, 34);
        assert_eq!(p.frame, [225.0, 197.0, 62.0, 54.0]);
        assert_eq!(p.text, [245.0, 207.0, 22.0, 34.0]);
        let mut c = Canvas::new();
        draw(
            &mut c,
            &f,
            &m,
            &WindowDraw {
                bytes: &text,
                placement: p,
                frame_type: 0,
                kind: TextKind::from_flags(0),
                shown: 99,
                choice: Some(1),
                time: 0.0,
            },
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
        // Below 720 lines only the PS2 frame's bounds apply.
        let m = Metrics::new(640, 480);
        assert_eq!(place(&m, 2, 2, 300, 100, 40, 18).frame, [228.0, 100.0, 72.0, 34.0]);
        assert_eq!(place(&m, 2, 3, 300, 100, 40, 18).frame, [228.0, 66.0, 72.0, 34.0]);
        assert_eq!(place(&m, 2, 0, 500, 410, 40, 18).frame, [440.0, 378.0, 72.0, 34.0]);
        assert_eq!(place(&m, 3, 0, -5, 0, 40, 18).frame, [0.0, 4.0, 40.0, 18.0]);
        // At 1080 lines the safe area pulls them in.
        let m = Metrics::new(1920, 1080);
        assert_eq!(place(&m, 2, 0, 500, 410, 40, 18).frame, [401.0, 350.0, 72.0, 34.0]);
        assert_eq!(place(&m, 3, 0, -5, 0, 40, 18).frame, [38.0, 31.0, 40.0, 18.0]);
    }

    /// The Zanarkand fans' subtitle (frame type 1, centred at 256, 448): measured 202 × 18, the frame pulled up
    /// into the safe area, the text start mapped into the widescreen.
    #[test]
    fn subtitle_placement() {
        let m = Metrics::new(1920, 1080);
        let p = place(&m, 1, 4, 256, 448, 202, 18);
        assert_eq!(p.frame, [123.0, 354.0, 266.0, 42.0]);
        assert!((p.text[0] - 186.077).abs() < 1e-3, "{}", p.text[0]);
        assert_eq!(&p.text[1..], &[366.0, 202.0, 18.0]);
    }

    #[test]
    fn reveal_limits_glyphs() {
        let text = [0x62, 0x70, 0x85, 0x74];
        let f = font();
        let m = Metrics::new(1920, 1080);
        let p = place(&m, 0, 0, 100, 100, 20, 18);
        let mut c = Canvas::new();
        draw(
            &mut c,
            &f,
            &m,
            &WindowDraw {
                bytes: &text,
                placement: p,
                frame_type: 3,
                kind: TextKind::from_flags(0),
                shown: 2,
                choice: None,
                time: 0.0,
            },
            false,
        );
        let glyphs: usize = c.draws.iter().map(|d| d.quads.len()).sum();
        assert_eq!(glyphs, 2 * 2);
    }
}
