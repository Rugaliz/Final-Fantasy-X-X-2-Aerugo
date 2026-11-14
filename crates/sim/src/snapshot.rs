//! Per-tick snapshots of what should be on screen, and interpolation between two of them.
//!
//! The simulation publishes one [`Snapshot`] per tick. The renderer keeps the previous and current one and
//! blends matching items (same id) by the elapsed fraction of the tick. Items marked discontinuous, or
//! present in only one snapshot, are shown as they are in the current snapshot.

/// Position, rotation (unit quaternion `[x, y, z, w]`) and scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
}

impl Transform {
    pub const IDENTITY: Transform =
        Transform { position: [0.0; 3], rotation: [0.0, 0.0, 0.0, 1.0], scale: [1.0; 3] };

    /// Rotation of `angle` radians about the z axis.
    pub fn with_rotation_z(mut self, angle: f32) -> Self {
        let (s, c) = (angle * 0.5).sin_cos();
        self.rotation = [0.0, 0.0, s, c];
        self
    }

    pub fn lerp(&self, other: &Transform, t: f32) -> Transform {
        Transform {
            position: lerp3(self.position, other.position, t),
            rotation: nlerp(self.rotation, other.rotation, t),
            scale: lerp3(self.scale, other.scale, t),
        }
    }

    /// Angle about the z axis (for 2D use).
    pub fn rotation_z(&self) -> f32 {
        let [_, _, z, w] = self.rotation;
        2.0 * z.atan2(w)
    }
}

/// One drawable thing in a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Stable identity across ticks.
    pub id: u32,
    pub transform: Transform,
    pub color: [f32; 4],
    /// Do not blend from the previous snapshot (teleport, cut, newly created).
    pub discontinuous: bool,
}

/// Everything visible after one simulation tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub tick: u64,
    /// Sorted by id (see [`Snapshot::sort`]).
    pub items: Vec<Item>,
}

impl Snapshot {
    pub fn sort(&mut self) {
        self.items.sort_by_key(|i| i.id);
    }
}

/// Blend `prev` → `cur` at `alpha` in `[0, 1]`. Both snapshots must be sorted by id.
pub fn interpolate(prev: &Snapshot, cur: &Snapshot, alpha: f32, out: &mut Vec<Item>) {
    out.clear();
    let mut p = prev.items.iter().peekable();
    for item in &cur.items {
        while p.next_if(|i| i.id < item.id).is_some() {}
        let blended = match p.peek() {
            Some(old) if old.id == item.id && !item.discontinuous => Item {
                id: item.id,
                transform: old.transform.lerp(&item.transform, alpha),
                color: lerp4(old.color, item.color, alpha),
                discontinuous: false,
            },
            _ => item.clone(),
        };
        out.push(blended);
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)]
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t), lerp(a[3], b[3], t)]
}

/// Normalized linear quaternion blend along the shorter arc. Accurate enough for the small per-tick
/// rotations involved here.
pub fn nlerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let dot: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
    let b = if dot < 0.0 { b.map(|v| -v) } else { b };
    let q = lerp4(a, b, t);
    let len = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    if len > 0.0 { q.map(|v| v / len) } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u32, x: f32, discontinuous: bool) -> Item {
        Item {
            id,
            transform: Transform { position: [x, 0.0, 0.0], ..Transform::IDENTITY },
            color: [1.0; 4],
            discontinuous,
        }
    }

    fn snap(tick: u64, items: Vec<Item>) -> Snapshot {
        let mut s = Snapshot { tick, items };
        s.sort();
        s
    }

    #[test]
    fn blends_matching_items_only() {
        let prev = snap(1, vec![item(1, 0.0, false), item(2, 0.0, false), item(9, 5.0, false)]);
        let cur = snap(2, vec![item(1, 10.0, false), item(2, 10.0, true), item(3, 7.0, false)]);
        let mut out = Vec::new();
        interpolate(&prev, &cur, 0.25, &mut out);
        assert_eq!(out.len(), 3, "items only in the previous snapshot disappear");
        assert_eq!(out[0].transform.position[0], 2.5);
        assert_eq!(out[1].transform.position[0], 10.0, "discontinuous items snap");
        assert_eq!(out[2].transform.position[0], 7.0, "new items appear unblended");
    }

    #[test]
    fn rotation_takes_the_short_way() {
        let a = Transform::IDENTITY.with_rotation_z(170f32.to_radians());
        let b = Transform::IDENTITY.with_rotation_z(-170f32.to_radians());
        let mid = a.lerp(&b, 0.5).rotation_z().to_degrees();
        assert!((mid.abs() - 180.0).abs() < 0.5, "{mid}");
    }
}
