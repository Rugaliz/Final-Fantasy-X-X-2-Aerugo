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
        if self.cleared_for.as_ref() != Some(&key) {
            for h in [t[0], t[1], t[2], t[3], t[4]] {
                ops.push(GameOp::ClearColor(TargetRef::Target(h), [0.0; 4]));
            }
            self.cleared_for = Some(key);
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
    /// mainChrYPos: the mean height of the registered main characters' first instance (0 with none).
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
