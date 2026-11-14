//! Offscreen render test. Skips (passes with a note) when no Vulkan 1.3 device is available, e.g. on CI
//! machines without a GPU or software rasterizer.

use aerugo_gfx::{Context, ContextOptions, Quad, offscreen};

fn context() -> Option<Context> {
    match Context::new_headless(&ContextOptions { validation: true, preferred_gpu: None }) {
        Ok(ctx) => Some(ctx),
        Err(e) => {
            eprintln!("skipping: no usable Vulkan device ({e:#})");
            None
        }
    }
}

#[test]
fn quad_covers_center_and_clear_color_remains_in_corner() {
    let Some(ctx) = context() else { return };
    let quad =
        Quad { center: [0.0, 0.0], half_size: [0.25, 0.25], rotation: 0.0, color: [1.0, 0.0, 0.0, 1.0] };
    let (w, h) = (64u32, 48u32);
    let pixels = offscreen::render_quads(&ctx, w, h, [0.0, 0.0, 1.0, 1.0], &[quad]).expect("render");
    assert_eq!(pixels.len(), (w * h * 4) as usize);
    let px = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    assert_eq!(px(w / 2, h / 2), [255, 0, 0, 255], "center should be the quad");
    assert_eq!(px(0, 0), [0, 0, 255, 255], "corner should be the clear color");
    // The virtual square is fitted to the shorter side (48 px), so the quad spans 12 px across.
    assert_eq!(px(w / 2 + 8, h / 2), [0, 0, 255, 255], "quad must not stretch horizontally");
}

#[test]
fn rotated_quad_moves_its_corners() {
    let Some(ctx) = context() else { return };
    let base = Quad { center: [0.0, 0.0], half_size: [0.5, 0.1], rotation: 0.0, color: [0.0, 1.0, 0.0, 1.0] };
    let rotated = Quad { rotation: std::f32::consts::FRAC_PI_2, ..base };
    let size = 64u32;
    let a = offscreen::render_quads(&ctx, size, size, [0.0; 4], &[base]).expect("render");
    let b = offscreen::render_quads(&ctx, size, size, [0.0; 4], &[rotated]).expect("render");
    let green = |p: &[u8], x: u32, y: u32| p[((y * size + x) * 4 + 1) as usize] == 255;
    // Horizontal bar: a point left of center is covered; after a quarter turn it is not, but one above is.
    assert!(green(&a, 16, 32));
    assert!(!green(&b, 16, 32));
    assert!(green(&b, 32, 16));
}

#[test]
fn positive_y_is_up() {
    let Some(ctx) = context() else { return };
    let quad = Quad { center: [0.0, 0.5], half_size: [0.2, 0.2], rotation: 0.0, color: [1.0, 1.0, 1.0, 1.0] };
    let size = 64u32;
    let p = offscreen::render_quads(&ctx, size, size, [0.0; 4], &[quad]).expect("render");
    let white = |x: u32, y: u32| p[((y * size + x) * 4) as usize] == 255;
    // Image row 0 is the top of the output.
    assert!(white(32, 16), "quad at +y must be in the upper half");
    assert!(!white(32, 48));
}
