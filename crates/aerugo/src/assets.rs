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

use std::collections::HashMap;

use aerugo_formats::effect::{self, Effect};
use aerugo_formats::mesh::{self, Model, VertexFormat};
use aerugo_formats::phyre::Cluster;
use aerugo_formats::{Game, effect::PassState};
use aerugo_gfx::{
    BlendFactor, BlendOp, BlendState, CompareOp, CullMode, MaterialState, MeshDesc, MeshHandle, MeshPart,
    Renderer, SceneVertex, TextureHandle, ToneCurve, Water,
};
use aerugo_vfs::Vfs;
use glam::{Mat4, Vec3};

/// A model cluster uploaded to the GPU: one mesh per instance, plus the textures it uses.
#[derive(Debug, Default)]
pub struct GpuModel {
    pub meshes: Vec<GpuMeshInstance>,
    pub textures: Vec<TextureHandle>,
    pub min: Vec3,
    pub max: Vec3,
    pub triangles: usize,
    pub vertices: usize,
    /// Problems that did not stop loading (missing textures, …).
    pub warnings: Vec<String>,
    pub model: Model,
    /// A map's colour-correction table (see [`aerugo_gfx::SceneView::color_correction`]).
    pub color_correction: Option<TextureHandle>,
    /// A map's scene settings (fog, background, glow, character light).
    pub scene: Option<mesh::SceneSettings>,
}

#[derive(Debug)]
pub struct GpuMeshInstance {
    pub name: String,
    pub mesh: MeshHandle,
    pub world: Mat4,
    pub skinned: bool,
    pub parts: Vec<String>,
    /// Format segment index of each GPU part.
    pub part_segments: Vec<usize>,
    /// Effect asset id of each GPU part's material.
    pub part_effects: Vec<String>,
    /// Index into the model's meshes (skeleton and segments live there).
    pub source: usize,
    pub skeleton: Option<aerugo_formats::skeleton::Skeleton>,
}

impl GpuModel {
    pub fn destroy(self, renderer: &mut Renderer) {
        for m in self.meshes {
            renderer.destroy_mesh(m.mesh);
        }
        for t in self.textures {
            renderer.destroy_texture(t);
        }
    }
}

fn read_vec<const N: usize>(
    cluster: &Cluster,
    s: Option<&mesh::VertexStream>,
    count: usize,
    default: [f32; N],
) -> Vec<[f32; N]> {
    let Some(s) = s else { return vec![default; count] };
    let f = mesh::stream_floats(cluster, s);
    let n = match s.format {
        VertexFormat::F32x2 => 2,
        VertexFormat::F32x3 => 3,
        VertexFormat::F32x4 | VertexFormat::U8x4 => 4,
    };
    (0..count)
        .map(|v| {
            let mut out = default;
            for (k, o) in out.iter_mut().enumerate().take(n) {
                if let Some(&x) = f.get(v * n + k) {
                    *o = x;
                }
            }
            out
        })
        .collect()
}

fn blend_factor(f: effect::BlendFactor) -> BlendFactor {
    use effect::BlendFactor as F;
    match f {
        F::Zero => BlendFactor::Zero,
        F::One => BlendFactor::One,
        F::SrcColor => BlendFactor::SrcColor,
        F::InvSrcColor => BlendFactor::OneMinusSrcColor,
        F::SrcAlpha => BlendFactor::SrcAlpha,
        F::InvSrcAlpha => BlendFactor::OneMinusSrcAlpha,
        F::DstAlpha => BlendFactor::DstAlpha,
        F::InvDstAlpha => BlendFactor::OneMinusDstAlpha,
        F::DstColor => BlendFactor::DstColor,
        F::InvDstColor => BlendFactor::OneMinusDstColor,
        F::SrcAlphaSat => BlendFactor::SrcAlphaSaturate,
        F::Constant => BlendFactor::Constant,
        F::InvConstant => BlendFactor::OneMinusConstant,
    }
}

fn blend_op(o: effect::BlendOp) -> BlendOp {
    match o {
        effect::BlendOp::Add => BlendOp::Add,
        effect::BlendOp::Subtract => BlendOp::Subtract,
        effect::BlendOp::ReverseSubtract => BlendOp::ReverseSubtract,
        effect::BlendOp::Min => BlendOp::Min,
        effect::BlendOp::Max => BlendOp::Max,
    }
}

/// The renderer state for a material's main pass.
pub fn material_state(p: &PassState) -> MaterialState {
    use effect::{Compare, Cull};
    MaterialState {
        blend: p.blend.map(|b| BlendState {
            src: blend_factor(b.src),
            dst: blend_factor(b.dst),
            op: blend_op(b.op),
            src_alpha: blend_factor(b.src_alpha),
            dst_alpha: blend_factor(b.dst_alpha),
            op_alpha: blend_op(b.op_alpha),
        }),
        write_mask: p.write_mask,
        cull: match p.cull {
            Cull::None => CullMode::None,
            Cull::Front => CullMode::Front,
            Cull::Back => CullMode::Back,
        },
        front_ccw: p.front_ccw,
        depth_test: p.depth_test,
        depth_write: p.depth_write,
        depth_compare: match p.depth_compare {
            Compare::Never => CompareOp::Never,
            Compare::Less => CompareOp::Less,
            Compare::Equal => CompareOp::Equal,
            Compare::LessEqual => CompareOp::LessOrEqual,
            Compare::Greater => CompareOp::Greater,
            Compare::NotEqual => CompareOp::NotEqual,
            Compare::GreaterEqual => CompareOp::GreaterOrEqual,
            Compare::Always => CompareOp::Always,
        },
        normals: false,
        alpha_to_coverage: p.alpha_to_coverage,
    }
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

/// What an effect's compiled pixel shaders do that the renderer copies (both vary by variant, not by family).
#[derive(Debug, Clone, Copy, Default)]
struct EffectCode {
    /// Some pixel shader alpha-tests (a DXBC `discard` instruction, either condition).
    discards: bool,
    /// The colour curve they end with: the sky curve where one has its 1/1.7 power, else the field curve where
    /// one has its 2.1 and 1.08 slopes, else none (`None` when the effect has no pixel shaders to read).
    curve: Option<ToneCurve>,
}

/// The code tokens of every pixel shader in an effect cluster (each DXBC container's SHDR or SHEX chunk).
fn pixel_shaders(data: &[u8]) -> Vec<Vec<u32>> {
    let word =
        |d: &[u8], o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = data.get(at..).and_then(|d| d.windows(4).position(|w| w == b"DXBC")) {
        let start = at + i;
        let size = word(data, start + 24).unwrap_or(0).max(4);
        let blob = &data[start..(start + size).min(data.len())];
        for c in 0..word(blob, 28).unwrap_or(0) {
            let Some(o) = word(blob, 32 + 4 * c) else { break };
            if !matches!(blob.get(o..o + 4), Some(b"SHDR" | b"SHEX")) {
                continue;
            }
            let len = word(blob, o + 4).unwrap_or(0);
            let code = blob.get(o + 8..o + 8 + len).unwrap_or_default();
            let tokens: Vec<u32> =
                code.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
            // The version token's program type is 0 for pixel shaders.
            if tokens.first().is_some_and(|v| v >> 16 == 0) {
                out.push(tokens);
            }
        }
        at = start + size;
    }
    out
}

/// Read an effect cluster's pixel shaders for what [`EffectCode`] records.
fn effect_code(vfs: &Vfs, game: Game, id: &str) -> EffectCode {
    let Some(data) = effect::effect_cluster_path(game, id).and_then(|p| vfs.read(&p).ok()) else {
        return EffectCode::default();
    };
    let shaders = pixel_shaders(&data);
    let has = |t: u32| shaders.iter().any(|s| s.contains(&t));
    // 1/1.7, 2.1 and 1.08 as f32.
    let curve = if shaders.is_empty() {
        None
    } else if has(0x3F16_9697) {
        Some(ToneCurve::Sky)
    } else if shaders.iter().any(|s| s.contains(&0x4006_6666) && s.contains(&0x3F8A_3D71)) {
        Some(ToneCurve::Field)
    } else {
        Some(ToneCurve::Clamp)
    };
    EffectCode { discards: has(0x0300_000D) || has(0x0304_000D), curve }
}

/// The fog of a map's scene settings: its colour, distances (game units) and curve, with the stored 0–255 limit
/// as a share of fog colour. The remaster's shaders do not use that limit: theirs is 0 (no fog) until a script
/// sets one (natives 8038, 803A, 803C), so callers replace it.
pub fn scene_fog(s: &mesh::SceneSettings) -> aerugo_gfx::Fog {
    aerugo_gfx::Fog {
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
    glow: Option<aerugo_gfx::Glow>,
    map: &str,
) -> aerugo_gfx::CharacterLight {
    let g = glow.unwrap_or(aerugo_gfx::Glow { upper: 0.0, lower: 0.0, gain: 0.0, spread: 0.0 });
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
    aerugo_gfx::CharacterLight {
        lights: [light(0), light(1), light(2)],
        ambient: color(3).map(|c| c * s),
        shade: 0.208125,
    }
}

/// The glow of a map's scene settings (thresholds, gain and spread).
pub fn scene_glow(s: &mesh::SceneSettings) -> aerugo_gfx::Glow {
    aerugo_gfx::Glow { upper: s.glow[0], lower: s.glow[1], gain: s.glow[2], spread: s.glow[3] }
}

/// Load a model cluster and everything it references.
pub fn load_model(vfs: &Vfs, renderer: &mut Renderer, path: &str) -> Result<GpuModel, String> {
    load(vfs, renderer, path, false)
}

/// A field map model. Map models face the other way from character models (they are turned into game space,
/// where characters are mirrored into it), so their triangles' front side is the other winding.
pub fn load_map_model(vfs: &Vfs, renderer: &mut Renderer, path: &str) -> Result<GpuModel, String> {
    load(vfs, renderer, path, true)
}

fn load(vfs: &Vfs, renderer: &mut Renderer, path: &str, map: bool) -> Result<GpuModel, String> {
    let cluster = Cluster::parse(vfs.read(path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let model = mesh::read_model(&cluster).map_err(|e| e.to_string())?;
    let mut out = GpuModel { min: Vec3::splat(f32::MAX), max: Vec3::splat(f32::MIN), ..Default::default() };
    let mut texture_cache: HashMap<String, Option<TextureHandle>> = HashMap::new();
    // Per effect: pass state, whether it alpha-tests, whether it ignores fog.
    let mut effect_cache: HashMap<String, (Option<MaterialState>, EffectCode, bool)> = HashMap::new();
    let game = game_of_path(path);
    for (source, m) in model.meshes.iter().enumerate() {
        // The remaster draws a map's sky objects (draw object kind 1) without their node's scale and offset:
        // with them the opening's sky shows its bright cloud band far higher than the captures do.
        let world = if map && m.kind == 1 { Mat4::IDENTITY } else { Mat4::from_cols_array(&m.world) };
        let mut part_segments = Vec::new();
        let mut part_effects = Vec::new();
        let mut vertices: Vec<SceneVertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let mut parts = Vec::new();
        let mut names = Vec::new();
        let mut skinned = false;
        for (si, seg) in m.segments.iter().enumerate() {
            let Some(pos) = seg.positions() else { continue };
            let count = pos.count as usize;
            let p = read_vec(&cluster, Some(pos), count, [0.0; 3]);
            let n = read_vec(&cluster, seg.normals(), count, [0.0, 1.0, 0.0]);
            // Texture coordinates as the instance binds them: the base texture's `Uv` (TEXCOORD0) and the
            // water shader's `Uv1` (TEXCOORD4) can each read either stream set.
            let uv_set = seg.input_set("ST", 0);
            let uv2_set = if seg.inputs.iter().any(|i| i.semantic == "ST" && i.index == 4) {
                seg.input_set("ST", 4)
            } else {
                1
            };
            let uv = read_vec(&cluster, seg.stream_in_set("ST", uv_set), count, [0.0; 2]);
            let uv2 = read_vec(&cluster, seg.stream_in_set("ST", uv2_set), count, [0.0; 2]);
            let tangent = read_vec(&cluster, seg.stream_in_set("Tangent", 0), count, [1.0, 0.0, 0.0]);
            let col = read_vec(&cluster, seg.stream("Color"), count, [1.0; 4]);
            let w = read_vec(&cluster, seg.stream("SkinWeights"), count, [0.0; 4]);
            let j = read_vec(&cluster, seg.stream("SkinIndices"), count, [0.0; 4]);
            skinned |= seg.stream("SkinIndices").is_some();
            // Map shaders multiply the texture by the material's colour and colour scale as well as the vertex
            // colour; each segment has its own vertices, so the two fold into the vertex colours here.
            let seg_material = m.materials.get(seg.material as usize);
            let get4 = |name: &str| {
                seg_material
                    .and_then(|mat| mat.value(name))
                    .filter(|v| v.len() == 4)
                    .map_or([1.0; 4], |v| [v[0], v[1], v[2], v[3]])
            };
            // The remaster's character shader never reads the "Color" stream (its data is not a colour): its
            // vertex colour is a height gradient between two material colours (both white by default), and
            // its light comes from run-time constants (left at a neutral 1 until they are read).
            let character = !map
                && seg_material.is_some_and(|mat| {
                    aerugo_formats::effect::effect_family(&mat.effect)
                        .eq_ignore_ascii_case("phyrechrlitshader")
                });
            let seg_family = seg_material
                .map(|mat| aerugo_formats::effect::effect_family(&mat.effect).to_ascii_lowercase())
                .unwrap_or_default();
            // The water and ice shaders apply their colours themselves (after the cube map lookups).
            let ice = map && seg_family == "phyreiceshader";
            let water = map && (seg_family == "phyrewatershader" || ice);
            let tint: [f32; 4] = if water {
                [1.0; 4]
            } else if map {
                let (a, b) = (get4("MaterialColour"), get4("clrScaleFactor"));
                // The additive effects shader brightens its colour by 1 + the emissiveness before the fog.
                let e = if seg_family == "additiveeffectsshader" {
                    get4("MaterialEmissiveness")
                } else {
                    [0.0; 4]
                };
                std::array::from_fn(|k| a[k] * b[k] * if k < 3 { 1.0 + e[k] } else { 1.0 })
            } else if character {
                let (a, b) = (get4("MaterialColour"), get4("VertexColorOrigin"));
                std::array::from_fn(|k| a[k] * b[k])
            } else {
                [1.0; 4]
            };
            let base = vertices.len() as u32;
            for v in 0..count {
                let wp = world.transform_point3(Vec3::from(p[v]));
                out.min = out.min.min(wp);
                out.max = out.max.max(wp);
                vertices.push(SceneVertex {
                    position: p[v],
                    normal: n[v],
                    uv: uv[v],
                    color: if character { tint } else { std::array::from_fn(|k| col[v][k] * tint[k]) },
                    weights: w[v],
                    joints: j[v].map(|x| x as u8),
                    uv2: uv2[v],
                    tangent: tangent[v],
                });
            }
            let first = indices.len() as u32;
            let idx = mesh::segment_indices(&cluster, seg);
            indices.extend(idx.iter().map(|&i| base + i.min(count.saturating_sub(1) as u32)));
            let material = m.materials.get(seg.material as usize);
            let mut texture_of = |asset: &str| {
                let tex_path = mesh::texture_cluster_path(path, asset);
                *texture_cache.entry(tex_path.clone()).or_insert_with(|| {
                    match load_texture(vfs, renderer, &tex_path) {
                        Ok(h) => {
                            out.textures.push(h);
                            Some(h)
                        }
                        Err(e) => {
                            out.warnings.push(format!("{tex_path}: {e}"));
                            None
                        }
                    }
                })
            };
            let texture = material.and_then(|mat| mat.base_texture()).and_then(&mut texture_of);
            let water_textures = water.then(|| {
                let names = if ice {
                    ["SpecularMapSampler", "", "", "CubeMapSampler"]
                } else {
                    ["TextureSampler1", "NormalMapSampler", "NormalMap2Sampler", "CubeMapSampler"]
                };
                names.map(|p| {
                    material
                        .and_then(|mat| mat.textures.iter().find(|t| t.parameter == p))
                        .and_then(|t| texture_of(&t.asset))
                })
            });
            names.push(format!(
                "segment {si}: {} triangles, material {}{}",
                idx.len() / 3,
                seg.material,
                material.and_then(|mat| mat.base_texture()).map(|t| format!(", {t}")).unwrap_or_default()
            ));
            let effect_id = material.map(|m| m.effect.clone()).unwrap_or_default();
            let (state, code, ignore_fog) = *effect_cache.entry(effect_id.clone()).or_insert_with(|| {
                let fx = load_effect(vfs, game, &effect_id);
                let ignore_fog = fx.as_ref().is_some_and(|fx| fx.switch("IGNORE_FOG"));
                let state = fx.and_then(|fx| fx.main_state()).map(|p| material_state(&p));
                if state.is_none() && !effect_id.is_empty() {
                    out.warnings.push(format!("effect {effect_id} not found; drawing with default state"));
                }
                (state, effect_code(vfs, game, &effect_id), ignore_fog)
            });
            // The effects' raster states are the remaster's (Direct3D, its own world); Aerugo draws in game space
            // (a proper rotation of the remaster's world for maps and characters alike), where every mesh's front
            // faces are the other winding.
            let mut material = state.unwrap_or_default();
            material.front_ccw = !material.front_ccw;
            // The remaster's lit map shaders and character shaders write the normal target (its ambient
            // occlusion reads it); its sky, water and blended passes do not.
            let family = aerugo_formats::effect::effect_family(&effect_id).to_ascii_lowercase();
            material.normals = material.blend.is_none()
                && matches!(family.as_str(), "phyredefaultlitshader" | "phyrechrlitshader");
            // Map parts end with the curve their shader variant has (the prerendered layers' field-curve constants
            // only curve their fog colour; they are drawn with the field curve until that shader is ported).
            let curve = match family.as_str() {
                _ if !map => ToneCurve::None,
                "phyreprerenderedshader" => ToneCurve::Field,
                _ => code.curve.unwrap_or(ToneCurve::Field),
            };
            let alpha_test = code.discards;
            let scalar_or = |name: &str, default: f32| {
                m.materials
                    .get(seg.material as usize)
                    .and_then(|mat| mat.value(name))
                    .and_then(|v| v.first().copied())
                    .unwrap_or(default)
            };
            let scalar = |name: &str| scalar_or(name, 0.0);
            let uv_scroll = [scalar("ScrollSpeed_X"), scalar("ScrollSpeed_Y")];
            let water = water_textures.map(|textures| {
                let get3 = |name: &str| {
                    seg_material
                        .and_then(|mat| mat.value(name))
                        .filter(|v| v.len() >= 3)
                        .map_or([1.0; 3], |v| [v[0], v[1], v[2]])
                };
                let (spec, colour, blend) =
                    (get3("SpecularColor"), get4("MaterialColour"), scalar("MultiTexBlendFactor"));
                let scroll =
                    |t: &str| [scalar(&format!("ScrollSpeed_{t}_X")), scalar(&format!("ScrollSpeed_{t}_Y"))];
                // Each normal map reads the base or the second coordinates, whichever set it is bound to.
                let normal_uv2 = [6, 7].map(|i| seg.input_set("ST", i) == uv2_set && uv2_set != uv_set);
                if ice {
                    return Water {
                        ice,
                        textures,
                        normal_uv2: [false; 2],
                        scroll: [[0.0; 2]; 4],
                        specular: [1.0, 1.0, 1.0, scalar("SpecularShininess")],
                        color: colour,
                        scale: get4("clrScaleFactor"),
                        fog: get3("fgEff"),
                    };
                }
                Water {
                    ice,
                    textures,
                    normal_uv2,
                    scroll: [scroll("tex0"), scroll("tex1"), scroll("nml1"), scroll("nml2")],
                    specular: [spec[0], spec[1], spec[2], scalar("SpecularShininess")],
                    color: colour.map(|c| c * blend),
                    scale: get4("clrScaleFactor"),
                    fog: get3("fgEff"),
                }
            });
            parts.push(MeshPart {
                first_index: first,
                index_count: idx.len() as u32,
                texture,
                uv_scroll,
                // Map shaders that alpha-test discard where the texture's alpha is under the material's threshold
                // (the additive effects shader tests it × the material colour's alpha); the others keep every
                // pixel. The character shader's opaque pass never alpha-tests; its transparent one tests texture ×
                // vertex alpha against the threshold.
                alpha_cutoff: match family.as_str() {
                    _ if character && material.blend.is_none() => 0.0,
                    _ if character => scalar("AlphaThreshold"),
                    _ if !map => 0.3,
                    _ if !alpha_test => 0.0,
                    "additiveeffectsshader" => {
                        let a = get4("MaterialColour")[3];
                        if a > 0.0 { scalar_or("AlphaThreshold", 0.5) / a } else { f32::INFINITY }
                    }
                    _ => scalar_or("AlphaThreshold", 0.5),
                },
                material,
                curve,
                water,
                fog: !ignore_fog,
            });
            part_segments.push(si);
            part_effects.push(effect_id);
        }
        if vertices.is_empty() {
            continue;
        }
        out.triangles += indices.len() / 3;
        out.vertices += vertices.len();
        let handle = renderer
            .create_mesh(&MeshDesc { vertices: &vertices, indices: &indices, parts })
            .map_err(|e| format!("{e:#}"))?;
        let skeleton = aerugo_formats::skeleton::read_skeleton(&cluster, m.mesh);
        out.meshes.push(GpuMeshInstance {
            name: m.name.clone(),
            mesh: handle,
            world,
            skinned: skinned && skeleton.is_some(),
            parts: names,
            part_segments,
            part_effects,
            source,
            skeleton,
        });
    }
    if out.meshes.is_empty() {
        out.min = Vec3::splat(-1.0);
        out.max = Vec3::splat(1.0);
    }
    out.model = model;
    if map {
        out.scene = mesh::scene_settings(&cluster);
    }
    if map
        && let Some(p) = mesh::color_correction_path(path)
        && vfs.exists(&p)
    {
        match load_texture(vfs, renderer, &p) {
            Ok(h) => {
                out.textures.push(h);
                out.color_correction = Some(h);
            }
            Err(e) => out.warnings.push(format!("{p}: {e}")),
        }
    }
    Ok(out)
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
        let glow = aerugo_gfx::Glow { upper: 1.0, lower: 0.35, gain: 0.12, spread: 0.3 };
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
        let glow = aerugo_gfx::Glow { upper: 1.0, lower: 0.35, gain: 0.4, spread: 0.6 };
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
