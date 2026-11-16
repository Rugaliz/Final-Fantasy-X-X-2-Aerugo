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
    Renderer, SceneVertex, TextureHandle, ToneCurve,
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
    let mut effect_cache: HashMap<String, (Option<MaterialState>, bool)> = HashMap::new();
    let game = game_of_path(path);
    for (source, m) in model.meshes.iter().enumerate() {
        let world = Mat4::from_cols_array(&m.world);
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
            let uv = read_vec(&cluster, seg.stream("ST"), count, [0.0; 2]);
            let col = read_vec(&cluster, seg.stream("Color"), count, [1.0; 4]);
            let w = read_vec(&cluster, seg.stream("SkinWeights"), count, [0.0; 4]);
            let j = read_vec(&cluster, seg.stream("SkinIndices"), count, [0.0; 4]);
            skinned |= seg.stream("SkinIndices").is_some();
            let base = vertices.len() as u32;
            for v in 0..count {
                let wp = world.transform_point3(Vec3::from(p[v]));
                out.min = out.min.min(wp);
                out.max = out.max.max(wp);
                vertices.push(SceneVertex {
                    position: p[v],
                    normal: n[v],
                    uv: uv[v],
                    color: col[v],
                    weights: w[v],
                    joints: j[v].map(|x| x as u8),
                });
            }
            let first = indices.len() as u32;
            let idx = mesh::segment_indices(&cluster, seg);
            indices.extend(idx.iter().map(|&i| base + i.min(count.saturating_sub(1) as u32)));
            let material = m.materials.get(seg.material as usize);
            let texture = match material.and_then(|mat| mat.base_texture()) {
                Some(asset) => {
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
                }
                None => None,
            };
            names.push(format!(
                "segment {si}: {} triangles, material {}{}",
                idx.len() / 3,
                seg.material,
                material.and_then(|mat| mat.base_texture()).map(|t| format!(", {t}")).unwrap_or_default()
            ));
            let effect_id = material.map(|m| m.effect.clone()).unwrap_or_default();
            let (state, alpha_test) = *effect_cache.entry(effect_id.clone()).or_insert_with(|| {
                let state = load_effect(vfs, game, &effect_id)
                    .and_then(|fx| fx.main_state())
                    .map(|p| material_state(&p));
                if state.is_none() && !effect_id.is_empty() {
                    out.warnings.push(format!("effect {effect_id} not found; drawing with default state"));
                }
                (state, effect_discards(vfs, game, &effect_id))
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
            parts.push(MeshPart {
                first_index: first,
                index_count: idx.len() as u32,
                texture,
                alpha_cutoff: match (curve, alpha_test) {
                    (ToneCurve::Sky, _) => 0.0,
                    (ToneCurve::Field, true) => 0.5,
                    _ => 0.3,
                },
                material,
                curve,
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
