//! What field entities react to: characters the player touches or talks to, lines the player crosses, and zones
//! (around a line, or a box) the player enters and leaves. The engine tests each entity after its worker's turn
//! and raises signal types that become requests on that worker.

/// Signal types.
pub const TALK: u8 = 0;
pub const TOUCH: u8 = 3;
pub const CROSS: u8 = 4;
pub const ENTER: u8 = 5;
pub const LEAVE: u8 = 6;
/// Inside a zone (raised every tick while there); the same type as a touch.
pub const INSIDE: u8 = 3;

/// An entity's trigger shape and state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trigger {
    /// Tested at all (characters by default; lines and boxes once a script sets them up).
    pub active: bool,
    /// Test again even if the player has not moved (set when a script changes the shape).
    pub dirty: bool,
    /// The player was inside last tick (zones) or touching (characters).
    pub inside: bool,
    /// The player's last position was inside the box itself (not its margin).
    pub in_box: bool,
    /// Side of the last crossing: 1 or −1.
    pub side: i8,
    /// A line's second point, or a box's half width (x) and half depth (z).
    pub b: [f32; 3],
    /// Half height of the test (a line crossing's height tolerance); not above 0 means none.
    pub height: f32,
    /// Height window for talking; below 0 means none.
    pub talk_height: f32,
    /// Half angle of the player's facing cone for talking.
    pub talk_angle: f32,
    pub talk_radius: f32,
    /// Touch radius (characters) or zone margin (lines and boxes).
    pub radius: f32,
}

impl Trigger {
    /// The setup defaults for a worker kind.
    pub fn for_kind(kind: u8) -> Trigger {
        let base = Trigger {
            active: false,
            dirty: false,
            inside: false,
            in_box: false,
            side: 0,
            b: [0.0; 3],
            height: -1.0,
            talk_height: -1.0,
            talk_angle: std::f32::consts::FRAC_PI_3,
            talk_radius: 0.0,
            radius: 1.0,
        };
        match kind {
            1 => Trigger { active: true, height: 5.0, talk_radius: 2.0, radius: 1.5, ..base },
            _ => base,
        }
    }
}

impl Default for Trigger {
    fn default() -> Trigger {
        Trigger::for_kind(0)
    }
}

/// The signal bits a worker of `kind` starts with (characters do not report touches until a script asks).
pub fn default_signal_bits(kind: u8) -> u32 {
    match kind {
        1 => 0xF7,
        2 | 3 | 5 | 6 => 0xFF,
        _ => 0,
    }
}

/// Twice the signed area of (a, b, p) on the ground plane.
fn orient(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

fn xz(p: [f32; 3]) -> [f32; 2] {
    [p[0], p[2]]
}

/// Whether the step `p0 → p1` crosses the line `a → b` on the ground plane: the side it crossed to (1 or −1), or
/// 0.
pub fn crossing(p0: [f32; 3], p1: [f32; 3], a: [f32; 3], b: [f32; 3]) -> i8 {
    let (p0, p1, a2, b2) = (xz(p0), xz(p1), xz(a), xz(b));
    let ob = orient(p0, p1, b2);
    if orient(p0, p1, a2) * ob < 0.0 && orient(a2, b2, p0) * orient(a2, b2, p1) < 0.0 {
        if ob > 0.0 { 1 } else { -1 }
    } else {
        0
    }
}

/// Height difference between the step and the line where they cross.
pub fn crossing_height(p0: [f32; 3], p1: [f32; 3], a: [f32; 3], b: [f32; 3]) -> f32 {
    let (dx, dz) = (p1[0] - p0[0], p1[2] - p0[2]);
    let (ex, ez) = (b[0] - a[0], b[2] - a[2]);
    let (fx, fz) = (a[0] - p0[0], a[2] - p0[2]);
    let det = dx * ez - ex * dz;
    if det == 0.0 {
        return -1.0;
    }
    let u = (fx * dz - dx * fz) / det;
    let t = if dx == 0.0 { (ez * u + fz) / dz } else { (ex * u + fx) / dx };
    (((b[1] - a[1]) * u + a[1]) - ((p1[1] - p0[1]) * t + p0[1])).abs()
}

/// The point of segment `a → b` nearest to `p`.
fn nearest_on_segment(p: [f32; 3], a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    let d = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1] + (p[2] - a[2]) * d[2]) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    [a[0] + d[0] * t, a[1] + d[1] * t, a[2] + d[2] * t]
}

/// The player for one test: where it was last tick and is now.
#[derive(Debug, Clone, Copy)]
pub struct PlayerStep {
    pub prev: [f32; 3],
    pub cur: [f32; 3],
}

impl PlayerStep {
    fn moved(&self) -> bool {
        self.prev[0] != self.cur[0] || self.prev[2] != self.cur[2]
    }
}

/// Zone signals from whether the player is inside now, was inside, and crossed the edge this tick.
fn zone_signals(inside: bool, was_inside: bool, crossed: bool, out: &mut Vec<u8>) {
    if inside {
        out.push(INSIDE);
        if !was_inside {
            out.push(ENTER);
        }
    } else {
        if crossed && !was_inside {
            out.push(ENTER);
        }
        if crossed || was_inside {
            out.push(LEAVE);
        }
    }
}

/// A character (kind 1) at `pos`: touched while the player stands within its radius and height.
pub fn test_character(t: &mut Trigger, pos: [f32; 3], step: PlayerStep, out: &mut Vec<u8>) {
    t.dirty = false;
    let p = step.cur;
    let mut touching = false;
    if (p[1] - pos[1]).abs() < t.height {
        let d2 = (p[0] - pos[0]).powi(2) + (p[2] - pos[2]).powi(2);
        touching = d2 <= t.radius * t.radius;
        if touching {
            out.push(TOUCH);
        }
    }
    t.inside = touching;
}

/// A line the player crosses (kind 5): from `a` (the entity's position) to `t.b`.
pub fn test_line(t: &mut Trigger, a: [f32; 3], step: PlayerStep, out: &mut Vec<u8>) {
    if step.moved() || t.dirty {
        t.dirty = false;
        if let Some(side) = crossed_within_height(t, a, step) {
            t.side = side;
            out.push(CROSS);
        }
    }
}

fn crossed_within_height(t: &Trigger, a: [f32; 3], step: PlayerStep) -> Option<i8> {
    let side = crossing(step.prev, step.cur, a, t.b);
    if side == 0 {
        return None;
    }
    let h = crossing_height(step.prev, step.cur, a, t.b);
    (t.height <= 0.0 || h <= t.height).then_some(side)
}

/// A zone around a line (kind 2): crossing it, and being within the margin of it.
pub fn test_line_zone(t: &mut Trigger, a: [f32; 3], step: PlayerStep, out: &mut Vec<u8>) {
    let mut crossed = false;
    if step.moved() || t.dirty {
        t.dirty = false;
        if let Some(side) = crossed_within_height(t, a, step) {
            t.side = side;
            crossed = true;
            out.push(CROSS);
        }
    }
    let p = step.cur;
    let q = nearest_on_segment(p, a, t.b);
    let d2 = (q[0] - p[0]).powi(2) + (q[2] - p[2]).powi(2);
    let outside =
        t.radius * t.radius < d2 || (t.height > 0.0 && (p[1] > q[1] + t.height || p[1] < q[1] - t.height));
    let was_inside = t.inside;
    t.inside = !outside;
    zone_signals(t.inside, was_inside, crossed, out);
}

/// Where `p` lies against a box of half sizes `hw`, `hd` (and half height `h` when above 0), in box space:
/// outcode bits (1 +x, 2 −x, 4 +z, 8 −z, 0x10 above, 0x20 below) and the point clamped to the box.
fn box_outcode(local: [f32; 3], hw: f32, hd: f32, h: f32) -> (u8, [f32; 2]) {
    let mut code = 0;
    let mut c = [local[0], local[2]];
    if local[0] >= hw {
        code |= 1;
        c[0] = hw;
    }
    if local[0] <= -hw {
        code |= 2;
        c[0] = -hw;
    }
    if local[2] >= hd {
        code |= 4;
        c[1] = hd;
    }
    if local[2] <= -hd {
        code |= 8;
        c[1] = -hd;
    }
    if h > 0.0 {
        if local[1] >= h {
            code |= 0x10;
        }
        if local[1] <= -h {
            code |= 0x20;
        }
    }
    (code, c)
}

/// A box zone (kind 3) centred on `centre`, turned by `yaw`: entering, leaving, being inside, and crossing.
pub fn test_box(t: &mut Trigger, centre: [f32; 3], yaw: f32, step: PlayerStep, out: &mut Vec<u8>) {
    t.dirty = false;
    let (s, c) = (-yaw).sin_cos();
    let local = |p: [f32; 3]| {
        let (dx, dy, dz) = (p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]);
        [c * dx - s * dz, dy, dx * s + dz * c]
    };
    let (hw, hd, h) = (t.b[0], t.b[2], t.height);
    let (prev, cur) = (local(step.prev), local(step.cur));
    let (code0, _) = box_outcode(prev, hw, hd, h);
    let (code1, clamped) = box_outcode(cur, hw, hd, h);
    let crossed = code0 & code1 == 0 && (code0 | code1) != 0;
    if crossed && !t.in_box {
        out.push(CROSS);
    }
    t.in_box = code1 == 0;
    let inside = if code1 == 0 {
        true
    } else if h <= 0.0 || (cur[1] <= h && -h <= cur[1]) {
        let d2 = (cur[0] - clamped[0]).powi(2) + (cur[2] - clamped[1]).powi(2);
        d2 <= t.radius * t.radius
    } else {
        false
    };
    let was_inside = t.inside;
    t.inside = inside;
    zone_signals(inside, was_inside, crossed, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_a_line_reports_the_side() {
        let (a, b) = ([0.0, 0.0, -5.0], [0.0, 0.0, 5.0]);
        assert_eq!(
            crossing([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], a, b),
            crossing([-1.0, 0.0, 1.0], [1.0, 0.0, 1.0], a, b)
        );
        assert_eq!(
            crossing([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], a, b),
            -crossing([1.0, 0.0, 0.0], [-1.0, 0.0, 0.0], a, b)
        );
        assert_ne!(crossing([-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], a, b), 0);
        assert_eq!(crossing([-1.0, 0.0, 6.0], [1.0, 0.0, 6.0], a, b), 0);
        assert_eq!(crossing([-2.0, 0.0, 0.0], [-1.0, 0.0, 0.0], a, b), 0);
    }

    #[test]
    fn a_line_signals_once_when_crossed() {
        let mut t = Trigger { active: true, b: [0.0, 0.0, 5.0], ..Trigger::for_kind(5) };
        let a = [0.0, 0.0, -5.0];
        let mut out = Vec::new();
        test_line(&mut t, a, PlayerStep { prev: [-2.0, 0.0, 0.0], cur: [-1.0, 0.0, 0.0] }, &mut out);
        assert!(out.is_empty());
        test_line(&mut t, a, PlayerStep { prev: [-1.0, 0.0, 0.0], cur: [1.0, 0.0, 0.0] }, &mut out);
        assert_eq!(out, [CROSS]);
    }

    #[test]
    fn a_box_reports_enter_inside_and_leave() {
        let mut t = Trigger { active: true, b: [2.0, 0.0, 2.0], radius: 0.0, ..Trigger::for_kind(3) };
        let mut out = Vec::new();
        let step = |prev: [f32; 3], cur: [f32; 3]| PlayerStep { prev, cur };
        test_box(&mut t, [0.0; 3], 0.0, step([-4.0, 0.0, 0.0], [-3.0, 0.0, 0.0]), &mut out);
        assert!(out.is_empty());
        test_box(&mut t, [0.0; 3], 0.0, step([-3.0, 0.0, 0.0], [-1.0, 0.0, 0.0]), &mut out);
        assert_eq!(out, [CROSS, INSIDE, ENTER]);
        out.clear();
        test_box(&mut t, [0.0; 3], 0.0, step([-1.0, 0.0, 0.0], [0.0, 0.0, 0.0]), &mut out);
        assert_eq!(out, [INSIDE]);
        out.clear();
        test_box(&mut t, [0.0; 3], 0.0, step([0.0, 0.0, 0.0], [3.0, 0.0, 0.0]), &mut out);
        assert_eq!(out, [LEAVE]);
    }
}
