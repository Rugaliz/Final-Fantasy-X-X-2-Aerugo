//! The character engine's walk mesh and ground step, as the original runs them.
//!
//! The mesh is the map package's walk-mesh section (the MAP1 header names it): a scale (+0xc ÷ 10), int16
//! vertices (x, y, z, pad) and the triangles of group 0 (0x10 bytes: three vertex indices, the three
//! neighbours across edges 0–1, 1–2 and 2–0 (−1: none), and an attribute word). Positions are worked in mesh
//! units (game units × scale).
//!
//! A body's ground step, every frame for bodies that follow the ground (flag bit 0x80 of +0x194 clear): its
//! position goes to mesh units (+0x3c); with no triangle yet it finds the one under it (the nearest floor at
//! or below the point among the triangles containing it, else the last one containing it); then it walks
//! towards position + velocity across the triangles' edges (into a neighbour when the neighbour is not closed
//! to the body and its floor is not more than the step height above the body; at most ten crossings),
//! sliding along the crossed edge when it stops (up to three tries); it keeps the triangle (+0x824), its
//! attribute word (+0x828; none: 0xfffe0000), the triangle's vertices (+0x8c…), the ground normal (+0x7c) and
//! the ground height (+0x16c), and takes the new x and z. With no mesh the position moves by the velocity, the
//! ground height is 0 and the normal (0, −1, 0).

use crate::chr::Chr;
use crate::chr::elements::normalize;

/// A walk-mesh triangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tri {
    pub v: [i16; 3],
    pub next: [i16; 3],
    pub attr: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WalkMesh {
    pub scale: f32,
    pub verts: Vec<[i16; 3]>,
    pub tris: Vec<Tri>,
}

/// The body record's fields the ground step uses.
const POS: u32 = 0xc;
const VEL_X: u32 = 0x4c;
const VEL_Z: u32 = 0x54;
const MESH_POS: u32 = 0x3c;
const NORMAL: u32 = 0x7c;
const TRI_VERTS: u32 = 0x8c;
const GROUND: u32 = 0x16c;
const FLAGS: u32 = 0x194;
const TRIANGLE: u32 = 0x824;
const ATTR: u32 = 0x828;
/// The step height in mesh units per scale unit.
const STEP: f32 = 300.0;
const NO_ATTR: u32 = 0xfffe_0000;

fn d(x: f32) -> f64 {
    f64::from(x)
}

/// Which side of the edge a → b the point (x, z) lies on (≤ 0: inside for the triangles' winding).
fn edge(a: [i16; 3], b: [i16; 3], x: f32, z: f32) -> f64 {
    let (ax, az, bx, bz) = (i32::from(a[0]), i32::from(a[2]), i32::from(b[0]), i32::from(b[2]));
    (f64::from(az - bz) * d(x) + f64::from(bx - ax) * d(z) + f64::from(ax * bz)) - f64::from(bx * az)
}

/// Which side of the line from the old position `o` to the target `p` vertex `v` lies on.
fn across(o: [f32; 2], p: [f32; 2], v: [i16; 3]) -> f64 {
    (d(o[0]) * d(p[1]) + (d(p[0]) - d(o[0])) * f64::from(v[2]) + (d(o[1]) - d(p[1])) * f64::from(v[0]))
        - d(o[1]) * d(p[0])
}

impl WalkMesh {
    /// The walk mesh of page `page` (the package's section `page + 2`) of a `mapout.vpa`.
    pub fn from_map_package(vpa: &[u8], page: usize) -> Option<WalkMesh> {
        let u32_at =
            |d: &[u8], at: usize| d.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        let i16_at =
            |d: &[u8], at: usize| d.get(at..at + 2).map(|b| i16::from_le_bytes(b.try_into().unwrap()));
        if !vpa.starts_with(b"MAP1") {
            return None;
        }
        let d = vpa.get(u32_at(vpa, 0x18 + 4 * page).filter(|&o| o != 0)? as usize..)?;
        let scale = (f64::from(f32::from_bits(u32_at(d, 0xc)?)) / 10.0) as f32;
        let verts_at = u32_at(d, 0x18)? as usize;
        let group = u32_at(d, 0x1c)? as usize;
        let count = i16_at(d, group + 8)?.max(0) as usize;
        let tris_at = u32_at(d, group + 0xc)? as usize;
        let mut tris = Vec::with_capacity(count);
        let mut top = 0i16;
        for t in 0..count {
            let at = tris_at + t * 0x10;
            let v = [i16_at(d, at)?, i16_at(d, at + 2)?, i16_at(d, at + 4)?];
            top = top.max(v[0]).max(v[1]).max(v[2]);
            tris.push(Tri {
                v,
                next: [i16_at(d, at + 6)?, i16_at(d, at + 8)?, i16_at(d, at + 0xa)?],
                attr: u32_at(d, at + 0xc)?,
            });
        }
        let verts = (0..=top.max(0) as usize)
            .map(|i| {
                let at = verts_at + i * 8;
                Some([i16_at(d, at)?, i16_at(d, at + 2)?, i16_at(d, at + 4)?])
            })
            .collect::<Option<Vec<_>>>()?;
        Some(WalkMesh { scale, verts, tris })
    }

    fn vert(&self, i: i16) -> [i16; 3] {
        usize::try_from(i).ok().and_then(|i| self.verts.get(i)).copied().unwrap_or([0; 3])
    }

    fn corners(&self, t: usize) -> [[i16; 3]; 3] {
        let v = self.tris[t].v;
        [self.vert(v[0]), self.vert(v[1]), self.vert(v[2])]
    }

    /// The triangle's unit normal: (v2 − v0) × (v1 − v0), normalised.
    pub fn normal(&self, t: usize) -> [f32; 4] {
        let [a, b, c] = self.corners(t);
        let e1 = [0, 1, 2].map(|k| (i32::from(b[k]) - i32::from(a[k])) as f32);
        let e2 = [0, 1, 2].map(|k| (i32::from(c[k]) - i32::from(a[k])) as f32);
        let n = [
            (d(e2[1]) * d(e1[2]) - d(e1[1]) * d(e2[2])) as f32,
            (d(e1[0]) * d(e2[2]) - d(e1[2]) * d(e2[0])) as f32,
            (d(e1[1]) * d(e2[0]) - d(e2[1]) * d(e1[0])) as f32,
            0.0,
        ];
        normalize(n)
    }

    /// The floor height of triangle `t` under (x, z) (mesh units; kept in double precision as the
    /// original's register result).
    pub fn height(&self, x: f32, z: f32, t: usize) -> f64 {
        let n = self.normal(t);
        let a = self.corners(t)[0];
        let top = d(n[2]) * f64::from(a[2]) + d(n[0]) * f64::from(a[0]) + d(n[1]) * f64::from(a[1]);
        (top - (d(z) * d(n[2]) + d(x) * d(n[0]))) / d(n[1])
    }

    fn inside(&self, t: usize, x: f32, z: f32) -> bool {
        let [a, b, c] = self.corners(t);
        edge(a, b, x, z) <= 0.0 && edge(b, c, x, z) <= 0.0 && edge(c, a, x, z) <= 0.0
    }

    /// The triangle under `p` (mesh units): the nearest floor at or below the point among the triangles
    /// containing it, else the last one containing it.
    pub fn find(&self, p: [f32; 3]) -> Option<usize> {
        let (mut best, mut best_h, mut last) = (None, 0i32, None);
        for t in 0..self.tris.len() {
            if !self.inside(t, p[0], p[2]) {
                continue;
            }
            let h = self.height(p[0], p[2], t) as f32 as i32;
            last = Some(t);
            if p[1] <= h as f32 && (best.is_none() || h < best_h) {
                best = Some(t);
                best_h = h;
            }
        }
        best.or(last)
    }

    /// Whether triangle `t` is closed to the body (its attribute's low 7 bits: 1 always, 2 to all but the
    /// player, 0xd by the body's flag bit 0x4 of +0x198, 0xe to the player; 0x30–0x3f through the scripts'
    /// remap table).
    fn closed(&self, t: usize, body: &Chr, is_player: bool, remap: &[u32; 64]) -> bool {
        let mut kind = self.tris[t].attr & 0x7f;
        loop {
            match kind {
                1 => return true,
                2 => return !is_player,
                0xd => return body.u32(0x198) >> 2 & 1 != 0,
                0xe => return is_player,
                0x30..=0x3f => kind = remap[kind as usize],
                _ => return false,
            }
        }
    }
}

/// The walk across the triangles' edges and the edge it stopped at.
struct Walk<'a> {
    mesh: &'a WalkMesh,
    crossings: u32,
    edge: Option<([i16; 3], [i16; 3])>,
}

impl Walk<'_> {
    /// From triangle `t` towards `p` (mesh units): Some(t) once inside a triangle, None when it stopped.
    fn go(&mut self, body: &mut Chr, p: [f32; 3], t: usize, is_player: bool, remap: &[u32; 64]) -> bool {
        let m = self.mesh;
        let tri = m.tris[t];
        let [a, b, c] = m.corners(t);
        let old = [body.f32(MESH_POS), body.f32(MESH_POS + 8)];
        let to = [p[0], p[2]];
        let mut side = 0;
        let mut next = -1i16;
        let mut stop = (a, b);
        let decided = 'pick: {
            if edge(a, b, p[0], p[2]) > 0.0 {
                side = 1;
                next = tri.next[0];
                stop = (a, b);
            }
            if edge(b, c, p[0], p[2]) > 0.0 {
                if side == 1 && across(old, to, b) < 0.0 {
                    break 'pick true;
                }
                next = tri.next[1];
                side = 2;
                stop = (b, c);
            }
            if edge(c, a, p[0], p[2]) <= 0.0 {
                if side == 0 {
                    for (k, v) in [a, b, c].iter().enumerate() {
                        for (j, x) in v.iter().enumerate() {
                            body.set_f32(TRI_VERTS + k as u32 * 0x10 + j as u32 * 4, f32::from(*x));
                        }
                    }
                    body.set_u16(TRIANGLE, t as u16);
                    return true;
                }
                break 'pick true;
            }
            if side == 1 && across(old, to, a) > 0.0 {
                break 'pick true;
            }
            if (side == 1 || side == 2) && across(old, to, c) < 0.0 {
                break 'pick true;
            }
            next = tri.next[2];
            stop = (c, a);
            true
        };
        let _ = decided;
        let open = next != -1 && !m.closed(next as usize, body, is_player, remap);
        if open || next == -1 {
            if self.crossings > 10 || next == -1 {
                self.edge = Some(stop);
                return false;
            }
            let n = next as usize;
            let ny = m.normal(n)[1];
            let step = STEP * m.scale;
            if ny == 0.0 || d(body.f32(MESH_POS + 4) - step) <= m.height(p[0], p[2], n) {
                self.crossings += 1;
                return self.go(body, p, n, is_player, remap);
            }
        }
        self.edge = Some(stop);
        false
    }
}

/// The slide along edge `e` from `old` to `to` (mesh units, x and z): the move turned into the edge's frame,
/// its part across the edge dropped, then pushed back a unit at a time while still on the outer side.
fn slide(e: ([i16; 3], [i16; 3]), old: [f32; 3], to: [f32; 3]) -> [f32; 3] {
    let dir = normalize([
        (i32::from(e.1[0]) - i32::from(e.0[0])) as f32,
        0.0,
        (i32::from(e.1[2]) - i32::from(e.0[2])) as f32,
        0.0,
    ]);
    let mv = [to[0] - old[0], 0.0, to[2] - old[2]];
    // The edge frame: columns (dir.x, 0, −dir.z), (0, 1, 0), (dir.z, 0, dir.x); its inverse is its transpose.
    let local = [
        (d(dir[0]) * d(mv[0]) + d(dir[2]) * d(mv[2])) as f32,
        mv[1],
        (d(dir[0]) * d(mv[2]) - d(dir[2]) * d(mv[0])) as f32,
    ];
    let back = |l: [f32; 3]| {
        [
            (d(dir[0]) * d(l[0]) - d(dir[2]) * d(l[2])) as f32,
            l[1],
            (d(dir[2]) * d(l[0]) + d(dir[0]) * d(l[2])) as f32,
        ]
    };
    let mut l = [local[0], local[1], 0.0];
    let mut s = back(l);
    while d(s[2]) * d(dir[0]) - d(s[0]) * d(dir[2]) > 0.0 {
        l[2] -= 1.0;
        s = back(l);
    }
    [s[0] + old[0], to[1], s[2] + old[2]]
}

/// The ground step of one body (see the module notes). `event` is the scene number (0x12a lifts body 1's
/// ground by one); `is_player` whether the body is the player's; `remap` the scripts' attribute remap table;
/// `refind` the engine's flag that makes bodies with flag bit 0x200 of +0x194 look their triangle up afresh
/// each frame without walking.
#[allow(clippy::too_many_arguments)]
pub fn ground_step(
    mesh: Option<&WalkMesh>,
    body: &mut Chr,
    id: u16,
    event: u16,
    is_player: bool,
    remap: &[u32; 64],
    refind: bool,
) {
    let pos = body.pos();
    let vel = [body.f32(VEL_X), body.f32(VEL_Z)];
    let Some(m) = mesh.filter(|m| !m.tris.is_empty()) else {
        body.set_u32(ATTR, NO_ATTR);
        body.set_f32(POS, pos[0] + vel[0]);
        body.set_f32(POS + 8, pos[2] + vel[1]);
        body.set_f32(GROUND, 0.0);
        body.set_f32(NORMAL, 0.0);
        body.set_f32(NORMAL + 4, -1.0);
        body.set_f32(NORMAL + 8, 0.0);
        return;
    };
    let s = m.scale;
    let mesh_pos = [pos[0] * s, pos[1] * s, pos[2] * s];
    for (k, v) in mesh_pos.iter().enumerate() {
        body.set_f32(MESH_POS + k as u32 * 4, *v);
    }
    let forced = refind && body.u32(FLAGS) & 0x200 != 0;
    if forced {
        body.set_u16(TRIANGLE, 0xffff);
    }
    if body.u16(TRIANGLE) == 0xffff {
        let t = m.find(mesh_pos).map_or(0xffff, |t| t as u16);
        body.set_u16(TRIANGLE, t);
    }
    let start = body.u16(TRIANGLE);
    let mut target = [mesh_pos[0] + s * vel[0], 0.0, mesh_pos[2] + s * vel[1]];
    if start == 0xffff {
        let k = if forced { 1.0 } else { 10.0 };
        body.set_f32(POS, pos[0] + vel[0] * k);
        body.set_f32(POS + 8, pos[2] + vel[1] * k);
        return;
    }
    let mut tries = 0;
    if !forced {
        loop {
            let mut walk = Walk { mesh: m, crossings: 0, edge: None };
            let t = body.u16(TRIANGLE) as usize;
            if walk.go(body, target, t, is_player, remap) || (refind && body.u32(FLAGS) & 0x200 != 0) {
                break;
            }
            if body.u16(TRIANGLE) != 0xffff
                && let Some(e) = walk.edge
            {
                target = slide(e, mesh_pos, target);
            }
            tries += 1;
            body.set_u16(TRIANGLE, start);
            if tries > 3 {
                return;
            }
        }
    }
    let t = body.u16(TRIANGLE);
    let h = if t == 0xffff {
        body.set_u32(ATTR, NO_ATTR);
        0.0
    } else {
        body.set_u32(ATTR, m.tris[t as usize].attr);
        let n = m.normal(t as usize);
        for k in 0..3u32 {
            body.set_f32(NORMAL + k * 4, n[k as usize]);
        }
        m.height(target[0], target[2], t as usize)
    };
    let h = (h / d(s)) as f32;
    let h = if event == 0x12a && id == 1 { h - 1.0 } else { h };
    body.set_f32(GROUND, h);
    body.set_f32(POS, (d(target[0]) / d(s)) as f32);
    body.set_f32(POS + 8, (d(target[2]) / d(s)) as f32);
}
