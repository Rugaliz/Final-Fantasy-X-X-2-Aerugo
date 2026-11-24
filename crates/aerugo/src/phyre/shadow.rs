//! The remaster's shadow maps: the per-map shadow mode, the shadow manager's render targets and what the
//! receiving materials (those sampling `MySMSampler`) are given.
//!
//! The shadow quality setting (`Shadow` in the game's settings: 1, 2, 3 → maps of 4096, 2048, 1024 texels; 0 →
//! no maps drawn) sizes the maps. Every map is RGBA8: the "original" and "second" advanced maps, two
//! temporaries for the blur, and an 8 × 8 dummy. A map load clears them all to (0, 0, 0, 0). Receivers sample
//! the original advanced map through `MySMSampler` in modes 2–4 (else the second one, which also holds the
//! faraway map), and `MySMSampler2` is the second advanced map in mode 4, else the dummy.

use aerugo_gfx::Renderer;
use aerugo_gfx::game::{GameOp, TargetDesc, TargetHandle, TargetRef};
use glam::Mat4;

use super::Value;

/// DXGI_FORMAT_R8G8B8A8_UNORM.
const RGBA8: u32 = 28;

/// The shadow maps' size for a quality setting (0 keeps the size but draws no maps).
pub fn map_size(quality: u32) -> u32 {
    match quality {
        1 => 4096,
        2 => 2048,
        3 => 1024,
        _ => 1024,
    }
}

/// Which character group a mode-3 map lets cast into the advanced map (the manager's filter function, picked
/// per map).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    A,
    B,
    C,
    D,
    E,
    F,
}

/// A map's shadow setup: the mode (0 none, 1 faraway only, 2 one advanced cascade, 3 one cascade with a caster
/// filter, 4 two cascades), the cascades' near and far distances, the light direction for mode 4, whether the
/// first draw after the load only clears, and the filter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapShadow {
    pub mode: u32,
    pub near: [f32; 2],
    pub far: [f32; 2],
    pub light: [f32; 4],
    pub clear_first: bool,
    pub filter: Option<Filter>,
    /// Mode 4's blur weights (the manager's two globals).
    pub blend: [f32; 2],
    /// Mode 4 with a fixed box around a point for cascade 0, and whether casters are tested against the view
    /// (else all count as visible).
    pub fixed_box: bool,
    pub visibility_test: bool,
}

/// The default distances: −10 near (unless the map sets it) and 310 far.
const FAR: f32 = 310.0;

impl MapShadow {
    fn none(mode: u32) -> MapShadow {
        MapShadow {
            mode,
            near: [0.0, 0.0],
            far: [FAR, 0.0],
            light: [0.0; 4],
            clear_first: false,
            filter: None,
            blend: [0.0; 2],
            fixed_box: false,
            visibility_test: true,
        }
    }

    fn cascade(mode: u32, filter: Option<Filter>) -> MapShadow {
        MapShadow { near: [-10.0, 0.0], far: [FAR, 0.0], clear_first: true, filter, ..MapShadow::none(mode) }
    }

    fn two_cascades() -> MapShadow {
        MapShadow {
            mode: 4,
            near: [-10.0, 170.0],
            far: [190.0, 570.0],
            light: [0.0, -1.0, 0.0, 0.0],
            clear_first: true,
            filter: None,
            blend: [f32::from_bits(0x3f66_6666), f32::from_bits(0x3d4c_cccd)],
            fixed_box: false,
            visibility_test: true,
        }
    }
}

/// The shadow setup the remaster picks when a map loads: by the scene's map id (the hash of the map package
/// path), the scene's flag that forces one cascade, the event number and the event's progress value.
pub fn map_shadow(map_id: u32, forced: bool, event: u32, progress: u32) -> MapShadow {
    if forced {
        return MapShadow::cascade(2, None);
    }
    let id = map_id as i32;
    if id < 1 {
        return MapShadow::none(0);
    }
    const ONE: &[i32] = &[0x99, 0xfc9, 0x1cac, 0x21c1];
    const FOUR: &[i32] = &[
        0x12b, 0x482, 0x56d, 0xa6c, 0xfb6, 0xb48, 0xc04, 0x1165, 0x132c, 0x17e6, 0x18e7, 0x1899, 0x1666,
        0x1cd6, 0x1b5f, 0x1b84, 0x1f90, 0x1f33, 0x200f, 0x1fa1, 0x248b, 0x2349, 0x25e2, 0x2638, 0x24f9,
        0x255c, 0x257c,
    ];
    const CASCADE: &[i32] = &[
        0x125c, 0x12ea, 0x131b, 0x131e, 0xac5, 0x4a6, 0x189, 0x211, 0x199, 0x1ca, 0x1f7, 0x150, 0x21e, 0x228,
        0x234, 0x265, 0x278, 0x2de, 0x2e6, 0x2fa, 0x456, 0x2fc, 0x3e8, 0x438, 0x6a1, 0x649, 0x66f, 0x672,
        0x679, 0x63b, 0x4c4, 0x513, 0x514, 0x5c5, 0x633, 0x822, 0x76f, 0x6e1, 0x6e7, 0x7b4, 0x819, 0x8a7,
        0x974, 0xaa4, 0xe6c, 0xbae, 0xad2, 0xb28, 0xb40, 0xdc5, 0xbea, 0xdfe, 0xeda, 0xeea, 0xf01, 0xf0b,
        0xf17, 0xf5b, 0xf62, 0xf85, 0xf99, 0xea7, 0xe7c, 0x1383, 0x13c0, 0x13ca, 0x142e, 0x1250, 0x1189,
        0x11cf, 0x11d3, 0x11f6, 0x11f7, 0xfdf, 0xfc1, 0x10cb, 0x1139, 0x1d58, 0x1a6b, 0x1818, 0x174f, 0x17bd,
        0x180b, 0x1657, 0x158a, 0x1505, 0x156a, 0x1823, 0x1883, 0x1891, 0x18cd, 0x19a9, 0x193b, 0x1955,
        0x1a3a, 0x1c99, 0x1cc0, 0x1cc2, 0x1d2e, 0x1c68, 0x1b8a, 0x1bbd, 0x1bcd, 0x1be4, 0x1c0e, 0x1c36,
        0x1c45, 0x1c52, 0x1b4f, 0x1acd, 0x1ad9, 0x222f, 0x1ed9, 0x1eda, 0x1f20, 0x1f48, 0x1eb6, 0x1dae,
        0x1e1c, 0x1e48, 0x1e6c, 0x1e8a, 0x20a6, 0x203b, 0x20a3, 0x20ad, 0x217f, 0x21e8, 0x25ca, 0x261e,
        0x268c, 0x25b0, 0x24a0, 0x24c9, 0x24d8, 0x24de, 0x24f8, 0x2506, 0x2566, 0x2574, 0x22db, 0x2416,
        0x2001,
    ];
    match id {
        0x12be if progress < 0xb54 => MapShadow::two_cascades(),
        0x12be => MapShadow::cascade(3, Some(Filter::B)),
        0x1432 => MapShadow::cascade(3, Some(Filter::C)),
        0x49d => MapShadow::cascade(3, Some(Filter::D)),
        0xb96 => MapShadow::cascade(3, Some(Filter::E)),
        0x178d => MapShadow::cascade(3, Some(Filter::F)),
        0x1d21 => MapShadow::cascade(3, Some(Filter::A)),
        0x1733 if event == 0x18a => MapShadow {
            mode: 4,
            near: [10.0, 2000.0],
            far: [310.0, 3000.0],
            light: [0.0, -1.0, f32::from_bits(0xbfdd_b22d), 0.0],
            clear_first: true,
            filter: None,
            blend: [f32::from_bits(0x40d4_4444), f32::from_bits(0xbfd8_51ec)],
            fixed_box: true,
            visibility_test: false,
        },
        0x1733 => MapShadow::none(0),
        _ if ONE.contains(&id) => MapShadow::none(1),
        _ if FOUR.contains(&id) => MapShadow::two_cascades(),
        _ if CASCADE.contains(&id) => MapShadow::cascade(2, None),
        _ => MapShadow::none(0),
    }
}

/// The shadow manager's maps (created for a size, cleared on each map load).
#[derive(Debug, Default)]
pub struct ShadowMaps {
    size: u32,
    /// Original advanced, second advanced, the two blur temporaries, the 8 × 8 dummy.
    targets: Option<[TargetHandle; 5]>,
    /// The map the last clear was for.
    cleared_for: Option<(String, u32)>,
    /// The frame after a map load only clears (the manager's first-draw flag).
    pub fresh: bool,
    /// The casters and the light matrices kept from the last change of the cascade's casters.
    pub casters: Casters,
    pub kept: Option<(LightFit, Vec<usize>)>,
}

impl ShadowMaps {
    /// The maps for `size` texels (made on first use or when the size changes).
    fn ensure(&mut self, renderer: &mut Renderer, size: u32) -> Option<[TargetHandle; 5]> {
        if self.size == size
            && let Some(t) = self.targets
        {
            return Some(t);
        }
        self.clear(renderer);
        let mut make = |s| {
            renderer
                .create_target(&TargetDesc { width: s, height: s, dxgi_format: RGBA8, samples: 1 })
                .map_err(|e| log::warn!("shadow map: {e:#}"))
                .ok()
        };
        let t = [make(size)?, make(size)?, make(size)?, make(size)?, make(8)?];
        self.size = size;
        self.targets = Some(t);
        self.cleared_for = None;
        Some(t)
    }

    /// The maps for this frame: on a new map, every map is cleared to (0, 0, 0, 0) first (as the map load's
    /// target initialisation does), in the manager's order (original, second, the temporaries, the dummy).
    pub fn frame(
        &mut self,
        renderer: &mut Renderer,
        size: u32,
        map: &str,
        map_id: u32,
        ops: &mut Vec<GameOp>,
    ) -> Option<[TargetHandle; 5]> {
        let t = self.ensure(renderer, size)?;
        let key = (map.to_owned(), map_id);
        self.fresh = false;
        if self.cleared_for.as_ref() != Some(&key) {
            for h in [t[0], t[1], t[2], t[3], t[4]] {
                ops.push(GameOp::ClearColor(TargetRef::Target(h), [0.0; 4]));
            }
            self.cleared_for = Some(key);
            self.fresh = true;
            self.kept = None;
        }
        Some(t)
    }

    pub fn clear(&mut self, renderer: &mut Renderer) {
        if let Some(t) = self.targets.take() {
            for h in t {
                renderer.destroy_target(h);
            }
        }
        self.cleared_for = None;
    }
}

/// What a receiving material is given: its two shadow maps and the values the map load writes into its
/// parameters (MySMScaleBias / MySMScaleBias2 from the map size: the maps keep a 32-texel border) and the ones
/// the frame rewrites (LightViewProjection / LightViewProjection2, mainChrYPos).
#[derive(Debug, Clone, Copy)]
pub struct Receivers {
    pub map: TargetHandle,
    pub map2: TargetHandle,
    pub size: u32,
    /// LightViewProjection and LightViewProjection2. The second is only computed in mode 4; in the other modes
    /// the manager never writes it (its allocation is not cleared) and the material samples the cleared dummy
    /// map through it, which reads zero wherever it points.
    pub light_view_projection: [Mat4; 2],
    /// MainChrYPos: the mean height of the registered main characters' first instance (0 with none).
    pub main_chr_y: f32,
}

impl Receivers {
    /// The maps for `mode`: the original advanced map in modes 2–4 (else the second one), the second advanced
    /// map as `MySMSampler2` in mode 4 (else the dummy).
    pub fn new(
        targets: [TargetHandle; 5],
        mode: u32,
        size: u32,
        light_view_projection: [Mat4; 2],
        main_chr_y: f32,
    ) -> Receivers {
        let map = if (2..=4).contains(&mode) { targets[0] } else { targets[1] };
        let map2 = if mode == 4 { targets[1] } else { targets[4] };
        Receivers { map, map2, size, light_view_projection, main_chr_y }
    }

    /// The receiving parameters by name.
    pub fn value(&self, name: &str) -> Option<Value> {
        match name {
            "MySMScaleBias" | "MySMScaleBias2" => {
                let s = self.size as f32;
                let k = ((s - 64.0) as f64 / s as f64) as f32;
                let b = 32.0 / s;
                Some(Value::Floats(vec![k, k, b, b]))
            }
            "LightViewProjection" => Some(Value::Matrix(self.light_view_projection[0])),
            "LightViewProjection2" => Some(Value::Matrix(self.light_view_projection[1])),
            "mainChrYPos" => Some(Value::Floats(vec![self.main_chr_y])),
            _ => None,
        }
    }
}

/// The light matrices of a one-cascade map (modes 1–3) for one frame: the light's view, the cascade's
/// orthographic projection and LightViewProjection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightFit {
    pub view: Mat4,
    pub projection: Mat4,
    pub light_view_projection: Mat4,
}

impl LightFit {
    /// The cascade's light view-projection before the texture bias.
    pub fn raw(&self) -> Mat4 {
        self.projection * self.view
    }
}

/// The light's turn about X (straight down) and its offset along Y.
/// The game's constant is −1.5708 (not exactly −π/2).
#[allow(clippy::approx_constant)]
const LIGHT_PITCH: f32 = -1.5708;
const LIGHT_OFFSET: f32 = -5.0;

/// The orthographic projection the manager builds (left, right, bottom, top, near, far).
fn ortho(l: f32, r: f32, b: f32, t: f32, n: f32, f: f32) -> Mat4 {
    let (x, y, z) = (1.0 / (r - l), 1.0 / (t - b), 1.0 / (n - f));
    Mat4::from_cols_array(&[
        x + x,
        0.0,
        0.0,
        0.0,
        0.0,
        y + y,
        0.0,
        0.0,
        0.0,
        0.0,
        z + z,
        0.0,
        x * -(l + r),
        y * -(b + t),
        z * (n + f),
        1.0,
    ])
}

/// The light fit for one cascade, as the field update does it in modes other than 4: the light's world is a
/// translation of −5 along Y times a turn of −1.5708 about X; the light view is the inverse of its rotation; the
/// camera's frustum corners at `camera_near + near` and `camera_near + far` (from the projection's x and y
/// scales) go through light view × the camera's view; their bounds, widened by 0.01, give the orthographic box
/// (depth from −(max z) − 100 to −(min z)); LightViewProjection maps the projected x, y, z from −1…1 to 0…1.
pub fn light_fit(view: Mat4, projection: Mat4, camera_near: f32, near: f32, far: f32) -> LightFit {
    let (s, c) = LIGHT_PITCH.sin_cos();
    let rotation =
        Mat4::from_cols_array(&[1.0, 0.0, 0.0, 0.0, 0.0, c, s, 0.0, 0.0, -s, c, 0.0, 0.0, 0.0, 0.0, 1.0]);
    let world = Mat4::from_translation(glam::Vec3::new(0.0, LIGHT_OFFSET, 0.0)) * rotation;
    let mut turn = world;
    turn.w_axis = glam::Vec4::W;
    let light_view = turn.inverse();
    let to_light = light_view * view;
    let (px, py) = (projection.x_axis.x, projection.y_axis.y);
    let (ix, iy) = (if px == 0.0 { 0.0 } else { 1.0 / px }, if py == 0.0 { 0.0 } else { 1.0 / py });
    let mut lo = glam::Vec3::splat(f32::MAX);
    let mut hi = glam::Vec3::splat(-f32::MAX);
    for d in [near + camera_near, far + camera_near] {
        let (a, b) = (d * ix, d * iy);
        for (x, y) in [(-a, -b), (a, -b), (a, b), (-a, b)] {
            let p = to_light.transform_point3(glam::Vec3::new(x, y, -d));
            lo = lo.min(p);
            hi = hi.max(p);
        }
    }
    let (lo, hi) = (lo - 0.01, hi + 0.01);
    let projection = ortho(lo.x, hi.x, lo.y, hi.y, -hi.z - 100.0, -lo.z);
    let bias = Mat4::from_cols_array(&[
        0.5, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0, 0.5, 0.0, 0.5, 0.5, 0.5, 1.0,
    ]);
    LightFit { view: light_view, projection, light_view_projection: bias * projection * light_view }
}

/// A mesh instance of a casting character: its world, and its local bounds (minimum corner and size).
#[derive(Debug, Clone, Copy)]
pub struct CasterInstance {
    pub world: Mat4,
    pub min: [f32; 3],
    pub size: [f32; 3],
}

/// A character casting a shadow this frame: its identity (the body), its model name and id, its instances,
/// the length of its root's second column × 1000, its ground height and its shade.
#[derive(Debug, Clone)]
pub struct CasterInput {
    pub key: usize,
    pub name: String,
    pub id: u16,
    pub instances: Vec<CasterInstance>,
    pub scale: f32,
    pub ground: f32,
    pub shade: f32,
}

/// A grid cell's caster: whose it is, its size (1 or 2 cells a side), whether it registered this frame and
/// is seen, the cell camera's matrix, the light-space extents, the drawn texel size, the UV rectangle of its
/// part of the map, 1 / scale and shade / 2.
#[derive(Debug, Clone)]
pub struct Cell {
    pub key: usize,
    pub span: u32,
    pub registered: bool,
    pub visible: bool,
    pub name: String,
    pub id: f32,
    pub matrix: Mat4,
    pub extent: [f32; 2],
    pub texels: [i32; 2],
    pub uv: [f32; 4],
    pub inv_scale: f32,
    pub half_shade: f32,
    pub instances: Vec<CasterInstance>,
}

/// The shadow manager's 8 × 8 grid of casters (a cell per character; a large one takes a 2 × 2 block).
#[derive(Debug, Default)]
pub struct Casters {
    pub cells: Vec<Option<Cell>>,
    taken: Vec<bool>,
    /// The main character's cell (the model named "c001").
    pub main: Option<usize>,
}

/// The box (minimum corner, size) through `m`: the component-wise bounds of its eight corners.
fn box_through(m: &Mat4, min: [f32; 3], size: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let o = m.transform_point3(glam::Vec3::from(min));
    let (x, y, z) =
        (m.x_axis.truncate() * size[0], m.y_axis.truncate() * size[1], m.z_axis.truncate() * size[2]);
    let corners = [o, o + x, o + y, o + x + y, o + z, o + x + z, o + y + z, o + x + y + z];
    let lo = corners.iter().fold(glam::Vec3::splat(f32::MAX), |a, c| a.min(*c));
    let hi = corners.iter().fold(glam::Vec3::splat(-f32::MAX), |a, c| a.max(*c));
    (lo.to_array(), hi.to_array())
}

/// The texel size of a cell for light-space extents (a, b) under a limit: when the larger fits, each extent
/// + 1 (truncated); else the larger becomes the limit and the other keeps the proportion.
fn texel_size(a: f32, b: f32, limit: i32) -> [i32; 2] {
    let l = limit as f32;
    if b < a {
        if l > a {
            return [(a + 1.0) as i32, (b + 1.0) as i32];
        }
        let r = if b == 0.0 { 0.0 } else { a / b };
        [limit, if r == 0.0 { 0 } else { (d(l) / d(r)) as i32 }]
    } else {
        if l > b {
            return [(a + 1.0) as i32, (b + 1.0) as i32];
        }
        let r = if a == 0.0 { 0.0 } else { b / a };
        [if r == 0.0 { 0 } else { (d(l) / d(r)) as i32 }, limit]
    }
}

fn d(x: f32) -> f64 {
    f64::from(x)
}

impl Casters {
    /// Every cell unregistered (the hand-off does this before the characters register).
    pub fn begin_frame(&mut self) {
        self.cells.resize(64, None);
        self.taken.resize(64, false);
        for c in self.cells.iter_mut().flatten() {
            c.registered = false;
        }
    }

    /// A body's cell taken back (its character is gone).
    pub fn remove(&mut self, key: usize) {
        self.cells.resize(64, None);
        self.taken.resize(64, false);
        for i in 0..64 {
            if self.cells[i].as_ref().is_some_and(|c| c.key == key) {
                let span = self.cells[i].as_ref().map_or(1, |c| c.span);
                for (dr, dc) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                    if span > 1 || (dr, dc) == (0, 0) {
                        let j = i + dr * 8 + dc;
                        if j < 64 {
                            self.taken[j] = false;
                        }
                    }
                }
                self.cells[i] = None;
                if self.main == Some(i) {
                    self.main = None;
                }
            }
        }
    }

    /// The first free cell (or 2 × 2 block), row by row.
    fn insert(&mut self, c: &CasterInput, span: u32) -> Option<usize> {
        let limit = if span == 1 { 8 } else { 7 };
        for row in 0..limit {
            for col in 0..limit {
                let i = row * 8 + col;
                let free = if span == 1 {
                    !self.taken[i]
                } else {
                    !self.taken[i] && !self.taken[i + 1] && !self.taken[i + 8] && !self.taken[i + 9]
                };
                if !free {
                    continue;
                }
                self.taken[i] = true;
                if span > 1 {
                    self.taken[i + 1] = true;
                    self.taken[i + 8] = true;
                    self.taken[i + 9] = true;
                }
                if c.name == "c001" && span == 1 {
                    self.main = Some(i);
                }
                self.cells[i] = Some(Cell {
                    key: c.key,
                    span,
                    registered: false,
                    visible: true,
                    name: c.name.chars().take(11).collect(),
                    id: f32::from(c.id),
                    matrix: Mat4::IDENTITY,
                    extent: [0.0; 2],
                    texels: [1, 1],
                    uv: [0.0; 4],
                    inv_scale: 0.0,
                    half_shade: 0.0,
                    instances: Vec::new(),
                });
                return Some(i);
            }
        }
        log::warn!("Insert failed, maybe the shadow map is full");
        None
    }

    /// A character's registration this frame (see the module notes): its cell (found or inserted), the cell
    /// camera, the extents, the texel size and the UV rectangle it returns (cell 0's when the grid is full).
    pub fn register(&mut self, c: &CasterInput, size: u32) -> [f32; 4] {
        self.cells.resize(64, None);
        self.taken.resize(64, false);
        let Some(first) = c.instances.first().copied() else { return [0.0; 4] };
        let found = self.cells.iter().position(|x| x.as_ref().is_some_and(|x| x.key == c.key));
        let i = match found {
            Some(i) => i,
            None => {
                let span = if first.size[0] > 30.0 || first.size[2] > 30.0 { 2 } else { 1 };
                match self.insert(c, span) {
                    Some(i) => i,
                    None => return self.cells[0].as_ref().map_or([0.0; 4], |x| x.uv),
                }
            }
        };
        let (row, col) = (i / 8, i % 8);
        let centre =
            glam::Vec3::new(first.min[0] + first.size[0] * 0.5, 0.0, first.size[2] * 0.5 + first.min[2]);
        let inv = if c.scale == 0.0 { 0.0 } else { 1.0 / c.scale };
        let mut a = first.world * Mat4::from_scale(glam::Vec3::splat(inv));
        let ty = a.w_axis.y.min(-c.ground) - 0.2;
        a.w_axis.y = ty;
        let b = a * Mat4::from_translation(centre);
        a.w_axis = glam::Vec4::new(b.w_axis.x, b.w_axis.y, b.w_axis.z, 1.0);
        let (s, cs) = LIGHT_PITCH.sin_cos();
        let turn = Mat4::from_cols_array(&[
            1.0, 0.0, 0.0, 0.0, 0.0, cs, s, 0.0, 0.0, -s, cs, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]);
        let matrix = a * turn;
        let (mut lo, mut hi) = ([f32::MAX; 3], [-f32::MAX; 3]);
        for inst in &c.instances {
            let m = inst.world * matrix;
            let (l, h) = box_through(&m, inst.min, inst.size);
            for k in 0..3 {
                lo[k] = lo[k].min(l[k]);
                hi[k] = hi[k].max(h[k]);
            }
        }
        let extent = [hi[0] - lo[0], hi[1] - lo[1]];
        let eighth = size as i32 / 8;
        let span = self.cells[i].as_ref().map_or(1, |x| x.span);
        let (k, factor) = if span == 1 { (eighth, 10.0) } else { (eighth * 2, 20.0) };
        let mut t = texel_size(extent[0] * factor, extent[1] * factor, k - 4);
        let cap = eighth * 2 - 4;
        for v in &mut t {
            if cap <= *v {
                *v = cap;
            }
            if *v < 1 {
                *v = 1;
            }
        }
        let s8 = (size as i32 & !7) as f32;
        let uv = [
            0.0 / s8 + col as f32 * 0.125,
            0.0 / s8 + row as f32 * 0.125,
            ((t[0] + 4) as f32 * 0.5) / s8 + col as f32 * 0.125,
            ((t[1] + 4) as f32 * 0.5) / s8 + row as f32 * 0.125,
        ];
        if let Some(cell) = self.cells[i].as_mut() {
            cell.registered = true;
            cell.matrix = matrix;
            cell.extent = extent;
            cell.texels = t;
            cell.uv = uv;
            cell.inv_scale = inv;
            cell.half_shade = c.shade * 0.5;
            cell.instances = c.instances.clone();
        }
        uv
    }
}

/// The clip-space outcode of a point (x, y, z, w): bits for x > w, y > w, z > w, x < −w, y < −w, z < −w.
fn outcode(p: glam::Vec4) -> u8 {
    let mut c = 0;
    if p.x - p.w > 0.0 {
        c |= 1;
    }
    if p.y - p.w > 0.0 {
        c |= 2;
    }
    if p.z - p.w > 0.0 {
        c |= 4;
    }
    if p.w + p.x < 0.0 {
        c |= 8;
    }
    if p.w + p.y < 0.0 {
        c |= 0x10;
    }
    if p.w + p.z < 0.0 {
        c |= 0x20;
    }
    c
}

/// The eight corners of a box (minimum corner, size) through `m`, in clip space.
fn clip_corners(m: &Mat4, min: [f32; 3], size: [f32; 3]) -> [glam::Vec4; 8] {
    let o = *m * glam::Vec4::new(min[0], min[1], min[2], 1.0);
    let (x, y, z) = (m.x_axis * size[0], m.y_axis * size[1], m.z_axis * size[2]);
    [o, o + x, o + y, o + x + y, o + z, o + x + z, o + y + z, o + x + y + z]
}

/// Whether the box is not wholly outside one clip plane.
fn box_meets_view(m: &Mat4, min: [f32; 3], size: [f32; 3]) -> bool {
    clip_corners(m, min, size).iter().fold(0xff, |a, p| a & outcode(*p)) == 0
}

/// Whether the box lies wholly inside the view.
fn box_inside_view(m: &Mat4, min: [f32; 3], size: [f32; 3]) -> bool {
    clip_corners(m, min, size).iter().all(|p| outcode(*p) == 0)
}

/// A character instance's visibility for the shadow manager: four points of its footprint (the box's
/// minimum x and z, its height less a third of its width and depth, and the corners across x and z), each
/// lifted to 0.9 of its w: unseen when they share an outcode bit, or when all four lie below −1.1 in y / w.
fn instance_seen(m: &Mat4, min: [f32; 3], size: [f32; 3]) -> bool {
    let y = (size[1] + min[1]) - (size[2] + size[0]) / 3.0;
    let p0 = *m * glam::Vec4::new(min[0], y, min[2], 1.0);
    let (x, z) = (m.x_axis * size[0], m.z_axis * size[2]);
    let pts = [p0 + x + z, p0 + z, p0 + x, p0];
    let lifted = pts.map(|p| glam::Vec4::new(p.x, p.w * 0.9, p.z, p.w));
    if lifted.iter().fold(0xff, |a, p| a & outcode(*p)) != 0 {
        return false;
    }
    !pts.iter().all(|p| p.y / p.w <= -1.1)
}

/// Cells bigger than this (light-space extent) only need to meet the cascade's view, smaller ones must lie in
/// it.
const LARGE: f32 = 120.0;

impl Casters {
    /// The cells registered this frame, row by row.
    fn registered(&self) -> impl Iterator<Item = (usize, &Cell)> {
        self.cells.iter().enumerate().filter_map(|(i, c)| c.as_ref().filter(|c| c.registered).map(|c| (i, c)))
    }

    /// Which registered cells are seen by the camera (`view_projection`): characters whose name starts with
    /// 'c' when one of their instances is seen, every other one always; all of them when the map turns the test
    /// off.
    pub fn visibility(&mut self, view_projection: Mat4, test: bool) {
        for c in self.cells.iter_mut().flatten().filter(|c| c.registered) {
            c.visible = !test
                || !c.name.starts_with('c')
                || c.instances.iter().any(|i| instance_seen(&(view_projection * i.world), i.min, i.size));
        }
    }

    /// The cells cast into cascade 0 under the cascade's light view-projection: registered, seen, and every
    /// instance in the cascade's view (meeting it for large cells, inside it for the rest). Returns the cells.
    pub fn cascade(&self, light: Mat4) -> Vec<usize> {
        self.registered()
            .filter(|(_, c)| c.visible)
            .filter(|(_, c)| {
                c.instances.iter().all(|i| {
                    let m = light * i.world;
                    if c.extent[0] > LARGE || c.extent[1] > LARGE {
                        box_meets_view(&m, i.min, i.size)
                    } else {
                        box_inside_view(&m, i.min, i.size)
                    }
                })
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// MainChrYPos: the main character's first instance's height when it is registered and seen, else the
    /// mean over the seen characters whose name starts with 'c', else over those starting with 'n', else 0.
    pub fn main_chr_y(&self) -> f32 {
        let y = |c: &Cell| c.instances.first().map_or(0.0, |i| i.world.w_axis.y);
        if let Some(c) = self.main.and_then(|m| self.cells.get(m)).and_then(Option::as_ref)
            && c.registered
            && c.visible
        {
            return y(c);
        }
        for first in ['c', 'n'] {
            let seen: Vec<f32> = self
                .registered()
                .filter(|(_, c)| c.visible && c.name.starts_with(first))
                .map(|(_, c)| y(c))
                .collect();
            if !seen.is_empty() {
                return seen.iter().sum::<f32>() / seen.len() as f32;
            }
        }
        0.0
    }
}

/// Two rectangles (x0, y0, x1, y1): their union when they overlap.
fn merge(a: [i32; 4], b: [i32; 4]) -> Option<[i32; 4]> {
    let (ix0, ix1, iy0, iy1) = (a[0].max(b[0]), a[2].min(b[2]), a[1].max(b[1]), a[3].min(b[3]));
    if ix1 < ix0 || iy1 < iy0 {
        return None;
    }
    Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
}

/// Overlapping rectangles merged (each merge padded by `pad`) until none overlap.
fn merge_all(rects: &mut Vec<[i32; 4]>, pad: i32) {
    'again: loop {
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if let Some(u) = merge(rects[i], rects[j]) {
                    rects[i] = [u[0] - pad, u[1] - pad, u[2] + pad, u[3] + pad];
                    rects.remove(j);
                    continue 'again;
                }
            }
        }
        return;
    }
}

fn clamp_rects(rects: &mut [[i32; 4]], limit: i32) {
    for r in rects {
        for v in r.iter_mut() {
            *v = (*v).min(limit).max(0);
        }
    }
}

impl Casters {
    /// The cascade's atlas: each cell's instances' boxes through LightViewProjection, in texels of the map's
    /// inner square (size − 64), widened by 3 and truncated; clamped, merged (padded by 3), clamped, aligned out
    /// to 32 texels, merged again.
    pub fn atlas(&self, cells: &[usize], light_view_projection: Mat4, size: u32) -> Vec<[i32; 4]> {
        let k = (size as i32 - 64) as f32;
        let mut rects = Vec::new();
        for &i in cells {
            let Some(c) = self.cells.get(i).and_then(Option::as_ref) else { continue };
            let (mut lo, mut hi) = ([f32::MAX; 3], [-f32::MAX; 3]);
            for inst in &c.instances {
                let (l, h) = box_through(&(light_view_projection * inst.world), inst.min, inst.size);
                for a in 0..3 {
                    lo[a] = lo[a].min(l[a]);
                    hi[a] = hi[a].max(h[a]);
                }
            }
            let t = |v: f32, o: f64| (d(v) * d(k) + o) as i32;
            rects.push([t(lo[0], -3.0), t(lo[1], -3.0), t(hi[0], 3.0), t(hi[1], 3.0)]);
        }
        let limit = size as i32 - 64;
        clamp_rects(&mut rects, limit);
        merge_all(&mut rects, 3);
        clamp_rects(&mut rects, limit);
        for r in &mut rects {
            let down = |v: i32| (v + (v >> 31 & 31)) & !31;
            let up = |v: i32| ((v + (v >> 31 & 31)) >> 5) * 32 + if v & 31 != 0 && v >= 0 { 32 } else { 0 };
            *r = [down(r[0]), down(r[1]), up(r[2]), up(r[3])];
        }
        merge_all(&mut rects, 0);
        rects
    }
}
