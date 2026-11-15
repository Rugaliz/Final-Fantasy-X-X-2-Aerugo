//! Player control: the stick (or d-pad) turned into a walk. Each direction's push ramps up by 32 a tick towards
//! its target (256 for a key, the stick's reach past a dead band), the pushes add up to a strength of 0..256
//! (nothing below 48), and the speed is the walk or run speed scaled by that strength. A character covers
//! speed / 20 a tick, from the tick after the push.

/// Push gained or lost per tick, in 1/256 of full.
const RAMP: i32 = 32;
/// Strength below which the player stands still.
const DEAD_STRENGTH: f32 = 48.0;
/// Stick values closer to the centre than this (of 256) count as centred.
const DEAD_BAND: f32 = 96.0;

/// Walking and running speeds (scripts can change them; running is the default, walking while cancel is held),
/// and the speed in water.
pub const WALK_SPEED: f32 = 16.0;
pub const RUN_SPEED: f32 = 38.0;
pub const SWIM_SPEED: f32 = 20.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerControl {
    /// Ramped pushes towards right, left, forward, back (0..256).
    pushes: [i32; 4],
    pub walk_speed: f32,
    pub run_speed: f32,
    pub swim_speed: f32,
}

impl Default for PlayerControl {
    fn default() -> PlayerControl {
        PlayerControl { pushes: [0; 4], walk_speed: WALK_SPEED, run_speed: RUN_SPEED, swim_speed: SWIM_SPEED }
    }
}

/// What one tick of control asks for: a direction on the screen (x right, y forward, unit length) and the
/// distance to cover next tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Push {
    pub dir: [f32; 2],
    pub step: f32,
}

impl PlayerControl {
    /// One tick: `stick` is −1..1 per axis (x right, y forward; keys give ±1), `walk` slows to the walking speed,
    /// `swim` uses the speed in water instead.
    pub fn tick(&mut self, stick: [f32; 2], walk: bool, swim: bool) -> Option<Push> {
        let reach = |v: f32| {
            let v = (v * 256.0).clamp(-256.0, 256.0);
            if v.abs() < DEAD_BAND { 0.0 } else { v }
        };
        let (x, y) = (reach(stick[0]), reach(stick[1]));
        let targets = [x.max(0.0), (-x).max(0.0), y.max(0.0), (-y).max(0.0)];
        for (p, t) in self.pushes.iter_mut().zip(targets) {
            let t = t as i32;
            *p = if t == 0 { (*p - RAMP).max(0) } else { (*p + RAMP).min(t) };
        }
        let [r, l, f, b] = self.pushes.map(|p| p as f32);
        let (vx, vy) = (r - l, f - b);
        let strength = (vx * vx + vy * vy).sqrt().min(256.0);
        if strength < DEAD_STRENGTH {
            return None;
        }
        let speed = if swim {
            self.swim_speed
        } else if walk {
            self.walk_speed
        } else {
            self.run_speed
        } * strength
            / 256.0;
        let len = (vx * vx + vy * vy).sqrt();
        Some(Push { dir: [vx / len, vy / len], step: speed / 20.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_key_ramps_up_to_the_run_speed() {
        let mut c = PlayerControl::default();
        // 32 is below the dead strength; then 64, 96, ... up to 256 after eight ticks.
        assert_eq!(c.tick([1.0, 0.0], false, false), None);
        let steps: Vec<f32> = (0..8).map(|_| c.tick([1.0, 0.0], false, false).unwrap().step).collect();
        assert!((steps[0] - 38.0 * 64.0 / 256.0 / 20.0).abs() < 1e-6);
        assert!((steps[6] - 38.0 / 20.0).abs() < 1e-6);
        assert_eq!(steps[6], steps[7]);
        // Letting go ramps down by the same amount.
        let p = c.tick([0.0, 0.0], false, false).unwrap();
        assert!((p.step - 38.0 * 224.0 / 256.0 / 20.0).abs() < 1e-6);
    }

    #[test]
    fn a_stick_inside_the_dead_band_does_nothing() {
        let mut c = PlayerControl::default();
        for _ in 0..10 {
            assert_eq!(c.tick([0.3, -0.3], false, false), None);
        }
    }

    #[test]
    fn walking_uses_the_walk_speed() {
        let mut c = PlayerControl::default();
        let mut last = None;
        for _ in 0..10 {
            last = c.tick([0.0, 1.0], true, false);
        }
        let p = last.unwrap();
        assert_eq!(p.dir, [0.0, 1.0]);
        assert!((p.step - 16.0 / 20.0).abs() < 1e-6);
    }
}
