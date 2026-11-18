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

/// Whether an effect's compiled shaders contain an alpha test (a DXBC `discard` instruction token, either
/// condition), which the remaster uses only in some variants.
fn effect_discards(vfs: &Vfs, game: Game, id: &str) -> bool {
    let Some(data) = effect::effect_cluster_path(game, id).and_then(|p| vfs.read(&p).ok()) else {
        return false;
    };
    data.chunks_exact(4)
        .any(|w| matches!(u32::from_le_bytes([w[0], w[1], w[2], w[3]]), 0x0300_000D | 0x0304_000D))
}

/// The fog of a map's scene settings: its colour, distances (game units) and curve, with the 0–255 limit as a
/// share of fog colour. Event scripts can change it in the original (natives 0x8037–0x8041, not ported yet).
pub fn scene_fog(s: &mesh::SceneSettings) -> aerugo_gfx::Fog {
    aerugo_gfx::Fog {
        color: s.fog_color,
        near: s.fog_near,
        far: s.fog_far,
        curve: s.fog_curve,
        limit: (s.fog_limit / 255.0).clamp(0.0, 1.0),
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
    let mut effect_cache: HashMap<String, (Option<MaterialState>, bool, bool)> = HashMap::new();
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
            // The water shader applies its colours itself (after the cube map lookups).
            let water = map
                && seg_material.is_some_and(|mat| {
                    aerugo_formats::effect::effect_family(&mat.effect)
                        .eq_ignore_ascii_case("phyrewatershader")
                });
            let tint: [f32; 4] = if water {
                [1.0; 4]
            } else if map {
                let (a, b) = (get4("MaterialColour"), get4("clrScaleFactor"));
                std::array::from_fn(|k| a[k] * b[k])
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
                ["TextureSampler1", "NormalMapSampler", "NormalMap2Sampler", "CubeMapSampler"].map(|p| {
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
            let (state, alpha_test, ignore_fog) =
                *effect_cache.entry(effect_id.clone()).or_insert_with(|| {
                    let fx = load_effect(vfs, game, &effect_id);
                    let ignore_fog = fx.as_ref().is_some_and(|fx| fx.switch("IGNORE_FOG"));
                    let state = fx.and_then(|fx| fx.main_state()).map(|p| material_state(&p));
                    if state.is_none() && !effect_id.is_empty() {
                        out.warnings
                            .push(format!("effect {effect_id} not found; drawing with default state"));
                    }
                    (state, effect_discards(vfs, game, &effect_id), ignore_fog)
                });
            let mut material = state.unwrap_or_default();
            if map {
                material.front_ccw = !material.front_ccw;
            }
            // The remaster's sky shader never alpha-tests (a fading sky keeps its pixels); the map variants that
            // alpha-test discard under the material's threshold (0.5 stands in until the material's own value is
            // read). Other map parts and characters keep 0.3 until their blending and thresholds are read.
            let family = aerugo_formats::effect::effect_family(&effect_id).to_ascii_lowercase();
            let curve = match family.as_str() {
                _ if !map => ToneCurve::None,
                "phyreuvanimationshader" => ToneCurve::Sky,
                _ => ToneCurve::Field,
            };
            let scalar = |name: &str| {
                m.materials
                    .get(seg.material as usize)
                    .and_then(|mat| mat.value(name))
                    .and_then(|v| v.first().copied())
                    .unwrap_or(0.0)
            };
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
                Water {
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
                alpha_cutoff: match (curve, alpha_test) {
                    (ToneCurve::Sky, _) => 0.0,
                    (ToneCurve::Field, true) => 0.5,
                    // Blended map parts whose shader never discards keep every pixel (faint ones blend in).
                    (ToneCurve::Field, false) if material.blend.is_some() => 0.0,
                    _ if character => scalar("AlphaThreshold"),
                    _ => 0.3,
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
