//! Bridges between the format readers and the renderer.

use aerugo_formats::texture::{PixelFormat, Texture};
use aerugo_gfx::{TexFormat, TextureDesc};

pub fn tex_format(f: PixelFormat) -> Option<TexFormat> {
    Some(match f {
        PixelFormat::Bc1 => TexFormat::Bc1,
        PixelFormat::Bc2 => TexFormat::Bc2,
        PixelFormat::Bc3 => TexFormat::Bc3,
        PixelFormat::Rgba8 => TexFormat::Rgba8,
        PixelFormat::Bgra8 => TexFormat::Bgra8,
        PixelFormat::R8 => TexFormat::R8,
        PixelFormat::Rg8 | PixelFormat::Rgba16F | PixelFormat::Rgba32F | PixelFormat::R32F => return None,
    })
}

/// Upload description for a texture read from a cluster (`data` is the cluster's texture data).
pub fn texture_desc<'a>(t: &'a Texture, data: &'a [u8]) -> Option<TextureDesc<'a>> {
    let format = tex_format(t.format?)?;
    let levels = t.levels.len() as u32 / t.faces.max(1);
    Some(TextureDesc {
        format,
        width: t.width,
        height: t.height,
        levels,
        layers: t.faces,
        cube: t.faces == 6,
        data: t.levels.iter().map(|r| data.get(r.clone()).unwrap_or(&[])).collect(),
        name: &t.name,
    })
}

use aerugo_formats::Game;
use aerugo_formats::effect::{self, Effect};
use aerugo_formats::mesh::{self, Model};
use aerugo_formats::phyre::Cluster;
use aerugo_gfx::{Renderer, TextureHandle};
use aerugo_vfs::Vfs;
use glam::{Mat4, Vec3};

/// A map's fog: colour, distances (game units), curve and the largest share of fog colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fog {
    pub color: [f32; 3],
    pub near: f32,
    pub far: f32,
    pub curve: f32,
    pub limit: f32,
}

/// A map's glow settings: the full and no-glow thresholds, the gain and the spread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glow {
    pub upper: f32,
    pub lower: f32,
    pub gain: f32,
    pub spread: f32,
}

/// The remaster's character light: three directional lights (directions towards each light in game space, and
/// colours), the ambient, and the shade coefficient.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CharacterLight {
    pub lights: [([f32; 3], [f32; 3]); 3],
    pub ambient: [f32; 3],
    pub shade: f32,
}

/// The game a data path belongs to.
pub fn game_of_path(path: &str) -> Game {
    if path.to_ascii_lowercase().starts_with("ffx-2_data") { Game::Ffx2 } else { Game::Ffx }
}

/// Read an effect cluster by its asset id.
pub fn load_effect(vfs: &Vfs, game: Game, id: &str) -> Option<Effect> {
    let path = effect::effect_cluster_path(game, id)?;
    let cluster = Cluster::parse(vfs.read(&path).ok()?).ok()?;
    effect::read_effect(&cluster)
}

/// The fog of a map's scene settings: its colour, distances (game units) and curve, with the stored 0–255 limit
/// as a share of fog colour. The remaster's shaders do not use that limit: theirs is 0 (no fog) until a script
/// sets one (natives 8038, 803A, 803C), so callers replace it.
pub fn scene_fog(s: &mesh::SceneSettings) -> Fog {
    Fog {
        color: s.fog_color,
        near: s.fog_near,
        far: s.fog_far,
        curve: s.fog_curve,
        limit: (s.fog_limit / 255.0).clamp(0.0, 1.0),
    }
}

/// The remaster's character light from the field light set and the map's glow settings. Each light's colour is
/// scaled by (1 − k) × 1.2 × 0.95 (the ambient's by × 1.05), where k dims the lights by their brightness
/// (0.3 r + 0.5 g + 0.2 b, after that scale) against the brightest one as already dimmed, through the glow
/// thresholds (settled to the fixed point the remaster's repeated updates reach):
/// k = clamp(0.5 × clamp(gain² + spread², 0.01, 1.4) × (clamp(lum / max, lower, upper) − lower) / clamp(upper −
/// lower, 0.01, 1.1), 0.01, 0.99). Directions point towards the lights; each character's shade colour × 0.208125
/// adds to the ambient. On four Zanarkand maps the colours are pushed away from grey (grey + 2.6 × (c − grey),
/// clamped) for that brightness, and on three of them for the ambient itself. `map` is the map's path or name
/// (e.g. containing `znkd08`). (Per-character light overrides are not ported.)
pub fn character_light(
    lights: &aerugo_sim::field::FieldLights,
    glow: Option<Glow>,
    map: &str,
) -> CharacterLight {
    let g = glow.unwrap_or(Glow { upper: 0.0, lower: 0.0, gain: 0.0, spread: 0.0 });
    let base = |i: usize| 1.2 * if i < 3 { 0.95 } else { 1.05 };
    let lum = |c: [f32; 3]| 0.3 * c[0] + 0.5 * c[1] + 0.2 * c[2];
    let vivid = |i: usize| {
        let names: &[&str] =
            if i < 3 { &["znkd10", "znkd14", "znkd08", "znkd09"] } else { &["znkd14", "znkd08", "znkd09"] };
        names.iter().any(|n| map.contains(n))
    };
    let color = |i: usize| {
        let c = lights.0[i].color;
        if !vivid(i) {
            return c;
        }
        let grey = (c[0] + c[1] + c[2]) * 0.333;
        c.map(|x| (grey + 2.6 * (x - grey)).clamp(0.0, 1.0))
    };
    let lums: Vec<f32> = (0..4).map(|i| lum(color(i)) * base(i)).collect();
    let span = (g.upper - g.lower).clamp(0.01, 1.1);
    let strength = (g.gain.clamp(0.01, 0.99).powi(2) + g.spread.clamp(0.01, 0.99).powi(2)).clamp(0.01, 1.4);
    // The remaster recomputes k each time the ambient is set, against the brightest light as already dimmed by
    // the previous k; scripts set it several times, so the lights settle on that recurrence's fixed point.
    let mut k = [0.0f32; 4];
    for _ in 0..8 {
        let max = (0..4).map(|i| lums[i] * (1.0 - k[i])).fold(0.0, f32::max);
        if max <= 0.01 {
            k = [0.0; 4];
            break;
        }
        let next: [f32; 4] = std::array::from_fn(|i| {
            let x = ((lums[i] / max).clamp(g.lower, g.upper) - g.lower) / span;
            (strength * x * 0.5).clamp(0.01, 0.99)
        });
        if next == k {
            break;
        }
        k = next;
    }
    let scale = |i: usize| (1.0 - k[i]) * base(i);
    let light = |i: usize| {
        let d = lights.0[i].direction();
        let s = scale(i);
        ([-d[0], -d[1], -d[2]], lights.0[i].color.map(|c| c * s))
    };
    let s = scale(3);
    CharacterLight {
        lights: [light(0), light(1), light(2)],
        ambient: color(3).map(|c| c * s),
        shade: 0.208125,
    }
}

/// The glow of a map's scene settings (thresholds, gain and spread).
pub fn scene_glow(s: &mesh::SceneSettings) -> Glow {
    Glow { upper: s.glow[0], lower: s.glow[1], gain: s.glow[2], spread: s.glow[3] }
}

/// A model's data for viewers and game logic (no GPU work): the parsed model, its bounds after each instance's
/// node transform, its scene settings, counts, and each mesh's skeleton.
#[derive(Debug)]
pub struct ModelInfo {
    pub model: Model,
    pub min: Vec3,
    pub max: Vec3,
    pub scene: Option<mesh::SceneSettings>,
    pub vertices: usize,
    pub triangles: usize,
    pub textures: usize,
    pub skeletons: Vec<Option<aerugo_formats::skeleton::Skeleton>>,
}

pub fn read_model_info(vfs: &Vfs, path: &str) -> Result<ModelInfo, String> {
    let cluster = Cluster::parse(vfs.read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let model = mesh::read_model(&cluster).map_err(|e| e.to_string())?;
    let (mut min, mut max) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    let (mut vertices, mut triangles) = (0, 0);
    let mut textures = std::collections::HashSet::new();
    for m in &model.meshes {
        let world = Mat4::from_cols_array(&m.world);
        for seg in &m.segments {
            triangles += seg.index_count as usize / 3;
            if let Some(pos) = seg.positions() {
                vertices += pos.count as usize;
                for p in mesh::stream_floats(&cluster, pos).chunks_exact(3) {
                    let w = world.transform_point3(Vec3::new(p[0], p[1], p[2]));
                    min = min.min(w);
                    max = max.max(w);
                }
            }
        }
        for mat in &m.materials {
            textures.extend(mat.textures.iter().map(|t| t.asset.clone()));
        }
    }
    if min.x > max.x {
        (min, max) = (Vec3::ZERO, Vec3::ZERO);
    }
    let skeletons =
        model.meshes.iter().map(|m| aerugo_formats::skeleton::read_skeleton(&cluster, m.mesh)).collect();
    Ok(ModelInfo {
        scene: mesh::scene_settings(&cluster),
        model,
        min,
        max,
        vertices,
        triangles,
        textures: textures.len(),
        skeletons,
    })
}

pub fn load_texture(vfs: &Vfs, renderer: &mut Renderer, path: &str) -> Result<TextureHandle, String> {
    let cluster = Cluster::parse(vfs.read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let t = aerugo_formats::texture::read_textures(&cluster)
        .into_iter()
        .next()
        .ok_or("no texture")?
        .map_err(|e| e.to_string())?;
    let desc = texture_desc(&t, cluster.texture_data()).ok_or("unsupported format")?;
    renderer.create_texture(&desc).map_err(|e| format!("{e:#}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aerugo_sim::field::{FieldLight, FieldLights};

    /// bjyt0400's lights and glow settings give the scales the original's character constants showed (light 0
    /// 1.0806, ambient 1.2474 against colour / 255), and its directions point towards the lights.
    #[test]
    fn character_light_matches_the_original() {
        let lights = FieldLights([
            FieldLight::from_script(75, 85, 125, 0, 70),
            FieldLight::from_script(65, 70, 80, 230, 23),
            FieldLight::from_script(0, 0, 0, 0, 0),
            FieldLight::from_script(30, 34, 50, 0, 0),
        ]);
        let glow = Glow { upper: 1.0, lower: 0.35, gain: 0.12, spread: 0.3 };
        let l = character_light(&lights, Some(glow), "bjyt04");
        let (dir, col) = l.lights[0];
        assert!((col[0] / (75.0 / 255.0) - 1.0806).abs() < 1e-3, "{col:?}");
        assert!((l.lights[1].1[0] / (65.0 / 255.0) - 1.0964).abs() < 1e-3, "{:?}", l.lights[1]);
        assert!((l.ambient[2] / (50.0 / 255.0) - 1.2474).abs() < 1e-3, "{:?}", l.ambient);
        // Pitch 70° shines downwards (y points down), so the light is above.
        assert!(dir[1] < -0.9, "{dir:?}");
        assert!((l.shade - 0.208125).abs() < 1e-6);
    }

    /// cdsp0700 (stronger glow settings dim more): the original's settled constants.
    #[test]
    fn character_light_settles_as_the_original() {
        let lights = FieldLights([
            FieldLight::from_script(71, 106, 174, 202, 340),
            FieldLight::from_script(55, 95, 149, 301, 338),
            FieldLight::from_script(0, 0, 0, 0, 0),
            FieldLight::from_script(24, 44, 64, 0, 0),
        ]);
        let glow = Glow { upper: 1.0, lower: 0.35, gain: 0.4, spread: 0.6 };
        let l = character_light(&lights, Some(glow), "cdsp07");
        let close = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 2e-4);
        assert!(close(l.lights[0].1, [0.234_884_7, 0.350_672_9, 0.575_632_9]), "{:?}", l.lights[0]);
        assert!(close(l.lights[1].1, [0.181_952_9, 0.314_282_4, 0.492_927_1]), "{:?}", l.lights[1]);
        assert!(close(l.ambient, [0.107_915_9, 0.197_845_8, 0.287_775_7]), "{:?}", l.ambient);
        // Towards the light: the negated direction it shines in (yaw 202°, pitch 340°).
        let d = l.lights[0].0;
        assert!(
            (d[0] - 0.352).abs() < 1e-3 && (d[1] - 0.342).abs() < 1e-3 && (d[2] - 0.8713).abs() < 1e-3,
            "{d:?}"
        );
    }
}
