//! The screen effect channels: five timed effects the scripts start (fades to and from black and white, a
//! colour fade, a colour filter, motion blur and the accumulation trail), stepped once a frame. Each frame the
//! running channels hand the remaster's screen fade their colour, alpha and blend flags (one fade pass each, in
//! channel order); the motion blur and accumulation switches go to the post-processing.
//!
//! Channel bytes as the original keeps them: mode, red, green, blue, then four shorts: frames left, value, target,
//! step. A channel runs while its bit of the active mask is set; it stops when its frames left reach 0.

/// One channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Channel {
    pub mode: u8,
    pub rgb: [u8; 3],
    pub count: i16,
    pub value: i16,
    pub target: i16,
    pub step: i16,
}

/// One fade pass handed to the remaster this frame: colour (0–1), alpha (the original's scale: 1 = 127/127,
/// white fades go up to 255/127) and the blend flags (0 normal, 0x10).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FadePass {
    pub rgb: [f32; 3],
    pub alpha: f32,
    pub flags: u32,
}

/// The five channels: 0 accumulation alpha, 1 motion blur, 2 colour fade, 3 colour filter, 4 black and white
/// fades.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScreenFx {
    /// Bit i: channel i is running.
    pub active: u32,
    pub channels: [Channel; 5],
    /// The motion blur is on, at this strength (value / 128).
    pub motion_blur: Option<f32>,
    /// The accumulation trail draws from the current frame (4019).
    pub accumulation_current: bool,
    /// The accumulation alpha the trail last took (401a).
    pub accumulation_alpha: Option<i16>,
    /// This frame's fade passes.
    pub passes: Vec<FadePass>,
    /// The scene is 0x102 (an immediate accumulation alpha then also zeroes the step).
    pub mode_0x102: bool,
}

impl ScreenFx {
    /// Fade in from (`fade_in`) or out to black, or white (`white`), over `frames` (4004–4007).
    pub fn fade(&mut self, frames: i16, fade_in: bool, white: bool) {
        self.active |= 0x10;
        let ch = &mut self.channels[4];
        ch.mode = if fade_in { u8::from(white) * 2 + 2 } else { u8::from(white) * 2 + 3 };
        ch.count = frames;
        ch.value = frames;
        ch.target = -1;
        ch.rgb = [0xFF, 0xFF, 0xFF];
    }

    /// A colour fade from the colour to clear over `frames` (4003).
    pub fn colour_fade(&mut self, frames: i16, rgb: [u8; 3]) {
        self.active |= 4;
        let ch = &mut self.channels[2];
        ch.count = frames;
        ch.value = frames;
        ch.rgb = rgb;
        ch.mode = 6;
    }

    /// A colour filter at strength `value` (0 turns it off at the next frame): mode 7 (4008) or 8 (4009).
    pub fn filter(&mut self, rgb: [u8; 3], value: i16, mode: u8) {
        self.active |= 8;
        let ch = &mut self.channels[3];
        ch.mode = 7;
        if value == 0 {
            ch.count = 0;
        } else {
            ch.rgb = rgb;
            ch.value = value;
            ch.count = 1;
        }
        if mode == 8 {
            ch.mode = 8;
        }
    }

    /// Turn the colour filter off at the next frame (400c).
    pub fn filter_off(&mut self) {
        self.channels[3].count = 0;
    }

    /// Motion blur at `value` / 128, or off (400a).
    pub fn blur(&mut self, value: i16) {
        self.active |= 2;
        let ch = &mut self.channels[1];
        ch.mode = 1;
        if value == 0 {
            ch.count = 0;
            ch.value = 0;
            self.motion_blur = None;
        } else {
            ch.count = 1;
            ch.value = value;
            self.motion_blur = Some(f32::from(value) * 0.0078125);
        }
    }

    /// The accumulation alpha moves to `alpha` over `frames` (at once when 0) (401a).
    pub fn accumulation(&mut self, alpha: i16, frames: i16) {
        let scene_0x102 = self.mode_0x102;
        self.active |= 1;
        let ch = &mut self.channels[0];
        ch.mode = 0;
        ch.count = frames;
        if frames == 0 {
            ch.value = alpha;
            if scene_0x102 {
                ch.step = 0;
            }
        } else {
            let d = (i32::from(ch.value) - i32::from(alpha)).abs();
            ch.step = ((d / i32::from(frames)) as i16).max(1);
            ch.target = alpha;
        }
        if ch.value != 0 && ch.count == 0 {
            ch.count = 1;
        }
        self.accumulation_alpha = Some(ch.value);
    }

    /// Clear every channel.
    pub fn clear(&mut self) {
        self.channels = Default::default();
        self.active = 0;
    }

    /// One frame: the fade passes for this frame, and the channels stepped.
    pub fn frame(&mut self) {
        self.passes.clear();
        if self.active == 0 {
            return;
        }
        let mut reset_accumulation = false;
        for i in 0..5 {
            let bit = 1u32 << i;
            if self.active & bit == 0 {
                continue;
            }
            let ch = &mut self.channels[i];
            if ch.count == 0 {
                self.active &= !bit;
                continue;
            }
            let rgb = ch.rgb.map(|v| f32::from(v) * 0.00390625);
            let count = i32::from(ch.count);
            let value = i32::from(ch.value);
            let pass = |alpha: f32, flags: u32| FadePass { rgb, alpha, flags };
            match ch.mode {
                0 => {
                    ch.count -= 1;
                    if ch.target != ch.value {
                        if ch.value < ch.target {
                            ch.value = ch.value.wrapping_add(ch.step);
                            if ch.target < ch.value {
                                ch.value = ch.target;
                            }
                        } else {
                            ch.value = ch.value.wrapping_sub(ch.step);
                            if ch.value < ch.target {
                                ch.value = ch.target;
                            }
                        }
                    }
                    if ch.value != 0 && ch.count == 0 {
                        ch.count = 1;
                    }
                    self.accumulation_alpha = Some(ch.value);
                }
                2 => {
                    ch.count -= 1;
                    let a = 1.0 - (0x7F - (i32::from(ch.count) * 0x7F) / value) as f32 / 127.0;
                    self.passes.push(FadePass { rgb: [0.0; 3], alpha: a, flags: 0 });
                    reset_accumulation = true;
                }
                3 => {
                    ch.count -= 1;
                    let a = if ch.count == 0 {
                        ch.count = 1;
                        0
                    } else {
                        (i32::from(ch.count) * 0x7F) / value
                    };
                    self.passes.push(FadePass { rgb: [0.0; 3], alpha: 1.0 - a as f32 / 127.0, flags: 0 });
                }
                4 => {
                    let a = (count * 0xFF) / value;
                    self.passes.push(pass(a as f32 / 127.0, 0));
                    ch.count -= 1;
                }
                5 => {
                    ch.count -= 1;
                    let a = if ch.count == 0 {
                        ch.count = 1;
                        0xFF
                    } else {
                        0xFF - (i32::from(ch.count) * 0xFF) / value
                    };
                    self.passes.push(pass(a as f32 / 127.0, 0));
                }
                6 => {
                    self.passes.push(pass(((count * 0x7F) / value) as f32 / 127.0, 0));
                    ch.count -= 1;
                }
                7 => self.passes.push(pass(value as f32 / 127.0, 0)),
                8 => self.passes.push(pass(value as f32 / 127.0, 0x10)),
                _ => {}
            }
            if std::mem::take(&mut reset_accumulation) {
                self.accumulation(0, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fade_out_to_black_goes_dark_and_holds() {
        let mut fx = ScreenFx::default();
        fx.fade(4, false, false);
        let mut alphas = vec![];
        for _ in 0..6 {
            fx.frame();
            alphas.push(fx.passes.first().map(|p| p.alpha));
        }
        // Count 3, 2, 1 → 0x7f·count/4 → 95, 63, 31; then held black.
        let expect = [1.0 - 95.0 / 127.0, 1.0 - 63.0 / 127.0, 1.0 - 31.0 / 127.0, 1.0, 1.0, 1.0];
        for (a, e) in alphas.iter().zip(expect) {
            assert!((a.unwrap() - e).abs() < 1e-6, "{alphas:?}");
        }
    }

    #[test]
    fn a_fade_in_from_black_clears_and_stops() {
        let mut fx = ScreenFx::default();
        fx.fade(2, true, false);
        fx.frame();
        assert!((fx.passes[0].alpha - (1.0 - (0x7F - 0x3F) as f32 / 127.0)).abs() < 1e-6);
        fx.frame();
        assert_eq!(fx.passes[0].alpha, 0.0);
        fx.frame();
        assert!(fx.passes.is_empty());
        assert_eq!(fx.active & 0x10, 0);
    }

    #[test]
    fn a_filter_holds_until_turned_off() {
        let mut fx = ScreenFx::default();
        fx.filter([0x80, 0x40, 0], 64, 7);
        for _ in 0..3 {
            fx.frame();
            assert_eq!(fx.passes, [FadePass { rgb: [0.5, 0.25, 0.0], alpha: 64.0 / 127.0, flags: 0 }]);
        }
        fx.filter_off();
        fx.frame();
        assert!(fx.passes.is_empty());
    }
}
