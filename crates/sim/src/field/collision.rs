//! Walkable ground from a map's triangles. The original keeps separate collision data in the PS2 map package;
//! until that is read, the render geometry stands in: a character stays on the floor below it and cannot climb
//! more than a step.

use aerugo_formats::mesh::{self, Model};
use aerugo_formats::phyre::Cluster;

/// Highest step a character climbs or drops in one move (map units; Y points down).
const STEP: f32 = 12.0;
/// The same for a walk mesh, where steep slopes are still walkable.
const WALK_MESH_STEP: f32 = 40.0;
const CELL: f32 = 64.0;

/// Triangles bucketed on a grid over X/Z.
#[derive(Debug, Clone, Default)]
pub struct Ground {
    tris: Vec<[[f32; 3]; 3]>,
    min: [f32; 2],
    dims: [usize; 2],
    cells: Vec<Vec<u32>>,
    /// How far above or below the current height a floor still counts as the one to walk on.
    step: f32,
}

impl Ground {
    /// Build from triangles in map space.
    pub fn new(tris: Vec<[[f32; 3]; 3]>) -> Ground {
        if tris.is_empty() {
            return Ground::default();
        }
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for t in &tris {
            for p in t {
                lo = [lo[0].min(p[0]), lo[1].min(p[2])];
                hi = [hi[0].max(p[0]), hi[1].max(p[2])];
            }
        }
        let dims = [
            (((hi[0] - lo[0]) / CELL) as usize + 1).min(1024),
            (((hi[1] - lo[1]) / CELL) as usize + 1).min(1024),
        ];
        let mut cells = vec![Vec::new(); dims[0] * dims[1]];
        for (i, t) in tris.iter().enumerate() {
            let (mut a, mut b) = ([f32::MAX; 2], [f32::MIN; 2]);
            for p in t {
                a = [a[0].min(p[0]), a[1].min(p[2])];
                b = [b[0].max(p[0]), b[1].max(p[2])];
            }
            let cx = |x: f32| (((x - lo[0]) / CELL) as usize).min(dims[0] - 1);
            let cz = |z: f32| (((z - lo[1]) / CELL) as usize).min(dims[1] - 1);
            for x in cx(a[0])..=cx(b[0]) {
                for z in cz(a[1])..=cz(b[1]) {
                    cells[z * dims[0] + x].push(i as u32);
                }
            }
        }
        Ground { tris, min: lo, dims, cells, step: STEP }
    }

    /// Triangles of every mesh of a map model, placed in game space (the model is in view space, turned half a
    /// turn around X: Y and Z flip). This is the drawn geometry, not the original's walk mesh, so floors are close
    /// but not exact.
    pub fn from_model(cluster: &Cluster, model: &Model) -> Ground {
        let mut tris = Vec::new();
        for m in &model.meshes {
            let w = &m.world;
            let place = |p: [f32; 3]| {
                [
                    w[0] * p[0] + w[4] * p[1] + w[8] * p[2] + w[12],
                    -(w[1] * p[0] + w[5] * p[1] + w[9] * p[2] + w[13]),
                    -(w[2] * p[0] + w[6] * p[1] + w[10] * p[2] + w[14]),
                ]
            };
            for seg in &m.segments {
                let Some(pos) = seg.positions() else { continue };
                let f = mesh::stream_floats(cluster, pos);
                let n = match pos.format {
                    mesh::VertexFormat::F32x2 => 2,
                    mesh::VertexFormat::F32x3 => 3,
                    _ => 4,
                };
                let vert = |i: u32| {
                    let i = i as usize * n;
                    place([
                        f.get(i).copied().unwrap_or(0.0),
                        f.get(i + 1).copied().unwrap_or(0.0),
                        f.get(i + 2).copied().unwrap_or(0.0),
                    ])
                };
                for t in mesh::segment_indices(cluster, seg).chunks_exact(3) {
                    tris.push([vert(t[0]), vert(t[1]), vert(t[2])]);
                }
            }
        }
        Ground::new(tris)
    }

    /// The walk mesh of a PS2 map package (`mapout.vpa`): the section the `MAP1` header names at +0x18 holds a
    /// scale (+0xC, tenths), int16 vertices (offset at +0x18; x, y, z, pad) and triangle groups (offset at +0x1C,
    /// count at +6; each 0x10 bytes with a triangle count at +8 and the triangles' offset at +0xC; triangles are
    /// 0x10 bytes starting with three vertex indices). Positions are vertices divided by the scale.
    pub fn from_walk_mesh(vpa: &[u8]) -> Option<Ground> {
        let u32_at =
            |d: &[u8], at: usize| d.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let i16_at =
            |d: &[u8], at: usize| d.get(at..at + 2).map(|b| i16::from_le_bytes(b.try_into().unwrap()));
        if !vpa.starts_with(b"MAP1") {
            return None;
        }
        let d = vpa.get(u32_at(vpa, 0x18)? as usize..)?;
        let scale = f32::from_bits(u32_at(d, 0xC)?) / 10.0;
        if scale <= 0.0 || !scale.is_finite() {
            return None;
        }
        let verts = u32_at(d, 0x18)? as usize;
        let groups = u32_at(d, 0x1C)? as usize;
        let count = i16_at(d, 6)?.max(0) as usize;
        let vertex = |i: i16| -> Option<[f32; 3]> {
            let at = verts + usize::try_from(i).ok()? * 8;
            Some([
                f32::from(i16_at(d, at)?) / scale,
                f32::from(i16_at(d, at + 2)?) / scale,
                f32::from(i16_at(d, at + 4)?) / scale,
            ])
        };
        let mut tris = Vec::new();
        for g in 0..count {
            let at = groups + g * 0x10;
            let n = i16_at(d, at + 8)?.max(0) as usize;
            let base = u32_at(d, at + 0xC)? as usize;
            for t in 0..n {
                let at = base + t * 0x10;
                tris.push([
                    vertex(i16_at(d, at)?)?,
                    vertex(i16_at(d, at + 2)?)?,
                    vertex(i16_at(d, at + 4)?)?,
                ]);
            }
        }
        // Every triangle of a walk mesh is walkable (only its open edges are walls), steep slopes too.
        (!tris.is_empty()).then(|| Ground { step: WALK_MESH_STEP, ..Ground::new(tris) })
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// Floor heights (Y, down positive) under (x, z).
    pub fn heights(&self, x: f32, z: f32) -> impl Iterator<Item = f32> + '_ {
        let cx = ((x - self.min[0]) / CELL).floor();
        let cz = ((z - self.min[1]) / CELL).floor();
        let cell = if self.dims[0] > 0
            && cx >= 0.0
            && cz >= 0.0
            && (cx as usize) < self.dims[0]
            && (cz as usize) < self.dims[1]
        {
            &self.cells[cz as usize * self.dims[0] + cx as usize][..]
        } else {
            &[][..]
        };
        cell.iter().filter_map(move |&i| height_at(&self.tris[i as usize], x, z))
    }

    /// Whether `p` stands on a floor (one within an ordinary step of its height).
    pub fn standing(&self, p: [f32; 3]) -> bool {
        self.heights(p[0], p[2]).any(|h| (h - p[1]).abs() <= STEP)
    }

    /// The floor under `p` within a step of its height.
    pub fn floor(&self, p: [f32; 3]) -> Option<f32> {
        self.heights(p[0], p[2])
            .filter(|&h| (h - p[1]).abs() <= self.step)
            .min_by(|a, b| (a - p[1]).abs().total_cmp(&(b - p[1]).abs()))
    }

    /// Move from `from` to `to` over the ground: the new position on the floor, or `None` when there is no floor
    /// within a step (a wall, a ledge, the edge of the map).
    pub fn slide(&self, from: [f32; 3], to: [f32; 3]) -> Option<[f32; 3]> {
        if self.is_empty() {
            return Some(to);
        }
        let h = self.floor([to[0], from[1], to[2]])?;
        Some([to[0], h, to[2]])
    }
}

/// Height of a triangle's plane at (x, z) when the point lies inside it (in X/Z).
fn height_at(t: &[[f32; 3]; 3], x: f32, z: f32) -> Option<f32> {
    let [a, b, c] = t;
    let d = (b[2] - c[2]) * (a[0] - c[0]) + (c[0] - b[0]) * (a[2] - c[2]);
    if d.abs() < 1e-6 {
        return None;
    }
    let l1 = ((b[2] - c[2]) * (x - c[0]) + (c[0] - b[0]) * (z - c[2])) / d;
    let l2 = ((c[2] - a[2]) * (x - c[0]) + (a[0] - c[0]) * (z - c[2])) / d;
    let l3 = 1.0 - l1 - l2;
    (l1 >= -1e-4 && l2 >= -1e-4 && l3 >= -1e-4).then(|| l1 * a[1] + l2 * b[1] + l3 * c[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(y: f32, x0: f32, x1: f32) -> Vec<[[f32; 3]; 3]> {
        vec![[[x0, y, 0.0], [x1, y, 0.0], [x1, y, 100.0]], [[x0, y, 0.0], [x1, y, 100.0], [x0, y, 100.0]]]
    }

    #[test]
    fn walks_floors_and_stops_at_walls_and_edges() {
        // Floor at y=0 for x in 0..100, a raised floor (y=-50, a wall's height above) for x in 100..200.
        let mut tris = quad(0.0, 0.0, 100.0);
        tris.extend(quad(-50.0, 100.0, 200.0));
        let g = Ground::new(tris);
        assert_eq!(g.slide([10.0, 0.0, 10.0], [20.0, 0.0, 10.0]), Some([20.0, 0.0, 10.0]));
        assert_eq!(g.slide([90.0, 0.0, 10.0], [110.0, 0.0, 10.0]), None, "too high to climb");
        assert_eq!(g.slide([10.0, 0.0, 10.0], [-10.0, 0.0, 10.0]), None, "off the map");
        // A small step is climbed.
        let mut tris = quad(0.0, 0.0, 100.0);
        tris.extend(quad(-5.0, 100.0, 200.0));
        let g = Ground::new(tris);
        assert_eq!(g.slide([90.0, 0.0, 10.0], [110.0, 0.0, 10.0]), Some([110.0, -5.0, 10.0]));
    }
}
