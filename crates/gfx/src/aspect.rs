//! Aspect policy for scenes made for one frame shape (pre-rendered 2D backgrounds) shown at another.
//!
//! The scene is projected for its design aspect, then a clip-space transform shows part of that frame (crop)
//! or all of it with bars (fit). Cropping never shows anything outside the designed frame; the crop window can
//! follow a point.

/// How a design frame is shown in a viewport of another shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AspectMode {
    /// Fill the viewport, cutting off the frame's excess.
    Crop,
    /// Show the whole frame with bars.
    Fit,
}

/// Clip-space scale and offset: `clip' = (sx·x + tx·w, sy·y + ty·w, z, w)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipTransform {
    pub sx: f32,
    pub sy: f32,
    pub tx: f32,
    pub ty: f32,
}

impl ClipTransform {
    pub const IDENTITY: ClipTransform = ClipTransform { sx: 1.0, sy: 1.0, tx: 0.0, ty: 0.0 };

    /// Column-major 4×4 matrix to multiply onto a projection (on the left).
    pub fn matrix(&self) -> [f32; 16] {
        [self.sx, 0.0, 0.0, 0.0, 0.0, self.sy, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, self.tx, self.ty, 0.0, 1.0]
    }

    /// The part of the design frame that is visible, in frame coordinates (−1…1): (x0, y0, x1, y1).
    pub fn visible_frame(&self) -> [f32; 4] {
        let x0 = ((-1.0 - self.tx) / self.sx).max(-1.0);
        let x1 = ((1.0 - self.tx) / self.sx).min(1.0);
        let y0 = ((-1.0 - self.ty) / self.sy).max(-1.0);
        let y1 = ((1.0 - self.ty) / self.sy).min(1.0);
        [x0, y0, x1, y1]
    }
}

/// Transform from a frame designed at `design` aspect (width / height) to a viewport of `view` aspect.
/// `follow` is the point the crop window centres on, in frame coordinates (−1…1); the window is clamped so it
/// stays inside the frame.
pub fn clip_transform(design: f32, view: f32, mode: AspectMode, follow: [f32; 2]) -> ClipTransform {
    if !(design > 0.0 && view > 0.0) {
        return ClipTransform::IDENTITY;
    }
    match mode {
        AspectMode::Fit => {
            if view > design {
                ClipTransform { sx: design / view, ..ClipTransform::IDENTITY }
            } else {
                ClipTransform { sy: view / design, ..ClipTransform::IDENTITY }
            }
        }
        AspectMode::Crop => {
            if view > design {
                // Wider viewport: full frame width, part of its height.
                let sy = view / design;
                let ty = (-follow[1] * sy).clamp(-(sy - 1.0), sy - 1.0);
                ClipTransform { sx: 1.0, sy, tx: 0.0, ty }
            } else {
                // Narrower viewport: full frame height, part of its width.
                let sx = design / view;
                let tx = (-follow[0] * sx).clamp(-(sx - 1.0), sx - 1.0);
                ClipTransform { sx, sy: 1.0, tx, ty: 0.0 }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATIOS: [f32; 4] = [4.0 / 3.0, 16.0 / 9.0, 21.0 / 9.0, 32.0 / 9.0];

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn crop_fills_and_stays_inside_the_frame_at_every_ratio() {
        let design = 16.0 / 9.0;
        for view in RATIOS {
            for follow in [[0.0, 0.0], [1.0, 1.0], [-1.0, -1.0], [0.3, -0.7]] {
                let t = clip_transform(design, view, AspectMode::Crop, follow);
                let [x0, y0, x1, y1] = t.visible_frame();
                assert!(x0 >= -1.0 - 1e-5 && x1 <= 1.0 + 1e-5 && y0 >= -1.0 - 1e-5 && y1 <= 1.0 + 1e-5);
                // The visible part of the frame has the viewport's shape (no bars, no stretching).
                let shape = (x1 - x0) / (y1 - y0) * design;
                assert!(approx(shape, view), "view {view}: visible shape {shape}");
            }
        }
    }

    #[test]
    fn crop_follows_within_limits() {
        let t = clip_transform(16.0 / 9.0, 32.0 / 9.0, AspectMode::Crop, [0.0, 1.0]);
        // Following the top edge shows the top of the frame.
        assert!(approx(t.visible_frame()[3], 1.0));
        let t = clip_transform(16.0 / 9.0, 4.0 / 3.0, AspectMode::Crop, [-1.0, 0.0]);
        assert!(approx(t.visible_frame()[0], -1.0));
        let centred = clip_transform(16.0 / 9.0, 4.0 / 3.0, AspectMode::Crop, [0.0, 0.0]);
        let [x0, _, x1, _] = centred.visible_frame();
        assert!(approx(x0, -x1));
    }

    #[test]
    fn fit_shows_the_whole_frame() {
        for view in RATIOS {
            let t = clip_transform(16.0 / 9.0, view, AspectMode::Fit, [0.5, 0.5]);
            assert_eq!(t.visible_frame(), [-1.0, -1.0, 1.0, 1.0]);
            assert!(t.sx <= 1.0 && t.sy <= 1.0);
        }
        assert_eq!(
            clip_transform(16.0 / 9.0, 16.0 / 9.0, AspectMode::Crop, [1.0, 1.0]),
            ClipTransform::IDENTITY
        );
    }
}
